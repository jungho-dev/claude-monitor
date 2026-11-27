//! store/limits.rs
//! 한도·컨텍스트·캐시 스냅샷 저장 (statusLine 지표, 경로·작업 폴더 같은 식별 정보는 저장하지 않음).

use super::Store;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

pub(crate) const LIMITS_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS limit_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL,
    session_id TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    five_hour_pct REAL,
    five_hour_resets_at INTEGER,
    seven_day_pct REAL,
    seven_day_resets_at INTEGER,
    context_pct REAL,
    context_window_size INTEGER,
    context_input_tokens INTEGER,
    context_cache_read_tokens INTEGER,
    context_cache_write_tokens INTEGER,
    cache_warm INTEGER,
    cache_caching_observed INTEGER,
    cache_ttl TEXT,
    cache_expires_at INTEGER,
    cache_requests INTEGER,
    cache_misses INTEGER,
    cache_hit_ratio REAL,
    cache_last_miss_at INTEGER,
    cache_last_miss_cause TEXT,
    session_cost_usd REAL
);
CREATE INDEX IF NOT EXISTS idx_limit_snapshots_ts ON limit_snapshots(ts);
CREATE INDEX IF NOT EXISTS idx_limit_snapshots_session_ts ON limit_snapshots(session_id, ts);
";

// 값이 같을 때 같은 세션을 다시 기록하기까지의 최소 간격 (초)
pub const LIMIT_SNAPSHOT_MIN_GAP_SECS: i64 = 60;

// 스냅샷 보관 일수 (limit_snapshots·ratelimit_snapshots 공통, 기록할 때 이보다 오래된 행 삭제)
pub const LIMIT_SNAPSHOT_RETENTION_DAYS: i64 = 30;

const LIMIT_COLUMNS: &str = "ts, session_id, model, five_hour_pct, five_hour_resets_at, \
     seven_day_pct, seven_day_resets_at, context_pct, context_window_size, context_input_tokens, \
     context_cache_read_tokens, context_cache_write_tokens, cache_warm, cache_caching_observed, \
     cache_ttl, cache_expires_at, cache_requests, cache_misses, cache_hit_ratio, \
     cache_last_miss_at, cache_last_miss_cause, session_cost_usd";

// 1. 스냅샷 행 ----------------------------------------------------------------
// *_resets_at, *_expires_at, *_miss_at 은 Unix epoch 초, 퍼센트는 0-100,
// context_input_tokens 는 컨텍스트에 든 입력 합계 (캐시 읽기·쓰기 포함),
// cache_last_miss_cause 는 원인 이름을 쉼표로 이은 값
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitSnapshot {
    pub ts: String,
    pub session_id: String,
    pub model: String,
    pub five_hour_pct: Option<f64>,
    pub five_hour_resets_at: Option<i64>,
    pub seven_day_pct: Option<f64>,
    pub seven_day_resets_at: Option<i64>,
    pub context_pct: Option<f64>,
    pub context_window_size: Option<i64>,
    pub context_input_tokens: Option<i64>,
    pub context_cache_read_tokens: Option<i64>,
    pub context_cache_write_tokens: Option<i64>,
    pub cache_warm: Option<bool>,
    pub cache_caching_observed: Option<bool>,
    pub cache_ttl: Option<String>,
    pub cache_expires_at: Option<i64>,
    pub cache_requests: Option<i64>,
    pub cache_misses: Option<i64>,
    pub cache_hit_ratio: Option<f64>,
    pub cache_last_miss_at: Option<i64>,
    pub cache_last_miss_cause: Option<String>,
    pub session_cost_usd: Option<f64>,
}

