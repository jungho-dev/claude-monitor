//! hooks/mod.rs
//! 훅 포트: Bash 재작성 후 가드 재검사 체인, 계측 중계, command 훅용 클라이언트.

pub mod chain;
pub mod client;
pub mod record;
pub mod relay;
pub mod rewrite;
pub mod standby;
pub mod upstream;

#[cfg(test)]
mod chain_tests;

pub use client::run_hook_client;

use crate::core::context::AppContext;
use crate::core::model::{HookEventRow, HookOutcome};
use crate::net::common::{
    ResponseBody, full_body, serve_local_http, serve_local_listener, text_response,
};
use anyhow::Context;
use bytes::Bytes;
use http_body_util::{BodyExt, Limited};
use hyper::body::Incoming;
use hyper::header::{CONTENT_TYPE, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use std::sync::Arc;
use std::time::Instant;
use upstream::UpstreamClient;

// 훅 요청 본문 상한 (Write 도구 입력 대비)
const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;

// 1. 훅 서버 상태 -------------------------------------------------------------
struct HookState {
    ctx: Arc<AppContext>,
    upstream: UpstreamClient,
}

// 2. 훅 서버 ------------------------------------------------------------------
pub async fn run_hook_server(ctx: Arc<AppContext>) -> anyhow::Result<()> {
    let port = ctx.config.ports.hooks;
    let state = Arc::new(HookState {
        ctx,
        upstream: UpstreamClient::build_upstream_client(),
    });
    serve_local_http(port, move |req| {
        let state = Arc::clone(&state);
        async move { route_hook_request(&state, req).await }
    })
    .await
}

/// 선점한 std 리스너로 훅 서버 실행.
pub async fn run_hook_server_on(
    ctx: Arc<AppContext>,
    listener: std::net::TcpListener,
) -> anyhow::Result<()> {
    listener
        .set_nonblocking(true)
        .context("훅 리스너 비차단 전환 실패")?;
    let listener =
        tokio::net::TcpListener::from_std(listener).context("훅 리스너 런타임 등록 실패")?;
    let state = Arc::new(HookState {
        ctx,
        upstream: UpstreamClient::build_upstream_client(),
    });
    serve_local_listener(listener, move |req| {
        let state = Arc::clone(&state);
        async move { route_hook_request(&state, req).await }
    })
    .await
}

// 3. 요청 분기 ----------------------------------------------------------------
async fn route_hook_request(state: &HookState, req: Request<Incoming>) -> Response<ResponseBody> {
    // 1. 상태 확인
    let started = Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    if method == Method::GET && path == "/health" {
        return text_response(StatusCode::OK, "ok");
    }
    if method != Method::POST {
        return text_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
    }

    // 2. 본문 수집 (오류 응답 전에 읽어 연결 재설정 방지)
    let body: Bytes = match Limited::new(req.into_body(), MAX_REQUEST_BODY_BYTES)
        .collect()
        .await
    {
        Ok(collected) => collected.to_bytes(),
        Err(_) => return text_response(StatusCode::PAYLOAD_TOO_LARGE, "body too large"),
    };

    // 3. 경로 판별
    let is_chain = path == "/hook/pretool-chain";
    let is_standby = path == "/hook/standby-hint";
    let relay_route: Option<String> = path.strip_prefix("/hook/relay/").map(str::to_string);
    if let Some(route) = &relay_route {
        let valid = !route.is_empty()
            && route
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !valid {
            return text_response(StatusCode::BAD_REQUEST, "invalid relay route");
        }
    } else if !is_chain && !is_standby {
        return text_response(StatusCode::NOT_FOUND, "not found");
    }

    // 4. 대기 복귀 안내
    if is_standby {
        let result = standby::evaluate_standby_hint(&state.ctx, &body);
        record::record_hook_event(
            &state.ctx,
            HookEventRow {
                session_id: result.session_id,
                route: "standby-hint".to_string(),
                guard: if result.hinted {
                    "standby-hint".to_string()
                } else {
                    String::new()
                },
                outcome: HookOutcome::Allow,
                duration_ms: Some(i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)),
                ..Default::default()
            },
        );
        let mut response = Response::new(full_body(result.body));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        return response;
    }

    // 5. 체인 판정
    let hooks = &state.ctx.config.hooks;
    if is_chain {
        let result = chain::evaluate_pretool_chain(&state.upstream, hooks, &body).await;
        record::record_hook_event(
            &state.ctx,
            HookEventRow {
                session_id: result.session_id.clone(),
                route: "pretool-chain".to_string(),
                guard: result.guard.clone(),
                outcome: result.outcome,
                detail: result.detail.clone(),
                duration_ms: Some(i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)),
                saved_ms: result.saved_ms,
                ..Default::default()
            },
        );
        let mut response = Response::new(full_body(result.body));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        return response;
    }

    // 6. 계측 중계
    let route = relay_route.unwrap_or_default();
    let result = relay::relay_hook_request(&state.upstream, hooks, &route, body).await;
    record::record_hook_event(
        &state.ctx,
        HookEventRow {
            session_id: result.session_id.clone(),
            route: format!("relay/{route}"),
            guard: result.guard.clone(),
            outcome: result.outcome,
            detail: String::new(),
            duration_ms: Some(i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)),
            saved_ms: None,
            ..Default::default()
        },
    );
    if !result.failure.is_empty() {
        return text_response(StatusCode::BAD_GATEWAY, result.failure);
    }
    let mut response = Response::new(full_body(result.body));
    *response.status_mut() = result.status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
}
