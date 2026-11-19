//! report/weekly.rs
//! ISO 주(월~일, 로컬) 단위 주간 리포트: 이번 주와 지난주 비교 자료 수집과 본문 생성.

use super::render::{
    ReportInput, format_count, format_tokens, write_compaction_section, write_comparison_table,
    write_gateway_section, write_hooks_section, write_suggestions_section, write_summary_lines,
    write_tools_section,
};
use super::table::{ColumnAlign, render_box_table};
use super::text::{ReportText, fill_template, select_report_text};
use super::{CACHE_BREAK_LIMIT, SUBAGENT_RUN_LIMIT, TOOL_LATENCY_LIMIT, local_midnight_utc};
use crate::core::config::ReportLanguage;
use crate::store::Store;
use crate::store::query::{AgentStatsRow, ModelStatsRow, OverviewStats};
use chrono::{Datelike, Duration, Local, NaiveDate, SecondsFormat, Weekday};
use std::collections::BTreeMap;
use std::fmt::Write;

const WEEK_KEY_YEAR_DIGITS: usize = 4;
const WEEK_KEY_WEEK_DIGITS: usize = 2;
const WEEK_DAYS: i64 = 7;

// 1. 일별 행 ------------------------------------------------------------------
#[derive(Debug, Clone, Default)]
pub struct DailyRow {
    pub date: NaiveDate,
    pub overview: OverviewStats,
    pub cost_usd: f64,
}

// 2. 주간 입력 ----------------------------------------------------------------
// base.today 는 이번 주, base.previous 는 지난주 개요
#[derive(Debug, Clone, Default)]
pub struct WeeklyInput {
    pub week_start: NaiveDate,
    pub base: ReportInput,
    pub days: Vec<DailyRow>,
    pub models: Vec<ModelStatsRow>,
    pub models_previous: Vec<ModelStatsRow>,
    pub agents: Vec<AgentStatsRow>,
    pub agents_previous: Vec<AgentStatsRow>,
}

// 3. 비용 표 행 ---------------------------------------------------------------
#[derive(Debug, Clone, Default)]
struct CostRow {
    name: String,
    requests: u64,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    cost_usd: f64,
    previous_cost_usd: f64,
}

// 4. 주 시작일 ----------------------------------------------------------------
// 날짜가 속한 ISO 주의 월요일
pub fn week_start_of(date: NaiveDate) -> NaiveDate {
    date - Duration::days(i64::from(date.weekday().num_days_from_monday()))
}

// 5. 주 키 --------------------------------------------------------------------
// 저장 키·파일 이름 YYYY-Www (ISO 주 연도 기준)
pub fn format_week_key(week_start: NaiveDate) -> String {
    week_start.format("%G-W%V").to_string()
}

// 6. 주 키 해석 ---------------------------------------------------------------
// YYYY-Www 형식만 받아 해당 주의 월요일, 없는 주 (예: 52주 해의 W53) 는 None
pub fn parse_week_key(key: &str) -> Option<NaiveDate> {
    let (year_text, week_text) = key.split_once("-W")?;
    if year_text.len() != WEEK_KEY_YEAR_DIGITS
        || week_text.len() != WEEK_KEY_WEEK_DIGITS
        || !year_text.bytes().all(|digit| digit.is_ascii_digit())
        || !week_text.bytes().all(|digit| digit.is_ascii_digit())
    {
        return None;
    }
    NaiveDate::from_isoywd_opt(
        year_text.parse().ok()?,
        week_text.parse().ok()?,
        Weekday::Mon,
    )
}

