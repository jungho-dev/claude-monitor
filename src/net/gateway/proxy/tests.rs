//! net/gateway/proxy/tests.rs
//! 프록시 커넥터 테스트: 환경변수 판독, NO_PROXY 판정, CONNECT 협상(가짜 프록시), 거부 시 502.

use super::{
    ProxyChoice, ProxyConnector, ProxySettings, bypasses_proxy, encode_base64, read_proxy_settings,
};
use crate::net::admin::test_support::{build_test_config, build_test_context, post_raw};
use crate::net::common::serve_local_listener;
use crate::net::gateway::{build_gateway_state_with_proxy, proxy_request};
use crate::transcript::test_support::TempDirGuard;
use hyper_util::client::legacy::connect::HttpConnector;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tower_service::Service;

const TUNNEL_OK: &str = "HTTP/1.1 200 Connection established\r\n\r\n";
const PROXY_REFUSED: &str = "HTTP/1.1 407 Proxy Authentication Required\r\n\r\n";

// 1. 환경변수 꾸러미 설정 -----------------------------------------------------
fn settings_from(pairs: &[(&str, &str)]) -> ProxySettings {
    read_proxy_settings(|name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.to_string())
    })
}

fn connector_from(pairs: &[(&str, &str)]) -> ProxyConnector {
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    ProxyConnector::new(http, settings_from(pairs))
}

// 2. 가짜 CONNECT 프록시 ------------------------------------------------------
// 연결 하나를 받아 요청 머리를 읽고, 응답과 터널 본문을 쓴 뒤 닫음. 요청 머리 반환.
async fn spawn_fake_proxy(
    reply: &'static str,
    tunnel_payload: &'static str,
) -> (u16, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("가짜 프록시 바인드");
    let port = listener.local_addr().expect("가짜 프록시 주소").port();
    let handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("프록시 수락");
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).await.unwrap_or(0) == 0 {
                break;
            }
            head.push(byte[0]);
        }
        stream
            .write_all(reply.as_bytes())
            .await
            .expect("프록시 응답 쓰기");
        stream
            .write_all(tunnel_payload.as_bytes())
            .await
            .expect("터널 본문 쓰기");

        // 클라이언트가 닫을 때까지 유지 (먼저 닫으면 Windows 에서 RST 로 응답이 유실될 수 있음)
        let _ = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte)).await;
        String::from_utf8_lossy(&head).into_owned()
    });
    (port, handle)
}

// 3. 환경변수 판독 ------------------------------------------------------------
#[test]
fn settings_read_env_names_and_ignore_blank() {
    let upper_wins = settings_from(&[
        ("HTTPS_PROXY", "http://upper.example.test:3128"),
        ("https_proxy", "http://lower.example.test:3128"),
    ]);
    match upper_wins.proxy {
        ProxyChoice::Tunnel(endpoint) => {
            assert_eq!(endpoint.uri.to_string(), "http://upper.example.test:3128/");
            assert!(endpoint.basic_auth.is_none());
        }
        other => panic!("터널 설정 기대: {other:?}"),
    }
    let lower_only = settings_from(&[("https_proxy", "proxy.example.test")]);
    match lower_only.proxy {
        ProxyChoice::Tunnel(endpoint) => {
            assert_eq!(endpoint.uri.to_string(), "http://proxy.example.test:80/");
        }
        other => panic!("스킴 생략은 http 로 취급: {other:?}"),
    }
    assert!(matches!(
        settings_from(&[("HTTPS_PROXY", "   ")]).proxy,
        ProxyChoice::Unset
    ));
    assert!(matches!(settings_from(&[]).proxy, ProxyChoice::Unset));
}

// 4. 잘못된 프록시 주소 -------------------------------------------------------
#[test]
fn settings_mark_unsupported_or_malformed_proxy_invalid() {
    for raw in [
        "https://proxy.example.test:8443",
        "socks5://proxy.example.test:1080",
        "http://",
        "http://user:pw@",
        "http://bad host:3128",
    ] {
        let settings = settings_from(&[("HTTPS_PROXY", raw)]);
        assert!(
            matches!(settings.proxy, ProxyChoice::Invalid(_)),
            "{raw}: {settings:?}"
        );
    }
}

