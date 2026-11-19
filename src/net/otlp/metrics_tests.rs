//! net/otlp/metrics_tests.rs
//! OTLP 지표 테스트: JSON·protobuf 변환, 누적 증분 환산, 개인정보 미보관, 귀속 조회, 서버 적재.

use super::metrics::{
    CumulativeTracker, MAX_POINTS_PER_REQUEST, ingest_metrics_envelope,
    map_metrics_payload as map_staged_metrics,
};
use super::proto::decode_metrics_request;
use super::run_otlp_server;
use crate::net::admin::test_support::{
    build_test_config, build_test_context, post_raw, wait_until_listening,
};
use crate::store::Store;
use crate::store::metrics::{AttributionDimension, METRICS_SCHEMA_SQL, MetricPointRow};
use crate::transcript::test_support::{TempDirGuard, unique_temp_dir};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

// 2025-11-15T09:46:40Z
const BASE_NANOS: u64 = 1_763_200_000_000_000_000;
const BASE_TS: &str = "2025-11-15T09:46:40.000Z";

// 1. JSON 시험 입력 -----------------------------------------------------------
fn string_attr(key: &str, text: &str) -> Value {
    json!({"key": key, "value": {"stringValue": text}})
}

fn json_envelope(resource_attrs: Vec<Value>, metrics: Vec<Value>) -> Value {
    json!({
        "resourceMetrics": [{
            "resource": {"attributes": resource_attrs},
            "scopeMetrics": [{"metrics": metrics}],
        }],
    })
}

fn sum_metric(name: &str, temporality: i64, points: Vec<Value>) -> Value {
    json!({
        "name": name,
        "sum": {"aggregationTemporality": temporality, "dataPoints": points},
    })
}

// 변환 뒤 기준값을 바로 반영하는 변환 전용 시험 도구 (적재 없이 변환 결과만 보는 시험용)
fn map_metrics_payload(envelope: &Value, tracker: &mut CumulativeTracker) -> Vec<MetricPointRow> {
    let mapped = map_staged_metrics(envelope, tracker);
    tracker.last_values.extend(mapped.baselines);
    mapped.rows
}

// 2. protobuf 시험 부호기 -----------------------------------------------------
// opentelemetry-proto 필드 번호: 요청 1=resourceMetrics, 리소스 지표 1=resource 2=scopeMetrics,
// 범위 지표 2=metrics, 지표 1=name 7=sum, Sum 1=dataPoints 2=temporality, 점 3=time 4=double 6=int 7=attributes
fn put_varint(buf: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            buf.push(byte);
            return;
        }
        buf.push(byte | 0x80);
    }
}

fn put_len(buf: &mut Vec<u8>, field: u64, payload: &[u8]) {
    put_varint(buf, (field << 3) | 2);
    put_varint(buf, payload.len() as u64);
    buf.extend_from_slice(payload);
}

fn put_fixed64(buf: &mut Vec<u8>, field: u64, bytes: [u8; 8]) {
    put_varint(buf, (field << 3) | 1);
    buf.extend_from_slice(&bytes);
}

fn proto_attr(key: &str, text: &str) -> Vec<u8> {
    let mut any_value = Vec::new();
    put_len(&mut any_value, 1, text.as_bytes());
    let mut pair = Vec::new();
    put_len(&mut pair, 1, key.as_bytes());
    put_len(&mut pair, 2, &any_value);
    pair
}

enum ProtoValue {
    Int(i64),
    Double(f64),
}

fn proto_point(attrs: &[(&str, &str)], time_nanos: u64, value: ProtoValue) -> Vec<u8> {
    let mut point = Vec::new();
    for (key, text) in attrs {
        put_len(&mut point, 7, &proto_attr(key, text));
    }
    put_fixed64(&mut point, 3, time_nanos.to_le_bytes());
    match value {
        ProtoValue::Int(number) => put_fixed64(&mut point, 6, number.to_le_bytes()),
        ProtoValue::Double(number) => put_fixed64(&mut point, 4, number.to_le_bytes()),
    }
    point
}

