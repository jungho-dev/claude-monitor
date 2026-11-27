//! tui/poller.rs
//! 배경 집계 수집과 설정·리포트·관리 요청 스레드 (UI 스레드는 I/O 없음, panic 은 패널 상태로 변환).

use crate::core::config::{AppConfig, DB_FILE_NAME, ReportLanguage, TOKEN_FILE_NAME};
use crate::core::model::LiveEvent;
use crate::inventory;
use crate::net::admin;
use crate::report;
use crate::serve::BUILD_ID;
use crate::store::Store;
use crate::store::metrics::{AttributionDimension, MetricAttributionRow};
use crate::store::query::{
    AgentStatsRow, CacheBreakRow, HookStatRow, ModelStatsRow, ProjectStatsRow, SkillStatsRow,
    ToolLatencyRow,
};
use crate::tui::format::fit_display;
use crate::tui::i18n::Failure;
use crate::tui::state::{
    ATTRIBUTION_DIMENSIONS, DashboardSnapshot, DbState, LimitsData, Panel, UiMessage, WindowKind,
};
use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveDate, SecondsFormat, Utc};
use std::cell::Cell;
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

pub const POLL_INTERVAL: Duration = Duration::from_secs(2);
// 무거운 집계 패널 갱신 주기 (구간 변경·새로고침 명령은 즉시)
const HEAVY_INTERVAL: Duration = Duration::from_secs(10);
const RECENT_EVENT_LIMIT: usize = 200;
const TOOL_ROW_LIMIT: usize = 200;
const CACHE_BREAK_LIMIT: usize = 20;
const MINUTE_SPAN_MINUTES: i64 = 60;
const RESPONSE_PREVIEW_WIDTH: usize = 60;

thread_local! {
    // 켜진 스레드의 panic 은 패널로 변환되므로 화면에 출력하지 않음
    pub static QUIET_PANIC: Cell<bool> = const { Cell::new(false) };
}

// 1. 수집 스레드 명령 ---------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollCommand {
    SetWindow(WindowKind),
    RefreshNow,
}

// 2. 관리 포트 요청 종류 ------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAction {
    Reindex,
    GenerateReport(NaiveDate),
    // 해당 주의 월요일
    GenerateWeeklyReport(NaiveDate),
}

// 관리 요청 결과 (문구는 UI 스레드가 화면 언어로 만듦, 본문은 데이터라 번역하지 않음)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminOutcome {
    NoToken,
    Done(String),
    Failed(Failure),
    Waiting,
}

// 3. panic 격리 ---------------------------------------------------------------
// 오류는 본문, panic 은 내부 오류로 변환
pub fn guard_panel<T>(work: impl FnOnce() -> anyhow::Result<T>) -> Panel<T> {
    QUIET_PANIC.with(|quiet| quiet.set(true));
    let outcome = panic::catch_unwind(AssertUnwindSafe(work));
    QUIET_PANIC.with(|quiet| quiet.set(false));

    match outcome {
        Ok(Ok(loaded)) => Panel::Ready(loaded),
        Ok(Err(err)) => Panel::Failed(Failure::Error(format!("{err:#}"))),
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("");
            Panel::Failed(Failure::Internal(message.to_string()))
        }
    }
}

// 4. 이벤트 시각과 식별 키 ----------------------------------------------------
pub fn live_event_timestamp(event: &LiveEvent) -> &str {
    match event {
        LiveEvent::Api(row) => &row.ts,
        LiveEvent::Tool(row) => &row.ts,
        LiveEvent::Hook(row) => &row.ts,
        LiveEvent::Otel(row) => &row.ts,
        LiveEvent::Notice { ts, .. } => ts,
    }
}

