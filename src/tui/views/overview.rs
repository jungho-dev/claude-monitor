//! tui/views/overview.rs
//! 개요 탭: 서버 상태 카드, KPI 카드, 절약 카드, 분당 추이, 실시간 이벤트 피드와 이벤트 상세.

use crate::core::model::{LiveEvent, PortStatus};
use crate::store::limits::LimitSnapshot;
use crate::store::query::MinuteBucket;
use crate::store::ratelimit::RatelimitSnapshotRow;
use crate::tui::cards::CardId;
use crate::tui::format::{
    fit_display, format_compact, format_count, format_duration_ms, pad_left_display,
};
use crate::tui::i18n::{self, Lang, Msg};
use crate::tui::layout::{SplitAxis, SplitGroup};
use crate::tui::sort::{compare_event_row, sort_rows};
use crate::tui::state::{AppState, DbState, LimitsData, Panel};
use crate::tui::theme;
use crate::tui::views::common::{
    CardTable, PanelNote, TableCell, TextCard, clip_spans_to_width, interactive_card,
    pack_spans_to_width, panel_message, render_card_table, render_db_gate, render_notice_card,
    render_text_card,
};
use crate::tui::views::feed::{
    FEED_COLUMNS, build_event_context_lines, build_event_detail_lines, build_feed_row,
};
use chrono::{DateTime, Local, NaiveDateTime, Utc};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Padding, Paragraph, Sparkline, Wrap};

const SPARK_MINUTES: usize = 60;
const CACHE_CARD_INDEX: usize = 2;
const HEADER_HEIGHT: u16 = 4;
const KPI_HEIGHT: u16 = 4;
const TREND_HEIGHT: u16 = 5;
// 본문이 이 높이 이상이면 추이 줄을 한 줄 키워 한도 카드에 API 키 줄까지 넣음
const TALL_LAYOUT_MIN_HEIGHT: u16 = 24;
const TREND_HEIGHT_TALL: u16 = 6;
// 분당 추이 카드가 읽을 수 있는 최소 폭 (이 밑으로 내려가면 두 추이 카드를 숨김)
const TREND_MIN_WIDTH: u16 = 12;
// 한도 카드: 공식 스냅샷이 이 시간(초)보다 오래되면 흐리게, 막대·퍼센트 색 기준(%)
pub(crate) const LIMIT_STALE_SECS: i64 = 1_800;
const LIMIT_WARN_PCT: f64 = 80.0;
const LIMIT_CRIT_PCT: f64 = 95.0;
// 한도 창 길이(초), 페이스 판정 허용 차(%p), 소진 예상 계산에 필요한 최소 기록 간격·되돌아볼 길이(초)
const FIVE_HOUR_SECS: i64 = 5 * 3_600;
const SEVEN_DAY_SECS: i64 = 7 * 86_400;
const PACE_TOLERANCE_PCT: f64 = 5.0;
const PROJECTION_MIN_SPAN_SECS: i64 = 300;
const PROJECTION_LOOKBACK_SECS: i64 = 3 * 3_600;
// 한도 줄 열 폭: 이름 칸, 퍼센트, 막대 최소·최대
const LIMIT_LABEL_WIDTH: usize = 3;
const LIMIT_PCT_WIDTH: usize = 4;
const BAR_MIN_WIDTH: usize = 4;
const BAR_MAX_WIDTH: usize = 10;
// 최소 막대로 리셋·페이스·소진 예상까지 다 담는 줄 폭 (이보다 넓으면 막대가 길어짐)
const LIMIT_FULL_ROW_WIDTH: usize = 42;
// 추이 줄까지 넣을 수 있는 최소 본문 높이
const FULL_LAYOUT_MIN_HEIGHT: u16 = 18;
// 이 폭 이상 카드만 안쪽 가로 여백 1열
const KPI_PADDING_MIN_WIDTH: u16 = 12;
// 절약 카드 항목 이름 폭과 값 열 폭
const SAVINGS_LABEL_WIDTH: usize = 14;
const SAVINGS_VALUE_WIDTH: usize = 9;
const KPI_CARD_IDS: [CardId; 6] = [
    CardId::KpiRequests,
    CardId::KpiTokens,
    CardId::KpiCache,
    CardId::KpiWait,
    CardId::KpiTools,
    CardId::KpiHooks,
];

// 1. 개요 탭 ------------------------------------------------------------------
pub fn render_overview_tab(frame: &mut Frame, area: Rect, state: &mut AppState) {
    // 1. DB 없음: 상태 카드만 남기고 아래 전체를 빈 상태로
    if !matches!(state.snapshot.db_state, DbState::Open) {
        let header_height = HEADER_HEIGHT.min(area.height);
        let header_area = Rect::new(area.x, area.y, area.width, header_height);
        render_status_header(frame, state, header_area);
        let rest = Rect::new(
            area.x,
            area.y + header_height,
            area.width,
            area.height - header_height,
        );
        render_db_gate(frame, state, rest);
        return;
    }

    // 2. 카드 줄 분할 (낮은 화면은 추이 줄 생략), 줄 경계는 드래그로 조절
    let header_cut = HEADER_HEIGHT;
    let kpi_cut = HEADER_HEIGHT + KPI_HEIGHT;
    let (header_area, kpi_area, trend_area, feed_area) = if area.height >= FULL_LAYOUT_MIN_HEIGHT {
        let trend_height = if area.height >= TALL_LAYOUT_MIN_HEIGHT {
            TREND_HEIGHT_TALL
        } else {
            TREND_HEIGHT
        };
        let rows = state.card_layout.split_cards(
            &mut state.hit_areas,
            SplitGroup::OverviewRows,
            SplitAxis::Rows,
            area,
            &[header_cut, kpi_cut, kpi_cut + trend_height],
            &[4, 4, 5, 3],
        );
        (rows[0], rows[1], Some(rows[2]), rows[3])
    } else {
        let rows = state.card_layout.split_cards(
            &mut state.hit_areas,
            SplitGroup::OverviewCompactRows,
            SplitAxis::Rows,
            area,
            &[header_cut, kpi_cut],
            &[3, 3, 3],
        );
        (rows[0], rows[1], None, rows[2])
    };

    // 3. 줄별 카드
    render_status_header(frame, state, header_area);
    render_kpi_cards(frame, state, kpi_area);
    if let Some(trend_area) = trend_area {
        render_savings_and_trends(frame, state, trend_area);
    }
    render_feed_row(frame, state, feed_area);
}

