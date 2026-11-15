//! net/otlp/mod.rs
//! OTLP http/json 수신 서버 (로그는 기록 행으로 적재, 지표·트레이스는 계수만).

mod mapping;
#[cfg(test)]
mod tests;

use crate::core::context::AppContext;
use crate::core::model::LiveEvent;
use crate::net::common::{
    ResponseBody, json_response, serve_local_http, serve_local_listener, text_response,
};
use anyhow::Context;
use flate2::read::GzDecoder;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::header::{CONTENT_ENCODING, CONTENT_TYPE};
use hyper::{Method, Request, Response, StatusCode};
use mapping::{OtlpMapping, map_logs_payload};
use std::borrow::Cow;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::Ordering;

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_LIVE_EVENTS_PER_BATCH: usize = 200;
const JSON_ONLY_HINT: &str = "use OTEL_EXPORTER_OTLP_PROTOCOL=http/json";

// 1. OTLP 수신 서버 -----------------------------------------------------------
pub async fn run_otlp_server(ctx: Arc<AppContext>) -> anyhow::Result<()> {
    let port = ctx.config.ports.otlp;
    serve_local_http(port, move |req| {
        let ctx = Arc::clone(&ctx);
        async move { handle_otlp_request(ctx, req).await }
    })
    .await
}

/// 선점한 std 리스너로 OTLP 서버 실행.
pub async fn run_otlp_server_on(
    ctx: Arc<AppContext>,
    listener: std::net::TcpListener,
) -> anyhow::Result<()> {
    listener
        .set_nonblocking(true)
        .context("OTLP 리스너 비차단 전환 실패")?;
    let listener =
        tokio::net::TcpListener::from_std(listener).context("OTLP 리스너 런타임 등록 실패")?;
    serve_local_listener(listener, move |req| {
        let ctx = Arc::clone(&ctx);
        async move { handle_otlp_request(ctx, req).await }
    })
    .await
}

// 2. 수신 요청 처리 -----------------------------------------------------------
async fn handle_otlp_request(
    ctx: Arc<AppContext>,
    req: Request<Incoming>,
) -> Response<ResponseBody> {
    // 1. 본문 수집 (거부 응답 전 소비)
    let (parts, body) = req.into_parts();
    let body_bytes = match Limited::new(body, MAX_BODY_BYTES).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(err) => {
            let status = if err.downcast_ref::<LengthLimitError>().is_some() {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::BAD_REQUEST
            };
            return text_response(status, "body rejected");
        }
    };

    // 2. 경로·메서드 검사
    let path = parts.uri.path().to_string();
    if !matches!(path.as_str(), "/v1/logs" | "/v1/metrics" | "/v1/traces") {
        return text_response(StatusCode::NOT_FOUND, "not found");
    }
    if parts.method != Method::POST {
        return text_response(StatusCode::METHOD_NOT_ALLOWED, "POST only");
    }

    // 3. 형식 검사 (JSON 전용, gzip 허용)
    let is_json = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .trim()
                .to_ascii_lowercase()
                .starts_with("application/json")
        })
        .unwrap_or(false);
    if !is_json {
        return text_response(StatusCode::UNSUPPORTED_MEDIA_TYPE, JSON_ONLY_HINT);
    }
    let encoding = parts
        .headers
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let gzip = match encoding.as_str() {
        "" | "identity" => false,
        "gzip" => true,
        _ => {
            return text_response(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported content-encoding",
            );
        }
    };

    // 4. 지표·트레이스 계수
    if path != "/v1/logs" {
        ctx.counters.otlp_batches.fetch_add(1, Ordering::Relaxed);
        return json_response(StatusCode::OK, &serde_json::json!({}));
    }

    // 5. 로그 해석 (해제, 파싱, 행 변환)
    let decoded = tokio::task::spawn_blocking(move || -> Result<OtlpMapping, StatusCode> {
        let json_bytes: Cow<[u8]> = if gzip {
            let mut inflated = Vec::new();
            let mut reader = GzDecoder::new(&body_bytes[..]).take(MAX_BODY_BYTES as u64 + 1);
            if reader.read_to_end(&mut inflated).is_err() {
                return Err(StatusCode::BAD_REQUEST);
            }
            if inflated.len() > MAX_BODY_BYTES {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            Cow::Owned(inflated)
        } else {
            Cow::Borrowed(&body_bytes[..])
        };
        map_logs_payload(&json_bytes).map_err(|_| StatusCode::BAD_REQUEST)
    })
    .await;
    let mapping = match decoded {
        Ok(Ok(mapping)) => mapping,
        Ok(Err(status)) => return text_response(status, "invalid otlp payload"),
        Err(_) => return text_response(StatusCode::INTERNAL_SERVER_ERROR, "decode failed"),
    };

    // 6. 적재 (블로킹 풀)
    let batch = mapping.batch;
    let store_ctx = Arc::clone(&ctx);
    let inserted = tokio::task::spawn_blocking(move || {
        let counts = store_ctx.store.insert_batch(&batch)?;
        Ok::<_, anyhow::Error>((counts, batch))
    })
    .await;
    let (counts, batch) = match inserted {
        Ok(Ok(stored)) => stored,
        Ok(Err(err)) => {
            eprintln!("[rap] OTLP 적재 실패: {err:#}");
            return text_response(StatusCode::INTERNAL_SERVER_ERROR, "store failed");
        }
        Err(_) => return text_response(StatusCode::INTERNAL_SERVER_ERROR, "store failed"),
    };

    // 7. 카운터·실시간 이벤트 (배치당 200건 상한)
    ctx.counters.otlp_batches.fetch_add(1, Ordering::Relaxed);
    ctx.counters
        .api_requests
        .fetch_add(counts.api_requests, Ordering::Relaxed);
    ctx.counters
        .tool_calls
        .fetch_add(counts.tool_calls, Ordering::Relaxed);
    let mut events: Vec<LiveEvent> = Vec::new();
    if counts.api_requests > 0 {
        events.extend(batch.api_requests.into_iter().map(LiveEvent::Api));
    }
    if counts.tool_calls > 0 {
        events.extend(batch.tool_calls.into_iter().map(LiveEvent::Tool));
    }
    if counts.hook_events > 0 {
        events.extend(batch.hook_events.into_iter().map(LiveEvent::Hook));
    }
    for event in events.into_iter().take(MAX_LIVE_EVENTS_PER_BATCH) {
        let _ = ctx.live_tx.send(event);
    }
    json_response(StatusCode::OK, &serde_json::json!({}))
}
