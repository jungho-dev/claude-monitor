//! net/gateway/tap.rs
//! 응답 본문 계측: 첫 바이트·첫 토큰 시각, SSE 사용량 추출, 종료·중단 시 기록 (본문 미보관).

use crate::core::context::AppContext;
use crate::core::model::{ApiRequestRow, IngestBatch, LiveEvent, SessionRow};
use crate::net::common::{BoxError, ResponseBody};
use bytes::Bytes;
use hyper::body::{Body, Frame, SizeHint};
use serde::Deserialize;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};
use std::time::Instant;

const MAX_PENDING_LINE_BYTES: usize = 1024 * 1024;
const MAX_JSON_TAIL_BYTES: usize = 1024 * 1024;

// 1. 사용량 집계 --------------------------------------------------------------
#[derive(Debug, Default, Clone)]
pub struct UsageTally {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
}

// 2. 응답 JSON 탐침 형태 ------------------------------------------------------
#[derive(Debug, Default, Deserialize)]
struct SseDataProbe {
    message: Option<MessageProbe>,
    usage: Option<UsageProbe>,
}

#[derive(Debug, Default, Deserialize)]
struct MessageProbe {
    model: Option<String>,
    usage: Option<UsageProbe>,
}

#[derive(Debug, Default, Deserialize)]
struct UsageProbe {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
}

// 3. 사용량 병합 --------------------------------------------------------------
fn merge_usage(tally: &mut UsageTally, usage: &UsageProbe) {
    if let Some(tokens) = usage.input_tokens {
        tally.input_tokens = tokens;
    }
    if let Some(tokens) = usage.output_tokens {
        tally.output_tokens = tokens;
    }
    if let Some(tokens) = usage.cache_read_input_tokens {
        tally.cache_read_tokens = tokens;
    }
    if let Some(tokens) = usage.cache_creation_input_tokens {
        tally.cache_creation_tokens = tokens;
    }
}

// 4. SSE 파서 -----------------------------------------------------------------
// 청크 경계와 무관하게 줄 단위 조립, event 줄 이름으로 분기
#[derive(Debug, Default)]
pub struct SseUsageParser {
    pending: Vec<u8>,
    current_event: String,
    disabled: bool,
    pub saw_first_delta: bool,
    pub tally: UsageTally,
}

impl SseUsageParser {
    pub fn feed(&mut self, chunk: &[u8]) {
        if self.disabled {
            return;
        }
        self.pending.extend_from_slice(chunk);
        let Self {
            pending,
            current_event,
            saw_first_delta,
            tally,
            ..
        } = &mut *self;

        // 1. 완성된 줄 처리
        let mut consumed = 0usize;
        while let Some(offset) = pending[consumed..].iter().position(|byte| *byte == b'\n') {
            let line_end = consumed + offset;
            let mut line = &pending[consumed..line_end];
            consumed = line_end + 1;
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            if line.is_empty() {
                current_event.clear();
                continue;
            }
            if let Some(name) = line.strip_prefix(b"event:") {
                *current_event = String::from_utf8_lossy(name).trim().to_string();
                continue;
            }
            let Some(data) = line.strip_prefix(b"data:") else {
                continue;
            };
            match current_event.as_str() {
                "content_block_delta" => *saw_first_delta = true,
                "message_start" | "message_delta" => {
                    if let Ok(probe) = serde_json::from_slice::<SseDataProbe>(data) {
                        if let Some(message) = probe.message {
                            if let Some(model) = message.model {
                                tally.model = model;
                            }
                            if let Some(usage) = message.usage {
                                merge_usage(tally, &usage);
                            }
                        }
                        if let Some(usage) = probe.usage {
                            merge_usage(tally, &usage);
                        }
                    }
                }
                _ => {}
            }
        }
        pending.drain(..consumed);

        // 2. 줄 길이 상한 (초과 시 파싱 중단)
        if pending.len() > MAX_PENDING_LINE_BYTES {
            pending.clear();
            self.disabled = true;
        }
    }
}

// 5. 요청 추적 ----------------------------------------------------------------
// 드롭 시 1회 기록 (정상 종료·중단·상류 오류 공통)
pub struct ResponseTap {
    ctx: Arc<AppContext>,
    row: ApiRequestRow,
    started: Instant,
    stream_requested: bool,
    first_byte_ms: Option<i64>,
    first_token_ms: Option<i64>,
    finished_ms: Option<i64>,
    sse: Option<SseUsageParser>,
    json_tail: Option<Vec<u8>>,
}

