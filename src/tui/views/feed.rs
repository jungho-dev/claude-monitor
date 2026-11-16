//! tui/views/feed.rs
//! 이벤트 피드: 표 열 명세와 이벤트 한 행의 칸, 상세 줄, 같은 종류 맥락 줄.

use crate::core::model::{HookOutcome, LiveEvent};
use crate::tui::format::{
    fit_display, fit_display_middle, format_clock_time, format_compact, format_duration_ms,
    pad_left_display, replace_control_chars, short_model, short_tool,
};
use crate::tui::i18n::{self, Lang, Msg};
use crate::tui::theme;
use crate::tui::views::common::{ColumnFit, ColumnSpec, TableCell};
use ratatui::layout::Alignment;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const CLOCK_WIDTH: usize = 8;
const TAG_WIDTH: usize = 4;
const COLUMN_GAP: usize = 2;
const DURATION_WIDTH: usize = 7;
const DETAIL_LABEL_WIDTH: usize = 10;
const CACHE_BAR_WIDTH: usize = 20;

// 피드 표 열 (정렬 열 번호와 순서 일치: sort.rs)
// 오른쪽 열부터 숨으므로 시각·종류·이름·소요가 앞, 에이전트와 토큰 열이 뒤, 이름과 에이전트는 읽을 수 있는 폭을 지킴
pub const FEED_COLUMNS: [ColumnSpec; 8] = [
    (Msg::FeedTime, 8, ColumnFit::Fixed, Alignment::Left),
    (Msg::FeedKind, 4, ColumnFit::Fixed, Alignment::Left),
    (Msg::FeedName, 8, ColumnFit::GrowKeep, Alignment::Left),
    (Msg::FeedDuration, 7, ColumnFit::Fixed, Alignment::Right),
    (Msg::FeedAgent, 8, ColumnFit::Keep, Alignment::Left),
    (Msg::FeedInput, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::FeedOutput, 7, ColumnFit::Fixed, Alignment::Right),
    (Msg::FeedCacheRead, 8, ColumnFit::Fixed, Alignment::Right),
];

