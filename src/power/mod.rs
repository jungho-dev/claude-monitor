//! power/mod.rs
//! 최신 대기 대응: 작업 중 절전 유예 요청, 대기 구간 감지, 복귀 안내 문구.

#[cfg(test)]
mod tests;

use crate::core::config::PowerConfig;
use crate::core::context::AppContext;
use crate::core::model::LiveEvent;
use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde::Serialize;
use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub const TICK_INTERVAL: Duration = Duration::from_secs(5);
const MAX_KEPT_GAPS: usize = 32;
const MAX_NOTIFIED_PAIRS: usize = 512;

// 1. Win32 FFI 선언 -----------------------------------------------------------
#[cfg(windows)]
mod ffi {
    pub const ES_CONTINUOUS: u32 = 0x8000_0000;
    pub const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn SetThreadExecutionState(flags: u32) -> u32;
    }
}

// 2. 대기 구간 ----------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StandbyGap {
    pub id: u64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub active: bool,
}

// 3. 안내 대상 훅 -------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintEvent {
    ToolFailure { duration_ms: Option<i64> },
    PromptSubmit,
}

// 4. 감시 주기 판정 -----------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickOutcome {
    pub gap: Option<StandbyGap>,
    pub hold: bool,
    pub active: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct MonitorTick {
    pub last_wall_ms: i64,
    pub last_activity_ms: i64,
    pub last_mark: u64,
}

