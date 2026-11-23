//! net/admin/mod.rs
//! 관리 포트: 상태·집계 JSON, 토큰 보호 재색인·리포트 실행 (SSE 는 events, 클라이언트는 client).

mod client;
mod events;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

pub use client::{fetch_status_blocking, post_admin_blocking};

use crate::core::context::AppContext;
use crate::core::model::{PortStatus, StatusSnapshot};
use crate::net::common::{
    ResponseBody, full_body, json_response, serve_local_http, serve_local_listener, text_response,
};
use crate::serve::BOUND_LISTENER_NAMES;
use crate::{report, transcript};
use anyhow::Context;
use chrono::{Duration as ChronoDuration, Local, NaiveDate, SecondsFormat, Utc};
use hyper::body::Incoming;
use hyper::header::{CONTENT_TYPE, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use ring::hmac;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const TOKEN_HEADER: &str = "x-cm-token";
const DEFAULT_HOURS: i64 = 24;
const MAX_HOURS: i64 = 720;
const AGGREGATE_ROW_LIMIT: usize = 100;
const PORT_PROBE_TIMEOUT: Duration = Duration::from_millis(50);

// 0. 리포트 요청 대상 ---------------------------------------------------------
// Week 는 해당 주의 월요일
#[derive(Debug, Clone, Copy)]
enum ReportTarget {
    Day(NaiveDate),
    Week(NaiveDate),
}

// 1. 관리 서버 ----------------------------------------------------------------
pub async fn run_admin_server(ctx: Arc<AppContext>) -> anyhow::Result<()> {
    let port = ctx.config.ports.admin;
    serve_local_http(port, move |req| {
        let ctx = Arc::clone(&ctx);
        async move { route_admin_request(ctx, req).await }
    })
    .await
}

/// 선점한 std 리스너로 관리 서버 실행 (기동 직전 포트 검사 틈 제거).
pub async fn run_admin_server_on(
    ctx: Arc<AppContext>,
    listener: std::net::TcpListener,
) -> anyhow::Result<()> {
    listener
        .set_nonblocking(true)
        .context("관리 리스너 비차단 전환 실패")?;
    let listener =
        tokio::net::TcpListener::from_std(listener).context("관리 리스너 런타임 등록 실패")?;
    serve_local_listener(listener, move |req| {
        let ctx = Arc::clone(&ctx);
        async move { route_admin_request(ctx, req).await }
    })
    .await
}

// 2. 요청 분기 ----------------------------------------------------------------
async fn route_admin_request(
    ctx: Arc<AppContext>,
    req: Request<Incoming>,
) -> Response<ResponseBody> {
    // 1. 요청 분해
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().map(str::to_string);

    // 2. 변경 요청 토큰 검사 (상수 시간 비교)
    if method == Method::POST {
        let given = req
            .headers()
            .get(TOKEN_HEADER)
            .map(|value| value.as_bytes())
            .unwrap_or_default();
        if !verify_admin_token(given, &ctx.admin_token) {
            return json_response(
                StatusCode::UNAUTHORIZED,
                &serde_json::json!({"error": "unauthorized"}),
            );
        }
    }

    // 3. 경로 분기
    match (method, path.as_str()) {
        (Method::GET, "/health") => text_response(StatusCode::OK, "ok"),
        (Method::GET, "/api/status") => respond_status(&ctx).await,
        (Method::GET, "/api/events") => events::open_event_stream(&ctx),
        (Method::GET, "/api/report") => respond_report_read(ctx, query).await,
        (Method::POST, "/api/report") => respond_report_write(ctx, query).await,
        (Method::POST, "/api/reindex") => respond_reindex(ctx).await,
        (Method::GET, "/api/power") => json_response(
            StatusCode::OK,
            &serde_json::json!({
                "enabled": ctx.config.power.enabled,
                "keep_awake": ctx.config.power.keep_awake,
                "holding": ctx.power.holding.load(Ordering::Relaxed),
                "gaps": ctx.power.list_gaps(),
            }),
        ),
        (Method::GET, other) if other.starts_with("/api/") => {
            respond_aggregate(ctx, &other["/api/".len()..], query).await
        }
        _ => json_response(
            StatusCode::NOT_FOUND,
            &serde_json::json!({"error": "not found"}),
        ),
    }
}

// 3. 상태 응답 ----------------------------------------------------------------
async fn respond_status(ctx: &Arc<AppContext>) -> Response<ResponseBody> {
    // 1. 포트 목록
    let planned = ctx.config.planned_ports();

    // 2. 수신 검사 (관리 포트는 항상 수신 중, 바인드 기록이 있으면 기록 기준)
    let probed = tokio::task::spawn_blocking(move || {
        build_port_rows(planned, BOUND_LISTENER_NAMES.get().map(Vec::as_slice))
    })
    .await;
    let port_rows = match probed {
        Ok(rows) => rows,
        Err(_) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &serde_json::json!({"error": "포트 검사 실패"}),
            );
        }
    };

    // 3. 스냅샷 구성
    let snapshot = StatusSnapshot {
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        started_at: ctx.started_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        uptime_secs: u64::try_from((Utc::now() - ctx.started_at).num_seconds()).unwrap_or(0),
        data_dir: ctx.config.data_dir.display().to_string(),
        claude_home: ctx.config.claude_home.display().to_string(),
        ports: port_rows,
        counters: ctx.counters.snapshot_counters(),
    };
    json_response(StatusCode::OK, &snapshot)
}

