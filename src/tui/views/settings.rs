//! tui/views/settings.rs
//! 설정 탭: 훅·MCP·에이전트·진단 카드 (카드마다 독립 스크롤·정렬 머리글, 경계 드래그 크기 조절).

use crate::inventory::{DoctorFinding, Inventory};
use crate::tui::cards::{CardId, HeaderSpan};
use crate::tui::format::{fit_display, format_count, pad_left_display};
use crate::tui::layout::{SplitAxis, SplitGroup};
use crate::tui::sort::{
    SortState, compare_agent_entry, compare_doctor_finding, compare_hook_entry, compare_mcp_entry,
    sort_rows,
};
use crate::tui::state::{AppState, Panel};
use crate::tui::theme;
use crate::tui::views::common::{
    TextCard, build_uniform_sort_header, panel_message, render_text_card,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

const HOOK_EVENT_WIDTH: usize = 18;
const HOOK_MATCHER_WIDTH: usize = 18;
const HOOK_TYPE_WIDTH: usize = 8;
const HOOK_ASYNC_WIDTH: usize = 6;
const HOOK_TIMEOUT_WIDTH: usize = 7;
const NAME_WIDTH: usize = 24;
const SMALL_COLUMN_WIDTH: usize = 10;
const MCP_MARK_WIDTH: usize = 4;
const AGENT_TURNS_WIDTH: usize = 7;
const DOCTOR_TAG_WIDTH: usize = 6;
// 카드 테두리 2열과 안쪽 여백 2열
const CARD_CHROME_WIDTH: u16 = 4;

// 1. 카드 줄 묶음 -------------------------------------------------------------
// 고정 머리글 줄(없을 수 있음), 머리글 열 클릭 구간, 본문 줄
#[derive(Default)]
pub struct CardLines {
    pub header: Option<Line<'static>>,
    pub header_cells: Vec<HeaderSpan>,
    pub lines: Vec<Line<'static>>,
}

// 2. 설정 탭 ------------------------------------------------------------------
pub fn render_settings_tab(frame: &mut Frame, area: Rect, state: &mut AppState) {
    // 1. 카드 분할 (훅 30%, MCP 20%, 에이전트 20%, 진단 30%)
    let height = u32::from(area.height);
    let panes = state.card_layout.split_cards(
        &mut state.hit_areas.split_hits,
        SplitGroup::SettingsRows,
        SplitAxis::Rows,
        area,
        &[
            (height * 30 / 100) as u16,
            (height * 50 / 100) as u16,
            (height * 70 / 100) as u16,
        ],
        &[4, 4, 4, 4],
    );
    let widths: Vec<usize> = panes
        .iter()
        .map(|pane| pane.width.saturating_sub(CARD_CHROME_WIDTH) as usize)
        .collect();

    // 2. 카드 내용 (적재 전에는 상태 문구)
    let hook_card = build_hook_lines(
        &state.inventory,
        widths[0],
        state.card_sorts[CardId::SettingsHooks as usize],
    );
    let mcp_card = build_mcp_lines(
        &state.inventory,
        widths[1],
        state.card_sorts[CardId::SettingsMcp as usize],
    );
    let agent_card = build_agent_lines(
        &state.inventory,
        widths[2],
        state.card_sorts[CardId::SettingsAgents as usize],
    );
    let doctor_card = build_doctor_lines(
        &state.doctor,
        widths[3],
        state.card_sorts[CardId::SettingsDoctor as usize],
    );
    let hook_count = match &state.inventory {
        Panel::Ready(loaded) => format!(" · {}건", format_count(loaded.hooks.len() as i64)),
        _ => String::new(),
    };
    let mcp_count = match &state.inventory {
        Panel::Ready(loaded) => format!(" · {}건", loaded.mcp_servers.len()),
        _ => String::new(),
    };
    let agent_count = match &state.inventory {
        Panel::Ready(loaded) => format!(" · {}건", loaded.agents.len()),
        _ => String::new(),
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
            format!(" · 실패 {failed} · 경고 {warned}")
        }
        _ => String::new(),
    };
    let claude_home = state.config.claude_home.display().to_string();

    // 3. 카드 그리기
    render_text_card(
        frame,
        state,
        panes[0],
        TextCard {
            card: CardId::SettingsHooks,
            title: format!("훅 등록{hook_count}"),
            hint: claude_home,
            header: hook_card.header,
            header_cells: hook_card.header_cells,
            lines: hook_card.lines,
        },
    );
    render_text_card(
        frame,
        state,
        panes[1],
        TextCard {
            card: CardId::SettingsMcp,
            title: format!("MCP 서버{mcp_count}"),
            hint: String::new(),
            header: mcp_card.header,
            header_cells: mcp_card.header_cells,
            lines: mcp_card.lines,
        },
    );
    render_text_card(
        frame,
        state,
        panes[2],
        TextCard {
            card: CardId::SettingsAgents,
            title: format!("에이전트·스킬·env{agent_count}"),
            hint: String::new(),
            header: agent_card.header,
            header_cells: agent_card.header_cells,
            lines: agent_card.lines,
        },
    );
    render_text_card(
        frame,
        state,
        panes[3],
        TextCard {
            card: CardId::SettingsDoctor,
            title: format!("진단{doctor_summary}"),
            hint: "r 새로고침".to_string(),
            header: doctor_card.header,
            header_cells: doctor_card.header_cells,
            lines: doctor_card.lines,
        },
    );
}

