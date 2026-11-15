//! tui/state.rs
//! TUI 화면 상태: 탭, 카드 포커스·배치·스크롤, 집계 구간, 패널 데이터, 키 입력 상태 전이.

use crate::core::config::AppConfig;
use crate::core::model::{LiveEvent, StatusSnapshot};
use crate::inventory::{DoctorFinding, Inventory};
use crate::store::query::{
    AgentStatsRow, CacheBreakRow, HookStatRow, MinuteBucket, ModelStatsRow, OverviewStats,
    ToolLatencyRow,
};
use crate::tui::cards::{
    CARD_COUNT, CardId, CardKind, CardView, ColumnDrag, CursorMotion, MAX_TABLE_COLUMNS,
    ScrollDrag, UiAction, UiHitAreas,
};
use crate::tui::format::{outline_entries, prepare_report_markdown};
use crate::tui::layout::{CardLayout, SplitDrag, SplitGroup};
use crate::tui::poller::live_event_key;
use crate::tui::sort::{
    SortChange, SortState, compare_agent_row, compare_cache_break_row, compare_event_row,
    compare_hook_row, compare_model_row, compare_tool_row, default_card_sorts, sort_rows,
    sort_spec,
};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

pub const TAB_COUNT: usize = 7;
pub const TOAST_LIFETIME: Duration = Duration::from_secs(4);
// 정렬해도 같은 행을 따라가는 선택 가능 표 카드
const SELECTABLE_SORT_CARDS: [CardId; 6] = [
    CardId::EventFeed,
    CardId::ToolTable,
    CardId::HookTable,
    CardId::ModelTable,
    CardId::AgentTable,
    CardId::CacheBreaks,
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
    ];

    pub fn tab_label(self) -> &'static str {
        match self {
            Self::Overview => "개요",
            Self::Tools => "도구",
            Self::Hooks => "훅",
            Self::Models => "모델",
            Self::Agents => "에이전트",
            Self::Settings => "설정",
            Self::Report => "리포트",
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
        }
    }

    // 열 폭을 끌어 조절하는 표 카드 (배치 초기화 대상)
    pub fn table_cards(self) -> &'static [CardId] {
        match self {
            Self::Tools => &[CardId::ToolTable],
            Self::Hooks => &[CardId::HookTable],
            Self::Models => &[CardId::ModelTable, CardId::CacheBreaks],
            Self::Agents => &[CardId::AgentTable],
            Self::Overview | Self::Settings | Self::Report => &[],
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
        }
    }
}

// 2. 집계 구간 ----------------------------------------------------------------
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WindowKind {
    #[default]
    Hour,
    Day,
    Week,
}

impl WindowKind {
    pub fn window_label(self) -> &'static str {
        match self {
            Self::Hour => "1시간",
            Self::Day => "24시간",
            Self::Week => "7일",
        }
    }

    pub fn window_span(self) -> ChronoDuration {
        match self {
            Self::Hour => ChronoDuration::hours(1),
            Self::Day => ChronoDuration::hours(24),
            Self::Week => ChronoDuration::days(7),
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
    Failed(String),
}

#[derive(Debug, Clone, Default)]
pub enum DbState {
    #[default]
    Pending,
    Missing,
    Open,
    Failed(String),
}

// 4. 수집 스냅샷 --------------------------------------------------------------
#[derive(Debug, Clone, Default)]
pub struct DashboardSnapshot {
    pub window: WindowKind,
    pub taken_at: Option<DateTime<Utc>>,
    pub db_state: DbState,
    pub status: Panel<StatusSnapshot>,
    pub overview: Panel<OverviewStats>,
    pub today: Panel<OverviewStats>,
    pub tools: Panel<Vec<ToolLatencyRow>>,
    pub hooks: Panel<Vec<HookStatRow>>,
    pub model_stats: Panel<Vec<ModelStatsRow>>,
    pub agent_stats: Panel<Vec<AgentStatsRow>>,
    pub cache_breaks: Panel<Vec<CacheBreakRow>>,
    pub minutes: Panel<Vec<MinuteBucket>>,
    pub events: Panel<Vec<LiveEvent>>,
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
        inventory: Panel<Inventory>,
        doctor: Panel<Vec<DoctorFinding>>,
    },
    Report {
        date: NaiveDate,
        body: Panel<Option<String>>,
    },
    Toast {
        text: String,
        level: ToastLevel,
    },
}

