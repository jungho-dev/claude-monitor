//! report/tests.rs
//! 색인 -> 집계 -> 리포트 흐름, 주간 리포트·언어·예약 시각·관리 라우트 시험.

use super::render::{ReportInput, format_count, format_count_delta, render_report_markdown};
use super::weekly::{DailyRow, WeeklyInput, render_weekly_markdown};
use super::{
    ScheduledReport, build_daily_report, build_weekly_report, compute_next_run_delay,
    format_week_key, generate_scheduled_report, local_midnight_utc, parse_week_key,
    run_report_scheduler, wait_for_first_index, week_start_of, weekly_report_target,
    write_daily_report, write_weekly_report,
};
use crate::core::config::ReportLanguage;
use crate::core::model::{IngestBatch, OtelEventRow};
use crate::net::admin::run_admin_server;
use crate::net::admin::test_support::{
    TEST_ADMIN_TOKEN, build_test_config, build_test_context, send_raw, wait_until_listening,
};
use crate::store::query::{AgentStatsRow, ModelStatsRow, ModelUsageRow, OverviewStats};
use crate::transcript::index_transcripts_once;
use crate::transcript::test_support::{TempDirGuard, prepare_fixture_workspace};
use chrono::{Duration, Local, NaiveDate, SecondsFormat, TimeZone};
use std::fs;
use std::sync::atomic::Ordering;
use unicode_width::UnicodeWidthStr;

// 1. 리포트 본문 --------------------------------------------------------------
#[test]
fn report_covers_indexed_fixture_day() {
    let mut workspace = prepare_fixture_workspace("report");
    workspace.config.report.language = ReportLanguage::Ko;
    index_transcripts_once(&workspace.config, &workspace.store).expect("색인");
    let report_date = NaiveDate::from_ymd_opt(2026, 11, 10).expect("날짜");

    // 1. 본문 구성
    let markdown =
        build_daily_report(&workspace.store, report_date, ReportLanguage::Ko).expect("리포트");
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
}

// 1-1. 전일 대비 표 -----------------------------------------------------------
#[test]
fn report_renders_previous_day_comparison_as_box_table() {
    let workspace = prepare_fixture_workspace("report-compare");
    index_transcripts_once(&workspace.config, &workspace.store).expect("색인");

    // 1. 전일 기록이 있는 날 (다음 날 리포트의 전일 = 시드 일자)
    let report_date = NaiveDate::from_ymd_opt(2026, 11, 11).expect("날짜");
    let markdown =
        build_daily_report(&workspace.store, report_date, ReportLanguage::Ko).expect("리포트");
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
}

// 2. 첫 색인 대기 -------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_index_wait_times_out_then_sees_index_run() {
    let config = build_test_config("report-wait");
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
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
}

// 3. 시작 시 어제 리포트 덮어쓰기 ---------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_overwrites_existing_yesterday_report() {
    // 1. 보관된 낡은 리포트 (색인 대기 없음)
    let mut config = build_test_config("report-overwrite");
    config.indexer.enabled = false;
    let _cleanup = TempDirGuard(config.data_dir.clone());
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
    let _ = scheduler.await;

    // 3. 덮어쓴 본문 확인
    let markdown = regenerated.expect("어제 리포트 재생성");
    assert!(markdown.contains("## 1. Summary"));
}