// 3-1. 포트 행 구성 -----------------------------------------------------------
/// 바인드 기록이 있으면 이 프로세스가 잡은 포트만 수신 중, 없으면 연결 시도로 판정.
fn build_port_rows(
    planned: Vec<(&'static str, u16, bool)>,
    bound_names: Option<&[&'static str]>,
) -> Vec<PortStatus> {
    planned
        .into_iter()
        .map(|(name, port, enabled)| {
            let listening = if name == "admin" {
                true
            } else if !enabled {
                false
            } else if let Some(names) = bound_names {
                names.contains(&name)
            } else {
                TcpStream::connect_timeout(
                    &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                    PORT_PROBE_TIMEOUT,
                )
                .is_ok()
            };
            PortStatus {
                name: name.to_string(),
                port,
                enabled,
                listening,
            }
        })
        .collect()
}

// 4. 집계 응답 ----------------------------------------------------------------
async fn respond_aggregate(
    ctx: Arc<AppContext>,
    name: &str,
    query: Option<String>,
) -> Response<ResponseBody> {
    // 1. 구간 계산
    let hours = match find_query_param(query.as_deref(), "hours") {
        Some(text) => match text.parse::<i64>() {
            Ok(parsed) => parsed.clamp(1, MAX_HOURS),
            Err(_) => {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &serde_json::json!({"error": "hours 형식 오류"}),
                );
            }
        },
        None => DEFAULT_HOURS,
    };
    let now = Utc::now();
    let until = now.to_rfc3339_opts(SecondsFormat::Millis, true);
    let since = (now - ChronoDuration::hours(hours)).to_rfc3339_opts(SecondsFormat::Millis, true);

    // 2. 조회 실행 (블로킹 풀)
    let name = name.to_string();
    let aggregated = run_blocking_job(move || -> anyhow::Result<Option<serde_json::Value>> {
        let payload = match name.as_str() {
            "overview" => serde_json::to_value(ctx.store.query_overview(&since, &until)?)?,
            "tools" => serde_json::to_value(ctx.store.query_tool_latency(
                &since,
                &until,
                AGGREGATE_ROW_LIMIT,
            )?)?,
            "hooks" => serde_json::to_value(ctx.store.query_hook_stats(&since, &until)?)?,
            "models" => serde_json::to_value(ctx.store.query_model_usage(&since, &until)?)?,
            "model-stats" => serde_json::to_value(ctx.store.query_model_stats(&since, &until)?)?,
            "agent-stats" => serde_json::to_value(ctx.store.query_agent_stats(&since, &until)?)?,
            "projects" => serde_json::to_value(ctx.store.query_project_stats(&since, &until)?)?,
            "limits" => serde_json::json!({
                "latest": ctx.store.latest_limit_snapshot()?,
                "history": ctx.store.limit_snapshots_since(&since, AGGREGATE_ROW_LIMIT)?,
            }),
            "ratelimit" => {
                // extra_json 은 저장된 JSON 문자열이라 응답에서는 객체로 풀어 내보냄
                let mut rows = serde_json::to_value(
                    ctx.store
                        .ratelimit_snapshots_since(&since, AGGREGATE_ROW_LIMIT)?,
                )?;
                if let Some(items) = rows.as_array_mut() {
                    for item in items {
                        let parsed = item
                            .get("extra_json")
                            .and_then(|extra| extra.as_str())
                            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok());
                        if let Some(parsed) = parsed {
                            item["extra_json"] = parsed;
                        }
                    }
                }
                rows
            }
            "gateway" => serde_json::to_value(ctx.store.query_gateway_timing(&since, &until)?)?,
            "cache-breaks" => serde_json::to_value(ctx.store.query_cache_breaks(
                &since,
                &until,
                AGGREGATE_ROW_LIMIT,
            )?)?,
            "minutes" => serde_json::to_value(ctx.store.query_minute_buckets(&since, &until)?)?,
            _ => return Ok(None),
        };
        Ok(Some(payload))
    })
    .await;

    // 3. 응답 분기
    match aggregated {
        Ok(Some(payload)) => json_response(StatusCode::OK, &payload),
        Ok(None) => json_response(
            StatusCode::NOT_FOUND,
            &serde_json::json!({"error": "not found"}),
        ),
        Err(failure) => *failure,
    }
}

