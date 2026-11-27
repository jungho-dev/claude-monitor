//! tui/views/tables.rs
//! 도구·훅 탭: 선택·정렬·스크롤 표 카드와 선택 행 상세 카드.

use crate::store::query::{HookStatRow, ToolLatencyRow};
use crate::tui::cards::CardId;
use crate::tui::format::{fit_display, format_count, format_duration_ms, short_tool};
use crate::tui::i18n::{self, Lang, Msg};
use crate::tui::layout::grid::{CardSlot, SlotHeight};
use crate::tui::sort::{compare_hook_row, compare_tool_row, sort_rows};
use crate::tui::state::{ActiveTab, AppState, Panel};
use crate::tui::theme;
use crate::tui::views::common::{
    CardTable, ColumnFit, ColumnSpec, TableCell, TextCard, panel_message, render_card_table,
    render_db_gate, render_notice_card, render_text_card,
};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};

// 도구 표 열 (정렬 열 번호와 순서 일치: sort.rs)
pub const TOOL_COLUMNS: [ColumnSpec; 8] = [
    (Msg::ColTool, 16, ColumnFit::Grow, Alignment::Left),
    (Msg::ColCalls, 7, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColErrors, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColAvg, 7, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColMax, 7, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColTotal, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColErrorRate, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColShare, 6, ColumnFit::Fixed, Alignment::Right),
];

// 훅 표 열
pub const HOOK_COLUMNS: [ColumnSpec; 9] = [
    (Msg::ColRoute, 12, ColumnFit::Shrink, Alignment::Left),
    (Msg::ColGuard, 16, ColumnFit::Shrink, Alignment::Left),
    (Msg::ColVerdict, 6, ColumnFit::Fixed, Alignment::Left),
    (Msg::ColDetail, 16, ColumnFit::Grow, Alignment::Left),
    (Msg::ColCount, 7, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColAvg, 7, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColTotal, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColSaved, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColShare, 6, ColumnFit::Fixed, Alignment::Right),
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

    // 2. 본문 행 (도구 이름 축약, 숫자 오른쪽 정렬, 없음은 흐리게, 비중은 전체 시간 합계 대비)
    let total_time_all: i64 = sorted.iter().map(|row| row.total_ms).sum();
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
            let error_rate = if tool.calls > 0 {
                vec![Span::styled(
                    format!("{:.1}%", tool.errors as f64 * 100.0 / tool.calls as f64),
                    error_style,
                )]
            } else {
                vec![Span::styled("-", theme::STYLE_DIM)]
            };
            let time_share = if total_time_all > 0 {
                vec![Span::styled(
                    format!(
                        "{:.1}%",
                        tool.total_ms as f64 * 100.0 / total_time_all as f64
                    ),
                    theme::STYLE_TEXT,
                )]
            } else {
                vec![Span::styled("-", theme::STYLE_DIM)]
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
                latency(tool.avg_ms),
                latency(tool.max_ms),
                vec![Span::raw(format_duration_ms(tool.total_ms))],
                error_rate,
                time_share,
            ]
        })
        .collect();

    // 3. 표와 상세 카드 배치 (탭 격자, 경계 드래그, 상세 기본 7행)
    let detail_height = 7.min(area.height.saturating_sub(5));
    let layout = state.card_layout;
    let placed = layout.arrange_tab(
        &mut state.hit_areas,
        ActiveTab::Tools,
        area,
        &[
            CardSlot {
                card: CardId::ToolTable,
                min_w: 24,
                min_h: 5,
                height: SlotHeight::Fill,
                weight: 3,
                rank: 0,
                reserve: 0,
            },
            CardSlot {
                card: CardId::ToolDetail,
                min_w: 20,
                min_h: 3,
                height: SlotHeight::Fixed(detail_height),
                weight: 2,
                rank: 0,
                reserve: 0,
            },
        ],
    );
    let table_area = placed
        .iter()
        .find(|(card, _)| *card == CardId::ToolTable)
        .map(|(_, rect)| *rect);
    let detail_area = placed
        .iter()
        .find(|(card, _)| *card == CardId::ToolDetail)
        .map(|(_, rect)| *rect);

    // 4. 표 그리기
    if let Some(table_area) = table_area {
        render_card_table(
            frame,
            state,
            table_area,
            CardTable {
                card: CardId::ToolTable,
                title: i18n::tool_table_title(state.lang, sorted.len()),
                hint: String::new(),
                columns: TOOL_COLUMNS.to_vec(),
                rows,
                aux_lines: Vec::new(),
            },
        );
    }

    // 5. 선택 도구 상세
    let Some(detail_area) = detail_area else {
        return;
    };
    let selected = state.card_views[CardId::ToolTable as usize].selected;
    let (detail_title, detail_lines) = build_tool_detail(state.lang, sorted.get(selected), &sorted);
    render_text_card(
        frame,
        state,
        detail_area,
        TextCard {
            card: CardId::ToolDetail,
            title: detail_title,
            hint: String::new(),
            lines: detail_lines,
        },
    );
}