// 4. 건수 천 단위 구분 --------------------------------------------------------
#[test]
fn report_counts_use_thousands_separators_and_keep_tables_aligned() {
    let input = ReportInput {
        report_date: NaiveDate::from_ymd_opt(2026, 11, 10).expect("날짜"),
        today: OverviewStats {
            api_requests: 1_234_567,
            sessions: 4_321,
            tool_calls: 1_200_000,
            output_tokens: 9_876_543,
            ..OverviewStats::default()
        },
        previous: OverviewStats {
            api_requests: 1_000_000,
            tool_calls: 900_000,
            output_tokens: 1_000_000,
            ..OverviewStats::default()
        },
        usage: vec![ModelUsageRow {
            model: "claude-opus-5-5".to_string(),
            agent: "main".to_string(),
            requests: 2_345_678,
            ..ModelUsageRow::default()
        }],
        ..ReportInput::default()
    };
    let markdown = render_report_markdown(&input, ReportLanguage::Ko);
    for expected in [
        "- API 요청: 1,234,567건 (세션 4,321개)",
        "- 도구 호출: 1,200,000건",
        "+234,567 (+23.5%)",
        "│ 2,345,678 │",
    ] {
        assert!(markdown.contains(expected), "누락: {expected}\n{markdown}");
    }
    assert!(!markdown.contains("1234567"));
    assert_eq!(format_count(-12_345), "-12,345");
    assert_eq!(format_count_delta(900, 1_000), "-100 (-10.0%)");

    // 표마다 줄 폭 일치
    let mut expected_width: Option<usize> = None;
    for line in markdown.lines() {
        if !line.starts_with(['┌', '├', '│', '└']) {
            expected_width = None;
            continue;
        }
        match expected_width {
            None => expected_width = Some(line.width()),
            Some(width) => assert_eq!(line.width(), width, "폭 불일치: {line}"),
        }
    }
}

// 5. 예약 대기 시간 -----------------------------------------------------------
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

// 6. 표 폭 검사 ---------------------------------------------------------------
// 표마다 줄 폭 일치를 확인하고 표 개수 반환
fn count_aligned_tables(markdown: &str) -> usize {
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
    table_count
}

// 7. 영어 일일 리포트 ---------------------------------------------------------
#[test]
fn english_daily_report_uses_english_text_and_keeps_tables_aligned() {
    let workspace = prepare_fixture_workspace("report-en");
    index_transcripts_once(&workspace.config, &workspace.store).expect("색인");
    let report_date = NaiveDate::from_ymd_opt(2026, 11, 10).expect("날짜");

    // 1. 핵심 줄 (숫자·모델 이름은 그대로)
    let markdown =
        build_daily_report(&workspace.store, report_date, ReportLanguage::En).expect("리포트");
    for expected in [
        "# Daily Agent Report 2026-11-10",
        "## 1. Summary",
        "- API requests: 7 (sessions ",
        "- Cache hit ratio: ",
        "## 2. Versus Previous Day",
        "- No previous-day records",
        "## 3. By Model and Agent (cost is an estimate)",
        "claude-opus-5-5",
        "## 4. Top 10 Tools by Latency",
        "## 5. Hook Decisions",
        "cli-guard",
        "## 6. Top Cache Breaks",
        "## 7. Suggestions",
        "OTLP records: 0",
    ] {
        assert!(markdown.contains(expected), "누락: {expected}\n{markdown}");
    }
    for korean in [
        "## 1. 요약",
        "일일 에이전트 리포트",
        "기록 없음",
        "자동 제안",
    ] {
        assert!(!markdown.contains(korean), "한국어 잔존: {korean}");
    }
    assert!(markdown.lines().any(|line| {
        line.starts_with('│') && line.contains("Model") && line.contains("Requests")
    }));

    // 2. 표 폭과 저장 (설정 언어 따름)
    assert!(count_aligned_tables(&markdown) >= 4);
    let mut english_workspace = workspace;
    english_workspace.config.report.language = ReportLanguage::En;
    write_daily_report(
        &english_workspace.config,
        &english_workspace.store,
        report_date,
    )
    .expect("저장");
    let saved = english_workspace
        .store
        .load_daily_report("2026-11-10")
        .expect("조회");
    assert_eq!(saved.as_deref(), Some(markdown.as_str()));
}

// 7-1. 영어 시간 표기 ---------------------------------------------------------
#[test]
fn english_report_formats_durations_with_english_units() {
    let input = ReportInput {
        report_date: NaiveDate::from_ymd_opt(2026, 11, 10).expect("날짜"),
        today: OverviewStats {
            api_requests: 3,
            model_wait_p50_ms: Some(1_500),
            model_wait_p90_ms: Some(192_000),
            saved_ms: 850,
            ..OverviewStats::default()
        },
        ..ReportInput::default()
    };
    let english = render_report_markdown(&input, ReportLanguage::En);
    assert!(
        english.contains("- Model wait: p50 1.5s / p90 3m 12s"),
        "{english}"
    );
    assert!(english.contains("- Time saved: 850ms "), "{english}");
    let korean = render_report_markdown(&input, ReportLanguage::Ko);
    assert!(
        korean.contains("- 모델 대기: p50 1.5초 / p90 3분 12초"),
        "{korean}"
    );
}

