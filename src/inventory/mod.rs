//! inventory/mod.rs
//! 흩어진 에이전트 설정 (훅, MCP, 에이전트, 스킬, env 키) 중앙 목록과 진단.

mod doctor;
mod load;

#[cfg(test)]
mod tests;

use crate::core::config::AppConfig;
use serde::{Deserialize, Serialize};
use std::path::Path;

// 1. 훅 등록 항목 -------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookEntry {
    pub event: String,
    pub matcher: String,
    pub if_condition: String,
    pub hook_type: String,
    // URL 또는 실행 파일 이름 (인자 값 제외)
    pub target: String,
    pub is_async: bool,
    pub timeout_secs: Option<u64>,
    pub on_failure: String,
}

// 2. MCP 서버 항목 ------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpEntry {
    pub name: String,
    pub transport: String,
    // 실행 파일 경로 또는 URL (env·헤더 값 제외)
    pub target: String,
    pub denied: bool,
}

// 3. 에이전트 항목 ------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentEntry {
    pub name: String,
    pub model: String,
    pub effort: String,
    pub max_turns: Option<u64>,
    pub path: String,
}

// 4. 스킬 항목 ----------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillEntry {
    pub name: String,
    pub description_chars: usize,
    pub path: String,
}

// 5. 진단 항목 ----------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DoctorFinding {
    // ok, warn, fail
    pub level: String,
    pub topic: String,
    pub message: String,
    pub fix_hint: String,
}

// 6. 설정 목록 ----------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Inventory {
    pub claude_home: String,
    pub hooks: Vec<HookEntry>,
    pub mcp_servers: Vec<McpEntry>,
    pub agents: Vec<AgentEntry>,
    // Claude Code 내장 에이전트 이름 (정의 파일 없음, 끄는 env 변수 반영)
    #[serde(default)]
    pub builtin_agents: Vec<String>,
    pub skills: Vec<SkillEntry>,
    // 값 제외, 키 이름만
    pub env_keys: Vec<String>,
    pub http_hook_allowlist: Vec<String>,
    // allowedHttpHookUrls 키 존재 여부 (미지정은 제한 없음, 빈 배열은 전체 차단)
    pub http_hook_allowlist_defined: bool,
    pub warnings: Vec<String>,
}

// 7. 설정 목록 적재 -----------------------------------------------------------
pub fn load_inventory(claude_home: &Path) -> anyhow::Result<Inventory> {
    load::read_inventory(claude_home)
}

// 8. 진단 실행 ----------------------------------------------------------------
pub fn run_doctor_checks(config: &AppConfig) -> anyhow::Result<Vec<DoctorFinding>> {
    doctor::run_doctor(config)
}
