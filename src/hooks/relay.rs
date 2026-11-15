//! hooks/relay.rs
//! 계측 중계: 지정 경로 훅을 기존 서버로 그대로 전달하고 판정·지연을 요약.

use crate::core::config::HookConfig;
use crate::core::model::HookOutcome;
use crate::hooks::chain::extract_guard_label;
use crate::hooks::upstream::UpstreamClient;
use bytes::Bytes;
use hyper::StatusCode;
use serde_json::Value;
use std::time::Duration;

// 중계 대상 훅의 Claude Code 측 시한(5~10초)보다 짧게 둔 상한
const RELAY_DEADLINE_MS: u64 = 9000;

// 1. 중계 결과 ----------------------------------------------------------------
pub struct RelayResult {
    pub status: StatusCode,
    pub body: Bytes,
    pub outcome: HookOutcome,
    pub guard: String,
    pub session_id: String,
    // 업스트림 실패 시 502 안내문 (성공이면 빈 문자열)
    pub failure: String,
}

// 2. 중계 실행 ----------------------------------------------------------------
/// `{relay_target}/hook/{route}` 로 본문 전달, 상태와 본문은 그대로 돌려줌.
pub async fn relay_hook_request(
    upstream: &UpstreamClient,
    hooks: &HookConfig,
    route: &str,
    body: Bytes,
) -> RelayResult {
    // 1. 세션 식별자 (본문 JSON 일 때만)
    let session_id = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();

    // 2. 업스트림 전달
    let url = format!("{}/hook/{route}", hooks.relay_target.trim_end_matches('/'));
    let deadline = Duration::from_millis(RELAY_DEADLINE_MS);
    let (status, reply) = match upstream.post_json_with_deadline(&url, body, deadline).await {
        Ok(reply) => reply,
        Err(err) => {
            return RelayResult {
                status: StatusCode::BAD_GATEWAY,
                body: Bytes::new(),
                outcome: HookOutcome::Error,
                guard: String::new(),
                session_id,
                failure: format!("relay 실패: {err}"),
            };
        }
    };

    // 3. 판정 해석 (거부·차단만 구분)
    let parsed: Option<Value> = serde_json::from_slice(&reply).ok();
    let mut outcome = HookOutcome::Allow;
    let mut guard = String::new();
    if let Some(Value::Object(map)) = &parsed {
        let specific = map.get("hookSpecificOutput");
        let denied = specific
            .and_then(|value| value.get("permissionDecision"))
            .and_then(Value::as_str)
            == Some("deny");
        let blocked = map.get("decision").and_then(Value::as_str) == Some("block");
        if denied || blocked {
            let reason = specific
                .and_then(|value| value.get("permissionDecisionReason"))
                .and_then(Value::as_str)
                .or_else(|| map.get("reason").and_then(Value::as_str))
                .unwrap_or("");
            outcome = HookOutcome::Deny;
            guard = extract_guard_label(reason);
        }
    }
    if !status.is_success() {
        outcome = HookOutcome::Error;
    }
    RelayResult {
        status,
        body: reply,
        outcome,
        guard,
        session_id,
        failure: String::new(),
    }
}