// 8. 주 경계·주 키 ------------------------------------------------------------
#[test]
fn week_keys_follow_iso_weeks_across_year_boundaries() {
    let day = |year: i32, month: u32, date: u32| {
        NaiveDate::from_ymd_opt(year, month, date).expect("날짜")
    };

    // 1. 연말·연초 (2026 은 1월 1일이 목요일이라 W53 보유, 2025-12-29 가 2026-W01 시작)
    assert_eq!(week_start_of(day(2026, 1, 1)), day(2025, 12, 29));
    assert_eq!(format_week_key(day(2025, 12, 29)), "2026-W01");
    assert_eq!(week_start_of(day(2025, 12, 28)), day(2025, 12, 22));
    assert_eq!(format_week_key(day(2025, 12, 22)), "2025-W52");
    assert_eq!(week_start_of(day(2021, 1, 3)), day(2020, 12, 28));
    assert_eq!(format_week_key(day(2020, 12, 28)), "2020-W53");
    assert_eq!(week_start_of(day(2026, 12, 31)), day(2026, 12, 28));
    assert_eq!(format_week_key(day(2026, 12, 28)), "2026-W53");

    // 2. 월요일·일요일 경계 (월~일)
    assert_eq!(week_start_of(day(2026, 11, 9)), day(2026, 11, 9));
    assert_eq!(week_start_of(day(2026, 11, 15)), day(2026, 11, 9));
    assert_eq!(week_start_of(day(2026, 11, 16)), day(2026, 11, 16));

    // 3. 키 해석 (왜복 일치, 없는 주·잘못된 형식 거부)
    for monday in [day(2026, 1, 5), day(2025, 12, 29), day(2020, 12, 28)] {
        assert_eq!(parse_week_key(&format_week_key(monday)), Some(monday));
    }
    assert_eq!(parse_week_key("2026-W53"), Some(day(2026, 12, 28)));
    assert_eq!(parse_week_key("2025-W53"), None);
    assert_eq!(parse_week_key("2021-W53"), None);
    for malformed in [
        "",
        "2026-W1",
        "26-W01",
        "2026W01",
        "2026-W00",
        "2026-W54",
        "+026-W01",
        "2026-w01",
        "2026-W01x",
    ] {
        assert_eq!(parse_week_key(malformed), None, "{malformed}");
    }
}