// 2. 서버 상태 카드 -----------------------------------------------------------
// 항목 이름은 흐리게, 값은 본문색, 항목 사이는 3칸
fn render_status_header(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let lang = state.lang;
    let block = interactive_card(
        state,
        CardId::ServerStatus,
        area,
        i18n::text(lang, Msg::TitleServerStatus),
        "",
        theme::BORDER_DIM,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let snapshot = &state.snapshot;

    // 1. serve 실행 상태 줄 (항목은 폭에 맞게 뒤쪽부터 통째로 덜어냄)
    let mut status_items: Vec<Vec<Span<'static>>> = Vec::new();
    match &snapshot.status {
        Panel::Ready(status) => {
            status_items.push(vec![Span::styled(
                format!("● {}", i18n::text(lang, Msg::ServeRunning)),
                theme::STYLE_OK,
            )]);
            let pairs = [
                (
                    i18n::text(lang, Msg::StatusUptime),
                    format_duration_ms(
                        i64::try_from(status.uptime_secs).unwrap_or(i64::MAX) * 1_000,
                    ),
                ),
                ("pid", status.pid.to_string()),
                (
                    i18n::text(lang, Msg::StatusVersion),
                    format!("v{}", status.version),
                ),
                (
                    i18n::text(lang, Msg::StatusOtlpBatches),
                    format_count(i64::try_from(status.counters.otlp_batches).unwrap_or(0)),
                ),
                (
                    i18n::text(lang, Msg::StatusIndexRuns),
                    format_count(i64::try_from(status.counters.index_runs).unwrap_or(0)),
                ),
                (
                    i18n::text(lang, Msg::StatusHookCalls),
                    format_count(i64::try_from(status.counters.hook_calls).unwrap_or(0)),
                ),
            ];
            for (label, value) in pairs {
                status_items.push(vec![
                    Span::styled(format!("{label} "), theme::STYLE_DIM),
                    Span::styled(value, theme::STYLE_TEXT),
                ]);
            }
        }
        Panel::Pending => {
            status_items.push(vec![Span::styled(
                format!("○ {}", i18n::text(lang, Msg::ServeChecking)),
                theme::STYLE_DIM,
            )]);
        }
        Panel::Failed(failure) => {
            status_items.push(vec![Span::styled(
                format!("○ {}", i18n::failure_text(lang, failure)),
                theme::STYLE_CRIT,
            )]);
            status_items.push(vec![Span::styled(
                i18n::text(lang, Msg::ServeRunHint),
                theme::STYLE_DIM,
            )]);
        }
    }
    let status_spans = pack_spans_to_width(status_items, &[3, 1], usize::from(inner.width));

    // 2. 포트 줄 (서버 응답이 없으면 설정 포트를 흐리게)
    let known = matches!(snapshot.status, Panel::Ready(_));
    let ports: Vec<PortStatus> = match &snapshot.status {
        Panel::Ready(status) => status.ports.clone(),
        _ => vec![
            PortStatus {
                name: "admin".to_string(),
                port: state.config.ports.admin,
                enabled: true,
                listening: false,
            },
            PortStatus {
                name: "otlp".to_string(),
                port: state.config.ports.otlp,
                enabled: true,
                listening: false,
            },
            PortStatus {
                name: "gateway".to_string(),
                port: state.config.ports.gateway,
                enabled: state.config.gateway.enabled,
                listening: false,
            },
            PortStatus {
                name: "hooks".to_string(),
                port: state.config.ports.hooks,
                enabled: state.config.hooks.enabled,
                listening: false,
            },
        ],
    };
    // 켜짐은 채운 점, 꺼짐·알 수 없음은 빈 점, 켜야 하는데 닫힘은 빈 점에 굵은 적색 (색만으로 구분하지 않음)
    let mut port_items: Vec<Vec<Span<'static>>> = Vec::new();
    for port in &ports {
        let (dot, dot_style) = if !known || !port.enabled {
            ("○", theme::STYLE_DIM)
        } else if port.listening {
            ("●", theme::STYLE_OK)
        } else {
            ("○", theme::STYLE_CRIT.add_modifier(Modifier::BOLD))
        };
        port_items.push(vec![
            Span::styled(dot, dot_style),
            Span::styled(format!(" {} ", port.name), theme::STYLE_DIM),
            Span::styled(port.port.to_string(), theme::STYLE_TEXT),
        ]);
    }
    let gateway_on = ports
        .iter()
        .find(|port| port.name == "gateway")
        .map_or(state.config.gateway.enabled, |port| port.enabled);
    port_items.push(vec![if gateway_on {
        Span::styled(i18n::text(lang, Msg::GatewayOn), theme::STYLE_OK)
    } else {
        Span::styled(i18n::text(lang, Msg::GatewayOff), theme::STYLE_DIM)
    }]);
    let port_spans = pack_spans_to_width(port_items, &[3, 1], usize::from(inner.width));

    frame.render_widget(
        Paragraph::new(vec![Line::from(status_spans), Line::from(port_spans)]),
        inner,
    );
}

// 3. KPI 카드 -----------------------------------------------------------------
// 값 줄은 숫자 굵게·라벨 흐리게, 폭이 모자라면 짧은 형태로, 보조 줄은 흐리게
fn render_kpi_cards(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // 1. 집계 준비 확인
    let lang = state.lang;
    let word = |msg: Msg| i18n::text(lang, msg);
    let overview = match &state.snapshot.overview {
        Panel::Ready(stats) => stats.clone(),
        other => {
            let note = panel_message(lang, other).unwrap_or_default();
            let title = word(Msg::TitleSummary);
            render_notice_card(frame, state, CardId::KpiRequests, area, title, note);
            return;
        }
    };

    // 2. 카드 내용 (제목, 짧은 제목, 값 줄 후보(긴 형태부터), 보조 줄)
    let strong = theme::STYLE_STRONG;
    let dim = theme::STYLE_DIM;
    let faint = theme::STYLE_FAINT;
    let tool_calls = i64::try_from(overview.tool_calls).unwrap_or(i64::MAX);
    let tool_errors = i64::try_from(overview.tool_errors).unwrap_or(i64::MAX);
    let hook_denials = i64::try_from(overview.hook_denials).unwrap_or(i64::MAX);
    let hook_rewrites = i64::try_from(overview.hook_rewrites).unwrap_or(i64::MAX);
    let error_rate = if overview.tool_calls > 0 {
        overview.tool_errors as f64 * 100.0 / overview.tool_calls as f64
    } else {
        0.0
    };
    let error_base = if error_rate >= 10.0 {
        theme::STYLE_CRIT
    } else if error_rate >= 3.0 {
        theme::STYLE_WARN
    } else if overview.tool_errors > 0 {
        theme::STYLE_TEXT
    } else {
        theme::STYLE_DIM
    };
    let error_style = error_base.add_modifier(Modifier::BOLD);
    let rewrite_share = if hook_denials + hook_rewrites > 0 {
        format!(
            "{:.0}%",
            hook_rewrites as f64 * 100.0 / (hook_denials + hook_rewrites) as f64
        )
    } else {
        "-".to_string()
    };
    let input_text = format_compact(overview.input_tokens);
    let output_text = format_compact(overview.output_tokens);
    let wait_p50 = overview
        .model_wait_p50_ms
        .map_or("-".to_string(), format_duration_ms);
    let wait_p90 = overview
        .model_wait_p90_ms
        .map_or("-".to_string(), format_duration_ms);
    let cards: [(&str, &str, Vec<Line<'static>>, Vec<Line<'static>>); 6] = [
        (
            word(Msg::KpiRequests),
            word(Msg::KpiRequestsShort),
            vec![Line::from(vec![
                Span::styled(
                    format_count(i64::try_from(overview.api_requests).unwrap_or(i64::MAX)),
                    strong,
                ),
                Span::styled(format!(" {}", word(Msg::KpiRequestsUnit)), dim),
            ])],
            vec![Line::from(Span::styled(
                i18n::sessions_detail(lang, overview.sessions),
                dim,
            ))],
        ),
        (
            word(Msg::KpiTokens),
            word(Msg::KpiTokensShort),
            vec![
                Line::from(vec![
                    Span::styled(format!("{} ", word(Msg::KpiInput)), dim),
                    Span::styled(input_text.clone(), strong),
                    Span::raw("  "),
                    Span::styled(format!("{} ", word(Msg::KpiOutput)), dim),
                    Span::styled(output_text.clone(), strong),
                ]),
                Line::from(vec![
                    Span::styled(input_text.clone(), strong),
                    Span::styled(" / ", faint),
                    Span::styled(output_text.clone(), strong),
                ]),
                Line::from(vec![
                    Span::styled(input_text, strong),
                    Span::styled("/", faint),
                    Span::styled(output_text, strong),
                ]),
            ],
            vec![
                Line::from(Span::styled(
                    i18n::cache_write_detail(
                        lang,
                        &format_compact(overview.cache_creation_tokens),
                        false,
                    ),
                    dim,
                )),
                Line::from(Span::styled(
                    i18n::cache_write_detail(
                        lang,
                        &format_compact(overview.cache_creation_tokens),
                        true,
                    ),
                    dim,
                )),
            ],
        ),
        (
            word(Msg::KpiCache),
            word(Msg::KpiCacheShort),
            vec![Line::from("")],
            vec![Line::from(Span::styled(
                i18n::cache_read_detail(lang, &format_compact(overview.cache_read_tokens)),
                dim,
            ))],
        ),
        (
            word(Msg::KpiWait),
            word(Msg::KpiWaitShort),
            vec![
                Line::from(vec![
                    Span::styled("p50 ", dim),
                    Span::styled(wait_p50.clone(), strong),
                    Span::raw("  "),
                    Span::styled("p90 ", dim),
                    Span::styled(wait_p90.clone(), strong),
                ]),
                Line::from(vec![
                    Span::styled(wait_p50.clone(), strong),
                    Span::styled(" / ", faint),
                    Span::styled(wait_p90.clone(), strong),
                ]),
                Line::from(vec![
                    Span::styled(wait_p50, strong),
                    Span::styled("/", faint),
                    Span::styled(wait_p90, strong),
                ]),
            ],
            vec![
                Line::from(Span::styled(word(Msg::KpiWaitDetail), dim)),
                Line::from(Span::styled(word(Msg::KpiWaitDetailShort), dim)),
            ],
        ),
        (
            word(Msg::KpiTools),
            word(Msg::KpiToolsShort),
            vec![
                Line::from(vec![
                    Span::styled(format!("{} ", word(Msg::KpiCalls)), dim),
                    Span::styled(format_count(tool_calls), strong),
                    Span::raw("  "),
                    Span::styled(format!("{} ", word(Msg::KpiErrors)), dim),
                    Span::styled(format_count(tool_errors), error_style),
                ]),
                Line::from(vec![
                    Span::styled(format_count(tool_calls), strong),
                    Span::styled(" / ", faint),
                    Span::styled(format_count(tool_errors), error_style),
                ]),
                Line::from(vec![
                    Span::styled(format_count(tool_calls), strong),
                    Span::styled("/", faint),
                    Span::styled(format_count(tool_errors), error_style),
                ]),
            ],
            vec![Line::from(Span::styled(
                i18n::error_rate_detail(lang, error_rate),
                dim,
            ))],
        ),
        (
            word(Msg::KpiHooks),
            word(Msg::KpiHooksShort),
            vec![
                Line::from(vec![
                    Span::styled(format!("{} ", word(Msg::KpiDeny)), dim),
                    Span::styled(
                        format_count(hook_denials),
                        theme::STYLE_CRIT.add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(format!("{} ", word(Msg::KpiRewrite)), dim),
                    Span::styled(
                        format_count(hook_rewrites),
                        theme::STYLE_OK.add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::styled(
                        format_count(hook_denials),
                        theme::STYLE_CRIT.add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" / ", faint),
                    Span::styled(
                        format_count(hook_rewrites),
                        theme::STYLE_OK.add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::styled(
                        format_count(hook_denials),
                        theme::STYLE_CRIT.add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("/", faint),
                    Span::styled(
                        format_count(hook_rewrites),
                        theme::STYLE_OK.add_modifier(Modifier::BOLD),
                    ),
                ]),
            ],
            vec![
                Line::from(Span::styled(
                    i18n::rewrite_share_detail(lang, &rewrite_share, false),
                    dim,
                )),
                Line::from(Span::styled(
                    i18n::rewrite_share_detail(lang, &rewrite_share, true),
                    dim,
                )),
            ],
        ),
    ];

    // 3. 카드 분할 (기본 균등 6칸, 카드 사이 경계 드래그)
    let default_columns = Layout::horizontal([Constraint::Fill(1); 6])
        .spacing(1)
        .split(area);
    let default_cuts: Vec<u16> = default_columns[..5]
        .iter()
        .map(|column| column.right() - area.x)
        .collect();
    let columns = state.card_layout.split_cards(
        &mut state.hit_areas,
        SplitGroup::OverviewKpiCols,
        SplitAxis::Columns,
        area,
        &default_cuts,
        &[8; 6],
    );

    // 4. 카드 그리기 (캐시 카드는 게이지, 줄은 폭에 맞는 첫 후보)
    let pick_fit = |variants: Vec<Line<'static>>, width: usize| -> Line<'static> {
        // 4-1. 폭에 맞는 첫 후보
        let last = variants.len() - 1;
        let chosen = variants
            .into_iter()
            .enumerate()
            .find(|(position, line)| line.width() <= width || *position == last)
            .map(|(_, line)| line)
            .unwrap_or_default();
        let mut spans = chosen.spans;
        let mut used: usize = spans.iter().map(Span::width).sum();
        if used <= width {
            return Line::from(spans);
        }

        // 4-2. 넘침 말줄임 (뒤 조각부터 덜어냄)
        while spans.len() > 1 && used + 1 > width {
            if let Some(tail) = spans.pop() {
                used -= tail.width();
            }
        }
        let tail_style = spans.last().map_or(Style::default(), |span| span.style);
        spans.push(Span::styled("…", tail_style));
        Line::from(clip_spans_to_width(spans, width))
    };
    for (index, (full_title, short_title, value_lines, detail_lines)) in
        cards.into_iter().enumerate()
    {
        let pane = columns[index];
        // 카드 폭이 모자라면 짧은 제목
        let title = if Span::raw(full_title).width() + 4 <= pane.width as usize {
            full_title
        } else {
            short_title
        };
        let mut block = interactive_card(
            state,
            KPI_CARD_IDS[index],
            pane,
            title,
            "",
            theme::BORDER_DIM,
        );
        if pane.width >= KPI_PADDING_MIN_WIDTH {
            block = block.padding(Padding::horizontal(1));
        }
        let inner = block.inner(pane);
        frame.render_widget(block, pane);
        if inner.height < 2 {
            continue;
        }
        let first_row = Rect::new(inner.x, inner.y, inner.width, 1);
        let second_row = Rect::new(inner.x, inner.y + 1, inner.width, 1);
        if index == CACHE_CARD_INDEX {
            let ratio = overview.cache_hit_ratio.clamp(0.0, 1.0);
            let gauge_color = if ratio >= 0.8 {
                theme::COLOR_OK
            } else if ratio >= 0.5 {
                theme::COLOR_WARN
            } else {
                theme::COLOR_CRIT
            };
            let gauge = Gauge::default()
                .ratio(ratio)
                .label(Span::styled(
                    format!("{:.1}%", ratio * 100.0),
                    Style::new().add_modifier(Modifier::BOLD),
                ))
                .gauge_style(Style::default().fg(gauge_color).bg(theme::GAUGE_TRACK));
            frame.render_widget(gauge, first_row);
        } else {
            let value_line = pick_fit(value_lines, inner.width as usize);
            frame.render_widget(Paragraph::new(value_line), first_row);
        }
        let detail_line = pick_fit(detail_lines, inner.width as usize);
        frame.render_widget(Paragraph::new(detail_line), second_row);
    }
}

// 4. 절약 카드와 분당 추이 ----------------------------------------------------
fn render_savings_and_trends(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let lang = state.lang;
    let word = |msg: Msg| i18n::text(lang, msg);
    // 1. 카드 분할 (오른쪽 끝 한도 카드 34~46열, 남는 폭에서 절약 30%를 31~40열로, 나머지 둘은 균등, 좋으면 한도 카드 폭을 먼저 지킴)
    let width = u32::from(area.width);
    let limits_width = (width * 38 / 100).clamp(38, 48).min(width * 45 / 100);
    let rest = width.saturating_sub(limits_width + 1);
    let first_cut = (rest * 30 / 100).clamp(28, 40).min(rest / 2);
    let second_cut = first_cut + 1 + rest.saturating_sub(first_cut + 1) / 2;
    // 추이 카드가 읽을 수 있는 최소 폭(TREND_MIN_WIDTH) 밑이면 두 추이 카드를 숨기고 한도·절약 카드가 폭을 나눔 (추이는 우선순위가 가장 낮음, 칸 위치와 무관)
    let requests_width = second_cut.saturating_sub(first_cut + 1);
    let tokens_width = rest.saturating_sub(second_cut + 1);
    let trends_hidden = requests_width.min(tokens_width) < u32::from(TREND_MIN_WIDTH);
    let group = SplitGroup::OverviewTrendCols;
    let mut layout = state.card_layout;
    let (cuts, slot_mins) = if trends_hidden {
        layout.clear_ratios(group);
        let wide_limits = (width * 50 / 100).clamp(38, 48).min(width * 55 / 100);
        layout.visible_slot_plan(
            group,
            &[1, 2],
            &[(width - wide_limits - 1) as u16, 0, 0, wide_limits as u16],
            &[24, 16, 16, 28],
            area.width,
        )
    } else {
        layout.visible_slot_plan(
            group,
            &[],
            &[
                first_cut as u16,
                requests_width as u16,
                tokens_width as u16,
                limits_width as u16,
            ],
            &[24, 16, 16, 28],
            area.width,
        )
    };
    let panes = layout.split_cards(
        &mut state.hit_areas,
        group,
        SplitAxis::Columns,
        area,
        &cuts,
        &slot_mins,
    );
    render_limits_card(frame, state, panes[3]);

    // 2. 오늘 절약 시간 대 거부 손실 추정
    let today = match &state.snapshot.today {
        Panel::Ready(stats) => Some(stats.clone()),
        _ => None,
    };
    match today {
        Some(today) => {
            let net = today.saved_ms - today.lost_ms_estimate;
            let net_style = if net >= 0 {
                theme::STYLE_OK
            } else {
                theme::STYLE_CRIT
            };
            let block = interactive_card(
                state,
                CardId::SavingsEffect,
                panes[0],
                word(Msg::TitleSavings),
                "",
                theme::BORDER_DIM,
            )
            .padding(Padding::horizontal(1));
            let inner = block.inner(panes[0]);
            frame.render_widget(block, panes[0]);
            let lines = vec![
                Line::from(vec![
                    Span::styled(
                        fit_display(word(Msg::SavingsSaved), SAVINGS_LABEL_WIDTH),
                        theme::STYLE_DIM,
                    ),
                    Span::styled(
                        pad_left_display(
                            &format!("+{}", format_duration_ms(today.saved_ms)),
                            SAVINGS_VALUE_WIDTH,
                        ),
                        theme::STYLE_OK.add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::styled(
                        fit_display(word(Msg::SavingsLoss), SAVINGS_LABEL_WIDTH),
                        theme::STYLE_DIM,
                    ),
                    Span::styled(
                        pad_left_display(
                            &format!("-{}", format_duration_ms(today.lost_ms_estimate)),
                            SAVINGS_VALUE_WIDTH,
                        ),
                        theme::STYLE_CRIT.add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::styled(
                        fit_display(word(Msg::SavingsNet), SAVINGS_LABEL_WIDTH),
                        theme::STYLE_TEXT,
                    ),
                    Span::styled(
                        pad_left_display(
                            &format!(
                                "{}{}",
                                if net >= 0 { "+" } else { "-" },
                                format_duration_ms(net.abs())
                            ),
                            SAVINGS_VALUE_WIDTH,
                        ),
                        net_style.add_modifier(Modifier::BOLD),
                    ),
                ]),
            ];
            frame.render_widget(Paragraph::new(lines), inner);
        }
        None => {
            let note = panel_message(lang, &state.snapshot.today).unwrap_or_default();
            render_notice_card(
                frame,
                state,
                CardId::SavingsEffect,
                panes[0],
                word(Msg::TitleSavings),
                note,
            );
        }
    }

    // 2-1. 추이 카드를 숨긴 폭에서는 그리지 않고 경계 끌기 영역도 둔다 (숨긴 칸과 겹치는 경계를 잡아 비율이 사라지는 일 방지)
    if trends_hidden {
        state
            .hit_areas
            .split_hits
            .retain(|hit| hit.group != SplitGroup::OverviewTrendCols);
        return;
    }

    // 3. 분당 추이가 준비되지 않으면 두 카드가 이웃한 칸일 때 하나의 안내 카드로 (사이 경계는 제거),
    // 순서를 바꿔 떨어져 있으면 카드마다 안내 카드
    if let Some(note) = panel_message(lang, &state.snapshot.minutes) {
        let request_slot = state.card_layout.slot_of(SplitGroup::OverviewTrendCols, 1);
        let token_slot = state.card_layout.slot_of(SplitGroup::OverviewTrendCols, 2);
        match (request_slot, token_slot) {
            (Some(request_slot), Some(token_slot)) if request_slot.abs_diff(token_slot) == 1 => {
                let joint = request_slot.min(token_slot);
                state.hit_areas.split_hits.retain(|hit| {
                    !(hit.group == SplitGroup::OverviewTrendCols && hit.index == joint)
                });
                render_notice_card(
                    frame,
                    state,
                    CardId::TrendRequests,
                    panes[1].union(panes[2]),
                    word(Msg::TitleTrend),
                    note,
                );
            }
            _ => {
                render_notice_card(
                    frame,
                    state,
                    CardId::TrendRequests,
                    panes[1],
                    word(Msg::TitleRequestsPerMin),
                    note.clone(),
                );
                render_notice_card(
                    frame,
                    state,
                    CardId::TrendTokens,
                    panes[2],
                    word(Msg::TitleTokensPerMin),
                    note,
                );
            }
        }
        return;
    }

    // 4. 분당 요청·출력 토큰 스파크라인 (분마다 k칸, 지금이 오른쪽 끝)
    let now = state.snapshot.taken_at.unwrap_or_else(Utc::now);
    let Panel::Ready(buckets) = &state.snapshot.minutes else {
        return;
    };
    let request_series = build_minute_series(buckets, now, |bucket| bucket.api_requests);
    let token_series = build_minute_series(buckets, now, |bucket| {
        u64::try_from(bucket.output_tokens).unwrap_or(0)
    });
    let trends: [(CardId, &str, &Vec<u64>, Color, Rect); 2] = [
        (
            CardId::TrendRequests,
            word(Msg::TitleRequestsPerMin),
            &request_series,
            theme::CHART_REQUESTS,
            panes[1],
        ),
        (
            CardId::TrendTokens,
            word(Msg::TitleTokensPerMin),
            &token_series,
            theme::CHART_TOKENS,
            panes[2],
        ),
    ];
    for (card, title, series, color, rect) in trends {
        let peak = series.iter().copied().max().unwrap_or(0);
        let mut block = interactive_card(
            state,
            card,
            rect,
            &i18n::trend_title(lang, title, &format_compact(peak as i64)),
            "",
            theme::BORDER_DIM,
        );
        if rect.width >= 24 {
            block = block
                .title_bottom(
                    Line::from(Span::styled(
                        format!(" {} ", word(Msg::TrendMinutesAgo)),
                        theme::STYLE_DIM,
                    ))
                    .left_aligned(),
                )
                .title_bottom(
                    Line::from(Span::styled(
                        format!(" {} ", word(Msg::TrendNow)),
                        theme::STYLE_DIM,
                    ))
                    .right_aligned(),
                );
        }
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        if inner.is_empty() {
            continue;
        }
        let repeat = (usize::from(inner.width) / SPARK_MINUTES).max(1);
        let tail = &series[series.len().saturating_sub(usize::from(inner.width))..];
        let expanded: Vec<u64> = tail
            .iter()
            .flat_map(|value| std::iter::repeat_n(*value, repeat))
            .collect();
        let drawn_width = (expanded.len() as u16).min(inner.width);
        let spark_area = Rect::new(
            inner.right() - drawn_width,
            inner.y,
            drawn_width,
            inner.height,
        );
        frame.render_widget(
            Sparkline::default()
                .data(expanded.iter())
                .style(Style::default().fg(color)),
            spark_area,
        );
    }
}

// 5. 분 단위 시계열 -----------------------------------------------------------
// 분 문자열을 해석할 수 있으면 현재 분 기준 60칸에 배치, 아니면 조회 순서의 마지막 60개
fn build_minute_series(
    buckets: &[MinuteBucket],
    now: DateTime<Utc>,
    pick: fn(&MinuteBucket) -> u64,
) -> Vec<u64> {
    let anchor_minute = now.timestamp().div_euclid(60);
    let mut placed = vec![0u64; SPARK_MINUTES];
    let mut all_parsed = true;
    for bucket in buckets {
        let normalized = bucket.minute.replace(' ', "T");
        let parsed = normalized
            .get(..16)
            .and_then(|prefix| NaiveDateTime::parse_from_str(prefix, "%Y-%m-%dT%H:%M").ok());
        let Some(naive) = parsed else {
            all_parsed = false;
            break;
        };
        let age = anchor_minute - naive.and_utc().timestamp().div_euclid(60);
        if (0..SPARK_MINUTES as i64).contains(&age) {
            placed[SPARK_MINUTES - 1 - age as usize] += pick(bucket);
        }
    }
    if all_parsed {
        return placed;
    }
    let raw: Vec<u64> = buckets.iter().map(pick).collect();
    raw[raw.len().saturating_sub(SPARK_MINUTES)..].to_vec()
}

// 6. 이벤트 피드와 상세 -------------------------------------------------------
// 피드는 고정 머리글과 열 격자, 상세는 선택 이벤트 항목과 같은 종류 맥락
fn render_feed_row(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // 1. 이벤트 준비 확인
    let lang = state.lang;
    let events = match &state.snapshot.events {
        Panel::Ready(rows) => rows.clone(),
        other => {
            let note = panel_message(lang, other).unwrap_or_default();
            let title = i18n::text(lang, Msg::TitleLiveEvents);
            render_notice_card(frame, state, CardId::EventFeed, area, title, note);
            return;
        }
    };
    if events.is_empty() {
        let feed_view = &mut state.card_views[CardId::EventFeed as usize];
        feed_view.total = 0;
        feed_view.visible = 0;
        let note = PanelNote {
            message: i18n::text(lang, Msg::NoEvents).to_string(),
            color: theme::FG_DIM,
            hint: Some(Msg::NoEventsHint),
        };
        render_notice_card(
            frame,
            state,
            CardId::EventFeed,
            area,
            &i18n::live_events_title(lang, 0),
            note,
        );
        return;
    }

    // 2. 피드와 상세 분할 (상세는 30%를 36~56열로, 피드가 60열 미만이면 6 대 4)
    let total_width = u32::from(area.width);
    let detail_width = (total_width * 30 / 100).clamp(36, 56);
    let feed_cut = if total_width.saturating_sub(detail_width + 1) < 60 {
        total_width * 60 / 100
    } else {
        total_width - detail_width - 1
    };
    let columns = state.card_layout.split_cards(
        &mut state.hit_areas,
        SplitGroup::OverviewFeedCols,
        SplitAxis::Columns,
        area,
        &[feed_cut as u16],
        &[24, 20],
    );

    // 3. 피드 표 (정렬한 행, 열 폭은 다른 표 카드와 같은 규칙: 자연 폭, 끌어 정한 폭, 머리글 정렬)
    let feed_sort = state.card_sorts[CardId::EventFeed as usize];
    let mut sorted = events.clone();
    sort_rows(&mut sorted, feed_sort, compare_event_row);
    let feed_rows: Vec<Vec<TableCell>> = sorted
        .iter()
        .map(|event| build_feed_row(lang, event))
        .collect();
    let (api_count, tool_count, hook_count) =
        events
            .iter()
            .fold((0usize, 0usize, 0usize), |counts, event| match event {
                LiveEvent::Api(_) => (counts.0 + 1, counts.1, counts.2),
                LiveEvent::Tool(_) => (counts.0, counts.1 + 1, counts.2),
                LiveEvent::Hook(_) => (counts.0, counts.1, counts.2 + 1),
                LiveEvent::Otel(_) | LiveEvent::Notice { .. } => counts,
            });
    render_card_table(
        frame,
        state,
        columns[0],
        CardTable {
            card: CardId::EventFeed,
            title: i18n::live_events_title(lang, events.len()),
            hint: i18n::feed_hint(lang, api_count, tool_count, hook_count),
            columns: FEED_COLUMNS.to_vec(),
            rows: feed_rows,
            aux_lines: Vec::new(),
        },
    );

    // 4. 선택 이벤트 상세 (남는 높이에 같은 종류 맥락, 맥락은 시각 내림차순 원본 기준)
    let selected = state.card_views[CardId::EventFeed as usize]
        .selected
        .min(sorted.len() - 1);
    let detail_inner_width = columns[1].width.saturating_sub(4) as usize;
    let mut detail_lines = build_event_detail_lines(lang, &sorted[selected], detail_inner_width);
    let spare_rows =
        usize::from(columns[1].height.saturating_sub(2)).saturating_sub(detail_lines.len());
    if spare_rows >= 4 {
        detail_lines.extend(build_event_context_lines(
            lang,
            &sorted[selected],
            &events,
            detail_inner_width,
            (spare_rows - 2).min(8),
        ));
    }
    render_text_card(
        frame,
        state,
        columns[1],
        TextCard {
            card: CardId::EventDetail,
            title: i18n::text(lang, Msg::TitleEventDetail).to_string(),
            hint: String::new(),
            lines: detail_lines,
        },
    );
}

// 7. 한도 카드 ----------------------------------------------------------------
// 좌우 폭에 맞춘 줄 목록과 테두리 우측 상단 문구 (출처·스냅샷 나이), empty = 보여 줄 없이 설정 안내만
#[derive(Debug, Default)]
struct LimitsView {
    lines: Vec<Line<'static>>,
    hint: String,
    empty: bool,
}

// 한도 줄 하나의 입력 (pct 없으면 -, resets_at 에서 span_secs 를 뺀 시각이 창 시작, note = 맨 뒤 부가 문구)
#[derive(Debug)]
struct LimitRow {
    label: Msg,
    pct: Option<f64>,
    resets_at: Option<i64>,
    span_secs: i64,
    exhaust_at: Option<i64>,
    note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pace {
    Ahead,
    Steady,
    Behind,
}

// 7-1. 카드 그리기 ------------------------------------------------------------
// 공식 스냅샷이 있으면 막대·카운트다운·페이스·소진 예상, 없으면 로컬 토큰 추정, 둘 다 없으면 statusLine 설정 안내
fn render_limits_card(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // 1. 보여 줄 계산 (준비되지 않은 패널은 안내 카드)
    let lang = state.lang;
    let title = i18n::text(lang, Msg::TitleLimits);
    let now = state.now_utc().timestamp();
    let inner_width = usize::from(area.width.saturating_sub(4));
    let inner_height = usize::from(area.height.saturating_sub(2));
    let view = match &state.snapshot.limits {
        Panel::Ready(data) => build_limits_view(lang, data, now, inner_width, inner_height),
        other => {
            let note = panel_message(lang, other).unwrap_or_default();
            render_notice_card(frame, state, CardId::LimitsCard, area, title, note);
            return;
        }
    };

    // 2. 카드 템두리와 본문 (설정 안내는 가운데 정렬·줄바꿈)
    let block = interactive_card(
        state,
        CardId::LimitsCard,
        area,
        title,
        &view.hint,
        theme::BORDER_DIM,
    )
    .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    if view.empty {
        let guide = vec![
            Line::from(Span::styled(
                i18n::text(lang, Msg::LimitsEmpty),
                theme::STYLE_DIM,
            )),
            Line::from(Span::styled(
                i18n::text(lang, Msg::LimitsEmptyHint),
                theme::STYLE_DIM,
            )),
        ];
        frame.render_widget(
            Paragraph::new(guide)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }
    frame.render_widget(Paragraph::new(view.lines).wrap(Wrap { trim: false }), inner);
}

// 7-2. 보여 줄 구성 -----------------------------------------------------------
// 5시간·7일·컨텍스트·API 키 순서로 높이만큼 담음, 오래된 스냅샷(30분 초과)은 전부 흘리게
// 스냅샷이 없으면 로컬 토큰 추정 두 줄(공식 퍼센트 아님)과 설정 안내, 추정도 없으면 empty
fn build_limits_view(
    lang: Lang,
    data: &LimitsData,
    now: i64,
    width: usize,
    height: usize,
) -> LimitsView {
    // 1. 공식 스냅샷 없음: 로컬 토큰 추정
    let Some(latest) = &data.latest else {
        let estimates = [
            (Msg::LimitsLabelFiveHour, &data.estimate_5h),
            (Msg::LimitsLabelSevenDay, &data.estimate_7d),
        ];
        let mut lines: Vec<Line<'static>> = Vec::new();
        for (label, estimate) in estimates {
            let Some(stats) = estimate else {
                continue;
            };
            let tokens = stats.input_tokens + stats.output_tokens;
            if tokens <= 0 {
                continue;
            }
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "{} ",
                        fit_display(i18n::text(lang, label), LIMIT_LABEL_WIDTH)
                    ),
                    theme::STYLE_DIM,
                ),
                Span::styled(
                    format!("{} ", i18n::text(lang, Msg::LimitsEstimate)),
                    theme::STYLE_DIM,
                ),
                Span::styled(format_compact(tokens), theme::STYLE_TEXT),
                Span::styled(
                    format!(" {}", i18n::text(lang, Msg::LimitsTokensUnit)),
                    theme::STYLE_DIM,
                ),
            ]));
        }
        if lines.is_empty() {
            return LimitsView {
                empty: true,
                ..LimitsView::default()
            };
        }
        lines.push(Line::from(Span::styled(
            i18n::text(lang, Msg::LimitsEmptyHint),
            theme::STYLE_DIM,
        )));
        lines.truncate(height);
        return LimitsView {
            lines,
            hint: i18n::text(lang, Msg::LimitsEstimate).to_string(),
            empty: false,
        };
    };

    // 2. 스냅샷 나이와 출처 문구 (너무 오래되면 흐림으로 보이고 소진 예상은 하지 않음)
    let age = DateTime::parse_from_rfc3339(&latest.ts)
        .ok()
        .map(|stamp| (now - stamp.timestamp()).max(0));
    let stale = age.is_some_and(|secs| secs > LIMIT_STALE_SECS);
    let source = i18n::text(lang, Msg::LimitsOfficial);
    let hint = match age {
        Some(secs) => i18n::limits_hint(lang, source, &format_age(secs), stale),
        None => source.to_string(),
    };

    // 3. 5시간·7일 줄 (같은 창의 스냅샷 기록으로 소진 예상)
    let five_points = collect_window_points(
        &data.history,
        |snapshot| (snapshot.five_hour_pct, snapshot.five_hour_resets_at),
        latest.five_hour_resets_at,
    );
    let seven_points = collect_window_points(
        &data.history,
        |snapshot| (snapshot.seven_day_pct, snapshot.seven_day_resets_at),
        latest.seven_day_resets_at,
    );
    let exhaust_of = |points: &[(i64, f64)], resets_at: Option<i64>| {
        resets_at
            .filter(|_| !stale)
            .and_then(|resets_at| project_exhaustion(points, resets_at, now))
    };
    let mut rows = vec![
        LimitRow {
            label: Msg::LimitsLabelFiveHour,
            pct: latest.five_hour_pct,
            resets_at: latest.five_hour_resets_at,
            span_secs: FIVE_HOUR_SECS,
            exhaust_at: exhaust_of(&five_points, latest.five_hour_resets_at),
            note: None,
        },
        LimitRow {
            label: Msg::LimitsLabelSevenDay,
            pct: latest.seven_day_pct,
            resets_at: latest.seven_day_resets_at,
            span_secs: SEVEN_DAY_SECS,
            exhaust_at: exhaust_of(&seven_points, latest.seven_day_resets_at),
            note: None,
        },
    ];

    // 4. 현재 세션 컨텍스트와 컴팩션 횟수 (기록이 현재 세션의 것일 때만 셈)
    if latest.context_pct.is_some() {
        let compactions = data
            .compaction
            .as_ref()
            .filter(|stat| stat.session_id == latest.session_id)
            .map_or(0, |stat| stat.count);
        rows.push(LimitRow {
            label: Msg::LimitsLabelContext,
            pct: latest.context_pct,
            resets_at: None,
            span_secs: 0,
            exhaust_at: None,
            note: Some(i18n::limits_compactions(lang, compactions)),
        });
    }
    let mut lines: Vec<Line<'static>> = rows
        .iter()
        .map(|row| build_limit_line(lang, row, now, width, stale))
        .collect();

    // 5. API 키 남은 양 (구간 안 헤더 기록이 있을 때)
    if let Some(api_line) = data
        .api_key
        .as_ref()
        .and_then(|row| build_api_key_line(lang, row, now, width, stale))
    {
        lines.push(api_line);
    }
    lines.truncate(height);
    LimitsView {
        lines,
        hint,
        empty: false,
    }
}

// 7-3. 한도 줄 조립 -----------------------------------------------------------
// 이름, 막대, 퍼센트, 그 뒤 부가 항목(리셋 카운트다운, 페이스, 소진 예상 시각, 부가 문구)
// 폭이 모자라면 뒤 항목부터 통째로 덜어내고 막대는 4칸까지 줄임, 색은 퍼센트 숫자와 항상 짝 (dim = 모두 흐림)
fn build_limit_line(
    lang: Lang,
    row: &LimitRow,
    now: i64,
    width: usize,
    dim: bool,
) -> Line<'static> {
    // 1. 값 문구와 스타일
    let tone = |style: Style| if dim { theme::STYLE_DIM } else { style };
    let label = format!(
        "{} ",
        fit_display(i18n::text(lang, row.label), LIMIT_LABEL_WIDTH)
    );
    let pct_text = pad_left_display(
        &row.pct.map_or("-".to_string(), |pct| format!("{pct:.0}%")),
        LIMIT_PCT_WIDTH,
    );
    let pct_style = row
        .pct
        .map_or(theme::STYLE_DIM, |pct| limit_style(pct, dim));
    let level_mark = row.pct.map_or("", limit_mark);
    let live_reset = row.resets_at.filter(|resets_at| *resets_at > now);
    let mut tails: Vec<(String, Style)> = Vec::new();
    if row.resets_at.is_some() {
        tails.push(match live_reset {
            Some(resets_at) => (
                i18n::limits_reset_in(lang, &format_countdown(resets_at - now)),
                tone(theme::STYLE_TEXT),
            ),
            None => (
                i18n::text(lang, Msg::LimitsResetDue).to_string(),
                theme::STYLE_DIM,
            ),
        });
    }
    if let (Some(pct), Some(resets_at)) = (row.pct, live_reset) {
        let (glyph, word, style) = match judge_pace(pct, resets_at, row.span_secs, now) {
            Pace::Ahead => ("▲", Msg::LimitsPaceAhead, tone(theme::STYLE_WARN)),
            Pace::Steady => ("●", Msg::LimitsPaceOn, theme::STYLE_DIM),
            Pace::Behind => ("▼", Msg::LimitsPaceBehind, tone(theme::STYLE_OK)),
        };
        tails.push((format!("{glyph} {}", i18n::text(lang, word)), style));
    }
    if let Some(exhaust_at) = row.exhaust_at {
        tails.push((
            format!("→ {}", format_exhaustion_clock(exhaust_at, now)),
            tone(theme::STYLE_WARN),
        ));
    }
    if let Some(note) = &row.note {
        tails.push((note.clone(), theme::STYLE_DIM));
    }

    // 2. 막대 폭은 모든 줄이 같아 세로로 정렬 (모든 항목이 들어갈 만큼 넓을 때만 늘어남), 항목은 앞에서부터 폭에 들어갈 때만 채택
    let bar_width = (BAR_MIN_WIDTH + width.saturating_sub(LIMIT_FULL_ROW_WIDTH)).min(BAR_MAX_WIDTH);
    let mut used =
        Span::raw(label.as_str()).width() + bar_width + 1 + LIMIT_PCT_WIDTH + level_mark.len();
    let mut shown: Vec<(String, Style)> = Vec::new();
    for (text, style) in tails {
        let needed = 2 + Span::raw(text.as_str()).width();
        if used + needed > width {
            break;
        }
        used += needed;
        shown.push((text, style));
    }

    // 3. 조립 (막대 채움 색은 퍼센트 숫자와 같은 단계)
    let mut spans = vec![Span::styled(label, theme::STYLE_DIM)];
    spans.extend(build_bar(row.pct, bar_width, pct_style));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        pct_text,
        pct_style.add_modifier(Modifier::BOLD),
    ));
    if !level_mark.is_empty() {
        spans.push(Span::styled(
            level_mark,
            pct_style.add_modifier(Modifier::BOLD),
        ));
    }
    for (text, style) in shown {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(text, style));
    }
    Line::from(clip_spans_to_width(spans, width))
}

