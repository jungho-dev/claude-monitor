//! store/mod.rs
//! SQLite 저장소: 스키마, 적재 묶음 기록, 색인 커서, 일일 리포트 보관.

pub mod limits;
pub mod metrics;
pub mod otel_events;
pub mod power;
pub mod query;
pub mod ratelimit;

use crate::core::model::{DataSource, IngestBatch, IngestCounts};
use anyhow::Context;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
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
    status INTEGER,
    cache_creation_5m_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_1h_tokens INTEGER NOT NULL DEFAULT 0,
    git_branch TEXT NOT NULL DEFAULT '',
    project TEXT NOT NULL DEFAULT ''
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
    error_kind TEXT NOT NULL DEFAULT '',
    detail TEXT NOT NULL DEFAULT ''
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
CREATE TABLE IF NOT EXISTS index_backfill_pending (
    marker INTEGER PRIMARY KEY CHECK (marker = 1)
);
CREATE TABLE IF NOT EXISTS daily_reports (
    report_date TEXT PRIMARY KEY,
    generated_at TEXT NOT NULL,
    markdown TEXT NOT NULL
);
";

// 기존 DB 에 덧붙이는 api_requests 열 (이름, 정의)
const API_REQUEST_ADDED_COLUMNS: [(&str, &str); 4] = [
    ("cache_creation_5m_tokens", "INTEGER NOT NULL DEFAULT 0"),
    ("cache_creation_1h_tokens", "INTEGER NOT NULL DEFAULT 0"),
    ("git_branch", "TEXT NOT NULL DEFAULT ''"),
    ("project", "TEXT NOT NULL DEFAULT ''"),
];

// 기존 DB 에 덧붙이는 tool_calls 열 (Skill 도구의 스킬 이름)
const TOOL_CALL_ADDED_COLUMNS: [(&str, &str); 1] = [("detail", "TEXT NOT NULL DEFAULT ''")];