// 2. 행 변환 ------------------------------------------------------------------
fn map_limit_row(row: &Row<'_>) -> rusqlite::Result<LimitSnapshot> {
    Ok(LimitSnapshot {
        ts: row.get(0)?,
        session_id: row.get(1)?,
        model: row.get(2)?,
        five_hour_pct: row.get(3)?,
        five_hour_resets_at: row.get(4)?,
        seven_day_pct: row.get(5)?,
        seven_day_resets_at: row.get(6)?,
        context_pct: row.get(7)?,
        context_window_size: row.get(8)?,
        context_input_tokens: row.get(9)?,
        context_cache_read_tokens: row.get(10)?,
        context_cache_write_tokens: row.get(11)?,
        cache_warm: row.get(12)?,
        cache_caching_observed: row.get(13)?,
        cache_ttl: row.get(14)?,
        cache_expires_at: row.get(15)?,
        cache_requests: row.get(16)?,
        cache_misses: row.get(17)?,
        cache_hit_ratio: row.get(18)?,
        cache_last_miss_at: row.get(19)?,
        cache_last_miss_cause: row.get(20)?,
        session_cost_usd: row.get(21)?,
    })
}

// 2-1. 보관 기준 시각 ---------------------------------------------------------
// 기록 시각 (해석 실패·미래면 현재) 에서 보관 일수를 뺀 RFC 3339 시각.
// 미래 시각 한 건이 이력을 지우지 못하게 현재를 상한으로 둠
// (두 스냅샷 표가 공유하므로 store 하위 모듈에 공개)
pub(super) fn compute_retention_cutoff(recorded_ts: &str) -> String {
    let now = Utc::now();
    let reference = DateTime::parse_from_rfc3339(recorded_ts)
        .map_or(now, |at| at.with_timezone(&Utc))
        .min(now);
    (reference - Duration::days(LIMIT_SNAPSHOT_RETENTION_DAYS))
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

// 3. 행 삽입 ------------------------------------------------------------------
// 삽입 후 보관 기간이 지난 행 삭제 (ts 인덱스 범위 삭제)
fn insert_snapshot_row(
    conn: &rusqlite::Connection,
    snapshot: &LimitSnapshot,
) -> rusqlite::Result<usize> {
    let inserted = conn.execute(
        &format!(
            "INSERT INTO limit_snapshots ({LIMIT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, \
             ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)"
        ),
        params![
            snapshot.ts,
            snapshot.session_id,
            snapshot.model,
            snapshot.five_hour_pct,
            snapshot.five_hour_resets_at,
            snapshot.seven_day_pct,
            snapshot.seven_day_resets_at,
            snapshot.context_pct,
            snapshot.context_window_size,
            snapshot.context_input_tokens,
            snapshot.context_cache_read_tokens,
            snapshot.context_cache_write_tokens,
            snapshot.cache_warm,
            snapshot.cache_caching_observed,
            snapshot.cache_ttl,
            snapshot.cache_expires_at,
            snapshot.cache_requests,
            snapshot.cache_misses,
            snapshot.cache_hit_ratio,
            snapshot.cache_last_miss_at,
            snapshot.cache_last_miss_cause,
            snapshot.session_cost_usd,
        ],
    )?;
    conn.execute(
        "DELETE FROM limit_snapshots WHERE ts < ?1",
        params![compute_retention_cutoff(&snapshot.ts)],
    )?;
    Ok(inserted)
}

// 4. 스냅샷 저장소 ------------------------------------------------------------
impl Store {
    // 4-1. 무조건 기록
    pub fn insert_limit_snapshot(&self, snapshot: &LimitSnapshot) -> anyhow::Result<()> {
        self.with_conn(|conn| insert_snapshot_row(conn, snapshot).map(|_| ()))
    }

    // 4-2. 변경 또는 최소 간격 경과 시에만 기록 (기록했으면 true)
    pub fn record_limit_snapshot(&self, snapshot: &LimitSnapshot) -> anyhow::Result<bool> {
        self.with_conn(|conn| {
            // 1. 같은 세션의 직전 스냅샷
            let previous = conn
                .query_row(
                    &format!(
                        "SELECT {LIMIT_COLUMNS} FROM limit_snapshots WHERE session_id = ?1 \
                         ORDER BY ts DESC, id DESC LIMIT 1"
                    ),
                    params![snapshot.session_id],
                    map_limit_row,
                )
                .optional()?;

            // 2. 값 동일 + 간격 미달이면 생략 (시각 해석 실패는 기록)
            if let Some(previous) = previous {
                let mut probe = previous.clone();
                probe.ts.clone_from(&snapshot.ts);
                let gap_secs = DateTime::parse_from_rfc3339(&snapshot.ts)
                    .ok()
                    .zip(DateTime::parse_from_rfc3339(&previous.ts).ok())
                    .map(|(now, before)| (now - before).num_seconds());
                if probe == *snapshot
                    && gap_secs.is_some_and(|gap| (0..LIMIT_SNAPSHOT_MIN_GAP_SECS).contains(&gap))
                {
                    return Ok(false);
                }
            }

            // 3. 기록
            insert_snapshot_row(conn, snapshot).map(|_| true)
        })
    }

    // 4-3. 5시간·주간 값이 있는 가장 최근 스냅샷 (세션 무관)
    pub fn latest_limit_snapshot(&self) -> anyhow::Result<Option<LimitSnapshot>> {
        self.with_conn(|conn| {
            conn.query_row(
                &format!(
                    "SELECT {LIMIT_COLUMNS} FROM limit_snapshots \
                     WHERE five_hour_pct IS NOT NULL OR seven_day_pct IS NOT NULL \
                     ORDER BY ts DESC, id DESC LIMIT 1"
                ),
                [],
                map_limit_row,
            )
            .optional()
        })
    }

    // 4-4. 세션별 가장 최근 스냅샷
    pub fn latest_limit_snapshot_for_session(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<LimitSnapshot>> {
        self.with_conn(|conn| {
            conn.query_row(
                &format!(
                    "SELECT {LIMIT_COLUMNS} FROM limit_snapshots WHERE session_id = ?1 \
                     ORDER BY ts DESC, id DESC LIMIT 1"
                ),
                params![session_id],
                map_limit_row,
            )
            .optional()
        })
    }

    // 4-5. 기준 시각 이후 스냅샷 (가장 최근 limit 건만 골라 오래된 순으로 반환)
    pub fn limit_snapshots_since(
        &self,
        since: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<LimitSnapshot>> {
        let row_limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {LIMIT_COLUMNS} FROM (SELECT {LIMIT_COLUMNS}, id FROM limit_snapshots \
                 WHERE ts >= ?1 ORDER BY ts DESC, id DESC LIMIT ?2) ORDER BY ts ASC, id ASC"
            ))?;
            stmt.query_map(params![since, row_limit], map_limit_row)?
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::LimitSnapshot;
    use crate::store::Store;
    use crate::transcript::test_support::unique_temp_dir;
    use chrono::{Duration, SecondsFormat, Utc};

    fn sample_snapshot(session_id: &str, ts: &str, five_hour_pct: Option<f64>) -> LimitSnapshot {
        LimitSnapshot {
            ts: ts.to_string(),
            session_id: session_id.to_string(),
            model: "claude-opus-5-5".to_string(),
            five_hour_pct,
            five_hour_resets_at: five_hour_pct.map(|_| 1_738_425_600),
            context_pct: Some(8.0),
            cache_warm: Some(true),
            cache_last_miss_cause: Some("tools_changed,likely_server_side".to_string()),
            ..LimitSnapshot::default()
        }
    }

    // 1. 스키마와 왕복 --------------------------------------------------------
    #[test]
    fn insert_roundtrips_every_column_and_creates_indexes() {
        // 1. 모든 열을 채운 행 기록
        let dir = unique_temp_dir("limits-roundtrip");
        let store = Store::open_store(&dir.join("limits.db")).expect("저장소 열기");
        let full = LimitSnapshot {
            ts: "2026-11-19T10:00:00.000Z".to_string(),
            session_id: "sess-1".to_string(),
            model: "claude-opus-5-5".to_string(),
            five_hour_pct: Some(23.5),
            five_hour_resets_at: Some(1_738_425_600),
            seven_day_pct: Some(41.2),
            seven_day_resets_at: Some(1_738_857_600),
            context_pct: Some(8.0),
            context_window_size: Some(200_000),
            context_input_tokens: Some(15_500),
            context_cache_read_tokens: Some(2_000),
            context_cache_write_tokens: Some(5_000),
            cache_warm: Some(true),
            cache_caching_observed: Some(true),
            cache_ttl: Some("1h".to_string()),
            cache_expires_at: Some(1_738_429_200),
            cache_requests: Some(14),
            cache_misses: Some(2),
            cache_hit_ratio: Some(0.91),
            cache_last_miss_at: Some(1_738_425_230),
            cache_last_miss_cause: Some("tools_changed".to_string()),
            session_cost_usd: Some(0.01234),
        };
        store.insert_limit_snapshot(&full).expect("기록");

        // 2. 조회 결과 동일
        let latest = store
            .latest_limit_snapshot_for_session("sess-1")
            .expect("세션 조회")
            .expect("행 있음");
        assert_eq!(latest, full);

        // 3. 인덱스 두 개
        let names: Vec<String> = store
            .with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'limit_snapshots'",
                )?;
                stmt.query_map([], |row| row.get(0))?.collect()
            })
            .expect("인덱스 조회");
        assert!(names.contains(&"idx_limit_snapshots_ts".to_string()));
        assert!(names.contains(&"idx_limit_snapshots_session_ts".to_string()));
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 2. 값 동일 중복 제거 ----------------------------------------------------
    #[test]
    fn record_skips_unchanged_within_gap_and_stores_after_gap() {
        let dir = unique_temp_dir("limits-dedupe");
        let store = Store::open_store(&dir.join("limits.db")).expect("저장소 열기");
        let first = sample_snapshot("sess-1", "2026-11-19T10:00:00.000Z", Some(10.0));
        assert!(store.record_limit_snapshot(&first).expect("첫 기록"));

        // 1. 같은 값 30초 뒤는 생략
        let same_soon = sample_snapshot("sess-1", "2026-11-19T10:00:30.000Z", Some(10.0));
        assert!(!store.record_limit_snapshot(&same_soon).expect("생략"));

        // 2. 값이 바뀌면 즉시 기록
        let changed = sample_snapshot("sess-1", "2026-11-19T10:00:31.000Z", Some(11.0));
        assert!(store.record_limit_snapshot(&changed).expect("변경 기록"));

        // 3. 같은 값이라도 간격 경과 후 기록
        let same_later = sample_snapshot("sess-1", "2026-11-19T10:01:31.000Z", Some(11.0));
        assert!(store.record_limit_snapshot(&same_later).expect("간격 기록"));

        // 4. 다른 세션은 독립
        let other = sample_snapshot("sess-2", "2026-11-19T10:01:32.000Z", Some(11.0));
        assert!(store.record_limit_snapshot(&other).expect("다른 세션"));

        let rows = store
            .limit_snapshots_since("2026-11-19T00:00:00.000Z", 100)
            .expect("구간 조회");
        assert_eq!(rows.len(), 4);
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 3. 최신 조회 ------------------------------------------------------------
    #[test]
    fn latest_snapshot_requires_limit_values_and_since_filters() {
        let dir = unique_temp_dir("limits-latest");
        let store = Store::open_store(&dir.join("limits.db")).expect("저장소 열기");
        assert!(store.latest_limit_snapshot().expect("빈 조회").is_none());

        // 1. 한도 값 없는 행이 더 최근이어도 한도 값 행을 반환
        let with_limits = sample_snapshot("sess-1", "2026-11-19T10:00:00.000Z", Some(10.0));
        let without_limits = sample_snapshot("sess-2", "2026-11-19T11:00:00.000Z", None);
        store.insert_limit_snapshot(&with_limits).expect("기록");
        store.insert_limit_snapshot(&without_limits).expect("기록");
        let latest = store
            .latest_limit_snapshot()
            .expect("최신 조회")
            .expect("행 있음");
        assert_eq!(latest.session_id, "sess-1");

        // 2. 세션별 최신은 값 유무와 무관
        let session_latest = store
            .latest_limit_snapshot_for_session("sess-2")
            .expect("세션 조회")
            .expect("행 있음");
        assert_eq!(session_latest.five_hour_pct, None);
        assert!(
            store
                .latest_limit_snapshot_for_session("none")
                .expect("없음 조회")
                .is_none()
        );

        // 3. 기준 시각 이후만 오래된 순
        let rows = store
            .limit_snapshots_since("2026-11-19T10:30:00.000Z", 100)
            .expect("구간 조회");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, "sess-2");
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 4. 조회 상한 ------------------------------------------------------------
    #[test]
    fn since_returns_newest_rows_up_to_limit_oldest_first() {
        let dir = unique_temp_dir("limits-row-limit");
        let store = Store::open_store(&dir.join("limits.db")).expect("저장소 열기");
        for minute in 0..5 {
            let ts = format!("2026-11-19T10:0{minute}:00.000Z");
            let snapshot = sample_snapshot("sess-1", &ts, Some(f64::from(minute)));
            store.insert_limit_snapshot(&snapshot).expect("기록");
        }
        let since = "2026-11-19T00:00:00.000Z";

        // 1. 상한보다 많으면 가장 최근 3건을 오래된 순으로
        let capped = store.limit_snapshots_since(since, 3).expect("상한 조회");
        let stamps: Vec<&str> = capped.iter().map(|row| row.ts.as_str()).collect();
        assert_eq!(
            stamps,
            vec![
                "2026-11-19T10:02:00.000Z",
                "2026-11-19T10:03:00.000Z",
                "2026-11-19T10:04:00.000Z",
            ]
        );

        // 2. 상한이 더 크면 전체
        assert_eq!(
            store.limit_snapshots_since(since, 100).expect("전체").len(),
            5
        );
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 5. 보관 기간 정리 -------------------------------------------------------
    #[test]
    fn insert_prunes_rows_older_than_retention_but_not_on_future_timestamps() {
        let dir = unique_temp_dir("limits-retention");
        let store = Store::open_store(&dir.join("limits.db")).expect("저장소 열기");
        let stamp = |days_ago: i64| {
            (Utc::now() - Duration::days(days_ago)).to_rfc3339_opts(SecondsFormat::Millis, true)
        };
        let count_rows = |store: &Store| -> i64 {
            store
                .with_conn(|conn| {
                    conn.query_row("SELECT COUNT(*) FROM limit_snapshots", [], |row| row.get(0))
                })
                .expect("행 수")
        };

        // 1. 보관 기간을 넘긴 행은 이후 기록 때 삭제
        let stale = sample_snapshot("sess-1", &stamp(45), Some(10.0));
        store.insert_limit_snapshot(&stale).expect("오래된 기록");
        assert_eq!(count_rows(&store), 1);
        let fresh = sample_snapshot("sess-1", &stamp(1), Some(11.0));
        assert!(store.record_limit_snapshot(&fresh).expect("최근 기록"));
        assert_eq!(count_rows(&store), 1);

        // 2. 미래 시각 행이 들어와도 현재 기준으로만 정리해 최근 이력 유지
        let future = sample_snapshot("sess-2", &stamp(-400), Some(12.0));
        store.insert_limit_snapshot(&future).expect("미래 기록");
        assert_eq!(count_rows(&store), 2);
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }
}