impl MonitorTick {
    /// 벽시계 도약은 대기 구간, 활동 표식 변화·스트림은 유예 연장.
    pub fn advance_tick(
        &mut self,
        now_ms: i64,
        activity_mark: u64,
        streaming: bool,
        config: &PowerConfig,
    ) -> TickOutcome {
        // 1. 직전 주기의 활동 (대기 직전 상태)
        let grace_ms =
            i64::try_from(config.awake_grace_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
        let since_last_ms = self.last_wall_ms.saturating_sub(self.last_activity_ms);
        let was_active = (0..grace_ms).contains(&since_last_ms);

        // 2. 대기 구간 (주기보다 크게 벌어진 시계)
        let threshold_ms =
            i64::try_from(config.gap_threshold_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
        let gap = (now_ms.saturating_sub(self.last_wall_ms) > threshold_ms).then_some(StandbyGap {
            id: 0,
            start_ms: self.last_wall_ms,
            end_ms: now_ms,
            active: was_active,
        });
        self.last_wall_ms = now_ms;

        // 3. 활동 판정 (시계 역행은 유예 종료)
        if streaming || activity_mark != self.last_mark {
            self.last_activity_ms = now_ms;
            self.last_mark = activity_mark;
        }
        let since_now_ms = now_ms.saturating_sub(self.last_activity_ms);
        let active = (0..grace_ms).contains(&since_now_ms);
        TickOutcome {
            gap,
            hold: config.keep_awake && active,
            active,
        }
    }
}

// 5. 전원 상태 ----------------------------------------------------------------
#[derive(Debug, Default)]
pub struct PowerState {
    pub holding: AtomicBool,
    pub last_tick_ms: AtomicI64,
    pub last_active: AtomicBool,
    ledger: Mutex<GapLedger>,
}

#[derive(Debug, Default)]
struct GapLedger {
    gaps: VecDeque<StandbyGap>,
    next_id: u64,
    notified: VecDeque<(String, i64)>,
}

impl PowerState {
    /// 번호 부여 후 보관 (상한 초과 시 오래된 것부터 버림).
    pub fn record_gap(&self, detected: StandbyGap) -> StandbyGap {
        let mut ledger = self
            .ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        ledger.next_id += 1;
        let gap = StandbyGap {
            id: ledger.next_id,
            ..detected
        };
        ledger.gaps.push_back(gap);
        while ledger.gaps.len() > MAX_KEPT_GAPS {
            ledger.gaps.pop_front();
        }
        gap
    }

    pub fn list_gaps(&self) -> Vec<StandbyGap> {
        let ledger = self
            .ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        ledger.gaps.iter().copied().collect()
    }

    /// 대기 구간이 걸친 실패·복귀 직후 프롬프트에 붙일 안내 문구.
    pub fn build_standby_hint(
        &self,
        session_id: &str,
        event: HintEvent,
        now_ms: i64,
        config: &PowerConfig,
    ) -> Option<String> {
        // 1. 감시 스레드가 아직 깨어나지 않은 진행 중 구간
        let threshold_ms =
            i64::try_from(config.gap_threshold_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
        let last_tick_ms = self.last_tick_ms.load(Ordering::Relaxed);
        let pending = (last_tick_ms > 0 && now_ms.saturating_sub(last_tick_ms) > threshold_ms)
            .then_some(StandbyGap {
                id: 0,
                start_ms: last_tick_ms,
                end_ms: now_ms,
                active: self.last_active.load(Ordering::Relaxed),
            });

        // 2. 대상 구간 선택
        let mut ledger = self
            .ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let candidates: Vec<StandbyGap> = ledger.gaps.iter().copied().chain(pending).collect();
        let selected: Vec<StandbyGap> = match event {
            // 도구 실행 구간과 겹친 대기
            HintEvent::ToolFailure {
                duration_ms: Some(duration_ms),
            } => {
                let tick_ms = i64::try_from(TICK_INTERVAL.as_millis()).unwrap_or(0);
                let started_ms = now_ms
                    .saturating_sub(duration_ms.max(0))
                    .saturating_sub(tick_ms);
                candidates
                    .into_iter()
                    .filter(|gap| gap.end_ms >= started_ms && gap.start_ms <= now_ms)
                    .collect()
            }
            // 활동 중 끊긴 최근 대기 (세션당 1회)
            HintEvent::ToolFailure { duration_ms: None } | HintEvent::PromptSubmit => {
                let window_ms =
                    i64::try_from(config.hint_window_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
                let window_start_ms = now_ms.saturating_sub(window_ms);
                let fresh: Vec<StandbyGap> = candidates
                    .into_iter()
                    .filter(|gap| gap.active && gap.end_ms >= window_start_ms)
                    .filter(|gap| {
                        !ledger.notified.iter().any(|(session, start_ms)| {
                            session == session_id && *start_ms == gap.start_ms
                        })
                    })
                    .collect();
                for gap in &fresh {
                    ledger
                        .notified
                        .push_back((session_id.to_string(), gap.start_ms));
                }
                while ledger.notified.len() > MAX_NOTIFIED_PAIRS {
                    ledger.notified.pop_front();
                }
                fresh
            }
        };
        drop(ledger);
        if selected.is_empty() {
            return None;
        }

        // 3. 구간 목록 문구 (현지 시각)
        let clock = |ms: i64| {
            DateTime::<Utc>::from_timestamp_millis(ms)
                .map(|at| at.with_timezone(&Local).format("%H:%M:%S").to_string())
                .unwrap_or_default()
        };
        let spans: Vec<String> = selected
            .iter()
            .map(|gap| {
                let minutes = gap.end_ms.saturating_sub(gap.start_ms) / 60_000;
                format!(
                    "{}~{} ({minutes}분)",
                    clock(gap.start_ms),
                    clock(gap.end_ms)
                )
            })
            .collect();
        let span_text = spans.join(", ");
        let hint = match event {
            HintEvent::ToolFailure {
                duration_ms: Some(_),
            } => format!(
                "[cm standby] 이 도구 실행 중 PC 가 최신 대기 모드였음: {span_text}. \
                 이 실패는 환경 원인일 가능성이 높음. 접근 방식을 바꾸지 말고 남은 부분 결과를 \
                 확인한 뒤 같은 단계를 한 번 다시 실행할 것."
            ),
            HintEvent::ToolFailure { duration_ms: None } => format!(
                "[cm standby] 최근 PC 가 최신 대기 모드였음: {span_text}. \
                 이 실패의 실행 시간은 알 수 없음. 오류가 시간 초과나 연결 끊김일 때만 대기 \
                 원인으로 보고 같은 단계를 한 번 다시 실행하고, 그 밖의 오류는 평소대로 다룰 것."
            ),
            HintEvent::PromptSubmit => format!(
                "[cm standby] 직전에 PC 가 최신 대기 모드였음: {span_text}. \
                 이 구간에 걸친 도구 실행, API 요청, 서브에이전트는 시간 초과나 연결 끊김으로 \
                 끝났을 수 있음. 진행 중이던 작업의 실제 상태를 먼저 확인하고, 대기로 끊긴 단계는 \
                 같은 방법으로 다시 실행할 것."
            ),
        };
        Some(hint)
    }
}

// 6. 감시 스레드 --------------------------------------------------------------
/// `cm-power` 스레드 기동 (절전 유예 요청은 스레드 단위).
pub fn spawn_power_monitor(ctx: Arc<AppContext>) -> io::Result<()> {
    thread::Builder::new()
        .name("cm-power".to_string())
        .spawn(move || {
            // 1. 시작 상태 (유예 없음, 첫 호출 반환값 0 대비 예비 호출)
            let _exit_guard = MonitorExitGuard { ctx: &ctx };
            let config = &ctx.config.power;
            let counters = &ctx.counters;
            let power = &ctx.power;
            let activity_mark = || {
                counters
                    .gateway_requests
                    .load(Ordering::Relaxed)
                    .wrapping_add(counters.hook_calls.load(Ordering::Relaxed))
            };
            let _ = apply_execution_state(false);
            let started_ms = Utc::now().timestamp_millis();
            power.last_tick_ms.store(started_ms, Ordering::Relaxed);
            let mut tick = MonitorTick {
                last_wall_ms: started_ms,
                last_activity_ms: i64::MIN,
                last_mark: activity_mark(),
            };

            loop {
                thread::sleep(TICK_INTERVAL);
                let now_ms = Utc::now().timestamp_millis();
                let streaming = counters.gateway_active_streams.load(Ordering::Relaxed) > 0;
                let outcome = tick.advance_tick(now_ms, activity_mark(), streaming, config);

                // 2. 대기 구간 기록·알림 (주기 시각보다 먼저 기록)
                if let Some(detected) = outcome.gap {
                    let gap = power.record_gap(detected);
                    let held = if power.holding.load(Ordering::Relaxed) {
                        "중"
                    } else {
                        "없음"
                    };
                    let text = format!(
                        "대기 구간 감지 #{}: {}초 (절전 유예 {held})",
                        gap.id,
                        (gap.end_ms - gap.start_ms) / 1000
                    );
                    let _ = writeln!(io::stderr(), "[cm] {text}");
                    let _ = ctx.live_tx.send(LiveEvent::Notice {
                        ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                        level: "warn".to_string(),
                        text,
                    });
                }
                power.last_active.store(outcome.active, Ordering::Relaxed);
                power.last_tick_ms.store(now_ms, Ordering::Relaxed);

                // 3. 절전 유예 전환
                if outcome.hold != power.holding.load(Ordering::Relaxed) {
                    if apply_execution_state(outcome.hold) {
                        power.holding.store(outcome.hold, Ordering::Relaxed);
                    } else {
                        let _ = writeln!(
                            io::stderr(),
                            "[cm] SetThreadExecutionState 실패 (요청 {})",
                            outcome.hold
                        );
                    }
                }
            }
        })
        .map(|_| ())
}

// 7. 감시 종료 표식 -----------------------------------------------------------
// panic 종료 시 진행 중 구간 판정 중단 (멈춘 주기 시각의 거짓 안내 방지)
struct MonitorExitGuard<'a> {
    ctx: &'a AppContext,
}

impl Drop for MonitorExitGuard<'_> {
    fn drop(&mut self) {
        let power = &self.ctx.power;
        power.last_tick_ms.store(0, Ordering::Relaxed);
        power.holding.store(false, Ordering::Relaxed);
        let text = "cm-power 종료: 대기 감시 중단 (serve 계속)".to_string();
        let _ = writeln!(io::stderr(), "[cm] {text}");
        let _ = self.ctx.live_tx.send(LiveEvent::Notice {
            ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            level: "warn".to_string(),
            text,
        });
    }
}

// 8. 절전 유예 요청 -----------------------------------------------------------
/// 시스템 절전만 유예 (화면 꺼짐·잠금 정책 유지).
fn apply_execution_state(hold: bool) -> bool {
    #[cfg(windows)]
    {
        let flags = if hold {
            ffi::ES_CONTINUOUS | ffi::ES_SYSTEM_REQUIRED
        } else {
            ffi::ES_CONTINUOUS
        };
        // SAFETY: 정수 플래그만 받는 호출, 포인터 인자 없음
        unsafe { ffi::SetThreadExecutionState(flags) != 0 }
    }
    #[cfg(not(windows))]
    {
        let _ = hold;
        false
    }
}