// 2. 도구 상세 ----------------------------------------------------------------
// 제목과 항목 줄 (호출·오류·오류율, 평균·최대, 합계 시간, 시간 점유율, 호출 점유율)
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
    let time_share = if total_all > 0 {
        format!("{:.1}%", tool.total_ms as f64 * 100.0 / total_all as f64)
    } else {
        "-".to_string()
    };
    let calls_all: u64 = all_tools.iter().map(|row| row.calls).sum();
    let call_share = if calls_all > 0 {
        format!("{:.1}%", tool.calls as f64 * 100.0 / calls_all as f64)
    } else {
        "-".to_string()
    };
    let error_rate = if tool.calls > 0 {
        tool.errors as f64 * 100.0 / tool.calls as f64
    } else {
        0.0
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
            i18n::text(lang, Msg::DetailAvgMax),
            format!(
                "{} / {}",
                tool.avg_ms.map_or("-".to_string(), format_duration_ms),
                tool.max_ms.map_or("-".to_string(), format_duration_ms),
            ),
        ),
        (
            i18n::text(lang, Msg::DetailTotalTime),
            format_duration_ms(tool.total_ms),
        ),
        (i18n::text(lang, Msg::DetailTimeShare), time_share),
        (i18n::text(lang, Msg::ToolCallShare), call_share),
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

    // 3. 본문 행 (판정 칸만 색, 숫자 오른쪽 정렬, 없음은 흐리게, 비중은 전체 횟수 대비)
    let count_all: u64 = sorted.iter().map(|row| row.count).sum();
    let rows: Vec<Vec<TableCell>> = sorted
        .iter()
        .map(|hook| {
            let latency = |millis: Option<i64>| match millis {
                Some(ms) => vec![Span::styled(format_duration_ms(ms), theme::STYLE_TEXT)],
                None => vec![Span::styled("-", theme::STYLE_DIM)],
            };
            let hook_total = if hook.total_ms > 0 {
                vec![Span::styled(
                    format_duration_ms(hook.total_ms),
                    theme::STYLE_TEXT,
                )]
            } else {
                vec![Span::styled("-", theme::STYLE_DIM)]
            };
            let count_share = if count_all > 0 {
                vec![Span::styled(
                    format!("{:.1}%", hook.count as f64 * 100.0 / count_all as f64),
                    theme::STYLE_TEXT,
                )]
            } else {
                vec![Span::styled("-", theme::STYLE_DIM)]
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
                latency(hook.avg_ms),
                hook_total,
                saved,
                count_share,
            ]
        })
        .collect();

    // 4. 표와 상세 카드 배치 (탭 격자, 상세 기본 9행)
    let detail_height = 9.min(area.height.saturating_sub(5));
    let layout = state.card_layout;
    let placed = layout.arrange_tab(
        &mut state.hit_areas,
        ActiveTab::Hooks,
        area,
        &[
            CardSlot {
                card: CardId::HookTable,
                min_w: 24,
                min_h: 5,
                height: SlotHeight::Fill,
                weight: 3,
                rank: 0,
                reserve: 0,
            },
            CardSlot {
                card: CardId::HookDetail,
                min_w: 20,
                min_h: 3,
                height: SlotHeight::Fixed(detail_height),
                weight: 2,
                rank: 0,
                reserve: 0,
            },
        ],
    );
    let table_area = placed
        .iter()
        .find(|(card, _)| *card == CardId::HookTable)
        .map(|(_, rect)| *rect);
    let detail_area = placed
        .iter()
        .find(|(card, _)| *card == CardId::HookDetail)
        .map(|(_, rect)| *rect);

    // 5. 표 그리기
    if let Some(table_area) = table_area {
        let title = i18n::hook_table_title(
            state.lang,
            &format_count(i64::try_from(denials).unwrap_or(i64::MAX)),
            &format_count(i64::try_from(rewrites).unwrap_or(i64::MAX)),
            &format_duration_ms(saved_total),
        );
        render_card_table(
            frame,
            state,
            table_area,
            CardTable {
                card: CardId::HookTable,
                title,
                hint: String::new(),
                columns: HOOK_COLUMNS.to_vec(),
                rows,
                aux_lines: Vec::new(),
            },
        );
    }

    // 6. 선택 훅 상세
    let Some(detail_area) = detail_area else {
        return;
    };
    let selected = state.card_views[CardId::HookTable as usize].selected;
    let (detail_title, detail_lines) = build_hook_detail(state.lang, sorted.get(selected), &sorted);
    render_text_card(
        frame,
        state,
        detail_area,
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
    let pairs: [(&str, String); 7] = [
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
            i18n::text(lang, Msg::DetailAvgMax),
            format!(
                "{} / {}",
                hook.avg_ms.map_or("-".to_string(), format_duration_ms),
                hook.max_ms.map_or("-".to_string(), format_duration_ms),
            ),
        ),
        (
            i18n::text(lang, Msg::DetailTotalTime),
            if hook.total_ms > 0 {
                format_duration_ms(hook.total_ms)
            } else {
                "-".to_string()
            },
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
    use crate::tui::layout::grid::TabGrid;
    use crate::tui::test_support::sample_state_with_tables;
    use crate::tui::views::render_frame;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    const KO: Lang = Lang::Ko;
    const SIZES: [(u16, u16); 3] = [(60, 16), (100, 30), (130, 40)];
    const BOTH: [Lang; 2] = [Lang::En, Lang::Ko];

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn tab_state(lang: Lang, tab: ActiveTab) -> AppState {
        let mut state = sample_state_with_tables();
        state.lang = lang;
        state.tab = tab;
        state
    }

    fn draw(state: &mut AppState, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_frame(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    // 화면 줄 글자 (넓은 글자 뒤 칸은 건너뜀)
    fn screen_rows(buffer: &Buffer) -> Vec<String> {
        let mut rows = Vec::new();
        for y in 0..buffer.area.height {
            let mut text = String::new();
            let mut skip = 0usize;
            for x in 0..buffer.area.width {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let symbol = buffer.cell((x, y)).map_or(" ", |cell| cell.symbol());
                skip = Span::raw(symbol).width().saturating_sub(1);
                text.push_str(symbol);
            }
            rows.push(text);
        }
        rows
    }

    // 두 머리글을 함께 담은 첫 줄 (표 머리글 줄, 카드 제목 테두리 줄 제외)
    fn header_row(rows: &[String], lang: Lang, first: Msg, second: Msg) -> String {
        let first = i18n::text(lang, first);
        let second = i18n::text(lang, second);
        rows.iter()
            .filter(|row| !row.contains('┏') && !row.contains('╭'))
            .find(|row| row.contains(first) && row.contains(second))
            .cloned()
            .unwrap_or_else(|| panic!("header row with {first} and {second} missing"))
    }

    fn card_rect(state: &AppState, card: CardId) -> Rect {
        state
            .hit_areas
            .card_rects
            .iter()
            .find(|(id, _)| *id == card)
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("{card:?} not drawn"))
    }

    // 탭별 표·상세 카드와 크기별 기대 높이 (60x16, 100x30, 130x40)
    struct TabCards {
        tab: ActiveTab,
        table_card: CardId,
        detail_card: CardId,
        heights: [(u16, u16); 3],
    }

    fn tab_cards() -> [TabCards; 2] {
        [
            TabCards {
                tab: ActiveTab::Tools,
                table_card: CardId::ToolTable,
                detail_card: CardId::ToolDetail,
                heights: [(5, 7), (19, 7), (29, 7)],
            },
            TabCards {
                tab: ActiveTab::Hooks,
                table_card: CardId::HookTable,
                detail_card: CardId::HookDetail,
                heights: [(5, 7), (17, 9), (27, 9)],
            },
        ]
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
                avg_ms: Some(50),
                max_ms: None,
            },
            ToolLatencyRow {
                tool_name: "Read".to_string(),
                calls: 5,
                errors: 0,
                p50_ms: None,
                p90_ms: None,
                total_ms: 1_000,
                avg_ms: None,
                max_ms: None,
            },
        ];
        let (title, lines) = build_tool_detail(KO, tools.first(), &tools);
        assert_eq!(title, "도구 상세 · Bash");
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text.iter().any(|row| row.contains("오류율 20.0%")));
        assert!(text.iter().any(|row| row.contains("75.0%")));
        assert!(text.iter().any(|row| row.contains("66.7%")));
        assert!(text.iter().any(|row| row.contains("50ms / -")));
        assert!(!text.iter().any(|row| row.contains("p50")));

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
                avg_ms: Some(2),
                max_ms: Some(5),
                total_ms: 6,
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
                avg_ms: None,
                max_ms: None,
                total_ms: 0,
            },
        ];
        let (title, lines) = build_hook_detail(KO, hooks.get(1), &hooks);
        assert_eq!(title, "훅 상세 · remoteguard");
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text.iter().any(|row| row.contains("전체 대비 25.0%")));
        assert!(text.iter().any(|row| row.contains("git push")));
    }

    #[test]
    fn tool_and_hook_tabs_never_show_percentiles() {
        for lang in BOTH {
            for (width, height) in SIZES {
                for tab in [ActiveTab::Tools, ActiveTab::Hooks] {
                    let mut state = tab_state(lang, tab);
                    let screen = screen_rows(&draw(&mut state, width, height))
                        .join("\n")
                        .to_lowercase();
                    assert!(!screen.contains("p50"), "{lang:?} {width}x{height}");
                    assert!(!screen.contains("p90"), "{lang:?} {width}x{height}");
                }
            }
        }
    }

    #[test]
    fn tool_tab_shows_new_columns_row_and_detail() {
        for lang in BOTH {
            let mut state = tab_state(lang, ActiveTab::Tools);
            let rows = screen_rows(&draw(&mut state, 130, 40));

            // 1. 머리글 (평균, 최대, 합계, 오류율, 비중)
            let header = header_row(&rows, lang, Msg::ColCalls, Msg::ColErrors);
            for label in [
                Msg::ColTool,
                Msg::ColAvg,
                Msg::ColMax,
                Msg::ColTotal,
                Msg::ColErrorRate,
                Msg::ColShare,
            ] {
                assert!(
                    header.contains(i18n::text(lang, label)),
                    "{lang:?} {header}"
                );
            }

            // 2. Bash 행 전체 값 (오류율 9/420, 시간 비중 96s/121s)
            let error_rate = format!("{:.1}%", 9.0 * 100.0 / 420.0);
            let time_share = format!("{:.1}%", 96_000.0 * 100.0 / 121_000.0);
            let expected = [
                "Bash",
                "420",
                "85ms",
                "1.9s",
                "1m 36s",
                &error_rate,
                &time_share,
            ];
            assert!(
                rows.iter()
                    .any(|row| expected.iter().all(|value| row.contains(value))),
                "{lang:?} Bash row"
            );

            // 3. 상세 줄
            let screen = rows.join("\n");
            for label in [
                Msg::DetailAvgMax,
                Msg::DetailTotalTime,
                Msg::DetailTimeShare,
                Msg::ToolCallShare,
            ] {
                assert!(
                    screen.contains(i18n::text(lang, label)),
                    "{lang:?} {label:?}"
                );
            }
        }
    }

    #[test]
    fn hook_tab_shows_new_columns_row_and_detail() {
        for lang in BOTH {
            let mut state = tab_state(lang, ActiveTab::Hooks);
            let rows = screen_rows(&draw(&mut state, 130, 40));

            // 1. 머리글 (평균, 합계, 절약, 비중)
            let header = header_row(&rows, lang, Msg::ColVerdict, Msg::ColCount);
            for label in [Msg::ColAvg, Msg::ColTotal, Msg::ColSaved, Msg::ColShare] {
                assert!(
                    header.contains(i18n::text(lang, label)),
                    "{lang:?} {header}"
                );
            }

            // 2. cliguard 행 전체 값 (횟수 비중 45/48)
            let count_share = format!("{:.1}%", 45.0 * 100.0 / 48.0);
            let expected = ["cliguard", "45", "3ms", "135ms", "3m 09s", &count_share];
            assert!(
                rows.iter()
                    .any(|row| expected.iter().all(|value| row.contains(value))),
                "{lang:?} cliguard row"
            );

            // 3. 상세 줄
            let screen = rows.join("\n");
            for label in [Msg::DetailAvgMax, Msg::DetailTotalTime] {
                assert!(
                    screen.contains(i18n::text(lang, label)),
                    "{lang:?} {label:?}"
                );
            }
        }
    }

    // 기본 격자: 표 위, 상세 아래로 본문을 빈틈없이 나눔 (예전 두 카드 분할과 같은 높이)
    #[test]
    fn default_grid_keeps_table_over_detail_heights_at_the_size_matrix() {
        for TabCards {
            tab,
            table_card,
            detail_card,
            heights,
        } in tab_cards()
        {
            for lang in BOTH {
                for ((width, height), (table_h, detail_h)) in SIZES.into_iter().zip(heights) {
                    let case = format!("{tab:?} {lang:?} {width}x{height}");
                    let mut state = tab_state(lang, tab);
                    draw(&mut state, width, height);
                    let table = card_rect(&state, table_card);
                    let detail = card_rect(&state, detail_card);
                    assert_eq!((table.height, detail.height), (table_h, detail_h), "{case}");
                    assert_eq!(table.bottom(), detail.y, "{case}");
                    assert_eq!((table.x, table.width), (detail.x, detail.width), "{case}");
                }
            }
        }
    }

    // 두 카드를 한 줄에 두면 같은 y, 최소 크기에서도 둘 다 그림
    #[test]
    fn table_and_detail_share_one_row_when_the_grid_joins_them() {
        for TabCards {
            tab,
            table_card,
            detail_card,
            ..
        } in tab_cards()
        {
            for (width, height) in SIZES {
                let case = format!("{tab:?} {width}x{height}");
                let mut state = tab_state(Lang::En, tab);
                let mut grid = TabGrid::default();
                grid.rows[0][0] = Some(table_card);
                grid.rows[0][1] = Some(detail_card);
                state.card_layout.set_grid(tab, grid);
                draw(&mut state, width, height);
                let table = card_rect(&state, table_card);
                let detail = card_rect(&state, detail_card);
                assert!(!table.intersects(detail), "{case}");
                if width >= 100 {
                    assert_eq!(table.y, detail.y, "{case}");
                }
            }
        }
    }

    #[test]
    fn narrow_tables_hide_right_columns_first() {
        for lang in BOTH {
            let mut state = tab_state(lang, ActiveTab::Tools);
            let rows = screen_rows(&draw(&mut state, 60, 16));
            let header = header_row(&rows, lang, Msg::ColTool, Msg::ColCalls);
            assert!(
                !header.contains(i18n::text(lang, Msg::ColShare)),
                "{lang:?} {header}"
            );

            let mut state = tab_state(lang, ActiveTab::Hooks);
            let rows = screen_rows(&draw(&mut state, 60, 16));
            let header = header_row(&rows, lang, Msg::ColVerdict, Msg::ColCount);
            assert!(
                !header.contains(i18n::text(lang, Msg::ColShare)),
                "{lang:?} {header}"
            );
        }
    }
}