fn proto_request(
    resource_attrs: &[(&str, &str)],
    name: &str,
    temporality: u64,
    points: &[Vec<u8>],
) -> Vec<u8> {
    let mut sum = Vec::new();
    for point in points {
        put_len(&mut sum, 1, point);
    }
    put_varint(&mut sum, 2 << 3);
    put_varint(&mut sum, temporality);
    let mut metric = Vec::new();
    put_len(&mut metric, 1, name.as_bytes());
    put_len(&mut metric, 7, &sum);
    let mut scope_metrics = Vec::new();
    put_len(&mut scope_metrics, 2, &metric);
    let mut resource = Vec::new();
    for (key, text) in resource_attrs {
        put_len(&mut resource, 1, &proto_attr(key, text));
    }
    let mut resource_metrics = Vec::new();
    put_len(&mut resource_metrics, 1, &resource);
    put_len(&mut resource_metrics, 2, &scope_metrics);
    let mut request = Vec::new();
    put_len(&mut request, 1, &resource_metrics);
    request
}

// 3. JSON 변환 ----------------------------------------------------------------
#[test]
fn map_metrics_payload_maps_sum_and_gauge_points() {
    // 1. 입력 (비용·토큰·세션 수·게이지, 히스토그램·타 접두사는 제외 대상)
    let attrs_cost = vec![
        string_attr("model", "claude-opus-4-1"),
        string_attr("skill.name", "commit"),
        string_attr("session.id", "s1"),
    ];
    let envelope = json_envelope(
        vec![],
        vec![
            sum_metric(
                "claude_code.cost.usage",
                1,
                vec![json!({
                    "timeUnixNano": BASE_NANOS.to_string(),
                    "asDouble": 0.25,
                    "attributes": attrs_cost,
                })],
            ),
            sum_metric(
                "claude_code.token.usage",
                1,
                vec![json!({
                    "timeUnixNano": BASE_NANOS.to_string(),
                    "asInt": "120",
                    "attributes": [
                        string_attr("type", "cacheRead"),
                        string_attr("mcp_server.name", "github"),
                        string_attr("mcp_tool.name", "create_issue"),
                        string_attr("plugin.name", "pr-tools"),
                        string_attr("agent.name", "reviewer"),
                        string_attr("query_source", "subagent"),
                        string_attr("session.id", "s1"),
                    ],
                })],
            ),
            sum_metric(
                "claude_code.session.count",
                1,
                vec![json!({"timeUnixNano": BASE_NANOS, "asInt": 1})],
            ),
            json!({"name": "claude_code.active_time.total", "histogram": {}}),
            sum_metric("other.metric", 1, vec![json!({"asInt": "9"})]),
            json!({
                "name": "claude_code.cost.usage",
                "gauge": {"dataPoints": [{"timeUnixNano": BASE_NANOS.to_string(), "asDouble": 0.1}]},
            }),
        ],
    );

    // 2. 변환 결과
    let rows = map_metrics_payload(&envelope, &mut CumulativeTracker::default());
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows[0],
        MetricPointRow {
            ts: BASE_TS.to_string(),
            session_id: "s1".to_string(),
            name: "cost.usage".to_string(),
            model: "claude-opus-4-1".to_string(),
            skill_name: "commit".to_string(),
            value: 0.25,
            temporality: "delta".to_string(),
            ..MetricPointRow::default()
        }
    );
    assert_eq!(rows[1].name, "token.usage");
    assert_eq!(rows[1].token_type, "cacheRead");
    assert_eq!(rows[1].value, 120.0);
    assert_eq!(rows[1].mcp_server_name, "github");
    assert_eq!(rows[1].mcp_tool_name, "create_issue");
    assert_eq!(rows[1].plugin_name, "pr-tools");
    assert_eq!(rows[1].agent_name, "reviewer");
    assert_eq!(rows[1].query_source, "subagent");
    assert_eq!(rows[2].name, "session.count");
    assert_eq!(rows[2].value, 1.0);
    assert_eq!(rows[3].temporality, "gauge");
}

// 4. 누적 증분 환산 -----------------------------------------------------------
#[test]
fn map_metrics_payload_converts_cumulative_to_deltas() {
    let build = |session: &str, total: f64| {
        json_envelope(
            vec![],
            vec![sum_metric(
                "claude_code.cost.usage",
                2,
                vec![json!({
                    "timeUnixNano": BASE_NANOS.to_string(),
                    "asDouble": total,
                    "attributes": [
                        string_attr("session.id", session),
                        string_attr("skill.name", "commit"),
                    ],
                })],
            )],
        )
    };
    let mut tracker = CumulativeTracker::default();

    // 1. 첫 표본은 기준만 저장
    assert!(map_metrics_payload(&build("c1", 10.0), &mut tracker).is_empty());

    // 2. 증가분만 저장
    let rows = map_metrics_payload(&build("c1", 25.0), &mut tracker);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].value, 15.0);
    assert_eq!(rows[0].temporality, "delta");

    // 3. 감소는 초기화로 보고 값 전체, 변화 없음은 행 없음
    let rows = map_metrics_payload(&build("c1", 5.0), &mut tracker);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].value, 5.0);
    assert!(map_metrics_payload(&build("c1", 5.0), &mut tracker).is_empty());

    // 4. 다른 계열은 독립 기준
    assert!(map_metrics_payload(&build("c2", 100.0), &mut tracker).is_empty());
    assert_eq!(tracker.last_values.len(), 2);
}

