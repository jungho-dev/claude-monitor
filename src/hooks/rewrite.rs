//! hooks/rewrite.rs
//! Bash 명령 보수적 재작성 (cat→bat, ls→eza, 경로 없는 eza, git diff→delta 파이프).

const BAT_PREFIX: &str = "bat --plain --paging=never";
const LS_FLAGS: [&str; 10] = [
    "-l", "-a", "-la", "-al", "-lh", "-lah", "-alh", "-1", "-A", "-h",
];
const DIFF_SKIP_FLAGS: [&str; 12] = [
    "--no-index",
    "--stat",
    "--shortstat",
    "--numstat",
    "--name-only",
    "--name-status",
    "--quiet",
    "--exit-code",
    "--check",
    "--summary",
    "--no-patch",
    "-s",
];

// 1. 재작성 결과 --------------------------------------------------------------
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashRewrite {
    pub command: String,
    pub notes: Vec<String>,
}

// 2. 토큰 형태 ----------------------------------------------------------------
// 명령 전체 기준 바이트 위치, 따옴표 밖 리다이렉션 문자 포함 여부
struct Token {
    start: usize,
    end: usize,
    redirect: bool,
}

// 3. 구간 형태 ----------------------------------------------------------------
// 구분자(&&, ||, ;, |) 사이 한 구간과 뒤따르는 구분자
struct Segment {
    start: usize,
    end: usize,
    tokens: Vec<Token>,
    separator: &'static str,
}

// 4. 구간 판정 ----------------------------------------------------------------
enum SegmentVerdict {
    Keep,
    Changed { text: String, note: &'static str },
    Reject,
}

// 5. 명령 재작성 --------------------------------------------------------------
/// 변환 가능한 구간만 바꾼 새 명령. 의심스러우면 None (가드가 기존대로 판정).
pub fn rewrite_bash_command(command: &str) -> Option<BashRewrite> {
    // 1. 구간 분해
    let segments = scan_command(command)?;

    // 2. 구간별 변환
    let mut rebuilt = String::with_capacity(command.len() + 32);
    let mut notes: Vec<String> = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        let previous_separator = if index == 0 {
            ""
        } else {
            segments[index - 1].separator
        };
        match convert_segment(command, segment, previous_separator) {
            SegmentVerdict::Keep => rebuilt.push_str(&command[segment.start..segment.end]),
            SegmentVerdict::Changed { text, note } => {
                rebuilt.push_str(&text);
                if !notes.iter().any(|existing| existing == note) {
                    notes.push(note.to_string());
                }
            }
            SegmentVerdict::Reject => return None,
        }
        rebuilt.push_str(segment.separator);
    }

    // 3. 변경 여부
    if notes.is_empty() {
        return None;
    }
    Some(BashRewrite {
        command: rebuilt,
        notes,
    })
}

