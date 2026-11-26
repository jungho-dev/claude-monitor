//! net/otlp/tests.rs
//! OTLP 테스트: 고정 JSON 변환, 키 대체 규칙, 서버 형식 검사·gzip·적재, protobuf 해석.

use super::mapping::map_logs_payload;
use super::proto::{decode_logs_request, decode_metrics_request};
use super::run_otlp_server;
use crate::core::model::{DataSource, HookOutcome, LiveEvent};
use crate::net::admin::test_support::{
    build_test_config, build_test_context, post_raw, send_raw, wait_until_listening,
};
use crate::store::Store;
use crate::transcript::test_support::{TempDirGuard, unique_temp_dir};
use flate2::Compression;
use flate2::write::GzEncoder;
use rusqlite::params;
use serde_json::{Value, json};
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

// 2-1. 캐시 쓰기 5분·1시간 속성 -----------------------------------------------
#[test]
fn map_logs_payload_reads_cache_creation_split_attributes() {
    let payload = r#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[
        {"timeUnixNano":"1763200000000000000","attributes":[
            {"key":"event.name","value":{"stringValue":"api_request"}},
            {"key":"session.id","value":{"stringValue":"sess-split"}},
            {"key":"request_id","value":{"stringValue":"req_split"}},
            {"key":"model","value":{"stringValue":"claude-opus-5-5"}},
            {"key":"cache_creation_5m_tokens","value":{"intValue":"700"}},
            {"key":"cache_creation_1h_tokens","value":{"intValue":300}}
        ]},
        {"timeUnixNano":"1763200001000000000","attributes":[
            {"key":"event.name","value":{"stringValue":"api_request"}},
            {"key":"request_id","value":{"stringValue":"req_total"}},
            {"key":"cache_creation_tokens","value":{"intValue":"500"}}
        ]},
        {"timeUnixNano":"1763200002000000000","attributes":[
            {"key":"event.name","value":{"stringValue":"api_request"}},
            {"key":"request_id","value":{"stringValue":"req_both"}},
            {"key":"cache_creation_tokens","value":{"intValue":"2000"}},
            {"key":"cache_creation_5m_tokens","value":{"intValue":"700"}},
            {"key":"cache_creation_1h_tokens","value":{"intValue":"300"}}
        ]}
    ]}]}]}"#;
    let mapping = map_logs_payload(payload.as_bytes()).expect("변환");
    let rows = &mapping.batch.api_requests;
    assert_eq!(rows.len(), 3);

    // 1. 분리 속성만 있으면 합계는 두 값의 합
    assert_eq!(
        (
            rows[0].cache_creation_5m_tokens,
            rows[0].cache_creation_1h_tokens,
            rows[0].cache_creation_tokens
        ),
        (700, 300, 1000)
    );

    // 2. 합계 속성만 있으면 분리는 0 (비용 계산이 합계를 5분으로 본다)
    assert_eq!(
        (
            rows[1].cache_creation_5m_tokens,
            rows[1].cache_creation_1h_tokens,
            rows[1].cache_creation_tokens
        ),
        (0, 0, 500)
    );

    // 3. 합계와 분리가 함께 오면 합계는 더 큰 값 유지
    assert_eq!(
        (
            rows[2].cache_creation_5m_tokens,
            rows[2].cache_creation_1h_tokens,
            rows[2].cache_creation_tokens
        ),
        (700, 300, 2000)
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
    let _cleanup = TempDirGuard(config.data_dir.clone());
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
    let _ = server.await;
}

// 5. 본문 상한과 gzip 폭탄 ----------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otlp_rejects_oversized_and_gzip_bomb() {
    // 1. 서버 기동
    let config = build_test_config("otlp-limit");
    let port = config.ports.otlp;
    let _cleanup = TempDirGuard(config.data_dir.clone());
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
    let _ = server.await;
}

// 6. protobuf 인코더 (테스트 전용) --------------------------------------------
fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value & 0x7f) as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_tag(out: &mut Vec<u8>, field: u32, wire: u8) {
    put_varint(out, (u64::from(field) << 3) | u64::from(wire));
}