// 3. 훅 등록 줄 ---------------------------------------------------------------
// (고정 머리글, 정렬한 훅 한 줄씩 + HTTP 훅 허용 목록), 적재 전에는 상태 문구 한 줄
pub fn build_hook_lines(inventory: &Panel<Inventory>, width: usize, sort: SortState) -> CardLines {
    let Panel::Ready(loaded) = inventory else {
        return CardLines {
            lines: unloaded_lines(inventory),
            ..CardLines::default()
        };
    };
    let mut lines: Vec<Line<'static>> = Vec::new();

    // 1. 머리글
    let target_width = width
        .saturating_sub(
            HOOK_EVENT_WIDTH
                + HOOK_MATCHER_WIDTH
                + HOOK_TYPE_WIDTH
                + HOOK_ASYNC_WIDTH
                + HOOK_TIMEOUT_WIDTH
                + 5,
        )
        .max(10);
    let (header_line, header_cells) = build_uniform_sort_header(
        &[
            ("이벤트", HOOK_EVENT_WIDTH, false),
            ("matcher", HOOK_MATCHER_WIDTH, false),
            ("종류", HOOK_TYPE_WIDTH, false),
            ("대상", target_width, false),
            ("async", HOOK_ASYNC_WIDTH, false),
            ("timeout", HOOK_TIMEOUT_WIDTH, true),
        ],
        sort,
    );

    // 2. 훅 줄 (정렬)
    let mut sorted = loaded.hooks.clone();
    sort_rows(&mut sorted, sort, compare_hook_entry);
    for hook in &sorted {
        let timeout = hook
            .timeout_secs
            .map_or("-".to_string(), |secs| format!("{secs}s"));
        lines.push(Line::from(Span::styled(
            format!(
                "{} {} {} {} {} {}",
                fit_display(&hook.event, HOOK_EVENT_WIDTH),
                fit_display(&hook.matcher, HOOK_MATCHER_WIDTH),
                fit_display(&hook.hook_type, HOOK_TYPE_WIDTH),
                fit_display(&hook.target, target_width),
                fit_display(if hook.is_async { "예" } else { "-" }, HOOK_ASYNC_WIDTH),
                pad_left_display(&timeout, HOOK_TIMEOUT_WIDTH),
            ),
            theme::STYLE_TEXT,
        )));
    }

    // 3. HTTP 훅 허용 목록
    if !loaded.http_hook_allowlist.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("HTTP 훅 허용 목록 ({}건)", loaded.http_hook_allowlist.len()),
            theme::STYLE_ACCENT,
        )));
        for allowed in &loaded.http_hook_allowlist {
            lines.push(Line::from(Span::styled(
                format!("  {allowed}"),
                theme::STYLE_TEXT,
            )));
        }
    }
    CardLines {
        header: Some(header_line.style(theme::STYLE_TABLE_HEADER)),
        header_cells,
        lines,
    }
}

