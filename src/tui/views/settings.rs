//! tui/views/settings.rs
//! 설정 탭: 훅·MCP·에이전트·진단 표 카드 (카드마다 독립 스크롤·정렬 머리글·열 경계 드래그, 표 아래 보조 줄 영역).

use crate::inventory::{DoctorFinding, Inventory};
use crate::tui::cards::CardId;
use crate::tui::format::{format_count, replace_control_chars};
use crate::tui::i18n::{self, Lang, Msg};
use crate::tui::layout::{SplitAxis, SplitGroup};
use crate::tui::sort::{
    SortState, compare_agent_entry, compare_doctor_finding, compare_hook_entry, compare_mcp_entry,
    sort_rows,
};
use crate::tui::state::{AppState, Panel};
use crate::tui::theme;
use crate::tui::views::common::{
    CardTable, ColumnFit, ColumnSpec, PanelNote, TableCell, panel_message, render_card_table,
    render_notice_card,
};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};

// 카드 테두리 2열과 안쪽 여백 2열
const CARD_CHROME_WIDTH: u16 = 4;
// 기본 배치에서 카드 하나가 갖는 최소 줄 수 (위아래 테두리 2줄과 본문 한 줄)
const MIN_CARD_ROWS: u16 = 3;

// 설정 표 열 (정렬 열 번호와 순서 일치: sort.rs)
pub const SETTINGS_HOOK_COLUMNS: [ColumnSpec; 6] = [
    (Msg::SetEvent, 12, ColumnFit::Shrink, Alignment::Left),
    (Msg::SetMatcher, 8, ColumnFit::Shrink, Alignment::Left),
    (Msg::SetType, 6, ColumnFit::Fixed, Alignment::Left),
    (Msg::SetTarget, 16, ColumnFit::Grow, Alignment::Left),
    (Msg::SetAsync, 5, ColumnFit::Fixed, Alignment::Left),
    (Msg::SetTimeout, 7, ColumnFit::Fixed, Alignment::Right),
];

pub const SETTINGS_MCP_COLUMNS: [ColumnSpec; 4] = [
    (Msg::SetStatus, 5, ColumnFit::Fixed, Alignment::Left),
    (Msg::SetName, 12, ColumnFit::Shrink, Alignment::Left),
    (Msg::SetTransport, 9, ColumnFit::Fixed, Alignment::Left),
    (Msg::SetTarget, 16, ColumnFit::Grow, Alignment::Left),
];

pub const SETTINGS_AGENT_COLUMNS: [ColumnSpec; 4] = [
    (Msg::SetName, 16, ColumnFit::Grow, Alignment::Left),
    (Msg::SetModel, 8, ColumnFit::Shrink, Alignment::Left),
    (Msg::SetEffort, 6, ColumnFit::Fixed, Alignment::Left),
    (Msg::SetMaxTurns, 5, ColumnFit::Fixed, Alignment::Right),
];

pub const SETTINGS_DOCTOR_COLUMNS: [ColumnSpec; 3] = [
    (Msg::SetLevel, 6, ColumnFit::Fixed, Alignment::Left),
    (Msg::SetTopic, 12, ColumnFit::Shrink, Alignment::Left),
    (Msg::SetMessage, 16, ColumnFit::Grow, Alignment::Left),
];

// 1. 카드 묶음 ----------------------------------------------------------------
// 표 영역의 정렬을 마친 행과 표 아래 보조 줄 영역 (보조 줄은 정렬·열 폭에 속하지 않음)
#[derive(Default)]
pub struct SettingsTable {
    pub rows: Vec<Vec<TableCell>>,
    pub aux_lines: Vec<Line<'static>>,
}

// 설정 카드 그리기 입력 (table 이 None 이면 note 로 안내 카드)
struct SettingsCard {
    card: CardId,
    title: String,
    hint: String,
    columns: Vec<ColumnSpec>,
    table: Option<SettingsTable>,
    note: Option<PanelNote>,
}

