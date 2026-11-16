//! transcript/tests.rs
//! 합성 기록 색인 단위 시험 (최초 읽기, 재실행, 이어 읽기, 훅 거부 판독).

use super::lines::parse_denial_text;
use super::test_support::prepare_fixture_workspace;
use super::{index_transcripts_once, run_indexer_loop};
use crate::core::context::{AppContext, LIVE_CHANNEL_CAPACITY, LiveCounters};
use crate::core::model::LiveEvent;
use crate::power::PowerState;
use chrono::Utc;
use std::fs;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::broadcast;

const SINCE: &str = "2026-11-10T00:00:00.000Z";
const UNTIL: &str = "2026-11-11T00:00:00.000Z";

// 1. 최초 읽기 ----------------------------------------------------------------
#[test]
fn first_run_indexes_main_and_subagent_rows() {
    let workspace = prepare_fixture_workspace("first-run");
    let summary = index_transcripts_once(&workspace.config, &workspace.store).expect("색인");

    // 1. 적재 건수 (msg_A 중복 병합)
    assert_eq!(summary.files_scanned, 2);
    assert_eq!(summary.files_changed, 2);
    assert_eq!(summary.counts.api_requests, 7);
    assert_eq!(summary.counts.tool_calls, 5);
    assert_eq!(summary.counts.tool_completions, 5);
    assert_eq!(summary.counts.hook_events, 2);

    // 2. 집계 (마지막 사용량 유지, 거부 2건)
    let overview = workspace.store.query_overview(SINCE, UNTIL).expect("개요");
    assert_eq!(overview.api_requests, 7);
    assert_eq!(overview.sessions, 1);
    assert_eq!(overview.input_tokens, 18);
    assert_eq!(overview.output_tokens, 610);
    assert_eq!(overview.cache_read_tokens, 92_500);
    assert_eq!(overview.cache_creation_tokens, 61_100);
    assert_eq!(overview.tool_calls, 5);
    assert_eq!(overview.tool_errors, 2);
    assert_eq!(overview.hook_denials, 2);
    assert_eq!(overview.lost_ms_estimate, 8400);

    // 3. 에이전트 라벨·프로젝트·모델 대기
    let usage = workspace
        .store
        .query_model_usage(SINCE, UNTIL)
        .expect("사용량");
    assert!(
        usage
            .iter()
            .any(|row| row.agent == "main" && row.requests == 5)
    );
    assert!(
        usage
            .iter()
            .any(|row| row.agent == "explorer" && row.requests == 2)
    );
    let project: String = workspace
        .store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT project FROM sessions WHERE session_id = 'sess-main-0001'",
                [],
                |row| row.get(0),
            )
        })
        .expect("세션");
    assert_eq!(project, "demo-project");
    let waits: Vec<(String, Option<i64>)> = workspace
        .store
        .with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT request_key, duration_ms FROM api_requests WHERE request_key IN ('msg_A', 'msg_B', 'msg_D') ORDER BY request_key",
            )?;
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect()
        })
        .expect("대기");
    assert_eq!(
        waits,
        vec![
            ("msg_A".to_string(), Some(3000)),
            ("msg_B".to_string(), Some(2000)),
            ("msg_D".to_string(), Some(5000)),
        ]
    );

    // 4. 훅 거부 (가드 라벨·명령 이름만)
    let hooks = workspace.store.query_hook_stats(SINCE, UNTIL).expect("훅");
    assert!(hooks.iter().any(|row| row.guard == "cli-guard"
        && row.detail == "cat"
        && row.route == "PreToolUse:Bash"));
    assert!(hooks.iter().any(|row| row.guard == "code-guard"
        && row.detail.is_empty()
        && row.route == "PreToolUse:Write"));

    // 5. 캐시 붕괴
    let breaks = workspace
        .store
        .query_cache_breaks(SINCE, UNTIL, 5)
        .expect("붕괴");
    assert_eq!(breaks.len(), 1);
    assert_eq!(breaks[0].likely_cause, "유휴 5분 초과 (TTL)");
}

