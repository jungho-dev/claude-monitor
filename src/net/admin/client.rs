//! net/admin/client.rs
//! 관리 포트 동기 HTTP/1.1 클라이언트 (TUI·status·명령줄 공용).

use crate::core::model::StatusSnapshot;
use anyhow::{Context, bail};
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const IO_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

// 1. 응답 형태 ----------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct AdminResponse {
    pub status: u16,
    pub reason: String,
    pub body: Vec<u8>,
}

// 2. 응답 머리 형태 -----------------------------------------------------------
struct ResponseHead {
    header_end: usize,
    status: u16,
    reason: String,
    content_length: Option<usize>,
    chunked: bool,
}

// 3. 상태 조회 클라이언트 (TUI·status 명령) -----------------------------------
pub fn fetch_status_blocking(port: u16) -> anyhow::Result<StatusSnapshot> {
    let response = send_admin_request(port, "GET", "/api/status", None)?;
    if !(200..300).contains(&response.status) {
        bail!(
            "관리 서버 응답 오류: HTTP {} {}",
            response.status,
            response.reason
        );
    }
    serde_json::from_slice(&response.body).context("상태 응답 형식 오류")
}

// 4. 관리 명령 클라이언트 -----------------------------------------------------
pub fn post_admin_blocking(port: u16, path: &str, token: &str) -> anyhow::Result<String> {
    let response = send_admin_request(port, "POST", path, Some(token))?;
    if !(200..300).contains(&response.status) {
        bail!(
            "관리 서버 응답 오류: HTTP {} {}",
            response.status,
            response.reason
        );
    }
    Ok(String::from_utf8_lossy(&response.body).into_owned())
}

// 5. 요청 송수신 --------------------------------------------------------------
pub(super) fn send_admin_request(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
) -> anyhow::Result<AdminResponse> {
    // 1. 입력 검사 (헤더 주입 방지)
    for text in [method, path].into_iter().chain(token) {
        if text.bytes().any(|byte| byte == b'\r' || byte == b'\n') {
            bail!("요청 값에 줄바꿈 포함");
        }
    }

    // 2. 연결
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .with_context(|| format!("관리 포트 연결 실패: {addr}"))?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    // 3. 요청 전송 (Connection: close 는 보내지 않고 본문 완결로 종료 판정)
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n");
    request.push_str("Accept: application/json\r\n");
    if let Some(token) = token {
        request.push_str(&format!("x-cm-token: {token}\r\n"));
    }
    if method == "POST" {
        request.push_str("Content-Length: 0\r\n");
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .context("요청 전송 실패")?;

    // 4. 응답 수신 (본문 완결 또는 EOF, 상한 16 MiB)
    let mut raw = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                raw.extend_from_slice(&buffer[..count]);
                if raw.len() > MAX_RESPONSE_BYTES {
                    bail!("응답 크기 초과");
                }
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err)
                if !raw.is_empty()
                    && matches!(
                        err.kind(),
                        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
                    ) =>
            {
                break;
            }
            Err(err) => return Err(err).context("응답 수신 실패"),
        }
        if let Some(head) = parse_response_head(&raw)? {
            let raw_body = &raw[head.header_end + 4..];
            let complete = if head.chunked {
                raw_body.ends_with(b"0\r\n\r\n")
            } else if let Some(length) = head.content_length {
                raw_body.len() >= length
            } else {
                false
            };
            if complete {
                break;
            }
        }
    }

    // 5. 본문 추출
    let head = parse_response_head(&raw)?.context("응답 헤더 끝 없음")?;
    let raw_body = &raw[head.header_end + 4..];
    let body = if head.chunked {
        decode_chunked_body(raw_body)?
    } else if let Some(length) = head.content_length {
        raw_body[..length.min(raw_body.len())].to_vec()
    } else {
        raw_body.to_vec()
    };
    Ok(AdminResponse {
        status: head.status,
        reason: head.reason,
        body,
    })
}

// 6. 응답 머리 해석 -----------------------------------------------------------
fn parse_response_head(raw: &[u8]) -> anyhow::Result<Option<ResponseHead>> {
    let Some(header_end) = raw.windows(4).position(|window| window == b"\r\n\r\n") else {
        return Ok(None);
    };
    let head_text = String::from_utf8_lossy(&raw[..header_end]).into_owned();
    let mut lines = head_text.split("\r\n");
    let mut status_parts = lines.next().unwrap_or("").splitn(3, ' ');
    let _version = status_parts.next();
    let status: u16 = status_parts
        .next()
        .and_then(|text| text.parse().ok())
        .context("상태 줄 형식 오류")?;
    let reason = status_parts.next().unwrap_or("").to_string();
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => content_length = value.parse().ok(),
                "transfer-encoding" => chunked = value.to_ascii_lowercase().contains("chunked"),
                _ => {}
            }
        }
    }
    Ok(Some(ResponseHead {
        header_end,
        status,
        reason,
        content_length,
        chunked,
    }))
}

// 7. 청크 본문 해제 -----------------------------------------------------------
pub(super) fn decode_chunked_body(raw: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut decoded = Vec::new();
    let mut cursor = 0usize;
    loop {
        // 1. 크기 줄
        let line_end = raw[cursor..]
            .windows(2)
            .position(|pair| pair == b"\r\n")
            .map(|offset| cursor + offset)
            .context("청크 크기 줄 없음")?;
        let size_text =
            std::str::from_utf8(&raw[cursor..line_end]).context("청크 크기 형식 오류")?;
        let size_hex = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).context("청크 크기 형식 오류")?;
        cursor = line_end + 2;

        // 2. 종료 청크
        if size == 0 {
            break;
        }

        // 3. 청크 본문
        let chunk_end = cursor
            .checked_add(size)
            .filter(|end| *end + 2 <= raw.len())
            .context("청크 본문 잘림")?;
        decoded.extend_from_slice(&raw[cursor..chunk_end]);
        cursor = chunk_end + 2;
    }
    Ok(decoded)
}
