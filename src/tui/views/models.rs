//! tui/views/models.rs
//! 모델·에이전트 탭: 모델별·에이전트 종류별 통계 표, 선택 행 상세 카드, 캐시 붕괴 후보 표.

use crate::store::limits::LimitSnapshot;
use crate::store::query::{AgentStatsRow, ModelStatsRow};
use crate::tui::cards::CardId;
use crate::tui::format::{
    fit_display, format_clock_time, format_compact, format_count, format_duration_ms, short_model,
};
use crate::tui::i18n::{self, Lang, Msg};
use crate::tui::layout::{SplitAxis, SplitGroup};
use crate::tui::sort::{compare_agent_row, compare_cache_break_row, compare_model_row, sort_rows};
use crate::tui::state::{AppState, Panel};
use crate::tui::theme;
use crate::tui::views::common::{
    CardTable, ColumnFit, ColumnSpec, PanelNote, TableCell, TextCard, clip_spans_to_width,
    interactive_card, panel_message, render_card_table, render_db_gate, render_notice_card,
    render_text_card,
};
use crate::tui::views::overview::{LIMIT_STALE_SECS, format_age, format_countdown};
use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Padding, Paragraph};

// 상세 항목 이름 칸 폭
const DETAIL_LABEL_WIDTH: usize = 23;

// 상세 카드 줄 수와 높이 (높이 = 줄 수 + 위아래 테두리 2줄)
const MODEL_DETAIL_LINES: usize = 6;
const AGENT_DETAIL_LINES: usize = 7;
const DETAIL_BORDER_ROWS: u16 = 2;
const MODEL_DETAIL_HEIGHT: u16 = MODEL_DETAIL_LINES as u16 + DETAIL_BORDER_ROWS;
const AGENT_DETAIL_HEIGHT: u16 = AGENT_DETAIL_LINES as u16 + DETAIL_BORDER_ROWS;

// 캐시 타이머 카드: 줄 수는 남은 시간·적중·최근 미스 셋, 본문이 이 높이 이상일 때만 기본 배치에 포함, 카드 최소 높이
const CACHE_CARD_HEIGHT: u16 = 3 + DETAIL_BORDER_ROWS;
const CACHE_CARD_MIN_BODY: u16 = 22;
const CACHE_CARD_MIN_ROWS: u16 = 3;
// 모델 탭 카드 그룹에서 캐시 타이머 카드의 번호 (낮은 화면에서 숨기는 카드)
const CACHE_TIMER_CARD: usize = 3;
// 캡시 적중 비율 색 기준 (0~1)
const CACHE_HIT_GOOD: f64 = 0.8;
const CACHE_HIT_FAIR: f64 = 0.5;
// 남은 시간이 이 초 미만이면 경고색
const CACHE_EXPIRY_WARN_SECS: i64 = 60;

// 모델 표 열 (정렬 열 번호와 순서 일치: sort.rs)
pub const MODEL_COLUMNS: [ColumnSpec; 13] = [
    (Msg::ColModel, 16, ColumnFit::Grow, Alignment::Left),
    (Msg::ColRequests, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColSessions, 5, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColAgents, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColInput, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColOutput, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColCacheRead, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColCacheWrite, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColHit, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColP50, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColP90, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColCostEst, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColLast, 8, ColumnFit::Fixed, Alignment::Right),
];

// 에이전트 표 열
pub const AGENT_COLUMNS: [ColumnSpec; 14] = [
    (Msg::ColAgent, 16, ColumnFit::Grow, Alignment::Left),
    (Msg::ColRuns, 5, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColRequests, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColSessions, 5, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColInput, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColOutput, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColCacheRead, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColCostEst, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColP50, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColP90, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColTools, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColErrors, 5, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColTopModel, 14, ColumnFit::Shrink, Alignment::Left),
    (Msg::ColLast, 8, ColumnFit::Fixed, Alignment::Right),
];

