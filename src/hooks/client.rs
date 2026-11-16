//! hooks/client.rs
//! command 훅 클라이언트와 블로킹 HTTP 요청 (stdin JSON 을 훅 포트로 중계).

use crate::core::config::AppConfig;
use crate::hooks::chain::fail_closed_body;
use anyhow::{Context, anyhow, bail};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

// stdin 본문 상한
const MAX_STDIN_BYTES: u64 = 16 * 1024 * 1024;
// 응답 본문 상한
const MAX_REPLY_BYTES: u64 = 64 * 1024 * 1024;
const CLIENT_TIMEOUT_SECS: u64 = 10;
// chunk 크기줄·트레일러 한 줄 상한
const MAX_CHUNK_LINE_BYTES: usize = 8192;
// 트레일러 줄 수 상한
const MAX_TRAILER_LINES: usize = 64;

// 1. 블로킹 HTTP 요청 ---------------------------------------------------------
/// std TcpStream 단발 요청. 상태 코드와 본문 반환.
pub fn blocking_http_request(
    host_port: &str,
    method: &str,
    path: &str,
    body: &[u8],
    timeout: Duration,
) -> anyhow::Result<(u16, Vec<u8>)> {
    // 1. 연결
    let addr = host_port
        .to_socket_addrs()
        .with_context(|| format!("주소 해석 실패: {host_port}"))?
        .next()
        .ok_or_else(|| anyhow!("주소 없음: {host_port}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .with_context(|| format!("연결 실패: {host_port}"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    // 2. 요청 전송 (헤더와 본문을 한 번에)
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host_port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut request_bytes: Vec<u8> = Vec::with_capacity(head.len() + body.len());
    request_bytes.extend_from_slice(head.as_bytes());
    request_bytes.extend_from_slice(body);
    stream.set_nodelay(true)?;
    stream.write_all(&request_bytes)?;
    stream.flush()?;

    // 3. 응답 헤더 수신
    let mut raw: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        if let Some(position) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        let count = stream.read(&mut chunk).context("응답 읽기 실패")?;
        if count == 0 {
            bail!("응답 헤더 전에 연결 종료");
        }
        raw.extend_from_slice(&chunk[..count]);
        if raw.len() as u64 > MAX_REPLY_BYTES {
            bail!("응답 상한 초과");
        }
    };
    let header_text = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let status: u16 = header_text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| anyhow!("응답 상태줄 형식 오류"))?;
    let chunked = header_text
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked");
    let declared: Option<usize> = header_text.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            value.trim().parse().ok()
        } else {
            None
        }
    });

    // 4. 응답 본문 수신 (길이 도달 또는 종료 chunk 와 트레일러 끝까지)
    let mut chunk_cursor = 0usize;
    let mut decoded_body: Vec<u8> = Vec::new();
    loop {
        let payload = &raw[header_end + 4..];
        let complete = if chunked {
            advance_chunked_body(payload, &mut chunk_cursor, &mut decoded_body)?
        } else {
            declared.is_some_and(|length| payload.len() >= length)
        };
        if complete {
            break;
        }
        let count = stream.read(&mut chunk).context("응답 본문 읽기 실패")?;
        if count == 0 {
            if chunked || declared.is_some() {
                bail!("응답 본문 잘림 (연결 종료)");
            }
            break;
        }
        raw.extend_from_slice(&chunk[..count]);
        if raw.len() as u64 > MAX_REPLY_BYTES {
            bail!("응답 상한 초과");
        }
    }
    let payload = &raw[header_end + 4..];
    if !chunked {
        return Ok((status, payload.to_vec()));
    }

    // 5. chunked 본문 반환
    Ok((status, decoded_body))
}

