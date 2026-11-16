//! power/tests.rs
//! 대기 구간 판정, 절전 유예 전환, 복귀 안내 문구 테스트.

use super::{HintEvent, MonitorTick, PowerState, StandbyGap};
use crate::core::config::PowerConfig;
use crate::hooks::standby::evaluate_standby_hint;
use crate::net::admin::test_support::{build_test_config, build_test_context};
use chrono::Utc;
use serde_json::Value;
use std::sync::atomic::Ordering;

const BASE_MS: i64 = 1_800_000_000_000;
const MINUTE_MS: i64 = 60_000;

// 1. 시험용 구간 --------------------------------------------------------------
fn detected_gap(start_ms: i64, end_ms: i64, active: bool) -> StandbyGap {
    StandbyGap {
        id: 0,
        start_ms,
        end_ms,
        active,
    }
}

// 2. 대기 구간 판정 -----------------------------------------------------------
#[test]
fn advance_tick_reports_gap_with_activity_before_standby() {
    let config = PowerConfig::default();
    let mut tick = MonitorTick {
        last_wall_ms: BASE_MS,
        last_activity_ms: i64::MIN,
        last_mark: 0,
    };

    // 1. 정상 주기 (활동 표식 변화)
    let normal = tick.advance_tick(BASE_MS + 5_000, 1, false, &config);
    assert_eq!(normal.gap, None);
    assert!(normal.active);

    // 2. 14분 도약은 활동 중 구간
    let resumed_ms = BASE_MS + 5_000 + 14 * MINUTE_MS;
    let jumped = tick.advance_tick(resumed_ms, 1, false, &config);
    assert_eq!(
        jumped.gap,
        Some(detected_gap(BASE_MS + 5_000, resumed_ms, true))
    );
    assert!(!jumped.active);

    // 3. 활동 없던 상태의 도약
    let idle_ms = resumed_ms + 20 * MINUTE_MS;
    let idle = tick.advance_tick(idle_ms, 1, false, &config);
    assert_eq!(idle.gap, Some(detected_gap(resumed_ms, idle_ms, false)));
}

// 3. 절전 유예 전환 -----------------------------------------------------------
#[test]
fn advance_tick_holds_within_grace_and_releases_after() {
    let config = PowerConfig::default();
    let grace_ms = i64::try_from(config.awake_grace_secs * 1000).expect("유예 ms");
    let mut tick = MonitorTick {
        last_wall_ms: BASE_MS,
        last_activity_ms: i64::MIN,
        last_mark: 0,
    };

    // 1. 활동 없음
    assert!(!tick.advance_tick(BASE_MS + 5_000, 0, false, &config).hold);

    // 2. 표식 변화로 유예 시작, 유예 안 유지
    assert!(tick.advance_tick(BASE_MS + 10_000, 3, false, &config).hold);
    assert!(
        tick.advance_tick(BASE_MS + 10_000 + grace_ms - 1, 3, false, &config)
            .hold
    );

    // 3. 유예 경과 후 해제, 스트림 중이면 다시 유지
    assert!(
        !tick
            .advance_tick(BASE_MS + 10_000 + grace_ms, 3, false, &config)
            .hold
    );
    assert!(
        tick.advance_tick(BASE_MS + 20_000 + grace_ms, 3, true, &config)
            .hold
    );

    // 4. 시계 역행은 유예 종료
    assert!(!tick.advance_tick(BASE_MS, 3, false, &config).hold);

    // 5. keep_awake 끔 (활동 판정은 유지)
    let off = PowerConfig {
        keep_awake: false,
        ..PowerConfig::default()
    };
    let disabled = tick.advance_tick(BASE_MS + 5_000, 9, true, &off);
    assert!(!disabled.hold);
    assert!(disabled.active);
}

