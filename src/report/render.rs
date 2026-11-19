//! report/render.rs
//! 집계 결과를 언어별 Markdown 리포트로 변환하고 규칙 기반 제안 생성 (문구는 text.rs).

use super::table::{ColumnAlign, render_box_table};
use super::text::{ReportText, fill_template, select_report_text};
use crate::core::config::ReportLanguage;
use crate::core::model::DENY_ROUND_TRIP_MS;
use crate::store::otel_events::{CompactionStat, SubagentRun};
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
const COMPACTION_TABLE_ROWS: usize = 10;
const SESSION_ID_CHARS: usize = 8;

// 1. 리포트 입력 --------------------------------------------------------------
// today 는 대상 구간 (일간은 당일, 주간은 이번 주), previous 는 직전 같은 길이 구간
#[derive(Debug, Clone, Default)]
pub struct ReportInput {
    pub report_date: NaiveDate,
    pub weekly: bool,
    pub today: OverviewStats,
    pub previous: OverviewStats,
    pub usage: Vec<ModelUsageRow>,
    pub tools: Vec<ToolLatencyRow>,
    pub hooks: Vec<HookStatRow>,
    pub breaks: Vec<CacheBreakRow>,
    pub gateway: GatewayTimingStats,
    pub sources: SourceCounts,
    pub compaction: Vec<CompactionStat>,
    pub subagents: Vec<SubagentRun>,
}

