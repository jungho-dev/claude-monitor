//! store/ratelimit.rs
//! 게이트웨이 응답의 rate-limit 헤더 스냅샷 저장·조회 (API 키 사용자 잔여량, 인증 헤더·본문 미보관).

use super::Store;
use super::limits::compute_retention_cutoff;
use rusqlite::{Row, params};
use serde::Serialize;

pub(crate) const RATELIMIT_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS ratelimit_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL,
    model TEXT NOT NULL DEFAULT '',
    status INTEGER,
    requests_limit INTEGER,
    requests_remaining INTEGER,
    requests_reset TEXT,
    tokens_limit INTEGER,
    tokens_remaining INTEGER,
    tokens_reset TEXT,
    input_tokens_limit INTEGER,
    input_tokens_remaining INTEGER,
    input_tokens_reset TEXT,
    output_tokens_limit INTEGER,
    output_tokens_remaining INTEGER,
    output_tokens_reset TEXT,
    retry_after_secs INTEGER,
    extra_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_ratelimit_snapshots_ts ON ratelimit_snapshots(ts);
";

const SNAPSHOT_COLUMNS: &str = "ts, model, status, \
    requests_limit, requests_remaining, requests_reset, \
    tokens_limit, tokens_remaining, tokens_reset, \
    input_tokens_limit, input_tokens_remaining, input_tokens_reset, \
    output_tokens_limit, output_tokens_remaining, output_tokens_reset, \
    retry_after_secs, extra_json";

// 1. 스냅샷 행 ----------------------------------------------------------------
// 응답 1건의 rate-limit 헤더 값, `*_reset` 은 RFC 3339 원문, extra_json 은 문서화되지 않은
// `anthropic-ratelimit-*` 헤더의 이름→값 JSON 객체 문자열
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RatelimitSnapshotRow {
    pub ts: String,
    pub model: String,
    pub status: Option<i64>,
    pub requests_limit: Option<i64>,
    pub requests_remaining: Option<i64>,
    pub requests_reset: Option<String>,
    pub tokens_limit: Option<i64>,
    pub tokens_remaining: Option<i64>,
    pub tokens_reset: Option<String>,
    pub input_tokens_limit: Option<i64>,
    pub input_tokens_remaining: Option<i64>,
    pub input_tokens_reset: Option<String>,
    pub output_tokens_limit: Option<i64>,
    pub output_tokens_remaining: Option<i64>,
    pub output_tokens_reset: Option<String>,
    pub retry_after_secs: Option<i64>,
    pub extra_json: String,
}

// 2. 행 변환 ------------------------------------------------------------------
// SNAPSHOT_COLUMNS 순서와 일치
fn map_snapshot_row(row: &Row<'_>) -> rusqlite::Result<RatelimitSnapshotRow> {
    Ok(RatelimitSnapshotRow {
        ts: row.get(0)?,
        model: row.get(1)?,
        status: row.get(2)?,
        requests_limit: row.get(3)?,
        requests_remaining: row.get(4)?,
        requests_reset: row.get(5)?,
        tokens_limit: row.get(6)?,
        tokens_remaining: row.get(7)?,
        tokens_reset: row.get(8)?,
        input_tokens_limit: row.get(9)?,
        input_tokens_remaining: row.get(10)?,
        input_tokens_reset: row.get(11)?,
        output_tokens_limit: row.get(12)?,
        output_tokens_remaining: row.get(13)?,
        output_tokens_reset: row.get(14)?,
        retry_after_secs: row.get(15)?,
        extra_json: row.get(16)?,
    })
}

