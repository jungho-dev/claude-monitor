//! net/gateway/tests.rs
//! 게이트웨이 테스트: SSE 경계 분할 파싱, 계측 본문 기록, 상류 오류 502·시간 초과 504, hop-by-hop 판별.

use super::is_hop_by_hop;
use super::run_gateway_server;
use super::tap::{SseUsageParser, TapBody, start_response_tap};
use crate::core::context::AppContext;
use crate::core::model::{ApiRequestRow, DataSource, LiveEvent};
use crate::net::admin::test_support::{
    build_test_config, build_test_context, pick_free_port, post_raw, wait_until_listening,
};
use crate::net::common::{BoxError, ResponseBody};
use crate::transcript::test_support::TempDirGuard;
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::{Body, Frame};
use hyper::header::HeaderName;
use rusqlite::params;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};
use std::time::Duration;

const SSE_SAMPLE: &str = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":12,\"cache_read_input_tokens\":100,\"cache_creation_input_tokens\":5,\"output_tokens\":1}}}\n\
\n\
event: ping\n\
data: {\"type\": \"ping\"}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"안녕\"}}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}\n\
\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\
\n";

// 1. 조각 본문 ----------------------------------------------------------------
struct ChunkBody {
    chunks: VecDeque<Bytes>,
}

impl Body for ChunkBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(self.chunks.pop_front().map(|chunk| Ok(Frame::data(chunk))))
    }
}

