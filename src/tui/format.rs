//! tui/format.rs
//! 화면 표시용 숫자·시간·토큰 서식, 표시 폭 기준 문자열 맞춤, 리포트 본문 준비·목차 추출.

use crate::report::table::convert_pipe_tables;
use chrono::{DateTime, Local};
use ratatui::text::Span;

// 1. 천 단위 구분 정수 --------------------------------------------------------
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

// 2. 토큰 축약 (K, M, B) ------------------------------------------------------
pub fn format_compact(amount: i64) -> String {
    let magnitude = amount.unsigned_abs() as f64;
    if magnitude < 1_000.0 {
        return amount.to_string();
    }
    let sign = if amount < 0 { "-" } else { "" };
    let (scaled, suffix) = if magnitude < 1_000_000.0 {
        (magnitude / 1_000.0, "K")
    } else if magnitude < 1_000_000_000.0 {
        (magnitude / 1_000_000.0, "M")
    } else {
        (magnitude / 1_000_000_000.0, "B")
    };
    format!("{sign}{scaled:.1}{suffix}")
}

// 3. 밀리초 기간 --------------------------------------------------------------
pub fn format_duration_ms(millis: i64) -> String {
    let millis = millis.max(0);
    if millis < 1_000 {
        return format!("{millis}ms");
    }
    if millis < 60_000 {
        return format!("{:.1}s", millis as f64 / 1_000.0);
    }
    let total_secs = millis / 1_000;
    if total_secs < 3_600 {
        return format!("{}m {:02}s", total_secs / 60, total_secs % 60);
    }
    format!("{}h {:02}m", total_secs / 3_600, (total_secs % 3_600) / 60)
}

// 4. 표시 폭 맞춤 -------------------------------------------------------------
// 넘치면 말줄임, 모자라면 공백으로 정확히 width 열, 제어 문자는 공백
pub fn fit_display(text: &str, width: usize) -> String {
    let total = Span::raw(text).width();
    let limit = if total > width {
        width.saturating_sub(1)
    } else {
        width
    };
    let mut fitted = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let shown = if ch.is_control() { ' ' } else { ch };
        let mut buffer = [0u8; 4];
        let shown_width = Span::raw(&*shown.encode_utf8(&mut buffer)).width();
        if used + shown_width > limit {
            break;
        }
        fitted.push(shown);
        used += shown_width;
    }
    if total > width && width > 0 {
        fitted.push('…');
        used += 1;
    }
    while used < width {
        fitted.push(' ');
        used += 1;
    }
    fitted
}

// 5. 시각 표시 ----------------------------------------------------------------
// RFC 3339 UTC 를 지역 시각 HH:MM:SS 로, 해석 실패 시 자리 표시
pub fn format_clock_time(timestamp: &str) -> String {
    match DateTime::parse_from_rfc3339(timestamp) {
        Ok(parsed) => parsed.with_timezone(&Local).format("%H:%M:%S").to_string(),
        Err(_) => "--:--:--".to_string(),
    }
}

// 6. 에이전트 축약 ------------------------------------------------------------
// 마지막 # 기준 이름과 꼬리(# + 앞 4자), 이름은 width - 5 로 말줄임, # 없으면 전체 맞춤, 빈 값은 -
pub fn short_agent(agent: &str, width: usize) -> (String, String) {
    if agent.is_empty() {
        return ("-".to_string(), String::new());
    }
    let Some((name, suffix)) = agent.rsplit_once('#') else {
        return (fit_display(agent, width), String::new());
    };
    let head = fit_display(name, width.saturating_sub(5))
        .trim_end()
        .to_string();
    let tail: String = std::iter::once('#').chain(suffix.chars().take(4)).collect();
    (head, tail)
}

// 7. 모델·도구 이름 축약 ------------------------------------------------------
// 앞의 claude- 제거
pub fn short_model(model: &str) -> &str {
    model.strip_prefix("claude-").unwrap_or(model)
}

// mcp__서버__도구 -> 서버·도구 (서버 끝의 -mcp 제거), 그 밖의 이름은 그대로
pub fn short_tool(name: &str) -> String {
    let Some(rest) = name.strip_prefix("mcp__") else {
        return name.to_string();
    };
    let Some((server, tool)) = rest.split_once("__") else {
        return name.to_string();
    };
    let server = server.strip_suffix("-mcp").unwrap_or(server);
    format!("{server}·{tool}")
}