// 7. 주간 자료 수집 -----------------------------------------------------------
// 오늘 이후 날짜는 일별 행에서 제외 (진행 중인 주의 실시간 빌드용)
pub fn collect_weekly_input(store: &Store, week_start: NaiveDate) -> anyhow::Result<WeeklyInput> {
    // 1. 구간 (지난주, 이번 주)
    let monday = week_start_of(week_start);
    let stamp =
        |day: NaiveDate| local_midnight_utc(day).to_rfc3339_opts(SecondsFormat::Millis, true);
    let previous_start = stamp(monday - Duration::days(WEEK_DAYS));
    let start = stamp(monday);
    let end = stamp(monday + Duration::days(WEEK_DAYS));

    // 2. 일별 행
    let last_day = Local::now().date_naive();
    let mut days: Vec<DailyRow> = Vec::new();
    for offset in 0..WEEK_DAYS {
        let day = monday + Duration::days(offset);
        if day > last_day {
            break;
        }
        let day_start = stamp(day);
        let day_end = stamp(day + Duration::days(1));
        let cost_usd = store
            .query_model_usage(&day_start, &day_end)?
            .iter()
            .map(|row| row.cost_usd_estimate)
            .sum();
        days.push(DailyRow {
            date: day,
            overview: store.query_overview(&day_start, &day_end)?,
            cost_usd,
        });
    }

    // 3. 주 단위 집계
    let base = ReportInput {
        report_date: monday,
        weekly: true,
        today: store.query_overview(&start, &end)?,
        previous: store.query_overview(&previous_start, &start)?,
        usage: Vec::new(),
        tools: store.query_tool_latency(&start, &end, TOOL_LATENCY_LIMIT)?,
        hooks: store.query_hook_stats(&start, &end)?,
        breaks: store.query_cache_breaks(&start, &end, CACHE_BREAK_LIMIT)?,
        gateway: store.query_gateway_timing(&start, &end)?,
        sources: store.query_source_counts(&start, &end)?,
        compaction: store.compaction_stats(&start, &end).unwrap_or_default(),
        subagents: store
            .subagent_runs(&start, &end, SUBAGENT_RUN_LIMIT)
            .unwrap_or_default(),
    };
    Ok(WeeklyInput {
        week_start: monday,
        base,
        days,
        models: store.query_model_stats(&start, &end)?,
        models_previous: store.query_model_stats(&previous_start, &start)?,
        agents: store.query_agent_stats(&start, &end)?,
        agents_previous: store.query_agent_stats(&previous_start, &start)?,
    })
}

// 8. 주간 리포트 본문 생성 ----------------------------------------------------
// 보관하지 않는 실시간 빌드에도 사용 (진행 중인 주)
pub fn build_weekly_report(
    store: &Store,
    week_start: NaiveDate,
    language: ReportLanguage,
) -> anyhow::Result<String> {
    let input = collect_weekly_input(store, week_start)?;
    Ok(render_weekly_markdown(&input, language))
}

// 9. 비용 표 기록 -------------------------------------------------------------
fn write_cost_table(text: &mut String, labels: &ReportText, headers: &[&str; 9], rows: &[CostRow]) {
    let table_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            vec![
                row.name.clone(),
                format_count(row.requests as i64),
                format_tokens(row.input_tokens),
                format_tokens(row.output_tokens),
                format_tokens(row.cache_read_tokens),
                format_tokens(row.cache_creation_tokens),
                format!("{:.2}", row.cost_usd),
                format!("{:.2}", row.previous_cost_usd),
                format!("{:+.2}", row.cost_usd - row.previous_cost_usd),
            ]
        })
        .collect();
    let mut aligns = vec![ColumnAlign::Right; headers.len()];
    aligns[0] = ColumnAlign::Left;
    let table = render_box_table(headers, &aligns, &table_rows);
    let _ = writeln!(text, "```text\n{}\n```", table.join("\n"));
    let cost_total: f64 = rows.iter().map(|row| row.cost_usd).sum();
    let _ = writeln!(
        text,
        "\n{}",
        fill_template(labels.usage_total_cost, &[&format!("{cost_total:.2}")])
    );
}