// 2. 행 조회 대기 -------------------------------------------------------------
async fn wait_for_row(
    ctx: &Arc<AppContext>,
    key_prefix: &str,
) -> (String, i64, i64, Option<i64>, Option<i64>, Option<i64>) {
    for _ in 0..100 {
        let found = ctx.store.with_conn(|conn| {
            conn.query_row(
                "SELECT model, input_tokens, output_tokens, first_byte_ms, first_token_ms, status \
                 FROM api_requests WHERE request_key LIKE ?1",
                params![format!("{key_prefix}%")],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
        });
        if let Ok(row) = found {
            return row;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    panic!("게이트웨이 행 저장 안 됨: {key_prefix}");
}

// 3. SSE 경계 분할 ------------------------------------------------------------
#[test]
fn sse_parser_reads_usage_at_every_split_point() {
    let bytes = SSE_SAMPLE.as_bytes();
    for split in 0..=bytes.len() {
        let mut parser = SseUsageParser::default();
        parser.feed(&bytes[..split]);
        parser.feed(&bytes[split..]);
        assert!(parser.saw_first_delta, "분할 {split}");
        assert_eq!(parser.tally.model, "claude-test", "분할 {split}");
        assert_eq!(parser.tally.input_tokens, 12, "분할 {split}");
        assert_eq!(parser.tally.output_tokens, 42, "분할 {split}");
        assert_eq!(parser.tally.cache_read_tokens, 100, "분할 {split}");
        assert_eq!(parser.tally.cache_creation_tokens, 5, "분할 {split}");
    }
}

// 4. SSE 바이트 단위 ----------------------------------------------------------
#[test]
fn sse_parser_handles_byte_chunks_and_crlf() {
    let crlf = SSE_SAMPLE.replace('\n', "\r\n");
    let mut parser = SseUsageParser::default();
    for byte in crlf.as_bytes() {
        parser.feed(std::slice::from_ref(byte));
    }
    assert!(parser.saw_first_delta);
    assert_eq!(parser.tally.output_tokens, 42);
    assert_eq!(parser.tally.model, "claude-test");

    // 델타 이전 절단
    let head_end = SSE_SAMPLE
        .find("event: content_block_delta")
        .expect("델타 위치");
    let mut truncated = SseUsageParser::default();
    truncated.feed(&SSE_SAMPLE.as_bytes()[..head_end]);
    assert!(!truncated.saw_first_delta);
    assert_eq!(truncated.tally.input_tokens, 12);
}

// 5. 계측 본문 정상 종료 ------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tap_body_records_completed_stream() {
    // 1. 준비 (바이트 경계 중간 분할)
    let config = build_test_config("gw-tap");
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let mut live_rx = ctx.live_tx.subscribe();
    let bytes = SSE_SAMPLE.as_bytes();
    let middle = bytes.len() / 2;
    let inner: ResponseBody = ChunkBody {
        chunks: VecDeque::from([
            Bytes::copy_from_slice(&bytes[..middle]),
            Bytes::copy_from_slice(&bytes[middle..]),
        ]),
    }
    .boxed();
    let row = ApiRequestRow {
        source: DataSource::Gateway,
        request_key: "gw:test:1".to_string(),
        ts: "2026-11-15T00:00:00.000Z".to_string(),
        session_id: "sess-gw".to_string(),
        agent: "main".to_string(),
        model: "request-model".to_string(),
        ..ApiRequestRow::default()
    };
    let mut tap = start_response_tap(Arc::clone(&ctx), row, true);
    tap.set_response_meta(200, "text/event-stream", false);
    assert_eq!(
        ctx.counters.gateway_active_streams.load(Ordering::Relaxed),
        1
    );

    // 2. 끝까지 소비
    let collected = TapBody::new(inner, tap).collect().await.expect("본문 수집");
    assert_eq!(collected.to_bytes().len(), bytes.len());
    assert_eq!(
        ctx.counters.gateway_active_streams.load(Ordering::Relaxed),
        0
    );

    // 3. 기록 확인
    let (model, input, output, first_byte, first_token, status) =
        wait_for_row(&ctx, "gw:test:1").await;
    assert_eq!(model, "claude-test");
    assert_eq!((input, output), (12, 42));
    assert!(first_byte.is_some());
    assert!(first_token.is_some());
    assert_eq!(status, Some(200));
    let event = tokio::time::timeout(Duration::from_secs(2), live_rx.recv())
        .await
        .expect("이벤트 대기")
        .expect("이벤트 수신");
    assert!(matches!(event, LiveEvent::Api(_)));
}

// 6. 계측 본문 중단 -----------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tap_body_records_aborted_stream() {
    let config = build_test_config("gw-abort");
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let bytes = SSE_SAMPLE.as_bytes();
    let head_end = SSE_SAMPLE
        .find("event: content_block_delta")
        .expect("델타 위치");
    let inner: ResponseBody = ChunkBody {
        chunks: VecDeque::from([
            Bytes::copy_from_slice(&bytes[..head_end]),
            Bytes::copy_from_slice(&bytes[head_end..]),
        ]),
    }
    .boxed();
    let row = ApiRequestRow {
        source: DataSource::Gateway,
        request_key: "gw:test:abort".to_string(),
        ts: "2026-11-15T00:00:00.000Z".to_string(),
        agent: "main".to_string(),
        ..ApiRequestRow::default()
    };
    let mut tap = start_response_tap(Arc::clone(&ctx), row, true);
    tap.set_response_meta(200, "text/event-stream", false);

    // 첫 조각만 읽고 드롭
    let mut body = TapBody::new(inner, tap);
    let first = body.frame().await.expect("첫 프레임").expect("프레임 오류");
    assert!(first.is_data());
    drop(body);

    let (model, input, output, first_byte, first_token, _) =
        wait_for_row(&ctx, "gw:test:abort").await;
    assert_eq!(model, "claude-test");
    assert_eq!((input, output), (12, 1));
    assert!(first_byte.is_some());
    assert_eq!(first_token, None);
    assert_eq!(
        ctx.counters.gateway_active_streams.load(Ordering::Relaxed),
        0
    );
}

// 7. 상류 연결 오류 -----------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_returns_502_when_upstream_unreachable() {
    // 1. 서버 기동 (닫힌 포트를 상류로 지정)
    let mut config = build_test_config("gw-502");
    let dead_port = pick_free_port();
    config.gateway.upstream = format!("https://127.0.0.1:{dead_port}");
    let port = config.ports.gateway;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let server = tokio::spawn(run_gateway_server(Arc::clone(&ctx)));
    wait_until_listening(port).await;

    // 2. 메시지 요청
    let (status, text) = post_raw(
        port,
        "/v1/messages",
        &[
            ("Content-Type", "application/json"),
            ("x-claude-code-session-id", "sess-502"),
        ],
        br#"{"model":"claude-test","stream":true,"messages":[]}"#,
    )
    .await;
    assert_eq!(status, 502);
    assert!(text.contains("cm gateway upstream error"));
    assert!(text.contains("\"type\":\"api_error\""));

    // 3. 오류 행 기록
    let (model, _, _, _, _, row_status) = wait_for_row(&ctx, "gw:").await;
    assert_eq!(model, "claude-test");
    assert_eq!(row_status, Some(502));
    assert_eq!(ctx.counters.gateway_requests.load(Ordering::Relaxed), 1);
    assert_eq!(
        ctx.counters.gateway_active_streams.load(Ordering::Relaxed),
        0
    );
    server.abort();
    let _ = server.await;
}

