//! store/query.rs
//! TUI·리포트·관리 API 공용 집계 조회 (구간은 RFC 3339 UTC 반열린 구간).

use crate::core::config::{ModelRateConfig, PricingConfig, price_overrides};
use crate::core::model::{
    ApiRequestRow, DENY_ROUND_TRIP_MS, DataSource, HookEventRow, HookOutcome, LiveEvent,
    ToolCallRow,
};
use crate::store::Store;
use chrono::DateTime;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

// 5분 캐시 쓰기 효과값 (분리 값이 없는 이전 행은 쓰기 합계 전부를 5분으로 계산)
macro_rules! write_5m_sql {
    () => {
        "MAX(cache_creation_tokens - cache_creation_1h_tokens, cache_creation_5m_tokens)"
    };
}

// 요청 프롬프트 토큰 (입력 + 캐시 읽기 + 캐시 쓰기)
macro_rules! prompt_tokens_sql {
    () => {
        concat!(
            "(input_tokens + cache_read_tokens + ",
            write_5m_sql!(),
            " + cache_creation_1h_tokens)"
        )
    };
}

// 프롬프트가 이 토큰을 넘으면 긴 프롬프트 단가 (haiku-5-5 만 두 단계)
const LONG_PROMPT_TOKENS: i64 = 100_000;
const CACHE_BREAK_MIN_CREATION: i64 = 20_000;
const CACHE_BREAK_READ_SHARE: f64 = 0.5;
const GAP_IDLE_HOUR_SECS: i64 = 3600;
const GAP_IDLE_TTL_SECS: i64 = 300;

// 훅 집계 묶음 키 (경로, 가드, 판정, 상세) 와 누적 (건수, 소요 목록, 절감)
type HookGroupKey = (String, String, String, String);
type HookGroupTotals = (u64, Vec<i64>, i64);

// 같은 거부는 기록 색인 쪽 우선, 짝 없는 훅 거부는 유지
pub(crate) const DENY_SOURCE_SQL: &str = "(outcome <> 'deny' OR source = 'transcript' \
    OR (source = 'hook' AND NOT EXISTS (\
    SELECT 1 FROM hook_events dn WHERE dn.outcome = 'deny' AND dn.source = 'transcript' \
    AND dn.session_id = hook_events.session_id AND hook_events.session_id <> '' \
    AND dn.ts BETWEEN strftime('%Y-%m-%dT%H:%M:%fZ', hook_events.ts, '-2 seconds') \
    AND strftime('%Y-%m-%dT%H:%M:%fZ', hook_events.ts, '+10 seconds'))))";

// 캐시 붕괴 직전 요청 조회 (구간 밖 폴백)
pub(crate) const CACHE_BREAK_PREVIOUS_SQL: &str = "SELECT ts, model, \
    input_tokens + cache_read_tokens + cache_creation_tokens \
    FROM api_requests WHERE source = 'transcript' AND session_id = ?1 \
    AND agent = ?2 AND ts < ?3 ORDER BY ts DESC LIMIT 1";

// 통계 요청 원본 (ts 인덱스 구간)
pub(crate) const STATS_REQUESTS_SQL: &str = concat!(
    "SELECT model, session_id, agent, input_tokens, output_tokens, cache_read_tokens, \
     cache_creation_tokens, duration_ms, ts, ",
    write_5m_sql!(),
    ", cache_creation_1h_tokens FROM api_requests \
     WHERE source = 'transcript' AND ts >= ?1 AND ts < ?2"
);

// 모델·에이전트 사용량 (프롬프트 길이 단계별 묶음, 단계별 합으로 비용 산출)
const MODEL_USAGE_SQL: &str = concat!(
    "SELECT model, agent, COUNT(*), SUM(input_tokens), SUM(output_tokens), \
     SUM(cache_read_tokens), SUM(cache_creation_tokens), SUM(",
    write_5m_sql!(),
    "), SUM(cache_creation_1h_tokens) FROM api_requests \
     WHERE source = 'transcript' AND ts >= ?1 AND ts < ?2 \
     GROUP BY model, agent, ",
    prompt_tokens_sql!(),
    " > ?3"
);

// 프로젝트·브랜치 집계 원본 (프로젝트는 요청 값, 없으면 세션 폴더명)
const PROJECT_REQUESTS_SQL: &str = concat!(
    "SELECT COALESCE(NULLIF(r.project, ''), s.project, ''), r.git_branch, r.session_id, r.model, \
     r.input_tokens, r.output_tokens, r.cache_read_tokens, ",
    write_5m_sql!(),
    ", r.cache_creation_1h_tokens, r.ts, r.agent FROM api_requests r \
     LEFT JOIN sessions s ON s.session_id = r.session_id \
     WHERE r.source = 'transcript' AND r.ts >= ?1 AND r.ts < ?2"
);

// 통계 도구 호출 원본 (ts 인덱스 구간)
pub(crate) const STATS_TOOL_CALLS_SQL: &str = "SELECT agent, is_error, ts, tool_name FROM tool_calls \
    WHERE source = 'transcript' AND ts >= ?1 AND ts < ?2";

// 스킬 호출 원본 (ts 인덱스 구간, detail 은 스킬 이름)
pub(crate) const SKILL_CALLS_SQL: &str = "SELECT detail, agent, session_id, ts, is_error \
    FROM tool_calls WHERE source = 'transcript' AND tool_name = 'Skill' AND ts >= ?1 AND ts < ?2";

// 상세 카드 상위 목록·최근 목록 길이
const SHARE_LIMIT: usize = 5;
const RECENT_LIMIT: usize = 5;

// 1. 개요 집계 ----------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OverviewStats {
    pub since: String,
    pub until: String,
    pub sessions: u64,
    pub api_requests: u64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_hit_ratio: f64,
    pub model_wait_p50_ms: Option<i64>,
    pub model_wait_p90_ms: Option<i64>,
    pub tool_calls: u64,
    pub tool_errors: u64,
    pub hook_denials: u64,
    pub hook_rewrites: u64,
    pub saved_ms: i64,
    pub lost_ms_estimate: i64,
}

// 2. 도구 지연 행 -------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolLatencyRow {
    pub tool_name: String,
    pub calls: u64,
    pub errors: u64,
    pub p50_ms: Option<i64>,
    pub p90_ms: Option<i64>,
    pub total_ms: i64,
}

// 3. 훅 집계 행 ---------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookStatRow {
    pub route: String,
    pub guard: String,
    pub outcome: String,
    pub detail: String,
    pub count: u64,
    pub p50_ms: Option<i64>,
    pub p90_ms: Option<i64>,
    pub saved_ms: i64,
}

// 4. 모델·에이전트 사용량 행 --------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUsageRow {
    pub model: String,
    pub agent: String,
    pub requests: u64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_creation_5m_tokens: i64,
    pub cache_creation_1h_tokens: i64,
    pub cost_usd_estimate: f64,
}

// 5. 게이트웨이 지연 집계 -----------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatewayTimingStats {
    pub requests: u64,
    pub errors: u64,
    pub first_byte_p50_ms: Option<i64>,
    pub first_byte_p90_ms: Option<i64>,
    pub first_token_p50_ms: Option<i64>,
    pub first_token_p90_ms: Option<i64>,
    pub total_p50_ms: Option<i64>,
    pub total_p90_ms: Option<i64>,
}

// 6. 캐시 붕괴 행 -------------------------------------------------------------
// 세션 중간 cache_creation 급증 요청
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheBreakRow {
    pub ts: String,
    pub session_id: String,
    pub agent: String,
    pub model: String,
    pub cache_creation_tokens: i64,
    pub previous_context_tokens: i64,
    pub gap_secs: i64,
    pub likely_cause: String,
}

// 7. 분 단위 추이 -------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MinuteBucket {
    pub minute: String,
    pub api_requests: u64,
    pub output_tokens: i64,
    pub tool_calls: u64,
    pub hook_denials: u64,
}

// 8. 출처별 요청 건수 ---------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceCounts {
    pub transcript: u64,
    pub otlp: u64,
    pub gateway: u64,
}

// 8-1. 모델 통계 행 -----------------------------------------------------------
// 모델별 한 행 (기록 색인 출처)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelStatsRow {
    pub model: String,
    pub requests: u64,
    pub sessions: u64,
    pub agent_types: u64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_creation_5m_tokens: i64,
    pub cache_creation_1h_tokens: i64,
    pub cache_hit_ratio: f64,
    pub wait_p50_ms: Option<i64>,
    pub wait_p90_ms: Option<i64>,
    pub cost_usd_estimate: f64,
    pub last_ts: String,
    #[serde(default)]
    pub first_ts: String,
    // 비용 순 상위 에이전트 종류
    #[serde(default)]
    pub agents: Vec<UsageShare>,
}

// 8-1-1. 사용 분포 항목 -------------------------------------------------------
// 상세 카드의 상위 목록 한 줄 (도구·스킬은 count 가 호출 수, 비용 0)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageShare {
    pub name: String,
    pub count: u64,
    pub errors: u64,
    pub output_tokens: i64,
    pub cost_usd_estimate: f64,
    pub last_ts: String,
}

// 8-1-2. 에이전트 실행 요약 ---------------------------------------------------
// 메인은 세션 id, 서브에이전트는 '종류#id' 가 실행 키
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentRunSummary {
    pub run: String,
    pub model: String,
    pub requests: u64,
    pub output_tokens: i64,
    pub cost_usd_estimate: f64,
    pub first_ts: String,
    pub last_ts: String,
}

// 8-1-3. 스킬 통계 행 ---------------------------------------------------------
// Skill 도구 호출 기준 (슬래시 명령 직접 호출은 기록에 도구 호출이 없어 제외)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillStatsRow {
    pub skill: String,
    pub calls: u64,
    pub errors: u64,
    pub sessions: u64,
    pub first_ts: String,
    pub last_ts: String,
    // 호출 수 순 호출 에이전트 종류
    pub agents: Vec<UsageShare>,
    // 최근 호출 (name 은 에이전트 종류, last_ts 는 호출 시각)
    pub recent: Vec<UsageShare>,
}

// 8-2. 에이전트 통계 행 -------------------------------------------------------
// 종류별 한 행 ('#' 앞 라벨)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentStatsRow {
    pub agent: String,
    pub runs: u64,
    pub requests: u64,
    pub sessions: u64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_creation_5m_tokens: i64,
    pub cache_creation_1h_tokens: i64,
    pub cache_hit_ratio: f64,
    pub wait_p50_ms: Option<i64>,
    pub wait_p90_ms: Option<i64>,
    pub cost_usd_estimate: f64,
    pub tool_calls: u64,
    pub tool_errors: u64,
    pub top_model: String,
    pub last_ts: String,
    #[serde(default)]
    pub first_ts: String,
    // 비용 순 사용 모델
    #[serde(default)]
    pub models: Vec<UsageShare>,
    // 호출 수 순 도구
    #[serde(default)]
    pub top_tools: Vec<UsageShare>,
    // 마지막 활동 순 최근 실행
    #[serde(default)]
    pub recent_runs: Vec<AgentRunSummary>,
}

// 8-2-1. 프로젝트·브랜치 통계 행 ----------------------------------------------
// 프로젝트는 작업 폴더 마지막 요소 (없으면 기록 세션 폴더명), 브랜치가 없으면 빈 문자열
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectStatsRow {
    pub project: String,
    pub git_branch: String,
    pub requests: u64,
    pub sessions: u64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_5m_tokens: i64,
    pub cache_creation_1h_tokens: i64,
    pub cost_usd_estimate: f64,
    pub last_ts: String,
    #[serde(default)]
    pub first_ts: String,
    // 비용 순 모델·에이전트 종류·세션
    #[serde(default)]
    pub models: Vec<UsageShare>,
    #[serde(default)]
    pub agents: Vec<UsageShare>,
    #[serde(default)]
    pub top_sessions: Vec<UsageShare>,
}

// 8-3. 모델 통계 누적 ---------------------------------------------------------
#[derive(Default)]
struct ModelStatsTotals {
    requests: u64,
    sessions: BTreeSet<String>,
    agent_types: BTreeSet<String>,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    cache_creation_5m_tokens: i64,
    cache_creation_1h_tokens: i64,
    cost_usd_estimate: f64,
    waits: Vec<i64>,
    first_ts: String,
    last_ts: String,
    agents: BTreeMap<String, UsageShare>,
}