// 2. 시간 표기 ----------------------------------------------------------------
// 1초 미만 ms, 1분 미만 초, 이후 분·초
pub fn format_duration_ms(text: &ReportText, duration_ms: Option<i64>) -> String {
    let Some(elapsed) = duration_ms else {
        return "-".to_string();
    };
    if elapsed.abs() < 1000 {
        return format!("{elapsed}ms");
    }
    if elapsed.abs() < 60_000 {
        let seconds = format!("{:.1}", elapsed as f64 / 1000.0);
        return fill_template(text.duration_seconds, &[&seconds]);
    }
    let sign = if elapsed < 0 { "-" } else { "" };
    let total_secs = elapsed.abs() / 1000;
    fill_template(
        text.duration_minutes,
        &[
            sign,
            &(total_secs / 60).to_string(),
            &format!("{:02}", total_secs % 60),
        ],
    )
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
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
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
pub fn build_suggestions(text: &ReportText, input: &ReportInput) -> Vec<String> {
    let mut suggestions: Vec<String> = Vec::new();

    // 1. 반복 거부 명령
    let mut deny_rows: Vec<&HookStatRow> = input
        .hooks
        .iter()
        .filter(|row| row.outcome == "deny" && row.count >= DENY_RULE_MIN_COUNT)
        .collect();
    deny_rows.sort_by_key(|row| std::cmp::Reverse(row.count));
    for row in deny_rows.into_iter().take(5) {
        let lost = format_duration_ms(text, Some(row.count as i64 * DENY_ROUND_TRIP_MS));
        let count = row.count.to_string();
        if row.detail.is_empty() {
            suggestions.push(fill_template(
                text.suggest_deny_unnamed,
                &[&row.guard, &count, &lost],
            ));
        } else {
            suggestions.push(fill_template(
                text.suggest_deny_named,
                &[&row.detail, &row.guard, &count, &lost],
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
            .map(|(cause, count)| {
                fill_template(text.suggest_break_cause, &[cause, &count.to_string()])
            })
            .collect();
        suggestions.push(fill_template(
            text.suggest_breaks,
            &[
                &input.breaks.len().to_string(),
                &format_tokens(created_total),
                &causes.join(", "),
            ],
        ));
    }

    // 3. 낮은 캐시 적중
    if input.today.api_requests >= LOW_CACHE_MIN_REQUESTS
        && input.today.cache_hit_ratio < LOW_CACHE_HIT_RATIO
    {
        suggestions.push(fill_template(
            text.suggest_low_cache,
            &[&format!("{:.1}", input.today.cache_hit_ratio * 100.0)],
        ));
    }

    // 4. 느린 도구
    for row in &input.tools {
        let slow = row.p90_ms.is_some_and(|p90| p90 > SLOW_TOOL_P90_MS);
        if slow && row.calls >= SLOW_TOOL_MIN_CALLS && row.tool_name != "AskUserQuestion" {
            suggestions.push(fill_template(
                text.suggest_slow_tool,
                &[
                    &row.tool_name,
                    &format_duration_ms(text, row.p90_ms),
                    &row.calls.to_string(),
                ],
            ));
        }
    }

    // 5. 텔레메트리 미설정
    if input.sources.otlp == 0 {
        suggestions.push(text.suggest_no_otlp.to_string());
    }

    // 6. 기록 없음
    if input.today.api_requests == 0 {
        let empty_note = if input.weekly {
            text.suggest_no_records_week
        } else {
            text.suggest_no_records_day
        };
        suggestions.push(empty_note.to_string());
    }
    suggestions
}

// 8. 요약 항목 ----------------------------------------------------------------
// 일간·주간 공용 요약 불릿
pub fn write_summary_lines(text: &mut String, labels: &ReportText, stats: &OverviewStats) {
    let _ = writeln!(
        text,
        "{}",
        fill_template(
            labels.summary_requests,
            &[
                &format_count(stats.api_requests as i64),
                &format_count(stats.sessions as i64),
            ],
        )
    );
    let _ = writeln!(
        text,
        "{}",
        fill_template(
            labels.summary_tokens,
            &[
                &format_tokens(stats.input_tokens),
                &format_tokens(stats.output_tokens),
                &format_tokens(stats.cache_read_tokens),
                &format_tokens(stats.cache_creation_tokens),
            ],
        )
    );
    let _ = writeln!(
        text,
        "{}",
        fill_template(
            labels.summary_cache_hit,
            &[&format!("{:.1}", stats.cache_hit_ratio * 100.0)],
        )
    );
    let _ = writeln!(
        text,
        "{}",
        fill_template(
            labels.summary_model_wait,
            &[
                &format_duration_ms(labels, stats.model_wait_p50_ms),
                &format_duration_ms(labels, stats.model_wait_p90_ms),
            ],
        )
    );
    let _ = writeln!(
        text,
        "{}",
        fill_template(
            labels.summary_tools,
            &[
                &format_count(stats.tool_calls as i64),
                &format_count(stats.tool_errors as i64),
            ],
        )
    );
    let _ = writeln!(
        text,
        "{}",
        fill_template(
            labels.summary_hooks,
            &[
                &format_count(stats.hook_denials as i64),
                &format_count(stats.hook_rewrites as i64),
            ],
        )
    );
    let _ = writeln!(
        text,
        "{}",
        fill_template(
            labels.summary_saved,
            &[&format_duration_ms(labels, Some(stats.saved_ms))],
        )
    );
    let _ = writeln!(
        text,
        "{}",
        fill_template(
            labels.summary_lost,
            &[
                &format_duration_ms(labels, Some(stats.lost_ms_estimate)),
                &format_count(stats.hook_denials as i64),
                &format_duration_ms(labels, Some(DENY_ROUND_TRIP_MS)),
            ],
        )
    );
}

// 9. 구간 대비 표 -------------------------------------------------------------
// 일간(전일)·주간(지난주) 공용, extra_rows 는 표 끝에 덧붙일 행
pub fn write_comparison_table(
    text: &mut String,
    labels: &ReportText,
    headers: &[&str; 4],
    current: &OverviewStats,
    previous: &OverviewStats,
    extra_rows: Vec<Vec<String>>,
) {
    // 1. 건수 행
    let mut compare_rows: Vec<Vec<String>> = Vec::new();
    let count_rows = [
        (
            labels.compare_api_requests,
            current.api_requests as i64,
            previous.api_requests as i64,
        ),
        (
            labels.compare_output_tokens,
            current.output_tokens,
            previous.output_tokens,
        ),
        (
            labels.compare_tool_calls,
            current.tool_calls as i64,
            previous.tool_calls as i64,
        ),
        (
            labels.compare_tool_errors,
            current.tool_errors as i64,
            previous.tool_errors as i64,
        ),
        (
            labels.compare_hook_denials,
            current.hook_denials as i64,
            previous.hook_denials as i64,
        ),
        (
            labels.compare_hook_rewrites,
            current.hook_rewrites as i64,
            previous.hook_rewrites as i64,
        ),
    ];
    for (label, now_count, before_count) in count_rows {
        compare_rows.push(vec![
            label.to_string(),
            format_count(now_count),
            format_count(before_count),
            format_count_delta(now_count, before_count),
        ]);
    }

    // 2. 적중률·대기 행
    compare_rows.push(vec![
        labels.compare_cache_hit.to_string(),
        format!("{:.1}%", current.cache_hit_ratio * 100.0),
        format!("{:.1}%", previous.cache_hit_ratio * 100.0),
        format!(
            "{:+.1}%p",
            (current.cache_hit_ratio - previous.cache_hit_ratio) * 100.0
        ),
    ]);
    let wait_rows = [
        (
            labels.compare_wait_p50,
            current.model_wait_p50_ms,
            previous.model_wait_p50_ms,
        ),
        (
            labels.compare_wait_p90,
            current.model_wait_p90_ms,
            previous.model_wait_p90_ms,
        ),
    ];
    for (label, now_wait, before_wait) in wait_rows {
        let delta = match (now_wait, before_wait) {
            (Some(now_ms), Some(before_ms)) if now_ms > before_ms => {
                format!("+{}", format_duration_ms(labels, Some(now_ms - before_ms)))
            }
            (Some(now_ms), Some(before_ms)) => format_duration_ms(labels, Some(now_ms - before_ms)),
            _ => "-".to_string(),
        };
        compare_rows.push(vec![
            label.to_string(),
            format_duration_ms(labels, now_wait),
            format_duration_ms(labels, before_wait),
            delta,
        ]);
    }
    compare_rows.extend(extra_rows);

    // 3. 표 기록
    let compare_table = render_box_table(
        headers,
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

// 10. 도구 지연 구간 ----------------------------------------------------------
pub fn write_tools_section(
    text: &mut String,
    labels: &ReportText,
    number: usize,
    tools: &[ToolLatencyRow],
) {
    let _ = writeln!(text, "\n## {number}. {}\n", labels.section_tools);
    if tools.is_empty() {
        let _ = writeln!(text, "{}", labels.empty_records);
        return;
    }
    let tool_rows: Vec<Vec<String>> = tools
        .iter()
        .take(10)
        .map(|row| {
            vec![
                row.tool_name.clone(),
                format_count(row.calls as i64),
                format_count(row.errors as i64),
                format_duration_ms(labels, row.p50_ms),
                format_duration_ms(labels, row.p90_ms),
                format_duration_ms(labels, Some(row.total_ms)),
            ]
        })
        .collect();
    let tool_table = render_box_table(
        &labels.tool_headers,
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

// 11. 훅 판정 구간 ------------------------------------------------------------
pub fn write_hooks_section(
    text: &mut String,
    labels: &ReportText,
    number: usize,
    hooks: &[HookStatRow],
) {
    let _ = writeln!(text, "\n## {number}. {}\n", labels.section_hooks);
    if hooks.is_empty() {
        let _ = writeln!(text, "{}", labels.empty_records);
        return;
    }
    let hook_rows: Vec<Vec<String>> = hooks
        .iter()
        .take(HOOK_TABLE_ROWS)
        .map(|row| {
            vec![
                row.outcome.clone(),
                row.route.clone(),
                row.guard.clone(),
                row.detail.clone(),
                format_count(row.count as i64),
                format_duration_ms(labels, row.p90_ms),
                format_duration_ms(labels, Some(row.saved_ms)),
            ]
        })
        .collect();
    let hook_table = render_box_table(
        &labels.hook_headers,
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
    if hooks.len() > HOOK_TABLE_ROWS {
        let omitted = (hooks.len() - HOOK_TABLE_ROWS).to_string();
        let _ = writeln!(
            text,
            "\n{}",
            fill_template(labels.hook_rows_omitted, &[&omitted])
        );
    }
}

// 12. 게이트웨이 구간 ---------------------------------------------------------
// 요청이 없으면 구간을 쓰지 않고 false
pub fn write_gateway_section(
    text: &mut String,
    labels: &ReportText,
    number: usize,
    gateway: &GatewayTimingStats,
) -> bool {
    if gateway.requests == 0 {
        return false;
    }
    let _ = writeln!(text, "\n## {number}. {}\n", labels.section_gateway);
    let _ = writeln!(
        text,
        "{}",
        fill_template(
            labels.gateway_requests,
            &[
                &format_count(gateway.requests as i64),
                &format_count(gateway.errors as i64),
            ],
        )
    );
    let timing_lines = [
        (
            labels.gateway_first_byte,
            gateway.first_byte_p50_ms,
            gateway.first_byte_p90_ms,
        ),
        (
            labels.gateway_first_token,
            gateway.first_token_p50_ms,
            gateway.first_token_p90_ms,
        ),
        (
            labels.gateway_total,
            gateway.total_p50_ms,
            gateway.total_p90_ms,
        ),
    ];
    for (template, p50_ms, p90_ms) in timing_lines {
        let _ = writeln!(
            text,
            "{}",
            fill_template(
                template,
                &[
                    &format_duration_ms(labels, p50_ms),
                    &format_duration_ms(labels, p90_ms),
                ],
            )
        );
    }
    true
}

// 12-1. 컴팩션·컨텍스트 구간 --------------------------------------------------
// 컴팩션과 서브에이전트 기록이 모두 없으면 구간을 쓰지 않고 false
pub fn write_compaction_section(
    text: &mut String,
    labels: &ReportText,
    number: usize,
    compaction: &[CompactionStat],
    subagents: &[SubagentRun],
) -> bool {
    if compaction.is_empty() && subagents.is_empty() {
        return false;
    }
    let _ = writeln!(text, "\n## {number}. {}\n", labels.section_compaction);
    let tokens_or_dash = |tokens: Option<i64>| tokens.map_or("-".to_string(), format_tokens);
    let short_session =
        |session_id: &str| -> String { session_id.chars().take(SESSION_ID_CHARS).collect() };

    // 1. 컴팩션 요약과 마지막 컴팩션 (조회가 최근 순이라 첫 행이 최근)
    if compaction.is_empty() {
        let _ = writeln!(text, "{}", labels.compaction_none);
    } else {
        let total: u64 = compaction.iter().map(|stat| stat.count).sum();
        let auto: u64 = compaction.iter().map(|stat| stat.auto_count).sum();
        let manual: u64 = compaction.iter().map(|stat| stat.manual_count).sum();
        let failed: u64 = compaction.iter().map(|stat| stat.failed_count).sum();
        let _ = writeln!(
            text,
            "{}",
            fill_template(
                labels.compaction_summary,
                &[
                    &format_count(total as i64),
                    &format_count(auto as i64),
                    &format_count(manual as i64),
                    &format_count(failed as i64),
                    &format_count(compaction.len() as i64),
                ],
            )
        );
        let latest = &compaction[0];
        let trigger = if latest.last_trigger.is_empty() {
            "-"
        } else {
            latest.last_trigger.as_str()
        };
        let _ = writeln!(
            text,
            "{}",
            fill_template(
                labels.compaction_last,
                &[
                    &format_local_time(&latest.last_ts),
                    trigger,
                    &short_session(&latest.session_id),
                    &tokens_or_dash(latest.last_pre_tokens),
                    &tokens_or_dash(latest.last_post_tokens),
                ],
            )
        );
    }

    // 2. 서브에이전트 합계 (소요가 있는 건만 평균에 포함)
    if subagents.is_empty() {
        let _ = writeln!(text, "{}", labels.subagent_none);
    } else {
        let timed: Vec<i64> = subagents.iter().filter_map(|run| run.duration_ms).collect();
        let total_ms: i64 = timed.iter().sum();
        let average_ms = if timed.is_empty() {
            None
        } else {
            Some(total_ms / timed.len() as i64)
        };
        let tool_uses: i64 = subagents.iter().filter_map(|run| run.tool_uses).sum();
        let _ = writeln!(
            text,
            "{}",
            fill_template(
                labels.subagent_summary,
                &[
                    &format_count(subagents.len() as i64),
                    &format_duration_ms(labels, Some(total_ms)),
                    &format_duration_ms(labels, average_ms),
                    &format_count(tool_uses),
                ],
            )
        );
    }

    // 3. 세션별 표 (최근 컴팩션 순, 상위만)
    if compaction.is_empty() {
        return true;
    }
    let session_rows: Vec<Vec<String>> = compaction
        .iter()
        .take(COMPACTION_TABLE_ROWS)
        .map(|stat| {
            vec![
                short_session(&stat.session_id),
                format_count(stat.count as i64),
                format_count(stat.auto_count as i64),
                format_count(stat.manual_count as i64),
                format_count(stat.failed_count as i64),
                format_local_time(&stat.last_ts),
                format!(
                    "{} -> {}",
                    tokens_or_dash(stat.last_pre_tokens),
                    tokens_or_dash(stat.last_post_tokens)
                ),
            ]
        })
        .collect();
    let session_table = render_box_table(
        &labels.compaction_headers,
        &[
            ColumnAlign::Left,
            ColumnAlign::Right,
            ColumnAlign::Right,
            ColumnAlign::Right,
            ColumnAlign::Right,
            ColumnAlign::Center,
            ColumnAlign::Right,
        ],
        &session_rows,
    );
    let _ = writeln!(text, "\n```text\n{}\n```", session_table.join("\n"));
    if compaction.len() > COMPACTION_TABLE_ROWS {
        let omitted = (compaction.len() - COMPACTION_TABLE_ROWS).to_string();
        let _ = writeln!(
            text,
            "\n{}",
            fill_template(labels.hook_rows_omitted, &[&omitted])
        );
    }
    true
}

// 13. 자동 제안 구간 ----------------------------------------------------------
pub fn write_suggestions_section(
    text: &mut String,
    labels: &ReportText,
    number: usize,
    input: &ReportInput,
) {
    let _ = writeln!(text, "\n## {number}. {}\n", labels.section_suggestions);
    let suggestions = build_suggestions(labels, input);
    if suggestions.is_empty() {
        let _ = writeln!(text, "{}", labels.empty_suggestions);
    }
    for suggestion in &suggestions {
        let _ = writeln!(text, "- {suggestion}");
    }
}

// 14. 일일 리포트 본문 --------------------------------------------------------
pub fn render_report_markdown(input: &ReportInput, language: ReportLanguage) -> String {
    let labels = select_report_text(language);
    let today = &input.today;
    let mut text = String::new();

    // 1. 요약
    let date_text = input.report_date.format("%Y-%m-%d").to_string();
    let _ = writeln!(
        text,
        "# {}",
        fill_template(labels.title_daily, &[&date_text])
    );
    let _ = writeln!(
        text,
        "\n{}\n",
        fill_template(labels.timezone_note, &[&today.since, &today.until])
    );
    let _ = writeln!(text, "## 1. {}\n", labels.section_summary);
    write_summary_lines(&mut text, labels, today);

    // 2. 전일 대비
    let _ = writeln!(text, "\n## 2. {}\n", labels.section_previous_day);
    if input.previous.api_requests == 0 && input.previous.tool_calls == 0 {
        let _ = writeln!(text, "{}", labels.empty_previous_day);
    } else {
        write_comparison_table(
            &mut text,
            labels,
            &labels.compare_headers_day,
            today,
            &input.previous,
            Vec::new(),
        );
    }

    // 3. 모델·에이전트별
    let _ = writeln!(text, "\n## 3. {}\n", labels.section_models_agents);
    if input.usage.is_empty() {
        let _ = writeln!(text, "{}", labels.empty_records);
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
            &labels.usage_headers,
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
            "\n{}",
            fill_template(labels.usage_total_cost, &[&format!("{cost_total:.2}")])
        );
    }

    // 4. 도구 지연·훅 판정
    write_tools_section(&mut text, labels, 4, &input.tools);
    write_hooks_section(&mut text, labels, 5, &input.hooks);

    // 5. 캐시 붕괴 상위
    let _ = writeln!(text, "\n## 6. {}\n", labels.section_breaks);
    if input.breaks.is_empty() {
        let _ = writeln!(text, "{}", labels.empty_not_applicable);
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
                    format_duration_ms(labels, Some(row.gap_secs * 1000)),
                    row.likely_cause.clone(),
                ]
            })
            .collect();
        let break_table = render_box_table(
            &labels.break_headers,
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

    // 6. 게이트웨이·자동 제안
    let mut next_section = 7;
    if write_gateway_section(&mut text, labels, next_section, &input.gateway) {
        next_section += 1;
    }
    if write_compaction_section(
        &mut text,
        labels,
        next_section,
        &input.compaction,
        &input.subagents,
    ) {
        next_section += 1;
    }
    write_suggestions_section(&mut text, labels, next_section, input);
    text
}
