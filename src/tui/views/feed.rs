//! tui/views/feed.rs
//! 이벤트 피드: 열 격자, 고정 머리글, 이벤트 한 줄, 상세 줄, 같은 종류 맥락 줄.

use crate::core::model::{HookOutcome, LiveEvent};
use crate::tui::cards::HeaderSpan;
use crate::tui::format::{
    fit_display, fit_display_middle, format_clock_time, format_compact, format_duration_ms,
    pad_left_display, short_agent, short_model, short_tool,
};
use crate::tui::sort::SortState;
use crate::tui::theme;
use crate::tui::views::common::{HeaderCell, build_sort_header};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

const CLOCK_WIDTH: usize = 8;
const TAG_WIDTH: usize = 4;
const COLUMN_GAP: usize = 2;
const TOKEN_IN_WIDTH: usize = 6;
const TOKEN_OUT_WIDTH: usize = 7;
const TOKEN_CACHE_WIDTH: usize = 8;
const DURATION_WIDTH: usize = 7;
const DETAIL_LABEL_WIDTH: usize = 10;
const CACHE_BAR_WIDTH: usize = 20;
const NAME_MIN_WIDTH: usize = 16;
const NAME_MAX_WIDTH: usize = 40;
// 정렬 열 번호 순서의 머리글 이름 (sort.rs 와 일치)
pub const FEED_COLUMN_LABELS: [&str; 8] = [
    "시각",
    "종류",
    "이름",
    "에이전트",
    "입력",
    "출력",
    "캐시읽기",
    "소요",
];

// 1. 열 배치 ------------------------------------------------------------------
// 폭 구간별로 보이는 열과 이름·에이전트 칸 폭 (한 프레임에 한 번 계산)
#[derive(Debug, Clone, Copy)]
pub struct FeedColumns {
    pub name: u16,
    pub agent: u16,
    pub show_in: bool,
    pub show_cache: bool,
    pub show_agent: bool,
    pub show_out: bool,
}

// 폭 구간별 열 선택: 100 이상 전체, 76 이상 입력 생략, 56 이상 캐시·에이전트도 생략, 미만 출력도 생략
// 이름 칸 = 가장 긴 이름(16~40)과 남는 폭 중 작은 값
pub fn derive_feed_columns(width: usize, longest_name: usize) -> FeedColumns {
    let (fixed, agent, show_agent, show_in, show_cache, show_out) = if width >= 100 {
        (69usize, 18u16, true, true, true, true)
    } else if width >= 76 {
        (58, 14, true, false, true, true)
    } else if width >= 56 {
        (33, 0, false, false, false, true)
    } else {
        (25, 0, false, false, false, false)
    };
    let name = longest_name
        .clamp(NAME_MIN_WIDTH, NAME_MAX_WIDTH)
        .min(width.saturating_sub(fixed));
    FeedColumns {
        name: name as u16,
        agent,
        show_in,
        show_cache,
        show_agent,
        show_out,
    }
}

// 2. 이벤트 이름 --------------------------------------------------------------
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

// 3. 머리글 줄 ----------------------------------------------------------------
// 본문 줄과 같은 폭 규칙, 숫자 머리글은 오른쪽 정렬, 정렬 열은 화살표
pub fn build_feed_header(
    columns: &FeedColumns,
    sort: SortState,
) -> (Line<'static>, Vec<HeaderSpan>) {
    let mut cells = vec![
        HeaderCell {
            column: 0,
            label: "시각",
            width: CLOCK_WIDTH,
            lead: 0,
            right: false,
        },
        HeaderCell {
            column: 1,
            label: "종류",
            width: TAG_WIDTH,
            lead: COLUMN_GAP,
            right: false,
        },
        HeaderCell {
            column: 2,
            label: "이름",
            width: usize::from(columns.name),
            lead: COLUMN_GAP,
            right: false,
        },
    ];
    if columns.show_agent {
        cells.push(HeaderCell {
            column: 3,
            label: "에이전트",
            width: usize::from(columns.agent),
            lead: COLUMN_GAP,
            right: false,
        });
    }
    let mut token_lead = COLUMN_GAP;
    let tokens = [
        (columns.show_in, 4, "입력", TOKEN_IN_WIDTH),
        (columns.show_out, 5, "출력", TOKEN_OUT_WIDTH),
        (columns.show_cache, 6, "캐시읽기", TOKEN_CACHE_WIDTH),
    ];
    for (shown, column, label, width) in tokens {
        if shown {
            cells.push(HeaderCell {
                column,
                label,
                width,
                lead: token_lead,
                right: true,
            });
            token_lead = 1;
        }
    }
    cells.push(HeaderCell {
        column: 7,
        label: "소요",
        width: DURATION_WIDTH,
        lead: token_lead,
        right: true,
    });
    build_sort_header(&cells, sort)
}