fn put_bytes(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
    put_tag(out, field, 2);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn put_key_value(out: &mut Vec<u8>, field: u32, key: &str, any: &[u8]) {
    let mut pair = Vec::new();
    put_bytes(&mut pair, 1, key.as_bytes());
    put_bytes(&mut pair, 2, any);
    put_bytes(out, field, &pair);
}

fn wrap_field(field: u32, inner: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    put_bytes(&mut out, field, inner);
    out
}

// OTLP/JSON AnyValue -> protobuf AnyValue
fn encode_any_value(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(text) = value["stringValue"].as_str() {
        put_bytes(&mut out, 1, text.as_bytes());
    } else if let Some(flag) = value["boolValue"].as_bool() {
        put_tag(&mut out, 2, 0);
        put_varint(&mut out, u64::from(flag));
    } else if !value["intValue"].is_null() {
        let number: i64 = match &value["intValue"] {
            Value::String(text) => text.parse().expect("정수 문자열"),
            other => other.as_i64().expect("정수"),
        };
        put_tag(&mut out, 3, 0);
        put_varint(&mut out, number as u64);
    } else if let Some(number) = value["doubleValue"].as_f64() {
        put_tag(&mut out, 4, 1);
        out.extend_from_slice(&number.to_le_bytes());
    } else if let Some(items) = value["arrayValue"]["values"].as_array() {
        let mut array = Vec::new();
        for item in items {
            put_bytes(&mut array, 1, &encode_any_value(item));
        }
        put_bytes(&mut out, 5, &array);
    } else if let Some(entries) = value["kvlistValue"]["values"].as_array() {
        let mut list = Vec::new();
        for entry in entries {
            let key = entry["key"].as_str().unwrap_or_default();
            put_key_value(&mut list, 1, key, &encode_any_value(&entry["value"]));
        }
        put_bytes(&mut out, 6, &list);
    }
    out
}

fn put_attributes(out: &mut Vec<u8>, field: u32, attributes: &Value) {
    for pair in attributes.as_array().into_iter().flatten() {
        let key = pair["key"].as_str().unwrap_or_default();
        put_key_value(out, field, key, &encode_any_value(&pair["value"]));
    }
}

// OTLP/JSON 로그 요청 -> protobuf (미지 필드·심각도 포함)
fn encode_logs_request(request: &Value) -> Vec<u8> {
    let mut export = Vec::new();
    for resource_logs in request["resourceLogs"].as_array().into_iter().flatten() {
        let mut resource_bytes = Vec::new();
        if !resource_logs["resource"].is_null() {
            let mut resource = Vec::new();
            put_attributes(&mut resource, 1, &resource_logs["resource"]["attributes"]);
            put_bytes(&mut resource_bytes, 1, &resource);
        }
        for scope_logs in resource_logs["scopeLogs"].as_array().into_iter().flatten() {
            let mut scope_bytes = Vec::new();
            if let Some(name) = scope_logs["scope"]["name"].as_str() {
                put_bytes(&mut scope_bytes, 1, &wrap_field(1, name.as_bytes()));
            }
            for record in scope_logs["logRecords"].as_array().into_iter().flatten() {
                let mut encoded = Vec::new();
                if let Some(nanos) = record["timeUnixNano"].as_str() {
                    put_tag(&mut encoded, 1, 1);
                    let nanos: u64 = nanos.parse().expect("나노초");
                    encoded.extend_from_slice(&nanos.to_le_bytes());
                }
                put_tag(&mut encoded, 2, 0);
                put_varint(&mut encoded, 9);
                put_bytes(&mut encoded, 3, b"INFO");
                if !record["body"].is_null() {
                    put_bytes(&mut encoded, 5, &encode_any_value(&record["body"]));
                }
                put_attributes(&mut encoded, 6, &record["attributes"]);
                put_tag(&mut encoded, 99, 0);
                put_varint(&mut encoded, 7);
                put_bytes(&mut encoded, 98, b"unknown");
                put_tag(&mut encoded, 97, 1);
                encoded.extend_from_slice(&[0u8; 8]);
                put_tag(&mut encoded, 96, 5);
                encoded.extend_from_slice(&[0u8; 4]);
                put_bytes(&mut scope_bytes, 2, &encoded);
            }
            put_bytes(&mut resource_bytes, 2, &scope_bytes);
        }
        put_bytes(&mut export, 1, &resource_bytes);
    }
    export
}

// 로그 레코드 1개를 요청 봉투로 감쌀
fn wrap_logs_record(record: &[u8]) -> Vec<u8> {
    let scope_logs = wrap_field(2, record);
    let resource_logs = wrap_field(2, &scope_logs);
    wrap_field(1, &resource_logs)
}

fn gzip_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(bytes).expect("압축");
    encoder.finish().expect("압축 종료")
}

// 7. protobuf 로그 변환 -------------------------------------------------------
#[test]
fn protobuf_logs_map_like_json() {
    for fixture in [MIXED_FIXTURE, FALLBACK_FIXTURE] {
        let request: Value = serde_json::from_str(fixture).expect("고정 JSON");
        let decoded = decode_logs_request(&encode_logs_request(&request)).expect("protobuf 해석");
        let from_json = map_logs_payload(fixture.as_bytes()).expect("JSON 변환");
        let json_bytes = serde_json::to_vec(&decoded).expect("직렬화");
        let from_proto = map_logs_payload(&json_bytes).expect("protobuf 변환");
        assert!(!from_proto.batch.api_requests.is_empty());
        assert_eq!(from_proto.ignored_events, from_json.ignored_events);
        assert_eq!(
            format!("{:?}", from_proto.batch),
            format!("{:?}", from_json.batch)
        );
    }
}