// 5. 요청당 점 수 상한 --------------------------------------------------------
#[test]
fn map_metrics_payload_caps_points_per_request() {
    let points: Vec<Value> = (0..MAX_POINTS_PER_REQUEST + 10)
        .map(|index| json!({"timeUnixNano": BASE_NANOS.to_string(), "asInt": index.to_string()}))
        .collect();
    let envelope = json_envelope(
        vec![],
        vec![sum_metric("claude_code.cost.usage", 1, points)],
    );
    let rows = map_metrics_payload(&envelope, &mut CumulativeTracker::default());
    assert_eq!(rows.len(), MAX_POINTS_PER_REQUEST);
}

// 6. 개인정보 미보관 ----------------------------------------------------------
#[test]
fn metric_points_never_store_identity_attributes() {
    // 1. 리소스·점 양쪽에 식별 속성을 실은 입력
    let identity = vec![
        string_attr("user.email", "secret.person@example.com"),
        string_attr("user.account_uuid", "uuid-secret-1111"),
        string_attr("user.account_id", "acct-secret-2222"),
        string_attr("user.id", "hash-secret-3333"),
        string_attr("organization.id", "org-secret-4444"),
    ];
    let mut point_attrs = identity.clone();
    point_attrs.push(string_attr("session.id", "priv-1"));
    point_attrs.push(string_attr("skill.name", "commit"));
    let envelope = json_envelope(
        identity,
        vec![sum_metric(
            "claude_code.cost.usage",
            1,
            vec![json!({
                "timeUnixNano": BASE_NANOS.to_string(),
                "asDouble": 0.5,
                "attributes": point_attrs,
            })],
        )],
    );
    let rows = map_metrics_payload(&envelope, &mut CumulativeTracker::default());
    assert_eq!(rows.len(), 1);

    // 2. 저장 후 모든 문자열 열에 비밀 값이 없음
    let dir = unique_temp_dir("metrics-privacy");
    let store = Store::open_store(&dir.join("privacy.db")).expect("저장소 열기");
    assert_eq!(store.insert_metric_points(&rows).expect("적재"), 1);
    let dumped: String = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT ts || '|' || session_id || '|' || name || '|' || token_type || '|' || model \
                 || '|' || query_source || '|' || agent_name || '|' || skill_name || '|' || plugin_name \
                 || '|' || mcp_server_name || '|' || mcp_tool_name || '|' || temporality \
                 FROM metric_points",
                [],
                |row| row.get(0),
            )
        })
        .expect("행 조회");
    assert!(dumped.contains("priv-1"));
    assert!(dumped.contains("commit"));
    for secret in ["secret", "example.com", "uuid", "acct", "hash", "org-"] {
        assert!(!dumped.contains(secret), "{secret} 유출: {dumped}");
    }
    assert!(!format!("{rows:?}").contains("secret"));
    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