// 막대: 채운 칸은 퍼센트 색, 빈 칸은 흐린 트랙, 0 보다 크면 최소 한 칸
fn build_bar(pct: Option<f64>, width: usize, fill_style: Style) -> Vec<Span<'static>> {
    let ratio = pct.map_or(0.0, |value| value.clamp(0.0, 100.0) / 100.0);
    let mut filled = ((ratio * width as f64).round() as usize).min(width);
    if ratio > 0.0 && filled == 0 {
        filled = 1;
    }
    vec![
        Span::styled("█".repeat(filled), fill_style),
        Span::styled("░".repeat(width - filled), theme::STYLE_FAINT),
    ]
}

// 퍼센트 단계 색: 80 이상 경고, 95 이상 위험(굵게), 그 밖 정상 (dim 이면 흐림)
fn limit_style(pct: f64, dim: bool) -> Style {
    if dim {
        theme::STYLE_DIM
    } else if pct >= LIMIT_CRIT_PCT {
        theme::STYLE_CRIT.add_modifier(Modifier::BOLD)
    } else if pct >= LIMIT_WARN_PCT {
        theme::STYLE_WARN
    } else {
        theme::STYLE_OK
    }
}

// 퍼센트 단계 표식: 색을 못 봐도 단계가 보이도록 경고 " !", 위험 " !!" (정상은 없음, 기준은 limit_style 과 같음)
fn limit_mark(pct: f64) -> &'static str {
    if pct >= LIMIT_CRIT_PCT {
        " !!"
    } else if pct >= LIMIT_WARN_PCT {
        " !"
    } else {
        ""
    }
}