// 8. JSON 값 표현 -------------------------------------------------------------
#[test]
fn decode_logs_request_uses_json_encodings() {
    // 1. 특수 AnyValue 조립
    let mut nan_any = Vec::new();
    put_tag(&mut nan_any, 4, 1);
    nan_any.extend_from_slice(&f64::NAN.to_le_bytes());
    let mut bool_any = Vec::new();
    put_tag(&mut bool_any, 2, 0);
    put_varint(&mut bool_any, 1);
    let mut blob_any = Vec::new();
    put_bytes(&mut blob_any, 7, &[0xff, 0xfe, 0x01, 0x02]);
    let negative_any = encode_any_value(&json!({ "intValue": "-5" }));
    let list_any = encode_any_value(&json!({
        "arrayValue": { "values": [{ "stringValue": "a" }, { "intValue": "2" }] }
    }));
    let map_any = encode_any_value(&json!({
        "kvlistValue": { "values": [{ "key": "k", "value": { "doubleValue": 1.5 } }] }
    }));

    // 2. 로그 레코드 조립
    let mut record = Vec::new();
    put_tag(&mut record, 1, 1);
    record.extend_from_slice(&1_763_200_000_000_000_000u64.to_le_bytes());
    put_tag(&mut record, 11, 1);
    record.extend_from_slice(&1_763_200_000_000_000_001u64.to_le_bytes());
    put_tag(&mut record, 2, 0);
    put_varint(&mut record, 17);
    put_bytes(&mut record, 3, b"ERROR");
    put_bytes(
        &mut record,
        5,
        &encode_any_value(&json!({ "stringValue": "hello" })),
    );
    put_key_value(&mut record, 6, "nan", &nan_any);
    put_key_value(&mut record, 6, "neg", &negative_any);
    put_key_value(&mut record, 6, "flag", &bool_any);
    put_key_value(&mut record, 6, "blob", &blob_any);
    put_key_value(&mut record, 6, "list", &list_any);
    put_key_value(&mut record, 6, "map", &map_any);
    put_tag(&mut record, 7, 0);
    put_varint(&mut record, 3);
    put_tag(&mut record, 8, 5);
    record.extend_from_slice(&1u32.to_le_bytes());
    put_bytes(&mut record, 9, &(1u8..=16).collect::<Vec<u8>>());
    put_bytes(&mut record, 10, &[0xab; 8]);
    put_bytes(&mut record, 12, b"ev");

    // 3. 해석 결과
    let decoded = decode_logs_request(&wrap_logs_record(&record)).expect("해석");
    let expected = json!({
        "resourceLogs": [{
            "scopeLogs": [{
                "logRecords": [{
                    "timeUnixNano": "1763200000000000000",
                    "observedTimeUnixNano": "1763200000000000001",
                    "severityNumber": 17,
                    "severityText": "ERROR",
                    "body": { "stringValue": "hello" },
                    "attributes": [
                        { "key": "nan", "value": { "doubleValue": "NaN" } },
                        { "key": "neg", "value": { "intValue": "-5" } },
                        { "key": "flag", "value": { "boolValue": true } },
                        { "key": "blob", "value": { "bytesValue": "//4BAg==" } },
                        { "key": "list", "value": { "arrayValue": { "values": [
                            { "stringValue": "a" },
                            { "intValue": "2" }
                        ] } } },
                        { "key": "map", "value": { "kvlistValue": { "values": [
                            { "key": "k", "value": { "doubleValue": 1.5 } }
                        ] } } }
                    ],
                    "droppedAttributesCount": 3,
                    "flags": 1,
                    "traceId": "0102030405060708090a0b0c0d0e0f10",
                    "spanId": "abababababababab",
                    "eventName": "ev"
                }]
            }]
        }]
    });
    assert_eq!(decoded, expected);
}

