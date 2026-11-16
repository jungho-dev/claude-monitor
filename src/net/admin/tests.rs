//! net/admin/tests.rs
//! 관리 포트 테스트: 동기 클라이언트, 토큰 검사, 집계 응답, SSE 스트림, 상태, 브라우저 거부, 리포트.

use super::client::{decode_chunked_body, send_admin_request};
use super::test_support::{
    TEST_ADMIN_TOKEN, build_test_config, build_test_context, post_raw, send_raw,
    wait_until_listening,
};
use super::{
    build_port_rows, fetch_status_blocking, post_admin_blocking, run_admin_server,
    verify_admin_token,
};
use crate::core::model::{ApiRequestRow, DataSource, IngestBatch, LiveEvent, ToolCallRow};
use crate::transcript::test_support::TempDirGuard;
use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// 1. 청크 해제 ----------------------------------------------------------------
#[test]
fn decode_chunked_body_joins_chunks() {
    let raw = b"5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\n\r\n";
    let decoded = decode_chunked_body(raw).expect("청크 해제");
    assert_eq!(decoded, b"hello world");
    assert!(decode_chunked_body(b"zz\r\nabc\r\n").is_err());
    assert!(decode_chunked_body(b"5\r\nhe").is_err());
}

// 1-1. 토큰 비교 --------------------------------------------------------------
#[test]
fn verify_admin_token_accepts_only_exact_match() {
    assert!(verify_admin_token(
        TEST_ADMIN_TOKEN.as_bytes(),
        TEST_ADMIN_TOKEN
    ));
    assert!(!verify_admin_token(b"", TEST_ADMIN_TOKEN));
    assert!(!verify_admin_token(b"wrong", TEST_ADMIN_TOKEN));
    assert!(!verify_admin_token(
        format!("{TEST_ADMIN_TOKEN}x").as_bytes(),
        TEST_ADMIN_TOKEN
    ));
    assert!(!verify_admin_token(
        &TEST_ADMIN_TOKEN.as_bytes()[1..],
        TEST_ADMIN_TOKEN
    ));
}

// 2. 상태·토큰·집계 -----------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_server_serves_status_and_guards_tokens() {
    // 1. 서버 기동
    let config = build_test_config("admin");
    let port = config.ports.admin;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let server = tokio::spawn(run_admin_server(ctx.clone()));
    wait_until_listening(port).await;

    // 2. 상태 조회
    let status = tokio::task::spawn_blocking(move || fetch_status_blocking(port))
        .await
        .expect("조인")
        .expect("상태 조회");
    assert_eq!(status.pid, std::process::id());
    assert_eq!(status.ports.len(), 4);
    assert_eq!(status.ports[0].name, "admin");
    assert!(status.ports[0].listening);
    assert!(!status.ports[1].listening);

    // 3. 토큰 검사
    let bad =
        tokio::task::spawn_blocking(move || post_admin_blocking(port, "/api/nothing", "wrong"))
            .await
            .expect("조인");
    assert!(format!("{:#}", bad.expect_err("401 기대")).contains("401"));
    let missing = tokio::task::spawn_blocking(move || {
        post_admin_blocking(port, "/api/nothing", TEST_ADMIN_TOKEN)
    })
    .await
    .expect("조인");
    assert!(format!("{:#}", missing.expect_err("404 기대")).contains("404"));

    // 4. 건강·집계·오류 경로
    let health =
        tokio::task::spawn_blocking(move || send_admin_request(port, "GET", "/health", None))
            .await
            .expect("조인")
            .expect("health");
    assert_eq!(health.status, 200);
    assert_eq!(health.body, b"ok");
    let overview =
        tokio::task::spawn_blocking(move || send_admin_request(port, "GET", "/api/overview", None))
            .await
            .expect("조인")
            .expect("overview");
    assert!(
        matches!(overview.status, 200 | 503),
        "상태 {}",
        overview.status
    );
    let bad_hours = tokio::task::spawn_blocking(move || {
        send_admin_request(port, "GET", "/api/overview?hours=abc", None)
    })
    .await
    .expect("조인")
    .expect("hours");
    assert_eq!(bad_hours.status, 400);
    let unknown =
        tokio::task::spawn_blocking(move || send_admin_request(port, "GET", "/api/unknown", None))
            .await
            .expect("조인")
            .expect("unknown");
    assert_eq!(unknown.status, 404);
    server.abort();
    let _ = server.await;
}

