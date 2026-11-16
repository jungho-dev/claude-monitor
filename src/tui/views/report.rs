//! tui/views/report.rs
//! 리포트 탭: 날짜 이동 버튼 줄, 목차 카드, 일일 리포트 Markdown 본문 카드.

use crate::tui::cards::{CardId, UiAction};
use crate::tui::format::{outline_entries, prepare_report_markdown};
use crate::tui::i18n::{self, Msg};
use crate::tui::layout::{SplitAxis, SplitGroup};
use crate::tui::state::{AppState, Panel};
use crate::tui::theme;
use crate::tui::views::common::{
    PanelNote, TextCard, panel_message, render_notice_card, render_text_card,
};
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
    let outline_width = ((u32::from(body_area.width) * 28 / 100) as u16).clamp(16, 36);
    let panes = state.card_layout.split_cards(
        &mut state.hit_areas,
        SplitGroup::ReportCols,
        SplitAxis::Columns,
        body_area,
        &[outline_width],
        &[16, 20],
    );

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

    // 3. 목차 카드 (제목이 없으면 안내 카드)
    if entries.is_empty() {
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
            panes[0],
            i18n::text(lang, Msg::TitleOutline),
            note,
        );
    } else {
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
            panes[0],
            TextCard {
                card: CardId::ReportOutline,
                title: i18n::text(lang, Msg::TitleOutline).to_string(),
                hint: String::new(),
                lines: outline_lines,
            },
        );
    }

    // 4. 본문 카드
    let body_title =
        i18n::daily_report_title(lang, &state.report_date.format("%Y-%m-%d").to_string());
    state.report_row_starts = render_text_card(
        frame,
        state,
        panes[1],
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
    let next_disabled = state.report_date >= state.today;
    let (next_name, next_key) = if next_disabled {
        (chip.fg(theme::FG_DIM), chip.fg(theme::FG_DIM))
    } else {
        (name_style, key_style)
    };
    let lang = state.lang;
    let chip_name = |msg: Msg| format!(" {} ", i18n::text(lang, msg));
    let day_note = if state.report_date == state.today {
        format!(" ({})", i18n::text(lang, Msg::DayToday))
    } else if Some(state.report_date) == state.today.pred_opt() {
        format!(" ({})", i18n::text(lang, Msg::DayYesterday))
    } else {
        String::new()
    };

    // 2. 칩 목록 (날짜는 칩 없이 굵은 큰 글자)
    let items: Vec<(Option<UiAction>, Line<'static>)> = vec![
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
                Span::styled(
                    format!(" {}", state.report_date.format("%Y-%m-%d")),
                    theme::STYLE_STRONG,
                ),
                Span::styled(format!("{day_note} "), theme::STYLE_DIM),
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
    ];

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
}
