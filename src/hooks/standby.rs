//! hooks/standby.rs
//! 대기 복귀 안내 훅: 도구 실패·프롬프트 제출에 대기 구간 문맥 추가.

use crate::core::context::AppContext;
use crate::power::HintEvent;
use bytes::Bytes;
use chrono::Utc;
use serde_json::{Value, json};

// 1. 안내 판정 결과 -----------------------------------------------------------
pub struct StandbyHintResult {
    pub body: Bytes,
    pub session_id: String,
    pub hinted: bool,
}

// 2. 안내 판정 ----------------------------------------------------------------
/// 본문은 이벤트 이름·세션·소요 시간만 읽음, 안내 없으면 빈 객체.
pub fn evaluate_standby_hint(ctx: &AppContext, body: &[u8]) -> StandbyHintResult {
    // 1. 입력 필드
    let payload: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let event_name = payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let event = match event_name {
        "PostToolUseFailure" => Some(HintEvent::ToolFailure {
            duration_ms: payload.get("duration_ms").and_then(Value::as_i64),
        }),
        "UserPromptSubmit" => Some(HintEvent::PromptSubmit),
        _ => None,
    };

    // 2. 안내 문구
    let hint = match event {
        Some(event) if ctx.config.power.enabled => ctx.power.build_standby_hint(
            &session_id,
            event,
            Utc::now().timestamp_millis(),
            &ctx.config.power,
        ),
        _ => None,
    };

    // 3. 응답 본문
    let answer = match &hint {
        Some(text) => json!({
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "additionalContext": text,
            }
        }),
        None => json!({}),
    };
    StandbyHintResult {
        body: Bytes::from(answer.to_string()),
        session_id,
        hinted: hint.is_some(),
    }
}
