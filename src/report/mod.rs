//! report/mod.rs
//! 일일·주간 에이전트 사용 종합 리포트 (Markdown) 생성·저장·예약.

mod render;
pub mod table;
#[cfg(test)]
mod tests;
mod text;
mod weekly;

use crate::core::config::{AppConfig, ReportLanguage};
use crate::core::context::AppContext;
use crate::core::model::LiveEvent;
use crate::store::Store;
use anyhow::Context;
use chrono::{
    DateTime, Datelike, Duration, Local, LocalResult, NaiveDate, NaiveTime, SecondsFormat,
    TimeZone, Utc, Weekday,
};
pub use render::format_count;
use render::{ReportInput, render_report_markdown};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
pub use weekly::{build_weekly_report, format_week_key, parse_week_key, week_start_of};

const TOOL_LATENCY_LIMIT: usize = 10;
const CACHE_BREAK_LIMIT: usize = 10;
const SUBAGENT_RUN_LIMIT: usize = 1000;
const SCHEDULE_MINUTE: u32 = 5;
const FIRST_INDEX_POLL_SECS: u64 = 2;
const FIRST_INDEX_WAIT_SECS: u64 = 10 * 60;

// 1. 예약 리포트 대상 ---------------------------------------------------------
// WeeklyIfStale 은 보관본이 없거나 그 주가 끝나기 전에 만든 부분 본(TUI g 키 등)일 때만 생성
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScheduledReport {
    Daily(NaiveDate),
    Weekly(NaiveDate),
    WeeklyIfStale(NaiveDate),
}

// 2. 로컬 자정의 UTC 변환 -----------------------------------------------------
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

// 3. 리포트 본문 생성 ---------------------------------------------------------
pub fn build_daily_report(
    store: &Store,
    report_date: NaiveDate,
    language: ReportLanguage,
) -> anyhow::Result<String> {
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
        weekly: false,
        today: store.query_overview(&start, &end)?,
        previous: store.query_overview(&previous_start, &start)?,
        usage: store.query_model_usage(&start, &end)?,
        tools: store.query_tool_latency(&start, &end, TOOL_LATENCY_LIMIT)?,
        hooks: store.query_hook_stats(&start, &end)?,
        breaks: store.query_cache_breaks(&start, &end, CACHE_BREAK_LIMIT)?,
        gateway: store.query_gateway_timing(&start, &end)?,
        sources: store.query_source_counts(&start, &end)?,
        // otel_events 표가 없는 구 DB 는 빈 구간으로 취급 (리포트 전체를 실패시키지 않음)
        compaction: store.compaction_stats(&start, &end).unwrap_or_default(),
        subagents: store
            .subagent_runs(&start, &end, SUBAGENT_RUN_LIMIT)
            .unwrap_or_default(),
    };
    Ok(render_report_markdown(&input, language))
}

// 4. 리포트 보관 (DB + 파일) --------------------------------------------------
// key 는 일일 YYYY-MM-DD, 주간 YYYY-Www (같은 표·같은 파일 이름 규칙)
fn store_report(
    config: &AppConfig,
    store: &Store,
    key: &str,
    markdown: &str,
) -> anyhow::Result<PathBuf> {
    // 1. DB 보관
    let generated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    store.save_daily_report(key, &generated_at, markdown)?;

    // 2. 파일 기록
    let dir = config.report_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("리포트 폴더 생성 실패: {}", dir.display()))?;
    let path = dir.join(format!("{key}.md"));
    fs::write(&path, markdown.as_bytes())
        .with_context(|| format!("리포트 파일 기록 실패: {}", path.display()))?;
    Ok(path)
}

// 5. 일일 리포트 저장 ---------------------------------------------------------
pub fn write_daily_report(
    config: &AppConfig,
    store: &Store,
    report_date: NaiveDate,
) -> anyhow::Result<PathBuf> {
    let markdown = build_daily_report(store, report_date, config.report.language)?;
    let date_text = report_date.format("%Y-%m-%d").to_string();
    store_report(config, store, &date_text, &markdown)
}

// 6. 주간 리포트 저장 ---------------------------------------------------------
// week_start 가 속한 ISO 주 (월~일) 의 리포트
pub fn write_weekly_report(
    config: &AppConfig,
    store: &Store,
    week_start: NaiveDate,
) -> anyhow::Result<PathBuf> {
    let monday = week_start_of(week_start);
    let markdown = build_weekly_report(store, monday, config.report.language)?;
    store_report(config, store, &format_week_key(monday), &markdown)
}