impl Store {
    // 3. 스냅샷 기록
    pub fn insert_ratelimit_snapshot(&self, snapshot: &RatelimitSnapshotRow) -> anyhow::Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO ratelimit_snapshots (ts, model, status, \
                 requests_limit, requests_remaining, requests_reset, \
                 tokens_limit, tokens_remaining, tokens_reset, \
                 input_tokens_limit, input_tokens_remaining, input_tokens_reset, \
                 output_tokens_limit, output_tokens_remaining, output_tokens_reset, \
                 retry_after_secs, extra_json) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    snapshot.ts,
                    snapshot.model,
                    snapshot.status,
                    snapshot.requests_limit,
                    snapshot.requests_remaining,
                    snapshot.requests_reset,
                    snapshot.tokens_limit,
                    snapshot.tokens_remaining,
                    snapshot.tokens_reset,
                    snapshot.input_tokens_limit,
                    snapshot.input_tokens_remaining,
                    snapshot.input_tokens_reset,
                    snapshot.output_tokens_limit,
                    snapshot.output_tokens_remaining,
                    snapshot.output_tokens_reset,
                    snapshot.retry_after_secs,
                    snapshot.extra_json,
                ],
            )?;

            // 보관 기간이 지난 행 삭제 (ts 인덱스 범위 삭제)
            conn.execute(
                "DELETE FROM ratelimit_snapshots WHERE ts < ?1",
                params![compute_retention_cutoff(&snapshot.ts)],
            )?;
            Ok(())
        })
    }

    // 4. 기준 시각 이후 스냅샷 (최신순, 최대 limit 건)
    pub fn ratelimit_snapshots_since(
        &self,
        since_ts: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<RatelimitSnapshotRow>> {
        let row_limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.with_conn(|conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {SNAPSHOT_COLUMNS} FROM ratelimit_snapshots \
                 WHERE ts >= ?1 ORDER BY ts DESC, id DESC LIMIT ?2"
            ))?;
            let rows = statement.query_map(params![since_ts, row_limit], map_snapshot_row)?;
            rows.collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::RatelimitSnapshotRow;
    use crate::net::admin::test_support::{build_test_config, build_test_context};
    use crate::transcript::test_support::TempDirGuard;
    use chrono::{Duration, SecondsFormat, Utc};

    // 1. 기록·기준 시각 조회 --------------------------------------------------
    #[test]
    fn ratelimit_snapshots_round_trip() {
        let config = build_test_config("rl-store");
        let _cleanup = TempDirGuard(config.data_dir.clone());
        let ctx = build_test_context(config);
        assert!(
            ctx.store
                .ratelimit_snapshots_since("", 10)
                .expect("빈 조회")
                .is_empty()
        );

        let older = RatelimitSnapshotRow {
            ts: "2026-11-19T00:00:00.000Z".to_string(),
            model: "claude-a".to_string(),
            status: Some(200),
            requests_limit: Some(50),
            requests_remaining: Some(49),
            requests_reset: Some("2026-11-19T00:01:00Z".to_string()),
            extra_json: "{}".to_string(),
            ..RatelimitSnapshotRow::default()
        };
        let newer = RatelimitSnapshotRow {
            ts: "2026-11-19T00:00:05.000Z".to_string(),
            model: "claude-b".to_string(),
            status: Some(429),
            retry_after_secs: Some(17),
            output_tokens_remaining: Some(0),
            extra_json: "{\"anthropic-ratelimit-unified-status\":\"rejected\"}".to_string(),
            ..RatelimitSnapshotRow::default()
        };
        ctx.store.insert_ratelimit_snapshot(&older).expect("기록 1");
        ctx.store.insert_ratelimit_snapshot(&newer).expect("기록 2");

        let since = ctx
            .store
            .ratelimit_snapshots_since("2026-11-19T00:00:01.000Z", 10)
            .expect("기준 시각 조회");
        assert_eq!(since, vec![newer.clone()]);
        let all = ctx
            .store
            .ratelimit_snapshots_since("2026-11-19T00:00:00.000Z", 10)
            .expect("전체 조회");
        assert_eq!(all, vec![newer, older]);
        let limited = ctx
            .store
            .ratelimit_snapshots_since("2026-11-19T00:00:00.000Z", 1)
            .expect("건수 제한 조회");
        assert_eq!(limited.len(), 1);
    }

    // 2. 보관 기간 정리 -------------------------------------------------------
    #[test]
    fn insert_prunes_rows_older_than_retention() {
        let config = build_test_config("rl-retention");
        let _cleanup = TempDirGuard(config.data_dir.clone());
        let ctx = build_test_context(config);
        let stamp = |days_ago: i64| {
            (Utc::now() - Duration::days(days_ago)).to_rfc3339_opts(SecondsFormat::Millis, true)
        };
        let snapshot = |ts: String| RatelimitSnapshotRow {
            ts,
            extra_json: "{}".to_string(),
            ..RatelimitSnapshotRow::default()
        };
        let count_rows = || -> i64 {
            ctx.store
                .with_conn(|conn| {
                    conn.query_row("SELECT COUNT(*) FROM ratelimit_snapshots", [], |row| {
                        row.get(0)
                    })
                })
                .expect("행 수")
        };

        // 1. 보관 기간을 넘긴 행은 이후 기록 때 삭제
        ctx.store
            .insert_ratelimit_snapshot(&snapshot(stamp(45)))
            .expect("오래된 기록");
        assert_eq!(count_rows(), 1);
        ctx.store
            .insert_ratelimit_snapshot(&snapshot(stamp(1)))
            .expect("최근 기록");
        assert_eq!(count_rows(), 1);

        // 2. 미래 시각 행이 들어와도 최근 이력 유지
        ctx.store
            .insert_ratelimit_snapshot(&snapshot(stamp(-400)))
            .expect("미래 기록");
        assert_eq!(count_rows(), 2);
    }
}