// 9. 주간 리포트 본문 ---------------------------------------------------------
#[test]
fn weekly_report_covers_week_with_daily_rows_in_both_languages() {
    let workspace = prepare_fixture_workspace("report-week");
    index_transcripts_once(&workspace.config, &workspace.store).expect("색인");
    let week_start = NaiveDate::from_ymd_opt(2026, 11, 9).expect("날짜");

    // 1. 영어 (지난주 기록 없음, 시드 2026-11-10 화요일 포함)
    let english =
        build_weekly_report(&workspace.store, week_start, ReportLanguage::En).expect("주간 리포트");
    for expected in [
        "# Weekly Agent Report 2026-W46 (2026-11-09 ~ 2026-11-15)",
        "## 1. This Week Summary",
        "## 2. Versus Last Week",
        "- No last-week records",
        "## 3. Daily Breakdown",
        "## 4. By Model (cost is an estimate)",
        "claude-opus-5-5",
        "## 5. By Agent (cost is an estimate)",
        "## 6. Top 10 Tools by Latency",
        "## 7. Hook Decisions",
        "## 8. Suggestions",
    ] {
        assert!(english.contains(expected), "누락: {expected}\n{english}");
    }
    assert!(english.lines().any(|line| {
        line.starts_with('│') && line.contains("2026-11-10") && line.contains(" Tue ")
    }));
    assert!(count_aligned_tables(&english) >= 4);

    // 2. 한국어 (기본, 일별 행의 요일 한글)
    let korean =
        build_weekly_report(&workspace.store, week_start, ReportLanguage::Ko).expect("주간 리포트");
    for expected in [
        "# 주간 에이전트 리포트 2026-W46 (2026-11-09 ~ 2026-11-15)",
        "## 1. 이번 주 요약",
        "- 지난주 기록 없음",
        "## 3. 일별 추이",
        "## 4. 모델별 (비용은 추정)",
        "## 5. 에이전트별 (비용은 추정)",
    ] {
        assert!(korean.contains(expected), "누락: {expected}\n{korean}");
    }
    assert!(korean.lines().any(|line| {
        line.starts_with('│') && line.contains("2026-11-10") && line.contains(" 화 ")
    }));
    assert!(count_aligned_tables(&korean) >= 4);

    // 3. 다음 주 (지난주 기록이 있으면 대비 표)
    let next_week = NaiveDate::from_ymd_opt(2026, 11, 16).expect("날짜");
    let compared =
        build_weekly_report(&workspace.store, next_week, ReportLanguage::En).expect("주간 리포트");
    assert!(
        compared.contains("## 2. Versus Last Week\n\n```text\n┌"),
        "{compared}"
    );
    assert!(compared.contains("Est. cost USD"));
    assert!(!compared.contains("- No last-week records"));
}

// 9-1. 진행 중인 주 실시간 빌드 -----------------------------------------------
#[test]
fn current_week_live_build_is_not_stored_and_accepts_any_day_of_the_week() {
    let workspace = prepare_fixture_workspace("report-week-live");
    let today = Local::now().date_naive();
    let report = build_weekly_report(&workspace.store, today, ReportLanguage::Ko).expect("빌드");
    let key = format_week_key(week_start_of(today));
    assert!(
        report.contains(&format!("# 주간 에이전트 리포트 {key} (")),
        "{report}"
    );
    assert!(report.contains("- 기록 없음"));
    assert!(report.contains("해당 주 기록 0건"));
    assert_eq!(workspace.store.load_daily_report(&key).expect("조회"), None);
}

// 9-2. 한글 모델 이름 표 정렬 -------------------------------------------------
#[test]
fn weekly_tables_stay_aligned_with_hangul_names_and_show_cost_change() {
    let monday = NaiveDate::from_ymd_opt(2026, 11, 9).expect("날짜");
    let mut input = WeeklyInput {
        week_start: monday,
        ..WeeklyInput::default()
    };
    input.base.today = OverviewStats {
        api_requests: 5,
        output_tokens: 2_000,
        ..OverviewStats::default()
    };
    input.base.previous = OverviewStats {
        api_requests: 4,
        output_tokens: 1_000,
        ..OverviewStats::default()
    };
    input.days = vec![DailyRow {
        date: monday,
        overview: input.base.today.clone(),
        cost_usd: 1.5,
    }];
    input.models = vec![ModelStatsRow {
        model: "한글 모델".to_string(),
        requests: 3,
        cost_usd_estimate: 1.5,
        ..ModelStatsRow::default()
    }];
    input.models_previous = vec![ModelStatsRow {
        model: "한글 모델".to_string(),
        cost_usd_estimate: 1.0,
        ..ModelStatsRow::default()
    }];
    input.agents = vec![AgentStatsRow {
        agent: "일꾼".to_string(),
        requests: 2,
        cost_usd_estimate: 0.25,
        ..AgentStatsRow::default()
    }];
    for language in [ReportLanguage::Ko, ReportLanguage::En] {
        let markdown = render_weekly_markdown(&input, language);
        assert!(markdown.contains("│ 한글 모델 "), "{markdown}");
        assert!(markdown.contains("+0.50 │"), "{markdown}");
        assert!(markdown.contains("│ 일꾼 "));
        assert!(count_aligned_tables(&markdown) >= 4, "{markdown}");
    }
}

