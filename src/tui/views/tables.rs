//! tui/views/tables.rs
//! 도구·훅 탭: 선택·정렬·스크롤 표 카드와 선택 행 상세 카드.

use crate::store::query::{HookStatRow, ToolLatencyRow};
use crate::tui::cards::CardId;
use crate::tui::format::{fit_display, format_count, format_duration_ms, short_tool};
use crate::tui::i18n::{self, Lang, Msg};
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
pub const TOOL_COLUMNS: [ColumnSpec; 6] = [
    (Msg::ColTool, 16, ColumnFit::Grow, Alignment::Left),
    (Msg::ColCalls, 7, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColErrors, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColP50, 7, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColP90, 7, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColTotal, 8, ColumnFit::Fixed, Alignment::Right),
];

// 훅 표 열
pub const HOOK_COLUMNS: [ColumnSpec; 8] = [
    (Msg::ColRoute, 12, ColumnFit::Shrink, Alignment::Left),
    (Msg::ColGuard, 16, ColumnFit::Shrink, Alignment::Left),
    (Msg::ColVerdict, 6, ColumnFit::Fixed, Alignment::Left),
    (Msg::ColDetail, 16, ColumnFit::Grow, Alignment::Left),
    (Msg::ColCount, 7, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColP50, 7, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColP90, 7, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColSaved, 8, ColumnFit::Fixed, Alignment::Right),
];

// 1. 도구 탭 ------------------------------------------------------------------
pub fn render_tools_tab(frame: &mut Frame, area: Rect, state: &mut AppState) {
    if render_db_gate(frame, state, area) {
        return;
    }
    let tools = match &state.snapshot.tools {
        Panel::Ready(rows) => rows.clone(),
        other => {
            let note = panel_message(state.lang, other).unwrap_or_default();
            let title = i18n::text(state.lang, Msg::TitleToolLatency);
            render_notice_card(frame, state, CardId::ToolTable, area, title, note);
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
        &mut state.hit_areas,
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
            title: i18n::tool_table_title(state.lang, sorted.len()),
            hint: String::new(),
            columns: TOOL_COLUMNS.to_vec(),
            rows,
            aux_lines: Vec::new(),
        },
    );

    // 5. 선택 도구 상세
    let selected = state.card_views[CardId::ToolTable as usize].selected;
    let (detail_title, detail_lines) = build_tool_detail(state.lang, sorted.get(selected), &sorted);
    render_text_card(
        frame,
        state,
        panes[1],
        TextCard {
            card: CardId::ToolDetail,
            title: detail_title,
            hint: String::new(),
            lines: detail_lines,
        },
    );
}