// 5. NO_PROXY 판정 ------------------------------------------------------------
#[test]
fn no_proxy_matches_suffix_wildcard_and_ignores_port() {
    let settings = settings_from(&[(
        "NO_PROXY",
        " .internal.test, Exact.Example.Test:8080 ,*.corp.test,10.0.0.5,,",
    )]);
    let rules = &settings.no_proxy;
    assert!(bypasses_proxy("api.internal.test", rules));
    assert!(bypasses_proxy("internal.test", rules));
    assert!(bypasses_proxy("exact.example.test", rules));
    assert!(bypasses_proxy("deep.sub.corp.test", rules));
    assert!(bypasses_proxy("10.0.0.5", rules));
    assert!(!bypasses_proxy("badinternal.test", rules));
    assert!(!bypasses_proxy("other.example.test", rules));
    assert!(!bypasses_proxy("110.0.0.5", rules));
    assert!(!bypasses_proxy("api.anthropic.com", rules));

    // `*` 는 전부 우회, 소문자 no_proxy 도 판독
    let star = settings_from(&[("no_proxy", "*")]);
    assert!(bypasses_proxy("api.anthropic.com", &star.no_proxy));
    assert!(!bypasses_proxy("api.anthropic.com", &[]));
}

// 6. 루프백 우회 --------------------------------------------------------------
#[test]
fn loopback_hosts_always_bypass() {
    for host in ["localhost", "LOCALHOST", "127.0.0.1", "127.9.9.9", "[::1]"] {
        assert!(bypasses_proxy(host, &[]), "{host}");
    }
    assert!(!bypasses_proxy("192.168.0.1", &[]));
}

// 7. Base64 -------------------------------------------------------------------
#[test]
fn base64_matches_rfc4648_vectors() {
    for (plain, expected) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(encode_base64(plain.as_bytes()), expected, "{plain}");
    }
}

// 8. Debug 에 자격 증명 미노출 ------------------------------------------------
#[test]
fn debug_output_redacts_proxy_credentials() {
    let settings = settings_from(&[(
        "HTTPS_PROXY",
        "http://user:dummy-pw@proxy.example.test:3128",
    )]);
    let printed = format!("{settings:?}");
    assert!(printed.contains("<redacted>"), "{printed}");
    assert!(!printed.contains("dummy-pw"), "{printed}");
    assert!(!printed.contains("dXNlcjpzZWNyZXRwdw"), "{printed}");
}

// 9. CONNECT 터널 (요청 머리, 인증, 터널 본문) --------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connector_opens_tunnel_with_basic_auth() {
    let (port, proxy) = spawn_fake_proxy(TUNNEL_OK, "tunnel-bytes").await;
    let proxy_url = format!("http://user:dummy%40pw@127.0.0.1:{port}");
    let mut connector = connector_from(&[("HTTPS_PROXY", &proxy_url)]);

    let mut stream = connector
        .call(
            "https://api.example.test:8443/v1/messages"
                .parse()
                .expect("주소"),
        )
        .await
        .expect("터널 연결");
    let mut received = [0_u8; 12];
    stream
        .inner_mut()
        .read_exact(&mut received)
        .await
        .expect("터널 본문 읽기");
    assert_eq!(&received, b"tunnel-bytes");
    drop(stream);

    let request = proxy.await.expect("프록시 작업");
    assert!(
        request.starts_with("CONNECT api.example.test:8443 HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(
        request.contains("\r\nHost: api.example.test:8443\r\n"),
        "{request}"
    );
    assert!(
        request.contains("\r\nProxy-Authorization: Basic dXNlcjpkdW1teUBwdw==\r\n"),
        "{request}"
    );
    assert!(request.ends_with("\r\n\r\n"));
}

