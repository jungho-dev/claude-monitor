//! net/otlp/live_tests.rs
//! OTLP 실측 fixture 테스트: Claude Code 2.1.328 http/json·protobuf 수신본 (정제본) 변환 검증.

use super::mapping::{OtlpMapping, map_logs_payload};
use super::run_otlp_server;
use crate::core::model::{DataSource, HookOutcome};
use crate::net::admin::test_support::{
    build_test_config, build_test_context, post_raw, wait_until_listening,
};
use crate::transcript::test_support::TempDirGuard;
use rusqlite::params;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;

macro_rules! live_fixture {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/otlp-live/",
            $name
        ))
    };
}

const JSON_LOGS: [&str; 4] = [
    live_fixture!("logs_json_1.json"),
    live_fixture!("logs_json_2.json"),
    live_fixture!("logs_json_3.json"),
    live_fixture!("logs_json_4.json"),
];
const PROTO_LOGS: [&str; 3] = [
    live_fixture!("logs_proto_1.json"),
    live_fixture!("logs_proto_2.json"),
    live_fixture!("logs_proto_3.json"),
];
const JSON_METRICS: [&str; 3] = [
    live_fixture!("metrics_json_1.json"),
    live_fixture!("metrics_json_2.json"),
    live_fixture!("metrics_json_3.json"),
];
const PROTO_METRICS: [&str; 3] = [
    live_fixture!("metrics_proto_1.json"),
    live_fixture!("metrics_proto_2.json"),
    live_fixture!("metrics_proto_3.json"),
];
const JSON_SESSION: &str = "cccccccc-0000-4000-8000-000000000001";
const MAPPED_EVENTS: [&str; 4] = [
    "api_request",
    "tool_result",
    "hook_execution_complete",
    "tool_decision",
];

// 1. 공통 도우미 --------------------------------------------------------------
fn map_all(fixtures: &[&str]) -> Vec<OtlpMapping> {
    fixtures
        .iter()
        .map(|fixture| map_logs_payload(fixture.as_bytes()).expect("변환"))
        .collect()
}

fn attribute_text(record: &Value, key: &str) -> Option<String> {
    let pair = record["attributes"]
        .as_array()?
        .iter()
        .find(|pair| pair["key"] == key)?;
    pair["value"]["stringValue"].as_str().map(str::to_string)
}

// 이벤트 이름별 레코드 수 (원본 JSON 기준)
fn event_counts(fixtures: &[&str]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for fixture in fixtures {
        let request: Value = serde_json::from_str(fixture).expect("고정 JSON");
        for resource in request["resourceLogs"].as_array().expect("봉투") {
            for scope in resource["scopeLogs"].as_array().expect("범위") {
                for record in scope["logRecords"].as_array().expect("레코드") {
                    let name = attribute_text(record, "event.name").expect("event.name");
                    *counts.entry(name).or_insert(0) += 1;
                }
            }
        }
    }
    counts
}

// 지표 이름별 데이터 점 수 (Sum 기준)
fn metric_points(fixtures: &[&str]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for fixture in fixtures {
        let request: Value = serde_json::from_str(fixture).expect("고정 JSON");
        for resource in request["resourceMetrics"].as_array().expect("봉투") {
            for scope in resource["scopeMetrics"].as_array().expect("범위") {
                for metric in scope["metrics"].as_array().expect("지표") {
                    let name = metric["name"].as_str().expect("이름").to_string();
                    let points = metric["sum"]["dataPoints"].as_array().expect("점").len();
                    *counts.entry(name).or_insert(0) += points;
                }
            }
        }
    }
    counts
}

fn ignored_names(counts: &BTreeMap<String, usize>) -> BTreeMap<&str, usize> {
    counts
        .iter()
        .filter(|(name, _)| !MAPPED_EVENTS.contains(&name.as_str()))
        .map(|(name, count)| (name.as_str(), *count))
        .collect()
}

