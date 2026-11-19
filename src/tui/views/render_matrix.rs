//! tui/views/render_matrix.rs
//! 화면 렌더 점검 (테스트 전용): 탭·언어·크기 행렬, 스크롤바·포커스·알림·점 상태 정확 칸, 적중 영역, 소스 규칙 검사.

use super::common::ColumnSpec;
use super::feed::FEED_COLUMNS;
use super::settings::{
    SETTINGS_AGENT_COLUMNS, SETTINGS_DOCTOR_COLUMNS, SETTINGS_HOOK_COLUMNS, SETTINGS_MCP_COLUMNS,
};
use super::{MIN_HEIGHT, MIN_WIDTH, render_frame};
use crate::core::model::LiveEvent;
use crate::inventory::{AgentEntry, DoctorFinding, HookEntry, Inventory, McpEntry};
use crate::store::query::{
    AgentStatsRow, CacheBreakRow, HookStatRow, ModelStatsRow, OverviewStats, ToolLatencyRow,
};
use crate::tui::cards::{CardId, CardKind, CardMove, ColumnHit, HeaderHit, UiAction};
use crate::tui::i18n::{self, Lang};
use crate::tui::layout::{IDENTITY_ORDER, SplitGroup};
use crate::tui::poller::POLL_INTERVAL;
use crate::tui::sort::{SortKind, SortState, sort_spec};
use crate::tui::state::{ActiveTab, AppState, LimitsData, Panel, Toast, ToastLevel, UiCommand};
use crate::tui::test_support::{
    fixed_limits_now, sample_config, sample_state_with_limits, sample_state_with_snapshot,
    sample_state_with_tables,
};
use crate::tui::theme;
use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveDate, SecondsFormat, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SIZES: [(u16, u16); 3] = [(MIN_WIDTH, MIN_HEIGHT), (100, 30), (130, 40)];
const THICK_GLYPHS: [&str; 6] = ["┏", "┓", "┗", "┛", "━", "┃"];
const DOUBLE_GLYPHS: [&str; 6] = ["╔", "╗", "╚", "╝", "═", "║"];

// 1. 공용 도우미 --------------------------------------------------------------
fn draw(state: &mut AppState, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render_frame(frame, state)).unwrap();
    terminal.backend().buffer().clone()
}

// 영역 한 줄의 글자 (넓은 글자 뒤 칸은 건너뜀)
fn rect_text(buffer: &Buffer, y: u16, from: u16, to: u16) -> String {
    let mut text = String::new();
    let mut skip = 0usize;
    for x in from..to {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        let symbol = buffer.cell((x, y)).map_or(" ", |cell| cell.symbol());
        skip = Span::raw(symbol).width().saturating_sub(1);
        text.push_str(symbol);
    }
    text
}

fn row_text(buffer: &Buffer, y: u16) -> String {
    rect_text(buffer, y, 0, buffer.area.width)
}

fn click(state: &mut AppState, column: u16, row: u16) -> UiCommand {
    let event = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    state.handle_mouse_event_at(event, Instant::now())
}

fn lang_name(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "en",
        Lang::Ko => "ko",
    }
}

// 모든 목록 카드가 화면보다 길도록 채운 상태 (표 다섯, 피드, 설정 넷, 리포트)
fn overflowing_state(tab: ActiveTab) -> AppState {
    let mut state = sample_state_with_tables();
    state.tab = tab;
    state.snapshot.tools = Panel::Ready(
        (0..60)
            .map(|index| ToolLatencyRow {
                tool_name: format!("Tool{index:03}"),
                calls: index + 1,
                total_ms: 100,
                ..ToolLatencyRow::default()
            })
            .collect(),
    );
    state.snapshot.hooks = Panel::Ready(
        (0..60)
            .map(|index| HookStatRow {
                route: "pretool".to_string(),
                guard: format!("guard{index:03}"),
                outcome: "deny".to_string(),
                count: index + 1,
                ..HookStatRow::default()
            })
            .collect(),
    );
    state.snapshot.model_stats = Panel::Ready(
        (0..60)
            .map(|index| ModelStatsRow {
                model: format!("claude-m{index:03}"),
                requests: index + 1,
                ..ModelStatsRow::default()
            })
            .collect(),
    );
    state.snapshot.agent_stats = Panel::Ready(
        (0..60)
            .map(|index| AgentStatsRow {
                agent: format!("agent{index:03}"),
                requests: index + 1,
                ..AgentStatsRow::default()
            })
            .collect(),
    );
    state.snapshot.cache_breaks = Panel::Ready(
        (0..60)
            .map(|index| CacheBreakRow {
                ts: "2026-11-15T01:00:00Z".to_string(),
                agent: format!("agent{index:03}"),
                ..CacheBreakRow::default()
            })
            .collect(),
    );
    state.snapshot.events = Panel::Ready(
        (0..80)
            .rev()
            .map(|index| LiveEvent::Notice {
                ts: format!("2026-11-15T01:{:02}:{:02}Z", index / 60, index % 60),
                level: "info".to_string(),
                text: format!("event {index}"),
            })
            .collect(),
    );
    state.inventory = Panel::Ready(Inventory {
        hooks: (0..40)
            .map(|index| HookEntry {
                event: format!("Event{index:02}"),
                hook_type: "command".to_string(),
                target: format!("hook{index}.exe"),
                ..HookEntry::default()
            })
            .collect(),
        mcp_servers: (0..40)
            .map(|index| McpEntry {
                name: format!("server{index:02}"),
                transport: "stdio".to_string(),
                target: format!("server{index}.exe"),
                denied: false,
            })
            .collect(),
        agents: (0..40)
            .map(|index| AgentEntry {
                name: format!("agent{index:02}"),
                model: "sonnet".to_string(),
                ..AgentEntry::default()
            })
            .collect(),
        ..Inventory::default()
    });
    state.doctor = Panel::Ready(
        (0..40)
            .map(|index| DoctorFinding {
                level: "warn".to_string(),
                topic: format!("topic{index:02}"),
                message: format!("message {index}"),
                fix_hint: String::new(),
            })
            .collect(),
    );
    let mut markdown = String::from("# Daily report\n");
    for index in 0..60 {
        markdown.push_str(&format!("\n## Item {index}\nbody\n"));
    }
    state.report = Panel::Ready(Some(markdown));
    state
}

