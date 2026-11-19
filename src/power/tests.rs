//! power/tests.rs
//! 대기 구간 판정, 절전 유예 전환, 복귀 안내 문구 테스트.

use super::{HintEvent, MonitorTick, PowerState, StandbyGap, persist_gap};
use crate::core::config::PowerConfig;
use crate::core::model::LiveEvent;
use crate::hooks::standby::evaluate_standby_hint;
use crate::net::admin::test_support::{build_test_config, build_test_context};
use crate::store::power::MAX_STORED_GAPS;
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

// 11. 저장 후 복원 ------------------------------------------------------------
#[test]
fn persisted_gaps_reload_with_continuous_ids_and_still_hint() {
    let config = PowerConfig::default();
    let ctx = build_test_context(build_test_config("standby-reload"));
    let now_ms = Utc::now().timestamp_millis();

    // 1. 기록과 저장
    for index in 0..3 {
        let start_ms = now_ms - (30 - index * 10) * MINUTE_MS;
        let gap = ctx
            .power
            .record_gap(detected_gap(start_ms, start_ms + 5 * MINUTE_MS, true));
        assert!(persist_gap(&ctx, &gap));
    }

    // 2. 새 상태에 복원 (serve 재시작)
    let restarted = PowerState::default();
    assert_eq!(restarted.restore_gaps(&ctx.store).expect("복원"), 3);
    assert_eq!(restarted.list_gaps(), ctx.power.list_gaps());

    // 3. 번호 이어감
    let next = restarted.record_gap(detected_gap(now_ms, now_ms + MINUTE_MS, false));
    assert_eq!(next.id, 4);

    // 4. 복원 구간 안내 (재시작 전 안내 이력은 유지 안 함)
    assert!(
        restarted
            .build_standby_hint("s1", HintEvent::PromptSubmit, now_ms, &config)
            .is_some()
    );
}

// 12. 복원 상한 ---------------------------------------------------------------
#[test]
fn restore_loads_latest_entries_only() {
    let ctx = build_test_context(build_test_config("standby-restore-cap"));
    for index in 0..40 {
        let gap = ctx
            .power
            .record_gap(detected_gap(BASE_MS + index, BASE_MS + index + 1, true));
        assert!(persist_gap(&ctx, &gap));
    }
    let restarted = PowerState::default();
    assert_eq!(restarted.restore_gaps(&ctx.store).expect("복원"), 32);
    let gaps = restarted.list_gaps();
    assert_eq!(gaps[0].id, 9);
    assert_eq!(gaps[31].id, 40);
    assert_eq!(
        restarted
            .record_gap(detected_gap(BASE_MS, BASE_MS + 1, true))
            .id,
        41
    );

    // 빈 저장소는 번호 1부터
    let empty_ctx = build_test_context(build_test_config("standby-restore-empty"));
    let fresh = PowerState::default();
    assert_eq!(fresh.restore_gaps(&empty_ctx.store).expect("빈 복원"), 0);
    assert_eq!(
        fresh
            .record_gap(detected_gap(BASE_MS, BASE_MS + 1, true))
            .id,
        1
    );
}