// 캐시 붕괴 표 열
pub const CACHE_BREAK_COLUMNS: [ColumnSpec; 7] = [
    (Msg::ColTime, 8, ColumnFit::Fixed, Alignment::Left),
    (Msg::ColAgent, 12, ColumnFit::Shrink, Alignment::Left),
    (Msg::ColModel, 16, ColumnFit::Grow, Alignment::Left),
    (Msg::ColNewCache, 9, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColPrevContext, 9, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColGap, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColLikelyCause, 16, ColumnFit::Grow, Alignment::Left),
];

// 1. 모델 탭 ------------------------------------------------------------------
// 모델 통계 표, 선택 모델 상세, 캐시 붕괴 후보 표
pub fn render_models_tab(frame: &mut Frame, area: Rect, state: &mut AppState) {
    if render_db_gate(frame, state, area) {
        return;
    }
    // 캐시 타이머 카드는 낮은 화면에서 숨김: 카드가 어느 칸에 놓였든 그 카드의 칸만 높이 0 (경계를 위로 끌면 드러남)
    let cache_height = if area.height >= CACHE_CARD_MIN_BODY {
        CACHE_CARD_HEIGHT
    } else {
        0
    };
    let upper_height = area.height - cache_height;
    let table_cut = (u32::from(upper_height) * 40 / 100) as u16;
    // 낮은 화면에서도 캐시 붕괴 표가 머리글과 한 행(4줄)은 갖도록 상세 카드가 먼저 줄어듦
    let detail_cut =
        table_cut + MODEL_DETAIL_HEIGHT.min(upper_height.saturating_sub(table_cut + 4));
    let hidden: &[usize] = if cache_height > 0 {
        &[]
    } else {
        &[CACHE_TIMER_CARD]
    };
    let (cuts, slot_mins) = state.card_layout.visible_slot_plan(
        SplitGroup::ModelsRows,
        hidden,
        &[
            table_cut,
            detail_cut - table_cut,
            upper_height - detail_cut,
            cache_height,
        ],
        &[5, 4, 3, CACHE_CARD_MIN_ROWS],
        area.height,
    );
    let panes = state.card_layout.split_cards(
        &mut state.hit_areas,
        SplitGroup::ModelsRows,
        SplitAxis::Rows,
        area,
        &cuts,
        &slot_mins,
    );

    // 1. 모델 통계 표와 선택 모델 상세
    let models: Option<Vec<ModelStatsRow>> = match &state.snapshot.model_stats {
        Panel::Ready(rows) => {
            let mut sorted = rows.clone();
            sort_rows(
                &mut sorted,
                state.card_sorts[CardId::ModelTable as usize],
                compare_model_row,
            );
            Some(sorted)
        }
        _ => None,
    };
    match &models {
        Some(sorted) => {
            let cost_total: f64 = sorted.iter().map(|row| row.cost_usd_estimate).sum();
            let rows: Vec<Vec<TableCell>> = sorted
                .iter()
                .map(|row| {
                    let wait = |millis: Option<i64>| match millis {
                        Some(ms) => vec![Span::styled(format_duration_ms(ms), theme::STYLE_TEXT)],
                        None => vec![Span::styled("-", theme::STYLE_DIM)],
                    };
                    vec![
                        vec![Span::raw(short_model(&row.model).to_string())],
                        vec![Span::raw(format_count(
                            i64::try_from(row.requests).unwrap_or(i64::MAX),
                        ))],
                        vec![Span::raw(format_count(
                            i64::try_from(row.sessions).unwrap_or(i64::MAX),
                        ))],
                        vec![Span::raw(format_count(
                            i64::try_from(row.agent_types).unwrap_or(i64::MAX),
                        ))],
                        vec![Span::raw(format_compact(row.input_tokens))],
                        vec![Span::raw(format_compact(row.output_tokens))],
                        vec![Span::raw(format_compact(row.cache_read_tokens))],
                        vec![Span::raw(format_compact(row.cache_creation_tokens))],
                        vec![Span::raw(format!("{:.1}%", row.cache_hit_ratio * 100.0))],
                        wait(row.wait_p50_ms),
                        wait(row.wait_p90_ms),
                        vec![Span::raw(format!("${:.2}", row.cost_usd_estimate))],
                        vec![Span::styled(
                            format_clock_time(&row.last_ts),
                            theme::STYLE_DIM,
                        )],
                    ]
                })
                .collect();
            render_card_table(
                frame,
                state,
                panes[0],
                CardTable {
                    card: CardId::ModelTable,
                    title: i18n::model_table_title(state.lang, cost_total),
                    hint: String::new(),
                    columns: MODEL_COLUMNS.to_vec(),
                    rows,
                    aux_lines: Vec::new(),
                },
            );
        }
        None => {
            let note = panel_message(state.lang, &state.snapshot.model_stats).unwrap_or_default();
            let title = i18n::text(state.lang, Msg::TitleModelStatsPending);
            render_notice_card(frame, state, CardId::ModelTable, panes[0], title, note);
        }
    }
    let selected = state.card_views[CardId::ModelTable as usize].selected;
    let selected_model = models.as_ref().and_then(|sorted| sorted.get(selected));
    let (detail_title, detail_lines) = build_model_detail(state.lang, selected_model);
    render_text_card(
        frame,
        state,
        panes[1],
        TextCard {
            card: CardId::ModelDetail,
            title: detail_title,
            hint: String::new(),
            lines: detail_lines,
        },
    );

    // 2. 캐시 붕괴 후보 표
    let breaks = match &state.snapshot.cache_breaks {
        Panel::Ready(rows) => {
            let mut sorted = rows.clone();
            sort_rows(
                &mut sorted,
                state.card_sorts[CardId::CacheBreaks as usize],
                compare_cache_break_row,
            );
            Some(sorted)
        }
        _ => None,
    };
    match breaks {
        Some(sorted) => {
            let rows: Vec<Vec<TableCell>> = sorted
                .iter()
                .map(|cache_break| {
                    let agent_style = if cache_break.agent == "main" || cache_break.agent.is_empty()
                    {
                        theme::STYLE_DIM
                    } else {
                        theme::STYLE_TEXT
                    };
                    vec![
                        vec![Span::raw(format_clock_time(&cache_break.ts))],
                        vec![Span::styled(cache_break.agent.clone(), agent_style)],
                        vec![Span::raw(short_model(&cache_break.model).to_string())],
                        vec![Span::styled(
                            format_compact(cache_break.cache_creation_tokens),
                            theme::STYLE_WARN,
                        )],
                        vec![Span::raw(format_compact(
                            cache_break.previous_context_tokens,
                        ))],
                        vec![Span::raw(format_duration_ms(
                            cache_break.gap_secs.saturating_mul(1_000),
                        ))],
                        vec![Span::raw(cache_break.likely_cause.clone())],
                    ]
                })
                .collect();
            render_card_table(
                frame,
                state,
                panes[2],
                CardTable {
                    card: CardId::CacheBreaks,
                    title: i18n::cache_breaks_title(state.lang, sorted.len()),
                    hint: String::new(),
                    columns: CACHE_BREAK_COLUMNS.to_vec(),
                    rows,
                    aux_lines: Vec::new(),
                },
            );
        }
        None => {
            let note = panel_message(state.lang, &state.snapshot.cache_breaks).unwrap_or_default();
            let title = i18n::text(state.lang, Msg::TitleCacheBreaks);
            render_notice_card(frame, state, CardId::CacheBreaks, panes[2], title, note);
        }
    }

    // 3. 캐시 타이머 카드
    render_cache_timer_card(frame, state, panes[3]);
}