// 2. 재실행 -------------------------------------------------------------------
#[test]
fn second_run_reads_nothing() {
    let workspace = prepare_fixture_workspace("second-run");
    index_transcripts_once(&workspace.config, &workspace.store).expect("첫 색인");
    let summary = index_transcripts_once(&workspace.config, &workspace.store).expect("재색인");
    assert_eq!(summary.files_changed, 0);
    assert_eq!(summary.bytes_read, 0);
    assert_eq!(summary.counts.api_requests, 0);
}

// 3. 이어 읽기 ----------------------------------------------------------------
#[test]
fn appended_lines_resume_with_saved_trigger() {
    let workspace = prepare_fixture_workspace("append");
    index_transcripts_once(&workspace.config, &workspace.store).expect("첫 색인");
    let main_path = workspace
        .projects_dir
        .join("demo-project")
        .join("sess-main-0001.jsonl");

    // 1. 완결 줄 1개와 불완전 줄 1개 추가
    let complete = "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5-5\",\"id\":\"msg_F\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"x\"}],\"usage\":{\"input_tokens\":1,\"output_tokens\":9,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}},\"timestamp\":\"2026-11-10T12:20:00.000Z\",\"sessionId\":\"sess-main-0001\"}\n";
    let partial_head = "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5-5\",\"id\":\"msg_G\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"x\"}],\"usage\":{\"input_tokens\":1,\"output_tokens\":9,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}},";
    let partial_tail =
        "\"timestamp\":\"2026-11-10T12:21:00.000Z\",\"sessionId\":\"sess-main-0001\"}\n";
    let mut appended = fs::OpenOptions::new()
        .append(true)
        .open(&main_path)
        .expect("추가 열기");
    appended.write_all(complete.as_bytes()).expect("완결 줄");
    appended
        .write_all(partial_head.as_bytes())
        .expect("불완전 줄");
    drop(appended);
    let summary = index_transcripts_once(&workspace.config, &workspace.store).expect("이어 색인");
    assert_eq!(summary.counts.api_requests, 1);
    let wait: Option<i64> = workspace
        .store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT duration_ms FROM api_requests WHERE request_key = 'msg_F'",
                [],
                |row| row.get(0),
            )
        })
        .expect("msg_F");
    assert_eq!(wait, Some(594_000));

    // 2. 나머지 도착 후 완결
    let mut appended = fs::OpenOptions::new()
        .append(true)
        .open(&main_path)
        .expect("추가 열기");
    appended.write_all(partial_tail.as_bytes()).expect("나머지");
    drop(appended);
    let summary = index_transcripts_once(&workspace.config, &workspace.store).expect("완결 색인");
    assert_eq!(summary.counts.api_requests, 1);

    // 3. 파일 축소 시 처음부터 재독
    fs::write(&main_path, complete.as_bytes()).expect("축소");
    let summary = index_transcripts_once(&workspace.config, &workspace.store).expect("재독");
    assert_eq!(summary.files_changed, 1);
    assert_eq!(summary.counts.api_requests, 0);
}

// 4. 훅 거부 판독 -------------------------------------------------------------
#[test]
fn denial_text_keeps_only_labels() {
    let cli = parse_denial_text(
        "PreToolUse:Bash hook error: \n[cli-guard]\n기존 CLI 차단됨 (감지: cat). 본문",
        "Bash",
    )
    .expect("cli-guard");
    assert_eq!(
        (cli.route.as_str(), cli.guard.as_str(), cli.command.as_str()),
        ("PreToolUse:Bash", "cli-guard", "cat")
    );

    let code = parse_denial_text(" [code-guard] 규칙", "Write").expect("code-guard");
    assert_eq!(
        (
            code.route.as_str(),
            code.guard.as_str(),
            code.command.as_str()
        ),
        ("PreToolUse:Write", "code-guard", "")
    );

    assert!(parse_denial_text("Error: command failed", "Bash").is_none());
    assert!(parse_denial_text("PreToolUse:Bash hook error: no label", "Bash").is_none());
    assert!(parse_denial_text(" [other] text", "Bash").is_none());
}

