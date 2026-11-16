//! net/admin/test_support.rs
//! 네트워크 테스트 공용 준비물 (임시 저장소·컨텍스트, 빈 포트, 원시 HTTP 요청).

use crate::core::config::{
    AppConfig, DB_FILE_NAME, GatewayConfig, HookConfig, IndexerConfig, PortConfig, PowerConfig,
    ReportConfig,
};
use crate::core::context::{AppContext, LIVE_CHANNEL_CAPACITY, LiveCounters};
use crate::power::PowerState;
use crate::store::Store;
use chrono::Utc;
use std::net::TcpListener as StdTcpListener;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;

pub const TEST_ADMIN_TOKEN: &str = "test-admin-token-0123456789abcdef";

// 1. 테스트 설정 --------------------------------------------------------------
pub fn build_test_config(label: &str) -> AppConfig {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let data_dir =
        std::env::temp_dir().join(format!("cm-test-{label}-{}-{nanos}", std::process::id()));
    let held: Vec<StdTcpListener> = (0..4)
        .map(|_| StdTcpListener::bind("127.0.0.1:0").expect("임시 포트 바인드"))
        .collect();
    let free: Vec<u16> = held
        .iter()
        .map(|listener| listener.local_addr().expect("임시 포트 주소").port())
        .collect();
    drop(held);
    AppConfig {
        claude_home: data_dir.join("claude"),
        data_dir,
        ports: PortConfig {
            admin: free[0],
            otlp: free[1],
            gateway: free[2],
            hooks: free[3],
        },
        gateway: GatewayConfig::default(),
        hooks: HookConfig::default(),
        indexer: IndexerConfig::default(),
        report: ReportConfig::default(),
        power: PowerConfig::default(),
        source_path: None,
    }
}

// 2. 테스트 컨텍스트 ----------------------------------------------------------
pub fn build_test_context(config: AppConfig) -> Arc<AppContext> {
    let store = Store::open_store(&config.data_dir.join(DB_FILE_NAME)).expect("임시 저장소 열기");
    let (live_tx, _) = broadcast::channel(LIVE_CHANNEL_CAPACITY);
    Arc::new(AppContext {
        config,
        store,
        live_tx,
        counters: LiveCounters::default(),
        started_at: Utc::now(),
        admin_token: TEST_ADMIN_TOKEN.to_string(),
        power: PowerState::default(),
    })
}

// 3. 빈 포트 선정 -------------------------------------------------------------
pub fn pick_free_port() -> u16 {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("임시 포트 바인드");
    listener.local_addr().expect("임시 포트 주소").port()
}

// 4. 수신 대기 ----------------------------------------------------------------
pub async fn wait_until_listening(port: u16) {
    for _ in 0..200 {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("포트 {port} 수신 시작 안 됨");
}

// 5. 원시 POST 요청 -----------------------------------------------------------
pub async fn post_raw(
    port: u16,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (u16, String) {
    let (status, _, body_text) = send_raw("POST", port, path, headers, body).await;
    (status, body_text)
}

// 6. 원시 요청 ----------------------------------------------------------------
pub async fn send_raw(
    method: &str,
    port: u16,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (u16, String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("테스트 서버 연결");
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("요청 헤더 전송");
    stream.write_all(body).await.expect("요청 본문 전송");

    // 본문 완결 또는 EOF 까지 수신 (Content-Length 기준)
    let mut raw = Vec::new();
    let mut buffer = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let read = tokio::time::timeout_at(deadline, stream.read(&mut buffer)).await;
        let count = read.expect("응답 대기 시간 초과").unwrap_or(0);
        if count == 0 {
            break;
        }
        raw.extend_from_slice(&buffer[..count]);
        let Some(header_end) = raw.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&raw[..header_end]).to_ascii_lowercase();
        let length = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        if raw.len() >= header_end + 4 + length {
            break;
        }
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("상태 줄");
    let (head_text, body_text) = text
        .split_once("\r\n\r\n")
        .map(|(head, rest)| (head.to_ascii_lowercase(), rest.to_string()))
        .unwrap_or_default();
    (status, head_text, body_text)
}
