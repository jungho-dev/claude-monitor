//! tui/views/charts.rs
//! 개요 탭 실시간 차트 카드: 분당 활동 선 그래프, 토큰 구성 막대, 도구 호출 상위 막대.

use crate::tui::cards::CardId;
use crate::tui::format::{fit_display, format_compact, format_count, pad_left_display, short_tool};
use crate::tui::i18n::{self, Msg};
use crate::tui::state::{AppState, Panel};
use crate::tui::theme;
use crate::tui::views::common::{PanelNote, interactive_card, pack_spans_to_width};
use crate::tui::views::common::{panel_message, render_notice_card};
use crate::tui::views::overview::{build_bar, build_minute_series};
use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Chart, Dataset, GraphType, Padding, Paragraph};
use unicode_width::UnicodeWidthStr;

// 선 그래프를 그리는 최소 높이 (범례 줄 아래 영역, 축 이름 줄 포함)
const PLOT_MIN_HEIGHT: u16 = 3;
// 막대를 그리는 최소 폭 (이름과 값을 다 담고 남는 폭이 이보다 좁으면 막대 없이 이름과 값만)
const BAR_MIN_WIDTH: usize = 3;
// 범례의 계열 표식 (계열 색 선 한 칸, 이름과 함께 표시)
const LEGEND_MARK: &str = "━ ";

// 선 그래프 계열 (label = 범례 이름, value = 범례 값 문구, points = (x, y) 오래된 순, 비면 범례만)
#[derive(Debug)]
struct LineSeries {
    label: &'static str,
    value: String,
    color: Color,
    points: Vec<(f64, f64)>,
}

