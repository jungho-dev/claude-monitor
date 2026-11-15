//! tui/views/gateway.rs
//! 게이트웨이 탭: 첫 바이트·첫 토큰·전체 지연 p50/p90 카드, 실시간 카운터 카드, 꺼짐 상태의 활성화 안내.

use crate::tui::cards::CardId;
use crate::tui::format::{format_count, format_duration_ms};
use crate::tui::layout::{SplitAxis, SplitGroup};
use crate::tui::state::{AppState, Panel};
use crate::tui::theme;
use crate::tui::views::common::{
    CardTable, ColumnFit, ColumnSpec, TableCell, TextCard, panel_message, render_card_table,
    render_db_gate, render_notice_card, render_text_card,
};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};

// 지연 표 카드 높이 = 지표 3행 + 머리글 1행 + 테두리 2행
const LATENCY_CARD_HEIGHT: u16 = 6;
// 지연 표 열 (지표 순서가 의미를 가지므로 정렬 없음)
const GATEWAY_COLUMNS: [ColumnSpec; 3] = [
    ("지표", 14, ColumnFit::Grow, Alignment::Left),
    ("p50", 12, ColumnFit::Fixed, Alignment::Right),
    ("p90", 12, ColumnFit::Fixed, Alignment::Right),
];

// 1. 게이트웨이 탭 ------------------------------------------------------------
pub fn render_gateway_tab(frame: &mut Frame, area: Rect, state: &mut AppState) {
    // 1. 꺼짐: 활성화 안내
    if !state.config.gateway.enabled {
        let config_hint = match &state.config.source_path {
            Some(path) => path.display().to_string(),
            None => state.config.data_dir.join("rap.toml").display().to_string(),
        };
        let lines = vec![
            Line::from(Span::styled("게이트웨이 꺼짐", theme::STYLE_WARN)),
            Line::from(""),
            Line::from(Span::styled("켜는 방법", theme::STYLE_ACCENT)),
            Line::from(vec![
                Span::styled("  1) ", theme::STYLE_DIM),
                Span::styled(
                    format!("{config_hint}  [gateway]  enabled = true"),
                    theme::STYLE_TEXT,
                ),
                Span::styled("  (serve 재시작)", theme::STYLE_DIM),
            ]),
            Line::from(vec![
                Span::styled("  2) ", theme::STYLE_DIM),
                Span::styled(
                    format!(
                        "ANTHROPIC_BASE_URL=http://127.0.0.1:{}",
                        state.config.ports.gateway
                    ),
                    theme::STYLE_TEXT,
                ),
                Span::styled("  (Claude Code 환경 변수)", theme::STYLE_DIM),
            ]),
            Line::from(""),
            Line::from(Span::styled("주의", theme::STYLE_ACCENT)),
            Line::from(vec![
                Span::styled("  - ", theme::STYLE_DIM),
                Span::styled(
                    "ANTHROPIC_BASE_URL 지정 시 ANTHROPIC_DEFAULT_HAIKU_MODEL 도 함께 설정",
                    theme::STYLE_WARN,
                ),
            ]),
            Line::from(Span::styled(
                "  - 게이트웨이를 켜기 전에는 첫 바이트·첫 토큰 지연이 집계되지 않음",
                theme::STYLE_DIM,
            )),
        ];
        render_text_card(
            frame,
            state,
            area,
            TextCard {
                card: CardId::GatewayGuide,
                title: "게이트웨이 (꺼짐)".to_string(),
                hint: String::new(),
                header: None,
                header_cells: Vec::new(),
                lines,
            },
        );
        return;
    }

    // 2. 켜짐: DB 와 지연 집계, 지연 카드와 카운터 카드 분할
    if render_db_gate(frame, state, area) {
        return;
    }
    let panes = state.card_layout.split_cards(
        &mut state.hit_areas.split_hits,
        SplitGroup::GatewayRows,
        SplitAxis::Rows,
        area,
        &[LATENCY_CARD_HEIGHT.min(area.height.saturating_sub(3))],
        &[5, 3],
    );
    let timing = match &state.snapshot.gateway {
        Panel::Ready(stats) => stats.clone(),
        other => {
            let (message, color) = panel_message(other).unwrap_or((String::new(), theme::FG_DIM));
            render_notice_card(
                frame,
                state,
                CardId::GatewayLatency,
                panes[0],
                "게이트웨이 지연",
                &message,
                color,
            );
            render_live_counters(frame, state, panes[1]);
            return;
        }
    };

    // 3. 지연 표 (측정 없음은 하이픈, 고정 지표 3행이라 정렬 없음)
    let metric_rows = [
        (
            "첫 바이트",
            timing.first_byte_p50_ms,
            timing.first_byte_p90_ms,
        ),
        (
            "첫 토큰",
            timing.first_token_p50_ms,
            timing.first_token_p90_ms,
        ),
        ("전체", timing.total_p50_ms, timing.total_p90_ms),
    ];
    let rows: Vec<Vec<TableCell>> = metric_rows
        .iter()
        .map(|(label, p50, p90)| {
            let latency = |millis: &Option<i64>| match millis {
                Some(ms) => vec![Span::styled(format_duration_ms(*ms), theme::STYLE_TEXT)],
                None => vec![Span::styled("-", theme::STYLE_DIM)],
            };
            vec![
                vec![Span::raw(label.to_string())],
                latency(p50),
                latency(p90),
            ]
        })
        .collect();
    render_card_table(
        frame,
        state,
        panes[0],
        CardTable {
            card: CardId::GatewayLatency,
            title: format!(
                "게이트웨이 지연 · 요청 {} · 오류 {}",
                format_count(i64::try_from(timing.requests).unwrap_or(i64::MAX)),
                format_count(i64::try_from(timing.errors).unwrap_or(i64::MAX))
            ),
            hint: String::new(),
            columns: GATEWAY_COLUMNS.to_vec(),
            rows,
        },
    );
    render_live_counters(frame, state, panes[1]);
}

// 2. 실시간 카운터 ------------------------------------------------------------
fn render_live_counters(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let line = match &state.snapshot.status {
        Panel::Ready(status) => Line::from(vec![
            Span::styled("게이트웨이 요청 ", theme::STYLE_DIM),
            Span::styled(
                format_count(i64::try_from(status.counters.gateway_requests).unwrap_or(i64::MAX)),
                theme::STYLE_STRONG,
            ),
            Span::styled("   진행 중 스트림 ", theme::STYLE_DIM),
            Span::styled(
                format_count(
                    i64::try_from(status.counters.gateway_active_streams).unwrap_or(i64::MAX),
                ),
                theme::STYLE_STRONG,
            ),
        ]),
        other => {
            let (message, _) = panel_message(other).unwrap_or((String::new(), theme::FG_DIM));
            Line::from(Span::styled(message, theme::STYLE_DIM))
        }
    };
    render_text_card(
        frame,
        state,
        area,
        TextCard {
            card: CardId::GatewayLive,
            title: "실시간 (serve 카운터)".to_string(),
            hint: String::new(),
            header: None,
            header_cells: Vec::new(),
            lines: vec![line],
        },
    );
}