// 5. 주기 루프 ----------------------------------------------------------------
#[tokio::test]
async fn indexer_loop_counts_first_run_and_publishes_later_rows() {
    // 1. 공유 상태 구성
    let mut workspace = prepare_fixture_workspace("loop");
    workspace.config.indexer.interval_secs = 1;
    let (live_tx, mut live_rx) = broadcast::channel(LIVE_CHANNEL_CAPACITY);
    let main_path = workspace
        .projects_dir
        .join("demo-project")
        .join("sess-main-0001.jsonl");
    let ctx = Arc::new(AppContext {
        config: workspace.config,
        store: workspace.store,
        live_tx,
        counters: LiveCounters::default(),
        started_at: Utc::now(),
        admin_token: "test".to_string(),
        power: PowerState::default(),
    });
    let loop_task = tokio::spawn(run_indexer_loop(Arc::clone(&ctx)));

    // 2. 첫 실행 (이벤트 없음)
    for _ in 0..100 {
        if ctx.counters.index_runs.load(Ordering::Relaxed) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(ctx.counters.api_requests.load(Ordering::Relaxed), 7);
    assert_eq!(ctx.counters.tool_calls.load(Ordering::Relaxed), 5);
    assert!(live_rx.try_recv().is_err());

    // 3. 추가 줄 반영 (이벤트 발행)
    let appended_line = "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5-5\",\"id\":\"msg_L\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"x\"}],\"usage\":{\"input_tokens\":1,\"output_tokens\":9}},\"timestamp\":\"2026-11-10T12:30:00.000Z\",\"sessionId\":\"sess-main-0001\"}\n";
    let mut appended = fs::OpenOptions::new()
        .append(true)
        .open(&main_path)
        .expect("추가 열기");
    appended
        .write_all(appended_line.as_bytes())
        .expect("추가 줄");
    drop(appended);
    let received = tokio::time::timeout(Duration::from_secs(10), live_rx.recv())
        .await
        .expect("이벤트 대기")
        .expect("이벤트");
    assert!(matches!(received, LiveEvent::Api(row) if row.request_key == "msg_L"));
    loop_task.abort();
    let _ = loop_task.await;
    drop(ctx);
}

// 6. 크기 역전 재색인 ---------------------------------------------------------
#[test]
fn shrunk_file_below_read_offset_is_reindexed_from_start() {
    // 1. 최초 색인과 커서 조회
    let workspace = prepare_fixture_workspace("shrink");
    index_transcripts_once(&workspace.config, &workspace.store).expect("첫 색인");
    let main_path = workspace
        .projects_dir
        .join("demo-project")
        .join("sess-main-0001.jsonl");
    let path_text = main_path.to_string_lossy().into_owned();
    let mut cursor = workspace
        .store
        .load_index_cursor(&path_text)
        .expect("커서 조회")
        .expect("커서 존재");

    // 2. 발견 크기보다 크고 읽기 위치보다 작게 재작성
    let rewritten = "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5-5\",\"id\":\"msg_S\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"x\"}],\"usage\":{\"input_tokens\":1,\"output_tokens\":9}},\"timestamp\":\"2026-11-10T12:40:00.000Z\",\"sessionId\":\"sess-main-0001\"}\n";
    assert!(cursor.read_offset > rewritten.len() as i64);
    cursor.file_size = 10;
    workspace
        .store
        .save_index_cursor(&cursor)
        .expect("커서 저장");
    fs::write(&main_path, rewritten.as_bytes()).expect("재작성");

    // 3. 재색인 후 새 내용 행 확인
    index_transcripts_once(&workspace.config, &workspace.store).expect("재색인");
    let stored: i64 = workspace
        .store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM api_requests WHERE request_key = 'msg_S'",
                [],
                |row| row.get(0),
            )
        })
        .expect("msg_S");
    assert_eq!(stored, 1);
}