// 8. 경로형 중간 말줄임 -------------------------------------------------------
// 폭을 넘으면 앞부분과 마지막 / 또는 \ 구간만 남기고 사이를 …, 구분자가 없거나 꼬리가 길면 끝 말줄임
pub fn fit_display_middle(text: &str, width: usize) -> String {
    let total = Span::raw(text).width();
    if total <= width {
        return text.to_string();
    }
    let tail_start = text.rfind(['/', '\\']);
    let tail = tail_start.map_or("", |index| &text[index..]);
    let tail_width = Span::raw(tail).width();
    if tail.is_empty() || tail_width + 2 > width {
        return fit_display(text, width).trim_end().to_string();
    }

    // 앞부분: 남은 폭 - 말줄임 1열
    let head_limit = width - tail_width - 1;
    let mut fitted = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let mut buffer = [0u8; 4];
        let shown_width = Span::raw(&*ch.encode_utf8(&mut buffer)).width();
        if used + shown_width > head_limit {
            break;
        }
        fitted.push(ch);
        used += shown_width;
    }
    fitted.push('…');
    fitted.push_str(tail);
    fitted
}

// 9. 오른쪽 정렬 --------------------------------------------------------------
// 표시 폭 기준 오른쪽 맞춤, 넘치면 말줄임 (정확히 width 열)
pub fn pad_left_display(text: &str, width: usize) -> String {
    let total = Span::raw(text).width();
    if total > width {
        return fit_display(text, width);
    }
    format!("{}{text}", " ".repeat(width - total))
}

// 10. 리포트 목차 -------------------------------------------------------------
// 제목 줄 하나 (line = 본문 줄 번호, level = # 개수)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineEntry {
    pub line: usize,
    pub level: usize,
    pub title: String,
}

// Markdown 에서 # ~ ### 제목만 추출 (코드 울타리 안은 제외)
pub fn outline_entries(markdown: &str) -> Vec<OutlineEntry> {
    let mut entries = Vec::new();
    let mut in_fence = false;
    for (line, text) in markdown.lines().enumerate() {
        let trimmed = text.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let level = trimmed.chars().take_while(|ch| *ch == '#').count();
        if !(1..=3).contains(&level) {
            continue;
        }
        if let Some(title) = trimmed[level..].strip_prefix(' ') {
            entries.push(OutlineEntry {
                line,
                level,
                title: title.trim().to_string(),
            });
        }
    }
    entries
}

