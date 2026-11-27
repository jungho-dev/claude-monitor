//! tui/views/projects.rs
//! 프로젝트 탭: 프로젝트·브랜치별 사용량 표, 선택 행 상세 카드, 스킬·MCP 서버·플러그인·에이전트·모델별 비용 귀속 표.

use crate::store::metrics::{AttributionDimension, MetricAttributionRow};
use crate::store::query::ProjectStatsRow;
use crate::tui::cards::CardId;
use crate::tui::format::{format_clock_time, format_compact, format_count, format_duration_ms};
use crate::tui::i18n::{self, Lang, Msg};
use crate::tui::layout::grid::{CardSlot, SlotHeight};
use crate::tui::sort::{compare_attribution_row, compare_project_row, sort_rows};
use crate::tui::state::{ActiveTab, AppState, DashboardSnapshot, Panel};
use crate::tui::theme;
use crate::tui::views::common::{
    CardTable, ColumnFit, ColumnSpec, PanelNote, TableCell, TextCard, panel_message,
    render_card_table, render_db_gate, render_notice_card, render_text_card,
};
use crate::tui::views::detail::{
    ShareBasis, WindowTotals, first_last, pair_line, per_request, share_lines, window_share,
};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};

// 상세 카드 항목 줄 수와 기본 높이 (항목 11줄 + 모델 구역 제목과 한 줄 + 테두리 2줄, 나머지 분포는 스크롤)
const PROJECT_DETAIL_LINES: usize = 11;
const DETAIL_BORDER_ROWS: u16 = 2;
const PROJECT_DETAIL_HEIGHT: u16 = PROJECT_DETAIL_LINES as u16 + 2 + DETAIL_BORDER_ROWS;
// 세션별 비용 구역의 세션 id 앞부분 길이
const SESSION_ID_PREFIX: usize = 8;

// 프로젝트 표 열 (정렬 열 번호와 순서 일치: sort.rs)
pub const PROJECT_COLUMNS: [ColumnSpec; 13] = [
    (Msg::ColProject, 14, ColumnFit::Shrink, Alignment::Left),
    (Msg::ColBranch, 12, ColumnFit::Grow, Alignment::Left),
    (Msg::ColRequests, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColSessions, 5, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColInput, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColOutput, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColCacheRead, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColCacheWrite5m, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColCacheWrite1h, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColCostEst, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColLast, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColTotalWait, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColTokPerReq, 8, ColumnFit::Fixed, Alignment::Right),
];

// 귀속 표 열
pub const ATTRIBUTION_COLUMNS: [ColumnSpec; 6] = [
    (Msg::ColName, 16, ColumnFit::Grow, Alignment::Left),
    (Msg::ColCost, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColInput, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColOutput, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColCacheRead, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColCacheWrite, 8, ColumnFit::Fixed, Alignment::Right),
];

// 1. 귀속 행 조회 -------------------------------------------------------------
// 스냅샷의 한 차원 행 (귀속 패널이 준비되지 않았거나 차원이 없으면 None)
pub fn attribution_rows(
    snapshot: &DashboardSnapshot,
    dimension: AttributionDimension,
) -> Option<&[MetricAttributionRow]> {
    let Panel::Ready(sets) = &snapshot.attribution else {
        return None;
    };
    sets.iter()
        .find(|(candidate, _)| *candidate == dimension)
        .map(|(_, rows)| rows.as_slice())
}