// 4. 칸 구성 ------------------------------------------------------------------
// 한 이벤트의 칸 내용 (이름, 에이전트 조각, 숫자 셋, 토큰 칸 대신 쓰는 조각, 소요)
struct FeedCells {
    name: String,
    name_style: Style,
    agent: Vec<(String, Style)>,
    numbers: Option<[String; 3]>,
    region: Vec<(String, Style)>,
    duration: (String, Style),
}

// 종류별 칸 채우기: 훅은 판정 라벨과 상세, 도구 오류는 오류 종류를 토큰 칸에 둠
fn build_feed_cells(event: &LiveEvent, columns: &FeedColumns) -> FeedCells {
    let agent_width = usize::from(columns.agent);
    let name = feed_event_name(event);
    match event {
        // 1. API 요청
        LiveEvent::Api(row) => {
            let (head, tail) = short_agent(&row.agent, agent_width);
            let head_style = if row.agent.is_empty() || row.agent == "main" {
                theme::STYLE_DIM
            } else {
                theme::STYLE_TEXT
            };
            FeedCells {
                name,
                name_style: theme::STYLE_TEXT,
                agent: vec![(head, head_style), (tail, theme::STYLE_DIM)],
                numbers: Some([
                    format_compact(row.input_tokens),
                    format_compact(row.output_tokens),
                    format_compact(row.cache_read_tokens),
                ]),
                region: Vec::new(),
                duration: row
                    .duration_ms
                    .map_or(("-".to_string(), theme::STYLE_DIM), |ms| {
                        (format_duration_ms(ms), theme::STYLE_TEXT)
                    }),
            }
        }

        // 2. 도구 호출
        LiveEvent::Tool(row) => {
            let (head, tail) = short_agent(&row.agent, agent_width);
            let head_style = if row.agent.is_empty() || row.agent == "main" {
                theme::STYLE_DIM
            } else {
                theme::STYLE_TEXT
            };
            let (name_style, region) = if row.is_error {
                let error_text = if row.error_kind.is_empty() {
                    "오류".to_string()
                } else {
                    format!("오류 {}", row.error_kind)
                };
                (theme::STYLE_CRIT, vec![(error_text, theme::STYLE_CRIT)])
            } else {
                (theme::STYLE_TEXT, Vec::new())
            };
            FeedCells {
                name,
                name_style,
                agent: vec![(head, head_style), (tail, theme::STYLE_DIM)],
                numbers: None,
                region,
                duration: row
                    .duration_ms
                    .map_or(("-".to_string(), theme::STYLE_DIM), |ms| {
                        (format_duration_ms(ms), theme::STYLE_TEXT)
                    }),
            }
        }

        // 3. 훅 판정 (빈 가드는 이름에서 빠지고, 에이전트 칸이 없으면 라벨을 토큰 칸 앞에)
        LiveEvent::Hook(row) => {
            let label = theme::outcome_label(row.outcome.as_str()).to_string();
            let label_style = theme::outcome_style(row.outcome.as_str());
            let name_style = match row.outcome {
                HookOutcome::Deny => theme::STYLE_CRIT,
                HookOutcome::Rewrite => theme::STYLE_OK,
                HookOutcome::Error => theme::STYLE_WARN,
                HookOutcome::Allow => theme::STYLE_TEXT,
            };
            let (agent, region) = if columns.show_agent {
                (
                    vec![(label, label_style)],
                    vec![(row.detail.clone(), theme::STYLE_DIM)],
                )
            } else {
                (
                    Vec::new(),
                    vec![
                        (label, label_style),
                        (" ".to_string(), theme::STYLE_DIM),
                        (row.detail.clone(), theme::STYLE_DIM),
                    ],
                )
            };
            let duration = match (row.saved_ms, row.duration_ms) {
                (Some(saved), _) => (format!("+{}", format_duration_ms(saved)), theme::STYLE_OK),
                (None, Some(ms)) => (format_duration_ms(ms), theme::STYLE_DIM),
                (None, None) => (String::new(), theme::STYLE_DIM),
            };
            FeedCells {
                name,
                name_style,
                agent,
                numbers: None,
                region,
                duration,
            }
        }

        // 4. 알림은 호출하는 쪽에서 문구 전체를 따로 그림
        LiveEvent::Notice { .. } => FeedCells {
            name,
            name_style: theme::STYLE_TEXT,
            agent: Vec::new(),
            numbers: None,
            region: Vec::new(),
            duration: (String::new(), theme::STYLE_DIM),
        },
    }
}