// 4. MCP 서버 줄 --------------------------------------------------------------
// 정렬한 서버 한 줄씩 + 적재 경고
pub fn build_mcp_lines(inventory: &Panel<Inventory>, width: usize, sort: SortState) -> CardLines {
    let Panel::Ready(loaded) = inventory else {
        return CardLines {
            lines: unloaded_lines(inventory),
            ..CardLines::default()
        };
    };
    let mut lines: Vec<Line<'static>> = Vec::new();

    // 1. 머리글
    let target_width = width
        .saturating_sub(MCP_MARK_WIDTH + NAME_WIDTH + SMALL_COLUMN_WIDTH + 3)
        .max(10);
    let (header_line, header_cells) = build_uniform_sort_header(
        &[
            ("상태", MCP_MARK_WIDTH, false),
            ("이름", NAME_WIDTH, false),
            ("전송", SMALL_COLUMN_WIDTH, false),
            ("대상", target_width, false),
        ],
        sort,
    );

    // 2. 서버 줄 (정렬)
    let mut sorted = loaded.mcp_servers.clone();
    sort_rows(&mut sorted, sort, compare_mcp_entry);
    for server in &sorted {
        let (mark, style) = if server.denied {
            ("차단", theme::STYLE_CRIT)
        } else {
            ("허용", theme::STYLE_OK)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", fit_display(mark, MCP_MARK_WIDTH)), style),
            Span::styled(
                format!(
                    "{} {} {}",
                    fit_display(&server.name, NAME_WIDTH),
                    fit_display(&server.transport, SMALL_COLUMN_WIDTH),
                    fit_display(&server.target, target_width),
                ),
                if server.denied {
                    theme::STYLE_DIM
                } else {
                    theme::STYLE_TEXT
                },
            ),
        ]));
    }

    // 3. 적재 경고
    for warning in &loaded.warnings {
        lines.push(Line::from(Span::styled(
            format!("! {warning}"),
            theme::STYLE_WARN,
        )));
    }
    CardLines {
        header: Some(header_line.style(theme::STYLE_TABLE_HEADER)),
        header_cells,
        lines,
    }
}