// 8-4. 에이전트 통계 누적 -----------------------------------------------------
#[derive(Default)]
struct AgentStatsTotals {
    run_keys: BTreeSet<String>,
    sessions: BTreeSet<String>,
    requests: u64,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    cache_creation_5m_tokens: i64,
    cache_creation_1h_tokens: i64,
    cost_usd_estimate: f64,
    waits: Vec<i64>,
    model_requests: BTreeMap<String, u64>,
    tool_calls: u64,
    tool_errors: u64,
    first_ts: String,
    last_ts: String,
    models: BTreeMap<String, UsageShare>,
    tools: BTreeMap<String, UsageShare>,
    runs: BTreeMap<String, AgentRunSummary>,
}

// 9. 캐시 붕괴 후보 원본 ------------------------------------------------------
struct CacheProbeRow {
    session_id: String,
    agent: String,
    ts: String,
    model: String,
    context_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
}

// 9-1. 프로젝트·브랜치 통계 누적 ----------------------------------------------
#[derive(Default)]
struct ProjectStatsTotals {
    requests: u64,
    sessions: BTreeSet<String>,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_5m_tokens: i64,
    cache_creation_1h_tokens: i64,
    cost_usd_estimate: f64,
    first_ts: String,
    last_ts: String,
    models: BTreeMap<String, UsageShare>,
    agents: BTreeMap<String, UsageShare>,
    sessions_usage: BTreeMap<String, UsageShare>,
}

// 9-2. 스킬 통계 누적 ---------------------------------------------------------
#[derive(Default)]
struct SkillStatsTotals {
    calls: u64,
    errors: u64,
    sessions: BTreeSet<String>,
    first_ts: String,
    agents: BTreeMap<String, UsageShare>,
    recent: Vec<UsageShare>,
}

// 9-3. 분포·구간 누적 도우미 --------------------------------------------------
// 한 건을 분포에 더함
fn add_share(
    shares: &mut BTreeMap<String, UsageShare>,
    name: &str,
    output_tokens: i64,
    cost: f64,
    is_error: bool,
    ts: &str,
) {
    let entry = shares.entry(name.to_string()).or_default();
    entry.count += 1;
    entry.errors += u64::from(is_error);
    entry.output_tokens += output_tokens;
    entry.cost_usd_estimate += cost;
    if ts > entry.last_ts.as_str() {
        entry.last_ts = ts.to_string();
    }
}

// 비용·건수 순 상위 항목만 남김
fn top_shares(shares: BTreeMap<String, UsageShare>, limit: usize) -> Vec<UsageShare> {
    let mut ranked: Vec<UsageShare> = shares
        .into_iter()
        .map(|(name, share)| UsageShare { name, ..share })
        .collect();
    ranked.sort_by(|left, right| {
        right
            .cost_usd_estimate
            .total_cmp(&left.cost_usd_estimate)
            .then_with(|| right.count.cmp(&left.count))
            .then_with(|| left.name.cmp(&right.name))
    });
    ranked.truncate(limit);
    ranked
}

// 가장 이른 시각 갱신
fn keep_first_ts(first_ts: &mut String, ts: &str) {
    if first_ts.is_empty() || ts < first_ts.as_str() {
        *first_ts = ts.to_string();
    }
}

// 10. 백분위 요약 -------------------------------------------------------------
// 정렬 후 최근접 순위법 p50·p90
pub fn summarize_percentiles(values: &mut [i64]) -> (Option<i64>, Option<i64>) {
    if values.is_empty() {
        return (None, None);
    }
    values.sort_unstable();
    let rank_at = |percent: usize| {
        let index = (values.len() * percent).div_ceil(100).max(1) - 1;
        values[index]
    };
    (Some(rank_at(50)), Some(rank_at(90)))
}

// 11. 단가표 ------------------------------------------------------------------
// USD per MTok, 쓰기 단가는 5분·1시간 TTL 별
#[derive(Debug, Clone, Copy, PartialEq)]
struct ModelRates {
    input: f64,
    output: f64,
    cache_write_5m: f64,
    cache_write_1h: f64,
    cache_read: f64,
}

// 모델 이름 접두어별 단가 (가장 긴 접두어 우선), long_prompt 는 프롬프트 초과 구간 단가
struct RateEntry {
    prefix: &'static str,
    rates: ModelRates,
    long_prompt: Option<ModelRates>,
}

// 쓰기 5분 1.25배, 1시간 2배, 읽기 0.1배 기본 배수
const fn standard_rates(input: f64, output: f64) -> ModelRates {
    ModelRates {
        input,
        output,
        cache_write_5m: input * 1.25,
        cache_write_1h: input * 2.0,
        cache_read: input * 0.1,
    }
}

// 출처 platform.claude.com/docs/en/about-claude/pricing (2026-11-19 조회, 전 항목 확인)
// opus-4 는 Opus 4·4.1 (4.5 이후는 개별 항목), sonnet-4 는 Sonnet 4 와 날짜판
const RATE_TABLE: [RateEntry; 17] = [
    RateEntry {
        prefix: "opus-5-5",
        rates: ModelRates {
            input: 4.0,
            output: 20.0,
            cache_write_5m: 5.0,
            cache_write_1h: 8.0,
            cache_read: 0.20,
        },
        long_prompt: None,
    },
    RateEntry {
        prefix: "opus-5",
        rates: standard_rates(5.0, 25.0),
        long_prompt: None,
    },
    RateEntry {
        prefix: "opus-4-8",
        rates: standard_rates(5.0, 25.0),
        long_prompt: None,
    },
    RateEntry {
        prefix: "opus-4-7",
        rates: standard_rates(5.0, 25.0),
        long_prompt: None,
    },
    RateEntry {
        prefix: "opus-4-6",
        rates: standard_rates(5.0, 25.0),
        long_prompt: None,
    },
    RateEntry {
        prefix: "opus-4-5",
        rates: standard_rates(5.0, 25.0),
        long_prompt: None,
    },
    RateEntry {
        prefix: "opus-4",
        rates: standard_rates(15.0, 75.0),
        long_prompt: None,
    },
    RateEntry {
        prefix: "sonnet-5-5",
        rates: ModelRates {
            input: 2.0,
            output: 10.0,
            cache_write_5m: 2.50,
            cache_write_1h: 4.0,
            cache_read: 0.10,
        },
        long_prompt: None,
    },
    RateEntry {
        prefix: "sonnet-5",
        rates: standard_rates(2.0, 10.0),
        long_prompt: None,
    },
    RateEntry {
        prefix: "sonnet-4-6",
        rates: standard_rates(3.0, 15.0),
        long_prompt: None,
    },
    RateEntry {
        prefix: "sonnet-4-5",
        rates: standard_rates(3.0, 15.0),
        long_prompt: None,
    },
    RateEntry {
        prefix: "sonnet-4",
        rates: standard_rates(3.0, 15.0),
        long_prompt: None,
    },
    RateEntry {
        prefix: "haiku-5-5",
        rates: standard_rates(0.10, 0.50),
        long_prompt: Some(standard_rates(0.50, 2.50)),
    },
    RateEntry {
        prefix: "haiku-4-5",
        rates: standard_rates(1.0, 5.0),
        long_prompt: None,
    },
    RateEntry {
        prefix: "fable-5-2",
        rates: ModelRates {
            input: 8.0,
            output: 40.0,
            cache_write_5m: 10.0,
            cache_write_1h: 16.0,
            cache_read: 0.20,
        },
        long_prompt: None,
    },
    RateEntry {
        prefix: "fable-5-1",
        rates: ModelRates {
            input: 10.0,
            output: 50.0,
            cache_write_5m: 12.50,
            cache_write_1h: 20.0,
            cache_read: 0.25,
        },
        long_prompt: None,
    },
    RateEntry {
        prefix: "fable-5",
        rates: standard_rates(10.0, 50.0),
        long_prompt: None,
    },
];

// 11-1. 단가 조회 -------------------------------------------------------------
// 설정 덮어쓰기가 맞으면 그 중 가장 긴 키 (단일 단가), 아니면 내장표에서 가장 긴 접두어, 없으면 None
fn find_rates(model: &str, overrides: &PricingConfig) -> Option<(ModelRates, Option<ModelRates>)> {
    let name = model.strip_prefix("claude-").unwrap_or(model);
    let custom: Option<(usize, &ModelRateConfig)> = overrides
        .models
        .iter()
        .filter_map(|(key, rate)| {
            let trimmed = key.trim();
            let prefix = trimmed.strip_prefix("claude-").unwrap_or(trimmed);
            name.starts_with(prefix).then_some((prefix.len(), rate))
        })
        .max_by_key(|(length, _)| *length);
    if let Some((_, rate)) = custom {
        let rates = ModelRates {
            input: rate.input,
            output: rate.output,
            cache_write_5m: rate.cache_write_5m.unwrap_or(rate.input * 1.25),
            cache_write_1h: rate.cache_write_1h.unwrap_or(rate.input * 2.0),
            cache_read: rate.cache_read.unwrap_or(rate.input * 0.1),
        };
        return Some((rates, None));
    }
    RATE_TABLE
        .iter()
        .filter(|entry| name.starts_with(entry.prefix))
        .max_by_key(|entry| entry.prefix.len())
        .map(|entry| (entry.rates, entry.long_prompt))
}

// 11-2. 비용 추정 -------------------------------------------------------------
// 요청 1건 단위 USD 추정 (프롬프트 단계 단가 판정이 요청 단위이므로 합산값이 아닌 요청 값을 넣음), 알 수 없는 모델은 0
pub fn estimate_cost_with_overrides(
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_5m_tokens: i64,
    cache_write_1h_tokens: i64,
    overrides: &PricingConfig,
) -> f64 {
    let Some((standard, long_prompt)) = find_rates(model, overrides) else {
        return 0.0;
    };
    let prompt_tokens =
        input_tokens + cache_read_tokens + cache_write_5m_tokens + cache_write_1h_tokens;
    let rates = match long_prompt {
        Some(long) if prompt_tokens > LONG_PROMPT_TOKENS => long,
        _ => standard,
    };
    (input_tokens as f64 * rates.input
        + output_tokens as f64 * rates.output
        + cache_read_tokens as f64 * rates.cache_read
        + cache_write_5m_tokens as f64 * rates.cache_write_5m
        + cache_write_1h_tokens as f64 * rates.cache_write_1h)
        / 1_000_000.0
}

// 11-3. 설치된 단가 덮어쓰기로 비용 추정 --------------------------------------
// cache_write_5m 은 이전 행을 위한 효과값 (분리 없으면 쓰기 합계)
pub fn estimate_cost_usd(
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_5m_tokens: i64,
    cache_write_1h_tokens: i64,
) -> f64 {
    estimate_cost_with_overrides(
        model,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_5m_tokens,
        cache_write_1h_tokens,
        price_overrides(),
    )
}

