//! inventory/load.rs
//! Claude 홈 설정 읽기: settings.json 훅·env 키, .claude.json MCP, 에이전트·스킬 머리글 (값 제외).

use super::{AgentEntry, HookEntry, Inventory, McpEntry, SkillEntry};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

const SCRIPT_EXTENSIONS: [&str; 9] = [
    ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".py", ".ps1", ".sh",
];

// Claude Code 내장 에이전트 (정의 파일 없음), Explore·Plan 은 settings.json env 의 끄기 변수가 켜져 있으면 빠짐
const BUILTIN_AGENTS: [&str; 3] = ["general-purpose", "statusline-setup", "claude-code-guide"];
const EXPLORE_PLAN_AGENTS: [&str; 2] = ["Explore", "Plan"];
const DISABLE_EXPLORE_PLAN_KEY: &str = "CLAUDE_CODE_DISABLE_EXPLORE_PLAN_AGENTS";

// 1. 설정 목록 읽기 -----------------------------------------------------------
pub fn read_inventory(claude_home: &Path) -> anyhow::Result<Inventory> {
    let mut inventory = Inventory {
        claude_home: claude_home.display().to_string(),
        builtin_agents: BUILTIN_AGENTS
            .iter()
            .chain(EXPLORE_PLAN_AGENTS.iter())
            .map(|name| (*name).to_string())
            .collect(),
        ..Default::default()
    };
    let denied = read_settings_file(claude_home, &mut inventory);
    read_mcp_servers(claude_home, &denied, &mut inventory);
    read_agent_files(claude_home, &mut inventory);
    read_skill_files(claude_home, &mut inventory);
    Ok(inventory)
}

// 2. settings.json 읽기 -------------------------------------------------------
// 훅 목록, env 키 이름, 허용 URL 을 채우고 차단된 MCP 서버 이름 반환
fn read_settings_file(claude_home: &Path, inventory: &mut Inventory) -> Vec<String> {
    // 1. 파일 해석
    let path = claude_home.join("settings.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            inventory
                .warnings
                .push(format!("settings.json 읽기 실패: {err}"));
            return Vec::new();
        }
    };
    let root: Value = match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
        Ok(root) => root,
        Err(err) => {
            inventory
                .warnings
                .push(format!("settings.json 형식 오류: {err}"));
            return Vec::new();
        }
    };

    // 2. 훅 등록
    if let Some(Value::Object(events)) = root.get("hooks") {
        for (event, groups) in events {
            let Some(groups) = groups.as_array() else {
                inventory
                    .warnings
                    .push(format!("hooks.{event} 형식 오류 (배열 아님)"));
                continue;
            };
            for group in groups {
                let matcher = group.get("matcher").and_then(Value::as_str).unwrap_or("");
                let hooks = group.get("hooks").and_then(Value::as_array);
                for hook in hooks.into_iter().flatten() {
                    let hook_type = hook.get("type").and_then(Value::as_str).unwrap_or("");
                    let target = match hook_type {
                        "http" => {
                            let url = hook.get("url").and_then(Value::as_str).unwrap_or("");
                            redact_url_target(url, true)
                        }
                        "command" => {
                            let command = hook.get("command").and_then(Value::as_str).unwrap_or("");
                            let args: Vec<String> = hook
                                .get("args")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(|arg| arg.as_str().map(str::to_string))
                                .collect();
                            describe_command_target(command, &args)
                        }
                        _ => String::new(),
                    };
                    let timeout = hook.get("timeout");
                    inventory.hooks.push(HookEntry {
                        event: event.clone(),
                        matcher: matcher.to_string(),
                        if_condition: hook
                            .get("if")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        hook_type: hook_type.to_string(),
                        target,
                        is_async: hook.get("async").and_then(Value::as_bool).unwrap_or(false),
                        timeout_secs: timeout.and_then(|value| {
                            value
                                .as_u64()
                                .or_else(|| value.as_f64().map(|secs| secs as u64))
                        }),
                        on_failure: hook
                            .get("onFailure")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    });
                }
            }
        }
    }

    // 3. env 키 이름 (값 제외), Explore·Plan 끄기 변수가 켜져 있으면 내장 목록에서 뺀
    if let Some(Value::Object(env)) = root.get("env") {
        inventory.env_keys = env.keys().cloned().collect();
        let explore_plan_disabled = env
            .get(DISABLE_EXPLORE_PLAN_KEY)
            .and_then(Value::as_str)
            .is_some_and(|flag| matches!(flag.trim(), "1" | "true"));
        if explore_plan_disabled {
            inventory
                .builtin_agents
                .retain(|name| !EXPLORE_PLAN_AGENTS.contains(&name.as_str()));
        }
    }

    // 4. 허용 URL
    if let Some(Value::Array(urls)) = root.get("allowedHttpHookUrls") {
        inventory.http_hook_allowlist_defined = true;
        inventory.http_hook_allowlist = urls
            .iter()
            .filter_map(|url| url.as_str().map(str::to_string))
            .collect();
    }

    // 5. 차단 MCP 서버 이름
    let mut denied: Vec<String> = Vec::new();
    if let Some(Value::Array(servers)) = root.get("deniedMcpServers") {
        for server in servers {
            let name = match server {
                Value::String(name) => Some(name.as_str()),
                other => other.get("serverName").and_then(Value::as_str),
            };
            if let Some(name) = name {
                denied.push(name.to_string());
            }
        }
    }
    denied
}

