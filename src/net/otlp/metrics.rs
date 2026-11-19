//! net/otlp/metrics.rs
//! OTLP 지표(Sum·Gauge) 해석: claude_code.* 점을 metric_points 행으로 변환 (누적은 증분 환산, 식별 속성 미보관).

use crate::store::Store;
use crate::store::metrics::MetricPointRow;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

const METRIC_PREFIX: &str = "claude_code.";
const SESSION_ID_KEY: &str = "session.id";
const TEMPORALITY_DELTA: &str = "delta";
const TEMPORALITY_GAUGE: &str = "gauge";
// 식별·조직 속성은 차원에도 식별 키에도 넣지 않음 (user.email, user.account_uuid, organization.id 등)
const BLOCKED_ATTRIBUTE_PREFIXES: [&str; 2] = ["user.", "organization."];

/// 요청 하나에서 만드는 점 수 상한 (초과분은 버림).
pub const MAX_POINTS_PER_REQUEST: usize = 5000;
/// 누적 계열 추적 수 상한 (초과 시 추적 초기화, 다음 표본부터 다시 기준을 잡음).
pub const MAX_TRACKED_SERIES: usize = 50_000;

// 1. 누적 계열 추적 -----------------------------------------------------------
// 계열 식별 키별 직전 누적값 (메모리만 사용, 재시작하면 첫 표본부터 다시 시작)
#[derive(Debug, Default)]
pub struct CumulativeTracker {
    pub last_values: HashMap<String, f64>,
}

// 2. 지표 봉투 변환 결과 ------------------------------------------------------
// baselines 는 적재가 끝난 뒤에만 추적기에 반영할 계열별 새 누적값 (적재 실패 시 버림)
#[derive(Debug, Default)]
pub struct MappedMetrics {
    pub rows: Vec<MetricPointRow>,
    pub baselines: HashMap<String, f64>,
}

// 3. 지표 봉투 변환 -----------------------------------------------------------
/// OTLP/JSON 형태 지표 봉투에서 claude_code.* 의 Sum·Gauge 점을 행으로 변환 (히스토그램류 무시).
/// 추적기는 읽기만 하고 새 기준값은 결과의 baselines 로 돌려줌.
pub fn map_metrics_payload(envelope: &Value, tracker: &CumulativeTracker) -> MappedMetrics {
    let mut rows: Vec<MetricPointRow> = Vec::new();
    let mut baselines: HashMap<String, f64> = HashMap::new();
    'collect: for resource in envelope["resourceMetrics"].as_array().into_iter().flatten() {
        let resource_attrs = flatten_attributes(&resource["resource"]["attributes"]);
        let metrics = resource["scopeMetrics"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|scope| scope["metrics"].as_array().into_iter().flatten());
        for metric in metrics {
            // 1. 대상 지표 (claude_code.* 의 Sum·Gauge 만)
            let Some(name) = metric["name"]
                .as_str()
                .and_then(|full| full.strip_prefix(METRIC_PREFIX))
            else {
                continue;
            };
            let (container, is_gauge) = if metric.get("sum").is_some() {
                (&metric["sum"], false)
            } else if metric.get("gauge").is_some() {
                (&metric["gauge"], true)
            } else {
                continue;
            };
            let cumulative = match &container["aggregationTemporality"] {
                Value::Number(code) => code.as_i64() == Some(2),
                Value::String(text) => text == "AGGREGATION_TEMPORALITY_CUMULATIVE" || text == "2",
                _ => false,
            };

            for point in container["dataPoints"].as_array().into_iter().flatten() {
                // 2. 상한 검사
                if rows.len() >= MAX_POINTS_PER_REQUEST {
                    break 'collect;
                }

                // 3. 값 (asInt 는 십진 문자열·숫자 모두 허용, 비유한 값은 버림)
                let value = ["asInt", "asDouble"]
                    .iter()
                    .find_map(|key| match point.get(key) {
                        Some(Value::Number(number)) => number.as_f64(),
                        Some(Value::String(text)) => text.parse::<f64>().ok(),
                        _ => None,
                    })
                    .filter(|number| number.is_finite());
                let Some(value) = value else {
                    continue;
                };

                // 4. 속성 (점 속성이 리소스 속성을 덮어씀)
                let mut attrs = resource_attrs.clone();
                attrs.extend(flatten_attributes(&point["attributes"]));

                // 5. 누적은 증분으로 환산 (첫 표본은 기준만 저장, 감소는 초기화로 보고 값 전체 사용)
                let delta = if cumulative && !is_gauge {
                    let series_key = format!("{name}|{attrs:?}");
                    let previous = baselines
                        .get(&series_key)
                        .or_else(|| tracker.last_values.get(&series_key))
                        .copied();
                    baselines.insert(series_key, value);
                    let step = match previous {
                        None => continue,
                        Some(previous) if value >= previous => value - previous,
                        Some(_) => value,
                    };
                    if step == 0.0 {
                        continue;
                    }
                    step
                } else {
                    value
                };

                // 6. 시각 (timeUnixNano, 없으면 수신 시각)
                let nanos = match &point["timeUnixNano"] {
                    Value::String(text) => text.parse::<i64>().ok(),
                    Value::Number(number) => number.as_i64(),
                    _ => None,
                };
                let ts = match nanos.filter(|stamp| *stamp > 0) {
                    Some(stamp) => DateTime::from_timestamp_nanos(stamp),
                    None => Utc::now(),
                }
                .to_rfc3339_opts(SecondsFormat::Millis, true);

                // 7. 행 (저장 차원만 취함)
                let pick = |key: &str| attrs.get(key).cloned().unwrap_or_default();
                rows.push(MetricPointRow {
                    ts,
                    session_id: pick(SESSION_ID_KEY),
                    name: name.to_string(),
                    token_type: pick("type"),
                    model: pick("model"),
                    query_source: pick("query_source"),
                    agent_name: pick("agent.name"),
                    skill_name: pick("skill.name"),
                    plugin_name: pick("plugin.name"),
                    mcp_server_name: pick("mcp_server.name"),
                    mcp_tool_name: pick("mcp_tool.name"),
                    value: delta,
                    temporality: if is_gauge {
                        TEMPORALITY_GAUGE
                    } else {
                        TEMPORALITY_DELTA
                    }
                    .to_string(),
                });
            }
        }
    }
    MappedMetrics { rows, baselines }
}