// 5. 조각 맞춤 ----------------------------------------------------------------
// 스타일 조각들을 앞에서부터 정확히 width 열에 맞춤 (넘치면 말줄임, 남으면 공백)
fn fit_styled_pieces(pieces: &[(String, Style)], width: usize) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut left = width;
    for (text, style) in pieces {
        if left == 0 {
            break;
        }
        let shown_width = Span::raw(text.as_str()).width().min(left);
        spans.push(Span::styled(fit_display(text, shown_width), *style));
        left -= shown_width;
    }
    if left > 0 {
        spans.push(Span::raw(" ".repeat(left)));
    }
    spans
}

// 6. 이벤트 한 줄 -------------------------------------------------------------
// 시각·종류 두 칸 뒤에 이름(심각도 색)과 열 격자, 알림은 이름 칸부터 문구 전체
// spans[0] = 시각, spans[1] = 종류 태그, spans[2] = 이름 칸 (테스트가 기대는 계약)
pub fn build_feed_line(event: &LiveEvent, columns: &FeedColumns) -> Line<'static> {
    let name_width = usize::from(columns.name);
    let agent_width = usize::from(columns.agent);
    let token_width = [
        (columns.show_in, TOKEN_IN_WIDTH),
        (columns.show_out, TOKEN_OUT_WIDTH),
        (columns.show_cache, TOKEN_CACHE_WIDTH),
    ]
    .iter()
    .filter(|(shown, _)| *shown)
    .map(|(_, width)| width + 1)
    .sum::<usize>();

    // 1. 시각·종류 칸
    let (clock, tag, tag_color) = match event {
        LiveEvent::Api(row) => (format_clock_time(&row.ts), "API ", theme::COLOR_API),
        LiveEvent::Tool(row) => (format_clock_time(&row.ts), "도구", theme::COLOR_TOOL),
        LiveEvent::Hook(row) => (format_clock_time(&row.ts), "훅  ", theme::COLOR_HOOK),
        LiveEvent::Notice { ts, .. } => (format_clock_time(ts), "알림", theme::COLOR_NOTICE),
    };
    let mut spans = vec![
        Span::styled(format!("{clock}  "), theme::STYLE_DIM),
        Span::styled(
            format!("{tag}  "),
            Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
        ),
    ];

    // 2. 알림: 이름 칸부터 줄 끝까지 문구
    if let LiveEvent::Notice { level, text, .. } = event {
        let agent_cell_width = if columns.show_agent {
            agent_width + COLUMN_GAP
        } else {
            0
        };
        let rest_width = name_width + COLUMN_GAP + agent_cell_width + token_width + DURATION_WIDTH;
        let level_style = match level.as_str() {
            "error" => theme::STYLE_CRIT,
            "warn" | "warning" => theme::STYLE_WARN,
            _ => theme::STYLE_TEXT,
        };
        spans.push(Span::styled(fit_display(text, rest_width), level_style));
        return Line::from(spans);
    }

    // 3. 이름·에이전트 칸
    let cells = build_feed_cells(event, columns);
    spans.push(Span::styled(
        fit_display(&cells.name, name_width),
        cells.name_style,
    ));
    spans.push(Span::raw("  "));
    if columns.show_agent {
        spans.extend(fit_styled_pieces(&cells.agent, agent_width));
        spans.push(Span::raw("  "));
    }

    // 4. 숫자 칸 (오른쪽 정렬) 또는 토큰 칸 조각
    if let Some(numbers) = &cells.numbers {
        let numeric = [
            (columns.show_in, TOKEN_IN_WIDTH, &numbers[0]),
            (columns.show_out, TOKEN_OUT_WIDTH, &numbers[1]),
            (columns.show_cache, TOKEN_CACHE_WIDTH, &numbers[2]),
        ];
        for (shown, width, number) in numeric {
            if shown {
                spans.push(Span::styled(
                    format!("{} ", pad_left_display(number, width)),
                    theme::STYLE_TEXT,
                ));
            }
        }
    } else if token_width > 0 {
        spans.extend(fit_styled_pieces(&cells.region, token_width));
    }

    // 5. 소요 칸
    spans.push(Span::styled(
        pad_left_display(&cells.duration.0, DURATION_WIDTH),
        cells.duration.1,
    ));
    Line::from(spans)
}

