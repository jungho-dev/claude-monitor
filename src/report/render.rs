//! report/render.rs
//! 집계 결과를 한국어 Markdown 리포트로 변환하고 규칙 기반 제안 생성.

use super::table::{ColumnAlign, render_box_table};
use crate::core::model::DENY_ROUND_TRIP_MS;
use crate::store::query::{
    CacheBreakRow, GatewayTimingStats, HookStatRow, ModelUsageRow, OverviewStats, SourceCounts,
    ToolLatencyRow,
};
use chrono::{DateTime, Local, NaiveDate};
use std::collections::BTreeMap;
use std::fmt::Write;

const SLOW_TOOL_P90_MS: i64 = 5000;
const SLOW_TOOL_MIN_CALLS: u64 = 3;
const DENY_RULE_MIN_COUNT: u64 = 3;
const LOW_CACHE_HIT_RATIO: f64 = 0.8;
const LOW_CACHE_MIN_REQUESTS: u64 = 50;
const HOOK_TABLE_ROWS: usize = 15;

// 1. 리포트 입력 --------------------------------------------------------------
#[derive(Debug, Clone, Default)]
pub struct ReportInput {
    pub report_date: NaiveDate,
    pub today: OverviewStats,
    pub previous: OverviewStats,
    pub usage: Vec<ModelUsageRow>,
    pub tools: Vec<ToolLatencyRow>,
    pub hooks: Vec<HookStatRow>,
    pub breaks: Vec<CacheBreakRow>,
    pub gateway: GatewayTimingStats,
    pub sources: SourceCounts,
}

// 2. 시간 표기 ----------------------------------------------------------------
// 1초 미만 ms, 1분 미만 초, 이후 분·초
pub fn format_duration_ms(duration_ms: Option<i64>) -> String {
    let Some(elapsed) = duration_ms else {
        return "-".to_string();
    };
    if elapsed.abs() < 1000 {
        return format!("{elapsed}ms");
    }
    if elapsed.abs() < 60_000 {
        return format!("{:.1}초", elapsed as f64 / 1000.0);
    }
    let sign = if elapsed < 0 { "-" } else { "" };
    let total_secs = elapsed.abs() / 1000;
    format!("{sign}{}분 {:02}초", total_secs / 60, total_secs % 60)
}

// 3. 토큰 표기 ----------------------------------------------------------------
pub fn format_tokens(tokens: i64) -> String {
    if tokens.abs() >= 1_000_000 {
        return format!("{:.2}M", tokens as f64 / 1_000_000.0);
    }
    if tokens.abs() >= 1_000 {
        return format!("{:.1}K", tokens as f64 / 1_000.0);
    }
    tokens.to_string()
}

