//! tui/mod.rs
//! ratatui 실시간 관제 화면 (개요, 도구, 훅, 모델, 에이전트, 설정, 리포트), 마우스·창·콘솔 글꼴 제어.

pub mod card_move;
pub mod cards;
pub mod format;
pub mod i18n;
pub mod layout;
pub mod layout_store;
pub mod mouse;
pub mod poller;
pub mod sort;
pub mod state;
pub mod state_writer;
pub mod tab_move;
#[cfg(test)]
pub mod test_support;
pub mod theme;
pub mod views;
pub mod win_ctrl;

use crate::core::config::{AppConfig, resolve_tui_state_path};
use crate::tui::cards::CardId;
use crate::tui::i18n::Msg;
use crate::tui::layout::SplitGroup;
use crate::tui::poller::{AdminAction, PollCommand, QUIET_PANIC};
use crate::tui::state::{AppState, ToastLevel, UiCommand, UiMessage};
use crate::tui::state_writer::{StateWriter, spawn_state_writer};
use anyhow::Context;
use chrono::Local;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::cell::Cell;
use std::env;
use std::io;
use std::mem;
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

// 입력 대기 단위, 변화가 있을 때만 다시 그림
const FRAME_INTERVAL: Duration = Duration::from_millis(100);
// 변화가 없어도 다시 그리는 최대 간격
const IDLE_REDRAW_INTERVAL: Duration = Duration::from_secs(1);
// 클래식 콘솔(conhost) 실행 시 적용할 글꼴, Windows Terminal 은 프로필 글꼴 사용
pub const DEFAULT_CONSOLE_FONT: &str = "D2Coding ligature";

// 창 최상단 고정 상태 (종료·panic 복원 경로가 공유)
static TOPMOST_ACTIVE: AtomicBool = AtomicBool::new(false);

// 1. 터미널 복원 가드 ---------------------------------------------------------
// 정상 종료·오류·panic 어느 경로든 Drop 으로 원래 화면 복구, 이어서 상태 저장 스레드의 남은 요청을 쓰고 합류 (마지막 확정 저장이 사라지지 않음)
struct TerminalGuard {
    writer: StateWriter,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
        // 마지막 쓰기가 실패로 끝났으면 UI 가 이미 닫혔으니 복원한 터미널에 한 줄로 알림
        if let Some(detail) = self.writer.finish_state_saves() {
            eprintln!("claude-monitor: UI state not saved: {detail}");
        }
    }
}

// 2. 터미널 복원 --------------------------------------------------------------
// 여러 번 불려도 안전 (실패는 무시), 켜 둔 최상단 고정도 해제
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        Show
    );
    if TOPMOST_ACTIVE.swap(false, Ordering::SeqCst) {
        win_ctrl::set_terminal_topmost(false);
    }
}

// 3. TUI 실행 -----------------------------------------------------------------
pub fn run_tui(config: &AppConfig) -> anyhow::Result<()> {
    // 1. 상태와 배경 수집 스레드 (상태 파일은 홈 공용 폴더 고정, 시작할 때 한 번 정함)
    let mut app = AppState::new(config.clone(), Local::now().date_naive());
    app.state_file = resolve_tui_state_path(
        &config.data_dir,
        env::var_os("HOME"),
        env::var_os("USERPROFILE"),
    );
    app.load_tui_state();

    // 스키마 갱신(쓰기 연결)은 수집 스레드가 최초 수집 전에 하고 실패는 알림으로 돌려보냄 (UI 스레드는 기다리지 않음)
    let (ui_tx, ui_rx) = mpsc::channel::<UiMessage>();
    let poll_tx =
        poller::spawn_poller(config.clone(), ui_tx.clone()).context("수집 스레드 시작 실패")?;
    let writer = spawn_state_writer(ui_tx.clone()).context("상태 저장 스레드 시작 실패")?;

    // 2. 창 핸들 보관과 콘솔 글꼴 (클래식 콘솔만 적용, 가드가 종료 때 원래 글꼴로 복원)
    win_ctrl::capture_initial_terminal_window();
    let _font_guard = win_ctrl::apply_console_font(DEFAULT_CONSOLE_FONT).ok();

    // 3. panic 시 화면 복구 후 메시지 출력 (배경 스레드 panic 은 패널로 변환되므로 침묵)
    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        if QUIET_PANIC.with(Cell::get) {
            return;
        }
        restore_terminal();
        previous_hook(panic_info);
    }));

    // 4. 터미널 진입 (마우스 캡처 포함)
    enable_raw_mode().context("raw 모드 진입 실패")?;
    let guard = TerminalGuard { writer };
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        Hide,
        SetTitle("claude-monitor")
    )
    .context("대체 화면 진입 실패")?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("터미널 초기화 실패")?;
    terminal.clear()?;

    // 5. 이벤트 루프
    let outcome = run_event_loop(
        &mut terminal,
        &mut app,
        &ui_rx,
        &ui_tx,
        &poll_tx,
        &guard.writer,
    );
    let _ = panic::take_hook();
    outcome
}