// 2. 프로젝트 탭 --------------------------------------------------------------
// 프로젝트·브랜치 표, 선택 행 상세, 귀속 표 (구간은 다른 탭과 같은 w 키)
pub fn render_projects_tab(frame: &mut Frame, area: Rect, state: &mut AppState) {
    if render_db_gate(frame, state, area) {
        return;
    }
    let table_height = (u32::from(area.height) * 40 / 100) as u16;
    // 낮은 화면에서도 귀속 표가 머리글과 한 행(4줄)은 갖도록 상세 카드가 먼저 줄어듦 (표는 최소 4줄)
    let detail_height =
        PROJECT_DETAIL_HEIGHT.min(area.height.saturating_sub(table_height.max(4) + 4));
    let slots = [
        CardSlot {
            card: CardId::ProjectTable,
            min_w: 24,
            min_h: 4,
            height: SlotHeight::Fixed(table_height),
            weight: 3,
            rank: 0,
            reserve: 0,
        },
        CardSlot {
            card: CardId::ProjectDetail,
            min_w: 20,
            min_h: 4,
            height: SlotHeight::Fixed(detail_height),
            weight: 2,
            rank: 0,
            reserve: 0,
        },
        CardSlot {
            card: CardId::AttributionTable,
            min_w: 24,
            min_h: 4,
            height: SlotHeight::Fill,
            weight: 3,
            rank: 0,
            reserve: 0,
        },
    ];
    let layout = state.card_layout;
    let placed = layout.arrange_tab(&mut state.hit_areas, ActiveTab::Projects, area, &slots);
    let table_area = placed
        .iter()
        .find(|(card, _)| *card == CardId::ProjectTable)
        .map(|(_, rect)| *rect);
    let detail_area = placed
        .iter()
        .find(|(card, _)| *card == CardId::ProjectDetail)
        .map(|(_, rect)| *rect);
    let attribution_area = placed
        .iter()
        .find(|(card, _)| *card == CardId::AttributionTable)
        .map(|(_, rect)| *rect);

    // 1. 프로젝트·브랜치 표
    let projects: Option<Vec<ProjectStatsRow>> = match &state.snapshot.projects {
        Panel::Ready(rows) => {
            let mut sorted = rows.clone();
            sort_rows(
                &mut sorted,
                state.card_sorts[CardId::ProjectTable as usize],
                compare_project_row,
            );
            Some(sorted)
        }
        _ => None,
    };
    let project_totals = projects
        .as_ref()
        .map_or(WindowTotals::default(), |sorted| WindowTotals {
            requests: sorted.iter().map(|row| row.requests).sum(),
            cost: sorted.iter().map(|row| row.cost_usd_estimate).sum(),
        });
    match (&projects, table_area) {
        (_, None) => {}
        (Some(sorted), Some(table_area)) => {
            let cost_total = project_totals.cost;
            let rows: Vec<Vec<TableCell>> = sorted
                .iter()
                .map(|row| {
                    let text_or_dash = |text: &str| {
                        if text.is_empty() {
                            vec![Span::styled("-", theme::STYLE_DIM)]
                        } else {
                            vec![Span::raw(text.to_string())]
                        }
                    };
                    let wait_total = if row.wait_total_ms == 0 {
                        vec![Span::styled("-", theme::STYLE_DIM)]
                    } else {
                        vec![Span::raw(format_duration_ms(row.wait_total_ms))]
                    };
                    let total_tokens = row.input_tokens
                        + row.output_tokens
                        + row.cache_read_tokens
                        + row.cache_creation_5m_tokens
                        + row.cache_creation_1h_tokens;
                    let tokens_per_request = if row.requests == 0 {
                        vec![Span::styled("-", theme::STYLE_DIM)]
                    } else {
                        let requests = i64::try_from(row.requests).unwrap_or(i64::MAX);
                        vec![Span::raw(format_compact(total_tokens / requests))]
                    };
                    vec![
                        text_or_dash(&row.project),
                        text_or_dash(&row.git_branch),
                        vec![Span::raw(format_count(
                            i64::try_from(row.requests).unwrap_or(i64::MAX),
                        ))],
                        vec![Span::raw(format_count(
                            i64::try_from(row.sessions).unwrap_or(i64::MAX),
                        ))],
                        vec![Span::raw(format_compact(row.input_tokens))],
                        vec![Span::raw(format_compact(row.output_tokens))],
                        vec![Span::raw(format_compact(row.cache_read_tokens))],
                        vec![Span::raw(format_compact(row.cache_creation_5m_tokens))],
                        vec![Span::raw(format_compact(row.cache_creation_1h_tokens))],
                        vec![Span::raw(format!("${:.2}", row.cost_usd_estimate))],
                        vec![Span::styled(
                            format_clock_time(&row.last_ts),
                            theme::STYLE_DIM,
                        )],
                        wait_total,
                        tokens_per_request,
                    ]
                })
                .collect();
            render_card_table(
                frame,
                state,
                table_area,
                CardTable {
                    card: CardId::ProjectTable,
                    title: i18n::project_table_title(state.lang, sorted.len(), cost_total),
                    hint: String::new(),
                    columns: PROJECT_COLUMNS.to_vec(),
                    rows,
                    aux_lines: Vec::new(),
                },
            );
        }
        (None, Some(table_area)) => {
            let note = panel_message(state.lang, &state.snapshot.projects).unwrap_or_default();
            let title = i18n::text(state.lang, Msg::TitleProjectStatsPending);
            render_notice_card(frame, state, CardId::ProjectTable, table_area, title, note);
        }
    }

    // 2. 선택 프로젝트 상세
    let selected = state.card_views[CardId::ProjectTable as usize].selected;
    let selected_project = projects.as_ref().and_then(|sorted| sorted.get(selected));
    let (detail_title, detail_lines) =
        build_project_detail(state.lang, selected_project, project_totals);
    if let Some(detail_area) = detail_area {
        render_text_card(
            frame,
            state,
            detail_area,
            TextCard {
                card: CardId::ProjectDetail,
                title: detail_title,
                hint: String::new(),
                lines: detail_lines,
            },
        );
    }

    // 3. 귀속 표 (d 키로 차원 순환, 지표가 없으면 OTel 수집 안내)
    let lang = state.lang;
    let dimension = state.attribution_dimension;
    let dimension_label = i18n::text(
        lang,
        match dimension {
            AttributionDimension::Skill => Msg::DimensionSkill,
            AttributionDimension::McpServer => Msg::DimensionMcpServer,
            AttributionDimension::Plugin => Msg::DimensionPlugin,
            AttributionDimension::Agent => Msg::DimensionAgent,
            AttributionDimension::Model => Msg::DimensionModel,
            AttributionDimension::QuerySource => Msg::DimensionQuerySource,
        },
    );
    let attribution = attribution_rows(&state.snapshot, dimension).filter(|rows| !rows.is_empty());
    match (attribution, attribution_area) {
        (_, None) => {}
        (Some(rows), Some(attribution_area)) => {
            let mut sorted = rows.to_vec();
            sort_rows(
                &mut sorted,
                state.card_sorts[CardId::AttributionTable as usize],
                compare_attribution_row,
            );
            let table_rows: Vec<Vec<TableCell>> = sorted
                .iter()
                .map(|row| {
                    vec![
                        vec![Span::raw(row.name.clone())],
                        vec![Span::raw(format!("${:.2}", row.cost_usd))],
                        vec![Span::raw(format_compact(row.input_tokens))],
                        vec![Span::raw(format_compact(row.output_tokens))],
                        vec![Span::raw(format_compact(row.cache_read_tokens))],
                        vec![Span::raw(format_compact(row.cache_creation_tokens))],
                    ]
                })
                .collect();
            render_card_table(
                frame,
                state,
                attribution_area,
                CardTable {
                    card: CardId::AttributionTable,
                    title: i18n::attribution_title(lang, dimension_label, sorted.len()),
                    hint: i18n::text(lang, Msg::HintDimension).to_string(),
                    columns: ATTRIBUTION_COLUMNS.to_vec(),
                    rows: table_rows,
                    aux_lines: Vec::new(),
                },
            );
        }
        (None, Some(attribution_area)) => {
            let note = match &state.snapshot.attribution {
                Panel::Ready(sets) => {
                    // 다른 차원에는 행이 있으면 이 차원만 빈 것, 모두 비면 OTel 지표 자체가 없음
                    let any_rows = sets.iter().any(|(_, rows)| !rows.is_empty());
                    let (message, hint) = if any_rows {
                        (Msg::AttributionNoDimension, Msg::AttributionDimensionHint)
                    } else {
                        (Msg::AttributionNoMetrics, Msg::AttributionEnableHint)
                    };
                    PanelNote {
                        message: i18n::text(lang, message).to_string(),
                        color: theme::FG_DIM,
                        hint: Some(hint),
                    }
                }
                other => panel_message(lang, other).unwrap_or_default(),
            };
            let title = i18n::attribution_title(lang, dimension_label, 0);
            render_notice_card(
                frame,
                state,
                CardId::AttributionTable,
                attribution_area,
                &title,
                note,
            );
        }
    }
}

