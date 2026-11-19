//! statusline/tests.rs
//! statusLine 명령 시험: 지표 추출, 한 줄 형식, 감싸기, 전송 예산.

use super::{extract_statusline_snapshot, format_statusline_segment, run_statusline_with};
use crate::store::limits::LimitSnapshot;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

// 구독자 형태 (rate_limits 있음, 경로·작업 폴더·PR 등 저장 금지 필드 포함)
const SUBSCRIBER_JSON: &str = r#"{"cwd":"C:/secret/project","session_id":"sess-sub","session_name":"my-private-session","transcript_path":"C:/secret/transcript.jsonl","model":{"id":"claude-opus-5-5","display_name":"Opus"},"workspace":{"current_dir":"C:/secret/project","project_dir":"C:/secret/project","repo":{"host":"github.com","owner":"someone","name":"private-repo"}},"cost":{"total_cost_usd":0.01234},"context_window":{"total_input_tokens":15500,"total_output_tokens":1200,"context_window_size":200000,"used_percentage":8,"remaining_percentage":92,"current_usage":{"input_tokens":8500,"output_tokens":1200,"cache_creation_input_tokens":5000,"cache_read_input_tokens":2000}},"prompt_cache":{"warm":true,"caching_observed":true,"ttl":"1h","expires_at":1738429200,"requests":14,"misses":2,"hit_ratio":0.91,"last_miss_at":1738425230,"last_miss_cause":{"causes":["tools_changed","likely_server_side"],"tools_added":2}},"rate_limits":{"five_hour":{"used_percentage":23.5,"resets_at":1738425600},"seven_day":{"used_percentage":41.2,"resets_at":1738857600}},"pr":{"number":7,"url":"https://example.invalid/pr/7"},"worktree":{"path":"C:/secret/wt"}}"#;

// API 키 형태 (rate_limits·prompt_cache 없음, 압축 직후 current_usage null)
const API_KEY_JSON: &str = r#"{"session_id":"sess-key","model":{"id":"claude-sonnet-5-5"},"cwd":"C:/secret/project","context_window":{"total_input_tokens":0,"total_output_tokens":0,"context_window_size":200000,"used_percentage":null,"remaining_percentage":null,"current_usage":null}}"#;

const CHILD_FILTER: &str = "statusline::tests::wrap_child_helper";

// 1. 구독자 지표 추출 ---------------------------------------------------------
#[test]
fn subscriber_payload_extracts_metrics_without_paths() {
    // 1. 지표 값
    let snapshot = extract_statusline_snapshot(SUBSCRIBER_JSON.as_bytes()).expect("지표 있음");
    assert_eq!(snapshot.session_id, "sess-sub");
    assert_eq!(snapshot.model, "claude-opus-5-5");
    assert_eq!(snapshot.five_hour_pct, Some(23.5));
    assert_eq!(snapshot.five_hour_resets_at, Some(1_738_425_600));
    assert_eq!(snapshot.seven_day_pct, Some(41.2));
    assert_eq!(snapshot.seven_day_resets_at, Some(1_738_857_600));
    assert_eq!(snapshot.context_pct, Some(8.0));
    assert_eq!(snapshot.context_window_size, Some(200_000));
    assert_eq!(snapshot.context_input_tokens, Some(15_500));
    assert_eq!(snapshot.context_cache_read_tokens, Some(2_000));
    assert_eq!(snapshot.context_cache_write_tokens, Some(5_000));
    assert_eq!(snapshot.cache_warm, Some(true));
    assert_eq!(snapshot.cache_ttl.as_deref(), Some("1h"));
    assert_eq!(snapshot.cache_expires_at, Some(1_738_429_200));
    assert_eq!(snapshot.cache_requests, Some(14));
    assert_eq!(snapshot.cache_misses, Some(2));
    assert_eq!(snapshot.cache_hit_ratio, Some(0.91));
    assert_eq!(snapshot.cache_last_miss_at, Some(1_738_425_230));
    assert_eq!(
        snapshot.cache_last_miss_cause.as_deref(),
        Some("tools_changed,likely_server_side")
    );
    assert_eq!(snapshot.session_cost_usd, Some(0.01234));

    // 2. 직렬화 본문에 경로·이름·저장소 정보 없음
    let payload = serde_json::to_string(&snapshot).expect("직렬화");
    for forbidden in [
        "secret",
        "transcript",
        "my-private-session",
        "private-repo",
        "someone",
        "example.invalid",
        "worktree",
    ] {
        assert!(!payload.contains(forbidden), "{forbidden}: {payload}");
    }
}