// 2. 훅 포트 POST -------------------------------------------------------------
/// `http://127.0.0.1:{port}/hook/{route}` 로 payload 전달 (경로 문자 검증 포함).
pub fn post_hook_payload(port: u16, route: &str, payload: &[u8]) -> anyhow::Result<(u16, Vec<u8>)> {
    // 1. 경로 검증 (헤더 삽입 방지)
    let segments: Vec<&str> = route.split('/').collect();
    let shape_ok = match segments.as_slice() {
        [single] => !single.is_empty(),
        ["relay", name] => !name.is_empty(),
        _ => false,
    };
    let chars_ok = route
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '/');
    if !shape_ok || !chars_ok {
        bail!("훅 경로 형식 오류 (a-z, 0-9, - 만 허용, relay/<이름> 가능)");
    }

    // 2. 전송
    let host_port = format!("127.0.0.1:{port}");
    let path = format!("/hook/{route}");
    blocking_http_request(
        &host_port,
        "POST",
        &path,
        payload,
        Duration::from_secs(CLIENT_TIMEOUT_SECS),
    )
}

// 3. command 훅 클라이언트 ----------------------------------------------------
/// stdin 전체(최대 16 MiB)를 훅 포트로 보내 응답을 stdout 에 출력 (실패는 pretool 만 거부 본문 후 0, 나머지 1).
pub fn run_hook_client(config: &AppConfig, route: &str) -> anyhow::Result<i32> {
    // 1. stdin 읽기 (상한 초과 판정은 전달 단계)
    let mut payload: Vec<u8> = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_STDIN_BYTES + 1)
        .read_to_end(&mut payload)
        .context("stdin 읽기 실패")?;

    // 2. 훅 포트 전달과 응답 출력
    let mut stdout = std::io::stdout().lock();
    relay_hook_payload(config.ports.hooks, route, &payload, &mut stdout)
}

// 4. 훅 포트 전달과 응답 출력 -------------------------------------------------
// 실패 시 pretool 경로는 fail-closed 거부 본문 출력 (command 훅은 exit 2 만 차단)
fn relay_hook_payload(
    port: u16,
    route: &str,
    payload: &[u8],
    output: &mut impl Write,
) -> anyhow::Result<i32> {
    // 1. 전달 시도
    let failure: String = if payload.len() as u64 > MAX_STDIN_BYTES {
        "stdin 이 16 MiB 상한을 넘음".to_string()
    } else {
        match post_hook_payload(port, route, payload) {
            Ok((status, reply)) if (200..300).contains(&status) => {
                output.write_all(&reply).context("stdout 쓰기 실패")?;
                output.flush().context("stdout 비우기 실패")?;
                return Ok(0);
            }
            Ok((status, _)) => format!("비정상 응답 상태 {status}"),
            Err(err) => format!("전달 실패: {err:#}"),
        }
    };

    // 2. 실패 처리 (pretool 은 거부 본문, 그 외 exit 1)
    eprintln!("[cm hook] {failure}");
    if !route.starts_with("pretool") {
        return Ok(1);
    }
    output
        .write_all(&fail_closed_body())
        .context("stdout 쓰기 실패")?;
    output.flush().context("stdout 비우기 실패")?;
    Ok(0)
}

// 5. chunked 본문 진행 --------------------------------------------------------
// 완성 chunk 만 이어 붙이고 종료 chunk·트레일러 끝이면 true (미완성은 false, 형식 위반은 오류)
fn advance_chunked_body(
    payload: &[u8],
    cursor: &mut usize,
    decoded: &mut Vec<u8>,
) -> anyhow::Result<bool> {
    loop {
        // 1. chunk 크기줄
        let pending = &payload[*cursor..];
        let Some(line_end) = pending.windows(2).position(|window| window == b"\r\n") else {
            if pending.len() > MAX_CHUNK_LINE_BYTES {
                bail!("chunk 크기줄 과다");
            }
            return Ok(false);
        };
        let size_text = String::from_utf8_lossy(&pending[..line_end]).to_string();
        let size_hex = size_text.split(';').next().unwrap_or("").trim();
        if !size_hex.bytes().all(|digit| digit.is_ascii_hexdigit()) {
            bail!("chunk 크기 형식 오류");
        }
        let size = usize::from_str_radix(size_hex, 16).context("chunk 크기 형식 오류")?;
        let body_start = *cursor + line_end + 2;

        // 2. 종료 chunk 와 트레일러 (빈 줄이 끝)
        if size == 0 {
            let mut trailer_start = body_start;
            for _ in 0..=MAX_TRAILER_LINES {
                let trailer = &payload[trailer_start..];
                let Some(end) = trailer.windows(2).position(|window| window == b"\r\n") else {
                    if trailer.len() > MAX_CHUNK_LINE_BYTES {
                        bail!("chunk 트레일러 줄 과다");
                    }
                    return Ok(false);
                };
                if end == 0 {
                    *cursor = trailer_start + 2;
                    return Ok(true);
                }
                trailer_start += end + 2;
            }
            bail!("chunk 트레일러 과다");
        }

        // 3. chunk 본문과 뒤따르는 CRLF
        let need = size
            .checked_add(2)
            .ok_or_else(|| anyhow!("chunk 본문 잘림"))?;
        if payload.len() - body_start < need {
            return Ok(false);
        }
        if &payload[body_start + size..body_start + need] != b"\r\n" {
            bail!("chunk 종료 CRLF 누락");
        }
        decoded.extend_from_slice(&payload[body_start..body_start + size]);
        *cursor = body_start + need;
    }
}