// 1. 이벤트 이름 --------------------------------------------------------------
// 이름 칸 문구: 모델(claude- 제거), 도구(mcp 축약), 훅 경로·가드 (빈 부분은 건너뜀)
pub fn feed_event_name(event: &LiveEvent) -> String {
    let name = match event {
        LiveEvent::Api(row) => short_model(&row.model).to_string(),
        LiveEvent::Tool(row) => short_tool(&row.tool_name),
        LiveEvent::Hook(row) => [row.route.as_str(), row.guard.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<&str>>()
            .join("·"),
        LiveEvent::Notice { .. } => String::new(),
    };
    if name.is_empty() {
        "-".to_string()
    } else {
        name
    }
}

// 2. 시각·종류 ----------------------------------------------------------------
// 시각 문자열, 종류 태그(정렬용 공백 포함), 태그 색
fn feed_clock_and_tag(lang: Lang, event: &LiveEvent) -> (String, &'static str, Color) {
    match event {
        LiveEvent::Api(row) => (
            format_clock_time(&row.ts),
            i18n::text(lang, Msg::TagApi),
            theme::COLOR_API,
        ),
        LiveEvent::Tool(row) => (
            format_clock_time(&row.ts),
            i18n::text(lang, Msg::TagTool),
            theme::COLOR_TOOL,
        ),
        LiveEvent::Hook(row) => (
            format_clock_time(&row.ts),
            i18n::text(lang, Msg::TagHook),
            theme::COLOR_HOOK,
        ),
        LiveEvent::Notice { ts, .. } => (
            format_clock_time(ts),
            i18n::text(lang, Msg::TagNotice),
            theme::COLOR_NOTICE,
        ),
    }
}

// 3. 이름·소요 스타일 ---------------------------------------------------------
// 이름 글자 색: 도구 오류 적색, 훅은 판정별 색, 그 밖은 본문색
fn feed_name_style(event: &LiveEvent) -> Style {
    match event {
        LiveEvent::Tool(row) if row.is_error => theme::STYLE_CRIT,
        LiveEvent::Hook(row) => match row.outcome {
            HookOutcome::Deny => theme::STYLE_CRIT,
            HookOutcome::Rewrite => theme::STYLE_OK,
            HookOutcome::Error => theme::STYLE_WARN,
            HookOutcome::Allow => theme::STYLE_TEXT,
        },
        _ => theme::STYLE_TEXT,
    }
}

// 소요 칸 글자와 스타일: 훅은 절약 시간(+), 없으면 걸린 시간, 값이 없으면 - 또는 빈 칸
fn feed_duration_cell(event: &LiveEvent) -> (String, Style) {
    let dash_or = |millis: Option<i64>| {
        millis.map_or(("-".to_string(), theme::STYLE_DIM), |ms| {
            (format_duration_ms(ms), theme::STYLE_TEXT)
        })
    };
    match event {
        LiveEvent::Api(row) => dash_or(row.duration_ms),
        LiveEvent::Tool(row) => dash_or(row.duration_ms),
        LiveEvent::Hook(row) => match (row.saved_ms, row.duration_ms) {
            (Some(saved), _) => (format!("+{}", format_duration_ms(saved)), theme::STYLE_OK),
            (None, Some(ms)) => (format_duration_ms(ms), theme::STYLE_DIM),
            (None, None) => (String::new(), theme::STYLE_DIM),
        },
        LiveEvent::Notice { .. } => (String::new(), theme::STYLE_DIM),
    }
}

// 4. 표 행 --------------------------------------------------------------------
// 한 이벤트의 칸 (열 순서 = FEED_COLUMNS), 훅 상세·도구 오류 문구는 이름 칸 뒤에, 알림 문구는 이름 칸에 두고 훅 판정은 에이전트 칸에 둠
// 칸은 자연 폭 그대로 두고 폭이 모자랄 때만 표 그리기가 끝을 말줄임, 제어 문자는 공백으로
pub fn build_feed_row(lang: Lang, event: &LiveEvent) -> Vec<TableCell> {
    let clean = replace_control_chars;
    let (clock, tag, tag_color) = feed_clock_and_tag(lang, event);
    let (duration, duration_style) = feed_duration_cell(event);
    let name_style = feed_name_style(event);

    // 1. 이름 칸
    let mut name_cell: TableCell = Vec::new();
    match event {
        LiveEvent::Notice { level, text, .. } => {
            let level_style = match level.as_str() {
                "error" => theme::STYLE_CRIT,
                "warn" | "warning" => theme::STYLE_WARN,
                _ => theme::STYLE_TEXT,
            };
            let shown = if text.is_empty() {
                "-".to_string()
            } else {
                clean(text)
            };
            name_cell.push(Span::styled(shown, level_style));
        }
        _ => name_cell.push(Span::styled(clean(&feed_event_name(event)), name_style)),
    }
    if let LiveEvent::Tool(row) = event
        && row.is_error
    {
        name_cell.push(Span::styled(
            format!(" {}", clean(&i18n::event_error(lang, &row.error_kind))),
            theme::STYLE_CRIT,
        ));
    }
    if let LiveEvent::Hook(row) = event
        && !row.detail.is_empty()
    {
        name_cell.push(Span::styled(
            format!(" {}", clean(&row.detail)),
            theme::STYLE_DIM,
        ));
    }

    // 2. 에이전트 칸 (이름 + # 꼬리 4자, 훅은 판정 라벨)
    let agent_cell: TableCell = match event {
        LiveEvent::Api(row) => feed_agent_cell(&clean(&row.agent)),
        LiveEvent::Tool(row) => feed_agent_cell(&clean(&row.agent)),
        LiveEvent::Hook(row) => vec![Span::styled(
            i18n::outcome_label(lang, row.outcome.as_str()),
            theme::outcome_style(row.outcome.as_str()),
        )],
        LiveEvent::Notice { .. } => Vec::new(),
    };

    // 3. 토큰 칸 (API 요청만)
    let tokens: [TableCell; 3] = match event {
        LiveEvent::Api(row) => [row.input_tokens, row.output_tokens, row.cache_read_tokens]
            .map(|count| vec![Span::styled(format_compact(count), theme::STYLE_TEXT)]),
        _ => [Vec::new(), Vec::new(), Vec::new()],
    };

    // 4. 열 순서대로 조립
    let [input_cell, output_cell, cache_cell] = tokens;
    vec![
        vec![Span::styled(clock, theme::STYLE_DIM)],
        vec![Span::styled(
            tag.trim_end().to_string(),
            Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
        )],
        name_cell,
        vec![Span::styled(duration, duration_style)],
        agent_cell,
        input_cell,
        output_cell,
        cache_cell,
    ]
}

// 에이전트 칸: 마지막 # 기준 이름과 꼬리(# + 앞 4자), 빈 값은 -, main 은 흐리게
fn feed_agent_cell(agent: &str) -> TableCell {
    if agent.is_empty() {
        return vec![Span::styled("-", theme::STYLE_DIM)];
    }
    let (name, tail) = match agent.rsplit_once('#') {
        Some((name, suffix)) => (
            name,
            std::iter::once('#')
                .chain(suffix.chars().take(4))
                .collect::<String>(),
        ),
        None => (agent, String::new()),
    };
    let name_style = if name == "main" {
        theme::STYLE_DIM
    } else {
        theme::STYLE_TEXT
    };
    let mut cell = vec![Span::styled(name.to_string(), name_style)];
    if !tail.is_empty() {
        cell.push(Span::styled(tail, theme::STYLE_DIM));
    }
    cell
}

// 5. 이벤트 상세 줄 -----------------------------------------------------------
// 종류 제목 한 줄 + 항목 이름·값 줄 (원문은 담지 않고 메타데이터만), 긴 값은 중간 말줄임
pub fn build_event_detail_lines(lang: Lang, event: &LiveEvent, width: usize) -> Vec<Line<'static>> {
    let word = |msg: Msg| i18n::text(lang, msg);
    let value_width = width.saturating_sub(DETAIL_LABEL_WIDTH);
    let bold = |color| Style::default().fg(color).add_modifier(Modifier::BOLD);
    let text_value = |value: &str| {
        if value.is_empty() {
            Span::styled("-", theme::STYLE_DIM)
        } else {
            Span::styled(fit_display_middle(value, value_width), theme::STYLE_TEXT)
        }
    };
    let duration_value = |millis: Option<i64>| {
        millis.map_or(Span::styled("-", theme::STYLE_DIM), |ms| {
            Span::styled(format_duration_ms(ms), theme::STYLE_TEXT)
        })
    };
    let (headline, pairs): (Line<'static>, Vec<(&str, Span<'static>)>) = match event {
        // 1. API 요청
        LiveEvent::Api(row) => (
            Line::from(Span::styled(word(Msg::DetailApi), bold(theme::COLOR_API))),
            vec![
                (
                    word(Msg::DetailTime),
                    text_value(&format_clock_time(&row.ts)),
                ),
                (word(Msg::DetailModel), text_value(&row.model)),
                (word(Msg::DetailAgent), text_value(&row.agent)),
                (
                    word(Msg::DetailInputTokens),
                    text_value(&format_compact(row.input_tokens)),
                ),
                (
                    word(Msg::DetailOutputTokens),
                    text_value(&format_compact(row.output_tokens)),
                ),
                (
                    word(Msg::DetailCacheRead),
                    text_value(&format_compact(row.cache_read_tokens)),
                ),
                (
                    word(Msg::DetailCacheWrite),
                    text_value(&format_compact(row.cache_creation_tokens)),
                ),
                (
                    word(Msg::DetailFirstByte),
                    duration_value(row.first_byte_ms),
                ),
                (
                    word(Msg::DetailFirstToken),
                    duration_value(row.first_token_ms),
                ),
                (
                    word(Msg::DetailSession),
                    text_value(&row.session_id.chars().take(8).collect::<String>()),
                ),
                (word(Msg::DetailDuration), duration_value(row.duration_ms)),
            ],
        ),

        // 2. 도구 호출
        LiveEvent::Tool(row) => {
            let server = row
                .tool_name
                .strip_prefix("mcp__")
                .and_then(|rest| rest.split_once("__"))
                .map_or("", |(server, _)| server);
            let result = if row.is_error {
                Span::styled(
                    format!("{} {}", word(Msg::FeedError), row.error_kind),
                    theme::STYLE_CRIT,
                )
            } else {
                Span::styled(word(Msg::DetailResultOk), theme::STYLE_TEXT)
            };
            (
                Line::from(Span::styled(word(Msg::DetailTool), bold(theme::COLOR_TOOL))),
                vec![
                    (
                        word(Msg::DetailTime),
                        text_value(&format_clock_time(&row.ts)),
                    ),
                    (word(Msg::DetailToolName), text_value(&row.tool_name)),
                    (word(Msg::DetailServer), text_value(server)),
                    (word(Msg::DetailAgent), text_value(&row.agent)),
                    (
                        word(Msg::DetailSession),
                        text_value(&row.session_id.chars().take(8).collect::<String>()),
                    ),
                    (word(Msg::DetailDuration), duration_value(row.duration_ms)),
                    (word(Msg::DetailResult), result),
                    (
                        word(Msg::DetailCallId),
                        text_value(&row.tool_use_id.chars().take(8).collect::<String>()),
                    ),
                ],
            )
        }

        // 3. 훅 판정 (가드가 비면 항목 생략)
        LiveEvent::Hook(row) => {
            let mut hook_pairs: Vec<(&str, Span<'static>)> = vec![
                (
                    word(Msg::DetailTime),
                    text_value(&format_clock_time(&row.ts)),
                ),
                (word(Msg::DetailRoute), text_value(&row.route)),
            ];
            if !row.guard.is_empty() {
                hook_pairs.push((word(Msg::DetailGuard), text_value(&row.guard)));
            }
            hook_pairs.push((
                word(Msg::DetailVerdict),
                Span::styled(
                    i18n::outcome_label(lang, row.outcome.as_str()),
                    theme::outcome_style(row.outcome.as_str()),
                ),
            ));
            hook_pairs.push((word(Msg::DetailDetail), text_value(&row.detail)));
            hook_pairs.push((word(Msg::DetailDuration), duration_value(row.duration_ms)));
            hook_pairs.push((word(Msg::DetailSaved), duration_value(row.saved_ms)));
            hook_pairs.push((
                word(Msg::DetailSession),
                text_value(&row.session_id.chars().take(8).collect::<String>()),
            ));
            (
                Line::from(Span::styled(word(Msg::DetailHook), bold(theme::COLOR_HOOK))),
                hook_pairs,
            )
        }

        // 4. 알림 (항목 3개 고정)
        LiveEvent::Notice { ts, level, text } => (
            Line::from(Span::styled(
                word(Msg::DetailNotice),
                bold(theme::COLOR_NOTICE),
            )),
            vec![
                (word(Msg::DetailTime), text_value(&format_clock_time(ts))),
                (word(Msg::DetailLevel), text_value(level)),
                (word(Msg::DetailContent), text_value(text)),
            ],
        ),
    };

    // 5. 줄 조립
    let mut lines = vec![headline];
    lines.extend(pairs.into_iter().map(|(name, value)| {
        Line::from(vec![
            Span::styled(fit_display(name, DETAIL_LABEL_WIDTH), theme::STYLE_DIM),
            value,
        ])
    }));

    // 6. API 캐시 적중 막대 (문맥 합계가 0이면 생략)
    if let LiveEvent::Api(row) = event {
        let context_total = row.input_tokens + row.cache_read_tokens + row.cache_creation_tokens;
        if context_total > 0 {
            let ratio = (row.cache_read_tokens as f64 / context_total as f64).clamp(0.0, 1.0);
            let filled = (ratio * CACHE_BAR_WIDTH as f64).round() as usize;
            lines.push(Line::from(vec![
                Span::styled(
                    fit_display(word(Msg::DetailCacheHit), DETAIL_LABEL_WIDTH),
                    theme::STYLE_DIM,
                ),
                Span::styled(format!("{:.1}% ", ratio * 100.0), theme::STYLE_TEXT),
                Span::styled("█".repeat(filled), theme::STYLE_OK),
                Span::styled(
                    "░".repeat(CACHE_BAR_WIDTH - filled),
                    Style::default().fg(theme::SCROLL_TRACK),
                ),
            ]));
        }
    }
    lines
}