// 4. 도구 실패 안내 -----------------------------------------------------------
#[test]
fn tool_failure_hint_requires_overlap_with_run_window() {
    let config = PowerConfig::default();
    let state = PowerState::default();
    state.record_gap(detected_gap(BASE_MS, BASE_MS + 14 * MINUTE_MS, false));
    let now_ms = BASE_MS + 15 * MINUTE_MS;

    // 1. 실행 구간이 대기와 겹침 (활동 여부 무관)
    let overlapping = HintEvent::ToolFailure {
        duration_ms: Some(16 * MINUTE_MS),
    };
    let hint = state
        .build_standby_hint("s1", overlapping, now_ms, &config)
        .expect("겹친 실패 안내");
    assert!(hint.contains("[cm standby]"), "{hint}");
    assert!(hint.contains("(14분)"), "{hint}");

    // 2. 같은 세션 재실패도 안내 (중복 억제 없음)
    assert!(
        state
            .build_standby_hint("s1", overlapping, now_ms, &config)
            .is_some()
    );

    // 3. 복귀 뒤 시작한 짧은 실행
    let after = HintEvent::ToolFailure {
        duration_ms: Some(10_000),
    };
    assert_eq!(state.build_standby_hint("s1", after, now_ms, &config), None);
}

// 5. 실행 시간 없는 실패 ------------------------------------------------------
#[test]
fn failure_without_duration_gets_conditional_hint_once() {
    let config = PowerConfig::default();
    let state = PowerState::default();
    state.record_gap(detected_gap(BASE_MS, BASE_MS + 10 * MINUTE_MS, true));
    let now_ms = BASE_MS + 11 * MINUTE_MS;
    let unknown = HintEvent::ToolFailure { duration_ms: None };

    // 1. 조건부 문구, 세션당 1회
    let hint = state
        .build_standby_hint("s1", unknown, now_ms, &config)
        .expect("조건부 안내");
    assert!(hint.contains("실행 시간은 알 수 없음"), "{hint}");
    assert_eq!(
        state.build_standby_hint("s1", unknown, now_ms, &config),
        None
    );
}

// 6. 프롬프트 안내 ------------------------------------------------------------
#[test]
fn prompt_hint_covers_active_gaps_once_per_session_within_window() {
    let config = PowerConfig::default();
    let state = PowerState::default();
    state.record_gap(detected_gap(BASE_MS, BASE_MS + 10 * MINUTE_MS, true));
    let now_ms = BASE_MS + 11 * MINUTE_MS;

    // 1. 세션마다 한 번
    assert!(
        state
            .build_standby_hint("s1", HintEvent::PromptSubmit, now_ms, &config)
            .is_some()
    );
    assert_eq!(
        state.build_standby_hint("s1", HintEvent::PromptSubmit, now_ms, &config),
        None
    );
    assert!(
        state
            .build_standby_hint("s2", HintEvent::PromptSubmit, now_ms, &config)
            .is_some()
    );

    // 2. 창 밖의 오래된 구간
    let window_ms = i64::try_from(config.hint_window_secs * 1000).expect("창 ms");
    let late_ms = BASE_MS + 10 * MINUTE_MS + window_ms + 1;
    assert_eq!(
        state.build_standby_hint("s3", HintEvent::PromptSubmit, late_ms, &config),
        None
    );

    // 3. 활동 없던 구간은 제외
    let idle = PowerState::default();
    idle.record_gap(detected_gap(BASE_MS, BASE_MS + 10 * MINUTE_MS, false));
    assert_eq!(
        idle.build_standby_hint("s1", HintEvent::PromptSubmit, now_ms, &config),
        None
    );
}

