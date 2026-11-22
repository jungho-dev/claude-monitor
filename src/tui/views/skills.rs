//! tui/views/skills.rs
//! 스킬 탭: 설치 목록·Skill 도구 호출·OTel 비용을 이름으로 합친 스킬 표와 선택 스킬 상세 카드.

use crate::inventory::Inventory;
use crate::store::metrics::AttributionDimension;
use crate::store::query::UsageShare;
use crate::tui::cards::CardId;
use crate::tui::format::{fit_display, format_clock_time, format_compact, format_count};
use crate::tui::i18n::{self, Lang, Msg};
use crate::tui::layout::{SplitAxis, SplitGroup};
use crate::tui::sort::{compare_skill_row, sort_rows};
use crate::tui::state::{AppState, DashboardSnapshot, Panel};
use crate::tui::theme;
use crate::tui::views::common::{
    CardTable, ColumnFit, ColumnSpec, PanelNote, TableCell, TextCard, panel_message,
    render_card_table, render_db_gate, render_notice_card, render_text_card,
};
use crate::tui::views::detail::{
    DETAIL_LABEL_WIDTH, ShareBasis, pair_line, percent, section_line, share_lines,
};
use crate::tui::views::projects::attribution_rows;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use std::collections::BTreeMap;

// 상세 카드 기본 높이 (항목 6줄 + 호출 에이전트·최근 호출 구역 + 테두리), 표는 머리글과 몇 행을 남김
const SKILL_DETAIL_HEIGHT: u16 = 16;
const SKILL_TABLE_MIN: u16 = 5;
const SKILL_DETAIL_MIN: u16 = 3;

// 스킬 표 열 (정렬 열 번호와 순서 일치: sort.rs)
pub const SKILL_COLUMNS: [ColumnSpec; 9] = [
    (Msg::ColSkill, 18, ColumnFit::Grow, Alignment::Left),
    (Msg::ColInstalled, 4, ColumnFit::Fixed, Alignment::Left),
    (Msg::ColCalls, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColErrors, 5, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColSessions, 5, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColTopCaller, 10, ColumnFit::Shrink, Alignment::Left),
    (Msg::ColCost, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColDescChars, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColLast, 8, ColumnFit::Fixed, Alignment::Right),
];

// 1. 스킬 행 ------------------------------------------------------------------
// installed·description_chars 가 None 이면 설치 목록을 아직 읽지 않음, cost_usd 가 None 이면 OTel 지표 없음
#[derive(Debug, Clone, Default)]
pub struct SkillRow {
    pub name: String,
    pub installed: Option<bool>,
    pub path: String,
    pub description_chars: Option<usize>,
    pub calls: u64,
    pub errors: u64,
    pub sessions: u64,
    pub top_caller: String,
    pub cost_usd: Option<f64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub first_ts: String,
    pub last_ts: String,
    pub callers: Vec<UsageShare>,
    pub recent: Vec<UsageShare>,
}

// 2. 세 출처 병합 -------------------------------------------------------------
// 호출 기록 행(호출 수 순) 뒤에 호출 없는 설치 스킬, 그 뒤에 OTel 에만 있는 이름 (호출 집계가 준비되지 않으면 None)
pub fn skill_rows(
    snapshot: &DashboardSnapshot,
    inventory: &Panel<Inventory>,
) -> Option<Vec<SkillRow>> {
    let Panel::Ready(stats) = &snapshot.skills else {
        return None;
    };
    let installed: Option<BTreeMap<&str, (&str, usize)>> = match inventory {
        Panel::Ready(inventory) => Some(
            inventory
                .skills
                .iter()
                .map(|skill| {
                    (
                        skill.name.as_str(),
                        (skill.path.as_str(), skill.description_chars),
                    )
                })
                .collect(),
        ),
        _ => None,
    };
    let costs = attribution_rows(snapshot, AttributionDimension::Skill).unwrap_or(&[]);

    // 1. 호출 기록 행
    let mut rows: Vec<SkillRow> = stats
        .iter()
        .map(|stat| SkillRow {
            name: stat.skill.clone(),
            calls: stat.calls,
            errors: stat.errors,
            sessions: stat.sessions,
            top_caller: stat
                .agents
                .first()
                .map(|caller| caller.name.clone())
                .unwrap_or_default(),
            first_ts: stat.first_ts.clone(),
            last_ts: stat.last_ts.clone(),
            callers: stat.agents.clone(),
            recent: stat.recent.clone(),
            ..SkillRow::default()
        })
        .collect();

    // 2. 호출 없는 설치 스킬 (이름 순)
    if let Some(installed) = &installed {
        for name in installed.keys() {
            if !rows.iter().any(|row| row.name == *name) {
                rows.push(SkillRow {
                    name: (*name).to_string(),
                    ..SkillRow::default()
                });
            }
        }
    }

    // 3. OTel 에만 있는 이름
    for cost in costs {
        if !rows.iter().any(|row| row.name == cost.name) {
            rows.push(SkillRow {
                name: cost.name.clone(),
                ..SkillRow::default()
            });
        }
    }

    // 4. 설치 정보와 비용 채움
    for row in &mut rows {
        if let Some(installed) = &installed {
            let found = installed.get(row.name.as_str());
            row.installed = Some(found.is_some());
            row.description_chars = found.map(|(_, chars)| *chars);
            row.path = found
                .map(|(path, _)| (*path).to_string())
                .unwrap_or_default();
        }
        if let Some(cost) = costs.iter().find(|cost| cost.name == row.name) {
            row.cost_usd = Some(cost.cost_usd);
            row.input_tokens = cost.input_tokens;
            row.output_tokens = cost.output_tokens;
        }
    }
    Some(rows)
}