#[cfg(test)]
mod tests {
    use super::{advance_chunked_body, blocking_http_request, relay_hook_payload};
    use serde_json::Value;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    // 1. pretool 경로 전달 실패 -----------------------------------------------
    #[test]
    fn pretool_route_failure_emits_deny_json() {
        // 1. 닫힌 포트로 전달 실패 유도
        let listener = TcpListener::bind("127.0.0.1:0").expect("바인드");
        let closed_port = listener.local_addr().expect("주소").port();
        drop(listener);
        let mut output: Vec<u8> = Vec::new();

        // 2. 거부 본문 출력과 종료 코드 0
        let code = relay_hook_payload(closed_port, "pretool-chain", b"{}", &mut output)
            .expect("전달 결과");
        assert_eq!(code, 0);
        let body: Value = serde_json::from_slice(&output).expect("JSON");
        assert_eq!(body["hookSpecificOutput"]["permissionDecision"], "deny");
    }

    // 2. relay 경로 전달 실패 -------------------------------------------------
    #[test]
    fn relay_route_failure_keeps_exit_1() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("바인드");
        let closed_port = listener.local_addr().expect("주소").port();
        drop(listener);
        let mut output: Vec<u8> = Vec::new();
        let code = relay_hook_payload(closed_port, "relay/codeguard", b"{}", &mut output)
            .expect("전달 결과");
        assert_eq!(code, 1);
        assert!(output.is_empty());
    }

    // 3. chunk 크기 덧셈 오버플로 ---------------------------------------------
    #[test]
    fn chunked_decode_rejects_overflowing_size() {
        // 1. 정상 chunk 복원
        let mut cursor = 0usize;
        let mut decoded: Vec<u8> = Vec::new();
        let complete = advance_chunked_body(b"3\r\nabc\r\n0\r\n\r\n", &mut cursor, &mut decoded)
            .expect("정상 복원");
        assert!(complete);
        assert_eq!(decoded, b"abc");

        // 2. usize::MAX 크기는 panic 없이 오류
        let mut cursor = 0usize;
        let mut decoded: Vec<u8> = Vec::new();
        let message = format!(
            "{:#}",
            advance_chunked_body(b"ffffffffffffffff\r\n0\r\n\r\n", &mut cursor, &mut decoded)
                .expect_err("오류 기대")
        );
        assert!(message.contains("chunk 본문 잘림"), "{message}");
    }

    // 4. chunk 데이터가 0 CRLF 로 끝남 ----------------------------------------
    #[test]
    fn chunked_data_ending_like_terminal_chunk_stays_incomplete() {
        // 1. 데이터 끝 패턴은 종료 chunk 가 아님
        let mut cursor = 0usize;
        let mut decoded: Vec<u8> = Vec::new();
        let complete =
            advance_chunked_body(b"4\r\na0\r\n\r\n", &mut cursor, &mut decoded).expect("부분 수신");
        assert!(!complete);
        assert_eq!(decoded, b"a0\r\n");

        // 2. 이어진 종료 chunk 로 완료
        let complete = advance_chunked_body(b"4\r\na0\r\n\r\n0\r\n\r\n", &mut cursor, &mut decoded)
            .expect("완료 수신");
        assert!(complete);
        assert_eq!(decoded, b"a0\r\n");
    }

    // 5. 확장·트레일러 종료 chunk ---------------------------------------------
    #[test]
    fn chunked_terminal_chunk_accepts_extension_and_trailers() {
        // 1. 확장이 붙은 종료 chunk
        let mut cursor = 0usize;
        let mut decoded: Vec<u8> = Vec::new();
        let complete = advance_chunked_body(
            b"3;ext=1\r\nabc\r\n0;done\r\n\r\n",
            &mut cursor,
            &mut decoded,
        )
        .expect("확장 수신");
        assert!(complete);
        assert_eq!(decoded, b"abc");

        // 2. 트레일러 끝 빈 줄까지 필요
        let mut cursor = 0usize;
        let mut decoded: Vec<u8> = Vec::new();
        let partial =
            advance_chunked_body(b"3\r\nabc\r\n0\r\nX-Sum: 1\r\n", &mut cursor, &mut decoded)
                .expect("트레일러 부분 수신");
        assert!(!partial);
        let complete = advance_chunked_body(
            b"3\r\nabc\r\n0\r\nX-Sum: 1\r\n\r\n",
            &mut cursor,
            &mut decoded,
        )
        .expect("트레일러 완료 수신");
        assert!(complete);
        assert_eq!(decoded, b"abc");
    }

    // 6. chunk 뒤 CRLF 누락 ---------------------------------------------------
    #[test]
    fn chunked_body_rejects_missing_crlf_after_chunk() {
        let mut cursor = 0usize;
        let mut decoded: Vec<u8> = Vec::new();
        let message = format!(
            "{:#}",
            advance_chunked_body(b"3\r\nabcXY0\r\n\r\n", &mut cursor, &mut decoded)
                .expect_err("오류 기대")
        );
        assert!(message.contains("chunk 종료 CRLF 누락"), "{message}");
    }

    // 7. 바이트 단위 증분 수신 ------------------------------------------------
    #[test]
    fn chunked_incremental_feed_matches_whole_decode() {
        // 1. 한 바이트씩 늘리며 진행
        let whole: &[u8] = b"5\r\nhello\r\n6;x=y\r\n world\r\n0\r\nT: v\r\n\r\n";
        let mut cursor = 0usize;
        let mut decoded: Vec<u8> = Vec::new();
        let mut finished_at: Option<usize> = None;
        for end in 1..=whole.len() {
            let complete =
                advance_chunked_body(&whole[..end], &mut cursor, &mut decoded).expect("증분 수신");
            if complete {
                finished_at = Some(end);
                break;
            }
        }

        // 2. 마지막 바이트에서만 완료
        assert_eq!(finished_at, Some(whole.len()));
        assert_eq!(decoded, b"hello world");
    }

    // 8. keep-alive 소켓의 chunked 응답 ---------------------------------------
    #[test]
    fn blocking_request_finishes_on_extension_terminal_chunk_without_close() {
        // 1. 종료 chunk 뒤 연결을 열어 둔 서버
        let listener = TcpListener::bind("127.0.0.1:0").expect("바인드");
        let port = listener.local_addr().expect("주소").port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("수락");
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).expect("요청 읽기");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\na0\r\n\r\n")
                .expect("첫 조각 쓰기");
            stream.flush().expect("비우기");
            std::thread::sleep(Duration::from_millis(200));
            stream.write_all(b"0;ext\r\n\r\n").expect("종료 조각 쓰기");
            stream.flush().expect("비우기");
            let mut rest: Vec<u8> = Vec::new();
            let _ = stream.read_to_end(&mut rest);
        });

        // 2. 연결 종료 없이 본문 복원
        let (status, body) = blocking_http_request(
            &format!("127.0.0.1:{port}"),
            "POST",
            "/hook/x",
            b"{}",
            Duration::from_secs(3),
        )
        .expect("응답 수신");
        assert_eq!(status, 200);
        assert_eq!(body, b"a0\r\n");
        server.join().expect("서버 스레드");
    }
}
