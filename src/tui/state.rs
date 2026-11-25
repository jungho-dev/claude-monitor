//! tui/state.rs
//! TUI 화면 상태: 탭, 카드 포커스·배치·스크롤, 집계 구간, 패널 데이터, 키 입력 상태 전이.

use crate::core::config::{AppConfig, TUI_STATE_FILE_NAME};
use crate::core::model::{LiveEvent, StatusSnapshot};
use crate::inventory::{DoctorFinding, Inventory};
use crate::report::week_start_of;
use crate::store::limits::LimitSnapshot;
use crate::store::metrics::{AttributionDimension, MetricAttributionRow};
use crate::store::otel_events::CompactionStat;
use crate::store::query::{
    AgentStatsRow, CacheBreakRow, HookStatRow, MinuteBucket, ModelStatsRow, OverviewStats,
    ProjectStatsRow, SkillStatsRow, ToolLatencyRow,
};
use crate::store::ratelimit::RatelimitSnapshotRow;
use crate::tui::cards::{
    CARD_COUNT, CardId, CardKind, CardMove, CardPress, CardView, ColumnDrag, CursorMotion,
    MAX_TABLE_COLUMNS, ScrollDrag, UiAction, UiHitAreas,
};
use crate::tui::format::{outline_entries, prepare_report_markdown};
use crate::tui::i18n::{self, Failure, Lang, Msg};
use crate::tui::layout::{CardLayout, SplitDrag, SplitGroup};
use crate::tui::poller::{AdminAction, AdminOutcome, live_event_key};
use crate::tui::sort::{
    SortChange, SortState, compare_agent_row, compare_attribution_row, compare_cache_break_row,
    compare_event_row, compare_hook_row, compare_model_row, compare_project_row, compare_skill_row,
    compare_tool_row, default_card_sorts, sort_rows, sort_spec,
};
use crate::tui::tab_move::TabDrag;
use crate::tui::views::models::agent_rows;
use crate::tui::views::projects::attribution_rows;
use crate::tui::views::skills::skill_rows;
use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveDate, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const TAB_COUNT: usize = 9;
pub const TOAST_LIFETIME: Duration = Duration::from_secs(4);
// 정렬해도 같은 행을 따라가는 선택 가능 표 카드
const SELECTABLE_SORT_CARDS: [CardId; 9] = [
    CardId::EventFeed,
    CardId::ToolTable,
    CardId::HookTable,
    CardId::ModelTable,
    CardId::AgentTable,
    CardId::CacheBreaks,
    CardId::ProjectTable,
    CardId::AttributionTable,
    CardId::SkillTable,
];
// 프로젝트 탭 귀속 표의 차원 순환 순서 (d 키)
pub const ATTRIBUTION_DIMENSIONS: [AttributionDimension; 5] = [
    AttributionDimension::Skill,
    AttributionDimension::McpServer,
    AttributionDimension::Plugin,
    AttributionDimension::Agent,
    AttributionDimension::Model,
];

// 1. 탭 -----------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    Overview,
    Tools,
    Hooks,
    Models,
    Agents,
    Settings,
    Report,
    Projects,
    Skills,
}

impl ActiveTab {
    pub const ALL: [Self; TAB_COUNT] = [
        Self::Overview,
        Self::Tools,
        Self::Hooks,
        Self::Models,
        Self::Agents,
        Self::Settings,
        Self::Report,
        Self::Projects,
        Self::Skills,
    ];

    // 나중에 더한 탭: 이전 상태 파일의 탭 순서에 없으면 맨 뒤에 붙여 복원
    pub const LATER_TABS: [Self; 1] = [Self::Skills];

    pub fn tab_label(self, lang: Lang) -> &'static str {
        let msg = match self {
            Self::Overview => Msg::TabOverview,
            Self::Tools => Msg::TabTools,
            Self::Hooks => Msg::TabHooks,
            Self::Models => Msg::TabModels,
            Self::Agents => Msg::TabAgents,
            Self::Settings => Msg::TabSettings,
            Self::Report => Msg::TabReport,
            Self::Projects => Msg::TabProjects,
            Self::Skills => Msg::TabSkills,
        };
        i18n::text(lang, msg)
    }

    // 상태 파일의 탭 순서 값 (표시 언어와 무관한 고정 이름)
    pub fn tab_key(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Tools => "tools",
            Self::Hooks => "hooks",
            Self::Models => "models",
            Self::Agents => "agents",
            Self::Settings => "settings",
            Self::Report => "report",
            Self::Projects => "projects",
            Self::Skills => "skills",
        }
    }

    // 클릭한 카드가 없을 때 키 입력을 받는 탭의 주 카드
    pub fn main_card(self) -> CardId {
        match self {
            Self::Overview => CardId::EventFeed,
            Self::Tools => CardId::ToolTable,
            Self::Hooks => CardId::HookTable,
            Self::Models => CardId::ModelTable,
            Self::Agents => CardId::AgentTable,
            Self::Settings => CardId::SettingsHooks,
            Self::Report => CardId::ReportBody,
            Self::Projects => CardId::ProjectTable,
            Self::Skills => CardId::SkillTable,
        }
    }

    // 열 폭을 끌어 조절하는 표 카드 (배치 초기화 대상)
    pub fn table_cards(self) -> &'static [CardId] {
        match self {
            Self::Overview => &[CardId::EventFeed],
            Self::Tools => &[CardId::ToolTable],
            Self::Hooks => &[CardId::HookTable],
            Self::Models => &[CardId::ModelTable, CardId::CacheBreaks],
            Self::Agents => &[CardId::AgentTable],
            Self::Settings => &[
                CardId::SettingsHooks,
                CardId::SettingsMcp,
                CardId::SettingsAgents,
                CardId::SettingsDoctor,
            ],
            Self::Report => &[],
            Self::Projects => &[CardId::ProjectTable, CardId::AttributionTable],
            Self::Skills => &[CardId::SkillTable],
        }
    }

    // 탭 안 카드 경계 그룹 (배치 초기화 대상)
    pub fn split_groups(self) -> &'static [SplitGroup] {
        match self {
            Self::Overview => &[
                SplitGroup::OverviewRows,
                SplitGroup::OverviewCompactRows,
                SplitGroup::OverviewKpiCols,
                SplitGroup::OverviewTrendCols,
                SplitGroup::OverviewFeedCols,
            ],
            Self::Tools => &[SplitGroup::ToolsRows],
            Self::Hooks => &[SplitGroup::HooksRows],
            Self::Models => &[SplitGroup::ModelsRows],
            Self::Agents => &[SplitGroup::AgentsRows],
            Self::Settings => &[SplitGroup::SettingsRows],
            Self::Report => &[SplitGroup::ReportCols],
            Self::Projects => &[SplitGroup::ProjectsRows],
            Self::Skills => &[SplitGroup::SkillsRows],
        }
    }
}

// 2. 집계 구간 ----------------------------------------------------------------
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WindowKind {
    #[default]
    Today,
    Hour,
    Day,
    Week,
}

impl WindowKind {
    pub fn window_label(self, lang: Lang) -> &'static str {
        let msg = match self {
            Self::Today => Msg::WindowToday,
            Self::Hour => Msg::WindowHour,
            Self::Day => Msg::WindowDay,
            Self::Week => Msg::WindowWeek,
        };
        i18n::text(lang, msg)
    }

    // None = 고정 길이 없이 로컬 자정부터 (오늘)
    pub fn window_span(self) -> Option<ChronoDuration> {
        match self {
            Self::Today => None,
            Self::Hour => Some(ChronoDuration::hours(1)),
            Self::Day => Some(ChronoDuration::hours(24)),
            Self::Week => Some(ChronoDuration::days(7)),
        }
    }
}

// 3. 패널 데이터 --------------------------------------------------------------
// 집계가 실패해도 화면은 유지
#[derive(Debug, Clone, Default)]
pub enum Panel<T> {
    #[default]
    Pending,
    Ready(T),
    Failed(Failure),
}

#[derive(Debug, Clone, Default)]
pub enum DbState {
    #[default]
    Pending,
    Missing,
    Open,
    Failed(Failure),
}

// 4. 수집 스냅샷 --------------------------------------------------------------
#[derive(Debug, Clone, Default)]
pub struct DashboardSnapshot {
    pub window: WindowKind,
    pub taken_at: Option<DateTime<Utc>>,
    pub db_state: DbState,
    pub status: Panel<StatusSnapshot>,
    // serve 가 이 TUI 실행 파일과 다른 빌드로 상주 중 (빌드 식별자 불일치)
    pub serve_stale: bool,
    pub overview: Panel<OverviewStats>,
    pub today: Panel<OverviewStats>,
    pub tools: Panel<Vec<ToolLatencyRow>>,
    pub hooks: Panel<Vec<HookStatRow>>,
    pub model_stats: Panel<Vec<ModelStatsRow>>,
    pub agent_stats: Panel<Vec<AgentStatsRow>>,
    pub cache_breaks: Panel<Vec<CacheBreakRow>>,
    pub minutes: Panel<Vec<MinuteBucket>>,
    pub events: Panel<Vec<LiveEvent>>,
    pub projects: Panel<Vec<ProjectStatsRow>>,
    // Skill 도구 호출 통계 (설치 목록과의 병합은 화면에서)
    pub skills: Panel<Vec<SkillStatsRow>>,
    // 차원별 귀속 행 (ATTRIBUTION_DIMENSIONS 순서)
    pub attribution: Panel<Vec<(AttributionDimension, Vec<MetricAttributionRow>)>>,
    pub limits: Panel<LimitsData>,
}

// 한도·캐시 카드 입력 (latest = 공식 최신 스냅샷, history = 소진 예상용 최근 스냅샷 오래된 순,
// api_key = 구간 안 최신 레이트리밋 헤더, compaction = 최신 세션의 컴팩션, estimate_* = 로컬 토큰 집계)
#[derive(Debug, Clone, Default)]
pub struct LimitsData {
    pub latest: Option<LimitSnapshot>,
    pub history: Vec<LimitSnapshot>,
    pub api_key: Option<RatelimitSnapshotRow>,
    pub compaction: Option<CompactionStat>,
    pub estimate_5h: Option<OverviewStats>,
    pub estimate_7d: Option<OverviewStats>,
}

// 5. 알림 ---------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub level: ToastLevel,
    pub expires_at: Instant,
}

// 6. 배경 스레드 → UI 메시지 --------------------------------------------------
#[derive(Debug)]
pub enum UiMessage {
    Snapshot(Box<DashboardSnapshot>),
    Settings {
        inventory: Box<Panel<Inventory>>,
        doctor: Panel<Vec<DoctorFinding>>,
    },
    Report {
        date: NaiveDate,
        body: Panel<Option<String>>,
    },
    // 관리 요청 결과 (알림 문구는 화면 언어로 UI 스레드에서 만듦)
    Admin {
        action: AdminAction,
        outcome: AdminOutcome,
    },
    // 상태 파일 저장 스레드의 실패 (detail = 번역하지 않는 오류 원문, 알림 문구는 UI 스레드가 만듦)
    StateSaveFailed {
        detail: String,
    },
    // 주간 리포트 본문 (week_start = 해당 주의 월요일)
    WeeklyReport {
        week_start: NaiveDate,
        body: Panel<Option<String>>,
    },
    // 수집 스레드의 DB 스키마 갱신 실패 (detail = 번역하지 않는 오류 원문, 알림 문구는 UI 스레드가 만듦)
    SchemaUpgradeFailed {
        detail: String,
    },
}

