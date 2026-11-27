//! tui/views/mod.rs
//! 화면 조립: 앱 바, 탭 바, 본문, 하단 버튼·안내, 카드 경계 손잡이, 도움말 겹침.

pub mod charts;
pub mod common;
pub mod detail;
pub mod feed;
pub mod models;
pub mod overview;
pub mod projects;
#[cfg(test)]
mod render_matrix;
pub mod report;
pub mod settings;
pub mod skills;
pub mod tables;

use crate::tui::cards::{CardKind, UiAction, UiHitAreas};
use crate::tui::format::fit_display;
use crate::tui::i18n::{self, Lang, Msg};
use crate::tui::poller::POLL_INTERVAL;
use crate::tui::sort::sort_spec;
use crate::tui::state::{ActiveTab, AppState, ToastLevel};
use crate::tui::theme;
use crate::tui::views::common::{render_column_handles, render_split_handles};
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
            i18n::too_small(state.lang, MIN_WIDTH, MIN_HEIGHT, area.width, area.height),
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
        ActiveTab::Settings => settings::render_settings_tab(frame, chunks[2], state),
        ActiveTab::Report => report::render_report_tab(frame, chunks[2], state),
        ActiveTab::Projects => projects::render_projects_tab(frame, chunks[2], state),
        ActiveTab::Skills => skills::render_skills_tab(frame, chunks[2], state),
    }
    render_footer(frame, chunks[3], state);

    // 3. 카드·열 경계 손잡이와 도움말 겹침
    render_split_handles(frame, state);
    render_column_handles(frame, state);
    if state.show_help {
        render_help_overlay(frame, area, state.lang);
    }
}