// 8. 상류 응답 머리 시간 초과 -------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_returns_504_when_upstream_never_answers() {
    // 1. 응답하지 않는 가짜 상류 (연결만 받고 유지)
    let silent_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("가짜 상류 바인드");
    let silent_port = silent_listener.local_addr().expect("가짜 상류 주소").port();
    let silent_upstream = tokio::spawn(async move {
        let mut held_streams = Vec::new();
        while let Ok((stream, _)) = silent_listener.accept().await {
            held_streams.push(stream);
        }
    });

    // 2. 서버 기동 (제한 1초, 병렬 테스트와 포트가 겹치면 재시도)
    let mut started = None;
    for _ in 0..5 {
        let mut config = build_test_config("gw-504");
        config.gateway.upstream = format!("https://127.0.0.1:{silent_port}");
        config.gateway.upstream_timeout_secs = 1;
        let port = config.ports.gateway;
        let cleanup = TempDirGuard(config.data_dir.clone());
        let ctx = build_test_context(config);
        let server = tokio::spawn(run_gateway_server(Arc::clone(&ctx)));
        tokio::time::sleep(Duration::from_millis(150)).await;
        if !server.is_finished() {
            started = Some((cleanup, port, ctx, server));
            break;
        }
    }
    let (_cleanup, port, ctx, server) = started.expect("게이트웨이 기동");
    wait_until_listening(port).await;

    // 3. 메시지 요청 (병렬 실행 중 간헐적 연결 오류 502 는 재시도)
    let mut status = 0;
    let mut text = String::new();
    for _ in 0..5 {
        (status, text) = post_raw(
            port,
            "/v1/messages",
            &[
                ("Content-Type", "application/json"),
                ("x-claude-code-session-id", "sess-504"),
            ],
            br#"{"model":"claude-test","stream":true,"messages":[]}"#,
        )
        .await;
        if status != 502 {
            break;
        }
    }
    assert_eq!(status, 504, "{text}");
    assert!(text.contains("cm gateway upstream timeout"));
    assert!(text.contains("\"type\":\"api_error\""));

    // 4. 오류 행 기록
    let mut timeout_rows = 0_i64;
    for _ in 0..100 {
        timeout_rows = ctx
            .store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM api_requests WHERE status = 504 AND model = 'claude-test'",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap_or(0);
        if timeout_rows > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert_eq!(timeout_rows, 1);
    assert_eq!(
        ctx.counters.gateway_active_streams.load(Ordering::Relaxed),
        0
    );
    server.abort();
    silent_upstream.abort();
    let _ = server.await;
    let _ = silent_upstream.await;
}

// 9. hop-by-hop 판별 ----------------------------------------------------------
#[test]
fn hop_by_hop_headers_are_filtered() {
    for name in [
        "connection",
        "keep-alive",
        "proxy-authorization",
        "te",
        "upgrade",
        "host",
        "transfer-encoding",
    ] {
        assert!(is_hop_by_hop(&HeaderName::from_static(name)), "{name}");
    }
    for name in [
        "x-api-key",
        "authorization",
        "content-type",
        "anthropic-version",
        "content-length",
    ] {
        assert!(!is_hop_by_hop(&HeaderName::from_static(name)), "{name}");
    }
}