// 7-4. API 키 줄 --------------------------------------------------------------
// 요청·토큰 남은 양/한도와 리셋까지 시간, 두 값이 모두 없으면 줄 없음 (남은 비율이 적을수록 경고·위험 색)
fn build_api_key_line(
    lang: Lang,
    row: &RatelimitSnapshotRow,
    now: i64,
    width: usize,
    dim: bool,
) -> Option<Line<'static>> {
    // 1. 요청·토큰 항목 (토큰은 통합 한도가 없으면 입력 토큰 한도)
    let token_pair = if row.tokens_remaining.is_some() {
        (row.tokens_remaining, row.tokens_limit)
    } else {
        (row.input_tokens_remaining, row.input_tokens_limit)
    };
    let mut items: Vec<Vec<Span<'static>>> = vec![vec![Span::styled(
        format!(
            "{} ",
            fit_display(i18n::text(lang, Msg::LimitsLabelApiKey), LIMIT_LABEL_WIDTH)
        ),
        theme::STYLE_DIM,
    )]];
    let pairs = [
        (
            Msg::LimitsRequestsShort,
            (row.requests_remaining, row.requests_limit),
        ),
        (Msg::LimitsTokensUnit, token_pair),
    ];
    for (unit, (remaining, limit)) in pairs {
        let Some(remaining) = remaining else {
            continue;
        };
        let used_pct = limit
            .filter(|total| *total > 0)
            .map_or(0.0, |total| 100.0 - remaining as f64 * 100.0 / total as f64);
        let value = match limit {
            Some(total) => format!("{}/{}", format_compact(remaining), format_compact(total)),
            None => format_compact(remaining),
        };
        items.push(vec![
            Span::styled(format!("{} ", i18n::text(lang, unit)), theme::STYLE_DIM),
            Span::styled(
                format!("{value}{}", limit_mark(used_pct)),
                limit_style(used_pct, dim).add_modifier(Modifier::BOLD),
            ),
        ]);
    }
    if items.len() == 1 {
        return None;
    }

    // 2. 요청 한도 리셋까지 (RFC 3339 해석이 되고 아직 안 지났을 때만)
    let reset_secs = row
        .requests_reset
        .as_deref()
        .and_then(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
        .map(|stamp| stamp.timestamp() - now)
        .filter(|secs| *secs > 0);
    if let Some(secs) = reset_secs {
        items.push(vec![Span::styled(
            i18n::limits_reset_in(lang, &format_countdown(secs)),
            theme::STYLE_DIM,
        )]);
    }
    Some(Line::from(pack_spans_to_width(items, &[2, 1], width)))
}