// 6. 명령 분해 ----------------------------------------------------------------
// 따옴표·이스케이프 인식 순회, 위험 구문은 None
fn scan_command(command: &str) -> Option<Vec<Segment>> {
    // 1. 위험 구문 거부 (bash 와 토큰 해석이 달라질 수 있는 구문 포함)
    let risky = command
        .chars()
        .any(|c| c.is_whitespace() && c != ' ' && c != '\t')
        || command.contains("<<")
        || command.contains(">|")
        || command.contains('`')
        || command.contains("$(")
        || command.contains("<(")
        || command.contains(">(")
        || command.contains("$'")
        || command.contains("${")
        || command.contains("[[");
    if risky {
        return None;
    }

    // 2. 문자 순회
    let chars: Vec<(usize, char)> = command.char_indices().collect();
    let mut segments: Vec<Segment> = Vec::new();
    let mut tokens: Vec<Token> = Vec::new();
    let mut segment_start = 0usize;
    let mut token_start: Option<usize> = None;
    let mut token_redirect = false;
    let mut after_redirect_op = false;
    let mut in_single = false;
    let mut in_double = false;
    let mut index = 0usize;
    while index < chars.len() {
        let (pos, ch) = chars[index];
        let follows_redirect_op = std::mem::replace(&mut after_redirect_op, false);

        // 2-1. 따옴표 안
        if in_single {
            if ch == '\'' {
                in_single = false;
            }
            index += 1;
            continue;
        }
        if in_double {
            if ch == '\\' {
                index += 2;
                continue;
            }
            if ch == '"' {
                in_double = false;
            }
            index += 1;
            continue;
        }

        // 2-2. 공백 (토큰 종료)
        if ch.is_whitespace() {
            if let Some(start) = token_start.take() {
                tokens.push(Token {
                    start,
                    end: pos,
                    redirect: token_redirect,
                });
                token_redirect = false;
            }
            index += 1;
            continue;
        }

        // 2-3. 구분자 (구간 종료)
        let next_char = chars.get(index + 1).map(|(_, next)| *next);
        let separator: Option<&'static str> = match (ch, next_char) {
            (';', _) => Some(";"),
            ('|', Some('|')) => Some("||"),
            ('|', _) => Some("|"),
            ('&', Some('&')) => Some("&&"),
            _ => None,
        };
        if let Some(separator) = separator {
            if let Some(start) = token_start.take() {
                tokens.push(Token {
                    start,
                    end: pos,
                    redirect: token_redirect,
                });
                token_redirect = false;
            }
            segments.push(Segment {
                start: segment_start,
                end: pos,
                tokens: std::mem::take(&mut tokens),
                separator,
            });
            segment_start = pos + separator.len();
            index += separator.len();
            continue;
        }

        // 2-4. 토큰 문자
        if token_start.is_none() {
            if ch == '#' {
                return None;
            }
            token_start = Some(pos);
        }
        if !ch.is_ascii() {
            return None;
        }
        match ch {
            '\'' => in_single = true,
            '"' => in_double = true,
            '{' | '}' => return None,
            '\\' => {
                if index + 1 >= chars.len() {
                    return None;
                }
                index += 1;
            }
            '<' | '>' => {
                token_redirect = true;
                after_redirect_op = true;
            }
            '(' | ')' => return None,
            '&' if !follows_redirect_op => return None,
            _ => {}
        }
        index += 1;
    }

    // 3. 닫힘 검사와 마지막 구간
    if in_single || in_double {
        return None;
    }
    if let Some(start) = token_start.take() {
        tokens.push(Token {
            start,
            end: command.len(),
            redirect: token_redirect,
        });
    }
    segments.push(Segment {
        start: segment_start,
        end: command.len(),
        tokens,
        separator: "",
    });
    Some(segments)
}

// 7. 구간 변환 분기 -----------------------------------------------------------
fn convert_segment(command: &str, segment: &Segment, previous_separator: &str) -> SegmentVerdict {
    // 1. 빈 구간
    let Some(first) = segment.tokens.first() else {
        return SegmentVerdict::Keep;
    };
    let head = &command[first.start..first.end];

    // 2. 선행 VAR=val 대입
    if let Some(equals) = head.find('=') {
        let name = &head[..equals];
        let valid_name = !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if valid_name {
            return SegmentVerdict::Keep;
        }
    }

    // 3. 명령별 규칙
    match head {
        "case" => SegmentVerdict::Reject,
        "cat" => convert_cat(command, segment),
        "ls" => convert_ls(command, segment),
        "eza" => convert_eza(command, segment),
        "git" => convert_git_diff(command, segment, previous_separator),
        _ => SegmentVerdict::Keep,
    }
}

// 8. cat 변환 -----------------------------------------------------------------
fn convert_cat(command: &str, segment: &Segment) -> SegmentVerdict {
    // 1. 인자 검사
    let arguments = &segment.tokens[1..];
    if arguments.is_empty() {
        return SegmentVerdict::Reject;
    }
    for token in arguments {
        let value = unquote_token(&command[token.start..token.end]);
        if token.redirect || value.starts_with('-') {
            return SegmentVerdict::Reject;
        }
    }

    // 2. 명령 이름만 교체 (나머지 간격 보존)
    let first = &segment.tokens[0];
    let text = format!(
        "{}{}{}",
        &command[segment.start..first.start],
        BAT_PREFIX,
        &command[first.end..segment.end]
    );
    SegmentVerdict::Changed {
        text,
        note: "cat → bat --plain --paging=never",
    }
}

