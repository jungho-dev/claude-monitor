//! net/admin/events.rs
//! 실시간 이벤트 SSE 스트림 (broadcast 구독을 채널 본문으로 무버퍼링 전달).

use crate::core::context::AppContext;
use crate::core::model::LiveEvent;
use crate::net::common::{BoxError, ResponseBody};
use bytes::Bytes;
use chrono::{SecondsFormat, Utc};
use http_body_util::BodyExt;
use hyper::body::{Body, Frame};
use hyper::header::{CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use hyper::{Response, StatusCode};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

const SSE_PING_SECS: u64 = 15;
const SSE_QUEUE_CAPACITY: usize = 64;

// 1. 채널 본문 ----------------------------------------------------------------
struct ChannelBody {
    receiver: mpsc::Receiver<Bytes>,
}

impl Body for ChannelBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.get_mut().receiver.poll_recv(cx) {
            Poll::Ready(Some(chunk)) => Poll::Ready(Some(Ok(Frame::data(chunk)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

// 2. SSE 스트림 개시 ----------------------------------------------------------
pub(super) fn open_event_stream(ctx: &Arc<AppContext>) -> Response<ResponseBody> {
    // 1. 구독과 채널
    let mut live_rx = ctx.live_tx.subscribe();
    let (sender, receiver) = mpsc::channel::<Bytes>(SSE_QUEUE_CAPACITY);
    let _ = sender.try_send(Bytes::from_static(b": connected\n\n"));

    // 2. 전달 태스크 (이벤트·15초 ping, 누락 시 Notice)
    tokio::spawn(async move {
        let mut ping = tokio::time::interval(Duration::from_secs(SSE_PING_SECS));
        ping.tick().await;
        loop {
            let chunk = tokio::select! {
                _ = sender.closed() => break,
                _ = ping.tick() => Bytes::from_static(b": ping\n\n"),
                received = live_rx.recv() => {
                    let event = match received {
                        Ok(event) => event,
                        Err(RecvError::Lagged(skipped)) => LiveEvent::Notice {
                            ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                            level: "warn".to_string(),
                            text: format!("실시간 이벤트 {skipped}건 누락"),
                        },
                        Err(RecvError::Closed) => break,
                    };
                    match serde_json::to_string(&event) {
                        Ok(json) => Bytes::from(format!("data: {json}\n\n")),
                        Err(_) => continue,
                    }
                }
            };
            if sender.send(chunk).await.is_err() {
                break;
            }
        }
    });

    // 3. 응답 구성
    let mut response = Response::new(ChannelBody { receiver }.boxed());
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}