// 12. 집계 조회 ---------------------------------------------------------------
impl Store {
    // 12-1. 개요
    pub fn query_overview(&self, since: &str, until: &str) -> anyhow::Result<OverviewStats> {
        self.with_conn(|conn| {
            let mut stats = OverviewStats {
                since: since.to_string(),
                until: until.to_string(),
                ..OverviewStats::default()
            };

            // 1. API 요청·토큰 (기록 색인 출처만)
            let (requests, sessions, input, output, cache_read, cache_creation) = conn.query_row(
                "SELECT COUNT(*), COUNT(DISTINCT session_id), COALESCE(SUM(input_tokens), 0), \
                 COALESCE(SUM(output_tokens), 0), COALESCE(SUM(cache_read_tokens), 0), \
                 COALESCE(SUM(cache_creation_tokens), 0) FROM api_requests \
                 WHERE source = 'transcript' AND ts >= ?1 AND ts < ?2",
                params![since, until],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )?;
            stats.api_requests = requests as u64;
            stats.sessions = sessions as u64;
            stats.input_tokens = input;
            stats.output_tokens = output;
            stats.cache_read_tokens = cache_read;
            stats.cache_creation_tokens = cache_creation;
            let context_total = input + cache_read + cache_creation;
            if context_total > 0 {
                stats.cache_hit_ratio = cache_read as f64 / context_total as f64;
            }

            // 2. 모델 대기 백분위
            let mut wait_stmt = conn.prepare_cached(
                "SELECT duration_ms FROM api_requests WHERE source = 'transcript' \
                 AND ts >= ?1 AND ts < ?2 AND duration_ms IS NOT NULL",
            )?;
            let mut waits: Vec<i64> = wait_stmt
                .query_map(params![since, until], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            let (wait_p50, wait_p90) = summarize_percentiles(&mut waits);
            stats.model_wait_p50_ms = wait_p50;
            stats.model_wait_p90_ms = wait_p90;

            // 3. 도구 호출·오류
            let (tool_calls, tool_errors) = conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(is_error), 0) FROM tool_calls \
                 WHERE ts >= ?1 AND ts < ?2",
                params![since, until],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )?;
            stats.tool_calls = tool_calls as u64;
            stats.tool_errors = tool_errors as u64;

            // 4. 훅 판정·절감·손실
            let hook_sql = format!(
                "SELECT COALESCE(SUM(outcome = 'deny'), 0), COALESCE(SUM(outcome = 'rewrite'), 0), \
                 COALESCE(SUM(saved_ms), 0) FROM hook_events \
                 WHERE ts >= ?1 AND ts < ?2 AND {DENY_SOURCE_SQL}"
            );
            let (denials, rewrites, saved) =
                conn.query_row(&hook_sql, params![since, until], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?;
            stats.hook_denials = denials as u64;
            stats.hook_rewrites = rewrites as u64;
            stats.saved_ms = saved;
            stats.lost_ms_estimate = denials * DENY_ROUND_TRIP_MS;
            Ok(stats)
        })
    }

