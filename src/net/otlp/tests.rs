//! net/otlp/tests.rs
//! OTLP 테스트: 고정 JSON 변환, 키 대체 규칙, 서버 형식 검사·gzip·적재.

use super::mapping::map_logs_payload;
use super::run_otlp_server;
use crate::core::model::{DataSource, HookOutcome, LiveEvent};
use crate::net::admin::test_support::{
    build_test_config, build_test_context, post_raw, wait_until_listening,
};
use flate2::Compression;
use flate2::write::GzEncoder;
use rusqlite::params;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::time::Duration;

const MIXED_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/otlp/logs_mixed.json"
));
const FALLBACK_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/otlp/logs_fallback_keys.json"
));

// 1. 혼합 이벤트 변환 ---------------------------------------------------------
#[test]
fn map_logs_payload_maps_all_event_kinds() {
    let mapping = map_logs_payload(MIXED_FIXTURE.as_bytes()).expect("변환");
    assert_eq!(mapping.ignored_events, 1);
    assert_eq!(mapping.batch.api_requests.len(), 2);
    assert_eq!(mapping.batch.tool_calls.len(), 1);
    assert_eq!(mapping.batch.hook_events.len(), 1);

    // 1. api_request (event.timestamp, 문자열 intValue, doubleValue 반올림)
    let api = &mapping.batch.api_requests[0];
    assert_eq!(api.source, DataSource::Otlp);
    assert_eq!(api.request_key, "otlp:req_abc");
    assert_eq!(api.ts, "2025-11-15T09:46:40.123Z");
    assert_eq!(api.session_id, "sess-1");
    assert_eq!(api.agent, "repl_main_thread");
    assert_eq!(api.model, "claude-opus-4-1");
    assert_eq!(api.input_tokens, 1200);
    assert_eq!(api.output_tokens, 340);
    assert_eq!(api.cache_read_tokens, 9000);
    assert_eq!(api.cache_creation_tokens, 150);
    assert_eq!(api.duration_ms, Some(2346));

    // 2. 본문 이름·timeUnixNano·client_request_id 대체
    let body_only = &mapping.batch.api_requests[1];
    assert_eq!(body_only.request_key, "otlp:client-9");
    assert_eq!(body_only.ts, "2025-11-15T09:46:44.000Z");
    assert_eq!(body_only.agent, "reviewer");
    assert_eq!(body_only.duration_ms, None);

    // 3. tool_result (접두사 허용, 원본 tool_use_id)
    let tool = &mapping.batch.tool_calls[0];
    assert_eq!(tool.tool_use_id, "toolu_01ABC");
    assert_eq!(tool.tool_name, "Bash");
    assert!(tool.is_error);
    assert_eq!(tool.error_kind, "Interrupted");
    assert_eq!(tool.duration_ms, Some(812));
    assert_eq!(tool.agent, "worker");
    assert_eq!(tool.ts, "2025-11-15T09:46:41.000Z");

    // 4. hook_execution_complete
    let hook = &mapping.batch.hook_events[0];
    assert_eq!(hook.route, "hook_event");
    assert_eq!(hook.guard, "PreToolUse:Bash");
    assert_eq!(hook.outcome, HookOutcome::Deny);
    assert_eq!(hook.duration_ms, Some(35));
    assert_eq!(hook.event_key, "otlp:sess-1:9:PreToolUse:Bash");

    // 5. 세션 구간
    let sessions = &mapping.batch.sessions;
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].session_id, "sess-1");
    assert_eq!(sessions[0].first_ts, "2025-11-15T09:46:40.123Z");
    assert_eq!(sessions[0].last_ts, "2025-11-15T09:46:42.000Z");

    // 6. 프롬프트 원문 미보관
    let dumped = format!("{:?}", mapping.batch);
    assert!(!dumped.contains("SECRET PROMPT TEXT"));
}

// 2. 키 대체 규칙 -------------------------------------------------------------
#[test]
fn map_logs_payload_builds_fallback_keys() {
    let mapping = map_logs_payload(FALLBACK_FIXTURE.as_bytes()).expect("변환");
    assert_eq!(mapping.ignored_events, 1);
    let api = &mapping.batch.api_requests[0];
    assert_eq!(api.request_key, "otlp:sess-3:2026-11-15T00:30:01.000Z:3");
    let hook = &mapping.batch.hook_events[0];
    assert_eq!(hook.outcome, HookOutcome::Error);
    assert_eq!(
        hook.event_key,
        "otlp:sess-3:2026-11-15T00:30:02.000Z:PostToolUse:Edit"
    );
}