// 6. 추적 시작 ----------------------------------------------------------------
pub fn start_response_tap(
    ctx: Arc<AppContext>,
    row: ApiRequestRow,
    stream_requested: bool,
) -> ResponseTap {
    ctx.counters
        .gateway_active_streams
        .fetch_add(1, Ordering::Relaxed);
    ResponseTap {
        ctx,
        row,
        started: Instant::now(),
        stream_requested,
        first_byte_ms: None,
        first_token_ms: None,
        finished_ms: None,
        sse: None,
        json_tail: None,
    }
}

impl ResponseTap {
    // 6-1. 응답 머리 반영
    pub fn set_response_meta(&mut self, status: u16, content_type: &str, encoded: bool) {
        self.row.status = Some(i64::from(status));
        if encoded {
            return;
        }
        if content_type.contains("text/event-stream") {
            self.sse = Some(SseUsageParser::default());
        } else if content_type.contains("application/json")
            && !self.stream_requested
            && (200..300).contains(&status)
        {
            self.json_tail = Some(Vec::new());
        }
    }

    // 6-2. 본문 조각 관측
    pub fn observe_chunk(&mut self, chunk: &[u8]) {
        let elapsed = elapsed_millis(self.started);
        if self.first_byte_ms.is_none() {
            self.first_byte_ms = Some(elapsed);
        }
        if let Some(parser) = self.sse.as_mut() {
            parser.feed(chunk);
            if parser.saw_first_delta && self.first_token_ms.is_none() {
                self.first_token_ms = Some(elapsed);
            }
        }
        if let Some(tail) = self.json_tail.as_mut() {
            if tail.len() + chunk.len() > MAX_JSON_TAIL_BYTES {
                self.json_tail = None;
            } else {
                tail.extend_from_slice(chunk);
            }
        }
    }
}

// 7. 경과 시간 ----------------------------------------------------------------
fn elapsed_millis(started: Instant) -> i64 {
    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

// 8. 요청 기록 ----------------------------------------------------------------
impl Drop for ResponseTap {
    fn drop(&mut self) {
        // 1. 활성 스트림 감소
        self.ctx
            .counters
            .gateway_active_streams
            .fetch_sub(1, Ordering::Relaxed);

        // 2. 사용량 확정
        let tally = if let Some(parser) = self.sse.take() {
            parser.tally
        } else if let Some(tail) = self.json_tail.take() {
            let mut tally = UsageTally::default();
            if let Ok(probe) = serde_json::from_slice::<MessageProbe>(&tail) {
                if let Some(model) = probe.model {
                    tally.model = model;
                }
                if let Some(usage) = probe.usage {
                    merge_usage(&mut tally, &usage);
                }
            }
            tally
        } else {
            UsageTally::default()
        };

        // 3. 행 완성
        let mut row = std::mem::take(&mut self.row);
        if !tally.model.is_empty() {
            row.model = tally.model;
        }
        row.input_tokens = tally.input_tokens;
        row.output_tokens = tally.output_tokens;
        row.cache_read_tokens = tally.cache_read_tokens;
        row.cache_creation_tokens = tally.cache_creation_tokens;
        row.duration_ms = Some(
            self.finished_ms
                .unwrap_or_else(|| elapsed_millis(self.started)),
        );
        row.first_byte_ms = self.first_byte_ms;
        row.first_token_ms = self.first_token_ms;

        // 4. 실시간 이벤트
        let _ = self.ctx.live_tx.send(LiveEvent::Api(row.clone()));

        // 5. 저장 (런타임 안이면 블로킹 풀)
        let mut batch = IngestBatch::default();
        if !row.session_id.is_empty() {
            batch.sessions.push(SessionRow {
                session_id: row.session_id.clone(),
                project: String::new(),
                first_ts: row.ts.clone(),
                last_ts: row.ts.clone(),
            });
        }
        batch.api_requests.push(row);
        let ctx = Arc::clone(&self.ctx);
        let insert = move || {
            if let Err(err) = ctx.store.insert_batch(&batch) {
                eprintln!("[rap] 게이트웨이 행 저장 실패: {err:#}");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => drop(handle.spawn_blocking(insert)),
            Err(_) => insert(),
        }
    }
}

// 9. 계측 본문 ----------------------------------------------------------------
pub struct TapBody {
    inner: ResponseBody,
    tap: ResponseTap,
}

impl TapBody {
    pub fn new(inner: ResponseBody, tap: ResponseTap) -> Self {
        Self { inner, tap }
    }
}

impl Body for TapBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(chunk) = frame.data_ref() {
                    this.tap.observe_chunk(chunk);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(None) => {
                if this.tap.finished_ms.is_none() {
                    this.tap.finished_ms = Some(elapsed_millis(this.tap.started));
                }
                Poll::Ready(None)
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