    // 12-2. 도구별 지연
    pub fn query_tool_latency(
        &self,
        since: &str,
        until: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<ToolLatencyRow>> {
        self.with_conn(|conn| {
            // 1. 호출 원본
            let mut stmt = conn.prepare_cached(
                "SELECT tool_name, duration_ms, is_error FROM tool_calls \
                 WHERE ts >= ?1 AND ts < ?2",
            )?;
            let mut grouped: BTreeMap<String, (u64, u64, Vec<i64>)> = BTreeMap::new();
            let mut rows = stmt.query(params![since, until])?;
            while let Some(row) = rows.next()? {
                let tool_name: String = row.get(0)?;
                let duration_ms: Option<i64> = row.get(1)?;
                let is_error: bool = row.get(2)?;
                let entry = grouped.entry(tool_name).or_default();
                entry.0 += 1;
                if is_error {
                    entry.1 += 1;
                }
                if let Some(elapsed) = duration_ms {
                    entry.2.push(elapsed);
                }
            }

            // 2. 도구별 요약·정렬
            let mut summary: Vec<ToolLatencyRow> = Vec::new();
            for (tool_name, (calls, errors, mut durations)) in grouped {
                let (p50_ms, p90_ms) = summarize_percentiles(&mut durations);
                summary.push(ToolLatencyRow {
                    tool_name,
                    calls,
                    errors,
                    p50_ms,
                    p90_ms,
                    total_ms: durations.iter().sum(),
                });
            }
            summary.sort_by(|left, right| {
                right
                    .total_ms
                    .cmp(&left.total_ms)
                    .then_with(|| right.calls.cmp(&left.calls))
            });
            summary.truncate(limit);
            Ok(summary)
        })
    }

    // 12-3. 훅 판정 집계
    pub fn query_hook_stats(&self, since: &str, until: &str) -> anyhow::Result<Vec<HookStatRow>> {
        self.with_conn(|conn| {
            // 1. 이벤트 원본 (건수·절감은 짝 없는 행만, 소요는 전체 행)
            let hook_sql = format!(
                "SELECT route, guard, outcome, detail, duration_ms, saved_ms, \
                 (CASE WHEN {DENY_SOURCE_SQL} THEN 1 ELSE 0 END) FROM hook_events \
                 WHERE ts >= ?1 AND ts < ?2"
            );
            let mut stmt = conn.prepare_cached(&hook_sql)?;
            let mut grouped: BTreeMap<HookGroupKey, HookGroupTotals> = BTreeMap::new();
            let mut rows = stmt.query(params![since, until])?;
            while let Some(row) = rows.next()? {
                let key = (
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                );
                let duration_ms: Option<i64> = row.get(4)?;
                let saved_ms: Option<i64> = row.get(5)?;
                let counted = row.get::<_, i64>(6)? != 0;
                let entry = grouped.entry(key).or_default();
                if counted {
                    entry.0 += 1;
                    entry.2 += saved_ms.unwrap_or(0);
                }
                if let Some(elapsed) = duration_ms {
                    entry.1.push(elapsed);
                }
            }

            // 2. 판정별 요약·정렬
            let mut summary: Vec<HookStatRow> = Vec::new();
            for ((route, guard, outcome, detail), (count, mut durations, saved_ms)) in grouped {
                let (p50_ms, p90_ms) = summarize_percentiles(&mut durations);
                summary.push(HookStatRow {
                    route,
                    guard,
                    outcome,
                    detail,
                    count,
                    p50_ms,
                    p90_ms,
                    saved_ms,
                });
            }
            summary.sort_by_key(|row| std::cmp::Reverse(row.count));
            Ok(summary)
        })
    }

    // 12-4. 모델·에이전트 사용량
    pub fn query_model_usage(
        &self,
        since: &str,
        until: &str,
    ) -> anyhow::Result<Vec<ModelUsageRow>> {
        self.with_conn(|conn| {
            // 1. 모델·에이전트 합산 (하위 에이전트 인스턴스 접미사 제거 후 병합, 비용은 프롬프트 단계 묶음별로 산출)
            let mut stmt = conn.prepare_cached(MODEL_USAGE_SQL)?;
            let mut merged: BTreeMap<(String, String), ModelUsageRow> = BTreeMap::new();
            let mut rows = stmt.query(params![since, until, LONG_PROMPT_TOKENS])?;
            while let Some(row) = rows.next()? {
                let model: String = row.get(0)?;
                let agent_full: String = row.get(1)?;
                let agent = agent_full.split('#').next().unwrap_or("").to_string();
                let input_tokens: i64 = row.get(3)?;
                let output_tokens: i64 = row.get(4)?;
                let cache_read_tokens: i64 = row.get(5)?;
                let cache_creation_5m_tokens: i64 = row.get(7)?;
                let cache_creation_1h_tokens: i64 = row.get(8)?;
                let cost = estimate_cost_usd(
                    &model,
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_creation_5m_tokens,
                    cache_creation_1h_tokens,
                );
                let entry = merged
                    .entry((model.clone(), agent.clone()))
                    .or_insert_with(|| ModelUsageRow {
                        model,
                        agent,
                        ..ModelUsageRow::default()
                    });
                entry.requests += row.get::<_, i64>(2)? as u64;
                entry.input_tokens += input_tokens;
                entry.output_tokens += output_tokens;
                entry.cache_read_tokens += cache_read_tokens;
                entry.cache_creation_tokens += row.get::<_, i64>(6)?;
                entry.cache_creation_5m_tokens += cache_creation_5m_tokens;
                entry.cache_creation_1h_tokens += cache_creation_1h_tokens;
                entry.cost_usd_estimate += cost;
            }

            // 2. 정렬
            let mut usage: Vec<ModelUsageRow> = merged.into_values().collect();
            usage.sort_by(|left, right| {
                right
                    .cost_usd_estimate
                    .total_cmp(&left.cost_usd_estimate)
                    .then_with(|| right.requests.cmp(&left.requests))
            });
            Ok(usage)
        })
    }

    // 12-5. 게이트웨이 지연
    pub fn query_gateway_timing(
        &self,
        since: &str,
        until: &str,
    ) -> anyhow::Result<GatewayTimingStats> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT first_byte_ms, first_token_ms, duration_ms, status FROM api_requests \
                 WHERE source = 'gateway' AND ts >= ?1 AND ts < ?2",
            )?;
            let mut stats = GatewayTimingStats::default();
            let mut first_bytes: Vec<i64> = Vec::new();
            let mut first_tokens: Vec<i64> = Vec::new();
            let mut totals: Vec<i64> = Vec::new();
            let mut rows = stmt.query(params![since, until])?;
            while let Some(row) = rows.next()? {
                stats.requests += 1;
                let status: Option<i64> = row.get(3)?;
                if status.is_some_and(|code| code >= 400) {
                    stats.errors += 1;
                }
                if let Some(elapsed) = row.get::<_, Option<i64>>(0)? {
                    first_bytes.push(elapsed);
                }
                if let Some(elapsed) = row.get::<_, Option<i64>>(1)? {
                    first_tokens.push(elapsed);
                }
                if let Some(elapsed) = row.get::<_, Option<i64>>(2)? {
                    totals.push(elapsed);
                }
            }
            (stats.first_byte_p50_ms, stats.first_byte_p90_ms) =
                summarize_percentiles(&mut first_bytes);
            (stats.first_token_p50_ms, stats.first_token_p90_ms) =
                summarize_percentiles(&mut first_tokens);
            (stats.total_p50_ms, stats.total_p90_ms) = summarize_percentiles(&mut totals);
            Ok(stats)
        })
    }

    // 12-6. 캐시 붕괴 후보
    pub fn query_cache_breaks(
        &self,
        since: &str,
        until: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<CacheBreakRow>> {
        self.with_conn(|conn| {
            // 1. 구간 요청 (세션·에이전트별 시각순)
            let mut stmt = conn.prepare_cached(
                "SELECT session_id, agent, ts, model, \
                 input_tokens + cache_read_tokens + cache_creation_tokens, \
                 cache_read_tokens, cache_creation_tokens FROM api_requests \
                 WHERE source = 'transcript' AND ts >= ?1 AND ts < ?2 \
                 ORDER BY session_id, agent, ts",
            )?;
            let probes: Vec<CacheProbeRow> = stmt
                .query_map(params![since, until], |row| {
                    Ok(CacheProbeRow {
                        session_id: row.get(0)?,
                        agent: row.get(1)?,
                        ts: row.get(2)?,
                        model: row.get(3)?,
                        context_tokens: row.get(4)?,
                        cache_read_tokens: row.get(5)?,
                        cache_creation_tokens: row.get(6)?,
                    })
                })?
                .collect::<Result<_, _>>()?;

            // 2. 직전 요청 대비 판정
            let mut breaks: Vec<CacheBreakRow> = Vec::new();
            for (index, probe) in probes.iter().enumerate() {
                if probe.cache_creation_tokens < CACHE_BREAK_MIN_CREATION {
                    continue;
                }
                let same_group = index > 0
                    && probes[index - 1].session_id == probe.session_id
                    && probes[index - 1].agent == probe.agent;
                let previous: Option<(String, String, i64)> = if same_group {
                    let before = &probes[index - 1];
                    Some((
                        before.ts.clone(),
                        before.model.clone(),
                        before.context_tokens,
                    ))
                } else {
                    conn.query_row(
                        CACHE_BREAK_PREVIOUS_SQL,
                        params![probe.session_id, probe.agent, probe.ts],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?
                };
                let Some((previous_ts, previous_model, previous_context)) = previous else {
                    continue;
                };
                if (probe.cache_read_tokens as f64)
                    >= CACHE_BREAK_READ_SHARE * previous_context as f64
                {
                    continue;
                }

                // 3. 원인 추정
                let gap_secs = match (
                    DateTime::parse_from_rfc3339(&probe.ts),
                    DateTime::parse_from_rfc3339(&previous_ts),
                ) {
                    (Ok(current), Ok(before)) => (current - before).num_seconds().max(0),
                    _ => 0,
                };
                let likely_cause = if gap_secs > GAP_IDLE_HOUR_SECS {
                    "유휴 1시간 초과"
                } else if gap_secs > GAP_IDLE_TTL_SECS {
                    "유휴 5분 초과 (TTL)"
                } else if probe.model != previous_model {
                    "모델 변경"
                } else {
                    "prefix 변경 (원인 미상)"
                };
                breaks.push(CacheBreakRow {
                    ts: probe.ts.clone(),
                    session_id: probe.session_id.clone(),
                    agent: probe.agent.clone(),
                    model: probe.model.clone(),
                    cache_creation_tokens: probe.cache_creation_tokens,
                    previous_context_tokens: previous_context,
                    gap_secs,
                    likely_cause: likely_cause.to_string(),
                });
            }
            breaks.sort_by(|left, right| {
                right.cache_creation_tokens.cmp(&left.cache_creation_tokens)
            });
            breaks.truncate(limit);
            Ok(breaks)
        })
    }

    // 12-7. 분 단위 추이
    pub fn query_minute_buckets(
        &self,
        since: &str,
        until: &str,
    ) -> anyhow::Result<Vec<MinuteBucket>> {
        self.with_conn(|conn| {
            let mut buckets: BTreeMap<String, MinuteBucket> = BTreeMap::new();

            // 1. API 요청
            let mut api_stmt = conn.prepare_cached(
                "SELECT substr(ts, 1, 16), COUNT(*), COALESCE(SUM(output_tokens), 0) \
                 FROM api_requests WHERE source = 'transcript' AND ts >= ?1 AND ts < ?2 \
                 GROUP BY 1",
            )?;
            let mut rows = api_stmt.query(params![since, until])?;
            while let Some(row) = rows.next()? {
                let minute: String = row.get(0)?;
                let bucket = buckets
                    .entry(minute.clone())
                    .or_insert_with(|| MinuteBucket {
                        minute: format!("{minute}:00Z"),
                        ..MinuteBucket::default()
                    });
                bucket.api_requests = row.get::<_, i64>(1)? as u64;
                bucket.output_tokens = row.get(2)?;
            }

            // 2. 도구 호출
            let mut tool_stmt = conn.prepare_cached(
                "SELECT substr(ts, 1, 16), COUNT(*) FROM tool_calls \
                 WHERE ts >= ?1 AND ts < ?2 GROUP BY 1",
            )?;
            let mut rows = tool_stmt.query(params![since, until])?;
            while let Some(row) = rows.next()? {
                let minute: String = row.get(0)?;
                let bucket = buckets
                    .entry(minute.clone())
                    .or_insert_with(|| MinuteBucket {
                        minute: format!("{minute}:00Z"),
                        ..MinuteBucket::default()
                    });
                bucket.tool_calls = row.get::<_, i64>(1)? as u64;
            }

            // 3. 훅 거부
            let deny_sql = format!(
                "SELECT substr(ts, 1, 16), COUNT(*) FROM hook_events \
                 WHERE outcome = 'deny' AND ts >= ?1 AND ts < ?2 AND {DENY_SOURCE_SQL} GROUP BY 1"
            );
            let mut deny_stmt = conn.prepare_cached(&deny_sql)?;
            let mut rows = deny_stmt.query(params![since, until])?;
            while let Some(row) = rows.next()? {
                let minute: String = row.get(0)?;
                let bucket = buckets
                    .entry(minute.clone())
                    .or_insert_with(|| MinuteBucket {
                        minute: format!("{minute}:00Z"),
                        ..MinuteBucket::default()
                    });
                bucket.hook_denials = row.get::<_, i64>(1)? as u64;
            }
            Ok(buckets.into_values().collect())
        })
    }

    // 12-8. 최근 이벤트 (TUI 실시간 피드)
    pub fn query_recent_events(&self, limit: usize) -> anyhow::Result<Vec<LiveEvent>> {
        self.with_conn(|conn| {
            let cap = limit as i64;
            let mut stamped: Vec<(String, LiveEvent)> = Vec::new();

            // 1. API 요청
            let mut api_stmt = conn.prepare_cached(
                "SELECT source, request_key, ts, session_id, agent, model, input_tokens, \
                 output_tokens, cache_read_tokens, cache_creation_tokens, duration_ms, \
                 first_byte_ms, first_token_ms, status, cache_creation_5m_tokens, \
                 cache_creation_1h_tokens, project, git_branch FROM api_requests \
                 ORDER BY ts DESC LIMIT ?1",
            )?;
            let mut rows = api_stmt.query(params![cap])?;
            while let Some(row) = rows.next()? {
                let source_text: String = row.get(0)?;
                let api_row = ApiRequestRow {
                    source: parse_source(&source_text),
                    request_key: row.get(1)?,
                    ts: row.get(2)?,
                    session_id: row.get(3)?,
                    agent: row.get(4)?,
                    model: row.get(5)?,
                    input_tokens: row.get(6)?,
                    output_tokens: row.get(7)?,
                    cache_read_tokens: row.get(8)?,
                    cache_creation_tokens: row.get(9)?,
                    duration_ms: row.get(10)?,
                    first_byte_ms: row.get(11)?,
                    first_token_ms: row.get(12)?,
                    status: row.get(13)?,
                    cache_creation_5m_tokens: row.get(14)?,
                    cache_creation_1h_tokens: row.get(15)?,
                    project: row.get(16)?,
                    git_branch: row.get(17)?,
                };
                stamped.push((api_row.ts.clone(), LiveEvent::Api(api_row)));
            }

            // 2. 도구 호출
            let mut tool_stmt = conn.prepare_cached(
                "SELECT source, tool_use_id, ts, session_id, agent, tool_name, duration_ms, \
                 is_error, error_kind, detail FROM tool_calls ORDER BY ts DESC LIMIT ?1",
            )?;
            let mut rows = tool_stmt.query(params![cap])?;
            while let Some(row) = rows.next()? {
                let source_text: String = row.get(0)?;
                let tool_row = ToolCallRow {
                    source: parse_source(&source_text),
                    tool_use_id: row.get(1)?,
                    ts: row.get(2)?,
                    session_id: row.get(3)?,
                    agent: row.get(4)?,
                    tool_name: row.get(5)?,
                    duration_ms: row.get(6)?,
                    is_error: row.get(7)?,
                    error_kind: row.get(8)?,
                    detail: row.get(9)?,
                };
                stamped.push((tool_row.ts.clone(), LiveEvent::Tool(tool_row)));
            }

            // 3. 훅 이벤트
            let mut hook_stmt = conn.prepare_cached(
                "SELECT source, event_key, ts, session_id, route, guard, outcome, detail, \
                 duration_ms, saved_ms FROM hook_events ORDER BY ts DESC LIMIT ?1",
            )?;
            let mut rows = hook_stmt.query(params![cap])?;
            while let Some(row) = rows.next()? {
                let source_text: String = row.get(0)?;
                let outcome_text: String = row.get(6)?;
                let hook_row = HookEventRow {
                    source: parse_source(&source_text),
                    event_key: row.get(1)?,
                    ts: row.get(2)?,
                    session_id: row.get(3)?,
                    route: row.get(4)?,
                    guard: row.get(5)?,
                    outcome: parse_outcome(&outcome_text),
                    detail: row.get(7)?,
                    duration_ms: row.get(8)?,
                    saved_ms: row.get(9)?,
                };
                stamped.push((hook_row.ts.clone(), LiveEvent::Hook(hook_row)));
            }

            // 4. 시각 역순 병합
            stamped.sort_by(|left, right| right.0.cmp(&left.0));
            stamped.truncate(limit);
            Ok(stamped.into_iter().map(|(_, event)| event).collect())
        })
    }

    // 12-9. 출처별 API 요청 건수
    pub fn query_source_counts(&self, since: &str, until: &str) -> anyhow::Result<SourceCounts> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT source, COUNT(*) FROM api_requests \
                 WHERE ts >= ?1 AND ts < ?2 GROUP BY source",
            )?;
            let mut counts = SourceCounts::default();
            let mut rows = stmt.query(params![since, until])?;
            while let Some(row) = rows.next()? {
                let source_text: String = row.get(0)?;
                let total = row.get::<_, i64>(1)? as u64;
                match source_text.as_str() {
                    "transcript" => counts.transcript = total,
                    "otlp" => counts.otlp = total,
                    "gateway" => counts.gateway = total,
                    _ => {}
                }
            }
            Ok(counts)
        })
    }

    // 12-10. 모델별 통계
    pub fn query_model_stats(
        &self,
        since: &str,
        until: &str,
    ) -> anyhow::Result<Vec<ModelStatsRow>> {
        self.with_conn(|conn| {
            // 1. 요청 누적
            let mut stmt = conn.prepare_cached(STATS_REQUESTS_SQL)?;
            let mut grouped: BTreeMap<String, ModelStatsTotals> = BTreeMap::new();
            let mut rows = stmt.query(params![since, until])?;
            while let Some(row) = rows.next()? {
                let model: String = row.get(0)?;
                let session_id: String = row.get(1)?;
                let agent_full: String = row.get(2)?;
                let request_ts: String = row.get(8)?;
                let input_tokens: i64 = row.get(3)?;
                let output_tokens: i64 = row.get(4)?;
                let cache_read_tokens: i64 = row.get(5)?;
                let cache_creation_5m_tokens: i64 = row.get(9)?;
                let cache_creation_1h_tokens: i64 = row.get(10)?;
                let cost = estimate_cost_usd(
                    &model,
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_creation_5m_tokens,
                    cache_creation_1h_tokens,
                );
                let entry = grouped.entry(model).or_default();
                let agent_type = agent_full.split('#').next().unwrap_or("");
                add_share(
                    &mut entry.agents,
                    agent_type,
                    output_tokens,
                    cost,
                    false,
                    &request_ts,
                );
                keep_first_ts(&mut entry.first_ts, &request_ts);
                entry.requests += 1;
                entry.sessions.insert(session_id);
                entry.agent_types.insert(agent_type.to_string());
                entry.input_tokens += input_tokens;
                entry.output_tokens += output_tokens;
                entry.cache_read_tokens += cache_read_tokens;
                entry.cache_creation_tokens += row.get::<_, i64>(6)?;
                entry.cache_creation_5m_tokens += cache_creation_5m_tokens;
                entry.cache_creation_1h_tokens += cache_creation_1h_tokens;
                entry.cost_usd_estimate += cost;
                if let Some(elapsed) = row.get::<_, Option<i64>>(7)? {
                    entry.waits.push(elapsed);
                }
                if request_ts > entry.last_ts {
                    entry.last_ts = request_ts;
                }
            }

            // 2. 행 구성
            let mut stats: Vec<ModelStatsRow> = Vec::new();
            for (model, mut totals) in grouped {
                let context_total =
                    totals.input_tokens + totals.cache_read_tokens + totals.cache_creation_tokens;
                let mut cache_hit_ratio = 0.0;
                if context_total > 0 {
                    cache_hit_ratio = totals.cache_read_tokens as f64 / context_total as f64;
                }
                let (wait_p50_ms, wait_p90_ms) = summarize_percentiles(&mut totals.waits);
                stats.push(ModelStatsRow {
                    cost_usd_estimate: totals.cost_usd_estimate,
                    model,
                    requests: totals.requests,
                    sessions: totals.sessions.len() as u64,
                    agent_types: totals.agent_types.len() as u64,
                    input_tokens: totals.input_tokens,
                    output_tokens: totals.output_tokens,
                    cache_read_tokens: totals.cache_read_tokens,
                    cache_creation_tokens: totals.cache_creation_tokens,
                    cache_creation_5m_tokens: totals.cache_creation_5m_tokens,
                    cache_creation_1h_tokens: totals.cache_creation_1h_tokens,
                    cache_hit_ratio,
                    wait_p50_ms,
                    wait_p90_ms,
                    last_ts: totals.last_ts,
                    first_ts: totals.first_ts,
                    agents: top_shares(totals.agents, SHARE_LIMIT),
                });
            }

            // 3. 비용·요청 순 정렬
            stats.sort_by(|left, right| {
                right
                    .cost_usd_estimate
                    .total_cmp(&left.cost_usd_estimate)
                    .then_with(|| right.requests.cmp(&left.requests))
                    .then_with(|| left.model.cmp(&right.model))
            });
            Ok(stats)
        })
    }

    // 12-11. 에이전트 종류별 통계
    pub fn query_agent_stats(
        &self,
        since: &str,
        until: &str,
    ) -> anyhow::Result<Vec<AgentStatsRow>> {
        self.with_conn(|conn| {
            // 1. 요청 누적 (메인은 세션 기준)
            let mut stmt = conn.prepare_cached(STATS_REQUESTS_SQL)?;
            let mut grouped: BTreeMap<String, AgentStatsTotals> = BTreeMap::new();
            let mut rows = stmt.query(params![since, until])?;
            while let Some(row) = rows.next()? {
                let model: String = row.get(0)?;
                let session_id: String = row.get(1)?;
                let agent_full: String = row.get(2)?;
                let request_ts: String = row.get(8)?;
                let input_tokens: i64 = row.get(3)?;
                let output_tokens: i64 = row.get(4)?;
                let cache_read_tokens: i64 = row.get(5)?;
                let cache_creation_tokens: i64 = row.get(6)?;
                let cache_creation_5m_tokens: i64 = row.get(9)?;
                let cache_creation_1h_tokens: i64 = row.get(10)?;
                let agent = agent_full.split('#').next().unwrap_or("").to_string();
                let run_key = if agent == "main" {
                    session_id.clone()
                } else {
                    agent_full
                };
                let cost = estimate_cost_usd(
                    &model,
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_creation_5m_tokens,
                    cache_creation_1h_tokens,
                );
                let entry = grouped.entry(agent).or_default();
                let run = entry.runs.entry(run_key.clone()).or_default();
                run.requests += 1;
                run.output_tokens += output_tokens;
                run.cost_usd_estimate += cost;
                keep_first_ts(&mut run.first_ts, &request_ts);
                if request_ts >= run.last_ts {
                    run.last_ts = request_ts.clone();
                    run.model = model.clone();
                }
                add_share(
                    &mut entry.models,
                    &model,
                    output_tokens,
                    cost,
                    false,
                    &request_ts,
                );
                keep_first_ts(&mut entry.first_ts, &request_ts);
                entry.run_keys.insert(run_key);
                entry.sessions.insert(session_id);
                entry.requests += 1;
                entry.input_tokens += input_tokens;
                entry.output_tokens += output_tokens;
                entry.cache_read_tokens += cache_read_tokens;
                entry.cache_creation_tokens += cache_creation_tokens;
                entry.cache_creation_5m_tokens += cache_creation_5m_tokens;
                entry.cache_creation_1h_tokens += cache_creation_1h_tokens;
                entry.cost_usd_estimate += cost;
                if let Some(elapsed) = row.get::<_, Option<i64>>(7)? {
                    entry.waits.push(elapsed);
                }
                *entry.model_requests.entry(model).or_default() += 1;
                if request_ts > entry.last_ts {
                    entry.last_ts = request_ts;
                }
            }

            // 2. 도구 호출 누적
            let mut tool_stmt = conn.prepare_cached(STATS_TOOL_CALLS_SQL)?;
            let mut rows = tool_stmt.query(params![since, until])?;
            while let Some(row) = rows.next()? {
                let agent_full: String = row.get(0)?;
                let is_error: bool = row.get(1)?;
                let call_ts: String = row.get(2)?;
                let tool_name: String = row.get(3)?;
                let agent = agent_full.split('#').next().unwrap_or("").to_string();
                let entry = grouped.entry(agent).or_default();
                add_share(&mut entry.tools, &tool_name, 0, 0.0, is_error, &call_ts);
                keep_first_ts(&mut entry.first_ts, &call_ts);
                entry.tool_calls += 1;
                if is_error {
                    entry.tool_errors += 1;
                }
                if call_ts > entry.last_ts {
                    entry.last_ts = call_ts;
                }
            }

            // 3. 행 구성
            let mut stats: Vec<AgentStatsRow> = Vec::new();
            for (agent, mut totals) in grouped {
                let context_total =
                    totals.input_tokens + totals.cache_read_tokens + totals.cache_creation_tokens;
                let mut cache_hit_ratio = 0.0;
                if context_total > 0 {
                    cache_hit_ratio = totals.cache_read_tokens as f64 / context_total as f64;
                }
                let (wait_p50_ms, wait_p90_ms) = summarize_percentiles(&mut totals.waits);
                let top_model = totals
                    .model_requests
                    .iter()
                    .max_by(|left, right| left.1.cmp(right.1).then_with(|| right.0.cmp(left.0)))
                    .map(|(name, _)| name.clone())
                    .unwrap_or_default();
                stats.push(AgentStatsRow {
                    agent,
                    runs: totals.run_keys.len() as u64,
                    requests: totals.requests,
                    sessions: totals.sessions.len() as u64,
                    input_tokens: totals.input_tokens,
                    output_tokens: totals.output_tokens,
                    cache_read_tokens: totals.cache_read_tokens,
                    cache_creation_tokens: totals.cache_creation_tokens,
                    cache_creation_5m_tokens: totals.cache_creation_5m_tokens,
                    cache_creation_1h_tokens: totals.cache_creation_1h_tokens,
                    cache_hit_ratio,
                    wait_p50_ms,
                    wait_p90_ms,
                    cost_usd_estimate: totals.cost_usd_estimate,
                    tool_calls: totals.tool_calls,
                    tool_errors: totals.tool_errors,
                    top_model,
                    last_ts: totals.last_ts,
                    first_ts: totals.first_ts,
                    models: top_shares(totals.models, SHARE_LIMIT),
                    top_tools: top_shares(totals.tools, SHARE_LIMIT),
                    recent_runs: recent_runs(totals.runs),
                });
            }

            // 4. 비용·요청 순 정렬
            stats.sort_by(|left, right| {
                right
                    .cost_usd_estimate
                    .total_cmp(&left.cost_usd_estimate)
                    .then_with(|| right.requests.cmp(&left.requests))
                    .then_with(|| left.agent.cmp(&right.agent))
            });
            Ok(stats)
        })
    }
}