// 1-1. 캐시 타이머 카드 -------------------------------------------------------
// 최신 공식 스냅샷의 프롬프트 캐시 상태: 만료까지 남은 시간(다시 그릴 때마다 줄어듦), 적중·미스 수, 최근 미스 시각·원인
fn render_cache_timer_card(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // 1. 숨김 높이면 그리지 않음, 준비되지 않은 패널은 안내 카드
    if area.height < CACHE_CARD_MIN_ROWS {
        return;
    }
    let lang = state.lang;
    let title = i18n::text(lang, Msg::TitleCacheTimer);
    let now = state.now_utc().timestamp();
    let inner_width = usize::from(area.width.saturating_sub(4));
    let built = match &state.snapshot.limits {
        Panel::Ready(data) => data
            .latest
            .as_ref()
            .map(|latest| build_cache_lines(lang, latest, now, inner_width)),
        other => {
            let note = panel_message(lang, other).unwrap_or_default();
            render_notice_card(frame, state, CardId::CacheTimer, area, title, note);
            return;
        }
    };

    // 2. 스냅샷 없음: 설정 안내
    let Some((lines, hint)) = built else {
        let note = PanelNote {
            message: i18n::text(lang, Msg::CacheEmpty).to_string(),
            color: theme::FG_DIM,
            hint: Some(Msg::LimitsEmptyHint),
        };
        render_notice_card(frame, state, CardId::CacheTimer, area, title, note);
        return;
    };

    // 3. 카드와 본문
    let block = interactive_card(
        state,
        CardId::CacheTimer,
        area,
        title,
        &hint,
        theme::BORDER_DIM,
    )
    .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

// 1-2. 캐시 타이머 줄 구성 ----------------------------------------------------
// 줄 1 = 상태 글자·단어·남은 시간·TTL, 줄 2 = 적중률·미스 수, 줄 3 = 최근 미스 시각·원인 (오래된 스냅샷은 흐리게)
fn build_cache_lines(
    lang: Lang,
    latest: &LimitSnapshot,
    now: i64,
    width: usize,
) -> (Vec<Line<'static>>, String) {
    // 1. 스냅샷 나이와 출처 문구
    let age = DateTime::parse_from_rfc3339(&latest.ts)
        .ok()
        .map(|stamp| (now - stamp.timestamp()).max(0));
    let stale = age.is_some_and(|secs| secs > LIMIT_STALE_SECS);
    let tone = |style: Style| if stale { theme::STYLE_DIM } else { style };
    let source = i18n::text(lang, Msg::LimitsOfficial);
    let hint = match age {
        Some(secs) => i18n::limits_hint(lang, source, &format_age(secs), stale),
        None => source.to_string(),
    };

    // 2. 줄 1: 만료까지 남은 시간 (남음은 단어와 글자 함께, 남은 시간이 짧으면 경고색)
    let (glyph, word, style, timing) = match (
        latest.cache_caching_observed,
        latest.cache_expires_at,
        latest.cache_warm,
    ) {
        (Some(false), _, _) => ("○", Msg::CacheUnknown, theme::STYLE_DIM, None),
        (_, Some(expires_at), _) if expires_at > now => {
            let left = expires_at - now;
            let style = if left < CACHE_EXPIRY_WARN_SECS {
                theme::STYLE_WARN
            } else {
                theme::STYLE_OK
            };
            (
                "●",
                Msg::CacheWarm,
                tone(style),
                Some(i18n::cache_left(lang, &format_countdown(left))),
            )
        }
        (_, Some(expires_at), _) => (
            "○",
            Msg::CacheCold,
            theme::STYLE_DIM,
            Some(i18n::cache_expired_ago(
                lang,
                &format_countdown(now - expires_at),
            )),
        ),
        (_, None, Some(true)) => ("●", Msg::CacheWarm, tone(theme::STYLE_OK), None),
        (_, None, Some(false)) => ("○", Msg::CacheCold, theme::STYLE_DIM, None),
        (_, None, None) => ("○", Msg::CacheUnknown, theme::STYLE_DIM, None),
    };
    let mut state_spans = vec![Span::styled(
        format!("{glyph} {}", i18n::text(lang, word)),
        style.add_modifier(Modifier::BOLD),
    )];
    if let Some(timing) = timing {
        state_spans.push(Span::styled(format!("  {timing}"), tone(theme::STYLE_TEXT)));
    }
    if let Some(ttl) = latest.cache_ttl.as_deref().filter(|ttl| !ttl.is_empty()) {
        state_spans.push(Span::styled(format!("  TTL {ttl}"), theme::STYLE_DIM));
    }

    // 3. 줄 2: 적중률 (색과 함께 경고 "!"·위험 "!!" 표식)과 미스 수
    let mut hit_spans = vec![Span::styled(
        format!("{} ", i18n::text(lang, Msg::CacheHit)),
        theme::STYLE_DIM,
    )];
    hit_spans.push(match latest.cache_hit_ratio {
        Some(ratio) => {
            let (hit_style, hit_mark) = if ratio >= CACHE_HIT_GOOD {
                (theme::STYLE_OK, "")
            } else if ratio >= CACHE_HIT_FAIR {
                (theme::STYLE_WARN, " !")
            } else {
                (theme::STYLE_CRIT, " !!")
            };
            Span::styled(
                format!("{:.0}%{hit_mark}", ratio * 100.0),
                tone(hit_style).add_modifier(Modifier::BOLD),
            )
        }
        None => Span::styled("-", theme::STYLE_DIM),
    });
    if let (Some(misses), Some(requests)) = (latest.cache_misses, latest.cache_requests) {
        hit_spans.push(Span::styled(
            format!("  {}", i18n::cache_misses_value(lang, misses, requests)),
            tone(theme::STYLE_TEXT),
        ));
    }

    // 4. 줄 3: 최근 미스 시각(지역 시각)과 원인 (원인 이름은 데이터라 번역하지 않음)
    let mut miss_spans: Vec<Span<'static>> = Vec::new();
    if let Some(missed_at) = latest.cache_last_miss_at {
        let clock = DateTime::from_timestamp(missed_at, 0)
            .map_or("--:--:--".to_string(), |stamp| {
                stamp.with_timezone(&Local).format("%H:%M:%S").to_string()
            });
        miss_spans.push(Span::styled(
            i18n::cache_last_miss(lang, &clock, &format_countdown(now - missed_at)),
            tone(theme::STYLE_TEXT),
        ));
    } else {
        miss_spans.push(Span::styled(
            i18n::text(lang, Msg::CacheNoMiss),
            theme::STYLE_DIM,
        ));
    }
    if let Some(cause) = latest
        .cache_last_miss_cause
        .as_deref()
        .filter(|cause| !cause.is_empty())
    {
        miss_spans.push(Span::styled(
            format!("  {}", i18n::cache_miss_cause(lang, cause)),
            tone(theme::STYLE_WARN),
        ));
    }
    let lines = [state_spans, hit_spans, miss_spans]
        .into_iter()
        .map(|spans| Line::from(clip_spans_to_width(spans, width)))
        .collect();
    (lines, hint)
}

// 2. 모델 상세 ----------------------------------------------------------------
// 제목과 항목 줄 (캐시 적중과 추정 비용 포함)
fn build_model_detail(lang: Lang, row: Option<&ModelStatsRow>) -> (String, Vec<Line<'static>>) {
    let Some(row) = row else {
        return (
            i18n::text(lang, Msg::TitleModelDetail).to_string(),
            vec![Line::from(Span::styled(
                i18n::text(lang, Msg::NoModelSelected),
                theme::STYLE_DIM,
            ))],
        );
    };
    let wait = |millis: Option<i64>| millis.map_or("-".to_string(), format_duration_ms);
    let pairs: [(&str, String); MODEL_DETAIL_LINES] = [
        (
            i18n::text(lang, Msg::ModelCounts),
            format!(
                "{} / {} / {}",
                format_count(i64::try_from(row.requests).unwrap_or(i64::MAX)),
                format_count(i64::try_from(row.sessions).unwrap_or(i64::MAX)),
                format_count(i64::try_from(row.agent_types).unwrap_or(i64::MAX)),
            ),
        ),
        (
            i18n::text(lang, Msg::InOut),
            format!(
                "{} / {}",
                format_compact(row.input_tokens),
                format_compact(row.output_tokens)
            ),
        ),
        (
            i18n::text(lang, Msg::CacheReadWrite),
            i18n::cache_read_write_value(
                lang,
                &format_compact(row.cache_read_tokens),
                &format_compact(row.cache_creation_tokens),
                row.cache_hit_ratio * 100.0,
            ),
        ),
        (
            i18n::text(lang, Msg::WaitP50P90),
            format!("{} / {}", wait(row.wait_p50_ms), wait(row.wait_p90_ms)),
        ),
        (
            i18n::text(lang, Msg::CostEstimate),
            i18n::cost_value(lang, row.cost_usd_estimate),
        ),
        (
            i18n::text(lang, Msg::LastRequest),
            format_clock_time(&row.last_ts),
        ),
    ];
    let lines = pairs
        .into_iter()
        .map(|(name, value)| {
            Line::from(vec![
                Span::styled(fit_display(name, DETAIL_LABEL_WIDTH), theme::STYLE_DIM),
                Span::styled(value, theme::STYLE_TEXT),
            ])
        })
        .collect();
    let base = i18n::text(lang, Msg::TitleModelDetail);
    let title = if row.model.is_empty() {
        base.to_string()
    } else {
        format!("{base} · {}", row.model)
    };
    (title, lines)
}

// 3. 에이전트 탭 --------------------------------------------------------------
// 에이전트 종류별 통계 표와 선택 에이전트 상세
pub fn render_agents_tab(frame: &mut Frame, area: Rect, state: &mut AppState) {
    if render_db_gate(frame, state, area) {
        return;
    }
    let detail_height = AGENT_DETAIL_HEIGHT.min(area.height.saturating_sub(5));
    let panes = state.card_layout.split_cards(
        &mut state.hit_areas,
        SplitGroup::AgentsRows,
        SplitAxis::Rows,
        area,
        &[area.height.saturating_sub(detail_height)],
        &[5, 3],
    );

    // 1. 에이전트 통계 표
    let agents: Option<Vec<AgentStatsRow>> = match &state.snapshot.agent_stats {
        Panel::Ready(rows) => {
            let mut sorted = rows.clone();
            sort_rows(
                &mut sorted,
                state.card_sorts[CardId::AgentTable as usize],
                compare_agent_row,
            );
            Some(sorted)
        }
        _ => None,
    };
    match &agents {
        Some(sorted) => {
            let cost_total: f64 = sorted.iter().map(|row| row.cost_usd_estimate).sum();
            let rows: Vec<Vec<TableCell>> = sorted
                .iter()
                .map(|row| {
                    let wait = |millis: Option<i64>| match millis {
                        Some(ms) => vec![Span::styled(format_duration_ms(ms), theme::STYLE_TEXT)],
                        None => vec![Span::styled("-", theme::STYLE_DIM)],
                    };
                    let name_style = if row.agent == "main" || row.agent.is_empty() {
                        theme::STYLE_DIM
                    } else {
                        theme::STYLE_TEXT
                    };
                    let error_style = if row.tool_errors > 0 {
                        theme::STYLE_CRIT
                    } else {
                        theme::STYLE_DIM
                    };
                    vec![
                        vec![Span::styled(row.agent.clone(), name_style)],
                        vec![Span::raw(format_count(
                            i64::try_from(row.runs).unwrap_or(i64::MAX),
                        ))],
                        vec![Span::raw(format_count(
                            i64::try_from(row.requests).unwrap_or(i64::MAX),
                        ))],
                        vec![Span::raw(format_count(
                            i64::try_from(row.sessions).unwrap_or(i64::MAX),
                        ))],
                        vec![Span::raw(format_compact(row.input_tokens))],
                        vec![Span::raw(format_compact(row.output_tokens))],
                        vec![Span::raw(format_compact(row.cache_read_tokens))],
                        vec![Span::raw(format!("${:.2}", row.cost_usd_estimate))],
                        wait(row.wait_p50_ms),
                        wait(row.wait_p90_ms),
                        vec![Span::raw(format_count(
                            i64::try_from(row.tool_calls).unwrap_or(i64::MAX),
                        ))],
                        vec![Span::styled(
                            format_count(i64::try_from(row.tool_errors).unwrap_or(i64::MAX)),
                            error_style,
                        )],
                        vec![Span::styled(
                            short_model(&row.top_model).to_string(),
                            theme::STYLE_DIM,
                        )],
                        vec![Span::styled(
                            format_clock_time(&row.last_ts),
                            theme::STYLE_DIM,
                        )],
                    ]
                })
                .collect();
            render_card_table(
                frame,
                state,
                panes[0],
                CardTable {
                    card: CardId::AgentTable,
                    title: i18n::agent_table_title(state.lang, sorted.len(), cost_total),
                    hint: String::new(),
                    columns: AGENT_COLUMNS.to_vec(),
                    rows,
                    aux_lines: Vec::new(),
                },
            );
        }
        None => {
            let note = panel_message(state.lang, &state.snapshot.agent_stats).unwrap_or_default();
            let title = i18n::text(state.lang, Msg::TitleAgentStatsPending);
            render_notice_card(frame, state, CardId::AgentTable, panes[0], title, note);
        }
    }

    // 2. 선택 에이전트 상세
    let selected = state.card_views[CardId::AgentTable as usize].selected;
    let selected_agent = agents.as_ref().and_then(|sorted| sorted.get(selected));
    let (detail_title, detail_lines) = build_agent_detail(state.lang, selected_agent);
    render_text_card(
        frame,
        state,
        panes[1],
        TextCard {
            card: CardId::AgentDetail,
            title: detail_title,
            hint: String::new(),
            lines: detail_lines,
        },
    );
}

// 4. 에이전트 상세 ------------------------------------------------------------
// 제목과 항목 줄 (도구 오류율, 주 모델, 추정 비용 포함)
fn build_agent_detail(lang: Lang, row: Option<&AgentStatsRow>) -> (String, Vec<Line<'static>>) {
    let Some(row) = row else {
        return (
            i18n::text(lang, Msg::TitleAgentDetail).to_string(),
            vec![Line::from(Span::styled(
                i18n::text(lang, Msg::NoAgentSelected),
                theme::STYLE_DIM,
            ))],
        );
    };
    let wait = |millis: Option<i64>| millis.map_or("-".to_string(), format_duration_ms);
    let error_rate = if row.tool_calls > 0 {
        row.tool_errors as f64 * 100.0 / row.tool_calls as f64
    } else {
        0.0
    };
    let top_model = if row.top_model.is_empty() {
        "-".to_string()
    } else {
        row.top_model.clone()
    };
    let pairs: [(&str, String); AGENT_DETAIL_LINES] = [
        (
            i18n::text(lang, Msg::AgentCounts),
            format!(
                "{} / {} / {}",
                format_count(i64::try_from(row.runs).unwrap_or(i64::MAX)),
                format_count(i64::try_from(row.requests).unwrap_or(i64::MAX)),
                format_count(i64::try_from(row.sessions).unwrap_or(i64::MAX)),
            ),
        ),
        (
            i18n::text(lang, Msg::InOut),
            format!(
                "{} / {}",
                format_compact(row.input_tokens),
                format_compact(row.output_tokens)
            ),
        ),
        (
            i18n::text(lang, Msg::CacheReadWrite),
            i18n::cache_read_write_value(
                lang,
                &format_compact(row.cache_read_tokens),
                &format_compact(row.cache_creation_tokens),
                row.cache_hit_ratio * 100.0,
            ),
        ),
        (
            i18n::text(lang, Msg::WaitP50P90),
            format!("{} / {}", wait(row.wait_p50_ms), wait(row.wait_p90_ms)),
        ),
        (
            i18n::text(lang, Msg::AgentToolCalls),
            i18n::tool_calls_value(
                lang,
                &format_count(i64::try_from(row.tool_calls).unwrap_or(i64::MAX)),
                &format_count(i64::try_from(row.tool_errors).unwrap_or(i64::MAX)),
                error_rate,
            ),
        ),
        (
            i18n::text(lang, Msg::CostEstimate),
            i18n::cost_value(lang, row.cost_usd_estimate),
        ),
        (
            i18n::text(lang, Msg::AgentTopLast),
            format!("{top_model} / {}", format_clock_time(&row.last_ts)),
        ),
    ];
    let lines = pairs
        .into_iter()
        .map(|(name, value)| {
            Line::from(vec![
                Span::styled(fit_display(name, DETAIL_LABEL_WIDTH), theme::STYLE_DIM),
                Span::styled(value, theme::STYLE_TEXT),
            ])
        })
        .collect();
    let base = i18n::text(lang, Msg::TitleAgentDetail);
    let title = if row.agent.is_empty() {
        base.to_string()
    } else {
        format!("{base} · {}", row.agent)
    };
    (title, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KO: Lang = Lang::Ko;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn model_detail_reports_cache_hit_cost_and_percentiles() {
        let row = ModelStatsRow {
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            model: "claude-sonnet-5-5".to_string(),
            requests: 900,
            sessions: 3,
            agent_types: 2,
            input_tokens: 1_500_000,
            output_tokens: 300_000,
            cache_read_tokens: 8_000_000,
            cache_creation_tokens: 500_000,
            cache_hit_ratio: 0.842,
            wait_p50_ms: Some(1_200),
            wait_p90_ms: None,
            cost_usd_estimate: 12.345,
            last_ts: "2026-11-15T01:00:00Z".to_string(),
        };
        let (title, lines) = build_model_detail(KO, Some(&row));
        assert_eq!(title, "모델 상세 · claude-sonnet-5-5");
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text.iter().any(|line| line.contains("900 / 3 / 2")));
        assert!(text.iter().any(|line| line.contains("적중 84.2%")));
        assert!(text.iter().any(|line| line.contains("1.2s / -")));
        assert!(text.iter().any(|line| line.contains("$12.35")));

        let (empty_title, empty_lines) = build_model_detail(KO, None);
        assert_eq!(empty_title, "모델 상세");
        assert_eq!(empty_lines.len(), 1);
    }

    #[test]
    fn agent_detail_reports_tool_error_rate_and_top_model() {
        let row = AgentStatsRow {
            agent: "worker".to_string(),
            runs: 3,
            requests: 40,
            sessions: 2,
            tool_calls: 50,
            tool_errors: 5,
            top_model: "claude-sonnet-5-5".to_string(),
            cost_usd_estimate: 1.5,
            ..AgentStatsRow::default()
        };
        let (title, lines) = build_agent_detail(KO, Some(&row));
        assert_eq!(title, "에이전트 상세 · worker");
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text.iter().any(|line| line.contains("3 / 40 / 2")));
        assert!(text.iter().any(|line| line.contains("오류율 10.0%")));
        assert!(text.iter().any(|line| line.contains("claude-sonnet-5-5")));

        let (empty_title, _) = build_agent_detail(KO, None);
        assert_eq!(empty_title, "에이전트 상세");
    }

    #[test]
    fn detail_titles_drop_the_separator_when_the_name_is_empty() {
        let nameless_model = ModelStatsRow::default();
        let (title, _) = build_model_detail(KO, Some(&nameless_model));
        assert_eq!(title, "모델 상세");

        let nameless_agent = AgentStatsRow::default();
        let (title, _) = build_agent_detail(KO, Some(&nameless_agent));
        assert_eq!(title, "에이전트 상세");
    }
}
