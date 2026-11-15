//! tui/views/tables.rs
//! 도구·훅 탭: 선택·정렬·스크롤 표 카드와 선택 행 상세 카드.

use crate::store::query::{HookStatRow, ToolLatencyRow};
use crate::tui::cards::CardId;
use crate::tui::format::{fit_display, format_count, format_duration_ms, short_tool};
use crate::tui::layout::{SplitAxis, SplitGroup};
use crate::tui::sort::{compare_hook_row, compare_tool_row, sort_rows};
use crate::tui::state::{AppState, Panel};
use crate::tui::theme;
use crate::tui::views::common::{
    CardTable, ColumnFit, ColumnSpec, TableCell, TextCard, panel_message, render_card_table,
    render_db_gate, render_notice_card, render_text_card,
};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};

// 도구 표 열 (정렬 열 번호와 순서 일치: sort.rs)
const TOOL_COLUMNS: [ColumnSpec; 6] = [
    ("도구", 16, ColumnFit::Grow, Alignment::Left),
    ("호출", 7, ColumnFit::Fixed, Alignment::Right),
    ("오류", 6, ColumnFit::Fixed, Alignment::Right),
    ("p50", 7, ColumnFit::Fixed, Alignment::Right),
    ("p90", 7, ColumnFit::Fixed, Alignment::Right),
    ("합계", 8, ColumnFit::Fixed, Alignment::Right),
];

// 훅 표 열
const HOOK_COLUMNS: [ColumnSpec; 8] = [
    ("경로", 12, ColumnFit::Shrink, Alignment::Left),
    ("가드", 16, ColumnFit::Shrink, Alignment::Left),
    ("판정", 6, ColumnFit::Fixed, Alignment::Left),
    ("상세", 16, ColumnFit::Grow, Alignment::Left),
    ("횟수", 7, ColumnFit::Fixed, Alignment::Right),
    ("p50", 7, ColumnFit::Fixed, Alignment::Right),
    ("p90", 7, ColumnFit::Fixed, Alignment::Right),
    ("절약", 8, ColumnFit::Fixed, Alignment::Right),
];

// 1. 도구 탭 ------------------------------------------------------------------
pub fn render_tools_tab(frame: &mut Frame, area: Rect, state: &mut AppState) {
    if render_db_gate(frame, state, area) {
        return;
    }
    let tools = match &state.snapshot.tools {
        Panel::Ready(rows) => rows.clone(),
        other => {
            let (message, color) = panel_message(other).unwrap_or((String::new(), theme::FG_DIM));
            render_notice_card(
                frame,
                state,
                CardId::ToolTable,
                area,
                "도구 지연",
                &message,
                color,
            );
            return;
        }
    };

    // 1. 정렬 사본
    let mut sorted = tools;
    sort_rows(
        &mut sorted,
        state.card_sorts[CardId::ToolTable as usize],
        compare_tool_row,
    );

    // 2. 본문 행 (도구 이름 축약, 숫자 오른쪽 정렬, 없음은 흐리게)
    let rows: Vec<Vec<TableCell>> = sorted
        .iter()
        .map(|tool| {
            let error_style = if tool.errors > 0 {
                theme::STYLE_CRIT
            } else {
                theme::STYLE_DIM
            };
            let latency = |millis: Option<i64>| match millis {
                Some(ms) => vec![Span::styled(format_duration_ms(ms), theme::STYLE_TEXT)],
                None => vec![Span::styled("-", theme::STYLE_DIM)],
            };
            vec![
                vec![Span::raw(short_tool(&tool.tool_name))],
                vec![Span::raw(format_count(
                    i64::try_from(tool.calls).unwrap_or(i64::MAX),
                ))],
                vec![Span::styled(
                    format_count(i64::try_from(tool.errors).unwrap_or(i64::MAX)),
                    error_style,
                )],
                latency(tool.p50_ms),
                latency(tool.p90_ms),
                vec![Span::raw(format_duration_ms(tool.total_ms))],
            ]
        })
        .collect();

    // 3. 표와 상세 카드 분할 (경계 드래그, 상세 기본 7행)
    let detail_height = 7.min(area.height.saturating_sub(5));
    let panes = state.card_layout.split_cards(
        &mut state.hit_areas.split_hits,
        SplitGroup::ToolsRows,
        SplitAxis::Rows,
        area,
        &[area.height.saturating_sub(detail_height)],
        &[5, 3],
    );

    // 4. 표 그리기
    render_card_table(
        frame,
        state,
        panes[0],
        CardTable {
            card: CardId::ToolTable,
            title: format!("도구 지연 · {}종", sorted.len()),
            hint: String::new(),
            columns: TOOL_COLUMNS.to_vec(),
            rows,
        },
    );

    // 5. 선택 도구 상세
    let selected = state.card_views[CardId::ToolTable as usize].selected;
    let (detail_title, detail_lines) = build_tool_detail(sorted.get(selected), &sorted);
    render_text_card(
        frame,
        state,
        panes[1],
        TextCard {
            card: CardId::ToolDetail,
            title: detail_title,
            hint: String::new(),
            header: None,
            header_cells: Vec::new(),
            lines: detail_lines,
        },
    );
}