// 9. ls 변환 ------------------------------------------------------------------
fn convert_ls(command: &str, segment: &Segment) -> SegmentVerdict {
    // 1. 플래그·경로 분리
    let mut flags: Vec<String> = Vec::new();
    let mut paths: Vec<&str> = Vec::new();
    for token in &segment.tokens[1..] {
        if token.redirect {
            return SegmentVerdict::Reject;
        }
        let raw = &command[token.start..token.end];
        let value = unquote_token(raw);
        if !value.starts_with('-') {
            paths.push(raw);
            continue;
        }
        if !LS_FLAGS.contains(&value.as_str()) {
            return SegmentVerdict::Reject;
        }
        let kept: String = value[1..]
            .chars()
            .filter(|flag| *flag != 'h')
            .map(|flag| if flag == 'A' { 'a' } else { flag })
            .collect();
        if !kept.is_empty() {
            flags.push(format!("-{kept}"));
        }
    }

    // 2. eza 명령 조립 (경로 없으면 현재 폴더)
    let mut words: Vec<String> = vec!["eza".to_string()];
    words.extend(flags);
    if paths.is_empty() {
        words.push(".".to_string());
    } else {
        words.extend(paths.iter().map(|path| (*path).to_string()));
    }
    let first = &segment.tokens[0];
    let last = &segment.tokens[segment.tokens.len() - 1];
    let text = format!(
        "{}{}{}",
        &command[segment.start..first.start],
        words.join(" "),
        &command[last.end..segment.end]
    );
    SegmentVerdict::Changed {
        text,
        note: "ls → eza",
    }
}

// 10. eza 경로 보강 -----------------------------------------------------------
fn convert_eza(command: &str, segment: &Segment) -> SegmentVerdict {
    // 1. 플래그만 있는 구간 확인
    for token in &segment.tokens[1..] {
        let value = unquote_token(&command[token.start..token.end]);
        if token.redirect || !value.starts_with('-') {
            return SegmentVerdict::Keep;
        }
    }

    // 2. 현재 폴더 경로 추가
    let last = &segment.tokens[segment.tokens.len() - 1];
    let text = format!(
        "{} .{}",
        &command[segment.start..last.end],
        &command[last.end..segment.end]
    );
    SegmentVerdict::Changed {
        text,
        note: "eza → eza .",
    }
}

// 11. git diff 파이프 ---------------------------------------------------------
fn convert_git_diff(command: &str, segment: &Segment, previous_separator: &str) -> SegmentVerdict {
    // 1. git diff 구간 확인
    let is_diff = segment
        .tokens
        .get(1)
        .is_some_and(|token| &command[token.start..token.end] == "diff");
    let controls_flow = segment.separator == "&&" || segment.separator == "||";
    if !is_diff || previous_separator == "|" || segment.separator == "|" || controls_flow {
        return SegmentVerdict::Keep;
    }

    // 2. 리다이렉션·요약·비교 옵션이면 유지
    for token in &segment.tokens {
        let value = unquote_token(&command[token.start..token.end]);
        let skip_flag = DIFF_SKIP_FLAGS.iter().any(|flag| {
            value == *flag
                || value
                    .strip_prefix(flag)
                    .is_some_and(|rest| rest.starts_with('='))
        });
        if token.redirect || skip_flag {
            return SegmentVerdict::Keep;
        }
    }

    // 3. delta 파이프 추가
    let last = &segment.tokens[segment.tokens.len() - 1];
    let text = format!(
        "{} | delta{}",
        &command[segment.start..last.end],
        &command[last.end..segment.end]
    );
    SegmentVerdict::Changed {
        text,
        note: "git diff → git diff | delta",
    }
}