// 2. 설정 탭 ------------------------------------------------------------------
pub fn render_settings_tab(frame: &mut Frame, area: Rect, state: &mut AppState) {
    // 1. 카드 분할 (훅 30%, MCP 20%, 에이전트 20%, 진단 30%), 낮은 화면에서도 카드마다 적어도 3줄(테두리 2줄과 본문 한 줄)
    let height = u32::from(area.height);
    let first_cut = ((height * 30 / 100) as u16).max(MIN_CARD_ROWS);
    let second_cut = ((height * 50 / 100) as u16).max(first_cut + MIN_CARD_ROWS);
    let third_cut = ((height * 70 / 100) as u16)
        .max(second_cut + MIN_CARD_ROWS)
        .min(area.height.saturating_sub(MIN_CARD_ROWS));
    let panes = state.card_layout.split_cards(
        &mut state.hit_areas,
        SplitGroup::SettingsRows,
        SplitAxis::Rows,
        area,
        &[first_cut, second_cut, third_cut],
        &[4, 4, 4, 4],
    );
    let agent_width = usize::from(panes[2].width.saturating_sub(CARD_CHROME_WIDTH));

    // 2. 카드 내용 (적재 전에는 안내 카드)
    let lang = state.lang;
    let hook_sort = state.card_sorts[CardId::SettingsHooks as usize];
    let mcp_sort = state.card_sorts[CardId::SettingsMcp as usize];
    let agent_sort = state.card_sorts[CardId::SettingsAgents as usize];
    let doctor_sort = state.card_sorts[CardId::SettingsDoctor as usize];
    let (hook_table, mcp_table, agent_table) = match &state.inventory {
        Panel::Ready(loaded) => (
            Some(build_hook_table(lang, loaded, hook_sort)),
            Some(build_mcp_table(lang, loaded, mcp_sort)),
            Some(build_agent_table(lang, loaded, agent_width, agent_sort)),
        ),
        _ => (None, None, None),
    };
    let doctor_table = match &state.doctor {
        Panel::Ready(findings) => Some(build_doctor_table(lang, findings, doctor_sort)),
        _ => None,
    };
    let (hook_count, mcp_count, agent_count) = match &state.inventory {
        Panel::Ready(loaded) => (
            i18n::settings_count(lang, loaded.hooks.len()),
            i18n::settings_count(lang, loaded.mcp_servers.len()),
            i18n::settings_count(lang, loaded.agents.len()),
        ),
        _ => (String::new(), String::new(), String::new()),
    };
    let doctor_summary = match &state.doctor {
        Panel::Ready(findings) => {
            let failed = findings
                .iter()
                .filter(|finding| finding.level == "fail")
                .count();
            let warned = findings
                .iter()
                .filter(|finding| finding.level == "warn")
                .count();
            i18n::doctor_summary(lang, failed, warned)
        }
        _ => String::new(),
    };
    let inventory_note = panel_message(lang, &state.inventory);
    let doctor_note = panel_message(lang, &state.doctor);
    let claude_home = state.config.claude_home.display().to_string();

    // 3. 카드 그리기
    render_settings_card(
        frame,
        state,
        panes[0],
        SettingsCard {
            card: CardId::SettingsHooks,
            title: format!(
                "{}{hook_count}",
                i18n::text(lang, Msg::TitleHooksRegistered)
            ),
            hint: claude_home,
            columns: SETTINGS_HOOK_COLUMNS.to_vec(),
            table: hook_table,
            note: inventory_note.clone(),
        },
    );
    render_settings_card(
        frame,
        state,
        panes[1],
        SettingsCard {
            card: CardId::SettingsMcp,
            title: format!("{}{mcp_count}", i18n::text(lang, Msg::TitleMcpServers)),
            hint: String::new(),
            columns: SETTINGS_MCP_COLUMNS.to_vec(),
            table: mcp_table,
            note: inventory_note.clone(),
        },
    );
    render_settings_card(
        frame,
        state,
        panes[2],
        SettingsCard {
            card: CardId::SettingsAgents,
            title: format!("{}{agent_count}", i18n::text(lang, Msg::TitleAgentsSkills)),
            hint: String::new(),
            columns: SETTINGS_AGENT_COLUMNS.to_vec(),
            table: agent_table,
            note: inventory_note,
        },
    );
    render_settings_card(
        frame,
        state,
        panes[3],
        SettingsCard {
            card: CardId::SettingsDoctor,
            title: format!(
                "{}{doctor_summary}",
                i18n::text(lang, Msg::TitleDiagnostics)
            ),
            hint: i18n::text(lang, Msg::HintRefresh).to_string(),
            columns: SETTINGS_DOCTOR_COLUMNS.to_vec(),
            table: doctor_table,
            note: doctor_note,
        },
    );
}

