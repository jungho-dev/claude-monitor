//! net/gateway/tests.rs
//! 게이트웨이 테스트: SSE 경계 분할 파싱, 계측 본문 기록, 상류 오류 502·시간 초과 504, hop-by-hop 판별.

use super::is_hop_by_hop;
use super::ratelimit::extract_ratelimit_snapshot;
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
use flate2::Compression;
use flate2::write::{GzEncoder, ZlibEncoder};
use http_body_util::BodyExt;
use hyper::HeaderMap;
use hyper::body::{Body, Frame};
use hyper::header::{HeaderName, HeaderValue};
use rusqlite::params;
use std::collections::VecDeque;
use std::io::Write;
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
    tap.set_response_meta(200, "text/event-stream", "");
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
    tap.set_response_meta(200, "text/event-stream", "");

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

// 10. 압축 조각 생성 ----------------------------------------------------------
// 조각마다 sync flush 해서 스트림처럼 나눠 도착하는 압축 바이트 (마지막은 트레일러)
fn encode_pieces(pieces: &[&str], use_zlib: bool) -> Vec<Bytes> {
    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    let mut zlib = ZlibEncoder::new(Vec::new(), Compression::default());
    let mut chunks = Vec::new();
    for piece in pieces {
        let sink = if use_zlib {
            zlib.write_all(piece.as_bytes()).expect("zlib 쓰기");
            zlib.flush().expect("zlib flush");
            zlib.get_mut()
        } else {
            gzip.write_all(piece.as_bytes()).expect("gzip 쓰기");
            gzip.flush().expect("gzip flush");
            gzip.get_mut()
        };
        chunks.push(Bytes::from(std::mem::take(sink)));
    }
    let trailer = if use_zlib {
        zlib.finish().expect("zlib 종료")
    } else {
        gzip.finish().expect("gzip 종료")
    };
    chunks.push(Bytes::from(trailer));
    chunks
}

// 11. 계측 본문 통과 ----------------------------------------------------------
// 조각을 그대로 계측 본문에 흘리고 클라이언트가 받을 바이트 반환
async fn pass_through_tap(
    ctx: &Arc<AppContext>,
    request_key: &str,
    stream_requested: bool,
    content_type: &str,
    content_encoding: &str,
    chunks: Vec<Bytes>,
) -> Vec<u8> {
    let row = ApiRequestRow {
        source: DataSource::Gateway,
        request_key: request_key.to_string(),
        ts: "2026-11-19T00:00:00.000Z".to_string(),
        agent: "main".to_string(),
        model: "request-model".to_string(),
        ..ApiRequestRow::default()
    };
    let mut tap = start_response_tap(Arc::clone(ctx), row, stream_requested);
    tap.set_response_meta(200, content_type, content_encoding);
    let inner: ResponseBody = ChunkBody {
        chunks: VecDeque::from(chunks),
    }
    .boxed();
    let collected = TapBody::new(inner, tap).collect().await.expect("본문 수집");
    collected.to_bytes().to_vec()
}

// 11-1. 캐시 쓰기 5분·1시간 분리 판독 -----------------------------------------
#[test]
fn sse_usage_reads_the_cache_write_split() {
    let mut parser = super::tap::SseUsageParser::default();
    parser.feed(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-test\",\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":900,\"cache_creation\":{\"ephemeral_5m_input_tokens\":600,\"ephemeral_1h_input_tokens\":300}}}}\n\n");
    assert_eq!(parser.tally.cache_creation_tokens, 900);
    assert_eq!(parser.tally.cache_creation_5m_tokens, 600);
    assert_eq!(parser.tally.cache_creation_1h_tokens, 300);
}

// 12. gzip SSE 사용량 판독 ----------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gzip_sse_yields_usage_and_keeps_client_bytes() {
    let config = build_test_config("gw-gzip-sse");
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let ping_at = SSE_SAMPLE.find("event: ping").expect("ping 위치");
    let delta_at = SSE_SAMPLE.find("event: message_delta").expect("delta 위치");
    let chunks = encode_pieces(
        &[
            &SSE_SAMPLE[..ping_at],
            &SSE_SAMPLE[ping_at..delta_at],
            &SSE_SAMPLE[delta_at..],
        ],
        false,
    );
    let expected: Vec<u8> = chunks.iter().flat_map(|chunk| chunk.to_vec()).collect();

    let delivered = pass_through_tap(
        &ctx,
        "gw:gzip:sse",
        true,
        "text/event-stream",
        "gzip",
        chunks,
    )
    .await;
    assert_eq!(delivered, expected, "클라이언트 바이트는 압축 원본 그대로");

    let (model, input, output, first_byte, first_token, status) =
        wait_for_row(&ctx, "gw:gzip:sse").await;
    assert_eq!(model, "claude-test");
    assert_eq!((input, output), (12, 42));
    assert!(first_byte.is_some());
    assert!(first_token.is_some());
    assert_eq!(status, Some(200));
}