// 11. 리포트 본문 준비 --------------------------------------------------------
// 파이프 표는 박스 표로 바꾸고 text 울타리 줄은 숨김 (목차 줄 번호와 본문 줄은 이 결과가 기준)
pub fn prepare_report_markdown(markdown: &str) -> String {
    let converted = convert_pipe_tables(markdown);
    let mut kept: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let mut hidden_fence = false;
    for line in converted.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_fence {
                in_fence = false;
                if hidden_fence {
                    hidden_fence = false;
                    continue;
                }
            } else {
                in_fence = true;
                hidden_fence = trimmed.trim_start_matches('`').trim() == "text";
                if hidden_fence {
                    continue;
                }
            }
        }
        kept.push(line);
    }
    kept.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_preparation_hides_text_fences_and_converts_pipe_tables() {
        let raw = "# 제목\n\n| 모델 | 요청 |\n|---|---:|\n| sonnet | 9 |\n\n```text\n┌──┐\n│ a│\n└──┘\n```\n\n```json\n{}\n```\n";
        let prepared = prepare_report_markdown(raw);
        let lines: Vec<&str> = prepared.lines().collect();
        assert!(!prepared.contains("```text"));
        assert!(!prepared.contains('|'));
        assert!(lines.iter().any(|line| line.starts_with('┌')));
        assert!(lines.iter().any(|line| line.starts_with("│ a")));
        // text 가 아닌 울타리는 그대로
        assert!(lines.contains(&"```json"));
        assert_eq!(
            lines.iter().filter(|line| line.starts_with("```")).count(),
            2
        );

        // 목차 줄 번호는 숨긴 줄을 뺀 본문 기준
        let outline = outline_entries(&prepared);
        assert_eq!(outline.len(), 1);
        assert_eq!(outline[0].line, 0);
    }

    #[test]
    fn outline_lists_headings_with_line_numbers_and_skips_code_fences() {
        let markdown = "# 일일\n\n## 요약\n- 항목\n```\n# 주석\n```\n### 세부\n#### 널\n#태그";
        let entries = outline_entries(markdown);
        let shown: Vec<(usize, usize, &str)> = entries
            .iter()
            .map(|entry| (entry.line, entry.level, entry.title.as_str()))
            .collect();
        assert_eq!(shown, [(0, 1, "일일"), (2, 2, "요약"), (7, 3, "세부")]);
    }

    #[test]
    fn count_groups_thousands_and_keeps_sign() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(1_234_567), "1,234,567");
        assert_eq!(format_count(-12_345), "-12,345");
    }

    #[test]
    fn compact_tokens_use_suffix_with_one_decimal() {
        assert_eq!(format_compact(999), "999");
        assert_eq!(format_compact(1_500), "1.5K");
        assert_eq!(format_compact(2_340_000), "2.3M");
        assert_eq!(format_compact(7_100_000_000), "7.1B");
        assert_eq!(format_compact(-1_500), "-1.5K");
    }

    #[test]
    fn duration_switches_unit_at_boundaries() {
        assert_eq!(format_duration_ms(-5), "0ms");
        assert_eq!(format_duration_ms(850), "850ms");
        assert_eq!(format_duration_ms(1_250), "1.2s");
        assert_eq!(format_duration_ms(192_000), "3m 12s");
        assert_eq!(format_duration_ms(3_900_000), "1h 05m");
    }

    #[test]
    fn fit_pads_and_truncates_by_display_width() {
        assert_eq!(fit_display("abc", 5), "abc  ");
        assert_eq!(fit_display("abcdef", 4), "abc…");
        // 한글 한 글자는 2열
        assert_eq!(Span::raw(fit_display("도구목록", 6)).width(), 6);
        assert_eq!(fit_display("도구목록", 6), "도구… ");
        assert_eq!(fit_display("a\nb", 3), "a b");
        assert_eq!(fit_display("abc", 0), "");
    }

    #[test]
    fn clock_time_falls_back_on_bad_input() {
        assert_eq!(format_clock_time("not-a-time"), "--:--:--");
        assert_eq!(format_clock_time("2026-11-15T01:02:03Z").len(), 8);
    }

    #[test]
    fn short_agent_splits_name_and_four_char_tail() {
        assert_eq!(
            short_agent("engineer-rdr2#a0a2bf9b", 18),
            ("engineer-rdr2".to_string(), "#a0a2".to_string())
        );
        // 이름이 폭 - 5 를 넘으면 말줄임, 꼬리는 유지
        let (head, tail) = short_agent("very-long-agent-name#abcdef12", 14);
        assert_eq!(head, "very-lon…");
        assert_eq!(tail, "#abcd");
        assert!(Span::raw(head).width() + Span::raw(tail).width() <= 14);

        // # 없음, 빈 값, 짧은 꼬리
        assert_eq!(
            short_agent("main", 6),
            ("main  ".to_string(), String::new())
        );
        assert_eq!(short_agent("", 10), ("-".to_string(), String::new()));
        assert_eq!(short_agent("a#b", 18), ("a".to_string(), "#b".to_string()));
    }

    #[test]
    fn short_model_strips_only_the_claude_prefix() {
        assert_eq!(short_model("claude-sonnet-5-5"), "sonnet-5-5");
        assert_eq!(short_model("sonnet"), "sonnet");
        assert_eq!(short_model("my-claude-x"), "my-claude-x");
    }

    #[test]
    fn short_tool_compacts_mcp_names_and_keeps_others() {
        assert_eq!(
            short_tool("mcp__rust-fs-mcp__file-read"),
            "rust-fs·file-read"
        );
        assert_eq!(short_tool("mcp__outline__search"), "outline·search");
        assert_eq!(short_tool("mcp__broken"), "mcp__broken");
        assert_eq!(short_tool("Bash"), "Bash");
    }

    #[test]
    fn middle_fit_keeps_head_and_last_path_segment() {
        assert_eq!(fit_display_middle("a/b.rs", 20), "a/b.rs");
        assert_eq!(
            fit_display_middle("C:/very/long/directory/tree/file.rs", 20),
            "C:/very/lon…/file.rs"
        );
        assert_eq!(
            fit_display_middle("C:\\very\\long\\directory\\file.rs", 16),
            "C:\\very…\\file.rs"
        );
        // 구분자가 없거나 꼬리가 폭에 안 들어가면 끝 말줄임
        assert_eq!(fit_display_middle("abcdefghij", 6), "abcde…");
        assert_eq!(fit_display_middle("x/abcdefghijkl", 6), "x/abc…");
        for width in 3..30 {
            let fitted = fit_display_middle("C:/데이터/긴폴더/하위폴더/보고서.md", width);
            assert!(Span::raw(fitted.as_str()).width() <= width, "{width}");
        }
    }

    #[test]
    fn pad_left_aligns_by_display_width_and_truncates() {
        assert_eq!(pad_left_display("12", 5), "   12");
        assert_eq!(pad_left_display("한글", 6), "  한글");
        assert_eq!(pad_left_display("abcdef", 4), "abc…");
        assert_eq!(Span::raw(pad_left_display("도구목록", 6)).width(), 6);
        assert_eq!(pad_left_display("a", 0), "");
    }
}