// 13. 보관 상한 ---------------------------------------------------------------
#[test]
fn store_keeps_newest_rows_only() {
    let ctx = build_test_context(build_test_config("standby-retention"));
    let total = MAX_STORED_GAPS + 5;
    for id in 1..=total {
        let gap = StandbyGap {
            id: u64::try_from(id).expect("번호"),
            start_ms: BASE_MS + id,
            end_ms: BASE_MS + id + 1,
            active: true,
        };
        ctx.store.save_standby_gap(&gap).expect("저장");
    }
    let (rows, oldest): (i64, i64) = ctx
        .store
        .with_conn(|conn| {
            conn.query_row("SELECT COUNT(*), MIN(id) FROM standby_gaps", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
        })
        .expect("행 수 조회");
    assert_eq!(rows, MAX_STORED_GAPS);
    assert_eq!(oldest, 6);

    // 같은 번호 재저장은 덮어쓰기
    let rewritten = StandbyGap {
        id: 6,
        start_ms: 1,
        end_ms: 2,
        active: false,
    };
    ctx.store.save_standby_gap(&rewritten).expect("재저장");
    let loaded = ctx.store.load_standby_gaps(1000).expect("조회");
    assert_eq!(loaded.len(), 1000);
    assert_eq!(loaded[0], rewritten);
}

// 14. 저장 실패 ---------------------------------------------------------------
#[test]
fn persist_failure_notifies_and_keeps_ledger_working() {
    let ctx = build_test_context(build_test_config("standby-save-fail"));
    let mut live_rx = ctx.live_tx.subscribe();
    ctx.store
        .with_conn(|conn| conn.execute_batch("DROP TABLE standby_gaps;"))
        .expect("테이블 삭제");

    // 1. 저장 실패는 알림만
    let gap = ctx
        .power
        .record_gap(detected_gap(BASE_MS, BASE_MS + 10 * MINUTE_MS, true));
    assert!(!persist_gap(&ctx, &gap));
    match live_rx.try_recv().expect("알림 수신") {
        LiveEvent::Notice { level, text, .. } => {
            assert_eq!(level, "warn");
            assert!(text.contains("저장 실패"), "{text}");
        }
        other => panic!("예상 밖 이벤트: {other:?}"),
    }

    // 2. 메모리 보관분과 번호는 그대로
    let next = ctx
        .power
        .record_gap(detected_gap(BASE_MS, BASE_MS + MINUTE_MS, true));
    assert_eq!(next.id, gap.id + 1);
    assert_eq!(ctx.power.list_gaps().len(), 2);

    // 3. 복원 실패도 오류 반환 (감시 시작은 계속)
    assert!(PowerState::default().restore_gaps(&ctx.store).is_err());
}

// 14-1. 시작 복원 실패 뒤 번호 다시 맞춤 --------------------------------------
#[test]
fn failed_restore_is_retried_before_ids_overlap_stored_gaps() {
    let ctx = build_test_context(build_test_config("standby-sync-retry"));
    ctx.store
        .save_standby_gap(&StandbyGap {
            id: 5,
            start_ms: BASE_MS,
            end_ms: BASE_MS + MINUTE_MS,
            active: true,
        })
        .expect("이전 실행 구간");

    // 1. 시작 복원 실패 (표 일시 사라짐): 맞춤 실패
    ctx.store
        .with_conn(|conn| {
            conn.execute_batch("ALTER TABLE standby_gaps RENAME TO standby_gaps_off;")
        })
        .expect("표 이름 변경");
    let restarted = PowerState::default();
    assert!(restarted.restore_gaps(&ctx.store).is_err());
    assert!(!restarted.sync_gap_ids(&ctx.store));

    // 2. 저장소 복구 뒤 첫 기록 전 맞춤: 번호는 저장된 최대값에서 이어짐
    ctx.store
        .with_conn(|conn| {
            conn.execute_batch("ALTER TABLE standby_gaps_off RENAME TO standby_gaps;")
        })
        .expect("표 복구");
    assert!(restarted.sync_gap_ids(&ctx.store));
    let next = restarted.record_gap(detected_gap(
        BASE_MS + 2 * MINUTE_MS,
        BASE_MS + 3 * MINUTE_MS,
        true,
    ));
    assert_eq!(next.id, 6);
    assert!(persist_gap(&ctx, &next));
    let stored = ctx.store.load_standby_gaps(10).expect("조회");
    assert_eq!(
        stored.iter().map(|gap| gap.id).collect::<Vec<u64>>(),
        vec![5, 6]
    );

    // 3. 맞췄으면 다시 읽지 않음 (표가 없어도 true)
    ctx.store
        .with_conn(|conn| conn.execute_batch("DROP TABLE standby_gaps;"))
        .expect("표 삭제");
    assert!(restarted.sync_gap_ids(&ctx.store));
}