// 4. 이벤트 루프 --------------------------------------------------------------
fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut AppState,
    ui_rx: &Receiver<UiMessage>,
    ui_tx: &Sender<UiMessage>,
    poll_tx: &Sender<PollCommand>,
    writer: &StateWriter,
) -> anyhow::Result<()> {
    let mut dirty = true;
    let mut last_draw = Instant::now();
    while !app.should_quit {
        // 1. 배경 스레드 메시지 반영, 카드 제목 길게 누름 시계, 알림 만료
        while let Ok(message) = ui_rx.try_recv() {
            app.apply_ui_message(message, Instant::now());
            dirty = true;
        }
        if app.tick(Instant::now()) {
            dirty = true;
        }
        // 자정을 넘기면 오늘을 새 날로 옮기고 (오늘 리포트를 보던 중이면 새 날 본문 다시 적재)
        if let Some(command) = app.refresh_today() {
            dispatch_ui_command(app, command, ui_tx, poll_tx, writer);
            dirty = true;
        }
        if app
            .toast
            .as_ref()
            .is_some_and(|toast| Instant::now() >= toast.expires_at)
        {
            app.toast = None;
            dirty = true;
        }

        // 2. 그리기 (변화가 있거나 최대 간격 경과, 크기 변경은 draw 가 자동 반영)
        if dirty || last_draw.elapsed() >= IDLE_REDRAW_INTERVAL {
            terminal.draw(|frame| views::render_frame(frame, app))?;
            dirty = false;
            last_draw = Instant::now();
        }

        // 3. 입력 대기 (알림이 있으면 만료 시각까지, 카드 제목을 누르는 중이면 이동 모드 진입까지 남은 시간과 짧은 쪽)
        let mut wait = match &app.toast {
            Some(toast) => toast
                .expires_at
                .saturating_duration_since(Instant::now())
                .min(FRAME_INTERVAL),
            None => FRAME_INTERVAL,
        };
        if let Some(remaining) = app.press_remaining(Instant::now()) {
            wait = wait.min(remaining);
        }
        if !event::poll(wait)? {
            continue;
        }

        // 4. 입력 처리 (밀린 입력은 한 번에 소화해 다시 그리는 횟수 억제)
        loop {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    let command = app.handle_key_event(key);
                    dispatch_ui_command(app, command, ui_tx, poll_tx, writer);
                    dirty = true;
                }
                Event::Mouse(mouse_event) => {
                    let hover_before = hover_signature(app);
                    let gesture_before = app.card_move.is_some()
                        || app.card_press.is_some()
                        || app.split_drag.is_some()
                        || app.column_drag.is_some()
                        || app.scroll_drag.is_some();
                    let command = app.handle_mouse_event(mouse_event);
                    dispatch_ui_command(app, command, ui_tx, poll_tx, writer);
                    // 이동은 강조 대상이 바뀐 때나 카드 이동·길게 누름·끌기 중이었을 때(확정·취소 가능)만 다시 그림
                    let hover_moved = hover_signature(app) != hover_before;
                    if mouse_event.kind != MouseEventKind::Moved || hover_moved || gesture_before {
                        dirty = true;
                    }
                }
                // 크기 변경 시 경계·칸 위치가 바뀌므로 카드 이동 취소와 강조 해제, 끌던 폭은 저장
                Event::Resize(_, _) => {
                    let command = app.handle_resize();
                    dispatch_ui_command(app, command, ui_tx, poll_tx, writer);
                    dirty = true;
                }
                _ => {}
            }
            // 다른 실행 요청에 밀린 끌기 확정 저장 (탭 전환 등)
            if mem::take(&mut app.save_pending) {
                dispatch_ui_command(app, UiCommand::SaveState, ui_tx, poll_tx, writer);
            }
            if app.should_quit || !event::poll(Duration::ZERO)? {
                break;
            }
        }
    }
    Ok(())
}

