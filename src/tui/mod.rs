//! tui/mod.rs
//! ratatui 실시간 관제 화면 (개요, 도구, 훅, 모델, 에이전트, 설정, 리포트), 마우스·창·콘솔 글꼴 제어.

pub mod cards;
pub mod format;
pub mod layout;
pub mod mouse;
pub mod poller;
pub mod sort;
pub mod state;
#[cfg(test)]
pub mod test_support;
pub mod theme;
pub mod views;
pub mod win_ctrl;

use crate::core::config::AppConfig;
use crate::tui::cards::CardId;
use crate::tui::layout::SplitGroup;
use crate::tui::poller::{AdminAction, PollCommand, QUIET_PANIC};
use crate::tui::state::{AppState, ToastLevel, UiCommand, UiMessage};
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
use std::io;
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
// 정상 종료·오류·panic 어느 경로든 Drop 으로 원래 화면 복구
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
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
    // 1. 상태와 배경 수집 스레드
    let mut app = AppState::new(config.clone(), Local::now().date_naive());
    let (ui_tx, ui_rx) = mpsc::channel::<UiMessage>();
    let poll_tx =
        poller::spawn_poller(config.clone(), ui_tx.clone()).context("수집 스레드 시작 실패")?;

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
    let _guard = TerminalGuard;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        Hide,
        SetTitle("rust-agents-proxy")
    )
    .context("대체 화면 진입 실패")?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("터미널 초기화 실패")?;
    terminal.clear()?;

    // 5. 이벤트 루프
    let outcome = run_event_loop(&mut terminal, &mut app, &ui_rx, &ui_tx, &poll_tx);
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
) -> anyhow::Result<()> {
    let mut dirty = true;
    let mut last_draw = Instant::now();
    while !app.should_quit {
        // 1. 배경 스레드 메시지 반영과 알림 만료
        while let Ok(message) = ui_rx.try_recv() {
            app.apply_ui_message(message, Instant::now());
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

        // 3. 입력 대기 (알림이 있으면 만료 시각까지 남은 시간과 짧은 쪽)
        let wait = match &app.toast {
            Some(toast) => toast
                .expires_at
                .saturating_duration_since(Instant::now())
                .min(FRAME_INTERVAL),
            None => FRAME_INTERVAL,
        };
        if !event::poll(wait)? {
            continue;
        }

        // 4. 입력 처리 (밀린 입력은 한 번에 소화해 다시 그리는 횟수 억제)
        loop {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    let command = app.handle_key_event(key);
                    dispatch_ui_command(app, command, ui_tx, poll_tx);
                    dirty = true;
                }
                Event::Mouse(mouse_event) => {
                    let hover_before = hover_signature(app);
                    let command = app.handle_mouse_event(mouse_event);
                    dispatch_ui_command(app, command, ui_tx, poll_tx);
                    // 이동은 강조 대상이 바뀐 때만 다시 그림
                    let hover_moved = hover_signature(app) != hover_before;
                    if mouse_event.kind != MouseEventKind::Moved || hover_moved {
                        dirty = true;
                    }
                }
                // 크기 변경 시 경계 위치가 바뀌므로 강조 해제
                Event::Resize(_, _) => {
                    app.split_hover = None;
                    app.column_hover = None;
                    app.column_drag = None;
                    app.hover_card = None;
                    app.scroll_hover = None;
                    app.scroll_drag = None;
                    dirty = true;
                }
                _ => {}
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
            poller::spawn_report_load(app.config.clone(), ui_tx.clone(), date);
        }
        UiCommand::GenerateReport(date) => {
            app.show_toast(
                &format!("리포트 생성 요청 {}", date.format("%Y-%m-%d")),
                ToastLevel::Info,
            );
            poller::spawn_admin_action(
                app.config.clone(),
                ui_tx.clone(),
                AdminAction::GenerateReport(date),
            );
        }
        UiCommand::Reindex => {
            app.show_toast("재색인 요청", ToastLevel::Info);
            poller::spawn_admin_action(app.config.clone(), ui_tx.clone(), AdminAction::Reindex);
        }
        UiCommand::SetTopmost(enabled) => {
            TOPMOST_ACTIVE.store(enabled, Ordering::SeqCst);
            win_ctrl::set_terminal_topmost(enabled);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        dispatch_ui_command(&mut app, UiCommand::RefreshNow, &ui_tx, &poll_tx);
        dispatch_ui_command(
            &mut app,
            UiCommand::SetWindow(state::WindowKind::Week),
            &ui_tx,
            &poll_tx,
        );
        assert_eq!(poll_rx.try_recv().unwrap(), PollCommand::RefreshNow);
        assert_eq!(
            poll_rx.try_recv().unwrap(),
            PollCommand::SetWindow(state::WindowKind::Week)
        );

        // 토큰 파일이 없는 임시 폴더: 관리 요청은 즉시 안내 알림으로 끝남
        app.config.data_dir = std::env::temp_dir().join("rap-tui-test-no-token-dir");
        dispatch_ui_command(&mut app, UiCommand::Reindex, &ui_tx, &poll_tx);
        assert_eq!(app.toast.as_ref().unwrap().text, "재색인 요청");
        let reply = ui_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            reply,
            UiMessage::Toast { ref text, level: ToastLevel::Error } if text == "serve 실행 후 사용 가능"
        ));
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

    #[test]
    fn default_console_font_matches_the_sibling_monitor() {
        assert_eq!(DEFAULT_CONSOLE_FONT, "D2Coding ligature");
        assert!(DEFAULT_CONSOLE_FONT.encode_utf16().count() < 32);
    }
}
