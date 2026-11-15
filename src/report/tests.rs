//! report/tests.rs
//! 색인 -> 집계 -> 리포트 흐름과 예약 시각 계산 시험.

use super::{
    build_daily_report, compute_next_run_delay, run_report_scheduler, wait_for_first_index,
    write_daily_report,
};
use crate::net::admin::test_support::{build_test_config, build_test_context};
use crate::transcript::index_transcripts_once;
use crate::transcript::test_support::prepare_fixture_workspace;
use chrono::{Duration, Local, NaiveDate, TimeZone};
use std::fs;
use std::sync::atomic::Ordering;
use unicode_width::UnicodeWidthStr;

// 1. 리포트 본문 --------------------------------------------------------------
#[test]
fn report_covers_indexed_fixture_day() {
    let workspace = prepare_fixture_workspace("report");
    index_transcripts_once(&workspace.config, &workspace.store).expect("색인");
    let report_date = NaiveDate::from_ymd_opt(2026, 11, 10).expect("날짜");

    // 1. 본문 구성
    let markdown = build_daily_report(&workspace.store, report_date).expect("리포트");
    for expected in [
        "## 1. 요약",
        "- API 요청: 7건",
        "## 2. 전일 대비",
        "전일 기록 없음",
        "## 3. 모델·에이전트별 (비용은 추정)",
        "claude-opus-5-5",
        "## 4. 도구 지연 상위 10",
        "## 5. 훅 판정",
        "cli-guard",
        "## 6. 캐시 붕괴 상위",
        "유휴 5분 초과 (TTL)",
        "## 7. 자동 제안",
        "OTLP 수집 0건",
    ] {
        assert!(markdown.contains(expected), "누락: {expected}");
    }
    assert!(!markdown.contains("게이트웨이"));

    // 2. 박스 표 형식 (파이프 표 없음, 표마다 text 펜스, 줄 폭 일치)
    assert!(!markdown.contains("| ---"), "파이프 표 잔존");
    assert!(!markdown.contains("\n| "), "파이프 행 잔존");
    assert!(markdown.contains("```text\n┌"));
    assert!(
        markdown
            .lines()
            .any(|line| line.starts_with('│') && line.contains("모델") && line.contains("요청"))
    );
    assert!(markdown.contains("└"));
    let mut table_count = 0;
    let mut expected_width: Option<usize> = None;
    for line in markdown.lines() {
        if !line.starts_with(['┌', '├', '│', '└']) {
            expected_width = None;
            continue;
        }
        match expected_width {
            None => {
                table_count += 1;
                expected_width = Some(line.width());
            }
            Some(width) => assert_eq!(line.width(), width, "폭 불일치: {line}"),
        }
    }
    assert!(table_count >= 4, "표 {table_count}개");

    // 3. 저장 (DB + 파일)
    let path = write_daily_report(&workspace.config, &workspace.store, report_date).expect("저장");
    assert!(path.ends_with("2026-11-10.md"));
    assert_eq!(fs::read_to_string(&path).expect("파일"), markdown);
    let saved = workspace
        .store
        .load_daily_report("2026-11-10")
        .expect("조회");
    assert_eq!(saved.as_deref(), Some(markdown.as_str()));
    fs::remove_dir_all(&workspace.root).ok();
}

// 1-1. 전일 대비 표 -----------------------------------------------------------
#[test]
fn report_renders_previous_day_comparison_as_box_table() {
    let workspace = prepare_fixture_workspace("report-compare");
    index_transcripts_once(&workspace.config, &workspace.store).expect("색인");

    // 1. 전일 기록이 있는 날 (다음 날 리포트의 전일 = 시드 일자)
    let report_date = NaiveDate::from_ymd_opt(2026, 11, 11).expect("날짜");
    let markdown = build_daily_report(&workspace.store, report_date).expect("리포트");
    assert!(
        markdown.contains("## 2. 전일 대비\n\n```text\n┌"),
        "{markdown}"
    );
    assert!(
        markdown
            .lines()
            .any(|line| line.starts_with('│') && line.contains("항목") && line.contains("증감"))
    );
    assert!(markdown.contains("│ API 요청 "));
    assert!(!markdown.contains("전일 기록 없음"));
    fs::remove_dir_all(&workspace.root).ok();
}

// 2. 첫 색인 대기 -------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_index_wait_times_out_then_sees_index_run() {
    let ctx = build_test_context(build_test_config("report-wait"));
    let poll_interval = std::time::Duration::from_millis(5);

    // 1. 색인 전 제한 시간 초과
    let timed_out =
        wait_for_first_index(&ctx, poll_interval, std::time::Duration::from_millis(40)).await;
    assert!(!timed_out);

    // 2. 색인 1회 후 즉시 통과
    ctx.counters.index_runs.fetch_add(1, Ordering::Relaxed);
    let indexed =
        wait_for_first_index(&ctx, poll_interval, std::time::Duration::from_secs(5)).await;
    assert!(indexed);
    fs::remove_dir_all(&ctx.config.data_dir).ok();
}

// 3. 시작 시 어제 리포트 덮어쓰기 ---------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_overwrites_existing_yesterday_report() {
    // 1. 보관된 낡은 리포트 (색인 대기 없음)
    let mut config = build_test_config("report-overwrite");
    config.indexer.enabled = false;
    let ctx = build_test_context(config);
    let yesterday = (Local::now() - Duration::days(1)).date_naive();
    let yesterday_text = yesterday.format("%Y-%m-%d").to_string();
    ctx.store
        .save_daily_report(&yesterday_text, "2000-01-01T00:00:00.000Z", "STALE REPORT")
        .expect("낡은 리포트 저장");

    // 2. 스케줄러 기동 후 재생성 대기
    let scheduler = tokio::spawn(run_report_scheduler(ctx.clone()));
    let mut regenerated: Option<String> = None;
    for _ in 0..100 {
        let saved = ctx.store.load_daily_report(&yesterday_text).expect("조회");
        if saved.as_deref() != Some("STALE REPORT") {
            regenerated = saved;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    scheduler.abort();

    // 3. 덮어쓴 본문 확인
    let markdown = regenerated.expect("어제 리포트 재생성");
    assert!(markdown.contains("## 1. 요약"));
    fs::remove_dir_all(&ctx.config.data_dir).ok();
}

// 4. 예약 대기 시간 -----------------------------------------------------------
#[test]
fn next_run_delay_targets_run_hour_plus_five_minutes() {
    let before = Local
        .with_ymd_and_hms(2026, 11, 10, 0, 2, 0)
        .single()
        .expect("시각");
    assert_eq!(compute_next_run_delay(before, 0).as_secs(), 3 * 60);
    let after = Local
        .with_ymd_and_hms(2026, 11, 10, 0, 10, 0)
        .single()
        .expect("시각");
    assert_eq!(
        compute_next_run_delay(after, 0).as_secs(),
        23 * 3600 + 55 * 60
    );
}
