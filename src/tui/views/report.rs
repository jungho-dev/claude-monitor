//! tui/views/report.rs
//! 리포트 탭: 날짜·주 이동 버튼 줄, 목차 카드, 일일·주간 리포트 Markdown 본문 카드.

use crate::report::{format_week_key, week_start_of};
use crate::tui::cards::{CardId, UiAction};
use crate::tui::format::{outline_entries, prepare_report_markdown};
use crate::tui::i18n::{self, Msg};
use crate::tui::layout::grid::{CardSlot, SlotHeight};
use crate::tui::state::{ActiveTab, AppState, Panel};
use crate::tui::theme;
use crate::tui::views::common::{
    PanelNote, TextCard, panel_message, render_notice_card, render_text_card,
};
use chrono::Duration as ChronoDuration;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

// 1. 리포트 탭 ----------------------------------------------------------------
pub fn render_report_tab(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let bar_area = Rect::new(area.x, area.y, area.width, area.height.min(1));
    let body_area = Rect::new(
        area.x,
        area.y + bar_area.height,
        area.width,
        area.height - bar_area.height,
    );
    render_date_bar(frame, state, bar_area);

    // 1. 목차 카드와 본문 카드 분할 (경계 드래그)
    // 기본 폭은 weight 비례: 목차 o, 본문 W - 1 - o (사이 빈 칸 1열)
    let outline_width = ((u32::from(body_area.width) * 28 / 100) as u16).clamp(16, 36);
    let body_weight = body_area
        .width
        .saturating_sub(1)
        .saturating_sub(outline_width);
    let slots = [
        CardSlot {
            card: CardId::ReportOutline,
            min_w: 16,
            min_h: 3,
            height: SlotHeight::Fill,
            weight: outline_width,
            rank: 0,
            reserve: 0,
        },
        CardSlot {
            card: CardId::ReportBody,
            min_w: 20,
            min_h: 3,
            height: SlotHeight::Fill,
            weight: body_weight,
            rank: 0,
            reserve: 0,
        },
    ];
    let layout = state.card_layout;
    let panes = layout.arrange_tab(&mut state.hit_areas, ActiveTab::Report, body_area, &slots);
    let pane_of = |card: CardId| {
        panes
            .iter()
            .find(|(placed, _)| *placed == card)
            .map(|(_, rect)| *rect)
    };
    let outline_pane = pane_of(CardId::ReportOutline);
    let body_pane = pane_of(CardId::ReportBody);

    // 2. 본문 줄과 목차 (파이프 표는 박스 표로 바꾸고 울타리 줄은 숨긴 본문 기준)
    let lang = state.lang;
    let markdown = match &state.report {
        Panel::Ready(Some(text)) => Some(prepare_report_markdown(text)),
        _ => None,
    };
    let body_lines: Vec<Line<'static>> = match &state.report {
        Panel::Ready(Some(_)) => style_report_lines(markdown.as_deref().unwrap_or("")),
        Panel::Ready(None) => vec![
            Line::from(Span::styled(
                i18n::text(lang, Msg::NoSavedReport),
                theme::STYLE_WARN,
            )),
            Line::from(Span::styled(
                i18n::text(lang, Msg::ReportGenerateHint),
                theme::STYLE_ACCENT,
            )),
        ],
        other => {
            let note = panel_message(lang, other).unwrap_or_default();
            vec![Line::from(Span::styled(
                note.message,
                Style::default().fg(note.color),
            ))]
        }
    };
    let entries = markdown.as_deref().map(outline_entries).unwrap_or_default();

    // 3. 목차 카드 (제목이 없으면 안내 카드, 격자에 보일 때만)
    if let Some(outline_rect) = outline_pane
        && entries.is_empty()
    {
        let outline_view = &mut state.card_views[CardId::ReportOutline as usize];
        outline_view.total = 0;
        outline_view.visible = 0;
        let note = PanelNote {
            message: i18n::text(lang, Msg::NoHeadings).to_string(),
            color: theme::FG_DIM,
            hint: None,
        };
        render_notice_card(
            frame,
            state,
            CardId::ReportOutline,
            outline_rect,
            i18n::text(lang, Msg::TitleOutline),
            note,
        );
    } else if let Some(outline_rect) = outline_pane {
        let outline_lines: Vec<Line<'static>> = entries
            .iter()
            .map(|entry| {
                let (indent, style) = match entry.level {
                    1 => ("", theme::STYLE_STRONG),
                    2 => ("  ", theme::STYLE_TEXT),
                    _ => ("    ", theme::STYLE_DIM),
                };
                Line::from(Span::styled(format!("{indent}{}", entry.title), style))
            })
            .collect();
        render_text_card(
            frame,
            state,
            outline_rect,
            TextCard {
                card: CardId::ReportOutline,
                title: i18n::text(lang, Msg::TitleOutline).to_string(),
                hint: String::new(),
                lines: outline_lines,
            },
        );
    }

    // 4. 본문 카드 (격자에 없으면 줄 시작 위치 비움)
    let Some(body_rect) = body_pane else {
        state.report_row_starts.clear();
        return;
    };
    let body_title = if state.report_weekly {
        i18n::weekly_report_title(lang, &format_week_key(state.report_week))
    } else {
        i18n::daily_report_title(lang, &state.report_date.format("%Y-%m-%d").to_string())
    };
    state.report_row_starts = render_text_card(
        frame,
        state,
        body_rect,
        TextCard {
            card: CardId::ReportBody,
            title: body_title,
            hint: String::new(),
            lines: body_lines,
        },
    );
}