// 3. 명령 대상 요약 -----------------------------------------------------------
/// 실행 파일 이름과 첫 스크립트 파일 이름만 남김 (그 외 인자 값 제외).
fn describe_command_target(command: &str, args: &[String]) -> String {
    // 1. 실행 파일과 인자 후보 분리
    let mut words: Vec<String> = command
        .split_whitespace()
        .map(|word| word.trim_matches('"').to_string())
        .collect();
    if words.is_empty() {
        return String::new();
    }
    let program = words.remove(0);
    let candidates: Vec<String> = if args.is_empty() {
        words
    } else {
        args.to_vec()
    };
    let program_name = program.rsplit(['/', '\\']).next().unwrap_or("").to_string();

    // 2. 첫 스크립트 파일 이름
    for candidate in &candidates {
        let base = candidate.rsplit(['/', '\\']).next().unwrap_or("");
        let lowered = base.to_ascii_lowercase();
        if SCRIPT_EXTENSIONS.iter().any(|ext| lowered.ends_with(ext)) {
            return format!("{program_name} {base}");
        }
    }
    program_name
}

// 4. URL 표시 대상 축약 -------------------------------------------------------
/// userinfo·질의·조각을 제거하고 `scheme://host[:port]` 만 남김 (`keep_path` 면 경로 유지).
fn redact_url_target(url: &str, keep_path: bool) -> String {
    // 1. 스킴 분리
    let (scheme_prefix, rest) = match url
        .split_once("://")
        .filter(|(scheme, _)| !scheme.contains(['/', '?', '#', '@']))
    {
        Some((scheme, rest)) => (format!("{scheme}://"), rest),
        None => (String::new(), url),
    };

    // 2. 권한부·경로 경계 (질의·조각 앞에서 종료)
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let path_end = rest.find(['?', '#']).unwrap_or(rest.len());
    let host_port = rest[..authority_end].rsplit('@').next().unwrap_or("");
    let path = if keep_path {
        &rest[authority_end..path_end]
    } else {
        ""
    };
    format!("{scheme_prefix}{host_port}{path}")
}

// 5. MCP 서버 읽기 ------------------------------------------------------------
fn read_mcp_servers(claude_home: &Path, denied: &[String], inventory: &mut Inventory) {
    // 1. 파일 탐색 (홈 아래 우선, 없으면 상위 폴더)
    let mut candidates: Vec<PathBuf> = vec![claude_home.join(".claude.json")];
    if let Some(parent) = claude_home.parent() {
        candidates.push(parent.join(".claude.json"));
    }
    let Some(path) = candidates.into_iter().find(|candidate| candidate.is_file()) else {
        inventory.warnings.push(".claude.json 없음".to_string());
        append_denied_only(denied, &BTreeSet::new(), inventory);
        return;
    };
    let root: Value = match fs::read_to_string(&path)
        .map_err(|err| err.to_string())
        .and_then(|text| {
            serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|err| err.to_string())
        }) {
        Ok(root) => root,
        Err(err) => {
            inventory
                .warnings
                .push(format!(".claude.json 읽기·해석 실패: {err}"));
            append_denied_only(denied, &BTreeSet::new(), inventory);
            return;
        }
    };

    // 2. 서버 항목 (명령 경로·URL 만, env·헤더 제외)
    let mut seen: BTreeSet<String> = BTreeSet::new();
    if let Some(Value::Object(servers)) = root.get("mcpServers") {
        for (name, config) in servers {
            let transport = config
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("stdio")
                .to_string();
            let target = match config.get("url").and_then(Value::as_str) {
                Some(url) => redact_url_target(url, false),
                None => config
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            };
            inventory.mcp_servers.push(McpEntry {
                name: name.clone(),
                transport,
                target,
                denied: denied.contains(name),
            });
            seen.insert(name.clone());
        }
    }

    // 3. 설정에만 있는 차단 서버
    append_denied_only(denied, &seen, inventory);
}