// 9. 지표 Sum 해석 ------------------------------------------------------------
#[test]
fn decode_metrics_request_reads_sum_and_gauge() {
    // 1. Sum (asInt, asDouble 두 점)
    let mut int_point = Vec::new();
    put_key_value(
        &mut int_point,
        7,
        "type",
        &encode_any_value(&json!({ "stringValue": "input" })),
    );
    put_tag(&mut int_point, 2, 1);
    int_point.extend_from_slice(&10u64.to_le_bytes());
    put_tag(&mut int_point, 3, 1);
    int_point.extend_from_slice(&20u64.to_le_bytes());
    put_tag(&mut int_point, 6, 1);
    int_point.extend_from_slice(&(-5i64).to_le_bytes());
    let mut double_point = Vec::new();
    put_tag(&mut double_point, 3, 1);
    double_point.extend_from_slice(&30u64.to_le_bytes());
    put_tag(&mut double_point, 4, 1);
    double_point.extend_from_slice(&0.25f64.to_le_bytes());
    put_tag(&mut double_point, 8, 0);
    put_varint(&mut double_point, 1);
    let mut sum = Vec::new();
    put_bytes(&mut sum, 1, &int_point);
    put_bytes(&mut sum, 1, &double_point);
    put_tag(&mut sum, 2, 0);
    put_varint(&mut sum, 1);
    put_tag(&mut sum, 3, 0);
    put_varint(&mut sum, 1);
    let mut usage = Vec::new();
    put_bytes(&mut usage, 1, b"claude_code.token.usage");
    put_bytes(&mut usage, 3, b"tokens");
    put_bytes(&mut usage, 7, &sum);

    // 2. Gauge·히스토그램 (내용 생략)
    let mut gauge_point = Vec::new();
    put_tag(&mut gauge_point, 3, 1);
    gauge_point.extend_from_slice(&40u64.to_le_bytes());
    put_tag(&mut gauge_point, 4, 1);
    gauge_point.extend_from_slice(&2.0f64.to_le_bytes());
    let mut active = Vec::new();
    put_bytes(&mut active, 1, b"active");
    put_bytes(&mut active, 5, &wrap_field(1, &gauge_point));
    let mut latency = Vec::new();
    put_bytes(&mut latency, 1, b"latency");
    put_bytes(&mut latency, 9, &wrap_field(1, b"opaque"));

    // 3. 요청 봉투
    let mut scope_metrics = Vec::new();
    put_bytes(&mut scope_metrics, 2, &usage);
    put_bytes(&mut scope_metrics, 2, &active);
    put_bytes(&mut scope_metrics, 2, &latency);
    let resource_metrics = wrap_field(2, &scope_metrics);
    let decoded = decode_metrics_request(&wrap_field(1, &resource_metrics)).expect("해석");
    let expected = json!({
        "resourceMetrics": [{
            "scopeMetrics": [{
                "metrics": [
                    {
                        "name": "claude_code.token.usage",
                        "unit": "tokens",
                        "sum": {
                            "dataPoints": [
                                {
                                    "attributes": [
                                        { "key": "type", "value": { "stringValue": "input" } }
                                    ],
                                    "startTimeUnixNano": "10",
                                    "timeUnixNano": "20",
                                    "asInt": "-5"
                                },
                                { "timeUnixNano": "30", "asDouble": 0.25, "flags": 1 }
                            ],
                            "aggregationTemporality": 1,
                            "isMonotonic": true
                        }
                    },
                    {
                        "name": "active",
                        "gauge": { "dataPoints": [{ "timeUnixNano": "40", "asDouble": 2.0 }] }
                    },
                    { "name": "latency", "histogram": {} }
                ]
            }]
        }]
    });
    assert_eq!(decoded, expected);
}

// 10. 잘못된 protobuf ---------------------------------------------------------
#[test]
fn decode_logs_request_rejects_malformed_input() {
    // 1. 절단 (모든 지점에서 패닉 없이 처리)
    let request: Value = serde_json::from_str(MIXED_FIXTURE).expect("고정 JSON");
    let valid = encode_logs_request(&request);
    assert!(decode_logs_request(&valid).is_ok());
    for cut in 0..valid.len() {
        let _ = decode_logs_request(&valid[..cut]);
    }
    assert!(decode_logs_request(&valid[..valid.len() - 1]).is_err());

    // 2. 개별 결함
    let mut huge_length = vec![0x0a];
    huge_length.extend_from_slice(&[0xff; 9]);
    huge_length.push(0x01);
    let mut varint_overflow = vec![0x08];
    varint_overflow.extend_from_slice(&[0xff; 10]);
    varint_overflow.push(0x01);
    let bad_utf8 = wrap_field(1, &wrap_field(3, &[0xff]));
    let malformed: Vec<(&str, Vec<u8>)> = vec![
        ("선언 길이 초과", vec![0x0a, 0xff, 0xff, 0xff, 0x0f]),
        ("길이 64비트 초과", huge_length),
        ("varint 초과", varint_overflow),
        ("필드 번호 0", vec![0x00, 0x01]),
        ("그룹 시작", vec![0x0b]),
        ("잘못된 와이어 타입", vec![0x0f]),
        ("fixed64 절단", vec![0x09, 1, 2, 3]),
        ("tag 절단", vec![0x80]),
        ("UTF-8 오류", bad_utf8),
    ];
    for (label, payload) in &malformed {
        assert!(decode_logs_request(payload).is_err(), "{label}");
    }
}

// 11. 중첩 깊이와 노드 상한 ---------------------------------------------------
#[test]
fn decode_logs_request_bounds_depth_and_nodes() {
    // 1. AnyValue 중첩 (31단 허용, 32단 거부)
    for (levels, accepted) in [(0usize, true), (31, true), (32, false), (500, false)] {
        let mut any = encode_any_value(&json!({ "stringValue": "leaf" }));
        for _ in 0..levels {
            any = wrap_field(5, &wrap_field(1, &any));
        }
        let record = wrap_field(5, &any);
        let result = decode_logs_request(&wrap_logs_record(&record));
        assert_eq!(result.is_ok(), accepted, "levels {levels}");
    }

    // 2. 빈 메시지 대량 반복 (노드 상한)
    let flood: Vec<u8> = [0x0a, 0x00].repeat(2_100_000);
    assert!(decode_logs_request(&flood).is_err());
}