// 선 그래프 카드 입력 (x·y 축은 0 부터 x_max·y_max, 축 이름은 양 끝 두 개)
// x 축 첫 이름의 폭만큼 y 축 왼쪽 열이 넓어지므로 짧은 이름을 씀
#[derive(Debug)]
struct LineCard {
    card: CardId,
    title: &'static str,
    hint: String,
    series: Vec<LineSeries>,
    x_max: f64,
    y_max: f64,
    x_labels: [&'static str; 2],
    y_labels: [String; 2],
}

// 가로 막대 한 줄 (value = 막대 길이 기준 값, flag = 맨 뒤 경고 꼬리, 없으면 빈 문자열)
#[derive(Debug)]
struct BarRow {
    label: String,
    value: u64,
    value_text: String,
    flag: String,
}

// 가로 막대 카드 입력 (rows 는 그릴 순서, 카드 높이를 넘는 뒤쪽 행은 그리지 않음)
#[derive(Debug)]
struct BarCard {
    card: CardId,
    title: &'static str,
    hint: String,
    color: Color,
    rows: Vec<BarRow>,
}

// 1. 분당 활동 선 그래프 ------------------------------------------------------
// 최근 60분의 분당 요청·도구 호출·훅 거부 (지금이 오른쪽 끝), 범례 값은 60분 합계
pub fn render_activity_chart(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // 1. 분당 추이 준비 확인
    let lang = state.lang;
    let title = i18n::text(lang, Msg::TitleChartActivity);
    if let Some(note) = panel_message(lang, &state.snapshot.minutes) {
        render_notice_card(frame, state, CardId::ChartActivity, area, title, note);
        return;
    }
    let now = state.snapshot.taken_at.unwrap_or_else(Utc::now);
    let Panel::Ready(buckets) = &state.snapshot.minutes else {
        return;
    };

    // 2. 계열 (이벤트 종류 색과 같은 색), 값이 전부 0 인 계열은 범례만
    let minute_values = [
        (
            Msg::KpiRequests,
            theme::COLOR_API,
            build_minute_series(buckets, now, |bucket| bucket.api_requests),
        ),
        (
            Msg::KpiToolsShort,
            theme::COLOR_TOOL,
            build_minute_series(buckets, now, |bucket| bucket.tool_calls),
        ),
        (
            Msg::KpiDeny,
            theme::COLOR_HOOK,
            build_minute_series(buckets, now, |bucket| bucket.hook_denials),
        ),
    ];
    let mut peak = 0u64;
    let mut minutes = 0usize;
    let mut series: Vec<LineSeries> = Vec::with_capacity(minute_values.len());
    for (label, color, values) in minute_values {
        let total: u64 = values.iter().sum();
        peak = peak.max(values.iter().copied().max().unwrap_or(0));
        minutes = minutes.max(values.len());
        let points: Vec<(f64, f64)> = if total == 0 {
            Vec::new()
        } else {
            values
                .iter()
                .enumerate()
                .map(|(minute, value)| (minute as f64, *value as f64))
                .collect()
        };
        series.push(LineSeries {
            label: i18n::text(lang, label),
            value: format_count(i64::try_from(total).unwrap_or(i64::MAX)),
            color,
            points,
        });
    }

    // 3. 카드 (우측 상단에 분당 최대값)
    let peak_text = format_compact(i64::try_from(peak).unwrap_or(i64::MAX));
    render_line_card(
        frame,
        state,
        area,
        LineCard {
            card: CardId::ChartActivity,
            title,
            hint: i18n::chart_peak(lang, &peak_text),
            series,
            x_max: minutes.saturating_sub(1).max(1) as f64,
            y_max: peak.max(1) as f64,
            x_labels: [
                i18n::text(lang, Msg::ChartMinutesAgo),
                i18n::text(lang, Msg::TrendNow),
            ],
            y_labels: ["0".to_string(), peak_text],
        },
    );
}

// 2. 토큰 구성 막대 -----------------------------------------------------------
// 선택 구간의 입력·출력·캐시 읽기·캐시 쓰기 토큰, 막대는 가장 큰 항목 기준, 값 뒤에 전체 대비 비율
pub fn render_token_mix(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // 1. 집계 준비 확인
    let lang = state.lang;
    let title = i18n::text(lang, Msg::TitleChartTokens);
    let stats = match &state.snapshot.overview {
        Panel::Ready(stats) => stats.clone(),
        other => {
            let note = panel_message(lang, other).unwrap_or_default();
            render_notice_card(frame, state, CardId::ChartTokens, area, title, note);
            return;
        }
    };

    // 2. 항목 행
    let parts = [
        (Msg::KpiInput, stats.input_tokens),
        (Msg::KpiOutput, stats.output_tokens),
        (Msg::ChartCacheRead, stats.cache_read_tokens),
        (Msg::ChartCacheWrite, stats.cache_creation_tokens),
    ];
    let total: i64 = parts.iter().map(|(_, tokens)| (*tokens).max(0)).sum();
    let rows: Vec<BarRow> = parts
        .iter()
        .map(|(label, tokens)| {
            let tokens = (*tokens).max(0);
            let share = tokens as f64 * 100.0 / total.max(1) as f64;
            BarRow {
                label: i18n::text(lang, *label).to_string(),
                value: tokens.unsigned_abs(),
                value_text: format!("{} {share:>3.0}%", format_compact(tokens)),
                flag: String::new(),
            }
        })
        .collect();

    // 3. 카드 (우측 상단에 집계 구간)
    render_bar_card(
        frame,
        state,
        area,
        BarCard {
            card: CardId::ChartTokens,
            title,
            hint: state.window.window_label(lang).to_string(),
            color: theme::CHART_TOKENS,
            rows,
        },
    );
}

// 3. 도구 호출 상위 막대 ------------------------------------------------------
// 선택 구간의 도구별 호출 수 내림차순, 오류가 있으면 값 뒤에 !오류수
pub fn render_top_tools(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // 1. 집계 준비 확인
    let lang = state.lang;
    let title = i18n::text(lang, Msg::TitleChartTools);
    let window_label = state.window.window_label(lang);
    let mut tools = match &state.snapshot.tools {
        Panel::Ready(rows) => rows.clone(),
        other => {
            let note = panel_message(lang, other).unwrap_or_default();
            render_notice_card(frame, state, CardId::ChartTools, area, title, note);
            return;
        }
    };
    if tools.is_empty() {
        let note = PanelNote {
            message: i18n::no_records_in_window(lang, window_label),
            color: theme::FG_DIM,
            hint: None,
        };
        render_notice_card(frame, state, CardId::ChartTools, area, title, note);
        return;
    }

    // 2. 호출 수 내림차순 행 (같으면 이름순으로 고정)
    tools.sort_by(|left, right| {
        right
            .calls
            .cmp(&left.calls)
            .then_with(|| left.tool_name.cmp(&right.tool_name))
    });
    let rows: Vec<BarRow> = tools
        .iter()
        .map(|tool| BarRow {
            label: short_tool(&tool.tool_name),
            value: tool.calls,
            value_text: format_count(i64::try_from(tool.calls).unwrap_or(i64::MAX)),
            flag: if tool.errors > 0 {
                format!("!{}", tool.errors)
            } else {
                String::new()
            },
        })
        .collect();

    // 3. 카드 (우측 상단에 집계 구간)
    render_bar_card(
        frame,
        state,
        area,
        BarCard {
            card: CardId::ChartTools,
            title,
            hint: window_label.to_string(),
            color: theme::COLOR_TOOL,
            rows,
        },
    );
}

// 4. 선 그래프 카드 (내부) ----------------------------------------------------
// 첫 줄 범례, 그 아래 축과 선 (사분 블록 표식), 낮은 카드는 범례만
fn render_line_card(frame: &mut Frame, state: &mut AppState, area: Rect, card: LineCard) {
    // 1. 카드 테두리
    let block = interactive_card(
        state,
        card.card,
        area,
        card.title,
        &card.hint,
        theme::BORDER_DIM,
    )
    .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    // 2. 범례 줄: 계열 색 표식, 이름, 값 (색만으로 구분하지 않음)
    // 전부 담기지 않으면 값을 빼고 이름만, 그래도 못 담는 뒤 계열은 범례와 선을 함께 뺌 (범례 없는 선은 그리지 않음)
    let width = usize::from(inner.width);
    let mark_width = Span::raw(LEGEND_MARK).width();
    let full_width: usize = card
        .series
        .iter()
        .map(|series| mark_width + Span::raw(series.label).width() + 1 + series.value.width())
        .sum();
    let with_values = full_width + card.series.len().saturating_sub(1) <= width;
    let mut legend_items: Vec<Vec<Span<'static>>> = Vec::with_capacity(card.series.len());
    let mut used = 0usize;
    for series in &card.series {
        let mut entry = vec![
            Span::styled(LEGEND_MARK, Style::default().fg(series.color)),
            Span::styled(series.label, theme::STYLE_DIM),
        ];
        if with_values {
            entry.push(Span::styled(
                format!(" {}", series.value),
                theme::STYLE_TEXT,
            ));
        }
        let entry_width: usize = entry.iter().map(Span::width).sum();
        let separator = usize::from(!legend_items.is_empty());
        if used + separator + entry_width > width {
            break;
        }
        used += separator + entry_width;
        legend_items.push(entry);
    }
    let plotted = legend_items.len();
    let legend = pack_spans_to_width(legend_items, &[3, 2, 1], width);
    frame.render_widget(
        Paragraph::new(Line::from(legend)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    // 3. 선 그래프 (범례에 담긴 계열 중 점이 있는 것만, 앞 계열이 위에 오도록 뒤 계열부터 그림)
    let plot = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
    if plot.height < PLOT_MIN_HEIGHT {
        return;
    }
    let datasets: Vec<Dataset> = card.series[..plotted]
        .iter()
        .rev()
        .filter(|series| !series.points.is_empty())
        .map(|series| {
            Dataset::default()
                .marker(Marker::Quadrant)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(series.color))
                .data(&series.points)
        })
        .collect();
    let x_axis = Axis::default()
        .bounds([0.0, card.x_max])
        .style(theme::STYLE_FAINT)
        .labels(
            card.x_labels
                .map(|label| Span::styled(label, theme::STYLE_DIM)),
        );
    let y_axis = Axis::default()
        .bounds([0.0, card.y_max])
        .style(theme::STYLE_FAINT)
        .labels(
            card.y_labels
                .iter()
                .map(|label| Span::styled(label.clone(), theme::STYLE_DIM)),
        );
    let chart = Chart::new(datasets)
        .x_axis(x_axis)
        .y_axis(y_axis)
        .legend_position(None);
    frame.render_widget(chart, plot);
}

// 5. 가로 막대 카드 (내부) ----------------------------------------------------
// 줄마다 이름, 막대, 값, 경고 꼬리: 이름과 값은 자연 폭, 남는 폭이 막대 (모자라면 막대를 먼저 빼고 그래도 넘칠 때만 이름을 줄임)
fn render_bar_card(frame: &mut Frame, state: &mut AppState, area: Rect, card: BarCard) {
    // 1. 카드 테두리
    let block = interactive_card(
        state,
        card.card,
        area,
        card.title,
        &card.hint,
        theme::BORDER_DIM,
    )
    .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    // 2. 열 폭 (보이는 행 기준), 행을 다 담고도 높이가 남으면 행 사이에 빈 줄
    let width = usize::from(inner.width);
    let height = usize::from(inner.height);
    let shown = &card.rows[..card.rows.len().min(height)];
    let spaced = shown.len() * 2 <= height + 1;
    let mut label_natural = 0usize;
    let mut value_width = 0usize;
    let mut flag_width = 0usize;
    for row in shown {
        label_natural = label_natural.max(row.label.width());
        value_width = value_width.max(row.value_text.width());
        flag_width = flag_width.max(row.flag.width());
    }
    let flag_room = if flag_width > 0 { flag_width + 1 } else { 0 };
    let fixed_width = 1 + value_width + flag_room;
    let label_width = label_natural.min(width.saturating_sub(fixed_width));
    let bar_width = width.saturating_sub(label_width + fixed_width + 1);
    let peak = shown.iter().map(|row| row.value).max().unwrap_or(0).max(1);

    // 3. 줄 조립
    let bar_style = Style::default().fg(card.color);
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(shown.len());
    for row in shown {
        if spaced && !lines.is_empty() {
            lines.push(Line::default());
        }
        let mut spans = vec![Span::styled(
            format!("{} ", fit_display(&row.label, label_width)),
            theme::STYLE_DIM,
        )];
        if bar_width >= BAR_MIN_WIDTH {
            let pct = row.value as f64 * 100.0 / peak as f64;
            spans.extend(build_bar(Some(pct), bar_width, bar_style));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            pad_left_display(&row.value_text, value_width),
            theme::STYLE_TEXT,
        ));
        if !row.flag.is_empty() {
            spans.push(Span::styled(
                format!(" {}", row.flag),
                theme::STYLE_CRIT.add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::query::MinuteBucket;
    use crate::tui::i18n::Lang;
    use crate::tui::layout::SplitGroup;
    use crate::tui::state::ActiveTab;
    use crate::tui::test_support::{fixed_limits_now, sample_state_with_limits};
    use crate::tui::views::render_frame;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    const CHART_CARDS: [CardId; 3] = [
        CardId::ChartActivity,
        CardId::ChartTokens,
        CardId::ChartTools,
    ];

    fn draw(state: &mut AppState, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_frame(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn card_rect(state: &AppState, card: CardId) -> Option<Rect> {
        state
            .hit_areas
            .card_rects
            .iter()
            .find(|(drawn, _)| *drawn == card)
            .map(|(_, rect)| *rect)
    }

    // 영역 안 글자 (줄마다 줄바꿈, 넓은 글자 뒤 칸은 건너뜀)
    fn rect_text(buffer: &Buffer, rect: Rect) -> String {
        let mut text = String::new();
        for y in rect.top()..rect.bottom() {
            let mut skip = 0usize;
            for x in rect.left()..rect.right() {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let symbol = buffer.cell((x, y)).map_or(" ", |cell| cell.symbol());
                skip = Span::raw(symbol).width().saturating_sub(1);
                text.push_str(symbol);
            }
            text.push('\n');
        }
        text
    }

    // 영역 안 빈 칸이 아닌 칸의 (글자, 글자색)
    fn rect_marks(buffer: &Buffer, rect: Rect) -> Vec<(String, Color)> {
        let mut marks = Vec::new();
        for y in rect.top()..rect.bottom() {
            for x in rect.left()..rect.right() {
                let Some(cell) = buffer.cell((x, y)) else {
                    continue;
                };
                if cell.symbol() != " " {
                    marks.push((cell.symbol().to_string(), cell.fg));
                }
            }
        }
        marks
    }

    // 카드 안쪽에서 범례 줄 아래 (선 그래프 영역)
    fn plot_rect(card: Rect) -> Rect {
        Rect::new(card.x + 1, card.y + 2, card.width - 2, card.height - 3)
    }

    // 고정 시계 기준 분 버킷: 지금 요청 7·도구 4, 10분 전 요청 3·도구 9, 거부 없음
    fn activity_state(lang: Lang) -> AppState {
        let mut state = sample_state_with_limits(lang);
        let now = fixed_limits_now();
        let minute_of = |minutes_ago: i64| {
            (now - chrono::Duration::minutes(minutes_ago))
                .format("%Y-%m-%dT%H:%M")
                .to_string()
        };
        state.snapshot.minutes = Panel::Ready(vec![
            MinuteBucket {
                minute: minute_of(10),
                api_requests: 3,
                output_tokens: 900,
                tool_calls: 9,
                hook_denials: 0,
            },
            MinuteBucket {
                minute: minute_of(0),
                api_requests: 7,
                output_tokens: 1_500,
                tool_calls: 4,
                hook_denials: 0,
            },
        ]);
        state
    }

    #[test]
    fn the_chart_row_draws_three_cards_above_the_feed_in_both_languages() {
        let titles = [
            (Lang::En, ["Activity/min", "Token mix", "Top tools"]),
            (Lang::Ko, ["분당 활동", "토큰 구성", "도구 호출 상위"]),
        ];
        for (lang, expected) in titles {
            for (width, height) in [(110u16, 40u16), (130, 40), (200, 60)] {
                let mut state = activity_state(lang);
                let buffer = draw(&mut state, width, height);
                let case = format!("{lang:?} {width}x{height}");
                let feed = card_rect(&state, CardId::EventFeed).expect("feed card");
                let first = card_rect(&state, CardId::ChartActivity).expect("chart card");
                let mut previous_right = 0u16;
                for (card, title) in CHART_CARDS.iter().zip(expected) {
                    let rect = card_rect(&state, *card).expect("chart card");
                    let top = rect_text(&buffer, Rect::new(rect.x, rect.y, rect.width, 1));
                    assert!(top.contains(title), "{case}: {top}");
                    assert!(rect.bottom() <= feed.y, "{case}: {rect:?} {feed:?}");
                    assert_eq!(rect.y, first.y, "{case}: {rect:?}");
                    assert!(rect.x >= previous_right, "{case}: {rect:?}");
                    assert!(rect.right() <= width, "{case}: {rect:?}");
                    previous_right = rect.right();
                }
                // 줄 경계와 차트 카드 사이 경계는 끌 수 있음 (격자 줄 3)
                let split_hits = &state.hit_areas.split_hits;
                assert!(
                    split_hits.iter().any(|hit| matches!(
                        hit.group,
                        SplitGroup::GridRows(ActiveTab::Overview, _)
                    )),
                    "{case}"
                );
                let column_grips = split_hits
                    .iter()
                    .filter(|hit| hit.group == SplitGroup::GridCols(ActiveTab::Overview, 3))
                    .count();
                assert_eq!(column_grips, 2, "{case}");
            }
        }
    }

    #[test]
    fn the_chart_row_is_left_out_when_the_body_is_too_low_for_its_reserve() {
        for lang in [Lang::En, Lang::Ko] {
            for (width, height) in [(60u16, 16u16), (100, 30), (130, 30)] {
                let mut state = activity_state(lang);
                draw(&mut state, width, height);
                let case = format!("{lang:?} {width}x{height}");
                for card in CHART_CARDS {
                    assert!(card_rect(&state, card).is_none(), "{case}: {card:?}");
                }
                assert!(
                    state
                        .hit_areas
                        .split_hits
                        .iter()
                        .all(|hit| hit.group != SplitGroup::GridCols(ActiveTab::Overview, 3)),
                    "{case}"
                );
                assert!(card_rect(&state, CardId::EventFeed).is_some(), "{case}");
            }
        }
    }

    #[test]
    fn the_activity_chart_plots_each_series_in_its_color_with_a_worded_legend() {
        let cases = [
            (
                Lang::En,
                ["Requests 10", "Tools 13", "Deny 0"],
                "peak 9",
                "-60m",
            ),
            (
                Lang::Ko,
                ["요청 10", "도구 13", "거부 0"],
                "최대 9",
                "-60분",
            ),
        ];
        for (lang, legend, peak, x_label) in cases {
            let mut state = activity_state(lang);
            let buffer = draw(&mut state, 130, 40);
            let rect = card_rect(&state, CardId::ChartActivity).expect("activity card");
            let text = rect_text(&buffer, rect);
            for item in legend {
                assert!(text.contains(item), "{lang:?}: {text}");
            }
            assert!(text.contains(peak), "{lang:?}: {text}");
            assert!(text.contains(x_label), "{lang:?}: {text}");

            // 범례 표식은 계열 색, 선은 값이 있는 계열만 (거부는 전부 0 이라 범례만)
            let legend_row = Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, 1);
            let legend_marks = rect_marks(&buffer, legend_row);
            for color in [theme::COLOR_API, theme::COLOR_TOOL, theme::COLOR_HOOK] {
                assert!(
                    legend_marks
                        .iter()
                        .any(|(symbol, fg)| symbol == "━" && *fg == color),
                    "{lang:?}: {color:?}"
                );
            }
            let plot_marks = rect_marks(&buffer, plot_rect(rect));
            for color in [theme::COLOR_API, theme::COLOR_TOOL] {
                assert!(
                    plot_marks.iter().any(|(_, fg)| *fg == color),
                    "{lang:?}: {color:?}"
                );
            }
            assert!(
                plot_marks.iter().all(|(_, fg)| *fg != theme::COLOR_HOOK),
                "{lang:?}"
            );
            // 포커스·강조색은 그래프에 쓰지 않음
            assert!(
                plot_marks
                    .iter()
                    .all(|(_, fg)| *fg != theme::BORDER_FOCUS && *fg != theme::FG_ACCENT),
                "{lang:?}"
            );
        }
    }

    // 범례가 다 담기지 않는 폭: 값을 빼고 이름은 모두 남김 (선만 남는 계열이 없음)
    #[test]
    fn a_narrow_activity_legend_drops_values_before_names_so_no_line_lacks_a_name() {
        let cases = [
            (Lang::En, ["Requests", "Tools", "Deny"]),
            (Lang::Ko, ["요청", "도구", "거부"]),
        ];
        for (lang, names) in cases {
            let mut state = activity_state(lang);
            let Panel::Ready(buckets) = &mut state.snapshot.minutes else {
                panic!("minute buckets");
            };
            buckets[0].api_requests = 5_230;
            buckets[0].tool_calls = 12_070;
            // 거부는 다른 분에 둠 (같은 분이면 위에 그리는 계열의 선에 가려짐)
            buckets[1].hook_denials = 4_100;
            let buffer = draw(&mut state, 110, 40);
            let rect = card_rect(&state, CardId::ChartActivity).expect("activity card");
            let legend_row = Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, 1);
            let legend = rect_text(&buffer, legend_row);
            for name in names {
                assert!(legend.contains(name), "{lang:?}: {legend}");
            }
            assert!(!legend.contains("5,2"), "{lang:?}: {legend}");
            let plot_marks = rect_marks(&buffer, plot_rect(rect));
            for color in [theme::COLOR_API, theme::COLOR_TOOL, theme::COLOR_HOOK] {
                assert!(
                    plot_marks.iter().any(|(_, fg)| *fg == color),
                    "{lang:?}: {color:?}"
                );
            }
        }
    }

    #[test]
    fn the_activity_chart_shows_the_panel_note_until_minute_data_is_ready() {
        let mut state = activity_state(Lang::En);
        state.snapshot.minutes = Panel::Pending;
        let buffer = draw(&mut state, 130, 40);
        let rect = card_rect(&state, CardId::ChartActivity).expect("activity card");
        let text = rect_text(&buffer, rect);
        assert!(
            text.contains(i18n::text(Lang::En, Msg::PanelPending)),
            "{text}"
        );
        assert!(
            rect_marks(&buffer, plot_rect(rect))
                .iter()
                .all(|(_, fg)| *fg != theme::COLOR_API)
        );
    }

    #[test]
    fn token_mix_and_top_tools_draw_bars_with_values_shares_and_error_flags() {
        let cases = [
            (Lang::En, ["In", "Out", "Cache read", "Cache write"]),
            (Lang::Ko, ["입력", "출력", "캐시 읽기", "캐시 쓰기"]),
        ];
        for (lang, labels) in cases {
            for (width, height) in [(110u16, 40u16), (130, 40)] {
                let mut state = activity_state(lang);
                let buffer = draw(&mut state, width, height);
                let case = format!("{lang:?} {width}x{height}");

                // 토큰 구성: 이름은 잘리지 않고 값과 전체 대비 비율이 붙음 (캐시 읽기 9.0M / 12.41M = 73%)
                let tokens = card_rect(&state, CardId::ChartTokens).expect("token card");
                let text = rect_text(&buffer, tokens);
                for label in labels {
                    assert!(text.contains(label), "{case}: {text}");
                }
                assert!(text.contains("9.0M  73%"), "{case}: {text}");
                assert!(text.contains("410.0K   3%"), "{case}: {text}");
                assert!(!text.contains('…'), "{case}: {text}");

                // 도구 상위: 호출 수 내림차순, 오류는 적색 굵은 !건수
                let tools = card_rect(&state, CardId::ChartTools).expect("tools card");
                let text = rect_text(&buffer, tools);
                let read_at = text.find("Read").expect("Read row");
                let bash_at = text.find("Bash").expect("Bash row");
                let edit_at = text.find("Edit").expect("Edit row");
                assert!(read_at < bash_at && bash_at < edit_at, "{case}: {text}");
                assert!(
                    text.contains("1,300") && text.contains("!9"),
                    "{case}: {text}"
                );
                let marks = rect_marks(&buffer, tools);
                assert!(
                    marks
                        .iter()
                        .any(|(symbol, fg)| symbol == "!" && *fg == theme::COLOR_CRIT),
                    "{case}"
                );
            }
        }

        // 넓은 카드에서는 가장 큰 항목이 막대를 가득 채움 (막대 색은 카드별 계열 색)
        let mut state = activity_state(Lang::En);
        let buffer = draw(&mut state, 200, 60);
        let bar_cards = [
            (CardId::ChartTokens, theme::CHART_TOKENS),
            (CardId::ChartTools, theme::COLOR_TOOL),
        ];
        for (card, color) in bar_cards {
            let rect = card_rect(&state, card).expect("bar card");
            let marks = rect_marks(&buffer, rect);
            assert!(
                marks
                    .iter()
                    .any(|(symbol, fg)| symbol == "█" && *fg == color),
                "{card:?}"
            );
            assert!(marks.iter().any(|(symbol, _)| symbol == "░"), "{card:?}");
        }
    }
}