// 3. 설정 카드 그리기 ---------------------------------------------------------
// 표가 준비되면 표 카드(머리글 정렬·열 경계 드래그·스크롤바 공용), 아니면 상태 문구 안내 카드
fn render_settings_card(frame: &mut Frame, state: &mut AppState, pane: Rect, spec: SettingsCard) {
    let SettingsCard {
        card,
        title,
        hint,
        columns,
        table,
        note,
    } = spec;
    match table {
        Some(built) => render_card_table(
            frame,
            state,
            pane,
            CardTable {
                card,
                title,
                hint,
                columns,
                rows: built.rows,
                aux_lines: built.aux_lines,
            },
        ),
        None => render_notice_card(frame, state, card, pane, &title, note.unwrap_or_default()),
    }
}

// 4. 훅 등록 표 ---------------------------------------------------------------
// 정렬한 훅 한 행씩, 표 아래 HTTP 훅 허용 목록
pub fn build_hook_table(lang: Lang, loaded: &Inventory, sort: SortState) -> SettingsTable {
    let mut sorted = loaded.hooks.clone();
    sort_rows(&mut sorted, sort, compare_hook_entry);
    let rows: Vec<Vec<TableCell>> = sorted
        .iter()
        .map(|hook| {
            let timeout = hook
                .timeout_secs
                .map_or("-".to_string(), |secs| format!("{secs}s"));
            let async_text = if hook.is_async {
                i18n::text(lang, Msg::SetYes)
            } else {
                "-"
            };
            vec![
                vec![Span::styled(
                    replace_control_chars(&hook.event),
                    theme::STYLE_TEXT,
                )],
                vec![Span::styled(
                    replace_control_chars(&hook.matcher),
                    theme::STYLE_TEXT,
                )],
                vec![Span::styled(
                    replace_control_chars(&hook.hook_type),
                    theme::STYLE_TEXT,
                )],
                vec![Span::styled(
                    replace_control_chars(&hook.target),
                    theme::STYLE_TEXT,
                )],
                vec![Span::styled(async_text, theme::STYLE_TEXT)],
                vec![Span::styled(timeout, theme::STYLE_TEXT)],
            ]
        })
        .collect();
    let mut aux_lines: Vec<Line<'static>> = Vec::new();
    if !loaded.http_hook_allowlist.is_empty() {
        aux_lines.push(Line::from(""));
        aux_lines.push(Line::from(Span::styled(
            i18n::allowlist_heading(lang, loaded.http_hook_allowlist.len()),
            theme::STYLE_ACCENT,
        )));
        for allowed in &loaded.http_hook_allowlist {
            aux_lines.push(Line::from(Span::styled(
                format!("  {}", replace_control_chars(allowed)),
                theme::STYLE_TEXT,
            )));
        }
    }
    SettingsTable { rows, aux_lines }
}

// 5. MCP 서버 표 --------------------------------------------------------------
// 정렬한 서버 한 행씩 (차단 서버는 흐리게), 표 아래 적재 경고
pub fn build_mcp_table(lang: Lang, loaded: &Inventory, sort: SortState) -> SettingsTable {
    let mut sorted = loaded.mcp_servers.clone();
    sort_rows(&mut sorted, sort, compare_mcp_entry);
    let rows: Vec<Vec<TableCell>> = sorted
        .iter()
        .map(|server| {
            let (mark, mark_style, body_style) = if server.denied {
                (
                    i18n::text(lang, Msg::SetBlocked),
                    theme::STYLE_CRIT,
                    theme::STYLE_DIM,
                )
            } else {
                (
                    i18n::text(lang, Msg::SetAllowed),
                    theme::STYLE_OK,
                    theme::STYLE_TEXT,
                )
            };
            vec![
                vec![Span::styled(mark, mark_style)],
                vec![Span::styled(
                    replace_control_chars(&server.name),
                    body_style,
                )],
                vec![Span::styled(
                    replace_control_chars(&server.transport),
                    body_style,
                )],
                vec![Span::styled(
                    replace_control_chars(&server.target),
                    body_style,
                )],
            ]
        })
        .collect();
    let aux_lines: Vec<Line<'static>> = loaded
        .warnings
        .iter()
        .map(|warning| {
            Line::from(Span::styled(
                format!("! {}", replace_control_chars(warning)),
                theme::STYLE_WARN,
            ))
        })
        .collect();
    SettingsTable { rows, aux_lines }
}