// 4. 지표 봉투 적재 -----------------------------------------------------------
/// 변환·적재·기준값 반영을 한 흐름으로 수행 (적재가 커밋된 뒤에만 추적기를 갱신하므로
/// 실패한 요청의 재전송은 같은 증분을 다시 얻음). 호출자는 요청 간 추적기 잠금을 쥐고 호출.
pub fn ingest_metrics_envelope(
    store: &Store,
    tracker: &mut CumulativeTracker,
    envelope: &Value,
) -> anyhow::Result<u64> {
    // 1. 변환 (추적기는 읽기만)
    let mapped = map_metrics_payload(envelope, tracker);

    // 2. 적재 (실패하면 기준값을 반영하지 않고 오류 반환)
    let inserted = store.insert_metric_points(&mapped.rows)?;

    // 3. 기준값 반영 (상한 초과 시 추적 초기화 후 이번 값부터 다시 기준을 잡음)
    if tracker.last_values.len() + mapped.baselines.len() > MAX_TRACKED_SERIES {
        tracker.last_values.clear();
    }
    tracker.last_values.extend(mapped.baselines);
    Ok(inserted)
}

// 5. 속성 평탄화 --------------------------------------------------------------
// KeyValue 배열을 문자열 맵으로 (배열·맵·바이트 값은 버리고 차단 접두사 키도 버림)
fn flatten_attributes(attributes: &Value) -> BTreeMap<String, String> {
    let mut flat = BTreeMap::new();
    for pair in attributes.as_array().into_iter().flatten() {
        let Some(key) = pair["key"].as_str() else {
            continue;
        };
        if BLOCKED_ATTRIBUTE_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
        {
            continue;
        }
        let any_value = &pair["value"];
        let text = if let Some(text) = any_value["stringValue"].as_str() {
            text.to_string()
        } else if let Some(number) = any_value.get("intValue") {
            match number {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            }
        } else if let Some(number) = any_value.get("doubleValue") {
            match number {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            }
        } else if let Some(flag) = any_value["boolValue"].as_bool() {
            flag.to_string()
        } else {
            continue;
        };
        flat.insert(key.to_string(), text);
    }
    flat
}