// 10. 주간 저장 ---------------------------------------------------------------
#[test]
fn weekly_report_is_stored_under_week_key_in_table_and_file() {
    let workspace = prepare_fixture_workspace("report-week-store");
    index_transcripts_once(&workspace.config, &workspace.store).expect("색인");
    let wednesday = NaiveDate::from_ymd_opt(2026, 11, 11).expect("날짜");

    // 1. 주 중 아무 날짜나 해당 주 월요일로 정규화
    let path = write_weekly_report(&workspace.config, &workspace.store, wednesday).expect("저장");
    assert!(path.ends_with("2026-W46.md"), "{}", path.display());
    let saved = workspace
        .store
        .load_daily_report("2026-W46")
        .expect("조회")
        .expect("보관본");
    assert_eq!(fs::read_to_string(&path).expect("파일"), saved);
    assert!(saved.starts_with("# Weekly Agent Report 2026-W46 (2026-11-09 ~ 2026-11-15)"));

    // 2. 일일 보관본과 키 충돌 없음
    assert_eq!(
        workspace
            .store
            .load_daily_report("2026-11-11")
            .expect("조회"),
        None
    );
}

// 11. 주간 예약 판정 ----------------------------------------------------------
#[test]
fn weekly_report_target_overwrites_on_monday_and_fills_missing_otherwise() {
    let day = |year: i32, month: u32, date: u32| {
        NaiveDate::from_ymd_opt(year, month, date).expect("날짜")
    };

    // 1. 월요일 예약 실행은 지난주 덮어쓰기
    assert_eq!(
        weekly_report_target(day(2026, 11, 16), false),
        ScheduledReport::Weekly(day(2026, 11, 9))
    );
    assert_eq!(
        weekly_report_target(day(2026, 1, 5), false),
        ScheduledReport::Weekly(day(2025, 12, 29))
    );

    // 2. 그 외 요일 (화요일 포함) 실행은 지난주 보관본이 없을 때만
    for date in 17..=22 {
        assert_eq!(
            weekly_report_target(day(2026, 11, date), false),
            ScheduledReport::WeeklyIfStale(day(2026, 11, 9)),
            "{date}"
        );
    }
    assert_eq!(
        weekly_report_target(day(2026, 11, 15), false),
        ScheduledReport::WeeklyIfStale(day(2026, 11, 2))
    );

    // 3. 시작 시에는 요일과 무관하게 보관본이 없을 때만
    for date in [16, 19, 22] {
        assert_eq!(
            weekly_report_target(day(2026, 11, date), true),
            ScheduledReport::WeeklyIfStale(day(2026, 11, 9)),
            "{date}"
        );
    }
}

// 11-0. 화요일 실행이 놓친 지난주 리포트 보완 ---------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tuesday_run_writes_missing_last_week_report_but_keeps_stored_copy() {
    let mut config = build_test_config("report-week-tuesday");
    config.indexer.enabled = false;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let tuesday = NaiveDate::from_ymd_opt(2026, 11, 17).expect("날짜");
    let last_week = NaiveDate::from_ymd_opt(2026, 11, 9).expect("날짜");
    let key = format_week_key(last_week);

    // 1. 월요일 실행을 놓쳐 보관본이 없음: 화요일 실행이 생성
    assert!(ctx.store.load_daily_report(&key).expect("조회").is_none());
    generate_scheduled_report(&ctx, weekly_report_target(tuesday, false)).await;
    let created = ctx.store.load_daily_report(&key).expect("조회");
    assert!(created.is_some_and(|text| text.contains("## 1. This Week Summary")));

    // 2. 이미 있으면 다시 실행해도 덮어쓰지 않음
    ctx.store
        .save_daily_report(&key, "2100-01-01T00:00:00.000Z", "KEEP WEEK")
        .expect("보관본 저장");
    generate_scheduled_report(&ctx, weekly_report_target(tuesday, false)).await;
    assert_eq!(
        ctx.store.load_daily_report(&key).expect("조회").as_deref(),
        Some("KEEP WEEK")
    );
}