// 12. 서버 protobuf 수신 ------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otlp_server_accepts_protobuf() {
    // 1. 서버 기동
    let config = build_test_config("otlp-proto");
    let port = config.ports.otlp;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let server = tokio::spawn(run_otlp_server(ctx.clone()));
    wait_until_listening(port).await;

    // 2. 평문 protobuf 적재
    let request: Value = serde_json::from_str(MIXED_FIXTURE).expect("고정 JSON");
    let encoded = encode_logs_request(&request);
    let (status, head, body) = send_raw(
        "POST",
        port,
        "/v1/logs",
        &[("Content-Type", "application/x-protobuf")],
        &encoded,
    )
    .await;
    assert_eq!(status, 200);
    assert!(head.contains("content-type: application/x-protobuf"));
    assert!(body.is_empty());
    assert_eq!(ctx.counters.otlp_batches.load(Ordering::Relaxed), 1);
    assert_eq!(ctx.counters.api_requests.load(Ordering::Relaxed), 2);
    assert_eq!(ctx.counters.tool_calls.load(Ordering::Relaxed), 1);
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

    // 3. gzip protobuf 재전송 (중복 제거)
    let (status, _) = post_raw(
        port,
        "/v1/logs",
        &[
            ("Content-Type", "application/x-protobuf"),
            ("Content-Encoding", "gzip"),
        ],
        &gzip_bytes(&encoded),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(ctx.counters.otlp_batches.load(Ordering::Relaxed), 2);
    assert_eq!(ctx.counters.api_requests.load(Ordering::Relaxed), 2);

    // 4. 잘못된 protobuf (절단·손상)
    let truncated = &encoded[..encoded.len() - 3];
    for payload in [truncated, &[0x0b][..]] {
        let (status, _) = post_raw(
            port,
            "/v1/logs",
            &[("Content-Type", "application/x-protobuf")],
            payload,
        )
        .await;
        assert_eq!(status, 400);
    }
    let (status, _) = post_raw(
        port,
        "/v1/logs",
        &[
            ("Content-Type", "application/x-protobuf"),
            ("Content-Encoding", "gzip"),
        ],
        b"not gzip",
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(ctx.counters.otlp_batches.load(Ordering::Relaxed), 2);

    // 5. 지표·트레이스 계수
    for path in ["/v1/metrics", "/v1/traces"] {
        let (status, _, body) = send_raw(
            "POST",
            port,
            path,
            &[("Content-Type", "application/x-protobuf")],
            &[],
        )
        .await;
        assert_eq!(status, 200);
        assert!(body.is_empty());
    }
    assert_eq!(ctx.counters.otlp_batches.load(Ordering::Relaxed), 4);

    // 6. 형식 거부와 gzip 폭탄
    let (status, text) = post_raw(port, "/v1/logs", &[("Content-Type", "text/plain")], b"{}").await;
    assert_eq!(status, 415);
    assert!(text.contains("http/json or http/protobuf"));
    let bomb = gzip_bytes(&vec![0u8; 17 * 1024 * 1024]);
    let (status, _) = post_raw(
        port,
        "/v1/logs",
        &[
            ("Content-Type", "application/x-protobuf"),
            ("Content-Encoding", "gzip"),
        ],
        &bomb,
    )
    .await;
    assert_eq!(status, 413);
    server.abort();
    let _ = server.await;
}

// 13. 신규 이벤트 시험 자료 ---------------------------------------------------
const PRIVATE_EMAIL: &str = "someone@example.invalid";
const PRIVATE_ACCOUNT: &str = "acct-uuid-0000";
const PRIVATE_ORG: &str = "org-id-0000";
const WINDOW_SINCE: &str = "2026-11-18T00:00:00.000Z";
const WINDOW_UNTIL: &str = "2026-11-19T00:00:00.000Z";

fn text_attr(key: &str, text: &str) -> Value {
    json!({ "key": key, "value": { "stringValue": text } })
}

fn int_attr(key: &str, number: i64) -> Value {
    json!({ "key": key, "value": { "intValue": number.to_string() } })
}

fn flag_attr(key: &str, flag: bool) -> Value {
    json!({ "key": key, "value": { "boolValue": flag } })
}

// 표준 속성(사용자·조직 식별 정보와 프롬프트 포함)을 붙인 로그 기록 한 건
fn otel_record(
    session_id: &str,
    event_name: &str,
    sequence: i64,
    stamp: &str,
    mut extra: Vec<Value>,
) -> Value {
    let mut attributes = vec![
        text_attr("event.name", event_name),
        text_attr("session.id", session_id),
        int_attr("event.sequence", sequence),
        text_attr("event.timestamp", stamp),
        text_attr("user.email", PRIVATE_EMAIL),
        text_attr("user.account_uuid", PRIVATE_ACCOUNT),
        text_attr("organization.id", PRIVATE_ORG),
        text_attr("prompt", "SECRET PROMPT TEXT"),
    ];
    attributes.append(&mut extra);
    json!({ "attributes": attributes })
}

// 신규 이벤트 여섯 종류 8건 (컴팩션 3건: 자동 성공, 수동 실패, 다른 세션)
fn build_otel_payload() -> Vec<u8> {
    let long_error = format!("e\n{}", "x".repeat(300));
    let records = vec![
        otel_record(
            "sess-o1",
            "compaction",
            1,
            "2026-11-18T10:00:00Z",
            vec![
                text_attr("agent_id", "ag-1"),
                text_attr("trigger", "auto"),
                flag_attr("success", true),
                int_attr("duration_ms", 4200),
                int_attr("pre_tokens", 180000),
                int_attr("post_tokens", 20000),
            ],
        ),
        otel_record(
            "sess-o1",
            "compaction",
            2,
            "2026-11-18T10:30:00Z",
            vec![
                text_attr("agent_id", "ag-1"),
                text_attr("trigger", "manual"),
                flag_attr("success", false),
                int_attr("pre_tokens", 150000),
                text_attr("error", "boom"),
            ],
        ),
        otel_record(
            "sess-o2",
            "compaction",
            1,
            "2026-11-18T09:00:00Z",
            vec![
                text_attr("trigger", "auto"),
                flag_attr("success", true),
                int_attr("pre_tokens", 100000),
                int_attr("post_tokens", 15000),
            ],
        ),
        otel_record(
            "sess-o1",
            "subagent_completed",
            3,
            "2026-11-18T10:05:00Z",
            vec![
                text_attr("agent_id", "ag-sub"),
                text_attr("parent_agent_id", "ag-1"),
                text_attr("agent_type", "Explore"),
                text_attr("agent.source", "built-in"),
                flag_attr("is_built_in", true),
                flag_attr("is_async", false),
                int_attr("total_tokens", 42000),
                int_attr("total_tool_uses", 7),
                int_attr("duration_ms", 60000),
                text_attr("model", "claude-haiku"),
                text_attr("final_model", "claude-sonnet"),
                flag_attr("model_swapped", true),
            ],
        ),
        otel_record(
            "sess-o1",
            "api_error",
            4,
            "2026-11-18T10:06:00Z",
            vec![
                text_attr("model", "claude-opus"),
                int_attr("status_code", 529),
                int_attr("attempt", 3),
                int_attr("duration_ms", 800),
                text_attr("error", &long_error),
            ],
        ),
        otel_record(
            "sess-o1",
            "api_retries_exhausted",
            5,
            "2026-11-18T10:07:00Z",
            vec![
                int_attr("total_attempts", 10),
                int_attr("total_retry_duration_ms", 90000),
            ],
        ),
        otel_record(
            "sess-o1",
            "claude_code.tool_decision",
            6,
            "2026-11-18T10:08:00Z",
            vec![
                text_attr("tool_name", "Bash"),
                text_attr("tool_use_id", "toolu_1"),
                text_attr("decision", "reject"),
                text_attr("source", "user_reject"),
                text_attr("permission_mode", "default"),
            ],
        ),
        otel_record(
            "sess-o1",
            "permission_mode_changed",
            7,
            "2026-11-18T10:09:00Z",
            vec![
                text_attr("from_mode", "default"),
                text_attr("to_mode", "acceptEdits"),
                text_attr("trigger", "shift_tab"),
            ],
        ),
    ];
    let envelope = json!({
        "resourceLogs": [{ "scopeLogs": [{ "logRecords": records }] }]
    });
    serde_json::to_vec(&envelope).expect("직렬화")
}

// 14. 신규 이벤트 변환 --------------------------------------------------------
#[test]
fn map_logs_payload_maps_otel_events() {
    let mapping = map_logs_payload(&build_otel_payload()).expect("변환");
    assert_eq!(mapping.ignored_events, 0);
    assert_eq!(mapping.batch.otel_events.len(), 8);
    assert_eq!(mapping.batch.sessions.len(), 2);
    let rows = &mapping.batch.otel_events;

    // 1. 컴팩션 (자동 성공, 수동 실패)
    let auto = &rows[0];
    assert_eq!(auto.category, "compaction");
    assert_eq!(auto.status, "auto");
    assert_eq!(auto.tokens_before, Some(180000));
    assert_eq!(auto.tokens_after, Some(20000));
    assert_eq!(auto.duration_ms, Some(4200));
    assert_eq!(auto.detail, "ok");
    assert_eq!(auto.agent_id, "ag-1");
    assert_eq!(auto.ts, "2026-11-18T10:00:00.000Z");
    assert_eq!(auto.event_key, "otlp:sess-o1:ag-1:1:compaction");
    let failed = &rows[1];
    assert_eq!(failed.status, "manual");
    assert_eq!(failed.tokens_after, None);
    assert_eq!(failed.detail, "failed boom");

    // 2. 서브에이전트
    let subagent = &rows[3];
    assert_eq!(subagent.category, "subagent");
    assert_eq!(subagent.name, "Explore");
    assert_eq!(subagent.status, "sync");
    assert_eq!(subagent.tokens_after, Some(42000));
    assert_eq!(subagent.count, Some(7));
    assert_eq!(subagent.duration_ms, Some(60000));
    assert_eq!(
        subagent.detail,
        "model=claude-haiku final=claude-sonnet source=built-in"
    );
    assert_eq!(
        subagent.event_key,
        "otlp:sess-o1:ag-sub:3:subagent_completed"
    );

    // 3. API 오류 (상세 200자 상한, 제어 문자 제거)와 재시도 소진
    let api_error = &rows[4];
    assert_eq!(api_error.category, "api_error");
    assert_eq!(api_error.name, "claude-opus");
    assert_eq!(api_error.status, "529");
    assert_eq!(api_error.count, Some(3));
    assert_eq!(api_error.duration_ms, Some(800));
    assert_eq!(api_error.detail.chars().count(), 200);
    assert!(api_error.detail.starts_with("e xxx"));
    let exhausted = &rows[5];
    assert_eq!(exhausted.category, "api_retries");
    assert_eq!(exhausted.status, "exhausted");
    assert_eq!(exhausted.count, Some(10));
    assert_eq!(exhausted.duration_ms, Some(90000));

    // 4. 권한 결정 (접두사 허용)과 모드 변경
    let decision = &rows[6];
    assert_eq!(decision.category, "tool_decision");
    assert_eq!(decision.name, "Bash");
    assert_eq!(decision.status, "reject");
    assert_eq!(decision.detail, "source=user_reject mode=default");
    assert!(decision.event_key.ends_with(":6:tool_decision:toolu_1"));
    let mode = &rows[7];
    assert_eq!(mode.category, "permission_mode");
    assert_eq!(mode.name, "shift_tab");
    assert_eq!(mode.status, "acceptEdits");
    assert_eq!(mode.detail, "default->acceptEdits");

    // 5. 식별 속성·프롬프트 미보관
    let dumped = format!("{:?}", mapping.batch);
    for secret in [
        PRIVATE_EMAIL,
        PRIVATE_ACCOUNT,
        PRIVATE_ORG,
        "SECRET PROMPT TEXT",
    ] {
        assert!(!dumped.contains(secret), "{secret}");
    }
}

// 15. 신규 이벤트 적재·중복 제거·조회 -----------------------------------------
#[test]
fn otel_events_store_dedupes_and_queries() {
    // 1. 적재와 재전송
    let dir = unique_temp_dir("otel-store");
    let store = Store::open_store(&dir.join("otel.db")).expect("저장소 열기");
    let mapping = map_logs_payload(&build_otel_payload()).expect("변환");
    let first = store.insert_batch(&mapping.batch).expect("첫 적재");
    assert_eq!(first.otel_events, 8);
    let second = store.insert_batch(&mapping.batch).expect("재적재");
    assert_eq!(second.otel_events, 0);

    // 2. 구간·종류·상한 조회 (최신순)
    let all = store
        .otel_events_since(WINDOW_SINCE, WINDOW_UNTIL, None, 100)
        .expect("전체 조회");
    assert_eq!(all.len(), 8);
    assert!(all.windows(2).all(|pair| pair[0].ts >= pair[1].ts));
    assert_eq!(all[0].status, "manual");
    let api_errors = store
        .otel_events_since(WINDOW_SINCE, WINDOW_UNTIL, Some("api_error"), 100)
        .expect("종류 조회");
    assert_eq!(api_errors.len(), 1);
    assert_eq!(api_errors[0].detail.chars().count(), 200);
    let limited = store
        .otel_events_since(WINDOW_SINCE, WINDOW_UNTIL, None, 3)
        .expect("상한 조회");
    assert_eq!(limited.len(), 3);
    let early = store
        .otel_events_since(WINDOW_SINCE, "2026-11-18T09:30:00.000Z", None, 100)
        .expect("이른 구간 조회");
    assert_eq!(early.len(), 1);
    let dumped = format!("{all:?}");
    for secret in [
        PRIVATE_EMAIL,
        PRIVATE_ACCOUNT,
        PRIVATE_ORG,
        "SECRET PROMPT TEXT",
    ] {
        assert!(!dumped.contains(secret), "{secret}");
    }

    // 3. 세션별 컴팩션 집계
    let stats = store
        .compaction_stats(WINDOW_SINCE, WINDOW_UNTIL)
        .expect("컴팩션 집계");
    assert_eq!(stats.len(), 2);
    assert_eq!(stats[0].session_id, "sess-o1");
    assert_eq!(stats[0].count, 2);
    assert_eq!(stats[0].auto_count, 1);
    assert_eq!(stats[0].manual_count, 1);
    assert_eq!(stats[0].failed_count, 1);
    assert_eq!(stats[0].last_trigger, "manual");
    assert_eq!(stats[0].last_pre_tokens, Some(150000));
    assert_eq!(stats[0].last_post_tokens, None);
    assert_eq!(stats[1].session_id, "sess-o2");
    assert_eq!(stats[1].count, 1);
    assert_eq!(stats[1].last_post_tokens, Some(15000));

    // 4. 서브에이전트 실행 (시작 시각 = 종료 - 소요)
    let runs = store
        .subagent_runs(WINDOW_SINCE, WINDOW_UNTIL, 10)
        .expect("서브에이전트 조회");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].agent_type, "Explore");
    assert_eq!(runs[0].mode, "sync");
    assert_eq!(runs[0].started_ts, "2026-11-18T10:04:00.000Z");
    assert_eq!(runs[0].ended_ts, "2026-11-18T10:05:00.000Z");
    assert_eq!(runs[0].total_tokens, Some(42000));
    assert_eq!(runs[0].tool_uses, Some(7));
    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

