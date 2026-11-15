//! report/table.rs
//! 가벼운 선 박스 표 렌더러(한글 표시 폭 2)와 GFM 파이프 표의 박스 표 변환.

use unicode_width::UnicodeWidthStr;

const ORDINAL_HEADERS: [&str; 7] = ["#", "no", "no.", "seq", "순서", "순번", "번호"];
const MARKER_CHARS: &str = "●○△◎✓✔✗✘×";
const NUMBER_CHARS: &str = " ,.+-%()msKMGBUSDp초분시간건회개";
const TIME_CHARS: &str = "0123456789:.~- TZ+";

// 1. 열 정렬 ------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnAlign {
    Left,
    Center,
    Right,
}

// 2. 칸 채움 ------------------------------------------------------------------
// 정렬별 칸 채움
fn pad_cell(text: &str, width: usize, align: ColumnAlign) -> String {
    let gap = width.saturating_sub(2 + text.width());
    match align {
        ColumnAlign::Right => format!(" {}{text} ", " ".repeat(gap)),
        ColumnAlign::Center => {
            let left_gap = gap / 2;
            format!(
                " {}{text}{} ",
                " ".repeat(left_gap),
                " ".repeat(gap - left_gap)
            )
        }
        ColumnAlign::Left => format!(" {text}{} ", " ".repeat(gap)),
    }
}

// 3. 박스 표 그리기 -----------------------------------------------------------
// 행마다 구분선을 둔 박스 표
pub fn render_box_table(
    headers: &[&str],
    aligns: &[ColumnAlign],
    rows: &[Vec<String>],
) -> Vec<String> {
    // 1. 셀 정리
    let column_count = headers.len();
    if column_count == 0 {
        return Vec::new();
    }
    let clean_cell = |text: &str| -> String {
        text.replace(['\r', '\n'], " ")
            .replace('`', "")
            .replace("**", "")
            .replace("\\|", "|")
            .trim()
            .to_string()
    };
    let header_cells: Vec<String> = headers.iter().map(|text| clean_cell(text)).collect();
    let body_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            (0..column_count)
                .map(|column| {
                    row.get(column)
                        .map(|text| clean_cell(text))
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect();

    // 2. 열 너비
    let widths: Vec<usize> = (0..column_count)
        .map(|column| {
            let body_width = body_rows
                .iter()
                .map(|row| row[column].width())
                .max()
                .unwrap_or(0);
            header_cells[column].width().max(body_width) + 2
        })
        .collect();
    let build_rule = |left: &str, joint: &str, right: &str| -> String {
        let spans: Vec<String> = widths.iter().map(|width| "─".repeat(*width)).collect();
        format!("{left}{}{right}", spans.join(joint))
    };
    let build_row = |cells: &[String], cell_aligns: &[ColumnAlign]| -> String {
        let padded: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(column, cell)| pad_cell(cell, widths[column], cell_aligns[column]))
            .collect();
        format!("│{}│", padded.join("│"))
    };

    // 3. 줄 조립
    let body_aligns: Vec<ColumnAlign> = (0..column_count)
        .map(|column| aligns.get(column).copied().unwrap_or(ColumnAlign::Left))
        .collect();
    let header_aligns = vec![ColumnAlign::Center; column_count];
    let mut lines = vec![
        build_rule("┌", "┬", "┐"),
        build_row(&header_cells, &header_aligns),
    ];
    for row in &body_rows {
        lines.push(build_rule("├", "┼", "┤"));
        lines.push(build_row(row, &body_aligns));
    }
    lines.push(build_rule("└", "┴", "┘"));
    lines
}

// 4. 파이프 행 분해 -----------------------------------------------------------
// 이스케이프 파이프 유지
fn split_pipe_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let body = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let mut cells: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut last_was_separator = false;
    let mut chars = body.chars().peekable();
    while let Some(letter) = chars.next() {
        last_was_separator = false;
        if letter == '\\' && chars.peek() == Some(&'|') {
            current.push('|');
            chars.next();
        } else if letter == '|' {
            cells.push(current.trim().to_string());
            current.clear();
            last_was_separator = true;
        } else {
            current.push(letter);
        }
    }
    if !(last_was_separator && current.is_empty()) {
        cells.push(current.trim().to_string());
    }
    cells
}