// 11-0-1. 주가 끝나기 전에 만든 부분 보관본 (주간 보기 g 키) 보완 -------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_weekly_copy_saved_before_the_week_ended_is_regenerated_on_a_non_monday_run() {
    let mut config = build_test_config("report-week-partial");
    config.indexer.enabled = false;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let tuesday = NaiveDate::from_ymd_opt(2026, 11, 17).expect("날짜");
    let week_start = NaiveDate::from_ymd_opt(2026, 11, 9).expect("날짜");
    let key = format_week_key(week_start);
    let week_end = local_midnight_utc(week_start + Duration::days(7));
    let stored_at = || {
        let text = ctx
            .store
            .daily_report_generated_at(&key)
            .expect("조회")
            .expect("보관본");
        chrono::DateTime::parse_from_rfc3339(&text).expect("RFC 3339")
    };

    // 1. 주가 끝나기 1분 전에 저장된 부분 본: 월요일이 아닌 실행이 다시 만듦
    let partial_at = (week_end - Duration::minutes(1)).to_rfc3339_opts(SecondsFormat::Millis, true);
    ctx.store
        .save_daily_report(&key, &partial_at, "PARTIAL WEEK")
        .expect("부분 본 저장");
    generate_scheduled_report(&ctx, weekly_report_target(tuesday, false)).await;
    let regenerated = ctx.store.load_daily_report(&key).expect("조회");
    assert!(regenerated.is_some_and(|text| text.contains("## 1. This Week Summary")));
    let generated_at = stored_at();
    assert!(generated_at >= week_end, "{generated_at}");

    // 2. 주가 끝난 뒤 만든 보관본은 다시 실행해도 그대로 (생성 시각 불변)
    generate_scheduled_report(&ctx, weekly_report_target(tuesday, true)).await;
    assert_eq!(stored_at(), generated_at);

    // 3. 주 끝에 정확히 맞춘 시각은 완성 본, 해석할 수 없는 시각은 다시 만듦
    let exact_end = week_end.to_rfc3339_opts(SecondsFormat::Millis, true);
    ctx.store
        .save_daily_report(&key, &exact_end, "EXACT END")
        .expect("보관본 저장");
    generate_scheduled_report(&ctx, weekly_report_target(tuesday, false)).await;
    assert_eq!(
        ctx.store.load_daily_report(&key).expect("조회").as_deref(),
        Some("EXACT END")
    );
    ctx.store
        .save_daily_report(&key, "not a time", "BROKEN STAMP")
        .expect("보관본 저장");
    generate_scheduled_report(&ctx, weekly_report_target(tuesday, false)).await;
    let replaced = ctx.store.load_daily_report(&key).expect("조회");
    assert!(replaced.is_some_and(|text| text.contains("## 1. This Week Summary")));
}

// 11-1. 예약 생성 종류 --------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduled_weekly_report_overwrites_but_if_missing_keeps_stored_copy() {
    let mut config = build_test_config("report-week-sched");
    config.indexer.enabled = false;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let week_start = NaiveDate::from_ymd_opt(2026, 11, 9).expect("날짜");
    let key = format_week_key(week_start);

    // 1. 보관본 없음: 시작 시 판정이 생성
    generate_scheduled_report(&ctx, ScheduledReport::WeeklyIfStale(week_start)).await;
    let created = ctx.store.load_daily_report(&key).expect("조회");
    assert!(created.is_some_and(|text| text.contains("## 1. This Week Summary")));

    // 2. 보관본 있음: 덮어쓰지 않음
    ctx.store
        .save_daily_report(&key, "2100-01-01T00:00:00.000Z", "STALE WEEK")
        .expect("낡은 리포트 저장");
    generate_scheduled_report(&ctx, ScheduledReport::WeeklyIfStale(week_start)).await;
    assert_eq!(
        ctx.store.load_daily_report(&key).expect("조회").as_deref(),
        Some("STALE WEEK")
    );

    // 3. 월요일 예약(Weekly): 덮어씀
    generate_scheduled_report(&ctx, ScheduledReport::Weekly(week_start)).await;
    let replaced = ctx.store.load_daily_report(&key).expect("조회");
    assert!(replaced.is_some_and(|text| text.contains("## 1. This Week Summary")));
}

