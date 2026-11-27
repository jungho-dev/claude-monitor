//! tui/views/overview.rs
//! 개요 탭: 서버 상태 카드, KPI 카드, 절약 카드, 분당 추이, 실시간 이벤트 피드와 이벤트 상세.

use crate::core::model::{LiveEvent, PortStatus};
use crate::store::query::MinuteBucket;
use crate::tui::cards::CardId;
use crate::tui::format::{format_compact, format_count, format_duration_ms};
use crate::tui::i18n::{self, Msg};
use crate::tui::layout::grid::{CardSlot, SlotHeight};
use crate::tui::sort::{compare_event_row, sort_rows};
use crate::tui::state::{ActiveTab, AppState, DbState, Panel};
use crate::tui::theme;
use crate::tui::views::charts::{render_activity_chart, render_token_mix, render_top_tools};
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
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Padding, Paragraph, Sparkline};

const SPARK_MINUTES: usize = 60;
const HEADER_HEIGHT: u16 = 4;
const KPI_HEIGHT: u16 = 4;
const TREND_HEIGHT: u16 = 5;
// 본문이 이 높이 이상이면 추이 줄을 한 줄 키움
const TALL_LAYOUT_MIN_HEIGHT: u16 = 24;
const TREND_HEIGHT_TALL: u16 = 6;
// 모델 탭 캐시 카드: 공식 스냅샷이 이 시간(초)보다 오래되면 흐리게
pub(crate) const LIMIT_STALE_SECS: i64 = 1_800;
// 이 폭 이상 카드만 안쪽 가로 여백 1열
const KPI_PADDING_MIN_WIDTH: u16 = 12;
// 차트 줄 높이 범위 (상태·KPI·추이 줄 아래 남는 높이의 40%)
const CHART_HEIGHT_MIN: u16 = 9;
const CHART_HEIGHT_MAX: u16 = 14;
// KPI 카드별 (카드, 제목, 짧은 제목)
const KPI_TITLES: [(CardId, Msg, Msg); 5] = [
    (CardId::KpiRequests, Msg::KpiRequests, Msg::KpiRequestsShort),
    (CardId::KpiTokens, Msg::KpiTokens, Msg::KpiTokensShort),
    (CardId::KpiCache, Msg::KpiCache, Msg::KpiCacheShort),
    (CardId::KpiTools, Msg::KpiTools, Msg::KpiToolsShort),
    (CardId::KpiHooks, Msg::KpiHooks, Msg::KpiHooksShort),
];

