//! store/query.rs
//! TUI·리포트·관리 API 공용 집계 조회 (구간은 RFC 3339 UTC 반열린 구간).

use crate::core::model::{
    ApiRequestRow, DENY_ROUND_TRIP_MS, DataSource, HookEventRow, HookOutcome, LiveEvent,
    ToolCallRow,
};
use crate::store::Store;
use chrono::DateTime;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
pub(crate) const STATS_REQUESTS_SQL: &str = "SELECT model, session_id, agent, input_tokens, \
    output_tokens, cache_read_tokens, cache_creation_tokens, duration_ms, ts FROM api_requests \
    WHERE source = 'transcript' AND ts >= ?1 AND ts < ?2";

// 통계 도구 호출 원본 (ts 인덱스 구간)
pub(crate) const STATS_TOOL_CALLS_SQL: &str = "SELECT agent, is_error, ts FROM tool_calls \
    WHERE source = 'transcript' AND ts >= ?1 AND ts < ?2";

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
    pub cache_hit_ratio: f64,
    pub wait_p50_ms: Option<i64>,
    pub wait_p90_ms: Option<i64>,
    pub cost_usd_estimate: f64,
    pub last_ts: String,
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
    pub cache_hit_ratio: f64,
    pub wait_p50_ms: Option<i64>,
    pub wait_p90_ms: Option<i64>,
    pub cost_usd_estimate: f64,
    pub tool_calls: u64,
    pub tool_errors: u64,
    pub top_model: String,
    pub last_ts: String,
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
    waits: Vec<i64>,
    last_ts: String,
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
    cost_usd_estimate: f64,
    waits: Vec<i64>,
    model_requests: BTreeMap<String, u64>,
    tool_calls: u64,
    tool_errors: u64,
    last_ts: String,
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

