//! tui/views/feed.rs
//! 이벤트 피드: 표 열 명세와 이벤트 한 행의 칸, 상세 줄, 같은 종류 맥락 줄.

use crate::core::model::{HookOutcome, LiveEvent, OtelEventRow};
use crate::tui::format::{
    fit_display, fit_display_middle, format_clock_time, format_compact, format_duration_ms,
    pad_left_display, replace_control_chars, short_model, short_tool,
};
use crate::tui::i18n::{self, Lang, Msg};
use crate::tui::theme;
use crate::tui::views::common::{ColumnFit, ColumnSpec, TableCell};
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
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
        LiveEvent::Otel(row) => [
            row.category.as_str(),
            row.name.as_str(),
            row.status.as_str(),
        ]
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
        LiveEvent::Otel(row) => {
            let (tag_msg, tag_color) = feed_otel_tag(row);
            (
                format_clock_time(&row.ts),
                i18n::text(lang, tag_msg),
                tag_color,
            )
        }
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
        LiveEvent::Otel(row) => match row.category.as_str() {
            "api_error" | "api_retries" => theme::STYLE_CRIT,
            "tool_decision" if feed_otel_is_rejected(row) => theme::STYLE_WARN,
            _ => theme::STYLE_TEXT,
        },
        _ => theme::STYLE_TEXT,
    }
}

// 3-1. OTLP 신규 이벤트 태그·색·이름 ------------------------------------------
// 종류 하나에 색 하나: API 오류·재시도 소진은 적색 계열, 거부·중단된 권한 결정은 경고색, 모드 변경은 중립색
// 포커스·강조색은 쓰지 않고 낱말로도 구분됨
fn feed_otel_tag(row: &OtelEventRow) -> (Msg, Color) {
    match row.category.as_str() {
        "compaction" => (Msg::TagCompaction, theme::COLOR_COMPACTION),
        "subagent" => (Msg::TagSubagent, theme::COLOR_SUBAGENT),
        "api_error" | "api_retries" => (Msg::TagApiError, theme::COLOR_CRIT),
        "tool_decision" if feed_otel_is_rejected(row) => (Msg::TagDecision, theme::COLOR_WARN),
        "tool_decision" => (Msg::TagDecision, theme::COLOR_NOTICE),
        "permission_mode" => (Msg::TagMode, theme::COLOR_NOTICE),
        _ => (Msg::TagNotice, theme::COLOR_NOTICE),
    }
}

// 상세 문구 `키=값 키=값` 에서 키의 값 (없으면 빈 문자열)
fn otel_detail_value<'a>(detail: &'a str, key: &str) -> &'a str {
    detail
        .split_whitespace()
        .find_map(|part| part.strip_prefix(key))
        .unwrap_or("")
}

// 권한 결정이 거부·중단인지 (결정 또는 출처에 reject·abort 포함)
fn feed_otel_is_rejected(row: &OtelEventRow) -> bool {
    let source = otel_detail_value(&row.detail, "source=");
    [row.status.as_str(), source]
        .iter()
        .any(|text| text.contains("reject") || text.contains("abort"))
}