// 7. 입력 → 실행 요청 ---------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCommand {
    None,
    RefreshNow,
    SetWindow(WindowKind),
    LoadSettings,
    // 집계 갱신과 설치 목록 재적재를 함께 (스킬 탭 r 키)
    RefreshWithSettings,
    LoadReport(NaiveDate),
    GenerateReport(NaiveDate),
    Reindex,
    SetTopmost(bool),
    // 카드 배치(순서·경계·열 폭)나 표시 언어가 확정돼 상태 파일 저장 필요 (쓰기는 저장 스레드가 함)
    SaveState,
    // 주간 보기: 해당 주의 월요일로 본문 적재·생성 요청
    LoadWeeklyReport(NaiveDate),
    GenerateWeeklyReport(NaiveDate),
}

// 8. 화면 상태 ----------------------------------------------------------------
pub struct AppState {
    pub config: AppConfig,
    // 상태 파일(tui.toml) 경로, 시작할 때 한 번 정함 (new 는 데이터 폴더 기준, 실행 진입점이 홈 공용 폴더로 교체)
    pub state_file: PathBuf,
    pub lang: Lang,
    pub today: NaiveDate,
    pub tab: ActiveTab,
    // 탭 바 표시 순서 (번호 키·Tab 순환이 따름, tui.toml 로 저장·복원), 탭을 눌러 끄는 중
    pub tab_order: [ActiveTab; TAB_COUNT],
    pub tab_drag: Option<TabDrag>,
    pub window: WindowKind,
    pub snapshot: DashboardSnapshot,
    // 카드별 정렬 (정렬 가능한 표 카드만 의미 있음)
    pub card_sorts: [SortState; CARD_COUNT],
    // 카드별 스크롤·선택 (탭을 오가도 유지)
    pub card_views: [CardView; CARD_COUNT],
    // 카드 경계 비율·순서 (tui.toml 로 저장·복원), 드래그 중·마우스 아래 경계
    pub card_layout: CardLayout,
    pub split_drag: Option<SplitDrag>,
    // 카드 제목을 누른 채 대기 중, 길게 눌러 진입한 카드 이동 중
    pub card_press: Option<CardPress>,
    pub card_move: Option<CardMove>,
    pub split_hover: Option<(SplitGroup, usize)>,
    // 카드별 끌어 정한 열 폭 (None 이면 자동 폭, tui.toml 로 저장·복원), 열 경계 드래그 중·마우스 아래 경계
    // last_column_click = 끌지 않고 뗀 직전 경계 클릭 (더블클릭 판정용)
    pub column_widths: [[Option<u16>; MAX_TABLE_COLUMNS]; CARD_COUNT],
    pub column_drag: Option<ColumnDrag>,
    pub column_hover: Option<(CardId, usize)>,
    pub last_column_click: Option<(CardId, usize, Instant)>,
    // 스크롤바 썸 드래그 중, 마우스가 트랙 위에 있는 카드
    pub scroll_drag: Option<ScrollDrag>,
    pub scroll_hover: Option<CardId>,
    // 다른 실행 요청에 밀려 아직 보내지 못한 상태 파일 저장 (이벤트 루프가 이벤트마다 확인해 보냄)
    pub save_pending: bool,
    // 클릭한 카드(선택 당시 탭에서만 유효), 마우스 아래 카드
    pub focused_card: Option<(ActiveTab, CardId)>,
    pub hover_card: Option<CardId>,
    pub hit_areas: UiHitAreas,
    pub always_on_top: bool,
    pub show_help: bool,
    pub toast: Option<Toast>,
    // 다른 빌드 serve 알림을 이미 띄운 serve pid (pid 마다 한 번)
    pub stale_serve_warned_pid: Option<u32>,
    pub report_date: NaiveDate,
    pub report: Panel<Option<String>>,
    // 리포트 본문 줄별 화면 시작 행 (줄바꿈 반영, 직전 그리기 기준)
    pub report_row_starts: Vec<usize>,
    pub inventory: Panel<Inventory>,
    pub doctor: Panel<Vec<DoctorFinding>>,
    pub settings_requested: bool,
    pub should_quit: bool,
    // 강제 재시작으로 끝냄 (터미널 복원 뒤 main 이 serve·TUI 를 최신 exe 로 재기동)
    pub restart_requested: bool,
    // 프로젝트 탭 귀속 표가 보이는 차원 (d 키로 순환)
    pub attribution_dimension: AttributionDimension,
    // 리포트 탭 주간 보기 (기본은 일간), 선택한 주의 월요일 (기본은 이번 주)
    pub report_weekly: bool,
    pub report_week: NaiveDate,
    // 테스트가 지금 시각을 고정 (None 이면 시스템 시각)
    pub clock_override: Option<DateTime<Utc>>,
}

impl AppState {
    // 8-1. 초기 상태 (리포트 날짜 기본값 = 오늘, 주간 보기 기본 주 = 이번 주)
    pub fn new(config: AppConfig, today: NaiveDate) -> Self {
        Self {
            state_file: config.data_dir.join(TUI_STATE_FILE_NAME),
            config,
            lang: Lang::default(),
            today,
            tab: ActiveTab::Overview,
            tab_order: ActiveTab::ALL,
            tab_drag: None,
            window: WindowKind::default(),
            snapshot: DashboardSnapshot::default(),
            card_sorts: default_card_sorts(),
            card_views: [CardView::default(); CARD_COUNT],
            card_layout: CardLayout::default(),
            split_drag: None,
            card_press: None,
            card_move: None,
            split_hover: None,
            column_widths: [[None; MAX_TABLE_COLUMNS]; CARD_COUNT],
            column_drag: None,
            column_hover: None,
            last_column_click: None,
            scroll_drag: None,
            scroll_hover: None,
            save_pending: false,
            focused_card: None,
            hover_card: None,
            hit_areas: UiHitAreas::default(),
            always_on_top: false,
            show_help: false,
            toast: None,
            stale_serve_warned_pid: None,
            report_date: today,
            report: Panel::Pending,
            report_row_starts: Vec::new(),
            inventory: Panel::Pending,
            doctor: Panel::Pending,
            settings_requested: false,
            should_quit: false,
            restart_requested: false,
            attribution_dimension: ATTRIBUTION_DIMENSIONS[0],
            report_weekly: false,
            report_week: week_start_of(today),
            clock_override: None,
        }
    }

    // 8-2. 탭 전환 (설정·에이전트·스킬은 첫 진입 시 설치 목록 적재, 리포트는 진입마다 적재 요청, 끌어 움직인 경계·열 폭은 버리지 않고 저장)
    pub fn switch_tab(&mut self, tab: ActiveTab) -> UiCommand {
        self.cancel_card_move();
        let saved = self.end_drags(None);
        self.tab = tab;
        self.hover_card = None;
        self.split_hover = None;
        self.column_hover = None;
        self.scroll_hover = None;
        let command = match tab {
            ActiveTab::Settings | ActiveTab::Agents | ActiveTab::Skills
                if !self.settings_requested =>
            {
                self.settings_requested = true;
                UiCommand::LoadSettings
            }
            ActiveTab::Report if self.report_weekly => {
                UiCommand::LoadWeeklyReport(self.report_week)
            }
            ActiveTab::Report => UiCommand::LoadReport(self.report_date),
            _ => UiCommand::None,
        };
        self.merge_save(saved, command)
    }

    // 8-2-1. 오늘 갱신: 시계가 넘어간 날로 today 를 옮기고, "오늘"·"이번 주"를 보던 리포트는 새 날·새 주로 따라감
    // 반환 = 오늘이 바뀔을 때만 Some (리포트 탭이 보는 대상이 바뀔으면 그 본문 적재 명령, 아니면 UiCommand::None)
    // 지금 시각은 now_utc() 라 테스트가 clock_override 로 날짜를 고정할 수 있음
    pub fn refresh_today(&mut self) -> Option<UiCommand> {
        let today = self.now_utc().with_timezone(&Local).date_naive();
        if today == self.today {
            return None;
        }

        // 1. 오늘을 보던 날짜·이번 주를 보던 주는 새 값으로 (지난 날·지난 주를 보던 중이면 그대로)
        let previous = self.today;
        let was_today = self.report_date == previous;
        let was_week = self.report_week == week_start_of(previous);
        self.today = today;
        if was_today {
            self.report_date = today;
        }
        if was_week {
            self.report_week = week_start_of(today);
        }

        // 2. 리포트 탭이 보는 대상이 옮겨갔을 때만 본문 다시 적재 (오늘이 바뀌면 주는 월요일에만 바뀜)
        let moved = if self.report_weekly {
            was_week && week_start_of(today) != week_start_of(previous)
        } else {
            was_today
        };
        if self.tab != ActiveTab::Report || !moved {
            return Some(UiCommand::None);
        }
        self.reset_report_view();
        Some(if self.report_weekly {
            UiCommand::LoadWeeklyReport(self.report_week)
        } else {
            UiCommand::LoadReport(self.report_date)
        })
    }

    // 8-3. 키 입력 대상 카드 (클릭한 카드가 이 탭 것이면 그 카드, 아니면 탭의 주 카드)
    pub fn focused_card_id(&self) -> CardId {
        match self.focused_card {
            Some((tab, card)) if tab == self.tab => card,
            _ => self.tab.main_card(),
        }
    }

    // 8-4. 알림 띄우기
    pub fn show_toast(&mut self, text: &str, level: ToastLevel) {
        self.toast = Some(Toast {
            text: text.to_string(),
            level,
            expires_at: Instant::now() + TOAST_LIFETIME,
        });
    }

    // 8-5. 키 입력 상태 전이
    pub fn handle_key_event(&mut self, key: KeyEvent) -> UiCommand {
        // 1. Ctrl+C 종료
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return UiCommand::None;
        }

        // 2. 도움말은 아무 키로 닫기
        if self.show_help {
            self.show_help = false;
            return UiCommand::None;
        }

        // 3. 카드·탭 이동 중·길게 누름 대기 중에는 Esc 취소만 받음 (순서를 바꾸지 않은 탭 누름은 끝내고 키 처리)
        if let Some(drag) = self.tab_drag
            && drag.baseline == self.tab_order
        {
            self.tab_drag = None;
        }
        if self.card_move.is_some() || self.card_press.is_some() || self.tab_drag.is_some() {
            if key.code == KeyCode::Esc {
                self.cancel_card_move();
                self.cancel_tab_drag();
            }
            return UiCommand::None;
        }