// 2. 날짜 이동 버튼 줄 --------------------------------------------------------
// ← 이전, 날짜(오늘·어제 표시), 다음 →, 생성 버튼의 클릭 영역 등록
fn render_date_bar(frame: &mut Frame, state: &mut AppState, area: Rect) {
    if area.is_empty() {
        return;
    }
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::BG_BAR)),
        area,
    );

    // 1. 칩 스타일 (하단 버튼과 동일: 키·화살표는 강조색, 이름은 본문색)
    let chip = Style::default().bg(theme::BG_TONAL);
    let key_style = chip.fg(theme::FG_ACCENT).add_modifier(Modifier::BOLD);
    let name_style = chip.fg(theme::FG_TEXT);
    let lang = state.lang;
    let weekly = state.report_weekly;
    let this_week = week_start_of(state.today);

    // 2. 보기별 날짜 문구와 다음 이동 가능 여부 (주간은 주 키와 월~일 범위, 일간은 날짜)
    let (next_disabled, date_text, range_text, day_note) = if weekly {
        let week_end = state.report_week + ChronoDuration::days(6);
        let note = if state.report_week == this_week {
            Some(Msg::WeekThis)
        } else if state.report_week == this_week - ChronoDuration::days(7) {
            Some(Msg::WeekLast)
        } else {
            None
        };
        (
            state.report_week >= this_week,
            format_week_key(state.report_week),
            format!(
                " {} ~ {}",
                state.report_week.format("%m-%d"),
                week_end.format("%m-%d")
            ),
            note,
        )
    } else {
        let note = if state.report_date == state.today {
            Some(Msg::DayToday)
        } else if Some(state.report_date) == state.today.pred_opt() {
            Some(Msg::DayYesterday)
        } else {
            None
        };
        (
            state.report_date >= state.today,
            state.report_date.format("%Y-%m-%d").to_string(),
            String::new(),
            note,
        )
    };
    let (next_name, next_key) = if next_disabled {
        (chip.fg(theme::FG_DIM), chip.fg(theme::FG_DIM))
    } else {
        (name_style, key_style)
    };
    let chip_name = |msg: Msg| format!(" {} ", i18n::text(lang, msg));

    // 3. 칩 목록 (날짜는 칩 없이 굵은 큰 글자, 보기 전환 칩은 뒤에 두어 좌폭에서 먼저 생략)
    // 날짜·이전·다음·생성 칩이 폭에 안 들어가면 오늘·어제·이번 주 같은 주석을 먼저 덮
    let build_items = |with_note: bool| -> Vec<(Option<UiAction>, Line<'static>)> {
        let note = match day_note {
            Some(msg) if with_note => format!(" ({})", i18n::text(lang, msg)),
            _ => String::new(),
        };
        vec![
            (
                Some(UiAction::ReportPrev),
                Line::from(vec![
                    Span::styled(" ←", key_style),
                    Span::styled(chip_name(Msg::ReportPrev), name_style),
                ]),
            ),
            (
                None,
                Line::from(vec![
                    Span::styled(format!(" {date_text}"), theme::STYLE_STRONG),
                    Span::styled(format!("{range_text}{note} "), theme::STYLE_DIM),
                ]),
            ),
            (
                Some(UiAction::ReportNext),
                Line::from(vec![
                    Span::styled(format!(" {}", i18n::text(lang, Msg::ReportNext)), next_name),
                    Span::styled(" → ", next_key),
                ]),
            ),
            (
                Some(UiAction::ReportGenerate),
                Line::from(vec![
                    Span::styled(" [g]", key_style),
                    Span::styled(chip_name(Msg::ReportGenerate), name_style),
                ]),
            ),
        ]
    };
    let mut items = build_items(true);
    let core_width: usize = items
        .iter()
        .map(|(_, line)| line.width() + 1)
        .sum::<usize>()
        + 1;
    if core_width > area.width as usize {
        items = build_items(false);
    }
    let toggle_name = if weekly {
        Msg::ReportViewDaily
    } else {
        Msg::ReportViewWeekly
    };
    items.push((
        Some(UiAction::ReportToggleView),
        Line::from(vec![
            Span::styled(" [v]", key_style),
            Span::styled(chip_name(toggle_name), name_style),
        ]),
    ));

    // 3. 왼쪽부터 배치 (남은 폭 초과 항목 생략)
    let row_end = area.x.saturating_add(area.width);
    let mut cursor = area.x.saturating_add(1);
    for (action, line) in items {
        let width = line.width() as u16;
        if cursor.saturating_add(width) > row_end {
            break;
        }
        let rect = Rect::new(cursor, area.y, width, area.height);
        let background = if action.is_some() {
            theme::BG_TONAL
        } else {
            theme::BG_BAR
        };
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(background)),
            rect,
        );
        if let Some(action) = action {
            state.hit_areas.button_rects.push((action, rect));
        }
        cursor = cursor.saturating_add(width + 1);
    }
}