// 11-2. 시작 시 지난주 리포트 생성 --------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_writes_last_week_report_on_start_when_missing() {
    // 1. 색인 대기 없이 스케줄러 기동
    let mut config = build_test_config("report-week-start");
    config.indexer.enabled = false;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let last_week = week_start_of(Local::now().date_naive()) - Duration::days(7);
    let key = format_week_key(last_week);
    let scheduler = tokio::spawn(run_report_scheduler(ctx.clone()));

    // 2. 지난주 보관본 대기
    let mut stored: Option<String> = None;
    for _ in 0..100 {
        stored = ctx.store.load_daily_report(&key).expect("조회");
        if stored.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    scheduler.abort();
    let _ = scheduler.await;

    // 3. 본문 확인
    let markdown = stored.expect("지난주 리포트 생성");
    assert!(markdown.contains(&format!("Weekly Agent Report {key} (")));
}

// 12. 관리 포트 주간 라우트 ---------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_week_report_routes_round_trip_and_reject_bad_input() {
    // 1. 서버 기동
    let config = build_test_config("report-week-admin");
    let port = config.ports.admin;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let server = tokio::spawn(run_admin_server(ctx.clone()));
    wait_until_listening(port).await;
    let token = [("x-cm-token", TEST_ADMIN_TOKEN)];

    // 2. 생성 (유효 토큰)
    let (status, _, text) = send_raw("POST", port, "/api/report?week=2026-W02", &token, b"").await;
    assert_eq!(status, 200, "{text}");
    assert!(text.contains("2026-W02.md"), "{text}");

    // 3. 조회
    let (status, head, text) = send_raw("GET", port, "/api/report?week=2026-W02", &[], b"").await;
    assert_eq!(status, 200, "{text}");
    assert!(head.contains("content-type: text/markdown"), "{head}");
    assert!(
        text.contains("2026-W02 (2026-01-05 ~ 2026-01-11)"),
        "{text}"
    );

    // 4. 없는 주와 일일 키 분리
    let (status, _, _) = send_raw("GET", port, "/api/report?week=2026-W03", &[], b"").await;
    assert_eq!(status, 404);
    let (status, _, _) = send_raw("GET", port, "/api/report?date=2026-01-05", &[], b"").await;
    assert_eq!(status, 404);

    // 5. 잘못된 요청 (형식 오류, 없는 주, date 와 week 동시, 둘 다 없음)
    for bad_path in [
        "/api/report?week=2026W02",
        "/api/report?week=2025-W53",
        "/api/report?week=2026-W54",
        "/api/report?week=2026-W02&date=2026-01-05",
        "/api/report",
    ] {
        let (status, _, _) = send_raw("GET", port, bad_path, &[], b"").await;
        assert_eq!(status, 400, "GET {bad_path}");
    }
    for bad_path in [
        "/api/report?week=2025-W53",
        "/api/report?week=2026-W02&date=2026-01-05",
    ] {
        let (status, _, _) = send_raw("POST", port, bad_path, &token, b"").await;
        assert_eq!(status, 400, "POST {bad_path}");
    }

    // 6. 토큰 없는 생성 거부
    let (status, _, _) = send_raw("POST", port, "/api/report?week=2026-W02", &[], b"").await;
    assert_eq!(status, 401);
    server.abort();
    let _ = server.await;
}