// 7. 입력 → 실행 요청 ---------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCommand {
    None,
    RefreshNow,
    SetWindow(WindowKind),
    LoadSettings,
    LoadReport(NaiveDate),
    GenerateReport(NaiveDate),
    Reindex,
    SetTopmost(bool),
}

// 8. 화면 상태 ----------------------------------------------------------------
pub struct AppState {
    pub config: AppConfig,
    pub today: NaiveDate,
    pub tab: ActiveTab,
    pub window: WindowKind,
    pub snapshot: DashboardSnapshot,
    // 카드별 정렬 (정렬 가능한 표 카드만 의미 있음)
    pub card_sorts: [SortState; CARD_COUNT],
    // 카드별 스크롤·선택 (탭을 오가도 유지)
    pub card_views: [CardView; CARD_COUNT],
    // 카드 경계 비율 (세션 동안 메모리에만 보관), 드래그 중·마우스 아래 경계
    pub card_layout: CardLayout,
    pub split_drag: Option<SplitDrag>,
    pub split_hover: Option<(SplitGroup, usize)>,
    // 카드별 끌어 정한 열 폭 (None 이면 자동 폭, 세션 동안 메모리에만 보관), 열 경계 드래그 중·마우스 아래 경계
    // last_column_click = 끌지 않고 뗀 직전 경계 클릭 (더블클릭 판정용)
    pub column_widths: [[Option<u16>; MAX_TABLE_COLUMNS]; CARD_COUNT],
    pub column_drag: Option<ColumnDrag>,
    pub column_hover: Option<(CardId, usize)>,
    pub last_column_click: Option<(CardId, usize, Instant)>,
    // 스크롤바 썸 드래그 중, 마우스가 트랙 위에 있는 카드
    pub scroll_drag: Option<ScrollDrag>,
    pub scroll_hover: Option<CardId>,
    // 클릭한 카드(선택 당시 탭에서만 유효), 마우스 아래 카드
    pub focused_card: Option<(ActiveTab, CardId)>,
    pub hover_card: Option<CardId>,
    pub hit_areas: UiHitAreas,
    pub always_on_top: bool,
    pub show_help: bool,
    pub toast: Option<Toast>,
    pub report_date: NaiveDate,
    pub report: Panel<Option<String>>,
    // 리포트 본문 줄별 화면 시작 행 (줄바꿈 반영, 직전 그리기 기준)
    pub report_row_starts: Vec<usize>,
    pub inventory: Panel<Inventory>,
    pub doctor: Panel<Vec<DoctorFinding>>,
    pub settings_requested: bool,
    pub should_quit: bool,
}

impl AppState {
    // 8-1. 초기 상태 (리포트 날짜 기본값 = 어제)
    pub fn new(config: AppConfig, today: NaiveDate) -> Self {
        Self {
            config,
            today,
            tab: ActiveTab::Overview,
            window: WindowKind::Hour,
            snapshot: DashboardSnapshot::default(),
            card_sorts: default_card_sorts(),
            card_views: [CardView::default(); CARD_COUNT],
            card_layout: CardLayout::default(),
            split_drag: None,
            split_hover: None,
            column_widths: [[None; MAX_TABLE_COLUMNS]; CARD_COUNT],
            column_drag: None,
            column_hover: None,
            last_column_click: None,
            scroll_drag: None,
            scroll_hover: None,
            focused_card: None,
            hover_card: None,
            hit_areas: UiHitAreas::default(),
            always_on_top: false,
            show_help: false,
            toast: None,
            report_date: today.pred_opt().unwrap_or(today),
            report: Panel::Pending,
            report_row_starts: Vec::new(),
            inventory: Panel::Pending,
            doctor: Panel::Pending,
            settings_requested: false,
            should_quit: false,
        }
    }

