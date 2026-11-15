//! store/mod.rs
//! SQLite 저장소: 스키마, 적재 묶음 기록, 색인 커서, 일일 리포트 보관.

pub mod query;

use crate::core::model::{IngestBatch, IngestCounts};
use anyhow::Context;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::fs;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS api_requests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL,
    request_key TEXT NOT NULL UNIQUE,
    ts TEXT NOT NULL,
    session_id TEXT NOT NULL DEFAULT '',
    agent TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    duration_ms INTEGER,
    first_byte_ms INTEGER,
    first_token_ms INTEGER,
    status INTEGER
);
CREATE INDEX IF NOT EXISTS idx_api_requests_ts ON api_requests(ts);
DROP INDEX IF EXISTS idx_api_requests_session;
CREATE INDEX IF NOT EXISTS idx_api_requests_session_agent ON api_requests(session_id, agent, ts);
CREATE TABLE IF NOT EXISTS tool_calls (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL,
    tool_use_id TEXT NOT NULL UNIQUE,
    ts TEXT NOT NULL,
    session_id TEXT NOT NULL DEFAULT '',
    agent TEXT NOT NULL DEFAULT '',
    tool_name TEXT NOT NULL DEFAULT '',
    duration_ms INTEGER,
    is_error INTEGER NOT NULL DEFAULT 0,
    error_kind TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_tool_calls_ts ON tool_calls(ts);
CREATE TABLE IF NOT EXISTS hook_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL,
    event_key TEXT NOT NULL UNIQUE,
    ts TEXT NOT NULL,
    session_id TEXT NOT NULL DEFAULT '',
    route TEXT NOT NULL DEFAULT '',
    guard TEXT NOT NULL DEFAULT '',
    outcome TEXT NOT NULL DEFAULT 'allow',
    detail TEXT NOT NULL DEFAULT '',
    duration_ms INTEGER,
    saved_ms INTEGER
);
CREATE INDEX IF NOT EXISTS idx_hook_events_ts ON hook_events(ts);
CREATE INDEX IF NOT EXISTS idx_hook_events_session_ts ON hook_events(session_id, ts);
CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY,
    project TEXT NOT NULL DEFAULT '',
    first_ts TEXT NOT NULL,
    last_ts TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_last_ts ON sessions(last_ts);
CREATE TABLE IF NOT EXISTS index_cursors (
    path TEXT PRIMARY KEY,
    file_size INTEGER NOT NULL,
    modified_ms INTEGER NOT NULL,
    read_offset INTEGER NOT NULL,
    state_json TEXT NOT NULL DEFAULT '{}'
);
CREATE TABLE IF NOT EXISTS daily_reports (
    report_date TEXT PRIMARY KEY,
    generated_at TEXT NOT NULL,
    markdown TEXT NOT NULL
);
";

// 1. 색인 커서 ----------------------------------------------------------------
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexCursor {
    pub path: String,
    pub file_size: i64,
    pub modified_ms: i64,
    pub read_offset: i64,
    // 파일별 이어 읽기 상태 (직전 트리거 시각 등)
    pub state_json: String,
}