// 3. 프로젝트 상세 ------------------------------------------------------------
// 제목(프로젝트·브랜치)과 항목 줄 (요청·세션, 입출력, 캐시 읽기·생성, 추정 비용, 요청·세션당 비용, 구간 점유, 처음·마지막 요청,
// 대기 평균·최대, 대기 합계, 토큰 합계·요청당)
// 이어서 모델·에이전트·세션별 비용 구역
fn build_project_detail(
    lang: Lang,
    row: Option<&ProjectStatsRow>,
    window: WindowTotals,
) -> (String, Vec<Line<'static>>) {
    let Some(row) = row else {
        return (
            i18n::text(lang, Msg::TitleProjectDetail).to_string(),
            vec![Line::from(Span::styled(
                i18n::text(lang, Msg::NoProjectSelected),
                theme::STYLE_DIM,
            ))],
        );
    };
    let wait_avg = row.wait_avg_ms.map_or("-".to_string(), format_duration_ms);
    let wait_max = row.wait_max_ms.map_or("-".to_string(), format_duration_ms);
    let total_tokens = row.input_tokens
        + row.output_tokens
        + row.cache_read_tokens
        + row.cache_creation_5m_tokens
        + row.cache_creation_1h_tokens;
    let tokens_per_request = if row.requests == 0 {
        "-".to_string()
    } else {
        format_compact(total_tokens / i64::try_from(row.requests).unwrap_or(i64::MAX))
    };
    let pairs: [(&str, String); PROJECT_DETAIL_LINES] = [
        (
            i18n::text(lang, Msg::ProjectCounts),
            format!(
                "{} / {}",
                format_count(i64::try_from(row.requests).unwrap_or(i64::MAX)),
                format_count(i64::try_from(row.sessions).unwrap_or(i64::MAX)),
            ),
        ),
        (
            i18n::text(lang, Msg::InOut),
            format!(
                "{} / {}",
                format_compact(row.input_tokens),
                format_compact(row.output_tokens)
            ),
        ),
        (
            i18n::text(lang, Msg::ProjectCache),
            format!(
                "{} / {} / {}",
                format_compact(row.cache_read_tokens),
                format_compact(row.cache_creation_5m_tokens),
                format_compact(row.cache_creation_1h_tokens)
            ),
        ),
        (
            i18n::text(lang, Msg::CostEstimate),
            i18n::cost_value(lang, row.cost_usd_estimate),
        ),
        (
            i18n::text(lang, Msg::DetailPerRequest),
            per_request(row.cost_usd_estimate, row.output_tokens, row.requests),
        ),
        (
            i18n::text(lang, Msg::DetailPerSession),
            if row.sessions > 0 {
                format!(
                    "{:.1} / ${:.2}",
                    row.requests as f64 / row.sessions as f64,
                    row.cost_usd_estimate / row.sessions as f64
                )
            } else {
                "-".to_string()
            },
        ),
        (
            i18n::text(lang, Msg::DetailWindowShare),
            window_share(lang, row.requests, row.cost_usd_estimate, window),
        ),
        (
            i18n::text(lang, Msg::DetailFirstLast),
            first_last(&row.first_ts, &row.last_ts),
        ),
        (
            i18n::text(lang, Msg::WaitAvgMax),
            format!("{wait_avg} / {wait_max}"),
        ),
        (
            i18n::text(lang, Msg::WaitTotal),
            if row.wait_total_ms == 0 {
                "-".to_string()
            } else {
                format_duration_ms(row.wait_total_ms)
            },
        ),
        (
            i18n::text(lang, Msg::DetailTokensPerReq),
            format!("{} / {tokens_per_request}", format_compact(total_tokens)),
        ),
    ];
    let mut lines: Vec<Line<'static>> = pairs
        .into_iter()
        .map(|(name, value)| pair_line(name, value))
        .collect();
    lines.extend(share_lines(
        lang,
        Msg::SectionModelsByCost,
        &row.models,
        ShareBasis::Cost,
        row.cost_usd_estimate,
    ));
    lines.extend(share_lines(
        lang,
        Msg::SectionAgentsByCost,
        &row.agents,
        ShareBasis::Cost,
        row.cost_usd_estimate,
    ));
    let sessions: Vec<_> = row
        .top_sessions
        .iter()
        .map(|session| crate::store::query::UsageShare {
            name: session.name.chars().take(SESSION_ID_PREFIX).collect(),
            ..session.clone()
        })
        .collect();
    lines.extend(share_lines(
        lang,
        Msg::SectionSessionsByCost,
        &sessions,
        ShareBasis::Cost,
        row.cost_usd_estimate,
    ));
    let mut title = i18n::text(lang, Msg::TitleProjectDetail).to_string();
    for segment in [&row.project, &row.git_branch] {
        if !segment.is_empty() {
            title = format!("{title} · {segment}");
        }
    }
    (title, lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::cards::{ColumnHit, HeaderHit};
    use crate::tui::layout::SplitGroup;
    use crate::tui::layout::grid::{TabGrid, default_rows};
    use crate::tui::sort::{SortKind, SortState, sort_spec};
    use crate::tui::state::{ATTRIBUTION_DIMENSIONS, UiCommand};
    use crate::tui::test_support::sample_state_with_tables;
    use crate::tui::views::detail::DETAIL_LABEL_WIDTH;
    use crate::tui::views::render_frame;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use std::time::{Duration, Instant};

    const SIZES: [(u16, u16); 3] = [(60, 16), (100, 30), (130, 40)];
    const BOTH: [Lang; 2] = [Lang::En, Lang::Ko];
    const PROJECT_CARDS: [CardId; 2] = [CardId::ProjectTable, CardId::AttributionTable];

    fn projects_state(lang: Lang) -> AppState {
        let mut state = sample_state_with_tables();
        state.lang = lang;
        state.tab = ActiveTab::Projects;
        state
    }

    fn draw(state: &mut AppState, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_frame(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    // 영역 한 줄의 글자 (넓은 글자 뒤 칸은 건너뜀)
    fn row_text(buffer: &Buffer, y: u16) -> String {
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
        text
    }

    fn screen_text(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| row_text(buffer, y))
            .collect::<Vec<String>>()
            .join("\n")
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(state: &mut AppState, column: u16, row: u16) -> UiCommand {
        state.handle_mouse_event_at(
            mouse(MouseEventKind::Down(MouseButton::Left), column, row),
            Instant::now(),
        )
    }

    fn press(state: &mut AppState, letter: char) -> UiCommand {
        state.handle_key_event(KeyEvent::new(KeyCode::Char(letter), KeyModifiers::NONE))
    }

    fn header_hits(state: &AppState, card: CardId) -> Vec<HeaderHit> {
        state
            .hit_areas
            .header_cells
            .iter()
            .filter(|hit| hit.card == card)
            .copied()
            .collect()
    }

    fn column_hits(state: &AppState, card: CardId) -> Vec<ColumnHit> {
        state
            .hit_areas
            .column_hits
            .iter()
            .filter(|hit| hit.card == card)
            .copied()
            .collect()
    }

    fn card_rect(state: &AppState, card: CardId) -> Rect {
        state
            .hit_areas
            .card_rects
            .iter()
            .find(|(id, _)| *id == card)
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("{card:?} 가 그려지지 않음"))
    }

    fn columns_of(card: CardId) -> Vec<ColumnSpec> {
        match card {
            CardId::ProjectTable => PROJECT_COLUMNS.to_vec(),
            _ => ATTRIBUTION_COLUMNS.to_vec(),
        }
    }

    fn grid_from(rows: &[&[CardId]]) -> TabGrid {
        let mut grid = TabGrid::default();
        for (row, cards) in rows.iter().enumerate() {
            for (column, card) in cards.iter().enumerate() {
                grid.rows[row][column] = Some(*card);
            }
        }
        grid
    }

    #[test]
    fn the_projects_tab_draws_its_three_cards_in_both_languages_at_the_size_matrix() {
        for lang in BOTH {
            for (width, height) in SIZES {
                let case = format!("{lang:?} {width}x{height}");
                let mut state = projects_state(lang);
                let buffer = draw(&mut state, width, height);
                let text = screen_text(&buffer);

                // 세 카드가 모두 등록되고 서로 겹치지 않으며 화면 안에 있음
                let cards = [
                    CardId::ProjectTable,
                    CardId::ProjectDetail,
                    CardId::AttributionTable,
                ];
                let frame = Rect::new(0, 0, width, height);
                for (index, card) in cards.iter().enumerate() {
                    let rect = card_rect(&state, *card);
                    assert_eq!(rect.intersection(frame), rect, "{case} {card:?}");
                    for other in &cards[index + 1..] {
                        assert!(!rect.intersects(card_rect(&state, *other)), "{case}");
                    }
                }

                // 제목과 머리글: 프로젝트 표와 귀속 표의 이름이 보임
                let (project_title, attribution_title) = match lang {
                    Lang::En => ("Projects", "Attribution"),
                    Lang::Ko => ("프로젝트", "귀속"),
                };
                assert!(text.contains(project_title), "{case}\n{text}");
                assert!(text.contains(attribution_title), "{case}\n{text}");
                assert_eq!(state.hit_areas.tab_rects.len(), 9, "{case}");
                // 폭이 모자라면 글자 열이 먼저 줄고 숫자는 유지됨 (100열, 130열은 열세 열 전부에 이름 앞부분)
                // 이름 전체는 200열 검사 (wait_and_tokens_per_request_columns_...)
                if width >= 100 {
                    assert!(text.contains("frontend-tui"), "{case}");
                    assert!(text.contains("$8.40"), "{case}");
                }
                if width >= 130 {
                    assert!(text.contains("claude-moni"), "{case}");
                    assert!(text.contains("feature/pr"), "{case}\n{text}");
                    assert!(text.contains("8.0K"), "{case}\n{text}");
                }
            }
        }
    }

    // 기본 격자: 카드 한 장이 한 줄, 높이는 지금의 공식, 줄 경계 드래그 영역 둘
    #[test]
    fn the_projects_cards_tile_the_body_with_the_default_heights_at_the_size_matrix() {
        let expected = [
            (60, 16, [4, 4, 4]),
            (100, 30, [10, 12, 4]),
            (130, 40, [14, 15, 7]),
        ];
        let cards = [
            CardId::ProjectTable,
            CardId::ProjectDetail,
            CardId::AttributionTable,
        ];
        for lang in BOTH {
            for (width, height, heights) in expected {
                let case = format!("{lang:?} {width}x{height}");
                let mut state = projects_state(lang);
                draw(&mut state, width, height);
                let mut top = 2;
                for (card, card_height) in cards.iter().zip(heights) {
                    let rect = card_rect(&state, *card);
                    assert_eq!(
                        (rect.x, rect.y, rect.width, rect.height),
                        (0, top, width, card_height),
                        "{case} {card:?}"
                    );
                    top += rect.height;
                }
                assert_eq!(top, height - 2, "{case}");
                let row_hits = state
                    .hit_areas
                    .split_hits
                    .iter()
                    .filter(|hit| hit.group == SplitGroup::GridRows(ActiveTab::Projects, 3))
                    .count();
                assert_eq!(row_hits, 2, "{case}");
            }
        }
    }

    // 격자 한 줄에 두 장을 두면 같은 y 에 나란히
    #[test]
    fn two_project_cards_in_one_grid_row_share_a_line_at_130_columns() {
        let pairs = [
            [CardId::ProjectTable, CardId::ProjectDetail],
            [CardId::ProjectDetail, CardId::AttributionTable],
        ];
        for [left, right] in pairs {
            let rest: Vec<CardId> = [
                CardId::ProjectTable,
                CardId::ProjectDetail,
                CardId::AttributionTable,
            ]
            .into_iter()
            .filter(|card| *card != left && *card != right)
            .collect();
            let mut state = projects_state(Lang::En);
            state
                .card_layout
                .set_grid(ActiveTab::Projects, grid_from(&[&[left, right], &rest]));
            draw(&mut state, 130, 40);
            let left_rect = card_rect(&state, left);
            let right_rect = card_rect(&state, right);
            assert_eq!(left_rect.y, right_rect.y, "{left:?} {right:?}");
            assert!(left_rect.right() < right_rect.x, "{left:?} {right:?}");
            assert!(!card_rect(&state, rest[0]).intersects(left_rect));
        }
    }

    #[test]
    fn the_tab_label_is_in_the_tab_bar_and_the_8_key_opens_the_tab() {
        for lang in BOTH {
            let mut state = sample_state_with_tables();
            state.lang = lang;
            let buffer = draw(&mut state, 130, 40);
            let tab_row = row_text(&buffer, 1);
            let expected = match lang {
                Lang::En => "8 Projects",
                Lang::Ko => "8 프로젝트",
            };
            assert!(tab_row.contains(expected), "{lang:?}: {tab_row}");
            press(&mut state, '8');
            assert_eq!(state.tab, ActiveTab::Projects, "{lang:?}");
        }
    }

    #[test]
    fn all_nine_tabs_stay_visible_and_clickable_from_60_to_wide_columns() {
        for lang in BOTH {
            for width in 60..=120u16 {
                let mut state = sample_state_with_tables();
                state.lang = lang;
                let buffer = draw(&mut state, width, 16);
                let case = format!("{lang:?} {width}");
                let rects: Vec<(ActiveTab, Rect)> = state.hit_areas.tab_rects.clone();
                assert_eq!(rects.len(), 9, "{case}");
                for pair in rects.windows(2) {
                    assert!(pair[0].1.right() <= pair[1].1.x, "{case}");
                }
                assert!(rects.last().unwrap().1.right() <= width, "{case}");
                let tab_row = row_text(&buffer, 1);
                // 현재 탭 이름은 항상 전부 보임
                assert!(
                    tab_row.contains(state.tab.tab_label(lang)),
                    "{case}: {tab_row}"
                );
                // 글자 폭이 들어가면(한국어 60열, 영어 67열 이상) 모든 탭 글자가 잘리지 않고 보임
                let glued: usize = ActiveTab::ALL
                    .iter()
                    .map(|tab| Span::raw(tab.tab_label(lang)).width() + 1)
                    .sum();
                if usize::from(width) >= glued {
                    for (tab, _) in &rects {
                        assert!(tab_row.contains(tab.tab_label(lang)), "{case}: {tab_row}");
                    }
                } else {
                    // 모자라면 다른 탭 이름은 말줄임, 번호는 모두 남음
                    assert!(tab_row.contains('…'), "{case}: {tab_row}");
                    for digit in 1..=9 {
                        assert!(tab_row.contains(&digit.to_string()), "{case}: {tab_row}");
                    }
                }
            }
            let mut state = sample_state_with_tables();
            state.lang = lang;
            draw(&mut state, 60, 16);
            // 현재 탭이 바뀌면 이름 폭도 바뀌므로 매번 마지막 그림의 영역을 클릭
            for tab in ActiveTab::ALL {
                let rect = state
                    .hit_areas
                    .tab_rects
                    .iter()
                    .find(|(hit_tab, _)| *hit_tab == tab)
                    .map(|(_, rect)| *rect)
                    .unwrap_or_else(|| panic!("{lang:?} {tab:?}"));
                click(&mut state, rect.x + rect.width / 2, rect.y);
                assert_eq!(state.tab, tab, "{lang:?} {tab:?}");
                draw(&mut state, 60, 16);
            }
        }
    }

    #[test]
    fn header_clicks_sort_numbers_descending_first_and_text_ascending_first_then_reset() {
        for card in PROJECT_CARDS {
            let spec = sort_spec(card).unwrap();
            let mut state = projects_state(Lang::En);
            draw(&mut state, 130, 40);
            let hits = header_hits(&state, card);
            let columns = columns_of(card);
            assert_eq!(hits.len(), columns.len(), "{card:?}");
            for kind in [SortKind::Number, SortKind::Text] {
                let hit = hits
                    .iter()
                    .find(|hit| spec.kinds[hit.column] == kind && hit.column != spec.default.column)
                    .unwrap_or_else(|| panic!("{card:?} {kind:?}"));
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
                    assert_eq!(state.card_sorts[card as usize], expected, "{card:?}");

                    // 머리글에 정렬 화살표, 제목에 정렬 열
                    let buffer = draw(&mut state, 130, 40);
                    let rect = card_rect(&state, card);
                    let label = i18n::text(Lang::En, columns[expected.column].0);
                    let header = row_text(&buffer, rect.y + 1);
                    assert!(
                        header.contains(&format!("{label}{}", expected.sort_arrow())),
                        "{card:?}: {header}"
                    );
                    assert!(
                        row_text(&buffer, rect.y).contains(&format!("Sort {label}")),
                        "{card:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn sorting_keeps_the_detail_card_on_the_selected_project() {
        let mut state = projects_state(Lang::En);
        let buffer = draw(&mut state, 130, 40);
        // 비용 내림차순 첫 행이 선택 상태
        assert!(screen_text(&buffer).contains("Project detail · claude-monitor · main"));

        // 선택을 두 번째 행으로, 이어서 요청 열 정렬로 바꾸어도 같은 프로젝트를 따라감
        state.card_views[CardId::ProjectTable as usize].selected = 1;
        let buffer = draw(&mut state, 130, 40);
        let text = screen_text(&buffer);
        assert!(text.contains("Project detail · claude-monitor · feature/projects-tab"));
        state.change_card_sort(
            CardId::ProjectTable,
            crate::tui::sort::SortChange::SelectColumn(1),
        );
        assert_eq!(state.card_sorts[CardId::ProjectTable as usize].column, 1);
        let buffer = draw(&mut state, 130, 40);
        assert!(
            screen_text(&buffer).contains("Project detail · claude-monitor · feature/projects-tab")
        );
    }

    #[test]
    fn dragging_a_column_boundary_resizes_it_and_a_quick_second_click_resets_to_auto() {
        for card in PROJECT_CARDS {
            let case = format!("{card:?}");
            let mut state = projects_state(Lang::En);
            draw(&mut state, 130, 40);
            let bound = column_hits(&state, card)[0];
            let before = header_hits(&state, card)[0].rect.width;
            assert_eq!(bound.width, before, "{case}");

            // 경계 칸은 평소 흐린 │
            let buffer = draw(&mut state, 130, 40);
            let cell = buffer.cell((bound.line.x, bound.line.y)).unwrap();
            assert_eq!(cell.symbol(), "│", "{case}");
            assert_eq!(cell.fg, theme::FG_FAINT, "{case}");

            let start = Instant::now();
            let after = |millis: u64| start + Duration::from_millis(millis);
            let down = |x: u16| mouse(MouseEventKind::Down(MouseButton::Left), x, bound.grab.y);
            state.handle_mouse_event_at(down(bound.line.x), after(0));
            state.handle_mouse_event_at(
                mouse(
                    MouseEventKind::Drag(MouseButton::Left),
                    bound.line.x - 3,
                    bound.grab.y,
                ),
                after(10),
            );
            let released = state.handle_mouse_event_at(
                mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    bound.line.x - 3,
                    bound.grab.y,
                ),
                after(20),
            );
            assert_eq!(released, UiCommand::SaveState, "{case}");
            assert_eq!(
                state.column_widths[card as usize][0],
                Some(before - 3),
                "{case}"
            );
            draw(&mut state, 130, 40);
            assert_eq!(
                header_hits(&state, card)[0].rect.width,
                before - 3,
                "{case}"
            );

            // 끌지 않은 클릭 뒤 곧바로 다시 누르면 자동 폭 복원
            let again = column_hits(&state, card)[0];
            state.handle_mouse_event_at(down(again.line.x), after(100));
            state.handle_mouse_event_at(
                mouse(
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
        }
    }

    #[test]
    fn the_l_key_resets_widths_and_card_layout_of_the_projects_tab_only() {
        let mut state = projects_state(Lang::En);
        for card in PROJECT_CARDS {
            state.column_widths[card as usize][1] = Some(21);
        }
        state.column_widths[CardId::ModelTable as usize][1] = Some(21);
        // 격자 줄 순서와 줄 높이 비율을 바꾼 뒤 l
        let tab_key = ActiveTab::Projects.tab_key();
        state.card_layout.set_grid(
            ActiveTab::Projects,
            grid_from(&[
                &[CardId::AttributionTable],
                &[CardId::ProjectTable],
                &[CardId::ProjectDetail],
            ]),
        );
        assert!(
            state
                .card_layout
                .restore_grid_ratios(&format!("{tab_key}_rows_3"), &[3_000, 6_000])
        );
        assert_ne!(
            state.card_layout.grid_of(ActiveTab::Projects),
            default_rows(ActiveTab::Projects)
        );
        assert_eq!(press(&mut state, 'l'), UiCommand::SaveState);
        for card in PROJECT_CARDS {
            assert!(
                state.column_widths[card as usize]
                    .iter()
                    .all(Option::is_none),
                "{card:?}"
            );
        }
        assert_eq!(
            state.card_layout.grid_of(ActiveTab::Projects),
            default_rows(ActiveTab::Projects)
        );
        let prefix = format!("{tab_key}_");
        assert!(
            state
                .card_layout
                .grid_ratio_entries()
                .iter()
                .all(|(key, _)| !key.starts_with(&prefix))
        );
        // 다른 탭의 열 폭은 그대로, 언어도 그대로
        assert_eq!(
            state.column_widths[CardId::ModelTable as usize][1],
            Some(21)
        );
        assert_eq!(state.lang, Lang::En);
    }

    #[test]
    fn the_d_key_cycles_the_dimension_changes_the_rows_and_only_acts_on_this_tab() {
        let mut state = projects_state(Lang::En);
        let text = screen_text(&draw(&mut state, 130, 40));
        assert!(text.contains("Attribution · by Skill · 2 names"), "{text}");
        assert!(text.contains("frontend-tui") && !text.contains("serena-mcp"));

        press(&mut state, 'd');
        let text = screen_text(&draw(&mut state, 130, 40));
        assert!(text.contains("by MCP server"), "{text}");
        assert!(text.contains("serena-mcp") && !text.contains("frontend-tui"));

        // 이 차원만 비면 다른 안내 (OTel 수집 안내가 아님)
        press(&mut state, 'd');
        let text = screen_text(&draw(&mut state, 130, 40));
        assert!(text.contains("No data for this dimension"), "{text}");
        assert!(text.contains("Press d to change dimension"), "{text}");
        assert!(!text.contains("OTEL_METRICS_EXPORTER"), "{text}");

        press(&mut state, 'd');
        let text = screen_text(&draw(&mut state, 130, 40));
        assert!(
            text.contains("by Agent") && text.contains("agent-planner"),
            "{text}"
        );
        press(&mut state, 'd');
        let text = screen_text(&draw(&mut state, 130, 40));
        assert!(
            text.contains("by Model") && text.contains("claude-sonnet-5-5"),
            "{text}"
        );

        // 다섯 차원을 돌면 처음으로, 선택·스크롤은 차원이 바뀔 때마다 처음으로
        state.card_views[CardId::AttributionTable as usize].selected = 1;
        press(&mut state, 'D');
        assert_eq!(state.attribution_dimension, ATTRIBUTION_DIMENSIONS[0]);
        assert_eq!(
            state.card_views[CardId::AttributionTable as usize].selected,
            0
        );

        // 다른 탭에서는 d 가 아무 일도 하지 않음
        state.tab = ActiveTab::Models;
        assert_eq!(press(&mut state, 'd'), UiCommand::None);
        assert_eq!(state.attribution_dimension, ATTRIBUTION_DIMENSIONS[0]);
    }

    #[test]
    fn the_footer_and_help_list_the_dimension_key_in_both_languages() {
        for lang in BOTH {
            let mut state = projects_state(lang);
            let hint = i18n::text(lang, Msg::HintDimension);
            let buffer = draw(&mut state, 130, 40);
            assert!(row_text(&buffer, 39).contains(hint), "{lang:?}");

            // 다른 탭의 안내 줄에는 없음
            state.tab = ActiveTab::Models;
            let buffer = draw(&mut state, 130, 40);
            assert!(!row_text(&buffer, 39).contains(hint), "{lang:?}");

            state.show_help = true;
            let text = screen_text(&draw(&mut state, 130, 50));
            assert!(
                text.contains(i18n::text(lang, Msg::HelpDimension)),
                "{lang:?}\n{text}"
            );
        }
    }

    #[test]
    fn empty_and_pending_states_explain_the_next_step_in_both_languages() {
        for lang in BOTH {
            // 지표가 하나도 없으면 OTel 수집 안내
            let mut state = projects_state(lang);
            state.snapshot.attribution = Panel::Ready(
                ATTRIBUTION_DIMENSIONS
                    .iter()
                    .map(|dimension| (*dimension, Vec::new()))
                    .collect(),
            );
            for (width, height) in SIZES {
                let text = screen_text(&draw(&mut state, width, height));
                let headline = i18n::text(lang, Msg::AttributionNoMetrics);
                assert!(text.contains(headline), "{lang:?} {width}x{height}\n{text}");
                if height >= 30 {
                    assert!(
                        text.contains("OTEL_METRICS_EXPORTER=otlp"),
                        "{lang:?} {width}x{height}\n{text}"
                    );
                }
            }

            // 집계 전에는 대기 안내, 프로젝트 상세는 선택 없음 안내
            let mut pending = projects_state(lang);
            pending.snapshot.projects = Panel::Pending;
            pending.snapshot.attribution = Panel::Pending;
            let text = screen_text(&draw(&mut pending, 130, 40));
            assert!(
                text.contains(i18n::text(lang, Msg::PanelPending)),
                "{lang:?}\n{text}"
            );
            assert!(text.contains(i18n::text(lang, Msg::NoProjectSelected)));

            // 구간 안 프로젝트가 없으면 표 안 빈 상태
            let mut none = projects_state(lang);
            none.snapshot.projects = Panel::Ready(Vec::new());
            let text = screen_text(&draw(&mut none, 130, 40));
            assert!(text.contains(i18n::text(lang, Msg::ChangeWindowHint)));
        }
    }

    #[test]
    fn project_detail_lists_cache_writes_and_the_cost_with_fitting_labels() {
        let row = ProjectStatsRow {
            project: "claude-monitor".to_string(),
            git_branch: "main".to_string(),
            requests: 1_200,
            sessions: 4,
            input_tokens: 2_000_000,
            output_tokens: 400_000,
            cache_read_tokens: 9_000_000,
            cache_creation_5m_tokens: 300_000,
            cache_creation_1h_tokens: 50_000,
            cost_usd_estimate: 12.345,
            wait_avg_ms: Some(1_500),
            wait_max_ms: Some(65_000),
            wait_total_ms: 7_380_000,
            last_ts: "2026-11-15T01:00:00Z".to_string(),
            ..ProjectStatsRow::default()
        };
        let line_text = |line: &Line<'_>| -> String {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };
        let window = WindowTotals {
            requests: 2_400,
            cost: 12.345,
        };
        for lang in BOTH {
            let (title, lines) = build_project_detail(lang, Some(&row), window);
            let text: Vec<String> = lines.iter().map(line_text).collect();
            assert!(title.ends_with("· claude-monitor · main"), "{title}");
            assert_eq!(lines.len(), PROJECT_DETAIL_LINES);
            // 요청당·세션당 비용과 구간 점유 (요청 절반, 비용 전부)
            assert!(
                text.iter().any(|line| line.contains("$0.0103 / 333")),
                "{text:?}"
            );
            assert!(
                text.iter().any(|line| line.contains("300.0 / $3.09")),
                "{text:?}"
            );
            assert!(
                text.iter().any(|line| line.contains("50.0% · ")),
                "{text:?}"
            );
            assert!(text.iter().any(|line| line.contains("100.0%")), "{text:?}");
            assert!(
                text.iter().any(|line| line.contains("1,200 / 4")),
                "{text:?}"
            );
            assert!(text.iter().any(|line| line.contains("$12.35")), "{text:?}");
            assert!(
                text.iter().any(|line| line.contains("300.0K / 50.0K")),
                "{text:?}"
            );
            // 대기 평균·최대, 대기 합계, 토큰 합계·요청당 (항목 끝 세 줄)
            let new_lines = [
                (Msg::WaitAvgMax, "1.5s / 1m 05s"),
                (Msg::WaitTotal, "2h 03m"),
                (Msg::DetailTokensPerReq, "M / 9.8K"),
            ];
            for (offset, (msg, value)) in new_lines.into_iter().enumerate() {
                let line = &text[PROJECT_DETAIL_LINES - 3 + offset];
                assert!(line.starts_with(i18n::text(lang, msg)), "{lang:?} {line}");
                assert!(line.contains(value), "{lang:?} {line}");
            }
            // 항목 이름은 이름 칸 안에서 한 칸 이상 비움
            for msg in [Msg::ProjectCounts, Msg::ProjectCache] {
                assert!(
                    Span::raw(i18n::text(lang, msg)).width() < DETAIL_LABEL_WIDTH,
                    "{lang:?} {msg:?}"
                );
            }
        }

        // 브랜치가 없으면 제목에 구분자를 덧붙이지 않음
        let nameless = ProjectStatsRow {
            project: "solo".to_string(),
            ..ProjectStatsRow::default()
        };
        let (title, nameless_lines) = build_project_detail(Lang::En, Some(&nameless), window);
        assert_eq!(title, "Project detail · solo");
        // 대기·요청이 없으면 값 자리에 -
        let tail: Vec<String> = nameless_lines[PROJECT_DETAIL_LINES - 3..PROJECT_DETAIL_LINES]
            .iter()
            .map(line_text)
            .collect();
        assert!(tail[0].ends_with("- / -"), "{tail:?}");
        assert!(tail[1].ends_with('-'), "{tail:?}");
        assert!(tail[2].ends_with("0 / -"), "{tail:?}");
        let (empty_title, empty_lines) = build_project_detail(Lang::Ko, None, window);
        assert_eq!(empty_title, "프로젝트 상세");
        assert_eq!(empty_lines.len(), 1);
    }

    #[test]
    fn wait_and_tokens_per_request_columns_and_detail_lines_render_and_hide_right_first() {
        for lang in BOTH {
            let wait_label = i18n::text(lang, Msg::ColTotalWait);
            let per_req_label = i18n::text(lang, Msg::ColTokPerReq);
            for (width, height) in [(60, 16), (100, 30), (130, 40), (200, 40)] {
                let case = format!("{lang:?} {width}x{height}");
                let mut state = projects_state(lang);
                if let Panel::Ready(rows) = &mut state.snapshot.projects {
                    rows[0].wait_total_ms = 125_000;
                }
                let buffer = draw(&mut state, width, height);
                let rect = card_rect(&state, CardId::ProjectTable);
                let header = row_text(&buffer, rect.y + 1);

                // 보이는 열은 항상 앞쪽부터 이어짐 (오른쪽 열이 먼저 숨음), 프로젝트 열은 남음
                let shown: Vec<usize> = header_hits(&state, CardId::ProjectTable)
                    .iter()
                    .map(|hit| hit.column)
                    .collect();
                let prefix: Vec<usize> = (0..shown.len()).collect();
                assert_eq!(shown, prefix, "{case}");
                assert!(shown.contains(&0), "{case}: {header}");
                assert_eq!(
                    header.contains(wait_label),
                    shown.contains(&11),
                    "{case}: {header}"
                );
                assert_eq!(
                    header.contains(per_req_label),
                    shown.contains(&12),
                    "{case}: {header}"
                );
                if width == 60 {
                    assert!(shown.len() < PROJECT_COLUMNS.len(), "{case}");
                    assert!(!header.contains(per_req_label), "{case}: {header}");
                }
                if width < 200 {
                    continue;
                }

                // 넓은 화면: 열세 개 전부, 첫 행은 대기 합계 2m 05s, 요청당 토큰 7.8M / 900
                assert_eq!(shown.len(), PROJECT_COLUMNS.len(), "{case}");
                let first = row_text(&buffer, rect.y + 2);
                for value in [
                    "claude-monitor",
                    "main",
                    "900",
                    "1.2M",
                    "300.0K",
                    "6.0M",
                    "$8.40",
                    "2m 05s",
                    "8.7K",
                ] {
                    assert!(first.contains(value), "{case} {value}: {first}");
                }
                // 대기가 없는 둘째 행은 대기 합계가 흐린 -, 요청당 토큰은 3.21M / 400
                let second = row_text(&buffer, rect.y + 3);
                assert!(second.contains("feature/projects-tab"), "{case}: {second}");
                assert!(second.contains("8.0K"), "{case}: {second}");
                let dash = header_hits(&state, CardId::ProjectTable)[11].rect;
                let dash_cells: Vec<_> = (dash.x..dash.right())
                    .filter_map(|x| buffer.cell((x, rect.y + 3)))
                    .filter(|cell| cell.symbol() == "-")
                    .collect();
                assert_eq!(dash_cells.len(), 1, "{case}: {second}");
                assert_eq!(dash_cells[0].fg, theme::FG_DIM, "{case}");
            }

            // 기본 상세 높이에 새 세 줄이 보임 (130x40)
            let mut state = projects_state(lang);
            let text = screen_text(&draw(&mut state, 130, 40));
            for msg in [Msg::WaitAvgMax, Msg::WaitTotal, Msg::DetailTokensPerReq] {
                assert!(
                    text.contains(i18n::text(lang, msg)),
                    "{lang:?} {msg:?}\n{text}"
                );
            }
            assert!(text.contains("7.8M / 8.7K"), "{lang:?}\n{text}");
        }
    }

    #[test]
    fn project_detail_lists_models_agents_and_shortened_sessions_by_cost() {
        let state = sample_state_with_tables();
        let Panel::Ready(projects) = &state.snapshot.projects else {
            panic!("프로젝트 표본");
        };
        let (_, lines) =
            build_project_detail(Lang::En, Some(&projects[0]), WindowTotals::default());
        let text: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        for heading in ["Models by cost", "Agents by cost", "Sessions by cost"] {
            assert!(
                text.iter().any(|line| line == heading),
                "{heading}: {text:?}"
            );
        }
        // 세션 id 는 앞 여덟 글자만, 점유율은 프로젝트 비용 기준
        let session = text
            .iter()
            .find(|line| line.starts_with("  0f9e8d7c "))
            .expect("세션 줄");
        assert!(session.ends_with("500 req · $5.00 · 60%"), "{session}");
        assert!(
            !text.iter().any(|line| line.contains("0f9e8d7c-")),
            "{text:?}"
        );
    }

    #[test]
    fn long_project_and_attribution_lists_show_a_gray_scrollbar_and_a_position_counter() {
        let gray = [
            theme::SCROLL_TRACK,
            theme::SCROLL_TRACK_HOT,
            theme::SCROLL_THUMB,
            theme::SCROLL_THUMB_FOCUS,
            theme::SCROLL_THUMB_HOVER,
            theme::SCROLL_THUMB_DRAG,
        ];
        for lang in BOTH {
            for (width, height) in SIZES {
                let mut state = projects_state(lang);
                state.snapshot.projects = Panel::Ready(
                    (0..60)
                        .map(|index| ProjectStatsRow {
                            project: format!("proj{index:03}"),
                            requests: index + 1,
                            ..ProjectStatsRow::default()
                        })
                        .collect(),
                );
                state.snapshot.attribution = Panel::Ready(vec![(
                    AttributionDimension::Skill,
                    (0..60)
                        .map(|index| MetricAttributionRow {
                            name: format!("skill{index:03}"),
                            cost_usd: f64::from(index),
                            ..MetricAttributionRow::default()
                        })
                        .collect(),
                )]);
                let buffer = draw(&mut state, width, height);
                for card in PROJECT_CARDS {
                    let case = format!("{lang:?} {card:?} {width}x{height}");
                    let view = state.card_views[card as usize];
                    let rect = card_rect(&state, card);
                    assert!(view.total > view.visible, "{case}");
                    if view.visible == 1 {
                        let thumb = buffer.cell((rect.right() - 2, rect.bottom() - 2)).unwrap();
                        assert_eq!(thumb.symbol(), "█", "{case}");
                        assert!(gray.contains(&thumb.fg), "{case}");
                    } else {
                        let hit = state
                            .hit_areas
                            .scroll_hits
                            .iter()
                            .find(|hit| hit.card == card)
                            .unwrap_or_else(|| panic!("{case}: 스크롤바 없음"));
                        assert_eq!(hit.track.x, rect.right() - 2, "{case}");
                        let mut thumb_cells = 0;
                        for y in hit.track.y..hit.track.bottom() {
                            let cell = buffer.cell((hit.track.x, y)).unwrap();
                            assert!(["░", "█"].contains(&cell.symbol()), "{case} y{y}");
                            assert!(gray.contains(&cell.fg), "{case} y{y}");
                            thumb_cells += usize::from(cell.symbol() == "█");
                        }
                        assert!(thumb_cells >= 1, "{case}");
                    }
                    let counter = row_text(&buffer, rect.bottom() - 1);
                    assert!(
                        counter.contains(&format!("/ {} ", view.total)),
                        "{case}: {counter}"
                    );
                }
            }
        }
    }

    #[test]
    fn row_clicks_and_arrow_keys_select_projects_and_the_detail_card_follows() {
        let mut state = projects_state(Lang::En);
        draw(&mut state, 130, 40);
        let hit = state
            .hit_areas
            .row_hits
            .iter()
            .find(|hit| hit.card == CardId::ProjectTable)
            .copied()
            .unwrap();
        assert_eq!(hit.len, 3);
        click(&mut state, hit.rect.x + 2, hit.rect.y + 1);
        assert_eq!(state.card_views[CardId::ProjectTable as usize].selected, 1);
        let text = screen_text(&draw(&mut state, 130, 40));
        assert!(text.contains("Project detail · claude-monitor · feature/projects-tab"));

        // 클릭한 표가 키 입력 대상: 아래 화살표는 다음 행, 범위 밖으로 나가지 않음
        state.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        state.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state.card_views[CardId::ProjectTable as usize].selected, 2);
        let text = screen_text(&draw(&mut state, 130, 40));
        assert!(text.contains("Project detail · rust-fs-mcp"));
        assert!(!text.contains("Project detail · rust-fs-mcp ·"));
    }

    #[test]
    fn attribution_rows_pick_the_requested_dimension_only_from_a_ready_panel() {
        let mut state = projects_state(Lang::En);
        let skills = attribution_rows(&state.snapshot, AttributionDimension::Skill).unwrap();
        assert_eq!(skills[0].name, "frontend-tui");
        assert!(attribution_rows(&state.snapshot, AttributionDimension::QuerySource).is_none());
        state.snapshot.attribution = Panel::Pending;
        assert!(attribution_rows(&state.snapshot, AttributionDimension::Skill).is_none());
    }
}