// 2. 앱 바 --------------------------------------------------------------------
// 왼쪽 배지·최상단 표시, 오른쪽 구간과 갱신 신선도 (폴링 주기 3배 넘으면 지연 표시)
fn render_app_bar(frame: &mut Frame, area: Rect, state: &AppState) {
    // 1. 신선도 표시
    let lang = state.lang;
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
                    Span::styled(i18n::stale(lang, age_secs), theme::STYLE_WARN),
                ]
            }
        }
        None => vec![Span::styled(
            format!("○ {}", i18n::text(lang, Msg::AppBarWaiting)),
            theme::STYLE_DIM,
        )],
    };
    let mut right_spans = vec![
        Span::styled(
            format!("{} ", i18n::text(lang, Msg::AppBarWindow)),
            theme::STYLE_DIM,
        ),
        Span::styled(state.window.window_label(lang), theme::STYLE_ACCENT),
        Span::raw("  "),
    ];
    right_spans.extend(freshness);
    right_spans.push(Span::raw(" "));
    let right = Line::from(right_spans);
    let right_width = (right.width() as u16).min(area.width / 2);
    let columns =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).split(area);

    // 2. 왼쪽 배지
    let mut left_spans = vec![Span::styled(
        " claude-monitor ",
        Style::default()
            .fg(theme::FG_ON_ACCENT)
            .bg(theme::FG_ACCENT)
            .add_modifier(Modifier::BOLD),
    )];
    if state.always_on_top {
        left_spans.push(Span::raw("  "));
        left_spans.push(Span::styled(
            format!(" {} ", i18n::text(lang, Msg::BadgePinned)),
            theme::STYLE_PINNED,
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
// 탭마다 클릭 영역 등록, 전체 폭을 넘으면 간격 0, 그래도 넘으면 번호 붙임·여백 없는 압축, 마지막은 현재 탭만 전체 이름
fn render_tab_bar(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let row_end = area.x.saturating_add(area.width);
    let mut cursor = area.x.saturating_add(1);

    // 1. 압축 단계 (0 전체, 1 간격 0, 2 번호 붙임·여백 없음·간격 1)
    let tab_count = ActiveTab::ALL.len();
    let label_width = |tab: ActiveTab| Span::raw(tab.tab_label(state.lang)).width();
    let full_labels: usize = ActiveTab::ALL.iter().map(|tab| label_width(*tab)).sum();
    let full_width = full_labels + 4 * tab_count + tab_count;
    let compact_width = full_labels + 4 * tab_count + 1;
    // 단계 3 = 번호 붙임·여백 없음·간격 0·왼쪽 여백 0 (번호 붙인 압축으로도 모자랄 때)
    let tight_width = full_labels + 2 * tab_count;
    let glued_width = full_labels + tab_count;
    let available = area.width as usize;
    let level = if full_width <= available {
        0
    } else if compact_width <= available {
        1
    } else if tight_width <= available {
        2
    } else if glued_width <= available {
        3
    } else {
        4
    };
    let gap = u16::from(level == 0 || level == 2);
    if level >= 3 {
        cursor = area.x;
    }
    // 단계 4 = 단계 3 에서 다른 탭 이름을 같은 폭 안으로 말줄임 (들어가는 가장 넓은 폭, 현재 탭은 전체 이름)
    let current_width = label_width(state.tab);
    let fits = |limit: usize| {
        let others: usize = ActiveTab::ALL
            .iter()
            .filter(|tab| **tab != state.tab)
            .map(|tab| label_width(*tab).min(limit))
            .sum();
        tab_count + current_width + others <= available
    };
    let widest = ActiveTab::ALL
        .iter()
        .map(|tab| label_width(*tab))
        .max()
        .unwrap_or(0);
    let other_limit = (0..=widest).rev().find(|limit| fits(*limit)).unwrap_or(0);

    // 2. 탭 그리기
    let tab_order = state.tab_order;
    for (index, tab) in tab_order.iter().enumerate() {
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
        let full_label = tab.tab_label(state.lang);
        let label = if level == 4 && state.tab != *tab && label_width(*tab) > other_limit {
            fit_display(full_label, other_limit)
        } else {
            full_label.to_string()
        };
        let line = match level {
            2..=4 => Line::from(vec![
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
// 칩 = 키 글자(강조색) + 이름, 폭이 모자라면 왼쪽 그룹의 뒤쪽부터 생략 (언어 칩은 마지막까지 유지)
fn render_footer_buttons(frame: &mut Frame, row: Rect, state: &mut AppState) {
    // 1. 칩 스타일 (핀 켜짐만 채움, 종료는 키 글자만 적색)
    let chip = Style::default().bg(theme::BG_TONAL);
    let key_style = chip.fg(theme::FG_ACCENT).add_modifier(Modifier::BOLD);
    let name_style = chip.fg(theme::FG_TEXT);
    let quit_key_style = chip.fg(theme::COLOR_CRIT).add_modifier(Modifier::BOLD);
    let (pin_key, pin_name) = if state.always_on_top {
        (theme::STYLE_PINNED, theme::STYLE_PINNED)
    } else {
        (key_style, name_style)
    };
    let lang = state.lang;
    let pin_label = if state.always_on_top {
        format!(" {} ", i18n::text(lang, Msg::ChipPinOn))
    } else {
        format!(" {} ", i18n::text(lang, Msg::ChipPinOff))
    };
    let window_label = format!(
        " {} {} ",
        i18n::text(lang, Msg::AppBarWindow),
        state.window.window_label(lang)
    );
    let chip_name = |msg: Msg| format!(" {} ", i18n::text(lang, msg));
    let mut left_chips: Vec<(UiAction, Line<'static>)> = vec![
        (
            UiAction::Reindex,
            Line::from(vec![
                Span::styled(" [i]", key_style),
                Span::styled(chip_name(Msg::ChipReindex), name_style),
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
                Span::styled(chip_name(Msg::ChipRefresh), name_style),
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
                Span::styled(chip_name(Msg::ChipResetLayout), name_style),
            ]),
        ),
    ];
    let language_chip: (UiAction, Line<'static>) = (
        UiAction::ToggleLanguage,
        Line::from(vec![
            Span::styled(" [L]", key_style),
            Span::styled(chip_name(Msg::ChipLanguage), name_style),
        ]),
    );
    // 빌드 불일치면 강제 재시작 칩을 경고색으로 맨 앞에 (폭이 모자라도 마지막까지 남음)
    if state.snapshot.serve_stale {
        let warn_key = chip.fg(theme::COLOR_WARN).add_modifier(Modifier::BOLD);
        left_chips.insert(
            0,
            (
                UiAction::ForceRestart,
                Line::from(vec![
                    Span::styled(" [!]", warn_key),
                    Span::styled(chip_name(Msg::ChipForceRestart), chip.fg(theme::COLOR_WARN)),
                ]),
            ),
        );
    }
    let right_chips: Vec<(UiAction, Line<'static>)> = vec![
        (
            UiAction::ShowHelp,
            Line::from(vec![
                Span::styled(" [?]", key_style),
                Span::styled(chip_name(Msg::ChipHelp), name_style),
            ]),
        ),
        (
            UiAction::Quit,
            Line::from(vec![
                Span::styled(" [q]", quit_key_style),
                Span::styled(chip_name(Msg::ChipQuit), name_style),
            ]),
        ),
    ];

    // 2. 배치 (오른쪽 그룹은 오른쪽 끝 1열 앞, 왼쪽 그룹은 남는 폭까지만, 언어 칩은 맨 끝 자리를 남긴 채 다른 칩부터 생략)
    let row_end = row.x.saturating_add(row.width);
    let right_width: u16 = right_chips
        .iter()
        .map(|(_, line)| line.width() as u16 + 1)
        .sum::<u16>()
        .saturating_sub(1);
    let right_start = row_end.saturating_sub(1 + right_width).max(row.x);
    let mut placed: Vec<(UiAction, Rect, Line<'static>)> = Vec::new();
    let mut cursor = row.x.saturating_add(1);
    let language_width = language_chip.1.width() as u16;
    for (action, line) in left_chips {
        let width = line.width() as u16;
        if cursor.saturating_add(width + 1 + language_width) >= right_start {
            break;
        }
        placed.push((action, Rect::new(cursor, row.y, width, row.height), line));
        cursor = cursor.saturating_add(width + 1);
    }
    if cursor.saturating_add(language_width) < right_start {
        let (action, line) = language_chip;
        placed.push((
            action,
            Rect::new(cursor, row.y, language_width, row.height),
            line,
        ));
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
    let lang = state.lang;
    let hint_texts = |msgs: &[Msg]| -> Vec<&'static str> {
        msgs.iter().map(|msg| i18n::text(lang, *msg)).collect()
    };
    let mut context_hints: Vec<&str> = Vec::new();
    let tab_moving = state
        .tab_drag
        .is_some_and(|drag| drag.baseline != state.tab_order);
    if state.card_move.is_some() || tab_moving {
        // 카드·탭 이동 중에는 놓기·취소 안내가 맨 앞, 다른 맥락 안내는 생략
        context_hints.extend(hint_texts(&[Msg::HintRelease, Msg::HintCancel]));
    } else {
        if state.split_hover.is_some() || state.split_drag.is_some() {
            context_hints.extend(hint_texts(&[Msg::HintDragCardSize, Msg::HintResetLayout]));
        }
        if state.column_hover.is_some() || state.column_drag.is_some() {
            context_hints.extend(hint_texts(&[
                Msg::HintDragColumnWidth,
                Msg::HintDoubleClickAuto,
                Msg::HintResetLayout,
            ]));
        }
        if state.scroll_hover.is_some() || state.scroll_drag.is_some() {
            context_hints.extend(hint_texts(&[Msg::HintDragScroll, Msg::HintTrackPage]));
        }
        match state.focused_card_id().kind() {
            CardKind::Selectable => {
                context_hints.extend(hint_texts(&[
                    Msg::HintSelectRow,
                    Msg::HintPage,
                    Msg::HintFirstLast,
                ]));
            }
            CardKind::Scrollable => {
                context_hints.extend(hint_texts(&[Msg::HintScroll, Msg::HintPage]));
            }
            CardKind::Static => {}
        }
        if sort_spec(state.focused_card_id()).is_some()
            || sort_spec(state.tab.main_card()).is_some()
        {
            context_hints.extend(hint_texts(&[Msg::HintSortColumn, Msg::HintSortOrder]));
        }
        if state.tab == ActiveTab::Report {
            let date_hint = if state.report_weekly {
                Msg::HintReportWeek
            } else {
                Msg::HintReportDate
            };
            context_hints.extend(hint_texts(&[
                date_hint,
                Msg::HintGenerate,
                Msg::HintReportView,
            ]));
        }
        if state.tab == ActiveTab::Projects {
            context_hints.extend(hint_texts(&[Msg::HintDimension]));
        }
    }
    let tail_hints = hint_texts(&[Msg::HintNextTab, Msg::HintAllKeys]);

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
fn render_help_overlay(frame: &mut Frame, area: Rect, lang: Lang) {
    // 1. 도움말 내용 (뜻이 빈 줄 = 구역 제목)
    let help_rows = i18n::help_rows(lang);
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
        .title(Span::styled(
            format!(" {} ", i18n::text(lang, Msg::HelpTitle)),
            theme::STYLE_STRONG,
        ))
        .title_bottom(
            Line::from(Span::styled(
                format!(" {} ", i18n::text(lang, Msg::HelpClose)),
                theme::STYLE_DIM,
            ))
            .right_aligned(),
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
            i18n::text(lang, Msg::HelpTruncated),
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
    use crate::tui::i18n::Failure;
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

        assert!(text.contains("claude-monitor"));
        assert!(text.contains("1 개요"));
        assert!(text.contains("serve 실행 중"));
        assert!(text.contains("게이트웨이 ON"));
        assert!(text.contains("요청"));
        assert!(text.contains("캐시 적중률"));
        assert!(text.contains("87.5%"));
        assert!(text.contains("훅 재작성 효과"));
        assert!(text.contains("순효과"));
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
        assert!(text.contains("[w] 구간 오늘"));
        assert!(text.contains("[t] 최상단 OFF"));
        assert!(text.contains("[L] KO"));
        assert!(text.contains("[q] 종료"));
        let actions: Vec<UiAction> = state
            .hit_areas
            .button_rects
            .iter()
            .map(|(action, _)| *action)
            .collect();
        assert_eq!(actions.len(), 8);
        assert!(actions.contains(&UiAction::ToggleLanguage));
        assert!(actions.contains(&UiAction::Reindex));
        assert!(actions.contains(&UiAction::Quit));
        assert_eq!(state.hit_areas.tab_rects.len(), 9);
    }

    #[test]
    fn force_restart_chip_leads_the_footer_only_while_serve_runs_another_build() {
        // 1. 같은 빌드: 칩 없음
        let mut state = sample_state_with_snapshot();
        let normal = render_to_text(&mut state, 130, 40).join("\n");
        assert!(!normal.contains("[!]"), "{normal}");
        assert!(
            state
                .hit_areas
                .button_rects
                .iter()
                .all(|(action, _)| *action != UiAction::ForceRestart)
        );

        // 2. 불일치: 경고색 칩이 맨 앞, 최소 폭에서도 남고 클릭 영역 등록
        state.snapshot.serve_stale = true;
        for (width, height) in [(130, 40), (MIN_WIDTH, MIN_HEIGHT)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render_frame(frame, &mut state))
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            let (action, rect) = state.hit_areas.button_rects[0];
            assert_eq!(action, UiAction::ForceRestart, "{width}x{height}");
            let footer = row_text(&buffer, rect.y, width);
            assert!(
                footer.starts_with("  [!] 강제 재시작 "),
                "{width}x{height}: {footer}"
            );
            assert_eq!(buffer.cell((rect.x + 1, rect.y)).unwrap().symbol(), "[");
            assert_eq!(
                buffer.cell((rect.x + 1, rect.y)).unwrap().fg,
                theme::COLOR_WARN,
                "{width}x{height}"
            );
        }
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
        assert!(actions.contains(&UiAction::ToggleLanguage));
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
        state.split_hover = Some((
            crate::tui::layout::SplitGroup::GridRows(ActiveTab::Overview, 5),
            0,
        ));
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
        let models_text = render_to_text(&mut state, 130, 30).join("\n");
        assert!(models_text.contains("추정 (API 단가)"));
        assert!(models_text.contains("$12.35"));
        assert!(models_text.contains("TTL 만료"));
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
        assert!(text.contains("2026-11-15 (오늘)"));
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
        state.lang = Lang::Ko;
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
        state.snapshot.overview = Panel::Failed(Failure::Internal("boom".to_string()));
        state.snapshot.minutes = Panel::Pending;
        state.snapshot.events = Panel::Failed(Failure::Error("조회 실패".to_string()));
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
    fn app_bar_shows_only_the_badge_without_a_caption() {
        let mut state = sample_state_with_snapshot();
        let app_bar = render_to_text(&mut state, 130, 40)[0].clone();
        assert!(app_bar.contains("claude-monitor"));
        assert!(!app_bar.contains("Claude Code 관제"));
        assert!(!app_bar.contains("관제"));
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
    fn static_kpi_card_never_gets_scrollbar() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Overview;
        // 보기 상태가 넘치는 값이어도 고정 카드는 그리지 않음
        let kpi = &mut state.card_views[CardId::KpiTools as usize];
        kpi.total = 50;
        kpi.visible = 5;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 5, 40, 8);
                let body = Rect::new(1, 6, 38, 6);
                common::render_card_scrollbar(frame, &mut state, CardId::KpiTools, area, body);
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
    fn all_nine_tabs_visible_and_clickable_at_minimum_width() {
        let mut state = sample_state_with_snapshot();
        // 탭 바는 둘째 줄
        let tab_row = render_to_text(&mut state, MIN_WIDTH, MIN_HEIGHT)[1].clone();
        assert_eq!(state.hit_areas.tab_rects.len(), 9);
        assert!(tab_row.contains("9스킬"));
        assert!(tab_row.contains("7리포트"));
        assert!(tab_row.contains("6설정"));
        assert!(tab_row.contains("5에이전트"));
        assert!(!tab_row.contains("게이트"));
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
            assert_eq!(rects.len(), 9, "{width}");
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
        assert_eq!(state.hit_areas.tab_rects.len(), 9);
        assert!(tab_row.contains("5 에이전트"));
        assert!(tab_row.contains("6 설정"));
        assert!(tab_row.contains("7 리포트"));

        // 정확히 전체 라벨이 들어가는 폭(89열)에서는 압축 없음 (탭 사이 간격 1)
        let tab_row = render_to_text(&mut state, 89, 24)[1].clone();
        assert!(tab_row.contains("9 스킬"));
        assert_eq!(state.hit_areas.tab_rects.len(), 9);
        let rects = &state.hit_areas.tab_rects;
        assert_eq!(rects[1].1.x, rects[0].1.right() + 1);
        // 한 칸 좁으면 간격 0, 더 좁으면 번호 붙임 압축
        let tab_row = render_to_text(&mut state, 88, 24)[1].clone();
        assert!(tab_row.contains("9 스킬"));
        assert_eq!(state.hit_areas.tab_rects.len(), 9);
        let rects = &state.hit_areas.tab_rects;
        assert_eq!(rects[1].1.x, rects[0].1.right());
        let tab_row = render_to_text(&mut state, 60, 24)[1].clone();
        assert!(tab_row.contains("7리포트"));
        assert!(tab_row.contains("9스킬"));
        assert_eq!(state.hit_areas.tab_rects.len(), 9);
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
                avg_ms: Some(12),
                max_ms: Some(3_456),
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
            avg_ms: Some(4),
            max_ms: Some(9),
            total_ms: 493_824,
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
                render_help_overlay(frame, area, Lang::Ko);
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

    fn buffer_of(state: &mut AppState, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_frame(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn row_text(buffer: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
        let mut text = String::new();
        let mut skip = 0usize;
        for x in 0..width {
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

    fn settings_state() -> AppState {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Settings;
        state
    }

    #[test]
    fn moved_card_in_move_mode_has_a_dashed_accent_border_and_a_moving_title() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut state = settings_state();
        let mut terminal = Terminal::new(TestBackend::new(110, 36)).unwrap();
        terminal
            .draw(|frame| render_frame(frame, &mut state))
            .unwrap();
        let mcp = card_rect_of(&state, CardId::SettingsMcp);
        let title = state
            .hit_areas
            .title_hits
            .iter()
            .find(|hit| hit.card == CardId::SettingsMcp)
            .map(|hit| hit.rect)
            .unwrap();
        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: title.x + 1,
            row: title.y,
            modifiers: KeyModifiers::NONE,
        };
        let base = Instant::now();

        // 누르는 동안(400ms 전)에는 일반 포커스 모양, 이동 표시 없음
        state.handle_mouse_event_at(down, base);
        let pressed = buffer_of(&mut state, 110, 36);
        assert_eq!(pressed.cell((mcp.x, mcp.y)).unwrap().symbol(), "┏");
        assert!(!row_text(&pressed, mcp.y, 110).contains("이동 ·"));

        // 400ms: 이동 모드, 들어 올린 카드는 점선 테두리와 강조색, 제목에 이동 표시 (이중선은 겹침 창 전용이라 쓰지 않음)
        assert!(state.tick(base + Duration::from_millis(400)));
        let lifted = buffer_of(&mut state, 110, 36);
        let corner = lifted.cell((mcp.x, mcp.y)).unwrap();
        assert_eq!(corner.symbol(), "┌");
        assert_eq!(corner.fg, theme::FG_ACCENT);
        assert_ne!(corner.fg, theme::BORDER_FOCUS);
        let right = lifted.cell((mcp.right() - 1, mcp.y)).unwrap();
        assert_eq!((right.symbol(), right.fg), ("┐", theme::FG_ACCENT));
        let bottom_left = lifted.cell((mcp.x, mcp.bottom() - 1)).unwrap();
        assert_eq!(bottom_left.symbol(), "└");
        assert_eq!(lifted.cell((mcp.x, mcp.y + 1)).unwrap().symbol(), "╎");
        for y in 0..36 {
            assert!(!row_text(&lifted, y, 110).contains('╔'), "{y}");
        }
        let top = row_text(&lifted, mcp.y, 110);
        assert!(top.contains("이동 · MCP 서버"), "{top}");
        // 제목 글자는 강조색 바탕 반전
        let title_cell = lifted.cell((mcp.x + 2, mcp.y)).unwrap();
        assert_eq!(title_cell.bg, theme::FG_ACCENT);
        assert_eq!(title_cell.fg, theme::FG_ON_ACCENT);

        // 다른 카드는 이중선이 아님 (포커스 카드는 이 탭의 주 카드 대신 눌린 카드)
        let hooks = card_rect_of(&state, CardId::SettingsHooks);
        assert_eq!(lifted.cell((hooks.x, hooks.y)).unwrap().symbol(), "╭");
        let doctor = card_rect_of(&state, CardId::SettingsDoctor);
        assert_eq!(lifted.cell((doctor.x, doctor.y)).unwrap().symbol(), "╭");

        // 푸터 안내 줄: 놓기·취소가 맨 앞
        let hint_row = row_text(&lifted, 35, 110);
        assert!(hint_row.contains("놓기 위치 확정"), "{hint_row}");
        assert!(hint_row.contains("Esc 취소"), "{hint_row}");
        assert!(
            hint_row.find("놓기 위치 확정").unwrap() < hint_row.find("Tab 다음 탭").unwrap(),
            "{hint_row}"
        );
        assert!(!hint_row.contains("정렬 열"), "{hint_row}");

        // 이동 중에도 설정 탭 격자 줄은 그려져 놓을 자리를 판정할 수 있음
        assert!(state.hit_areas.grid_lines.iter().any(|line| {
            line.tab == ActiveTab::Settings
                && line
                    .cards
                    .iter()
                    .any(|(card, _)| *card == CardId::SettingsMcp)
        }));

        // Esc 로 취소하면 다시 둥근 모서리
        state.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            KeyModifiers::NONE,
        ));
        let restored = buffer_of(&mut state, 110, 36);
        assert_eq!(restored.cell((mcp.x, mcp.y)).unwrap().symbol(), "┏");
        assert!(!row_text(&restored, mcp.y, 110).contains("이동 ·"));
        let plain_hints = row_text(&restored, 35, 110);
        assert!(!plain_hints.contains("놓기 위치 확정"), "{plain_hints}");
    }

    #[test]
    fn settings_grid_move_draws_mcp_on_the_first_line_and_hooks_below_it() {
        use crate::tui::layout::grid::{DropTarget, apply_drop, default_rows};

        for (width, height) in [(110u16, 36u16), (MIN_WIDTH, MIN_HEIGHT)] {
            let mut state = settings_state();
            let before = buffer_of(&mut state, width, height);
            let first_line = |state: &AppState| {
                state
                    .hit_areas
                    .grid_lines
                    .iter()
                    .filter(|line| line.tab == ActiveTab::Settings)
                    .map(|line| line.cards.iter().map(|(card, _)| *card).collect())
                    .collect::<Vec<Vec<CardId>>>()
            };
            // 기본 격자: 네 카드가 한 줄씩, 훅 등록이 맨 위
            let lines = first_line(&state);
            assert_eq!(lines.len(), 4, "{width}x{height}");
            assert_eq!(lines[0], [CardId::SettingsHooks], "{width}x{height}");
            let hooks = card_rect_of(&state, CardId::SettingsHooks);
            assert!(row_text(&before, hooks.y, width).contains("훅 등록"));

            // MCP 서버를 맨 위 새 줄로
            let moved = apply_drop(
                &default_rows(ActiveTab::Settings),
                CardId::SettingsMcp,
                DropTarget::RowAbove(CardId::SettingsHooks),
            )
            .unwrap();
            state.card_layout.set_grid(ActiveTab::Settings, moved);
            let after = buffer_of(&mut state, width, height);
            let lines = first_line(&state);
            assert_eq!(
                lines,
                [
                    vec![CardId::SettingsMcp],
                    vec![CardId::SettingsHooks],
                    vec![CardId::SettingsAgents],
                    vec![CardId::SettingsDoctor],
                ],
                "{width}x{height}"
            );
            let mcp = card_rect_of(&state, CardId::SettingsMcp);
            let hooks = card_rect_of(&state, CardId::SettingsHooks);
            assert!(mcp.y < hooks.y, "{width}x{height}");
            let first_row = row_text(&after, mcp.y, width);
            assert!(
                first_row.contains("MCP 서버"),
                "{width}x{height}: {first_row}"
            );
            assert!(
                !first_row.contains("훅 등록"),
                "{width}x{height}: {first_row}"
            );
            let second_row = row_text(&after, hooks.y, width);
            assert!(
                second_row.contains("훅 등록"),
                "{width}x{height}: {second_row}"
            );
        }
    }

    #[test]
    fn every_grid_card_renders_at_every_drop_target_at_both_sizes_inside_its_line() {
        use crate::tui::layout::grid::{DropTarget, apply_drop, default_rows};

        // 격자의 모든 카드를 다른 모든 카드의 앞·뒤·위 새 줄·아래 새 줄로 옮긴 격자 목록
        let mut cases = Vec::new();
        for tab in ActiveTab::ALL {
            let base = default_rows(tab);
            let cards: Vec<CardId> = base.rows.iter().flatten().flatten().copied().collect();
            let moves = cards
                .iter()
                .flat_map(|moving| cards.iter().map(move |anchor| (*moving, *anchor)));
            for (moving, anchor) in moves {
                for target in [
                    DropTarget::Before(anchor),
                    DropTarget::After(anchor),
                    DropTarget::RowAbove(anchor),
                    DropTarget::RowBelow(anchor),
                ] {
                    cases.extend(apply_drop(&base, moving, target).map(|grid| (tab, grid)));
                }
            }
        }

        // 그려진 격자 카드는 줄 영역 안에 있고 카드 영역은 격자가 준 영역과 같음
        for (tab, grid) in cases {
            for (width, height) in [(110u16, 40u16), (MIN_WIDTH, MIN_HEIGHT)] {
                let mut state = sample_state_with_tables();
                state.tab = tab;
                state.card_layout.set_grid(tab, grid);
                render_to_text(&mut state, width, height);
                let placed = state
                    .hit_areas
                    .grid_lines
                    .iter()
                    .flat_map(|line| line.cards.iter().map(move |card| (line.rect, *card)));
                for (line_rect, (card, rect)) in placed {
                    let case = format!("{tab:?} {card:?} {width}x{height} {grid:?}");
                    assert_eq!(line_rect.union(rect), line_rect, "{case}");
                    let drawn = state
                        .hit_areas
                        .card_rects
                        .iter()
                        .find(|(drawn, _)| *drawn == card);
                    if let Some((_, drawn_rect)) = drawn {
                        assert_eq!(*drawn_rect, rect, "{case}");
                    }
                }
            }
        }
    }

    #[test]
    fn pending_trend_cards_each_show_their_own_notice_side_by_side_or_apart() {
        use crate::tui::layout::grid::{DropTarget, apply_drop, default_rows};

        // 분당 추이가 준비되지 않은 개요: 이웃해도 떨어져도 카드마다 자기 제목의 안내 (합친 안내 카드 없음)
        let base = default_rows(ActiveTab::Overview);
        let apart = apply_drop(
            &base,
            CardId::TrendTokens,
            DropTarget::RowBelow(CardId::TrendRequests),
        )
        .unwrap();
        for grid in [base, apart] {
            let mut state = sample_state_with_snapshot();
            state.snapshot.minutes = Panel::Pending;
            state.card_layout.set_grid(ActiveTab::Overview, grid);
            let text = render_to_text(&mut state, 130, 40).join("\n");
            assert!(text.contains("분당 요청"), "{text}");
            assert!(text.contains("분당 출력 토큰"), "{text}");
            assert!(!text.contains("분당 추이"), "{text}");
            let notices = state
                .hit_areas
                .card_rects
                .iter()
                .filter(|(card, _)| matches!(card, CardId::TrendRequests | CardId::TrendTokens))
                .count();
            assert_eq!(notices, 2);
        }
    }

    #[test]
    fn help_overlay_lists_the_card_move_keys_and_the_order_reset() {
        let mut state = sample_state_with_snapshot();
        state.show_help = true;
        let text = render_to_text(&mut state, 100, 40).join("\n");
        assert!(text.contains("Shift+방향키"), "{text}");
        assert!(text.contains("포커스 카드 위치 이동"), "{text}");
        assert!(text.contains("제목 길게 누름+드래그"), "{text}");
        assert!(text.contains("카드 위치 이동 (Esc 취소)"), "{text}");
        assert!(text.contains("카드 크기·순서·열 폭 기본 배치"), "{text}");
        assert!(text.contains("스크롤바"), "{text}");

        // 낮은 창은 잘림 표시를 유지
        let text = render_to_text(&mut state, MIN_WIDTH, MIN_HEIGHT).join("\n");
        assert!(text.contains("창을 키우면 전체 표시"), "{text}");
    }

    #[test]
    fn every_tab_renders_at_minimum_size_without_panic() {
        for tab in ActiveTab::ALL {
            let mut state = sample_state_with_tables();
            state.tab = tab;
            render_to_text(&mut state, MIN_WIDTH, MIN_HEIGHT);
            render_to_text(&mut state, 200, 60);
        }
    }

    #[test]
    fn a_fresh_state_defaults_to_english_and_renders_english_chrome() {
        let mut state = AppState::new(
            sample_config(),
            chrono::NaiveDate::from_ymd_opt(2026, 11, 15).unwrap(),
        );
        assert_eq!(state.lang, Lang::En);
        let text = render_to_text(&mut state, 130, 40).join("\n");
        assert!(text.contains("Overview"), "{text}");
        assert!(text.contains("[q] Quit"), "{text}");
        assert!(text.contains("[?] Help"), "{text}");
        assert!(text.contains("[L] EN"), "{text}");
        assert!(text.contains("Waiting for first poll"), "{text}");
        assert!(!text.contains("개요"), "{text}");
    }

    #[test]
    fn english_overview_and_footer_use_the_english_catalog() {
        let mut state = sample_state_with_snapshot();
        state.lang = Lang::En;
        let text = render_to_text(&mut state, 130, 40).join("\n");
        for expected in [
            "1 Overview",
            "serve running",
            "Gateway ON",
            "Cache hit rate",
            "Net effect",
            "Live events (4)",
            "Event detail",
            "[i] Reindex",
            "[w] Window Today",
            "[r] Refresh",
            "[t] Pin OFF",
            "[l] Reset layout",
            "[L] EN",
            "[?] Help",
            "[q] Quit",
            "Tab Next tab",
            "? All keys",
        ] {
            assert!(text.contains(expected), "{expected}\n{text}");
        }
        // 데이터 값은 번역하지 않음
        assert!(text.contains("grep -> rg"));
        assert!(text.contains("cliguard"));
    }

    #[test]
    fn every_tab_renders_in_both_languages_with_help_and_quit_chips() {
        for lang in [Lang::En, Lang::Ko] {
            let (help, quit) = match lang {
                Lang::En => ("[?] Help", "[q] Quit"),
                Lang::Ko => ("[?] 도움말", "[q] 종료"),
            };
            for tab in ActiveTab::ALL {
                for (width, height) in [(MIN_WIDTH, MIN_HEIGHT), (100, 30), (130, 40)] {
                    let mut state = sample_state_with_tables();
                    state.lang = lang;
                    state.tab = tab;
                    let rows = render_to_text(&mut state, width, height);
                    let footer = &rows[usize::from(height) - 2];
                    let case = format!("{lang:?} {tab:?} {width}x{height}: {footer}");
                    assert!(footer.contains(help), "{case}");
                    assert!(footer.contains(quit), "{case}");
                    assert_eq!(state.hit_areas.tab_rects.len(), 9, "{case}");
                    let kinds: Vec<UiAction> = state
                        .hit_areas
                        .button_rects
                        .iter()
                        .map(|(action, _)| *action)
                        .collect();
                    assert!(kinds.contains(&UiAction::ShowHelp), "{case}");
                    assert!(kinds.contains(&UiAction::Quit), "{case}");
                }
            }
        }
    }

    #[test]
    fn language_chip_is_the_last_left_chip_to_drop_and_survives_the_minimum_size() {
        // 최소 크기: 다른 왼쪽 칩이 뒤쪽부터 먼저 빠지고 언어 칩·도움말·종료는 남음 (두 언어 모두)
        for (lang, language, help, quit, reindex) in [
            (Lang::En, "[L] EN", "[?] Help", "[q] Quit", "[i] Reindex"),
            (Lang::Ko, "[L] KO", "[?] 도움말", "[q] 종료", "[i] 재색인"),
        ] {
            let mut state = sample_state_with_snapshot();
            state.lang = lang;
            let rows = render_to_text(&mut state, MIN_WIDTH, MIN_HEIGHT);
            let footer = &rows[usize::from(MIN_HEIGHT) - 2];
            for expected in [language, help, quit, reindex] {
                assert!(footer.contains(expected), "{lang:?} {expected}: {footer}");
            }
            assert!(!footer.contains("[l]"), "{lang:?}: {footer}");
            let actions: Vec<UiAction> = state
                .hit_areas
                .button_rects
                .iter()
                .map(|(action, _)| *action)
                .collect();
            for kept in [
                UiAction::ToggleLanguage,
                UiAction::ShowHelp,
                UiAction::Quit,
                UiAction::Reindex,
            ] {
                assert!(actions.contains(&kept), "{lang:?} {kept:?}");
            }
            assert!(!actions.contains(&UiAction::ResetLayout), "{lang:?}");

            // 언어 칩 클릭 영역은 줄 안에 겹침 없이 놓이고, 도움말보다 앞
            let rect_of = |wanted: UiAction| {
                state
                    .hit_areas
                    .button_rects
                    .iter()
                    .find(|(action, _)| *action == wanted)
                    .map(|(_, rect)| *rect)
                    .unwrap()
            };
            let chip = rect_of(UiAction::ToggleLanguage);
            assert_eq!(chip.y, MIN_HEIGHT - 2, "{lang:?}");
            assert!(chip.width > 0 && chip.right() < rect_of(UiAction::ShowHelp).x);
            for (action, rect) in &state.hit_areas.button_rects {
                if *action != UiAction::ToggleLanguage {
                    assert!(!rect.intersects(chip), "{lang:?} {action:?}");
                }
            }
        }

        // 넓으면 언어 칩이 왼쪽 그룹 마지막, 도움말보다 앞 (자리는 예전과 같음)
        let mut wide = sample_state_with_snapshot();
        wide.lang = Lang::En;
        render_to_text(&mut wide, 130, 40);
        let rect_of = |action: UiAction| {
            wide.hit_areas
                .button_rects
                .iter()
                .find(|(candidate, _)| *candidate == action)
                .map(|(_, rect)| *rect)
                .unwrap()
        };
        let language = rect_of(UiAction::ToggleLanguage);
        assert!(rect_of(UiAction::ResetLayout).right() < language.x);
        assert!(language.right() < rect_of(UiAction::ShowHelp).x);
    }

    #[test]
    fn switching_the_language_changes_the_rendered_text_immediately() {
        let mut state = sample_state_with_snapshot();
        state.lang = Lang::En;
        let english = render_to_text(&mut state, 130, 40).join("\n");
        assert!(english.contains("1 Overview") && english.contains("[L] EN"));

        state.perform_action(UiAction::ToggleLanguage);
        let korean = render_to_text(&mut state, 130, 40).join("\n");
        assert!(
            korean.contains("1 개요") && korean.contains("[L] KO"),
            "{korean}"
        );
        assert!(korean.contains("언어: 한국어"), "{korean}");
        assert!(!korean.contains("Overview"), "{korean}");

        state.perform_action(UiAction::ToggleLanguage);
        let back = render_to_text(&mut state, 130, 40).join("\n");
        assert!(
            back.contains("1 Overview") && back.contains("Language: English"),
            "{back}"
        );
    }

    #[test]
    fn english_hint_row_keeps_the_tail_hints_at_the_minimum_size() {
        let mut state = sample_state_with_snapshot();
        state.lang = Lang::En;
        let rows = render_to_text(&mut state, MIN_WIDTH, MIN_HEIGHT);
        let hints = &rows[usize::from(MIN_HEIGHT) - 1];
        assert!(hints.contains("Tab Next tab"), "{hints}");
        assert!(hints.contains("? All keys"), "{hints}");

        // 넓은 화면에서는 포커스 카드 종류 맥락 안내가 앞에 붙음
        let rows = render_to_text(&mut state, 130, 40);
        let hints = &rows[39];
        assert!(hints.contains("↑↓ Select row"), "{hints}");
        assert!(hints.contains("s Sort column"), "{hints}");
        assert!(hints.contains("Tab Next tab"), "{hints}");
    }

    #[test]
    fn english_help_overlay_lists_the_language_key_and_keeps_key_names() {
        let mut state = sample_state_with_snapshot();
        state.lang = Lang::En;
        state.show_help = true;
        let text = render_to_text(&mut state, 130, 40).join("\n");
        for expected in [
            "Keyboard",
            "Mouse",
            "1-9, Tab, Shift-Tab",
            "Switch language (EN / KO)",
            "Shift+Arrows",
            "q, Esc, Ctrl+C",
            "Any key or click: close",
        ] {
            assert!(text.contains(expected), "{expected}\n{text}");
        }
        state.lang = Lang::Ko;
        let text = render_to_text(&mut state, 130, 40).join("\n");
        assert!(text.contains("표시 언어 전환 (EN / KO)"), "{text}");
        assert!(text.contains("q, Esc, Ctrl+C"), "{text}");
    }

    #[test]
    fn english_empty_states_toasts_and_size_gate_use_the_catalog() {
        let mut state = sample_state_with_snapshot();
        state.lang = Lang::En;
        state.snapshot.events = Panel::Ready(Vec::new());
        state.snapshot.minutes = Panel::Pending;
        state.snapshot.overview = Panel::Failed(Failure::Error("query failed".to_string()));
        let text = render_to_text(&mut state, 120, 36).join("\n");
        assert!(text.contains("No events"), "{text}");
        assert!(text.contains("Waiting for first poll"), "{text}");
        assert!(text.contains("query failed"), "{text}");
        assert!(text.contains("Press r to retry"), "{text}");

        state.snapshot.db_state = DbState::Missing;
        let text = render_to_text(&mut state, 100, 30).join("\n");
        assert!(text.contains("No database"), "{text}");
        assert!(text.contains("Run serve or index first"), "{text}");

        let rows = render_to_text(&mut state, 50, 10);
        assert!(
            rows.iter()
                .any(|row| row.contains("Terminal too small (min 60x16, now 50x10)")),
            "{rows:?}"
        );
    }
}