// 4. 건수 표기 ----------------------------------------------------------------
// 천 단위 구분 정수 (TUI 와 공용)
pub fn format_count(amount: i64) -> String {
    let digits = amount.unsigned_abs().to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if amount < 0 {
        grouped.push('-');
    }
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

// 5. 증감 표기 ----------------------------------------------------------------
// 건수는 부호 차이와 비율, 이전 0 이면 차이만
pub fn format_count_delta(current: i64, previous: i64) -> String {
    let diff = current - previous;
    let signed_diff = format!("{}{}", if diff >= 0 { "+" } else { "" }, format_count(diff));
    if previous == 0 {
        return signed_diff;
    }
    format!(
        "{signed_diff} ({:+.1}%)",
        diff as f64 * 100.0 / previous as f64
    )
}

// 6. 시각 표기 ----------------------------------------------------------------
// RFC 3339 UTC 를 로컬 시:분:초 로 변환, 실패 시 원문 앞부분
pub fn format_local_time(timestamp: &str) -> String {
    match DateTime::parse_from_rfc3339(timestamp) {
        Ok(parsed) => parsed.with_timezone(&Local).format("%H:%M:%S").to_string(),
        Err(_) => timestamp.chars().take(19).collect(),
    }
}

// 7. 규칙 기반 제안 -----------------------------------------------------------
pub fn build_suggestions(input: &ReportInput) -> Vec<String> {
    let mut suggestions: Vec<String> = Vec::new();

    // 1. 반복 거부 명령
    let mut deny_rows: Vec<&HookStatRow> = input
        .hooks
        .iter()
        .filter(|row| row.outcome == "deny" && row.count >= DENY_RULE_MIN_COUNT)
        .collect();
    deny_rows.sort_by_key(|row| std::cmp::Reverse(row.count));
    for row in deny_rows.into_iter().take(5) {
        let lost = format_duration_ms(Some(row.count as i64 * DENY_ROUND_TRIP_MS));
        if row.detail.is_empty() {
            suggestions.push(format!(
                "가드 `{}` 거부 {}회 (명령 이름 미감지, 손실 약 {lost}): 거부 사유별 재작성 규칙 후보 확인",
                row.guard, row.count
            ));
        } else {
            suggestions.push(format!(
                "`{}` 명령이 `{}` 에 {}회 거부됨 (손실 약 {lost}): 재작성 규칙 후보",
                row.detail, row.guard, row.count
            ));
        }
    }

    // 2. 캐시 붕괴 원인 분포
    if !input.breaks.is_empty() {
        let mut by_cause: BTreeMap<&str, u64> = BTreeMap::new();
        let mut created_total: i64 = 0;
        for row in &input.breaks {
            *by_cause.entry(row.likely_cause.as_str()).or_default() += 1;
            created_total += row.cache_creation_tokens;
        }
        let causes: Vec<String> = by_cause
            .iter()
            .map(|(cause, count)| format!("{cause} {count}건"))
            .collect();
        suggestions.push(format!(
            "캐시 붕괴 상위 {}건 (재생성 {} 토큰): {}. 원인 미상은 시스템 프롬프트·도구 정의·훅 주입 변동 점검",
            input.breaks.len(),
            format_tokens(created_total),
            causes.join(", ")
        ));
    }

    // 3. 낮은 캐시 적중
    if input.today.api_requests >= LOW_CACHE_MIN_REQUESTS
        && input.today.cache_hit_ratio < LOW_CACHE_HIT_RATIO
    {
        suggestions.push(format!(
            "캐시 적중률 {:.1}% 로 낮음: 세션 중간 컨텍스트 변경·긴 유휴 여부 확인",
            input.today.cache_hit_ratio * 100.0
        ));
    }

    // 4. 느린 도구
    for row in &input.tools {
        let slow = row.p90_ms.is_some_and(|p90| p90 > SLOW_TOOL_P90_MS);
        if slow && row.calls >= SLOW_TOOL_MIN_CALLS && row.tool_name != "AskUserQuestion" {
            suggestions.push(format!(
                "도구 `{}` p90 {} ({}회): 병렬화·타임아웃·대체 경로 검토",
                row.tool_name,
                format_duration_ms(row.p90_ms),
                row.calls
            ));
        }
    }

    // 5. 텔레메트리 미설정
    if input.sources.otlp == 0 {
        suggestions.push(
            "OTLP 수집 0건: Claude Code 에 CLAUDE_CODE_ENABLE_TELEMETRY=1, OTEL_EXPORTER_OTLP_PROTOCOL=http/json, OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:47331 설정 시 요청 단위 지표 보강"
                .to_string(),
        );
    }

    // 6. 기록 없음
    if input.today.api_requests == 0 {
        suggestions.push(
            "해당 일자 기록 0건: serve 색인 동작 또는 `rust-agents-proxy index` 실행 여부 확인"
                .to_string(),
        );
    }
    suggestions
}

// 8. 리포트 본문 --------------------------------------------------------------
pub fn render_report_markdown(input: &ReportInput) -> String {
    let today = &input.today;
    let previous = &input.previous;
    let mut text = String::new();

    // 1. 요약
    let _ = writeln!(
        text,
        "# 일일 에이전트 리포트 {}",
        input.report_date.format("%Y-%m-%d")
    );
    let _ = writeln!(
        text,
        "\n기준 시간대: 로컬 (UTC 변환 구간 {} ~ {})\n",
        today.since, today.until
    );
    let _ = writeln!(text, "## 1. 요약\n");
    let _ = writeln!(
        text,
        "- API 요청: {}건 (세션 {}개)",
        format_count(today.api_requests as i64),
        format_count(today.sessions as i64)
    );
    let _ = writeln!(
        text,
        "- 토큰: 입력 {} / 출력 {} / 캐시 읽기 {} / 캐시 생성 {}",
        format_tokens(today.input_tokens),
        format_tokens(today.output_tokens),
        format_tokens(today.cache_read_tokens),
        format_tokens(today.cache_creation_tokens)
    );
    let _ = writeln!(text, "- 캐시 적중률: {:.1}%", today.cache_hit_ratio * 100.0);
    let _ = writeln!(
        text,
        "- 모델 대기: p50 {} / p90 {}",
        format_duration_ms(today.model_wait_p50_ms),
        format_duration_ms(today.model_wait_p90_ms)
    );
    let _ = writeln!(
        text,
        "- 도구 호출: {}건 (오류 {}건)",
        format_count(today.tool_calls as i64),
        format_count(today.tool_errors as i64)
    );
    let _ = writeln!(
        text,
        "- 훅: 거부 {}건 / 재작성 {}건",
        format_count(today.hook_denials as i64),
        format_count(today.hook_rewrites as i64)
    );
    let _ = writeln!(
        text,
        "- 절감 시간: {} (재작성으로 피한 거부 왕복)",
        format_duration_ms(Some(today.saved_ms))
    );
    let _ = writeln!(
        text,
        "- 손실 시간: {} (추정, 거부 {}건 x {})",
        format_duration_ms(Some(today.lost_ms_estimate)),
        format_count(today.hook_denials as i64),
        format_duration_ms(Some(DENY_ROUND_TRIP_MS))
    );

    // 2. 전일 대비
    let _ = writeln!(text, "\n## 2. 전일 대비\n");
    if previous.api_requests == 0 && previous.tool_calls == 0 {
        let _ = writeln!(text, "- 전일 기록 없음");
    } else {
        let mut compare_rows: Vec<Vec<String>> = Vec::new();
        let count_rows = [
            (
                "API 요청",
                today.api_requests as i64,
                previous.api_requests as i64,
            ),
            ("출력 토큰", today.output_tokens, previous.output_tokens),
            (
                "도구 호출",
                today.tool_calls as i64,
                previous.tool_calls as i64,
            ),
            (
                "도구 오류",
                today.tool_errors as i64,
                previous.tool_errors as i64,
            ),
            (
                "훅 거부",
                today.hook_denials as i64,
                previous.hook_denials as i64,
            ),
            (
                "훅 재작성",
                today.hook_rewrites as i64,
                previous.hook_rewrites as i64,
            ),
        ];
        for (label, current, before) in count_rows {
            compare_rows.push(vec![
                label.to_string(),
                format_count(current),
                format_count(before),
                format_count_delta(current, before),
            ]);
        }
        compare_rows.push(vec![
            "캐시 적중률".to_string(),
            format!("{:.1}%", today.cache_hit_ratio * 100.0),
            format!("{:.1}%", previous.cache_hit_ratio * 100.0),
            format!(
                "{:+.1}%p",
                (today.cache_hit_ratio - previous.cache_hit_ratio) * 100.0
            ),
        ]);
        let wait_rows = [
            (
                "모델 대기 p50",
                today.model_wait_p50_ms,
                previous.model_wait_p50_ms,
            ),
            (
                "모델 대기 p90",
                today.model_wait_p90_ms,
                previous.model_wait_p90_ms,
            ),
        ];
        for (label, current, before) in wait_rows {
            let delta = match (current, before) {
                (Some(now_ms), Some(before_ms)) if now_ms > before_ms => {
                    format!("+{}", format_duration_ms(Some(now_ms - before_ms)))
                }
                (Some(now_ms), Some(before_ms)) => format_duration_ms(Some(now_ms - before_ms)),
                _ => "-".to_string(),
            };
            compare_rows.push(vec![
                label.to_string(),
                format_duration_ms(current),
                format_duration_ms(before),
                delta,
            ]);
        }
        let compare_table = render_box_table(
            &["항목", "당일", "전일", "증감"],
            &[
                ColumnAlign::Left,
                ColumnAlign::Right,
                ColumnAlign::Right,
                ColumnAlign::Right,
            ],
            &compare_rows,
        );
        let _ = writeln!(text, "```text\n{}\n```", compare_table.join("\n"));
    }

    // 3. 모델·에이전트별
    let _ = writeln!(text, "\n## 3. 모델·에이전트별 (비용은 추정)\n");
    if input.usage.is_empty() {
        let _ = writeln!(text, "- 기록 없음");
    } else {
        let mut usage_rows: Vec<Vec<String>> = Vec::new();
        let mut cost_total = 0.0;
        for row in &input.usage {
            cost_total += row.cost_usd_estimate;
            usage_rows.push(vec![
                row.model.clone(),
                row.agent.clone(),
                format_count(row.requests as i64),
                format_tokens(row.input_tokens),
                format_tokens(row.output_tokens),
                format_tokens(row.cache_read_tokens),
                format_tokens(row.cache_creation_tokens),
                format!("{:.2}", row.cost_usd_estimate),
            ]);
        }
        let usage_table = render_box_table(
            &[
                "모델",
                "에이전트",
                "요청",
                "입력",
                "출력",
                "캐시 읽기",
                "캐시 생성",
                "비용 USD (추정)",
            ],
            &[
                ColumnAlign::Left,
                ColumnAlign::Left,
                ColumnAlign::Right,
                ColumnAlign::Right,
                ColumnAlign::Right,
                ColumnAlign::Right,
                ColumnAlign::Right,
                ColumnAlign::Right,
            ],
            &usage_rows,
        );
        let _ = writeln!(text, "```text\n{}\n```", usage_table.join("\n"));
        let _ = writeln!(
            text,
            "\n- 합계 추정 비용: {cost_total:.2} USD (API 단가 기준 추정, 구독 요금과 무관)"
        );
    }

    // 4. 도구 지연 상위 10
    let _ = writeln!(text, "\n## 4. 도구 지연 상위 10\n");
    if input.tools.is_empty() {
        let _ = writeln!(text, "- 기록 없음");
    } else {
        let tool_rows: Vec<Vec<String>> = input
            .tools
            .iter()
            .take(10)
            .map(|row| {
                vec![
                    row.tool_name.clone(),
                    format_count(row.calls as i64),
                    format_count(row.errors as i64),
                    format_duration_ms(row.p50_ms),
                    format_duration_ms(row.p90_ms),
                    format_duration_ms(Some(row.total_ms)),
                ]
            })
            .collect();
        let tool_table = render_box_table(
            &["도구", "호출", "오류", "p50", "p90", "합계"],
            &[
                ColumnAlign::Left,
                ColumnAlign::Right,
                ColumnAlign::Right,
                ColumnAlign::Right,
                ColumnAlign::Right,
                ColumnAlign::Right,
            ],
            &tool_rows,
        );
        let _ = writeln!(text, "```text\n{}\n```", tool_table.join("\n"));
    }

    // 5. 훅 판정
    let _ = writeln!(text, "\n## 5. 훅 판정\n");
    if input.hooks.is_empty() {
        let _ = writeln!(text, "- 기록 없음");
    } else {
        let hook_rows: Vec<Vec<String>> = input
            .hooks
            .iter()
            .take(HOOK_TABLE_ROWS)
            .map(|row| {
                vec![
                    row.outcome.clone(),
                    row.route.clone(),
                    row.guard.clone(),
                    row.detail.clone(),
                    format_count(row.count as i64),
                    format_duration_ms(row.p90_ms),
                    format_duration_ms(Some(row.saved_ms)),
                ]
            })
            .collect();
        let hook_table = render_box_table(
            &["판정", "경로", "가드", "상세", "건수", "p90", "절감"],
            &[
                ColumnAlign::Left,
                ColumnAlign::Left,
                ColumnAlign::Left,
                ColumnAlign::Left,
                ColumnAlign::Right,
                ColumnAlign::Right,
                ColumnAlign::Right,
            ],
            &hook_rows,
        );
        let _ = writeln!(text, "```text\n{}\n```", hook_table.join("\n"));
        if input.hooks.len() > HOOK_TABLE_ROWS {
            let _ = writeln!(
                text,
                "\n- 나머지 {}행 생략",
                input.hooks.len() - HOOK_TABLE_ROWS
            );
        }
    }

    // 6. 캐시 붕괴 상위
    let _ = writeln!(text, "\n## 6. 캐시 붕괴 상위\n");
    if input.breaks.is_empty() {
        let _ = writeln!(text, "- 해당 없음");
    } else {
        let break_rows: Vec<Vec<String>> = input
            .breaks
            .iter()
            .map(|row| {
                vec![
                    format_local_time(&row.ts),
                    row.agent.clone(),
                    row.model.clone(),
                    format_tokens(row.cache_creation_tokens),
                    format_tokens(row.previous_context_tokens),
                    format_duration_ms(Some(row.gap_secs * 1000)),
                    row.likely_cause.clone(),
                ]
            })
            .collect();
        let break_table = render_box_table(
            &[
                "시각",
                "에이전트",
                "모델",
                "재생성",
                "직전 컨텍스트",
                "간격",
                "추정 원인",
            ],
            &[
                ColumnAlign::Center,
                ColumnAlign::Left,
                ColumnAlign::Left,
                ColumnAlign::Right,
                ColumnAlign::Right,
                ColumnAlign::Right,
                ColumnAlign::Left,
            ],
            &break_rows,
        );
        let _ = writeln!(text, "```text\n{}\n```", break_table.join("\n"));
    }

    // 7. 게이트웨이
    let gateway = &input.gateway;
    let mut next_section = 7;
    if gateway.requests > 0 {
        let _ = writeln!(text, "\n## 7. 게이트웨이\n");
        let _ = writeln!(
            text,
            "- 요청: {}건 (오류 {}건)",
            format_count(gateway.requests as i64),
            format_count(gateway.errors as i64)
        );
        let _ = writeln!(
            text,
            "- 첫 바이트: p50 {} / p90 {}",
            format_duration_ms(gateway.first_byte_p50_ms),
            format_duration_ms(gateway.first_byte_p90_ms)
        );
        let _ = writeln!(
            text,
            "- 첫 토큰: p50 {} / p90 {}",
            format_duration_ms(gateway.first_token_p50_ms),
            format_duration_ms(gateway.first_token_p90_ms)
        );
        let _ = writeln!(
            text,
            "- 전체: p50 {} / p90 {}",
            format_duration_ms(gateway.total_p50_ms),
            format_duration_ms(gateway.total_p90_ms)
        );
        next_section = 8;
    }

    // 8. 자동 제안
    let _ = writeln!(text, "\n## {next_section}. 자동 제안\n");
    let suggestions = build_suggestions(input);
    if suggestions.is_empty() {
        let _ = writeln!(text, "- 제안 없음");
    }
    for suggestion in &suggestions {
        let _ = writeln!(text, "- {suggestion}");
    }
    text
}