// 7. protobuf 변환 ------------------------------------------------------------
#[test]
fn protobuf_payload_maps_like_json() {
    // 1. 증분 비용 (asDouble)·토큰 (asInt) 점
    let cost = proto_request(
        &[("user.email", "secret.person@example.com")],
        "claude_code.cost.usage",
        1,
        &[proto_point(
            &[
                ("model", "m1"),
                ("skill.name", "commit"),
                ("session.id", "p1"),
            ],
            BASE_NANOS,
            ProtoValue::Double(0.5),
        )],
    );
    let envelope = decode_metrics_request(&cost).expect("비용 해석");
    let rows = map_metrics_payload(&envelope, &mut CumulativeTracker::default());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].ts, BASE_TS);
    assert_eq!(rows[0].name, "cost.usage");
    assert_eq!(rows[0].skill_name, "commit");
    assert_eq!(rows[0].model, "m1");
    assert_eq!(rows[0].session_id, "p1");
    assert_eq!(rows[0].value, 0.5);
    assert!(!format!("{rows:?}").contains("secret"));
    let tokens = proto_request(
        &[],
        "claude_code.token.usage",
        1,
        &[proto_point(
            &[("type", "input"), ("session.id", "p1")],
            BASE_NANOS,
            ProtoValue::Int(42),
        )],
    );
    let envelope = decode_metrics_request(&tokens).expect("토큰 해석");
    let rows = map_metrics_payload(&envelope, &mut CumulativeTracker::default());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].token_type, "input");
    assert_eq!(rows[0].value, 42.0);

    // 2. 누적 (첫 표본 제외, 이후 증분)
    let mut tracker = CumulativeTracker::default();
    let cumulative = |total: i64| {
        let payload = proto_request(
            &[],
            "claude_code.token.usage",
            2,
            &[proto_point(
                &[("type", "output"), ("session.id", "p2")],
                BASE_NANOS,
                ProtoValue::Int(total),
            )],
        );
        decode_metrics_request(&payload).expect("누적 해석")
    };
    assert!(map_metrics_payload(&cumulative(100), &mut tracker).is_empty());
    let rows = map_metrics_payload(&cumulative(130), &mut tracker);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].value, 30.0);
}

