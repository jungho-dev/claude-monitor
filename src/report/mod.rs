//! report/mod.rs
//! 하루 단위 에이전트 사용 종합 리포트 (Markdown) 생성·저장·예약.

mod render;
pub mod table;
#[cfg(test)]
mod tests;

use crate::core::config::AppConfig;
use crate::core::context::AppContext;
use crate::core::model::LiveEvent;
use crate::store::Store;
use anyhow::Context;
use chrono::{
    DateTime, Duration, Local, LocalResult, NaiveDate, NaiveTime, SecondsFormat, TimeZone, Utc,
};
use render::{ReportInput, render_report_markdown};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

const TOOL_LATENCY_LIMIT: usize = 10;
const CACHE_BREAK_LIMIT: usize = 10;
const SCHEDULE_MINUTE: u32 = 5;
const FIRST_INDEX_POLL_SECS: u64 = 2;
const FIRST_INDEX_WAIT_SECS: u64 = 10 * 60;

// 1. 로컬 자정의 UTC 변환 -----------------------------------------------------
// 자정이 없는 날 (DST 공백) 은 1시간 뒤, 중복 시각은 이른 쪽
fn local_midnight_utc(day: NaiveDate) -> DateTime<Utc> {
    let midnight = day.and_time(NaiveTime::MIN);
    match Local.from_local_datetime(&midnight) {
        LocalResult::Single(moment) => moment.with_timezone(&Utc),
        LocalResult::Ambiguous(earliest, _) => earliest.with_timezone(&Utc),
        LocalResult::None => match Local.from_local_datetime(&(midnight + Duration::hours(1))) {
            LocalResult::Single(moment) => moment.with_timezone(&Utc),
            LocalResult::Ambiguous(earliest, _) => earliest.with_timezone(&Utc),
            LocalResult::None => midnight.and_utc(),
        },
    }
}

// 2. 리포트 본문 생성 ---------------------------------------------------------
pub fn build_daily_report(store: &Store, report_date: NaiveDate) -> anyhow::Result<String> {
    // 1. 구간 (전일, 당일)
    let previous_date = report_date.pred_opt().unwrap_or(report_date);
    let next_date = report_date.succ_opt().unwrap_or(report_date);
    let previous_start =
        local_midnight_utc(previous_date).to_rfc3339_opts(SecondsFormat::Millis, true);
    let start = local_midnight_utc(report_date).to_rfc3339_opts(SecondsFormat::Millis, true);
    let end = local_midnight_utc(next_date).to_rfc3339_opts(SecondsFormat::Millis, true);

    // 2. 집계 조회
    let input = ReportInput {
        report_date,
        today: store.query_overview(&start, &end)?,
        previous: store.query_overview(&previous_start, &start)?,
        usage: store.query_model_usage(&start, &end)?,
        tools: store.query_tool_latency(&start, &end, TOOL_LATENCY_LIMIT)?,
        hooks: store.query_hook_stats(&start, &end)?,
        breaks: store.query_cache_breaks(&start, &end, CACHE_BREAK_LIMIT)?,
        gateway: store.query_gateway_timing(&start, &end)?,
        sources: store.query_source_counts(&start, &end)?,
    };
    Ok(render_report_markdown(&input))
}

// 3. 리포트 저장 (DB + 파일) --------------------------------------------------
pub fn write_daily_report(
    config: &AppConfig,
    store: &Store,
    report_date: NaiveDate,
) -> anyhow::Result<PathBuf> {
    // 1. 본문 생성·DB 보관
    let markdown = build_daily_report(store, report_date)?;
    let date_text = report_date.format("%Y-%m-%d").to_string();
    let generated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    store.save_daily_report(&date_text, &generated_at, &markdown)?;

    // 2. 파일 기록
    let dir = config.report_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("리포트 폴더 생성 실패: {}", dir.display()))?;
    let path = dir.join(format!("{date_text}.md"));
    fs::write(&path, markdown.as_bytes())
        .with_context(|| format!("리포트 파일 기록 실패: {}", path.display()))?;
    Ok(path)
}

// 4. 다음 실행까지 대기 시간 --------------------------------------------------
// 로컬 run_hour:05, 이미 지났으면 다음 날
pub fn compute_next_run_delay(now: DateTime<Local>, run_hour: u32) -> std::time::Duration {
    let run_time =
        NaiveTime::from_hms_opt(run_hour.min(23), SCHEDULE_MINUTE, 0).unwrap_or(NaiveTime::MIN);
    let mut target = now.date_naive().and_time(run_time);
    if target <= now.naive_local() {
        target += Duration::days(1);
    }
    let target_moment = match Local.from_local_datetime(&target) {
        LocalResult::Single(moment) => moment,
        LocalResult::Ambiguous(earliest, _) => earliest,
        LocalResult::None => now + Duration::hours(1),
    };
    (target_moment - now)
        .to_std()
        .unwrap_or(std::time::Duration::from_secs(60))
}

// 5. 예약 리포트 생성 ---------------------------------------------------------
async fn generate_scheduled_report(ctx: &Arc<AppContext>, report_date: NaiveDate) {
    let worker_ctx = Arc::clone(ctx);
    let joined = tokio::task::spawn_blocking(move || {
        write_daily_report(&worker_ctx.config, &worker_ctx.store, report_date)
    })
    .await;
    let failure = match joined {
        Ok(Ok(_)) => return,
        Ok(Err(err)) => format!("{err:#}"),
        Err(err) => format!("리포트 작업 중단: {err}"),
    };
    eprintln!("[rap] 일일 리포트 실패 ({report_date}): {failure}");
    let _ = ctx.live_tx.send(LiveEvent::Notice {
        ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        level: "error".to_string(),
        text: format!("일일 리포트 실패 ({report_date}): {failure}"),
    });
}

// 6. 첫 색인 대기 -------------------------------------------------------------
// 색인 1회 완료 시 true, 제한 시간 초과 시 false
async fn wait_for_first_index(
    ctx: &Arc<AppContext>,
    poll_interval: std::time::Duration,
    wait_limit: std::time::Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + wait_limit;
    while ctx.counters.index_runs.load(Ordering::Relaxed) < 1 {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(poll_interval).await;
    }
    true
}

// 7. 자정 예약 루프 -----------------------------------------------------------
pub async fn run_report_scheduler(ctx: Arc<AppContext>) -> anyhow::Result<()> {
    // 1. 첫 색인 대기
    if ctx.config.indexer.enabled {
        let indexed = wait_for_first_index(
            &ctx,
            std::time::Duration::from_secs(FIRST_INDEX_POLL_SECS),
            std::time::Duration::from_secs(FIRST_INDEX_WAIT_SECS),
        )
        .await;
        if !indexed {
            eprintln!("[rap] 첫 색인 대기 초과: 어제 리포트를 현재 색인 상태로 생성");
            let _ = ctx.live_tx.send(LiveEvent::Notice {
                ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                level: "warn".to_string(),
                text: "첫 색인 대기 초과: 어제 리포트가 불완전할 수 있음".to_string(),
            });
        }
    }

    // 2. 어제 리포트 재생성 (기존 리포트 덮어씀)
    let yesterday = (Local::now() - Duration::days(1)).date_naive();
    generate_scheduled_report(&ctx, yesterday).await;

    // 3. 매일 예약 시각에 전일 리포트
    loop {
        let delay = compute_next_run_delay(Local::now(), ctx.config.report.run_hour);
        tokio::time::sleep(delay + std::time::Duration::from_secs(1)).await;
        let report_date = (Local::now() - Duration::days(1)).date_naive();
        generate_scheduled_report(&ctx, report_date).await;
    }
}
