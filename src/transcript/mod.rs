//! transcript/mod.rs
//! Claude Code 세션 기록 (projects/**/*.jsonl) 증분 색인.

mod files;
mod lines;
mod scan;
#[cfg(test)]
pub mod test_support;
#[cfg(test)]
mod tests;

use crate::core::config::AppConfig;
use crate::core::context::AppContext;
use crate::core::model::{IngestBatch, IngestCounts, LiveEvent};
use crate::store::{IndexCursor, Store};
use chrono::{SecondsFormat, Utc};
use files::discover_transcript_files;
use scan::{ScanRequest, scan_transcript_file};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const COMMIT_CHUNK_ROWS: usize = 2000;
const LIVE_EVENT_CAP: usize = 200;
const DAY_MS: i64 = 86_400_000;

// 1. 색인 결과 ----------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexSummary {
    pub files_scanned: u64,
    pub files_changed: u64,
    pub bytes_read: u64,
    pub elapsed_ms: u64,
    pub counts: IngestCounts,
}

// 2. 커서 이어 읽기 상태 ------------------------------------------------------
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct CursorState {
    trigger_ms: Option<i64>,
}

// 3. 1회 색인 -----------------------------------------------------------------
pub fn index_transcripts_once(config: &AppConfig, store: &Store) -> anyhow::Result<IndexSummary> {
    let (summary, _) = index_transcripts_with_events(config, store, 0)?;
    Ok(summary)
}

