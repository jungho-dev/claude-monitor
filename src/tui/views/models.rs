//! tui/views/models.rs
//! 모델·에이전트 탭: 모델별·에이전트 종류별 통계 표, 선택 행 상세 카드, 캐시 붕괴 후보 표.

use crate::inventory::Inventory;
use crate::store::limits::LimitSnapshot;
use crate::store::query::{AgentRunSummary, AgentStatsRow, ModelStatsRow};
use crate::tui::cards::CardId;
use crate::tui::format::{
    fit_display, format_clock_time, format_compact, format_count, format_duration_ms, short_model,
};
use crate::tui::i18n::{self, Lang, Msg};
use crate::tui::layout::grid::{CardSlot, SlotHeight};
use crate::tui::sort::{compare_agent_row, compare_cache_break_row, compare_model_row, sort_rows};
use crate::tui::state::{ActiveTab, AppState, Panel};
use crate::tui::theme;
use crate::tui::views::common::{
    CardTable, ColumnFit, ColumnSpec, PanelNote, TableCell, TextCard, clip_spans_to_width,
    interactive_card, panel_message, render_card_table, render_db_gate, render_notice_card,
    render_text_card,
};
use crate::tui::views::detail::{
    DETAIL_LABEL_WIDTH, ShareBasis, WindowTotals, cache_split, first_last, pair_line, per_request,
    section_line, share_lines, window_share,
};
use crate::tui::views::overview::{LIMIT_STALE_SECS, format_age, format_countdown};
use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Padding, Paragraph};

// 상세 카드 기본 높이 (높이 = 보이는 줄 수 + 위아래 테두리 2줄, 넘치는 분포 목록은 스크롤)
// 모델 = 항목 11줄 + 에이전트 구역 제목과 두 줄, 에이전트 = 본문의 절반 남짓 (최소는 항목 9줄)
const DETAIL_BORDER_ROWS: u16 = 2;
const MODEL_DETAIL_HEIGHT: u16 = 14 + DETAIL_BORDER_ROWS;
const AGENT_DETAIL_MIN_HEIGHT: u16 = 9 + DETAIL_BORDER_ROWS;
const AGENT_DETAIL_SHARE_PERCENT: u32 = 55;
// 에이전트 표가 지키는 최소 높이 (머리글과 두어 행)
const AGENT_TABLE_MIN: u16 = 5;
// 최근 실행 줄의 실행 id 앞부분 길이
const RUN_ID_PREFIX: usize = 8;

// 캐시 타이머 카드: 줄 수는 남은 시간·적중·최근 미스 셋, 본문이 이 높이 이상일 때만 기본 배치에 포함, 카드 최소 높이
const CACHE_CARD_HEIGHT: u16 = 3 + DETAIL_BORDER_ROWS;
const CACHE_CARD_MIN_BODY: u16 = 22;
const CACHE_CARD_MIN_ROWS: u16 = 3;
// 모델 표 기본 높이 상한 (큰 화면에서 상세 전체와 캐시 붕괴 두 행이 보이도록)
const MODEL_TABLE_MAX_HEIGHT: u16 = 10;
// 캡시 적중 비율 색 기준 (0~1)
const CACHE_HIT_GOOD: f64 = 0.8;
const CACHE_HIT_FAIR: f64 = 0.5;
// 남은 시간이 이 초 미만이면 경고색
const CACHE_EXPIRY_WARN_SECS: i64 = 60;