// 12. 컴팩션·컨텍스트 구간 ----------------------------------------------------
fn compaction_fixture_batch() -> IngestBatch {
    let otel_row = |key: &str, ts: &str, category: &str| OtelEventRow {
        event_key: key.to_string(),
        ts: ts.to_string(),
        session_id: "sess-comp-1".to_string(),
        category: category.to_string(),
        ..OtelEventRow::default()
    };
    IngestBatch {
        otel_events: vec![
            OtelEventRow {
                status: "auto".to_string(),
                tokens_before: Some(180_000),
                tokens_after: Some(42_000),
                detail: "ok".to_string(),
                ..otel_row("k-auto", "2026-11-10T12:00:00.000Z", "compaction")
            },
            OtelEventRow {
                status: "manual".to_string(),
                tokens_before: Some(150_000),
                detail: "failed timeout".to_string(),
                ..otel_row("k-manual", "2026-11-10T12:30:00.000Z", "compaction")
            },
            OtelEventRow {
                name: "Explore".to_string(),
                status: "sync".to_string(),
                duration_ms: Some(60_000),
                tokens_after: Some(42_000),
                count: Some(7),
                ..otel_row("k-sub", "2026-11-10T12:45:00.000Z", "subagent")
            },
        ],
        ..IngestBatch::default()
    }
}

#[test]
fn compaction_section_is_written_only_with_data_in_both_languages() {
    let workspace = prepare_fixture_workspace("report-compaction");
    index_transcripts_once(&workspace.config, &workspace.store).expect("색인");
    let report_date = NaiveDate::from_ymd_opt(2026, 11, 10).expect("날짜");
    let week_start = NaiveDate::from_ymd_opt(2026, 11, 9).expect("날짜");

    // 1. 기록이 없으면 구간 없음 (기존 구간 번호 유지)
    let before =
        build_daily_report(&workspace.store, report_date, ReportLanguage::Ko).expect("리포트");
    assert!(!before.contains("컴팩션·컨텍스트"));
    assert!(before.contains("## 7. 자동 제안"));
    let tables_before = count_aligned_tables(&before);
    workspace
        .store
        .insert_batch(&compaction_fixture_batch())
        .expect("이벤트 적재");

    // 2. 일일 한국어 (자동 제안 번호가 한 칸 밀림)
    let korean =
        build_daily_report(&workspace.store, report_date, ReportLanguage::Ko).expect("리포트");
    for expected in [
        "## 7. 컴팩션·컨텍스트",
        "- 컴팩션: 2회 (자동 1 / 수동 1 / 실패 1, 세션 1개)",
        "(manual, 세션 sess-com): 150.0K -> - 토큰",
        "- 서브에이전트: 1건 (소요 합계 1분 00초 / 평균 1분 00초, 도구 호출 7회)",
        "직전 -> 이후 토큰",
        "## 8. 자동 제안",
    ] {
        assert!(korean.contains(expected), "누락: {expected}\n{korean}");
    }
    assert_eq!(count_aligned_tables(&korean), tables_before + 1);

    // 3. 일일 영어
    let english =
        build_daily_report(&workspace.store, report_date, ReportLanguage::En).expect("리포트");
    for expected in [
        "## 7. Compaction and Context",
        "- Compactions: 2 (auto 1 / manual 1 / failed 1, sessions 1)",
        "- Subagents: 1 runs (total time 1m 00s / average 1m 00s, tool uses 7)",
        "Pre -> post tokens",
        "## 8. Suggestions",
    ] {
        assert!(english.contains(expected), "누락: {expected}\n{english}");
    }
    assert!(english.lines().any(|line| {
        line.starts_with('│') && line.contains("sess-com") && line.contains("180.0K -> 42.0K")
            || line.starts_with('│') && line.contains("sess-com") && line.contains("150.0K -> -")
    }));
    count_aligned_tables(&english);

    // 4. 주간 (영어·한국어)
    let weekly_en =
        build_weekly_report(&workspace.store, week_start, ReportLanguage::En).expect("주간 리포트");
    assert!(
        weekly_en.contains("## 8. Compaction and Context"),
        "{weekly_en}"
    );
    assert!(weekly_en.contains("## 9. Suggestions"));
    count_aligned_tables(&weekly_en);
    let weekly_ko =
        build_weekly_report(&workspace.store, week_start, ReportLanguage::Ko).expect("주간 리포트");
    assert!(weekly_ko.contains("## 8. 컴팩션·컨텍스트"));
    assert!(weekly_ko.contains("## 9. 자동 제안"));
    count_aligned_tables(&weekly_ko);
}