// 5. 리포트 대상 해석 ---------------------------------------------------------
// date=YYYY-MM-DD (일일) 또는 week=YYYY-Www (주간), 둘 다 없으면 None
fn parse_report_target(query: Option<&str>) -> Result<Option<ReportTarget>, &'static str> {
    let date_text = find_query_param(query, "date");
    let week_text = find_query_param(query, "week");
    match (date_text, week_text) {
        (Some(_), Some(_)) => Err("date 와 week 는 함께 쓸 수 없음"),
        (Some(text), None) => NaiveDate::parse_from_str(&text, "%Y-%m-%d")
            .map(|date| Some(ReportTarget::Day(date)))
            .map_err(|_| "date=YYYY-MM-DD 형식 오류"),
        (None, Some(text)) => report::parse_week_key(&text)
            .map(|week_start| Some(ReportTarget::Week(week_start)))
            .ok_or("week=YYYY-Www 형식 오류"),
        (None, None) => Ok(None),
    }
}

// 5-1. 리포트 조회 응답 -------------------------------------------------------
async fn respond_report_read(
    ctx: Arc<AppContext>,
    query: Option<String>,
) -> Response<ResponseBody> {
    // 1. 대상 검증
    let report_key = match parse_report_target(query.as_deref()) {
        Ok(Some(ReportTarget::Day(date))) => date.format("%Y-%m-%d").to_string(),
        Ok(Some(ReportTarget::Week(week_start))) => report::format_week_key(week_start),
        Ok(None) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": "date=YYYY-MM-DD 또는 week=YYYY-Www 필요"}),
            );
        }
        Err(message) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": message}),
            );
        }
    };

    // 2. 보관본 조회
    let loaded = run_blocking_job(move || ctx.store.load_daily_report(&report_key)).await;

    // 3. 응답 분기
    match loaded {
        Ok(Some(markdown)) => {
            let mut response = Response::new(full_body(markdown));
            response.headers_mut().insert(
                CONTENT_TYPE,
                HeaderValue::from_static("text/markdown; charset=utf-8"),
            );
            response
        }
        Ok(None) => json_response(
            StatusCode::NOT_FOUND,
            &serde_json::json!({"error": "리포트 없음"}),
        ),
        Err(failure) => *failure,
    }
}

// 6. 리포트 생성 응답 ---------------------------------------------------------
async fn respond_report_write(
    ctx: Arc<AppContext>,
    query: Option<String>,
) -> Response<ResponseBody> {
    // 1. 대상 결정 (기본 어제 일일)
    let target = match parse_report_target(query.as_deref()) {
        Ok(Some(target)) => target,
        Ok(None) => ReportTarget::Day((Local::now() - ChronoDuration::days(1)).date_naive()),
        Err(message) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": message}),
            );
        }
    };

    // 2. 생성 실행 (블로킹 풀)
    let written = run_blocking_job(move || match target {
        ReportTarget::Day(date) => report::write_daily_report(&ctx.config, &ctx.store, date),
        ReportTarget::Week(week_start) => {
            report::write_weekly_report(&ctx.config, &ctx.store, week_start)
        }
    })
    .await;

    // 3. 응답 분기
    match written {
        Ok(path) => json_response(
            StatusCode::OK,
            &serde_json::json!({"path": path.display().to_string()}),
        ),
        Err(failure) => *failure,
    }
}

// 7. 재색인 응답 --------------------------------------------------------------
async fn respond_reindex(ctx: Arc<AppContext>) -> Response<ResponseBody> {
    let indexed =
        run_blocking_job(move || transcript::index_transcripts_once(&ctx.config, &ctx.store)).await;
    match indexed {
        Ok(summary) => json_response(StatusCode::OK, &summary),
        Err(failure) => *failure,
    }
}

// 8. 토큰 비교 ----------------------------------------------------------------
// HMAC 태그 비교 (ring 상수 시간 검증, 길이 누설 없음)
fn verify_admin_token(given: &[u8], expected: &str) -> bool {
    let key = hmac::Key::new(hmac::HMAC_SHA256, expected.as_bytes());
    let expected_tag = hmac::sign(&key, expected.as_bytes());
    hmac::verify(&key, given, expected_tag.as_ref()).is_ok()
}

// 9. 질의 값 추출 -------------------------------------------------------------
fn find_query_param(query: Option<&str>, key: &str) -> Option<String> {
    query?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value.to_string())
}

// 10. 블로킹 작업 실행 --------------------------------------------------------
// 오류·panic 은 500 응답으로 반환 (응답 값이 커서 오류 쪽은 Box, clippy::result_large_err)
async fn run_blocking_job<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, Box<Response<ResponseBody>>> {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(err)) => Err(Box::new(json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({"error": format!("{err:#}")}),
        ))),
        Err(_) => Err(Box::new(json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({"error": "내부 오류 (작업 중단)"}),
        ))),
    }
}