// 2. 이벤트 목록 --------------------------------------------------------------
#[test]
fn live_json_logs_carry_expected_event_names() {
    let counts = event_counts(&JSON_LOGS);
    let expected: BTreeMap<String, usize> = [
        ("api_request", 2),
        ("assistant_response", 1),
        ("hook_execution_complete", 5),
        ("hook_execution_start", 5),
        ("hook_registered", 26),
        ("managed_settings_resolved", 1),
        ("mcp_server_connection", 6),
        ("plugin_loaded", 6),
        ("tool_decision", 1),
        ("tool_result", 1),
        ("user_prompt", 1),
    ]
    .into_iter()
    .map(|(name, count)| (name.to_string(), count))
    .collect();
    assert_eq!(counts, expected);
}

// 3. 로그 행 변환 -------------------------------------------------------------
#[test]
fn live_json_logs_map_api_tool_and_hook_rows() {
    let mappings = map_all(&JSON_LOGS);
    let api: Vec<_> = mappings
        .iter()
        .flat_map(|mapping| &mapping.batch.api_requests)
        .collect();
    let tools: Vec<_> = mappings
        .iter()
        .flat_map(|mapping| &mapping.batch.tool_calls)
        .collect();
    let hooks: Vec<_> = mappings
        .iter()
        .flat_map(|mapping| &mapping.batch.hook_events)
        .collect();

    // 1. api_request (event.sequence 숫자, query_source 가 에이전트)
    assert_eq!(api.len(), 2);
    assert_eq!(api[0].source, DataSource::Otlp);
    assert_eq!(api[0].request_key, "otlp:req_FAKE000001");
    assert_eq!(api[0].ts, "2026-11-19T11:03:53.721Z");
    assert_eq!(api[0].session_id, JSON_SESSION);
    assert_eq!(api[0].agent, "sdk");
    assert_eq!(api[0].model, "claude-opus-5-5");
    assert_eq!(
        (
            api[0].input_tokens,
            api[0].output_tokens,
            api[0].cache_read_tokens
        ),
        (2, 74, 49477)
    );
    assert_eq!(
        (
            api[0].cache_creation_tokens,
            api[0].cache_creation_5m_tokens,
            api[0].cache_creation_1h_tokens
        ),
        (0, 0, 0)
    );
    assert_eq!(api[0].duration_ms, Some(3743));
    assert_eq!(api[1].request_key, "otlp:req_FAKE000002");
    assert_eq!(
        (
            api[1].input_tokens,
            api[1].output_tokens,
            api[1].cache_read_tokens
        ),
        (2, 109, 49640)
    );
    assert_eq!(api[1].duration_ms, Some(2388));

    // 2. tool_result (agent.name 없음, success 문자열)
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool_use_id, "toolu_FAKE000001");
    assert_eq!(tools[0].tool_name, "Bash");
    assert_eq!(tools[0].ts, "2026-11-19T11:03:53.998Z");
    assert_eq!(tools[0].session_id, JSON_SESSION);
    assert_eq!(tools[0].agent, "");
    assert!(!tools[0].is_error);
    assert_eq!(tools[0].error_kind, "");
    assert_eq!(tools[0].duration_ms, Some(260));

    // 3. hook_execution_complete (이름별 수명 주기 이벤트 전부 허용)
    let summary: Vec<(String, i64, String)> = hooks
        .iter()
        .map(|hook| {
            (
                hook.guard.clone(),
                hook.duration_ms.expect("소요"),
                hook.event_key.clone(),
            )
        })
        .collect();
    let expected: Vec<(String, i64, String)> = [
        ("SessionStart:startup", 137, 35),
        ("UserPromptSubmit", 68, 38),
        ("PreToolUse:Bash", 4, 44),
        ("PostToolBatch", 4, 48),
        ("Stop", 9, 53),
    ]
    .into_iter()
    .map(|(guard, duration, sequence)| {
        (
            guard.to_string(),
            duration,
            format!("otlp:{JSON_SESSION}:{sequence}:{guard}"),
        )
    })
    .collect();
    assert_eq!(summary, expected);
    assert!(hooks.iter().all(|hook| hook.outcome == HookOutcome::Allow));
    assert!(hooks.iter().all(|hook| hook.route == "hook_event"));

    // 4. 세션 하나
    let sessions: BTreeSet<&str> = mappings
        .iter()
        .flat_map(|mapping| &mapping.batch.sessions)
        .map(|row| row.session_id.as_str())
        .collect();
    assert_eq!(sessions, BTreeSet::from([JSON_SESSION]));
}