// 12-12. 프로젝트·브랜치별 통계
impl Store {
    pub fn query_project_stats(
        &self,
        since: &str,
        until: &str,
    ) -> anyhow::Result<Vec<ProjectStatsRow>> {
        self.with_conn(|conn| {
            // 1. 요청 누적 (프로젝트·브랜치 쌍별, 비용은 요청 단위)
            let mut stmt = conn.prepare_cached(PROJECT_REQUESTS_SQL)?;
            let mut grouped: BTreeMap<(String, String), ProjectStatsTotals> = BTreeMap::new();
            let mut rows = stmt.query(params![since, until])?;
            while let Some(row) = rows.next()? {
                let project: String = row.get(0)?;
                let git_branch: String = row.get(1)?;
                let session_id: String = row.get(2)?;
                let model: String = row.get(3)?;
                let input_tokens: i64 = row.get(4)?;
                let output_tokens: i64 = row.get(5)?;
                let cache_read_tokens: i64 = row.get(6)?;
                let cache_creation_5m_tokens: i64 = row.get(7)?;
                let cache_creation_1h_tokens: i64 = row.get(8)?;
                let request_ts: String = row.get(9)?;
                let agent_full: String = row.get(10)?;
                let cost = estimate_cost_usd(
                    &model,
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_creation_5m_tokens,
                    cache_creation_1h_tokens,
                );
                let entry = grouped.entry((project, git_branch)).or_default();
                let agent_type = agent_full.split('#').next().unwrap_or("");
                add_share(
                    &mut entry.models,
                    &model,
                    output_tokens,
                    cost,
                    false,
                    &request_ts,
                );
                add_share(
                    &mut entry.agents,
                    agent_type,
                    output_tokens,
                    cost,
                    false,
                    &request_ts,
                );
                add_share(
                    &mut entry.sessions_usage,
                    &session_id,
                    output_tokens,
                    cost,
                    false,
                    &request_ts,
                );
                keep_first_ts(&mut entry.first_ts, &request_ts);
                entry.requests += 1;
                entry.sessions.insert(session_id);
                entry.input_tokens += input_tokens;
                entry.output_tokens += output_tokens;
                entry.cache_read_tokens += cache_read_tokens;
                entry.cache_creation_5m_tokens += cache_creation_5m_tokens;
                entry.cache_creation_1h_tokens += cache_creation_1h_tokens;
                entry.cost_usd_estimate += cost;
                if request_ts > entry.last_ts {
                    entry.last_ts = request_ts;
                }
            }

            // 2. 행 구성
            let mut stats: Vec<ProjectStatsRow> = Vec::new();
            for ((project, git_branch), totals) in grouped {
                stats.push(ProjectStatsRow {
                    project,
                    git_branch,
                    requests: totals.requests,
                    sessions: totals.sessions.len() as u64,
                    input_tokens: totals.input_tokens,
                    output_tokens: totals.output_tokens,
                    cache_read_tokens: totals.cache_read_tokens,
                    cache_creation_5m_tokens: totals.cache_creation_5m_tokens,
                    cache_creation_1h_tokens: totals.cache_creation_1h_tokens,
                    cost_usd_estimate: totals.cost_usd_estimate,
                    last_ts: totals.last_ts,
                    first_ts: totals.first_ts,
                    models: top_shares(totals.models, SHARE_LIMIT),
                    agents: top_shares(totals.agents, SHARE_LIMIT),
                    top_sessions: top_shares(totals.sessions_usage, SHARE_LIMIT),
                });
            }

            // 3. 비용·요청 순 정렬
            stats.sort_by(|left, right| {
                right
                    .cost_usd_estimate
                    .total_cmp(&left.cost_usd_estimate)
                    .then_with(|| right.requests.cmp(&left.requests))
                    .then_with(|| left.project.cmp(&right.project))
                    .then_with(|| left.git_branch.cmp(&right.git_branch))
            });
            Ok(stats)
        })
    }

    // 12-13. 스킬별 호출 통계 (호출 수·이름 순, 이름이 빈 행은 백필 전 기록)
    pub fn query_skill_stats(
        &self,
        since: &str,
        until: &str,
    ) -> anyhow::Result<Vec<SkillStatsRow>> {
        self.with_conn(|conn| {
            // 1. 호출 누적
            let mut stmt = conn.prepare_cached(SKILL_CALLS_SQL)?;
            let mut grouped: BTreeMap<String, SkillStatsTotals> = BTreeMap::new();
            let mut rows = stmt.query(params![since, until])?;
            while let Some(row) = rows.next()? {
                let skill: String = row.get(0)?;
                let agent_full: String = row.get(1)?;
                let session_id: String = row.get(2)?;
                let call_ts: String = row.get(3)?;
                let is_error: bool = row.get(4)?;
                let agent_type = agent_full.split('#').next().unwrap_or("");
                let entry = grouped.entry(skill).or_default();
                entry.calls += 1;
                entry.errors += u64::from(is_error);
                entry.sessions.insert(session_id);
                keep_first_ts(&mut entry.first_ts, &call_ts);
                add_share(&mut entry.agents, agent_type, 0, 0.0, is_error, &call_ts);
                entry.recent.push(UsageShare {
                    name: agent_type.to_string(),
                    count: 1,
                    errors: u64::from(is_error),
                    last_ts: call_ts,
                    ..UsageShare::default()
                });
            }

            // 2. 행 구성
            let mut stats: Vec<SkillStatsRow> = grouped
                .into_iter()
                .map(|(skill, mut totals)| {
                    totals
                        .recent
                        .sort_by(|left, right| right.last_ts.cmp(&left.last_ts));
                    totals.recent.truncate(RECENT_LIMIT);
                    SkillStatsRow {
                        skill,
                        calls: totals.calls,
                        errors: totals.errors,
                        sessions: totals.sessions.len() as u64,
                        last_ts: totals
                            .recent
                            .first()
                            .map(|call| call.last_ts.clone())
                            .unwrap_or_default(),
                        first_ts: totals.first_ts,
                        agents: top_shares(totals.agents, SHARE_LIMIT),
                        recent: totals.recent,
                    }
                })
                .collect();

            // 3. 호출 수·이름 순 정렬
            stats.sort_by(|left, right| {
                right
                    .calls
                    .cmp(&left.calls)
                    .then_with(|| left.skill.cmp(&right.skill))
            });
            Ok(stats)
        })
    }
}