// 7. 다음 실행까지 대기 시간 --------------------------------------------------
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

// 8. 주간 리포트 예약 판정 ----------------------------------------------------
// 매 실행 지난주 (월요일 날짜) 대상: 월요일 예약 실행만 덮어쓰고,
// 그 외 요일·시작 시는 보관본이 없거나 그 주가 끝나기 전 부분 본일 때만 생성 (월요일 내내 PC 가 잠든 경우 보완)
fn weekly_report_target(run_date: NaiveDate, on_start: bool) -> ScheduledReport {
    let last_week = week_start_of(run_date) - Duration::days(7);
    if !on_start && run_date.weekday() == Weekday::Mon {
        return ScheduledReport::Weekly(last_week);
    }
    ScheduledReport::WeeklyIfStale(last_week)
}

// 9. 예약 리포트 생성 ---------------------------------------------------------
async fn generate_scheduled_report(ctx: &Arc<AppContext>, target: ScheduledReport) {
    let (kind_label, key) = match target {
        ScheduledReport::Daily(date) => ("일일", date.format("%Y-%m-%d").to_string()),
        ScheduledReport::Weekly(week_start) | ScheduledReport::WeeklyIfStale(week_start) => {
            ("주간", format_week_key(week_start))
        }
    };
    let worker_ctx = Arc::clone(ctx);
    let joined = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        match target {
            ScheduledReport::Daily(date) => {
                write_daily_report(&worker_ctx.config, &worker_ctx.store, date)?;
            }
            ScheduledReport::Weekly(week_start) => {
                write_weekly_report(&worker_ctx.config, &worker_ctx.store, week_start)?;
            }
            ScheduledReport::WeeklyIfStale(week_start) => {
                // 생성 시각이 그 주 끝(다음 주 월요일 로컬 00:00) 이전이면 부분 본, 해석할 수 없는 시각도 다시 만듦
                let generated_at = worker_ctx
                    .store
                    .daily_report_generated_at(&format_week_key(week_start))?;
                let week_end = local_midnight_utc(week_start + Duration::days(7));
                let complete = generated_at
                    .and_then(|stamp| DateTime::parse_from_rfc3339(&stamp).ok())
                    .is_some_and(|stamp| stamp.with_timezone(&Utc) >= week_end);
                if !complete {
                    write_weekly_report(&worker_ctx.config, &worker_ctx.store, week_start)?;
                }
            }
        }
        Ok(())
    })
    .await;
    let failure = match joined {
        Ok(Ok(())) => return,
        Ok(Err(err)) => format!("{err:#}"),
        Err(err) => format!("리포트 작업 중단: {err}"),
    };
    eprintln!("[cm] {kind_label} 리포트 실패 ({key}): {failure}");
    let _ = ctx.live_tx.send(LiveEvent::Notice {
        ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        level: "error".to_string(),
        text: format!("{kind_label} 리포트 실패 ({key}): {failure}"),
    });
}

// 10. 첫 색인 대기 ------------------------------------------------------------
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

// 11. 자정 예약 루프 ----------------------------------------------------------
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
            eprintln!("[cm] 첫 색인 대기 초과: 어제 리포트를 현재 색인 상태로 생성");
            let _ = ctx.live_tx.send(LiveEvent::Notice {
                ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                level: "warn".to_string(),
                text: "첫 색인 대기 초과: 어제 리포트가 불완전할 수 있음".to_string(),
            });
        }
    }

    // 2. 어제 리포트 재생성 (기존 리포트 덮어씀)
    let yesterday = (Local::now() - Duration::days(1)).date_naive();
    generate_scheduled_report(&ctx, ScheduledReport::Daily(yesterday)).await;

    // 3. 지난주 리포트 (보관본이 없을 때만)
    generate_scheduled_report(&ctx, weekly_report_target(Local::now().date_naive(), true)).await;

    // 4. 매일 예약 시각에 전일 리포트, 지난주 리포트는 월요일 덮어쓰기·그 외 보관본 없을 때 보완
    loop {
        let delay = compute_next_run_delay(Local::now(), ctx.config.report.run_hour);
        tokio::time::sleep(delay + std::time::Duration::from_secs(1)).await;
        let run_date = Local::now().date_naive();
        let report_date = (Local::now() - Duration::days(1)).date_naive();
        generate_scheduled_report(&ctx, ScheduledReport::Daily(report_date)).await;
        generate_scheduled_report(&ctx, weekly_report_target(run_date, false)).await;
    }
}