// 7-5. 페이스·소진 예상 계산 --------------------------------------------------
// 창 경과 비율(%)과 사용률(%)의 차가 허용 범위 밖이면 빠름·느림, 안이면 적정
fn judge_pace(pct: f64, resets_at: i64, span_secs: i64, now: i64) -> Pace {
    let elapsed_pct =
        ((now - (resets_at - span_secs)) as f64 * 100.0 / span_secs as f64).clamp(0.0, 100.0);
    let gap = pct - elapsed_pct;
    if gap > PACE_TOLERANCE_PCT {
        Pace::Ahead
    } else if gap < -PACE_TOLERANCE_PCT {
        Pace::Behind
    } else {
        Pace::Steady
    }
}

// 같은 한도 창(리셋 시각이 같음)의 (스냅샷 시각 초, 사용률) 기록, 오래된 순
fn collect_window_points(
    history: &[LimitSnapshot],
    pick: fn(&LimitSnapshot) -> (Option<f64>, Option<i64>),
    resets_at: Option<i64>,
) -> Vec<(i64, f64)> {
    let Some(resets_at) = resets_at else {
        return Vec::new();
    };
    history
        .iter()
        .filter_map(|snapshot| {
            let (pct, snapshot_resets_at) = pick(snapshot);
            let stamp = DateTime::parse_from_rfc3339(&snapshot.ts).ok()?;
            (snapshot_resets_at == Some(resets_at)).then_some((stamp.timestamp(), pct?))
        })
        .collect()
}