// 5. 구분 행 해석 -------------------------------------------------------------
// 열별 명시 정렬
fn parse_delimiter_aligns(cells: &[String]) -> Option<Vec<Option<ColumnAlign>>> {
    if cells.is_empty() {
        return None;
    }
    let mut parsed: Vec<Option<ColumnAlign>> = Vec::with_capacity(cells.len());
    for cell in cells {
        let after_left = cell.strip_prefix(':');
        let after_right = after_left.unwrap_or(cell).strip_suffix(':');
        let core = after_right.or(after_left).unwrap_or(cell);
        if core.is_empty() || !core.chars().all(|dash| dash == '-') {
            return None;
        }
        parsed.push(match (after_left.is_some(), after_right.is_some()) {
            (true, true) => Some(ColumnAlign::Center),
            (false, true) => Some(ColumnAlign::Right),
            (true, false) => Some(ColumnAlign::Left),
            (false, false) => None,
        });
    }
    Some(parsed)
}

// 6. 열 정렬 추정 -------------------------------------------------------------
// 내용 기반 정렬 추정
fn infer_column_align(header: &str, cells: &[&str]) -> ColumnAlign {
    if ORDINAL_HEADERS.contains(&header.trim().to_lowercase().as_str()) {
        return ColumnAlign::Center;
    }
    let filled: Vec<&str> = cells
        .iter()
        .map(|cell| cell.trim())
        .filter(|cell| !cell.is_empty() && *cell != "-")
        .collect();
    if filled.is_empty() {
        return ColumnAlign::Left;
    }
    let centered = filled.iter().all(|cell| {
        let bytes = cell.as_bytes();
        let is_marker = cell.chars().count() == 1
            && cell
                .chars()
                .all(|mark| MARKER_CHARS.contains(mark) || mark == 'Y' || mark == 'N');
        let is_date = bytes.len() >= 7
            && bytes[..4].iter().all(u8::is_ascii_digit)
            && b"-./".contains(&bytes[4])
            && bytes[5..7].iter().all(u8::is_ascii_digit);
        let is_time = cell.contains(':') && cell.chars().all(|part| TIME_CHARS.contains(part));
        is_marker || is_date || is_time
    });
    if centered {
        return ColumnAlign::Center;
    }
    let numeric = filled.iter().all(|cell| {
        let digits = cell.trim_start_matches(['+', '-']);
        digits.starts_with(|first: char| first.is_ascii_digit())
            && cell
                .chars()
                .all(|part| part.is_ascii_digit() || NUMBER_CHARS.contains(part))
    });
    if numeric {
        return ColumnAlign::Right;
    }
    ColumnAlign::Left
}

