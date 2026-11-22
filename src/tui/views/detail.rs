//! tui/views/detail.rs
//! 상세 카드 공용 줄 구성: 항목 이름·값 한 줄, 구역 제목, 비용·호출 분포 목록, 점유율 계산.

use crate::store::query::UsageShare;
use crate::tui::format::{fit_display, format_clock_time, format_compact, format_count};
use crate::tui::i18n::{self, Lang, Msg};
use crate::tui::theme;
use ratatui::text::{Line, Span};

// 상세 항목 이름 칸 폭 (모델·에이전트·프로젝트·스킬 상세 공용)
pub const DETAIL_LABEL_WIDTH: usize = 23;
// 분포 목록 이름 앞 들여쓰기
const SHARE_INDENT: &str = "  ";

// 1. 분포 기준 ----------------------------------------------------------------
// 비용 = 요청 수와 비용, 점유율은 비용 기준 / 호출 = 호출·오류 수, 점유율은 호출 수 기준
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareBasis {
    Cost,
    Calls,
}

// 2. 항목 줄 ------------------------------------------------------------------
// 이름 칸은 흐리게 고정 폭, 값은 본문색
pub fn pair_line(name: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(fit_display(name, DETAIL_LABEL_WIDTH), theme::STYLE_DIM),
        Span::styled(value, theme::STYLE_TEXT),
    ])
}

// 3. 구역 제목 ----------------------------------------------------------------
pub fn section_line(lang: Lang, heading: Msg) -> Line<'static> {
    Line::from(Span::styled(
        i18n::text(lang, heading).to_string(),
        theme::STYLE_ACCENT,
    ))
}

// 4. 분포 목록 ----------------------------------------------------------------
// 빈 목록은 구역째 생략, 이름이 비면 "-", 점유율 분모는 total (0 이면 0%)
pub fn share_lines(
    lang: Lang,
    heading: Msg,
    shares: &[UsageShare],
    basis: ShareBasis,
    total: f64,
) -> Vec<Line<'static>> {
    if shares.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![section_line(lang, heading)];
    for share in shares {
        let name = if share.name.is_empty() {
            "-"
        } else {
            share.name.as_str()
        };
        let count = format_count(i64::try_from(share.count).unwrap_or(i64::MAX));
        let value = match basis {
            ShareBasis::Cost => i18n::share_cost_value(
                lang,
                &count,
                share.cost_usd_estimate,
                percent(share.cost_usd_estimate, total),
            ),
            ShareBasis::Calls => i18n::share_calls_value(
                lang,
                &count,
                &format_count(i64::try_from(share.errors).unwrap_or(i64::MAX)),
                percent(share.count as f64, total),
            ),
        };
        let value_style = if basis == ShareBasis::Calls && share.errors > 0 {
            theme::STYLE_WARN
        } else {
            theme::STYLE_TEXT
        };
        lines.push(Line::from(vec![
            Span::styled(
                fit_display(&format!("{SHARE_INDENT}{name}"), DETAIL_LABEL_WIDTH),
                theme::STYLE_TEXT,
            ),
            Span::styled(value, value_style),
        ]));
    }
    lines
}

// 5. 점유율 -------------------------------------------------------------------
// 분모가 0 이하이면 0
pub fn percent(part: f64, total: f64) -> f64 {
    if total > 0.0 {
        part * 100.0 / total
    } else {
        0.0
    }
}

// 상세 카드 점유율 분모 (구간 안 표 전체의 요청·비용 합)
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowTotals {
    pub requests: u64,
    pub cost: f64,
}

// 6. 값 문구 ------------------------------------------------------------------
// 구간 점유: 요청·비용 비율
pub fn window_share(lang: Lang, requests: u64, cost: f64, window: WindowTotals) -> String {
    i18n::window_share_value(
        lang,
        percent(requests as f64, window.requests as f64),
        percent(cost, window.cost),
    )
}

// 요청당 비용과 출력 토큰 (요청이 없으면 -)
pub fn per_request(cost: f64, output_tokens: i64, requests: u64) -> String {
    if requests == 0 {
        return "-".to_string();
    }
    let average_output = output_tokens / i64::try_from(requests).unwrap_or(i64::MAX);
    format!(
        "${:.4} / {}",
        cost / requests as f64,
        format_compact(average_output)
    )
}

// 캐시 생성 5분·1시간
pub fn cache_split(write_5m: i64, write_1h: i64) -> String {
    format!(
        "{} / {}",
        format_compact(write_5m),
        format_compact(write_1h)
    )
}

// 처음·마지막 시각 (없는 값은 -)
pub fn first_last(first_ts: &str, last_ts: &str) -> String {
    let clock = |ts: &str| {
        if ts.is_empty() {
            "-".to_string()
        } else {
            format_clock_time(ts)
        }
    };
    format!("{} / {}", clock(first_ts), clock(last_ts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::usage_share;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn share_lines_show_a_heading_then_one_indented_row_per_entry() {
        let shares = [
            usage_share("main", 3, 0, 6.0, "2026-11-15T01:00:00Z"),
            usage_share("worker", 1, 1, 2.0, "2026-11-15T01:00:00Z"),
        ];
        let cost = share_lines(
            Lang::En,
            Msg::SectionAgentsByCost,
            &shares,
            ShareBasis::Cost,
            8.0,
        );
        let text: Vec<String> = cost.iter().map(line_text).collect();
        assert_eq!(text.len(), 3);
        assert_eq!(text[0], "Agents by cost");
        assert!(text[1].starts_with("  main"), "{text:?}");
        assert!(text[1].ends_with("3 req · $6.00 · 75%"), "{text:?}");

        let calls = share_lines(
            Lang::Ko,
            Msg::SectionCallers,
            &shares,
            ShareBasis::Calls,
            4.0,
        );
        let text: Vec<String> = calls.iter().map(line_text).collect();
        assert!(text[2].ends_with("호출 1 · 오류 1 · 25%"), "{text:?}");
        // 오류가 있는 줄은 경고색
        assert_eq!(calls[2].spans[1].style, theme::STYLE_WARN);

        // 빈 목록은 구역 제목도 없음, 분모 0 은 0%
        assert!(share_lines(Lang::En, Msg::SectionCallers, &[], ShareBasis::Calls, 0.0).is_empty());
        assert_eq!(percent(1.0, 0.0), 0.0);
    }
}
