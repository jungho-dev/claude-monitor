//! net/gateway/proxy.rs
//! 상류 연결 커넥터: HTTPS_PROXY 가 있으면 http 프록시에 CONNECT 터널을 열고, 없거나 NO_PROXY 대상이면 직접 연결.

#[cfg(test)]
mod tests;

use crate::net::common::BoxError;
use hyper::Uri;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioIo;
use std::fmt;
use std::future::Future;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tower_service::Service;

const HANDSHAKE_TIMEOUT_SECS: u64 = 15;
const MAX_CONNECT_REPLY_BYTES: usize = 8 * 1024;
const DEFAULT_HTTPS_PORT: u16 = 443;
const DEFAULT_PROXY_PORT: u16 = 80;
const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

// 1. 프록시 설정 형태 ---------------------------------------------------------
/// 환경변수에서 읽은 프록시 설정 (자격 증명은 basic_auth 토큰에만 존재, 로그·Debug 출력 금지).
#[derive(Debug)]
pub struct ProxySettings {
    pub proxy: ProxyChoice,
    pub no_proxy: Vec<String>,
}

#[derive(Debug)]
pub enum ProxyChoice {
    Unset,
    Tunnel(ProxyEndpoint),
    /// 지원하지 않거나 형식이 틀린 주소 (프록시 대상 연결은 거부, 주소 원문은 보관하지 않음).
    Invalid(&'static str),
}

pub struct ProxyEndpoint {
    pub uri: Uri,
    pub basic_auth: Option<String>,
}

impl fmt::Debug for ProxyEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let auth = if self.basic_auth.is_some() {
            "<redacted>"
        } else {
            "none"
        };
        formatter
            .debug_struct("ProxyEndpoint")
            .field("uri", &self.uri)
            .field("basic_auth", &auth)
            .finish()
    }
}

// 2. 환경변수 판독 ------------------------------------------------------------
/// HTTPS_PROXY(https_proxy), NO_PROXY(no_proxy) 를 읽어 설정 구성. 빈 값은 미설정으로 취급.
pub fn read_proxy_settings(lookup: impl Fn(&str) -> Option<String>) -> ProxySettings {
    let first_set = |names: [&str; 2]| {
        names.iter().find_map(|name| {
            lookup(name)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
    };
    let proxy = match first_set(["HTTPS_PROXY", "https_proxy"]) {
        Some(raw) => parse_proxy_url(&raw),
        None => ProxyChoice::Unset,
    };
    let mut no_proxy = Vec::new();
    for raw_entry in first_set(["NO_PROXY", "no_proxy"])
        .unwrap_or_default()
        .split(',')
    {
        let mut entry = raw_entry.trim().to_ascii_lowercase();
        if let Some((head, tail)) = entry.rsplit_once(':')
            && !tail.is_empty()
            && tail.bytes().all(|byte| byte.is_ascii_digit())
            && (!head.contains(':') || head.ends_with(']'))
        {
            entry = head.to_string();
        }
        let bare = entry.trim_start_matches("*.").trim_start_matches('.');
        if !bare.is_empty() {
            no_proxy.push(bare.to_string());
        }
    }
    ProxySettings { proxy, no_proxy }
}

// 3. 프록시 주소 해석 ---------------------------------------------------------
/// http:// 주소(스킴 생략 시 http)만 허용, userinfo 는 Basic 토큰으로 변환.
fn parse_proxy_url(raw: &str) -> ProxyChoice {
    let (scheme, rest) = raw.split_once("://").unwrap_or(("http", raw));
    if !scheme.eq_ignore_ascii_case("http") {
        return ProxyChoice::Invalid("only http:// proxy addresses are supported");
    }
    let authority_text = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let (userinfo, host_port) = match authority_text.rsplit_once('@') {
        Some((userinfo, host_port)) => (userinfo, host_port),
        None => ("", authority_text),
    };
    let Ok(parsed) = format!("http://{host_port}").parse::<Uri>() else {
        return ProxyChoice::Invalid("proxy address is malformed");
    };
    let Some(host) = parsed.host().filter(|host| !host.is_empty()) else {
        return ProxyChoice::Invalid("proxy address has no host");
    };
    let port = parsed.port_u16().unwrap_or(DEFAULT_PROXY_PORT);
    let Ok(uri) = format!("http://{host}:{port}").parse::<Uri>() else {
        return ProxyChoice::Invalid("proxy address is malformed");
    };
    let basic_auth = if userinfo.is_empty() {
        None
    } else {
        Some(encode_base64(&decode_percent(userinfo)))
    };
    ProxyChoice::Tunnel(ProxyEndpoint { uri, basic_auth })
}

// 4. 퍼센트 디코딩 ------------------------------------------------------------
/// %XX 를 바이트로 복원, 잘못된 시퀀스는 원문 유지.
fn decode_percent(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(pair) = bytes.get(index + 1..index + 3)
            && let Ok(hex) = std::str::from_utf8(pair)
            && let Ok(value) = u8::from_str_radix(hex, 16)
        {
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    decoded
}

// 5. Base64 인코딩 ------------------------------------------------------------
fn encode_base64(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        let group = (u32::from(chunk[0]) << 16) | (u32::from(second) << 8) | u32::from(third);
        encoded.push(BASE64_ALPHABET[(group >> 18) as usize & 63] as char);
        encoded.push(BASE64_ALPHABET[(group >> 12) as usize & 63] as char);
        encoded.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(group >> 6) as usize & 63] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            BASE64_ALPHABET[group as usize & 63] as char
        } else {
            '='
        });
    }
    encoded
}