// 10. 포트 생략과 인증 생략 ---------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connector_defaults_to_port_443_without_auth() {
    let (port, proxy) = spawn_fake_proxy(TUNNEL_OK, "").await;
    let proxy_url = format!("http://127.0.0.1:{port}");
    let mut connector = connector_from(&[("HTTPS_PROXY", &proxy_url)]);

    connector
        .call("https://api.example.test".parse().expect("주소"))
        .await
        .expect("터널 연결");
    let request = proxy.await.expect("프록시 작업");
    assert!(
        request.starts_with("CONNECT api.example.test:443 HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(!request.contains("Proxy-Authorization"), "{request}");
}

// 11. 프록시 우회 (프록시 미설정, 루프백 목적지) ------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connector_connects_directly_when_no_proxy_applies() {
    let target = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("목적지 바인드");
    let target_port = target.local_addr().expect("목적지 주소").port();
    let silent_proxy = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("프록시 바인드");
    let proxy_port = silent_proxy.local_addr().expect("프록시 주소").port();
    let proxy_url = format!("http://127.0.0.1:{proxy_port}");
    let dst: hyper::Uri = format!("https://127.0.0.1:{target_port}")
        .parse()
        .expect("주소");

    // 1. 프록시 미설정: 목적지로 직접 연결
    let mut unset = connector_from(&[]);
    unset.call(dst.clone()).await.expect("직접 연결");
    tokio::time::timeout(Duration::from_secs(2), target.accept())
        .await
        .expect("목적지 수락 대기")
        .expect("목적지 수락");

    // 2. 프록시 설정 + 루프백 목적지: 프록시 연결 시도 없이 직접 연결
    let mut bypass = connector_from(&[("HTTPS_PROXY", &proxy_url)]);
    bypass.call(dst).await.expect("우회 직접 연결");
    tokio::time::timeout(Duration::from_secs(2), target.accept())
        .await
        .expect("목적지 수락 대기")
        .expect("목적지 수락");
    let proxy_hit = tokio::time::timeout(Duration::from_millis(200), silent_proxy.accept()).await;
    assert!(proxy_hit.is_err(), "프록시로 연결이 가면 안 됨");
}

// 12. 프록시 거부 (407) 와 잘못된 주소 ----------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connector_fails_on_refused_connect_and_invalid_proxy() {
    // 1. 407 거부
    let (port, proxy) = spawn_fake_proxy(PROXY_REFUSED, "").await;
    let proxy_url = format!("http://user:dummy-pw@127.0.0.1:{port}");
    let mut refused = connector_from(&[("HTTPS_PROXY", &proxy_url)]);
    let Err(error) = refused
        .call("https://api.example.test".parse().expect("주소"))
        .await
    else {
        panic!("407 은 연결 실패여야 함");
    };
    assert!(error.to_string().contains("407"), "{error}");
    assert!(!error.to_string().contains("dummy-pw"), "{error}");
    proxy.await.expect("프록시 작업");

    // 2. https 프록시 주소: 직접 연결로 우회하지 않고 거부
    let mut invalid = connector_from(&[("HTTPS_PROXY", "https://proxy.example.test:8443")]);
    let Err(error) = invalid
        .call("https://api.example.test".parse().expect("주소"))
        .await
    else {
        panic!("잘못된 프록시 주소는 연결 실패여야 함");
    };
    assert!(error.to_string().contains("only http://"), "{error}");
}

// 13. 프록시 거부 시 게이트웨이 502 -------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_returns_502_when_proxy_refuses_connect() {
    // 1. 407 을 돌려주는 가짜 프록시와 그 프록시를 쓰는 게이트웨이
    let (proxy_port, proxy) = spawn_fake_proxy(PROXY_REFUSED, "").await;
    let mut config = build_test_config("gw-proxy-407");
    config.gateway.upstream = "https://api.example.test".to_string();
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let proxy_url = format!("http://user:dummy-pw@127.0.0.1:{proxy_port}");
    let state = build_gateway_state_with_proxy(
        Arc::clone(&ctx),
        settings_from(&[("HTTPS_PROXY", &proxy_url)]),
    )
    .expect("게이트웨이 상태");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("게이트웨이 바인드");
    let port = listener.local_addr().expect("게이트웨이 주소").port();
    let server = tokio::spawn(serve_local_listener(listener, move |req| {
        let state = Arc::clone(&state);
        async move { proxy_request(state, req).await }
    }));

    // 2. 메시지 요청은 502 이고 자격 증명은 응답에 없음
    let (status, text) = post_raw(
        port,
        "/v1/messages",
        &[("Content-Type", "application/json")],
        br#"{"model":"claude-test","stream":true,"messages":[]}"#,
    )
    .await;
    assert_eq!(status, 502, "{text}");
    assert!(text.contains("cm gateway upstream error"), "{text}");
    assert!(text.contains("407"), "{text}");
    assert!(!text.contains("dummy-pw"), "{text}");
    assert!(!text.contains("dXNlcjpkdW1teS1wdw"), "{text}");
    let request = proxy.await.expect("프록시 작업");
    assert!(
        request.contains("Proxy-Authorization: Basic dXNlcjpkdW1teS1wdw=="),
        "{request}"
    );

    // 3. 오류 행 기록 대기
    let mut error_rows = 0_i64;
    for _ in 0..100 {
        error_rows = ctx
            .store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM api_requests WHERE status = 502",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap_or(0);
        if error_rows > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert_eq!(error_rows, 1);
    server.abort();
    let _ = server.await;
}
