//! transcript/scan.rs
//! 기록 파일 한 개의 오프셋 이후 완결 줄을 읽어 적재 묶음으로 변환.

use crate::core::model::{
    ApiRequestRow, DataSource, HookEventRow, HookOutcome, IngestBatch, SessionRow, ToolCallRow,
    ToolCompletionRow,
};
use crate::transcript::files::TranscriptFile;
use crate::transcript::lines::{TranscriptLine, parse_denial_text};
use anyhow::Context;
use chrono::DateTime;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};

const READ_BUFFER_BYTES: usize = 256 * 1024;
const BLOB_LINE_PREFIX: &[u8] = b"{\"type\":\"api-request";
const AGENT_ID_CHARS: usize = 8;

// 1. 읽기 요청 ----------------------------------------------------------------
#[derive(Debug)]
pub struct ScanRequest<'a> {
    pub file: &'a TranscriptFile,
    pub start_offset: u64,
    // 직전 트리거 줄 시각 (이어 읽기 상태)
    pub trigger_ms: Option<i64>,
}

// 2. 읽기 결과 ----------------------------------------------------------------
#[derive(Debug, Default)]
pub struct ScanOutput {
    pub batch: IngestBatch,
    pub end_offset: u64,
    pub trigger_ms: Option<i64>,
    pub bytes_read: u64,
}

// 3. 줄 해석 문맥 -------------------------------------------------------------
struct LineContext {
    project: String,
    is_subagent: bool,
    agent_label: String,
    fallback_session: String,
}

// 4. 대기 중 요청 -------------------------------------------------------------
struct PendingRequest {
    row: ApiRequestRow,
    trigger_ms: Option<i64>,
}

// 5. 읽기 상태 ----------------------------------------------------------------
#[derive(Default)]
struct ScanState {
    trigger_ms: Option<i64>,
    pending: Vec<PendingRequest>,
    pending_index: HashMap<String, usize>,
    tool_calls: Vec<ToolCallRow>,
    tool_completions: Vec<ToolCompletionRow>,
    hook_events: Vec<HookEventRow>,
    tool_names: HashMap<String, String>,
    sessions: HashMap<String, (String, String)>,
}

// 6. 하위 에이전트 메타 -------------------------------------------------------
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AgentMeta {
    agent_type: Option<String>,
}

// 7. 파일 읽기 ----------------------------------------------------------------
// 마지막 완결 줄까지만 소비, 이후 불완전 줄은 다음 실행으로 넘김
pub fn scan_transcript_file(request: &ScanRequest) -> anyhow::Result<ScanOutput> {
    // 1. 문맥 결정
    let file = request.file;
    let stem = file
        .path
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut context = LineContext {
        project: file.project.clone(),
        is_subagent: file.is_subagent,
        agent_label: "main".to_string(),
        fallback_session: stem.clone(),
    };
    if file.is_subagent {
        let agent_id: String = stem
            .trim_start_matches("agent-")
            .chars()
            .take(AGENT_ID_CHARS)
            .collect();
        let meta_text =
            std::fs::read_to_string(file.path.with_extension("meta.json")).unwrap_or_default();
        let meta: AgentMeta = serde_json::from_str(&meta_text).unwrap_or_default();
        let agent_type = meta.agent_type.unwrap_or_else(|| "subagent".to_string());
        context.agent_label = format!("{agent_type}#{agent_id}");
        context.fallback_session = file
            .path
            .parent()
            .and_then(|dir| dir.parent())
            .and_then(|dir| dir.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
    }

    // 2. 오프셋 이동
    let source = File::open(&file.path)
        .with_context(|| format!("기록 파일 열기 실패: {}", file.path.display()))?;
    let mut reader = BufReader::with_capacity(READ_BUFFER_BYTES, source);
    reader.seek(SeekFrom::Start(request.start_offset))?;
    let mut state = ScanState {
        trigger_ms: request.trigger_ms,
        ..ScanState::default()
    };

    // 3. 완결 줄 순회
    let mut buffer: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut offset = request.start_offset;
    loop {
        buffer.clear();
        let read = reader.read_until(b'\n', &mut buffer)?;
        if read == 0 || buffer.last() != Some(&b'\n') {
            break;
        }
        offset += read as u64;

        // 사전 걸러내기 (요청 원문 덩어리, 무관 유형)
        if buffer.starts_with(BLOB_LINE_PREFIX) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&buffer) else {
            continue;
        };
        if !text.contains("\"type\":\"assistant\"") && !text.contains("\"type\":\"user\"") {
            continue;
        }
        let Ok(line) = serde_json::from_str::<TranscriptLine>(text) else {
            continue;
        };
        apply_line(&mut state, &context, line);
    }

    // 4. 결과 조립
    let mut batch = IngestBatch::default();
    for pending in state.pending {
        batch.api_requests.push(pending.row);
    }
    batch.tool_calls = state.tool_calls;
    batch.tool_completions = state.tool_completions;
    batch.hook_events = state.hook_events;
    for (session_id, (first_ts, last_ts)) in state.sessions {
        batch.sessions.push(SessionRow {
            session_id,
            project: context.project.clone(),
            first_ts,
            last_ts,
        });
    }
    Ok(ScanOutput {
        batch,
        end_offset: offset,
        trigger_ms: state.trigger_ms,
        bytes_read: offset - request.start_offset,
    })
}