// 11. 비용 추정 ---------------------------------------------------------------
// API 단가 기준 USD 추정 (MTok 단위 입력·출력), 알 수 없는 모델은 0
pub fn estimate_cost_usd(
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
) -> f64 {
    let name = model.strip_prefix("claude-").unwrap_or(model);
    let (input_rate, output_rate) = if name.starts_with("opus-5-5") {
        (4.0, 20.0)
    } else if name.starts_with("opus-5") || name.starts_with("opus-4") {
        (5.0, 25.0)
    } else if name.starts_with("sonnet-5") {
        (2.0, 10.0)
    } else if name.starts_with("sonnet-4-6") {
        (3.0, 15.0)
    } else if name.starts_with("haiku-5-5") {
        (0.10, 0.50)
    } else if name.starts_with("haiku-4-5") {
        (1.0, 5.0)
    } else if name.starts_with("fable-5-2") {
        (8.0, 40.0)
    } else if name.starts_with("fable-5") {
        (10.0, 50.0)
    } else {
        return 0.0;
    };
    (input_tokens as f64 * input_rate
        + output_tokens as f64 * output_rate
        + cache_read_tokens as f64 * input_rate * 0.1
        + cache_creation_tokens as f64 * input_rate * 1.25)
        / 1_000_000.0
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
            // 1. 모델·에이전트 합산 (하위 에이전트 인스턴스 접미사 제거 후 병합)
            let mut stmt = conn.prepare_cached(
                "SELECT model, agent, COUNT(*), SUM(input_tokens), SUM(output_tokens), \
                 SUM(cache_read_tokens), SUM(cache_creation_tokens) FROM api_requests \
                 WHERE source = 'transcript' AND ts >= ?1 AND ts < ?2 GROUP BY model, agent",
            )?;
            let mut merged: BTreeMap<(String, String), ModelUsageRow> = BTreeMap::new();
            let mut rows = stmt.query(params![since, until])?;
            while let Some(row) = rows.next()? {
                let model: String = row.get(0)?;
                let agent_full: String = row.get(1)?;
                let agent = agent_full.split('#').next().unwrap_or("").to_string();
                let entry = merged
                    .entry((model.clone(), agent.clone()))
                    .or_insert_with(|| ModelUsageRow {
                        model,
                        agent,
                        ..ModelUsageRow::default()
                    });
                entry.requests += row.get::<_, i64>(2)? as u64;
                entry.input_tokens += row.get::<_, i64>(3)?;
                entry.output_tokens += row.get::<_, i64>(4)?;
                entry.cache_read_tokens += row.get::<_, i64>(5)?;
                entry.cache_creation_tokens += row.get::<_, i64>(6)?;
            }

            // 2. 비용 추정·정렬
            let mut usage: Vec<ModelUsageRow> = merged.into_values().collect();
            for entry in &mut usage {
                entry.cost_usd_estimate = estimate_cost_usd(
                    &entry.model,
                    entry.input_tokens,
                    entry.output_tokens,
                    entry.cache_read_tokens,
                    entry.cache_creation_tokens,
                );
            }
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
                 first_byte_ms, first_token_ms, status FROM api_requests \
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
                };
                stamped.push((api_row.ts.clone(), LiveEvent::Api(api_row)));
            }

            // 2. 도구 호출
            let mut tool_stmt = conn.prepare_cached(
                "SELECT source, tool_use_id, ts, session_id, agent, tool_name, duration_ms, \
                 is_error, error_kind FROM tool_calls ORDER BY ts DESC LIMIT ?1",
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
                let entry = grouped.entry(model).or_default();
                entry.requests += 1;
                entry.sessions.insert(session_id);
                entry
                    .agent_types
                    .insert(agent_full.split('#').next().unwrap_or("").to_string());
                entry.input_tokens += row.get::<_, i64>(3)?;
                entry.output_tokens += row.get::<_, i64>(4)?;
                entry.cache_read_tokens += row.get::<_, i64>(5)?;
                entry.cache_creation_tokens += row.get::<_, i64>(6)?;
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
                    cost_usd_estimate: estimate_cost_usd(
                        &model,
                        totals.input_tokens,
                        totals.output_tokens,
                        totals.cache_read_tokens,
                        totals.cache_creation_tokens,
                    ),
                    model,
                    requests: totals.requests,
                    sessions: totals.sessions.len() as u64,
                    agent_types: totals.agent_types.len() as u64,
                    input_tokens: totals.input_tokens,
                    output_tokens: totals.output_tokens,
                    cache_read_tokens: totals.cache_read_tokens,
                    cache_creation_tokens: totals.cache_creation_tokens,
                    cache_hit_ratio,
                    wait_p50_ms,
                    wait_p90_ms,
                    last_ts: totals.last_ts,
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
                let agent = agent_full.split('#').next().unwrap_or("").to_string();
                let run_key = if agent == "main" {
                    session_id.clone()
                } else {
                    agent_full
                };
                let entry = grouped.entry(agent).or_default();
                entry.run_keys.insert(run_key);
                entry.sessions.insert(session_id);
                entry.requests += 1;
                entry.input_tokens += input_tokens;
                entry.output_tokens += output_tokens;
                entry.cache_read_tokens += cache_read_tokens;
                entry.cache_creation_tokens += cache_creation_tokens;
                entry.cost_usd_estimate += estimate_cost_usd(
                    &model,
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_creation_tokens,
                );
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
                let agent = agent_full.split('#').next().unwrap_or("").to_string();
                let entry = grouped.entry(agent).or_default();
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
                    cache_hit_ratio,
                    wait_p50_ms,
                    wait_p90_ms,
                    cost_usd_estimate: totals.cost_usd_estimate,
                    tool_calls: totals.tool_calls,
                    tool_errors: totals.tool_errors,
                    top_model,
                    last_ts: totals.last_ts,
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
    use crate::transcript::test_support::unique_temp_dir;

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

    #[test]
    fn cost_follows_rate_table() {
        let opus = estimate_cost_usd(
            "claude-opus-5-5",
            1_000_000,
            1_000_000,
            1_000_000,
            1_000_000,
        );
        assert!((opus - (4.0 + 20.0 + 0.4 + 5.0)).abs() < 1e-9);
        let older_opus = estimate_cost_usd("claude-opus-4-1", 1_000_000, 0, 0, 0);
        assert!((older_opus - 5.0).abs() < 1e-9);
        let haiku = estimate_cost_usd("claude-haiku-4-5-20251001", 1_000_000, 1_000_000, 0, 0);
        assert!((haiku - 6.0).abs() < 1e-9);
        assert_eq!(estimate_cost_usd("claude-unknown-9", 5, 5, 5, 5), 0.0);
    }

    #[test]
    fn queries_cover_seeded_rows() {
        // 1. 시드 적재
        let dir = unique_temp_dir("query-seed");
        let store = Store::open_store(&dir.join("rap.sqlite")).expect("DB 열기");
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
        std::fs::remove_dir_all(&dir).ok();
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
        let store = Store::open_store(&dir.join("rap.sqlite")).expect("DB 열기");
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
        let expected_cost = estimate_cost_usd("claude-opus-5-5", 500, 250, 4000, 500);
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
        std::fs::remove_dir_all(&dir).ok();
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
        let main_cost = 3.0 * estimate_cost_usd("claude-opus-5-5", 100, 50, 800, 100)
            + estimate_cost_usd("claude-sonnet-5", 100, 50, 800, 100);
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
        std::fs::remove_dir_all(&dir).ok();
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
        std::fs::remove_dir_all(&dir).ok();
    }
}
