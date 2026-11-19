//! store/power.rs
//! 대기 구간 영속화: 구간 기록, 최근 구간 복원, 보관 상한 정리.

use super::Store;
use crate::power::StandbyGap;
use anyhow::Context;
use chrono::{SecondsFormat, Utc};
use rusqlite::params;

// 최신 번호 기준 보관 행 수 (이보다 오래된 번호는 기록 때 정리)
pub const MAX_STORED_GAPS: i64 = 1000;

pub const STANDBY_GAPS_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS standby_gaps (
    id INTEGER PRIMARY KEY,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    active INTEGER NOT NULL DEFAULT 0,
    recorded_at TEXT NOT NULL
);
";

impl Store {
    // 1. 대기 구간 기록 (같은 번호는 덮어쓰기, 보관 상한 밖은 삭제)
    pub fn save_standby_gap(&self, gap: &StandbyGap) -> anyhow::Result<()> {
        let id = i64::try_from(gap.id).context("대기 구간 번호 범위 초과")?;
        let recorded_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        self.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO standby_gaps (id, start_ms, end_ms, active, recorded_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, gap.start_ms, gap.end_ms, gap.active, recorded_at],
            )?;
            conn.execute(
                "DELETE FROM standby_gaps WHERE id <= (SELECT MAX(id) FROM standby_gaps) - ?1",
                params![MAX_STORED_GAPS],
            )
            .map(|_| ())
        })
    }

    // 2. 최근 대기 구간 조회 (번호 오름차순)
    pub fn load_standby_gaps(&self, limit: usize) -> anyhow::Result<Vec<StandbyGap>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut gaps = self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, start_ms, end_ms, active FROM standby_gaps \
                 WHERE id > 0 ORDER BY id DESC LIMIT ?1",
            )?;
            stmt.query_map(params![limit], |row| {
                Ok(StandbyGap {
                    id: u64::try_from(row.get::<_, i64>(0)?).unwrap_or_default(),
                    start_ms: row.get(1)?,
                    end_ms: row.get(2)?,
                    active: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<StandbyGap>>>()
        })?;
        gaps.reverse();
        Ok(gaps)
    }
}