// 8. 줄 반영 ------------------------------------------------------------------
fn apply_line(state: &mut ScanState, context: &LineContext, line: TranscriptLine) {
    // 1. 공통 필드
    let Some(kind) = line.r#type.as_deref() else {
        return;
    };
    if kind != "user" && kind != "assistant" {
        return;
    }
    let (Some(timestamp), Some(message)) = (line.timestamp, line.message) else {
        return;
    };
    let ts_ms = DateTime::parse_from_rfc3339(&timestamp)
        .ok()
        .map(|parsed| parsed.timestamp_millis());
    let session_id = match line.session_id {
        Some(found) if !found.is_empty() => found,
        _ => context.fallback_session.clone(),
    };
    let agent = match (context.is_subagent, line.agent_id.as_deref()) {
        (false, Some(agent_id)) if !agent_id.is_empty() => {
            let short: String = agent_id.chars().take(AGENT_ID_CHARS).collect();
            format!("subagent#{short}")
        }
        (false, _) if line.is_sidechain == Some(true) => "sidechain".to_string(),
        _ => context.agent_label.clone(),
    };

    // 2. 세션 구간
    let span = state
        .sessions
        .entry(session_id.clone())
        .or_insert_with(|| (timestamp.clone(), timestamp.clone()));
    if timestamp < span.0 {
        span.0 = timestamp.clone();
    }
    if timestamp > span.1 {
        span.1 = timestamp.clone();
    }

    // 3. 사용자 줄 (트리거, 도구 결과)
    if kind == "user" {
        if ts_ms.is_some() {
            state.trigger_ms = ts_ms;
        }
        for block in message.content {
            if block.r#type.as_deref() != Some("tool_result") {
                continue;
            }
            let Some(tool_use_id) = block.tool_use_id else {
                continue;
            };
            let is_error = block.is_error.unwrap_or(false);
            let mut denial = None;
            if is_error {
                let tool_name = state
                    .tool_names
                    .get(&tool_use_id)
                    .map(String::as_str)
                    .unwrap_or("unknown");
                let text = block.content.map(|prefix| prefix.0).unwrap_or_default();
                denial = parse_denial_text(&text, tool_name);
            }
            let error_kind = match (&denial, is_error) {
                (Some(mark), _) => format!("hook:{}", mark.guard),
                (None, true) => "tool_error".to_string(),
                (None, false) => String::new(),
            };
            state.tool_completions.push(ToolCompletionRow {
                tool_use_id: tool_use_id.clone(),
                ts: timestamp.clone(),
                is_error,
                error_kind,
            });
            if let Some(mark) = denial {
                state.hook_events.push(HookEventRow {
                    source: DataSource::Transcript,
                    event_key: format!("transcript:{tool_use_id}:deny"),
                    ts: timestamp.clone(),
                    session_id: session_id.clone(),
                    route: mark.route,
                    guard: mark.guard,
                    outcome: HookOutcome::Deny,
                    detail: mark.command,
                    duration_ms: None,
                    saved_ms: None,
                });
            }
        }
        return;
    }

    // 4. 도구 호출 시작
    for block in &message.content {
        if block.r#type.as_deref() != Some("tool_use") {
            continue;
        }
        let (Some(tool_use_id), Some(tool_name)) = (&block.id, &block.name) else {
            continue;
        };
        state
            .tool_names
            .insert(tool_use_id.clone(), tool_name.clone());
        state.tool_calls.push(ToolCallRow {
            source: DataSource::Transcript,
            tool_use_id: tool_use_id.clone(),
            ts: timestamp.clone(),
            session_id: session_id.clone(),
            agent: agent.clone(),
            tool_name: tool_name.clone(),
            duration_ms: None,
            is_error: false,
            error_kind: String::new(),
        });
    }

    // 5. 모델 요청 (message.id 중복 시 마지막 사용량 유지)
    let (Some(message_id), Some(usage)) = (message.id, message.usage) else {
        return;
    };
    let model = message.model.unwrap_or_default();
    if message_id.is_empty() || model.is_empty() || model == "<synthetic>" {
        return;
    }
    let slot = match state.pending_index.get(&message_id) {
        Some(&found) => found,
        None => {
            state.pending.push(PendingRequest {
                row: ApiRequestRow {
                    source: DataSource::Transcript,
                    request_key: message_id.clone(),
                    ts: timestamp.clone(),
                    session_id: session_id.clone(),
                    agent: agent.clone(),
                    model: model.clone(),
                    ..ApiRequestRow::default()
                },
                trigger_ms: state.trigger_ms,
            });
            state
                .pending_index
                .insert(message_id, state.pending.len() - 1);
            state.pending.len() - 1
        }
    };
    let pending = &mut state.pending[slot];
    pending.row.ts = timestamp;
    pending.row.model = model;
    pending.row.input_tokens = usage.input_tokens.unwrap_or(0);
    pending.row.output_tokens = usage.output_tokens.unwrap_or(0);
    pending.row.cache_read_tokens = usage.cache_read_input_tokens.unwrap_or(0);
    pending.row.cache_creation_tokens = usage.cache_creation_input_tokens.unwrap_or(0);
    pending.row.duration_ms = match (pending.trigger_ms, ts_ms) {
        (Some(trigger), Some(last)) => Some((last - trigger).max(0)),
        _ => None,
    };
}
