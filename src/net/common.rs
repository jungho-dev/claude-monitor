//! net/common.rs
//! 127.0.0.1 전용 HTTP 서버 루프, 브라우저 발신 차단, 응답 본문 공용 형태.

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{CONTENT_TYPE, HOST, HeaderValue};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use std::convert::Infallible;
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::TcpListener;

const ACCEPT_BACKOFF_BASE: Duration = Duration::from_millis(100);
const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type ResponseBody = BoxBody<Bytes, BoxError>;

// 1. 고정 본문 ----------------------------------------------------------------
pub fn full_body(bytes: impl Into<Bytes>) -> ResponseBody {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed()
}

// 2. JSON 응답 ----------------------------------------------------------------
pub fn json_response<T: serde::Serialize>(
    status: StatusCode,
    payload: &T,
) -> Response<ResponseBody> {
    let (status, body) = match serde_json::to_vec(payload) {
        Ok(bytes) => (status, bytes),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{{\"error\":\"직렬화 실패: {err}\"}}").into_bytes(),
        ),
    };
    let mut response = Response::new(full_body(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
}

// 3. 텍스트 응답 --------------------------------------------------------------
pub fn text_response(status: StatusCode, text: impl Into<String>) -> Response<ResponseBody> {
    let mut response = Response::new(full_body(text.into()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

// 4. 로컬 요청 검사 -----------------------------------------------------------
// DNS rebinding·브라우저 발신 차단 (Host 불일치, Origin, Sec-Fetch-Site)
pub(crate) fn reject_non_local<B>(req: &Request<B>, port: u16) -> Option<Response<ResponseBody>> {
    let host_ok = match req
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
    {
        Some(host) => host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}"),
        None => false,
    };
    if !host_ok {
        return Some(text_response(StatusCode::FORBIDDEN, "host not allowed"));
    }
    if req.headers().contains_key("origin") || req.headers().contains_key("sec-fetch-site") {
        return Some(text_response(
            StatusCode::FORBIDDEN,
            "browser requests not allowed",
        ));
    }
    None
}

// 5. 서버 루프 ----------------------------------------------------------------
/// 127.0.0.1:port 바인드 후 수신 루프 위임.
pub async fn serve_local_http<F, Fut>(port: u16, handler: F) -> anyhow::Result<()>
where
    F: Fn(Request<Incoming>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response<ResponseBody>> + Send + 'static,
{
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|err| anyhow::anyhow!("포트 수신 실패 {addr}: {err}"))?;
    serve_local_listener(listener, handler).await
}

// 6. 보유 리스너 수신 루프 ----------------------------------------------------
/// 이미 바인드한 리스너로 수신, 연결마다 태스크 분리 (handler panic 은 해당 연결만 종료).
pub async fn serve_local_listener<F, Fut>(listener: TcpListener, handler: F) -> anyhow::Result<()>
where
    F: Fn(Request<Incoming>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response<ResponseBody>> + Send + 'static,
{
    let addr = listener
        .local_addr()
        .map_err(|err| anyhow::anyhow!("리스너 주소 확인 실패: {err}"))?;
    let port = addr.port();
    let mut accept_failures: u32 = 0;
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(accepted) => {
                accept_failures = 0;
                accepted
            }
            Err(err) => {
                // 연속 실패는 지수 대기, 로그는 1·2·4·8회째만
                accept_failures = accept_failures.saturating_add(1);
                if accept_failures.is_power_of_two() {
                    eprintln!("[rap] accept 실패 {addr} ({accept_failures}회째): {err}");
                }
                let delay = accept_retry_delay(&err, accept_failures);
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                continue;
            }
        };
        let handler = handler.clone();
        tokio::spawn(async move {
            let service = service_fn(move |req: Request<Incoming>| {
                let handler = handler.clone();
                async move {
                    if let Some(denied) = reject_non_local(&req, port) {
                        return Ok::<_, Infallible>(denied);
                    }
                    Ok(handler(req).await)
                }
            });
            let builder = auto::Builder::new(TokioExecutor::new());
            if let Err(err) = builder
                .serve_connection_with_upgrades(TokioIo::new(stream), service)
                .await
            {
                // 상대 종료·리셋·순수 연결 확인은 기록 제외 (원인 체인 검사)
                let mut quiet = false;
                let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(err.as_ref());
                while let Some(current) = cause {
                    if let Some(io_err) = current.downcast_ref::<std::io::Error>() {
                        quiet |= matches!(
                            io_err.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                                | std::io::ErrorKind::BrokenPipe
                                | std::io::ErrorKind::NotConnected
                                | std::io::ErrorKind::UnexpectedEof
                        );
                    }
                    if let Some(hyper_err) = current.downcast_ref::<hyper::Error>() {
                        quiet |= hyper_err.is_incomplete_message() || hyper_err.is_canceled();
                    }
                    cause = current.source();
                }
                let text = err.to_string();
                quiet |= text.contains("connection closed") || text.contains("shutting down");
                if !quiet {
                    eprintln!("[rap] 연결 오류 :{port}: {text}");
                }
            }
        });
    }
}

// 7. 수신 실패 재시도 대기 ----------------------------------------------------
/// 상대 쪽 연결 실패는 즉시 재시도, 그 외는 100ms 부터 두 배씩 최대 1초.
fn accept_retry_delay(err: &std::io::Error, consecutive_failures: u32) -> Duration {
    if matches!(
        err.kind(),
        std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::Interrupted
    ) {
        return Duration::ZERO;
    }
    let doublings = consecutive_failures.saturating_sub(1).min(4);
    (ACCEPT_BACKOFF_BASE * 2u32.pow(doublings)).min(ACCEPT_BACKOFF_MAX)
}

#[cfg(test)]
mod tests {
    use super::{ACCEPT_BACKOFF_MAX, accept_retry_delay, reject_non_local};
    use hyper::{Request, StatusCode};
    use std::io::{Error, ErrorKind};
    use std::time::Duration;

    // 1. 로컬 요청 검사 표 ----------------------------------------------------
    #[test]
    fn reject_non_local_table() {
        let port: u16 = 47999;
        let own_host = format!("127.0.0.1:{port}");
        let localhost = format!("localhost:{port}");
        let other_port = format!("127.0.0.1:{}", port + 1);
        let evil_host = format!("evil.test:{port}");
        let cases = vec![
            ("Host 없음", None, vec![], true),
            ("낯선 Host", Some(evil_host.as_str()), vec![], true),
            ("다른 포트", Some(other_port.as_str()), vec![], true),
            ("localhost", Some(localhost.as_str()), vec![], false),
            ("127.0.0.1", Some(own_host.as_str()), vec![], false),
            (
                "Origin",
                Some(own_host.as_str()),
                vec![("Origin", "http://127.0.0.1")],
                true,
            ),
            (
                "Sec-Fetch-Site",
                Some(own_host.as_str()),
                vec![("Sec-Fetch-Site", "same-origin")],
                true,
            ),
        ];
        for (label, host, extra_headers, rejected) in cases {
            let mut builder = Request::builder().uri("/api/status");
            if let Some(host) = host {
                builder = builder.header("Host", host);
            }
            for (name, header_value) in extra_headers {
                builder = builder.header(name, header_value);
            }
            let request = builder.body(()).expect("요청 구성");
            let denied = reject_non_local(&request, port);
            assert_eq!(denied.is_some(), rejected, "{label}");
            if let Some(response) = denied {
                assert_eq!(response.status(), StatusCode::FORBIDDEN, "{label}");
            }
        }
    }

    // 2. 수신 실패 대기 -------------------------------------------------------
    #[test]
    fn accept_retry_delay_backs_off_except_per_connection_errors() {
        // 1. 연결 단위 오류는 즉시 재시도
        for kind in [
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::Interrupted,
        ] {
            assert_eq!(accept_retry_delay(&Error::from(kind), 7), Duration::ZERO);
        }

        // 2. 자원 고갈류는 두 배씩 늘어 상한에서 멈춤
        let exhausted = Error::from(ErrorKind::OutOfMemory);
        let delays: Vec<Duration> = (1..=8)
            .map(|failures| accept_retry_delay(&exhausted, failures))
            .collect();
        assert_eq!(delays[0], Duration::from_millis(100));
        assert_eq!(delays[1], Duration::from_millis(200));
        assert_eq!(delays[2], Duration::from_millis(400));
        assert_eq!(delays[3], Duration::from_millis(800));
        assert_eq!(delays[4], ACCEPT_BACKOFF_MAX);
        assert!(delays.iter().all(|delay| *delay <= ACCEPT_BACKOFF_MAX));
        assert_eq!(accept_retry_delay(&exhausted, u32::MAX), ACCEPT_BACKOFF_MAX);
    }
}