// 이름 칸 문구와 그 뒤 흐린 보조 문구
// 컴팩션 `auto 180.0K -> 42.0K`, 서브에이전트 `유형 · 12.3s · 18 tools`, API 오류 `모델 · 529 · attempt 3`,
// 권한 결정 `Bash · reject · user_reject`, 모드 변경 `default -> plan`
fn feed_otel_parts(lang: Lang, row: &OtelEventRow) -> (String, String) {
    let dash_or = |text: &str| {
        if text.is_empty() {
            "-".to_string()
        } else {
            text.to_string()
        }
    };
    let tokens = |amount: Option<i64>| amount.map_or("-".to_string(), format_compact);
    match row.category.as_str() {
        "compaction" => {
            let mut name = format!(
                "{} {} -> {}",
                dash_or(&row.status),
                tokens(row.tokens_before),
                tokens(row.tokens_after)
            );
            let Some(reason) = row.detail.strip_prefix("failed") else {
                return (name, String::new());
            };
            name.push(' ');
            name.push_str(i18n::text(lang, Msg::FeedFailed));
            (name, reason.trim().to_string())
        }
        "subagent" => {
            let mut parts = vec![dash_or(&row.name)];
            if let Some(millis) = row.duration_ms {
                parts.push(format_duration_ms(millis));
            }
            if let Some(count) = row.count {
                parts.push(i18n::tools_count_text(lang, count));
            }
            (parts.join(" · "), row.detail.clone())
        }
        "api_error" => {
            let mut parts = vec![dash_or(short_model(&row.name)), dash_or(&row.status)];
            if let Some(attempt) = row.count {
                parts.push(i18n::attempt_text(lang, attempt));
            }
            (parts.join(" · "), row.detail.clone())
        }
        "api_retries" => (
            i18n::retries_exhausted_text(
                lang,
                &row.count.map_or("-".to_string(), |tries| tries.to_string()),
            ),
            String::new(),
        ),
        "tool_decision" => (
            [
                dash_or(&row.name),
                dash_or(&row.status),
                dash_or(otel_detail_value(&row.detail, "source=")),
            ]
            .join(" · "),
            otel_detail_value(&row.detail, "mode=").to_string(),
        ),
        "permission_mode" => (
            dash_or(&row.detail.replacen("->", " -> ", 1)),
            row.name.clone(),
        ),
        _ => (dash_or(&row.category), row.detail.clone()),
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
        LiveEvent::Otel(row) => dash_or(row.duration_ms),
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
        LiveEvent::Otel(row) => {
            let (name, tail) = feed_otel_parts(lang, row);
            name_cell.push(Span::styled(clean(&name), name_style));
            if !tail.is_empty() {
                name_cell.push(Span::styled(format!(" {}", clean(&tail)), theme::STYLE_DIM));
            }
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
        LiveEvent::Otel(_) | LiveEvent::Notice { .. } => Vec::new(),
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

        // 3-1. OTLP 신규 이벤트 (종류마다 제목·항목이 다름, 메타데이터만)
        LiveEvent::Otel(row) => build_otel_detail(lang, row, value_width),

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

// 5-1. OTLP 신규 이벤트 상세 --------------------------------------------------
// 종류별 제목 줄과 항목 쌍 (원문은 담지 않음), 서브에이전트 시작 시각은 종료 시각에서 소요를 뺀 값
fn build_otel_detail(
    lang: Lang,
    row: &OtelEventRow,
    value_width: usize,
) -> (Line<'static>, Vec<(&'static str, Span<'static>)>) {
    let word = |msg: Msg| i18n::text(lang, msg);
    let text_value = |value: &str| {
        if value.is_empty() {
            Span::styled("-", theme::STYLE_DIM)
        } else {
            Span::styled(fit_display_middle(value, value_width), theme::STYLE_TEXT)
        }
    };
    let tokens_value = |amount: Option<i64>| {
        amount.map_or(Span::styled("-", theme::STYLE_DIM), |tokens| {
            Span::styled(format_compact(tokens), theme::STYLE_TEXT)
        })
    };
    let duration_value = |millis: Option<i64>| {
        millis.map_or(Span::styled("-", theme::STYLE_DIM), |ms| {
            Span::styled(format_duration_ms(ms), theme::STYLE_TEXT)
        })
    };
    let count_value =
        |amount: Option<i64>| text_value(&amount.map_or(String::new(), |n| n.to_string()));
    let mut pairs: Vec<(&'static str, Span<'static>)> = vec![(
        word(Msg::DetailTime),
        text_value(&format_clock_time(&row.ts)),
    )];

    // 1. 종류별 항목
    let headline_msg = match row.category.as_str() {
        "compaction" => {
            let result = match row.detail.strip_prefix("failed") {
                Some(reason) => Span::styled(
                    fit_display_middle(
                        &format!("{} {}", word(Msg::FeedFailed), reason.trim()),
                        value_width,
                    ),
                    theme::STYLE_CRIT,
                ),
                None => Span::styled(word(Msg::DetailResultOk), theme::STYLE_TEXT),
            };
            pairs.extend([
                (word(Msg::DetailTrigger), text_value(&row.status)),
                (word(Msg::DetailBefore), tokens_value(row.tokens_before)),
                (word(Msg::DetailAfter), tokens_value(row.tokens_after)),
                (word(Msg::DetailDuration), duration_value(row.duration_ms)),
                (word(Msg::DetailResult), result),
            ]);
            Msg::DetailCompaction
        }
        "subagent" => {
            let started = match (DateTime::parse_from_rfc3339(&row.ts), row.duration_ms) {
                (Ok(ended), Some(millis)) => format_clock_time(
                    &(ended.with_timezone(&Utc) - ChronoDuration::milliseconds(millis))
                        .to_rfc3339_opts(SecondsFormat::Millis, true),
                ),
                _ => String::new(),
            };
            pairs.extend([
                (word(Msg::DetailType), text_value(&row.name)),
                (word(Msg::DetailMode), text_value(&row.status)),
                (word(Msg::DetailStarted), text_value(&started)),
                (
                    word(Msg::DetailEnded),
                    text_value(&format_clock_time(&row.ts)),
                ),
                (word(Msg::DetailDuration), duration_value(row.duration_ms)),
                (word(Msg::DetailTokens), tokens_value(row.tokens_after)),
                (word(Msg::DetailToolUses), count_value(row.count)),
                (word(Msg::DetailDetail), text_value(&row.detail)),
            ]);
            Msg::DetailSubagent
        }
        "api_error" => {
            pairs.extend([
                (word(Msg::DetailModel), text_value(&row.name)),
                (word(Msg::DetailStatusCode), text_value(&row.status)),
                (word(Msg::DetailAttempt), count_value(row.count)),
                (word(Msg::DetailDetail), text_value(&row.detail)),
            ]);
            Msg::DetailApiError
        }
        "api_retries" => {
            pairs.extend([
                (word(Msg::DetailStatusCode), text_value(&row.status)),
                (word(Msg::DetailAttempt), count_value(row.count)),
                (word(Msg::DetailRetryTime), duration_value(row.duration_ms)),
            ]);
            Msg::DetailApiRetries
        }
        "tool_decision" => {
            let decision_style = if feed_otel_is_rejected(row) {
                theme::STYLE_WARN
            } else {
                theme::STYLE_TEXT
            };
            pairs.extend([
                (word(Msg::DetailToolName), text_value(&row.name)),
                (
                    word(Msg::DetailDecisionValue),
                    Span::styled(fit_display_middle(&row.status, value_width), decision_style),
                ),
                (
                    word(Msg::DetailSource),
                    text_value(otel_detail_value(&row.detail, "source=")),
                ),
                (
                    word(Msg::DetailMode),
                    text_value(otel_detail_value(&row.detail, "mode=")),
                ),
            ]);
            Msg::DetailDecision
        }
        "permission_mode" => {
            let (from_mode, to_mode) = row
                .detail
                .split_once("->")
                .unwrap_or(("", row.status.as_str()));
            pairs.extend([
                (word(Msg::DetailTrigger), text_value(&row.name)),
                (word(Msg::DetailFrom), text_value(from_mode)),
                (word(Msg::DetailTo), text_value(to_mode)),
            ]);
            Msg::DetailPermMode
        }
        _ => {
            pairs.extend([
                (word(Msg::DetailContent), text_value(&row.category)),
                (word(Msg::DetailDetail), text_value(&row.detail)),
            ]);
            Msg::DetailNotice
        }
    };

    // 2. 공통 항목 (에이전트 id 는 있을 때만)
    if !row.agent_id.is_empty() {
        pairs.push((word(Msg::DetailAgent), text_value(&row.agent_id)));
    }
    pairs.push((
        word(Msg::DetailSession),
        text_value(&row.session_id.chars().take(8).collect::<String>()),
    ));
    let (_, tag_color) = feed_otel_tag(row);
    (
        Line::from(Span::styled(
            word(headline_msg),
            Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
        )),
        pairs,
    )
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
        LiveEvent::Otel(_) | LiveEvent::Notice { .. } => return Vec::new(),
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
            LiveEvent::Otel(_) | LiveEvent::Notice { .. } => {}
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

    // OTLP 신규 이벤트 여덟 종류: (이름, 행)
    fn otel_events() -> Vec<(&'static str, OtelEventRow)> {
        let base = |category: &str| OtelEventRow {
            ts: "2026-11-15T01:00:20Z".to_string(),
            session_id: "sess-aaaa0001".to_string(),
            category: category.to_string(),
            ..OtelEventRow::default()
        };
        vec![
            (
                "compaction",
                OtelEventRow {
                    status: "auto".to_string(),
                    tokens_before: Some(180_000),
                    tokens_after: Some(42_000),
                    detail: "ok".to_string(),
                    ..base("compaction")
                },
            ),
            (
                "compaction_failed",
                OtelEventRow {
                    status: "manual".to_string(),
                    tokens_before: Some(150_000),
                    detail: "failed timeout".to_string(),
                    ..base("compaction")
                },
            ),
            (
                "subagent",
                OtelEventRow {
                    name: "Explore".to_string(),
                    status: "sync".to_string(),
                    agent_id: "agent-0001".to_string(),
                    duration_ms: Some(12_300),
                    tokens_after: Some(42_000),
                    count: Some(18),
                    detail: "model=claude-sonnet-5-5".to_string(),
                    ..base("subagent")
                },
            ),
            (
                "api_error",
                OtelEventRow {
                    name: "claude-opus-5-5".to_string(),
                    status: "529".to_string(),
                    count: Some(3),
                    detail: "overloaded".to_string(),
                    ..base("api_error")
                },
            ),
            (
                "api_retries",
                OtelEventRow {
                    status: "exhausted".to_string(),
                    count: Some(10),
                    duration_ms: Some(90_000),
                    ..base("api_retries")
                },
            ),
            (
                "decision_reject",
                OtelEventRow {
                    name: "Bash".to_string(),
                    status: "reject".to_string(),
                    detail: "source=user_reject mode=default".to_string(),
                    ..base("tool_decision")
                },
            ),
            (
                "decision_accept",
                OtelEventRow {
                    name: "Bash".to_string(),
                    status: "accept".to_string(),
                    detail: "source=config mode=plan".to_string(),
                    ..base("tool_decision")
                },
            ),
            (
                "permission_mode",
                OtelEventRow {
                    name: "shift_tab".to_string(),
                    status: "plan".to_string(),
                    detail: "default->plan".to_string(),
                    ..base("permission_mode")
                },
            ),
        ]
    }

    #[test]
    fn otel_rows_get_their_own_tag_color_and_readable_name_in_both_languages() {
        // (태그 영어, 태그 한국어, 태그 색, 이름 스타일, 이름 칸 영어, 이름 칸 한국어)
        let expected: [(&str, &str, &str, Color, Style, &str, &str); 8] = [
            (
                "compaction",
                "Comp",
                "압축",
                theme::COLOR_COMPACTION,
                theme::STYLE_TEXT,
                "auto 180.0K -> 42.0K",
                "auto 180.0K -> 42.0K",
            ),
            (
                "compaction_failed",
                "Comp",
                "압축",
                theme::COLOR_COMPACTION,
                theme::STYLE_TEXT,
                "manual 150.0K -> - failed timeout",
                "manual 150.0K -> - 실패 timeout",
            ),
            (
                "subagent",
                "Sub",
                "서브",
                theme::COLOR_SUBAGENT,
                theme::STYLE_TEXT,
                "Explore · 12.3s · 18 tools model=claude-sonnet-5-5",
                "Explore · 12.3s · 도구 18 model=claude-sonnet-5-5",
            ),
            (
                "api_error",
                "Err",
                "오류",
                theme::COLOR_CRIT,
                theme::STYLE_CRIT,
                "opus-5-5 · 529 · attempt 3 overloaded",
                "opus-5-5 · 529 · 3회차 overloaded",
            ),
            (
                "api_retries",
                "Err",
                "오류",
                theme::COLOR_CRIT,
                theme::STYLE_CRIT,
                "retries exhausted · 10 tries",
                "재시도 소진 · 10회",
            ),
            (
                "decision_reject",
                "Perm",
                "권한",
                theme::COLOR_WARN,
                theme::STYLE_WARN,
                "Bash · reject · user_reject default",
                "Bash · reject · user_reject default",
            ),
            (
                "decision_accept",
                "Perm",
                "권한",
                theme::COLOR_NOTICE,
                theme::STYLE_TEXT,
                "Bash · accept · config plan",
                "Bash · accept · config plan",
            ),
            (
                "permission_mode",
                "Mode",
                "모드",
                theme::COLOR_NOTICE,
                theme::STYLE_TEXT,
                "default -> plan shift_tab",
                "default -> plan shift_tab",
            ),
        ];
        let events = otel_events();
        for (index, (label, tag_en, tag_ko, color, name_style, name_en, name_ko)) in
            expected.iter().enumerate()
        {
            assert_eq!(events[index].0, *label);
            let event = LiveEvent::Otel(events[index].1.clone());
            for (lang, tag, name) in [(Lang::En, tag_en, name_en), (KO, tag_ko, name_ko)] {
                let row = build_feed_row(lang, &event);
                assert_eq!(row.len(), 8);
                assert_eq!(cell_text(&row[1]), *tag, "{label}");
                assert_eq!(row[1][0].style.fg, Some(*color), "{label}");
                assert_eq!(cell_text(&row[2]), *name, "{label}");
                assert_eq!(row[2][0].style, *name_style, "{label}");
                assert_ne!(*color, theme::BORDER_FOCUS);
                assert_ne!(*color, theme::FG_ACCENT);
            }
        }

        // 소요 칸: 서브에이전트·재시도 소진만 값이 있고 나머지는 -
        let duration_of = |name: &str| {
            let found = events.iter().find(|(label, _)| *label == name).unwrap();
            cell_text(&build_feed_row(KO, &LiveEvent::Otel(found.1.clone()))[3])
        };
        assert_eq!(duration_of("subagent"), "12.3s");
        assert_eq!(duration_of("api_retries"), "1m 30s");
        assert_eq!(duration_of("api_error"), "-");

        // 정렬 키는 언어와 무관한 원본 이름 그대로
        let compaction = LiveEvent::Otel(events[0].1.clone());
        assert_eq!(feed_event_name(&compaction), "compaction·auto");
    }

    #[test]
    fn otel_detail_lists_start_end_and_duration_for_subagents_and_labels_every_kind() {
        let events = otel_events();
        let lines_of = |lang: Lang, label: &str| -> Vec<String> {
            let found = events.iter().find(|(name, _)| *name == label).unwrap();
            build_event_detail_lines(lang, &LiveEvent::Otel(found.1.clone()), 60)
                .iter()
                .map(line_text)
                .collect()
        };

        // 서브에이전트: 시작 = 종료 - 소요
        let ended = format_clock_time("2026-11-15T01:00:20Z");
        let started = format_clock_time("2026-11-15T01:00:07.700Z");
        assert_ne!(started, ended);
        let english = lines_of(Lang::En, "subagent");
        assert_eq!(english[0], "Subagent");
        for (label, value) in [
            ("Type", "Explore"),
            ("Mode", "sync"),
            ("Started", started.as_str()),
            ("Ended", ended.as_str()),
            ("Duration", "12.3s"),
            ("Tokens", "42.0K"),
            ("Tools", "18"),
            ("Agent", "agent-0001"),
            ("Session", "sess-aaa"),
        ] {
            assert!(
                english
                    .iter()
                    .any(|row| row.starts_with(label) && row.ends_with(value)),
                "{label} {value}: {english:?}"
            );
        }
        let korean = lines_of(KO, "subagent");
        assert_eq!(korean[0], "서브에이전트");
        assert!(
            korean
                .iter()
                .any(|row| row.starts_with("시작") && row.ends_with(started.as_str()))
        );
        assert!(
            korean
                .iter()
                .any(|row| row.starts_with("종료") && row.ends_with(ended.as_str()))
        );

        // 나머지 종류: 제목과 핵심 항목
        let cases: [(&str, &str, &str, &str); 7] = [
            ("compaction", "Compaction", "Before", "180.0K"),
            (
                "compaction_failed",
                "Compaction",
                "Result",
                "failed timeout",
            ),
            ("api_error", "API error", "Status", "529"),
            ("api_retries", "API retries", "Retry time", "1m 30s"),
            ("decision_reject", "Tool decision", "Source", "user_reject"),
            ("decision_accept", "Tool decision", "Decision", "accept"),
            ("permission_mode", "Permission mode", "From", "default"),
        ];
        for (label, headline, item, value) in cases {
            let rows = lines_of(Lang::En, label);
            assert_eq!(rows[0], headline, "{label}");
            assert!(
                rows.iter()
                    .any(|row| row.starts_with(item) && row.ends_with(value)),
                "{label} {item}: {rows:?}"
            );
        }
        let korean_mode = lines_of(KO, "permission_mode");
        assert_eq!(korean_mode[0], "권한 모드 변경");
        assert!(
            korean_mode
                .iter()
                .any(|row| row.starts_with("이후") && row.ends_with("plan"))
        );
    }

    // 버퍼 전체를 줄 문자열로 (넓은 글자 뒤 칸 건너뜀)
    fn buffer_rows(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
        (0..buffer.area.height)
            .map(|y| {
                let mut text = String::new();
                let mut skip = 0usize;
                for x in 0..buffer.area.width {
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

    #[test]
    fn otel_detail_card_shows_headline_and_items_for_the_selected_event() {
        use crate::tui::test_support::sample_state_with_snapshot;
        use crate::tui::views::render_frame;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        // (이벤트 이름, 영어 제목, 한국어 제목, 영어 항목, 한국어 항목)
        let cases: [(&str, &str, &str, &str, &str); 8] = [
            ("compaction", "Compaction", "컴팩션", "Trigger", "트리거"),
            (
                "compaction_failed",
                "Compaction",
                "컴팩션",
                "Result",
                "결과",
            ),
            ("subagent", "Subagent", "서브에이전트", "Started", "시작"),
            ("api_error", "API error", "API 오류", "Attempt", "시도"),
            (
                "api_retries",
                "API retries",
                "API 재시도",
                "Retry time",
                "대기 시간",
            ),
            (
                "decision_reject",
                "Tool decision",
                "도구 권한 결정",
                "Source",
                "출처",
            ),
            (
                "decision_accept",
                "Tool decision",
                "도구 권한 결정",
                "Decision",
                "결정",
            ),
            (
                "permission_mode",
                "Permission mode",
                "권한 모드 변경",
                "From",
                "이전",
            ),
        ];
        let events = otel_events();
        for (label, head_en, head_ko, item_en, item_ko) in cases {
            let target = events.iter().find(|(name, _)| *name == label).unwrap();
            for lang in [Lang::En, KO] {
                for (width, height) in [(100u16, 30u16), (130, 40)] {
                    let mut state = sample_state_with_snapshot();
                    state.lang = lang;
                    state.snapshot.events =
                        crate::tui::state::Panel::Ready(vec![LiveEvent::Otel(target.1.clone())]);
                    state.card_views[CardId::EventFeed as usize].selected = 0;
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal
                        .draw(|frame| render_frame(frame, &mut state))
                        .unwrap();
                    let rows = buffer_rows(terminal.backend().buffer());
                    let (head, item) = match lang {
                        Lang::En => (head_en, item_en),
                        Lang::Ko => (head_ko, item_ko),
                    };
                    let case = format!("{label} {lang:?} {width}x{height}");
                    assert!(
                        rows.iter().any(|row| row.contains(head)),
                        "{case} headline\n{}",
                        rows.join("\n")
                    );
                    assert!(rows.iter().any(|row| row.contains(item)), "{case} item");
                }
            }
        }
    }

    #[test]
    fn otel_rows_render_in_the_feed_at_every_size_in_both_languages_with_their_tag_color() {
        use crate::tui::test_support::sample_state_with_snapshot;
        use crate::tui::views::render_frame;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        // (이벤트 이름, 영어 태그, 한국어 태그, 영어 이름 조각, 한국어 이름 조각, 태그 색)
        let cases: [(&str, &str, &str, &str, &str, Color); 8] = [
            (
                "compaction",
                "Comp",
                "압축",
                "auto 180.0K",
                "auto 180.0K",
                theme::COLOR_COMPACTION,
            ),
            (
                "compaction_failed",
                "Comp",
                "압축",
                "manual 150.0K",
                "manual 150.0K",
                theme::COLOR_COMPACTION,
            ),
            (
                "subagent",
                "Sub",
                "서브",
                "Explore · 12.3s",
                "Explore · 12.3s",
                theme::COLOR_SUBAGENT,
            ),
            (
                "api_error",
                "Err",
                "오류",
                "opus-5-5 · 529",
                "opus-5-5 · 529",
                theme::COLOR_CRIT,
            ),
            (
                "api_retries",
                "Err",
                "오류",
                "retries exhausted",
                "재시도 소진",
                theme::COLOR_CRIT,
            ),
            (
                "decision_reject",
                "Perm",
                "권한",
                "Bash · reject",
                "Bash · reject",
                theme::COLOR_WARN,
            ),
            (
                "decision_accept",
                "Perm",
                "권한",
                "Bash · accept",
                "Bash · accept",
                theme::COLOR_NOTICE,
            ),
            (
                "permission_mode",
                "Mode",
                "모드",
                "default -> plan",
                "default -> plan",
                theme::COLOR_NOTICE,
            ),
        ];
        let events = otel_events();
        for (label, tag_en, tag_ko, name_en, name_ko, color) in cases {
            let target = events.iter().find(|(name, _)| *name == label).unwrap();
            for lang in [Lang::En, KO] {
                for (width, height) in [(60u16, 16u16), (100, 30), (130, 40)] {
                    let mut state = sample_state_with_snapshot();
                    state.lang = lang;
                    // 대상 행이 선택 행 색에 덮이지 않게 더 오래된 알림을 두고 그 행을 선택
                    state.snapshot.events = crate::tui::state::Panel::Ready(vec![
                        LiveEvent::Otel(target.1.clone()),
                        LiveEvent::Notice {
                            ts: "2026-11-15T00:00:00Z".to_string(),
                            level: "info".to_string(),
                            text: "filler".to_string(),
                        },
                    ]);
                    state.card_views[CardId::EventFeed as usize].selected = 1;
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal
                        .draw(|frame| render_frame(frame, &mut state))
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    let rows = buffer_rows(buffer);
                    let case = format!("{label} {lang:?} {width}x{height}");
                    let (tag, full_name) = match lang {
                        Lang::En => (tag_en, name_en),
                        Lang::Ko => (tag_ko, name_ko),
                    };
                    // 폭이 좋으면 이름 칸이 말줄임되므로 앞 4글자만 확인 (전체 이름은 행 단위 시험에서 확인)
                    let name: String = if width < 130 {
                        full_name.chars().take(4).collect()
                    } else {
                        full_name.to_string()
                    };
                    let name = name.as_str();
                    assert!(
                        rows.iter().any(|row| row.contains(name)),
                        "{case} name\n{}",
                        rows.join("\n")
                    );

                    // 표 안쪽의 이벤트 행에서 태그 글자 칸의 글자색이 종류 색
                    let feed = state
                        .hit_areas
                        .card_rects
                        .iter()
                        .find(|(card, _)| *card == CardId::EventFeed)
                        .map(|(_, rect)| *rect)
                        .unwrap();
                    let mut tag_cell = None;
                    for y in feed.y + 1..feed.bottom() {
                        let row = &rows[y as usize];
                        if row.contains(name) {
                            // 시각 숫자 다음 처음 나오는 글자가 태그의 첫 글자
                            let first = tag.chars().next().unwrap().to_string();
                            let column = (feed.x..feed.right())
                                .find(|x| buffer[(*x, y)].symbol() == first)
                                .unwrap_or_else(|| panic!("{case} tag"));
                            tag_cell = Some((column, y));
                            break;
                        }
                    }
                    let (column, y) = tag_cell.unwrap_or_else(|| panic!("{case} row"));
                    assert_eq!(buffer[(column, y)].fg, color, "{case}");
                    assert_ne!(buffer[(column, y)].fg, theme::BORDER_FOCUS, "{case}");
                }
            }
        }
    }
}