// 최근 기록의 선형 증가율로 100% 에 닿는 시각 (리셋 전에 닿을 때만, 기록 간격이 너무 짧거나 증가가 없으면 None)
fn project_exhaustion(points: &[(i64, f64)], resets_at: i64, now: i64) -> Option<i64> {
    let (last_at, last_pct) = *points.last()?;
    if last_pct >= 100.0 {
        return None;
    }
    let (first_at, first_pct) = points
        .iter()
        .copied()
        .find(|(stamp, _)| *stamp >= last_at - PROJECTION_LOOKBACK_SECS)?;
    let span = last_at - first_at;
    let rise = last_pct - first_pct;
    if span < PROJECTION_MIN_SPAN_SECS || rise <= 0.0 {
        return None;
    }
    let remaining_secs = ((100.0 - last_pct) * span as f64 / rise) as i64;
    let exhaust_at = (last_at + remaining_secs).max(now);
    (exhaust_at < resets_at).then_some(exhaust_at)
}

// 7-6. 시간 표시 --------------------------------------------------------------
// 남은 시간: 1일 이상 일+시, 1시간 이상 시+분, 1분 이상 분+초, 그 밖 초 (단위 글자는 언어와 무관)
pub(crate) fn format_countdown(secs: i64) -> String {
    let secs = secs.max(0);
    let (days, hours, minutes, seconds) = (
        secs / 86_400,
        secs % 86_400 / 3_600,
        secs % 3_600 / 60,
        secs % 60,
    );
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

// 경과 시간 한 자리: 초, 분, 시, 일
pub(crate) fn format_age(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

// 소진 예상 시각: 오늘이면 시:분, 아니면 월-일 시:분 (지역 시각)
fn format_exhaustion_clock(exhaust_at: i64, now: i64) -> String {
    let Some(exhaust) =
        DateTime::from_timestamp(exhaust_at, 0).map(|stamp| stamp.with_timezone(&Local))
    else {
        return "--:--".to_string();
    };
    let same_day = DateTime::from_timestamp(now, 0)
        .is_some_and(|stamp| stamp.with_timezone(&Local).date_naive() == exhaust.date_naive());
    if same_day {
        exhaust.format("%H:%M").to_string()
    } else {
        exhaust.format("%m-%d %H:%M").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minute_label(now: DateTime<Utc>, minutes_ago: i64) -> String {
        (now - chrono::Duration::minutes(minutes_ago))
            .format("%Y-%m-%dT%H:%M")
            .to_string()
    }

    #[test]
    fn minute_series_places_buckets_by_age_and_fills_gaps() {
        let now = Utc::now();
        let buckets = vec![
            MinuteBucket {
                minute: minute_label(now, 0),
                api_requests: 5,
                ..MinuteBucket::default()
            },
            MinuteBucket {
                minute: minute_label(now, 3),
                api_requests: 2,
                ..MinuteBucket::default()
            },
            MinuteBucket {
                minute: minute_label(now, 90),
                api_requests: 9,
                ..MinuteBucket::default()
            },
        ];
        let series = build_minute_series(&buckets, now, |bucket| bucket.api_requests);
        assert_eq!(series.len(), SPARK_MINUTES);
        assert_eq!(series[SPARK_MINUTES - 1], 5);
        assert_eq!(series[SPARK_MINUTES - 4], 2);
        // 60분 밖 버킷은 버림
        assert_eq!(series.iter().sum::<u64>(), 7);
    }

    #[test]
    fn minute_series_falls_back_to_query_order_on_unknown_format() {
        let buckets: Vec<MinuteBucket> = (1..=3)
            .map(|count| MinuteBucket {
                minute: format!("minute-{count}"),
                api_requests: count,
                ..MinuteBucket::default()
            })
            .collect();
        let series = build_minute_series(&buckets, Utc::now(), |bucket| bucket.api_requests);
        assert_eq!(series, [1, 2, 3]);
    }

    #[test]
    fn countdown_and_age_pick_the_two_largest_units_and_clamp_negatives() {
        assert_eq!(format_countdown(-5), "0s");
        assert_eq!(format_countdown(59), "59s");
        assert_eq!(format_countdown(60), "1m 00s");
        assert_eq!(format_countdown(252), "4m 12s");
        assert_eq!(format_countdown(3_600), "1h 00m");
        assert_eq!(format_countdown(2 * 3_600 + 14 * 60 + 59), "2h 14m");
        assert_eq!(format_countdown(86_400 + 4 * 3_600), "1d 4h");
        assert_eq!(format_age(12), "12s");
        assert_eq!(format_age(45 * 60), "45m");
        assert_eq!(format_age(5 * 3_600), "5h");
        assert_eq!(format_age(3 * 86_400), "3d");
    }

    #[test]
    fn pace_compares_used_percent_with_elapsed_window_percent() {
        let now = 1_000_000;
        // 5시간 창의 40% 지점 (2시간 경과)
        let resets_at = now + 3 * 3_600;
        assert_eq!(
            judge_pace(46.0, resets_at, FIVE_HOUR_SECS, now),
            Pace::Ahead
        );
        assert_eq!(
            judge_pace(45.0, resets_at, FIVE_HOUR_SECS, now),
            Pace::Steady
        );
        assert_eq!(
            judge_pace(35.0, resets_at, FIVE_HOUR_SECS, now),
            Pace::Steady
        );
        assert_eq!(
            judge_pace(34.0, resets_at, FIVE_HOUR_SECS, now),
            Pace::Behind
        );
    }

    #[test]
    fn limit_colors_switch_at_80_and_95_and_dim_overrides_them() {
        assert_eq!(limit_style(79.9, false), theme::STYLE_OK);
        assert_eq!(limit_style(80.0, false), theme::STYLE_WARN);
        assert_eq!(limit_style(94.9, false), theme::STYLE_WARN);
        assert_eq!(
            limit_style(95.0, false),
            theme::STYLE_CRIT.add_modifier(Modifier::BOLD)
        );
        assert_eq!(limit_style(99.0, true), theme::STYLE_DIM);
    }

    // 색을 못 봐도 단계를 알 수 있도록 경고 " !"·위험 " !!" 표식이 퍼센트 뒤에 붙고 줄 폭은 넘지 않음
    #[test]
    fn limit_lines_carry_a_level_mark_next_to_the_percent_and_stay_within_the_width() {
        assert_eq!(limit_mark(79.9), "");
        assert_eq!(limit_mark(80.0), " !");
        assert_eq!(limit_mark(94.9), " !");
        assert_eq!(limit_mark(95.0), " !!");

        let row = |pct: f64| LimitRow {
            label: Msg::LimitsLabelFiveHour,
            pct: Some(pct),
            resets_at: Some(1_000_000 + 3_600),
            span_secs: FIVE_HOUR_SECS,
            exhaust_at: None,
            note: None,
        };
        for lang in [Lang::En, Lang::Ko] {
            for width in [28usize, 36, 60] {
                for (pct, mark) in [(70.0, ""), (85.0, " !"), (97.0, " !!")] {
                    let line = build_limit_line(lang, &row(pct), 1_000_000, width, false);
                    let text: String = line
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect();
                    let case = format!("{lang:?} {width} {pct} {text}");
                    assert!(text.contains(&format!("{pct:.0}%{mark}")), "{case}");
                    assert_eq!(text.contains('!'), !mark.is_empty(), "{case}");
                    assert!(line.width() <= width, "{case}");
                }
            }
        }
    }

    #[test]
    fn projection_shows_only_when_the_linear_rate_reaches_100_before_the_reset() {
        let now = 1_000_000;
        let hour = 3_600;
        // 한 시간에 20%p: 70% 에서 30%p 남아 한 시간 반 뒤 (리셋보다 이름)
        let points = [(now - hour, 50.0), (now, 70.0)];
        assert_eq!(
            project_exhaustion(&points, now + 3 * hour, now),
            Some(now + hour * 3 / 2)
        );
        // 리셋이 그보다 이르면 숨김
        assert_eq!(project_exhaustion(&points, now + hour, now), None);
        // 증가 없음, 감소, 기록 하나, 기록 간격 5분 미만, 이미 100% 는 숨김
        let flat = [(now - hour, 70.0), (now, 70.0)];
        assert_eq!(project_exhaustion(&flat, now + 3 * hour, now), None);
        let falling = [(now - hour, 80.0), (now, 70.0)];
        assert_eq!(project_exhaustion(&falling, now + 3 * hour, now), None);
        assert_eq!(
            project_exhaustion(&[(now, 70.0)], now + 3 * hour, now),
            None
        );
        let brief = [(now - 120, 50.0), (now, 70.0)];
        assert_eq!(project_exhaustion(&brief, now + 3 * hour, now), None);
        let full = [(now - hour, 90.0), (now, 100.0)];
        assert_eq!(project_exhaustion(&full, now + 3 * hour, now), None);
        // 되돌아볼 길이 밖의 오래된 기록은 쓰지 않음 (최근 3시간 안 첫 기록부터)
        let old_first = [(now - 10 * hour, 0.0), (now - hour, 50.0), (now, 70.0)];
        assert_eq!(
            project_exhaustion(&old_first, now + 3 * hour, now),
            Some(now + hour * 3 / 2)
        );
    }

    #[test]
    fn window_points_keep_only_snapshots_of_the_same_reset_window() {
        let snapshot = |ts: &str, pct: f64, resets_at: i64| LimitSnapshot {
            ts: ts.to_string(),
            five_hour_pct: Some(pct),
            five_hour_resets_at: Some(resets_at),
            ..LimitSnapshot::default()
        };
        let history = [
            snapshot("2026-11-19T07:00:00Z", 10.0, 100),
            snapshot("2026-11-19T08:00:00Z", 20.0, 200),
            snapshot("2026-11-19T09:00:00Z", 30.0, 200),
            snapshot("not a time", 40.0, 200),
        ];
        let points = collect_window_points(
            &history,
            |snapshot| (snapshot.five_hour_pct, snapshot.five_hour_resets_at),
            Some(200),
        );
        assert_eq!(points.len(), 2);
        assert_eq!(points[1].1, 30.0);
        assert!(points[1].0 > points[0].0);
        let none = collect_window_points(&history, |snapshot| (snapshot.five_hour_pct, None), None);
        assert!(none.is_empty());
    }
}