// 8. 귀속 조회 ----------------------------------------------------------------
#[test]
fn metric_attribution_groups_by_dimension() {
    // 1. 시드 (구간 안 비용·토큰, 구간 밖, 게이지, 차원 없음, 타 지표)
    let dir = unique_temp_dir("metrics-attr");
    let store = Store::open_store(&dir.join("attr.db")).expect("저장소 열기");
    let point = |ts: &str, name: &str, token_type: &str, skill: &str, model: &str, value: f64| {
        MetricPointRow {
            ts: ts.to_string(),
            name: name.to_string(),
            token_type: token_type.to_string(),
            skill_name: skill.to_string(),
            model: model.to_string(),
            value,
            temporality: "delta".to_string(),
            ..MetricPointRow::default()
        }
    };
    let mut gauge = point(
        "2026-11-10T10:10:00.000Z",
        "cost.usage",
        "",
        "commit",
        "sonnet",
        9.0,
    );
    gauge.temporality = "gauge".to_string();
    let mut github = point(
        "2026-11-10T10:20:00.000Z",
        "cost.usage",
        "",
        "",
        "sonnet",
        0.4,
    );
    github.mcp_server_name = "github".to_string();
    let rows = vec![
        point(
            "2026-11-10T10:00:00.000Z",
            "cost.usage",
            "",
            "commit",
            "sonnet",
            0.3,
        ),
        point(
            "2026-11-10T10:30:00.000Z",
            "cost.usage",
            "",
            "commit",
            "sonnet",
            0.2,
        ),
        point(
            "2026-11-10T10:00:00.000Z",
            "token.usage",
            "input",
            "commit",
            "sonnet",
            100.0,
        ),
        point(
            "2026-11-10T10:00:00.000Z",
            "token.usage",
            "output",
            "commit",
            "sonnet",
            50.0,
        ),
        point(
            "2026-11-10T10:00:00.000Z",
            "token.usage",
            "cacheRead",
            "commit",
            "sonnet",
            1000.0,
        ),
        point(
            "2026-11-10T10:00:00.000Z",
            "token.usage",
            "cacheCreation",
            "commit",
            "sonnet",
            10.0,
        ),
        point(
            "2026-11-10T10:05:00.000Z",
            "cost.usage",
            "",
            "review",
            "sonnet",
            0.1,
        ),
        point(
            "2026-11-10T10:05:00.000Z",
            "token.usage",
            "input",
            "review",
            "sonnet",
            7.0,
        ),
        point(
            "2026-11-10T10:06:00.000Z",
            "cost.usage",
            "",
            "",
            "opus",
            2.0,
        ),
        point(
            "2026-11-10T12:00:00.000Z",
            "cost.usage",
            "",
            "commit",
            "sonnet",
            5.0,
        ),
        point(
            "2026-11-10T10:07:00.000Z",
            "session.count",
            "",
            "commit",
            "sonnet",
            3.0,
        ),
        gauge,
        github,
    ];
    assert_eq!(store.insert_metric_points(&rows).expect("적재"), 13);

    // 2. 스킬 차원 (이름 없는 점·게이지·구간 밖·타 지표 제외, 비용 큰 순)
    let since = "2026-11-10T10:00:00.000Z";
    let until = "2026-11-10T11:00:00.000Z";
    let by_skill = store
        .query_metric_attribution(since, until, AttributionDimension::Skill)
        .expect("스킬 귀속");
    assert_eq!(by_skill.len(), 2);
    assert_eq!(by_skill[0].name, "commit");
    assert!((by_skill[0].cost_usd - 0.5).abs() < 1e-9);
    assert_eq!(by_skill[0].input_tokens, 100);
    assert_eq!(by_skill[0].output_tokens, 50);
    assert_eq!(by_skill[0].cache_read_tokens, 1000);
    assert_eq!(by_skill[0].cache_creation_tokens, 10);
    assert_eq!(by_skill[1].name, "review");
    assert!((by_skill[1].cost_usd - 0.1).abs() < 1e-9);
    assert_eq!(by_skill[1].input_tokens, 7);
    assert_eq!(by_skill[1].output_tokens, 0);

    // 3. 모델·MCP 서버 차원
    let by_model = store
        .query_metric_attribution(since, until, AttributionDimension::Model)
        .expect("모델 귀속");
    assert_eq!(by_model.len(), 2);
    assert_eq!(by_model[0].name, "opus");
    assert!((by_model[0].cost_usd - 2.0).abs() < 1e-9);
    assert_eq!(by_model[1].name, "sonnet");
    assert!((by_model[1].cost_usd - 1.0).abs() < 1e-9);
    let by_server = store
        .query_metric_attribution(since, until, AttributionDimension::McpServer)
        .expect("MCP 귀속");
    assert_eq!(by_server.len(), 1);
    assert_eq!(by_server[0].name, "github");
    assert!((by_server[0].cost_usd - 0.4).abs() < 1e-9);
    assert!(
        store
            .query_metric_attribution(since, until, AttributionDimension::Plugin)
            .expect("플러그인 귀속")
            .is_empty()
    );

    // 4. 직렬화 형태와 차원 해석
    let encoded = serde_json::to_value(&by_skill[1]).expect("직렬화");
    assert_eq!(encoded["name"], "review");
    assert_eq!(encoded["input_tokens"], 7);
    assert_eq!(
        AttributionDimension::parse("mcp_server"),
        Some(AttributionDimension::McpServer)
    );
    assert_eq!(AttributionDimension::parse("unknown"), None);
    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

// 8-1. 적재 실패 뒤 재전송 ----------------------------------------------------
#[test]
fn failed_insert_keeps_baseline_so_resend_keeps_delta() {
    let dir = unique_temp_dir("metrics-resend");
    let store = Store::open_store(&dir.join("resend.db")).expect("저장소 열기");
    let build = |total: f64| {
        json_envelope(
            vec![],
            vec![sum_metric(
                "claude_code.cost.usage",
                2,
                vec![json!({
                    "timeUnixNano": BASE_NANOS.to_string(),
                    "asDouble": total,
                    "attributes": [string_attr("session.id", "resend-1")],
                })],
            )],
        )
    };
    let mut tracker = CumulativeTracker::default();

    // 1. 첫 표본은 기준만 저장
    let seeded = ingest_metrics_envelope(&store, &mut tracker, &build(10.0)).expect("기준 표본");
    assert_eq!(seeded, 0);

    // 2. 적재 실패 (표 제거) 는 오류이고 기준값은 그대로
    store
        .with_conn(|conn| conn.execute_batch("DROP TABLE metric_points"))
        .expect("표 제거");
    assert!(ingest_metrics_envelope(&store, &mut tracker, &build(25.0)).is_err());
    assert_eq!(
        tracker.last_values.values().copied().collect::<Vec<f64>>(),
        vec![10.0]
    );

    // 3. 복구 뒤 같은 누적값 재전송은 증분 15 보존
    store
        .with_conn(|conn| conn.execute_batch(METRICS_SCHEMA_SQL))
        .expect("표 복원");
    let resent = ingest_metrics_envelope(&store, &mut tracker, &build(25.0)).expect("재전송");
    assert_eq!(resent, 1);
    let stored: f64 = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM metric_points WHERE session_id = 'resend-1'",
                [],
                |row| row.get(0),
            )
        })
        .expect("저장 행");
    assert_eq!(stored, 15.0);

    // 4. 커밋 뒤 같은 값 재전송은 행 없음
    let again = ingest_metrics_envelope(&store, &mut tracker, &build(25.0)).expect("다시 재전송");
    assert_eq!(again, 0);
    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