// 4. 무시 이벤트 --------------------------------------------------------------
#[test]
fn live_json_logs_ignore_lifecycle_events() {
    let counts = event_counts(&JSON_LOGS);
    let ignored = ignored_names(&counts);
    let expected = BTreeMap::from([
        ("assistant_response", 1),
        ("hook_execution_start", 5),
        ("hook_registered", 26),
        ("managed_settings_resolved", 1),
        ("mcp_server_connection", 6),
        ("plugin_loaded", 6),
        ("user_prompt", 1),
    ]);
    assert_eq!(ignored, expected);

    let mapped_total: usize = map_all(&JSON_LOGS)
        .into_iter()
        .map(|mapping| {
            mapping.batch.api_requests.len()
                + mapping.batch.tool_calls.len()
                + mapping.batch.hook_events.len()
                + mapping.batch.otel_events.len()
        })
        .sum();

    // tool_decision 은 otel_events 행으로 적재
    let decisions: Vec<_> = map_all(&JSON_LOGS)
        .into_iter()
        .flat_map(|mapping| mapping.batch.otel_events)
        .collect();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].category, "tool_decision");
    assert_eq!(decisions[0].name, "Bash");
    assert_eq!(decisions[0].status, "accept");
    assert_eq!(decisions[0].detail, "source=config mode=bypassPermissions");
    let ignored_total: u64 = map_all(&JSON_LOGS)
        .iter()
        .map(|mapping| mapping.ignored_events)
        .sum();
    assert_eq!(mapped_total, 9);
    assert_eq!(ignored_total, expected.values().sum::<usize>() as u64);
}

