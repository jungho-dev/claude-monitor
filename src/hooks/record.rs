//! hooks/record.rs
//! 훅 판정 기록: 카운터 증가, 실시간 이벤트 발행, 응답을 늦추지 않는 저장소 기록.

use crate::core::context::AppContext;
use crate::core::model::{DataSource, HookEventRow, HookOutcome, LiveEvent};
use chrono::{SecondsFormat, Utc};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_EVENT_SEQ: AtomicU64 = AtomicU64::new(1);

// 1. 훅 이벤트 기록 -----------------------------------------------------------
/// 호출자는 route·guard·outcome·detail·session_id·소요·절감만 채움 (출처·키·시각은 여기서 채움).
pub fn record_hook_event(ctx: &Arc<AppContext>, mut row: HookEventRow) {
    // 1. 식별 필드 채움
    let now = Utc::now();
    row.source = DataSource::Hook;
    row.ts = now.to_rfc3339_opts(SecondsFormat::Millis, true);
    row.event_key = format!(
        "hook-{}-{}-{}",
        now.timestamp_millis(),
        std::process::id(),
        NEXT_EVENT_SEQ.fetch_add(1, Ordering::Relaxed)
    );

    // 2. 카운터
    let counters = &ctx.counters;
    counters.hook_calls.fetch_add(1, Ordering::Relaxed);
    match row.outcome {
        HookOutcome::Allow => {}
        HookOutcome::Deny => {
            counters.hook_denials.fetch_add(1, Ordering::Relaxed);
        }
        HookOutcome::Rewrite => {
            counters.hook_rewrites.fetch_add(1, Ordering::Relaxed);
        }
        HookOutcome::Error => {
            counters.hook_errors.fetch_add(1, Ordering::Relaxed);
        }
    }
    if let Some(saved) = row.saved_ms {
        counters
            .saved_ms
            .fetch_add(u64::try_from(saved).unwrap_or(0), Ordering::Relaxed);
    }

    // 3. 실시간 이벤트 (수신자 없으면 무시)
    let _ = ctx.live_tx.send(LiveEvent::Hook(row.clone()));

    // 4. 저장소 기록 (응답과 분리)
    let store_ctx = Arc::clone(ctx);
    tokio::task::spawn_blocking(move || {
        let batch = crate::core::model::IngestBatch {
            hook_events: vec![row],
            ..Default::default()
        };
        if let Err(err) = store_ctx.store.insert_batch(&batch) {
            eprintln!("[rap] 훅 이벤트 기록 실패: {err:#}");
        }
    });
}