        // 4. 키별 전이 (번호 키·Tab 순환은 탭 바 표시 순서를 따름)
        let slot = self
            .tab_order
            .iter()
            .position(|tab| *tab == self.tab)
            .unwrap_or(0);
        let card = self.focused_card_id();
        match key.code {
            KeyCode::Char('q' | 'Q') | KeyCode::Esc => self.perform_action(UiAction::Quit),
            KeyCode::Char('?') => self.perform_action(UiAction::ShowHelp),
            KeyCode::Char('!') => self.perform_action(UiAction::ForceRestart),
            KeyCode::BackTab => self.switch_tab(self.tab_order[(slot + TAB_COUNT - 1) % TAB_COUNT]),
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.switch_tab(self.tab_order[(slot + TAB_COUNT - 1) % TAB_COUNT])
            }
            KeyCode::Tab => self.switch_tab(self.tab_order[(slot + 1) % TAB_COUNT]),
            KeyCode::Char(digit @ '1'..='9') => {
                self.switch_tab(self.tab_order[digit as usize - '1' as usize])
            }
            KeyCode::Char('<') => self.move_current_tab(-1),
            KeyCode::Char('>') => self.move_current_tab(1),
            // Shift+방향키는 포커스 카드 위치 이동 (리포트 날짜 이동 등 일반 방향키보다 먼저)
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right
                if key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.shift_focused_card(card, key.code)
            }
            KeyCode::Up => self.step_focused_card(card, CursorMotion::Lines(-1)),
            KeyCode::Down => self.step_focused_card(card, CursorMotion::Lines(1)),
            KeyCode::PageUp => self.step_focused_card(card, CursorMotion::Pages(-1)),
            KeyCode::PageDown => self.step_focused_card(card, CursorMotion::Pages(1)),
            KeyCode::Home => self.step_focused_card(card, CursorMotion::Home),
            KeyCode::End => self.step_focused_card(card, CursorMotion::End),
            KeyCode::Left if self.tab == ActiveTab::Report => {
                self.perform_action(UiAction::ReportPrev)
            }
            KeyCode::Right if self.tab == ActiveTab::Report => {
                self.perform_action(UiAction::ReportNext)
            }
            KeyCode::Char('w' | 'W') => self.perform_action(UiAction::CycleWindow),
            KeyCode::Char(letter @ ('s' | 'S')) => {
                // 포커스 카드가 정렬 표가 아니면 탭의 주 카드
                let target = if sort_spec(card).is_some() {
                    Some(card)
                } else {
                    Some(self.tab.main_card()).filter(|main| sort_spec(*main).is_some())
                };
                let change = if letter == 's' {
                    SortChange::NextColumn
                } else {
                    SortChange::ReverseDirection
                };
                if let Some(target_card) = target {
                    self.change_card_sort(target_card, change);
                }
                UiCommand::None
            }
            KeyCode::Char('r' | 'R') => self.perform_action(UiAction::RefreshView),
            KeyCode::Char('g' | 'G') if self.tab == ActiveTab::Report => {
                self.perform_action(UiAction::ReportGenerate)
            }
            KeyCode::Char('i' | 'I') => self.perform_action(UiAction::Reindex),
            KeyCode::Char('t' | 'T') => self.perform_action(UiAction::TogglePin),
            KeyCode::Char('l') => self.perform_action(UiAction::ResetLayout),
            KeyCode::Char('L') => self.perform_action(UiAction::ToggleLanguage),
            KeyCode::Char('d' | 'D') if self.tab == ActiveTab::Projects => {
                self.cycle_attribution_dimension();
                UiCommand::SaveState
            }
            KeyCode::Char('v' | 'V') if self.tab == ActiveTab::Report => {
                self.perform_action(UiAction::ReportToggleView)
            }
            _ => UiCommand::None,
        }
    }

    // 8-6. 키·버튼 공용 동작
    pub fn perform_action(&mut self, action: UiAction) -> UiCommand {
        match action {
            UiAction::Quit => {
                self.should_quit = true;
                UiCommand::None
            }
            UiAction::ShowHelp => {
                self.show_help = true;
                UiCommand::None
            }
            UiAction::Reindex => UiCommand::Reindex,
            UiAction::CycleWindow => {
                self.window = match self.window {
                    WindowKind::Today => WindowKind::Hour,
                    WindowKind::Hour => WindowKind::Day,
                    WindowKind::Day => WindowKind::Week,
                    WindowKind::Week => WindowKind::Today,
                };
                UiCommand::SetWindow(self.window)
            }
            UiAction::RefreshView => match self.tab {
                ActiveTab::Settings => UiCommand::LoadSettings,
                // 스킬 탭은 집계와 설치 목록을 함께 다시 읽음
                ActiveTab::Skills => UiCommand::RefreshWithSettings,
                ActiveTab::Report if self.report_weekly => {
                    UiCommand::LoadWeeklyReport(self.report_week)
                }
                ActiveTab::Report => UiCommand::LoadReport(self.report_date),
                _ => UiCommand::RefreshNow,
            },
            UiAction::TogglePin => {
                self.always_on_top = !self.always_on_top;
                let toast = if self.always_on_top {
                    Msg::ToastPinned
                } else {
                    Msg::ToastUnpinned
                };
                self.show_toast(i18n::text(self.lang, toast), ToastLevel::Info);
                UiCommand::SetTopmost(self.always_on_top)
            }
            UiAction::ResetLayout => {
                self.card_layout.reset_groups(self.tab.split_groups());
                for card in self.tab.table_cards() {
                    self.column_widths[*card as usize] = [None; MAX_TABLE_COLUMNS];
                }
                self.split_drag = None;
                self.split_hover = None;
                self.column_drag = None;
                self.column_hover = None;
                self.show_toast(
                    i18n::text(self.lang, Msg::ToastLayoutReset),
                    ToastLevel::Info,
                );
                UiCommand::SaveState
            }
            UiAction::ToggleLanguage => {
                self.lang = match self.lang {
                    Lang::En => Lang::Ko,
                    Lang::Ko => Lang::En,
                };
                self.show_toast(i18n::text(self.lang, Msg::ToastLanguage), ToastLevel::Info);
                UiCommand::SaveState
            }
            // 빌드가 같으면 버튼이 없으므로 키도 무시
            UiAction::ForceRestart if self.snapshot.serve_stale => {
                self.restart_requested = true;
                self.should_quit = true;
                UiCommand::None
            }
            UiAction::ForceRestart => UiCommand::None,
            UiAction::ReportPrev => {
                if self.report_weekly {
                    self.report_week -= ChronoDuration::days(7);
                    self.reset_report_view();
                    return UiCommand::LoadWeeklyReport(self.report_week);
                }
                self.report_date = self.report_date.pred_opt().unwrap_or(self.report_date);
                self.reset_report_view();
                UiCommand::LoadReport(self.report_date)
            }
            UiAction::ReportNext => {
                if self.report_weekly {
                    let next_week = self.report_week + ChronoDuration::days(7);
                    if next_week > week_start_of(self.today) {
                        return UiCommand::None;
                    }
                    self.report_week = next_week;
                    self.reset_report_view();
                    return UiCommand::LoadWeeklyReport(self.report_week);
                }
                let next_date = self.report_date.succ_opt().unwrap_or(self.report_date);
                if next_date > self.today {
                    return UiCommand::None;
                }
                self.report_date = next_date;
                self.reset_report_view();
                UiCommand::LoadReport(self.report_date)
            }
            UiAction::ReportGenerate if self.report_weekly => {
                UiCommand::GenerateWeeklyReport(self.report_week)
            }
            UiAction::ReportGenerate => UiCommand::GenerateReport(self.report_date),
            UiAction::ReportToggleView => {
                self.report_weekly = !self.report_weekly;
                self.reset_report_view();
                if self.report_weekly {
                    UiCommand::LoadWeeklyReport(self.report_week)
                } else {
                    UiCommand::LoadReport(self.report_date)
                }
            }
        }
    }

    // 귀속 표 차원 순환 (프로젝트 탭 d 키, 다른 차원의 행이라 선택·스크롤은 처음으로)
    pub fn cycle_attribution_dimension(&mut self) {
        let current = ATTRIBUTION_DIMENSIONS
            .iter()
            .position(|dimension| *dimension == self.attribution_dimension)
            .unwrap_or(0);
        self.attribution_dimension =
            ATTRIBUTION_DIMENSIONS[(current + 1) % ATTRIBUTION_DIMENSIONS.len()];
        self.card_views[CardId::AttributionTable as usize] = CardView::default();
    }

    // 리포트 날짜가 바뀌면 본문·목차 위치 처음으로
    fn reset_report_view(&mut self) {
        self.report = Panel::Pending;
        self.report_row_starts.clear();
        self.card_views[CardId::ReportBody as usize] = CardView::default();
        self.card_views[CardId::ReportOutline as usize] = CardView::default();
    }

    // 8-6-1. 카드 정렬 변경 (머리글 클릭·s·S 공용, 선택 행은 같은 항목을 따라감)
    pub fn change_card_sort(&mut self, card: CardId, change: SortChange) {
        let Some(spec) = sort_spec(card) else {
            return;
        };
        let keys = self.capture_selection_keys();
        self.card_sorts[card as usize].apply_change(change, spec);
        self.restore_selection(keys, false);
        if card.kind() == CardKind::Scrollable {
            self.card_views[card as usize].scroll = 0;
        }
    }

    // 8-7. 키 이동 (선택 카드는 선택 행과 화면 위치, 그 외는 화면 위치만)
    pub fn step_focused_card(&mut self, card: CardId, motion: CursorMotion) -> UiCommand {
        let view = &mut self.card_views[card as usize];
        let page = view.visible.max(1) as isize;
        let last_row = view.total.saturating_sub(1);
        let max_scroll = view.total.saturating_sub(view.visible);
        let selectable = card.kind() == CardKind::Selectable;
        let current = if selectable {
            view.selected
        } else {
            view.scroll
        };
        let target = match motion {
            CursorMotion::Lines(delta) => current.saturating_add_signed(delta),
            CursorMotion::Pages(pages) => current.saturating_add_signed(pages * page),
            CursorMotion::Home => 0,
            CursorMotion::End => usize::MAX,
        };
        if selectable {
            view.selected = target.min(last_row);
            if view.selected < view.scroll {
                view.scroll = view.selected;
            } else if view.visible > 0 && view.selected >= view.scroll + view.visible {
                view.scroll = view.selected + 1 - view.visible;
            }
        } else {
            view.scroll = target.min(max_scroll);
        }
        if card == CardId::ReportOutline {
            self.follow_outline_selection();
        }
        UiCommand::None
    }

    // 8-8. 휠 스크롤 (선택 행은 그대로, 화면 위치만) 반환 = 위치가 바뀌었는지
    pub fn scroll_card_view(&mut self, card: CardId, delta: isize) -> bool {
        if card.kind() == CardKind::Static {
            return false;
        }
        let view = &mut self.card_views[card as usize];
        let max_scroll = view.total.saturating_sub(view.visible);
        let before = view.scroll;
        view.scroll = view.scroll.saturating_add_signed(delta).min(max_scroll);
        before != view.scroll
    }

    // 썸 드래그용 화면 위치 지정 (고정 카드 무시)
    pub fn set_card_scroll(&mut self, card: CardId, scroll: usize) {
        if card.kind() == CardKind::Static {
            return;
        }
        let view = &mut self.card_views[card as usize];
        view.scroll = scroll.min(view.total.saturating_sub(view.visible));
    }

    // 8-9. 표 행 클릭 선택, 리포트 목차는 본문 위치까지 이동
    pub fn select_card_row(&mut self, card: CardId, index: usize) {
        let view = &mut self.card_views[card as usize];
        view.selected = index.min(view.total.saturating_sub(1));
        if card == CardId::ReportOutline {
            self.follow_outline_selection();
        }
    }

    // 선택한 목차 제목 줄로 리포트 본문 이동
    fn follow_outline_selection(&mut self) {
        let Panel::Ready(Some(markdown)) = &self.report else {
            return;
        };
        let entries = outline_entries(&prepare_report_markdown(markdown));
        let selected = self.card_views[CardId::ReportOutline as usize].selected;
        if let Some(entry) = entries.get(selected) {
            let row = self
                .report_row_starts
                .get(entry.line)
                .copied()
                .unwrap_or(entry.line);
            self.card_views[CardId::ReportBody as usize].scroll = row;
        }
    }

    // 8-10. 배경 스레드 메시지 반영
    pub fn apply_ui_message(&mut self, message: UiMessage, now: Instant) {
        match message {
            UiMessage::Snapshot(snapshot) => {
                // 다른 빌드 serve 는 pid 마다 한 번 알림 (구간과 무관)
                if let Panel::Ready(status) = &snapshot.status
                    && snapshot.serve_stale
                    && self.stale_serve_warned_pid != Some(status.pid)
                {
                    self.stale_serve_warned_pid = Some(status.pid);
                    self.toast = Some(Toast {
                        text: i18n::serve_stale_build(self.lang, status.pid),
                        level: ToastLevel::Error,
                        expires_at: now + TOAST_LIFETIME,
                    });
                }
                // 구간을 바꾼 뒤 늦게 온 이전 구간 스냅샷은 버림
                if snapshot.window == self.window {
                    let keys = self.capture_selection_keys();
                    self.snapshot = *snapshot;
                    self.restore_selection(keys, true);
                }
            }
            UiMessage::Settings { inventory, doctor } => {
                self.inventory = *inventory;
                self.doctor = doctor;
            }
            UiMessage::Report { date, body } => {
                if date == self.report_date && !self.report_weekly {
                    self.report = body;
                }
            }
            UiMessage::WeeklyReport { week_start, body } => {
                if week_start == self.report_week && self.report_weekly {
                    self.report = body;
                }
            }
            UiMessage::Admin { action, outcome } => {
                let (text, level) = i18n::admin_toast(self.lang, action, &outcome);
                self.toast = Some(Toast {
                    text,
                    level,
                    expires_at: now + TOAST_LIFETIME,
                });
            }
            UiMessage::StateSaveFailed { detail } => {
                self.toast = Some(Toast {
                    text: i18n::state_save_failed(self.lang, &detail),
                    level: ToastLevel::Error,
                    expires_at: now + TOAST_LIFETIME,
                });
            }
            UiMessage::SchemaUpgradeFailed { detail } => {
                self.toast = Some(Toast {
                    text: i18n::schema_upgrade_failed(self.lang, &detail),
                    level: ToastLevel::Error,
                    expires_at: now + TOAST_LIFETIME,
                });
            }
        }
    }

    // 8-11. 카드별 화면 순서 행 키 (현재 정렬 기준, 준비 전 패널은 None)
    fn card_row_keys(&self, card: CardId) -> Option<Vec<String>> {
        let sort = self.card_sorts[card as usize];
        match card {
            CardId::EventFeed => {
                let Panel::Ready(events) = &self.snapshot.events else {
                    return None;
                };
                let mut sorted = events.clone();
                sort_rows(&mut sorted, sort, compare_event_row);
                Some(sorted.iter().map(live_event_key).collect())
            }
            CardId::ToolTable => {
                let Panel::Ready(tools) = &self.snapshot.tools else {
                    return None;
                };
                let mut sorted = tools.clone();
                sort_rows(&mut sorted, sort, compare_tool_row);
                Some(sorted.iter().map(|row| row.tool_name.clone()).collect())
            }
            CardId::HookTable => {
                let Panel::Ready(hooks) = &self.snapshot.hooks else {
                    return None;
                };
                let mut sorted = hooks.clone();
                sort_rows(&mut sorted, sort, compare_hook_row);
                Some(
                    sorted
                        .iter()
                        .map(|row| {
                            [
                                row.route.as_str(),
                                row.guard.as_str(),
                                row.outcome.as_str(),
                                row.detail.as_str(),
                            ]
                            .join("\u{1}")
                        })
                        .collect(),
                )
            }
            CardId::ModelTable => {
                let Panel::Ready(models) = &self.snapshot.model_stats else {
                    return None;
                };
                let mut sorted = models.clone();
                sort_rows(&mut sorted, sort, compare_model_row);
                Some(sorted.iter().map(|row| row.model.clone()).collect())
            }
            CardId::AgentTable => {
                let mut sorted = agent_rows(&self.snapshot.agent_stats, &self.inventory)?;
                sort_rows(&mut sorted, sort, compare_agent_row);
                Some(sorted.into_iter().map(|row| row.agent).collect())
            }
            CardId::CacheBreaks => {
                let Panel::Ready(breaks) = &self.snapshot.cache_breaks else {
                    return None;
                };
                let mut sorted = breaks.clone();
                sort_rows(&mut sorted, sort, compare_cache_break_row);
                Some(
                    sorted
                        .iter()
                        .map(|row| {
                            [row.ts.as_str(), row.session_id.as_str(), row.model.as_str()]
                                .join("\u{1}")
                        })
                        .collect(),
                )
            }
            CardId::ProjectTable => {
                let Panel::Ready(projects) = &self.snapshot.projects else {
                    return None;
                };
                let mut sorted = projects.clone();
                sort_rows(&mut sorted, sort, compare_project_row);
                Some(
                    sorted
                        .iter()
                        .map(|row| [row.project.as_str(), row.git_branch.as_str()].join("\u{1}"))
                        .collect(),
                )
            }
            CardId::AttributionTable => {
                let rows = attribution_rows(&self.snapshot, self.attribution_dimension)?;
                let mut sorted = rows.to_vec();
                sort_rows(&mut sorted, sort, compare_attribution_row);
                Some(sorted.iter().map(|row| row.name.clone()).collect())
            }
            CardId::SkillTable => {
                let mut sorted = skill_rows(&self.snapshot, &self.inventory)?;
                sort_rows(&mut sorted, sort, compare_skill_row);
                Some(sorted.into_iter().map(|row| row.name).collect())
            }
            _ => None,
        }
    }

    // 8-12. 스냅샷 교체·정렬 변경 전 선택 항목 키
    fn capture_selection_keys(&self) -> Vec<(CardId, Option<String>)> {
        SELECTABLE_SORT_CARDS
            .iter()
            .map(|card| {
                let selected = self.card_views[*card as usize].selected;
                let key = self
                    .card_row_keys(*card)
                    .and_then(|keys| keys.get(selected).cloned());
                (*card, key)
            })
            .collect()
    }

    // 8-13. 교체·정렬 뒤 같은 항목으로 선택·화면 위치 이동 (못 찾으면 유지)
    // follow_latest = 피드가 맨 위에 있으면 최신을 따라가는 스냅샷 교체 규칙 적용 여부
    fn restore_selection(&mut self, keys: Vec<(CardId, Option<String>)>, follow_latest: bool) {
        for (card, key) in keys {
            let Some(key) = key else {
                continue;
            };
            let view = self.card_views[card as usize];
            if follow_latest && card == CardId::EventFeed && view.selected == 0 && view.scroll == 0
            {
                continue;
            }
            let Some(row_keys) = self.card_row_keys(card) else {
                continue;
            };
            let Some(new_index) = row_keys.iter().position(|row_key| *row_key == key) else {
                continue;
            };
            let view = &mut self.card_views[card as usize];
            let shift = new_index as isize - view.selected as isize;
            view.scroll = view.scroll.saturating_add_signed(shift);
            view.selected = new_index;
        }
    }
}