    // 8-2. 탭 전환 (설정은 첫 진입 시, 리포트는 진입마다 적재 요청)
    pub fn switch_tab(&mut self, tab: ActiveTab) -> UiCommand {
        self.tab = tab;
        self.hover_card = None;
        self.split_hover = None;
        self.column_hover = None;
        self.column_drag = None;
        self.scroll_hover = None;
        self.scroll_drag = None;
        match tab {
            ActiveTab::Settings if !self.settings_requested => {
                self.settings_requested = true;
                UiCommand::LoadSettings
            }
            ActiveTab::Report => UiCommand::LoadReport(self.report_date),
            _ => UiCommand::None,
        }
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

        // 3. 키별 전이
        let slot = self.tab as usize;
        let card = self.focused_card_id();
        match key.code {
            KeyCode::Char('q' | 'Q') | KeyCode::Esc => self.perform_action(UiAction::Quit),
            KeyCode::Char('?') => self.perform_action(UiAction::ShowHelp),
            KeyCode::BackTab => self.switch_tab(ActiveTab::ALL[(slot + TAB_COUNT - 1) % TAB_COUNT]),
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.switch_tab(ActiveTab::ALL[(slot + TAB_COUNT - 1) % TAB_COUNT])
            }
            KeyCode::Tab => self.switch_tab(ActiveTab::ALL[(slot + 1) % TAB_COUNT]),
            KeyCode::Char(digit @ '1'..='7') => {
                self.switch_tab(ActiveTab::ALL[digit as usize - '1' as usize])
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
            KeyCode::Char('l' | 'L') => self.perform_action(UiAction::ResetLayout),
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
                    WindowKind::Hour => WindowKind::Day,
                    WindowKind::Day => WindowKind::Week,
                    WindowKind::Week => WindowKind::Hour,
                };
                UiCommand::SetWindow(self.window)
            }
            UiAction::RefreshView => match self.tab {
                ActiveTab::Settings => UiCommand::LoadSettings,
                ActiveTab::Report => UiCommand::LoadReport(self.report_date),
                _ => UiCommand::RefreshNow,
            },
            UiAction::TogglePin => {
                self.always_on_top = !self.always_on_top;
                let text = if self.always_on_top {
                    "창 최상단 고정"
                } else {
                    "창 최상단 고정 해제"
                };
                self.show_toast(text, ToastLevel::Info);
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
                self.show_toast("카드 크기·열 폭 기본 배치 복원", ToastLevel::Info);
                UiCommand::None
            }
            UiAction::ReportPrev => {
                self.report_date = self.report_date.pred_opt().unwrap_or(self.report_date);
                self.reset_report_view();
                UiCommand::LoadReport(self.report_date)
            }
            UiAction::ReportNext => {
                let next_date = self.report_date.succ_opt().unwrap_or(self.report_date);
                if next_date > self.today {
                    return UiCommand::None;
                }
                self.report_date = next_date;
                self.reset_report_view();
                UiCommand::LoadReport(self.report_date)
            }
            UiAction::ReportGenerate => UiCommand::GenerateReport(self.report_date),
        }
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
                // 구간을 바꾼 뒤 늦게 온 이전 구간 스냅샷은 버림
                if snapshot.window == self.window {
                    let keys = self.capture_selection_keys();
                    self.snapshot = *snapshot;
                    self.restore_selection(keys, true);
                }
            }
            UiMessage::Settings { inventory, doctor } => {
                self.inventory = inventory;
                self.doctor = doctor;
            }
            UiMessage::Report { date, body } => {
                if date == self.report_date {
                    self.report = body;
                }
            }
            UiMessage::Toast { text, level } => {
                self.toast = Some(Toast {
                    text,
                    level,
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
                let Panel::Ready(agents) = &self.snapshot.agent_stats else {
                    return None;
                };
                let mut sorted = agents.clone();
                sort_rows(&mut sorted, sort, compare_agent_row);
                Some(sorted.iter().map(|row| row.agent.clone()).collect())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::sample_config;
    use crossterm::event::KeyEventKind;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    fn sample_state() -> AppState {
        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        AppState::new(sample_config(), today)
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
        assert_eq!(state.tab, ActiveTab::Report);

        state.handle_key_event(press(KeyCode::Tab));
        assert_eq!(state.tab, ActiveTab::Overview);

        let shift_tab =
            KeyEvent::new_with_kind(KeyCode::Tab, KeyModifiers::SHIFT, KeyEventKind::Press);
        state.handle_key_event(shift_tab);
        assert_eq!(state.tab, ActiveTab::Report);
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

        let yesterday = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('7'))),
            UiCommand::LoadReport(yesterday)
        );
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('g'))),
            UiCommand::GenerateReport(yesterday)
        );
    }

    #[test]
    fn report_date_moves_with_arrows_and_stops_at_today() {
        let mut state = sample_state();
        state.handle_key_event(press(KeyCode::Char('7')));
        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();

        let moved = state.handle_key_event(press(KeyCode::Right));
        assert_eq!(moved, UiCommand::LoadReport(today));
        assert_eq!(state.report_date, today);

        // 오늘 이후로는 이동 없음
        assert_eq!(
            state.handle_key_event(press(KeyCode::Right)),
            UiCommand::None
        );
        assert_eq!(state.report_date, today);

        let earlier = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        assert_eq!(
            state.handle_key_event(press(KeyCode::Left)),
            UiCommand::LoadReport(earlier)
        );
        assert!(matches!(state.report, Panel::Pending));
    }

    #[test]
    fn window_key_cycles_and_stale_snapshot_is_dropped() {
        let mut state = sample_state();
        assert_eq!(
            state.handle_key_event(press(KeyCode::Char('w'))),
            UiCommand::SetWindow(WindowKind::Day)
        );
        state.handle_key_event(press(KeyCode::Char('w')));
        assert_eq!(state.window, WindowKind::Week);
        state.handle_key_event(press(KeyCode::Char('w')));
        assert_eq!(state.window, WindowKind::Hour);

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
        assert_eq!(TAB_COUNT, 7);
        assert_eq!(ActiveTab::Models.main_card(), CardId::ModelTable);
        assert_eq!(ActiveTab::Agents.main_card(), CardId::AgentTable);
        state.handle_key_event(press(KeyCode::Char('8')));
        assert_eq!(state.tab, ActiveTab::Report);
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
        // 호출 오름차순으로 바꾸면 Bash, Edit, Read 이라 선택은 셋째 행
        state.change_card_sort(CardId::ToolTable, SortChange::SelectColumn(1));
        assert_eq!(state.card_views[table].selected, 2);
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

        // 모델 표: 요청 내림차순(a, b, c)에서 둘째 행 b 선택 후 세션 오름차순(a, c, b)으로
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
        assert_eq!(state.card_views[models].selected, 2);
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
        use crate::tui::layout::{SplitAxis, SplitHit};
        use ratatui::layout::Rect;

        let mut state = sample_state();
        let mut hits: Vec<SplitHit> = Vec::new();
        let area = Rect::new(0, 0, 100, 30);
        state.card_layout.split_cards(
            &mut hits,
            SplitGroup::ToolsRows,
            SplitAxis::Rows,
            area,
            &[20],
            &[5, 3],
        );
        let hit = hits[0];
        state.card_layout.drag_cut(&hit, 10);
        assert!(
            state
                .card_layout
                .has_custom_groups(&[SplitGroup::ToolsRows])
        );

        // 개요 탭에서 초기화해도 도구 탭 배치는 유지
        state.handle_key_event(press(KeyCode::Char('l')));
        assert!(
            state
                .card_layout
                .has_custom_groups(&[SplitGroup::ToolsRows])
        );
        assert!(state.toast.is_some());

        state.handle_key_event(press(KeyCode::Char('2')));
        state.handle_key_event(press(KeyCode::Char('l')));
        assert!(
            !state
                .card_layout
                .has_custom_groups(&[SplitGroup::ToolsRows])
        );
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
    fn toast_message_sets_expiry_from_now() {
        let mut state = sample_state();
        let now = Instant::now();
        state.apply_ui_message(
            UiMessage::Toast {
                text: "재색인 완료".to_string(),
                level: ToastLevel::Success,
            },
            now,
        );
        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.text, "재색인 완료");
        assert_eq!(toast.expires_at, now + TOAST_LIFETIME);
    }
}