// 12. 토큰 값 복원 ------------------------------------------------------------
// 따옴표·역슬래시를 벗긴 bash 인자 값 (플래그 판정용)
fn unquote_token(raw: &str) -> String {
    let mut value = String::with_capacity(raw.len());
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                value.push(ch);
            }
            continue;
        }
        match ch {
            '\'' if !in_double => in_single = true,
            '"' => in_double = !in_double,
            '\\' => match chars.next() {
                Some(escaped) if in_double && !matches!(escaped, '"' | '\\' | '$' | '`') => {
                    value.push('\\');
                    value.push(escaped);
                }
                Some(escaped) => value.push(escaped),
                None => value.push('\\'),
            },
            _ => value.push(ch),
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    // 1. 변환 표 --------------------------------------------------------------
    #[test]
    fn rewrite_table_matches_expected_commands() {
        let table: Vec<(&str, Option<&str>)> = vec![
            // cat 변환
            (
                "cat README.md",
                Some("bat --plain --paging=never README.md"),
            ),
            (
                "cat a.txt b.txt",
                Some("bat --plain --paging=never a.txt b.txt"),
            ),
            (
                "cat \"my file.txt\"",
                Some("bat --plain --paging=never \"my file.txt\""),
            ),
            ("cat 'a b' c", Some("bat --plain --paging=never 'a b' c")),
            (
                "cat   spaced.txt  ",
                Some("bat --plain --paging=never   spaced.txt  "),
            ),
            (
                "cat file | grep x",
                Some("bat --plain --paging=never file | grep x"),
            ),
            (
                "cd src && cat a.rs",
                Some("cd src && bat --plain --paging=never a.rs"),
            ),
            (
                "cat a.rs; cat b.rs",
                Some("bat --plain --paging=never a.rs; bat --plain --paging=never b.rs"),
            ),
            // cat 거부
            ("cat -n file", None),
            ("cat \"-n\" file", None),
            ("cat -", None),
            ("cat", None),
            ("cat file > out.txt", None),
            ("cat < in.txt", None),
            ("cat file 2>&1", None),
            ("cat a # memo", None),
            ("echo x | cat", None),
            // ls 변환
            ("ls", Some("eza .")),
            ("ls -la", Some("eza -la .")),
            ("ls -lah src", Some("eza -la src")),
            ("ls -A", Some("eza -a .")),
            ("ls -h", Some("eza .")),
            ("ls -1 docs", Some("eza -1 docs")),
            ("ls src tests", Some("eza src tests")),
            ("ls \"my dir\"", Some("eza \"my dir\"")),
            ("ls -l | head", Some("eza -l . | head")),
            // ls 거부
            ("ls -R", None),
            ("ls -lt", None),
            ("ls > out.txt", None),
            ("ls -l; cat -n x", None),
            // eza 경로 보강
            ("eza", Some("eza .")),
            ("eza -la", Some("eza -la .")),
            ("eza -T | head", Some("eza -T . | head")),
            // eza 유지
            ("eza -la .", None),
            ("eza src", None),
            ("eza -l src", None),
            // git diff 파이프
            ("git diff", Some("git diff | delta")),
            (
                "git diff HEAD~1 -- src/main.rs",
                Some("git diff HEAD~1 -- src/main.rs | delta"),
            ),
            ("git diff; echo ok", Some("git diff | delta; echo ok")),
            ("echo ok && git diff", Some("echo ok && git diff | delta")),
            (
                "git diff --name-only && cat a",
                Some("git diff --name-only && bat --plain --paging=never a"),
            ),
            // git diff 제어 흐름 유지 (&&·|| 직전, --no-index)
            ("git diff && echo ok", None),
            ("git diff || echo fail", None),
            ("git diff --no-index a b", None),
            ("git diff a b --no-index", None),
            ("git diff \"--no-index\" a b", None),
            ("git diff --no-index a b && echo same || echo differ", None),
            ("git diff \"--stat\"", None),
            (
                "git diff && cat a",
                Some("git diff && bat --plain --paging=never a"),
            ),
            // git diff 유지
            ("git diff --stat", None),
            ("git diff --name-status HEAD", None),
            ("git diff -s", None),
            ("git diff --stat=80", None),
            ("git diff | delta", None),
            ("git diff | head", None),
            ("git diff > p.patch", None),
            ("git status", None),
            // 건드리지 않는 명령
            ("grep foo a.txt", None),
            ("find . -name x", None),
            ("du -sh .", None),
            ("bat --plain a.txt", None),
            ("xargs cat", None),
            // 대입·따옴표 안 구분자
            ("FOO=1 cat file", None),
            ("echo \"a && cat b\"", None),
            ("echo 'x; ls' && ls", Some("echo 'x; ls' && eza .")),
            // 위험 구문 거부
            ("cat <<EOF", None),
            ("cat <<< \"x\"", None),
            ("echo $(cat a)", None),
            ("echo `cat a`", None),
            ("cat <(ls)", None),
            ("cat \"a", None),
            ("cat 'a", None),
            ("cat a\nls", None),
            ("cat a & ls", None),
            ("(cat a)", None),
            ("cat a\\", None),
            // 이스케이프된 리다이렉션 문자 뒤 백그라운드 &
            ("git diff a\\>&echo hi", None),
            ("git diff a\\<&echo hi", None),
            ("git diff -- x\\>&ls -la", None),
            ("eza -l\\>&echo -n", None),
            ("ls -l a\\>&echo -la", None),
            ("cat a\\>&cat b", None),
            // 리다이렉션 fd 복제는 구간 유지
            ("git diff a>&2", None),
            ("ls 2>&1", None),
            // noclobber 무시 리다이렉션 거부
            ("echo hi >|cat a", None),
            ("printf x >| ls", None),
            ("git diff >|out", None),
            // 연쇄
            (
                "cat a || ls -la",
                Some("bat --plain --paging=never a || eza -la ."),
            ),
            // bash 와 토큰 해석이 다른 구문 거부
            ("cat $'a\\' ; ls ; \\'' \\'", None),
            ("echo ${x:-a; ls -l }", None),
            ("echo \"${x}\" && ls", None),
            ("ls\u{a0}foo", None),
            ("ls\u{a0}", None),
            ("ls foo\u{2003}bar", None),
            ("ls \"-s\" src", None),
            ("ls '-R'", None),
            ("cat 한글.txt", None),
            ("ls 폴더", None),
            ("[[ -f a ]] && ls", None),
            ("cat a && case x in y", None),
            ("{ ls; }", None),
            ("ls {a,b}", None),
            ("cat a; }", None),
            // 따옴표 안 비ASCII·중괄호는 변환 유지
            (
                "cat \"한글.txt\"",
                Some("bat --plain --paging=never \"한글.txt\""),
            ),
            ("echo \"{\" && ls", Some("echo \"{\" && eza .")),
            ("ls \"-la\"", Some("eza -la .")),
        ];
        assert!(table.len() >= 30);
        for (input, expected) in table {
            let actual = rewrite_bash_command(input).map(|rewrite| rewrite.command);
            assert_eq!(actual.as_deref(), expected, "입력: {input:?}");
        }
    }

    // 2. 변환 메모 ------------------------------------------------------------
    #[test]
    fn rewrite_notes_name_each_changed_rule_once() {
        let rewrite = rewrite_bash_command("cat a; cat b && ls && git diff").expect("변환 기대");
        assert_eq!(
            rewrite.notes,
            vec![
                "cat → bat --plain --paging=never".to_string(),
                "ls → eza".to_string(),
                "git diff → git diff | delta".to_string(),
            ]
        );
    }

    // 3. 재변환 안정성 --------------------------------------------------------
    #[test]
    fn rewritten_command_needs_no_second_rewrite() {
        let inputs = [
            "cat a.txt",
            "ls -lah",
            "eza -la",
            "git diff HEAD",
            "cd x && cat y",
        ];
        for input in inputs {
            let first = rewrite_bash_command(input).expect("변환 기대");
            assert!(
                rewrite_bash_command(&first.command).is_none(),
                "재변환 발생: {}",
                first.command
            );
        }
    }
}