// 2. 크기 행렬 ----------------------------------------------------------------
#[test]
fn every_tab_renders_in_both_languages_at_the_size_matrix_with_consistent_chrome() {
    for lang in [Lang::En, Lang::Ko] {
        for tab in ActiveTab::ALL {
            for (width, height) in SIZES {
                let mut state = sample_state_with_tables();
                state.lang = lang;
                state.tab = tab;
                let buffer = draw(&mut state, width, height);
                let case = format!("{} {tab:?} {width}x{height}", lang_name(lang));
                let frame = Rect::new(0, 0, width, height);

                // 1. 앱 바 배지와 탭 바 (탭마다 적중 영역이 그 탭 글자를 덮음)
                assert!(row_text(&buffer, 0).contains("claude-monitor"), "{case}");
                assert_eq!(state.hit_areas.tab_rects.len(), 8, "{case}");
                for (hit_tab, rect) in &state.hit_areas.tab_rects {
                    let text = rect_text(&buffer, rect.y, rect.x, rect.right());
                    assert_eq!(rect.y, 1, "{case}");
                    assert!(text.contains(hit_tab.tab_label(lang)), "{case}: {text}");
                }

                // 2. 하단 칩: 도움말·종료·언어 칩은 모든 크기에 있고, 칩끼리 겹치지 않으며 글자가 [키] 로 시작
                let chips: Vec<(UiAction, Rect)> = state
                    .hit_areas
                    .button_rects
                    .iter()
                    .filter(|(_, rect)| rect.y == height - 2)
                    .copied()
                    .collect();
                for needed in [UiAction::ShowHelp, UiAction::Quit, UiAction::ToggleLanguage] {
                    assert!(chips.iter().any(|(action, _)| *action == needed), "{case}");
                }
                for (index, (action, rect)) in chips.iter().enumerate() {
                    let text = rect_text(&buffer, rect.y, rect.x, rect.right());
                    assert!(
                        text.trim_start().starts_with('['),
                        "{case} {action:?}: {text}"
                    );
                    for (_, other) in &chips[index + 1..] {
                        assert!(!rect.intersects(*other), "{case} {action:?}");
                    }
                }

                // 3. 모든 적중 영역이 화면 안
                let hit_rects: Vec<Rect> = state
                    .hit_areas
                    .button_rects
                    .iter()
                    .map(|(_, rect)| *rect)
                    .chain(state.hit_areas.header_cells.iter().map(|hit| hit.rect))
                    .chain(state.hit_areas.row_hits.iter().map(|hit| hit.rect))
                    .chain(state.hit_areas.scroll_hits.iter().map(|hit| hit.track))
                    .chain(state.hit_areas.column_hits.iter().map(|hit| hit.grab))
                    .chain(state.hit_areas.title_hits.iter().map(|hit| hit.rect))
                    .chain(state.hit_areas.card_rects.iter().map(|(_, rect)| *rect))
                    .collect();
                for rect in hit_rects {
                    assert_eq!(rect.intersection(frame), rect, "{case}: {rect:?}");
                }

                // 4. 예약 색: 오류 알림이 없으면 적색 채움 없음, 스크롤바 칸은 포커스·강조색이 아님, 포커스색은 굵은 선과 반전 제목에만
                for y in 0..height {
                    for x in 0..width {
                        let cell = buffer.cell((x, y)).unwrap();
                        assert_ne!(cell.bg, theme::COLOR_CRIT_BG, "{case} ({x},{y})");
                        if cell.symbol() == "░" || cell.symbol() == "█" {
                            assert_ne!(cell.fg, theme::BORDER_FOCUS, "{case} ({x},{y})");
                            assert_ne!(cell.fg, theme::FG_ACCENT, "{case} ({x},{y})");
                        }
                        if cell.fg == theme::BORDER_FOCUS {
                            assert!(
                                THICK_GLYPHS.contains(&cell.symbol()),
                                "{case} ({x},{y}): {}",
                                cell.symbol()
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn below_minimum_sizes_show_only_the_centered_gate_line_in_both_languages() {
    for lang in [Lang::En, Lang::Ko] {
        for (width, height) in [(59u16, 16u16), (60, 15), (50, 10)] {
            let mut state = sample_state_with_tables();
            state.lang = lang;
            let buffer = draw(&mut state, width, height);
            let case = format!("{} {width}x{height}", lang_name(lang));
            let expected = match lang {
                Lang::En => format!(
                    "Terminal too small (min {MIN_WIDTH}x{MIN_HEIGHT}, now {width}x{height})"
                ),
                Lang::Ko => format!(
                    "터미널 크기 부족 (최소 {MIN_WIDTH}x{MIN_HEIGHT}, 현재 {width}x{height})"
                ),
            };
            let lines: Vec<(u16, String)> = (0..height)
                .map(|y| (y, row_text(&buffer, y)))
                .filter(|(_, text)| !text.trim().is_empty())
                .collect();
            assert_eq!(lines.len(), 1, "{case}: {lines:?}");
            assert_eq!(lines[0].0, height / 2, "{case}");
            assert_eq!(lines[0].1.trim(), expected, "{case}");

            // 적중 영역 없음, 문구는 경고색
            let notice_x =
                u16::try_from(lines[0].1.chars().position(|letter| letter != ' ').unwrap())
                    .unwrap();
            assert_eq!(
                buffer.cell((notice_x, height / 2)).unwrap().fg,
                theme::COLOR_WARN,
                "{case}"
            );
            assert!(state.hit_areas.tab_rects.is_empty(), "{case}");
            assert!(state.hit_areas.button_rects.is_empty(), "{case}");
            assert!(state.hit_areas.card_rects.is_empty(), "{case}");
        }
    }

    // 아주 작은 화면도 패닉 없이 그림
    for (width, height) in [(1u16, 1u16), (20, 3), (59, 1)] {
        let mut state = sample_state_with_tables();
        draw(&mut state, width, height);
        assert!(state.hit_areas.button_rects.is_empty());
    }
}

// 3. 스크롤바 -----------------------------------------------------------------
#[test]
fn every_overflowing_card_shows_a_gray_scrollbar_gutter_and_a_position_counter() {
    let mut checked: Vec<CardId> = Vec::new();
    let mut one_cell_tracks: Vec<CardId> = Vec::new();
    for lang in [Lang::En, Lang::Ko] {
        for tab in ActiveTab::ALL {
            for (width, height) in SIZES {
                let mut state = overflowing_state(tab);
                state.lang = lang;
                let buffer = draw(&mut state, width, height);
                let case = format!("{} {tab:?} {width}x{height}", lang_name(lang));
                let gray = [
                    theme::SCROLL_TRACK,
                    theme::SCROLL_TRACK_HOT,
                    theme::SCROLL_THUMB,
                    theme::SCROLL_THUMB_FOCUS,
                    theme::SCROLL_THUMB_HOVER,
                    theme::SCROLL_THUMB_DRAG,
                ];
                for (card, rect) in state.hit_areas.card_rects.clone() {
                    let view = state.card_views[card as usize];
                    if card.kind() == CardKind::Static || view.total <= view.visible {
                        continue;
                    }

                    // 본문이 두 줄 이상이면 오른쪽 거터에 트랙과 썸, 한 줄이면 거터의 그 한 칸이 썸 (끌 곳이 없어 클릭 영역은 없음)
                    if view.visible == 1 {
                        let y = rect.bottom() - 2;
                        let cell = buffer.cell((rect.right() - 2, y)).unwrap();
                        assert_eq!(cell.symbol(), "█", "{case} {card:?}: 한 칸 썸 없음");
                        assert!(gray.contains(&cell.fg), "{case} {card:?}");
                        assert_ne!(cell.fg, theme::BORDER_FOCUS, "{case} {card:?}");
                        assert!(
                            state
                                .hit_areas
                                .scroll_hits
                                .iter()
                                .all(|hit| hit.card != card),
                            "{case} {card:?}: 한 칸 트랙에 클릭 영역"
                        );
                        let border = buffer.cell((rect.right() - 1, y)).unwrap();
                        assert!(!["░", "█"].contains(&border.symbol()), "{case} {card:?}");
                        one_cell_tracks.push(card);
                    }
                    if view.visible >= 2 {
                        let hit = state
                            .hit_areas
                            .scroll_hits
                            .iter()
                            .find(|hit| hit.card == card)
                            .unwrap_or_else(|| panic!("{case} {card:?}: 스크롤바 없음"));
                        assert_eq!(hit.track.x, rect.right() - 2, "{case} {card:?}");
                        let mut thumb_cells = 0;
                        for y in hit.track.y..hit.track.bottom() {
                            let cell = buffer.cell((hit.track.x, y)).unwrap();
                            assert!(["░", "█"].contains(&cell.symbol()), "{case} {card:?} y{y}");
                            assert!(gray.contains(&cell.fg), "{case} {card:?} y{y}");
                            thumb_cells += usize::from(cell.symbol() == "█");
                        }
                        assert!(thumb_cells >= 1, "{case} {card:?}");
                        let border = buffer.cell((rect.right() - 1, hit.track.y)).unwrap();
                        assert!(!["░", "█"].contains(&border.symbol()), "{case} {card:?}");
                        checked.push(card);
                    }
                    let counter = row_text(&buffer, rect.bottom() - 1);
                    assert!(
                        counter.contains(&format!("/ {} ", view.total)),
                        "{case} {card:?}: {counter}"
                    );
                }
            }
        }
    }
    checked.sort_by_key(|card| *card as usize);
    checked.dedup();
    for needed in [
        CardId::EventFeed,
        CardId::EventDetail,
        CardId::ToolTable,
        CardId::HookTable,
        CardId::ModelTable,
        CardId::CacheBreaks,
        CardId::AgentTable,
        CardId::SettingsHooks,
        CardId::SettingsMcp,
        CardId::SettingsAgents,
        CardId::SettingsDoctor,
        CardId::ReportBody,
        CardId::ReportOutline,
    ] {
        assert!(checked.contains(&needed), "{needed:?} 는 점검되지 않음");
    }

    // 최소 크기의 낮은 카드(설정 넷, 이벤트 피드)는 한 칸 트랙으로 점검됨
    one_cell_tracks.sort_by_key(|card| *card as usize);
    one_cell_tracks.dedup();
    for needed in [
        CardId::EventFeed,
        CardId::SettingsHooks,
        CardId::SettingsMcp,
        CardId::SettingsAgents,
        CardId::SettingsDoctor,
    ] {
        assert!(
            one_cell_tracks.contains(&needed),
            "{needed:?} 의 한 칸 트랙은 점검되지 않음: {one_cell_tracks:?}"
        );
    }
}

// 4. 제목·상태 점 -------------------------------------------------------------
#[test]
fn long_card_titles_drop_middle_segments_and_keep_the_sort_tail_at_the_minimum_size() {
    let cases = [
        (
            Lang::En,
            ActiveTab::Hooks,
            "Hook verdicts · Deny 3 · Rewrite 45 · Sort Count↓",
        ),
        (
            Lang::En,
            ActiveTab::Agents,
            "Agent stats · 2 kinds · est. (API rates) · Sort Reqs↓",
        ),
        (
            Lang::En,
            ActiveTab::Models,
            "Model stats · est. (API rates) · Sort Reqs↓",
        ),
        (
            Lang::Ko,
            ActiveTab::Hooks,
            "훅 판정 · 거부 3 · 재작성 45 · 절약 3m 09s · 정렬 횟수↓",
        ),
        (
            Lang::Ko,
            ActiveTab::Agents,
            "에이전트 통계 · 2종 · 추정 (API 단가) · 정렬 요청↓",
        ),
    ];
    for (lang, tab, expected) in cases {
        let mut state = sample_state_with_tables();
        state.lang = lang;
        state.tab = tab;
        let buffer = draw(&mut state, MIN_WIDTH, MIN_HEIGHT);
        let top = row_text(&buffer, 2);
        let case = format!("{} {tab:?}: {top}", lang_name(lang));
        assert!(top.contains(expected), "{case}");
        // 제목이 테두리를 넘지 않고 오른쪽 모서리가 온전
        assert!(top.ends_with('┓'), "{case}");
    }

    // 카드 폭보다 긴 이동 중 제목은 이동 표시를 남기고 카드 이름 쪽이 줄어듦
    let mut state = sample_state_with_tables();
    state.lang = Lang::En;
    state.tab = ActiveTab::Hooks;
    state.card_move = Some(CardMove {
        card: CardId::HookTable,
        group: SplitGroup::HooksRows,
        index: 0,
        baseline: IDENTITY_ORDER,
        target: 0,
    });
    let buffer = draw(&mut state, MIN_WIDTH, MIN_HEIGHT);
    let top = row_text(&buffer, 2);
    assert!(top.contains("Moving · "), "{top}");
    assert!(top.contains("Sort Count↓"), "{top}");
    assert_eq!(top.chars().last(), Some('┐'), "{top}");
}

#[test]
fn server_status_card_drops_whole_items_and_marks_a_closed_port_without_color_alone() {
    for lang in [Lang::En, Lang::Ko] {
        let mut state = sample_state_with_snapshot();
        state.lang = lang;
        let buffer = draw(&mut state, MIN_WIDTH, MIN_HEIGHT);
        let case = lang_name(lang);
        let status = row_text(&buffer, 3);
        let ports = row_text(&buffer, 4);

        // 네 포트가 모두 보이고 항목이 중간에서 잘리지 않음 (… 없음)
        for port in ["admin 47330", "otlp 47331", "gateway 47332", "hooks 47333"] {
            assert!(ports.contains(port), "{case}: {ports}");
        }
        assert!(!status.contains('…') && !ports.contains('…'), "{case}");
        assert!(status.contains("pid 4242"), "{case}: {status}");

        // 켜짐 ● 녹색, 닫힘 ○ 굵은 적색 (글자 모양으로도 구분)
        let dot_x = |name: &str| {
            let at = ports.find(&format!(" {name} ")).unwrap();
            u16::try_from(ports[..at].chars().count()).unwrap() - 1
        };
        let up = buffer.cell((dot_x("admin"), 4)).unwrap();
        assert_eq!(up.symbol(), "●", "{case}");
        assert_eq!(up.fg, theme::COLOR_OK, "{case}");
        let down = buffer.cell((dot_x("hooks"), 4)).unwrap();
        assert_eq!(down.symbol(), "○", "{case}");
        assert_eq!(down.fg, theme::COLOR_CRIT, "{case}");
        assert!(down.modifier.contains(Modifier::BOLD), "{case}");

        // 넓으면 간격 3칸과 게이트웨이 표시까지 모두 보임
        let wide = row_text(&draw(&mut state, 130, 40), 4);
        let gateway = match lang {
            Lang::En => "Gateway ON",
            Lang::Ko => "게이트웨이 ON",
        };
        assert!(wide.contains("47330   ● otlp"), "{case}: {wide}");
        assert!(wide.contains(gateway), "{case}: {wide}");
    }
}

#[test]
fn freshness_dot_is_green_when_fresh_warn_with_age_when_stale_and_hollow_when_waiting() {
    let mut state = sample_state_with_snapshot();
    state.lang = Lang::En;
    let dot_of = |buffer: &Buffer| {
        (0..130u16)
            .find(|x| ["●", "○"].contains(&buffer.cell((*x, 0)).unwrap().symbol()))
            .map(|x| buffer.cell((x, 0)).unwrap().clone())
            .unwrap()
    };

    let fresh = dot_of(&draw(&mut state, 130, 40));
    assert_eq!((fresh.symbol(), fresh.fg), ("●", theme::COLOR_OK));

    // 폴링 주기(5초) 3배를 넘기면 경고색과 경과 시간 글자
    state.snapshot.taken_at = Some(Utc::now() - ChronoDuration::seconds(30));
    let buffer = draw(&mut state, 130, 40);
    let stale = dot_of(&buffer);
    assert_eq!((stale.symbol(), stale.fg), ("●", theme::COLOR_WARN));
    assert!(row_text(&buffer, 0).contains("Stale 30s"));

    state.snapshot.taken_at = None;
    let buffer = draw(&mut state, 130, 40);
    let waiting = dot_of(&buffer);
    assert_eq!((waiting.symbol(), waiting.fg), ("○", theme::FG_DIM));
    assert!(row_text(&buffer, 0).contains("Waiting"));
}

// 5. 알림·테두리 종류 ---------------------------------------------------------
#[test]
fn only_the_error_toast_is_a_red_fill_and_toasts_last_four_seconds() {
    let mut state = sample_state_with_snapshot();
    state.lang = Lang::En;
    for (level, dot_color, filled) in [
        (ToastLevel::Info, theme::FG_ACCENT, false),
        (ToastLevel::Success, theme::COLOR_OK, false),
        (ToastLevel::Error, theme::FG_STRONG, true),
    ] {
        state.show_toast("Toast text", level);
        let lifetime = state
            .toast
            .as_ref()
            .unwrap()
            .expires_at
            .saturating_duration_since(Instant::now());
        assert!(
            lifetime <= Duration::from_secs(4) && lifetime > Duration::from_millis(3_900),
            "{lifetime:?}"
        );
        let buffer = draw(&mut state, MIN_WIDTH, MIN_HEIGHT);
        let y = MIN_HEIGHT - 1;
        assert!(
            row_text(&buffer, y).starts_with("  ● Toast text"),
            "{level:?}"
        );
        // 점 칸(1~3열)과 글자 칸: 오류만 적색 채움과 굵은 글자
        for x in 1..=13 {
            let cell = buffer.cell((x, y)).unwrap();
            assert_eq!(cell.bg == theme::COLOR_CRIT_BG, filled, "{level:?} x{x}");
            assert_eq!(cell.bg == theme::BG_TONAL, !filled, "{level:?} x{x}");
        }
        let dot = buffer.cell((2, y)).unwrap();
        assert_eq!(dot.fg, dot_color, "{level:?}");
        assert_eq!(dot.modifier.contains(Modifier::BOLD), filled, "{level:?}");
    }

    // 만료 시각이 지나면 이벤트 루프가 지우는 값 (4초 상수와 일치)
    let expired = Toast {
        text: "x".to_string(),
        level: ToastLevel::Info,
        expires_at: Instant::now() + crate::tui::state::TOAST_LIFETIME,
    };
    assert_eq!(crate::tui::state::TOAST_LIFETIME, Duration::from_secs(4));
    assert!(expired.expires_at > Instant::now());
}

#[test]
fn double_lines_belong_to_the_help_overlay_and_thick_lines_to_the_single_focused_card() {
    // 1. 겹침 창이 없으면 어느 탭에도 이중선이 없고, 포커스 카드는 정확히 하나
    for tab in ActiveTab::ALL {
        let mut state = sample_state_with_tables();
        state.tab = tab;
        let buffer = draw(&mut state, 100, 30);
        let mut thick_corners = 0;
        for y in 0..30 {
            for x in 0..100 {
                let symbol = buffer.cell((x, y)).unwrap().symbol();
                assert!(!DOUBLE_GLYPHS.contains(&symbol), "{tab:?} ({x},{y})");
                thick_corners += usize::from(symbol == "┏");
            }
        }
        assert_eq!(thick_corners, 1, "{tab:?}");
    }

    // 2. 이동 중 카드는 점선, 이중선은 그대로 없음
    let mut state = sample_state_with_tables();
    state.tab = ActiveTab::Settings;
    state.card_move = Some(CardMove {
        card: CardId::SettingsMcp,
        group: SplitGroup::SettingsRows,
        index: 1,
        baseline: IDENTITY_ORDER,
        target: 1,
    });
    let buffer = draw(&mut state, 110, 36);
    let mut dashed = 0;
    for y in 0..36 {
        for x in 0..110 {
            let symbol = buffer.cell((x, y)).unwrap().symbol();
            assert!(!DOUBLE_GLYPHS.contains(&symbol), "move ({x},{y})");
            dashed += usize::from(symbol == "╌" || symbol == "╎");
        }
    }
    assert!(dashed > 0);

    // 3. 도움말은 이중선 상자 하나 (안쪽 칸만 이중선 글자)
    let mut state = sample_state_with_tables();
    state.show_help = true;
    let buffer = draw(&mut state, 100, 30);
    let mut doubles: Vec<(u16, u16)> = Vec::new();
    for y in 0..30 {
        for x in 0..100 {
            if DOUBLE_GLYPHS.contains(&buffer.cell((x, y)).unwrap().symbol()) {
                doubles.push((x, y));
            }
        }
    }
    let left = doubles.iter().map(|(x, _)| *x).min().unwrap();
    let right = doubles.iter().map(|(x, _)| *x).max().unwrap();
    let top = doubles.iter().map(|(_, y)| *y).min().unwrap();
    let bottom = doubles.iter().map(|(_, y)| *y).max().unwrap();
    assert_eq!(buffer.cell((left, top)).unwrap().symbol(), "╔");
    assert_eq!(buffer.cell((right, bottom)).unwrap().symbol(), "╝");
    assert!(left > 0 && right < 99);
}

// 6. 하단 칩·안내 줄 ----------------------------------------------------------
#[test]
fn footer_chip_order_and_hint_tail_hold_at_every_width_from_minimum_to_wide() {
    for lang in [Lang::En, Lang::Ko] {
        for width in MIN_WIDTH..=130 {
            let mut state = sample_state_with_snapshot();
            state.lang = lang;
            let buffer = draw(&mut state, width, MIN_HEIGHT);
            let case = format!("{} {width}", lang_name(lang));
            let chips: Vec<UiAction> = state
                .hit_areas
                .button_rects
                .iter()
                .map(|(action, _)| *action)
                .collect();

            // 왼쪽 칩은 앞에서부터 차례로 남고, 언어 칩은 다른 왼쪽 칩보다 늦게까지 남음, 도움말·종료는 오른쪽 끝에 고정
            let left_order = [
                UiAction::Reindex,
                UiAction::CycleWindow,
                UiAction::RefreshView,
                UiAction::TogglePin,
                UiAction::ResetLayout,
            ];
            let shown_left = left_order
                .iter()
                .filter(|action| chips.contains(action))
                .count();
            assert!(
                left_order[..shown_left]
                    .iter()
                    .all(|action| chips.contains(action)),
                "{case}"
            );
            assert!(chips.contains(&UiAction::ToggleLanguage), "{case}");
            let quit = state
                .hit_areas
                .button_rects
                .iter()
                .find(|(action, _)| *action == UiAction::Quit)
                .map(|(_, rect)| *rect)
                .unwrap();
            assert_eq!(quit.right(), width - 1, "{case}");

            // 꼬리 안내(다음 탭, 전체 단축키)는 폭이 얼마든 항상 보임
            let hints = row_text(&buffer, MIN_HEIGHT - 1);
            let (next_tab, all_keys) = match lang {
                Lang::En => ("Tab Next tab", "? All keys"),
                Lang::Ko => ("Tab 다음 탭", "? 전체 단축키"),
            };
            assert!(
                hints.contains(next_tab) && hints.contains(all_keys),
                "{case}: {hints}"
            );
        }
    }
}

#[test]
fn clicking_every_registered_tab_and_chip_runs_its_action_at_the_minimum_size() {
    for lang in [Lang::En, Lang::Ko] {
        let mut probe = sample_state_with_tables();
        probe.lang = lang;
        draw(&mut probe, MIN_WIDTH, MIN_HEIGHT);
        let tabs = probe.hit_areas.tab_rects.clone();
        let chips = probe.hit_areas.button_rects.clone();
        let case = lang_name(lang);

        for (tab, rect) in tabs {
            let mut state = sample_state_with_tables();
            state.lang = lang;
            draw(&mut state, MIN_WIDTH, MIN_HEIGHT);
            click(&mut state, rect.x + rect.width / 2, rect.y);
            assert_eq!(state.tab, tab, "{case}");
        }
        for (action, rect) in chips {
            let mut state = sample_state_with_tables();
            state.lang = lang;
            draw(&mut state, MIN_WIDTH, MIN_HEIGHT);
            let command = click(&mut state, rect.x + rect.width / 2, rect.y);
            match action {
                UiAction::Quit => assert!(state.should_quit, "{case}"),
                UiAction::ShowHelp => assert!(state.show_help, "{case}"),
                UiAction::ToggleLanguage => {
                    assert_ne!(state.lang, lang, "{case}");
                    assert_eq!(command, UiCommand::SaveState, "{case}");
                }
                UiAction::Reindex => assert_eq!(command, UiCommand::Reindex, "{case}"),
                UiAction::RefreshView => assert_eq!(command, UiCommand::RefreshNow, "{case}"),
                UiAction::CycleWindow => assert!(matches!(command, UiCommand::SetWindow(_))),
                UiAction::TogglePin => assert!(state.always_on_top, "{case}"),
                UiAction::ResetLayout => assert_eq!(command, UiCommand::SaveState, "{case}"),
                other => panic!("{case}: 예상 밖 칩 {other:?}"),
            }
        }
    }
}

// 7. 낮은 화면의 카드 ---------------------------------------------------------
#[test]
fn settings_and_models_cards_keep_a_body_row_at_the_minimum_size() {
    for lang in [Lang::En, Lang::Ko] {
        let mut state = sample_state_with_tables();
        state.lang = lang;
        state.tab = ActiveTab::Settings;
        let buffer = draw(&mut state, MIN_WIDTH, MIN_HEIGHT);
        let case = lang_name(lang);
        for card in [
            CardId::SettingsHooks,
            CardId::SettingsMcp,
            CardId::SettingsAgents,
            CardId::SettingsDoctor,
        ] {
            let rect = state
                .hit_areas
                .card_rects
                .iter()
                .find(|(id, _)| *id == card)
                .map(|(_, rect)| *rect)
                .unwrap();
            assert!(rect.height >= 3, "{case} {card:?}");
            let body = rect_text(&buffer, rect.y + 1, rect.x + 2, rect.right() - 2);
            assert!(
                !body.trim().is_empty(),
                "{case} {card:?}: 본문 줄이 비어 있음"
            );
        }

        state.tab = ActiveTab::Models;
        let buffer = draw(&mut state, MIN_WIDTH, MIN_HEIGHT);
        let rect = state
            .hit_areas
            .card_rects
            .iter()
            .find(|(id, _)| *id == CardId::CacheBreaks)
            .map(|(_, rect)| *rect)
            .unwrap();
        assert!(rect.height >= 4, "{case}");
        let first_row = rect_text(&buffer, rect.y + 2, rect.x + 2, rect.right() - 2);
        assert!(first_row.contains(":00:00"), "{case}: {first_row}");
    }
}

// 8. 표는 상자선 표 (파이프 문자 없음) ----------------------------------------
#[test]
fn no_tab_draws_ascii_pipe_tables() {
    for lang in [Lang::En, Lang::Ko] {
        for tab in ActiveTab::ALL {
            let mut state = sample_state_with_tables();
            state.lang = lang;
            state.tab = tab;
            let buffer = draw(&mut state, 130, 40);
            for y in 0..40 {
                assert!(
                    !row_text(&buffer, y).contains('|'),
                    "{} {tab:?} {y}",
                    lang_name(lang)
                );
            }
        }
    }
}

// 8-1. 적중 영역 --------------------------------------------------------------
#[test]
fn registered_header_row_and_title_hit_areas_respond_to_a_click_at_the_size_matrix() {
    for lang in [Lang::En, Lang::Ko] {
        for tab in ActiveTab::ALL {
            for (width, height) in SIZES {
                let case = format!("{} {tab:?} {width}x{height}", lang_name(lang));
                let fresh = || {
                    let mut state = overflowing_state(tab);
                    state.lang = lang;
                    draw(&mut state, width, height);
                    state
                };
                let probe = fresh();
                let mut clicked = 0;

                // 머리글: 누른 열이 그 카드의 정렬 열이 됨
                for hit in probe.hit_areas.header_cells.clone() {
                    let mut state = fresh();
                    click(&mut state, hit.rect.x + hit.rect.width / 2, hit.rect.y);
                    assert_eq!(
                        state.card_sorts[hit.card as usize].column, hit.column,
                        "{case} {hit:?}"
                    );
                    clicked += 1;
                }

                // 본문 행: 둘째 행을 누르면 그 행이 선택됨
                for hit in probe.hit_areas.row_hits.clone() {
                    if hit.len < 2 || hit.rect.height < 2 {
                        continue;
                    }
                    let mut state = fresh();
                    click(&mut state, hit.rect.x + 1, hit.rect.y + 1);
                    assert_eq!(
                        state.card_views[hit.card as usize].selected,
                        hit.offset + 1,
                        "{case} {hit:?}"
                    );
                    clicked += 1;
                }

                // 제목 글자: 순서를 바꿀 수 있는 카드는 길게 누름 대기가 시작됨
                for hit in probe.hit_areas.title_hits.clone() {
                    let Some((group, _)) = hit.card.reorder_slot() else {
                        continue;
                    };
                    if !probe
                        .hit_areas
                        .slot_groups
                        .iter()
                        .any(|slots| slots.group == group)
                    {
                        continue;
                    }
                    let mut state = fresh();
                    click(&mut state, hit.rect.x, hit.rect.y);
                    let press = state.card_press.unwrap_or_else(|| panic!("{case} {hit:?}"));
                    assert_eq!(press.card, hit.card, "{case}");
                    clicked += 1;
                }
                assert!(clicked > 0, "{case}");

                // 같은 탭의 카드는 서로 겹치지 않음
                let cards = &probe.hit_areas.card_rects;
                for (index, (first, first_rect)) in cards.iter().enumerate() {
                    for (second, second_rect) in &cards[index + 1..] {
                        assert!(
                            !first_rect.intersects(*second_rect),
                            "{case}: {first:?} {first_rect:?} / {second:?} {second_rect:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn scrollbar_track_click_pages_and_the_thumb_grab_starts_a_drag_at_the_size_matrix() {
    for lang in [Lang::En, Lang::Ko] {
        for tab in ActiveTab::ALL {
            for (width, height) in SIZES {
                let case = format!("{} {tab:?} {width}x{height}", lang_name(lang));
                let fresh = || {
                    let mut state = overflowing_state(tab);
                    state.lang = lang;
                    draw(&mut state, width, height);
                    state
                };
                let probe = fresh();
                for hit in probe.hit_areas.scroll_hits.clone() {
                    let thumb = crate::tui::cards::scroll_thumb(
                        hit.total,
                        hit.visible,
                        hit.scroll,
                        hit.track.height,
                    )
                    .unwrap();

                    // 썸 위를 누르면 드래그 시작
                    let mut state = fresh();
                    click(&mut state, hit.track.x, hit.track.y + thumb.start);
                    assert_eq!(
                        state.scroll_drag.map(|drag| drag.card),
                        Some(hit.card),
                        "{case} {hit:?}"
                    );

                    // 썸 아래 트랙을 누르면 한 화면 아래로
                    let below = thumb.start + thumb.len;
                    if below < hit.track.height {
                        let mut state = fresh();
                        click(&mut state, hit.track.x, hit.track.y + below);
                        assert_eq!(
                            state.card_views[hit.card as usize].scroll,
                            hit.visible.min(hit.total - hit.visible),
                            "{case} {hit:?}"
                        );
                    }
                }
            }
        }
    }
}

// 6-1. 앱 바 신선도 -----------------------------------------------------------
#[test]
fn freshness_turns_stale_only_after_three_poll_intervals() {
    let limit = i64::try_from(POLL_INTERVAL.as_secs() * 3).unwrap();
    let mut state = sample_state_with_snapshot();
    state.lang = Lang::En;

    state.snapshot.taken_at = Some(Utc::now() - ChronoDuration::seconds(limit - 1));
    let top = row_text(&draw(&mut state, 130, 40), 0);
    assert!(!top.contains("Stale"), "{top}");

    state.snapshot.taken_at = Some(Utc::now() - ChronoDuration::seconds(limit + 2));
    let top = row_text(&draw(&mut state, 130, 40), 0);
    assert!(top.contains("Stale"), "{top}");
}

// 6-2. 카드 테두리 보조 문구 --------------------------------------------------
#[test]
fn card_border_hint_is_dropped_before_the_title_when_the_card_is_narrow() {
    for (lang, title, hint) in [
        (Lang::En, "Live events (4)", "API 1 · Tools 1"),
        (Lang::Ko, "실시간 이벤트 (4건)", "API 1 · 도구 1"),
    ] {
        let mut state = sample_state_with_snapshot();
        state.lang = lang;
        let top_of = |state: &mut AppState, width: u16, height: u16| {
            let buffer = draw(state, width, height);
            let rect = state
                .hit_areas
                .card_rects
                .iter()
                .find(|(id, _)| *id == CardId::EventFeed)
                .map(|(_, rect)| *rect)
                .unwrap();
            rect_text(&buffer, rect.y, rect.x, rect.right())
        };
        let wide = top_of(&mut state, 130, 40);
        assert!(wide.contains(title) && wide.contains(hint), "{wide}");
        let narrow = top_of(&mut state, MIN_WIDTH, MIN_HEIGHT);
        assert!(narrow.contains(title) && !narrow.contains(hint), "{narrow}");
    }
}

// 9. 소스 규칙 검사 -----------------------------------------------------------
fn tui_source_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tui"),
        &mut found,
    );
    found.sort();
    found
}

// 테스트 모듈 앞까지의 본문
fn non_test_code(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap();
    match text.find("#[cfg(test)]") {
        Some(end) => text[..end].to_string(),
        None => text,
    }
}

#[test]
fn views_use_no_raw_colors_and_no_inline_korean_display_strings() {
    let files = tui_source_files();
    assert!(files.len() >= 20, "{}", files.len());
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // 팔레트 정의 파일, 화면에 쓰지 않는 콘솔 글꼴 오류 문구, 테스트 전용 합성 값은 제외
        if [
            "theme.rs",
            "win_ctrl.rs",
            "test_support.rs",
            "render_matrix.rs",
            "dump_probe.rs",
        ]
        .contains(&name.as_str())
        {
            continue;
        }
        let code = non_test_code(path);
        for pattern in ["Color::", "Indexed(", "Rgb("] {
            assert!(!code.contains(pattern), "{name}: 팔레트 밖 색 {pattern}");
        }
        if name == "i18n.rs" {
            continue;
        }
        for (index, line) in code.lines().enumerate() {
            let body = line.split("//").next().unwrap_or("");
            let hangul = body
                .chars()
                .any(|letter| ('\u{AC00}'..='\u{D7A3}').contains(&letter));
            assert!(
                !hangul,
                "{name}:{}: 화면 문구는 카탈로그에서만: {line}",
                index + 1
            );
        }
    }
}

#[test]
fn source_files_carry_their_path_header_and_the_src_root_holds_only_main_and_lib() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut root_files: Vec<String> = fs::read_dir(&src)
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    root_files.sort();
    assert_eq!(root_files, ["lib.rs", "main.rs"]);

    for path in tui_source_files() {
        let relative = path
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = fs::read_to_string(&path).unwrap();
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some(format!("//! {relative}").as_str()),
            "{relative}"
        );
        assert!(
            lines.next().is_some_and(|line| line.starts_with("//! ")),
            "{relative}"
        );
    }
}

// 소스 규칙 검사가 쓰는 설정 생성이 테스트 도우미와 어긋나지 않는지 (사용하지 않는 import 방지)
#[test]
fn render_matrix_state_helpers_build_the_expected_defaults() {
    let state = AppState::new(
        sample_config(),
        NaiveDate::from_ymd_opt(2026, 11, 15).unwrap(),
    );
    assert_eq!(state.lang, Lang::En);
    assert_eq!(overflowing_state(ActiveTab::Tools).tab, ActiveTab::Tools);
}

// 10. 이벤트 피드·설정 표 카드 ------------------------------------------------
// 다섯 표 카드와 같은 열 경계 드래그·머리글 정렬·자연 폭 맞춤·l 초기화를 받은 카드 (이벤트 피드, 설정 넷)
const NEW_TABLE_CARDS: [(ActiveTab, CardId); 5] = [
    (ActiveTab::Overview, CardId::EventFeed),
    (ActiveTab::Settings, CardId::SettingsHooks),
    (ActiveTab::Settings, CardId::SettingsMcp),
    (ActiveTab::Settings, CardId::SettingsAgents),
    (ActiveTab::Settings, CardId::SettingsDoctor),
];

fn columns_of(card: CardId) -> Vec<ColumnSpec> {
    match card {
        CardId::EventFeed => FEED_COLUMNS.to_vec(),
        CardId::SettingsHooks => SETTINGS_HOOK_COLUMNS.to_vec(),
        CardId::SettingsMcp => SETTINGS_MCP_COLUMNS.to_vec(),
        CardId::SettingsAgents => SETTINGS_AGENT_COLUMNS.to_vec(),
        CardId::SettingsDoctor => SETTINGS_DOCTOR_COLUMNS.to_vec(),
        other => panic!("열 명세가 없는 카드: {other:?}"),
    }
}

fn card_rect_of(state: &AppState, card: CardId) -> Rect {
    state
        .hit_areas
        .card_rects
        .iter()
        .find(|(id, _)| *id == card)
        .map(|(_, rect)| *rect)
        .unwrap_or_else(|| panic!("{card:?} 가 그려지지 않음"))
}

// 카드 안쪽(좌우 테두리와 여백 제외) 한 줄의 글자
fn card_inner_row(buffer: &Buffer, state: &AppState, card: CardId, y: u16) -> String {
    let rect = card_rect_of(state, card);
    rect_text(buffer, y, rect.x + 2, rect.right() - 2)
}

fn header_hits_of(state: &AppState, card: CardId) -> Vec<HeaderHit> {
    state
        .hit_areas
        .header_cells
        .iter()
        .filter(|hit| hit.card == card)
        .copied()
        .collect()
}

fn column_hits_of(state: &AppState, card: CardId) -> Vec<ColumnHit> {
    state
        .hit_areas
        .column_hits
        .iter()
        .filter(|hit| hit.card == card)
        .copied()
        .collect()
}

fn mouse_event(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn event_feed_and_settings_cards_draw_header_labels_and_natural_width_rows_at_the_size_matrix() {
    // 큰 화면에서 두 언어 모두 줄 안에 말줄임 없이 보여야 하는 조각 (카드마다)
    let fragments = |card: CardId| -> &'static [&'static str] {
        match card {
            CardId::EventFeed => &["sonnet-5-5", "2.1s"],
            CardId::SettingsHooks => &["PreToolUse", "http://127.0.0.1:47333/pretool"],
            CardId::SettingsMcp => &["outline", "rust-fs-mcp.exe"],
            CardId::SettingsAgents => &["worker", "sonnet"],
            CardId::SettingsDoctor => &["47333", "→"],
            other => panic!("{other:?}"),
        }
    };
    for lang in [Lang::En, Lang::Ko] {
        for (tab, card) in NEW_TABLE_CARDS {
            for (width, height) in SIZES {
                let case = format!("{} {card:?} {width}x{height}", lang_name(lang));
                let mut state = sample_state_with_tables();
                state.lang = lang;
                state.tab = tab;
                let buffer = draw(&mut state, width, height);
                let rect = card_rect_of(&state, card);
                let hits = header_hits_of(&state, card);
                let shown: Vec<usize> = hits.iter().map(|hit| hit.column).collect();

                // 1. 낮은 설정 카드(최소 크기)는 머리글 대신 첫 본문 줄, 그 밖은 오른쪽 열부터 숨은 앞쪽 열 묶음의 머리글
                let expected = match (card, width) {
                    (CardId::EventFeed, 60) => 4,
                    (CardId::EventFeed, 100) => 7,
                    (CardId::EventFeed, _) => 8,
                    (_, 60) => 0,
                    (other, _) => columns_of(other).len(),
                };
                assert_eq!(shown, (0..expected).collect::<Vec<usize>>(), "{case}");
                if expected == 0 {
                    assert!(column_hits_of(&state, card).is_empty(), "{case}");
                    let first_row = card_inner_row(&buffer, &state, card, rect.y + 1);
                    assert!(!first_row.trim().is_empty(), "{case}");
                    continue;
                }

                // 2. 머리글 줄: 보이는 열의 문구가 모두 있고 정렬 열에 화살표
                let header = card_inner_row(&buffer, &state, card, rect.y + 1);
                let columns = columns_of(card);
                for column in &shown {
                    let label = i18n::text(lang, columns[*column].0);
                    assert!(header.contains(label), "{case}: {label} / {header}");
                }
                let sort = state.card_sorts[card as usize];
                let sorted_label = i18n::text(lang, columns[sort.column].0);
                assert!(
                    header.contains(&format!("{sorted_label}{}", sort.sort_arrow())),
                    "{case}: {header}"
                );

                // 3. 본문: 큰 화면에서는 모든 조각이 자연 폭으로 보임
                if width >= 100 {
                    let body: String = (rect.y + 2..rect.bottom() - 1)
                        .map(|y| card_inner_row(&buffer, &state, card, y))
                        .collect::<Vec<_>>()
                        .join("\n");
                    for fragment in fragments(card) {
                        assert!(body.contains(fragment), "{case}: {fragment} / {body}");
                    }
                    if width >= 130 {
                        assert!(!body.contains('…'), "{case}: {body}");
                    }
                }
            }
        }
    }
}

#[test]
fn feed_hook_row_keeps_route_guard_and_detail_in_the_name_cell_and_the_verdict_in_the_agent_cell() {
    for lang in [Lang::En, Lang::Ko] {
        let mut state = sample_state_with_tables();
        state.lang = lang;
        let buffer = draw(&mut state, 130, 40);
        let rect = card_rect_of(&state, CardId::EventFeed);
        let rows: Vec<String> = (rect.y + 2..rect.bottom() - 1)
            .map(|y| card_inner_row(&buffer, &state, CardId::EventFeed, y))
            .collect();
        let verdict = i18n::outcome_label(lang, "rewrite");
        let hook_row = rows
            .iter()
            .find(|row| row.contains("pretool·cliguard"))
            .unwrap_or_else(|| panic!("{}: 훅 행 없음 {rows:?}", lang_name(lang)));
        assert!(
            hook_row.contains("pretool·cliguard grep -> rg"),
            "{hook_row}"
        );
        assert!(hook_row.contains(verdict), "{hook_row}");
        // 이름 칸 글자는 판정색(재작성 녹색), 상세는 흐림
        let name_cells = Span::raw(&hook_row[..hook_row.find("pretool").unwrap()]).width();
        let name_x = rect.x + 2 + u16::try_from(name_cells).unwrap();
        let hook_index = rows.iter().position(|row| row == hook_row).unwrap();
        let hook_y = rect.y + 2 + u16::try_from(hook_index).unwrap();
        assert_eq!(buffer.cell((name_x, hook_y)).unwrap().fg, theme::COLOR_OK);
        let detail_x = name_x + u16::try_from("pretool·cliguard ".chars().count()).unwrap();
        assert_eq!(buffer.cell((detail_x, hook_y)).unwrap().fg, theme::FG_DIM);
    }
}

#[test]
fn new_table_cards_register_header_and_boundary_hit_areas_that_match_the_drawn_columns() {
    for lang in [Lang::En, Lang::Ko] {
        for (tab, card) in NEW_TABLE_CARDS {
            for (width, height) in SIZES {
                let case = format!("{} {card:?} {width}x{height}", lang_name(lang));
                let mut state = sample_state_with_tables();
                state.lang = lang;
                state.tab = tab;
                let buffer = draw(&mut state, width, height);
                let headers = header_hits_of(&state, card);
                let bounds = column_hits_of(&state, card);

                // 보이는 열 사이마다 경계 하나 (마지막 열 오른쪽은 없음), 머리글 줄이 없으면 둘 다 없음
                assert_eq!(bounds.len(), headers.len().saturating_sub(1), "{case}");
                for (index, bound) in bounds.iter().enumerate() {
                    let left = &headers[index];
                    assert_eq!(bound.column, left.column, "{case}");
                    assert_eq!(bound.width, left.rect.width, "{case}");
                    assert_eq!(bound.line.x, left.rect.right(), "{case}");
                    assert_eq!(bound.line.y, left.rect.y, "{case}");
                    assert!(
                        bound.min <= bound.width && bound.width <= bound.max,
                        "{case}"
                    );
                    assert!(bound.line.height >= 2, "{case}");

                    // 잡는 칸: 머리글 한 행, 경계 칸을 포함해 양옆 열로 최대 한 칸씩, 양옆 머리글 밖으로 나가지 않음
                    let right = &headers[index + 1];
                    assert_eq!(
                        (bound.grab.y, bound.grab.height),
                        (bound.line.y, 1),
                        "{case}"
                    );
                    assert!(bound.grab.x <= bound.line.x, "{case}");
                    assert!(bound.line.x < bound.grab.right(), "{case}");
                    assert!((1..=3).contains(&bound.grab.width), "{case}");
                    assert!(bound.grab.x > left.rect.x, "{case}");
                    assert!(bound.grab.right() < right.rect.right(), "{case}");

                    // 오른쪽 이웃 머리글이 그 바로 다음 칸에서 시작하고, 경계 칸은 평소 흐린 │
                    assert_eq!(right.rect.x, bound.line.x + 1, "{case}");
                    let cell = buffer.cell((bound.line.x, bound.line.y)).unwrap();
                    assert_eq!(cell.symbol(), "│", "{case}");
                    assert_eq!(cell.fg, theme::FG_FAINT, "{case}");
                }
            }
        }
    }
}

#[test]
fn dragging_a_new_card_boundary_resizes_the_column_and_a_quick_second_click_resets_to_auto() {
    for (tab, card) in NEW_TABLE_CARDS {
        let case = format!("{card:?}");
        let mut state = sample_state_with_tables();
        state.lang = Lang::En;
        state.tab = tab;
        draw(&mut state, 130, 40);
        let bound = column_hits_of(&state, card)[0];
        let before = header_hits_of(&state, card)[0].rect.width;
        assert_eq!(bound.width, before, "{case}");
        let start = Instant::now();
        let after = |millis: u64| start + Duration::from_millis(millis);
        let down = |x: u16| mouse_event(MouseEventKind::Down(MouseButton::Left), x, bound.grab.y);

        // 끌기: 폭 +3, 놓으면 저장 요청
        state.handle_mouse_event_at(down(bound.line.x), after(0));
        state.handle_mouse_event_at(
            mouse_event(
                MouseEventKind::Drag(MouseButton::Left),
                bound.line.x + 3,
                bound.grab.y,
            ),
            after(10),
        );
        let released = state.handle_mouse_event_at(
            mouse_event(
                MouseEventKind::Up(MouseButton::Left),
                bound.line.x + 3,
                bound.grab.y,
            ),
            after(20),
        );
        assert_eq!(released, UiCommand::SaveState, "{case}");
        assert_eq!(
            state.column_widths[card as usize][0],
            Some(before + 3),
            "{case}"
        );
        draw(&mut state, 130, 40);
        assert_eq!(
            header_hits_of(&state, card)[0].rect.width,
            before + 3,
            "{case}"
        );

        // 끌지 않은 클릭 뒤 곧바로 다시 누르면 자동 폭 복원
        let again = column_hits_of(&state, card)[0];
        state.handle_mouse_event_at(down(again.line.x), after(100));
        state.handle_mouse_event_at(
            mouse_event(
                MouseEventKind::Up(MouseButton::Left),
                again.line.x,
                again.grab.y,
            ),
            after(110),
        );
        let reset = state.handle_mouse_event_at(down(again.line.x), after(200));
        assert_eq!(reset, UiCommand::SaveState, "{case}");
        assert!(
            state.column_widths[card as usize]
                .iter()
                .all(Option::is_none),
            "{case}"
        );
        draw(&mut state, 130, 40);
        assert_eq!(header_hits_of(&state, card)[0].rect.width, before, "{case}");
    }
}

#[test]
fn new_table_card_headers_sort_numbers_descending_first_and_text_ascending_first_then_reset() {
    for (tab, card) in NEW_TABLE_CARDS {
        let spec = sort_spec(card).unwrap();
        let mut state = sample_state_with_tables();
        state.lang = Lang::En;
        state.tab = tab;
        draw(&mut state, 130, 40);
        let hits = header_hits_of(&state, card);
        let columns = columns_of(card);
        let mut tested = 0;
        for kind in [SortKind::Number, SortKind::Text] {
            let Some(hit) = hits
                .iter()
                .find(|hit| spec.kinds[hit.column] == kind && hit.column != spec.default.column)
            else {
                continue;
            };
            let first = kind == SortKind::Number;
            state.card_sorts[card as usize] = spec.default;
            let steps = [
                SortState {
                    column: hit.column,
                    descending: first,
                },
                SortState {
                    column: hit.column,
                    descending: !first,
                },
                spec.default,
            ];
            for expected in steps {
                click(&mut state, hit.rect.x + hit.rect.width / 2, hit.rect.y);
                assert_eq!(
                    state.card_sorts[card as usize], expected,
                    "{card:?} {kind:?} column {}",
                    hit.column
                );

                // 머리글 문구에 정렬 화살표가 따라가고 제목에도 정렬 열이 표시됨
                let buffer = draw(&mut state, 130, 40);
                let rect = card_rect_of(&state, card);
                let header = card_inner_row(&buffer, &state, card, rect.y + 1);
                let label = i18n::text(Lang::En, columns[expected.column].0);
                assert!(
                    header.contains(&format!("{label}{}", expected.sort_arrow())),
                    "{card:?}: {header}"
                );
                let title = row_text(&buffer, rect.y);
                assert!(
                    title.contains(&format!("Sort {label}")),
                    "{card:?}: {title}"
                );
            }
            tested += 1;
        }
        assert!(tested >= 1, "{card:?}");
    }
}

#[test]
fn the_l_key_resets_dragged_widths_of_the_new_cards_on_their_own_tab_only() {
    let press = |state: &mut AppState, letter: char| {
        state.handle_key_event(KeyEvent::new(KeyCode::Char(letter), KeyModifiers::NONE))
    };
    let mut state = sample_state_with_tables();
    for (_, card) in NEW_TABLE_CARDS {
        state.column_widths[card as usize][1] = Some(21);
    }

    // 개요 탭: 이벤트 피드만 초기화
    state.tab = ActiveTab::Overview;
    assert_eq!(press(&mut state, 'l'), UiCommand::SaveState);
    assert!(
        state.column_widths[CardId::EventFeed as usize]
            .iter()
            .all(Option::is_none)
    );
    for card in [
        CardId::SettingsHooks,
        CardId::SettingsMcp,
        CardId::SettingsAgents,
        CardId::SettingsDoctor,
    ] {
        assert_eq!(state.column_widths[card as usize][1], Some(21), "{card:?}");
    }

    // 설정 탭: 설정 네 카드가 초기화되고 언어는 그대로 (L 이 아님)
    state.tab = ActiveTab::Settings;
    state.lang = Lang::Ko;
    assert_eq!(press(&mut state, 'l'), UiCommand::SaveState);
    for card in [
        CardId::SettingsHooks,
        CardId::SettingsMcp,
        CardId::SettingsAgents,
        CardId::SettingsDoctor,
    ] {
        assert!(
            state.column_widths[card as usize]
                .iter()
                .all(Option::is_none),
            "{card:?}"
        );
    }
    assert_eq!(state.lang, Lang::Ko);
}

#[test]
fn text_columns_shrink_with_an_ellipsis_before_the_numbers_and_the_feed_keeps_its_name_floor() {
    for lang in [Lang::En, Lang::Ko] {
        // 설정 훅 카드: 아주 긴 대상은 끝 말줄임, 비동기·타임아웃 칸은 그대로
        let mut state = sample_state_with_tables();
        state.lang = lang;
        state.tab = ActiveTab::Settings;
        let long_target = format!("http://127.0.0.1:47333/{}", "x".repeat(100));
        if let Panel::Ready(inventory) = &mut state.inventory {
            inventory.hooks[0].target = long_target.clone();
            inventory.hooks[0].timeout_secs = Some(5);
        }
        let buffer = draw(&mut state, 100, 30);
        let rect = card_rect_of(&state, CardId::SettingsHooks);
        let body: Vec<String> = (rect.y + 2..rect.bottom() - 1)
            .map(|y| card_inner_row(&buffer, &state, CardId::SettingsHooks, y))
            .collect();
        let row = body
            .iter()
            .find(|row| row.contains("http://127.0.0.1:47333/"))
            .unwrap_or_else(|| panic!("{}: {body:?}", lang_name(lang)));
        assert!(row.contains('…'), "{row}");
        assert!(!row.contains(&long_target), "{row}");
        assert!(row.trim_end().ends_with("5s"), "{row}");

        // 이벤트 피드: 긴 훅 상세는 이름 칸에서만 줄고, 이름은 지킬 폭 아래로 줄지 않으며 소요 칸은 그대로
        let mut state = sample_state_with_tables();
        state.lang = lang;
        if let Panel::Ready(events) = &mut state.snapshot.events {
            for event in events.iter_mut() {
                if let LiveEvent::Hook(row) = event {
                    row.detail = "d".repeat(120);
                    row.saved_ms = Some(1_500);
                }
            }
        }
        let buffer = draw(&mut state, 100, 30);
        let rect = card_rect_of(&state, CardId::EventFeed);
        let rows: Vec<String> = (rect.y + 2..rect.bottom() - 1)
            .map(|y| card_inner_row(&buffer, &state, CardId::EventFeed, y))
            .collect();
        let hook_row = rows
            .iter()
            .find(|row| row.contains("pretool·"))
            .unwrap_or_else(|| panic!("{}: {rows:?}", lang_name(lang)));
        assert!(hook_row.contains('…'), "{hook_row}");
        assert!(hook_row.contains("+1.5s"), "{hook_row}");
        let name_hit = header_hits_of(&state, CardId::EventFeed)[2];
        assert!(name_hit.rect.width >= 8, "{}", name_hit.rect.width);
    }
}

// 10-1. 최상단 고정 표시 ------------------------------------------------------
// 한 줄의 [from, to) 칸을 글자 칸 단위로 훑어 모두 PIN_BG 바탕·굵은 흰 글자인지 확인하고 글자 칸 수를 돌려줌
// (넓은 글자의 뒤 칸은 글자 폭만큼 건너뜀)
fn count_pinned_cells(buffer: &Buffer, y: u16, from: u16, to: u16, case: &str) -> usize {
    let mut x = from;
    let mut counted = 0;
    while x < to {
        let cell = buffer.cell((x, y)).unwrap();
        assert_eq!(cell.bg, theme::PIN_BG, "{case} x{x}");
        assert_eq!(cell.fg, theme::FG_STRONG, "{case} x{x}");
        assert!(cell.modifier.contains(Modifier::BOLD), "{case} x{x}");
        x += u16::try_from(Span::raw(cell.symbol()).width().max(1)).unwrap();
        counted += 1;
    }
    assert_eq!(x, to, "{case}: 글자 칸이 구간 끝에 맞지 않음");
    counted
}

#[test]
fn pinned_badge_and_chip_use_their_own_fill_with_the_word_and_never_the_warn_color() {
    for lang in [Lang::En, Lang::Ko] {
        let (badge_word, chip_word, off_word) = match lang {
            Lang::En => ("Pinned", "Pin ON", "Pin OFF"),
            Lang::Ko => ("최상단", "최상단 ON", "최상단 OFF"),
        };
        for (width, height) in SIZES {
            let case = format!("{} {width}x{height}", lang_name(lang));
            for stale in [false, true] {
                let case = format!("{case} stale={stale}");
                let mut state = sample_state_with_snapshot();
                state.lang = lang;
                state.always_on_top = true;
                if stale {
                    state.snapshot.taken_at = Some(Utc::now() - ChronoDuration::seconds(60));
                }
                let buffer = draw(&mut state, width, height);

                // 1. 앱 바 배지: 낱말 그대로, 채움은 PIN_BG 이고 글자는 굵은 흰색 (넓은 글자의 뒤 칸은 글자 폭만큼 건너뜀)
                let badge_cells: Vec<u16> = (0..width)
                    .filter(|x| buffer.cell((*x, 0)).unwrap().bg == theme::PIN_BG)
                    .collect();
                assert!(!badge_cells.is_empty(), "{case}");
                let badge_from = badge_cells[0];
                let badge_last = badge_cells[badge_cells.len() - 1];
                let last_width = Span::raw(buffer.cell((badge_last, 0)).unwrap().symbol()).width();
                let badge_to = badge_last + u16::try_from(last_width.max(1)).unwrap();
                assert_eq!(
                    rect_text(&buffer, 0, badge_from, badge_to),
                    format!(" {badge_word} "),
                    "{case}"
                );
                let badge_steps = count_pinned_cells(&buffer, 0, badge_from, badge_to, &case);
                assert_eq!(badge_steps, badge_cells.len(), "{case}");

                // 2. 경고색은 글자 전용: 어느 칸도 경고 채움이 아니고, 지연 표시 점만 경고색 글자
                let mut pinned_below = 0;
                for y in 0..height {
                    for x in 0..width {
                        let cell = buffer.cell((x, y)).unwrap();
                        assert_ne!(cell.bg, theme::COLOR_WARN, "{case} ({x},{y})");
                        if y > 0 && cell.bg == theme::PIN_BG {
                            pinned_below += 1;
                        }
                    }
                }
                let dot = (0..width)
                    .map(|x| buffer.cell((x, 0)).unwrap())
                    .find(|cell| cell.symbol() == "●")
                    .unwrap();
                assert_eq!(
                    dot.fg,
                    if stale {
                        theme::COLOR_WARN
                    } else {
                        theme::COLOR_OK
                    },
                    "{case}"
                );

                // 3. 하단 칩 (칩이 보이는 크기에서): 키 글자와 낱말 전부 PIN_BG, 다른 곳에는 PIN_BG 없음
                let chip = state
                    .hit_areas
                    .button_rects
                    .iter()
                    .find(|(action, _)| *action == UiAction::TogglePin)
                    .map(|(_, rect)| *rect);
                if let Some(chip) = chip {
                    let text = rect_text(&buffer, chip.y, chip.x, chip.right());
                    assert!(
                        text.contains("[t]") && text.contains(chip_word),
                        "{case}: {text}"
                    );
                    let chip_steps =
                        count_pinned_cells(&buffer, chip.y, chip.x, chip.right(), &case);
                    assert_eq!(pinned_below, chip_steps, "{case}: 칩 밖에 PIN_BG");
                } else {
                    assert!(width < 100, "{case}: 칩이 보여야 하는 크기");
                    assert_eq!(pinned_below, 0, "{case}");
                }
            }

            // 4. 꺼짐: 배지 없고 칩은 보통 칩 바탕, 화면 어디에도 PIN_BG 없음
            let mut state = sample_state_with_snapshot();
            state.lang = lang;
            let buffer = draw(&mut state, width, height);
            for y in 0..height {
                for x in 0..width {
                    assert_ne!(buffer.cell((x, y)).unwrap().bg, theme::PIN_BG, "{case}");
                }
            }
            if let Some((_, chip)) = state
                .hit_areas
                .button_rects
                .iter()
                .find(|(action, _)| *action == UiAction::TogglePin)
            {
                let text = rect_text(&buffer, chip.y, chip.x, chip.right());
                assert!(text.contains(off_word), "{case}: {text}");
                assert_eq!(buffer.cell((chip.x, chip.y)).unwrap().bg, theme::BG_TONAL);
            }
        }
    }
}

// 10. 한도 카드·캐시 타이머 카드 ----------------------------------------------
// 카드 영역(테두리 포함) 안 모든 줄의 글자
fn card_text(buffer: &Buffer, state: &AppState, card: CardId) -> String {
    let rect = card_rect_of(state, card);
    (rect.y..rect.bottom())
        .map(|y| rect_text(buffer, y, rect.x, rect.right()))
        .collect::<Vec<_>>()
        .join("\n")
}

// 한도 카드에 스냅샷 나이(초)를 지정
fn limits_state_aged(lang: Lang, age_secs: i64) -> AppState {
    let mut state = sample_state_with_limits(lang);
    if let Panel::Ready(data) = &mut state.snapshot.limits
        && let Some(latest) = data.latest.as_mut()
    {
        latest.ts = (fixed_limits_now() - ChronoDuration::seconds(age_secs))
            .to_rfc3339_opts(SecondsFormat::Millis, true);
    }
    state
}

// 카드 안쪽 한 줄(테두리 안 첫 줄이 0)의 첫 막대 채움 칸 글자 색
fn first_bar_color(
    buffer: &Buffer,
    state: &AppState,
    card: CardId,
    row: u16,
) -> Option<ratatui::style::Color> {
    let rect = card_rect_of(state, card);
    let y = rect.y + 1 + row;
    (rect.x + 1..rect.right() - 1)
        .find(|x| {
            buffer
                .cell((*x, y))
                .is_some_and(|cell| cell.symbol() == "█")
        })
        .and_then(|x| buffer.cell((x, y)).and_then(|cell| cell.style().fg))
}

#[test]
fn limits_card_shows_bars_countdowns_pace_projection_context_and_api_key_at_the_size_matrix() {
    // 5시간 줄 소진 예상: 마지막 스냅샷(12초 전, 70%) + 남은 30%를 한 시간 20%p 속도로 = 5370초 뒤
    let exhaust_clock = DateTime::from_timestamp(fixed_limits_now().timestamp() + 5_370, 0)
        .unwrap()
        .with_timezone(&Local)
        .format("%H:%M")
        .to_string();
    for lang in [Lang::En, Lang::Ko] {
        for (width, height) in [(100u16, 30u16), (130, 40)] {
            let case = format!("{} {width}x{height}", lang_name(lang));
            let mut state = sample_state_with_limits(lang);
            let buffer = draw(&mut state, width, height);
            let text = card_text(&buffer, &state, CardId::LimitsCard);
            let rect = card_rect_of(&state, CardId::LimitsCard);
            assert_eq!(rect.right(), width, "{case}");
            assert_eq!(rect.height, 6, "{case}");
            assert!(
                state
                    .hit_areas
                    .title_hits
                    .iter()
                    .any(|hit| hit.card == CardId::LimitsCard),
                "{case}"
            );

            // 이름·퍼센트 숫자·막대·리셋 카운트다운·페이스 글자와 단어·컨텍스트·API 키 남은 양
            let (reset, pace, compact, hint) = match lang {
                Lang::En => ("in 3h 00m", "▲ ahead", "compact 2", "official · 12s ago"),
                Lang::Ko => ("3h 00m 후", "▲ 빠름", "압축 2회", "공식 · 12s 전"),
            };
            for needle in [
                "5h",
                "7d",
                "ctx",
                "API",
                "70%",
                "35%",
                "62%",
                "█",
                "░",
                reset,
                pace,
                compact,
                hint,
                "480/500",
                "80.0K/100.0K",
            ] {
                assert!(text.contains(needle), "{case} {needle}\n{text}");
            }

            // API 키 리셋 카운트다운도 넓은 카드에만
            let api_reset = match lang {
                Lang::En => "in 12m 00s",
                Lang::Ko => "12m 00s 후",
            };
            assert_eq!(text.contains(api_reset), width == 130, "{case}\n{text}");

            // 소진 예상 시각은 넓은 카드에만 (좋으면 페이스보다 뒤에서 덜어냄)
            let projection = format!("→ {exhaust_clock}");
            assert_eq!(text.contains(&projection), width == 130, "{case}\n{text}");
        }
    }
}

#[test]
fn limits_bar_and_number_turn_warn_at_80_and_critical_at_95_and_stay_paired() {
    for (pct, color) in [
        (0.0, None),
        (79.0, Some(theme::COLOR_OK)),
        (80.0, Some(theme::COLOR_WARN)),
        (94.0, Some(theme::COLOR_WARN)),
        (95.0, Some(theme::COLOR_CRIT)),
        (100.0, Some(theme::COLOR_CRIT)),
    ] {
        for lang in [Lang::En, Lang::Ko] {
            let case = format!("{pct} {}", lang_name(lang));
            let mut state = sample_state_with_limits(lang);
            if let Panel::Ready(data) = &mut state.snapshot.limits
                && let Some(latest) = data.latest.as_mut()
            {
                latest.five_hour_pct = Some(pct);
            }
            let buffer = draw(&mut state, 130, 40);
            let text = card_text(&buffer, &state, CardId::LimitsCard);
            assert!(text.contains(&format!("{pct:.0}%")), "{case}\n{text}");
            assert_eq!(
                first_bar_color(&buffer, &state, CardId::LimitsCard, 0),
                color,
                "{case}"
            );

            // 숫자 칸도 같은 색 (색만으로 알리지 않고 숫자가 함께 있음)
            if let Some(expected) = color {
                let rect = card_rect_of(&state, CardId::LimitsCard);
                let y = rect.y + 1;
                let number_x = (rect.x + 1..rect.right() - 1)
                    .find(|x| {
                        buffer
                            .cell((*x, y))
                            .is_some_and(|cell| cell.symbol() == "%")
                    })
                    .unwrap();
                let digit = buffer.cell((number_x - 1, y)).unwrap();
                assert_eq!(digit.style().fg, Some(expected), "{case}");
            }
        }
    }
}

#[test]
fn a_stale_limits_snapshot_dims_every_value_names_the_age_and_hides_the_projection() {
    for lang in [Lang::En, Lang::Ko] {
        for (width, height) in [(100u16, 30u16), (130, 40)] {
            let case = format!("{} {width}x{height}", lang_name(lang));
            let mut state = limits_state_aged(lang, 45 * 60);
            let buffer = draw(&mut state, width, height);
            let text = card_text(&buffer, &state, CardId::LimitsCard);
            let word = match lang {
                Lang::En => "stale 45m",
                Lang::Ko => "지연 45m",
            };
            assert!(text.contains(word), "{case}\n{text}");
            assert!(
                text.contains("70%") && text.contains("62%"),
                "{case}\n{text}"
            );
            assert!(!text.contains('→'), "{case}\n{text}");
            let rect = card_rect_of(&state, CardId::LimitsCard);
            for y in rect.y + 1..rect.bottom() - 1 {
                for x in rect.x + 1..rect.right() - 1 {
                    let fg = buffer.cell((x, y)).unwrap().style().fg;
                    assert!(
                        ![
                            Some(theme::COLOR_OK),
                            Some(theme::COLOR_WARN),
                            Some(theme::COLOR_CRIT)
                        ]
                        .contains(&fg),
                        "{case} ({x},{y})"
                    );
                }
            }

            // 30분 이하는 흐리지 않음: 색 한도 표시가 있고 단어 stale 없음
            let mut fresh = limits_state_aged(lang, 29 * 60);
            let buffer = draw(&mut fresh, width, height);
            let text = card_text(&buffer, &fresh, CardId::LimitsCard);
            assert!(!text.contains(word), "{case}\n{text}");
            assert_eq!(
                first_bar_color(&buffer, &fresh, CardId::LimitsCard, 0),
                Some(theme::COLOR_OK),
                "{case}"
            );
        }
    }
}

#[test]
fn limits_card_without_a_snapshot_guides_to_the_statusline_or_falls_back_to_labeled_estimates() {
    for lang in [Lang::En, Lang::Ko] {
        for (width, height) in [(100u16, 30u16), (130, 40)] {
            let case = format!("{} {width}x{height}", lang_name(lang));

            // 빈 상태: 제목 한 줄과 설정 안내
            let mut state = sample_state_with_limits(lang);
            state.snapshot.limits = Panel::Ready(LimitsData::default());
            let buffer = draw(&mut state, width, height);
            let text = card_text(&buffer, &state, CardId::LimitsCard);
            let headline = match lang {
                Lang::En => "No official limit data",
                Lang::Ko => "공식 한도 값 없음",
            };
            assert!(text.contains(headline), "{case}\n{text}");
            assert!(text.contains("statusLine"), "{case}\n{text}");
            assert!(text.contains("statusline"), "{case}\n{text}");

            // 추정만 있을 때: 추정 단어와 토큰 수, 공식 퍼센트 없음
            let estimate = |tokens: i64| {
                Some(OverviewStats {
                    input_tokens: tokens,
                    output_tokens: tokens / 5,
                    ..OverviewStats::default()
                })
            };
            state.snapshot.limits = Panel::Ready(LimitsData {
                estimate_5h: estimate(1_000_000),
                estimate_7d: estimate(4_500_000),
                ..LimitsData::default()
            });
            let buffer = draw(&mut state, width, height);
            let text = card_text(&buffer, &state, CardId::LimitsCard);
            let word = match lang {
                Lang::En => "estimate",
                Lang::Ko => "추정",
            };
            for needle in [word, "1.2M", "5.4M", "5h", "7d"] {
                assert!(text.contains(needle), "{case} {needle}\n{text}");
            }
            assert!(!text.contains('%'), "{case}\n{text}");

            // 아직 수집 전
            state.snapshot.limits = Panel::Pending;
            let buffer = draw(&mut state, width, height);
            let text = card_text(&buffer, &state, CardId::LimitsCard);
            let pending = i18n::text(lang, i18n::Msg::PanelPending);
            assert!(text.contains(pending), "{case}\n{text}");
        }
    }
}

#[test]
fn limits_and_cache_cards_follow_the_size_rules_at_minimum_and_narrow_sizes() {
    for lang in [Lang::En, Lang::Ko] {
        let case = lang_name(lang);

        // 최소 크기의 낮은 개요: 추이 줄과 함께 한도 카드는 빠지고 올바른 본문은 그대로
        let mut state = sample_state_with_limits(lang);
        draw(&mut state, MIN_WIDTH, MIN_HEIGHT);
        assert!(
            !state
                .hit_areas
                .card_rects
                .iter()
                .any(|(card, _)| *card == CardId::LimitsCard),
            "{case}"
        );

        // 모델 탭의 최소 크기: 캐시 타이머 카드는 숨김, 표·상세·붕괴 표는 그대로
        let mut state = sample_state_with_limits(lang);
        state.tab = ActiveTab::Models;
        draw(&mut state, MIN_WIDTH, MIN_HEIGHT);
        let drawn: Vec<CardId> = state
            .hit_areas
            .card_rects
            .iter()
            .map(|(card, _)| *card)
            .collect();
        assert!(!drawn.contains(&CardId::CacheTimer), "{case}");
        for card in [CardId::ModelTable, CardId::ModelDetail, CardId::CacheBreaks] {
            assert!(drawn.contains(&card), "{case} {card:?}");
        }

        // 폭이 좁은 큰 화면(60x30): 한도 카드는 오른쪽 끝, 이름·퍼센트·카운트다운을 유지
        let mut state = sample_state_with_limits(lang);
        let buffer = draw(&mut state, 60, 30);
        let rect = card_rect_of(&state, CardId::LimitsCard);
        assert_eq!(rect.right(), 60, "{case}");
        let text = card_text(&buffer, &state, CardId::LimitsCard);
        for needle in ["5h", "7d", "70%"] {
            assert!(text.contains(needle), "{case} {needle}\n{text}");
        }

        // 카드 영역끼리 곹치지 않고 내용이 현재 카드 안에 있음 (모델 탭 카드 높이 최소 22행에서도 네 카드)
        let mut state = sample_state_with_limits(lang);
        state.tab = ActiveTab::Models;
        draw(&mut state, 100, 26);
        let mut rects: Vec<Rect> = [
            CardId::ModelTable,
            CardId::ModelDetail,
            CardId::CacheBreaks,
            CardId::CacheTimer,
        ]
        .iter()
        .map(|card| card_rect_of(&state, *card))
        .collect();
        rects.sort_by_key(|rect| rect.y);
        for pair in rects.windows(2) {
            assert_eq!(pair[0].bottom(), pair[1].y, "{case}");
        }
        assert!(
            rects.iter().all(|rect| rect.height >= 3),
            "{case} {rects:?}"
        );
        assert_eq!(rects[3].bottom(), 24, "{case}");
    }
}

// 낮은 모델 탭에서 사라지는 카드는 항상 캐시 타이머 (그 카드를 어느 칸으로 옮겨도 표·상세·붕괴 표는 남음)
#[test]
fn the_cache_timer_is_the_card_that_disappears_on_a_short_models_tab_wherever_it_sits() {
    for lang in [Lang::En, Lang::Ko] {
        for slot in 0..4 {
            let case = format!("{} slot {slot}", lang_name(lang));
            let mut state = sample_state_with_limits(lang);
            state.tab = ActiveTab::Models;
            state
                .card_layout
                .move_card_to_slot(SplitGroup::ModelsRows, &IDENTITY_ORDER, 3, slot);
            let buffer = draw(&mut state, 100, 24);
            let drawn: Vec<CardId> = state
                .hit_areas
                .card_rects
                .iter()
                .map(|(card, _)| *card)
                .collect();
            assert!(!drawn.contains(&CardId::CacheTimer), "{case} {drawn:?}");
            for (card, min_height) in [
                (CardId::ModelTable, 5),
                (CardId::ModelDetail, 4),
                (CardId::CacheBreaks, 3),
            ] {
                assert!(drawn.contains(&card), "{case} {card:?}");
                let rect = card_rect_of(&state, card);
                assert!(rect.height >= min_height, "{case} {card:?} {rect:?}");
            }
            let detail = card_text(&buffer, &state, CardId::ModelDetail);
            assert!(detail.contains("claude-"), "{case}\n{detail}");

            // 높은 화면에서는 캐시 타이머가 옮겨 놓은 칸에서 그려짐
            draw(&mut state, 100, 30);
            let rect = card_rect_of(&state, CardId::CacheTimer);
            assert!(rect.height >= 3, "{case} {rect:?}");
        }
    }
}

// 폭 60 개요 추이 줄: 읽을 수 있는 폭(12열) 밑의 추이 카드 둘은 숨기고 절약·한도 카드가 폭을 나눠 갖음
#[test]
fn the_overview_trend_row_hides_the_trend_cards_below_a_readable_width() {
    for lang in [Lang::En, Lang::Ko] {
        let case = lang_name(lang);
        let mut state = sample_state_with_limits(lang);
        let buffer = draw(&mut state, 60, 24);
        let drawn: Vec<CardId> = state
            .hit_areas
            .card_rects
            .iter()
            .map(|(card, _)| *card)
            .collect();
        assert!(
            !drawn.contains(&CardId::TrendRequests) && !drawn.contains(&CardId::TrendTokens),
            "{case} {drawn:?}"
        );
        let savings = card_rect_of(&state, CardId::SavingsEffect);
        let limits = card_rect_of(&state, CardId::LimitsCard);
        assert!(savings.width >= 24 && limits.width >= 28, "{case}");
        assert_eq!(savings.right() + 1, limits.x, "{case}");
        assert_eq!(limits.right(), 60, "{case}");
        let text = card_text(&buffer, &state, CardId::LimitsCard);
        for needle in ["5h", "7d", "70%"] {
            assert!(text.contains(needle), "{case} {needle}\n{text}");
        }
        assert!(
            state
                .hit_areas
                .split_hits
                .iter()
                .all(|hit| hit.group != SplitGroup::OverviewTrendCols),
            "{case}"
        );

        // 저장한 비율이 있거나 카드 순서를 바꿔도 같은 폭에서는 추이 카드가 숨김 (폭을 남기지 않음)
        assert!(
            state
                .card_layout
                .restore_ratios(SplitGroup::OverviewTrendCols, &[2_000, 5_000, 7_000])
        );
        state
            .card_layout
            .move_card_to_slot(SplitGroup::OverviewTrendCols, &IDENTITY_ORDER, 3, 0);
        draw(&mut state, 60, 24);
        let limits = card_rect_of(&state, CardId::LimitsCard);
        let savings = card_rect_of(&state, CardId::SavingsEffect);
        assert!(
            !state
                .hit_areas
                .card_rects
                .iter()
                .any(|(card, _)| *card == CardId::TrendTokens),
            "{case}"
        );
        assert!(limits.x < savings.x, "{case}");
        assert_eq!(savings.right(), 60, "{case}");

        // 중간·큰 폭에서는 추이 카드가 그대로 보이고 읽을 수 있는 폭을 갖음
        for (width, height) in [(100u16, 30u16), (130, 40)] {
            let mut state = sample_state_with_limits(lang);
            draw(&mut state, width, height);
            for card in [CardId::TrendRequests, CardId::TrendTokens] {
                let rect = card_rect_of(&state, card);
                assert!(
                    rect.width >= 12,
                    "{case} {width}x{height} {card:?} {rect:?}"
                );
            }
        }
    }
}

// 한도 퍼센트·캐시 적중 단계는 색과 함께 표식(경고 !, 위험 !!)이 붙고 정상은 표식이 없음
#[test]
fn limit_and_cache_hit_levels_carry_a_mark_besides_the_color_at_the_size_matrix() {
    for lang in [Lang::En, Lang::Ko] {
        let hit_word = match lang {
            Lang::En => "hit",
            Lang::Ko => "적중",
        };
        for (width, height) in [(100u16, 30u16), (130, 40)] {
            let case = format!("{} {width}x{height}", lang_name(lang));

            // 정상 단계: 표식 없음
            let mut state = sample_state_with_limits(lang);
            let buffer = draw(&mut state, width, height);
            let limits = card_text(&buffer, &state, CardId::LimitsCard);
            assert!(!limits.contains('!'), "{case}\n{limits}");
            state.tab = ActiveTab::Models;
            let buffer = draw(&mut state, width, height);
            let cache = card_text(&buffer, &state, CardId::CacheTimer);
            assert!(!cache.contains('!'), "{case}\n{cache}");

            // 경고·위험 단계: 퍼센트 뒤에 표식
            for (ratio, mark) in [(0.6, "!"), (0.3, "!!")] {
                let mut state = sample_state_with_limits(lang);
                if let Panel::Ready(data) = &mut state.snapshot.limits
                    && let Some(latest) = data.latest.as_mut()
                {
                    latest.five_hour_pct = Some(85.0);
                    latest.seven_day_pct = Some(97.0);
                    latest.cache_hit_ratio = Some(ratio);
                }
                let buffer = draw(&mut state, width, height);
                let limits = card_text(&buffer, &state, CardId::LimitsCard);
                assert!(limits.contains("85% !"), "{case}\n{limits}");
                assert!(limits.contains("97% !!"), "{case}\n{limits}");
                state.tab = ActiveTab::Models;
                let buffer = draw(&mut state, width, height);
                let cache = card_text(&buffer, &state, CardId::CacheTimer);
                let needle = format!("{hit_word} {:.0}% {mark}", ratio * 100.0);
                assert!(cache.contains(&needle), "{case} {needle}\n{cache}");
            }
        }
    }
}

#[test]
fn the_limits_card_joins_the_overview_card_group_reorder_and_the_l_reset() {
    let mut state = sample_state_with_limits(Lang::En);
    assert_eq!(
        CardId::LimitsCard.reorder_slot(),
        Some((SplitGroup::OverviewTrendCols, 3))
    );
    let buffer = draw(&mut state, 130, 40);
    let default_rect = card_rect_of(&state, CardId::LimitsCard);
    assert!(default_rect.x > card_rect_of(&state, CardId::SavingsEffect).x);
    assert!(buffer.area.width == 130);

    // 맨 앞 칸으로 옮기면 칸 크기는 칸에 남고 한도 카드가 왼쪽 끝에 옴
    state
        .card_layout
        .move_card_to_slot(SplitGroup::OverviewTrendCols, &IDENTITY_ORDER, 3, 0);
    let buffer = draw(&mut state, 130, 40);
    let moved_rect = card_rect_of(&state, CardId::LimitsCard);
    assert!(moved_rect.x < card_rect_of(&state, CardId::SavingsEffect).x);
    assert_eq!(moved_rect.width, 28);
    let text = card_text(&buffer, &state, CardId::LimitsCard);
    assert!(text.contains("5h") && text.contains("70%"), "{text}");

    // l 키로 순서 초기화
    let command = state.handle_key_event(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    assert_eq!(command, UiCommand::SaveState);
    assert_eq!(
        state.card_layout.order_of(SplitGroup::OverviewTrendCols),
        IDENTITY_ORDER
    );
    draw(&mut state, 130, 40);
    assert_eq!(card_rect_of(&state, CardId::LimitsCard), default_rect);
}

#[test]
fn cache_timer_card_counts_down_on_every_redraw_and_reports_hit_misses_and_the_last_miss() {
    for lang in [Lang::En, Lang::Ko] {
        for (width, height) in [(100u16, 30u16), (130, 40)] {
            let case = format!("{} {width}x{height}", lang_name(lang));
            let mut state = sample_state_with_limits(lang);
            state.tab = ActiveTab::Models;
            let words: [&str; 9] = match lang {
                Lang::En => [
                    "warm",
                    "4m 12s left",
                    "TTL 5m",
                    "hit 82%",
                    "misses 9/50",
                    "last miss",
                    "3m 00s ago",
                    "cause ttl_expired",
                    "official · 12s ago",
                ],
                Lang::Ko => [
                    "유지",
                    "4m 12s 남음",
                    "TTL 5m",
                    "적중 82%",
                    "미스 9/50",
                    "최근 미스",
                    "3m 00s 전",
                    "원인 ttl_expired",
                    "공식 · 12s 전",
                ],
            };
            let buffer = draw(&mut state, width, height);
            let text = card_text(&buffer, &state, CardId::CacheTimer);
            for needle in words {
                assert!(text.contains(needle), "{case} {needle}\n{text}");
            }
            let rect = card_rect_of(&state, CardId::CacheTimer);
            assert_eq!(rect.height, 5, "{case}");
            assert_eq!(rect.bottom(), height - 2, "{case}");
            let dot = buffer.cell((rect.x + 2, rect.y + 1)).unwrap();
            assert_eq!(
                (dot.symbol(), dot.style().fg),
                ("●", Some(theme::COLOR_OK)),
                "{case}"
            );

            // 30초 뒤 다시 그리면 남은 시간이 줄고 최근 미스 경과는 늘어남 (스냅샷은 그대로)
            state.clock_override = Some(fixed_limits_now() + ChronoDuration::seconds(30));
            let buffer = draw(&mut state, width, height);
            let text = card_text(&buffer, &state, CardId::CacheTimer);
            let (left, ago) = match lang {
                Lang::En => ("3m 42s left", "3m 30s ago"),
                Lang::Ko => ("3m 42s 남음", "3m 30s 전"),
            };
            assert!(text.contains(left) && text.contains(ago), "{case}\n{text}");

            // 1분 미만 남으면 경고색 점과 남은 시간 단어
            state.clock_override = Some(fixed_limits_now() + ChronoDuration::seconds(200));
            let buffer = draw(&mut state, width, height);
            let text = card_text(&buffer, &state, CardId::CacheTimer);
            let dot = buffer.cell((rect.x + 2, rect.y + 1)).unwrap();
            assert_eq!(dot.style().fg, Some(theme::COLOR_WARN), "{case}");
            assert!(
                text.contains("0m 52s") || text.contains("52s"),
                "{case}\n{text}"
            );

            // 만료 뒤: 빈 점과 단어 cold 또는 만료, 만료 후 경과
            state.clock_override = Some(fixed_limits_now() + ChronoDuration::seconds(300));
            let buffer = draw(&mut state, width, height);
            let text = card_text(&buffer, &state, CardId::CacheTimer);
            let (cold, expired) = match lang {
                Lang::En => ("cold", "expired 48s ago"),
                Lang::Ko => ("만료", "48s 전 만료"),
            };
            assert!(
                text.contains(cold) && text.contains(expired),
                "{case}\n{text}"
            );
            let dot = buffer.cell((rect.x + 2, rect.y + 1)).unwrap();
            assert_eq!(
                (dot.symbol(), dot.style().fg),
                ("○", Some(theme::FG_DIM)),
                "{case}"
            );

            // 스냅샷 없음
            state.snapshot.limits = Panel::Ready(LimitsData::default());
            let buffer = draw(&mut state, width, height);
            let text = card_text(&buffer, &state, CardId::CacheTimer);
            let empty = match lang {
                Lang::En => "No cache data",
                Lang::Ko => "캐시 값 없음",
            };
            assert!(text.contains(empty), "{case}\n{text}");
        }
    }
}