// 3. 스킬 탭 ------------------------------------------------------------------
// 스킬 표와 선택 스킬 상세 (구간은 다른 탭과 같은 w 키, r 은 집계와 설치 목록을 함께 다시 읽음)
pub fn render_skills_tab(frame: &mut Frame, area: Rect, state: &mut AppState) {
    if render_db_gate(frame, state, area) {
        return;
    }
    let detail_height = SKILL_DETAIL_HEIGHT.min(area.height.saturating_sub(SKILL_TABLE_MIN));
    let panes = state.card_layout.split_cards(
        &mut state.hit_areas,
        SplitGroup::SkillsRows,
        SplitAxis::Rows,
        area,
        &[area.height.saturating_sub(detail_height)],
        &[SKILL_TABLE_MIN, SKILL_DETAIL_MIN],
    );
    let lang = state.lang;
    let inventory_ready = matches!(state.inventory, Panel::Ready(_));

    // 1. 스킬 표
    let rows = skill_rows(&state.snapshot, &state.inventory).map(|mut rows| {
        sort_rows(
            &mut rows,
            state.card_sorts[CardId::SkillTable as usize],
            compare_skill_row,
        );
        rows
    });
    let total_calls: u64 = rows
        .as_ref()
        .map_or(0, |rows| rows.iter().map(|row| row.calls).sum());
    match &rows {
        Some(sorted) if !sorted.is_empty() => {
            let installed = match &state.inventory {
                Panel::Ready(inventory) => Some(inventory.skills.len()),
                _ => None,
            };
            let used = sorted.iter().filter(|row| row.calls > 0).count();
            let table_rows: Vec<Vec<TableCell>> =
                sorted.iter().map(|row| skill_cells(lang, row)).collect();
            render_card_table(
                frame,
                state,
                panes[0],
                CardTable {
                    card: CardId::SkillTable,
                    title: i18n::skill_table_title(lang, installed, used, total_calls),
                    hint: i18n::text(lang, Msg::HintSkillSource).to_string(),
                    columns: SKILL_COLUMNS.to_vec(),
                    rows: table_rows,
                    aux_lines: Vec::new(),
                },
            );
        }
        Some(_) => {
            let note = PanelNote {
                message: i18n::text(lang, Msg::SkillsEmpty).to_string(),
                color: theme::FG_DIM,
                hint: Some(Msg::SkillsEmptyHint),
            };
            let title = i18n::text(lang, Msg::TitleSkillsPending);
            render_notice_card(frame, state, CardId::SkillTable, panes[0], title, note);
        }
        None => {
            let note = panel_message(lang, &state.snapshot.skills).unwrap_or_default();
            let title = i18n::text(lang, Msg::TitleSkillsPending);
            render_notice_card(frame, state, CardId::SkillTable, panes[0], title, note);
        }
    }

    // 2. 선택 스킬 상세
    let selected = state.card_views[CardId::SkillTable as usize].selected;
    let selected_row = rows.as_ref().and_then(|sorted| sorted.get(selected));
    let (title, lines) = build_skill_detail(lang, selected_row, total_calls, inventory_ready);
    render_text_card(
        frame,
        state,
        panes[1],
        TextCard {
            card: CardId::SkillDetail,
            title,
            hint: String::new(),
            lines,
        },
    );
}