// 2. 도구 상세 ----------------------------------------------------------------
// 제목과 항목 줄 (전체 합계 대비 점유율과 호출당 평균 포함)
fn build_tool_detail(
    tool: Option<&ToolLatencyRow>,
    all_tools: &[ToolLatencyRow],
) -> (String, Vec<Line<'static>>) {
    let Some(tool) = tool else {
        return (
            "도구 상세".to_string(),
            vec![Line::from(Span::styled(
                "선택한 도구 없음",
                theme::STYLE_DIM,
            ))],
        );
    };
    let total_all: i64 = all_tools.iter().map(|row| row.total_ms).sum();
    let share = if total_all > 0 {
        tool.total_ms as f64 * 100.0 / total_all as f64
    } else {
        0.0
    };
    let error_rate = if tool.calls > 0 {
        tool.errors as f64 * 100.0 / tool.calls as f64
    } else {
        0.0
    };
    let average = if tool.calls > 0 {
        tool.total_ms / i64::try_from(tool.calls).unwrap_or(i64::MAX).max(1)
    } else {
        0
    };
    let pairs: [(&str, String); 5] = [
        (
            "호출 / 오류",
            format!(
                "{} / {}  (오류율 {error_rate:.1}%)",
                format_count(i64::try_from(tool.calls).unwrap_or(i64::MAX)),
                format_count(i64::try_from(tool.errors).unwrap_or(i64::MAX)),
            ),
        ),
        (
            "지연 p50 / p90",
            format!(
                "{} / {}",
                tool.p50_ms.map_or("-".to_string(), format_duration_ms),
                tool.p90_ms.map_or("-".to_string(), format_duration_ms),
            ),
        ),
        ("합계 시간", format_duration_ms(tool.total_ms)),
        ("호출당 평균", format_duration_ms(average)),
        ("전체 대비", format!("{share:.1}%")),
    ];
    let lines = pairs
        .into_iter()
        .map(|(name, value)| {
            Line::from(vec![
                Span::styled(fit_display(name, 16), theme::STYLE_DIM),
                Span::styled(value, theme::STYLE_TEXT),
            ])
        })
        .collect();
    (format!("도구 상세 · {}", tool.tool_name), lines)
}

// 3. 훅 탭 --------------------------------------------------------------------
pub fn render_hooks_tab(frame: &mut Frame, area: Rect, state: &mut AppState) {
    if render_db_gate(frame, state, area) {
        return;
    }
    let hooks = match &state.snapshot.hooks {
        Panel::Ready(rows) => rows.clone(),
        other => {
            let (message, color) = panel_message(other).unwrap_or((String::new(), theme::FG_DIM));
            render_notice_card(
                frame,
                state,
                CardId::HookTable,
                area,
                "훅 판정",
                &message,
                color,
            );
            return;
        }
    };

    // 1. 합계 (제목 표시)
    let mut denials = 0u64;
    let mut rewrites = 0u64;
    let mut saved_total = 0i64;
    for hook in &hooks {
        match hook.outcome.as_str() {
            "deny" => denials += hook.count,
            "rewrite" => rewrites += hook.count,
            _ => {}
        }
        saved_total += hook.saved_ms;
    }

    // 2. 정렬 사본
    let mut sorted = hooks;
    sort_rows(
        &mut sorted,
        state.card_sorts[CardId::HookTable as usize],
        compare_hook_row,
    );

    // 3. 본문 행 (판정 칸만 색, 숫자 오른쪽 정렬, 없음은 흐리게)
    let rows: Vec<Vec<TableCell>> = sorted
        .iter()
        .map(|hook| {
            let latency = |millis: Option<i64>| match millis {
                Some(ms) => vec![Span::styled(format_duration_ms(ms), theme::STYLE_TEXT)],
                None => vec![Span::styled("-", theme::STYLE_DIM)],
            };
            let saved = if hook.saved_ms > 0 {
                vec![Span::styled(
                    format_duration_ms(hook.saved_ms),
                    theme::STYLE_OK,
                )]
            } else {
                vec![Span::styled("-", theme::STYLE_DIM)]
            };
            vec![
                vec![Span::raw(hook.route.clone())],
                vec![Span::raw(hook.guard.clone())],
                vec![Span::styled(
                    theme::outcome_label(&hook.outcome),
                    theme::outcome_style(&hook.outcome),
                )],
                vec![Span::raw(hook.detail.clone())],
                vec![Span::raw(format_count(
                    i64::try_from(hook.count).unwrap_or(i64::MAX),
                ))],
                latency(hook.p50_ms),
                latency(hook.p90_ms),
                saved,
            ]
        })
        .collect();

    // 4. 표와 상세 카드 분할 (상세 기본 8행)
    let detail_height = 8.min(area.height.saturating_sub(5));
    let panes = state.card_layout.split_cards(
        &mut state.hit_areas.split_hits,
        SplitGroup::HooksRows,
        SplitAxis::Rows,
        area,
        &[area.height.saturating_sub(detail_height)],
        &[5, 3],
    );

    // 5. 표 그리기
    let title = format!(
        "훅 판정 · 거부 {} · 재작성 {} · 절약 {}",
        format_count(i64::try_from(denials).unwrap_or(i64::MAX)),
        format_count(i64::try_from(rewrites).unwrap_or(i64::MAX)),
        format_duration_ms(saved_total)
    );
    render_card_table(
        frame,
        state,
        panes[0],
        CardTable {
            card: CardId::HookTable,
            title,
            hint: String::new(),
            columns: HOOK_COLUMNS.to_vec(),
            rows,
        },
    );

    // 6. 선택 훅 상세
    let selected = state.card_views[CardId::HookTable as usize].selected;
    let (detail_title, detail_lines) = build_hook_detail(sorted.get(selected), &sorted);
    render_text_card(
        frame,
        state,
        panes[1],
        TextCard {
            card: CardId::HookDetail,
            title: detail_title,
            hint: String::new(),
            header: None,
            header_cells: Vec::new(),
            lines: detail_lines,
        },
    );
}