// 8-n. 지금 시각 (카운트다운이 다시 그릴 때마다 흐르도록 그리기에서 읽음)
impl AppState {
    pub fn now_utc(&self) -> DateTime<Utc> {
        self.clock_override.unwrap_or_else(Utc::now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::cards::SlotGroupHit;
    use crate::tui::test_support::sample_config;
    use chrono::TimeZone;
    use crossterm::event::KeyEventKind;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    // 기존 한국어 문구 단언을 위해 한국어로 고정
    fn sample_state() -> AppState {
        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let mut state = AppState::new(sample_config(), today);
        state.lang = Lang::Ko;
        state
    }

    #[test]
    fn digit_and_tab_keys_switch_tabs_with_wraparound() {
        let mut state = sample_state();
        assert_eq!(state.tab, ActiveTab::Overview);

        state.handle_key_event(press(KeyCode::Char('3')));
        assert_eq!(state.tab, ActiveTab::Hooks);

        state.handle_key_event(press(KeyCode::Tab));
        assert_eq!(state.tab, ActiveTab::Models);

        state.handle_key_event(press(KeyCode::BackTab));
        state.handle_key_event(press(KeyCode::BackTab));
        state.handle_key_event(press(KeyCode::BackTab));
        assert_eq!(state.tab, ActiveTab::Overview);
        state.handle_key_event(press(KeyCode::BackTab));
        assert_eq!(state.tab, ActiveTab::Skills);

        state.handle_key_event(press(KeyCode::Tab));
        assert_eq!(state.tab, ActiveTab::Overview);

        let shift_tab =
            KeyEvent::new_with_kind(KeyCode::Tab, KeyModifiers::SHIFT, KeyEventKind::Press);
        state.handle_key_event(shift_tab);
        assert_eq!(state.tab, ActiveTab::Skills);
    }

    #[test]
    fn settings_tab_requests_load_once_and_report_tab_every_time() {
        let mut state = sample_state();
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('6'))),
            UiCommand::LoadSettings
        );
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('5'))),
            UiCommand::None
        );
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('6'))),
            UiCommand::None
        );
        // r 은 설정 탭에서 재적재
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('r'))),
            UiCommand::LoadSettings
        );

        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('7'))),
            UiCommand::LoadReport(today)
        );
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('g'))),
            UiCommand::GenerateReport(today)
        );
    }

    #[test]
    fn report_date_starts_today_moves_with_arrows_and_stops_at_today() {
        let mut state = sample_state();
        state.handle_key_event(press(KeyCode::Char('7')));
        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let yesterday = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        assert_eq!(state.report_date, today);

        // 오늘 이후로는 이동 없음
        assert_eq!(
            state.handle_key_event(press(KeyCode::Right)),
            UiCommand::None
        );
        assert_eq!(state.report_date, today);

        assert_eq!(
            state.handle_key_event(press(KeyCode::Left)),
            UiCommand::LoadReport(yesterday)
        );
        assert!(matches!(state.report, Panel::Pending));
        assert_eq!(
            state.handle_key_event(press(KeyCode::Right)),
            UiCommand::LoadReport(today)
        );
        assert_eq!(state.report_date, today);
    }

    #[test]
    fn v_key_switches_to_the_current_week_and_arrows_move_by_week() {
        let mut state = sample_state();
        // today 2026-11-15 (일요일) -> 이번 주는 11-09 월요일부터
        let this_week = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let last_week = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        assert!(!state.report_weekly);
        assert_eq!(state.report_week, this_week);

        // v 는 리포트 탭에서만, 일간이 기본이고 일간 날짜는 그대로
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('v'))),
            UiCommand::None
        );
        assert!(!state.report_weekly);
        state.handle_key_event(press(KeyCode::Char('7')));
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('v'))),
            UiCommand::LoadWeeklyReport(this_week)
        );
        assert!(state.report_weekly);
        assert!(matches!(state.report, Panel::Pending));
        assert_eq!(state.report_date, today);

        // 이번 주를 넘어 다음 주로 가지 않고, 지난 주로는 번지수로 이동
        assert_eq!(
            state.handle_key_event(press(KeyCode::Right)),
            UiCommand::None
        );
        assert_eq!(
            state.handle_key_event(press(KeyCode::Left)),
            UiCommand::LoadWeeklyReport(last_week)
        );
        assert_eq!(state.report_week, last_week);
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('g'))),
            UiCommand::GenerateWeeklyReport(last_week)
        );
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('r'))),
            UiCommand::LoadWeeklyReport(last_week)
        );
        assert_eq!(
            state.handle_key_event(press(KeyCode::Right)),
            UiCommand::LoadWeeklyReport(this_week)
        );

        // 다른 탭을 거쳐 돌아와도 주간 보기 유지, 일간으로 되돌리면 오늘 리포트
        state.handle_key_event(press(KeyCode::Char('1')));
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('7'))),
            UiCommand::LoadWeeklyReport(this_week)
        );
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('v'))),
            UiCommand::LoadReport(today)
        );
        assert!(!state.report_weekly);
    }

    #[test]
    fn report_messages_apply_only_to_the_view_and_period_on_screen() {
        let mut state = sample_state();
        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let this_week = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let now = Instant::now();

        // 일간 보기: 주간 본문은 버림
        state.apply_ui_message(
            UiMessage::WeeklyReport {
                week_start: this_week,
                body: Panel::Ready(Some("weekly".to_string())),
            },
            now,
        );
        assert!(matches!(state.report, Panel::Pending));
        state.apply_ui_message(
            UiMessage::Report {
                date: today,
                body: Panel::Ready(Some("daily".to_string())),
            },
            now,
        );
        assert!(matches!(&state.report, Panel::Ready(Some(text)) if text == "daily"));

        // 주간 보기: 일간 본문과 다른 주 본문은 버림
        state.perform_action(UiAction::ReportToggleView);
        state.apply_ui_message(
            UiMessage::Report {
                date: today,
                body: Panel::Ready(Some("late daily".to_string())),
            },
            now,
        );
        state.apply_ui_message(
            UiMessage::WeeklyReport {
                week_start: NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
                body: Panel::Ready(Some("other week".to_string())),
            },
            now,
        );
        assert!(matches!(state.report, Panel::Pending));
        state.apply_ui_message(
            UiMessage::WeeklyReport {
                week_start: this_week,
                body: Panel::Ready(Some("weekly".to_string())),
            },
            now,
        );
        assert!(matches!(&state.report, Panel::Ready(Some(text)) if text == "weekly"));
    }

    #[test]
    fn window_key_cycles_and_stale_snapshot_is_dropped() {
        let mut state = sample_state();
        assert_eq!(state.window, WindowKind::Today);
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('w'))),
            UiCommand::SetWindow(WindowKind::Hour)
        );
        state.handle_key_event(press(KeyCode::Char('w')));
        assert_eq!(state.window, WindowKind::Day);
        state.handle_key_event(press(KeyCode::Char('w')));
        assert_eq!(state.window, WindowKind::Week);
        state.handle_key_event(press(KeyCode::Char('w')));
        assert_eq!(state.window, WindowKind::Today);

        state.window = WindowKind::Day;
        let stale = DashboardSnapshot {
            window: WindowKind::Hour,
            db_state: DbState::Open,
            ..DashboardSnapshot::default()
        };
        state.apply_ui_message(UiMessage::Snapshot(Box::new(stale)), Instant::now());
        assert!(matches!(state.snapshot.db_state, DbState::Pending));

        let fresh = DashboardSnapshot {
            window: WindowKind::Day,
            db_state: DbState::Open,
            ..DashboardSnapshot::default()
        };
        state.apply_ui_message(UiMessage::Snapshot(Box::new(fresh)), Instant::now());
        assert!(matches!(state.snapshot.db_state, DbState::Open));
    }

    #[test]
    fn selectable_card_keys_move_selection_and_keep_it_in_view() {
        let mut state = sample_state();
        let feed = CardId::EventFeed as usize;
        state.card_views[feed].total = 50;
        state.card_views[feed].visible = 10;

        state.handle_key_event(press(KeyCode::Up));
        assert_eq!(state.card_views[feed].selected, 0);

        state.handle_key_event(press(KeyCode::Down));
        state.handle_key_event(press(KeyCode::Down));
        assert_eq!(state.card_views[feed].selected, 2);
        assert_eq!(state.card_views[feed].scroll, 0);

        // 한 화면 아래로: 선택이 화면 밖으로 나가면 화면이 따라감
        state.handle_key_event(press(KeyCode::PageDown));
        assert_eq!(state.card_views[feed].selected, 12);
        assert_eq!(state.card_views[feed].scroll, 3);
        state.handle_key_event(press(KeyCode::PageUp));
        assert_eq!(state.card_views[feed].selected, 2);
        assert_eq!(state.card_views[feed].scroll, 2);

        state.handle_key_event(press(KeyCode::End));
        assert_eq!(state.card_views[feed].selected, 49);
        assert_eq!(state.card_views[feed].scroll, 40);
        state.handle_key_event(press(KeyCode::Down));
        assert_eq!(state.card_views[feed].selected, 49);
        state.handle_key_event(press(KeyCode::Home));
        assert_eq!(state.card_views[feed].selected, 0);
        assert_eq!(state.card_views[feed].scroll, 0);

        // 카드마다 따로 보관
        state.handle_key_event(press(KeyCode::Char('2')));
        assert_eq!(state.card_views[CardId::ToolTable as usize].selected, 0);
    }

    #[test]
    fn scrollable_card_keys_and_wheel_move_view_only_within_range() {
        let mut state = sample_state();
        state.handle_key_event(press(KeyCode::Char('6')));
        let hooks = CardId::SettingsHooks as usize;
        state.card_views[hooks].total = 30;
        state.card_views[hooks].visible = 10;

        state.handle_key_event(press(KeyCode::Down));
        assert_eq!(state.card_views[hooks].scroll, 1);
        state.handle_key_event(press(KeyCode::End));
        assert_eq!(state.card_views[hooks].scroll, 20);
        state.handle_key_event(press(KeyCode::Down));
        assert_eq!(state.card_views[hooks].scroll, 20);

        assert!(state.scroll_card_view(CardId::SettingsHooks, -5));
        assert_eq!(state.card_views[hooks].scroll, 15);
        assert!(state.scroll_card_view(CardId::SettingsHooks, 100));
        assert_eq!(state.card_views[hooks].scroll, 20);
        assert!(!state.scroll_card_view(CardId::SettingsHooks, 1));
        // 고정 카드는 휠을 받지 않음
        assert!(!state.scroll_card_view(CardId::KpiTools, 3));
    }

    #[test]
    fn focus_follows_clicked_card_only_on_its_tab() {
        let mut state = sample_state();
        assert_eq!(state.focused_card_id(), CardId::EventFeed);

        state.focused_card = Some((ActiveTab::Overview, CardId::KpiCache));
        assert_eq!(state.focused_card_id(), CardId::KpiCache);

        state.switch_tab(ActiveTab::Tools);
        assert_eq!(state.focused_card_id(), CardId::ToolTable);
    }

    #[test]
    fn sort_keys_act_on_the_focused_sortable_card_or_the_tab_main_card() {
        let mut state = sample_state();
        let feed = CardId::EventFeed as usize;
        let tools = CardId::ToolTable as usize;

        // 개요 탭: 포커스가 없으면 주 카드(이벤트 피드)의 시각 내림차순에서 시작
        assert_eq!(
            (
                state.card_sorts[feed].column,
                state.card_sorts[feed].descending
            ),
            (0, true)
        );
        state.handle_key_event(press(KeyCode::Char('s')));
        assert_eq!(
            (
                state.card_sorts[feed].column,
                state.card_sorts[feed].descending
            ),
            (1, false)
        );
        assert_eq!(state.card_sorts[tools].column, 5);

        // 도구 탭: 기본은 합계 내림차순, s 는 다음 열, S 는 방향 반전
        state.handle_key_event(press(KeyCode::Char('2')));
        assert_eq!(
            (
                state.card_sorts[tools].column,
                state.card_sorts[tools].descending
            ),
            (5, true)
        );
        state.handle_key_event(press(KeyCode::Char('s')));
        assert_eq!(
            (
                state.card_sorts[tools].column,
                state.card_sorts[tools].descending
            ),
            (0, false)
        );
        state.handle_key_event(press(KeyCode::Char('S')));
        assert!(state.card_sorts[tools].descending);

        // 정렬 없는 카드가 포커스여도 탭의 주 카드를 정렬
        state.focused_card = Some((ActiveTab::Tools, CardId::ToolDetail));
        state.handle_key_event(press(KeyCode::Char('s')));
        assert_eq!(state.card_sorts[tools].column, 1);

        // 정렬 표가 없는 탭은 무시
        state.handle_key_event(press(KeyCode::Char('7')));
        assert_eq!(state.tab, ActiveTab::Report);
        let before = state.card_sorts;
        state.handle_key_event(press(KeyCode::Char('s')));
        assert_eq!(state.card_sorts, before);
    }

    #[test]
    fn seven_tabs_map_to_digits_one_through_seven() {
        let mut state = sample_state();
        let expected = [
            ('1', ActiveTab::Overview),
            ('2', ActiveTab::Tools),
            ('3', ActiveTab::Hooks),
            ('4', ActiveTab::Models),
            ('5', ActiveTab::Agents),
            ('6', ActiveTab::Settings),
            ('7', ActiveTab::Report),
        ];
        for (digit, tab) in expected {
            state.handle_key_event(press(KeyCode::Char(digit)));
            assert_eq!(state.tab, tab, "{digit}");
        }
        assert_eq!(TAB_COUNT, 9);
        assert_eq!(ActiveTab::Models.main_card(), CardId::ModelTable);
        assert_eq!(ActiveTab::Agents.main_card(), CardId::AgentTable);
        state.handle_key_event(press(KeyCode::Char('8')));
        assert_eq!(state.tab, ActiveTab::Projects);
        assert_eq!(ActiveTab::Projects.main_card(), CardId::ProjectTable);
        state.handle_key_event(press(KeyCode::Char('9')));
        assert_eq!(state.tab, ActiveTab::Skills);
        assert_eq!(ActiveTab::Skills.main_card(), CardId::SkillTable);
        state.handle_key_event(press(KeyCode::Char('0')));
        assert_eq!(state.tab, ActiveTab::Skills);
    }

    #[test]
    fn sort_change_keeps_the_selected_row_identity_for_every_selectable_table() {
        let mut state = sample_state();
        let tool = |name: &str, calls: u64, total_ms: i64| ToolLatencyRow {
            tool_name: name.to_string(),
            calls,
            total_ms,
            ..ToolLatencyRow::default()
        };
        state.snapshot.tools = Panel::Ready(vec![
            tool("Bash", 5, 900),
            tool("Read", 90, 500),
            tool("Edit", 20, 100),
        ]);
        let table = CardId::ToolTable as usize;

        // 합계 내림차순: Bash, Read, Edit 에서 Read 선택
        state.card_views[table].selected = 1;
        // 호출 내림차순으로 바꾸면 Read, Edit, Bash 이라 선택은 첫째 행 (숫자 열은 내림차순부터)
        state.change_card_sort(CardId::ToolTable, SortChange::SelectColumn(1));
        assert_eq!(state.card_views[table].selected, 0);
        // 이름 오름차순: Bash, Edit, Read 에서 Read 는 그대로 셋째 행
        state.change_card_sort(CardId::ToolTable, SortChange::SelectColumn(0));
        assert_eq!(state.card_views[table].selected, 2);
        // 같은 열을 다시 선택하면 내림차순 (Read, Edit, Bash)
        state.change_card_sort(CardId::ToolTable, SortChange::SelectColumn(0));
        assert_eq!(state.card_views[table].selected, 0);
        // 세 번째 선택은 기본 정렬 (합계 내림차순: Bash, Read, Edit)이라 Read 는 둘째 행
        state.change_card_sort(CardId::ToolTable, SortChange::SelectColumn(0));
        assert_eq!(state.card_sorts[table], default_card_sorts()[table]);
        assert_eq!(state.card_views[table].selected, 1);

        // 이벤트 피드: 시각 내림차순에서 둘째 이벤트 선택 후 시각 오름차순으로
        let (old, _) = feed_with_prepended(5, 0);
        state.snapshot.events = Panel::Ready(old);
        let feed = CardId::EventFeed as usize;
        state.card_views[feed].selected = 1;
        state.change_card_sort(CardId::EventFeed, SortChange::SelectColumn(0));
        assert_eq!(state.card_views[feed].selected, 3);

        // 모델 표: 요청 내림차순(a, b, c)에서 둘째 행 b 선택 후 세션 내림차순(b, c, a)으로
        let model = |name: &str, requests: u64, sessions: u64| ModelStatsRow {
            model: name.to_string(),
            requests,
            sessions,
            ..ModelStatsRow::default()
        };
        state.snapshot.model_stats = Panel::Ready(vec![
            model("a", 90, 1),
            model("b", 50, 9),
            model("c", 10, 5),
        ]);
        let models = CardId::ModelTable as usize;
        state.card_views[models].selected = 1;
        state.change_card_sort(CardId::ModelTable, SortChange::SelectColumn(2));
        assert_eq!(state.card_views[models].selected, 0);
    }

    #[test]
    fn sort_change_on_a_scrollable_settings_card_resets_its_scroll() {
        let mut state = sample_state();
        let hooks = CardId::SettingsHooks as usize;
        state.card_views[hooks].scroll = 7;
        state.change_card_sort(CardId::SettingsHooks, SortChange::SelectColumn(3));
        assert_eq!(state.card_views[hooks].scroll, 0);
        assert_eq!(state.card_sorts[hooks].column, 3);
        // 정렬 없는 카드는 무시
        let before = state.card_sorts;
        state.change_card_sort(CardId::KpiTools, SortChange::NextColumn);
        assert_eq!(state.card_sorts, before);
    }

    #[test]
    fn help_swallows_next_key_and_quit_keys_exit() {
        let mut state = sample_state();
        state.handle_key_event(press(KeyCode::Char('?')));
        assert!(state.show_help);

        // 도움말 중 Esc 는 닫기만
        state.handle_key_event(press(KeyCode::Esc));
        assert!(!state.show_help);
        assert!(!state.should_quit);

        state.handle_key_event(press(KeyCode::Esc));
        assert!(state.should_quit);

        let mut other = sample_state();
        other.handle_key_event(press(KeyCode::Char('q')));
        assert!(other.should_quit);

        let mut interrupted = sample_state();
        let ctrl_c = KeyEvent::new_with_kind(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );
        interrupted.handle_key_event(ctrl_c);
        assert!(interrupted.should_quit);
    }

    #[test]
    fn force_restart_quits_for_restart_only_when_serve_runs_another_build() {
        // 같은 빌드면 ! 키와 버튼 동작 모두 무시
        let mut state = sample_state();
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('!'))),
            UiCommand::None
        );
        state.perform_action(UiAction::ForceRestart);
        assert!(!state.should_quit && !state.restart_requested);

        // 빌드 불일치면 재시작 표시와 함께 종료
        state.snapshot.serve_stale = true;
        state.handle_key_event(press(KeyCode::Char('!')));
        assert!(state.should_quit && state.restart_requested);

        // 일반 종료는 재시작 표시 없음
        let mut quitting = sample_state();
        quitting.snapshot.serve_stale = true;
        quitting.handle_key_event(press(KeyCode::Char('q')));
        assert!(quitting.should_quit && !quitting.restart_requested);
    }

    #[test]
    fn action_keys_map_to_commands() {
        let mut state = sample_state();
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('i'))),
            UiCommand::Reindex
        );
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('r'))),
            UiCommand::RefreshNow
        );
        // g 는 리포트 탭에서만
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('g'))),
            UiCommand::None
        );

        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('t'))),
            UiCommand::SetTopmost(true)
        );
        assert!(state.always_on_top);
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('t'))),
            UiCommand::SetTopmost(false)
        );
        assert!(!state.always_on_top);
    }

    #[test]
    fn layout_reset_key_clears_only_current_tab_groups() {
        use crate::tui::cards::UiHitAreas;
        use crate::tui::layout::{IDENTITY_ORDER, SplitAxis};
        use ratatui::layout::Rect;

        let mut state = sample_state();
        let mut hit_areas = UiHitAreas::default();
        let area = Rect::new(0, 0, 100, 30);
        state.card_layout.split_cards(
            &mut hit_areas,
            SplitGroup::ToolsRows,
            SplitAxis::Rows,
            area,
            &[20],
            &[5, 3],
        );
        let hit = hit_areas.split_hits[0];
        state.card_layout.drag_cut(&hit, 10);
        state
            .card_layout
            .move_card_to_slot(SplitGroup::ToolsRows, &IDENTITY_ORDER, 0, 1);
        assert!(
            state
                .card_layout
                .has_custom_groups(&[SplitGroup::ToolsRows])
        );

        // 개요 탭에서 초기화해도 도구 탭 배치는 유지 (저장 요청은 항상)
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('l'))),
            UiCommand::SaveState
        );
        assert!(
            state
                .card_layout
                .has_custom_groups(&[SplitGroup::ToolsRows])
        );
        assert!(state.toast.is_some());

        // 도구 탭에서는 경계 비율과 카드 순서가 함께 기본으로
        state.handle_key_event(press(KeyCode::Char('2')));
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('l'))),
            UiCommand::SaveState
        );
        assert!(
            !state
                .card_layout
                .has_custom_groups(&[SplitGroup::ToolsRows])
        );
        assert_eq!(
            state.card_layout.order_of(SplitGroup::ToolsRows),
            IDENTITY_ORDER
        );
    }

    fn shift_press(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::SHIFT, KeyEventKind::Press)
    }

    // 그리기 없이 모든 순서 그룹이 이번 프레임에 그려진 것으로 표시
    fn mark_all_groups_drawn(state: &mut AppState) {
        for group in SplitGroup::ALL {
            state.hit_areas.slot_groups.push(SlotGroupHit {
                group,
                axis: group.axis(),
                slots: Vec::new(),
                cards: Vec::new(),
            });
        }
    }

    #[test]
    fn shift_down_and_up_move_the_focused_card_within_a_rows_group() {
        let mut state = sample_state();
        mark_all_groups_drawn(&mut state);
        state.handle_key_event(press(KeyCode::Char('6')));
        assert_eq!(state.focused_card_id(), CardId::SettingsHooks);

        // 첫 카드를 한 칸 아래로: 다음 카드가 한 칸 위로, 알림과 저장 요청
        assert_eq!(
            state.handle_key_event(shift_press(KeyCode::Down)),
            UiCommand::SaveState
        );
        assert_eq!(
            state.card_layout.order_of(SplitGroup::SettingsRows)[..4],
            [1, 0, 2, 3]
        );
        assert_eq!(
            state.toast.as_ref().map(|toast| toast.text.as_str()),
            Some("카드 위치 변경 (l 기본 배치)")
        );
        assert_eq!(state.toast.as_ref().unwrap().level, ToastLevel::Info);

        // 위로 되돌리면 기본 배치와 같아짐, 맨 위에서 더 올리면 아무 일 없음
        assert_eq!(
            state.handle_key_event(shift_press(KeyCode::Up)),
            UiCommand::SaveState
        );
        assert_eq!(state.card_layout, CardLayout::default());
        assert_eq!(
            state.handle_key_event(shift_press(KeyCode::Up)),
            UiCommand::None
        );
        assert_eq!(state.card_layout, CardLayout::default());

        // 세로 나열 그룹은 ←→ 를 받지 않음
        assert_eq!(
            state.handle_key_event(shift_press(KeyCode::Right)),
            UiCommand::None
        );
        assert_eq!(state.card_layout, CardLayout::default());

        // 마지막 카드 아래로는 이동 없음
        state.focused_card = Some((ActiveTab::Settings, CardId::SettingsDoctor));
        assert_eq!(
            state.handle_key_event(shift_press(KeyCode::Down)),
            UiCommand::None
        );
        assert_eq!(
            state.handle_key_event(shift_press(KeyCode::Up)),
            UiCommand::SaveState
        );
        assert_eq!(
            state.card_layout.order_of(SplitGroup::SettingsRows)[..4],
            [0, 1, 3, 2]
        );
    }

    #[test]
    fn shift_left_and_right_move_cards_within_a_columns_group_and_keep_report_date_keys() {
        let mut state = sample_state();
        mark_all_groups_drawn(&mut state);
        // 개요 탭 기본 포커스(이벤트 피드)는 가로 그룹: ←→ 만 받음
        assert_eq!(state.focused_card_id(), CardId::EventFeed);
        assert_eq!(
            state.handle_key_event(shift_press(KeyCode::Down)),
            UiCommand::None
        );
        assert_eq!(
            state.handle_key_event(shift_press(KeyCode::Left)),
            UiCommand::None
        );
        assert_eq!(
            state.handle_key_event(shift_press(KeyCode::Right)),
            UiCommand::SaveState
        );
        assert_eq!(
            state.card_layout.order_of(SplitGroup::OverviewFeedCols)[..2],
            [1, 0]
        );
        assert!(
            !state
                .card_layout
                .has_custom_groups(&[SplitGroup::OverviewKpiCols])
        );

        // 리포트 탭: Shift+←→ 는 카드 이동, 일반 ←→ 는 날짜 이동
        state.handle_key_event(press(KeyCode::Char('7')));
        assert_eq!(state.focused_card_id(), CardId::ReportBody);
        assert_eq!(
            state.handle_key_event(shift_press(KeyCode::Left)),
            UiCommand::SaveState
        );
        assert_eq!(
            state.card_layout.order_of(SplitGroup::ReportCols)[..2],
            [1, 0]
        );
        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let yesterday = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        assert_eq!(
            state.handle_key_event(press(KeyCode::Left)),
            UiCommand::LoadReport(yesterday)
        );
        assert_eq!(
            state.handle_key_event(press(KeyCode::Right)),
            UiCommand::LoadReport(today)
        );
        assert_eq!(
            state.card_layout.order_of(SplitGroup::ReportCols)[..2],
            [1, 0]
        );
    }

    #[test]
    fn plain_arrows_still_scroll_and_cards_outside_reorder_groups_ignore_shift_arrows() {
        let mut state = sample_state();
        mark_all_groups_drawn(&mut state);
        state.handle_key_event(press(KeyCode::Char('6')));
        let hooks = CardId::SettingsHooks as usize;
        state.card_views[hooks].total = 30;
        state.card_views[hooks].visible = 10;

        // 일반 ↓ 는 스크롤, Shift+↓ 는 스크롤하지 않고 카드를 이동
        state.handle_key_event(press(KeyCode::Down));
        assert_eq!(state.card_views[hooks].scroll, 1);
        state.handle_key_event(shift_press(KeyCode::Down));
        assert_eq!(state.card_views[hooks].scroll, 1);
        assert_eq!(
            state.card_layout.order_of(SplitGroup::SettingsRows)[..2],
            [1, 0]
        );

        // 서버 상태 카드는 순서 대상이 아님
        state.handle_key_event(press(KeyCode::Char('1')));
        state.focused_card = Some((ActiveTab::Overview, CardId::ServerStatus));
        let before = state.card_layout;
        assert_eq!(
            state.handle_key_event(shift_press(KeyCode::Down)),
            UiCommand::None
        );
        assert_eq!(
            state.handle_key_event(shift_press(KeyCode::Right)),
            UiCommand::None
        );
        assert_eq!(state.card_layout, before);
    }

    #[test]
    fn esc_during_a_card_move_cancels_it_instead_of_quitting_and_other_keys_wait() {
        use crate::tui::cards::CardMove;

        let mut state = sample_state();
        state.handle_key_event(press(KeyCode::Char('6')));
        let baseline = state.card_layout.order_of(SplitGroup::SettingsRows);
        state
            .card_layout
            .move_card_to_slot(SplitGroup::SettingsRows, &baseline, 0, 2);
        state.card_move = Some(CardMove {
            card: CardId::SettingsHooks,
            group: SplitGroup::SettingsRows,
            index: 0,
            baseline,
            target: 2,
        });

        // 이동 중 일반 키는 무시, 탭도 바뀌지 않음
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('2'))),
            UiCommand::None
        );
        assert_eq!(state.tab, ActiveTab::Settings);
        assert!(state.card_move.is_some());

        // Esc: 종료하지 않고 누르기 전 순서로 복원
        assert_eq!(state.handle_key_event(press(KeyCode::Esc)), UiCommand::None);
        assert!(!state.should_quit);
        assert!(state.card_move.is_none());
        assert_eq!(state.card_layout, CardLayout::default());

        // 이동이 끝나면 Esc 는 다시 종료
        state.handle_key_event(press(KeyCode::Esc));
        assert!(state.should_quit);
    }

    #[test]
    fn switching_tabs_cancels_a_pending_press_and_a_card_move() {
        use crate::tui::cards::{CardMove, CardPress};

        let mut state = sample_state();
        state.handle_key_event(press(KeyCode::Char('6')));
        let baseline = state.card_layout.order_of(SplitGroup::SettingsRows);
        state
            .card_layout
            .move_card_to_slot(SplitGroup::SettingsRows, &baseline, 0, 3);
        state.card_move = Some(CardMove {
            card: CardId::SettingsHooks,
            group: SplitGroup::SettingsRows,
            index: 0,
            baseline,
            target: 3,
        });
        state.card_press = Some(CardPress {
            card: CardId::SettingsMcp,
            group: SplitGroup::SettingsRows,
            index: 1,
            started: Instant::now(),
            cell: ratatui::layout::Position::new(0, 0),
        });

        state.switch_tab(ActiveTab::Hooks);
        assert!(state.card_move.is_none());
        assert!(state.card_press.is_none());
        assert_eq!(state.card_layout, CardLayout::default());
    }

    #[test]
    fn outline_selection_moves_report_body_to_heading_line() {
        let mut state = sample_state();
        state.tab = ActiveTab::Report;
        state.report = Panel::Ready(Some("# 제목\n\n## 요약\n본문\n\n## 상세\n".to_string()));
        state.card_views[CardId::ReportOutline as usize].total = 3;
        state.select_card_row(CardId::ReportOutline, 2);
        assert_eq!(state.card_views[CardId::ReportOutline as usize].selected, 2);
        assert_eq!(state.card_views[CardId::ReportBody as usize].scroll, 5);

        state.focused_card = Some((ActiveTab::Report, CardId::ReportOutline));
        state.handle_key_event(press(KeyCode::Up));
        assert_eq!(state.card_views[CardId::ReportBody as usize].scroll, 2);
    }

    fn notice_event(ts: &str) -> LiveEvent {
        LiveEvent::Notice {
            ts: ts.to_string(),
            level: "info".to_string(),
            text: format!("알림 {ts}"),
        }
    }

    fn feed_with_prepended(old_count: usize, new_count: usize) -> (Vec<LiveEvent>, Vec<LiveEvent>) {
        let old: Vec<LiveEvent> = (0..old_count)
            .rev()
            .map(|index| notice_event(&format!("2026-11-15T01:00:{index:02}Z")))
            .collect();
        let fresh: Vec<LiveEvent> = (old_count..old_count + new_count)
            .rev()
            .map(|index| notice_event(&format!("2026-11-15T01:00:{index:02}Z")))
            .chain(old.iter().cloned())
            .collect();
        (old, fresh)
    }

    #[test]
    fn event_selection_follows_item_when_new_events_prepend() {
        let mut state = sample_state();
        let (old, fresh) = feed_with_prepended(10, 2);
        state.snapshot.events = Panel::Ready(old);
        let feed = CardId::EventFeed as usize;
        state.card_views[feed].selected = 2;
        state.card_views[feed].scroll = 1;

        let snapshot = DashboardSnapshot {
            events: Panel::Ready(fresh),
            ..DashboardSnapshot::default()
        };
        state.apply_ui_message(UiMessage::Snapshot(Box::new(snapshot)), Instant::now());
        assert_eq!(state.card_views[feed].selected, 4);
        assert_eq!(state.card_views[feed].scroll, 3);
    }

    #[test]
    fn event_feed_at_top_keeps_following_latest() {
        let mut state = sample_state();
        let (old, fresh) = feed_with_prepended(10, 3);
        state.snapshot.events = Panel::Ready(old);

        let snapshot = DashboardSnapshot {
            events: Panel::Ready(fresh),
            ..DashboardSnapshot::default()
        };
        state.apply_ui_message(UiMessage::Snapshot(Box::new(snapshot)), Instant::now());
        let feed = CardId::EventFeed as usize;
        assert_eq!(state.card_views[feed].selected, 0);
        assert_eq!(state.card_views[feed].scroll, 0);
    }

    #[test]
    fn selection_stays_when_the_item_disappears() {
        let mut state = sample_state();
        let (old, _) = feed_with_prepended(10, 0);
        state.snapshot.events = Panel::Ready(old);
        let feed = CardId::EventFeed as usize;
        state.card_views[feed].selected = 8;
        state.card_views[feed].scroll = 2;

        let (_, unrelated) = feed_with_prepended(3, 3);
        let snapshot = DashboardSnapshot {
            events: Panel::Ready(unrelated[..3].to_vec()),
            ..DashboardSnapshot::default()
        };
        state.apply_ui_message(UiMessage::Snapshot(Box::new(snapshot)), Instant::now());
        assert_eq!(state.card_views[feed].selected, 8);
        assert_eq!(state.card_views[feed].scroll, 2);
    }

    #[test]
    fn tool_selection_follows_name_after_rank_change() {
        let mut state = sample_state();
        let tool = |name: &str, calls: u64| ToolLatencyRow {
            tool_name: name.to_string(),
            calls,
            ..ToolLatencyRow::default()
        };
        state.snapshot.tools = Panel::Ready(vec![tool("Bash", 10), tool("Read", 5)]);
        let table = CardId::ToolTable as usize;
        // 호출 수 내림차순으로 정렬
        state.card_sorts[table] = SortState {
            column: 1,
            descending: true,
        };
        assert_eq!(state.card_views[table].selected, 0);

        // 호출 순위가 뒤집히면 Bash 는 둘째 행
        let snapshot = DashboardSnapshot {
            tools: Panel::Ready(vec![tool("Bash", 3), tool("Read", 9)]),
            ..DashboardSnapshot::default()
        };
        state.apply_ui_message(UiMessage::Snapshot(Box::new(snapshot)), Instant::now());
        assert_eq!(state.card_views[table].selected, 1);
    }

    #[test]
    fn hook_selection_follows_route_guard_outcome_detail() {
        let mut state = sample_state();
        let hook = |guard: &str, detail: &str| HookStatRow {
            route: "pretool".to_string(),
            guard: guard.to_string(),
            outcome: "deny".to_string(),
            detail: detail.to_string(),
            ..HookStatRow::default()
        };
        state.snapshot.hooks = Panel::Ready(vec![hook("a", "x"), hook("b", "y")]);
        let table = CardId::HookTable as usize;
        state.card_views[table].selected = 1;

        let snapshot = DashboardSnapshot {
            hooks: Panel::Ready(vec![hook("c", "z"), hook("a", "x"), hook("b", "y")]),
            ..DashboardSnapshot::default()
        };
        state.apply_ui_message(UiMessage::Snapshot(Box::new(snapshot)), Instant::now());
        assert_eq!(state.card_views[table].selected, 2);
    }

    #[test]
    fn feed_thumb_drag_survives_unchanged_snapshot() {
        let mut state = sample_state();
        let (old, _) = feed_with_prepended(40, 0);
        state.snapshot.events = Panel::Ready(old.clone());
        let feed = CardId::EventFeed as usize;
        state.card_views[feed].total = 40;
        state.card_views[feed].visible = 10;
        state.card_views[feed].selected = 2;
        state.set_card_scroll(CardId::EventFeed, 999);
        assert_eq!(state.card_views[feed].scroll, 30);

        let snapshot = DashboardSnapshot {
            events: Panel::Ready(old),
            ..DashboardSnapshot::default()
        };
        state.apply_ui_message(UiMessage::Snapshot(Box::new(snapshot)), Instant::now());
        assert_eq!(state.card_views[feed].selected, 2);
        assert_eq!(state.card_views[feed].scroll, 30);
    }

    #[test]
    fn tool_wheel_scroll_survives_unchanged_snapshot() {
        let mut state = sample_state();
        let tools: Vec<ToolLatencyRow> = (0..40)
            .map(|index| ToolLatencyRow {
                tool_name: format!("tool{index:02}"),
                calls: 100 - index,
                ..ToolLatencyRow::default()
            })
            .collect();
        state.snapshot.tools = Panel::Ready(tools.clone());
        let table = CardId::ToolTable as usize;
        state.card_views[table].total = 40;
        state.card_views[table].visible = 10;
        assert!(state.scroll_card_view(CardId::ToolTable, 6));

        let snapshot = DashboardSnapshot {
            tools: Panel::Ready(tools),
            ..DashboardSnapshot::default()
        };
        state.apply_ui_message(UiMessage::Snapshot(Box::new(snapshot)), Instant::now());
        assert_eq!(state.card_views[table].selected, 0);
        assert_eq!(state.card_views[table].scroll, 6);
    }

    #[test]
    fn hook_scroll_survives_unchanged_snapshot_and_shifts_with_rank() {
        let mut state = sample_state();
        let hook = |guard: &str| HookStatRow {
            route: "pretool".to_string(),
            guard: guard.to_string(),
            outcome: "deny".to_string(),
            ..HookStatRow::default()
        };
        let rows: Vec<HookStatRow> = (0..40).map(|index| hook(&format!("g{index:02}"))).collect();
        state.snapshot.hooks = Panel::Ready(rows.clone());
        let table = CardId::HookTable as usize;
        state.card_views[table].total = 40;
        state.card_views[table].visible = 10;
        state.card_views[table].selected = 1;
        state.card_views[table].scroll = 10;

        let snapshot = DashboardSnapshot {
            hooks: Panel::Ready(rows.clone()),
            ..DashboardSnapshot::default()
        };
        state.apply_ui_message(UiMessage::Snapshot(Box::new(snapshot)), Instant::now());
        assert_eq!(state.card_views[table].selected, 1);
        assert_eq!(state.card_views[table].scroll, 10);

        // 위에 한 행이 끼어들면 선택과 화면 위치가 같이 한 칸 이동
        let mut shifted = vec![hook("new")];
        shifted.extend(rows);
        let snapshot = DashboardSnapshot {
            hooks: Panel::Ready(shifted),
            ..DashboardSnapshot::default()
        };
        state.apply_ui_message(UiMessage::Snapshot(Box::new(snapshot)), Instant::now());
        assert_eq!(state.card_views[table].selected, 2);
        assert_eq!(state.card_views[table].scroll, 11);
    }

    #[test]
    fn set_card_scroll_clamps_and_ignores_static_cards() {
        let mut state = sample_state();
        let feed = CardId::EventFeed as usize;
        state.card_views[feed].total = 30;
        state.card_views[feed].visible = 10;
        state.set_card_scroll(CardId::EventFeed, 999);
        assert_eq!(state.card_views[feed].scroll, 20);
        state.set_card_scroll(CardId::EventFeed, 4);
        assert_eq!(state.card_views[feed].scroll, 4);

        state.set_card_scroll(CardId::KpiTools, 5);
        assert_eq!(state.card_views[CardId::KpiTools as usize].scroll, 0);
    }

    #[test]
    fn a_fresh_state_starts_in_english() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let state = AppState::new(sample_config(), today);
        assert_eq!(state.lang, Lang::En);
        assert_eq!(ActiveTab::Overview.tab_label(state.lang), "Overview");
        assert_eq!(state.window, WindowKind::Today);
        assert_eq!(state.window.window_label(state.lang), "Today");
        assert_eq!(WindowKind::Hour.window_label(state.lang), "1h");
    }

    #[test]
    fn shift_l_toggles_the_language_both_ways_and_requests_a_save() {
        let mut state = sample_state();
        assert_eq!(state.lang, Lang::Ko);

        assert_eq!(
            state.handle_key_event(shift_press(KeyCode::Char('L'))),
            UiCommand::SaveState
        );
        assert_eq!(state.lang, Lang::En);
        let toast = state.toast.as_ref().unwrap();
        assert_eq!(
            (toast.text.as_str(), toast.level),
            ("Language: English", ToastLevel::Info)
        );

        // 콤키 조합 없이 대문자만 와도 같은 키
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('L'))),
            UiCommand::SaveState
        );
        assert_eq!(state.lang, Lang::Ko);
        assert_eq!(state.toast.as_ref().unwrap().text, "언어: 한국어");

        // 도움말이 열려 있으면 닫기만
        state.show_help = true;
        assert_eq!(
            state.handle_key_event(shift_press(KeyCode::Char('L'))),
            UiCommand::None
        );
        assert_eq!(state.lang, Lang::Ko);
        assert!(!state.show_help);
    }

    #[test]
    fn plain_l_resets_the_layout_and_keeps_the_language() {
        for lang in [Lang::En, Lang::Ko] {
            let mut state = sample_state();
            state.lang = lang;
            assert_eq!(
                state.handle_key_event(press(KeyCode::Char('l'))),
                UiCommand::SaveState
            );
            assert_eq!(state.lang, lang);
            let expected = match lang {
                Lang::En => "Card size, order, widths reset",
                Lang::Ko => "카드 크기·순서·열 폭 기본 배치 복원",
            };
            assert_eq!(state.toast.as_ref().unwrap().text, expected);
        }
    }

    #[test]
    fn pin_toast_follows_the_active_language() {
        let mut state = sample_state();
        state.lang = Lang::En;
        state.handle_key_event(press(KeyCode::Char('t')));
        assert_eq!(state.toast.as_ref().unwrap().text, "Window pinned on top");
        state.handle_key_event(press(KeyCode::Char('t')));
        assert_eq!(state.toast.as_ref().unwrap().text, "Window unpinned");
        state.lang = Lang::Ko;
        state.handle_key_event(press(KeyCode::Char('t')));
        assert_eq!(state.toast.as_ref().unwrap().text, "창 최상단 고정");
    }

    #[test]
    fn toast_message_sets_expiry_from_now() {
        let mut state = sample_state();
        let now = Instant::now();
        let reindexed = || UiMessage::Admin {
            action: AdminAction::Reindex,
            outcome: AdminOutcome::Done("ok".to_string()),
        };
        state.apply_ui_message(reindexed(), now);
        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.text, "재색인 완료 ok");
        assert_eq!(toast.level, ToastLevel::Success);
        assert_eq!(toast.expires_at, now + TOAST_LIFETIME);

        // 알림 문구는 수신 시점의 화면 언어로 만듦
        state.lang = Lang::En;
        state.apply_ui_message(reindexed(), now);
        assert_eq!(state.toast.as_ref().unwrap().text, "Reindex done ok");
    }

    #[test]
    fn a_state_save_failure_message_becomes_an_error_toast_in_the_active_language() {
        let mut state = sample_state();
        let now = Instant::now();
        state.apply_ui_message(
            UiMessage::StateSaveFailed {
                detail: "access denied".to_string(),
            },
            now,
        );
        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.text, "화면 상태 저장 실패: access denied");
        assert_eq!(toast.level, ToastLevel::Error);
        assert_eq!(toast.expires_at, now + TOAST_LIFETIME);

        state.lang = Lang::En;
        state.apply_ui_message(
            UiMessage::StateSaveFailed {
                detail: "access denied".to_string(),
            },
            now,
        );
        assert_eq!(
            state.toast.as_ref().unwrap().text,
            "Saving UI state failed: access denied"
        );
    }

    #[test]
    fn a_schema_upgrade_failure_becomes_an_error_toast_in_the_active_language() {
        let mut state = sample_state();
        let now = Instant::now();
        let failed = || UiMessage::SchemaUpgradeFailed {
            detail: "database is locked".to_string(),
        };
        state.apply_ui_message(failed(), now);
        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.text, "DB 스키마 갱신 실패: database is locked");
        assert_eq!(toast.level, ToastLevel::Error);
        assert_eq!(toast.expires_at, now + TOAST_LIFETIME);

        state.lang = Lang::En;
        state.apply_ui_message(failed(), now);
        assert_eq!(
            state.toast.as_ref().unwrap().text,
            "DB schema upgrade failed: database is locked"
        );
    }

    #[test]
    fn a_stale_serve_build_raises_one_error_toast_per_serve_pid() {
        let mut state = sample_state();
        let now = Instant::now();
        let window = state.window;
        let stale = |pid: u32, serve_stale: bool| {
            UiMessage::Snapshot(Box::new(DashboardSnapshot {
                window,
                status: Panel::Ready(StatusSnapshot {
                    pid,
                    ..StatusSnapshot::default()
                }),
                serve_stale,
                ..DashboardSnapshot::default()
            }))
        };

        // 같은 빌드면 알림 없음
        state.apply_ui_message(stale(7, false), now);
        assert!(state.toast.is_none());

        // 다른 빌드면 pid 를 담은 오류 알림 한 번
        state.apply_ui_message(stale(7, true), now);
        let toast = state.toast.take().unwrap();
        assert_eq!(
            toast.text,
            "serve(pid 7)가 다른 빌드로 실행 중: 하단 [!] 강제 재시작으로 최신 빌드 재실행"
        );
        assert_eq!(toast.level, ToastLevel::Error);
        assert_eq!(toast.expires_at, now + TOAST_LIFETIME);

        // 같은 serve 는 다시 알리지 않고, 다른 pid 로 바뀌면 다시 알림
        state.apply_ui_message(stale(7, true), now);
        assert!(state.toast.is_none());
        state.lang = Lang::En;
        state.apply_ui_message(stale(8, true), now);
        assert_eq!(
            state.toast.as_ref().unwrap().text,
            "serve (pid 8) runs another build: press [!] Force restart to rerun on the latest build"
        );
    }

    // 자정 직후 (시계를 지정한 날 정오 로컬로 고정, 시간대와 무관)
    fn local_noon(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Local
            .with_ymd_and_hms(year, month, day, 12, 0, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    #[test]
    fn crossing_midnight_moves_today_and_follows_the_day_and_week_being_viewed() {
        // 시작 때 today = 2026-11-15(일), 일간 보기는 오늘, 주간 기본 주 = 11-09(월)
        let mut state = sample_state();
        state.tab = ActiveTab::Report;
        state.clock_override = Some(local_noon(2026, 11, 15));
        assert_eq!(state.refresh_today(), None);

        // 1. 자정을 넘김: 오늘 리포트를 보던 중이면 새 오늘로 따라가고 본문 재적재
        state.report = Panel::Ready(Some("old live body".to_string()));
        state.clock_override = Some(local_noon(2026, 11, 16));
        assert_eq!(
            state.refresh_today(),
            Some(UiCommand::LoadReport(date(2026, 11, 16)))
        );
        assert_eq!(state.today, date(2026, 11, 16));
        assert_eq!(state.report_date, date(2026, 11, 16));
        assert_eq!(state.report_week, date(2026, 11, 16));
        assert!(matches!(state.report, Panel::Pending));
        assert_eq!(state.refresh_today(), None);

        // 새 오늘은 다시 "오늘"이라 다음 날 이동은 막히고, 하루 전은 허용
        assert_eq!(
            state.handle_key_event(press(KeyCode::Right)),
            UiCommand::None
        );
        assert_eq!(
            state.handle_key_event(press(KeyCode::Left)),
            UiCommand::LoadReport(date(2026, 11, 15))
        );
    }

    #[test]
    fn crossing_midnight_keeps_a_past_day_and_week_and_skips_reloads_that_change_nothing() {
        // 지난 날짜를 보는 중이면 그대로 (today 만 옮김, 본문 재적재 없음)
        let mut state = sample_state();
        state.tab = ActiveTab::Report;
        state.report_date = date(2026, 11, 13);
        state.report = Panel::Ready(Some("stored body".to_string()));
        state.clock_override = Some(local_noon(2026, 11, 16));
        assert_eq!(state.refresh_today(), Some(UiCommand::None));
        assert_eq!(state.today, date(2026, 11, 16));
        assert_eq!(state.report_date, date(2026, 11, 13));
        assert!(matches!(state.report, Panel::Ready(Some(_))));

        // 주간 보기: 지난 주를 보는 중이면 그대로, 이번 주를 보는 중이면 새 주를 따라감
        let mut state = sample_state();
        state.tab = ActiveTab::Report;
        state.report_weekly = true;
        state.report_week = date(2026, 11, 2);
        state.clock_override = Some(local_noon(2026, 11, 16));
        assert_eq!(state.refresh_today(), Some(UiCommand::None));
        assert_eq!(state.report_week, date(2026, 11, 2));

        let mut state = sample_state();
        state.tab = ActiveTab::Report;
        state.report_weekly = true;
        state.clock_override = Some(local_noon(2026, 11, 16));
        assert_eq!(
            state.refresh_today(),
            Some(UiCommand::LoadWeeklyReport(date(2026, 11, 16)))
        );

        // 같은 주 안의 자정(화→수)은 주간 본문을 다시 적재하지 않음
        let mut state = sample_state();
        state.tab = ActiveTab::Report;
        state.today = date(2026, 11, 10);
        state.report_date = date(2026, 11, 10);
        state.report_week = date(2026, 11, 9);
        state.report_weekly = true;
        state.report = Panel::Ready(Some("weekly".to_string()));
        state.clock_override = Some(local_noon(2026, 11, 11));
        assert_eq!(state.refresh_today(), Some(UiCommand::None));
        assert_eq!(state.report_week, date(2026, 11, 9));
        assert!(matches!(state.report, Panel::Ready(Some(_))));

        // 리포트 탭이 아니면 날짜만 따라가고 적재 요청은 없음 (탭에 들어갈 때 새 날로 적재)
        let mut state = sample_state();
        state.clock_override = Some(local_noon(2026, 11, 16));
        assert_eq!(state.refresh_today(), Some(UiCommand::None));
        assert_eq!(state.report_date, date(2026, 11, 16));
        assert_eq!(
            state.switch_tab(ActiveTab::Report),
            UiCommand::LoadReport(date(2026, 11, 16))
        );
    }
}
