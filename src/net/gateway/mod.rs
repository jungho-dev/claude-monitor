//! net/gateway/mod.rs
//! Anthropic API 투명 중계 서버: 헤더 복사, 상류 호출, 응답 계측 연결 (HTTPS_PROXY 환경변수 미적용).

mod tap;
#[cfg(test)]
mod tests;

use crate::core::config::GatewayConfig;
use crate::core::context::AppContext;
use crate::core::model::{ApiRequestRow, DataSource};
use crate::net::common::{
    BoxError, ResponseBody, json_response, serve_local_http, serve_local_listener, text_response,
};
use anyhow::{Context, bail};
use bytes::Bytes;
use chrono::{SecondsFormat, Utc};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{CONTENT_ENCODING, CONTENT_TYPE, HeaderName};
use hyper::{HeaderMap, Method, Request, Response, StatusCode, Uri};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tap::{TapBody, start_response_tap};

const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const POOL_IDLE_SECS: u64 = 90;
const MESSAGES_PATH: &str = "/v1/messages";
const SESSION_HEADER: &str = "x-claude-code-session-id";
const AGENT_HEADER: &str = "x-claude-code-agent-id";

type UpstreamClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

// 1. 중계 상태 ----------------------------------------------------------------
struct GatewayState {
    ctx: Arc<AppContext>,
    client: UpstreamClient,
    upstream_base: String,
    request_counter: AtomicU64,
}

// 2. 요청 본문 탐침 형태 ------------------------------------------------------
#[derive(Debug, Default, Deserialize)]
struct MessagesProbe {
    model: Option<String>,
    stream: Option<bool>,
}

// 3. 게이트웨이 서버 ----------------------------------------------------------
pub async fn run_gateway_server(ctx: Arc<AppContext>) -> anyhow::Result<()> {
    let port = ctx.config.ports.gateway;
    let state = build_gateway_state(ctx)?;
    serve_local_http(port, move |req| {
        let state = Arc::clone(&state);
        async move { proxy_request(state, req).await }
    })
    .await
}

/// 선점한 std 리스너로 게이트웨이 서버 실행.
pub async fn run_gateway_server_on(
    ctx: Arc<AppContext>,
    listener: std::net::TcpListener,
) -> anyhow::Result<()> {
    listener
        .set_nonblocking(true)
        .context("게이트웨이 리스너 비차단 전환 실패")?;
    let listener = tokio::net::TcpListener::from_std(listener)
        .context("게이트웨이 리스너 런타임 등록 실패")?;
    let state = build_gateway_state(ctx)?;
    serve_local_listener(listener, move |req| {
        let state = Arc::clone(&state);
        async move { proxy_request(state, req).await }
    })
    .await
}

// 4. 요청 중계 ----------------------------------------------------------------
async fn proxy_request(state: Arc<GatewayState>, req: Request<Incoming>) -> Response<ResponseBody> {
    // 1. 요청 분해
    let (parts, body) = req.into_parts();
    let path = parts.uri.path().to_string();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let session_id = parts
        .headers
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let agent = parts
        .headers
        .get(AGENT_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or("main")
        .to_string();
    state
        .ctx
        .counters
        .gateway_requests
        .fetch_add(1, Ordering::Relaxed);

    // 2. 본문 수집 (상한 64 MiB)
    let body_bytes = match Limited::new(body, MAX_REQUEST_BYTES).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(err) => {
            let status = if err
                .downcast_ref::<http_body_util::LengthLimitError>()
                .is_some()
            {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::BAD_REQUEST
            };
            return text_response(status, "request body rejected");
        }
    };

    // 3. 메시지 요청 탐침 (model, stream 만 읽음)
    let tracked = parts.method == Method::POST && path == MESSAGES_PATH;
    let probe: MessagesProbe = if tracked {
        serde_json::from_slice(&body_bytes).unwrap_or_default()
    } else {
        MessagesProbe::default()
    };

    // 4. 상류 요청 구성 (hop-by-hop 과 host 제외)
    let target: Uri = match format!("{}{}", state.upstream_base, path_and_query).parse() {
        Ok(uri) => uri,
        Err(_) => return text_response(StatusCode::BAD_REQUEST, "bad request target"),
    };
    let mut upstream_req = Request::new(Full::new(body_bytes));
    *upstream_req.method_mut() = parts.method.clone();
    *upstream_req.uri_mut() = target;
    for (name, value) in parts.headers.iter() {
        if !is_hop_by_hop(name) {
            upstream_req
                .headers_mut()
                .append(name.clone(), value.clone());
        }
    }

    // 5. 계측 시작 (메시지 요청만)
    let tap = if tracked {
        let request_key = format!(
            "gw:{}:{}",
            Utc::now().timestamp_millis(),
            state.request_counter.fetch_add(1, Ordering::Relaxed)
        );
        let row = ApiRequestRow {
            source: DataSource::Gateway,
            request_key,
            ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            session_id,
            agent,
            model: probe.model.unwrap_or_default(),
            ..ApiRequestRow::default()
        };
        Some(start_response_tap(
            Arc::clone(&state.ctx),
            row,
            probe.stream.unwrap_or(false),
        ))
    } else {
        None
    };

    // 6. 상류 호출 (연결 오류는 502, 머리 대기 초과는 504, 기록은 tap 드롭에서)
    let upstream_timeout = Duration::from_secs(state.ctx.config.gateway.upstream_timeout_secs);
    let upstream_result =
        tokio::time::timeout(upstream_timeout, state.client.request(upstream_req));
    let upstream_response = match upstream_result.await {
        Ok(Ok(response)) => response,
        Err(_elapsed) => {
            if let Some(mut tap) = tap {
                tap.set_response_meta(504, "", true);
            }
            return json_response(
                StatusCode::GATEWAY_TIMEOUT,
                &serde_json::json!({
                    "type": "error",
                    "error": {
                        "type": "api_error",
                        "message": format!(
                            "rap gateway upstream timeout: no response headers within {}s",
                            upstream_timeout.as_secs()
                        ),
                    },
                }),
            );
        }
        Ok(Err(err)) => {
            let mut message = err.to_string();
            let mut source = std::error::Error::source(&err);
            while let Some(inner) = source {
                message.push_str(": ");
                message.push_str(&inner.to_string());
                source = inner.source();
            }
            if let Some(mut tap) = tap {
                tap.set_response_meta(502, "", true);
            }
            return json_response(
                StatusCode::BAD_GATEWAY,
                &serde_json::json!({
                    "type": "error",
                    "error": {
                        "type": "api_error",
                        "message": format!("rap gateway upstream error: {message}"),
                    },
                }),
            );
        }
    };

    // 7. 응답 구성 (본문은 무버퍼 스트리밍)
    let (response_parts, response_body) = upstream_response.into_parts();
    let content_type = response_parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let encoded = response_parts.headers.contains_key(CONTENT_ENCODING);
    let mut response_headers = HeaderMap::new();
    for (name, value) in response_parts.headers.iter() {
        if !is_hop_by_hop(name) {
            response_headers.append(name.clone(), value.clone());
        }
    }
    let boxed_body: ResponseBody = response_body
        .map_err(|err| -> BoxError { Box::new(err) })
        .boxed();
    let final_body = match tap {
        Some(mut tap) => {
            tap.set_response_meta(response_parts.status.as_u16(), &content_type, encoded);
            TapBody::new(boxed_body, tap).boxed()
        }
        None => boxed_body,
    };
    let mut response = Response::new(final_body);
    *response.status_mut() = response_parts.status;
    *response.headers_mut() = response_headers;
    response
}