// 2. API 키 형태 추출 ---------------------------------------------------------
#[test]
fn api_key_payload_has_no_limits_and_null_usage_is_none() {
    // 1. 한도·캐시 없음, null 사용률·사용량은 None (줄 출력 없음)
    let snapshot = extract_statusline_snapshot(API_KEY_JSON.as_bytes()).expect("크기 있음");
    assert_eq!(snapshot.session_id, "sess-key");
    assert_eq!(snapshot.five_hour_pct, None);
    assert_eq!(snapshot.context_pct, None);
    assert_eq!(snapshot.context_cache_read_tokens, None);
    assert_eq!(snapshot.context_window_size, Some(200_000));
    assert_eq!(format_statusline_segment(&snapshot), "");
    // 2. 컨텍스트 값이 생기면 한도 없이도 추출
    let with_context = API_KEY_JSON.replace("\"used_percentage\":null", "\"used_percentage\":63.4");
    let snapshot = extract_statusline_snapshot(with_context.as_bytes()).expect("컨텍스트 있음");
    assert_eq!(snapshot.five_hour_pct, None);
    assert_eq!(snapshot.seven_day_pct, None);
    assert_eq!(snapshot.context_pct, Some(63.4));
    assert_eq!(snapshot.context_cache_read_tokens, None);
    assert_eq!(snapshot.cache_warm, None);
    assert_eq!(format_statusline_segment(&snapshot), "ctx 63%");
}

// 3. 비정상 입력 --------------------------------------------------------------
#[test]
fn malformed_or_metricless_input_extracts_nothing() {
    for raw in ["", "not json", "[]", "42", "{}", r#"{"session_id":"s"}"#] {
        assert!(
            extract_statusline_snapshot(raw.as_bytes()).is_none(),
            "{raw}"
        );
    }

    // 사용률만 남은 비율로 오는 형태는 100 에서 뺀 값
    let remaining_only = r#"{"context_window":{"remaining_percentage":37}}"#;
    let snapshot = extract_statusline_snapshot(remaining_only.as_bytes()).expect("비율 있음");
    assert_eq!(snapshot.context_pct, Some(63.0));
}

// 4. 한 줄 형식 ---------------------------------------------------------------
#[test]
fn segment_lists_only_present_parts() {
    let snapshot = extract_statusline_snapshot(SUBSCRIBER_JSON.as_bytes()).expect("지표 있음");
    assert_eq!(
        format_statusline_segment(&snapshot),
        "5h 24% | 7d 41% | ctx 8%"
    );

    let five_only = LimitSnapshot {
        five_hour_pct: Some(42.0),
        ..LimitSnapshot::default()
    };
    assert_eq!(format_statusline_segment(&five_only), "5h 42%");
    assert_eq!(format_statusline_segment(&LimitSnapshot::default()), "");
}

// 5. 닫힌 포트 ----------------------------------------------------------------
#[test]
fn closed_port_prints_line_within_budget() {
    // 1. 닫힌 포트 확보
    let listener = TcpListener::bind("127.0.0.1:0").expect("바인드");
    let closed_port = listener.local_addr().expect("주소").port();
    drop(listener);

    // 2. 실행 시간과 출력
    let mut output: Vec<u8> = Vec::new();
    let started = Instant::now();
    let code = run_statusline_with(
        closed_port,
        &[],
        false,
        SUBSCRIBER_JSON.as_bytes(),
        &mut output,
    );
    let elapsed = started.elapsed();
    assert_eq!(code, 0);
    assert_eq!(
        String::from_utf8(output).expect("UTF-8"),
        "5h 24% | 7d 41% | ctx 8%\n"
    );
    assert!(elapsed < Duration::from_millis(800), "{elapsed:?}");
}

// 6. 전송 본문 ----------------------------------------------------------------
#[test]
fn delivery_posts_metadata_only_payload_to_route() {
    // 1. 요청을 받아 두는 가짜 훅 서버
    let listener = TcpListener::bind("127.0.0.1:0").expect("바인드");
    let port = listener.local_addr().expect("주소").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("수락");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("읽기 제한");
        let mut received: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let count = stream.read(&mut chunk).expect("요청 읽기");
            received.extend_from_slice(&chunk[..count]);
            let text = String::from_utf8_lossy(&received).to_string();
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
                continue;
            };
            let length: usize = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::to_string)
                })
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0);
            if body.len() >= length {
                break;
            }
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
            .expect("응답");
        String::from_utf8_lossy(&received).to_string()
    });

    // 2. 실행 후 요청 점검
    let mut output: Vec<u8> = Vec::new();
    let code = run_statusline_with(port, &[], false, SUBSCRIBER_JSON.as_bytes(), &mut output);
    assert_eq!(code, 0);
    let request = server.join().expect("서버 스레드");
    assert!(
        request.starts_with("POST /hook/statusline HTTP/1.1"),
        "{request}"
    );
    let (_, body) = request.split_once("\r\n\r\n").expect("본문 구분");
    let sent: serde_json::Value = serde_json::from_str(body).expect("JSON 본문");
    assert_eq!(sent["session_id"], "sess-sub");
    assert_eq!(sent["five_hour_pct"], 23.5);
    assert!(!body.contains("secret"), "{body}");
    assert!(!body.contains("transcript"), "{body}");
}