// 모델 표 열 (정렬 열 번호와 순서 일치: sort.rs)
pub const MODEL_COLUMNS: [ColumnSpec; 14] = [
    (Msg::ColModel, 16, ColumnFit::Grow, Alignment::Left),
    (Msg::ColRequests, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColSessions, 5, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColAgents, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColInput, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColOutput, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColCacheRead, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColCacheWrite, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColHit, 6, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColAvgWait, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColTotalWait, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColCostEst, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColLast, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColTokPerReq, 8, ColumnFit::Fixed, Alignment::Right),
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
    (Msg::ColAvgWait, 8, ColumnFit::Fixed, Alignment::Right),
    (Msg::ColTotalWait, 8, ColumnFit::Fixed, Alignment::Right),
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
    // 캐시 타이머 카드는 낮은 화면에서 숨김: 카드가 어느 줄에 놓였든 조건을 넘기지 않아 줄 동료가 폭을 받음
    let cache_shown = area.height >= CACHE_CARD_MIN_BODY;
    let cache_height = if cache_shown { CACHE_CARD_HEIGHT } else { 0 };
    let upper_height = area.height - cache_height;
    let table_height = ((u32::from(upper_height) * 40 / 100) as u16).min(MODEL_TABLE_MAX_HEIGHT);
    // 낮은 화면에서도 캐시 붕괴 표가 머리글과 한 행(4줄)은 갖도록 상세 카드가 먼저 줄어듦
    let detail_height = MODEL_DETAIL_HEIGHT.min(upper_height.saturating_sub(table_height + 4));
    let mut slots = vec![
        CardSlot {
            card: CardId::ModelTable,
            min_w: 24,
            min_h: 4,
            height: SlotHeight::Fixed(table_height),
            weight: 3,
            rank: 0,
            reserve: 0,
        },
        CardSlot {
            card: CardId::ModelDetail,
            min_w: 20,
            min_h: 4,
            height: SlotHeight::Fixed(detail_height),
            weight: 2,
            rank: 0,
            reserve: 0,
        },
        CardSlot {
            card: CardId::CacheBreaks,
            min_w: 24,
            min_h: 4,
            height: SlotHeight::Fill,
            weight: 3,
            rank: 0,
            reserve: 0,
        },
    ];
    if cache_shown {
        slots.push(CardSlot {
            card: CardId::CacheTimer,
            min_w: 20,
            min_h: CACHE_CARD_MIN_ROWS,
            height: SlotHeight::Fixed(CACHE_CARD_HEIGHT),
            weight: 1,
            rank: 0,
            reserve: 0,
        });
    }
    let layout = state.card_layout;
    let placed = layout.arrange_tab(&mut state.hit_areas, ActiveTab::Models, area, &slots);
    let table_area = placed
        .iter()
        .find(|(card, _)| *card == CardId::ModelTable)
        .map(|(_, rect)| *rect);
    let detail_area = placed
        .iter()
        .find(|(card, _)| *card == CardId::ModelDetail)
        .map(|(_, rect)| *rect);
    let breaks_area = placed
        .iter()
        .find(|(card, _)| *card == CardId::CacheBreaks)
        .map(|(_, rect)| *rect);
    let timer_area = placed
        .iter()
        .find(|(card, _)| *card == CardId::CacheTimer)
        .map(|(_, rect)| *rect);

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
    let model_totals = models
        .as_ref()
        .map_or(WindowTotals::default(), |sorted| WindowTotals {
            requests: sorted.iter().map(|row| row.requests).sum(),
            cost: sorted.iter().map(|row| row.cost_usd_estimate).sum(),
        });
    match (&models, table_area) {
        (_, None) => {}
        (Some(sorted), Some(table_area)) => {
            let cost_total = model_totals.cost;
            let rows: Vec<Vec<TableCell>> = sorted
                .iter()
                .map(|row| {
                    let wait = |millis: Option<i64>| match millis {
                        Some(ms) => vec![Span::styled(format_duration_ms(ms), theme::STYLE_TEXT)],
                        None => vec![Span::styled("-", theme::STYLE_DIM)],
                    };
                    let total_tokens = row.input_tokens
                        + row.output_tokens
                        + row.cache_read_tokens
                        + row.cache_creation_tokens;
                    let tokens_per_request = if row.requests > 0 {
                        vec![Span::raw(format_compact(
                            (total_tokens as f64 / row.requests as f64).round() as i64,
                        ))]
                    } else {
                        vec![Span::styled("-", theme::STYLE_DIM)]
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
                        wait(row.wait_avg_ms),
                        wait(Some(row.wait_total_ms).filter(|millis| *millis > 0)),
                        vec![Span::raw(format!("${:.2}", row.cost_usd_estimate))],
                        vec![Span::styled(
                            format_clock_time(&row.last_ts),
                            theme::STYLE_DIM,
                        )],
                        tokens_per_request,
                    ]
                })
                .collect();
            render_card_table(
                frame,
                state,
                table_area,
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
        (None, Some(table_area)) => {
            let note = panel_message(state.lang, &state.snapshot.model_stats).unwrap_or_default();
            let title = i18n::text(state.lang, Msg::TitleModelStatsPending);
            render_notice_card(frame, state, CardId::ModelTable, table_area, title, note);
        }
    }
    let selected = state.card_views[CardId::ModelTable as usize].selected;
    let selected_model = models.as_ref().and_then(|sorted| sorted.get(selected));
    let (detail_title, detail_lines) = build_model_detail(state.lang, selected_model, model_totals);
    if let Some(detail_area) = detail_area {
        render_text_card(
            frame,
            state,
            detail_area,
            TextCard {
                card: CardId::ModelDetail,
                title: detail_title,
                hint: String::new(),
                lines: detail_lines,
            },
        );
    }

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
    match (breaks, breaks_area) {
        (_, None) => {}
        (Some(sorted), Some(breaks_area)) => {
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
                breaks_area,
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
        (None, Some(breaks_area)) => {
            let note = panel_message(state.lang, &state.snapshot.cache_breaks).unwrap_or_default();
            let title = i18n::text(state.lang, Msg::TitleCacheBreaks);
            render_notice_card(frame, state, CardId::CacheBreaks, breaks_area, title, note);
        }
    }

    // 3. 캐시 타이머 카드
    if let Some(timer_area) = timer_area {
        render_cache_timer_card(frame, state, timer_area);
    }
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
// 제목과 항목 줄 (캐시 적중·생성 분리, 추정 비용, 요청당 비용, 구간 점유), 이어서 에이전트별 비용 구역
fn build_model_detail(
    lang: Lang,
    row: Option<&ModelStatsRow>,
    window: WindowTotals,
) -> (String, Vec<Line<'static>>) {
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
    let total_tokens =
        row.input_tokens + row.output_tokens + row.cache_read_tokens + row.cache_creation_tokens;
    let tokens_per_request = if row.requests > 0 {
        format_compact((total_tokens as f64 / row.requests as f64).round() as i64)
    } else {
        "-".to_string()
    };
    let pairs: [(&str, String); 11] = [
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
            i18n::text(lang, Msg::DetailCacheSplit),
            cache_split(row.cache_creation_5m_tokens, row.cache_creation_1h_tokens),
        ),
        (
            i18n::text(lang, Msg::WaitAvgMax),
            format!("{} / {}", wait(row.wait_avg_ms), wait(row.wait_max_ms)),
        ),
        (
            i18n::text(lang, Msg::WaitTotal),
            wait(Some(row.wait_total_ms).filter(|millis| *millis > 0)),
        ),
        (
            i18n::text(lang, Msg::DetailTokensPerReq),
            format!("{} / {tokens_per_request}", format_compact(total_tokens)),
        ),
        (
            i18n::text(lang, Msg::CostEstimate),
            i18n::cost_value(lang, row.cost_usd_estimate),
        ),
        (
            i18n::text(lang, Msg::DetailPerRequest),
            per_request(row.cost_usd_estimate, row.output_tokens, row.requests),
        ),
        (
            i18n::text(lang, Msg::DetailWindowShare),
            window_share(lang, row.requests, row.cost_usd_estimate, window),
        ),
        (
            i18n::text(lang, Msg::DetailFirstLast),
            first_last(&row.first_ts, &row.last_ts),
        ),
    ];
    let mut lines: Vec<Line<'static>> = pairs
        .into_iter()
        .map(|(name, value)| pair_line(name, value))
        .collect();
    lines.extend(share_lines(
        lang,
        Msg::SectionAgentsByCost,
        &row.agents,
        ShareBasis::Cost,
        row.cost_usd_estimate,
    ));
    let base = i18n::text(lang, Msg::TitleModelDetail);
    let title = if row.model.is_empty() {
        base.to_string()
    } else {
        format!("{base} · {}", row.model)
    };
    (title, lines)
}

// 3. 에이전트 행 병합 ---------------------------------------------------------
// 호출 기록 행 뒤에 호출 없는 설정 에이전트(~/.claude/agents, 이름 순)와 내장 에이전트를 값 0 행으로 붙임
// 집계가 준비되지 않으면 None, 설정 목록을 읽기 전이면 호출 기록 행만
pub fn agent_rows(
    stats: &Panel<Vec<AgentStatsRow>>,
    inventory: &Panel<Inventory>,
) -> Option<Vec<AgentStatsRow>> {
    let Panel::Ready(stats) = stats else {
        return None;
    };
    let mut rows = stats.clone();
    let Panel::Ready(inventory) = inventory else {
        return Some(rows);
    };
    let configured = inventory.agents.iter().map(|entry| &entry.name);
    for name in configured.chain(inventory.builtin_agents.iter()) {
        if !rows.iter().any(|row| row.agent == *name) {
            rows.push(AgentStatsRow {
                agent: name.clone(),
                ..AgentStatsRow::default()
            });
        }
    }
    Some(rows)
}

// 3-1. 에이전트 탭 ------------------------------------------------------------
// 에이전트 종류별 통계 표(호출 없는 설정 에이전트 포함)와 선택 에이전트 상세
pub fn render_agents_tab(frame: &mut Frame, area: Rect, state: &mut AppState) {
    if render_db_gate(frame, state, area) {
        return;
    }
    // 상세가 분포·최근 실행까지 담아 본문의 절반 남짓을 쓰고, 표는 머리글과 두어 행을 남김
    let detail_height = ((u32::from(area.height) * AGENT_DETAIL_SHARE_PERCENT / 100) as u16)
        .max(AGENT_DETAIL_MIN_HEIGHT)
        .min(area.height.saturating_sub(AGENT_TABLE_MIN));
    let slots = [
        CardSlot {
            card: CardId::AgentTable,
            min_w: 24,
            min_h: AGENT_TABLE_MIN,
            height: SlotHeight::Fill,
            weight: 3,
            rank: 0,
            reserve: 0,
        },
        CardSlot {
            card: CardId::AgentDetail,
            min_w: 20,
            min_h: 3,
            height: SlotHeight::Fixed(detail_height),
            weight: 2,
            rank: 0,
            reserve: 0,
        },
    ];
    let layout = state.card_layout;
    let placed = layout.arrange_tab(&mut state.hit_areas, ActiveTab::Agents, area, &slots);
    let table_area = placed
        .iter()
        .find(|(card, _)| *card == CardId::AgentTable)
        .map(|(_, rect)| *rect);
    let detail_area = placed
        .iter()
        .find(|(card, _)| *card == CardId::AgentDetail)
        .map(|(_, rect)| *rect);

    // 1. 에이전트 통계 표
    let agents: Option<Vec<AgentStatsRow>> =
        agent_rows(&state.snapshot.agent_stats, &state.inventory).map(|mut sorted| {
            sort_rows(
                &mut sorted,
                state.card_sorts[CardId::AgentTable as usize],
                compare_agent_row,
            );
            sorted
        });
    let agent_totals = agents
        .as_ref()
        .map_or(WindowTotals::default(), |sorted| WindowTotals {
            requests: sorted.iter().map(|row| row.requests).sum(),
            cost: sorted.iter().map(|row| row.cost_usd_estimate).sum(),
        });
    match (&agents, table_area) {
        (_, None) => {}
        (Some(sorted), Some(table_area)) => {
            let cost_total = agent_totals.cost;
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
                        wait(row.wait_avg_ms),
                        wait(Some(row.wait_total_ms).filter(|millis| *millis > 0)),
                        vec![Span::raw(format_count(
                            i64::try_from(row.tool_calls).unwrap_or(i64::MAX),
                        ))],
                        vec![Span::styled(
                            format_count(i64::try_from(row.tool_errors).unwrap_or(i64::MAX)),
                            error_style,
                        )],
                        vec![Span::styled(
                            if row.top_model.is_empty() {
                                "-".to_string()
                            } else {
                                short_model(&row.top_model).to_string()
                            },
                            theme::STYLE_DIM,
                        )],
                        vec![Span::styled(
                            if row.last_ts.is_empty() {
                                "-".to_string()
                            } else {
                                format_clock_time(&row.last_ts)
                            },
                            theme::STYLE_DIM,
                        )],
                    ]
                })
                .collect();
            render_card_table(
                frame,
                state,
                table_area,
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
        (None, Some(table_area)) => {
            let note = panel_message(state.lang, &state.snapshot.agent_stats).unwrap_or_default();
            let title = i18n::text(state.lang, Msg::TitleAgentStatsPending);
            render_notice_card(frame, state, CardId::AgentTable, table_area, title, note);
        }
    }

    // 2. 선택 에이전트 상세
    let selected = state.card_views[CardId::AgentTable as usize].selected;
    let selected_agent = agents.as_ref().and_then(|sorted| sorted.get(selected));
    let (detail_title, detail_lines) =
        build_agent_detail(state.lang, selected_agent, agent_totals, &state.inventory);
    if let Some(detail_area) = detail_area {
        render_text_card(
            frame,
            state,
            detail_area,
            TextCard {
                card: CardId::AgentDetail,
                title: detail_title,
                hint: String::new(),
                lines: detail_lines,
            },
        );
    }
}

// 4. 에이전트 상세 ------------------------------------------------------------
// 제목과 항목 줄 (도구 오류율, 실행당 요청·비용, 구간 점유, 정의 파일 설정값, 주 모델), 이어서 모델·도구·최근 실행 구역
fn build_agent_detail(
    lang: Lang,
    row: Option<&AgentStatsRow>,
    window: WindowTotals,
    inventory: &Panel<Inventory>,
) -> (String, Vec<Line<'static>>) {
    let Some(row) = row else {
        return (
            i18n::text(lang, Msg::TitleAgentDetail).to_string(),
            vec![Line::from(Span::styled(
                i18n::text(lang, Msg::NoAgentSelected),
                theme::STYLE_DIM,
            ))],
        );
    };
    let count = |value: u64| format_count(i64::try_from(value).unwrap_or(i64::MAX));
    let wait = |millis: Option<i64>| millis.map_or("-".to_string(), format_duration_ms);
    let error_rate = if row.tool_calls > 0 {
        row.tool_errors as f64 * 100.0 / row.tool_calls as f64
    } else {
        0.0
    };
    let total_tokens =
        row.input_tokens + row.output_tokens + row.cache_read_tokens + row.cache_creation_tokens;
    let tokens_per_request = if row.requests > 0 {
        format_compact((total_tokens as f64 / row.requests as f64).round() as i64)
    } else {
        "-".to_string()
    };
    let top_model = if row.top_model.is_empty() {
        "-".to_string()
    } else {
        row.top_model.clone()
    };
    let per_run = if row.runs > 0 {
        format!(
            "{:.1} / ${:.3}",
            row.requests as f64 / row.runs as f64,
            row.cost_usd_estimate / row.runs as f64
        )
    } else {
        "-".to_string()
    };
    let pairs: [(&str, String); 14] = [
        (
            i18n::text(lang, Msg::AgentCounts),
            format!(
                "{} / {} / {}",
                count(row.runs),
                count(row.requests),
                count(row.sessions),
            ),
        ),
        (i18n::text(lang, Msg::DetailPerRun), per_run),
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
            i18n::text(lang, Msg::DetailCacheSplit),
            cache_split(row.cache_creation_5m_tokens, row.cache_creation_1h_tokens),
        ),
        (
            i18n::text(lang, Msg::WaitAvgMax),
            format!("{} / {}", wait(row.wait_avg_ms), wait(row.wait_max_ms)),
        ),
        (
            i18n::text(lang, Msg::WaitTotal),
            wait(Some(row.wait_total_ms).filter(|millis| *millis > 0)),
        ),
        (
            i18n::text(lang, Msg::DetailTokensPerReq),
            format!("{} / {tokens_per_request}", format_compact(total_tokens)),
        ),
        (
            i18n::text(lang, Msg::AgentToolCalls),
            i18n::tool_calls_value(
                lang,
                &count(row.tool_calls),
                &count(row.tool_errors),
                error_rate,
            ),
        ),
        (
            i18n::text(lang, Msg::CostEstimate),
            i18n::cost_value(lang, row.cost_usd_estimate),
        ),
        (
            i18n::text(lang, Msg::DetailWindowShare),
            window_share(lang, row.requests, row.cost_usd_estimate, window),
        ),
        (
            i18n::text(lang, Msg::DetailConfigured),
            agent_config(lang, &row.agent, inventory),
        ),
        (
            i18n::text(lang, Msg::AgentTopLast),
            format!("{top_model} / {}", format_clock_time(&row.last_ts)),
        ),
        (
            i18n::text(lang, Msg::DetailFirstSeen),
            if row.first_ts.is_empty() {
                "-".to_string()
            } else {
                format_clock_time(&row.first_ts)
            },
        ),
    ];
    let mut lines: Vec<Line<'static>> = pairs
        .into_iter()
        .map(|(name, value)| pair_line(name, value))
        .collect();

    // 모델별 비용, 도구별 호출, 최근 실행
    lines.extend(share_lines(
        lang,
        Msg::SectionModelsByCost,
        &row.models,
        ShareBasis::Cost,
        row.cost_usd_estimate,
    ));
    lines.extend(share_lines(
        lang,
        Msg::SectionToolsByCalls,
        &row.top_tools,
        ShareBasis::Calls,
        row.tool_calls as f64,
    ));
    if !row.recent_runs.is_empty() {
        lines.push(section_line(lang, Msg::SectionRecentRuns));
        lines.extend(row.recent_runs.iter().map(|run| run_line(lang, run)));
    }

    let base = i18n::text(lang, Msg::TitleAgentDetail);
    let title = if row.agent.is_empty() {
        base.to_string()
    } else {
        format!("{base} · {}", row.agent)
    };
    (title, lines)
}

// 4-1. 정의 파일 설정값 -------------------------------------------------------
// ~/.claude/agents 의 같은 이름 정의 (메인·내장 에이전트는 정의 파일이 없음, 목록을 읽기 전이면 읽는 중)
fn agent_config(lang: Lang, agent: &str, inventory: &Panel<Inventory>) -> String {
    if agent == "main" || agent.is_empty() {
        return i18n::text(lang, Msg::DetailConfigMain).to_string();
    }
    let loaded = match inventory {
        Panel::Ready(inventory) => inventory,
        Panel::Pending => return i18n::text(lang, Msg::DetailConfigPending).to_string(),
        Panel::Failed(_) => return "-".to_string(),
    };
    let found = loaded.agents.iter().find(|entry| entry.name == agent);
    if found.is_none() && loaded.builtin_agents.iter().any(|name| name == agent) {
        return i18n::text(lang, Msg::DetailConfigBuiltin).to_string();
    }
    let Some(entry) = found else {
        return i18n::text(lang, Msg::DetailConfigMissing).to_string();
    };
    let or_dash = |text: &str| {
        if text.is_empty() {
            "-".to_string()
        } else {
            text.to_string()
        }
    };
    i18n::agent_config_value(
        lang,
        &or_dash(&entry.model),
        &or_dash(&entry.effort),
        &entry
            .max_turns
            .map_or("-".to_string(), |turns| turns.to_string()),
    )
}

// 4-2. 최근 실행 한 줄 --------------------------------------------------------
// 이름 칸 = 실행 id 앞부분 (서브에이전트는 # 뒤, 메인은 세션 id), 값 = 마지막 모델·요청·비용·시작-끝 시각
fn run_line(lang: Lang, run: &AgentRunSummary) -> Line<'static> {
    let id = match run.run.rsplit_once('#') {
        Some((_, suffix)) => format!(
            "#{}",
            suffix.chars().take(RUN_ID_PREFIX).collect::<String>()
        ),
        None => run.run.chars().take(RUN_ID_PREFIX).collect(),
    };
    let minute = |ts: &str| format_clock_time(ts).chars().take(5).collect::<String>();
    let span = format!("{}-{}", minute(&run.first_ts), minute(&run.last_ts));
    let model = if run.model.is_empty() {
        "-"
    } else {
        short_model(&run.model)
    };
    Line::from(vec![
        Span::styled(
            fit_display(&format!("  {id}"), DETAIL_LABEL_WIDTH),
            theme::STYLE_TEXT,
        ),
        Span::styled(
            i18n::run_value(
                lang,
                model,
                &format_count(i64::try_from(run.requests).unwrap_or(i64::MAX)),
                run.cost_usd_estimate,
                &span,
            ),
            theme::STYLE_TEXT,
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::layout::SplitGroup;
    use crate::tui::layout::grid::{TabGrid, default_rows};
    use crate::tui::test_support::sample_state_with_tables;
    use crate::tui::views::render_frame;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    const KO: Lang = Lang::Ko;
    const SIZES: [(u16, u16); 3] = [(60, 16), (100, 30), (130, 40)];
    const BOTH: [Lang; 2] = [Lang::En, Lang::Ko];
    // 모든 열이 보이는 넓은 화면
    const WIDE: (u16, u16) = (200, 40);
    const MODEL_CARDS: [CardId; 4] = [
        CardId::ModelTable,
        CardId::ModelDetail,
        CardId::CacheBreaks,
        CardId::CacheTimer,
    ];
    const AGENT_CARDS: [CardId; 2] = [CardId::AgentTable, CardId::AgentDetail];

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn tab_state(lang: Lang, tab: ActiveTab) -> AppState {
        let mut state = sample_state_with_tables();
        state.lang = lang;
        state.tab = tab;
        state
    }

    fn draw(state: &mut AppState, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_frame(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    // 화면 전체 줄 (넓은 글자 뒤 칸은 건너뜀)
    fn screen_rows(buffer: &Buffer) -> Vec<String> {
        let mut rows = Vec::new();
        for y in 0..buffer.area.height {
            let mut text = String::new();
            let mut skip = 0usize;
            for x in 0..buffer.area.width {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let symbol = buffer.cell((x, y)).map_or(" ", |cell| cell.symbol());
                skip = Span::raw(symbol).width().saturating_sub(1);
                text.push_str(symbol);
            }
            rows.push(text);
        }
        rows
    }

    // 그려진 머리글 열 번호 (왼쪽부터)
    fn header_columns(state: &AppState, card: CardId) -> Vec<usize> {
        state
            .hit_areas
            .header_cells
            .iter()
            .filter(|hit| hit.card == card)
            .map(|hit| hit.column)
            .collect()
    }

    // 이번 프레임에 그려진 카드 영역 (없으면 None)
    fn drawn_rect(state: &AppState, card: CardId) -> Option<Rect> {
        state
            .hit_areas
            .card_rects
            .iter()
            .find(|(id, _)| *id == card)
            .map(|(_, rect)| *rect)
    }

    fn grid_from(rows: &[&[CardId]]) -> TabGrid {
        let mut grid = TabGrid::default();
        for (row, cards) in rows.iter().enumerate() {
            for (column, card) in cards.iter().enumerate() {
                grid.rows[row][column] = Some(*card);
            }
        }
        grid
    }

    // 위에서 아래로 쌓인 카드가 본문(앱 바·탭 바 아래부터 아래 두 줄 앞까지)을 빈틈·격침 없이 채움, 반환 = 카드 높이
    fn stacked_heights(state: &AppState, cards: &[CardId], width: u16, height: u16) -> Vec<u16> {
        let mut top = 2;
        let mut heights = Vec::new();
        for card in cards {
            let rect = drawn_rect(state, *card).unwrap_or_else(|| panic!("{card:?} 없음"));
            assert_eq!((rect.x, rect.y, rect.width), (0, top, width), "{card:?}");
            top += rect.height;
            heights.push(rect.height);
        }
        assert_eq!(top, height - 2, "{cards:?}");
        heights
    }

    #[test]
    fn model_detail_reports_cache_hit_cost_and_waits() {
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
            wait_avg_ms: Some(1_200),
            wait_max_ms: None,
            wait_total_ms: 1_080_000,
            cost_usd_estimate: 12.345,
            last_ts: "2026-11-15T01:00:00Z".to_string(),
            ..ModelStatsRow::default()
        };
        let window = WindowTotals {
            requests: 1_800,
            cost: 24.69,
        };
        let (title, lines) = build_model_detail(KO, Some(&row), window);
        assert_eq!(title, "모델 상세 · claude-sonnet-5-5");
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text.iter().any(|line| line.contains("900 / 3 / 2")));
        assert!(text.iter().any(|line| line.contains("적중 84.2%")));
        assert!(text.iter().any(|line| line.contains("1.2s / -")));
        // 대기 합계, 토큰 합계 / 요청당 (10.3M / 900)
        assert!(
            text.iter()
                .any(|line| line.starts_with("대기 합계") && line.ends_with("18m 00s")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|line| line.starts_with("토큰 합계 / 요청당")
                    && line.ends_with("10.3M / 11.4K")),
            "{text:?}"
        );
        assert!(text.iter().any(|line| line.contains("$12.35")));
        // 요청당 비용·출력, 구간 점유 (요청·비용 모두 절반)
        assert!(
            text.iter().any(|line| line.contains("$0.0137 / 333")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|line| line.contains("요청 50.0% · 비용 50.0%")),
            "{text:?}"
        );
        // 에이전트 분포가 없으면 구역도 없음, 있으면 제목과 줄
        assert_eq!(lines.len(), 11);
        let with_agents = ModelStatsRow {
            agents: vec![crate::tui::test_support::usage_share(
                "main",
                900,
                0,
                12.345,
                "2026-11-15T01:00:00Z",
            )],
            ..row.clone()
        };
        let (_, lines) = build_model_detail(KO, Some(&with_agents), window);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(text[11], "에이전트별 비용");
        assert!(text[12].ends_with("요청 900 · $12.35 · 100%"), "{text:?}");

        let (empty_title, empty_lines) = build_model_detail(KO, None, window);
        assert_eq!(empty_title, "모델 상세");
        assert_eq!(empty_lines.len(), 1);

        // 요청·대기 기록이 없는 행은 대기 "- / -", 합계 "-", 요청당 "-"
        let (_, idle_lines) = build_model_detail(KO, Some(&ModelStatsRow::default()), window);
        let idle: Vec<String> = idle_lines.iter().map(line_text).collect();
        assert!(
            idle.iter()
                .any(|line| line.starts_with("대기 평균 / 최대") && line.ends_with("- / -"))
        );
        assert!(
            idle.iter()
                .any(|line| line.starts_with("대기 합계") && line.ends_with(" -"))
        );
        assert!(
            idle.iter()
                .any(|line| line.starts_with("토큰 합계 / 요청당") && line.ends_with("0 / -"))
        );
    }

    #[test]
    fn agent_detail_reports_tool_error_rate_top_model_config_and_recent_runs() {
        let row = AgentStatsRow {
            agent: "worker".to_string(),
            runs: 3,
            requests: 40,
            sessions: 2,
            tool_calls: 50,
            tool_errors: 5,
            input_tokens: 60_000,
            output_tokens: 20_000,
            wait_avg_ms: Some(800),
            wait_max_ms: Some(2_500),
            wait_total_ms: 32_000,
            top_model: "claude-sonnet-5-5".to_string(),
            cost_usd_estimate: 1.5,
            recent_runs: vec![AgentRunSummary {
                run: "worker#a1b2c3d4e5f6".to_string(),
                model: "claude-haiku-4-5".to_string(),
                requests: 12,
                cost_usd_estimate: 0.25,
                first_ts: "2026-11-15T01:20:00Z".to_string(),
                last_ts: "2026-11-15T01:30:00Z".to_string(),
                ..AgentRunSummary::default()
            }],
            ..AgentStatsRow::default()
        };
        let state = crate::tui::test_support::sample_state_with_tables();
        let window = WindowTotals {
            requests: 80,
            cost: 3.0,
        };
        let (title, lines) = build_agent_detail(KO, Some(&row), window, &state.inventory);
        assert_eq!(title, "에이전트 상세 · worker");
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text.iter().any(|line| line.contains("3 / 40 / 2")));
        assert!(text.iter().any(|line| line.contains("오류율 10.0%")));
        // 대기 평균 / 최대, 대기 합계, 토큰 합계 / 요청당 (80K / 40)
        assert!(
            text.iter()
                .any(|line| line.starts_with("대기 평균 / 최대") && line.ends_with("800ms / 2.5s")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|line| line.starts_with("대기 합계") && line.ends_with("32.0s")),
            "{text:?}"
        );
        assert!(
            text.iter().any(
                |line| line.starts_with("토큰 합계 / 요청당") && line.ends_with("80.0K / 2.0K")
            ),
            "{text:?}"
        );
        assert!(text.iter().any(|line| line.contains("claude-sonnet-5-5")));
        // 실행당 요청·비용, 구간 점유, agents 폴더 정의의 설정값
        assert!(
            text.iter().any(|line| line.contains("13.3 / $0.500")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|line| line.contains("요청 50.0% · 비용 50.0%"))
        );
        assert!(
            text.iter()
                .any(|line| line.contains("모델 sonnet · effort high · 최대턴 40")),
            "{text:?}"
        );
        // 최근 실행: # 뒤 여덟 글자와 마지막 모델·요청·비용
        assert!(text.iter().any(|line| line == "최근 실행"), "{text:?}");
        let run = text
            .iter()
            .find(|line| line.starts_with("  #a1b2c3d4 "))
            .expect("실행 줄");
        assert!(run.contains("haiku-4-5 · 요청 12 · $0.25"), "{run}");

        // 메인은 정의 파일 없음, 목록에 없으면 없음, 읽기 전이면 읽는 중
        assert_eq!(
            agent_config(KO, "main", &state.inventory),
            "메인 세션 (정의 파일 없음)"
        );
        assert!(agent_config(KO, "ghost", &state.inventory).contains("정의 없음"));
        assert!(agent_config(Lang::En, "worker", &Panel::Pending).contains("reading"));

        let (empty_title, _) = build_agent_detail(KO, None, window, &state.inventory);
        assert_eq!(empty_title, "에이전트 상세");
    }

    #[test]
    fn detail_titles_drop_the_separator_when_the_name_is_empty() {
        let window = WindowTotals::default();
        let nameless_model = ModelStatsRow::default();
        let (title, _) = build_model_detail(KO, Some(&nameless_model), window);
        assert_eq!(title, "모델 상세");

        let nameless_agent = AgentStatsRow::default();
        let (title, _) = build_agent_detail(KO, Some(&nameless_agent), window, &Panel::Pending);
        assert_eq!(title, "에이전트 상세");
    }

    #[test]
    fn agent_rows_append_configured_agents_without_calls_as_zero_rows() {
        let mut state = crate::tui::test_support::sample_state_with_tables();
        if let Panel::Ready(inventory) = &mut state.inventory {
            inventory.builtin_agents = vec!["general-purpose".to_string(), "worker".to_string()];
        }
        let rows = agent_rows(&state.snapshot.agent_stats, &state.inventory).expect("행");
        let names: Vec<&str> = rows.iter().map(|row| row.agent.as_str()).collect();
        // 호출 기록 행(main, worker) 뒤에 호출 없는 설정 에이전트 tester, 내장 general-purpose 한 번씩
        assert_eq!(names, ["main", "worker", "tester", "general-purpose"]);
        for zero in &rows[2..] {
            assert_eq!((zero.runs, zero.requests, zero.tool_calls), (0, 0, 0));
            assert_eq!(zero.cost_usd_estimate, 0.0);
        }
        assert_eq!(
            agent_config(KO, "general-purpose", &state.inventory),
            "내장 에이전트 (정의 파일 없음)"
        );
        // 같은 이름의 정의 파일이 있으면 파일 설정값이 우선
        assert!(agent_config(KO, "worker", &state.inventory).contains("sonnet"));

        // 설정 목록을 읽기 전이면 호출 기록 행만, 집계 전이면 None
        let only_stats = agent_rows(&state.snapshot.agent_stats, &Panel::Pending).expect("행");
        assert_eq!(only_stats.len(), 2);
        assert!(agent_rows(&Panel::Pending, &state.inventory).is_none());
    }

    #[test]
    fn models_and_agents_tabs_show_wait_and_token_columns_without_percentiles() {
        let tabs = [
            (
                ActiveTab::Models,
                CardId::ModelTable,
                MODEL_COLUMNS.to_vec(),
                vec![Msg::ColAvgWait, Msg::ColTotalWait, Msg::ColTokPerReq],
            ),
            (
                ActiveTab::Agents,
                CardId::AgentTable,
                AGENT_COLUMNS.to_vec(),
                vec![Msg::ColAvgWait, Msg::ColTotalWait],
            ),
        ];
        for (tab, card, columns, new_headers) in tabs {
            for lang in BOTH {
                // 1. 크기 행렬: p50·p90 없음, 오른쪽 열부터 숨고 이름 열은 남음
                for (width, height) in SIZES.into_iter().chain([WIDE]) {
                    let case = format!("{tab:?} {lang:?} {width}x{height}");
                    let mut state = tab_state(lang, tab);
                    let buffer = draw(&mut state, width, height);
                    let text = screen_rows(&buffer).join("\n");
                    let lower = text.to_lowercase();
                    assert!(
                        !lower.contains("p50") && !lower.contains("p90"),
                        "{case}\n{text}"
                    );
                    let shown = header_columns(&state, card);
                    let prefix: Vec<usize> = (0..shown.len()).collect();
                    assert_eq!(shown, prefix, "{case}");
                    assert!(!shown.is_empty(), "{case}");
                    if (width, height) == (60, 16) {
                        assert!(shown.len() < columns.len(), "{case} {shown:?}");
                    }
                    if (width, height) == WIDE {
                        assert_eq!(shown.len(), columns.len(), "{case}");
                        for header in &new_headers {
                            let label = i18n::text(lang, *header);
                            assert!(text.contains(label), "{case} {label}\n{text}");
                        }
                    }
                }

                // 2. 130x40: 상세 카드에 대기 평균/최대·합계·토큰 요청당 줄
                let mut state = tab_state(lang, tab);
                let text = screen_rows(&draw(&mut state, 130, 40)).join("\n");
                for label in [Msg::WaitAvgMax, Msg::WaitTotal, Msg::DetailTokensPerReq] {
                    let label = i18n::text(lang, label);
                    assert!(text.contains(label), "{tab:?} {lang:?} {label}\n{text}");
                }
            }
        }
    }

    #[test]
    fn model_and_agent_rows_format_average_total_wait_and_tokens_per_request() {
        // 모델: sonnet = 1.2s · 18m 00s · 11.4K (10.3M / 900), haiku = 300ms · 7m 30s · 913
        let mut state = tab_state(Lang::En, ActiveTab::Models);
        let rows = screen_rows(&draw(&mut state, WIDE.0, WIDE.1));
        let find_row = |rows: &[String], key: &str| {
            rows.iter()
                .find(|row| row.contains(key))
                .cloned()
                .unwrap_or_else(|| panic!("{key} 행 없음\n{}", rows.join("\n")))
        };
        let sonnet = find_row(&rows, "18m 00s");
        assert!(sonnet.contains("sonnet-5-5"), "{sonnet}");
        assert!(
            sonnet.contains(" 1.2s ") && sonnet.contains(" 11.4K"),
            "{sonnet}"
        );
        let haiku = find_row(&rows, "7m 30s");
        assert!(haiku.contains("haiku-4-5"), "{haiku}");
        assert!(
            haiku.contains(" 300ms ") && haiku.contains(" 913"),
            "{haiku}"
        );

        // 에이전트: main = 1.1s · 12m 50s, worker = 400ms · 11m 20s, 호출 없는 행은 "-"
        let mut state = tab_state(Lang::En, ActiveTab::Agents);
        let rows = screen_rows(&draw(&mut state, WIDE.0, WIDE.1));
        let main = find_row(&rows, "12m 50s");
        assert!(main.contains("main") && main.contains(" 1.1s "), "{main}");
        let worker = find_row(&rows, "11m 20s");
        assert!(
            worker.contains("worker") && worker.contains(" 400ms "),
            "{worker}"
        );
    }

    // 기본 격자: 카드 한 장이 한 줄, 높이는 지금의 공식 (모델 표는 10행 상한, 캐시 타이머는 본문 22행 이상일 때만)
    #[test]
    fn models_and_agents_cards_tile_the_body_with_the_default_heights_at_the_size_matrix() {
        let models: [(u16, u16, &[u16]); 3] = [
            (60, 16, &[4, 4, 4]),
            (100, 30, &[8, 9, 4, 5]),
            (130, 40, &[10, 16, 5, 5]),
        ];
        let agents: [(u16, u16, &[u16]); 3] = [
            (60, 16, &[5, 7]),
            (100, 30, &[12, 14]),
            (130, 40, &[17, 19]),
        ];
        for lang in BOTH {
            for (width, height, heights) in models {
                let case = format!("Models {lang:?} {width}x{height}");
                let mut state = tab_state(lang, ActiveTab::Models);
                draw(&mut state, width, height);
                let cards = &MODEL_CARDS[..heights.len()];
                assert_eq!(
                    stacked_heights(&state, cards, width, height),
                    heights,
                    "{case}"
                );
                assert_eq!(
                    drawn_rect(&state, CardId::CacheTimer).is_some(),
                    heights.len() == 4,
                    "{case}"
                );
                // 줄 경계 드래그 영역은 보이는 줄 수 그룹에 경계마다 하나
                let group = SplitGroup::GridRows(ActiveTab::Models, heights.len() as u8);
                let row_hits = state
                    .hit_areas
                    .split_hits
                    .iter()
                    .filter(|hit| hit.group == group)
                    .count();
                assert_eq!(row_hits, heights.len() - 1, "{case}");
            }
            for (width, height, heights) in agents {
                let case = format!("Agents {lang:?} {width}x{height}");
                let mut state = tab_state(lang, ActiveTab::Agents);
                draw(&mut state, width, height);
                assert_eq!(
                    stacked_heights(&state, &AGENT_CARDS, width, height),
                    heights,
                    "{case}"
                );
            }
        }
    }

    // 낮은 본문(100x24)에서는 캐시 타이머가 어느 줄에 놓였든 없고 줄 동료가 폭을 받음, 100x30 에서는 보임
    #[test]
    fn the_cache_timer_is_absent_on_a_short_body_wherever_it_sits_in_the_grid() {
        let grids = [
            default_rows(ActiveTab::Models),
            grid_from(&[
                &[CardId::CacheTimer],
                &[CardId::ModelTable],
                &[CardId::ModelDetail],
                &[CardId::CacheBreaks],
            ]),
            grid_from(&[
                &[CardId::ModelTable, CardId::CacheTimer],
                &[CardId::ModelDetail],
                &[CardId::CacheBreaks],
            ]),
        ];
        for (index, grid) in grids.into_iter().enumerate() {
            let case = format!("grid {index}");
            let mut state = tab_state(Lang::En, ActiveTab::Models);
            state.card_layout.set_grid(ActiveTab::Models, grid);
            draw(&mut state, 100, 24);
            assert_eq!(drawn_rect(&state, CardId::CacheTimer), None, "{case}");
            for card in &MODEL_CARDS[..3] {
                let rect = drawn_rect(&state, *card).unwrap_or_else(|| panic!("{case} {card:?}"));
                assert_eq!(rect.width, 100, "{case} {card:?}");
            }

            draw(&mut state, 100, 30);
            let timer = drawn_rect(&state, CardId::CacheTimer).expect("캐시 타이머");
            assert!(timer.height >= CACHE_CARD_HEIGHT, "{case} {timer:?}");
            if index == 2 {
                let table = drawn_rect(&state, CardId::ModelTable).expect("모델 표");
                assert_eq!(table.y, timer.y, "{case}");
                assert!(table.right() < timer.x, "{case}");
            }
        }
    }

    // 격자 한 줄에 두 장을 두면 같은 y, 모델 탭 네 장 한 줄은 60열에서 두 화면 줄로 넘어가고 모두 보임
    #[test]
    fn two_cards_in_one_grid_row_share_a_line_and_four_model_cards_wrap_at_60_columns() {
        let pairs = [
            (
                ActiveTab::Models,
                grid_from(&[
                    &[CardId::ModelTable, CardId::ModelDetail],
                    &[CardId::CacheBreaks],
                    &[CardId::CacheTimer],
                ]),
                [CardId::ModelTable, CardId::ModelDetail],
            ),
            (
                ActiveTab::Models,
                grid_from(&[
                    &[CardId::ModelTable],
                    &[CardId::ModelDetail],
                    &[CardId::CacheBreaks, CardId::CacheTimer],
                ]),
                [CardId::CacheBreaks, CardId::CacheTimer],
            ),
            (ActiveTab::Agents, grid_from(&[&AGENT_CARDS]), AGENT_CARDS),
        ];
        for (tab, grid, [left, right]) in pairs {
            let case = format!("{tab:?} {left:?} {right:?}");
            let mut state = tab_state(Lang::En, tab);
            state.card_layout.set_grid(tab, grid);
            draw(&mut state, 130, 40);
            let left_rect = drawn_rect(&state, left).expect("왼쪽 카드");
            let right_rect = drawn_rect(&state, right).expect("오른쪽 카드");
            assert_eq!(left_rect.y, right_rect.y, "{case}");
            assert!(left_rect.right() < right_rect.x, "{case}");
        }

        let mut state = tab_state(Lang::En, ActiveTab::Models);
        state
            .card_layout
            .set_grid(ActiveTab::Models, grid_from(&[&MODEL_CARDS]));
        draw(&mut state, 60, 30);
        let rects: Vec<Rect> = MODEL_CARDS
            .iter()
            .map(|card| drawn_rect(&state, *card).unwrap_or_else(|| panic!("{card:?}")))
            .collect();
        assert_eq!(rects[0].y, rects[1].y);
        assert_eq!(rects[2].y, rects[3].y);
        assert!(rects[0].bottom() <= rects[2].y);
        for (index, rect) in rects.iter().enumerate() {
            assert!(
                rect.right() <= 60 && rect.bottom() <= 28,
                "{index} {rect:?}"
            );
            for other in &rects[index + 1..] {
                assert!(!rect.intersects(*other), "{rect:?} {other:?}");
            }
        }
    }
}
