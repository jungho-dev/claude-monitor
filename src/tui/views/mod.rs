//! tui/views/mod.rs
//! 화면 조립: 앱 바, 탭 바, 본문, 하단 버튼·안내, 카드 경계 손잡이, 도움말 겹침.

pub mod common;
pub mod feed;
pub mod gateway;
pub mod models;
pub mod overview;
pub mod report;
pub mod settings;
pub mod tables;

use crate::tui::cards::{CardKind, UiAction, UiHitAreas};
use crate::tui::format::fit_display;
use crate::tui::poller::POLL_INTERVAL;
use crate::tui::sort::sort_spec;
use crate::tui::state::{ActiveTab, AppState, ToastLevel};
use crate::tui::theme;
use crate::tui::views::common::render_split_handles;
use chrono::{Local, Utc};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};
use unicode_width::UnicodeWidthStr;

const MIN_WIDTH: u16 = 60;
const MIN_HEIGHT: u16 = 16;
// 갱신 지연으로 보는 기준 (폴링 주기의 3배)
const STALE_POLL_MULTIPLE: u64 = 3;

// 1. 전체 프레임 --------------------------------------------------------------
pub fn render_frame(frame: &mut Frame, state: &mut AppState) {
    let area = frame.area();
    state.hit_areas = UiHitAreas::default();
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::BG_BASE).fg(theme::FG_TEXT)),
        area,
    );

    // 1. 최소 크기 미달
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let notice = Paragraph::new(Line::from(Span::styled(
            format!(
                "터미널 크기 부족 (최소 {MIN_WIDTH}x{MIN_HEIGHT}, 현재 {}x{})",
                area.width, area.height
            ),
            theme::STYLE_WARN,
        )))
        .alignment(Alignment::Center);
        frame.render_widget(
            notice,
            Rect::new(area.x, area.y + area.height / 2, area.width, 1),
        );
        return;
    }

    // 2. 앱 바, 탭 바, 본문, 하단 두 줄
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .split(area);
    render_app_bar(frame, chunks[0], state);
    render_tab_bar(frame, chunks[1], state);
    match state.tab {
        ActiveTab::Overview => overview::render_overview_tab(frame, chunks[2], state),
        ActiveTab::Tools => tables::render_tools_tab(frame, chunks[2], state),
        ActiveTab::Hooks => tables::render_hooks_tab(frame, chunks[2], state),
        ActiveTab::Models => models::render_models_tab(frame, chunks[2], state),
        ActiveTab::Agents => models::render_agents_tab(frame, chunks[2], state),
        ActiveTab::Gateway => gateway::render_gateway_tab(frame, chunks[2], state),
        ActiveTab::Settings => settings::render_settings_tab(frame, chunks[2], state),
        ActiveTab::Report => report::render_report_tab(frame, chunks[2], state),
    }
    render_footer(frame, chunks[3], state);

    // 3. 카드 경계 손잡이와 도움말 겹침
    render_split_handles(frame, state);
    if state.show_help {
        render_help_overlay(frame, area);
    }
}

// 2. 앱 바 --------------------------------------------------------------------
// 왼쪽 배지·최상단 표시, 오른쪽 구간과 갱신 신선도 (폴링 주기 3배 넘으면 지연 표시)
fn render_app_bar(frame: &mut Frame, area: Rect, state: &AppState) {
    // 1. 신선도 표시
    let stale_secs =
        i64::try_from(POLL_INTERVAL.as_secs() * STALE_POLL_MULTIPLE).unwrap_or(i64::MAX);
    let freshness: Vec<Span<'static>> = match state.snapshot.taken_at {
        Some(taken_at) => {
            let age_secs = (Utc::now() - taken_at).num_seconds();
            if age_secs <= stale_secs {
                vec![
                    Span::styled("● ", theme::STYLE_OK),
                    Span::styled(
                        taken_at
                            .with_timezone(&Local)
                            .format("%H:%M:%S")
                            .to_string(),
                        theme::STYLE_DIM,
                    ),
                ]
            } else {
                vec![
                    Span::styled("● ", theme::STYLE_WARN),
                    Span::styled(format!("갱신 지연 {age_secs}s"), theme::STYLE_WARN),
                ]
            }
        }
        None => vec![Span::styled("○ 대기", theme::STYLE_DIM)],
    };
    let mut right_spans = vec![
        Span::styled("구간 ", theme::STYLE_DIM),
        Span::styled(state.window.window_label(), theme::STYLE_ACCENT),
        Span::raw("  "),
    ];
    right_spans.extend(freshness);
    right_spans.push(Span::raw(" "));
    let right = Line::from(right_spans);
    let right_width = (right.width() as u16).min(area.width / 2);
    let columns =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).split(area);

    // 2. 왼쪽 배지
    let mut left_spans = vec![
        Span::styled(
            " rust-agents-proxy ",
            Style::default()
                .fg(theme::FG_ON_ACCENT)
                .bg(theme::FG_ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" Claude Code 관제", theme::STYLE_DIM),
    ];
    if state.always_on_top {
        left_spans.push(Span::raw("  "));
        left_spans.push(Span::styled(
            " 최상단 ",
            Style::default()
                .fg(theme::FG_ON_ACCENT)
                .bg(theme::COLOR_WARN)
                .add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(left_spans)).style(Style::default().bg(theme::BG_BAR)),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(right).style(Style::default().bg(theme::BG_BAR)),
        columns[1],
    );
}

// 3. 탭 바 --------------------------------------------------------------------
// 탭마다 클릭 영역 등록, 전체 라벨이 폭을 넘으면 짧은 이름·간격 0, 그래도 넘으면 번호 붙임·여백 없는 압축
fn render_tab_bar(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let row_end = area.x.saturating_add(area.width);
    let mut cursor = area.x.saturating_add(1);

    // 1. 압축 단계 (0 전체, 1 짧은 이름·간격 0, 2 번호 붙임 짧은 이름·간격 1)
    let tab_count = ActiveTab::ALL.len();
    let full_labels: usize = ActiveTab::ALL
        .iter()
        .map(|tab| Span::raw(tab.tab_label()).width())
        .sum();
    let short_labels: usize = ActiveTab::ALL
        .iter()
        .map(|tab| Span::raw(tab.short_label()).width())
        .sum();
    let full_width = full_labels + 4 * tab_count + tab_count;
    let compact_width = short_labels + 4 * tab_count + 1;
    let level = if full_width <= area.width as usize {
        0
    } else if compact_width <= area.width as usize {
        1
    } else {
        2
    };
    let gap = u16::from(level != 1);

    // 2. 탭 그리기
    for (index, tab) in ActiveTab::ALL.iter().enumerate() {
        let (key_style, name_style, background) = if state.tab == *tab {
            (
                Style::default()
                    .fg(theme::FG_ACCENT)
                    .add_modifier(Modifier::BOLD),
                Style::default()
                    .fg(theme::FG_STRONG)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                theme::BG_PANEL,
            )
        } else {
            (
                theme::STYLE_DIM,
                Style::default().fg(theme::TAB_IDLE_NAME),
                theme::BG_BASE,
            )
        };
        let label = if level == 0 {
            tab.tab_label()
        } else {
            tab.short_label()
        };
        let line = match level {
            2 => Line::from(vec![
                Span::styled(format!("{}", index + 1), key_style),
                Span::styled(label, name_style),
            ]),
            _ => Line::from(vec![
                Span::styled(format!(" {} ", index + 1), key_style),
                Span::styled(label, name_style),
                Span::raw(" "),
            ]),
        };
        let width = line.width() as u16;
        if cursor.saturating_add(width) > row_end {
            break;
        }
        let rect = Rect::new(cursor, area.y, width, area.height);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(background)),
            rect,
        );
        state.hit_areas.tab_rects.push((*tab, rect));
        cursor = cursor.saturating_add(width + gap);
    }
}