// 2. 저장소 -------------------------------------------------------------------
// 연결 1개 직렬 사용, 비동기 문맥은 spawn_blocking 경유
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    // 2-1. 쓰기 연결 열기
    pub fn open_store(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("데이터 폴더 생성 실패: {}", parent.display()))?;
        }
        let conn =
            Connection::open(path).with_context(|| format!("DB 열기 실패: {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;",
        )?;
        conn.execute_batch(SCHEMA_SQL).context("스키마 생성 실패")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // 2-2. 읽기 전용 연결 열기 (TUI·리포트 조회)
    pub fn open_read_only(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("DB 읽기 전용 열기 실패: {}", path.display()))?;
        conn.execute_batch("PRAGMA busy_timeout = 5000; PRAGMA cache_size = -16384;")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // 2-3. 연결 대여 (조회 모듈 공용)
    pub fn with_conn<T>(
        &self,
        work: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> anyhow::Result<T> {
        // 오염 잠금 복구 (열린 트랜잭션은 드롭 시 롤백)
        let guard = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        work(&guard).map_err(anyhow::Error::from)
    }

    // 2-4. 적재 묶음 기록 (단일 트랜잭션, 중복 키 무시)
    pub fn insert_batch(&self, batch: &IngestBatch) -> anyhow::Result<IngestCounts> {
        let mut guard = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        let tx = guard.transaction()?;
        let mut counts = IngestCounts::default();

        // 1. API 요청 (신규만 집계, 읽기 경계 분할 시 최신 usage 로 갱신)
        {
            let mut insert_stmt = tx.prepare_cached(
                "INSERT INTO api_requests (source, request_key, ts, session_id, agent, model, \
                 input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, duration_ms, \
                 first_byte_ms, first_token_ms, status) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
                 ON CONFLICT(request_key) DO NOTHING",
            )?;
            let mut refresh_stmt = tx.prepare_cached(
                "UPDATE api_requests SET \
                 input_tokens = MAX(input_tokens, ?1), \
                 output_tokens = MAX(output_tokens, ?2), \
                 cache_read_tokens = MAX(cache_read_tokens, ?3), \
                 cache_creation_tokens = MAX(cache_creation_tokens, ?4), \
                 duration_ms = COALESCE(MAX(duration_ms, ?5), duration_ms, ?5) \
                 WHERE request_key = ?6 \
                 AND (?2 > output_tokens OR ?1 > input_tokens OR ?3 > cache_read_tokens \
                 OR ?4 > cache_creation_tokens OR COALESCE(?5, 0) > COALESCE(duration_ms, 0))",
            )?;
            for row in &batch.api_requests {
                let inserted = insert_stmt.execute(params![
                    row.source.as_str(),
                    row.request_key,
                    row.ts,
                    row.session_id,
                    row.agent,
                    row.model,
                    row.input_tokens,
                    row.output_tokens,
                    row.cache_read_tokens,
                    row.cache_creation_tokens,
                    row.duration_ms,
                    row.first_byte_ms,
                    row.first_token_ms,
                    row.status,
                ])?;
                if inserted > 0 {
                    counts.api_requests += 1;
                    continue;
                }

                // 기존 키 (갱신은 집계 제외)
                refresh_stmt.execute(params![
                    row.input_tokens,
                    row.output_tokens,
                    row.cache_read_tokens,
                    row.cache_creation_tokens,
                    row.duration_ms,
                    row.request_key,
                ])?;
            }
        }

        // 2. 도구 호출 시작
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO tool_calls (source, tool_use_id, ts, session_id, agent, tool_name, \
                 duration_ms, is_error, error_kind) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for row in &batch.tool_calls {
                counts.tool_calls += stmt.execute(params![
                    row.source.as_str(),
                    row.tool_use_id,
                    row.ts,
                    row.session_id,
                    row.agent,
                    row.tool_name,
                    row.duration_ms,
                    row.is_error,
                    row.error_kind,
                ])? as u64;
            }
        }

        // 3. 도구 호출 완료 (시작 ts 와의 차이)
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE tool_calls SET \
                 duration_ms = MAX(0, CAST(ROUND((julianday(?2) - julianday(ts)) * 86400000) AS INTEGER)), \
                 is_error = ?3, error_kind = ?4 \
                 WHERE tool_use_id = ?1 AND duration_ms IS NULL",
            )?;
            for row in &batch.tool_completions {
                counts.tool_completions += stmt.execute(params![
                    row.tool_use_id,
                    row.ts,
                    row.is_error,
                    row.error_kind
                ])? as u64;
            }
        }

        // 4. 훅 이벤트
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO hook_events (source, event_key, ts, session_id, route, guard, \
                 outcome, detail, duration_ms, saved_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for row in &batch.hook_events {
                counts.hook_events += stmt.execute(params![
                    row.source.as_str(),
                    row.event_key,
                    row.ts,
                    row.session_id,
                    row.route,
                    row.guard,
                    row.outcome.as_str(),
                    row.detail,
                    row.duration_ms,
                    row.saved_ms,
                ])? as u64;
            }
        }

        // 5. 세션 구간 확장
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO sessions (session_id, project, first_ts, last_ts) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(session_id) DO UPDATE SET \
                 project = CASE WHEN excluded.project <> '' THEN excluded.project ELSE sessions.project END, \
                 first_ts = MIN(sessions.first_ts, excluded.first_ts), \
                 last_ts = MAX(sessions.last_ts, excluded.last_ts)",
            )?;
            for row in &batch.sessions {
                counts.sessions += stmt.execute(params![
                    row.session_id,
                    row.project,
                    row.first_ts,
                    row.last_ts
                ])? as u64;
            }
        }

        tx.commit()?;
        Ok(counts)
    }

    // 2-5. 색인 커서 조회
    pub fn load_index_cursor(&self, path: &str) -> anyhow::Result<Option<IndexCursor>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT path, file_size, modified_ms, read_offset, state_json FROM index_cursors WHERE path = ?1",
                params![path],
                |row| {
                    Ok(IndexCursor {
                        path: row.get(0)?,
                        file_size: row.get(1)?,
                        modified_ms: row.get(2)?,
                        read_offset: row.get(3)?,
                        state_json: row.get(4)?,
                    })
                },
            )
            .optional()
        })
    }

    // 2-6. 색인 커서 저장
    pub fn save_index_cursor(&self, cursor: &IndexCursor) -> anyhow::Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO index_cursors (path, file_size, modified_ms, read_offset, state_json) \
                 VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(path) DO UPDATE SET \
                 file_size = excluded.file_size, modified_ms = excluded.modified_ms, \
                 read_offset = excluded.read_offset, state_json = excluded.state_json",
                params![
                    cursor.path,
                    cursor.file_size,
                    cursor.modified_ms,
                    cursor.read_offset,
                    cursor.state_json,
                ],
            )
            .map(|_| ())
        })
    }

    // 2-7. 일일 리포트 저장
    pub fn save_daily_report(
        &self,
        report_date: &str,
        generated_at: &str,
        markdown: &str,
    ) -> anyhow::Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO daily_reports (report_date, generated_at, markdown) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(report_date) DO UPDATE SET generated_at = excluded.generated_at, \
                 markdown = excluded.markdown",
                params![report_date, generated_at, markdown],
            )
            .map(|_| ())
        })
    }

    // 2-8. 일일 리포트 조회
    pub fn load_daily_report(&self, report_date: &str) -> anyhow::Result<Option<String>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT markdown FROM daily_reports WHERE report_date = ?1",
                params![report_date],
                |row| row.get(0),
            )
            .optional()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Store;
    use super::query::{CACHE_BREAK_PREVIOUS_SQL, DENY_SOURCE_SQL};
    use crate::core::model::{ApiRequestRow, DataSource, HookEventRow, HookOutcome, IngestBatch};
    use crate::transcript::test_support::unique_temp_dir;
    use rusqlite::params;
    use std::sync::Arc;

    // 1. 오염 잠금 복구 -------------------------------------------------------
    #[test]
    fn store_recovers_after_poisoned_lock() {
        // 1. 잠금 보유 중 panic 으로 오염
        let dir = std::env::temp_dir().join(format!(
            "rap-store-poison-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let store = Arc::new(Store::open_store(&dir.join("poison.db")).expect("저장소 열기"));
        let poisoner = Arc::clone(&store);
        let joined = std::thread::spawn(move || {
            let _ = poisoner.with_conn(|_| -> rusqlite::Result<()> { panic!("의도적 panic") });
        })
        .join();
        assert!(joined.is_err());
        assert!(store.conn.is_poisoned());

        // 2. 이후 조회·적재 정상
        let one: i64 = store
            .with_conn(|conn| conn.query_row("SELECT 1", [], |row| row.get(0)))
            .expect("오염 후 조회");
        assert_eq!(one, 1);
        store
            .insert_batch(&IngestBatch::default())
            .expect("오염 후 적재");
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 2. 세션 에이전트 인덱스 -------------------------------------------------
    #[test]
    fn schema_has_session_agent_index() {
        // 1. 구형 인덱스가 남은 DB 준비
        let dir = unique_temp_dir("store-index");
        let db_path = dir.join("index.db");
        let store = Store::open_store(&db_path).expect("저장소 열기");
        store
            .with_conn(|conn| {
                conn.execute_batch(
                    "CREATE INDEX idx_api_requests_session ON api_requests(session_id, ts);",
                )
            })
            .expect("구형 인덱스 생성");
        drop(store);

        // 2. 재열기 후 인덱스 교체 확인
        let store = Store::open_store(&db_path).expect("저장소 재열기");
        let names: Vec<String> = store
            .with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'api_requests'",
                )?;
                stmt.query_map([], |row| row.get(0))?.collect()
            })
            .expect("인덱스 조회");
        assert!(names.contains(&"idx_api_requests_session_agent".to_string()));
        assert!(!names.contains(&"idx_api_requests_session".to_string()));
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 3. 캐시 붕괴 직전 요청 계획 ---------------------------------------------
    #[test]
    fn cache_break_fallback_uses_session_agent_index() {
        let dir = unique_temp_dir("store-plan");
        let store = Store::open_store(&dir.join("plan.db")).expect("저장소 열기");
        let plan: String = store
            .with_conn(|conn| {
                let mut stmt =
                    conn.prepare(&format!("EXPLAIN QUERY PLAN {CACHE_BREAK_PREVIOUS_SQL}"))?;
                let details: Vec<String> = stmt
                    .query_map(
                        params!["sess-1", "main", "2026-11-10T12:00:00.000Z"],
                        |row| row.get(3),
                    )?
                    .collect::<Result<_, _>>()?;
                Ok(details.join("\n"))
            })
            .expect("계획 조회");
        assert!(plan.contains("idx_api_requests_session_agent"), "{plan}");
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 4. 훅 거부 시험 행 ------------------------------------------------------
    fn deny_row(key: &str, source: DataSource, session_id: &str, ts: &str) -> HookEventRow {
        HookEventRow {
            source,
            event_key: key.to_string(),
            ts: ts.to_string(),
            session_id: session_id.to_string(),
            route: "PreToolUse:Bash".to_string(),
            guard: "cli-guard".to_string(),
            outcome: HookOutcome::Deny,
            detail: "cat".to_string(),
            ..HookEventRow::default()
        }
    }

    // 5. 훅 거부 이벤트 단위 중복 제거 ----------------------------------------
    #[test]
    fn hook_deny_dedupes_per_event() {
        // 1. 시드 (짝 있음, 짝 없음과 다른 세션, 세션 없음)
        let dir = unique_temp_dir("store-deny");
        let store = Store::open_store(&dir.join("deny.db")).expect("저장소 열기");
        let batch = IngestBatch {
            hook_events: vec![
                deny_row(
                    "hook:a",
                    DataSource::Hook,
                    "sess-a",
                    "2026-11-10T12:00:00.000Z",
                ),
                deny_row(
                    "transcript:a",
                    DataSource::Transcript,
                    "sess-a",
                    "2026-11-10T12:00:03.000Z",
                ),
                deny_row(
                    "hook:b",
                    DataSource::Hook,
                    "sess-b",
                    "2026-11-10T13:10:00.000Z",
                ),
                deny_row(
                    "transcript:c",
                    DataSource::Transcript,
                    "sess-c",
                    "2026-11-10T13:10:02.000Z",
                ),
                deny_row(
                    "hook:none",
                    DataSource::Hook,
                    "",
                    "2026-11-10T14:10:00.000Z",
                ),
            ],
            ..IngestBatch::default()
        };
        store.insert_batch(&batch).expect("시드 적재");

        // 2. 시간대별 개요·훅 표·분 추이 단언
        let windows = [
            ("2026-11-10T12:00:00.000Z", "2026-11-10T13:00:00.000Z", 1u64),
            ("2026-11-10T13:00:00.000Z", "2026-11-10T14:00:00.000Z", 2u64),
            ("2026-11-10T14:00:00.000Z", "2026-11-10T15:00:00.000Z", 1u64),
        ];
        for (since, until, expected) in windows {
            let overview = store.query_overview(since, until).expect("개요");
            assert_eq!(overview.hook_denials, expected, "개요 {since}");
            let stats = store.query_hook_stats(since, until).expect("훅 표");
            let listed: u64 = stats
                .iter()
                .filter(|row| row.outcome == "deny")
                .map(|row| row.count)
                .sum();
            assert_eq!(listed, expected, "훅 표 {since}");
            let buckets = store.query_minute_buckets(since, until).expect("분 추이");
            let bucketed: u64 = buckets.iter().map(|bucket| bucket.hook_denials).sum();
            assert_eq!(bucketed, expected, "분 추이 {since}");
        }
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 6. 읽기 전용 캐시 크기 --------------------------------------------------
    #[test]
    fn read_only_connection_sets_cache_size() {
        let dir = unique_temp_dir("store-cache");
        let db_path = dir.join("cache.db");
        drop(Store::open_store(&db_path).expect("저장소 열기"));
        let reader = Store::open_read_only(&db_path).expect("읽기 전용 열기");
        let cache_size: i64 = reader
            .with_conn(|conn| conn.query_row("PRAGMA cache_size", [], |row| row.get(0)))
            .expect("캐시 크기 조회");
        assert_eq!(cache_size, -16384);
        drop(reader);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 7. 거부 중복 제거 계획 --------------------------------------------------
    #[test]
    fn deny_dedupe_subquery_seeks_session_ts_range() {
        let dir = unique_temp_dir("store-deny-plan");
        let store = Store::open_store(&dir.join("plan.db")).expect("저장소 열기");
        let plan: String = store
            .with_conn(|conn| {
                let mut stmt = conn.prepare(&format!(
                    "EXPLAIN QUERY PLAN SELECT COUNT(*) FROM hook_events \
                     WHERE ts >= ?1 AND ts < ?2 AND {DENY_SOURCE_SQL}"
                ))?;
                let details: Vec<String> = stmt
                    .query_map(
                        params!["2026-11-10T12:00:00.000Z", "2026-11-10T13:00:00.000Z"],
                        |row| row.get(3),
                    )?
                    .collect::<Result<_, _>>()?;
                Ok(details.join("\n"))
            })
            .expect("계획 조회");
        assert!(
            plan.contains("idx_hook_events_session_ts (session_id=? AND ts>? AND ts<?)"),
            "{plan}"
        );
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 8. 거부 소요 보존 -------------------------------------------------------
    #[test]
    fn hook_stats_keep_deny_latency_of_paired_rows() {
        // 1. 시드 (같은 묶음 짝, 다른 묶음 짝)
        let dir = unique_temp_dir("store-deny-latency");
        let store = Store::open_store(&dir.join("latency.db")).expect("저장소 열기");
        let mut same_group_hook = deny_row(
            "hook:a",
            DataSource::Hook,
            "sess-a",
            "2026-11-10T12:00:00.000Z",
        );
        same_group_hook.duration_ms = Some(40);
        let mut other_group_hook = deny_row(
            "hook:b",
            DataSource::Hook,
            "sess-b",
            "2026-11-10T12:10:00.000Z",
        );
        other_group_hook.route = "pretool-chain".to_string();
        other_group_hook.duration_ms = Some(25);
        let batch = IngestBatch {
            hook_events: vec![
                same_group_hook,
                deny_row(
                    "transcript:a",
                    DataSource::Transcript,
                    "sess-a",
                    "2026-11-10T12:00:03.000Z",
                ),
                other_group_hook,
                deny_row(
                    "transcript:b",
                    DataSource::Transcript,
                    "sess-b",
                    "2026-11-10T12:10:03.000Z",
                ),
            ],
            ..IngestBatch::default()
        };
        store.insert_batch(&batch).expect("시드 적재");

        // 2. 건수는 중복 제거, 소요는 짝 행 포함
        let stats = store
            .query_hook_stats("2026-11-10T12:00:00.000Z", "2026-11-10T13:00:00.000Z")
            .expect("훅 표");
        let counted: u64 = stats.iter().map(|row| row.count).sum();
        assert_eq!(counted, 2);
        let same_group = stats
            .iter()
            .find(|row| row.route == "PreToolUse:Bash")
            .expect("같은 묶음");
        assert_eq!(same_group.count, 2);
        assert_eq!(same_group.p50_ms, Some(40));
        let other_group = stats
            .iter()
            .find(|row| row.route == "pretool-chain")
            .expect("다른 묶음");
        assert_eq!(other_group.count, 0);
        assert_eq!(other_group.p50_ms, Some(25));
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 9. API 요청 신규 건수 ---------------------------------------------------
    #[test]
    fn api_request_counts_only_new_rows() {
        // 1. 첫 적재 (신규 1건)
        let dir = unique_temp_dir("store-api-count");
        let store = Store::open_store(&dir.join("count.db")).expect("저장소 열기");
        let first_row = ApiRequestRow {
            source: DataSource::Transcript,
            request_key: "msg_A".to_string(),
            ts: "2026-11-10T12:00:00.000Z".to_string(),
            output_tokens: 10,
            ..ApiRequestRow::default()
        };
        let first_batch = IngestBatch {
            api_requests: vec![first_row.clone()],
            ..IngestBatch::default()
        };
        let first_counts = store.insert_batch(&first_batch).expect("첫 적재");
        assert_eq!(first_counts.api_requests, 1);

        // 2. 같은 메시지 갱신 (집계 불변, 저장값 갱신)
        let grown_row = ApiRequestRow {
            output_tokens: 30,
            duration_ms: Some(2000),
            ..first_row.clone()
        };
        let grown_batch = IngestBatch {
            api_requests: vec![grown_row],
            ..IngestBatch::default()
        };
        let grown_counts = store.insert_batch(&grown_batch).expect("갱신 적재");
        assert_eq!(grown_counts.api_requests, 0);
        let (stored_output, stored_wait): (i64, Option<i64>) = store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT output_tokens, duration_ms FROM api_requests WHERE request_key = 'msg_A'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
            })
            .expect("저장값 조회");
        assert_eq!((stored_output, stored_wait), (30, Some(2000)));

        // 3. 갱신 뒤 신규 키 (신규 1건)
        let next_row = ApiRequestRow {
            request_key: "msg_B".to_string(),
            ..first_row
        };
        let next_batch = IngestBatch {
            api_requests: vec![next_row],
            ..IngestBatch::default()
        };
        let next_counts = store.insert_batch(&next_batch).expect("신규 적재");
        assert_eq!(next_counts.api_requests, 1);
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }
}
