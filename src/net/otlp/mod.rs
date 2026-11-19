//! net/otlp/mod.rs
//! OTLP http/json·http/protobuf 수신 서버 (로그는 기록 행, 지표는 지표 점으로 적재, 트레이스는 계수만).

#[cfg(test)]
mod live_tests;
mod mapping;
mod metrics;
#[cfg(test)]
mod metrics_tests;
mod proto;
#[cfg(test)]
mod tests;

use crate::core::context::AppContext;
use crate::core::model::LiveEvent;
use crate::net::common::{
    ResponseBody, full_body, json_response, serve_local_http, serve_local_listener, text_response,
};
use anyhow::Context;
use flate2::read::GzDecoder;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONTENT_ENCODING, CONTENT_TYPE, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use mapping::{OtlpMapping, map_logs_payload};
use metrics::{CumulativeTracker, ingest_metrics_envelope};
use proto::{decode_logs_request, decode_metrics_request};
use std::borrow::Cow;
use std::io::Read;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_LIVE_EVENTS_PER_BATCH: usize = 200;
const UNSUPPORTED_TYPE_HINT: &str = "use OTEL_EXPORTER_OTLP_PROTOCOL=http/json or http/protobuf";

// 누적 지표의 계열별 직전 값 (프로세스 수명 동안 유지, 요청 간 공유)
static CUMULATIVE_TRACKER: Mutex<Option<CumulativeTracker>> = Mutex::new(None);

// 3. 수신 형식 ----------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq)]
enum PayloadFormat {
    Json,
    Protobuf,
}

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

    // 3. 형식 검사 (JSON·protobuf, gzip 허용)
    let content_type = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let format = if content_type.starts_with("application/json") {
        PayloadFormat::Json
    } else if content_type.starts_with("application/x-protobuf")
        || content_type.starts_with("application/protobuf")
    {
        PayloadFormat::Protobuf
    } else {
        return text_response(StatusCode::UNSUPPORTED_MEDIA_TYPE, UNSUPPORTED_TYPE_HINT);
    };
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

    // 4. 지표 해석·적재, 트레이스 계수
    if path == "/v1/metrics" {
        return handle_metrics_export(ctx, body_bytes, format, gzip).await;
    }
    if path != "/v1/logs" {
        ctx.counters.otlp_batches.fetch_add(1, Ordering::Relaxed);
        return build_export_response(format);
    }

    // 5. 로그 해석 (해제, 파싱, 행 변환)
    let decoded = tokio::task::spawn_blocking(move || -> Result<OtlpMapping, StatusCode> {
        let payload_bytes: Cow<[u8]> = if gzip {
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
        let mapped = match format {
            PayloadFormat::Json => map_logs_payload(&payload_bytes),
            PayloadFormat::Protobuf => decode_logs_request(&payload_bytes).and_then(|request| {
                let json_bytes = serde_json::to_vec(&request)?;
                map_logs_payload(&json_bytes)
            }),
        };
        mapped.map_err(|_| StatusCode::BAD_REQUEST)
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
            eprintln!("[cm] OTLP 적재 실패: {err:#}");
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
    if counts.otel_events > 0 {
        events.extend(batch.otel_events.into_iter().map(LiveEvent::Otel));
    }
    for event in events.into_iter().take(MAX_LIVE_EVENTS_PER_BATCH) {
        let _ = ctx.live_tx.send(event);
    }
    build_export_response(format)
}

// 5. 지표 수신 ----------------------------------------------------------------
async fn handle_metrics_export(
    ctx: Arc<AppContext>,
    body_bytes: Bytes,
    format: PayloadFormat,
    gzip: bool,
) -> Response<ResponseBody> {
    // 1. 해제, 파싱, 누적 증분 환산, 적재 (블로킹 풀, 기준값은 적재 커밋 뒤에만 갱신)
    let store_ctx = Arc::clone(&ctx);
    let ingested = tokio::task::spawn_blocking(move || {
        let payload_bytes: Cow<[u8]> = if gzip {
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
        let envelope = match format {
            PayloadFormat::Json => serde_json::from_slice(&payload_bytes).ok(),
            PayloadFormat::Protobuf => decode_metrics_request(&payload_bytes).ok(),
        };
        let Some(envelope) = envelope else {
            return Err(StatusCode::BAD_REQUEST);
        };
        let mut guard = CUMULATIVE_TRACKER
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let tracker = guard.get_or_insert_with(CumulativeTracker::default);
        Ok(ingest_metrics_envelope(
            &store_ctx.store,
            tracker,
            &envelope,
        ))
    })
    .await;

    // 2. 결과 분기
    match ingested {
        Ok(Ok(Ok(_))) => {}
        Ok(Ok(Err(err))) => {
            eprintln!("[cm] OTLP 지표 적재 실패: {err:#}");
            return text_response(StatusCode::INTERNAL_SERVER_ERROR, "store failed");
        }
        Ok(Err(status)) => return text_response(status, "invalid otlp payload"),
        Err(_) => return text_response(StatusCode::INTERNAL_SERVER_ERROR, "decode failed"),
    }

    // 3. 카운터
    ctx.counters.otlp_batches.fetch_add(1, Ordering::Relaxed);
    build_export_response(format)
}

// 4. 수신 응답 ----------------------------------------------------------------
// protobuf 는 빈 응답 메시지 (본문 없음), JSON 은 빈 객체
fn build_export_response(format: PayloadFormat) -> Response<ResponseBody> {
    if format == PayloadFormat::Json {
        return json_response(StatusCode::OK, &serde_json::json!({}));
    }
    let mut response = Response::new(full_body(Vec::<u8>::new()));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-protobuf"),
    );
    response
}