// 16. 신규 이벤트 서버 수신·실시간 이벤트 -------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otlp_server_stores_and_emits_otel_events() {
    // 1. 서버 기동
    let config = build_test_config("otlp-otel");
    let port = config.ports.otlp;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let mut live_rx = ctx.live_tx.subscribe();
    let server = tokio::spawn(run_otlp_server(ctx.clone()));
    wait_until_listening(port).await;

    // 2. 첫 전송: 8건 저장, 실시간 이벤트 8건
    let payload = build_otel_payload();
    let headers = [("Content-Type", "application/json")];
    let (status, _) = post_raw(port, "/v1/logs", &headers, &payload).await;
    assert_eq!(status, 200);
    for index in 0..8 {
        let event = tokio::time::timeout(Duration::from_secs(2), live_rx.recv())
            .await
            .expect("이벤트 대기")
            .expect("이벤트 수신");
        match event {
            LiveEvent::Otel(row) if index == 0 => assert_eq!(row.category, "compaction"),
            LiveEvent::Otel(_) => {}
            other => panic!("예상 밖 이벤트: {other:?}"),
        }
    }
    let stored: i64 = ctx
        .store
        .with_conn(|conn| conn.query_row("SELECT COUNT(*) FROM otel_events", [], |row| row.get(0)))
        .expect("저장 건수");
    assert_eq!(stored, 8);

    // 3. 재전송: 저장 불변, 실시간 이벤트 없음
    let (status, _) = post_raw(port, "/v1/logs", &headers, &payload).await;
    assert_eq!(status, 200);
    let silent = tokio::time::timeout(Duration::from_millis(300), live_rx.recv()).await;
    assert!(silent.is_err());
    let stored: i64 = ctx
        .store
        .with_conn(|conn| conn.query_row("SELECT COUNT(*) FROM otel_events", [], |row| row.get(0)))
        .expect("저장 건수");
    assert_eq!(stored, 8);
    server.abort();
    let _ = server.await;
}