// 3-1. 표 한 행 ---------------------------------------------------------------
fn skill_cells(lang: Lang, row: &SkillRow) -> Vec<TableCell> {
    let count = |value: u64| format_count(i64::try_from(value).unwrap_or(i64::MAX));
    let dim_when_zero = |value: u64| {
        if value == 0 {
            theme::STYLE_DIM
        } else {
            theme::STYLE_TEXT
        }
    };
    let dash = || vec![Span::styled("-", theme::STYLE_DIM)];
    let name = if row.name.is_empty() {
        vec![Span::styled(
            i18n::text(lang, Msg::SkillUnnamed).to_string(),
            theme::STYLE_DIM,
        )]
    } else {
        vec![Span::raw(row.name.clone())]
    };
    let installed = match row.installed {
        Some(true) => vec![Span::styled("●", theme::STYLE_OK)],
        Some(false) => vec![Span::styled("○", theme::STYLE_DIM)],
        None => vec![Span::styled("?", theme::STYLE_DIM)],
    };
    let error_style = if row.errors > 0 {
        theme::STYLE_CRIT
    } else {
        theme::STYLE_DIM
    };
    vec![
        name,
        installed,
        vec![Span::styled(count(row.calls), dim_when_zero(row.calls))],
        vec![Span::styled(count(row.errors), error_style)],
        vec![Span::styled(
            count(row.sessions),
            dim_when_zero(row.sessions),
        )],
        if row.top_caller.is_empty() {
            dash()
        } else {
            vec![Span::styled(row.top_caller.clone(), theme::STYLE_DIM)]
        },
        match row.cost_usd {
            Some(cost) => vec![Span::raw(format!("${cost:.2}"))],
            None => dash(),
        },
        match row.description_chars {
            Some(chars) => vec![Span::styled(
                format_count(i64::try_from(chars).unwrap_or(i64::MAX)),
                theme::STYLE_DIM,
            )],
            None => dash(),
        },
        if row.last_ts.is_empty() {
            dash()
        } else {
            vec![Span::styled(
                format_clock_time(&row.last_ts),
                theme::STYLE_DIM,
            )]
        },
    ]
}