// 5. 에이전트·스킬·env 줄 -----------------------------------------------------
// 에이전트 표(머리글 정렬)와 스킬·env 요약 줄
pub fn build_agent_lines(inventory: &Panel<Inventory>, width: usize, sort: SortState) -> CardLines {
    let Panel::Ready(loaded) = inventory else {
        return CardLines {
            lines: unloaded_lines(inventory),
            ..CardLines::default()
        };
    };
    let mut lines: Vec<Line<'static>> = Vec::new();

    // 1. 머리글과 에이전트 줄 (정렬)
    let (header_line, header_cells) = build_uniform_sort_header(
        &[
            ("이름", NAME_WIDTH, false),
            ("모델", SMALL_COLUMN_WIDTH, false),
            ("effort", SMALL_COLUMN_WIDTH, false),
            ("최대턴", AGENT_TURNS_WIDTH, true),
        ],
        sort,
    );
    let mut sorted = loaded.agents.clone();
    sort_rows(&mut sorted, sort, compare_agent_entry);
    for agent in &sorted {
        let max_turns = agent
            .max_turns
            .map_or("-".to_string(), |turns| turns.to_string());
        lines.push(Line::from(Span::styled(
            format!(
                "{} {} {} {max_turns:>AGENT_TURNS_WIDTH$}",
                fit_display(&agent.name, NAME_WIDTH),
                fit_display(&agent.model, SMALL_COLUMN_WIDTH),
                fit_display(&agent.effort, SMALL_COLUMN_WIDTH),
            ),
            theme::STYLE_TEXT,
        )));
    }

    // 2. 스킬 (개수와 설명 길이 합계)
    let description_total: usize = loaded
        .skills
        .iter()
        .map(|skill| skill.description_chars)
        .sum();
    lines.push(Line::from(vec![
        Span::styled("스킬 ", theme::STYLE_ACCENT),
        Span::styled(
            format!(
                "{}개 · 설명 합계 {}자",
                loaded.skills.len(),
                format_count(description_total as i64)
            ),
            theme::STYLE_TEXT,
        ),
    ]));

    // 3. env 키 (폭에 맞춰 줄바꿈)
    lines.push(Line::from(Span::styled(
        format!("env 키 ({}개)", loaded.env_keys.len()),
        theme::STYLE_ACCENT,
    )));
    let wrap_width = width.saturating_sub(2).max(20);
    let mut current = String::new();
    for key in &loaded.env_keys {
        let key_width = Span::raw(key.as_str()).width();
        if !current.is_empty() && Span::raw(current.as_str()).width() + 2 + key_width > wrap_width {
            lines.push(Line::from(Span::styled(
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
        lines.push(Line::from(Span::styled(
            format!("  {current}"),
            theme::STYLE_TEXT,
        )));
    }
    CardLines {
        header: Some(header_line.style(theme::STYLE_TABLE_HEADER)),
        header_cells,
        lines,
    }
}

// 6. 진단 결과 줄 -------------------------------------------------------------
// 수준별 색 (ok 녹색, warn 노랑, fail 적색), 해결 힌트는 다음 줄, 정렬해도 힌트는 항목에 붙음
pub fn build_doctor_lines(
    doctor: &Panel<Vec<DoctorFinding>>,
    width: usize,
    sort: SortState,
) -> CardLines {
    let Panel::Ready(findings) = doctor else {
        return CardLines {
            lines: unloaded_lines(doctor),
            ..CardLines::default()
        };
    };
    let mut lines: Vec<Line<'static>> = Vec::new();

    // 1. 머리글
    let body_width = width
        .saturating_sub(DOCTOR_TAG_WIDTH + NAME_WIDTH + 2)
        .max(10);
    let (header_line, header_cells) = build_uniform_sort_header(
        &[
            ("수준", DOCTOR_TAG_WIDTH, false),
            ("주제", NAME_WIDTH, false),
            ("내용", body_width, false),
        ],
        sort,
    );

    // 2. 진단 줄 (정렬)
    let mut sorted = findings.clone();
    sort_rows(&mut sorted, sort, compare_doctor_finding);
    for finding in &sorted {
        let (tag, style) = match finding.level.as_str() {
            "ok" => ("[정상]", theme::STYLE_OK),
            "warn" => ("[경고]", theme::STYLE_WARN),
            "fail" => ("[실패]", theme::STYLE_CRIT),
            _ => ("[기타]", theme::STYLE_DIM),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{tag} "), style),
            Span::styled(
                format!("{} ", fit_display(&finding.topic, NAME_WIDTH)),
                theme::STYLE_TEXT,
            ),
            Span::styled(
                fit_display(&finding.message, body_width)
                    .trim_end()
                    .to_string(),
                theme::STYLE_TEXT,
            ),
        ]));
        if !finding.fix_hint.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("    → ", theme::STYLE_ACCENT),
                Span::styled(
                    fit_display(&finding.fix_hint, width.saturating_sub(6).max(10))
                        .trim_end()
                        .to_string(),
                    theme::STYLE_DIM,
                ),
            ]));
        }
    }
    CardLines {
        header: Some(header_line.style(theme::STYLE_TABLE_HEADER)),
        header_cells,
        lines,
    }
}

