//! hooks/upstream.rs
//! 기존 훅 서버(가드)로 가는 keep-alive HTTP 클라이언트와 마감 시한 POST.

use anyhow::{Context, anyhow};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::header::CONTENT_TYPE;
use hyper::{Method, Request, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioTimer};
use std::time::Duration;

// 가드 응답 본문 상한
const MAX_UPSTREAM_BODY_BYTES: usize = 8 * 1024 * 1024;
// Bun 서버 유휴 종료보다 짧게 두어 끊긴 연결 재사용 방지
const POOL_IDLE_SECS: u64 = 5;

// 1. 업스트림 클라이언트 ------------------------------------------------------
pub struct UpstreamClient {
    client: Client<HttpConnector, Full<Bytes>>,
}

impl UpstreamClient {
    // 1-1. 연결 재사용 클라이언트 생성
    pub fn build_upstream_client() -> Self {
        let client = Client::builder(TokioExecutor::new())
            .pool_timer(TokioTimer::new())
            .pool_idle_timeout(Duration::from_secs(POOL_IDLE_SECS))
            .pool_max_idle_per_host(8)
            .build_http::<Full<Bytes>>();
        Self { client }
    }

    // 1-2. 마감 시한 JSON POST (상태 코드와 본문 반환)
    pub async fn post_json_with_deadline(
        &self,
        url: &str,
        body: Bytes,
        deadline: Duration,
    ) -> anyhow::Result<(StatusCode, Bytes)> {
        // 1. 요청 구성
        let request = Request::builder()
            .method(Method::POST)
            .uri(url)
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(body))
            .context("업스트림 요청 구성 실패")?;

        // 2. 시한 안 전송과 본문 수집
        let exchange = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|err| anyhow!("업스트림 연결 실패: {err}"))?;
            let status = response.status();
            let collected = Limited::new(response.into_body(), MAX_UPSTREAM_BODY_BYTES)
                .collect()
                .await
                .map_err(|err| anyhow!("업스트림 본문 읽기 실패: {err}"))?;
            Ok::<_, anyhow::Error>((status, collected.to_bytes()))
        };
        match tokio::time::timeout(deadline, exchange).await {
            Ok(outcome) => outcome,
            Err(_) => Err(anyhow!(
                "업스트림 응답 시한 초과 ({} ms)",
                deadline.as_millis()
            )),
        }
    }
}