// 3. 잘못된 JSON --------------------------------------------------------------
#[test]
fn map_logs_payload_rejects_invalid_json() {
    assert!(map_logs_payload(b"not json").is_err());
    let empty = map_logs_payload(b"{}").expect("빈 봉투");
    assert!(empty.batch.api_requests.is_empty());
    assert_eq!(empty.ignored_events, 0);
}

// 4. 서버 수신·적재 -----------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otlp_server_validates_and_stores_logs() {
    // 1. 서버 기동
    let config = build_test_config("otlp");
    let port = config.ports.otlp;
    let ctx = build_test_context(config);
    let mut live_rx = ctx.live_tx.subscribe();
    let server = tokio::spawn(run_otlp_server(ctx.clone()));
    wait_until_listening(port).await;

    // 2. 형식 거부
    let (status, text) = post_raw(port, "/v1/logs", &[("Content-Type", "text/plain")], b"{}").await;
    assert_eq!(status, 415);
    assert!(text.contains("OTEL_EXPORTER_OTLP_PROTOCOL=http/json"));
    let (status, _) = post_raw(
        port,
        "/v1/unknown",
        &[("Content-Type", "application/json")],
        b"{}",
    )
    .await;
    assert_eq!(status, 404);
    let (status, _) = post_raw(
        port,
        "/v1/logs",
        &[("Content-Type", "application/json")],
        b"{",
    )
    .await;
    assert_eq!(status, 400);

    // 3. gzip 로그 적재
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(MIXED_FIXTURE.as_bytes()).expect("압축");
    let gzipped = encoder.finish().expect("압축 종료");
    let (status, text) = post_raw(
        port,
        "/v1/logs",
        &[
            ("Content-Type", "application/json; charset=utf-8"),
            ("Content-Encoding", "gzip"),
        ],
        &gzipped,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(text, "{}");
    assert_eq!(ctx.counters.otlp_batches.load(Ordering::Relaxed), 1);
    assert_eq!(ctx.counters.api_requests.load(Ordering::Relaxed), 2);
    assert_eq!(ctx.counters.tool_calls.load(Ordering::Relaxed), 1);
    assert_eq!(ctx.counters.hook_calls.load(Ordering::Relaxed), 0);

    // 4. 저장 행·실시간 이벤트
    let stored: i64 = ctx
        .store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT input_tokens FROM api_requests WHERE request_key = ?1",
                params!["otlp:req_abc"],
                |row| row.get(0),
            )
        })
        .expect("저장 행");
    assert_eq!(stored, 1200);
    let event = tokio::time::timeout(Duration::from_secs(2), live_rx.recv())
        .await
        .expect("이벤트 대기")
        .expect("이벤트 수신");
    assert!(matches!(event, LiveEvent::Api(_)));

    // 5. 재전송 중복 제거와 지표 계수
    let (status, _) = post_raw(
        port,
        "/v1/logs",
        &[("Content-Type", "application/json")],
        MIXED_FIXTURE.as_bytes(),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(ctx.counters.api_requests.load(Ordering::Relaxed), 2);
    let (status, text) = post_raw(
        port,
        "/v1/metrics",
        &[("Content-Type", "application/json")],
        b"{}",
    )
    .await;
    assert_eq!((status, text.as_str()), (200, "{}"));
    assert_eq!(ctx.counters.otlp_batches.load(Ordering::Relaxed), 3);
    server.abort();
}

// 5. 본문 상한과 gzip 폭탄 ----------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otlp_rejects_oversized_and_gzip_bomb() {
    // 1. 서버 기동
    let config = build_test_config("otlp-limit");
    let port = config.ports.otlp;
    let ctx = build_test_context(config);
    let server = tokio::spawn(run_otlp_server(ctx.clone()));
    wait_until_listening(port).await;

    // 2. 원본 16 MiB + 1 바이트
    let oversized = vec![b' '; 16 * 1024 * 1024 + 1];
    let (status, _) = post_raw(
        port,
        "/v1/logs",
        &[("Content-Type", "application/json")],
        &oversized,
    )
    .await;
    assert_eq!(status, 413);

    // 3. 해제 후 17 MiB 가 되는 gzip
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(&vec![0u8; 17 * 1024 * 1024])
        .expect("압축");
    let bomb = encoder.finish().expect("압축 종료");
    assert!(bomb.len() < 1024 * 1024, "압축본은 상한 미만이어야 함");
    let (status, _) = post_raw(
        port,
        "/v1/logs",
        &[
            ("Content-Type", "application/json"),
            ("Content-Encoding", "gzip"),
        ],
        &bomb,
    )
    .await;
    assert_eq!(status, 413);
    assert_eq!(ctx.counters.otlp_batches.load(Ordering::Relaxed), 0);
    server.abort();
}