// 2. 도구 상세 ----------------------------------------------------------------
// 제목과 항목 줄 (전체 합계 대비 점유율과 호출당 평균 포함)
fn build_tool_detail(
    lang: Lang,
    tool: Option<&ToolLatencyRow>,
    all_tools: &[ToolLatencyRow],
) -> (String, Vec<Line<'static>>) {
    let Some(tool) = tool else {
        return (
            i18n::text(lang, Msg::TitleToolDetail).to_string(),
            vec![Line::from(Span::styled(
                i18n::text(lang, Msg::NoToolSelected),
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
            i18n::text(lang, Msg::ToolCallsErrors),
            i18n::tool_calls_value(
                lang,
                &format_count(i64::try_from(tool.calls).unwrap_or(i64::MAX)),
                &format_count(i64::try_from(tool.errors).unwrap_or(i64::MAX)),
                error_rate,
            ),
        ),
        (
            i18n::text(lang, Msg::LatencyP50P90),
            format!(
                "{} / {}",
                tool.p50_ms.map_or("-".to_string(), format_duration_ms),
                tool.p90_ms.map_or("-".to_string(), format_duration_ms),
            ),
        ),
        (
            i18n::text(lang, Msg::ToolTotalTime),
            format_duration_ms(tool.total_ms),
        ),
        (
            i18n::text(lang, Msg::ToolAvgPerCall),
            format_duration_ms(average),
        ),
        (i18n::text(lang, Msg::ToolShare), format!("{share:.1}%")),
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
    let base = i18n::text(lang, Msg::TitleToolDetail);
    let title = if tool.tool_name.is_empty() {
        base.to_string()
    } else {
        format!("{base} · {}", tool.tool_name)
    };
    (title, lines)
}

// 3. 훅 탭 --------------------------------------------------------------------
pub fn render_hooks_tab(frame: &mut Frame, area: Rect, state: &mut AppState) {
    if render_db_gate(frame, state, area) {
        return;
    }
    let hooks = match &state.snapshot.hooks {
        Panel::Ready(rows) => rows.clone(),
        other => {
            let note = panel_message(state.lang, other).unwrap_or_default();
            let title = i18n::text(state.lang, Msg::TitleHookVerdicts);
            render_notice_card(frame, state, CardId::HookTable, area, title, note);
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
                    i18n::outcome_label(state.lang, &hook.outcome),
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
        &mut state.hit_areas,
        SplitGroup::HooksRows,
        SplitAxis::Rows,
        area,
        &[area.height.saturating_sub(detail_height)],
        &[5, 3],
    );

    // 5. 표 그리기
    let title = i18n::hook_table_title(
        state.lang,
        &format_count(i64::try_from(denials).unwrap_or(i64::MAX)),
        &format_count(i64::try_from(rewrites).unwrap_or(i64::MAX)),
        &format_duration_ms(saved_total),
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
            aux_lines: Vec::new(),
        },
    );

    // 6. 선택 훅 상세
    let selected = state.card_views[CardId::HookTable as usize].selected;
    let (detail_title, detail_lines) = build_hook_detail(state.lang, sorted.get(selected), &sorted);
    render_text_card(
        frame,
        state,
        panes[1],
        TextCard {
            card: CardId::HookDetail,
            title: detail_title,
            hint: String::new(),
            lines: detail_lines,
        },
    );
}

// 4. 훅 상세 ------------------------------------------------------------------
// 제목과 항목 줄 (같은 판정 전체 횟수 대비 비중 포함)
fn build_hook_detail(
    lang: Lang,
    hook: Option<&HookStatRow>,
    all_hooks: &[HookStatRow],
) -> (String, Vec<Line<'static>>) {
    let Some(hook) = hook else {
        return (
            i18n::text(lang, Msg::TitleHookDetail).to_string(),
            vec![Line::from(Span::styled(
                i18n::text(lang, Msg::NoHookSelected),
                theme::STYLE_DIM,
            ))],
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
            i18n::text(lang, Msg::HookRouteGuard),
            if hook.guard.is_empty() {
                hook.route.clone()
            } else {
                format!("{} / {}", hook.route, hook.guard)
            },
        ),
        (
            i18n::text(lang, Msg::DetailVerdict),
            i18n::outcome_label(lang, &hook.outcome).to_string(),
        ),
        (i18n::text(lang, Msg::DetailDetail), hook.detail.clone()),
        (
            i18n::text(lang, Msg::ColCount),
            i18n::count_with_share(
                lang,
                &format_count(i64::try_from(hook.count).unwrap_or(i64::MAX)),
                share,
            ),
        ),
        (
            i18n::text(lang, Msg::LatencyP50P90),
            format!(
                "{} / {}",
                hook.p50_ms.map_or("-".to_string(), format_duration_ms),
                hook.p90_ms.map_or("-".to_string(), format_duration_ms),
            ),
        ),
        (
            i18n::text(lang, Msg::HookTimeSaved),
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
    // 제목 보조값 (가드, 없으면 경로)
    let subject = if hook.guard.is_empty() {
        hook.route.as_str()
    } else {
        hook.guard.as_str()
    };
    let base = i18n::text(lang, Msg::TitleHookDetail);
    let title = if subject.is_empty() {
        base.to_string()
    } else {
        format!("{base} · {subject}")
    };
    (title, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KO: Lang = Lang::Ko;

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
        let (title, lines) = build_tool_detail(KO, tools.first(), &tools);
        assert_eq!(title, "도구 상세 · Bash");
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text.iter().any(|row| row.contains("오류율 20.0%")));
        assert!(text.iter().any(|row| row.contains("75.0%")));
        assert!(text.iter().any(|row| row.contains("300ms")));

        let (empty_title, empty_lines) = build_tool_detail(KO, None, &tools);
        assert_eq!(empty_title, "도구 상세");
        assert_eq!(empty_lines.len(), 1);

        let nameless = vec![ToolLatencyRow::default()];
        let (title, _) = build_tool_detail(KO, nameless.first(), &nameless);
        assert_eq!(title, "도구 상세");
    }

    #[test]
    fn hook_detail_title_never_leaves_a_dangling_separator() {
        let hook = |route: &str, guard: &str| HookStatRow {
            route: route.to_string(),
            guard: guard.to_string(),
            outcome: "allow".to_string(),
            ..HookStatRow::default()
        };

        // 가드가 비면 경로를 보조값으로, 둘 다 비면 제목만
        let no_guard = vec![hook("pretool-chain", "")];
        let (title, _) = build_hook_detail(KO, no_guard.first(), &no_guard);
        assert_eq!(title, "훅 상세 · pretool-chain");
        let nothing = vec![hook("", "")];
        let (title, _) = build_hook_detail(KO, nothing.first(), &nothing);
        assert_eq!(title, "훅 상세");
        let guarded = vec![hook("pretool", "cli-guard")];
        let (title, _) = build_hook_detail(KO, guarded.first(), &guarded);
        assert_eq!(title, "훅 상세 · cli-guard");
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
        let (title, lines) = build_hook_detail(KO, hooks.get(1), &hooks);
        assert_eq!(title, "훅 상세 · remoteguard");
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text.iter().any(|row| row.contains("전체 대비 25.0%")));
        assert!(text.iter().any(|row| row.contains("git push")));
    }
}