// 3. Markdown 줄 꾸밈 ---------------------------------------------------------
// 제목(#), 목록(-, *), 표(|), 코드 울타리, 인용(>) 만 구분
pub fn style_markdown_line(line: &str) -> Line<'static> {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];

    // 1. 제목
    if let Some(heading) = trimmed.strip_prefix("### ") {
        return Line::from(Span::styled(
            format!("{indent}· {heading}"),
            Style::default()
                .fg(theme::COLOR_HOOK)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(heading) = trimmed.strip_prefix("## ") {
        return Line::from(Span::styled(
            format!("{indent}{heading}"),
            theme::STYLE_ACCENT.add_modifier(Modifier::UNDERLINED),
        ));
    }
    if let Some(heading) = trimmed.strip_prefix("# ") {
        return Line::from(Span::styled(
            format!("{indent}{heading}"),
            Style::default()
                .fg(theme::FG_ON_ACCENT)
                .bg(theme::FG_ACCENT)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // 2. 목록
    if let Some(body) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        return Line::from(vec![
            Span::styled(format!("{indent}• "), theme::STYLE_OK),
            Span::styled(body.to_string(), theme::STYLE_TEXT),
        ]);
    }

    // 3. 표 (구분선은 흐리게)
    if trimmed.starts_with('|') {
        let is_separator = trimmed
            .chars()
            .all(|ch| matches!(ch, '|' | '-' | ':' | ' '));
        let style = if is_separator {
            theme::STYLE_DIM
        } else {
            theme::STYLE_TEXT
        };
        return Line::from(Span::styled(line.to_string(), style));
    }

    // 4. 코드 울타리와 인용
    if trimmed.starts_with("```") {
        return Line::from(Span::styled(line.to_string(), theme::STYLE_DIM));
    }
    if trimmed.starts_with('>') {
        return Line::from(Span::styled(
            line.to_string(),
            theme::STYLE_DIM.add_modifier(Modifier::ITALIC),
        ));
    }

    // 5. 본문
    Line::from(Span::styled(line.to_string(), theme::STYLE_TEXT))
}

// 4. 리포트 본문 줄 꾸밈 ------------------------------------------------------
// Markdown 줄 꾸밈에 박스 표(┌ ├ └ │)를 더함: 선은 뮤트색, 첫 │ 줄(머리글 행)은 강조, 표 줄은 그대로
pub fn style_report_lines(markdown: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut header_next = false;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        match trimmed.chars().next() {
            // 1. 표 위·가운데·아래 선
            Some(edge @ ('┌' | '├' | '└')) => {
                header_next = edge == '┌';
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    theme::STYLE_TABLE_LINE,
                )));
            }

            // 2. 표 행 (세로선은 뮤트색, 글자는 본문색, 머리글 행은 굵게)
            Some('│') => {
                let text_style = if header_next {
                    theme::STYLE_STRONG
                } else {
                    theme::STYLE_TEXT
                };
                header_next = false;
                let mut spans: Vec<Span<'static>> = Vec::new();
                let mut piece = String::new();
                for letter in line.chars() {
                    if letter != '│' {
                        piece.push(letter);
                        continue;
                    }
                    if !piece.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut piece), text_style));
                    }
                    spans.push(Span::styled("│", theme::STYLE_TABLE_LINE));
                }
                if !piece.is_empty() {
                    spans.push(Span::styled(piece, text_style));
                }
                lines.push(Line::from(spans));
            }

            // 3. 그 밖의 줄
            _ => {
                header_next = false;
                lines.push(style_markdown_line(line));
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::i18n::Lang;
    use crate::tui::layout::grid::TabGrid;
    use crate::tui::state::{ActiveTab, Panel, UiCommand};
    use crate::tui::test_support::sample_state_with_tables;
    use crate::tui::views::render_frame;
    use chrono::NaiveDate;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::Instant;

    // 화면 전체 줄 문자열 (넓은 글자 뒤 칸 건너뜀)
    fn draw_rows(state: &mut AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_frame(frame, state)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                let mut text = String::new();
                let mut skip = 0usize;
                for x in 0..width {
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    let symbol = buffer[(x, y)].symbol();
                    skip = Span::raw(symbol).width().saturating_sub(1);
                    text.push_str(symbol);
                }
                text
            })
            .collect()
    }

    fn report_state(lang: Lang) -> AppState {
        let mut state = sample_state_with_tables();
        state.lang = lang;
        state.tab = ActiveTab::Report;
        state.report = Panel::Ready(Some("# Daily\n\n## 1. Summary\n- ok".to_string()));
        state
    }

    fn click_at(state: &mut AppState, column: u16, row: u16) -> UiCommand {
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        state.handle_mouse_event_at(event, Instant::now())
    }

    fn button_center(state: &AppState, wanted: UiAction) -> Option<(u16, u16)> {
        state
            .hit_areas
            .button_rects
            .iter()
            .find(|(action, _)| *action == wanted)
            .map(|(_, rect)| (rect.x + rect.width / 2, rect.y))
    }

    fn card_rect(state: &AppState, wanted: CardId) -> Option<Rect> {
        state
            .hit_areas
            .card_rects
            .iter()
            .find(|(card, _)| *card == wanted)
            .map(|(_, rect)| *rect)
    }

    #[test]
    fn daily_view_opens_on_today_at_every_size_in_both_languages() {
        for lang in [Lang::En, Lang::Ko] {
            for (width, height) in [(60u16, 16u16), (100, 30), (130, 40)] {
                let mut state = report_state(lang);
                let case = format!("{lang:?} {width}x{height}");
                assert!(!state.report_weekly, "{case}");
                assert_eq!(state.report_date, state.today, "{case}");
                let rows = draw_rows(&mut state, width, height);
                let today_note = match lang {
                    Lang::En => "(today)",
                    Lang::Ko => "(오늘)",
                };
                assert!(
                    rows.iter()
                        .any(|row| row.contains("2026-11-15") && row.contains(today_note)),
                    "{case}"
                );
                assert!(!rows.iter().any(|row| row.contains("W46")), "{case}");
                assert!(
                    button_center(&state, UiAction::ReportGenerate).is_some(),
                    "{case}"
                );
            }
        }
    }

    #[test]
    fn weekly_view_shows_the_current_week_title_range_and_chips_at_every_size() {
        for lang in [Lang::En, Lang::Ko] {
            for (width, height) in [(60u16, 16u16), (100, 30), (130, 40)] {
                let mut state = report_state(lang);
                let case = format!("{lang:?} {width}x{height}");
                assert_eq!(
                    state.perform_action(UiAction::ReportToggleView),
                    UiCommand::LoadWeeklyReport(NaiveDate::from_ymd_opt(2026, 11, 9).unwrap())
                );
                state.report = Panel::Ready(Some("# Weekly\n\n## 1. Summary\n- ok".to_string()));
                let rows = draw_rows(&mut state, width, height);

                // 날짜 줄: 주 키와 월~일 범위, 본문 카드 제목
                assert!(
                    rows.iter()
                        .any(|row| row.contains("2026-W46") && row.contains("11-09 ~ 11-15")),
                    "{case}"
                );
                let title = match lang {
                    Lang::En => "Weekly report · 2026-W46",
                    Lang::Ko => "주간 리포트 · 2026-W46",
                };
                assert!(rows.iter().any(|row| row.contains(title)), "{case}");
                assert!(rows.iter().any(|row| row.contains("Summary")), "{case}");
                assert!(
                    button_center(&state, UiAction::ReportPrev).is_some(),
                    "{case}"
                );
                assert!(
                    button_center(&state, UiAction::ReportNext).is_some(),
                    "{case}"
                );

                // 보기 전환 칩은 폭이 될 때 보이고 (이번 주 주석은 넓을 때만), 최소 폭에서는 v 키가 대신함
                if width >= 100 {
                    assert!(
                        button_center(&state, UiAction::ReportToggleView).is_some(),
                        "{case}"
                    );
                    let (week_note, view_name) = match lang {
                        Lang::En => ("(this week)", "[v] Daily"),
                        Lang::Ko => ("(이번 주)", "[v] 일간"),
                    };
                    assert!(rows.iter().any(|row| row.contains(week_note)), "{case}");
                    assert!(rows.iter().any(|row| row.contains(view_name)), "{case}");
                }
            }
        }
    }

    #[test]
    fn weekly_chips_move_by_week_generate_the_selected_week_and_switch_back() {
        for lang in [Lang::En, Lang::Ko] {
            let mut state = report_state(lang);
            let this_week = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
            let last_week = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
            let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
            draw_rows(&mut state, 130, 40);
            let (x, y) = button_center(&state, UiAction::ReportToggleView).unwrap();
            assert_eq!(
                click_at(&mut state, x, y),
                UiCommand::LoadWeeklyReport(this_week)
            );

            // 이번 주에서 다음은 비활성, 이전은 지난 주 적재 요청 후 지난주 주석
            let rows = draw_rows(&mut state, 130, 40);
            let (x, y) = button_center(&state, UiAction::ReportNext).unwrap();
            assert_eq!(click_at(&mut state, x, y), UiCommand::None);
            assert!(rows.iter().any(|row| row.contains("2026-W46")));
            let (x, y) = button_center(&state, UiAction::ReportPrev).unwrap();
            assert_eq!(
                click_at(&mut state, x, y),
                UiCommand::LoadWeeklyReport(last_week)
            );
            let rows = draw_rows(&mut state, 130, 40);
            let last_note = match lang {
                Lang::En => "(last week)",
                Lang::Ko => "(지난주)",
            };
            assert!(
                rows.iter()
                    .any(|row| row.contains("2026-W45") && row.contains(last_note)),
                "{lang:?}"
            );

            // 생성 칩은 선택한 주, 보기 칩은 일간 복귀 (오늘 리포트)
            let (x, y) = button_center(&state, UiAction::ReportGenerate).unwrap();
            assert_eq!(
                click_at(&mut state, x, y),
                UiCommand::GenerateWeeklyReport(last_week)
            );
            let (x, y) = button_center(&state, UiAction::ReportToggleView).unwrap();
            assert_eq!(click_at(&mut state, x, y), UiCommand::LoadReport(today));
            assert!(!state.report_weekly);
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn box_tables_keep_lines_verbatim_with_muted_rules_and_strong_header_row() {
        let markdown = "## 표\n┌────┬────┐\n│ 모델 │ 요청 │\n├────┼────┤\n│ a │ 9 │\n└────┴────┘";
        let lines = style_report_lines(markdown);
        assert_eq!(lines.len(), 6);
        for (shown, expected) in lines.iter().zip(markdown.lines()).skip(1) {
            assert_eq!(line_text(shown), expected);
        }

        // 선 줄은 뮤트색 한 조각
        assert_eq!(lines[1].spans.len(), 1);
        assert_eq!(lines[1].spans[0].style, theme::STYLE_TABLE_LINE);
        assert_eq!(lines[3].spans[0].style, theme::STYLE_TABLE_LINE);
        assert_eq!(lines[5].spans[0].style, theme::STYLE_TABLE_LINE);

        // 머리글 행 글자는 굵게, 본문 행은 본문색, 세로선은 어디서나 뮤트색
        let header_text = lines[2]
            .spans
            .iter()
            .find(|span| span.content.contains("모델"));
        assert_eq!(header_text.unwrap().style, theme::STYLE_STRONG);
        let body_text = lines[4]
            .spans
            .iter()
            .find(|span| span.content.contains('a'));
        assert_eq!(body_text.unwrap().style, theme::STYLE_TEXT);
        for index in [2, 4] {
            for span in lines[index].spans.iter().filter(|span| span.content == "│") {
                assert_eq!(span.style, theme::STYLE_TABLE_LINE);
            }
        }
    }

    #[test]
    fn markdown_headings_bullets_and_tables_get_distinct_styles() {
        let h1 = style_markdown_line("# 일일 리포트");
        let h2 = style_markdown_line("## 요약");
        let h3 = style_markdown_line("### 세부");
        assert_eq!(h1.spans[0].content, "일일 리포트");
        assert_eq!(h2.spans[0].content, "요약");
        assert_eq!(h3.spans[0].content, "· 세부");
        assert_ne!(h1.spans[0].style, h2.spans[0].style);

        let bullet = style_markdown_line("  - 요청 12건");
        assert_eq!(bullet.spans[0].content, "  • ");
        assert_eq!(bullet.spans[1].content, "요청 12건");

        let separator = style_markdown_line("|---|:--:|");
        let row = style_markdown_line("| a | b |");
        assert_eq!(separator.spans[0].style, theme::STYLE_DIM);
        assert_eq!(row.spans[0].style, theme::STYLE_TEXT);

        let plain = style_markdown_line("본문 줄");
        assert_eq!(plain.spans[0].style, theme::STYLE_TEXT);
    }

    // 기본 격자: 목차와 본문이 한 줄에 지금의 폭으로, 사이 빈 칸 1열
    #[test]
    fn default_grid_puts_the_outline_beside_the_body_with_todays_widths_at_every_size() {
        let sizes: [((u16, u16), (u16, u16)); 3] = [
            ((60, 16), (16, 43)),
            ((100, 30), (28, 71)),
            ((130, 40), (36, 93)),
        ];
        for lang in [Lang::En, Lang::Ko] {
            for ((width, height), widths) in sizes {
                let case = format!("{lang:?} {width}x{height}");
                let mut state = report_state(lang);
                draw_rows(&mut state, width, height);
                let outline = card_rect(&state, CardId::ReportOutline).unwrap();
                let body = card_rect(&state, CardId::ReportBody).unwrap();
                assert_eq!((outline.width, body.width), widths, "{case}");
                assert_eq!((outline.y, outline.height), (body.y, body.height), "{case}");
                assert_eq!(body.x, outline.right() + 1, "{case}");
                assert_eq!((outline.x, body.right()), (0, width), "{case}");
            }
        }
    }

    // 따로 놓은 격자 줄: 목차 아래 본문, 둘 다 그림
    #[test]
    fn outline_and_body_in_separate_grid_rows_draw_stacked() {
        let mut state = report_state(Lang::En);
        let mut grid = TabGrid::default();
        grid.rows[0][0] = Some(CardId::ReportOutline);
        grid.rows[1][0] = Some(CardId::ReportBody);
        state.card_layout.set_grid(ActiveTab::Report, grid);
        let rows = draw_rows(&mut state, 130, 40);
        let outline = card_rect(&state, CardId::ReportOutline).unwrap();
        let body = card_rect(&state, CardId::ReportBody).unwrap();
        assert_eq!((outline.x, outline.width), (body.x, body.width));
        assert!(body.y >= outline.bottom(), "{outline:?} {body:?}");
        assert!(rows.iter().any(|row| row.contains("Summary")));
    }
}
