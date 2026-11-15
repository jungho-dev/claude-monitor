//! net/otlp/mapping.rs
//! OTLP http/json 로그 봉투 해석과 기록 행 변환 (속성 일부만 취함, 본문·프롬프트 미보관).

use crate::core::model::{
    ApiRequestRow, DataSource, HookEventRow, HookOutcome, IngestBatch, SessionRow, ToolCallRow,
};
use anyhow::Context;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

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
            _ => false,
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
        cache_creation_tokens: read_i64_attr(attrs, "cache_creation_tokens").unwrap_or(0),
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
