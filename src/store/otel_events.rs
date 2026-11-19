//! store/otel_events.rs
//! OTLP 신규 이벤트(컴팩션, 서브에이전트, API 오류, 권한 결정·모드 변경) 테이블과 조회 (구간은 RFC 3339 UTC 반열린 구간).

use crate::core::model::OtelEventRow;
use crate::store::Store;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{Row, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// 컴팩션 집계 시 훑는 행 상한 (구간당 컴팩션은 드묾)
const COMPACTION_SCAN_LIMIT: usize = 5000;

pub(super) const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS otel_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_key TEXT NOT NULL UNIQUE,
    ts TEXT NOT NULL,
    session_id TEXT NOT NULL DEFAULT '',
    agent_id TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT '',
    duration_ms INTEGER,
    tokens_before INTEGER,
    tokens_after INTEGER,
    count INTEGER,
    detail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_otel_events_ts ON otel_events(ts);
CREATE INDEX IF NOT EXISTS idx_otel_events_kind_ts ON otel_events(kind, ts);
";

const SELECT_COLUMNS_SQL: &str = "event_key, ts, session_id, agent_id, kind, name, status, \
    duration_ms, tokens_before, tokens_after, count, detail";

// 1. 세션별 컴팩션 집계 -------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactionStat {
    pub session_id: String,
    pub count: u64,
    pub auto_count: u64,
    pub manual_count: u64,
    pub failed_count: u64,
    // 가장 최근 컴팩션 시각과 그 전후 토큰, 트리거 (auto|manual)
    pub last_ts: String,
    pub last_trigger: String,
    pub last_pre_tokens: Option<i64>,
    pub last_post_tokens: Option<i64>,
}

// 2. 서브에이전트 실행 한 건 --------------------------------------------------
// started_ts 는 종료 시각에서 소요를 뺀 값 (소요가 없으면 종료 시각)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubagentRun {
    pub started_ts: String,
    pub ended_ts: String,
    pub session_id: String,
    pub agent_id: String,
    pub agent_type: String,
    // async | sync
    pub mode: String,
    pub duration_ms: Option<i64>,
    // 마지막 요청 기준 토큰 (누적 아님)
    pub total_tokens: Option<i64>,
    pub tool_uses: Option<i64>,
    // 모델·교체 모델·출처
    pub detail: String,
}

// 3. 이벤트 적재 (중복 키 무시, 신규 건수 반환) -------------------------------
pub(super) fn insert_otel_events(
    tx: &Transaction<'_>,
    rows: &[OtelEventRow],
) -> rusqlite::Result<u64> {
    let mut stmt = tx.prepare_cached(
        "INSERT OR IGNORE INTO otel_events (event_key, ts, session_id, agent_id, kind, name, \
         status, duration_ms, tokens_before, tokens_after, count, detail) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;
    let mut inserted = 0u64;
    for row in rows {
        inserted += stmt.execute(params![
            row.event_key,
            row.ts,
            row.session_id,
            row.agent_id,
            row.category,
            row.name,
            row.status,
            row.duration_ms,
            row.tokens_before,
            row.tokens_after,
            row.count,
            row.detail,
        ])? as u64;
    }
    Ok(inserted)
}

// 4. 행 읽기 (SELECT_COLUMNS_SQL 열 순서) -------------------------------------
fn read_otel_row(row: &Row<'_>) -> rusqlite::Result<OtelEventRow> {
    Ok(OtelEventRow {
        event_key: row.get(0)?,
        ts: row.get(1)?,
        session_id: row.get(2)?,
        agent_id: row.get(3)?,
        category: row.get(4)?,
        name: row.get(5)?,
        status: row.get(6)?,
        duration_ms: row.get(7)?,
        tokens_before: row.get(8)?,
        tokens_after: row.get(9)?,
        count: row.get(10)?,
        detail: row.get(11)?,
    })
}

impl Store {
    // 4-1. 구간 이벤트 (최신순, kind 로 한 종류만 거를 수 있음)
    pub fn otel_events_since(
        &self,
        since: &str,
        until: &str,
        kind: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<OtelEventRow>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {SELECT_COLUMNS_SQL} FROM otel_events \
                 WHERE ts >= ?1 AND ts < ?2 AND (?3 IS NULL OR kind = ?3) \
                 ORDER BY ts DESC, id DESC LIMIT ?4"
            ))?;
            let rows = stmt.query_map(params![since, until, kind, limit as i64], read_otel_row)?;
            rows.collect()
        })
    }

    // 4-2. 세션별 컴팩션 집계 (최근 컴팩션 순)
    pub fn compaction_stats(
        &self,
        since: &str,
        until: &str,
    ) -> anyhow::Result<Vec<CompactionStat>> {
        // 1. 최신순으로 훑어 세션별 첫 행을 최근 컴팩션으로 취함
        let rows =
            self.otel_events_since(since, until, Some("compaction"), COMPACTION_SCAN_LIMIT)?;
        let mut by_session: BTreeMap<String, CompactionStat> = BTreeMap::new();
        for row in rows {
            let stat = by_session
                .entry(row.session_id.clone())
                .or_insert_with(|| CompactionStat {
                    session_id: row.session_id.clone(),
                    last_ts: row.ts.clone(),
                    last_trigger: row.status.clone(),
                    last_pre_tokens: row.tokens_before,
                    last_post_tokens: row.tokens_after,
                    ..CompactionStat::default()
                });
            stat.count += 1;
            match row.status.as_str() {
                "auto" => stat.auto_count += 1,
                "manual" => stat.manual_count += 1,
                _ => {}
            }
            if row.detail.starts_with("failed") {
                stat.failed_count += 1;
            }
        }

        // 2. 최근 컴팩션 시각 역순
        let mut stats: Vec<CompactionStat> = by_session.into_values().collect();
        stats.sort_by(|left, right| right.last_ts.cmp(&left.last_ts));
        Ok(stats)
    }

    // 4-3. 서브에이전트 실행 목록 (종료 시각 역순)
    pub fn subagent_runs(
        &self,
        since: &str,
        until: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<SubagentRun>> {
        let rows = self.otel_events_since(since, until, Some("subagent"), limit)?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let started_ts = match (DateTime::parse_from_rfc3339(&row.ts), row.duration_ms) {
                    (Ok(ended), Some(millis)) => (ended.with_timezone(&Utc)
                        - Duration::milliseconds(millis))
                    .to_rfc3339_opts(SecondsFormat::Millis, true),
                    _ => row.ts.clone(),
                };
                SubagentRun {
                    started_ts,
                    ended_ts: row.ts,
                    session_id: row.session_id,
                    agent_id: row.agent_id,
                    agent_type: row.name,
                    mode: row.status,
                    duration_ms: row.duration_ms,
                    total_tokens: row.tokens_after,
                    tool_uses: row.count,
                    detail: row.detail,
                }
            })
            .collect())
    }
}