// 17. MCP 가림 이름 복원 ------------------------------------------------------
#[test]
fn map_logs_payload_restores_mcp_tool_name() {
    // 1. tool_parameters 있음·없음·깨짐
    let records = vec![
        otel_record(
            "sess-m",
            "tool_result",
            1,
            "2026-11-26T01:00:00Z",
            vec![
                text_attr("tool_use_id", "toolu_M1"),
                text_attr("tool_name", "mcp_tool"),
                text_attr(
                    "tool_parameters",
                    r#"{"mcp_server_name":"rust-fs-mcp","mcp_tool_name":"file-read"}"#,
                ),
            ],
        ),
        otel_record(
            "sess-m",
            "tool_result",
            2,
            "2026-11-26T01:00:01Z",
            vec![
                text_attr("tool_use_id", "toolu_M2"),
                text_attr("tool_name", "mcp_tool"),
            ],
        ),
        otel_record(
            "sess-m",
            "tool_result",
            3,
            "2026-11-26T01:00:02Z",
            vec![
                text_attr("tool_use_id", "toolu_M3"),
                text_attr("tool_name", "mcp_tool"),
                text_attr("tool_parameters", "{broken"),
            ],
        ),
    ];
    let envelope = json!({
        "resourceLogs": [{ "scopeLogs": [{ "logRecords": records }] }]
    });
    let payload = serde_json::to_vec(&envelope).expect("직렬화");

    // 2. 실제 이름 조립, 없거나 깨지면 가림 이름 유지
    let mapping = map_logs_payload(&payload).expect("변환");
    let names: Vec<&str> = mapping
        .batch
        .tool_calls
        .iter()
        .map(|row| row.tool_name.as_str())
        .collect();
    assert_eq!(
        names,
        ["mcp__rust-fs-mcp__file-read", "mcp_tool", "mcp_tool"]
    );
}
