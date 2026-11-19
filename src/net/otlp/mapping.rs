//! net/otlp/mapping.rs
//! OTLP http/json 로그 봉투 해석과 기록 행 변환 (속성 일부만 취함, 본문·프롬프트 미보관).

use crate::core::model::{
    ApiRequestRow, DataSource, HookEventRow, HookOutcome, IngestBatch, OtelEventRow, SessionRow,
    ToolCallRow,
};
use anyhow::Context;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

// 상세 문구 상한 (문자 수)
const DETAIL_MAX_CHARS: usize = 200;

// 1. 변환 결과 ----------------------------------------------------------------
#[derive(Debug, Default)]
pub struct OtlpMapping {
    pub batch: IngestBatch,
    pub ignored_events: u64,
}

// 2. 로그 봉투 형태 -----------------------------------------------------------
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct LogsEnvelope {
    resource_logs: Vec<ResourceLogs>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ResourceLogs {
    scope_logs: Vec<ScopeLogs>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ScopeLogs {
    log_records: Vec<LogRecord>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct LogRecord {
    time_unix_nano: Value,
    body: Option<AnyValue>,
    attributes: Vec<KeyValue>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct KeyValue {
    key: String,
    value: AnyValue,
}

// intValue 는 JSON 문자열로도 도착
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AnyValue {
    string_value: Option<String>,
    int_value: Option<Value>,
    double_value: Option<Value>,
    bool_value: Option<bool>,
}

// 3. 이벤트 공통 필드 ---------------------------------------------------------
struct EventFields {
    attrs: HashMap<String, String>,
    session_id: String,
    ts: String,
    sequence: String,
}

// 4. 로그 변환 ----------------------------------------------------------------
pub fn map_logs_payload(payload: &[u8]) -> anyhow::Result<OtlpMapping> {
    let envelope: LogsEnvelope =
        serde_json::from_slice(payload).context("OTLP 로그 JSON 형식 오류")?;
    let mut mapping = OtlpMapping::default();
    let mut session_spans: BTreeMap<String, (String, String)> = BTreeMap::new();

    for record in envelope
        .resource_logs
        .iter()
        .flat_map(|resource| &resource.scope_logs)
        .flat_map(|scope| &scope.log_records)
    {
        // 1. 이벤트 이름 (event.name 우선, 본문 대체)
        let attrs = flatten_attributes(&record.attributes);
        let raw_name = attrs
            .get("event.name")
            .cloned()
            .or_else(|| {
                record
                    .body
                    .as_ref()
                    .and_then(|body| body.string_value.clone())
            })
            .unwrap_or_default();
        let event_name = raw_name
            .strip_prefix("claude_code.")
            .unwrap_or(&raw_name)
            .to_string();

        // 2. 공통 필드
        let fields = EventFields {
            session_id: attrs.get("session.id").cloned().unwrap_or_default(),
            ts: resolve_event_ts(&attrs, &record.time_unix_nano),
            sequence: attrs.get("event.sequence").cloned().unwrap_or_default(),
            attrs,
        };

        // 3. 이벤트별 변환
        let mapped = match event_name.as_str() {
            "api_request" => {
                mapping.batch.api_requests.push(map_api_request(&fields));
                true
            }
            "tool_result" => match map_tool_result(&fields) {
                Some(row) => {
                    mapping.batch.tool_calls.push(row);
                    true
                }
                None => false,
            },
            "hook_execution_complete" => {
                mapping.batch.hook_events.push(map_hook_complete(&fields));
                true
            }
            other => match map_otel_event(other, &fields) {
                Some(row) => {
                    mapping.batch.otel_events.push(row);
                    true
                }
                None => false,
            },
        };
        if !mapped {
            mapping.ignored_events += 1;
            continue;
        }

        // 4. 세션 구간 누적
        if !fields.session_id.is_empty() {
            let span = session_spans
                .entry(fields.session_id.clone())
                .or_insert_with(|| (fields.ts.clone(), fields.ts.clone()));
            if fields.ts < span.0 {
                span.0 = fields.ts.clone();
            }
            if fields.ts > span.1 {
                span.1 = fields.ts.clone();
            }
        }
    }

    mapping.batch.sessions = session_spans
        .into_iter()
        .map(|(session_id, (first_ts, last_ts))| SessionRow {
            session_id,
            project: String::new(),
            first_ts,
            last_ts,
        })
        .collect();
    Ok(mapping)
}

// 5. 속성 평탄화 --------------------------------------------------------------
fn flatten_attributes(attributes: &[KeyValue]) -> HashMap<String, String> {
    let mut flat = HashMap::with_capacity(attributes.len());
    for pair in attributes {
        let text = if let Some(text) = &pair.value.string_value {
            text.clone()
        } else if let Some(int_value) = &pair.value.int_value {
            match int_value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            }
        } else if let Some(double_value) = &pair.value.double_value {
            match double_value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            }
        } else if let Some(flag) = pair.value.bool_value {
            flag.to_string()
        } else {
            continue;
        };
        flat.insert(pair.key.clone(), text);
    }
    flat
}

// 6. 정수 속성 읽기 -----------------------------------------------------------
fn read_i64_attr(attrs: &HashMap<String, String>, key: &str) -> Option<i64> {
    let text = attrs.get(key)?;
    match text.parse::<i64>() {
        Ok(parsed) => Some(parsed),
        Err(_) => text.parse::<f64>().ok().map(|parsed| parsed.round() as i64),
    }
}

// 7. 이벤트 시각 결정 ---------------------------------------------------------
fn resolve_event_ts(attrs: &HashMap<String, String>, time_unix_nano: &Value) -> String {
    // 1. event.timestamp
    let stamped = attrs
        .get("event.timestamp")
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok());
    if let Some(parsed) = stamped {
        return parsed
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true);
    }

    // 2. timeUnixNano
    let nanos = match time_unix_nano {
        Value::String(text) => text.parse::<i64>().ok(),
        Value::Number(number) => number.as_i64(),
        _ => None,
    };
    if let Some(nanos) = nanos.filter(|value| *value > 0) {
        return DateTime::from_timestamp_nanos(nanos).to_rfc3339_opts(SecondsFormat::Millis, true);
    }

    // 3. 수신 시각
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

// 8. api_request 변환 ---------------------------------------------------------
fn map_api_request(fields: &EventFields) -> ApiRequestRow {
    let attrs = &fields.attrs;
    let id_source = attrs
        .get("request_id")
        .filter(|text| !text.is_empty())
        .or_else(|| {
            attrs
                .get("client_request_id")
                .filter(|text| !text.is_empty())
        });
    let request_key = match id_source {
        Some(id) => format!("otlp:{id}"),
        None => format!(
            "otlp:{}:{}:{}",
            fields.session_id, fields.ts, fields.sequence
        ),
    };
    let agent = attrs
        .get("agent.name")
        .filter(|text| !text.is_empty())
        .or_else(|| attrs.get("query_source"))
        .cloned()
        .unwrap_or_default();
    let creation_5m = read_i64_attr(attrs, "cache_creation_5m_tokens").unwrap_or(0);
    let creation_1h = read_i64_attr(attrs, "cache_creation_1h_tokens").unwrap_or(0);
    let creation_total = read_i64_attr(attrs, "cache_creation_tokens")
        .unwrap_or(0)
        .max(creation_5m + creation_1h);
    ApiRequestRow {
        source: DataSource::Otlp,
        request_key,
        ts: fields.ts.clone(),
        session_id: fields.session_id.clone(),
        agent,
        model: attrs.get("model").cloned().unwrap_or_default(),
        input_tokens: read_i64_attr(attrs, "input_tokens").unwrap_or(0),
        output_tokens: read_i64_attr(attrs, "output_tokens").unwrap_or(0),
        cache_read_tokens: read_i64_attr(attrs, "cache_read_tokens").unwrap_or(0),
        cache_creation_tokens: creation_total,
        cache_creation_5m_tokens: creation_5m,
        cache_creation_1h_tokens: creation_1h,
        duration_ms: read_i64_attr(attrs, "duration_ms"),
        ..ApiRequestRow::default()
    }
}

// 9. tool_result 변환 ---------------------------------------------------------
fn map_tool_result(fields: &EventFields) -> Option<ToolCallRow> {
    let attrs = &fields.attrs;
    let tool_use_id = attrs.get("tool_use_id").filter(|text| !text.is_empty())?;
    Some(ToolCallRow {
        source: DataSource::Otlp,
        tool_use_id: tool_use_id.clone(),
        ts: fields.ts.clone(),
        session_id: fields.session_id.clone(),
        agent: attrs.get("agent.name").cloned().unwrap_or_default(),
        tool_name: attrs.get("tool_name").cloned().unwrap_or_default(),
        duration_ms: read_i64_attr(attrs, "duration_ms"),
        is_error: attrs.get("success").map(String::as_str) == Some("false"),
        error_kind: attrs.get("error_type").cloned().unwrap_or_default(),
    })
}

// 10. hook_execution_complete 변환 --------------------------------------------
fn map_hook_complete(fields: &EventFields) -> HookEventRow {
    let attrs = &fields.attrs;
    let hook_name = attrs.get("hook_name").cloned().unwrap_or_default();
    let order_part = if fields.sequence.is_empty() {
        &fields.ts
    } else {
        &fields.sequence
    };
    let outcome = if read_i64_attr(attrs, "num_blocking").unwrap_or(0) > 0 {
        HookOutcome::Deny
    } else if read_i64_attr(attrs, "num_non_blocking_error").unwrap_or(0) > 0 {
        HookOutcome::Error
    } else {
        HookOutcome::Allow
    };
    HookEventRow {
        source: DataSource::Otlp,
        event_key: format!("otlp:{}:{order_part}:{hook_name}", fields.session_id),
        ts: fields.ts.clone(),
        session_id: fields.session_id.clone(),
        route: "hook_event".to_string(),
        guard: hook_name,
        outcome,
        duration_ms: read_i64_attr(attrs, "total_duration_ms"),
        ..HookEventRow::default()
    }
}

// 11. 상세 문구 다듬기 --------------------------------------------------------
// 제어 문자는 공백으로 바꾸고 200자에서 자름
fn clip_detail(text: &str) -> String {
    text.trim()
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(DETAIL_MAX_CHARS)
        .collect()
}

// 12. 신규 이벤트 변환 --------------------------------------------------------
// compaction, subagent_completed, api_error, api_retries_exhausted, tool_decision,
// permission_mode_changed 만 취하고 사용자 식별 속성(user.*, organization.id)과 내용은 보관하지 않음
fn map_otel_event(event_name: &str, fields: &EventFields) -> Option<OtelEventRow> {
    let attrs = &fields.attrs;
    let text_of = |key: &str| attrs.get(key).cloned().unwrap_or_default();
    let mut row = OtelEventRow {
        ts: fields.ts.clone(),
        session_id: fields.session_id.clone(),
        agent_id: text_of("agent_id"),
        duration_ms: read_i64_attr(attrs, "duration_ms"),
        ..OtelEventRow::default()
    };

    // 1. 이벤트별 필드
    let mut key_suffix = String::new();
    match event_name {
        "compaction" => {
            row.category = "compaction".to_string();
            row.status = text_of("trigger");
            row.tokens_before = read_i64_attr(attrs, "pre_tokens");
            row.tokens_after = read_i64_attr(attrs, "post_tokens");
            row.detail = if attrs.get("success").map(String::as_str) == Some("false") {
                clip_detail(&format!("failed {}", text_of("error")))
            } else {
                "ok".to_string()
            };
        }
        "subagent_completed" => {
            row.category = "subagent".to_string();
            row.name = text_of("agent_type");
            row.status = if attrs.get("is_async").map(String::as_str) == Some("true") {
                "async".to_string()
            } else {
                "sync".to_string()
            };
            row.tokens_after = read_i64_attr(attrs, "total_tokens");
            row.count = read_i64_attr(attrs, "total_tool_uses");
            let mut parts = vec![format!("model={}", text_of("model"))];
            if attrs.get("model_swapped").map(String::as_str) == Some("true") {
                parts.push(format!("final={}", text_of("final_model")));
            }
            if let Some(source) = attrs.get("agent.source").filter(|text| !text.is_empty()) {
                parts.push(format!("source={source}"));
            }
            row.detail = clip_detail(&parts.join(" "));
        }
        "api_error" => {
            row.category = "api_error".to_string();
            row.name = text_of("model");
            row.status = text_of("status_code");
            row.count = read_i64_attr(attrs, "attempt");
            row.detail = clip_detail(&text_of("error"));
        }
        "api_retries_exhausted" => {
            row.category = "api_retries".to_string();
            row.status = "exhausted".to_string();
            row.count = read_i64_attr(attrs, "total_attempts");
            row.duration_ms = read_i64_attr(attrs, "total_retry_duration_ms");
        }
        "tool_decision" => {
            row.category = "tool_decision".to_string();
            row.name = text_of("tool_name");
            row.status = text_of("decision");
            row.detail = clip_detail(&format!(
                "source={} mode={}",
                text_of("source"),
                text_of("permission_mode")
            ));
            key_suffix = format!(":{}", text_of("tool_use_id"));
        }
        "permission_mode_changed" => {
            row.category = "permission_mode".to_string();
            row.name = text_of("trigger");
            row.status = text_of("to_mode");
            row.detail = clip_detail(&format!("{}->{}", text_of("from_mode"), text_of("to_mode")));
        }
        _ => return None,
    }

    // 2. 중복 제거 키 (세션, 에이전트, 순번 또는 시각, 이벤트 이름)
    let order_part = if fields.sequence.is_empty() {
        &fields.ts
    } else {
        &fields.sequence
    };
    row.event_key = format!(
        "otlp:{}:{}:{order_part}:{event_name}{key_suffix}",
        fields.session_id, row.agent_id
    );
    Some(row)
}