// 4. 훅 상세 ------------------------------------------------------------------
// 제목과 항목 줄 (같은 판정 전체 횟수 대비 비중 포함)
fn build_hook_detail(
    hook: Option<&HookStatRow>,
    all_hooks: &[HookStatRow],
) -> (String, Vec<Line<'static>>) {
    let Some(hook) = hook else {
        return (
            "훅 상세".to_string(),
            vec![Line::from(Span::styled("선택한 훅 없음", theme::STYLE_DIM))],
        );
    };
    let total_count: u64 = all_hooks.iter().map(|row| row.count).sum();
    let share = if total_count > 0 {
        hook.count as f64 * 100.0 / total_count as f64
    } else {
        0.0
    };
    let pairs: [(&str, String); 6] = [
        (
            "경로 / 가드",
            if hook.guard.is_empty() {
                hook.route.clone()
            } else {
                format!("{} / {}", hook.route, hook.guard)
            },
        ),
        ("판정", theme::outcome_label(&hook.outcome).to_string()),
        ("상세", hook.detail.clone()),
        (
            "횟수",
            format!(
                "{}  (전체 대비 {share:.1}%)",
                format_count(i64::try_from(hook.count).unwrap_or(i64::MAX))
            ),
        ),
        (
            "지연 p50 / p90",
            format!(
                "{} / {}",
                hook.p50_ms.map_or("-".to_string(), format_duration_ms),
                hook.p90_ms.map_or("-".to_string(), format_duration_ms),
            ),
        ),
        (
            "절약 시간",
            if hook.saved_ms > 0 {
                format_duration_ms(hook.saved_ms)
            } else {
                "-".to_string()
            },
        ),
    ];
    let lines = pairs
        .into_iter()
        .map(|(name, value)| {
            Line::from(vec![
                Span::styled(fit_display(name, 16), theme::STYLE_DIM),
                Span::styled(value, theme::STYLE_TEXT),
            ])
        })
        .collect();
    (format!("훅 상세 · {}", hook.guard), lines)
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
    fn tool_detail_reports_share_average_and_error_rate() {
        let tools = vec![
            ToolLatencyRow {
                tool_name: "Bash".to_string(),
                calls: 10,
                errors: 2,
                p50_ms: Some(50),
                p90_ms: None,
                total_ms: 3_000,
            },
            ToolLatencyRow {
                tool_name: "Read".to_string(),
                calls: 5,
                errors: 0,
                p50_ms: None,
                p90_ms: None,
                total_ms: 1_000,
            },
        ];
        let (title, lines) = build_tool_detail(tools.first(), &tools);
        assert_eq!(title, "도구 상세 · Bash");
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text.iter().any(|row| row.contains("오류율 20.0%")));
        assert!(text.iter().any(|row| row.contains("75.0%")));
        assert!(text.iter().any(|row| row.contains("300ms")));

        let (empty_title, empty_lines) = build_tool_detail(None, &tools);
        assert_eq!(empty_title, "도구 상세");
        assert_eq!(empty_lines.len(), 1);
    }

    #[test]
    fn hook_detail_reports_share_of_total_count() {
        let hooks = vec![
            HookStatRow {
                route: "pretool".to_string(),
                guard: "cliguard".to_string(),
                outcome: "rewrite".to_string(),
                detail: "grep -> rg".to_string(),
                count: 3,
                p50_ms: Some(2),
                p90_ms: Some(5),
                saved_ms: 900,
            },
            HookStatRow {
                route: "pretool".to_string(),
                guard: "remoteguard".to_string(),
                outcome: "deny".to_string(),
                detail: "git push".to_string(),
                count: 1,
                p50_ms: None,
                p90_ms: None,
                saved_ms: 0,
            },
        ];
        let (title, lines) = build_hook_detail(hooks.get(1), &hooks);
        assert_eq!(title, "훅 상세 · remoteguard");
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text.iter().any(|row| row.contains("전체 대비 25.0%")));
        assert!(text.iter().any(|row| row.contains("git push")));
    }
}