// OTLP 선점 도구 호출 행을 기록 출처로 승격하는 재색인을 마친 스키마 판 (PRAGMA user_version)
const TOOL_CALL_PROMOTE_VERSION: i64 = 1;

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
        let mut conn =
            Connection::open(path).with_context(|| format!("DB 열기 실패: {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;",
        )?;
        conn.execute_batch(SCHEMA_SQL).context("스키마 생성 실패")?;
        conn.execute_batch(otel_events::SCHEMA_SQL)
            .context("otel_events 스키마 생성 실패")?;
        conn.execute_batch(metrics::METRICS_SCHEMA_SQL)
            .context("지표 스키마 생성 실패")?;
        conn.execute_batch(power::STANDBY_GAPS_SCHEMA_SQL)
            .context("대기 구간 스키마 생성 실패")?;
        conn.execute_batch(limits::LIMITS_SCHEMA_SQL)
            .context("한도 스냅샷 스키마 생성 실패")?;
        conn.execute_batch(ratelimit::RATELIMIT_SCHEMA_SQL)
            .context("레이트리밋 스키마 생성 실패")?;

        // 기존 DB 열 추가 (있으면 건너뜀, 새로 붙으면 커서를 비워 백필 구간 재읽기)
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut added = false;
        let tables: [(&str, &[(&str, &str)]); 2] = [
            ("api_requests", &API_REQUEST_ADDED_COLUMNS),
            ("tool_calls", &TOOL_CALL_ADDED_COLUMNS),
        ];
        for (table, columns) in tables {
            let existing: Vec<String> = tx
                .prepare(&format!("PRAGMA table_info({table})"))?
                .query_map([], |row| row.get(1))?
                .collect::<Result<_, _>>()?;
            for (name, definition) in columns {
                if existing.iter().any(|column| column == name) {
                    continue;
                }
                tx.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {name} {definition}"
                ))
                .with_context(|| format!("열 추가 실패: {table}.{name}"))?;
                added = true;
            }
        }

        // OTLP 가 먼저 넣은 도구 호출 행이 있던 DB 는 한 번 재색인해 기록 출처로 승격
        let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < TOOL_CALL_PROMOTE_VERSION {
            let has_otlp_first: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM tool_calls WHERE source <> 'transcript')",
                [],
                |row| row.get(0),
            )?;
            added |= has_otlp_first;
            tx.execute_batch(&format!(
                "PRAGMA user_version = {TOOL_CALL_PROMOTE_VERSION}"
            ))?;
        }
        if added {
            // 동시에 도는 이전 바이너리가 빈 커서를 소비해 새 열 없이 다시 색인할 수 있으므로
            // 대기 표식을 남겨 새 색인기의 다음 실행이 커서를 다시 비움 (consume_index_backfill)
            tx.execute("DELETE FROM index_cursors", [])?;
            tx.execute(
                "INSERT OR IGNORE INTO index_backfill_pending (marker) VALUES (1)",
                [],
            )?;
        }
        tx.commit()?;
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
                 first_byte_ms, first_token_ms, status, cache_creation_5m_tokens, \
                 cache_creation_1h_tokens, git_branch, project) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18) \
                 ON CONFLICT(request_key) DO NOTHING",
            )?;
            let mut refresh_stmt = tx.prepare_cached(
                "UPDATE api_requests SET \
                 input_tokens = MAX(input_tokens, ?1), \
                 output_tokens = MAX(output_tokens, ?2), \
                 cache_read_tokens = MAX(cache_read_tokens, ?3), \
                 cache_creation_tokens = MAX(cache_creation_tokens, ?4), \
                 duration_ms = COALESCE(MAX(duration_ms, ?5), duration_ms, ?5), \
                 cache_creation_5m_tokens = MAX(cache_creation_5m_tokens, ?7), \
                 cache_creation_1h_tokens = MAX(cache_creation_1h_tokens, ?8), \
                 git_branch = CASE WHEN git_branch = '' THEN ?9 ELSE git_branch END, \
                 project = CASE WHEN project = '' THEN ?10 ELSE project END \
                 WHERE request_key = ?6 \
                 AND (?2 > output_tokens OR ?1 > input_tokens OR ?3 > cache_read_tokens \
                 OR ?4 > cache_creation_tokens OR COALESCE(?5, 0) > COALESCE(duration_ms, 0) \
                 OR ?7 > cache_creation_5m_tokens OR ?8 > cache_creation_1h_tokens \
                 OR (git_branch = '' AND ?9 <> '') OR (project = '' AND ?10 <> ''))",
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
                    row.cache_creation_5m_tokens,
                    row.cache_creation_1h_tokens,
                    row.git_branch,
                    row.project,
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
                    row.cache_creation_5m_tokens,
                    row.cache_creation_1h_tokens,
                    row.git_branch,
                    row.project,
                ])?;
            }
        }

        // 2. 도구 호출 시작 (신규만 집계, 백필 재읽기는 비어 있던 스킬 이름만 채움)
        // 같은 tool_use_id 를 OTLP 가 먼저 넣었으면 기록 행이 출처·에이전트를 넘겨받음
        // (통계 조회는 transcript 출처만 읽으므로 승격하지 않으면 해당 호출이 빠짐)
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO tool_calls (source, tool_use_id, ts, session_id, agent, tool_name, \
                 duration_ms, is_error, error_kind, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            let mut detail_stmt = tx.prepare_cached(
                "UPDATE tool_calls SET detail = ?2 WHERE tool_use_id = ?1 AND detail = ''",
            )?;
            let mut promote_stmt = tx.prepare_cached(
                "UPDATE tool_calls SET source = ?2, agent = ?3, \
                 session_id = CASE WHEN session_id = '' THEN ?4 ELSE session_id END \
                 WHERE tool_use_id = ?1 AND source <> ?2",
            )?;
            for row in &batch.tool_calls {
                let inserted = stmt.execute(params![
                    row.source.as_str(),
                    row.tool_use_id,
                    row.ts,
                    row.session_id,
                    row.agent,
                    row.tool_name,
                    row.duration_ms,
                    row.is_error,
                    row.error_kind,
                    row.detail,
                ])? as u64;
                counts.tool_calls += inserted;
                if inserted == 0 && !row.detail.is_empty() {
                    detail_stmt.execute(params![row.tool_use_id, row.detail])?;
                }
                if inserted == 0 && row.source == DataSource::Transcript {
                    promote_stmt.execute(params![
                        row.tool_use_id,
                        row.source.as_str(),
                        row.agent,
                        row.session_id,
                    ])?;
                }
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

        // 4-1. OTLP 신규 이벤트
        counts.otel_events = otel_events::insert_otel_events(&tx, &batch.otel_events)?;

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

    // 2-5-1. 백필 대기 표식 소비 (표식이 있었으면 커서를 비우고 true, 한 번만)
    pub fn consume_index_backfill(&self) -> anyhow::Result<bool> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            let pending = tx.execute("DELETE FROM index_backfill_pending", [])? > 0;
            if pending {
                tx.execute("DELETE FROM index_cursors", [])?;
            }
            tx.commit()?;
            Ok(pending)
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

    // 2-9. 일일 리포트 생성 시각 조회 (RFC 3339, 보관본이 없으면 None)
    pub fn daily_report_generated_at(&self, report_date: &str) -> anyhow::Result<Option<String>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT generated_at FROM daily_reports WHERE report_date = ?1",
                params![report_date],
                |row| row.get(0),
            )
            .optional()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::query::{CACHE_BREAK_PREVIOUS_SQL, DENY_SOURCE_SQL};
    use super::{IndexCursor, Store};
    use crate::core::model::{
        ApiRequestRow, DataSource, HookEventRow, HookOutcome, IngestBatch, ToolCallRow,
    };
    use crate::transcript::test_support::unique_temp_dir;
    use rusqlite::{Connection, params};
    use std::sync::Arc;

    // 1. 오염 잠금 복구 -------------------------------------------------------
    #[test]
    fn store_recovers_after_poisoned_lock() {
        // 1. 잠금 보유 중 panic 으로 오염
        let dir = std::env::temp_dir().join(format!(
            "cm-store-poison-{}-{}",
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

    // 10. 기존 DB 열 추가 (멱등) ----------------------------------------------
    #[test]
    fn old_schema_db_gains_columns_once_and_clears_cursors_once() {
        // 1. 이전 스키마 DB (신규 열 없음) 준비: 요청 1건, 색인 커서 1건
        let dir = unique_temp_dir("store-migrate");
        let db_path = dir.join("old.db");
        let old = Connection::open(&db_path).expect("이전 DB 열기");
        old.execute_batch(
            "CREATE TABLE api_requests (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT NOT NULL, \
                request_key TEXT NOT NULL UNIQUE, ts TEXT NOT NULL, \
                session_id TEXT NOT NULL DEFAULT '', agent TEXT NOT NULL DEFAULT '', \
                model TEXT NOT NULL DEFAULT '', input_tokens INTEGER NOT NULL DEFAULT 0, \
                output_tokens INTEGER NOT NULL DEFAULT 0, \
                cache_read_tokens INTEGER NOT NULL DEFAULT 0, \
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0, duration_ms INTEGER, \
                first_byte_ms INTEGER, first_token_ms INTEGER, status INTEGER); \
             CREATE TABLE index_cursors (path TEXT PRIMARY KEY, file_size INTEGER NOT NULL, \
                modified_ms INTEGER NOT NULL, read_offset INTEGER NOT NULL, \
                state_json TEXT NOT NULL DEFAULT '{}'); \
             INSERT INTO api_requests (source, request_key, ts, cache_creation_tokens) \
                VALUES ('transcript', 'msg_old', '2026-11-10T12:00:00.000Z', 900); \
             INSERT INTO index_cursors (path, file_size, modified_ms, read_offset) \
                VALUES ('a.jsonl', 10, 20, 10);",
        )
        .expect("이전 스키마 시드");
        drop(old);
        let column_names = |store: &Store| -> Vec<String> {
            store
                .with_conn(|conn| {
                    let mut stmt = conn.prepare("PRAGMA table_info(api_requests)")?;
                    stmt.query_map([], |row| row.get(1))?.collect()
                })
                .expect("열 조회")
        };
        let cursor_count = |store: &Store| -> i64 {
            store
                .with_conn(|conn| {
                    conn.query_row("SELECT COUNT(*) FROM index_cursors", [], |row| row.get(0))
                })
                .expect("커서 조회")
        };

        // 2. 첫 열기: 열 4개 추가, 색인 커서 비움, 기존 행과 합계 유지
        let store = Store::open_store(&db_path).expect("업그레이드 열기");
        let columns = column_names(&store);
        for name in [
            "cache_creation_5m_tokens",
            "cache_creation_1h_tokens",
            "git_branch",
            "project",
        ] {
            assert_eq!(
                columns.iter().filter(|found| *found == name).count(),
                1,
                "{name}"
            );
        }
        assert_eq!(cursor_count(&store), 0);
        let (total, split_5m, project): (i64, i64, String) = store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT cache_creation_tokens, cache_creation_5m_tokens, project \
                     FROM api_requests WHERE request_key = 'msg_old'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
            })
            .expect("기존 행 조회");
        assert_eq!((total, split_5m, project.as_str()), (900, 0, ""));

        // 3. 두 번째 열기: 열 불변, 다시 저장한 커서는 유지 (커서를 또 비우지 않음)
        store
            .save_index_cursor(&IndexCursor {
                path: "b.jsonl".to_string(),
                file_size: 1,
                modified_ms: 2,
                read_offset: 1,
                state_json: "{}".to_string(),
            })
            .expect("커서 저장");
        drop(store);
        let reopened = Store::open_store(&db_path).expect("재열기");
        assert_eq!(column_names(&reopened), columns);
        assert_eq!(cursor_count(&reopened), 1);
        drop(reopened);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 10-1. 백필 대기 표식 (열 추가 때만 심고 한 번만 소비) -------------------
    #[test]
    fn column_migration_leaves_backfill_marker_consumed_once() {
        // 1. 이전 스키마 DB 열기: 표식이 심기고 커서는 비워짐
        let dir = unique_temp_dir("store-backfill-marker");
        let db_path = dir.join("old.db");
        let old = Connection::open(&db_path).expect("이전 DB 열기");
        old.execute_batch(
            "CREATE TABLE api_requests (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT NOT NULL, \
                request_key TEXT NOT NULL UNIQUE, ts TEXT NOT NULL, \
                session_id TEXT NOT NULL DEFAULT '', agent TEXT NOT NULL DEFAULT '', \
                model TEXT NOT NULL DEFAULT '', input_tokens INTEGER NOT NULL DEFAULT 0, \
                output_tokens INTEGER NOT NULL DEFAULT 0, \
                cache_read_tokens INTEGER NOT NULL DEFAULT 0, \
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0, duration_ms INTEGER, \
                first_byte_ms INTEGER, first_token_ms INTEGER, status INTEGER);",
        )
        .expect("이전 스키마 시드");
        drop(old);
        let cursor = |path: &str| IndexCursor {
            path: path.to_string(),
            file_size: 1,
            modified_ms: 2,
            read_offset: 1,
            state_json: "{}".to_string(),
        };
        let cursor_count = |store: &Store| -> i64 {
            store
                .with_conn(|conn| {
                    conn.query_row("SELECT COUNT(*) FROM index_cursors", [], |row| row.get(0))
                })
                .expect("커서 조회")
        };
        let store = Store::open_store(&db_path).expect("업그레이드 열기");
        assert_eq!(cursor_count(&store), 0);

        // 2. 이전 바이너리가 빈 커서를 소비해 다시 채웠다고 하면 소비가 비움
        store
            .save_index_cursor(&cursor("stale.jsonl"))
            .expect("커서 저장");
        assert!(store.consume_index_backfill().expect("첫 소비"));
        assert_eq!(cursor_count(&store), 0);

        // 3. 한 번만: 이후 커서는 유지
        store
            .save_index_cursor(&cursor("kept.jsonl"))
            .expect("커서 저장");
        assert!(!store.consume_index_backfill().expect("두 번째 소비"));
        assert_eq!(cursor_count(&store), 1);

        // 4. 재열기는 표식을 다시 심지 않음 (새 DB 도 마찬가지)
        drop(store);
        let reopened = Store::open_store(&db_path).expect("재열기");
        assert!(!reopened.consume_index_backfill().expect("재열기 소비"));
        assert_eq!(cursor_count(&reopened), 1);
        drop(reopened);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 11. 새 열 보존·빈 값 채움 갱신 ------------------------------------------
    #[test]
    fn api_request_upsert_keeps_split_columns_and_fills_empty_ones() {
        // 1. 처음 적재 (분리·브랜치 없음)
        let dir = unique_temp_dir("store-split-upsert");
        let store = Store::open_store(&dir.join("split.db")).expect("저장소 열기");
        let first_row = ApiRequestRow {
            source: DataSource::Transcript,
            request_key: "msg_A".to_string(),
            ts: "2026-11-10T12:00:00.000Z".to_string(),
            cache_creation_tokens: 1000,
            ..ApiRequestRow::default()
        };
        store
            .insert_batch(&IngestBatch {
                api_requests: vec![first_row.clone()],
                ..IngestBatch::default()
            })
            .expect("첫 적재");

        // 2. 같은 키로 분리·브랜치·프로젝트 도착 (신규 건수 0, 값 채움)
        let filled_row = ApiRequestRow {
            cache_creation_5m_tokens: 700,
            cache_creation_1h_tokens: 300,
            git_branch: "main".to_string(),
            project: "demo".to_string(),
            ..first_row.clone()
        };
        let counts = store
            .insert_batch(&IngestBatch {
                api_requests: vec![filled_row],
                ..IngestBatch::default()
            })
            .expect("갱신 적재");
        assert_eq!(counts.api_requests, 0);

        // 3. 다른 브랜치로 다시 도착해도 첫 값 유지, 분리 값은 더 큰 값 유지
        let other_row = ApiRequestRow {
            cache_creation_5m_tokens: 100,
            git_branch: "other".to_string(),
            project: "other".to_string(),
            ..first_row
        };
        store
            .insert_batch(&IngestBatch {
                api_requests: vec![other_row],
                ..IngestBatch::default()
            })
            .expect("후속 적재");
        let stored: (i64, i64, String, String) = store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT cache_creation_5m_tokens, cache_creation_1h_tokens, git_branch, project \
                     FROM api_requests WHERE request_key = 'msg_A'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
            })
            .expect("저장값 조회");
        assert_eq!(stored, (700, 300, "main".to_string(), "demo".to_string()));
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }
    // 12. OTLP 선점 도구 호출 행 승격 -----------------------------------------
    #[test]
    fn transcript_tool_call_promotes_otlp_first_row_and_reopen_plants_backfill() {
        // 1. OTLP 가 먼저 적재 (에이전트 빈 값, 스킬 이름 있음)
        let dir = unique_temp_dir("store-tool-promote");
        let db_path = dir.join("promote.db");
        let store = Store::open_store(&db_path).expect("저장소 열기");
        let otlp_row = ToolCallRow {
            source: DataSource::Otlp,
            tool_use_id: "toolu_A".to_string(),
            ts: "2026-11-22T01:00:00.000Z".to_string(),
            session_id: "sess-a".to_string(),
            tool_name: "Skill".to_string(),
            duration_ms: Some(6),
            detail: "core-git".to_string(),
            ..ToolCallRow::default()
        };
        store
            .insert_batch(&IngestBatch {
                tool_calls: vec![otlp_row.clone()],
                ..IngestBatch::default()
            })
            .expect("OTLP 적재");

        // 2. 재열기: 승격 판 이전이면 표식을 한 번 심음 (판 기록 후 재열기는 심지 않음)
        drop(store);
        let store = Store::open_store(&db_path).expect("판 갱신 열기");
        assert!(!store.consume_index_backfill().expect("새 DB 는 판만 기록"));
        store
            .with_conn(|conn| conn.execute_batch("PRAGMA user_version = 0"))
            .expect("이전 판 흉내");
        drop(store);
        let store = Store::open_store(&db_path).expect("이전 판 열기");
        assert!(store.consume_index_backfill().expect("표식 소비"));
        drop(store);
        let store = Store::open_store(&db_path).expect("재열기");
        assert!(!store.consume_index_backfill().expect("두 번째 소비"));

        // 3. 같은 id 의 기록 행: 신규 0건, 출처·에이전트 승격, OTLP 값 유지
        let counts = store
            .insert_batch(&IngestBatch {
                tool_calls: vec![ToolCallRow {
                    source: DataSource::Transcript,
                    agent: "main".to_string(),
                    duration_ms: None,
                    detail: "other".to_string(),
                    ..otlp_row
                }],
                ..IngestBatch::default()
            })
            .expect("기록 적재");
        assert_eq!(counts.tool_calls, 0);
        let stored: (String, String, String, Option<i64>) = store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT source, agent, detail, duration_ms FROM tool_calls                      WHERE tool_use_id = 'toolu_A'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
            })
            .expect("저장값 조회");
        assert_eq!(
            stored,
            (
                "transcript".to_string(),
                "main".to_string(),
                "core-git".to_string(),
                Some(6)
            )
        );
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }
}