// 6. 차단 전용 서버 추가 ------------------------------------------------------
fn append_denied_only(denied: &[String], seen: &BTreeSet<String>, inventory: &mut Inventory) {
    for name in denied {
        if !seen.contains(name) {
            inventory.mcp_servers.push(McpEntry {
                name: name.clone(),
                transport: String::new(),
                target: String::new(),
                denied: true,
            });
        }
    }
}

// 7. 에이전트 읽기 ------------------------------------------------------------
fn read_agent_files(claude_home: &Path, inventory: &mut Inventory) {
    let dir = claude_home.join("agents");
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_markdown = path.extension().and_then(|ext| ext.to_str()) == Some("md");
        if !is_markdown || !path.is_file() {
            continue;
        }
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                inventory
                    .warnings
                    .push(format!("에이전트 읽기 실패 {}: {err}", path.display()));
                continue;
            }
        };
        let meta = parse_frontmatter(&text);
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("")
            .to_string();
        inventory.agents.push(AgentEntry {
            name: meta.get("name").cloned().unwrap_or(stem),
            model: meta.get("model").cloned().unwrap_or_default(),
            effort: meta.get("effort").cloned().unwrap_or_default(),
            max_turns: meta.get("maxTurns").and_then(|turns| turns.parse().ok()),
            path: path.display().to_string(),
        });
    }
    inventory
        .agents
        .sort_by(|left, right| left.name.cmp(&right.name));
}

// 8. 스킬 읽기 ----------------------------------------------------------------
fn read_skill_files(claude_home: &Path, inventory: &mut Inventory) {
    let dir = claude_home.join("skills");
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let skill_path = entry.path().join("SKILL.md");
        if !skill_path.is_file() {
            continue;
        }
        let text = match fs::read_to_string(&skill_path) {
            Ok(text) => text,
            Err(err) => {
                inventory
                    .warnings
                    .push(format!("스킬 읽기 실패 {}: {err}", skill_path.display()));
                continue;
            }
        };
        let meta = parse_frontmatter(&text);
        let folder = entry.file_name().to_string_lossy().to_string();
        inventory.skills.push(SkillEntry {
            name: meta.get("name").cloned().unwrap_or(folder),
            description_chars: meta
                .get("description")
                .map(|description| description.chars().count())
                .unwrap_or(0),
            path: skill_path.display().to_string(),
        });
    }
    inventory
        .skills
        .sort_by(|left, right| left.name.cmp(&right.name));
}

// 9. 머리글 해석 --------------------------------------------------------------
/// `---` 사이 `key: value` 줄을 읽음 (따옴표 제거, 들여쓴 이어짐 줄은 공백으로 이음).
fn parse_frontmatter(text: &str) -> HashMap<String, String> {
    let mut fields: HashMap<String, String> = HashMap::new();
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return fields;
    }
    let mut current_key: Option<String> = None;
    for line in lines {
        if line.trim_end() == "---" {
            break;
        }

        // 이어짐 줄
        if line.starts_with(' ') || line.starts_with('\t') {
            let existing = current_key.as_ref().and_then(|key| fields.get_mut(key));
            if let Some(existing) = existing {
                if !existing.is_empty() {
                    existing.push(' ');
                }
                existing.push_str(line.trim());
            }
            continue;
        }

        // 새 키 줄
        let Some((key, raw_value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.contains(' ') {
            continue;
        }
        let trimmed = raw_value.trim();
        let value = if matches!(trimmed, ">" | "|" | ">-" | "|-" | ">+" | "|+") {
            String::new()
        } else if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
            trimmed[1..trimmed.len() - 1].replace("\\\"", "\"")
        } else if trimmed.len() >= 2 && trimmed.starts_with('\'') && trimmed.ends_with('\'') {
            trimmed[1..trimmed.len() - 1].replace("''", "'")
        } else {
            trimmed.to_string()
        };
        fields.insert(key.to_string(), value);
        current_key = Some(key.to_string());
    }
    fields
}