// 5. 비용 속성 ----------------------------------------------------------------
#[test]
fn live_api_request_carries_unmapped_cost_attributes() {
    // 원본에 비용이 있으나 ApiRequestRow 에는 비용 필드가 없음
    let request: Value = serde_json::from_str(JSON_LOGS[2]).expect("고정 JSON");
    let records = request["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
        .as_array()
        .expect("레코드");
    let api = records
        .iter()
        .find(|record| attribute_text(record, "event.name").as_deref() == Some("api_request"))
        .expect("api_request");
    let attributes = api["attributes"].as_array().expect("속성");
    let find = |key: &str| {
        attributes
            .iter()
            .find(|pair| pair["key"] == key)
            .map(|pair| pair["value"].clone())
    };
    assert_eq!(find("cost_usd_micros").expect("micros")["intValue"], 11383);
    assert_eq!(find("cost_usd").expect("usd")["doubleValue"], 0.0113834);
    assert_eq!(find("ttft_ms").expect("ttft")["intValue"], 3290);
}

// 6. protobuf 수신본 ----------------------------------------------------------
#[test]
fn live_protobuf_logs_map_like_json_run() {
    let json_counts = event_counts(&JSON_LOGS);
    let proto_counts = event_counts(&PROTO_LOGS);
    let json_names: BTreeSet<_> = json_counts.keys().collect();
    let proto_names: BTreeSet<_> = proto_counts.keys().collect();
    assert_eq!(proto_names, json_names);

    // 1. 해석된 protobuf 는 intValue 가 문자열, 행 수는 JSON 수신과 같음
    let mappings = map_all(&PROTO_LOGS);
    let api: Vec<_> = mappings
        .iter()
        .flat_map(|mapping| &mapping.batch.api_requests)
        .collect();
    let tools: Vec<_> = mappings
        .iter()
        .flat_map(|mapping| &mapping.batch.tool_calls)
        .collect();
    let hooks: Vec<_> = mappings
        .iter()
        .flat_map(|mapping| &mapping.batch.hook_events)
        .collect();
    assert_eq!((api.len(), tools.len(), hooks.len()), (2, 1, 5));
    assert_eq!(api[0].ts, "2026-11-19T11:04:05.527Z");
    assert_eq!(
        (
            api[0].input_tokens,
            api[0].output_tokens,
            api[0].cache_read_tokens
        ),
        (2, 74, 49477)
    );
    assert_eq!(api[0].agent, "sdk");
    assert_eq!(
        (
            api[1].input_tokens,
            api[1].output_tokens,
            api[1].cache_read_tokens
        ),
        (2, 128, 49640)
    );
    assert_eq!(tools[0].tool_name, "Bash");
    assert_eq!(tools[0].duration_ms, Some(224));
    let proto_session = "cccccccc-0000-4000-8000-000000000002";
    assert_eq!(
        hooks[0].event_key,
        format!("otlp:{proto_session}:35:SessionStart:startup")
    );
    assert_eq!(hooks[0].duration_ms, Some(126));

    // 2. 무시 이벤트도 같은 이름 집합
    let ignored_total: u64 = mappings.iter().map(|mapping| mapping.ignored_events).sum();
    assert_eq!(
        ignored_total,
        ignored_names(&proto_counts).values().sum::<usize>() as u64
    );
}

// 7. 지표 이름 ----------------------------------------------------------------
#[test]
fn live_metrics_cover_session_cost_token_and_active_time() {
    let json_points = metric_points(&JSON_METRICS);
    let expected: BTreeMap<String, usize> = [
        ("claude_code.active_time.total", 1),
        ("claude_code.cost.usage", 2),
        ("claude_code.session.count", 1),
        ("claude_code.token.usage", 8),
    ]
    .into_iter()
    .map(|(name, count)| (name.to_string(), count))
    .collect();
    assert_eq!(json_points, expected);

    let proto_points = metric_points(&PROTO_METRICS);
    let json_names: BTreeSet<_> = json_points.keys().collect();
    let proto_names: BTreeSet<_> = proto_points.keys().collect();
    assert_eq!(proto_names, json_names);
}

// 8. 정제 상태 ----------------------------------------------------------------
#[test]
fn live_fixtures_hold_placeholders_only() {
    let all = JSON_LOGS
        .iter()
        .chain(&PROTO_LOGS)
        .chain(&JSON_METRICS)
        .chain(&PROTO_METRICS);
    for fixture in all {
        assert!(!fixture.contains("C:\\"), "Windows 경로");
        assert!(!fixture.contains("C:/"), "Windows 경로");
        for email in fixture.split("\"user.email\"").skip(1) {
            let value = email.split("\"stringValue\":\"").nth(1).expect("값");
            assert!(value.starts_with("user1@example.invalid"));
        }
    }
}

// 9. 서버 수신·적재 -----------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otlp_server_stores_live_json_batches() {
    // 1. 서버 기동
    let config = build_test_config("otlp-live");
    let port = config.ports.otlp;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let server = tokio::spawn(run_otlp_server(ctx.clone()));
    wait_until_listening(port).await;

    // 2. 로그 4건·지표 3건 전송
    let headers = [("Content-Type", "application/json")];
    for fixture in JSON_LOGS {
        let (status, text) = post_raw(port, "/v1/logs", &headers, fixture.as_bytes()).await;
        assert_eq!((status, text.as_str()), (200, "{}"));
    }
    for fixture in JSON_METRICS {
        let (status, _) = post_raw(port, "/v1/metrics", &headers, fixture.as_bytes()).await;
        assert_eq!(status, 200);
    }
    assert_eq!(ctx.counters.otlp_batches.load(Ordering::Relaxed), 7);
    assert_eq!(ctx.counters.api_requests.load(Ordering::Relaxed), 2);
    assert_eq!(ctx.counters.tool_calls.load(Ordering::Relaxed), 1);

    // 3. 저장 행
    let stored: i64 = ctx
        .store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT output_tokens FROM api_requests WHERE request_key = ?1",
                params!["otlp:req_FAKE000001"],
                |row| row.get(0),
            )
        })
        .expect("저장 행");
    assert_eq!(stored, 74);
    server.abort();
    let _ = server.await;
}
