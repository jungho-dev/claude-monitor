//! hooks/statusline.rs
//! statusLine 지표 수신: 한도·컨텍스트·캐시 스냅샷을 변경 시 또는 분 단위로 저장.

use crate::core::context::AppContext;
use crate::store::limits::LimitSnapshot;
use chrono::{SecondsFormat, Utc};
use serde_json::Value;
use std::sync::Arc;

// 문자열 필드 상한 (비정상 본문 차단)
const MAX_TEXT_BYTES: usize = 512;

// 1. 수신 결과 ----------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatuslineIntake {
    Stored,
    Skipped,
    Malformed,
}

// 2. 지표 수신 ----------------------------------------------------------------
/// 본문은 클라이언트가 추출한 지표 JSON, 시각은 서버가 채움. 형식 오류는 저장 없이 Malformed.
pub async fn record_statusline_snapshot(ctx: &Arc<AppContext>, body: &[u8]) -> StatuslineIntake {
    // 1. 본문 해석 (JSON 객체만 허용)
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return StatuslineIntake::Malformed;
    };
    if !value.is_object() {
        return StatuslineIntake::Malformed;
    }
    let Ok(mut snapshot) = serde_json::from_value::<LimitSnapshot>(value) else {
        return StatuslineIntake::Malformed;
    };

    // 2. 문자열 길이 검증
    let oversized = [
        Some(&snapshot.session_id),
        Some(&snapshot.model),
        snapshot.cache_ttl.as_ref(),
        snapshot.cache_last_miss_cause.as_ref(),
    ]
    .into_iter()
    .flatten()
    .any(|field| field.len() > MAX_TEXT_BYTES);
    if oversized {
        return StatuslineIntake::Malformed;
    }

    // 3. 지표 없음은 저장하지 않음 (식별 필드만 있는 본문)
    let identity_only = LimitSnapshot {
        session_id: snapshot.session_id.clone(),
        model: snapshot.model.clone(),
        ..LimitSnapshot::default()
    };
    snapshot.ts = String::new();
    if snapshot == identity_only {
        return StatuslineIntake::Skipped;
    }

    // 4. 기록 (응답 전에 끝내 시험·재시도가 결정적, 연결이 끊겨도 작업은 이어짐)
    snapshot.ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let store_ctx = Arc::clone(ctx);
    let stored =
        tokio::task::spawn_blocking(move || store_ctx.store.record_limit_snapshot(&snapshot)).await;
    match stored {
        Ok(Ok(true)) => StatuslineIntake::Stored,
        Ok(Ok(false)) => StatuslineIntake::Skipped,
        Ok(Err(err)) => {
            eprintln!("[cm] 한도 스냅샷 기록 실패: {err:#}");
            StatuslineIntake::Skipped
        }
        Err(err) => {
            eprintln!("[cm] 한도 스냅샷 작업 실패: {err}");
            StatuslineIntake::Skipped
        }
    }
}
