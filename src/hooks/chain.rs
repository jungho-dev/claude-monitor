//! hooks/chain.rs
//! pretool-chain 판정: Bash 재작성 후 가드 재검사, 가드 판정 합성, 실패 시 fail-closed.

use crate::core::config::HookConfig;
use crate::core::model::{DENY_ROUND_TRIP_MS, HookOutcome};
use crate::hooks::rewrite::{BashRewrite, rewrite_bash_command};
use crate::hooks::upstream::UpstreamClient;
use bytes::Bytes;
use serde_json::{Map, Value, json};
use std::time::Duration;

pub const FAIL_CLOSED_REASON: &str = "[rap] pretool-guard 응답 없음 (fail-closed)";
// 가드 사유에서 이름만 남기는 구형 CLI 목록 (명령 원문 보관 금지)
const LEGACY_NAMES: [&str; 9] = [
    "cat", "ls", "eza", "grep", "find", "du", "top", "btop", "git diff",
];

// 1. 판정 결과 ----------------------------------------------------------------
pub struct ChainResult {
    pub body: Bytes,
    pub outcome: HookOutcome,
    pub guard: String,
    pub detail: String,
    pub session_id: String,
    pub saved_ms: Option<i64>,
}

// 2. 체인 판정 ----------------------------------------------------------------
/// 재작성 -> 가드 전달 -> 가드 판정 합성. 응답 본문과 기록용 요약 반환.
pub async fn evaluate_pretool_chain(
    upstream: &UpstreamClient,
    hooks: &HookConfig,
    body: &[u8],
) -> ChainResult {
    // 1. 입력 해석
    let payload: Option<Value> = serde_json::from_slice(body).ok();
    let session_id = payload
        .as_ref()
        .and_then(|value| value.get("session_id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    // 2. Bash 재작성
    let mut rewrite: Option<BashRewrite> = None;
    let mut forward_body = Bytes::copy_from_slice(body);
    let found = match (&payload, hooks.rewrite_enabled) {
        (Some(payload), true) => {
            let is_bash = payload.get("tool_name").and_then(Value::as_str) == Some("Bash");
            let command = payload
                .pointer("/tool_input/command")
                .and_then(Value::as_str);
            match (is_bash, command) {
                (true, Some(command)) => rewrite_bash_command(command),
                _ => None,
            }
        }
        _ => None,
    };
    if let (Some(found), Some(payload)) = (found, &payload) {
        let mut patched = payload.clone();
        patched["tool_input"]["command"] = Value::String(found.command.clone());
        if let Ok(bytes) = serde_json::to_vec(&patched) {
            forward_body = Bytes::from(bytes);
            rewrite = Some(found);
        }
    }
    let rewrite_names: String = match &rewrite {
        Some(found) => found
            .notes
            .iter()
            .filter_map(|note| note.split(" → ").next())
            .collect::<Vec<&str>>()
            .join(","),
        None => String::new(),
    };

    // 3. 가드 전달 (시한 초과·비정상 응답은 fail-closed)
    let url = format!(
        "{}/hook/pretool-guard",
        hooks.relay_target.trim_end_matches('/')
    );
    let deadline = Duration::from_millis(hooks.guard_deadline_ms);
    let reply = upstream
        .post_json_with_deadline(&url, forward_body, deadline)
        .await;
    let mut guard_bytes = match reply {
        Ok((status, bytes)) if status.is_success() => bytes,
        _ => return fail_closed_result(session_id, rewrite_names),
    };
    let mut guard_json: Map<String, Value> = match serde_json::from_slice(&guard_bytes) {
        Ok(Value::Object(map)) => map,
        _ => return fail_closed_result(session_id, rewrite_names),
    };

    // 3-1. 재작성 후 보류·미지 판정 (가드가 못 본 원본 명령 재검사)
    let first_decision = guard_json
        .get("hookSpecificOutput")
        .and_then(|specific| specific.get("permissionDecision"))
        .and_then(Value::as_str);
    let first_blocked = guard_json.get("decision").and_then(Value::as_str) == Some("block");
    let needs_recheck = rewrite.is_some()
        && !first_blocked
        && !matches!(first_decision, None | Some("allow" | "ask" | "deny"));
    if needs_recheck {
        rewrite = None;
        let recheck = upstream
            .post_json_with_deadline(&url, Bytes::copy_from_slice(body), deadline)
            .await;
        guard_bytes = match recheck {
            Ok((status, bytes)) if status.is_success() => bytes,
            _ => return fail_closed_result(session_id, rewrite_names),
        };
        guard_json = match serde_json::from_slice(&guard_bytes) {
            Ok(Value::Object(map)) => map,
            _ => return fail_closed_result(session_id, rewrite_names),
        };
    }

    // 4. 가드 거부 (본문 그대로 반환)
    let decision = guard_json
        .get("hookSpecificOutput")
        .and_then(|specific| specific.get("permissionDecision"))
        .and_then(Value::as_str);
    let blocked = guard_json.get("decision").and_then(Value::as_str) == Some("block");
    if decision == Some("deny") || blocked {
        let reason = guard_json
            .get("hookSpecificOutput")
            .and_then(|specific| specific.get("permissionDecisionReason"))
            .and_then(Value::as_str)
            .or_else(|| guard_json.get("reason").and_then(Value::as_str))
            .unwrap_or("");
        let mut detail = extract_legacy_names(reason);
        if detail.is_empty() {
            detail = rewrite_names;
        }
        return ChainResult {
            body: guard_bytes,
            outcome: HookOutcome::Deny,
            guard: extract_guard_label(reason),
            detail,
            session_id,
            saved_ms: None,
        };
    }

    // 5. 재작성 없음·가드 자체 입력 교체·보류·미지 판정 (가드 본문 그대로)
    let guard_replaced_input = guard_json
        .get("hookSpecificOutput")
        .and_then(|specific| specific.get("updatedInput"))
        .is_some();
    let decision_name: Option<String> = decision.map(str::to_string);
    let decision_rewritable = matches!(decision_name.as_deref(), None | Some("allow" | "ask"));
    let rewrite = match rewrite {
        Some(found) if decision_rewritable && !guard_replaced_input => found,
        _ => {
            return ChainResult {
                body: guard_bytes,
                outcome: HookOutcome::Allow,
                guard: String::new(),
                detail: String::new(),
                session_id,
                saved_ms: None,
            };
        }
    };

    // 6. 재작성 응답 합성 (가드가 내린 판정만 유지, 허용 신규 생성 금지)
    let mut root = guard_json;
    let mut specific = match root.remove("hookSpecificOutput") {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    let guard_context = specific
        .get("additionalContext")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let verdict_note = match decision_name.as_deref() {
        Some("allow") => "가드 재검사 통과",
        Some("ask") => "가드 재검사 확인 요청",
        _ => "가드 재검사 판정 없음",
    };
    let rap_context = format!(
        "[rap] 명령 자동 변환: {} ({verdict_note})",
        rewrite.notes.join(", ")
    );
    let context = if guard_context.is_empty() {
        rap_context
    } else {
        format!("{guard_context}\n{rap_context}")
    };
    let mut updated_input = payload
        .as_ref()
        .and_then(|value| value.get("tool_input"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    updated_input["command"] = Value::String(rewrite.command.clone());
    specific.insert("hookEventName".to_string(), json!("PreToolUse"));
    match decision_name.as_deref() {
        Some("allow") => {
            specific.insert("permissionDecision".to_string(), json!("allow"));
            specific.insert(
                "permissionDecisionReason".to_string(),
                json!("[rap] rewrite"),
            );
        }
        Some("ask") => {}
        _ => {
            specific.remove("permissionDecision");
            specific.remove("permissionDecisionReason");
        }
    }
    specific.insert("updatedInput".to_string(), updated_input);
    specific.insert("additionalContext".to_string(), Value::String(context));
    root.insert("hookSpecificOutput".to_string(), Value::Object(specific));
    let body = serde_json::to_vec(&Value::Object(root)).unwrap_or_default();
    ChainResult {
        body: Bytes::from(body),
        outcome: HookOutcome::Rewrite,
        guard: String::new(),
        detail: rewrite_names,
        session_id,
        saved_ms: Some(DENY_ROUND_TRIP_MS),
    }
}

// 3. fail-closed 판정 ---------------------------------------------------------
/// 가드 판정 불가 시 거부 응답 본문 (command 훅 클라이언트 공용).
pub(crate) fn fail_closed_body() -> Vec<u8> {
    let body = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": FAIL_CLOSED_REASON,
        }
    });
    serde_json::to_vec(&body).unwrap_or_default()
}

fn fail_closed_result(session_id: String, detail: String) -> ChainResult {
    ChainResult {
        body: Bytes::from(fail_closed_body()),
        outcome: HookOutcome::Error,
        guard: "rap".to_string(),
        detail,
        session_id,
        saved_ms: None,
    }
}

// 4. 가드 라벨 추출 -----------------------------------------------------------
/// 사유의 첫 `[label]` (소문자·숫자·하이픈) 에서 label 만 추출, 없으면 빈 문자열.
pub fn extract_guard_label(reason: &str) -> String {
    let Some(open) = reason.find('[') else {
        return String::new();
    };
    let Some(length) = reason[open + 1..].find(']') else {
        return String::new();
    };
    let label = &reason[open + 1..open + 1 + length];
    let valid = !label.is_empty()
        && label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        label.to_string()
    } else {
        String::new()
    }
}

// 5. 구형 명령 이름 추출 ------------------------------------------------------
/// 사유의 `감지:` 뒤 항목 중 구형 CLI 이름만 쉼표로 이어 반환 (원문 보관 금지).
pub fn extract_legacy_names(reason: &str) -> String {
    let Some(marker) = reason.find("감지:") else {
        return String::new();
    };
    let tail = &reason[marker + "감지:".len()..];
    let items = &tail[..tail.find(['\n', ')']).unwrap_or(tail.len())];
    let mut names: Vec<&str> = Vec::new();
    for piece in items.split(',') {
        let name = piece.trim();
        if LEGACY_NAMES.contains(&name) && !names.contains(&name) {
            names.push(name);
        }
    }
    names.join(",")
}