// 마우스 이동으로 바뀔 수 있는 강조 상태: 카드, 카드 경계, 열 경계, 스크롤바 트랙
type HoverSignature = (
    Option<CardId>,
    Option<(SplitGroup, usize)>,
    Option<(CardId, usize)>,
    Option<CardId>,
);

fn hover_signature(app: &AppState) -> HoverSignature {
    (
        app.hover_card,
        app.split_hover,
        app.column_hover,
        app.scroll_hover,
    )
}

// 5. 실행 요청 전달 -----------------------------------------------------------
// 모든 I/O 는 배경 스레드로, 결과는 UiMessage 로 돌아옴 (창 제어는 즉시 반환 호출)
fn dispatch_ui_command(
    app: &mut AppState,
    command: UiCommand,
    ui_tx: &Sender<UiMessage>,
    poll_tx: &Sender<PollCommand>,
    writer: &StateWriter,
) {
    match command {
        UiCommand::None => {}
        UiCommand::RefreshNow => {
            let _ = poll_tx.send(PollCommand::RefreshNow);
        }
        UiCommand::SetWindow(window) => {
            let _ = poll_tx.send(PollCommand::SetWindow(window));
        }
        UiCommand::LoadSettings => {
            app.inventory = state::Panel::Pending;
            app.doctor = state::Panel::Pending;
            poller::spawn_settings_load(app.config.clone(), ui_tx.clone());
        }
        UiCommand::LoadReport(date) => {
            // 실시간 여부는 UI 가 지금 오늘로 보는 날 기준 (수집 스레드가 시계를 다시 읽지 않음)
            let live = date == app.today;
            poller::spawn_report_load(app.config.clone(), ui_tx.clone(), date, live);
        }
        UiCommand::GenerateReport(date) => {
            app.show_toast(
                &i18n::report_requested(app.lang, &date.format("%Y-%m-%d").to_string()),
                ToastLevel::Info,
            );
            poller::spawn_admin_action(
                app.config.clone(),
                ui_tx.clone(),
                AdminAction::GenerateReport(date),
                app.today,
            );
        }
        UiCommand::LoadWeeklyReport(week_start) => {
            let live = week_start == crate::report::week_start_of(app.today);
            poller::spawn_weekly_report_load(app.config.clone(), ui_tx.clone(), week_start, live);
        }
        UiCommand::GenerateWeeklyReport(week_start) => {
            app.show_toast(
                &i18n::report_requested(app.lang, &crate::report::format_week_key(week_start)),
                ToastLevel::Info,
            );
            poller::spawn_admin_action(
                app.config.clone(),
                ui_tx.clone(),
                AdminAction::GenerateWeeklyReport(week_start),
                app.today,
            );
        }
        UiCommand::Reindex => {
            app.show_toast(
                i18n::text(app.lang, Msg::ToastReindexRequested),
                ToastLevel::Info,
            );
            poller::spawn_admin_action(
                app.config.clone(),
                ui_tx.clone(),
                AdminAction::Reindex,
                app.today,
            );
        }
        UiCommand::SetTopmost(enabled) => {
            TOPMOST_ACTIVE.store(enabled, Ordering::SeqCst);
            win_ctrl::set_terminal_topmost(enabled);
        }
        // 확정 이벤트에서는 파일 내용만 만들어 저장 스레드로 보냄 (쓰기와 실패 알림은 저장 스레드와 UiMessage 가 맡아 입력 스레드는 기다리지 않음)
        UiCommand::SaveState => match app.build_state_save() {
            Ok(save) => {
                if writer.queue_state_save(save).is_err() {
                    let text = i18n::state_save_failed(app.lang, "state writer stopped");
                    app.show_toast(&text, ToastLevel::Error);
                }
            }
            Err(detail) => {
                let text = i18n::state_save_failed(app.lang, &detail);
                app.show_toast(&text, ToastLevel::Error);
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::poller::AdminOutcome;
    use crate::tui::test_support::sample_config;
    use chrono::NaiveDate;

    #[test]
    fn dispatch_forwards_poll_commands_and_sets_request_toasts() {
        let mut app = AppState::new(
            sample_config(),
            NaiveDate::from_ymd_opt(2026, 11, 15).unwrap(),
        );
        let (ui_tx, ui_rx) = mpsc::channel::<UiMessage>();
        let (poll_tx, poll_rx) = mpsc::channel::<PollCommand>();
        let writer = spawn_state_writer(ui_tx.clone()).unwrap();

        dispatch_ui_command(&mut app, UiCommand::RefreshNow, &ui_tx, &poll_tx, &writer);
        dispatch_ui_command(
            &mut app,
            UiCommand::SetWindow(state::WindowKind::Week),
            &ui_tx,
            &poll_tx,
            &writer,
        );
        assert_eq!(poll_rx.try_recv().unwrap(), PollCommand::RefreshNow);
        assert_eq!(
            poll_rx.try_recv().unwrap(),
            PollCommand::SetWindow(state::WindowKind::Week)
        );

        // 토큰 파일이 없는 임시 폴더: 관리 요청은 즉시 안내 알림으로 끝남
        app.config.data_dir = std::env::temp_dir().join("cm-tui-test-no-token-dir");
        dispatch_ui_command(&mut app, UiCommand::Reindex, &ui_tx, &poll_tx, &writer);
        assert_eq!(app.toast.as_ref().unwrap().text, "Reindex requested");
        let reply = ui_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            reply,
            UiMessage::Admin {
                action: AdminAction::Reindex,
                outcome: AdminOutcome::NoToken
            }
        ));
        app.apply_ui_message(reply, Instant::now());
        let toast = app.toast.as_ref().unwrap();
        assert_eq!(toast.text, "Available after running serve");
        assert_eq!(toast.level, ToastLevel::Error);
    }

    #[test]
    fn dispatch_save_state_queues_the_write_on_the_writer_thread_and_reports_failures() {
        use crate::tui::layout::{IDENTITY_ORDER, SplitGroup};

        let dir = std::env::temp_dir().join(format!("cm-tui-save-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = AppState::new(
            sample_config(),
            NaiveDate::from_ymd_opt(2026, 11, 15).unwrap(),
        );
        app.state_file = dir.join("tui.toml");
        app.card_layout
            .move_card_to_slot(SplitGroup::ToolsRows, &IDENTITY_ORDER, 0, 1);
        let (ui_tx, ui_rx) = mpsc::channel::<UiMessage>();
        let (poll_tx, _poll_rx) = mpsc::channel::<PollCommand>();
        let mut writer = spawn_state_writer(ui_tx.clone()).unwrap();

        // 요청만 보내고 돌아옴, 합류가 끝나면 파일이 있고 실패 메시지는 없음
        dispatch_ui_command(&mut app, UiCommand::SaveState, &ui_tx, &poll_tx, &writer);
        assert!(app.toast.is_none(), "{:?}", app.toast);
        assert_eq!(writer.finish_state_saves(), None);
        let text = std::fs::read_to_string(dir.join("tui.toml")).unwrap();
        assert!(text.contains("tools_rows = [1, 0]"), "{text}");
        assert!(ui_rx.try_recv().is_err());

        // 저장할 수 없는 위치(파일이 폴더 자리를 차지)는 UiMessage 로 돌아와 오류 알림이 됨
        let mut writer = spawn_state_writer(ui_tx.clone()).unwrap();
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        app.state_file = blocker.join("tui.toml");
        dispatch_ui_command(&mut app, UiCommand::SaveState, &ui_tx, &poll_tx, &writer);
        assert!(app.toast.is_none(), "{:?}", app.toast);
        let unresolved = writer.finish_state_saves();
        let message = ui_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(message, UiMessage::StateSaveFailed { .. }));
        app.apply_ui_message(message, Instant::now());
        let toast = app.toast.as_ref().unwrap();
        assert_eq!(toast.level, ToastLevel::Error);
        assert!(
            toast.text.starts_with("Saving UI state failed"),
            "{}",
            toast.text
        );
        assert!(unresolved.is_some());

        // 저장 스레드가 이미 끝났다면 요청이 사라지지 않고 바로 오류 알림
        app.toast = None;
        dispatch_ui_command(&mut app, UiCommand::SaveState, &ui_tx, &poll_tx, &writer);
        let toast = app.toast.as_ref().unwrap();
        assert_eq!(toast.level, ToastLevel::Error);
        assert!(
            toast.text.contains("state writer stopped"),
            "{}",
            toast.text
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resize_during_a_moved_column_drag_dispatches_a_save_of_the_dragged_width() {
        use crate::tui::test_support::sample_state_with_tables;
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent};
        use ratatui::backend::TestBackend;

        let dir = std::env::temp_dir().join(format!("cm-tui-resize-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = sample_state_with_tables();
        app.tab = state::ActiveTab::Hooks;
        app.state_file = dir.join("tui.toml");
        let mut terminal = Terminal::new(TestBackend::new(200, 30)).unwrap();
        terminal
            .draw(|frame| views::render_frame(frame, &mut app))
            .unwrap();
        let grab = app
            .hit_areas
            .column_hits
            .iter()
            .find(|hit| hit.card == CardId::HookTable && hit.column == 0)
            .map(|hit| hit.line)
            .unwrap();
        let at = |kind: MouseEventKind, column: u16| MouseEvent {
            kind,
            column,
            row: grab.y,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse_event(at(MouseEventKind::Down(MouseButton::Left), grab.x));
        app.handle_mouse_event(at(MouseEventKind::Drag(MouseButton::Left), grab.x + 3));
        let (ui_tx, ui_rx) = mpsc::channel::<UiMessage>();
        let (poll_tx, _poll_rx) = mpsc::channel::<PollCommand>();
        let mut writer = spawn_state_writer(ui_tx.clone()).unwrap();

        // 이벤트 루프의 Resize 경로와 같은 호출
        let command = app.handle_resize();
        dispatch_ui_command(&mut app, command, &ui_tx, &poll_tx, &writer);
        assert!(
            app.toast
                .as_ref()
                .is_none_or(|toast| toast.level != ToastLevel::Error)
        );
        assert_eq!(writer.finish_state_saves(), None);
        assert!(ui_rx.try_recv().is_err());
        let text = std::fs::read_to_string(dir.join("tui.toml")).unwrap();
        assert!(text.contains("hook_table"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn moved_without_hover_change_is_not_dirty() {
        use crate::tui::test_support::sample_state_with_snapshot;
        use crossterm::event::{KeyModifiers, MouseEvent};
        use ratatui::backend::TestBackend;

        let mut app = sample_state_with_snapshot();
        let mut terminal = Terminal::new(TestBackend::new(130, 40)).unwrap();
        terminal
            .draw(|frame| views::render_frame(frame, &mut app))
            .unwrap();
        let card_rect = |app: &AppState, wanted: CardId| {
            app.hit_areas
                .card_rects
                .iter()
                .find(|(card, _)| *card == wanted)
                .map(|(_, rect)| *rect)
                .unwrap()
        };
        let moved = |column: u16, row: u16| MouseEvent {
            kind: MouseEventKind::Moved,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };

        // 같은 카드 안 이동은 서명 불변
        let feed = card_rect(&app, CardId::EventFeed);
        app.handle_mouse_event(moved(feed.x + 2, feed.y + 2));
        let before = hover_signature(&app);
        app.handle_mouse_event(moved(feed.x + 3, feed.y + 2));
        assert_eq!(hover_signature(&app), before);

        // 다른 카드로 넘어가면 서명 변경
        let kpi = card_rect(&app, CardId::KpiCache);
        app.handle_mouse_event(moved(kpi.x + 2, kpi.y + 1));
        assert_ne!(hover_signature(&app), before);
    }

    // 실시간 빌드 여부는 UI 가 지금 오늘로 보는 날 기준: 시스템 시각과 다른 오늘(자정 뒤 새 날)도 바로 만들고 이전 날은 저장본만 읽음
    #[test]
    fn report_loads_are_live_for_the_day_and_week_the_ui_calls_today() {
        use crate::core::config::DB_FILE_NAME;
        use crate::store::Store;

        let dir = std::env::temp_dir().join(format!("cm-tui-live-today-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        drop(Store::open_store(&dir.join(DB_FILE_NAME)).unwrap());
        let config = AppConfig {
            data_dir: dir.clone(),
            ..sample_config()
        };
        // UI 의 오늘 = 2026-11-16(월), 시스템 시각과 무관
        let today = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        let yesterday = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let mut app = AppState::new(config, today);
        let (ui_tx, ui_rx) = mpsc::channel::<UiMessage>();
        let (poll_tx, _poll_rx) = mpsc::channel::<PollCommand>();
        let writer = spawn_state_writer(ui_tx.clone()).unwrap();
        let next = || ui_rx.recv_timeout(Duration::from_secs(10)).unwrap();

        dispatch_ui_command(
            &mut app,
            UiCommand::LoadReport(today),
            &ui_tx,
            &poll_tx,
            &writer,
        );
        let UiMessage::Report { date, body } = next() else {
            panic!("report message");
        };
        assert_eq!(date, today);
        assert!(matches!(&body, state::Panel::Ready(Some(text)) if text.contains("2026-11-16")));

        dispatch_ui_command(
            &mut app,
            UiCommand::LoadReport(yesterday),
            &ui_tx,
            &poll_tx,
            &writer,
        );
        let UiMessage::Report { body, .. } = next() else {
            panic!("report message");
        };
        assert!(matches!(body, state::Panel::Ready(None)));

        // 주간: UI 의 이번 주(11-16 시작)는 바로 만들고 지난 주(11-09)는 저장본만
        let this_week = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        let last_week = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        dispatch_ui_command(
            &mut app,
            UiCommand::LoadWeeklyReport(this_week),
            &ui_tx,
            &poll_tx,
            &writer,
        );
        let UiMessage::WeeklyReport { body, .. } = next() else {
            panic!("weekly message");
        };
        assert!(matches!(body, state::Panel::Ready(Some(_))));
        dispatch_ui_command(
            &mut app,
            UiCommand::LoadWeeklyReport(last_week),
            &ui_tx,
            &poll_tx,
            &writer,
        );
        let UiMessage::WeeklyReport { body, .. } = next() else {
            panic!("weekly message");
        };
        assert!(matches!(body, state::Panel::Ready(None)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_console_font_matches_the_sibling_monitor() {
        assert_eq!(DEFAULT_CONSOLE_FONT, "D2Coding ligature");
        assert!(DEFAULT_CONSOLE_FONT.encode_utf16().count() < 32);
    }
}