// 6. 에이전트·스킬·env 표 -----------------------------------------------------
// 정렬한 에이전트 한 행씩, 표 아래 스킬·env 요약 줄 (env 키는 width 에 맞춰 줄바꿈)
pub fn build_agent_table(
    lang: Lang,
    loaded: &Inventory,
    width: usize,
    sort: SortState,
) -> SettingsTable {
    // 1. 에이전트 행 (정렬)
    let mut sorted = loaded.agents.clone();
    sort_rows(&mut sorted, sort, compare_agent_entry);
    let rows: Vec<Vec<TableCell>> = sorted
        .iter()
        .map(|agent| {
            let max_turns = agent
                .max_turns
                .map_or("-".to_string(), |turns| turns.to_string());
            vec![
                vec![Span::styled(
                    replace_control_chars(&agent.name),
                    theme::STYLE_TEXT,
                )],
                vec![Span::styled(
                    replace_control_chars(&agent.model),
                    theme::STYLE_TEXT,
                )],
                vec![Span::styled(
                    replace_control_chars(&agent.effort),
                    theme::STYLE_TEXT,
                )],
                vec![Span::styled(max_turns, theme::STYLE_TEXT)],
            ]
        })
        .collect();

    // 2. 스킬 (개수와 설명 길이 합계)
    let mut aux_lines: Vec<Line<'static>> = Vec::new();
    let description_total: usize = loaded
        .skills
        .iter()
        .map(|skill| skill.description_chars)
        .sum();
    aux_lines.push(Line::from(vec![
        Span::styled(
            format!("{} ", i18n::text(lang, Msg::SettingsSkills)),
            theme::STYLE_ACCENT,
        ),
        Span::styled(
            i18n::skills_summary(
                lang,
                loaded.skills.len(),
                &format_count(description_total as i64),
            ),
            theme::STYLE_TEXT,
        ),
    ]));

    // 3. env 키 (폭에 맞춰 줄바꿈)
    aux_lines.push(Line::from(Span::styled(
        i18n::env_keys_heading(lang, loaded.env_keys.len()),
        theme::STYLE_ACCENT,
    )));
    let wrap_width = width.saturating_sub(2).max(20);
    let mut current = String::new();
    for key in &loaded.env_keys {
        let key_width = Span::raw(key.as_str()).width();
        if !current.is_empty() && Span::raw(current.as_str()).width() + 2 + key_width > wrap_width {
            aux_lines.push(Line::from(Span::styled(
                format!("  {current}"),
                theme::STYLE_TEXT,
            )));
            current.clear();
        }
        if !current.is_empty() {
            current.push_str(", ");
        }
        current.push_str(key);
    }
    if !current.is_empty() {
        aux_lines.push(Line::from(Span::styled(
            format!("  {current}"),
            theme::STYLE_TEXT,
        )));
    }
    SettingsTable { rows, aux_lines }
}

