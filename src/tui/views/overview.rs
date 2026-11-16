//! tui/views/overview.rs
//! 개요 탭: 서버 상태 카드, KPI 카드, 절약 카드, 분당 추이, 실시간 이벤트 피드와 이벤트 상세.

use crate::core::model::{LiveEvent, PortStatus};
use crate::store::query::MinuteBucket;
use crate::tui::cards::CardId;
use crate::tui::format::{
    fit_display, format_compact, format_count, format_duration_ms, pad_left_display,
};
use crate::tui::i18n::{self, Msg};
use crate::tui::layout::{SplitAxis, SplitGroup};
use crate::tui::sort::{compare_event_row, sort_rows};
use crate::tui::state::{AppState, DbState, Panel};
use crate::tui::theme;
use crate::tui::views::common::{
    CardTable, PanelNote, TableCell, TextCard, clip_spans_to_width, interactive_card,
    pack_spans_to_width, panel_message, render_card_table, render_db_gate, render_notice_card,
    render_text_card,
};
use crate::tui::views::feed::{
    FEED_COLUMNS, build_event_context_lines, build_event_detail_lines, build_feed_row,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Padding, Paragraph, Sparkline};

const SPARK_MINUTES: usize = 60;
const CACHE_CARD_INDEX: usize = 2;
const HEADER_HEIGHT: u16 = 4;
const KPI_HEIGHT: u16 = 4;
const TREND_HEIGHT: u16 = 5;
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
        let rows = state.card_layout.split_cards(
            &mut state.hit_areas,
            SplitGroup::OverviewRows,
            SplitAxis::Rows,
            area,
            &[header_cut, kpi_cut, kpi_cut + TREND_HEIGHT],
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
    // 1. 카드 분할 (절약 30%를 31~40열로, 나머지 둘은 균등)
    let width = u32::from(area.width);
    let first_cut = (width * 30 / 100).clamp(31, 40);
    let second_cut = first_cut + 1 + width.saturating_sub(first_cut + 1) / 2;
    let panes = state.card_layout.split_cards(
        &mut state.hit_areas,
        SplitGroup::OverviewTrendCols,
        SplitAxis::Columns,
        area,
        &[first_cut as u16, second_cut as u16],
        &[24, 16, 16],
    );

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
                LiveEvent::Notice { .. } => counts,
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
}