// 7. 적재 전 상태 줄 ----------------------------------------------------------
// Pending·실패 문구를 색과 함께 한 줄로
fn unloaded_lines<T>(panel: &Panel<T>) -> Vec<Line<'static>> {
    let (message, color) = panel_message(panel).unwrap_or((String::new(), theme::FG_DIM));
    vec![Line::from(Span::styled(
        message,
        Style::default().fg(color),
    ))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::{AgentEntry, HookEntry, McpEntry, SkillEntry};
    use crate::tui::sort::sort_spec;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
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

    #[test]
    fn inventory_cards_cover_hooks_mcp_agents_skills_and_env() {
        let inventory = Panel::Ready(sample_inventory());
        let hook_card = build_hook_lines(&inventory, 100, default_sort(CardId::SettingsHooks));
        let hook_text: Vec<String> = hook_card.lines.iter().map(line_text).collect();
        assert!(line_text(&hook_card.header.unwrap()).contains("matcher"));
        assert_eq!(hook_card.header_cells.len(), 6);
        assert_eq!(hook_text.len(), 1);
        assert!(hook_text[0].contains("PreToolUse"));
        assert!(hook_text[0].contains("5s"));

        let mcp_card = build_mcp_lines(&inventory, 100, default_sort(CardId::SettingsMcp));
        let mcp_text: Vec<String> = mcp_card.lines.iter().map(line_text).collect();
        assert!(mcp_text[0].contains("차단"));
        assert_eq!(mcp_card.header_cells.len(), 4);

        let agent_card = build_agent_lines(&inventory, 100, default_sort(CardId::SettingsAgents));
        let agent_text = agent_card
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(agent_text.contains("worker"));
        assert!(agent_text.contains("스킬 2개 · 설명 합계 1,320자"));
        assert!(agent_text.contains("BASH_DEFAULT_TIMEOUT_MS, DEBUG"));
        assert!(line_text(&agent_card.header.unwrap()).contains("최대턴"));
    }

    #[test]
    fn doctor_card_colors_findings_and_prints_fix_hints() {
        let findings = vec![
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
        ];
        let card = build_doctor_lines(
            &Panel::Ready(findings),
            100,
            default_sort(CardId::SettingsDoctor),
        );
        let text = card
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("→ 다른 포트로 변경"));
        assert_eq!(card.lines.len(), 3);
        // 기본 정렬은 심각한 수준이 먼저, 힌트는 해당 항목 바로 뒤
        assert_eq!(card.lines[0].spans[0].style, theme::STYLE_CRIT);
        assert!(line_text(&card.lines[1]).contains("다른 포트로 변경"));
        assert_eq!(card.lines[2].spans[0].style, theme::STYLE_OK);
    }

    #[test]
    fn header_sort_changes_row_order_and_marks_the_active_column() {
        let mut inventory = sample_inventory();
        inventory.hooks.push(HookEntry {
            event: "Notification".to_string(),
            timeout_secs: Some(30),
            ..HookEntry::default()
        });
        let panel = Panel::Ready(inventory);
        let by_timeout = SortState {
            column: 5,
            descending: true,
        };
        let card = build_hook_lines(&panel, 100, by_timeout);
        assert!(line_text(&card.lines[0]).contains("Notification"));
        assert!(line_text(&card.header.unwrap()).contains("timeout↓"));

        let by_event = SortState {
            column: 0,
            descending: false,
        };
        let card = build_hook_lines(&panel, 100, by_event);
        assert!(line_text(&card.lines[0]).contains("Notification"));
        let reversed = SortState {
            column: 0,
            descending: true,
        };
        let card = build_hook_lines(&panel, 100, reversed);
        assert!(line_text(&card.lines[0]).contains("PreToolUse"));
    }

    #[test]
    fn unloaded_panels_show_one_status_line() {
        let inventory: Panel<Inventory> = Panel::Failed("내부 오류: boom".to_string());
        let card = build_hook_lines(&inventory, 80, default_sort(CardId::SettingsHooks));
        assert!(card.header.is_none());
        assert_eq!(line_text(&card.lines[0]), "내부 오류: boom");
        let doctor: Panel<Vec<DoctorFinding>> = Panel::Pending;
        let card = build_doctor_lines(&doctor, 80, default_sort(CardId::SettingsDoctor));
        assert_eq!(line_text(&card.lines[0]), "집계 대기 중");
    }
}