// 7. 감싸기 도우미 자식 -------------------------------------------------------
// 감싼 명령 역할: 시험 실행 파일을 자식으로 다시 띄웠을 때만 동작 (평소에는 통과)
#[test]
fn wrap_child_helper() {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|arg| arg == "cm-child-mode") {
        return;
    }
    let mut stdin_text = String::new();
    std::io::stdin()
        .read_to_string(&mut stdin_text)
        .expect("stdin 읽기");
    println!("CM-CHILD-STDIN:{stdin_text}:CM-CHILD-END");
    if args.iter().any(|arg| arg == "cm-child-exit7") {
        std::process::exit(7);
    }
}

fn child_command(extra_mode: &str) -> Vec<String> {
    let executable = std::env::current_exe().expect("시험 실행 파일");
    vec![
        executable.to_string_lossy().to_string(),
        "--exact".to_string(),
        CHILD_FILTER.to_string(),
        "--nocapture".to_string(),
        "--test-threads=1".to_string(),
        "cm-child-mode".to_string(),
        extra_mode.to_string(),
    ]
}

// 8. 감싸기 출력 통과 ---------------------------------------------------------
#[test]
fn wrap_passes_stdout_through_and_appends_segment() {
    // 1. 감싼 명령 출력은 같은 stdin 바이트를 담아 그대로 통과
    let listener = TcpListener::bind("127.0.0.1:0").expect("바인드");
    let closed_port = listener.local_addr().expect("주소").port();
    drop(listener);
    let command = child_command("cm-child-plain");
    let mut plain: Vec<u8> = Vec::new();
    let code = run_statusline_with(
        closed_port,
        &command,
        false,
        SUBSCRIBER_JSON.as_bytes(),
        &mut plain,
    );
    let plain_text = String::from_utf8(plain).expect("UTF-8");
    assert_eq!(code, 0);
    assert!(
        plain_text.contains(&format!("CM-CHILD-STDIN:{SUBSCRIBER_JSON}:CM-CHILD-END")),
        "{plain_text}"
    );
    assert!(!plain_text.contains("5h 24%"), "{plain_text}");

    // 2. 덧붙임은 자식 출력 뒤 새 줄
    let mut appended: Vec<u8> = Vec::new();
    run_statusline_with(
        closed_port,
        &command,
        true,
        SUBSCRIBER_JSON.as_bytes(),
        &mut appended,
    );
    let appended_text = String::from_utf8(appended).expect("UTF-8");
    assert!(
        appended_text.contains(&format!("CM-CHILD-STDIN:{SUBSCRIBER_JSON}:CM-CHILD-END")),
        "{appended_text}"
    );
    assert!(
        appended_text.ends_with("\n5h 24% | 7d 41% | ctx 8%\n"),
        "{appended_text}"
    );
}

// 9. 감싸기 종료 코드 ---------------------------------------------------------
#[test]
fn wrap_passes_exit_status_through() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("바인드");
    let closed_port = listener.local_addr().expect("주소").port();
    drop(listener);
    let mut output: Vec<u8> = Vec::new();
    let code = run_statusline_with(
        closed_port,
        &child_command("cm-child-exit7"),
        false,
        SUBSCRIBER_JSON.as_bytes(),
        &mut output,
    );
    assert_eq!(code, 7);
}

// 10. 감싼 명령 실행 실패 -----------------------------------------------------
#[test]
fn wrap_spawn_failure_falls_back_to_own_line() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("바인드");
    let closed_port = listener.local_addr().expect("주소").port();
    drop(listener);
    let mut output: Vec<u8> = Vec::new();
    let code = run_statusline_with(
        closed_port,
        &["cm-no-such-program-0001".to_string()],
        false,
        SUBSCRIBER_JSON.as_bytes(),
        &mut output,
    );
    assert_eq!(code, 0);
    assert_eq!(
        String::from_utf8(output).expect("UTF-8"),
        "5h 24% | 7d 41% | ctx 8%\n"
    );
}