// 7. 미기록 진행 구간 ---------------------------------------------------------
#[test]
fn pending_gap_counts_before_monitor_wakes_and_dedupes_after_record() {
    let config = PowerConfig::default();
    let state = PowerState::default();
    state.last_tick_ms.store(BASE_MS, Ordering::Relaxed);
    state.last_active.store(true, Ordering::Relaxed);

    // 1. 마지막 주기가 기준보다 오래됨
    let now_ms = BASE_MS + 5 * MINUTE_MS;
    let event = HintEvent::ToolFailure {
        duration_ms: Some(6 * MINUTE_MS),
    };
    let hint = state
        .build_standby_hint("s1", event, now_ms, &config)
        .expect("진행 구간 안내");
    assert!(hint.contains("(5분)"), "{hint}");

    // 2. 진행 구간 프롬프트 안내 뒤 같은 구간 기록은 재안내 없음
    assert!(
        state
            .build_standby_hint("s1", HintEvent::PromptSubmit, now_ms, &config)
            .is_some()
    );
    state.record_gap(detected_gap(BASE_MS, now_ms + 3_000, true));
    state.last_tick_ms.store(now_ms + 3_000, Ordering::Relaxed);
    assert_eq!(
        state.build_standby_hint("s1", HintEvent::PromptSubmit, now_ms + 4_000, &config),
        None
    );

    // 3. 최근 주기면 진행 구간 없음
    let fresh = PowerState::default();
    fresh.last_tick_ms.store(now_ms - 5_000, Ordering::Relaxed);
    assert_eq!(fresh.build_standby_hint("s1", event, now_ms, &config), None);

    // 4. 감시 종료 표식(0)이면 진행 구간 없음
    let stopped = PowerState::default();
    assert_eq!(
        stopped.build_standby_hint("s1", event, now_ms, &config),
        None
    );
}

// 8. 보관 상한 ----------------------------------------------------------------
#[test]
fn record_gap_keeps_latest_entries_only() {
    let state = PowerState::default();
    for index in 0..40 {
        state.record_gap(detected_gap(BASE_MS + index, BASE_MS + index + 1, true));
    }
    let gaps = state.list_gaps();
    assert_eq!(gaps.len(), 32);
    assert_eq!(gaps[0].id, 9);
    assert_eq!(gaps[31].id, 40);
}

// 9. 훅 응답 형식 -------------------------------------------------------------
#[test]
fn standby_hook_answers_context_or_empty_object() {
    let ctx = build_test_context(build_test_config("standby-hint"));
    let now_ms = Utc::now().timestamp_millis();
    ctx.power.record_gap(detected_gap(
        now_ms - 20 * MINUTE_MS,
        now_ms - 2 * MINUTE_MS,
        true,
    ));

    // 1. 겹친 도구 실패
    let failure =
        br#"{"session_id":"s1","hook_event_name":"PostToolUseFailure","duration_ms":1500000}"#;
    let answered = evaluate_standby_hint(&ctx, failure);
    assert!(answered.hinted);
    assert_eq!(answered.session_id, "s1");
    let parsed: Value = serde_json::from_slice(&answered.body).expect("JSON 응답");
    let specific = &parsed["hookSpecificOutput"];
    assert_eq!(specific["hookEventName"], "PostToolUseFailure");
    assert!(
        specific["additionalContext"]
            .as_str()
            .is_some_and(|text| text.contains("[cm standby]"))
    );

    // 2. 대상 아닌 이벤트와 깨진 본문
    for body in [
        br#"{"session_id":"s1","hook_event_name":"PostToolUse"}"#.as_slice(),
        b"not json".as_slice(),
    ] {
        let skipped = evaluate_standby_hint(&ctx, body);
        assert!(!skipped.hinted);
        assert_eq!(skipped.body.as_ref(), b"{}");
    }
}

// 10. 기능 꺼짐 ---------------------------------------------------------------
#[test]
fn standby_hook_is_silent_when_power_disabled() {
    let mut config = build_test_config("standby-off");
    config.power.enabled = false;
    let ctx = build_test_context(config);
    let now_ms = Utc::now().timestamp_millis();
    ctx.power.record_gap(detected_gap(
        now_ms - 5 * MINUTE_MS,
        now_ms - MINUTE_MS,
        true,
    ));
    let prompt = br#"{"session_id":"s1","hook_event_name":"UserPromptSubmit"}"#;
    let skipped = evaluate_standby_hint(&ctx, prompt);
    assert!(!skipped.hinted);
    assert_eq!(skipped.body.as_ref(), b"{}");
}