// 6. 같은 종류 맥락 줄 --------------------------------------------------------
// 구분선, 집계 한 줄, 같은 에이전트(도구는 같은 도구, 훅은 같은 가드)의 최근 이벤트 최대 row_budget 개
pub fn build_event_context_lines(
    lang: Lang,
    event: &LiveEvent,
    events: &[LiveEvent],
    width: usize,
    row_budget: usize,
) -> Vec<Line<'static>> {
    // 1. 같은 종류 이벤트 모으기 (알림은 맥락 없음)
    let (title, matching): (&str, Vec<&LiveEvent>) = match event {
        LiveEvent::Api(row) => (
            i18n::text(lang, Msg::ContextSameAgent),
            events
                .iter()
                .filter(|other| match other {
                    LiveEvent::Api(candidate) => candidate.agent == row.agent,
                    LiveEvent::Tool(candidate) => candidate.agent == row.agent,
                    _ => false,
                })
                .collect(),
        ),
        LiveEvent::Tool(row) => (
            i18n::text(lang, Msg::ContextSameTool),
            events
                .iter()
                .filter(|other| {
                    matches!(other, LiveEvent::Tool(candidate) if candidate.tool_name == row.tool_name)
                })
                .collect(),
        ),
        LiveEvent::Hook(row) => (
            i18n::text(lang, Msg::ContextSameGuard),
            events
                .iter()
                .filter(|other| {
                    matches!(other, LiveEvent::Hook(candidate) if candidate.guard == row.guard)
                })
                .collect(),
        ),
        LiveEvent::Notice { .. } => return Vec::new(),
    };

    // 2. 집계 한 줄
    let mut api_count = 0usize;
    let mut tool_count = 0usize;
    let mut hook_count = 0usize;
    let mut error_count = 0usize;
    let mut denial_count = 0usize;
    let mut rewrite_count = 0usize;
    let mut duration_sum = 0i64;
    let mut duration_count = 0i64;
    for other in &matching {
        match other {
            LiveEvent::Api(candidate) => {
                api_count += 1;
                if let Some(ms) = candidate.duration_ms {
                    duration_sum += ms;
                    duration_count += 1;
                }
            }
            LiveEvent::Tool(candidate) => {
                tool_count += 1;
                error_count += usize::from(candidate.is_error);
                if let Some(ms) = candidate.duration_ms {
                    duration_sum += ms;
                    duration_count += 1;
                }
            }
            LiveEvent::Hook(candidate) => {
                hook_count += 1;
                denial_count += usize::from(candidate.outcome == HookOutcome::Deny);
                rewrite_count += usize::from(candidate.outcome == HookOutcome::Rewrite);
            }
            LiveEvent::Notice { .. } => {}
        }
    }
    let average = if duration_count > 0 {
        format_duration_ms(duration_sum / duration_count)
    } else {
        "-".to_string()
    };
    let summary = match event {
        LiveEvent::Api(_) => {
            i18n::context_summary_api(lang, api_count, tool_count, error_count, &average)
        }
        LiveEvent::Tool(_) => i18n::context_summary_tool(lang, tool_count, error_count, &average),
        _ => i18n::context_summary_hook(lang, hook_count, denial_count, rewrite_count),
    };

    // 3. 구분선과 집계 줄
    let rule_head = format!("── {title} ");
    let rule_fill = width.saturating_sub(Span::raw(rule_head.as_str()).width());
    let mut lines = vec![
        Line::from(Span::styled(
            format!("{rule_head}{}", "─".repeat(rule_fill)),
            theme::STYLE_DIM,
        )),
        Line::from(Span::styled(
            fit_display(&summary, width).trim_end().to_string(),
            theme::STYLE_TEXT,
        )),
    ];

    // 4. 최근 이벤트 (시각 8, 종류 4, 이름 flex, 소요 7 오른쪽 정렬)
    let name_width = width
        .saturating_sub(
            CLOCK_WIDTH + COLUMN_GAP + TAG_WIDTH + COLUMN_GAP + COLUMN_GAP + DURATION_WIDTH,
        )
        .max(1);
    for other in matching.iter().take(row_budget) {
        let (clock, tag, tag_color) = feed_clock_and_tag(lang, other);
        let (duration, duration_style) = feed_duration_cell(other);
        lines.push(Line::from(vec![
            Span::styled(format!("{clock}  "), theme::STYLE_DIM),
            Span::styled(
                format!("{tag}  "),
                Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                fit_display(&feed_event_name(other), name_width),
                feed_name_style(other),
            ),
            Span::raw("  "),
            Span::styled(pad_left_display(&duration, DURATION_WIDTH), duration_style),
        ]));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{ApiRequestRow, HookEventRow, ToolCallRow};
    use crate::tui::cards::CardId;
    use crate::tui::sort::sort_spec;

    const KO: Lang = Lang::Ko;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn cell_text(cell: &TableCell) -> String {
        cell.iter().map(|span| span.content.as_ref()).collect()
    }

    fn api_event(output_tokens: i64, agent: &str) -> LiveEvent {
        LiveEvent::Api(ApiRequestRow {
            ts: "2026-11-15T01:00:03Z".to_string(),
            model: "claude-sonnet-5-5".to_string(),
            agent: agent.to_string(),
            input_tokens: 1_200,
            output_tokens,
            cache_read_tokens: 176_200_000,
            duration_ms: Some(2_100),
            ..ApiRequestRow::default()
        })
    }

    #[test]
    fn api_tag_cells_in_the_rendered_feed_use_the_api_color_not_the_accent() {
        use crate::tui::test_support::sample_state_with_snapshot;
        use crate::tui::views::render_frame;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut state = sample_state_with_snapshot();
        // API 행(첫 행)이 선택 행 색으로 덮이지 않게 다른 행을 선택
        state.card_views[CardId::EventFeed as usize].selected = 1;
        let mut terminal = Terminal::new(TestBackend::new(130, 40)).unwrap();
        terminal
            .draw(|frame| render_frame(frame, &mut state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let feed = state
            .hit_areas
            .card_rects
            .iter()
            .find(|(card, _)| *card == CardId::EventFeed)
            .map(|(_, rect)| *rect)
            .unwrap();

        // 제목 줄(위 테두리)의 개수 안내 문구에도 API 가 있으니 표 안쪽 줄만 봄
        let mut tag_starts = Vec::new();
        for y in feed.y + 1..feed.bottom() {
            for x in feed.x..feed.right().saturating_sub(2) {
                let word: String = (x..x + 3)
                    .map(|column| buffer[(column, y)].symbol())
                    .collect();
                if word == "API" {
                    tag_starts.push((x, y));
                }
            }
        }
        assert!(!tag_starts.is_empty());
        assert_eq!(theme::COLOR_API, Color::Indexed(111));
        assert_ne!(theme::COLOR_API, theme::FG_ACCENT);
        for (x, y) in tag_starts {
            assert_eq!(buffer[(x, y)].fg, theme::COLOR_API);
        }
    }

    #[test]
    fn feed_columns_match_the_sort_spec_one_to_one_in_order() {
        let spec = sort_spec(CardId::EventFeed).unwrap();
        assert_eq!(FEED_COLUMNS.len(), spec.kinds.len());
        let labels: Vec<Msg> = FEED_COLUMNS.iter().map(|column| column.0).collect();
        assert_eq!(
            labels,
            [
                Msg::FeedTime,
                Msg::FeedKind,
                Msg::FeedName,
                Msg::FeedDuration,
                Msg::FeedAgent,
                Msg::FeedInput,
                Msg::FeedOutput,
                Msg::FeedCacheRead
            ]
        );
        // 숫자 열(오른쪽 정렬)은 숫자·시간 정렬 열
        for (index, column) in FEED_COLUMNS.iter().enumerate() {
            let numeric = spec.kinds[index] == crate::tui::sort::SortKind::Number;
            assert_eq!(
                column.3 == Alignment::Right,
                numeric && index != 0,
                "{index}"
            );
        }
    }

    #[test]
    fn each_kind_fills_the_eight_cells_in_column_order() {
        let tool = LiveEvent::Tool(ToolCallRow {
            ts: "2026-11-15T01:00:02Z".to_string(),
            tool_name: "mcp__rust-fs-mcp__file-read".to_string(),
            agent: "engineer-rdr2#a0a2bf9b".to_string(),
            is_error: true,
            error_kind: "timeout".to_string(),
            duration_ms: Some(85),
            ..ToolCallRow::default()
        });
        let hook = LiveEvent::Hook(HookEventRow {
            ts: "2026-11-15T01:00:01Z".to_string(),
            route: "pretool".to_string(),
            guard: "cliguard".to_string(),
            outcome: HookOutcome::Rewrite,
            detail: "grep -> rg".to_string(),
            saved_ms: Some(1_500),
            ..HookEventRow::default()
        });
        let notice = LiveEvent::Notice {
            ts: "2026-11-15T01:00:00Z".to_string(),
            level: "warn".to_string(),
            text: "느린 색인".to_string(),
        };

        // API: 모델(claude- 제거), 소요, 에이전트 이름과 꼬리, 토큰 셋
        let row = build_feed_row(KO, &api_event(340, "engineer-rdr2#a0a2bf9b"));
        assert_eq!(row.len(), 8);
        assert_eq!(cell_text(&row[1]), "API");
        assert_eq!(cell_text(&row[2]), "sonnet-5-5");
        assert_eq!(cell_text(&row[3]), "2.1s");
        assert_eq!(cell_text(&row[4]), "engineer-rdr2#a0a2");
        assert_eq!(row[4][1].style, theme::STYLE_DIM);
        assert_eq!(cell_text(&row[5]), "1.2K");
        assert_eq!(cell_text(&row[6]), "340");
        assert_eq!(cell_text(&row[7]), "176.2M");

        // 도구 오류: 이름이 적색, 오류 문구는 이름 뒤, 토큰 칸 비움
        let row = build_feed_row(KO, &tool);
        assert_eq!(cell_text(&row[1]), "도구");
        assert_eq!(cell_text(&row[2]), "rust-fs·file-read 오류 timeout");
        assert_eq!(row[2][0].style, theme::STYLE_CRIT);
        assert_eq!(cell_text(&row[3]), "85ms");
        assert!(row[5..].iter().all(Vec::is_empty));

        // 훅: 이름은 판정 색, 상세는 이름 뒤 흐리게, 판정 라벨은 에이전트 칸, 소요는 절약 시간
        let row = build_feed_row(KO, &hook);
        assert_eq!(cell_text(&row[1]), "훅");
        assert_eq!(cell_text(&row[2]), "pretool·cliguard grep -> rg");
        assert_eq!(row[2][0].style, theme::STYLE_OK);
        assert_eq!(row[2][1].style, theme::STYLE_DIM);
        assert_eq!(cell_text(&row[4]), i18n::outcome_label(KO, "rewrite"));
        assert_eq!(cell_text(&row[3]), "+1.5s");

        // 알림: 문구 전체가 이름 칸, 수준 색
        let row = build_feed_row(KO, &notice);
        assert_eq!(cell_text(&row[1]), "알림");
        assert_eq!(cell_text(&row[2]), "느린 색인");
        assert_eq!(row[2][0].style, theme::STYLE_WARN);
        assert!(row[3..].iter().all(|cell| cell_text(cell).is_empty()));
    }

    #[test]
    fn hook_row_with_empty_guard_and_detail_has_no_dangling_separator() {
        let hook = LiveEvent::Hook(HookEventRow {
            ts: "2026-11-15T01:00:01Z".to_string(),
            route: "pretool-chain".to_string(),
            outcome: HookOutcome::Allow,
            ..HookEventRow::default()
        });
        let row = build_feed_row(KO, &hook);
        assert_eq!(cell_text(&row[2]), "pretool-chain");
        assert!(!cell_text(&row[2]).contains('/'));
        assert_eq!(feed_event_name(&hook), "pretool-chain");

        // 가드가 있으면 가운데점으로 이음
        let guarded = LiveEvent::Hook(HookEventRow {
            route: "pretool".to_string(),
            guard: "cliguard".to_string(),
            ..HookEventRow::default()
        });
        assert_eq!(feed_event_name(&guarded), "pretool·cliguard");
    }

    #[test]
    fn row_cells_turn_control_characters_into_spaces_and_empty_agents_into_a_dash() {
        let hook = LiveEvent::Hook(HookEventRow {
            route: "pre\ttool".to_string(),
            detail: "line one\nline two".to_string(),
            ..HookEventRow::default()
        });
        let row = build_feed_row(KO, &hook);
        let name = cell_text(&row[2]);
        assert_eq!(name, "pre tool line one line two");
        assert!(!name.chars().any(char::is_control));

        let row = build_feed_row(KO, &api_event(1, ""));
        assert_eq!(cell_text(&row[4]), "-");
        assert_eq!(row[4][0].style, theme::STYLE_DIM);
        let row = build_feed_row(KO, &api_event(1, "main"));
        assert_eq!(row[4][0].style, theme::STYLE_DIM);
        let row = build_feed_row(KO, &api_event(1, "worker"));
        assert_eq!(cell_text(&row[4]), "worker");
        assert_eq!(row[4][0].style, theme::STYLE_TEXT);
    }

    #[test]
    fn event_detail_lists_labeled_metadata_for_every_kind() {
        let hook = LiveEvent::Hook(HookEventRow {
            ts: "2026-11-15T01:00:01Z".to_string(),
            route: "pretool".to_string(),
            guard: "cliguard".to_string(),
            outcome: HookOutcome::Rewrite,
            detail: "grep -> rg".to_string(),
            saved_ms: Some(1_500),
            ..HookEventRow::default()
        });
        let text: Vec<String> = build_event_detail_lines(KO, &hook, 60)
            .iter()
            .map(line_text)
            .collect();
        assert_eq!(text[0], "훅 판정");
        assert!(text.iter().any(|row| row.contains("cliguard")));
        assert!(text.iter().any(|row| row.contains("grep -> rg")));
        assert!(text.iter().any(|row| row.contains("1.5s")));

        let notice = LiveEvent::Notice {
            ts: "2026-11-15T01:00:00Z".to_string(),
            level: "warn".to_string(),
            text: "느린 색인".to_string(),
        };
        let text: Vec<String> = build_event_detail_lines(KO, &notice, 60)
            .iter()
            .map(line_text)
            .collect();
        assert_eq!(text.len(), 4);
        assert!(text.iter().any(|row| row.contains("느린 색인")));
    }

    #[test]
    fn api_detail_adds_cache_bar_and_full_identifiers() {
        let text: Vec<String> =
            build_event_detail_lines(KO, &api_event(340, "engineer-rdr2#a0a2bf9b"), 60)
                .iter()
                .map(line_text)
                .collect();
        assert!(text.iter().any(|row| row.contains("claude-sonnet-5-5")));
        assert!(
            text.iter()
                .any(|row| row.contains("engineer-rdr2#a0a2bf9b"))
        );
        let bar = text
            .iter()
            .find(|row| row.starts_with("캐시 적중"))
            .unwrap();
        assert!(bar.contains('%'));
        assert_eq!(bar.matches('█').count() + bar.matches('░').count(), 20);
    }

    #[test]
    fn context_lines_summarize_same_agent_events() {
        let events = vec![
            api_event(340, "worker"),
            LiveEvent::Tool(ToolCallRow {
                ts: "2026-11-15T01:00:02Z".to_string(),
                tool_name: "Bash".to_string(),
                agent: "worker".to_string(),
                is_error: true,
                duration_ms: Some(1_000),
                ..ToolCallRow::default()
            }),
            api_event(10, "other"),
        ];
        let lines = build_event_context_lines(KO, &events[0], &events, 50, 8);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text[0].starts_with("── 같은 에이전트 "));
        assert_eq!(Span::raw(text[0].as_str()).width(), 50);
        assert!(text[1].contains("API 1 · 도구 1 · 오류 1"));
        // 같은 에이전트 두 건만, 예산 1이면 한 건
        assert_eq!(lines.len(), 4);
        assert_eq!(
            build_event_context_lines(KO, &events[0], &events, 50, 1).len(),
            3
        );

        let notice = LiveEvent::Notice {
            ts: String::new(),
            level: "info".to_string(),
            text: String::new(),
        };
        assert!(build_event_context_lines(KO, &notice, &events, 50, 8).is_empty());
    }
}