// 6. NO_PROXY 판정 ------------------------------------------------------------
/// 루프백 호스트, `*`, 정확 일치, 도메인 접미(점 경계) 일치 시 프록시 우회. 포트는 무시, IP 는 정확 일치만.
pub fn bypasses_proxy(host: &str, no_proxy: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let ip = bare.parse::<IpAddr>().ok();
    if bare == "localhost" || ip.is_some_and(|address| address.is_loopback()) {
        return true;
    }
    no_proxy.iter().any(|entry| {
        entry == "*"
            || host == *entry
            || (ip.is_none()
                && host
                    .strip_suffix(entry.as_str())
                    .is_some_and(|prefix| prefix.ends_with('.')))
    })
}

// 7. CONNECT 터널 협상 --------------------------------------------------------
/// 프록시 연결 위에서 CONNECT 요청을 보내고 2xx 응답 머리까지 읽음 (이후 바이트는 터널 본문이라 읽지 않음).
async fn open_connect_tunnel(
    stream: &mut TcpStream,
    target: &str,
    basic_auth: Option<&str>,
) -> io::Result<()> {
    // 1. 요청 전송
    let auth_line = basic_auth
        .map(|token| format!("Proxy-Authorization: Basic {token}\r\n"))
        .unwrap_or_default();
    let request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n{auth_line}\r\n");
    stream.write_all(request.as_bytes()).await?;

    // 2. 응답 머리까지 한 바이트씩 읽음 (터널 본문을 삼키지 않기 위함)
    let mut reply = Vec::with_capacity(256);
    let mut byte = [0_u8; 1];
    while !reply.ends_with(b"\r\n\r\n") {
        if reply.len() >= MAX_CONNECT_REPLY_BYTES {
            return Err(io::Error::other("proxy CONNECT reply is too large"));
        }
        if stream.read(&mut byte).await? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "proxy closed the connection during CONNECT",
            ));
        }
        reply.push(byte[0]);
    }

    // 3. 상태 줄 판독 (HTTP/1.x 2xx 만 성공, 사유 문구는 오류에 싣지 않음)
    let status_line = reply
        .split(|byte| *byte == b'\r')
        .next()
        .unwrap_or_default();
    let status_text = String::from_utf8_lossy(status_line);
    let mut fields = status_text.split_whitespace();
    let version_ok = fields
        .next()
        .is_some_and(|version| version.starts_with("HTTP/1."));
    let status = fields.next().and_then(|code| code.parse::<u16>().ok());
    match status {
        Some(code) if version_ok && (200..300).contains(&code) => Ok(()),
        Some(code) if version_ok => Err(io::Error::other(format!(
            "proxy CONNECT refused with status {code}"
        ))),
        _ => Err(io::Error::other("proxy CONNECT reply is malformed")),
    }
}

// 8. 상류 연결 ----------------------------------------------------------------
async fn connect_upstream(
    mut inner: HttpConnector,
    settings: Arc<ProxySettings>,
    dst: Uri,
) -> Result<TokioIo<TcpStream>, BoxError> {
    // 1. 터널 필요 여부 (https 상류이고 NO_PROXY·루프백이 아닐 때)
    let host = dst.host().unwrap_or_default().to_string();
    let tunnel_wanted =
        dst.scheme_str() == Some("https") && !bypasses_proxy(&host, &settings.no_proxy);
    let endpoint = match (&settings.proxy, tunnel_wanted) {
        (ProxyChoice::Tunnel(endpoint), true) => endpoint,
        (ProxyChoice::Invalid(reason), true) => {
            return Err(io::Error::other(format!("HTTPS_PROXY is unusable: {reason}")).into());
        }
        _ => return Ok(inner.call(dst).await?),
    };

    // 2. 프록시 연결 후 CONNECT 협상
    let target = format!("{host}:{}", dst.port_u16().unwrap_or(DEFAULT_HTTPS_PORT));
    let mut stream = inner.call(endpoint.uri.clone()).await?;
    let handshake =
        open_connect_tunnel(stream.inner_mut(), &target, endpoint.basic_auth.as_deref());
    tokio::time::timeout(Duration::from_secs(HANDSHAKE_TIMEOUT_SECS), handshake)
        .await
        .map_err(|_| io::Error::other("proxy CONNECT timed out"))??;
    Ok(stream)
}

// 9. 커넥터 -------------------------------------------------------------------
/// HttpConnector 를 감싸 프록시 터널을 선택 적용하는 Service (TLS 는 바깥 HttpsConnector 가 터널 위에서 수행).
#[derive(Clone)]
pub struct ProxyConnector {
    inner: HttpConnector,
    settings: Arc<ProxySettings>,
}

impl ProxyConnector {
    pub fn new(inner: HttpConnector, settings: ProxySettings) -> Self {
        Self {
            inner,
            settings: Arc::new(settings),
        }
    }
}

impl Service<Uri> for ProxyConnector {
    type Response = TokioIo<TcpStream>;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        Box::pin(connect_upstream(
            self.inner.clone(),
            Arc::clone(&self.settings),
            dst,
        ))
    }
}