// 갱신 전후 같은 이벤트를 찾는 키: 시각, 종류 태그, 모델·도구·훅 경로·알림 문구
pub fn live_event_key(event: &LiveEvent) -> String {
    let (tag, target) = match event {
        LiveEvent::Api(row) => ("api", row.model.clone()),
        LiveEvent::Tool(row) => ("tool", row.tool_name.clone()),
        LiveEvent::Hook(row) => ("hook", format!("{}/{}", row.route, row.guard)),
        LiveEvent::Otel(row) => ("otel", format!("{}/{}", row.category, row.name)),
        LiveEvent::Notice { text, .. } => ("notice", text.clone()),
    };
    format!("{}\u{1}{tag}\u{1}{target}", live_event_timestamp(event))
}

// 5. 수집 스레드 기동 ---------------------------------------------------------
pub fn spawn_poller(
    config: AppConfig,
    ui_tx: Sender<UiMessage>,
) -> anyhow::Result<Sender<PollCommand>> {
    let (command_tx, command_rx) = mpsc::channel();
    thread::Builder::new()
        .name("cm-tui-poller".to_string())
        .spawn(move || run_poll_loop(config, ui_tx, command_rx))?;
    Ok(command_tx)
}

// 6. 수집 루프와 무거운 패널 캐시 ---------------------------------------------
// 무거운 집계 다섯 패널의 마지막 조회 결과
#[derive(Default)]
struct HeavyPanels {
    tools: Panel<Vec<ToolLatencyRow>>,
    hooks: Panel<Vec<HookStatRow>>,
    model_stats: Panel<Vec<ModelStatsRow>>,
    agent_stats: Panel<Vec<AgentStatsRow>>,
    cache_breaks: Panel<Vec<CacheBreakRow>>,
    projects: Panel<Vec<ProjectStatsRow>>,
    skills: Panel<Vec<SkillStatsRow>>,
    attribution: Panel<Vec<(AttributionDimension, Vec<MetricAttributionRow>)>>,
}

// 무거운 패널 재조회 여부: 강제 요청, 첫 수집, 주기 경과
fn heavy_due(last: Option<Instant>, now: Instant, forced: bool) -> bool {
    forced || last.is_none_or(|stamp| now.duration_since(stamp) >= HEAVY_INTERVAL)
}

// 이전 버전 serve 가 만든 DB 는 열 추가가 안 됐을 수 있어 읽기 전용 수집 전에 쓰기 연결로 스키마를 맞춤
// (DB 가 없으면 만들지 않음, 바쁜 serve 와 경합해 오래 걸릴 수 있어 UI 스레드가 아니라 수집 스레드에서 하고, 실패하면 오류 원문을 돌려줌)
fn upgrade_store_schema(config: &AppConfig) -> Option<String> {
    let db_path = config.data_dir.join(DB_FILE_NAME);
    if !db_path.is_file() {
        return None;
    }
    match guard_panel(|| Store::open_store(&db_path)) {
        Panel::Failed(Failure::Error(detail) | Failure::Internal(detail)) => Some(detail),
        _ => None,
    }
}

