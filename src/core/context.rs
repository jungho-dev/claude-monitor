//! core/context.rs
//! serve 실행 중 모든 포트·작업이 공유하는 상태 (설정, 저장소, 이벤트 버스, 카운터).

use crate::core::config::AppConfig;
use crate::core::model::{CounterSnapshot, LiveEvent};
use crate::store::Store;
use chrono::{DateTime, Utc};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;

pub const LIVE_CHANNEL_CAPACITY: usize = 1024;

// 1. 실시간 카운터 ------------------------------------------------------------
#[derive(Debug, Default)]
pub struct LiveCounters {
    pub api_requests: AtomicU64,
    pub tool_calls: AtomicU64,
    pub hook_calls: AtomicU64,
    pub hook_rewrites: AtomicU64,
    pub hook_denials: AtomicU64,
    pub hook_errors: AtomicU64,
    pub saved_ms: AtomicU64,
    pub gateway_requests: AtomicU64,
    pub gateway_active_streams: AtomicU64,
    pub otlp_batches: AtomicU64,
    pub index_runs: AtomicU64,
}

// 2. 공유 상태 ----------------------------------------------------------------
// 비밀값 보유로 Debug 미구현
pub struct AppContext {
    pub config: AppConfig,
    pub store: Store,
    pub live_tx: broadcast::Sender<LiveEvent>,
    pub counters: LiveCounters,
    pub started_at: DateTime<Utc>,
    pub admin_token: String,
}

// 3. 카운터 스냅샷 ------------------------------------------------------------
impl LiveCounters {
    pub fn snapshot_counters(&self) -> CounterSnapshot {
        CounterSnapshot {
            api_requests: self.api_requests.load(Ordering::Relaxed),
            tool_calls: self.tool_calls.load(Ordering::Relaxed),
            hook_calls: self.hook_calls.load(Ordering::Relaxed),
            hook_rewrites: self.hook_rewrites.load(Ordering::Relaxed),
            hook_denials: self.hook_denials.load(Ordering::Relaxed),
            hook_errors: self.hook_errors.load(Ordering::Relaxed),
            saved_ms: self.saved_ms.load(Ordering::Relaxed),
            gateway_requests: self.gateway_requests.load(Ordering::Relaxed),
            gateway_active_streams: self.gateway_active_streams.load(Ordering::Relaxed),
            otlp_batches: self.otlp_batches.load(Ordering::Relaxed),
            index_runs: self.index_runs.load(Ordering::Relaxed),
        }
    }
}