// 3. SSE 스트림 ---------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_events_stream_forwards_live_events() {
    // 1. 서버 기동과 구독
    let config = build_test_config("admin-sse");
    let port = config.ports.admin;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let server = tokio::spawn(run_admin_server(ctx.clone()));
    wait_until_listening(port).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.expect("연결");
    stream
        .write_all(format!("GET /api/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes())
        .await
        .expect("요청 전송");

    // 2. 연결 표식 수신
    let mut received = String::new();
    let mut buffer = [0u8; 2048];
    while !received.contains(": connected") {
        let count = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
            .await
            .expect("표식 대기 시간 초과")
            .expect("읽기");
        assert!(count > 0, "스트림 조기 종료");
        received.push_str(&String::from_utf8_lossy(&buffer[..count]));
    }
    assert!(received.contains("text/event-stream"));

    // 3. 이벤트 전달
    ctx.live_tx
        .send(LiveEvent::Notice {
            ts: "2026-11-15T00:00:00.000Z".to_string(),
            level: "info".to_string(),
            text: "probe".to_string(),
        })
        .expect("구독자 있음");
    while !received.contains("\"kind\":\"notice\"") {
        let count = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
            .await
            .expect("이벤트 대기 시간 초과")
            .expect("읽기");
        assert!(count > 0, "스트림 조기 종료");
        received.push_str(&String::from_utf8_lossy(&buffer[..count]));
    }
    assert!(received.contains("data: {"));
    server.abort();
    let _ = server.await;
}

// 4. 꺼진 게이트웨이 상태 -----------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reports_disabled_gateway_without_probe_delay() {
    // 1. 서버 기동 (꺼진 게이트웨이 포트는 점유해도 검사 제외)
    let mut config = build_test_config("admin-status-gateway");
    config.gateway.enabled = false;
    let port = config.ports.admin;
    let gateway_holder = std::net::TcpListener::bind(("127.0.0.1", config.ports.gateway))
        .expect("게이트웨이 포트 점유");
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let server = tokio::spawn(run_admin_server(ctx.clone()));
    wait_until_listening(port).await;

    // 2. 상태 조회
    let status = tokio::task::spawn_blocking(move || fetch_status_blocking(port))
        .await
        .expect("조인")
        .expect("상태 조회");
    let gateway = status
        .ports
        .iter()
        .find(|row| row.name == "gateway")
        .expect("게이트웨이 행");
    assert!(!gateway.enabled);
    assert!(!gateway.listening);
    drop(gateway_holder);
    server.abort();
    let _ = server.await;
}

// 5. 브라우저 발신 거부 -------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rejects_browser_origin() {
    let config = build_test_config("admin-origin");
    let port = config.ports.admin;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let server = tokio::spawn(run_admin_server(ctx.clone()));
    wait_until_listening(port).await;

    // 1. 유효 토큰이어도 Origin 이 있으면 거부
    let (status, text) = post_raw(
        port,
        "/api/reindex",
        &[
            ("x-cm-token", TEST_ADMIN_TOKEN),
            ("Origin", "http://evil.test"),
        ],
        b"",
    )
    .await;
    assert_eq!(status, 403);
    assert!(text.contains("browser requests not allowed"), "{text}");
    server.abort();
    let _ = server.await;
}