// 8-2. 증분 묶음 재전송 중복 방지 ---------------------------------------------
#[test]
fn resent_delta_batch_is_not_inserted_twice() {
    let dir = unique_temp_dir("metrics-dedup");
    let store = Store::open_store(&dir.join("dedup.db")).expect("저장소 열기");
    let point = |name: &str, token_type: &str, value: f64| MetricPointRow {
        ts: BASE_TS.to_string(),
        session_id: "dedup-1".to_string(),
        name: name.to_string(),
        token_type: token_type.to_string(),
        value,
        temporality: "delta".to_string(),
        ..MetricPointRow::default()
    };
    let batch = vec![
        point("cost.usage", "", 0.5),
        point("token.usage", "input", 10.0),
        point("token.usage", "output", 20.0),
    ];

    // 1. 재전송은 신규 0건, 저장 행은 그대로 3건
    assert_eq!(store.insert_metric_points(&batch).expect("첫 적재"), 3);
    assert_eq!(store.insert_metric_points(&batch).expect("재전송"), 0);
    let count: i64 = store
        .with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM metric_points", [], |row| row.get(0))
        })
        .expect("행 수");
    assert_eq!(count, 3);

    // 2. 식별 속성이 다른 점 (다른 시각·다른 토큰 종류) 은 별개 행
    let mut later = point("cost.usage", "", 0.5);
    later.ts = "2025-11-15T09:46:41.000Z".to_string();
    let other_type = point("token.usage", "cacheRead", 5.0);
    assert_eq!(
        store
            .insert_metric_points(&[later, other_type])
            .expect("별개 점"),
        2
    );
    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

// 9. 서버 수신·적재 -----------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otlp_server_stores_metrics_and_rejects_bad_payloads() {
    // 1. 서버 기동
    let config = build_test_config("otlp-metrics");
    let port = config.ports.otlp;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let server = tokio::spawn(run_otlp_server(ctx.clone()));
    wait_until_listening(port).await;
    let proto_header = [("Content-Type", "application/x-protobuf")];

    // 2. protobuf 증분 비용 적재
    let delta = proto_request(
        &[],
        "claude_code.cost.usage",
        1,
        &[proto_point(
            &[("skill.name", "commit"), ("session.id", "srv-delta")],
            BASE_NANOS,
            ProtoValue::Double(0.75),
        )],
    );
    let (status, text) = post_raw(port, "/v1/metrics", &proto_header, &delta).await;
    assert_eq!((status, text.as_str()), (200, ""));
    assert_eq!(ctx.counters.otlp_batches.load(Ordering::Relaxed), 1);
    let stored: f64 = ctx
        .store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM metric_points WHERE session_id = 'srv-delta' AND skill_name = 'commit'",
                [],
                |row| row.get(0),
            )
        })
        .expect("저장 행");
    assert_eq!(stored, 0.75);

    // 3. JSON 누적 (첫 요청은 행 없음, 둘째 요청은 증분)
    let cumulative = |total: f64| {
        json_envelope(
            vec![],
            vec![sum_metric(
                "claude_code.token.usage",
                2,
                vec![json!({
                    "timeUnixNano": BASE_NANOS.to_string(),
                    "asInt": total.to_string(),
                    "attributes": [
                        string_attr("type", "input"),
                        string_attr("session.id", "srv-cumulative"),
                    ],
                })],
            )],
        )
        .to_string()
    };
    let json_header = [("Content-Type", "application/json")];
    for total in [100.0, 160.0] {
        let (status, text) = post_raw(
            port,
            "/v1/metrics",
            &json_header,
            cumulative(total).as_bytes(),
        )
        .await;
        assert_eq!((status, text.as_str()), (200, "{}"));
    }
    let deltas: Vec<f64> = ctx
        .store
        .with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT value FROM metric_points WHERE session_id = 'srv-cumulative'")?;
            stmt.query_map([], |row| row.get(0))?.collect()
        })
        .expect("증분 조회");
    assert_eq!(deltas, vec![60.0]);

    // 4. 잘못된 본문은 400, 계수 불변
    let (status, _) = post_raw(port, "/v1/metrics", &json_header, b"{").await;
    assert_eq!(status, 400);
    let (status, _) = post_raw(port, "/v1/metrics", &proto_header, &[0xff]).await;
    assert_eq!(status, 400);
    assert_eq!(ctx.counters.otlp_batches.load(Ordering::Relaxed), 3);
    server.abort();
    let _ = server.await;
}