// 4. 하단 두 줄 ---------------------------------------------------------------
// 1행 클릭 버튼(왼쪽 기능, 오른쪽 도움말·종료), 2행 알림과 상황별 단축키 안내
fn render_footer(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::BG_BAR)),
        area,
    );
    render_footer_buttons(frame, rows[0], state);
    render_footer_hints(frame, rows[1], state);
}

// 5. 하단 버튼 줄 -------------------------------------------------------------
// 칩 = 키 글자(강조색) + 이름, 폭이 모자라면 왼쪽 그룹의 뒤쪽부터 생략
fn render_footer_buttons(frame: &mut Frame, row: Rect, state: &mut AppState) {
    // 1. 칩 스타일 (핀 켜짐만 채움, 종료는 키 글자만 적색)
    let chip = Style::default().bg(theme::BG_TONAL);
    let key_style = chip.fg(theme::FG_ACCENT).add_modifier(Modifier::BOLD);
    let name_style = chip.fg(theme::FG_TEXT);
    let quit_key_style = chip.fg(theme::COLOR_CRIT).add_modifier(Modifier::BOLD);
    let pin_on_style = Style::default()
        .fg(theme::FG_ON_ACCENT)
        .bg(theme::COLOR_WARN)
        .add_modifier(Modifier::BOLD);
    let (pin_key, pin_name) = if state.always_on_top {
        (pin_on_style, pin_on_style)
    } else {
        (key_style, name_style)
    };
    let pin_label = if state.always_on_top {
        " 최상단 ON "
    } else {
        " 최상단 OFF "
    };
    let window_label = format!(" 구간 {} ", state.window.window_label());
    let left_chips: Vec<(UiAction, Line<'static>)> = vec![
        (
            UiAction::Reindex,
            Line::from(vec![
                Span::styled(" [i]", key_style),
                Span::styled(" 재색인 ", name_style),
            ]),
        ),
        (
            UiAction::CycleWindow,
            Line::from(vec![
                Span::styled(" [w]", key_style),
                Span::styled(window_label, name_style),
            ]),
        ),
        (
            UiAction::RefreshView,
            Line::from(vec![
                Span::styled(" [r]", key_style),
                Span::styled(" 새로고침 ", name_style),
            ]),
        ),
        (
            UiAction::TogglePin,
            Line::from(vec![
                Span::styled(" [t]", pin_key),
                Span::styled(pin_label, pin_name),
            ]),
        ),
        (
            UiAction::ResetLayout,
            Line::from(vec![
                Span::styled(" [l]", key_style),
                Span::styled(" 배치 초기화 ", name_style),
            ]),
        ),
    ];
    let right_chips: Vec<(UiAction, Line<'static>)> = vec![
        (
            UiAction::ShowHelp,
            Line::from(vec![
                Span::styled(" [?]", key_style),
                Span::styled(" 도움말 ", name_style),
            ]),
        ),
        (
            UiAction::Quit,
            Line::from(vec![
                Span::styled(" [q]", quit_key_style),
                Span::styled(" 종료 ", name_style),
            ]),
        ),
    ];

    // 2. 배치 (오른쪽 그룹은 오른쪽 끝 1열 앞, 왼쪽 그룹은 남는 폭까지만)
    let row_end = row.x.saturating_add(row.width);
    let right_width: u16 = right_chips
        .iter()
        .map(|(_, line)| line.width() as u16 + 1)
        .sum::<u16>()
        .saturating_sub(1);
    let right_start = row_end.saturating_sub(1 + right_width).max(row.x);
    let mut placed: Vec<(UiAction, Rect, Line<'static>)> = Vec::new();
    let mut cursor = row.x.saturating_add(1);
    for (action, line) in left_chips {
        let width = line.width() as u16;
        if cursor.saturating_add(width) >= right_start {
            break;
        }
        placed.push((action, Rect::new(cursor, row.y, width, row.height), line));
        cursor = cursor.saturating_add(width + 1);
    }
    let mut cursor = right_start;
    for (action, line) in right_chips {
        let width = line.width() as u16;
        placed.push((action, Rect::new(cursor, row.y, width, row.height), line));
        cursor = cursor.saturating_add(width + 1);
    }

    // 3. 그리기와 클릭 영역 등록
    for (action, rect, line) in placed {
        frame.render_widget(Paragraph::new(line).style(chip), rect);
        state.hit_areas.button_rects.push((action, rect));
    }
}

// 6. 하단 안내 줄 -------------------------------------------------------------
// 알림 먼저, 이어서 포인터·포커스 카드·탭 맥락 단축키 (꼬리 안내는 폭을 먼저 확보)
fn render_footer_hints(frame: &mut Frame, row: Rect, state: &AppState) {
    // 1. 알림 칩 (오류만 적색 채움, 최대 폭은 줄 폭 - 2)
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    if let Some(toast) = &state.toast {
        let chip = Style::default().bg(theme::BG_TONAL);
        let (dot_style, text_style) = match toast.level {
            ToastLevel::Info => (chip.fg(theme::FG_ACCENT), chip.fg(theme::FG_TEXT)),
            ToastLevel::Success => (chip.fg(theme::COLOR_OK), chip.fg(theme::FG_TEXT)),
            ToastLevel::Error => {
                let filled = Style::default()
                    .fg(theme::FG_STRONG)
                    .bg(theme::COLOR_CRIT_BG)
                    .add_modifier(Modifier::BOLD);
                (filled, filled)
            }
        };
        let text_budget = (row.width as usize).saturating_sub(2 + 4);
        let shown = if Span::raw(toast.text.as_str()).width() > text_budget {
            fit_display(&toast.text, text_budget).trim_end().to_string()
        } else {
            toast.text.clone()
        };
        spans.push(Span::styled(" ● ", dot_style));
        spans.push(Span::styled(shown, text_style));
        spans.push(Span::styled(" ", text_style));
    }

    // 2. 맥락 안내 (포인터 > 포커스 카드 종류 > 탭), 꼬리 안내는 항상 마지막
    let mut context_hints: Vec<&str> = Vec::new();
    if state.split_hover.is_some() || state.split_drag.is_some() {
        context_hints.extend(["드래그 카드 크기", "l 기본 배치"]);
    }
    if state.scroll_hover.is_some() || state.scroll_drag.is_some() {
        context_hints.extend(["드래그 스크롤", "트랙클릭 한 화면"]);
    }
    match state.focused_card_id().kind() {
        CardKind::Selectable => {
            context_hints.extend(["↑↓ 행 선택", "PgUp/PgDn 페이지", "Home/End 처음·끝"]);
        }
        CardKind::Scrollable => context_hints.extend(["↑↓ 스크롤", "PgUp/PgDn 페이지"]),
        CardKind::Static => {}
    }
    if sort_spec(state.focused_card_id()).is_some() || sort_spec(state.tab.main_card()).is_some() {
        context_hints.extend(["s 정렬 열", "S 방향"]);
    }
    if state.tab == ActiveTab::Report {
        context_hints.extend(["←→ 날짜", "g 생성"]);
    }
    let tail_hints = ["Tab 다음 탭", "? 전체 단축키"];

    // 3. 남은 폭에 들어가는 안내만 (키 글자 강조, 구분은 공백 2칸)
    let mut room = (row.width as usize).saturating_sub(Line::from(spans.clone()).width());
    let tail_width: usize = tail_hints
        .iter()
        .map(|hint| 2 + Span::raw(*hint).width())
        .sum();
    let reserved = if tail_width <= room { tail_width } else { 0 };
    let mut shown_hints: Vec<&str> = Vec::new();
    for hint in &context_hints {
        let piece_width = 2 + Span::raw(*hint).width();
        if piece_width > room.saturating_sub(reserved) {
            continue;
        }
        shown_hints.push(hint);
        room -= piece_width;
    }
    for hint in &tail_hints {
        let piece_width = 2 + Span::raw(*hint).width();
        if piece_width > room {
            continue;
        }
        shown_hints.push(hint);
        room -= piece_width;
    }
    // 구분 공백 (왼쪽 여백 1열 유지)
    let mut leading = state.toast.is_some();
    for hint in shown_hints {
        let (key, rest) = hint.split_once(' ').unwrap_or((hint, ""));
        if leading {
            spans.push(Span::raw("  "));
        }
        leading = true;
        spans.push(Span::styled(
            key.to_string(),
            Style::default()
                .fg(theme::HINT_KEY)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {rest}"), theme::STYLE_DIM));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::BG_BAR)),
        row,
    );
}

// 7. 도움말 겹침 --------------------------------------------------------------
// 바깥은 어둡게, 이중선 상자에 키보드·마우스 두 구역
fn render_help_overlay(frame: &mut Frame, area: Rect) {
    // 1. 도움말 내용 (뜻이 빈 줄 = 구역 제목)
    let help_rows: [(&str, &str); 19] = [
        ("키보드", ""),
        ("1-8, Tab, Shift-Tab", "탭 이동 (개요 ~ 리포트 여덟 탭)"),
        ("↑ ↓ PgUp PgDn Home End", "선택 카드의 행 이동·스크롤"),
        ("w", "집계 구간 전환 (1시간, 24시간, 7일)"),
        ("s / S", "선택한 표 정렬 열 변경 / 방향 반전"),
        ("←  →", "리포트 날짜 이동"),
        ("g", "리포트 생성 (serve 실행 필요)"),
        ("r", "설정·리포트 다시 불러오기, 그 외 즉시 갱신"),
        ("i", "기록 재색인 요청 (serve 실행 필요)"),
        ("t", "창 최상단 고정 켜기·끄기"),
        ("l", "현재 탭 카드 크기 기본 배치"),
        ("?", "도움말 열기·닫기"),
        ("q, Esc, Ctrl+C", "종료"),
        ("마우스", ""),
        ("탭·버튼 클릭", "탭 전환, 하단 버튼 실행"),
        ("카드·행 클릭", "카드 선택(굵은 테두리), 표 행 선택"),
        (
            "표 머리글 클릭",
            "그 열로 정렬, 다시 클릭하면 방향 반전 (↑ ↓ 표시)",
        ),
        (
            "휠 / 경계 드래그",
            "마우스 아래 카드 스크롤 / 카드 크기 조절",
        ),
        ("스크롤바", "썸 드래그 이동, 트랙 클릭 한 화면 이동"),
    ];
    let width = area.width.saturating_sub(4).min(76);
    let height = (help_rows.len() as u16 + 4).min(area.height.saturating_sub(2));
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );

    // 2. 바깥 어둡게, 이중선 상자 (굵은 포커스 테두리와 구분)
    let buf = frame.buffer_mut();
    buf.set_style(
        area,
        Style::default()
            .fg(theme::BORDER_DIM)
            .remove_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(theme::FG_ACCENT))
        .title(Span::styled(" 도움말 ", theme::STYLE_STRONG))
        .title_bottom(
            Line::from(Span::styled(" 아무 키·클릭: 닫기 ", theme::STYLE_DIM)).right_aligned(),
        )
        .padding(Padding::horizontal(1))
        .style(Style::default().bg(theme::BG_PANEL));
    let inner = block.inner(rect);

    // 왼쪽 밖 걸친 넓은 글자 지움 (테두리 어긋남 방지)
    if rect.x > area.x {
        let buf = frame.buffer_mut();
        for row_y in rect.y..rect.bottom() {
            let cell = &mut buf[(rect.x - 1, row_y)];
            if UnicodeWidthStr::width(cell.symbol()) > 1 {
                cell.set_symbol(" ");
            }
        }
    }
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    // 3. 줄 구성 (키 열 폭 = 가장 긴 키 + 2, 최대 24)
    let key_width = help_rows
        .iter()
        .filter(|(_, meaning)| !meaning.is_empty())
        .map(|(keys, _)| Span::raw(*keys).width())
        .max()
        .map_or(0, |widest| (widest + 2).min(24));
    let mut lines: Vec<Line<'static>> = help_rows
        .iter()
        .map(|(keys, meaning)| {
            if meaning.is_empty() {
                return Line::from(Span::styled((*keys).to_string(), theme::STYLE_ACCENT));
            }
            Line::from(vec![
                Span::styled(
                    fit_display(keys, key_width),
                    Style::default()
                        .fg(theme::HINT_KEY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled((*meaning).to_string(), theme::STYLE_TEXT),
            ])
        })
        .collect();

    // 4. 남는 높이가 줄 수보다 작으면 마지막 줄을 잘림 표시로 대체
    let room = inner.height as usize;
    if lines.len() > room {
        lines.truncate(room.saturating_sub(1));
        lines.push(Line::from(Span::styled(
            "… (창을 키우면 전체 표시)",
            theme::STYLE_DIM,
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::query::{HookStatRow, ToolLatencyRow};
    use crate::tui::cards::CardId;
    use crate::tui::sort::SortState;
    use crate::tui::state::{DbState, Panel, Toast, ToastLevel};
    use crate::tui::test_support::{
        sample_config, sample_state_with_many_events, sample_state_with_many_tools,
        sample_state_with_snapshot, sample_state_with_tables,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::{Duration, Instant};

    // 버퍼를 줄 단위 문자열로 (넓은 문자 뒤 빈 칸은 건너뜀)
    fn render_to_text(state: &mut AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_frame(frame, state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut rows = Vec::new();
        for y in 0..height {
            let mut row = String::new();
            let mut skip = 0usize;
            for x in 0..width {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let symbol = buffer.cell((x, y)).map_or(" ", |cell| cell.symbol());
                skip = Span::raw(symbol).width().saturating_sub(1);
                row.push_str(symbol);
            }
            rows.push(row);
        }
        rows
    }

    #[test]
    fn overview_renders_header_kpis_savings_and_feed() {
        let mut state = sample_state_with_snapshot();
        let text = render_to_text(&mut state, 130, 40).join("\n");

        assert!(text.contains("rust-agents-proxy"));
        assert!(text.contains("1 개요"));
        assert!(text.contains("serve 실행 중"));
        assert!(text.contains("게이트웨이 OFF"));
        assert!(text.contains("요청"));
        assert!(text.contains("캐시 적중률"));
        assert!(text.contains("87.5%"));
        assert!(text.contains("오늘 절약 시간"));
        assert!(text.contains("거부 손실 추정"));
        assert!(text.contains("실시간 이벤트 (4건)"));
        assert!(text.contains("grep -> rg"));
        // 선택 이벤트 상세 카드
        assert!(text.contains("이벤트 상세"));
    }

    #[test]
    fn footer_shows_clickable_buttons_and_registers_their_areas() {
        let mut state = sample_state_with_snapshot();
        let text = render_to_text(&mut state, 130, 40).join("\n");
        assert!(text.contains("[i] 재색인"));
        assert!(text.contains("[w] 구간 1시간"));
        assert!(text.contains("[t] 최상단 OFF"));
        assert!(text.contains("[q] 종료"));
        let actions: Vec<UiAction> = state
            .hit_areas
            .button_rects
            .iter()
            .map(|(action, _)| *action)
            .collect();
        assert_eq!(actions.len(), 7);
        assert!(actions.contains(&UiAction::Reindex));
        assert!(actions.contains(&UiAction::Quit));
        assert_eq!(state.hit_areas.tab_rects.len(), 8);
    }

    #[test]
    fn footer_chips_share_one_neutral_surface_and_only_quit_key_is_red() {
        let mut state = sample_state_with_snapshot();
        let mut terminal = Terminal::new(TestBackend::new(130, 40)).unwrap();
        terminal
            .draw(|frame| render_frame(frame, &mut state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let button_of = |action: UiAction| {
            state
                .hit_areas
                .button_rects
                .iter()
                .find(|(candidate, _)| *candidate == action)
                .map(|(_, rect)| *rect)
                .unwrap()
        };

        // 모든 칩은 중립 톤 바탕, 키 글자는 강조색 (재색인도 기본 칩)
        let reindex = button_of(UiAction::Reindex);
        let key = buffer.cell((reindex.x + 1, reindex.y)).unwrap();
        assert_eq!(key.symbol(), "[");
        assert_eq!(key.fg, theme::FG_ACCENT);
        assert_eq!(key.bg, theme::BG_TONAL);
        let name = buffer.cell((reindex.x + 5, reindex.y)).unwrap();
        assert_eq!(name.fg, theme::FG_TEXT);

        // 종료는 키 글자만 적색, 바탕은 중립
        let quit = button_of(UiAction::Quit);
        let quit_key = buffer.cell((quit.x + 1, quit.y)).unwrap();
        assert_eq!(quit_key.fg, theme::COLOR_CRIT);
        assert_eq!(quit_key.bg, theme::BG_TONAL);

        // 화면 어디에도 적색 채움 없음 (오류 알림 제외)
        for y in 0..40 {
            for x in 0..130 {
                assert_ne!(buffer.cell((x, y)).unwrap().bg, theme::COLOR_CRIT_BG);
            }
        }

        // 도움말·종료는 오른쪽 끝에서 1열 앞
        assert_eq!(quit.right(), 129);
        assert!(button_of(UiAction::ShowHelp).right() < quit.x);
    }

    #[test]
    fn footer_keeps_help_and_quit_at_minimum_width_and_drops_left_buttons_first() {
        let mut state = sample_state_with_snapshot();
        render_to_text(&mut state, MIN_WIDTH, MIN_HEIGHT);
        let actions: Vec<UiAction> = state
            .hit_areas
            .button_rects
            .iter()
            .map(|(action, _)| *action)
            .collect();
        assert!(actions.contains(&UiAction::ShowHelp));
        assert!(actions.contains(&UiAction::Quit));
        assert!(actions.contains(&UiAction::Reindex));
        assert!(!actions.contains(&UiAction::ResetLayout));
        for (_, rect) in &state.hit_areas.button_rects {
            assert!(rect.right() < MIN_WIDTH);
        }
    }

    #[test]
    fn error_toast_is_the_only_red_fill_and_hints_follow_context() {
        let mut state = sample_state_with_snapshot();
        let rows = render_to_text(&mut state, 130, 40);
        // 기본 포커스 = 이벤트 피드(행 선택 카드), 꼬리 안내는 항상
        assert!(rows[39].contains("↑↓ 행 선택"));
        assert!(rows[39].contains("Tab 다음 탭"));
        assert!(rows[39].contains("? 전체 단축키"));

        // 포인터 맥락이 우선
        state.split_hover = Some((crate::tui::layout::SplitGroup::OverviewRows, 0));
        let rows = render_to_text(&mut state, 130, 40);
        assert!(rows[39].contains("드래그 카드 크기"));
        state.split_hover = None;
        state.scroll_hover = Some(CardId::EventFeed);
        let rows = render_to_text(&mut state, 130, 40);
        assert!(rows[39].contains("트랙클릭 한 화면"));
        state.scroll_hover = None;

        // 탭 맥락
        state.tab = ActiveTab::Report;
        let rows = render_to_text(&mut state, 130, 40);
        assert!(rows[39].contains("←→ 날짜"));

        // 오류 알림은 적색 채움, 안내는 그 뒤로
        state.toast = Some(Toast {
            text: "실패".to_string(),
            level: ToastLevel::Error,
            expires_at: Instant::now() + Duration::from_secs(4),
        });
        let mut terminal = Terminal::new(TestBackend::new(130, 40)).unwrap();
        terminal
            .draw(|frame| render_frame(frame, &mut state))
            .unwrap();
        let toast_cell = terminal.backend().buffer().cell((3, 39)).unwrap();
        assert_eq!(toast_cell.bg, theme::COLOR_CRIT_BG);
        assert_eq!(toast_cell.fg, theme::FG_STRONG);
    }

    #[test]
    fn app_bar_marks_stale_snapshots_and_missing_snapshots() {
        let mut state = sample_state_with_snapshot();
        let fresh = render_to_text(&mut state, 130, 40)[0].clone();
        assert!(fresh.contains("● "));
        assert!(!fresh.contains("갱신 지연"));

        state.snapshot.taken_at = Some(chrono::Utc::now() - chrono::Duration::seconds(30));
        let stale = render_to_text(&mut state, 130, 40)[0].clone();
        assert!(stale.contains("갱신 지연 3"));

        state.snapshot.taken_at = None;
        let missing = render_to_text(&mut state, 130, 40)[0].clone();
        assert!(missing.contains("○ 대기"));
    }

    #[test]
    fn active_tab_uses_panel_surface_and_strong_underlined_name() {
        let mut state = sample_state_with_snapshot();
        let mut terminal = Terminal::new(TestBackend::new(130, 40)).unwrap();
        terminal
            .draw(|frame| render_frame(frame, &mut state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let overview = state.hit_areas.tab_rects[0].1;
        let name = buffer.cell((overview.x + 3, overview.y)).unwrap();
        assert_eq!(name.bg, theme::BG_PANEL);
        assert_eq!(name.fg, theme::FG_STRONG);
        assert!(name.modifier.contains(Modifier::UNDERLINED));
        let tools = state.hit_areas.tab_rects[1].1;
        let idle = buffer.cell((tools.x + 3, tools.y)).unwrap();
        assert_eq!(idle.fg, theme::TAB_IDLE_NAME);
        assert_eq!(idle.bg, theme::BG_BASE);
    }

    #[test]
    fn help_overlay_dims_the_backdrop_and_uses_a_double_border() {
        let mut state = sample_state_with_snapshot();
        state.show_help = true;
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal
            .draw(|frame| render_frame(frame, &mut state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        // 바깥 어둡게, 상자는 이중선 강조색
        assert_eq!(buffer.cell((1, 0)).unwrap().fg, theme::BORDER_DIM);
        let mut corner = None;
        for y in 0..40 {
            for x in 0..100 {
                if buffer.cell((x, y)).unwrap().symbol() == "╔" {
                    corner = Some(buffer.cell((x, y)).unwrap().fg);
                }
            }
        }
        assert_eq!(corner, Some(theme::FG_ACCENT));
        let text = render_to_text(&mut state, 100, 40).join("\n");
        assert!(text.contains("키보드"));
        assert!(text.contains("마우스"));
        assert!(text.contains("아무 키·클릭: 닫기"));
    }

    #[test]
    fn focused_card_is_drawn_with_thick_border_and_inverted_title() {
        let mut state = sample_state_with_snapshot();
        let mut terminal = Terminal::new(TestBackend::new(130, 40)).unwrap();
        terminal
            .draw(|frame| render_frame(frame, &mut state))
            .unwrap();
        let feed = state
            .hit_areas
            .card_rects
            .iter()
            .find(|(card, _)| *card == CardId::EventFeed)
            .map(|(_, rect)| *rect)
            .unwrap();
        let buffer = terminal.backend().buffer();
        let corner = buffer.cell((feed.x, feed.y)).unwrap();
        // 탭 주 카드 기본 포커스: 굵은 모서리와 포커스 색
        assert_eq!(corner.symbol(), "┏");
        assert_eq!(corner.fg, theme::BORDER_FOCUS);

        // 다른 카드는 둥근 모서리
        let kpi = state
            .hit_areas
            .card_rects
            .iter()
            .find(|(card, _)| *card == CardId::KpiCache)
            .map(|(_, rect)| *rect)
            .unwrap();
        assert_eq!(buffer.cell((kpi.x, kpi.y)).unwrap().symbol(), "╭");
    }

    #[test]
    fn table_tabs_render_sorted_rows_totals_and_sort_arrow() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Tools;
        let tools = render_to_text(&mut state, 100, 24);
        let tools_text = tools.join("\n");
        // 기본 정렬 = 합계 시간 내림차순: Bash, Read, Edit
        assert!(tools_text.contains("정렬 합계↓"));
        let read_row = tools.iter().position(|row| row.contains("Read")).unwrap();
        let bash_row = tools.iter().position(|row| row.contains("Bash")).unwrap();
        let edit_row = tools.iter().position(|row| row.contains("Edit")).unwrap();
        assert!(bash_row < read_row && read_row < edit_row);

        // 호출 열로 바꾸면 숫자 내림차순: Read, Bash, Edit
        state.card_sorts[CardId::ToolTable as usize] = SortState {
            column: 1,
            descending: true,
        };
        let tools = render_to_text(&mut state, 100, 24);
        assert!(tools.join("\n").contains("정렬 호출↓"));
        let read_row = tools.iter().position(|row| row.contains("Read")).unwrap();
        let bash_row = tools.iter().position(|row| row.contains("Bash")).unwrap();
        assert!(read_row < bash_row);

        // 이름 열로 바꾸면 오름차순
        state.card_sorts[CardId::ToolTable as usize] = SortState {
            column: 0,
            descending: false,
        };
        let tools = render_to_text(&mut state, 100, 24);
        let bash_row = tools.iter().position(|row| row.contains("Bash")).unwrap();
        let read_row = tools.iter().position(|row| row.contains("Read")).unwrap();
        assert!(bash_row < read_row);

        state.tab = ActiveTab::Hooks;
        let hooks_text = render_to_text(&mut state, 100, 24).join("\n");
        assert!(hooks_text.contains("거부 3 · 재작성 45"));
        assert!(hooks_text.contains("git push"));
        assert!(hooks_text.contains("정렬 횟수↓"));

        state.tab = ActiveTab::Models;
        let models_text = render_to_text(&mut state, 100, 30).join("\n");
        assert!(models_text.contains("추정 (API 단가)"));
        assert!(models_text.contains("$12.35"));
        assert!(models_text.contains("TTL 만료"));

        state.tab = ActiveTab::Gateway;
        state.config.gateway.enabled = true;
        let gateway_text = render_to_text(&mut state, 100, 30).join("\n");
        assert!(gateway_text.contains("첫 바이트"));
        assert!(gateway_text.contains("900ms"));
    }

    #[test]
    fn tool_and_hook_detail_cards_follow_the_selected_row() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Tools;
        let text = render_to_text(&mut state, 110, 30).join("\n");
        // 합계 내림차순 첫 행 = Bash
        assert!(text.contains("도구 상세 · Bash"));

        state.card_views[CardId::ToolTable as usize].selected = 1;
        let text = render_to_text(&mut state, 110, 30).join("\n");
        assert!(text.contains("도구 상세 · Read"));

        state.tab = ActiveTab::Hooks;
        let text = render_to_text(&mut state, 110, 30).join("\n");
        assert!(text.contains("훅 상세 · cliguard"));
    }

    #[test]
    fn settings_tab_splits_into_cards_with_independent_scroll_state() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Settings;
        let text = render_to_text(&mut state, 110, 36).join("\n");
        assert!(text.contains("훅 등록"));
        assert!(text.contains("MCP 서버"));
        assert!(text.contains("에이전트"));
        assert!(text.contains("진단"));
        for card in [
            CardId::SettingsHooks,
            CardId::SettingsMcp,
            CardId::SettingsAgents,
            CardId::SettingsDoctor,
        ] {
            assert!(state.hit_areas.card_rects.iter().any(|(id, _)| *id == card));
        }
        assert_eq!(
            state
                .hit_areas
                .split_hits
                .iter()
                .filter(|hit| hit.axis == crate::tui::layout::SplitAxis::Rows)
                .count(),
            3
        );
    }

    #[test]
    fn report_tab_styles_markdown_lists_outline_and_clamps_scroll() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Report;
        let text = render_to_text(&mut state, 100, 24).join("\n");
        assert!(text.contains("2026-11-14 (어제)"));
        assert!(text.contains("• 요청 1,234건"));
        // 목차 카드
        assert!(text.contains("목차"));
        assert!(text.contains("요약"));
        assert!(
            state
                .hit_areas
                .button_rects
                .iter()
                .any(|(action, _)| *action == UiAction::ReportPrev)
        );

        // End 로 과도하게 내려도 마지막 화면으로 보정
        state.card_views[CardId::ReportBody as usize].scroll = usize::MAX;
        render_to_text(&mut state, 100, 16);
        let clamped = state.card_views[CardId::ReportBody as usize].scroll;
        assert!(clamped < 20);
    }

    #[test]
    fn overview_shows_empty_state_when_database_missing() {
        let mut state = AppState::new(
            sample_config(),
            chrono::NaiveDate::from_ymd_opt(2026, 11, 15).unwrap(),
        );
        state.snapshot.db_state = DbState::Missing;
        let text = render_to_text(&mut state, 100, 30).join("\n");
        assert!(text.contains("DB 없음"));
        assert!(text.contains("serve 또는 index 실행 필요"));
        // 서버 응답이 없는 상태의 헤더
        assert!(text.contains("상태 확인 중"));
    }

    #[test]
    fn failed_panels_render_notice_instead_of_crashing() {
        let mut state = sample_state_with_snapshot();
        state.snapshot.overview = Panel::Failed("내부 오류: boom".to_string());
        state.snapshot.minutes = Panel::Pending;
        state.snapshot.events = Panel::Failed("조회 실패".to_string());
        let text = render_to_text(&mut state, 120, 36).join("\n");
        assert!(text.contains("내부 오류: boom"));
        assert!(text.contains("집계 대기 중"));
        assert!(text.contains("조회 실패"));
    }

    #[test]
    fn low_overview_keeps_status_kpi_and_feed_without_trend_row() {
        let mut state = sample_state_with_snapshot();
        let text = render_to_text(&mut state, 100, 16).join("\n");
        assert!(text.contains("서버 상태"));
        assert!(text.contains("캐시 적중률"));
        assert!(text.contains("실시간 이벤트"));
        assert!(!text.contains("분당 요청"));
    }

    #[test]
    fn gateway_tab_explains_how_to_enable_when_disabled() {
        let mut state = sample_state_with_snapshot();
        state.tab = ActiveTab::Gateway;
        let text = render_to_text(&mut state, 130, 30).join("\n");
        assert!(text.contains("게이트웨이 꺼짐"));
        assert!(text.contains("[gateway]  enabled = true"));
        assert!(text.contains("ANTHROPIC_BASE_URL=http://127.0.0.1:47332"));
        assert!(text.contains("ANTHROPIC_DEFAULT_HAIKU_MODEL"));
    }

    #[test]
    fn small_terminal_toast_and_help_are_drawn() {
        let mut state = sample_state_with_snapshot();
        let text = render_to_text(&mut state, 40, 10).join("\n");
        assert!(text.contains("터미널 크기 부족"));

        state.toast = Some(Toast {
            text: "serve 실행 후 사용 가능".to_string(),
            level: ToastLevel::Error,
            expires_at: Instant::now() + Duration::from_secs(4),
        });
        state.show_help = true;
        let text = render_to_text(&mut state, 100, 30).join("\n");
        assert!(text.contains("serve 실행 후 사용 가능"));
        assert!(text.contains("도움말"));
        assert!(text.contains("탭 이동"));
        assert!(text.contains("카드·행 클릭"));
    }

    fn card_rect_of(state: &AppState, wanted: CardId) -> Rect {
        state
            .hit_areas
            .card_rects
            .iter()
            .find(|(card, _)| *card == wanted)
            .map(|(_, rect)| *rect)
            .unwrap()
    }

    #[test]
    fn table_scrollbar_draws_thumb_in_gutter_and_keeps_focus_border_intact() {
        let mut state = sample_state_with_many_tools(40);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| render_frame(frame, &mut state))
            .unwrap();
        let table = card_rect_of(&state, CardId::ToolTable);
        let gutter_x = table.right() - 2;
        let border_x = table.right() - 1;
        let buffer = terminal.backend().buffer();

        // 본문 첫 행부터 거터에 썸 (포커스 테두리와 다른 회색), 머리글 행은 빈 칸
        let thumb = buffer.cell((gutter_x, table.y + 2)).unwrap();
        assert_eq!(thumb.symbol(), "█");
        assert_eq!(thumb.fg, theme::SCROLL_THUMB_FOCUS);
        assert_ne!(thumb.fg, theme::BORDER_FOCUS);
        assert_eq!(buffer.cell((gutter_x, table.y + 1)).unwrap().symbol(), " ");

        // 회귀 방지: 테두리 열은 본문 내내 포커스 색 굵은 세로선
        for y in table.y + 1..table.bottom() - 1 {
            let border = buffer.cell((border_x, y)).unwrap();
            assert_eq!(border.symbol(), "┃", "{y}");
            assert_eq!(border.fg, theme::BORDER_FOCUS, "{y}");
        }
        assert_eq!(
            buffer
                .cell((border_x, table.bottom() - 1))
                .unwrap()
                .symbol(),
            "┛"
        );

        // 트랙 칸은 ░, 썸 아래는 조용한 트랙색
        let rest = buffer.cell((gutter_x, table.bottom() - 2)).unwrap();
        assert_eq!(rest.symbol(), "░");
        assert_eq!(rest.fg, theme::SCROLL_TRACK);

        let hit = state
            .hit_areas
            .scroll_hits
            .iter()
            .find(|hit| hit.card == CardId::ToolTable)
            .unwrap();
        assert_eq!(hit.total, 40);
        assert_eq!(hit.track.x, gutter_x);
        assert_eq!(hit.track.width, 2);
        assert_eq!(hit.track.y, table.y + 2);
        assert_eq!(usize::from(hit.track.height), hit.visible);
    }

    #[test]
    fn selection_band_spans_padding_to_gutter_and_dims_when_card_is_unfocused() {
        let mut state = sample_state_with_many_tools(40);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| render_frame(frame, &mut state))
            .unwrap();
        let table = card_rect_of(&state, CardId::ToolTable);
        let band_y = table.y + 2;
        let buffer = terminal.backend().buffer();

        // 포커스 카드: 왼쪽 여백 열부터 거터 직전까지 진한 바탕, 거터와 테두리는 제외
        for x in table.x + 1..table.right() - 2 {
            assert_eq!(
                buffer.cell((x, band_y)).unwrap().bg,
                theme::BG_SELECTED,
                "{x}"
            );
        }
        assert_ne!(
            buffer.cell((table.x, band_y)).unwrap().bg,
            theme::BG_SELECTED
        );
        assert_ne!(
            buffer.cell((table.right() - 2, band_y)).unwrap().bg,
            theme::BG_SELECTED
        );
        assert_eq!(
            buffer.cell((table.x + 3, band_y)).unwrap().fg,
            theme::FG_STRONG
        );

        // 포커스가 다른 카드로 가면 바탕만 희미하게 (글자색 유지)
        state.focused_card = Some((ActiveTab::Tools, CardId::ToolDetail));
        terminal
            .draw(|frame| render_frame(frame, &mut state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let idle = buffer.cell((table.x + 3, band_y)).unwrap();
        assert_eq!(idle.bg, theme::BG_SELECTED_IDLE);
        assert_ne!(idle.fg, theme::FG_STRONG);
    }

    #[test]
    fn text_card_scrollbar_is_drawn_only_for_overflowing_cards() {
        let mut state = sample_state_with_many_events(60);
        let mut terminal = Terminal::new(TestBackend::new(130, 40)).unwrap();
        terminal
            .draw(|frame| render_frame(frame, &mut state))
            .unwrap();
        let detail = card_rect_of(&state, CardId::EventDetail);
        assert!(
            !state
                .hit_areas
                .scroll_hits
                .iter()
                .any(|hit| hit.card == CardId::EventDetail)
        );
        let feed = card_rect_of(&state, CardId::EventFeed);
        let hit = state
            .hit_areas
            .scroll_hits
            .iter()
            .find(|hit| hit.card == CardId::EventFeed)
            .unwrap();
        assert_eq!(detail.y, feed.y);
        let buffer = terminal.backend().buffer();
        let thumb = buffer.cell((hit.track.x, hit.track.y)).unwrap();
        assert_eq!(thumb.symbol(), "█");
        assert_eq!(hit.track.x, feed.right() - 2);
        assert_eq!(
            buffer
                .cell((feed.right() - 1, hit.track.y))
                .unwrap()
                .symbol(),
            "┃"
        );
    }

    #[test]
    fn no_scrollbar_when_rows_fit() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Tools;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| render_frame(frame, &mut state))
            .unwrap();
        let table = card_rect_of(&state, CardId::ToolTable);
        let buffer = terminal.backend().buffer();
        assert!(state.hit_areas.scroll_hits.is_empty());
        for y in table.y + 1..table.bottom() - 1 {
            assert_eq!(buffer.cell((table.right() - 1, y)).unwrap().symbol(), "┃");
        }
    }

    #[test]
    fn static_gateway_live_never_gets_scrollbar() {
        let mut state = sample_state_with_tables();
        state.config.gateway.enabled = true;
        state.tab = ActiveTab::Gateway;
        // 보기 상태가 넘치는 값이어도 고정 카드는 그리지 않음
        let live = &mut state.card_views[CardId::GatewayLive as usize];
        live.total = 50;
        live.visible = 5;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 5, 40, 8);
                let body = Rect::new(1, 6, 38, 6);
                common::render_card_scrollbar(frame, &mut state, CardId::GatewayLive, area, body);
            })
            .unwrap();
        assert!(state.hit_areas.scroll_hits.is_empty());
    }

    #[test]
    fn help_overlay_marks_truncation_at_minimum_height() {
        let mut state = sample_state_with_snapshot();
        state.show_help = true;
        let text = render_to_text(&mut state, MIN_WIDTH, MIN_HEIGHT).join("\n");
        assert!(text.contains('…'));
        assert!(text.contains("창을 키우면 전체 표시"));

        // 넓은 화면은 잘림 표시 없이 마지막 안내까지 표시
        let text = render_to_text(&mut state, 100, 40).join("\n");
        assert!(!text.contains("창을 키우면 전체 표시"));
        assert!(text.contains("스크롤바"));
    }

    #[test]
    fn all_eight_tabs_visible_and_clickable_at_minimum_width() {
        let mut state = sample_state_with_snapshot();
        // 탭 바는 둘째 줄
        let tab_row = render_to_text(&mut state, MIN_WIDTH, MIN_HEIGHT)[1].clone();
        assert_eq!(state.hit_areas.tab_rects.len(), 8);
        assert!(tab_row.contains("8리포트"));
        assert!(tab_row.contains("5에이전트"));
        assert!(tab_row.contains("6게이트"));
        assert!(!tab_row.contains("게이트웨이"));
        let last = state.hit_areas.tab_rects.last().unwrap().1;
        assert!(last.right() <= MIN_WIDTH);
    }

    #[test]
    fn tab_rects_never_overlap_or_overflow_at_any_width_from_minimum_to_wide() {
        for width in MIN_WIDTH..=120 {
            let mut state = sample_state_with_snapshot();
            render_to_text(&mut state, width, MIN_HEIGHT);
            let rects: Vec<Rect> = state
                .hit_areas
                .tab_rects
                .iter()
                .map(|(_, rect)| *rect)
                .collect();
            assert_eq!(rects.len(), 8, "{width}");
            for pair in rects.windows(2) {
                assert!(pair[0].right() <= pair[1].x, "{width}");
            }
            assert!(rects.last().unwrap().right() <= width, "{width}");
            let order: Vec<ActiveTab> = state
                .hit_areas
                .tab_rects
                .iter()
                .map(|(tab, _)| *tab)
                .collect();
            assert_eq!(order, ActiveTab::ALL, "{width}");
        }
    }

    #[test]
    fn wide_terminal_keeps_full_labels() {
        let mut state = sample_state_with_snapshot();
        let tab_row = render_to_text(&mut state, 100, 24)[1].clone();
        assert_eq!(state.hit_areas.tab_rects.len(), 8);
        assert!(tab_row.contains("6 게이트웨이"));
        assert!(tab_row.contains("5 에이전트"));

        // 정확히 전체 라벨이 들어가는 폭(82열)에서는 압축 없음
        let tab_row = render_to_text(&mut state, 82, 24)[1].clone();
        assert!(tab_row.contains("6 게이트웨이"));
        assert_eq!(state.hit_areas.tab_rects.len(), 8);
        // 한 칸 좁으면 짧은 이름 (간격 0), 더 좁으면 번호 붙임 압축
        let tab_row = render_to_text(&mut state, 81, 24)[1].clone();
        assert!(!tab_row.contains("게이트웨이"));
        assert!(tab_row.contains("6 게이트"));
        assert_eq!(state.hit_areas.tab_rects.len(), 8);
        let tab_row = render_to_text(&mut state, 70, 24)[1].clone();
        assert!(tab_row.contains("6게이트"));
        assert_eq!(state.hit_areas.tab_rects.len(), 8);
    }

    #[test]
    fn footer_hint_line_starts_at_one_column_margin() {
        let mut state = sample_state_with_snapshot();
        let rows = render_to_text(&mut state, 130, 40);
        let hint_row = &rows[39];
        assert!(hint_row.starts_with(" ↑↓ 행 선택"), "{hint_row}");

        // 알림이 있으면 알림 칩이 1열에서 시작
        state.toast = Some(Toast {
            text: "완료".to_string(),
            level: ToastLevel::Success,
            expires_at: Instant::now() + Duration::from_secs(4),
        });
        let rows = render_to_text(&mut state, 130, 40);
        assert!(rows[39].starts_with("  ● 완료"), "{}", rows[39]);
    }

    #[test]
    fn narrow_tables_ellipsize_names_first_and_keep_every_digit_of_numbers() {
        let mut state = sample_state_with_tables();
        state.snapshot.tools = Panel::Ready(vec![
            ToolLatencyRow {
                tool_name: "mcp__a-very-long-server-name-mcp__an-extremely-long-tool-name"
                    .to_string(),
                calls: 1_234_567,
                errors: 8_901,
                p50_ms: Some(12),
                p90_ms: Some(3_456),
                total_ms: 98_765_432,
            },
            ToolLatencyRow {
                tool_name: "Read".to_string(),
                calls: 1_300,
                total_ms: 21_000,
                ..ToolLatencyRow::default()
            },
        ]);
        state.tab = ActiveTab::Tools;
        for width in [60, 64, 70, 80, 100] {
            let text = render_to_text(&mut state, width, 24).join("\n");
            for digits in ["1,234,567", "8,901", "3.5s", "1,300", "21.0s", "27h 26m"] {
                assert!(text.contains(digits), "{width}: {digits}\n{text}");
            }
            if width <= 80 {
                assert!(text.contains('…'), "{width}");
            }
        }

        // 훅 표도 넓이가 모자라면 이름 열을 줄이고 숫자는 그대로
        state.snapshot.hooks = Panel::Ready(vec![HookStatRow {
            route: "pretool-chain-with-long-name".to_string(),
            guard: "remoteguard-with-a-long-name".to_string(),
            outcome: "deny".to_string(),
            detail: "git push --force origin main".to_string(),
            count: 123_456,
            p50_ms: Some(4),
            p90_ms: Some(9),
            saved_ms: 0,
        }]);
        state.tab = ActiveTab::Hooks;
        for width in [60, 70, 80, 100] {
            let text = render_to_text(&mut state, width, 24).join("\n");
            assert!(text.contains("123,456"), "{width}\n{text}");
        }
    }

    #[test]
    fn sorted_column_header_shows_an_arrow_in_every_table_tab() {
        let mut state = sample_state_with_tables();
        let cases = [
            (ActiveTab::Overview, "시각↓"),
            (ActiveTab::Tools, "합계↓"),
            (ActiveTab::Hooks, "횟수↓"),
            (ActiveTab::Models, "요청↓"),
            (ActiveTab::Agents, "요청↓"),
            (ActiveTab::Settings, "이벤트↑"),
        ];
        for (tab, arrow_label) in cases {
            state.tab = tab;
            let text = render_to_text(&mut state, 170, 60).join("\n");
            assert!(text.contains(arrow_label), "{tab:?}: {arrow_label}");
        }
        // 비정렬 표(게이트웨이 지연)에는 화살표 없음
        state.config.gateway.enabled = true;
        state.tab = ActiveTab::Gateway;
        let rows = render_to_text(&mut state, 100, 30);
        let latency = card_rect_of(&state, CardId::GatewayLatency);
        let card_text = rows[usize::from(latency.y)..usize::from(latency.bottom())].join("\n");
        assert!(card_text.contains("첫 바이트"));
        assert!(!card_text.contains('↑') && !card_text.contains('↓'));
    }

    #[test]
    fn models_and_agents_tabs_show_stats_tables_and_detail_cards() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Models;
        let models = render_to_text(&mut state, 150, 40).join("\n");
        assert!(models.contains("모델 통계"));
        assert!(models.contains("추정 (API 단가)"));
        assert!(models.contains("haiku-4-5"));
        assert!(models.contains("$12.35"));
        assert!(models.contains("84.2%"));
        assert!(models.contains("비용추정"));
        // 요청 내림차순 첫 행 = haiku, 상세 카드가 따라감
        assert!(models.contains("모델 상세 · claude-haiku-4-5"));
        // 캐시 붕괴 표는 모델 탭에
        assert!(models.contains("캐시 붕괴 후보"));
        assert!(models.contains("TTL 만료"));

        state.card_views[CardId::ModelTable as usize].selected = 1;
        let models = render_to_text(&mut state, 150, 40).join("\n");
        assert!(models.contains("모델 상세 · claude-sonnet-5-5"));

        state.tab = ActiveTab::Agents;
        let agents = render_to_text(&mut state, 150, 40).join("\n");
        assert!(agents.contains("에이전트 통계"));
        assert!(agents.contains("worker"));
        assert!(agents.contains("에이전트 상세 · worker"));
        assert!(agents.contains("주 모델"));
        assert!(!agents.contains("캐시 붕괴"));

        // 집계 전에는 안내 카드
        state.snapshot.agent_stats = Panel::Pending;
        let agents = render_to_text(&mut state, 150, 40).join("\n");
        assert!(agents.contains("집계 대기 중"));
        assert!(agents.contains("선택한 에이전트 없음"));
    }

    #[test]
    fn detail_cards_show_every_line_without_scroll_counter() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Agents;
        let agents = render_to_text(&mut state, 200, 55).join("\n");
        assert!(agents.contains("주 모델 / 마지막 요청"), "{agents}");
        assert!(!agents.contains("1-6 / 7"));

        state.tab = ActiveTab::Models;
        let models = render_to_text(&mut state, 200, 55).join("\n");
        assert!(models.contains("마지막 요청"), "{models}");
        assert!(!models.contains("1-5 / 6"));
    }

    #[test]
    fn report_prose_wraps_box_tables_ellipsize_and_outline_jump_follows_rows() {
        let prose = "가나다라마바사 ".repeat(30);
        let rule = "─".repeat(150);
        let markdown = format!(
            "# 리포트\n\n## 하나\n{prose}\n```text\n┌{rule}┐\n│ {rule} │\n└{rule}┘\n```\n## 둘\n끝 문장\n"
        );
        let mut state = sample_state_with_tables();
        state.report = Panel::Ready(Some(markdown.clone()));
        state.tab = ActiveTab::Report;
        let rows = render_to_text(&mut state, 100, 40);
        let text = rows.join("\n");

        // 일반 문장은 잘림 없이 전부 표시, 박스 표 줄은 끝 …
        assert_eq!(text.matches("가나다라마바사").count(), 30, "{text}");
        assert!(rows.iter().any(|row| row.contains("─…")), "{text}");

        // 스크롤 총 행 수는 줄바꿈 반영, 시작 행은 단조 증가
        let starts = state.report_row_starts.clone();
        let total = state.card_views[CardId::ReportBody as usize].total;
        assert!(total > starts.len(), "{total} {}", starts.len());
        assert!(starts.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(starts.windows(2).any(|pair| pair[1] - pair[0] > 1));

        // 목차 하나를 누르면 줄바꿈 뒤 행으로 이동
        let entries = crate::tui::format::outline_entries(
            &crate::tui::format::prepare_report_markdown(&markdown),
        );
        let last = entries.last().expect("목차");
        state.select_card_row(CardId::ReportOutline, entries.len() - 1);
        let scroll = state.card_views[CardId::ReportBody as usize].scroll;
        assert_eq!(scroll, starts[last.line]);
        assert!(scroll > last.line);
    }

    #[test]
    fn kpi_values_end_with_ellipsis_instead_of_cutting_digits_at_narrow_width() {
        let mut state = sample_state_with_snapshot();
        if let Panel::Ready(stats) = &mut state.snapshot.overview {
            stats.api_requests = 12_345_678;
            stats.input_tokens = 2_345_678;
            stats.output_tokens = 410_000;
        }
        let text = render_to_text(&mut state, 60, 16).join("\n");
        assert!(text.contains('…'), "{text}");
        assert!(!text.contains("2.3M/41 "), "{text}");
    }

    #[test]
    fn help_overlay_keeps_one_column_padding_inside_the_border() {
        let mut state = sample_state_with_snapshot();
        state.show_help = true;
        let text = render_to_text(&mut state, 100, 40).join("\n");
        assert!(text.contains("║ 키보드"), "{text}");
        assert!(!text.contains("║키보드"), "{text}");
    }

    #[test]
    fn report_hides_text_fences_and_shows_box_tables_in_muted_lines() {
        let mut state = sample_state_with_tables();
        state.report = Panel::Ready(Some(
            "# 리포트\n\n## 표\n```text\n┌──────┬────┐\n│ 모델 │ 요청 │\n├──────┼────┤\n│ a    │   9 │\n└──────┴────┘\n```\n\n레거시\n| 항목 | 값 |\n|---|---|\n| x | 1 |\n"
                .to_string(),
        ));
        state.tab = ActiveTab::Report;
        let text = render_to_text(&mut state, 110, 30).join("\n");
        let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
        terminal
            .draw(|frame| render_frame(frame, &mut state))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut corner = None;
        for y in 0..30 {
            for x in 0..110 {
                let cell = buffer.cell((x, y)).unwrap();
                if cell.symbol() == "┌" && corner.is_none() {
                    corner = Some(cell.fg);
                }
            }
        }
        assert!(!text.contains("```"));
        assert!(!text.contains('|'));
        assert!(text.contains("│ 모델"));
        assert!(text.contains("│ x"));
        assert_eq!(corner, Some(theme::FG_TABLE_LINE));

        // 목차 클릭 위치는 숨긴 울타리 줄을 빼고 센 본문 줄 기준
        state.select_card_row(CardId::ReportOutline, 1);
        assert_eq!(state.card_views[CardId::ReportBody as usize].scroll, 2);
    }

    #[test]
    fn help_overlay_borders_stay_intact_next_to_wide_korean_characters() {
        let area = Rect::new(0, 0, 100, 40);
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal
            .draw(|frame| {
                // 배경 한글이 상자 왼쪽 경계에 걸침
                let backdrop = format!("a{}", "한".repeat(49));
                let lines: Vec<Line> = (0..40).map(|_| Line::from(backdrop.as_str())).collect();
                frame.render_widget(Paragraph::new(lines), area);
                render_help_overlay(frame, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut left = None;
        let mut right = None;
        for x in 0..100 {
            if buffer.cell((x, 12)).unwrap().symbol() == "║" {
                left.get_or_insert(x);
                right = Some(x);
            }
        }
        let (left, right) = (left.unwrap(), right.unwrap());
        assert!(right > left);
        for y in 0..40 {
            let first = buffer.cell((left, y)).unwrap().symbol();
            if !matches!(first, "║" | "╔" | "╚") {
                continue;
            }
            // 왼쪽 바깥 칸은 넓은 글자가 아니고, 오른쪽 테두리도 온전
            let outside = buffer.cell((left - 1, y)).unwrap().symbol();
            assert!(UnicodeWidthStr::width(outside) <= 1, "{y}: {outside}");
            let last = buffer.cell((right, y)).unwrap().symbol();
            assert!(matches!(last, "║" | "╗" | "╝"), "{y}: {last}");
        }
    }

    #[test]
    fn every_tab_renders_at_minimum_size_without_panic() {
        for tab in ActiveTab::ALL {
            let mut state = sample_state_with_tables();
            state.config.gateway.enabled = true;
            state.tab = tab;
            render_to_text(&mut state, MIN_WIDTH, MIN_HEIGHT);
            render_to_text(&mut state, 200, 60);
        }
    }
}
