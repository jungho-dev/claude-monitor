//! core/model.rs
//! 저장소·실시간 이벤트·관리 API 가 공유하는 기록 행과 상태 형태.

use serde::{Deserialize, Serialize};

// 훅 거부 1회 모델 재왕복 (7일 실측 중앙값)
pub const DENY_ROUND_TRIP_MS: i64 = 4200;

// 1. 데이터 출처 --------------------------------------------------------------
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DataSource {
    #[default]
    Transcript,
    Otlp,
    Gateway,
    Hook,
}

impl DataSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transcript => "transcript",
            Self::Otlp => "otlp",
            Self::Gateway => "gateway",
            Self::Hook => "hook",
        }
    }
}

// 2. 훅 판정 결과 -------------------------------------------------------------
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookOutcome {
    #[default]
    Allow,
    Deny,
    Rewrite,
    Error,
}

impl HookOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Rewrite => "rewrite",
            Self::Error => "error",
        }
    }
}

// 3. API 요청 행 --------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApiRequestRow {
    pub source: DataSource,
    // 출처별 중복 제거 키 (message.id, request_id)
    pub request_key: String,
    // RFC 3339 UTC
    pub ts: String,
    pub session_id: String,
    pub agent: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    // 트리거부터 마지막 블록까지 모델 대기
    pub duration_ms: Option<i64>,
    pub first_byte_ms: Option<i64>,
    pub first_token_ms: Option<i64>,
    pub status: Option<i64>,
}

// 4. 도구 호출 행 -------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCallRow {
    pub source: DataSource,
    pub tool_use_id: String,
    pub ts: String,
    pub session_id: String,
    pub agent: String,
    pub tool_name: String,
    pub duration_ms: Option<i64>,
    pub is_error: bool,
    pub error_kind: String,
}

// 5. 도구 완료 행 -------------------------------------------------------------
// 시작 행 ts 와의 차이로 소요 산출
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCompletionRow {
    pub tool_use_id: String,
    pub ts: String,
    pub is_error: bool,
    pub error_kind: String,
}

// 6. 훅 이벤트 행 -------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookEventRow {
    pub source: DataSource,
    pub event_key: String,
    pub ts: String,
    pub session_id: String,
    pub route: String,
    pub guard: String,
    pub outcome: HookOutcome,
    // 가드 라벨·감지 명령 이름만 (원문 금지)
    pub detail: String,
    pub duration_ms: Option<i64>,
    // 재작성으로 피한 거부 왕복 추정
    pub saved_ms: Option<i64>,
}

// 7. 세션 행 ------------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionRow {
    pub session_id: String,
    pub project: String,
    pub first_ts: String,
    pub last_ts: String,
}

// 8. 적재 묶음 ----------------------------------------------------------------
#[derive(Debug, Clone, Default)]
pub struct IngestBatch {
    pub api_requests: Vec<ApiRequestRow>,
    pub tool_calls: Vec<ToolCallRow>,
    pub tool_completions: Vec<ToolCompletionRow>,
    pub hook_events: Vec<HookEventRow>,
    pub sessions: Vec<SessionRow>,
}

// 9. 적재 건수 ----------------------------------------------------------------
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct IngestCounts {
    pub api_requests: u64,
    pub tool_calls: u64,
    pub tool_completions: u64,
    pub hook_events: u64,
    pub sessions: u64,
}

// 10. 실시간 이벤트 -----------------------------------------------------------
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LiveEvent {
    Api(ApiRequestRow),
    Tool(ToolCallRow),
    Hook(HookEventRow),
    Notice {
        ts: String,
        level: String,
        text: String,
    },
}

// 11. 포트 상태 ---------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PortStatus {
    pub name: String,
    pub port: u16,
    pub enabled: bool,
    pub listening: bool,
}

// 12. 카운터 스냅샷 -----------------------------------------------------------
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct CounterSnapshot {
    pub api_requests: u64,
    pub tool_calls: u64,
    pub hook_calls: u64,
    pub hook_rewrites: u64,
    pub hook_denials: u64,
    pub hook_errors: u64,
    pub saved_ms: u64,
    pub gateway_requests: u64,
    pub gateway_active_streams: u64,
    pub otlp_batches: u64,
    pub index_runs: u64,
}

// 13. 관리 API 상태 -----------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub version: String,
    pub pid: u32,
    pub started_at: String,
    pub uptime_secs: u64,
    pub data_dir: String,
    pub claude_home: String,
    pub ports: Vec<PortStatus>,
    pub counters: CounterSnapshot,
}