// 7. 파이프 표 변환 -----------------------------------------------------------
// 파이프 표의 박스 표 변환
pub fn convert_pipe_tables(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut output: Vec<String> = Vec::with_capacity(lines.len());
    let mut open_fence: Option<(char, usize)> = None;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let plain = line.trim_end_matches('\r');
        let trimmed = plain.trim_start();

        // 1. 코드 펜스 추적
        let fence_mark = trimmed
            .chars()
            .next()
            .filter(|mark| *mark == '`' || *mark == '~');
        if let Some(mark) = fence_mark {
            let run = trimmed.chars().take_while(|part| *part == mark).count();
            if run >= 3 {
                match open_fence {
                    None => open_fence = Some((mark, run)),
                    Some((open_mark, open_run))
                        if open_mark == mark
                            && run >= open_run
                            && trimmed[run..].trim().is_empty() =>
                    {
                        open_fence = None;
                    }
                    Some(_) => {}
                }
                output.push(line.to_string());
                index += 1;
                continue;
            }
        }
        if open_fence.is_some() {
            output.push(line.to_string());
            index += 1;
            continue;
        }

        // 2. 표 시작 판정
        let next_plain = lines
            .get(index + 1)
            .map(|next| next.trim_end_matches('\r'))
            .unwrap_or("");
        let header_cells = split_pipe_cells(plain);
        let delimiter_aligns = if plain.contains('|') && next_plain.contains('|') {
            parse_delimiter_aligns(&split_pipe_cells(next_plain))
        } else {
            None
        };
        let Some(explicit_aligns) =
            delimiter_aligns.filter(|found| found.len() == header_cells.len())
        else {
            output.push(line.to_string());
            index += 1;
            continue;
        };

        // 3. 본문 행 수집
        let column_count = header_cells.len();
        let mut body_rows: Vec<Vec<String>> = Vec::new();
        let mut cursor = index + 2;
        while cursor < lines.len() {
            let body_plain = lines[cursor].trim_end_matches('\r');
            let body_trimmed = body_plain.trim();
            if body_trimmed.is_empty()
                || !body_trimmed.contains('|')
                || body_trimmed.starts_with("```")
                || body_trimmed.starts_with("~~~")
            {
                break;
            }
            let mut cells = split_pipe_cells(body_plain);
            cells.resize(column_count, String::new());
            body_rows.push(cells);
            cursor += 1;
        }

        // 4. 정렬 결정
        let aligns: Vec<ColumnAlign> = (0..column_count)
            .map(|column| match explicit_aligns[column] {
                Some(explicit) => explicit,
                None => {
                    let column_cells: Vec<&str> =
                        body_rows.iter().map(|row| row[column].as_str()).collect();
                    infer_column_align(&header_cells[column], &column_cells)
                }
            })
            .collect();

        // 5. 박스 표 기록
        let indent = &plain[..plain.len() - trimmed.len()];
        let header_texts: Vec<&str> = header_cells.iter().map(String::as_str).collect();
        output.push(format!("{indent}```text"));
        for table_line in render_box_table(&header_texts, &aligns, &body_rows) {
            output.push(format!("{indent}{table_line}"));
        }
        output.push(format!("{indent}```"));
        index = cursor;
    }
    output.join("\n")
}