// 4. 1회 색인과 실시간 이벤트 -------------------------------------------------
// event_cap 0 이면 이벤트 수집 생략
pub fn index_transcripts_with_events(
    config: &AppConfig,
    store: &Store,
    event_cap: usize,
) -> anyhow::Result<(IndexSummary, Vec<LiveEvent>)> {
    // 1. 대상 파일
    let started = Instant::now();
    let mut summary = IndexSummary::default();
    let mut stamped: Vec<(String, LiveEvent)> = Vec::new();
    let root = config.projects_dir();
    if !root.is_dir() {
        return Ok((summary, Vec::new()));
    }
    let cutoff_ms =
        Utc::now().timestamp_millis() - i64::from(config.indexer.backfill_days) * DAY_MS;
    let transcript_files = discover_transcript_files(&root, cutoff_ms);

    // 2. 파일별 증분 읽기
    for file in &transcript_files {
        summary.files_scanned += 1;
        let path_text = file.path.to_string_lossy().into_owned();
        let saved = store.load_index_cursor(&path_text)?;
        let (start_offset, trigger_ms) = match &saved {
            Some(cursor)
                if cursor.file_size == file.size && cursor.modified_ms == file.modified_ms =>
            {
                continue;
            }
            Some(cursor) if file.size >= cursor.read_offset => {
                let state: CursorState =
                    serde_json::from_str(&cursor.state_json).unwrap_or_default();
                (cursor.read_offset.max(0) as u64, state.trigger_ms)
            }
            _ => (0, None),
        };
        let request = ScanRequest {
            file,
            start_offset,
            trigger_ms,
        };
        let output = match scan_transcript_file(&request) {
            Ok(output) => output,
            Err(err) => {
                eprintln!("[cm] 색인 건너뜀: {err:#}");
                continue;
            }
        };

        // 3. 실시간 이벤트 후보
        if event_cap > 0 {
            for row in &output.batch.api_requests {
                stamped.push((row.ts.clone(), LiveEvent::Api(row.clone())));
            }
            for row in &output.batch.tool_calls {
                stamped.push((row.ts.clone(), LiveEvent::Tool(row.clone())));
            }
            for row in &output.batch.hook_events {
                stamped.push((row.ts.clone(), LiveEvent::Hook(row.clone())));
            }
        }

        // 4. 묶음 적재 (호출 시작 먼저, 완료는 이후)
        let mut batches: Vec<IngestBatch> = Vec::new();
        for rows in output.batch.tool_calls.chunks(COMMIT_CHUNK_ROWS) {
            batches.push(IngestBatch {
                tool_calls: rows.to_vec(),
                ..IngestBatch::default()
            });
        }
        let longest = [
            output.batch.api_requests.len(),
            output.batch.tool_completions.len(),
            output.batch.hook_events.len(),
            output.batch.sessions.len(),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        for from in (0..longest).step_by(COMMIT_CHUNK_ROWS) {
            batches.push(IngestBatch {
                api_requests: output
                    .batch
                    .api_requests
                    .iter()
                    .skip(from)
                    .take(COMMIT_CHUNK_ROWS)
                    .cloned()
                    .collect(),
                tool_completions: output
                    .batch
                    .tool_completions
                    .iter()
                    .skip(from)
                    .take(COMMIT_CHUNK_ROWS)
                    .cloned()
                    .collect(),
                hook_events: output
                    .batch
                    .hook_events
                    .iter()
                    .skip(from)
                    .take(COMMIT_CHUNK_ROWS)
                    .cloned()
                    .collect(),
                sessions: output
                    .batch
                    .sessions
                    .iter()
                    .skip(from)
                    .take(COMMIT_CHUNK_ROWS)
                    .cloned()
                    .collect(),
                ..IngestBatch::default()
            });
        }
        for chunk in &batches {
            let counts = store.insert_batch(chunk)?;
            summary.counts.api_requests += counts.api_requests;
            summary.counts.tool_calls += counts.tool_calls;
            summary.counts.tool_completions += counts.tool_completions;
            summary.counts.hook_events += counts.hook_events;
            summary.counts.sessions += counts.sessions;
        }

        // 5. 커서 저장
        let state_json = serde_json::to_string(&CursorState {
            trigger_ms: output.trigger_ms,
        })?;
        store.save_index_cursor(&IndexCursor {
            path: path_text,
            file_size: file.size,
            modified_ms: file.modified_ms,
            read_offset: output.end_offset as i64,
            state_json,
        })?;
        if output.bytes_read > 0 {
            summary.files_changed += 1;
            summary.bytes_read += output.bytes_read;
        }
    }

    // 6. 이벤트 정렬·상한
    stamped.sort_by(|left, right| left.0.cmp(&right.0));
    let skip = stamped.len().saturating_sub(event_cap);
    let events: Vec<LiveEvent> = stamped
        .into_iter()
        .skip(skip)
        .map(|(_, event)| event)
        .collect();
    summary.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok((summary, events))
}

// 5. 주기 색인 루프 -----------------------------------------------------------
pub async fn run_indexer_loop(ctx: Arc<AppContext>) -> anyhow::Result<()> {
    let interval = Duration::from_secs(ctx.config.indexer.interval_secs.max(1));
    let mut first_run = true;

    loop {
        // 1. 블로킹 색인 (첫 실행은 이벤트 생략)
        let event_cap = if first_run { 0 } else { LIVE_EVENT_CAP };
        first_run = false;
        let worker_ctx = Arc::clone(&ctx);
        let joined = tokio::task::spawn_blocking(move || {
            index_transcripts_with_events(&worker_ctx.config, &worker_ctx.store, event_cap)
        })
        .await;

        // 2. 결과 반영
        let failure = match joined {
            Ok(Ok((summary, events))) => {
                ctx.counters.index_runs.fetch_add(1, Ordering::Relaxed);
                ctx.counters
                    .api_requests
                    .fetch_add(summary.counts.api_requests, Ordering::Relaxed);
                ctx.counters
                    .tool_calls
                    .fetch_add(summary.counts.tool_calls, Ordering::Relaxed);
                for event in events {
                    let _ = ctx.live_tx.send(event);
                }
                None
            }
            Ok(Err(err)) => Some(format!("{err:#}")),
            Err(err) => Some(format!("색인 작업 중단: {err}")),
        };
        if let Some(text) = failure {
            let _ = ctx.live_tx.send(LiveEvent::Notice {
                ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                level: "error".to_string(),
                text: format!("기록 색인 실패: {text}"),
            });
        }

        // 3. 다음 주기 대기
        tokio::time::sleep(interval).await;
    }
}