// 10. 주간 리포트 본문 --------------------------------------------------------
pub fn render_weekly_markdown(input: &WeeklyInput, language: ReportLanguage) -> String {
    let labels = select_report_text(language);
    let base = &input.base;
    let this_week = &base.today;
    let this_cost: f64 = input.models.iter().map(|row| row.cost_usd_estimate).sum();
    let previous_cost: f64 = input
        .models_previous
        .iter()
        .map(|row| row.cost_usd_estimate)
        .sum();
    let mut text = String::new();

    // 1. 요약
    let week_end = input.week_start + Duration::days(WEEK_DAYS - 1);
    let title = fill_template(
        labels.title_weekly,
        &[
            &format_week_key(input.week_start),
            &input.week_start.format("%Y-%m-%d").to_string(),
            &week_end.format("%Y-%m-%d").to_string(),
        ],
    );
    let _ = writeln!(text, "# {title}");
    let _ = writeln!(
        text,
        "\n{}\n",
        fill_template(labels.timezone_note, &[&this_week.since, &this_week.until])
    );
    let _ = writeln!(text, "## 1. {}\n", labels.section_summary_week);
    write_summary_lines(&mut text, labels, this_week);
    let _ = writeln!(
        text,
        "{}",
        fill_template(labels.summary_cost, &[&format!("{this_cost:.2}")])
    );

    // 2. 지난주 대비
    let _ = writeln!(text, "\n## 2. {}\n", labels.section_previous_week);
    if base.previous.api_requests == 0 && base.previous.tool_calls == 0 {
        let _ = writeln!(text, "{}", labels.empty_previous_week);
    } else {
        let cost_row = vec![
            labels.compare_cost.to_string(),
            format!("{this_cost:.2}"),
            format!("{previous_cost:.2}"),
            format!("{:+.2}", this_cost - previous_cost),
        ];
        write_comparison_table(
            &mut text,
            labels,
            &labels.compare_headers_week,
            this_week,
            &base.previous,
            vec![cost_row],
        );
    }

    // 3. 일별 추이
    let _ = writeln!(text, "\n## 3. {}\n", labels.section_daily);
    if input.days.is_empty() {
        let _ = writeln!(text, "{}", labels.empty_records);
    } else {
        let daily_rows: Vec<Vec<String>> = input
            .days
            .iter()
            .map(|row| {
                let weekday_index = row.date.weekday().num_days_from_monday() as usize;
                vec![
                    row.date.format("%Y-%m-%d").to_string(),
                    labels.weekdays[weekday_index].to_string(),
                    format_count(row.overview.api_requests as i64),
                    format_tokens(row.overview.output_tokens),
                    format_count(row.overview.tool_calls as i64),
                    format_count(row.overview.hook_denials as i64),
                    format!("{:.1}%", row.overview.cache_hit_ratio * 100.0),
                    format!("{:.2}", row.cost_usd),
                ]
            })
            .collect();
        let mut aligns = vec![ColumnAlign::Right; labels.daily_headers.len()];
        aligns[0] = ColumnAlign::Center;
        aligns[1] = ColumnAlign::Center;
        let daily_table = render_box_table(&labels.daily_headers, &aligns, &daily_rows);
        let _ = writeln!(text, "```text\n{}\n```", daily_table.join("\n"));
    }

    // 4. 모델별
    let _ = writeln!(text, "\n## 4. {}\n", labels.section_models);
    if input.models.is_empty() {
        let _ = writeln!(text, "{}", labels.empty_records);
    } else {
        let previous_by_model: BTreeMap<&str, f64> = input
            .models_previous
            .iter()
            .map(|row| (row.model.as_str(), row.cost_usd_estimate))
            .collect();
        let model_rows: Vec<CostRow> = input
            .models
            .iter()
            .map(|row| CostRow {
                name: row.model.clone(),
                requests: row.requests,
                input_tokens: row.input_tokens,
                output_tokens: row.output_tokens,
                cache_read_tokens: row.cache_read_tokens,
                cache_creation_tokens: row.cache_creation_tokens,
                cost_usd: row.cost_usd_estimate,
                previous_cost_usd: previous_by_model
                    .get(row.model.as_str())
                    .copied()
                    .unwrap_or(0.0),
            })
            .collect();
        write_cost_table(&mut text, labels, &labels.model_headers_week, &model_rows);
    }

    // 5. 에이전트별
    let _ = writeln!(text, "\n## 5. {}\n", labels.section_agents);
    if input.agents.is_empty() {
        let _ = writeln!(text, "{}", labels.empty_records);
    } else {
        let previous_by_agent: BTreeMap<&str, f64> = input
            .agents_previous
            .iter()
            .map(|row| (row.agent.as_str(), row.cost_usd_estimate))
            .collect();
        let agent_rows: Vec<CostRow> = input
            .agents
            .iter()
            .map(|row| CostRow {
                name: row.agent.clone(),
                requests: row.requests,
                input_tokens: row.input_tokens,
                output_tokens: row.output_tokens,
                cache_read_tokens: row.cache_read_tokens,
                cache_creation_tokens: row.cache_creation_tokens,
                cost_usd: row.cost_usd_estimate,
                previous_cost_usd: previous_by_agent
                    .get(row.agent.as_str())
                    .copied()
                    .unwrap_or(0.0),
            })
            .collect();
        write_cost_table(&mut text, labels, &labels.agent_headers_week, &agent_rows);
    }

    // 6. 도구·훅·게이트웨이·자동 제안
    write_tools_section(&mut text, labels, 6, &base.tools);
    write_hooks_section(&mut text, labels, 7, &base.hooks);
    let mut next_section = 8;
    if write_gateway_section(&mut text, labels, next_section, &base.gateway) {
        next_section += 1;
    }
    if write_compaction_section(
        &mut text,
        labels,
        next_section,
        &base.compaction,
        &base.subagents,
    ) {
        next_section += 1;
    }
    write_suggestions_section(&mut text, labels, next_section, base);
    text
}