// 12-14. 마지막 활동 순 최근 실행
fn recent_runs(runs: BTreeMap<String, AgentRunSummary>) -> Vec<AgentRunSummary> {
    let mut ordered: Vec<AgentRunSummary> = runs
        .into_iter()
        .map(|(run, summary)| AgentRunSummary { run, ..summary })
        .collect();
    ordered.sort_by(|left, right| {
        right
            .last_ts
            .cmp(&left.last_ts)
            .then_with(|| left.run.cmp(&right.run))
    });
    ordered.truncate(RECENT_LIMIT);
    ordered
}

// 13. 출처·판정 문자열 복원 ---------------------------------------------------
fn parse_source(text: &str) -> DataSource {
    match text {
        "otlp" => DataSource::Otlp,
        "gateway" => DataSource::Gateway,
        "hook" => DataSource::Hook,
        _ => DataSource::Transcript,
    }
}

fn parse_outcome(text: &str) -> HookOutcome {
    match text {
        "deny" => HookOutcome::Deny,
        "rewrite" => HookOutcome::Rewrite,
        "error" => HookOutcome::Error,
        _ => HookOutcome::Allow,
    }
}

// 14. 단위 시험 ---------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{IngestBatch, SessionRow};
    use crate::transcript::test_support::{remove_temp_dir, unique_temp_dir};

    fn api_row(key: &str, ts: &str, model: &str, read: i64, create: i64) -> ApiRequestRow {
        ApiRequestRow {
            source: DataSource::Transcript,
            request_key: key.to_string(),
            ts: ts.to_string(),
            session_id: "sess-1".to_string(),
            agent: "main".to_string(),
            model: model.to_string(),
            input_tokens: 10,
            output_tokens: 50,
            cache_read_tokens: read,
            cache_creation_tokens: create,
            duration_ms: Some(1000),
            ..ApiRequestRow::default()
        }
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let mut values = vec![10, 50, 20, 40, 30, 60, 80, 70, 90, 100];
        assert_eq!(summarize_percentiles(&mut values), (Some(50), Some(90)));
        let mut empty: Vec<i64> = Vec::new();
        assert_eq!(summarize_percentiles(&mut empty), (None, None));
    }

    // 토큰 다섯 칸 (입력, 출력, 캐시 읽기, 5분 쓰기, 1시간 쓰기) 각 1M 의 비용이 단가 합과 같은지 확인
    fn assert_million_cost(model: &str, expected: f64) {
        let million = 1_000_000;
        let cost = estimate_cost_usd(model, million, million, million, million, million);
        assert!(
            (cost - expected).abs() < 1e-9,
            "{model}: {cost} != {expected}"
        );
    }

    #[test]
    fn cost_follows_rate_table() {
        // 1. 모델별 단가 (입력 + 출력 + 읽기 + 5분 쓰기 + 1시간 쓰기)
        assert_million_cost("claude-opus-5-5", 4.0 + 20.0 + 0.20 + 5.0 + 8.0);
        for model in [
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6-20260101",
            "claude-opus-4-5",
        ] {
            assert_million_cost(model, 5.0 + 25.0 + 0.50 + 6.25 + 10.0);
        }
        for model in ["claude-opus-4-1-20250805", "claude-opus-4-20250514"] {
            assert_million_cost(model, 15.0 + 75.0 + 1.50 + 18.75 + 30.0);
        }
        assert_million_cost("claude-sonnet-4-20250514", 3.0 + 15.0 + 0.30 + 3.75 + 6.0);
        assert_million_cost("claude-sonnet-5-5", 2.0 + 10.0 + 0.10 + 2.50 + 4.0);
        assert_million_cost("claude-sonnet-5", 2.0 + 10.0 + 0.20 + 2.50 + 4.0);
        assert_million_cost("claude-sonnet-4-6", 3.0 + 15.0 + 0.30 + 3.75 + 6.0);
        assert_million_cost("claude-sonnet-4-5-20250929", 3.0 + 15.0 + 0.30 + 3.75 + 6.0);
        assert_million_cost("claude-haiku-4-5-20251001", 1.0 + 5.0 + 0.10 + 1.25 + 2.0);
        assert_million_cost("claude-fable-5-2", 8.0 + 40.0 + 0.20 + 10.0 + 16.0);
        assert_million_cost("claude-fable-5-1", 10.0 + 50.0 + 0.25 + 12.50 + 20.0);
        assert_million_cost("claude-fable-5-3", 10.0 + 50.0 + 1.0 + 12.50 + 20.0);

        // 2. 모르는 모델·매핑 없는 계열은 0, claude- 접두사 없이도 매칭
        assert_eq!(estimate_cost_usd("claude-unknown-9", 5, 5, 5, 5, 5), 0.0);
        assert_eq!(estimate_cost_usd("claude-sonnet-3-7", 5, 5, 5, 5, 5), 0.0);
        assert_eq!(estimate_cost_usd("", 5, 5, 5, 5, 5), 0.0);
        assert!((estimate_cost_usd("opus-5-5", 1_000_000, 0, 0, 0, 0) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn haiku_five_five_switches_rate_above_one_hundred_thousand_prompt_tokens() {
        // haiku-5-5 하위 0.10/0.50/0.125/0.20/0.01, 상위 0.50/2.50/0.625/1/0.05 (공식 단가표)
        // 1. 프롬프트 정확히 100k (입력 + 읽기 + 쓰기): 하위 단가
        let at_limit = estimate_cost_usd("claude-haiku-5-5", 50_000, 10_000, 30_000, 20_000, 0);
        let expected_low =
            (50_000.0 * 0.10 + 10_000.0 * 0.50 + 30_000.0 * 0.01 + 20_000.0 * 0.125) / 1_000_000.0;
        assert!((at_limit - expected_low).abs() < 1e-12, "{at_limit}");

        // 2. 1토큰 초과: 상위 단가
        let above = estimate_cost_usd("claude-haiku-5-5", 50_001, 10_000, 30_000, 20_000, 0);
        let expected_high =
            (50_001.0 * 0.50 + 10_000.0 * 2.50 + 30_000.0 * 0.05 + 20_000.0 * 0.625) / 1_000_000.0;
        assert!((above - expected_high).abs() < 1e-12, "{above}");

        // 3. 1시간 쓰기도 프롬프트에 포함
        let with_hour = estimate_cost_usd("claude-haiku-5-5", 50_000, 0, 30_000, 10_000, 10_001);
        let expected_hour =
            (50_000.0 * 0.50 + 30_000.0 * 0.05 + 10_000.0 * 0.625 + 10_001.0 * 1.0) / 1_000_000.0;
        assert!((with_hour - expected_hour).abs() < 1e-12, "{with_hour}");
    }

    #[test]
    fn price_overrides_replace_matching_models_only() {
        let mut overrides = PricingConfig::default();
        overrides.models.insert(
            "opus-5-5".to_string(),
            ModelRateConfig {
                input: 1.0,
                output: 2.0,
                cache_write_5m: Some(3.0),
                cache_write_1h: None,
                cache_read: Some(0.5),
            },
        );
        overrides.models.insert(
            "claude-brand-new".to_string(),
            ModelRateConfig {
                input: 10.0,
                output: 20.0,
                cache_write_5m: None,
                cache_write_1h: None,
                cache_read: None,
            },
        );
        overrides.models.insert(
            "haiku-5-5".to_string(),
            ModelRateConfig {
                input: 0.10,
                output: 0.50,
                cache_write_5m: None,
                cache_write_1h: None,
                cache_read: None,
            },
        );
        let million = 1_000_000;
        let cost = |model: &str| {
            estimate_cost_with_overrides(
                model, million, million, million, million, million, &overrides,
            )
        };

        // 1. 덮어쓴 모델 (생략한 쓰기 1시간은 입력의 2배)
        assert!((cost("claude-opus-5-5-20260101") - (1.0 + 2.0 + 0.5 + 3.0 + 2.0)).abs() < 1e-9);

        // 2. 내장표에 없는 모델 추가 (기본 배수)
        assert!((cost("claude-brand-new-1") - (10.0 + 20.0 + 1.0 + 12.5 + 20.0)).abs() < 1e-9);

        // 3. 다른 모델은 내장표 유지, 덮어쓴 haiku 는 프롬프트가 커도 단일 단가
        assert!((cost("claude-opus-5") - (5.0 + 25.0 + 0.5 + 6.25 + 10.0)).abs() < 1e-9);
        assert!((cost("claude-haiku-5-5") - (0.10 + 0.50 + 0.01 + 0.125 + 0.20)).abs() < 1e-9);
        assert_eq!(
            estimate_cost_with_overrides("claude-unknown-9", 1, 1, 1, 1, 1, &overrides),
            0.0
        );
    }

    #[test]
    fn queries_cover_seeded_rows() {
        // 1. 시드 적재
        let dir = unique_temp_dir("query-seed");
        let store = Store::open_store(&dir.join("cm.sqlite")).expect("DB 열기");
        let batch = IngestBatch {
            api_requests: vec![
                api_row(
                    "m1",
                    "2026-11-10T12:00:00.000Z",
                    "claude-opus-5-5",
                    0,
                    30_000,
                ),
                api_row(
                    "m2",
                    "2026-11-10T12:00:20.000Z",
                    "claude-opus-5-5",
                    30_000,
                    500,
                ),
                api_row(
                    "m3",
                    "2026-11-10T12:10:00.000Z",
                    "claude-opus-5-5",
                    1_000,
                    25_000,
                ),
                api_row(
                    "m4",
                    "2026-11-10T12:10:30.000Z",
                    "claude-sonnet-5",
                    100,
                    22_000,
                ),
                ApiRequestRow {
                    source: DataSource::Otlp,
                    ..api_row(
                        "otlp-1",
                        "2026-11-10T12:00:05.000Z",
                        "claude-opus-5-5",
                        0,
                        1,
                    )
                },
            ],
            tool_calls: vec![ToolCallRow {
                source: DataSource::Transcript,
                tool_use_id: "tu1".to_string(),
                ts: "2026-11-10T12:00:10.000Z".to_string(),
                session_id: "sess-1".to_string(),
                agent: "main".to_string(),
                tool_name: "Read".to_string(),
                duration_ms: Some(200),
                ..ToolCallRow::default()
            }],
            hook_events: vec![
                HookEventRow {
                    source: DataSource::Transcript,
                    event_key: "transcript:tu1:deny".to_string(),
                    ts: "2026-11-10T12:00:11.000Z".to_string(),
                    session_id: "sess-1".to_string(),
                    route: "PreToolUse:Bash".to_string(),
                    guard: "cli-guard".to_string(),
                    outcome: HookOutcome::Deny,
                    detail: "cat".to_string(),
                    ..HookEventRow::default()
                },
                HookEventRow {
                    source: DataSource::Hook,
                    event_key: "hook:1".to_string(),
                    ts: "2026-11-10T12:00:11.500Z".to_string(),
                    session_id: "sess-1".to_string(),
                    route: "PreToolUse:Bash".to_string(),
                    guard: "cli-guard".to_string(),
                    outcome: HookOutcome::Deny,
                    detail: "cat".to_string(),
                    ..HookEventRow::default()
                },
                HookEventRow {
                    source: DataSource::Hook,
                    event_key: "hook:2".to_string(),
                    ts: "2026-11-10T12:01:00.000Z".to_string(),
                    route: "PreToolUse:Bash".to_string(),
                    guard: "rewrite".to_string(),
                    outcome: HookOutcome::Rewrite,
                    saved_ms: Some(4200),
                    ..HookEventRow::default()
                },
            ],
            sessions: vec![SessionRow {
                session_id: "sess-1".to_string(),
                project: "proj".to_string(),
                first_ts: "2026-11-10T12:00:00.000Z".to_string(),
                last_ts: "2026-11-10T12:10:30.000Z".to_string(),
            }],
            ..IngestBatch::default()
        };
        store.insert_batch(&batch).expect("시드 적재");
        let since = "2026-11-10T00:00:00.000Z";
        let until = "2026-11-11T00:00:00.000Z";

        // 2. 개요 (OTLP 제외, 거부 이중 집계 방지)
        let overview = store.query_overview(since, until).expect("개요");
        assert_eq!(overview.api_requests, 4);
        assert_eq!(overview.hook_denials, 1);
        assert_eq!(overview.hook_rewrites, 1);
        assert_eq!(overview.saved_ms, 4200);
        assert_eq!(overview.lost_ms_estimate, DENY_ROUND_TRIP_MS);
        assert_eq!(overview.model_wait_p50_ms, Some(1000));
        assert!(overview.cache_hit_ratio > 0.0 && overview.cache_hit_ratio < 1.0);

        // 3. 캐시 붕괴 (유휴 초과, 모델 변경)
        let breaks = store.query_cache_breaks(since, until, 10).expect("붕괴");
        assert_eq!(breaks.len(), 2);
        assert_eq!(breaks[0].cache_creation_tokens, 25_000);
        assert_eq!(breaks[0].likely_cause, "유휴 5분 초과 (TTL)");
        assert_eq!(breaks[1].likely_cause, "모델 변경");

        // 4. 모델 사용량·분 추이·최근 이벤트
        let usage = store.query_model_usage(since, until).expect("사용량");
        assert_eq!(usage.len(), 2);
        assert_eq!(usage[0].model, "claude-opus-5-5");
        assert!(usage[0].cost_usd_estimate > 0.0);
        let minutes = store.query_minute_buckets(since, until).expect("분 추이");
        assert_eq!(
            minutes.first().map(|bucket| bucket.minute.as_str()),
            Some("2026-11-10T12:00:00Z")
        );
        assert_eq!(minutes[0].hook_denials, 1);
        let recent = store.query_recent_events(3).expect("최근");
        assert_eq!(recent.len(), 3);
        let counts = store.query_source_counts(since, until).expect("출처");
        assert_eq!((counts.transcript, counts.otlp), (4, 1));
        drop(store);
        remove_temp_dir(&dir);
    }

    const STATS_SINCE: &str = "2026-11-10T00:00:00.000Z";
    const STATS_UNTIL: &str = "2026-11-11T00:00:00.000Z";

    fn stats_api_row(
        key: &str,
        ts: &str,
        session_id: &str,
        agent: &str,
        model: &str,
        duration_ms: Option<i64>,
    ) -> ApiRequestRow {
        ApiRequestRow {
            source: DataSource::Transcript,
            request_key: key.to_string(),
            ts: ts.to_string(),
            session_id: session_id.to_string(),
            agent: agent.to_string(),
            model: model.to_string(),
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 800,
            cache_creation_tokens: 100,
            duration_ms,
            ..ApiRequestRow::default()
        }
    }

    fn stats_tool_row(
        key: &str,
        ts: &str,
        agent: &str,
        is_error: bool,
        source: DataSource,
    ) -> ToolCallRow {
        ToolCallRow {
            source,
            tool_use_id: key.to_string(),
            ts: ts.to_string(),
            session_id: "sess-a".to_string(),
            agent: agent.to_string(),
            tool_name: "Bash".to_string(),
            duration_ms: Some(10),
            is_error,
            ..ToolCallRow::default()
        }
    }

    // 통계 시드 (메인·하위 두 종류·모델 두 개)
    fn seed_stats_store(label: &str) -> (Store, std::path::PathBuf) {
        let dir = unique_temp_dir(label);
        let store = Store::open_store(&dir.join("cm.sqlite")).expect("DB 열기");
        let opus = "claude-opus-5-5";
        let sonnet = "claude-sonnet-5";
        let batch = IngestBatch {
            api_requests: vec![
                stats_api_row(
                    "a1",
                    "2026-11-10T12:00:00.000Z",
                    "sess-a",
                    "main",
                    opus,
                    Some(1000),
                ),
                stats_api_row(
                    "a2",
                    "2026-11-10T12:01:00.000Z",
                    "sess-a",
                    "main",
                    opus,
                    Some(3000),
                ),
                stats_api_row(
                    "a3",
                    "2026-11-10T12:02:00.000Z",
                    "sess-a",
                    "main",
                    sonnet,
                    Some(2000),
                ),
                stats_api_row(
                    "w1",
                    "2026-11-10T12:03:00.000Z",
                    "sess-a",
                    "worker#w1",
                    opus,
                    Some(4000),
                ),
                stats_api_row(
                    "w2",
                    "2026-11-10T12:04:00.000Z",
                    "sess-a",
                    "worker#w1",
                    opus,
                    Some(6000),
                ),
                stats_api_row(
                    "w3",
                    "2026-11-10T12:05:00.000Z",
                    "sess-a",
                    "worker#w2",
                    sonnet,
                    Some(5000),
                ),
                stats_api_row(
                    "e1",
                    "2026-11-10T12:06:00.000Z",
                    "sess-a",
                    "explorer#e1",
                    sonnet,
                    None,
                ),
                stats_api_row(
                    "b1",
                    "2026-11-10T13:00:00.000Z",
                    "sess-b",
                    "main",
                    opus,
                    Some(1000),
                ),
                stats_api_row(
                    "b2",
                    "2026-11-10T13:01:00.000Z",
                    "sess-b",
                    "worker#w3",
                    sonnet,
                    Some(2000),
                ),
                ApiRequestRow {
                    source: DataSource::Otlp,
                    ..stats_api_row(
                        "otlp-1",
                        "2026-11-10T12:00:30.000Z",
                        "sess-a",
                        "main",
                        opus,
                        Some(9),
                    )
                },
                stats_api_row(
                    "out-of-range",
                    "2026-11-11T00:00:00.000Z",
                    "sess-z",
                    "main",
                    opus,
                    Some(9),
                ),
            ],
            tool_calls: vec![
                stats_tool_row(
                    "t1",
                    "2026-11-10T12:00:10.000Z",
                    "main",
                    false,
                    DataSource::Transcript,
                ),
                stats_tool_row(
                    "t2",
                    "2026-11-10T12:01:10.000Z",
                    "main",
                    true,
                    DataSource::Transcript,
                ),
                stats_tool_row(
                    "t3",
                    "2026-11-10T12:03:10.000Z",
                    "worker#w1",
                    true,
                    DataSource::Transcript,
                ),
                stats_tool_row(
                    "t4",
                    "2026-11-10T12:06:10.000Z",
                    "explorer#e1",
                    false,
                    DataSource::Transcript,
                ),
                stats_tool_row(
                    "t5",
                    "2026-11-10T13:30:00.000Z",
                    "explorer#e1",
                    false,
                    DataSource::Transcript,
                ),
                stats_tool_row(
                    "t6",
                    "2026-11-10T12:00:20.000Z",
                    "main",
                    true,
                    DataSource::Otlp,
                ),
            ],
            ..IngestBatch::default()
        };
        store.insert_batch(&batch).expect("시드 적재");
        (store, dir)
    }

    #[test]
    fn model_stats_aggregate_per_model() {
        let (store, dir) = seed_stats_store("model-stats");
        let stats = store
            .query_model_stats(STATS_SINCE, STATS_UNTIL)
            .expect("모델 통계");
        assert_eq!(stats.len(), 2);

        // 1. 비용 큰 순서 (opus 5건, sonnet 4건)
        let opus = &stats[0];
        assert_eq!(opus.model, "claude-opus-5-5");
        assert_eq!(opus.requests, 5);
        assert_eq!(opus.sessions, 2);
        assert_eq!(opus.agent_types, 2);
        assert_eq!(opus.input_tokens, 500);
        assert_eq!(opus.output_tokens, 250);
        assert_eq!(opus.cache_read_tokens, 4000);
        assert_eq!(opus.cache_creation_tokens, 500);
        assert!((opus.cache_hit_ratio - 0.8).abs() < 1e-9);
        assert_eq!(
            (opus.wait_p50_ms, opus.wait_p90_ms),
            (Some(3000), Some(6000))
        );
        assert_eq!(opus.last_ts, "2026-11-10T13:00:00.000Z");
        let expected_cost = estimate_cost_usd("claude-opus-5-5", 500, 250, 4000, 500, 0);
        assert!((opus.cost_usd_estimate - expected_cost).abs() < 1e-12);

        // 2. 대기 없는 요청은 백분위 제외, 에이전트 종류 3
        let sonnet = &stats[1];
        assert_eq!(sonnet.model, "claude-sonnet-5");
        assert_eq!(sonnet.requests, 4);
        assert_eq!(sonnet.sessions, 2);
        assert_eq!(sonnet.agent_types, 3);
        assert_eq!(
            (sonnet.wait_p50_ms, sonnet.wait_p90_ms),
            (Some(2000), Some(5000))
        );
        assert_eq!(sonnet.last_ts, "2026-11-10T13:01:00.000Z");
        drop(store);
        remove_temp_dir(&dir);
    }

    #[test]
    fn agent_stats_aggregate_per_agent_type() {
        let (store, dir) = seed_stats_store("agent-stats");
        let stats = store
            .query_agent_stats(STATS_SINCE, STATS_UNTIL)
            .expect("에이전트 통계");
        let names: Vec<&str> = stats.iter().map(|row| row.agent.as_str()).collect();
        assert_eq!(names, ["main", "worker", "explorer"]);

        // 1. 메인 (실행은 세션 수, OTLP·구간 밖 제외)
        let main = &stats[0];
        assert_eq!((main.runs, main.requests, main.sessions), (2, 4, 2));
        assert_eq!(
            (main.wait_p50_ms, main.wait_p90_ms),
            (Some(1000), Some(3000))
        );
        assert_eq!(main.top_model, "claude-opus-5-5");
        assert_eq!((main.tool_calls, main.tool_errors), (2, 1));
        assert_eq!(main.input_tokens, 400);
        assert!((main.cache_hit_ratio - 0.8).abs() < 1e-9);
        let main_cost = 3.0 * estimate_cost_usd("claude-opus-5-5", 100, 50, 800, 100, 0)
            + estimate_cost_usd("claude-sonnet-5", 100, 50, 800, 100, 0);
        assert!((main.cost_usd_estimate - main_cost).abs() < 1e-12);

        // 2. 워커 (실행은 '#' 포함 전체 라벨 3개, 최다 모델 동률은 이름순)
        let worker = &stats[1];
        assert_eq!((worker.runs, worker.requests, worker.sessions), (3, 4, 2));
        assert_eq!(
            (worker.wait_p50_ms, worker.wait_p90_ms),
            (Some(4000), Some(6000))
        );
        assert_eq!(worker.top_model, "claude-opus-5-5");
        assert_eq!((worker.tool_calls, worker.tool_errors), (1, 1));

        // 3. 탐색기 (대기 없음, 마지막 시각은 도구 호출 포함)
        let explorer = &stats[2];
        assert_eq!((explorer.runs, explorer.requests), (1, 1));
        assert_eq!((explorer.wait_p50_ms, explorer.wait_p90_ms), (None, None));
        assert_eq!(explorer.top_model, "claude-sonnet-5");
        assert_eq!((explorer.tool_calls, explorer.tool_errors), (2, 0));
        assert_eq!(explorer.last_ts, "2026-11-10T13:30:00.000Z");
        drop(store);
        remove_temp_dir(&dir);
    }

    #[test]
    fn stats_queries_seek_timestamp_indexes() {
        let (store, dir) = seed_stats_store("stats-plan");
        let plans: Vec<String> = [STATS_REQUESTS_SQL, STATS_TOOL_CALLS_SQL]
            .iter()
            .map(|sql| {
                store
                    .with_conn(|conn| {
                        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                        let details: Vec<String> = stmt
                            .query_map(params![STATS_SINCE, STATS_UNTIL], |row| row.get(3))?
                            .collect::<Result<_, _>>()?;
                        Ok(details.join("\n"))
                    })
                    .expect("계획 조회")
            })
            .collect();
        assert!(plans[0].contains("idx_api_requests_ts"), "{}", plans[0]);
        assert!(plans[1].contains("idx_tool_calls_ts"), "{}", plans[1]);
        drop(store);
        remove_temp_dir(&dir);
    }

    // 크기 분리 시드 행 (매개: 키, 모델, 입력, 읽기, 쓰기 합계, (5분, 1시간))
    fn split_api_row(
        key: &str,
        model: &str,
        input: i64,
        read: i64,
        total: i64,
        split: (i64, i64),
    ) -> ApiRequestRow {
        ApiRequestRow {
            source: DataSource::Transcript,
            request_key: key.to_string(),
            ts: "2026-11-10T12:00:00.000Z".to_string(),
            session_id: "sess-s".to_string(),
            agent: "main".to_string(),
            model: model.to_string(),
            input_tokens: input,
            output_tokens: 1_000,
            cache_read_tokens: read,
            cache_creation_tokens: total,
            cache_creation_5m_tokens: split.0,
            cache_creation_1h_tokens: split.1,
            ..ApiRequestRow::default()
        }
    }

    #[test]
    fn cache_split_feeds_stats_and_per_request_cost() {
        // 1. 시드 (이전 행은 분리 없음, 새 행은 5분·1시간, haiku 두 건은 프롬프트 크기가 다름)
        let dir = unique_temp_dir("query-split");
        let store = Store::open_store(&dir.join("cm.sqlite")).expect("DB 열기");
        let opus = "claude-opus-5-5";
        let haiku = "claude-haiku-5-5";
        let rows = vec![
            split_api_row("old", opus, 100, 0, 1_000, (0, 0)),
            split_api_row("new", opus, 100, 0, 3_000, (1_000, 2_000)),
            split_api_row("big", haiku, 60_000, 50_000, 5_000, (5_000, 0)),
            split_api_row("small", haiku, 1_000, 0, 0, (0, 0)),
        ];
        store
            .insert_batch(&IngestBatch {
                api_requests: rows,
                ..IngestBatch::default()
            })
            .expect("시드 적재");

        // 2. 모델 통계 (이전 행의 쓰기 합계는 5분, 종합 합계는 유지)
        let stats = store
            .query_model_stats(STATS_SINCE, STATS_UNTIL)
            .expect("모델 통계");
        let opus_row = stats.iter().find(|row| row.model == opus).expect("opus");
        assert_eq!(opus_row.cache_creation_tokens, 4_000);
        assert_eq!(opus_row.cache_creation_5m_tokens, 2_000);
        assert_eq!(opus_row.cache_creation_1h_tokens, 2_000);
        let expected_opus = estimate_cost_usd(opus, 100, 1_000, 0, 1_000, 0)
            + estimate_cost_usd(opus, 100, 1_000, 0, 1_000, 2_000);
        assert!((opus_row.cost_usd_estimate - expected_opus).abs() < 1e-12);

        // 3. haiku 비용은 요청별 단계 (합산 토큰으로 한 번에 계산한 값과 달라야 함)
        let haiku_row = stats.iter().find(|row| row.model == haiku).expect("haiku");
        let per_request = estimate_cost_usd(haiku, 60_000, 1_000, 50_000, 5_000, 0)
            + estimate_cost_usd(haiku, 1_000, 1_000, 0, 0, 0);
        let from_sums = estimate_cost_usd(haiku, 61_000, 2_000, 50_000, 5_000, 0);
        assert!((haiku_row.cost_usd_estimate - per_request).abs() < 1e-12);
        assert!((per_request - from_sums).abs() > 1e-6);

        // 4. 모델 사용량·에이전트 통계도 같은 비용
        let usage = store
            .query_model_usage(STATS_SINCE, STATS_UNTIL)
            .expect("사용량");
        let usage_haiku = usage.iter().find(|row| row.model == haiku).expect("haiku");
        assert!((usage_haiku.cost_usd_estimate - per_request).abs() < 1e-12);
        assert_eq!(usage_haiku.cache_creation_5m_tokens, 5_000);
        let usage_opus = usage.iter().find(|row| row.model == opus).expect("opus");
        assert_eq!(usage_opus.cache_creation_5m_tokens, 2_000);
        assert_eq!(usage_opus.cache_creation_1h_tokens, 2_000);
        assert!((usage_opus.cost_usd_estimate - expected_opus).abs() < 1e-12);
        let agents = store
            .query_agent_stats(STATS_SINCE, STATS_UNTIL)
            .expect("에이전트 통계");
        assert_eq!(agents[0].cache_creation_5m_tokens, 7_000);
        assert_eq!(agents[0].cache_creation_1h_tokens, 2_000);
        assert!((agents[0].cost_usd_estimate - (expected_opus + per_request)).abs() < 1e-12);
        drop(store);
        remove_temp_dir(&dir);
    }

    // 프로젝트 시드 행 (매개: 키, 프로젝트, 브랜치, 세션, 시각)
    fn project_api_row(
        key: &str,
        project: &str,
        branch: &str,
        session_id: &str,
        ts: &str,
    ) -> ApiRequestRow {
        ApiRequestRow {
            project: project.to_string(),
            git_branch: branch.to_string(),
            session_id: session_id.to_string(),
            ..api_row(key, ts, "claude-opus-5-5", 0, 0)
        }
    }

    #[test]
    fn project_stats_group_by_project_and_branch() {
        // 1. 시드 (플래그·구간 밖·OTLP 행은 제외, 프로젝트 비어 있으면 세션 폴더명)
        let dir = unique_temp_dir("query-project");
        let store = Store::open_store(&dir.join("cm.sqlite")).expect("DB 열기");
        let mut grown =
            project_api_row("a2", "alpha", "main", "sess-1", "2026-11-10T12:05:00.000Z");
        grown.cache_creation_tokens = 3_000;
        grown.cache_creation_5m_tokens = 1_000;
        grown.cache_creation_1h_tokens = 2_000;
        let batch = IngestBatch {
            api_requests: vec![
                project_api_row("a1", "alpha", "main", "sess-1", "2026-11-10T12:00:00.000Z"),
                grown,
                project_api_row("a3", "alpha", "main", "sess-2", "2026-11-10T12:10:00.000Z"),
                project_api_row(
                    "f1",
                    "alpha",
                    "feature/x",
                    "sess-3",
                    "2026-11-10T13:00:00.000Z",
                ),
                project_api_row("s1", "", "", "sess-4", "2026-11-10T14:00:00.000Z"),
                project_api_row("n1", "", "", "sess-none", "2026-11-10T14:01:00.000Z"),
                ApiRequestRow {
                    source: DataSource::Otlp,
                    ..project_api_row("o1", "alpha", "main", "sess-1", "2026-11-10T12:01:00.000Z")
                },
                project_api_row("out", "alpha", "main", "sess-1", "2026-11-11T00:00:00.000Z"),
            ],
            sessions: vec![SessionRow {
                session_id: "sess-4".to_string(),
                project: "folder-beta".to_string(),
                first_ts: "2026-11-10T14:00:00.000Z".to_string(),
                last_ts: "2026-11-10T14:00:00.000Z".to_string(),
            }],
            ..IngestBatch::default()
        };
        store.insert_batch(&batch).expect("시드 적재");

        // 2. 묶음 개수·정렬 (비용 큰 alpha/main 이 첫 행, 비용·요청 같으면 프로젝트 이름순)
        let stats = store
            .query_project_stats(STATS_SINCE, STATS_UNTIL)
            .expect("프로젝트 통계");
        let keys: Vec<(&str, &str)> = stats
            .iter()
            .map(|row| (row.project.as_str(), row.git_branch.as_str()))
            .collect();
        assert_eq!(
            keys,
            [
                ("alpha", "main"),
                ("", ""),
                ("alpha", "feature/x"),
                ("folder-beta", "")
            ]
        );

        // 3. alpha/main (요청 3건, 세션 2개, 토큰·쓰기 분리·비용·마지막 시각)
        let main = &stats[0];
        assert_eq!((main.requests, main.sessions), (3, 2));
        assert_eq!((main.input_tokens, main.output_tokens), (30, 150));
        assert_eq!(main.cache_creation_5m_tokens, 1_000);
        assert_eq!(main.cache_creation_1h_tokens, 2_000);
        let expected = 2.0 * estimate_cost_usd("claude-opus-5-5", 10, 50, 0, 0, 0)
            + estimate_cost_usd("claude-opus-5-5", 10, 50, 0, 1_000, 2_000);
        assert!((main.cost_usd_estimate - expected).abs() < 1e-12);
        assert_eq!(main.last_ts, "2026-11-10T12:10:00.000Z");

        // 4. 세션 없음·다른 브랜치·세션 폴더명 대체 (각 1건)
        for single in &stats[1..] {
            assert_eq!(
                (single.requests, single.sessions),
                (1, 1),
                "{}",
                single.project
            );
        }
        drop(store);
        remove_temp_dir(&dir);
    }

    // Skill 도구 호출 행 (detail = 스킬 이름)
    fn skill_row(key: &str, ts: &str, session_id: &str, agent: &str, skill: &str) -> ToolCallRow {
        ToolCallRow {
            source: DataSource::Transcript,
            tool_use_id: key.to_string(),
            ts: ts.to_string(),
            session_id: session_id.to_string(),
            agent: agent.to_string(),
            tool_name: "Skill".to_string(),
            detail: skill.to_string(),
            ..ToolCallRow::default()
        }
    }

    #[test]
    fn skill_stats_group_transcript_skill_calls_and_backfill_only_fills_empty_names() {
        let dir = unique_temp_dir("skill-stats");
        let store = Store::open_store(&dir.join("cm.sqlite")).expect("DB 열기");

        // 1. 시드: s1 은 이전 바이너리가 이름 없이 적재한 행, Bash·OTLP·구간 밖 행은 제외 대상
        let mut errored = skill_row(
            "s2",
            "2026-11-10T12:05:00.000Z",
            "sess-a",
            "main",
            "frontend-tui",
        );
        errored.is_error = true;
        let first = IngestBatch {
            tool_calls: vec![
                skill_row("s1", "2026-11-10T12:00:00.000Z", "sess-a", "main", ""),
                errored,
                skill_row(
                    "s3",
                    "2026-11-10T12:10:00.000Z",
                    "sess-b",
                    "worker#abc",
                    "frontend-tui",
                ),
                skill_row(
                    "s4",
                    "2026-11-10T12:20:00.000Z",
                    "sess-b",
                    "main",
                    "core-response",
                ),
                stats_tool_row(
                    "b1",
                    "2026-11-10T12:30:00.000Z",
                    "main",
                    false,
                    DataSource::Transcript,
                ),
                ToolCallRow {
                    source: DataSource::Otlp,
                    ..skill_row(
                        "o1",
                        "2026-11-10T12:40:00.000Z",
                        "sess-a",
                        "main",
                        "frontend-tui",
                    )
                },
                skill_row(
                    "s5",
                    "2026-11-12T00:00:00.000Z",
                    "sess-a",
                    "main",
                    "frontend-tui",
                ),
            ],
            ..IngestBatch::default()
        };
        store.insert_batch(&first).expect("시드 적재");

        // 2. 다시 읽기: 비어 있던 s1 만 채우고 이미 있는 s2 이름은 유지, 신규 건수 0
        let replay = IngestBatch {
            tool_calls: vec![
                skill_row(
                    "s1",
                    "2026-11-10T12:00:00.000Z",
                    "sess-a",
                    "main",
                    "frontend-tui",
                ),
                skill_row(
                    "s2",
                    "2026-11-10T12:05:00.000Z",
                    "sess-a",
                    "main",
                    "utils-table",
                ),
            ],
            ..IngestBatch::default()
        };
        let counts = store.insert_batch(&replay).expect("다시 적재");
        assert_eq!(counts.tool_calls, 0);

        // 3. 구간 집계: 호출 수 내림차순, 호출자는 # 앞 종류 이름
        let stats = store
            .query_skill_stats("2026-11-10T00:00:00.000Z", "2026-11-11T00:00:00.000Z")
            .expect("스킬 통계");
        let names: Vec<&str> = stats.iter().map(|row| row.skill.as_str()).collect();
        assert_eq!(names, ["frontend-tui", "core-response"]);
        let tui = &stats[0];
        assert_eq!((tui.calls, tui.errors, tui.sessions), (3, 1, 2));
        assert_eq!(tui.first_ts, "2026-11-10T12:00:00.000Z");
        assert_eq!(tui.last_ts, "2026-11-10T12:10:00.000Z");
        let callers: Vec<(&str, u64, u64)> = tui
            .agents
            .iter()
            .map(|share| (share.name.as_str(), share.count, share.errors))
            .collect();
        assert_eq!(callers, [("main", 2, 1), ("worker", 1, 0)]);
        assert_eq!(tui.recent.len(), 3);
        assert_eq!(tui.recent[0].name, "worker");
        drop(store);
        remove_temp_dir(&dir);
    }
}
