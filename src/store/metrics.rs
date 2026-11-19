//! store/metrics.rs
//! 지표 점(metric_points) 스키마, 적재, 스킬·MCP 서버·플러그인 등 차원별 비용 귀속 조회.

use super::Store;
use rusqlite::params;
use serde::{Deserialize, Serialize};

pub const METRICS_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS metric_points (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL,
    session_id TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL,
    token_type TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    query_source TEXT NOT NULL DEFAULT '',
    agent_name TEXT NOT NULL DEFAULT '',
    skill_name TEXT NOT NULL DEFAULT '',
    plugin_name TEXT NOT NULL DEFAULT '',
    mcp_server_name TEXT NOT NULL DEFAULT '',
    mcp_tool_name TEXT NOT NULL DEFAULT '',
    value REAL NOT NULL,
    temporality TEXT NOT NULL DEFAULT 'delta'
);
CREATE INDEX IF NOT EXISTS idx_metric_points_ts ON metric_points(ts);
CREATE INDEX IF NOT EXISTS idx_metric_points_name_ts ON metric_points(name, ts);
CREATE UNIQUE INDEX IF NOT EXISTS uq_metric_points_identity ON metric_points(
    name, ts, session_id, token_type, model, query_source,
    agent_name, skill_name, plugin_name, mcp_server_name, mcp_tool_name, temporality, value
);
";

// 1. 지표 점 행 ---------------------------------------------------------------
// name 은 claude_code. 접두사를 뗀 형태 (cost.usage, token.usage 등)
// token_type 은 OTLP type 속성 (토큰 종류 input·output·cacheRead·cacheCreation, 줄 수 added·removed 등)
// temporality 는 delta (수신 증분·누적 환산 증분) 또는 gauge
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MetricPointRow {
    pub ts: String,
    pub session_id: String,
    pub name: String,
    pub token_type: String,
    pub model: String,
    pub query_source: String,
    pub agent_name: String,
    pub skill_name: String,
    pub plugin_name: String,
    pub mcp_server_name: String,
    pub mcp_tool_name: String,
    pub value: f64,
    pub temporality: String,
}

// 2. 귀속 차원 ----------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionDimension {
    Skill,
    McpServer,
    Plugin,
    Agent,
    Model,
    QuerySource,
}

impl AttributionDimension {
    /// 요청 문자열(skill·mcp_server·plugin·agent·model·query_source)을 차원으로 해석.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "skill" => Some(Self::Skill),
            "mcp_server" => Some(Self::McpServer),
            "plugin" => Some(Self::Plugin),
            "agent" => Some(Self::Agent),
            "model" => Some(Self::Model),
            "query_source" => Some(Self::QuerySource),
            _ => None,
        }
    }

    // SQL 에 끼우는 열 이름 (열거형이 고정하므로 주입 불가)
    fn column(self) -> &'static str {
        match self {
            Self::Skill => "skill_name",
            Self::McpServer => "mcp_server_name",
            Self::Plugin => "plugin_name",
            Self::Agent => "agent_name",
            Self::Model => "model",
            Self::QuerySource => "query_source",
        }
    }
}

// 3. 귀속 결과 행 -------------------------------------------------------------
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MetricAttributionRow {
    pub name: String,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
}

impl Store {
    // 3-1. 지표 점 적재 (단일 트랜잭션, 식별 속성이 같은 점은 무시해 재전송 중복 방지, 신규 행 수 반환)
    pub fn insert_metric_points(&self, rows: &[MetricPointRow]) -> anyhow::Result<u64> {
        if rows.is_empty() {
            return Ok(0);
        }
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            let mut inserted = 0u64;
            {
                let mut stmt = tx.prepare_cached(
                    "INSERT OR IGNORE INTO metric_points (ts, session_id, name, token_type, model, \
                     query_source, agent_name, skill_name, plugin_name, mcp_server_name, \
                     mcp_tool_name, value, temporality) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                )?;
                for row in rows {
                    inserted += stmt.execute(params![
                        row.ts,
                        row.session_id,
                        row.name,
                        row.token_type,
                        row.model,
                        row.query_source,
                        row.agent_name,
                        row.skill_name,
                        row.plugin_name,
                        row.mcp_server_name,
                        row.mcp_tool_name,
                        row.value,
                        row.temporality,
                    ])? as u64;
                }
            }
            tx.commit()?;
            Ok(inserted)
        })
    }

    // 3-2. 차원별 비용·토큰 귀속 (구간 [since, until), 증분 행만, 이름 없는 점 제외, 비용 큰 순)
    pub fn query_metric_attribution(
        &self,
        since: &str,
        until: &str,
        dimension: AttributionDimension,
    ) -> anyhow::Result<Vec<MetricAttributionRow>> {
        let column = dimension.column();
        let sql = format!(
            "SELECT {column}, \
             SUM(CASE WHEN name = 'cost.usage' THEN value ELSE 0 END) AS cost_usd, \
             SUM(CASE WHEN name = 'token.usage' AND token_type = 'input' THEN value ELSE 0 END), \
             SUM(CASE WHEN name = 'token.usage' AND token_type = 'output' THEN value ELSE 0 END), \
             SUM(CASE WHEN name = 'token.usage' AND token_type = 'cacheRead' THEN value ELSE 0 END), \
             SUM(CASE WHEN name = 'token.usage' AND token_type = 'cacheCreation' THEN value ELSE 0 END) \
             FROM metric_points \
             WHERE ts >= ?1 AND ts < ?2 AND temporality = 'delta' \
             AND name IN ('cost.usage', 'token.usage') AND {column} <> '' \
             GROUP BY {column} ORDER BY cost_usd DESC, {column}"
        );
        self.with_conn(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            stmt.query_map(params![since, until], |row| {
                Ok(MetricAttributionRow {
                    name: row.get(0)?,
                    cost_usd: row.get(1)?,
                    input_tokens: row.get::<_, f64>(2)?.round() as i64,
                    output_tokens: row.get::<_, f64>(3)?.round() as i64,
                    cache_read_tokens: row.get::<_, f64>(4)?.round() as i64,
                    cache_creation_tokens: row.get::<_, f64>(5)?.round() as i64,
                })
            })?
            .collect()
        })
    }
}