// 4. 스킬 상세 ----------------------------------------------------------------
// 호출·점유·처음과 마지막 호출·설치 위치·설명 길이·OTel 비용, 이어서 호출 에이전트와 최근 호출 구역
fn build_skill_detail(
    lang: Lang,
    row: Option<&SkillRow>,
    total_calls: u64,
    inventory_ready: bool,
) -> (String, Vec<Line<'static>>) {
    let base = i18n::text(lang, Msg::TitleSkillDetail);
    let Some(row) = row else {
        return (
            base.to_string(),
            vec![Line::from(Span::styled(
                i18n::text(lang, Msg::NoSkillSelected),
                theme::STYLE_DIM,
            ))],
        );
    };
    let count = |value: u64| format_count(i64::try_from(value).unwrap_or(i64::MAX));

    // 1. 호출 요약 (호출이 없으면 한 줄로)
    let mut lines = Vec::new();
    if row.calls > 0 {
        lines.push(pair_line(
            i18n::text(lang, Msg::SkillCalls),
            i18n::skill_calls_value(
                lang,
                &count(row.calls),
                &count(row.errors),
                &count(row.sessions),
                percent(row.errors as f64, row.calls as f64),
            ),
        ));
        lines.push(pair_line(
            i18n::text(lang, Msg::SkillShare),
            format!("{:.1}%", percent(row.calls as f64, total_calls as f64)),
        ));
        lines.push(pair_line(
            i18n::text(lang, Msg::SkillFirstLast),
            format!(
                "{} / {}",
                format_clock_time(&row.first_ts),
                format_clock_time(&row.last_ts)
            ),
        ));
    } else {
        lines.push(pair_line(
            i18n::text(lang, Msg::SkillCalls),
            i18n::text(lang, Msg::SkillNeverCalled).to_string(),
        ));
    }

    // 2. 설치 위치·설명 길이 (설치 목록을 읽기 전이면 읽는 중)
    let (location, description) = match (inventory_ready, row.installed) {
        (false, _) | (true, None) => {
            let pending = i18n::text(lang, Msg::SkillInventoryPending).to_string();
            (pending, "-".to_string())
        }
        (true, Some(true)) => (
            row.path.clone(),
            row.description_chars.map_or("-".to_string(), |chars| {
                i18n::chars_value(
                    lang,
                    &format_count(i64::try_from(chars).unwrap_or(i64::MAX)),
                )
            }),
        ),
        (true, Some(false)) => (
            i18n::text(lang, Msg::SkillNotInstalled).to_string(),
            "-".to_string(),
        ),
    };
    lines.push(pair_line(i18n::text(lang, Msg::SkillInstalled), location));
    lines.push(pair_line(
        i18n::text(lang, Msg::SkillDescription),
        description,
    ));

    // 3. OTel 비용
    let cost = match row.cost_usd {
        Some(cost) => i18n::otel_cost_value(
            lang,
            cost,
            &format_compact(row.input_tokens),
            &format_compact(row.output_tokens),
        ),
        None => i18n::text(lang, Msg::SkillNoOtel).to_string(),
    };
    lines.push(pair_line(i18n::text(lang, Msg::SkillCostOtel), cost));

    // 4. 호출 에이전트와 최근 호출
    lines.extend(share_lines(
        lang,
        Msg::SectionCallers,
        &row.callers,
        ShareBasis::Calls,
        row.calls as f64,
    ));
    if !row.recent.is_empty() {
        lines.push(section_line(lang, Msg::SectionRecentCalls));
        for call in &row.recent {
            let mut spans = vec![
                Span::styled(
                    fit_display(
                        &format!("  {}", format_clock_time(&call.last_ts)),
                        DETAIL_LABEL_WIDTH,
                    ),
                    theme::STYLE_TEXT,
                ),
                Span::styled(
                    if call.name.is_empty() {
                        "-".to_string()
                    } else {
                        call.name.clone()
                    },
                    theme::STYLE_TEXT,
                ),
            ];
            if call.errors > 0 {
                spans.push(Span::styled(
                    format!(" · {}", i18n::text(lang, Msg::SkillCallError)),
                    theme::STYLE_CRIT,
                ));
            }
            lines.push(Line::from(spans));
        }
    }

    let name = if row.name.is_empty() {
        i18n::text(lang, Msg::SkillUnnamed)
    } else {
        row.name.as_str()
    };
    (format!("{base} · {name}"), lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::query::SkillStatsRow;
    use crate::tui::cards::HeaderHit;
    use crate::tui::sort::{SortChange, SortState, sort_spec};
    use crate::tui::state::{ActiveTab, UiCommand};
    use crate::tui::test_support::sample_state_with_tables;
    use crate::tui::views::render_frame;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use std::time::Instant;

    const SIZES: [(u16, u16); 3] = [(60, 16), (100, 30), (130, 40)];
    const BOTH: [Lang; 2] = [Lang::En, Lang::Ko];

    fn skills_state(lang: Lang) -> AppState {
        let mut state = sample_state_with_tables();
        state.lang = lang;
        state.tab = ActiveTab::Skills;
        state
    }

    fn draw(state: &mut AppState, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_frame(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

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

    fn click(state: &mut AppState, column: u16, row: u16) -> UiCommand {
        state.handle_mouse_event_at(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
            Instant::now(),
        )
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

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn rows_merge_calls_installed_skills_and_otel_names_once_each() {
        let state = sample_state_with_tables();
        let rows = skill_rows(&state.snapshot, &state.inventory).expect("행");
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        // 호출 기록 순 다음 호출 없는 설치 스킬, 그다음 OTel 에만 있는 이름
        assert_eq!(
            names,
            [
                "frontend-tui",
                "core-response",
                "utils-table",
                "languages-rust"
            ]
        );
        let tui = &rows[0];
        assert_eq!(tui.installed, Some(true));
        assert_eq!(tui.description_chars, Some(180));
        assert_eq!(tui.cost_usd, Some(1.25));
        assert_eq!(tui.top_caller, "main");
        let table = &rows[2];
        assert_eq!((table.calls, table.installed), (0, Some(true)));
        assert_eq!(table.cost_usd, None);
        let rust = &rows[3];
        assert_eq!(rust.installed, Some(false));
        assert_eq!(rust.cost_usd, Some(0.75));

        // 설치 목록을 읽기 전이면 설치 여부를 모름, 호출 집계 전이면 행 없음
        let mut pending = sample_state_with_tables();
        pending.inventory = Panel::Pending;
        let rows = skill_rows(&pending.snapshot, &pending.inventory).expect("행");
        assert!(rows.iter().all(|row| row.installed.is_none()));
        assert_eq!(rows.len(), 3);
        pending.snapshot.skills = Panel::Pending;
        assert!(skill_rows(&pending.snapshot, &pending.inventory).is_none());
    }

    #[test]
    fn the_skills_tab_draws_both_cards_in_both_languages_at_the_size_matrix() {
        for lang in BOTH {
            for (width, height) in SIZES {
                let case = format!("{lang:?} {width}x{height}");
                let mut state = skills_state(lang);
                let buffer = draw(&mut state, width, height);
                let text = screen_text(&buffer);
                let frame = Rect::new(0, 0, width, height);
                let table = card_rect(&state, CardId::SkillTable);
                let detail = card_rect(&state, CardId::SkillDetail);
                assert_eq!(table.intersection(frame), table, "{case}");
                assert_eq!(detail.intersection(frame), detail, "{case}");
                assert!(!table.intersects(detail), "{case}");
                assert_eq!(state.hit_areas.tab_rects.len(), 9, "{case}");
                let title = match lang {
                    Lang::En => "Skills · 3 installed · 2 used · 9 calls",
                    Lang::Ko => "스킬 · 설치 3개 · 사용 2개 · 호출 9회",
                };
                if width >= 100 {
                    assert!(text.contains(title), "{case}\n{text}");
                    assert!(text.contains("frontend-tui"), "{case}");
                    assert!(text.contains("utils-table"), "{case}");
                    assert!(text.contains("$1.25"), "{case}");
                }
                if height >= 30 {
                    // 호출 수 내림차순 첫 행이 선택되고 상세가 따라감
                    let detail_title = match lang {
                        Lang::En => "Skill detail · frontend-tui",
                        Lang::Ko => "스킬 상세 · frontend-tui",
                    };
                    assert!(text.contains(detail_title), "{case}\n{text}");
                    assert!(text.contains("skills/frontend-tui/SKILL.md"), "{case}");
                }
            }
        }
    }

    #[test]
    fn the_9_key_opens_the_tab_and_requests_the_skill_files_once() {
        let mut state = sample_state_with_tables();
        let command = state.handle_key_event(KeyEvent::new(KeyCode::Char('9'), KeyModifiers::NONE));
        assert_eq!(state.tab, ActiveTab::Skills);
        assert_eq!(command, UiCommand::LoadSettings);
        let again = state.handle_key_event(KeyEvent::new(KeyCode::Char('9'), KeyModifiers::NONE));
        assert_eq!(again, UiCommand::None);
        // r 은 집계와 설치 목록을 함께 다시 읽음
        let refresh = state.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(refresh, UiCommand::RefreshWithSettings);
    }

    #[test]
    fn header_clicks_sort_and_the_detail_card_stays_on_the_selected_skill() {
        let spec = sort_spec(CardId::SkillTable).unwrap();
        assert_eq!(spec.kinds.len(), SKILL_COLUMNS.len());
        let mut state = skills_state(Lang::En);
        draw(&mut state, 130, 40);
        let hits: Vec<HeaderHit> = state
            .hit_areas
            .header_cells
            .iter()
            .filter(|hit| hit.card == CardId::SkillTable)
            .copied()
            .collect();
        assert_eq!(hits.len(), SKILL_COLUMNS.len());

        // 이름 열 클릭: 문자 열은 오름차순부터
        let name_hit = hits.iter().find(|hit| hit.column == 0).unwrap();
        click(&mut state, name_hit.rect.x + 1, name_hit.rect.y);
        assert_eq!(
            state.card_sorts[CardId::SkillTable as usize],
            SortState {
                column: 0,
                descending: false
            }
        );
        let text = screen_text(&draw(&mut state, 130, 40));
        let core_at = text.find("core-response").unwrap();
        let tui_at = text.find("frontend-tui").unwrap();
        assert!(core_at < tui_at, "{text}");

        // 두 번째 행(frontend-tui)을 고른 뒤 호출 열로 정렬해도 같은 스킬을 따라감
        state.card_views[CardId::SkillTable as usize].selected = 1;
        let text = screen_text(&draw(&mut state, 130, 40));
        assert!(text.contains("Skill detail · frontend-tui"), "{text}");
        state.change_card_sort(CardId::SkillTable, SortChange::SelectColumn(2));
        let text = screen_text(&draw(&mut state, 130, 40));
        assert!(text.contains("Skill detail · frontend-tui"), "{text}");
    }

    #[test]
    fn detail_lists_calls_location_cost_callers_and_recent_calls() {
        let state = sample_state_with_tables();
        let rows = skill_rows(&state.snapshot, &state.inventory).unwrap();
        for lang in BOTH {
            let (title, lines) = build_skill_detail(lang, Some(&rows[0]), 9, true);
            let text: Vec<String> = lines.iter().map(line_text).collect();
            assert!(title.ends_with("· frontend-tui"), "{title}");
            assert!(
                text.iter().any(|line| line.contains("6 / 1 / 2")),
                "{text:?}"
            );
            assert!(text.iter().any(|line| line.contains("66.7%")), "{text:?}");
            assert!(
                text.iter().any(|line| line.contains("SKILL.md")),
                "{text:?}"
            );
            assert!(text.iter().any(|line| line.contains("$1.25")), "{text:?}");
            let callers = i18n::text(lang, Msg::SectionCallers);
            let recent = i18n::text(lang, Msg::SectionRecentCalls);
            assert!(text.iter().any(|line| line == callers), "{text:?}");
            assert!(text.iter().any(|line| line == recent), "{text:?}");
            let error_word = i18n::text(lang, Msg::SkillCallError);
            assert!(
                text.iter().any(|line| line.ends_with(error_word)),
                "{text:?}"
            );
        }

        // 호출 없는 설치 스킬, OTel 에만 있는 스킬, 설치 목록 읽기 전
        let (_, idle) = build_skill_detail(Lang::En, Some(&rows[2]), 9, true);
        let idle: Vec<String> = idle.iter().map(line_text).collect();
        assert!(
            idle.iter()
                .any(|line| line.contains("not called in this window"))
        );
        assert!(idle.iter().any(|line| line.contains("no OTel metric")));
        let (_, otel_only) = build_skill_detail(Lang::Ko, Some(&rows[3]), 9, true);
        let otel_only: Vec<String> = otel_only.iter().map(line_text).collect();
        assert!(
            otel_only.iter().any(|line| line.contains("호출 기록만")),
            "{otel_only:?}"
        );
        let (_, pending) = build_skill_detail(Lang::En, Some(&rows[0]), 9, false);
        let pending: Vec<String> = pending.iter().map(line_text).collect();
        assert!(
            pending
                .iter()
                .any(|line| line.contains("reading skill files"))
        );
        let (empty_title, empty) = build_skill_detail(Lang::Ko, None, 0, true);
        assert_eq!(empty_title, "스킬 상세");
        assert_eq!(empty.len(), 1);
    }

    #[test]
    fn empty_pending_and_unnamed_states_explain_themselves_in_both_languages() {
        for lang in BOTH {
            // 설치·호출 모두 없으면 안내와 다음 단계
            let mut none = skills_state(lang);
            none.snapshot.skills = Panel::Ready(Vec::new());
            if let Panel::Ready(inventory) = &mut none.inventory {
                inventory.skills.clear();
            }
            none.snapshot.attribution = Panel::Ready(Vec::new());
            let text = screen_text(&draw(&mut none, 130, 40));
            assert!(text.contains(i18n::text(lang, Msg::SkillsEmpty)), "{text}");
            assert!(
                text.contains(i18n::text(lang, Msg::SkillsEmptyHint)),
                "{text}"
            );

            // 집계 전에는 대기 안내, 상세는 선택 없음
            let mut pending = skills_state(lang);
            pending.snapshot.skills = Panel::Pending;
            let text = screen_text(&draw(&mut pending, 130, 40));
            assert!(text.contains(i18n::text(lang, Msg::PanelPending)), "{text}");
            assert!(
                text.contains(i18n::text(lang, Msg::NoSkillSelected)),
                "{text}"
            );

            // 백필 전 이름 없는 호출은 이름 미기록으로
            let mut unnamed = skills_state(lang);
            unnamed.snapshot.skills = Panel::Ready(vec![SkillStatsRow {
                calls: 2,
                ..SkillStatsRow::default()
            }]);
            let text = screen_text(&draw(&mut unnamed, 130, 40));
            assert!(text.contains(i18n::text(lang, Msg::SkillUnnamed)), "{text}");
        }
    }
}