// 13. 압축 JSON 사용량 판독 (인코딩별) ----------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compressed_json_usage_by_encoding() {
    let config = build_test_config("gw-json-enc");
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let json = "{\"id\":\"msg_j\",\"model\":\"claude-json\",\"usage\":{\"input_tokens\":7,\"output_tokens\":9}}";
    let (head, tail) = json.split_at(json.len() / 2);

    // 1. gzip, deflate: 해제해 사용량 기록, 클라이언트 바이트 동일
    for (key, encoding, use_zlib) in [
        ("gw:json:gzip", "gzip", false),
        ("gw:json:deflate", "deflate", true),
    ] {
        let chunks = encode_pieces(&[head, tail], use_zlib);
        let expected: Vec<u8> = chunks.iter().flat_map(|chunk| chunk.to_vec()).collect();
        let delivered =
            pass_through_tap(&ctx, key, false, "application/json", encoding, chunks).await;
        assert_eq!(delivered, expected, "{encoding}");
        let (model, input, output, _, _, _) = wait_for_row(&ctx, key).await;
        assert_eq!(model, "claude-json", "{encoding}");
        assert_eq!((input, output), (7, 9), "{encoding}");
    }

    // 2. br: 미지원 인코딩은 사용량 미판독 (요청 모델 유지), 바이트는 그대로
    let raw = vec![Bytes::from_static(b"\x1b\x20\x00opaque-brotli-bytes")];
    let delivered = pass_through_tap(
        &ctx,
        "gw:json:br",
        false,
        "application/json",
        "br",
        raw.clone(),
    )
    .await;
    assert_eq!(delivered, raw[0].to_vec());
    let (model, input, output, _, _, _) = wait_for_row(&ctx, "gw:json:br").await;
    assert_eq!(model, "request-model");
    assert_eq!((input, output), (0, 0));
}

type RatelimitCase<'a> = (&'a str, u16, &'a [(&'a str, &'a str)]);

// 14. rate-limit 스냅샷 기록 --------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tap_records_ratelimit_snapshots() {
    let config = build_test_config("gw-ratelimit");
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let cases: [RatelimitCase; 3] = [
        (
            "gw:rl:429",
            429,
            &[
                ("retry-after", "5"),
                ("anthropic-ratelimit-unified-status", "rejected"),
                ("x-api-key", "secret-key"),
            ],
        ),
        ("gw:rl:429-bare", 429, &[]),
        (
            "gw:rl:200",
            200,
            &[
                ("anthropic-ratelimit-requests-remaining", "10"),
                ("anthropic-ratelimit-requests-reset", "2026-11-19T00:01:00Z"),
            ],
        ),
    ];
    for (key, status, pairs) in cases {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                HeaderName::from_bytes(name.as_bytes()).expect("헤더 이름"),
                HeaderValue::from_str(value).expect("헤더 값"),
            );
        }
        let row = ApiRequestRow {
            source: DataSource::Gateway,
            request_key: key.to_string(),
            ts: "2026-11-19T00:00:00.000Z".to_string(),
            agent: "main".to_string(),
            model: "request-model".to_string(),
            ..ApiRequestRow::default()
        };
        let mut tap = start_response_tap(Arc::clone(&ctx), row, false);
        tap.set_response_meta(status, "application/json", "");
        tap.set_ratelimit_snapshot(extract_ratelimit_snapshot(&headers, status));
        let inner: ResponseBody = ChunkBody {
            chunks: VecDeque::from([Bytes::from_static(b"{}")]),
        }
        .boxed();
        TapBody::new(inner, tap).collect().await.expect("본문 수집");
        wait_for_row(&ctx, key).await;
    }

    // 저장은 비동기이므로 3건이 모일 때까지 대기
    let mut snapshots = Vec::new();
    for _ in 0..100 {
        snapshots = ctx
            .store
            .ratelimit_snapshots_since("", 10)
            .expect("스냅샷 조회");
        if snapshots.len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert_eq!(snapshots.len(), 3);
    let with_headers = snapshots
        .iter()
        .find(|snapshot| snapshot.retry_after_secs == Some(5))
        .expect("retry-after 429 행");
    assert_eq!(with_headers.status, Some(429));
    assert_eq!(with_headers.model, "request-model");
    assert!(
        with_headers
            .extra_json
            .contains("anthropic-ratelimit-unified-status")
    );
    assert!(!with_headers.extra_json.contains("secret"));
    let bare = snapshots
        .iter()
        .find(|snapshot| snapshot.status == Some(429) && snapshot.retry_after_secs.is_none())
        .expect("헤더 없는 429 행");
    assert_eq!(bare.requests_remaining, None);
    assert_eq!(bare.extra_json, "{}");
    let ok = snapshots
        .iter()
        .find(|snapshot| snapshot.status == Some(200))
        .expect("200 행");
    assert_eq!(ok.requests_remaining, Some(10));
    assert_eq!(ok.requests_reset.as_deref(), Some("2026-11-19T00:01:00Z"));
}