// 6. 리포트 생성·조회 ---------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn report_routes_round_trip() {
    // 1. 서버 기동
    let config = build_test_config("admin-report");
    let port = config.ports.admin;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let server = tokio::spawn(run_admin_server(ctx.clone()));
    wait_until_listening(port).await;

    // 2. 생성 (유효 토큰)
    let (status, _, text) = send_raw(
        "POST",
        port,
        "/api/report?date=2026-01-02",
        &[("x-cm-token", TEST_ADMIN_TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, 200, "{text}");
    assert!(text.contains("\"path\""), "{text}");

    // 3. 조회
    let (status, head, text) = send_raw("GET", port, "/api/report?date=2026-01-02", &[], b"").await;
    assert_eq!(status, 200, "{text}");
    assert!(head.contains("content-type: text/markdown"), "{head}");
    assert!(!text.is_empty());

    // 4. 날짜 없음과 없는 날짜
    let (status, _, _) = send_raw("GET", port, "/api/report", &[], b"").await;
    assert_eq!(status, 400);
    let (status, _, _) = send_raw("GET", port, "/api/report?date=2026-01-03", &[], b"").await;
    assert_eq!(status, 404);

    // 5. 토큰 없는 생성 거부
    let (status, _, _) = send_raw("POST", port, "/api/report?date=2026-01-02", &[], b"").await;
    assert_eq!(status, 401);
    server.abort();
    let _ = server.await;
}

// 6-1. 모델·에이전트 통계 -----------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stats_routes_return_seeded_aggregates() {
    // 1. 서버 기동과 시드 (최근 10분)
    let config = build_test_config("admin-stats");
    let port = config.ports.admin;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let seeded_ts =
        (Utc::now() - ChronoDuration::minutes(10)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let seeded_rows: Vec<ApiRequestRow> = [
        ("r1", "main", "claude-opus-5-5"),
        ("r2", "worker#w1", "claude-opus-5-5"),
        ("r3", "worker#w2", "claude-sonnet-5"),
    ]
    .iter()
    .map(|(key, agent, model)| ApiRequestRow {
        source: DataSource::Transcript,
        request_key: key.to_string(),
        ts: seeded_ts.clone(),
        session_id: "sess-1".to_string(),
        agent: agent.to_string(),
        model: model.to_string(),
        input_tokens: 100,
        output_tokens: 50,
        cache_read_tokens: 800,
        cache_creation_tokens: 100,
        duration_ms: Some(1000),
        ..ApiRequestRow::default()
    })
    .collect();
    let seeded_tool = ToolCallRow {
        source: DataSource::Transcript,
        tool_use_id: "tu1".to_string(),
        ts: seeded_ts.clone(),
        session_id: "sess-1".to_string(),
        agent: "worker#w1".to_string(),
        tool_name: "Bash".to_string(),
        is_error: true,
        ..ToolCallRow::default()
    };
    ctx.store
        .insert_batch(&IngestBatch {
            api_requests: seeded_rows,
            tool_calls: vec![seeded_tool],
            ..IngestBatch::default()
        })
        .expect("시드 적재");
    let server = tokio::spawn(run_admin_server(ctx.clone()));
    wait_until_listening(port).await;

    // 2. 모델 통계 (비용 큰 순서)
    let (status, _, text) = send_raw("GET", port, "/api/model-stats?hours=1", &[], b"").await;
    assert_eq!(status, 200, "{text}");
    let models: serde_json::Value = serde_json::from_str(&text).expect("모델 통계 JSON");
    assert_eq!(models.as_array().map(Vec::len), Some(2));
    assert_eq!(models[0]["model"], "claude-opus-5-5");
    assert_eq!(models[0]["requests"], 2);
    assert_eq!(models[0]["agent_types"], 2);
    assert_eq!(models[1]["model"], "claude-sonnet-5");

    // 3. 에이전트 통계 (워커 2 실행, 도구 오류 1)
    let (status, _, text) = send_raw("GET", port, "/api/agent-stats", &[], b"").await;
    assert_eq!(status, 200, "{text}");
    let agents: serde_json::Value = serde_json::from_str(&text).expect("에이전트 통계 JSON");
    assert_eq!(agents.as_array().map(Vec::len), Some(2));
    assert_eq!(agents[0]["agent"], "worker");
    assert_eq!(agents[0]["runs"], 2);
    assert_eq!(agents[0]["tool_errors"], 1);
    assert_eq!(agents[1]["agent"], "main");
    assert_eq!(agents[1]["runs"], 1);

    // 4. 형식 오류·토큰 검사
    let (status, _, _) = send_raw("GET", port, "/api/model-stats?hours=abc", &[], b"").await;
    assert_eq!(status, 400);
    let (status, _, _) = send_raw("GET", port, "/api/agent-stats?hours=abc", &[], b"").await;
    assert_eq!(status, 400);
    let (status, _, text) = send_raw("POST", port, "/api/model-stats", &[], b"").await;
    assert_eq!(status, 401, "{text}");
    server.abort();
    let _ = server.await;
}

// 7. 바인드 기록 기준 수신 여부 -----------------------------------------------
#[test]
fn port_rows_follow_bound_record_over_connect_probe() {
    // 1. 다른 프로세스가 점유한 OTLP 포트 흉내
    let foreign = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("포트 점유");
    let foreign_port = foreign.local_addr().expect("주소").port();
    let planned = vec![
        ("admin", 47330, true),
        ("otlp", foreign_port, true),
        ("hooks", 47333, true),
        ("gateway", 47332, false),
    ];

    // 2. 바인드 기록 있음 (otlp 생략)
    let rows = build_port_rows(planned.clone(), Some(&["admin", "hooks"]));
    assert!(rows[0].listening);
    assert!(!rows[1].listening, "점유 포트라도 cm 가 못 잡았으면 미수신");
    assert!(rows[2].listening);
    assert!(!rows[3].listening);

    // 3. 기록 없음 (연결 시도 폴백)
    let rows = build_port_rows(planned, None);
    assert!(rows[1].listening);
    assert!(!rows[3].listening);
    drop(foreign);
}