// 2초마다 또는 명령 도착 즉시 수집, UI 가 닫히면 종료 (최초 수집 전에 스키마 갱신 한 번)
fn run_poll_loop(config: AppConfig, ui_tx: Sender<UiMessage>, command_rx: Receiver<PollCommand>) {
    if let Some(detail) = upgrade_store_schema(&config)
        && ui_tx
            .send(UiMessage::SchemaUpgradeFailed { detail })
            .is_err()
    {
        return;
    }
    let mut window = WindowKind::default();
    let mut store: Option<Store> = None;
    let mut heavy = HeavyPanels::default();
    let mut last_heavy: Option<Instant> = None;
    let mut forced = false;
    loop {
        // 1. 수집 (DB 가 열린 수집만 무거운 패널 갱신 시각 기록)
        let now = Instant::now();
        let due = heavy_due(last_heavy, now, forced);
        let snapshot = collect_snapshot(&config, window, &mut store, &mut heavy, due);
        last_heavy = match (&snapshot.db_state, due) {
            (DbState::Open, true) => Some(now),
            (DbState::Open, false) => last_heavy,
            _ => None,
        };
        forced = false;
        if ui_tx.send(UiMessage::Snapshot(Box::new(snapshot))).is_err() {
            return;
        }

        // 2. 대기 (구간 변경은 캐시를 비우고 재조회, 새로고침은 재조회만)
        match command_rx.recv_timeout(POLL_INTERVAL) {
            Ok(PollCommand::SetWindow(next_window)) => {
                window = next_window;
                heavy = HeavyPanels::default();
                forced = true;
            }
            Ok(PollCommand::RefreshNow) => forced = true,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

// 7. 스냅샷 수집 --------------------------------------------------------------
// refresh_heavy 이면 무거운 다섯 패널을 조회해 캐시 갱신, 아니면 캐시 복사
fn collect_snapshot(
    config: &AppConfig,
    window: WindowKind,
    store: &mut Option<Store>,
    heavy: &mut HeavyPanels,
    refresh_heavy: bool,
) -> DashboardSnapshot {
    let now = Utc::now();
    let mut snapshot = DashboardSnapshot {
        window,
        taken_at: Some(now),
        ..DashboardSnapshot::default()
    };

    // 1. 관리 포트 상태 (연결 실패는 serve 미실행)
    snapshot.status = match guard_panel(|| admin::fetch_status_blocking(config.ports.admin)) {
        Panel::Failed(_) => Panel::Failed(Failure::ServeOff),
        other => other,
    };
    snapshot.serve_stale = matches!(
        &snapshot.status,
        Panel::Ready(status) if !BUILD_ID.is_empty() && status.build_id != *BUILD_ID
    );

    // 2. DB 열기 (없으면 빈 상태)
    let db_path = config.data_dir.join(DB_FILE_NAME);
    if !db_path.is_file() {
        *store = None;
        snapshot.db_state = DbState::Missing;
        return snapshot;
    }
    if store.is_none() {
        match guard_panel(|| Store::open_read_only(&db_path)) {
            Panel::Ready(opened) => *store = Some(opened),
            Panel::Failed(failure) => {
                snapshot.db_state = DbState::Failed(failure);
                return snapshot;
            }
            Panel::Pending => {
                snapshot.db_state = DbState::Failed(Failure::DbOpenFailed);
                return snapshot;
            }
        }
    }
    let Some(open_store) = store.as_ref() else {
        return snapshot;
    };
    snapshot.db_state = DbState::Open;

    // 3. 구간 경계
    let WindowBounds {
        since,
        until,
        minute_since,
        today_since,
    } = build_window_bounds(now, window);

    // 4. 가벼운 집계 조회 (매번)
    snapshot.overview = guard_panel(|| open_store.query_overview(&since, &until));
    snapshot.today = guard_panel(|| open_store.query_overview(&today_since, &until));
    snapshot.minutes = guard_panel(|| open_store.query_minute_buckets(&minute_since, &until));
    snapshot.events = guard_panel(|| open_store.query_recent_events(RECENT_EVENT_LIMIT));

    // 5. 무거운 집계 조회 (주기·명령 때만, 그 외에는 캐시)
    if refresh_heavy {
        heavy.tools = guard_panel(|| open_store.query_tool_latency(&since, &until, TOOL_ROW_LIMIT));
        heavy.hooks = guard_panel(|| open_store.query_hook_stats(&since, &until));
        heavy.model_stats = guard_panel(|| open_store.query_model_stats(&since, &until));
        heavy.agent_stats = guard_panel(|| open_store.query_agent_stats(&since, &until));
        heavy.cache_breaks =
            guard_panel(|| open_store.query_cache_breaks(&since, &until, CACHE_BREAK_LIMIT));
        heavy.projects = guard_panel(|| open_store.query_project_stats(&since, &until));
        heavy.skills = guard_panel(|| open_store.query_skill_stats(&since, &until));
        // 지표 테이블이 없는 이전 DB 는 지표 없음(빈 목록)으로 취급
        heavy.attribution = guard_panel(|| {
            let mut by_dimension = Vec::with_capacity(ATTRIBUTION_DIMENSIONS.len());
            for dimension in ATTRIBUTION_DIMENSIONS {
                let rows = match open_store.query_metric_attribution(&since, &until, dimension) {
                    Ok(rows) => rows,
                    Err(err) if format!("{err:#}").contains("no such table") => Vec::new(),
                    Err(err) => return Err(err),
                };
                by_dimension.push((dimension, rows));
            }
            Ok(by_dimension)
        });
    }
    snapshot.tools = heavy.tools.clone();
    snapshot.hooks = heavy.hooks.clone();
    snapshot.model_stats = heavy.model_stats.clone();
    snapshot.agent_stats = heavy.agent_stats.clone();
    snapshot.cache_breaks = heavy.cache_breaks.clone();
    snapshot.projects = heavy.projects.clone();
    snapshot.skills = heavy.skills.clone();
    snapshot.attribution = heavy.attribution.clone();

    // 5-1. 모델 탭 캐시 카드 입력 (공식 최신 스냅샷, 매번)
    snapshot.limits = guard_panel(|| {
        Ok(LimitsData {
            latest: open_store.latest_limit_snapshot()?,
        })
    });

    // 6. 최근 이벤트 최신순 정렬 (조회 순서 가정 제거)
    if let Panel::Ready(events) = &mut snapshot.events {
        events.sort_by(|left, right| live_event_timestamp(right).cmp(live_event_timestamp(left)));
    }

    snapshot
}

// 7-1. 구간 경계 --------------------------------------------------------------
// 문자열 비교라 저장 ts 와 밀리초 정밀도 일치
struct WindowBounds {
    since: String,
    until: String,
    minute_since: String,
    today_since: String,
}

// 반열린 구간 (현재 이벤트 여유 1분, 오늘은 로컬 자정부터)
fn build_window_bounds(now: DateTime<Utc>, window: WindowKind) -> WindowBounds {
    let today_start = match Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|midnight| midnight.and_local_timezone(Local).earliest())
    {
        Some(midnight) => midnight.with_timezone(&Utc),
        None => now - ChronoDuration::hours(24),
    };
    let since = match window.window_span() {
        Some(span) => now - span,
        None => today_start,
    };
    WindowBounds {
        since: since.to_rfc3339_opts(SecondsFormat::Millis, true),
        until: (now + ChronoDuration::minutes(1)).to_rfc3339_opts(SecondsFormat::Millis, true),
        minute_since: (now - ChronoDuration::minutes(MINUTE_SPAN_MINUTES))
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        today_since: today_start.to_rfc3339_opts(SecondsFormat::Millis, true),
    }
}

// 8. 설정 적재 스레드 ---------------------------------------------------------
pub fn spawn_settings_load(config: AppConfig, ui_tx: Sender<UiMessage>) {
    let _ = thread::Builder::new()
        .name("cm-tui-settings".to_string())
        .spawn(move || {
            let inventory = Box::new(guard_panel(|| {
                inventory::load_inventory(&config.claude_home)
            }));
            let doctor = guard_panel(|| inventory::run_doctor_checks(&config));
            let _ = ui_tx.send(UiMessage::Settings { inventory, doctor });
        });
}

// 9. 리포트 본문 적재 ---------------------------------------------------------
// DB 없음은 안내 문구, 리포트 없음은 Ready(None)
// live = 화면이 지금 "오늘"로 보는 날 (시계를 다시 읽지 않고 UI 의 판단을 그대로 따름)
pub fn load_report_body(config: &AppConfig, date: NaiveDate, live: bool) -> Panel<Option<String>> {
    let db_path = config.data_dir.join(DB_FILE_NAME);
    if !db_path.is_file() {
        return Panel::Failed(Failure::DbMissing);
    }
    // 오늘은 아직 끝나지 않은 날이라 저장본 대신 읽기 전용 DB 로 지금까지의 리포트를 바로 만듦 (저장하지 않음)
    guard_panel(|| {
        let store = Store::open_read_only(&db_path)?;
        if live {
            return report::build_daily_report(&store, date, config.report.language).map(Some);
        }
        store.load_daily_report(&date.format("%Y-%m-%d").to_string())
    })
}

// 9-1. 주간 리포트 본문 적재 --------------------------------------------------
// live = 화면이 지금 "이번 주"로 보는 주: 읽기 전용 DB 로 지금까지의 주간 본문을 바로 만들고, 아니면 저장본을 읽음 (저장 키 YYYY-Www)
pub fn load_weekly_report_body(
    config: &AppConfig,
    week_start: NaiveDate,
    live: bool,
) -> Panel<Option<String>> {
    let db_path = config.data_dir.join(DB_FILE_NAME);
    if !db_path.is_file() {
        return Panel::Failed(Failure::DbMissing);
    }
    guard_panel(|| {
        let store = Store::open_read_only(&db_path)?;
        if live {
            return report::build_weekly_report(&store, week_start, config.report.language)
                .map(Some);
        }
        store.load_daily_report(&report::format_week_key(week_start))
    })
}

pub fn spawn_weekly_report_load(
    config: AppConfig,
    ui_tx: Sender<UiMessage>,
    week_start: NaiveDate,
    live: bool,
) {
    let _ = thread::Builder::new()
        .name("cm-tui-report-week".to_string())
        .spawn(move || {
            let body = load_weekly_report_body(&config, week_start, live);
            let _ = ui_tx.send(UiMessage::WeeklyReport {
                week_start,
                language: config.report.language,
                body,
            });
        });
}

// 10. 리포트 적재 스레드 ------------------------------------------------------
pub fn spawn_report_load(config: AppConfig, ui_tx: Sender<UiMessage>, date: NaiveDate, live: bool) {
    let _ = thread::Builder::new()
        .name("cm-tui-report".to_string())
        .spawn(move || {
            let body = load_report_body(&config, date, live);
            let _ = ui_tx.send(UiMessage::Report {
                date,
                language: config.report.language,
                body,
            });
        });
}

// 11. 관리 포트 요청 스레드 ---------------------------------------------------
// 토큰 파일 없음 또는 빈 값은 serve 미실행으로 안내, 리포트 생성 성공 시 본문 재적재
// today = UI 가 지금 오늘로 보는 날 (재적재의 실시간 여부는 이 날 기준, 시계를 다시 읽지 않음)
pub fn spawn_admin_action(
    config: AppConfig,
    ui_tx: Sender<UiMessage>,
    action: AdminAction,
    today: NaiveDate,
) {
    let _ = thread::Builder::new()
        .name("cm-tui-admin".to_string())
        .spawn(move || {
            // 1. 요청 경로 (리포트는 설정의 리포트 언어를 lang 으로 실어 보냄: 호출 쪽이 화면 언어로 맞춘 값)
            let lang_code = match config.report.language {
                ReportLanguage::Ko => "ko",
                ReportLanguage::En => "en",
            };
            let request_path = match action {
                AdminAction::Reindex => "/api/reindex".to_string(),
                AdminAction::GenerateReport(date) => {
                    format!(
                        "/api/report?date={}&lang={lang_code}",
                        date.format("%Y-%m-%d")
                    )
                }
                AdminAction::GenerateWeeklyReport(week_start) => {
                    format!(
                        "/api/report?week={}&lang={lang_code}",
                        report::format_week_key(week_start)
                    )
                }
            };

            // 2. 토큰 읽기
            let token = fs::read_to_string(config.data_dir.join(TOKEN_FILE_NAME))
                .map(|text| text.trim().to_string())
                .unwrap_or_default();
            if token.is_empty() {
                let _ = ui_tx.send(UiMessage::Admin {
                    action,
                    outcome: AdminOutcome::NoToken,
                });
                return;
            }

            // 3. 요청과 결과 알림
            let outcome = guard_panel(|| {
                admin::post_admin_blocking(config.ports.admin, &request_path, &token)
            });
            let result = match outcome {
                Panel::Ready(response) => AdminOutcome::Done(
                    fit_display(&response, RESPONSE_PREVIEW_WIDTH)
                        .trim_end()
                        .to_string(),
                ),
                Panel::Failed(reason) => AdminOutcome::Failed(reason),
                Panel::Pending => AdminOutcome::Waiting,
            };
            let succeeded = matches!(result, AdminOutcome::Done(_));
            let _ = ui_tx.send(UiMessage::Admin {
                action,
                outcome: result,
            });

            // 4. 리포트 생성 후 본문 갱신
            if let (true, AdminAction::GenerateReport(date)) = (succeeded, action) {
                let body = load_report_body(&config, date, date == today);
                let _ = ui_tx.send(UiMessage::Report {
                    date,
                    language: config.report.language,
                    body,
                });
            }
            if let (true, AdminAction::GenerateWeeklyReport(week_start)) = (succeeded, action) {
                let live = week_start == report::week_start_of(today);
                let body = load_weekly_report_body(&config, week_start, live);
                let _ = ui_tx.send(UiMessage::WeeklyReport {
                    week_start,
                    language: config.report.language,
                    body,
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{ApiRequestRow, ToolCallRow};
    use chrono::TimeZone;

    #[test]
    fn guard_panel_maps_ready_error_and_panics() {
        let ready = guard_panel(|| Ok(7));
        assert!(matches!(ready, Panel::Ready(7)));

        let failed: Panel<i32> = guard_panel(|| anyhow::bail!("연결 거부"));
        assert!(
            matches!(&failed, Panel::Failed(Failure::Error(text)) if text.contains("연결 거부"))
        );

        let todo_panel: Panel<i32> = guard_panel(|| todo!("worker: someone"));
        assert!(matches!(&todo_panel, Panel::Failed(Failure::Internal(_))));

        let crashed: Panel<i32> = guard_panel(|| panic!("boom"));
        assert!(
            matches!(&crashed, Panel::Failed(Failure::Internal(text)) if text.contains("boom"))
        );

        // panic 뒤 격리 표시는 해제
        assert!(!QUIET_PANIC.with(Cell::get));
    }

    #[test]
    fn todays_report_is_built_live_and_other_days_read_the_stored_copy() {
        let dir = std::env::temp_dir().join(format!("cm-report-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Store::open_store(&dir.join(DB_FILE_NAME)).unwrap();
        let config = AppConfig {
            data_dir: dir.clone(),
            ..crate::tui::test_support::sample_config()
        };
        let today = Local::now().date_naive();

        // 오늘: 저장본이 없어도 지금까지의 본문을 만듦
        let live = load_report_body(&config, today, true);
        let label = today.format("%Y-%m-%d").to_string();
        assert!(
            matches!(&live, Panel::Ready(Some(body)) if body.contains(&label)),
            "{label}"
        );

        // 어제: 저장본만 읽으므로 없으면 빈 값
        let yesterday = today.pred_opt().unwrap();
        assert!(matches!(
            load_report_body(&config, yesterday, false),
            Panel::Ready(None)
        ));

        // 실시간 여부는 호출한 UI 의 판단을 따름 (시계를 다시 읽지 않음): 자정이 지나 지난 날이 된 날은 live=false 로 저장본만, 새 오늘은 live=true 로 바로 만듦
        assert!(matches!(
            load_report_body(&config, today, false),
            Panel::Ready(None)
        ));
        let next_day = today.succ_opt().unwrap();
        let next_label = next_day.format("%Y-%m-%d").to_string();
        assert!(
            matches!(
                &load_report_body(&config, next_day, true),
                Panel::Ready(Some(body)) if body.contains(&next_label)
            ),
            "{next_label}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn this_week_is_built_live_and_past_weeks_read_the_stored_copy() {
        let dir = std::env::temp_dir().join(format!("cm-report-week-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let store = Store::open_store(&dir.join(DB_FILE_NAME)).unwrap();
        let config = AppConfig {
            data_dir: dir.clone(),
            ..crate::tui::test_support::sample_config()
        };
        let this_week = report::week_start_of(Local::now().date_naive());
        let last_week = this_week - ChronoDuration::days(7);

        // 이번 주: 저장본이 없어도 지금까지의 주간 본문을 만들고 저장하지 않음
        let key = report::format_week_key(this_week);
        let live = load_weekly_report_body(&config, this_week, true);
        assert!(
            matches!(&live, Panel::Ready(Some(body)) if body.contains(&key)),
            "{key}"
        );
        assert_eq!(store.load_daily_report(&key).unwrap(), None);

        // 지난 주: 저장본만 읽으므로 없으면 빈 값, 저장하면 그 본문
        assert!(matches!(
            load_weekly_report_body(&config, last_week, false),
            Panel::Ready(None)
        ));

        // 실시간 여부는 UI 의 판단: 자정을 넘긴 이전 주는 live=false 로 저장본만, 새 이번 주는 live=true 로 바로 만듦
        assert!(matches!(
            load_weekly_report_body(&config, this_week, false),
            Panel::Ready(None)
        ));
        let next_week = this_week + ChronoDuration::days(7);
        let next_key = report::format_week_key(next_week);
        assert!(matches!(
            &load_weekly_report_body(&config, next_week, true),
            Panel::Ready(Some(body)) if body.contains(&next_key)
        ));
        store
            .save_daily_report(
                &report::format_week_key(last_week),
                "2026-11-15T00:00:00.000Z",
                "stored weekly body",
            )
            .unwrap();
        assert!(matches!(
            &load_weekly_report_body(&config, last_week, false),
            Panel::Ready(Some(body)) if body == "stored weekly body"
        ));

        // DB 없음은 일간과 같은 안내
        let missing = AppConfig {
            data_dir: dir.join("missing"),
            ..config
        };
        assert!(matches!(
            load_weekly_report_body(&missing, this_week, true),
            Panel::Failed(Failure::DbMissing)
        ));
        drop(store);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn limits_panel_carries_the_latest_snapshot_on_every_poll() {
        use crate::store::limits::LimitSnapshot;

        let dir = std::env::temp_dir().join(format!("cm-limits-poll-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let writer = Store::open_store(&dir.join(DB_FILE_NAME)).unwrap();
        writer
            .insert_limit_snapshot(&LimitSnapshot {
                ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                session_id: "sess-1".to_string(),
                five_hour_pct: Some(42.0),
                ..LimitSnapshot::default()
            })
            .unwrap();
        drop(writer);
        let config = AppConfig {
            data_dir: dir.clone(),
            ..crate::tui::test_support::sample_config()
        };
        let mut store = None;
        let mut heavy = HeavyPanels::default();

        // 처음(무거운 주기): 공식 최신 스냅샷
        let first = collect_snapshot(&config, WindowKind::Hour, &mut store, &mut heavy, true);
        let Panel::Ready(data) = &first.limits else {
            panic!("{:?}", first.limits);
        };
        assert_eq!(
            data.latest.as_ref().and_then(|latest| latest.five_hour_pct),
            Some(42.0)
        );

        // 무거운 주기가 아니어도 스냅샷은 다시 읽음
        let second = collect_snapshot(&config, WindowKind::Hour, &mut store, &mut heavy, false);
        let Panel::Ready(cached) = &second.limits else {
            panic!("{:?}", second.limits);
        };
        assert!(cached.latest.is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn heavy_panels_refresh_only_when_due() {
        let start = Instant::now();

        // 첫 호출은 항상 갱신
        assert!(heavy_due(None, start, false));
        // 9초 뒤 대기, 10초 뒤 갱신
        assert!(!heavy_due(
            Some(start),
            start + Duration::from_secs(9),
            false
        ));
        assert!(heavy_due(
            Some(start),
            start + Duration::from_secs(10),
            false
        ));
        // 새로고침·구간 변경은 즉시 갱신
        assert!(heavy_due(Some(start), start + Duration::from_secs(1), true));
    }

    #[test]
    fn event_key_separates_kind_target_and_time() {
        let tool = |ts: &str, name: &str| {
            LiveEvent::Tool(ToolCallRow {
                ts: ts.to_string(),
                tool_name: name.to_string(),
                ..ToolCallRow::default()
            })
        };
        assert_eq!(
            live_event_key(&tool("t1", "Bash")),
            live_event_key(&tool("t1", "Bash"))
        );
        assert_ne!(
            live_event_key(&tool("t1", "Bash")),
            live_event_key(&tool("t2", "Bash"))
        );
        assert_ne!(
            live_event_key(&tool("t1", "Bash")),
            live_event_key(&tool("t1", "Read"))
        );

        let api = LiveEvent::Api(ApiRequestRow {
            ts: "t1".to_string(),
            model: "Bash".to_string(),
            ..ApiRequestRow::default()
        });
        assert_ne!(live_event_key(&api), live_event_key(&tool("t1", "Bash")));
    }

    #[test]
    fn event_timestamp_reads_every_variant() {
        let api = LiveEvent::Api(ApiRequestRow {
            ts: "a".to_string(),
            ..ApiRequestRow::default()
        });
        let tool = LiveEvent::Tool(ToolCallRow {
            ts: "b".to_string(),
            ..ToolCallRow::default()
        });
        let notice = LiveEvent::Notice {
            ts: "c".to_string(),
            level: String::new(),
            text: String::new(),
        };
        assert_eq!(live_event_timestamp(&api), "a");
        assert_eq!(live_event_timestamp(&tool), "b");
        assert_eq!(live_event_timestamp(&notice), "c");
    }

    #[test]
    fn window_bounds_use_millisecond_precision_like_stored_ts() {
        let now = Utc.with_ymd_and_hms(2026, 11, 14, 16, 0, 0).unwrap();
        let bounds = build_window_bounds(now, WindowKind::Hour);
        assert_eq!(bounds.since, "2026-11-14T15:00:00.000Z");
        assert_eq!(bounds.until, "2026-11-14T16:01:00.000Z");
        assert_eq!(bounds.minute_since, "2026-11-14T15:00:00.000Z");
        assert_eq!(bounds.today_since.len(), 24);
        assert!(bounds.today_since.ends_with(".000Z"));

        // 구간 시작 직후 밀리초 이벤트는 경계에 포함
        let stored_ts = "2026-11-14T15:00:00.500Z";
        assert!(stored_ts >= bounds.since.as_str());
        assert!(stored_ts >= bounds.minute_since.as_str());

        // 오늘 구간은 로컬 자정부터
        let today = build_window_bounds(now, WindowKind::Today);
        assert_eq!(today.since, today.today_since);
    }

    #[test]
    fn report_body_reports_missing_database() {
        let mut config = crate::tui::test_support::sample_config();
        config.data_dir = std::env::temp_dir().join("cm-tui-test-no-such-dir");
        let date = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        let body = load_report_body(&config, date, false);
        assert!(matches!(&body, Panel::Failed(Failure::DbMissing)));
    }

    // 스키마 갱신은 수집 스레드의 최초 수집 전 작업: DB 없음·정상은 조용, 열 수 없는 손상 파일은 오류 원문을 돌려줌
    #[test]
    fn schema_upgrade_runs_on_the_poller_side_and_reports_a_failure_detail() {
        let dir = std::env::temp_dir().join(format!("cm-schema-upgrade-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let config = AppConfig {
            data_dir: dir.clone(),
            ..crate::tui::test_support::sample_config()
        };

        // DB 없음: 만들지 않고 실패도 아님
        assert_eq!(upgrade_store_schema(&config), None);
        assert!(!dir.join(DB_FILE_NAME).exists());

        // 정상 DB: 실패 없음
        drop(Store::open_store(&dir.join(DB_FILE_NAME)).unwrap());
        assert_eq!(upgrade_store_schema(&config), None);

        // SQLite 가 아닌 파일: 실패 원문을 돌려줌 (알림은 UI 스레드가 화면 언어로 만듦)
        fs::write(dir.join(DB_FILE_NAME), vec![b'x'; 4096]).unwrap();
        let detail = upgrade_store_schema(&config);
        assert!(detail.is_some_and(|text| !text.is_empty()));
        let _ = fs::remove_dir_all(&dir);
    }
}