// 8. 단위 시험 ----------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn rows_of(raw: &[&[&str]]) -> Vec<Vec<String>> {
        raw.iter()
            .map(|row| row.iter().map(|cell| cell.to_string()).collect())
            .collect()
    }

    #[test]
    fn renders_ascii_table_like_script() {
        let lines = render_box_table(
            &["Name", "Count", "Ratio"],
            &[ColumnAlign::Left, ColumnAlign::Right, ColumnAlign::Right],
            &rows_of(&[
                &["alpha", "12", "50.0%"],
                &["beta-long", "1,234", "8.5%"],
                &["g", "7", "100.0%"],
            ]),
        );
        let expected = "\
┌───────────┬───────┬────────┐
│   Name    │ Count │ Ratio  │
├───────────┼───────┼────────┤
│ alpha     │    12 │  50.0% │
├───────────┼───────┼────────┤
│ beta-long │ 1,234 │   8.5% │
├───────────┼───────┼────────┤
│ g         │     7 │ 100.0% │
└───────────┴───────┴────────┘";
        assert_eq!(lines.join("\n"), expected);
    }

    #[test]
    fn renders_hangul_table_like_script() {
        let lines = render_box_table(
            &["순서", "모델", "요청", "캐시 적중률"],
            &[
                ColumnAlign::Center,
                ColumnAlign::Left,
                ColumnAlign::Right,
                ColumnAlign::Right,
            ],
            &rows_of(&[
                &["1", "claude-opus-5-5", "1,234", "93.5%"],
                &["2", "한글 모델", "56", "8.0%"],
                &["10", "sonnet", "7", "-"],
            ]),
        );
        let expected = "\
┌──────┬─────────────────┬───────┬─────────────┐
│ 순서 │      모델       │ 요청  │ 캐시 적중률 │
├──────┼─────────────────┼───────┼─────────────┤
│  1   │ claude-opus-5-5 │ 1,234 │       93.5% │
├──────┼─────────────────┼───────┼─────────────┤
│  2   │ 한글 모델       │    56 │        8.0% │
├──────┼─────────────────┼───────┼─────────────┤
│  10  │ sonnet          │     7 │           - │
└──────┴─────────────────┴───────┴─────────────┘";
        assert_eq!(lines.join("\n"), expected);
    }

    #[test]
    fn renders_markers_odd_gap_and_empty_cell_like_script() {
        let lines = render_box_table(
            &["표식", "시각", "상세"],
            &[ColumnAlign::Center, ColumnAlign::Center, ColumnAlign::Left],
            &rows_of(&[
                &["●", "12:00:01", "가드 cli-guard"],
                &["○", "9:5:1", ""],
                &["△", "2026-11-10", "a"],
            ]),
        );
        let expected = "\
┌──────┬────────────┬────────────────┐
│ 표식 │    시각    │      상세      │
├──────┼────────────┼────────────────┤
│  ●   │  12:00:01  │ 가드 cli-guard │
├──────┼────────────┼────────────────┤
│  ○   │   9:5:1    │                │
├──────┼────────────┼────────────────┤
│  △   │ 2026-11-10 │ a              │
└──────┴────────────┴────────────────┘";
        assert_eq!(lines.join("\n"), expected);
    }

    #[test]
    fn strips_markdown_and_tolerates_ragged_rows() {
        let lines = render_box_table(
            &["항목", "값"],
            &[ColumnAlign::Left],
            &rows_of(&[&["`code` **bold**"], &["a\nb", "x", "extra"]]),
        );
        assert_eq!(lines[3], "│ code bold │    │");
        assert_eq!(lines[5], "│ a b       │ x  │");
        assert!(render_box_table(&[], &[], &[]).is_empty());
    }

    #[test]
    fn converts_pipe_table_with_inferred_alignment() {
        let markdown = "# 제목\n\n| 항목 | 건수 | 시각 | 비고 |\n| --- | ---: | :---: | :--- |\n| 가 | 12 | 09:00 | x |\n| 나나 | 1,234 | 10:30 | 긴 설명 |\n\n끝\n";
        let converted = convert_pipe_tables(markdown);
        let expected = "# 제목\n\n```text\n\
┌──────┬───────┬───────┬─────────┐\n\
│ 항목 │ 건수  │ 시각  │  비고   │\n\
├──────┼───────┼───────┼─────────┤\n\
│ 가   │    12 │ 09:00 │ x       │\n\
├──────┼───────┼───────┼─────────┤\n\
│ 나나 │ 1,234 │ 10:30 │ 긴 설명 │\n\
└──────┴───────┴───────┴─────────┘\n\
```\n\n끝\n";
        assert_eq!(converted, expected);
    }

    #[test]
    fn infers_alignment_from_content_without_markers() {
        let markdown = "| # | 이름 | 수량 | 일자 | 처리 |\n| --- | --- | --- | --- | --- |\n| 1 | a | 3 | 2026-11-10 | 1.5초 |\n| 2 | b | 40 | 2026-11-11 | - |\n";
        let converted = convert_pipe_tables(markdown);
        let table_lines: Vec<&str> = converted.lines().collect();
        assert_eq!(table_lines[2], "│ # │ 이름 │ 수량 │    일자    │ 처리  │");
        assert_eq!(table_lines[4], "│ 1 │ a    │    3 │ 2026-11-10 │ 1.5초 │");
        assert_eq!(table_lines[6], "│ 2 │ b    │   40 │ 2026-11-11 │     - │");
    }

    #[test]
    fn leaves_fences_headings_and_plain_pipes_untouched() {
        let markdown =
            "제목 | 설명\n---\n\n```md\n| a | b |\n| - | - |\n| 1 | 2 |\n```\n\n- 항목 | 구분\n";
        assert_eq!(convert_pipe_tables(markdown), markdown);
    }

    #[test]
    fn keeps_indent_and_crlf_outside_tables() {
        let markdown = "머리\r\n  | a | b |\r\n  | --- | --- |\r\n  | 1 | 2 |\r\n꼬리\r\n";
        let converted = convert_pipe_tables(markdown);
        assert!(converted.starts_with("머리\r\n  ```text\n  ┌"));
        assert!(converted.ends_with("  ```\n꼬리\r\n"));
    }
}