// 7. 진단 결과 표 -------------------------------------------------------------
// 수준별 색 (ok 녹색, warn 노랑, fail 적색), 해결 힌트는 내용 칸 뒤에 붙어 정렬해도 항목을 따라감
pub fn build_doctor_table(
    lang: Lang,
    findings: &[DoctorFinding],
    sort: SortState,
) -> SettingsTable {
    let mut sorted = findings.to_vec();
    sort_rows(&mut sorted, sort, compare_doctor_finding);
    let rows: Vec<Vec<TableCell>> = sorted
        .iter()
        .map(|finding| {
            let (tag, style) = match finding.level.as_str() {
                "ok" => (Msg::DoctorOk, theme::STYLE_OK),
                "warn" => (Msg::DoctorWarn, theme::STYLE_WARN),
                "fail" => (Msg::DoctorFail, theme::STYLE_CRIT),
                _ => (Msg::DoctorOther, theme::STYLE_DIM),
            };
            let mut message = vec![Span::styled(
                replace_control_chars(&finding.message),
                theme::STYLE_TEXT,
            )];
            if !finding.fix_hint.is_empty() {
                message.push(Span::styled(" → ", theme::STYLE_ACCENT));
                message.push(Span::styled(
                    replace_control_chars(&finding.fix_hint),
                    theme::STYLE_DIM,
                ));
            }
            vec![
                vec![Span::styled(i18n::text(lang, tag), style)],
                vec![Span::styled(
                    replace_control_chars(&finding.topic),
                    theme::STYLE_TEXT,
                )],
                message,
            ]
        })
        .collect();
    SettingsTable {
        rows,
        aux_lines: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::{AgentEntry, HookEntry, McpEntry, SkillEntry};
    use crate::tui::sort::sort_spec;

    const KO: Lang = Lang::Ko;

    fn cell_text(cell: &TableCell) -> String {
        cell.iter().map(|span| span.content.as_ref()).collect()
    }

    fn row_texts(table: &SettingsTable) -> Vec<Vec<String>> {
        table
            .rows
            .iter()
            .map(|row| row.iter().map(cell_text).collect())
            .collect()
    }

    fn aux_texts(table: &SettingsTable) -> Vec<String> {
        table
            .aux_lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    fn default_sort(card: CardId) -> SortState {
        sort_spec(card).unwrap().default
    }

    fn sample_inventory() -> Inventory {
        Inventory {
            hooks: vec![HookEntry {
                event: "PreToolUse".to_string(),
                matcher: "Bash".to_string(),
                hook_type: "http".to_string(),
                target: "http://127.0.0.1:47333/pretool".to_string(),
                is_async: false,
                timeout_secs: Some(5),
                ..HookEntry::default()
            }],
            mcp_servers: vec![McpEntry {
                name: "outline".to_string(),
                transport: "http".to_string(),
                target: "https://example.invalid".to_string(),
                denied: true,
            }],
            agents: vec![AgentEntry {
                name: "worker".to_string(),
                model: "sonnet".to_string(),
                effort: "high".to_string(),
                ..AgentEntry::default()
            }],
            skills: vec![
                SkillEntry {
                    description_chars: 120,
                    ..SkillEntry::default()
                },
                SkillEntry {
                    description_chars: 1_200,
                    ..SkillEntry::default()
                },
            ],
            env_keys: vec!["BASH_DEFAULT_TIMEOUT_MS".to_string(), "DEBUG".to_string()],
            ..Inventory::default()
        }
    }

    fn sample_findings() -> Vec<DoctorFinding> {
        vec![
            DoctorFinding {
                level: "ok".to_string(),
                topic: "토큰".to_string(),
                message: "정상".to_string(),
                fix_hint: String::new(),
            },
            DoctorFinding {
                level: "fail".to_string(),
                topic: "포트".to_string(),
                message: "47333 사용 중".to_string(),
                fix_hint: "다른 포트로 변경".to_string(),
            },
        ]
    }

    #[test]
    fn settings_columns_match_their_sort_specs_and_every_row_has_one_cell_per_column() {
        let inventory = sample_inventory();
        let findings = sample_findings();
        let cases: [(CardId, usize, SettingsTable); 4] = [
            (
                CardId::SettingsHooks,
                SETTINGS_HOOK_COLUMNS.len(),
                build_hook_table(KO, &inventory, default_sort(CardId::SettingsHooks)),
            ),
            (
                CardId::SettingsMcp,
                SETTINGS_MCP_COLUMNS.len(),
                build_mcp_table(KO, &inventory, default_sort(CardId::SettingsMcp)),
            ),
            (
                CardId::SettingsAgents,
                SETTINGS_AGENT_COLUMNS.len(),
                build_agent_table(KO, &inventory, 80, default_sort(CardId::SettingsAgents)),
            ),
            (
                CardId::SettingsDoctor,
                SETTINGS_DOCTOR_COLUMNS.len(),
                build_doctor_table(KO, &findings, default_sort(CardId::SettingsDoctor)),
            ),
        ];
        for (card, columns, table) in cases {
            assert_eq!(sort_spec(card).unwrap().kinds.len(), columns, "{card:?}");
            assert!(!table.rows.is_empty(), "{card:?}");
            for row in &table.rows {
                assert_eq!(row.len(), columns, "{card:?}");
            }
        }
    }

    #[test]
    fn inventory_tables_cover_hooks_mcp_agents_skills_and_env() {
        let inventory = sample_inventory();
        let hooks = build_hook_table(KO, &inventory, default_sort(CardId::SettingsHooks));
        assert_eq!(
            row_texts(&hooks),
            [[
                "PreToolUse",
                "Bash",
                "http",
                "http://127.0.0.1:47333/pretool",
                "-",
                "5s"
            ]]
        );
        assert!(hooks.aux_lines.is_empty());

        let mcp = build_mcp_table(KO, &inventory, default_sort(CardId::SettingsMcp));
        assert_eq!(
            row_texts(&mcp),
            [["차단", "outline", "http", "https://example.invalid"]]
        );
        assert_eq!(mcp.rows[0][0][0].style, theme::STYLE_CRIT);
        assert_eq!(mcp.rows[0][1][0].style, theme::STYLE_DIM);

        let agents = build_agent_table(KO, &inventory, 100, default_sort(CardId::SettingsAgents));
        assert_eq!(row_texts(&agents), [["worker", "sonnet", "high", "-"]]);
        let aux = aux_texts(&agents).join("\n");
        assert!(aux.contains("스킬 2개 · 설명 합계 1,320자"), "{aux}");
        assert!(aux.contains("BASH_DEFAULT_TIMEOUT_MS, DEBUG"), "{aux}");
    }

    #[test]
    fn doctor_rows_color_findings_and_keep_the_fix_hint_with_its_item() {
        let findings = sample_findings();
        let table = build_doctor_table(KO, &findings, default_sort(CardId::SettingsDoctor));
        // 기본 정렬은 심각한 수준이 먼저, 힌트는 해당 항목의 내용 칸 뒤
        assert_eq!(
            row_texts(&table),
            [
                ["[실패]", "포트", "47333 사용 중 → 다른 포트로 변경"],
                ["[정상]", "토큰", "정상"]
            ]
        );
        assert_eq!(table.rows[0][0][0].style, theme::STYLE_CRIT);
        assert_eq!(table.rows[1][0][0].style, theme::STYLE_OK);
        assert!(table.aux_lines.is_empty());

        // 주제 열 정렬을 바꿔도 힌트는 같은 항목을 따라감
        let ascending = SortState {
            column: 1,
            descending: false,
        };
        let table = build_doctor_table(KO, &findings, ascending);
        assert_eq!(row_texts(&table)[0][2], "정상");
        assert!(row_texts(&table)[1][2].ends_with("다른 포트로 변경"));
    }

    #[test]
    fn header_sort_changes_row_order() {
        let mut inventory = sample_inventory();
        inventory.hooks.push(HookEntry {
            event: "Notification".to_string(),
            timeout_secs: Some(30),
            ..HookEntry::default()
        });
        let first_event =
            |sort: SortState| row_texts(&build_hook_table(KO, &inventory, sort))[0][0].clone();
        let by_timeout = SortState {
            column: 5,
            descending: true,
        };
        assert_eq!(first_event(by_timeout), "Notification");
        let by_event = SortState {
            column: 0,
            descending: false,
        };
        assert_eq!(first_event(by_event), "Notification");
        let reversed = SortState {
            column: 0,
            descending: true,
        };
        assert_eq!(first_event(reversed), "PreToolUse");
    }

    #[test]
    fn aux_lines_hold_the_allowlist_warnings_and_wrapped_env_keys() {
        let mut inventory = sample_inventory();
        inventory.http_hook_allowlist = vec!["http://127.0.0.1:47333/*".to_string()];
        inventory.warnings = vec!["settings.json: skipped".to_string()];
        let hooks = build_hook_table(KO, &inventory, default_sort(CardId::SettingsHooks));
        let aux = aux_texts(&hooks);
        assert_eq!(aux.len(), 3);
        assert!(aux[0].is_empty());
        assert!(aux[2].contains("http://127.0.0.1:47333/*"), "{aux:?}");
        let mcp = build_mcp_table(KO, &inventory, default_sort(CardId::SettingsMcp));
        assert_eq!(aux_texts(&mcp), ["! settings.json: skipped"]);
        assert_eq!(mcp.aux_lines[0].spans[0].style, theme::STYLE_WARN);

        // 좁은 폭에서는 env 키가 여러 줄로 이어짐, 키는 중간에서 잘리지 않음
        let sort = default_sort(CardId::SettingsAgents);
        let narrow = aux_texts(&build_agent_table(KO, &inventory, 20, sort));
        let wide = aux_texts(&build_agent_table(KO, &inventory, 100, sort));
        assert!(narrow.len() > wide.len(), "{narrow:?} / {wide:?}");
        assert!(
            narrow
                .iter()
                .any(|line| line.contains("BASH_DEFAULT_TIMEOUT_MS"))
        );
    }

    #[test]
    fn values_from_settings_files_never_carry_control_characters_into_cells() {
        let mut inventory = sample_inventory();
        inventory.hooks[0].target = "a\nb\tc".to_string();
        inventory.warnings = vec!["x\ny".to_string()];
        let hooks = build_hook_table(KO, &inventory, default_sort(CardId::SettingsHooks));
        assert_eq!(row_texts(&hooks)[0][3], "a b c");
        let mcp = build_mcp_table(KO, &inventory, default_sort(CardId::SettingsMcp));
        assert_eq!(aux_texts(&mcp), ["! x y"]);
    }
}