// 5. hop-by-hop 헤더 판별 -----------------------------------------------------
fn is_hop_by_hop(name: &HeaderName) -> bool {
    let lowered = name.as_str();
    matches!(
        lowered,
        "connection" | "keep-alive" | "te" | "trailer" | "transfer-encoding" | "upgrade" | "host"
    ) || lowered.starts_with("proxy-")
}

// 6. 상류 주소 검증 -----------------------------------------------------------
/// https 상류만 허용, 경로 끝 슬래시를 뺀 기준 주소 반환.
pub fn validate_gateway_upstream(config: &GatewayConfig) -> anyhow::Result<String> {
    let upstream_uri: Uri = config
        .upstream
        .parse()
        .context("gateway.upstream 주소 형식 오류")?;
    if upstream_uri.scheme_str() != Some("https") {
        bail!("gateway.upstream 은 https 주소만 허용");
    }
    let authority = upstream_uri
        .authority()
        .context("gateway.upstream 에 호스트 없음")?;
    Ok(format!(
        "https://{authority}{}",
        upstream_uri.path().trim_end_matches('/')
    ))
}

// 7. 중계 상태 구성 -----------------------------------------------------------
fn build_gateway_state(ctx: Arc<AppContext>) -> anyhow::Result<Arc<GatewayState>> {
    // 1. 상류 주소 검증
    let upstream_base = validate_gateway_upstream(&ctx.config.gateway)?;

    // 2. 클라이언트 (rustls ring 제공자는 1회만 설치, 본문 시간 제한 없음)
    let _ = rustls::crypto::ring::default_provider().install_default();
    let connector = HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_only()
        .enable_http1()
        .enable_http2()
        .build();
    let client = Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(POOL_IDLE_SECS))
        .build(connector);

    // 3. 상태 조립
    Ok(Arc::new(GatewayState {
        ctx,
        client,
        upstream_base,
        request_counter: AtomicU64::new(0),
    }))
}

#[cfg(test)]
mod upstream_tests {
    use super::validate_gateway_upstream;
    use crate::core::config::GatewayConfig;

    // 1. 상류 주소 검증 -------------------------------------------------------
    #[test]
    fn validate_gateway_upstream_rejects_http() {
        let http = GatewayConfig {
            upstream: "http://api.example.test".to_string(),
            ..GatewayConfig::default()
        };
        let message = format!(
            "{:#}",
            validate_gateway_upstream(&http).expect_err("http 거부")
        );
        assert!(message.contains("https"), "{message}");

        let no_scheme = GatewayConfig {
            upstream: "api.example.test".to_string(),
            ..GatewayConfig::default()
        };
        assert!(validate_gateway_upstream(&no_scheme).is_err());

        let base = validate_gateway_upstream(&GatewayConfig {
            upstream: "https://api.example.test/prefix/".to_string(),
            ..GatewayConfig::default()
        })
        .expect("https 통과");
        assert_eq!(base, "https://api.example.test/prefix");
    }
}