// 1. 개요 탭 ------------------------------------------------------------------
// 탭 격자 배치 (카드는 어느 줄로든 옮길 수 있음), 카드마다 받은 영역에 그림
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

    // 2. 카드 조건 (추이 rank 2·차트 rank 3 줄은 높이가 모자라면 먼저 숨김, 상세는 이벤트가 있을 때만)
    let trend_height = if area.height >= TALL_LAYOUT_MIN_HEIGHT {
        TREND_HEIGHT_TALL
    } else {
        TREND_HEIGHT
    };
    let feed_height = area
        .height
        .saturating_sub(HEADER_HEIGHT + KPI_HEIGHT + trend_height);
    let chart_height = (feed_height * 2 / 5).clamp(CHART_HEIGHT_MIN, CHART_HEIGHT_MAX);
    let fixed_cards: [(CardId, u16, u16, u16, u16, u8, u16); 12] = [
        (CardId::ServerStatus, 20, 3, HEADER_HEIGHT, 1, 0, 0),
        (CardId::KpiRequests, 8, 3, KPI_HEIGHT, 10, 0, 0),
        (CardId::KpiTokens, 8, 3, KPI_HEIGHT, 10, 0, 0),
        (CardId::KpiCache, 8, 3, KPI_HEIGHT, 10, 0, 0),
        (CardId::KpiTools, 8, 3, KPI_HEIGHT, 10, 0, 0),
        (CardId::KpiHooks, 8, 3, KPI_HEIGHT, 10, 0, 0),
        (CardId::SavingsEffect, 8, 3, KPI_HEIGHT, 11, 0, 0),
        (CardId::TrendRequests, 16, 5, trend_height, 1, 2, 5),
        (CardId::TrendTokens, 16, 5, trend_height, 1, 2, 5),
        (CardId::ChartActivity, 28, 6, chart_height, 38, 3, 9),
        (CardId::ChartTokens, 27, 6, chart_height, 32, 3, 9),
        (CardId::ChartTools, 21, 6, chart_height, 30, 3, 9),
    ];
    let mut slots: Vec<CardSlot> = Vec::with_capacity(fixed_cards.len() + 2);
    for (card, min_w, min_h, rows, weight, rank, reserve) in fixed_cards {
        slots.push(CardSlot {
            card,
            min_w,
            min_h,
            height: SlotHeight::Fixed(rows),
            weight,
            rank,
            reserve,
        });
    }
    slots.push(CardSlot {
        card: CardId::EventFeed,
        min_w: 24,
        min_h: 3,
        height: SlotHeight::Fill,
        weight: 7,
        rank: 0,
        reserve: 0,
    });
    if matches!(&state.snapshot.events, Panel::Ready(rows) if !rows.is_empty()) {
        slots.push(CardSlot {
            card: CardId::EventDetail,
            min_w: 20,
            min_h: 3,
            height: SlotHeight::Fill,
            weight: 3,
            rank: 0,
            reserve: 0,
        });
    }

    // 3. 격자 배치 (줄·카드 경계 드래그와 줄 적중 영역은 arrange_tab 이 등록)
    let layout = state.card_layout;
    let placed = layout.arrange_tab(&mut state.hit_areas, ActiveTab::Overview, area, &slots);

    // 4. 카드별 그리기 (보이는 카드만)
    for (card, rect) in placed {
        match card {
            CardId::ServerStatus => render_status_header(frame, state, rect),
            CardId::KpiRequests
            | CardId::KpiTokens
            | CardId::KpiCache
            | CardId::KpiTools
            | CardId::KpiHooks => render_kpi_card(frame, state, card, rect),
            CardId::SavingsEffect => render_savings_card(frame, state, rect),
            CardId::TrendRequests | CardId::TrendTokens => {
                render_trend_card(frame, state, card, rect);
            }
            CardId::ChartActivity => render_activity_chart(frame, state, rect),
            CardId::ChartTokens => render_token_mix(frame, state, rect),
            CardId::ChartTools => render_top_tools(frame, state, rect),
            CardId::EventFeed => render_event_feed(frame, state, rect),
            CardId::EventDetail => render_event_detail(frame, state, rect),
            CardId::Gate
            | CardId::ToolTable
            | CardId::ToolDetail
            | CardId::HookTable
            | CardId::HookDetail
            | CardId::ModelTable
            | CardId::ModelDetail
            | CardId::CacheBreaks
            | CardId::AgentTable
            | CardId::AgentDetail
            | CardId::SettingsHooks
            | CardId::SettingsMcp
            | CardId::SettingsAgents
            | CardId::SettingsDoctor
            | CardId::ReportBody
            | CardId::ReportOutline
            | CardId::ProjectTable
            | CardId::ProjectDetail
            | CardId::AttributionTable
            | CardId::CacheTimer
            | CardId::SkillTable
            | CardId::SkillDetail => {}
        }
    }
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
            let (serve_label, serve_style) = if snapshot.serve_stale {
                (i18n::text(lang, Msg::ServeStaleBuild), theme::STYLE_WARN)
            } else {
                (i18n::text(lang, Msg::ServeRunning), theme::STYLE_OK)
            };
            status_items.push(vec![Span::styled(format!("● {serve_label}"), serve_style)]);
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
// 값 줄은 숫자 굵게·라벨 흐리게, 폭이 모자라면 짧은 형태로, 보조 줄은 흐리게 (캐시 카드는 게이지)
fn render_kpi_card(frame: &mut Frame, state: &mut AppState, card: CardId, area: Rect) {
    // 1. 제목 (카드 폭이 모자라면 짧은 제목)
    let lang = state.lang;
    let word = |msg: Msg| i18n::text(lang, msg);
    let Some((_, full_msg, short_msg)) = KPI_TITLES.iter().find(|(kpi, _, _)| *kpi == card) else {
        return;
    };
    let full_title = word(*full_msg);
    let title = if Span::raw(full_title).width() + 4 <= usize::from(area.width) {
        full_title
    } else {
        word(*short_msg)
    };

    // 2. 집계 준비 확인 (준비 전·실패는 이 카드 영역의 안내 카드)
    let overview = match &state.snapshot.overview {
        Panel::Ready(stats) => stats.clone(),
        other => {
            let note = panel_message(lang, other).unwrap_or_default();
            render_notice_card(frame, state, card, area, title, note);
            return;
        }
    };

    // 3. 카드 내용 (값 줄 후보는 긴 형태부터, 보조 줄)
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
    let deny_style = theme::STYLE_CRIT.add_modifier(Modifier::BOLD);
    let rewrite_style = theme::STYLE_OK.add_modifier(Modifier::BOLD);
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
    let contents: [(CardId, Vec<Line<'static>>, Vec<Line<'static>>); 5] = [
        (
            CardId::KpiRequests,
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
            CardId::KpiTokens,
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
            CardId::KpiCache,
            vec![Line::from("")],
            vec![Line::from(Span::styled(
                i18n::cache_read_detail(lang, &format_compact(overview.cache_read_tokens)),
                dim,
            ))],
        ),
        (
            CardId::KpiTools,
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
            CardId::KpiHooks,
            vec![
                Line::from(vec![
                    Span::styled(format!("{} ", word(Msg::KpiDeny)), dim),
                    Span::styled(format_count(hook_denials), deny_style),
                    Span::raw("  "),
                    Span::styled(format!("{} ", word(Msg::KpiRewrite)), dim),
                    Span::styled(format_count(hook_rewrites), rewrite_style),
                ]),
                Line::from(vec![
                    Span::styled(format_count(hook_denials), deny_style),
                    Span::styled(" / ", faint),
                    Span::styled(format_count(hook_rewrites), rewrite_style),
                ]),
                Line::from(vec![
                    Span::styled(format_count(hook_denials), deny_style),
                    Span::styled("/", faint),
                    Span::styled(format_count(hook_rewrites), rewrite_style),
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
    let Some((_, value_lines, detail_lines)) =
        contents.into_iter().find(|(kpi, _, _)| *kpi == card)
    else {
        return;
    };

    // 4. 카드 그리기 (캐시 카드는 게이지, 줄은 폭에 맞는 첫 후보)
    let mut block = interactive_card(state, card, area, title, "", theme::BORDER_DIM);
    if area.width >= KPI_PADDING_MIN_WIDTH {
        block = block.padding(Padding::horizontal(1));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 2 {
        return;
    }
    let first_row = Rect::new(inner.x, inner.y, inner.width, 1);
    let second_row = Rect::new(inner.x, inner.y + 1, inner.width, 1);
    if card == CardId::KpiCache {
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
        let value_line = fit_variant_line(value_lines, inner.width as usize);
        frame.render_widget(Paragraph::new(value_line), first_row);
    }
    let detail_line = fit_variant_line(detail_lines, inner.width as usize);
    frame.render_widget(Paragraph::new(detail_line), second_row);
}

// 4. 절약 카드 ----------------------------------------------------------------
// 오늘 절약 시간 대 거부 손실 추정: 1행 순효과, 2행 절약·손실 (긴 이름, 짧은 이름, 값만 순으로 폭에 맞춤)
fn render_savings_card(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // 1. 제목 (카드 폭이 모자라면 짧은 제목)
    let lang = state.lang;
    let word = |msg: Msg| i18n::text(lang, msg);
    let full_title = word(Msg::TitleSavings);
    let title = if Span::raw(full_title).width() + 4 <= usize::from(area.width) {
        full_title
    } else {
        word(Msg::TitleSavingsShort)
    };

    // 2. 오늘 집계 확인 (준비 전·실패는 이 카드 영역의 안내 카드)
    let today = match &state.snapshot.today {
        Panel::Ready(stats) => stats.clone(),
        other => {
            let note = panel_message(lang, other).unwrap_or_default();
            render_notice_card(frame, state, CardId::SavingsEffect, area, title, note);
            return;
        }
    };

    // 3. 줄 후보 (순효과는 부호에 따라 녹색·적색, 절약 녹색, 손실 적색)
    let net = today.saved_ms - today.lost_ms_estimate;
    let net_style = if net >= 0 {
        theme::STYLE_OK
    } else {
        theme::STYLE_CRIT
    }
    .add_modifier(Modifier::BOLD);
    let net_sign = if net >= 0 { "+" } else { "-" };
    let net_text = format!("{net_sign}{}", format_duration_ms(net.abs()));
    let saved_text = format!("+{}", format_duration_ms(today.saved_ms));
    let loss_text = format!("-{}", format_duration_ms(today.lost_ms_estimate));
    let saved_style = theme::STYLE_OK.add_modifier(Modifier::BOLD);
    let loss_style = theme::STYLE_CRIT.add_modifier(Modifier::BOLD);
    let net_lines = vec![
        Line::from(vec![
            Span::styled(format!("{} ", word(Msg::SavingsNet)), theme::STYLE_TEXT),
            Span::styled(net_text.clone(), net_style),
        ]),
        Line::from(vec![
            Span::styled(
                format!("{} ", word(Msg::SavingsNetShort)),
                theme::STYLE_TEXT,
            ),
            Span::styled(net_text.clone(), net_style),
        ]),
        Line::from(Span::styled(net_text, net_style)),
    ];
    let mut split_lines: Vec<Line<'static>> = Vec::with_capacity(4);
    for (saved_label, loss_label) in [
        (Msg::SavingsSaved, Msg::SavingsLoss),
        (Msg::SavingsSavedShort, Msg::SavingsLossShort),
    ] {
        split_lines.push(Line::from(vec![
            Span::styled(format!("{} ", word(saved_label)), theme::STYLE_DIM),
            Span::styled(saved_text.clone(), saved_style),
            Span::raw("  "),
            Span::styled(format!("{} ", word(loss_label)), theme::STYLE_DIM),
            Span::styled(loss_text.clone(), loss_style),
        ]));
    }
    for separator in [" / ", "/"] {
        split_lines.push(Line::from(vec![
            Span::styled(saved_text.clone(), saved_style),
            Span::styled(separator, theme::STYLE_FAINT),
            Span::styled(loss_text.clone(), loss_style),
        ]));
    }

    // 4. 카드 그리기
    let mut block = interactive_card(
        state,
        CardId::SavingsEffect,
        area,
        title,
        "",
        theme::BORDER_DIM,
    );
    if area.width >= KPI_PADDING_MIN_WIDTH {
        block = block.padding(Padding::horizontal(1));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    let width = usize::from(inner.width);
    let lines = vec![
        fit_variant_line(net_lines, width),
        fit_variant_line(split_lines, width),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

// 5. 분당 추이 카드 -----------------------------------------------------------
// 분당 요청 또는 출력 토큰 스파크라인 (분마다 k칸, 지금이 오른쪽 끝), 준비 전은 카드마다 안내
fn render_trend_card(frame: &mut Frame, state: &mut AppState, card: CardId, area: Rect) {
    // 1. 카드 종류별 제목·계열 색
    let lang = state.lang;
    let word = |msg: Msg| i18n::text(lang, msg);
    let tokens = card == CardId::TrendTokens;
    let (title, color) = if tokens {
        (word(Msg::TitleTokensPerMin), theme::CHART_TOKENS)
    } else {
        (word(Msg::TitleRequestsPerMin), theme::CHART_REQUESTS)
    };

    // 2. 분당 추이 준비 확인 (이 카드 영역의 안내 카드)
    if let Some(note) = panel_message(lang, &state.snapshot.minutes) {
        render_notice_card(frame, state, card, area, title, note);
        return;
    }
    let now = state.snapshot.taken_at.unwrap_or_else(Utc::now);
    let Panel::Ready(buckets) = &state.snapshot.minutes else {
        return;
    };
    let series = if tokens {
        build_minute_series(buckets, now, |bucket| {
            u64::try_from(bucket.output_tokens).unwrap_or(0)
        })
    } else {
        build_minute_series(buckets, now, |bucket| bucket.api_requests)
    };

    // 3. 카드 테두리 (제목에 분당 최대값, 넓으면 아래 테두리에 시간 양 끝)
    let peak = series.iter().copied().max().unwrap_or(0);
    let mut block = interactive_card(
        state,
        card,
        area,
        &i18n::trend_title(lang, title, &format_compact(peak as i64)),
        "",
        theme::BORDER_DIM,
    );
    if area.width >= 24 {
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
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    // 4. 스파크라인 (분마다 같은 칸 수, 오른쪽 정렬)
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

// 6. 분 단위 시계열 -----------------------------------------------------------
// 분 문자열을 해석할 수 있으면 현재 분 기준 60칸에 배치, 아니면 조회 순서의 마지막 60개
pub(crate) fn build_minute_series(
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

// 7. 이벤트 피드 --------------------------------------------------------------
// 고정 머리글과 열 격자 (열 폭은 다른 표 카드와 같은 규칙: 자연 폭, 끌어 정한 폭, 머리글 정렬)
fn render_event_feed(frame: &mut Frame, state: &mut AppState, area: Rect) {
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

    // 2. 피드 표 (정렬한 행, 머리글 힌트는 종류별 건수)
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
        area,
        CardTable {
            card: CardId::EventFeed,
            title: i18n::live_events_title(lang, events.len()),
            hint: i18n::feed_hint(lang, api_count, tool_count, hook_count),
            columns: FEED_COLUMNS.to_vec(),
            rows: feed_rows,
            aux_lines: Vec::new(),
        },
    );
}

// 8. 이벤트 상세 --------------------------------------------------------------
// 피드에서 선택한 이벤트 항목과 남는 높이에 같은 종류 맥락 (맥락은 시각 내림차순 원본 기준)
fn render_event_detail(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // 1. 피드와 같은 정렬로 선택 이벤트 찾기 (이벤트가 없으면 그리지 않음)
    let lang = state.lang;
    let Panel::Ready(events) = &state.snapshot.events else {
        return;
    };
    if events.is_empty() {
        return;
    }
    let events = events.clone();
    let mut sorted = events.clone();
    sort_rows(
        &mut sorted,
        state.card_sorts[CardId::EventFeed as usize],
        compare_event_row,
    );
    let selected = state.card_views[CardId::EventFeed as usize]
        .selected
        .min(sorted.len() - 1);

    // 2. 상세 줄과 맥락 줄
    let detail_inner_width = area.width.saturating_sub(4) as usize;
    let mut detail_lines = build_event_detail_lines(lang, &sorted[selected], detail_inner_width);
    let spare_rows = usize::from(area.height.saturating_sub(2)).saturating_sub(detail_lines.len());
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
        area,
        TextCard {
            card: CardId::EventDetail,
            title: i18n::text(lang, Msg::TitleEventDetail).to_string(),
            hint: String::new(),
            lines: detail_lines,
        },
    );
}

// 9. 막대 ---------------------------------------------------------------------
// 채운 칸은 주어진 색, 빈 칸은 흐린 트랙, 0 보다 크면 최소 한 칸
pub(crate) fn build_bar(pct: Option<f64>, width: usize, fill_style: Style) -> Vec<Span<'static>> {
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

// 10. 시간 표시 ---------------------------------------------------------------
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

// 11. 폭 맞춤 줄 (내부) -------------------------------------------------------
// 후보 중 폭에 맞는 첫 줄, 마지막 후보도 넘치면 뒤 조각부터 덜어내고 말줄임
fn fit_variant_line(variants: Vec<Line<'static>>, width: usize) -> Line<'static> {
    // 1. 폭에 맞는 첫 후보
    let last = variants.len().saturating_sub(1);
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

    // 2. 넘침 말줄임 (뒤 조각부터 덜어냄)
    while spans.len() > 1 && used + 1 > width {
        if let Some(tail) = spans.pop() {
            used -= tail.width();
        }
    }
    let tail_style = spans.last().map_or(Style::default(), |span| span.style);
    spans.push(Span::styled("…", tail_style));
    Line::from(clip_spans_to_width(spans, width))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::i18n::Lang;
    use crate::tui::layout::grid::{DropTarget, apply_drop, default_rows};
    use crate::tui::test_support::sample_state_with_snapshot;
    use crate::tui::views::render_frame;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    // 최소 크기에서도 보이는 개요 카드, 중간 크기에 더해지는 추이, 큰 크기에 더해지는 차트
    const BASE_CARDS: [CardId; 9] = [
        CardId::ServerStatus,
        CardId::KpiRequests,
        CardId::KpiTokens,
        CardId::KpiCache,
        CardId::KpiTools,
        CardId::KpiHooks,
        CardId::SavingsEffect,
        CardId::EventFeed,
        CardId::EventDetail,
    ];
    const TREND_CARDS: [CardId; 2] = [CardId::TrendRequests, CardId::TrendTokens];
    const CHART_CARDS: [CardId; 3] = [
        CardId::ChartActivity,
        CardId::ChartTokens,
        CardId::ChartTools,
    ];

    fn minute_label(now: DateTime<Utc>, minutes_ago: i64) -> String {
        (now - chrono::Duration::minutes(minutes_ago))
            .format("%Y-%m-%dT%H:%M")
            .to_string()
    }

    fn overview_state(lang: Lang) -> AppState {
        let mut state = sample_state_with_snapshot();
        state.lang = lang;
        state
    }

    fn draw(state: &mut AppState, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_frame(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    // 영역 한 줄의 글자 (넓은 글자 뒤 칸은 건너뜀)
    fn line_text(buffer: &Buffer, y: u16, from: u16, to: u16) -> String {
        let mut text = String::new();
        let mut skip = 0usize;
        for x in from..to {
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

    fn card_rect(state: &AppState, card: CardId) -> Option<Rect> {
        state
            .hit_areas
            .card_rects
            .iter()
            .find(|(drawn, _)| *drawn == card)
            .map(|(_, rect)| *rect)
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

    // 60x16 은 상태·KPI 여섯 장(절약 포함)·피드 둘, 100x30 은 추이 둘 추가, 130x40 은 차트 셋까지
    #[test]
    fn the_overview_grid_draws_disjoint_cards_inside_the_body_at_the_size_matrix() {
        let wide: Vec<CardId> = TREND_CARDS.iter().chain(&CHART_CARDS).copied().collect();
        for lang in [Lang::En, Lang::Ko] {
            for (width, height, extra) in [
                (60u16, 16u16, &[][..]),
                (100, 30, &TREND_CARDS[..]),
                (130, 40, &wide[..]),
            ] {
                let case = format!("{lang:?} {width}x{height}");
                let mut state = overview_state(lang);
                draw(&mut state, width, height);

                // 보이는 카드 목록
                let mut expected: Vec<CardId> = BASE_CARDS.iter().chain(extra).copied().collect();
                let mut drawn: Vec<CardId> = state
                    .hit_areas
                    .card_rects
                    .iter()
                    .map(|(card, _)| *card)
                    .collect();
                expected.sort_by_key(|card| *card as usize);
                drawn.sort_by_key(|card| *card as usize);
                assert_eq!(drawn, expected, "{case}");

                // 본문 안, 서로 겹치지 않음
                let body = Rect::new(0, 2, width, height - 4);
                let rects = &state.hit_areas.card_rects;
                for (index, (card, rect)) in rects.iter().enumerate() {
                    assert_eq!(body.union(*rect), body, "{case} {card:?}");
                    for (other, other_rect) in &rects[index + 1..] {
                        assert!(!rect.intersects(*other_rect), "{case} {card:?} {other:?}");
                    }
                }

                // 절약 카드는 KPI 카드와 같은 줄, 같은 높이
                let kpi = card_rect(&state, CardId::KpiRequests).unwrap();
                for card in &BASE_CARDS[2..7] {
                    let rect = card_rect(&state, *card).unwrap();
                    assert_eq!(
                        (rect.y, rect.height),
                        (kpi.y, kpi.height),
                        "{case} {card:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn no_overview_row_shows_p50_p90_or_a_limits_title_in_either_language() {
        for lang in [Lang::En, Lang::Ko] {
            for (width, height) in [(60u16, 16u16), (100, 30), (130, 40)] {
                let mut state = overview_state(lang);
                let buffer = draw(&mut state, width, height);
                for y in 0..height {
                    let row = line_text(&buffer, y, 0, width);
                    for needle in ["p50", "p90", "Limits", "Limit usage trend", "한도"] {
                        assert!(
                            !row.contains(needle),
                            "{lang:?} {width}x{height} {needle}: {row}"
                        );
                    }
                }
            }
        }
    }

    // 큰 화면은 1행 순효과·2행 절약·손실 값, 최소 화면은 이름 없는 가장 짧은 형태
    #[test]
    fn the_savings_card_shows_net_saved_and_loss_on_two_lines_and_its_shortest_form_at_minimum() {
        let net = format!("+{}", format_duration_ms(189_000 - 12_600));
        let saved = format!("+{}", format_duration_ms(189_000));
        let loss = format!("-{}", format_duration_ms(12_600));
        for lang in [Lang::En, Lang::Ko] {
            let mut state = overview_state(lang);
            let buffer = draw(&mut state, 130, 40);
            let rect = card_rect(&state, CardId::SavingsEffect).unwrap();
            assert_eq!(rect.height, 4, "{lang:?}");
            let first = line_text(&buffer, rect.y + 1, rect.x + 1, rect.right() - 1);
            let second = line_text(&buffer, rect.y + 2, rect.x + 1, rect.right() - 1);
            assert!(first.contains(&net), "{lang:?}: {first}");
            assert!(
                second.contains(&saved) && second.contains(&loss),
                "{lang:?}: {second}"
            );
            let net_x = (rect.x..rect.right())
                .find(|x| buffer.cell((*x, rect.y + 1)).unwrap().symbol() == "+")
                .unwrap();
            assert_eq!(
                buffer.cell((net_x, rect.y + 1)).unwrap().fg,
                theme::COLOR_OK,
                "{lang:?}"
            );

            let buffer = draw(&mut state, 60, 16);
            let rect = card_rect(&state, CardId::SavingsEffect).unwrap();
            let first = line_text(&buffer, rect.y + 1, rect.x + 1, rect.right() - 1);
            let second = line_text(&buffer, rect.y + 2, rect.x + 1, rect.right() - 1);
            assert!(first.trim_start().starts_with('+'), "{lang:?}: {first}");
            assert!(second.trim_start().starts_with('+'), "{lang:?}: {second}");
            for label in [
                Msg::SavingsNetShort,
                Msg::SavingsSaved,
                Msg::SavingsSavedShort,
                Msg::SavingsLossShort,
            ] {
                let word = i18n::text(lang, label);
                assert!(
                    !first.contains(word) && !second.contains(word),
                    "{lang:?} {word}"
                );
            }
        }
    }

    // 최소 폭의 합이 폭을 넘는 여섯 장 줄: 넘치는 카드는 다음 화면 줄의 왼쪽 끝으로 (카드를 빼지 않음)
    #[test]
    fn a_six_card_row_wraps_at_60_columns_instead_of_dropping_a_card() {
        let base = default_rows(ActiveTab::Overview);
        let lifted = apply_drop(
            &base,
            CardId::KpiHooks,
            DropTarget::RowAbove(CardId::ServerStatus),
        )
        .unwrap();
        let joined = apply_drop(
            &lifted,
            CardId::ServerStatus,
            DropTarget::Before(CardId::KpiRequests),
        )
        .unwrap();
        for lang in [Lang::En, Lang::Ko] {
            let mut state = overview_state(lang);
            state.card_layout.set_grid(ActiveTab::Overview, joined);
            draw(&mut state, 60, 24);
            for card in &BASE_CARDS[..7] {
                assert!(card_rect(&state, *card).is_some(), "{lang:?} {card:?}");
            }
            let status = card_rect(&state, CardId::ServerStatus).unwrap();
            let tools = card_rect(&state, CardId::KpiTools).unwrap();
            let savings = card_rect(&state, CardId::SavingsEffect).unwrap();
            assert_eq!(status.y, tools.y, "{lang:?}");
            assert_eq!(savings.y, status.bottom(), "{lang:?}");
            assert_eq!(savings.x, 0, "{lang:?}");
        }
    }

    // 준비 전 패널: 카드마다 자기 영역의 안내 카드 (이웃 카드와 합치지 않음)
    #[test]
    fn pending_panels_show_a_notice_card_in_each_cards_own_rectangle() {
        for lang in [Lang::En, Lang::Ko] {
            let mut state = overview_state(lang);
            state.snapshot.overview = Panel::Pending;
            state.snapshot.today = Panel::Pending;
            state.snapshot.minutes = Panel::Pending;
            let buffer = draw(&mut state, 130, 40);
            for card in BASE_CARDS[1..7].iter().chain(&TREND_CARDS) {
                let count = state
                    .hit_areas
                    .card_rects
                    .iter()
                    .filter(|(drawn, _)| drawn == card)
                    .count();
                assert_eq!(count, 1, "{lang:?} {card:?}");
                assert!(
                    card_rect(&state, *card).unwrap().width < 70,
                    "{lang:?} {card:?}"
                );
            }
            for (card, title) in [
                (CardId::TrendRequests, Msg::TitleRequestsPerMin),
                (CardId::TrendTokens, Msg::TitleTokensPerMin),
            ] {
                let rect = card_rect(&state, card).unwrap();
                let top = line_text(&buffer, rect.y, rect.x, rect.right());
                assert!(top.contains(i18n::text(lang, title)), "{lang:?}: {top}");
            }
        }
    }
}