// 폭만으로 열 배치를 정해 한 줄 생성 (테스트용)
#[cfg(test)]
pub fn build_event_line(event: &LiveEvent, width: usize) -> Line<'static> {
    build_feed_line(event, &derive_feed_columns(width, NAME_MAX_WIDTH))
}

// 7. 이벤트 상세 줄 -----------------------------------------------------------
// 종류 제목 한 줄 + 항목 이름·값 줄 (원문은 담지 않고 메타데이터만), 긴 값은 중간 말줄임
pub fn build_event_detail_lines(event: &LiveEvent, width: usize) -> Vec<Line<'static>> {
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
            Line::from(Span::styled("API 요청", bold(theme::COLOR_API))),
            vec![
                ("시각", text_value(&format_clock_time(&row.ts))),
                ("모델", text_value(&row.model)),
                ("에이전트", text_value(&row.agent)),
                ("입력 토큰", text_value(&format_compact(row.input_tokens))),
                ("출력 토큰", text_value(&format_compact(row.output_tokens))),
                (
                    "캐시 읽기",
                    text_value(&format_compact(row.cache_read_tokens)),
                ),
                (
                    "캐시 생성",
                    text_value(&format_compact(row.cache_creation_tokens)),
                ),
                ("첫 바이트", duration_value(row.first_byte_ms)),
                ("첫 토큰", duration_value(row.first_token_ms)),
                (
                    "세션",
                    text_value(&row.session_id.chars().take(8).collect::<String>()),
                ),
                ("소요", duration_value(row.duration_ms)),
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
                Span::styled(format!("오류 {}", row.error_kind), theme::STYLE_CRIT)
            } else {
                Span::styled("정상", theme::STYLE_TEXT)
            };
            (
                Line::from(Span::styled("도구 호출", bold(theme::COLOR_TOOL))),
                vec![
                    ("시각", text_value(&format_clock_time(&row.ts))),
                    ("도구", text_value(&row.tool_name)),
                    ("서버", text_value(server)),
                    ("에이전트", text_value(&row.agent)),
                    (
                        "세션",
                        text_value(&row.session_id.chars().take(8).collect::<String>()),
                    ),
                    ("소요", duration_value(row.duration_ms)),
                    ("결과", result),
                    (
                        "호출 id",
                        text_value(&row.tool_use_id.chars().take(8).collect::<String>()),
                    ),
                ],
            )
        }

        // 3. 훅 판정 (가드가 비면 항목 생략)
        LiveEvent::Hook(row) => {
            let mut hook_pairs: Vec<(&str, Span<'static>)> = vec![
                ("시각", text_value(&format_clock_time(&row.ts))),
                ("경로", text_value(&row.route)),
            ];
            if !row.guard.is_empty() {
                hook_pairs.push(("가드", text_value(&row.guard)));
            }
            hook_pairs.push((
                "판정",
                Span::styled(
                    theme::outcome_label(row.outcome.as_str()),
                    theme::outcome_style(row.outcome.as_str()),
                ),
            ));
            hook_pairs.push(("상세", text_value(&row.detail)));
            hook_pairs.push(("소요", duration_value(row.duration_ms)));
            hook_pairs.push(("절약", duration_value(row.saved_ms)));
            hook_pairs.push((
                "세션",
                text_value(&row.session_id.chars().take(8).collect::<String>()),
            ));
            (
                Line::from(Span::styled("훅 판정", bold(theme::COLOR_HOOK))),
                hook_pairs,
            )
        }

        // 4. 알림 (항목 3개 고정)
        LiveEvent::Notice { ts, level, text } => (
            Line::from(Span::styled("알림", bold(theme::COLOR_NOTICE))),
            vec![
                ("시각", text_value(&format_clock_time(ts))),
                ("수준", text_value(level)),
                ("내용", text_value(text)),
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
                    fit_display("캐시 적중", DETAIL_LABEL_WIDTH),
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

// 8. 같은 종류 맥락 줄 --------------------------------------------------------
// 구분선, 집계 한 줄, 같은 에이전트(도구는 같은 도구, 훅은 같은 가드)의 최근 이벤트 최대 row_budget 개
pub fn build_event_context_lines(
    event: &LiveEvent,
    events: &[LiveEvent],
    width: usize,
    row_budget: usize,
) -> Vec<Line<'static>> {
    // 1. 같은 종류 이벤트 모으기 (알림은 맥락 없음)
    let (title, matching): (&str, Vec<&LiveEvent>) = match event {
        LiveEvent::Api(row) => (
            "같은 에이전트",
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
            "같은 도구",
            events
                .iter()
                .filter(|other| {
                    matches!(other, LiveEvent::Tool(candidate) if candidate.tool_name == row.tool_name)
                })
                .collect(),
        ),
        LiveEvent::Hook(row) => (
            "같은 가드",
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
        LiveEvent::Api(_) => format!(
            "API {api_count} · 도구 {tool_count} · 오류 {error_count} · 평균 소요 {average}"
        ),
        LiveEvent::Tool(_) => {
            format!("호출 {tool_count} · 오류 {error_count} · 평균 소요 {average}")
        }
        _ => format!("훅 {hook_count} · 거부 {denial_count} · 재작성 {rewrite_count}"),
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
    let compact_columns = derive_feed_columns(width, 0);
    let name_width = width
        .saturating_sub(
            CLOCK_WIDTH + COLUMN_GAP + TAG_WIDTH + COLUMN_GAP + COLUMN_GAP + DURATION_WIDTH,
        )
        .max(1);
    for other in matching.iter().take(row_budget) {
        let (clock, tag, tag_color) = match other {
            LiveEvent::Api(row) => (format_clock_time(&row.ts), "API ", theme::COLOR_API),
            LiveEvent::Tool(row) => (format_clock_time(&row.ts), "도구", theme::COLOR_TOOL),
            LiveEvent::Hook(row) => (format_clock_time(&row.ts), "훅  ", theme::COLOR_HOOK),
            LiveEvent::Notice { ts, .. } => (format_clock_time(ts), "알림", theme::COLOR_NOTICE),
        };
        let cells = build_feed_cells(other, &compact_columns);
        lines.push(Line::from(vec![
            Span::styled(format!("{clock}  "), theme::STYLE_DIM),
            Span::styled(
                format!("{tag}  "),
                Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(fit_display(&cells.name, name_width), cells.name_style),
            Span::raw("  "),
            Span::styled(
                pad_left_display(&cells.duration.0, DURATION_WIDTH),
                cells.duration.1,
            ),
        ]));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{ApiRequestRow, HookEventRow, ToolCallRow};

    const TIME_DESCENDING: SortState = SortState {
        column: 0,
        descending: true,
    };

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
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
    fn event_line_tags_each_kind_and_fits_width() {
        let hook = LiveEvent::Hook(HookEventRow {
            ts: "2026-11-15T01:00:01Z".to_string(),
            route: "pretool".to_string(),
            guard: "cliguard".to_string(),
            outcome: HookOutcome::Deny,
            detail: "x".repeat(200),
            ..HookEventRow::default()
        });
        let line = build_event_line(&hook, 60);
        assert!(line.width() <= 60);
        assert_eq!(line.spans[2].style, theme::STYLE_CRIT);

        let notice = LiveEvent::Notice {
            ts: "2026-11-15T01:00:00Z".to_string(),
            level: "warn".to_string(),
            text: "느린 색인".to_string(),
        };
        let line = build_event_line(&notice, 60);
        assert_eq!(line.spans[1].content, "알림  ");
        assert_eq!(line.spans[2].style, theme::STYLE_WARN);
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
        let text: Vec<String> = build_event_detail_lines(&hook, 60)
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
        let text: Vec<String> = build_event_detail_lines(&notice, 60)
            .iter()
            .map(line_text)
            .collect();
        assert_eq!(text.len(), 4);
        assert!(text.iter().any(|row| row.contains("느린 색인")));
    }

    #[test]
    fn hook_row_with_empty_guard_has_no_dangling_slash() {
        let hook = LiveEvent::Hook(HookEventRow {
            ts: "2026-11-15T01:00:01Z".to_string(),
            route: "pretool-chain".to_string(),
            outcome: HookOutcome::Allow,
            ..HookEventRow::default()
        });
        for width in [40, 60, 80, 120] {
            let text = line_text(&build_event_line(&hook, width));
            assert!(!text.contains('/'), "{width}: {text}");
            assert!(text.contains("pretool-chain"));
        }
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
    fn feed_tiers_keep_every_row_within_width_and_aligned() {
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
            level: "info".to_string(),
            text: "색인 완료".to_string(),
        };
        let first = api_event(340, "main");
        let second = api_event(911_900, "engineer-rdr2#a0a2bf9b");
        for width in [30, 40, 56, 60, 75, 76, 80, 99, 100, 120, 200] {
            let columns = derive_feed_columns(width, 40);
            let expected = build_feed_header(&columns, TIME_DESCENDING).0.width();
            for event in [&first, &second, &tool, &hook, &notice] {
                let line = build_feed_line(event, &columns);
                assert!(line.width() <= width.max(25), "{width}: {}", line.width());
                assert_eq!(line.width(), expected, "{width}: {}", line_text(&line));
            }
        }

        // 숫자 열은 모든 줄에서 같은 x 에서 끝남 (출력 열: 340 / 911.9K)
        for width in [80, 120] {
            let columns = derive_feed_columns(width, 40);
            let end_of = |event: &LiveEvent, token: &str| {
                let text = line_text(&build_feed_line(event, &columns));
                let found = text.find(token).unwrap();
                Span::raw(&text[..found + token.len()]).width()
            };
            assert_eq!(end_of(&first, "340"), end_of(&second, "911.9K"), "{width}");
        }
    }

    #[test]
    fn feed_tiers_drop_columns_by_width() {
        let wide = derive_feed_columns(120, 40);
        assert!(wide.show_in && wide.show_out && wide.show_cache && wide.show_agent);
        assert_eq!((wide.agent, wide.name), (18, 40));
        let mid = derive_feed_columns(80, 40);
        assert!(!mid.show_in && mid.show_out && mid.show_cache && mid.show_agent);
        assert_eq!(mid.agent, 14);
        let narrow = derive_feed_columns(60, 40);
        assert!(narrow.show_out && !narrow.show_cache && !narrow.show_agent);
        assert_eq!(narrow.name, 27);
        let tiny = derive_feed_columns(40, 40);
        assert!(!tiny.show_out);
        assert_eq!(tiny.name, 15);
        // 가장 긴 이름이 짧아도 이름 칸은 최소 16
        assert_eq!(derive_feed_columns(200, 3).name, 16);
    }

    #[test]
    fn api_row_shortens_model_and_agent_and_drops_inline_labels() {
        let columns = derive_feed_columns(120, 40);
        let text = line_text(&build_feed_line(
            &api_event(340, "engineer-rdr2#a0a2bf9b"),
            &columns,
        ));
        assert!(text.contains("sonnet-5-5"));
        assert!(!text.contains("claude-"));
        assert!(text.contains("engineer-rdr2#a0a2"));
        assert!(!text.contains("a0a2bf9b"));
        assert!(!text.contains("입력"));
        assert!(!text.contains("캐시읽기"));
    }

    #[test]
    fn header_uses_the_same_columns_as_rows() {
        let header =
            line_text(&build_feed_header(&derive_feed_columns(120, 40), TIME_DESCENDING).0);
        for label in [
            "시각",
            "종류",
            "이름",
            "에이전트",
            "입력",
            "출력",
            "캐시읽기",
            "소요",
        ] {
            assert!(header.contains(label), "{label}");
        }
        let narrow = line_text(&build_feed_header(&derive_feed_columns(60, 40), TIME_DESCENDING).0);
        assert!(narrow.contains("출력") && narrow.contains("소요"));
        assert!(!narrow.contains("입력") && !narrow.contains("에이전트"));
    }

    #[test]
    fn api_detail_adds_cache_bar_and_full_identifiers() {
        let text: Vec<String> =
            build_event_detail_lines(&api_event(340, "engineer-rdr2#a0a2bf9b"), 60)
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
        let lines = build_event_context_lines(&events[0], &events, 50, 8);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text[0].starts_with("── 같은 에이전트 "));
        assert_eq!(Span::raw(text[0].as_str()).width(), 50);
        assert!(text[1].contains("API 1 · 도구 1 · 오류 1"));
        // 같은 에이전트 두 건만, 예산 1이면 한 건
        assert_eq!(lines.len(), 4);
        assert_eq!(
            build_event_context_lines(&events[0], &events, 50, 1).len(),
            3
        );

        let notice = LiveEvent::Notice {
            ts: String::new(),
            level: "info".to_string(),
            text: String::new(),
        };
        assert!(build_event_context_lines(&notice, &events, 50, 8).is_empty());
    }

    #[test]
    fn header_arrow_marks_only_the_sorted_column_and_keeps_total_width() {
        for width in [60, 80, 120] {
            let columns = derive_feed_columns(width, 40);
            let baseline = build_feed_header(&columns, TIME_DESCENDING).0.width();
            for column in 0..8 {
                for descending in [false, true] {
                    let sort = SortState { column, descending };
                    let (line, spans) = build_feed_header(&columns, sort);
                    let text = line_text(&line);
                    let arrow = if descending { '↓' } else { '↑' };
                    let visible = spans.iter().any(|span| span.column == column);
                    assert_eq!(line.width(), baseline, "{width}/{column}");
                    assert_eq!(text.matches(['↑', '↓']).count(), usize::from(visible));
                    assert_eq!(text.contains(arrow), visible, "{width}/{column}: {text}");
                }
            }
        }
    }
}
