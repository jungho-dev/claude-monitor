//! statusline/mod.rs
//! statusLine 연동 명령: stdin 지표를 훅 포트로 전송하고 한 줄 출력 (기존 명령 감싸기 지원).

use crate::core::config::AppConfig;
use crate::hooks::client::blocking_http_request;
use crate::store::limits::LimitSnapshot;
use anyhow::Context;
use serde_json::Value;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

// stdin 본문 상한 (statusLine JSON 은 수 KB)
const MAX_STDIN_BYTES: u64 = 1024 * 1024;
// 전송 총 예산 (연결·쓰기·읽기 각각의 한도이자 종료 전 대기 한도)
const POST_BUDGET: Duration = Duration::from_millis(250);
const SNAPSHOT_ROUTE: &str = "/hook/statusline";

// 1. 명령 진입 ----------------------------------------------------------------
/// stdin 을 읽어 지표를 전송하고 출력. `wrap` 이 비어 있지 않으면 그 명령을 감싸 실행 (종료 코드 반환).
pub fn run_statusline(config: &AppConfig, wrap: &[String], append: bool) -> anyhow::Result<i32> {
    // 1. stdin 읽기 (읽기 오류는 읽은 만큼만 사용)
    let mut raw: Vec<u8> = Vec::new();
    let _ = std::io::stdin()
        .lock()
        .take(MAX_STDIN_BYTES)
        .read_to_end(&mut raw);

    // 2. 실행과 출력
    let mut stdout = std::io::stdout().lock();
    Ok(run_statusline_with(
        config.ports.hooks,
        wrap,
        append,
        &raw,
        &mut stdout,
    ))
}

// 2. 입력 주입 실행 -----------------------------------------------------------
// 전송 실패와 출력 오류는 조용히 무시 (statusLine 출력을 늦추거나 깨뜨리지 않음)
pub(crate) fn run_statusline_with(
    port: u16,
    wrap: &[String],
    append: bool,
    raw: &[u8],
    output: &mut impl Write,
) -> i32 {
    // 1. 지표 추출과 전송 시작 (감싼 명령과 병렬)
    let started = Instant::now();
    let snapshot = extract_statusline_snapshot(raw);
    let segment = snapshot
        .as_ref()
        .map(format_statusline_segment)
        .unwrap_or_default();
    let delivery = snapshot.and_then(|metrics| {
        let payload = serde_json::to_vec(&metrics).ok()?;
        let (done_tx, done_rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let _ = blocking_http_request(
                &format!("127.0.0.1:{port}"),
                "POST",
                SNAPSHOT_ROUTE,
                &payload,
                POST_BUDGET,
            );
            let _ = done_tx.send(());
        });
        Some(done_rx)
    });

    // 2. 출력 (명령 없음은 한 줄, 있으면 감싼 명령 출력 그대로)
    let code = match wrap.split_first() {
        None => {
            if !segment.is_empty() {
                let _ = writeln!(output, "{segment}");
            }
            0
        }
        Some((program, args)) => match run_wrapped_command(program, args, raw) {
            Ok((child_stdout, child_code)) => {
                let _ = output.write_all(&child_stdout);
                if append && !segment.is_empty() {
                    if !child_stdout.is_empty() && !child_stdout.ends_with(b"\n") {
                        let _ = output.write_all(b"\n");
                    }
                    let _ = writeln!(output, "{segment}");
                }
                child_code
            }
            Err(err) => {
                // 실행 실패: 오류는 stderr, 화면은 우리 한 줄로 대체
                eprintln!("[cm statusline] 감싼 명령 실행 실패: {err:#}");
                if !segment.is_empty() {
                    let _ = writeln!(output, "{segment}");
                }
                0
            }
        },
    };
    let _ = output.flush();

    // 3. 전송 마무리 대기 (남은 예산만큼, 서버 없음은 연결 실패로 즉시 끝남)
    if let Some(done_rx) = delivery {
        let _ = done_rx.recv_timeout(POST_BUDGET.saturating_sub(started.elapsed()));
    }
    code
}

// 3. 감싼 명령 실행 -----------------------------------------------------------
// 같은 stdin 바이트를 전달하고 stdout 전체와 종료 코드 반환 (stderr 는 그대로 상속)
fn run_wrapped_command(
    program: &str,
    args: &[String],
    stdin_bytes: &[u8],
) -> anyhow::Result<(Vec<u8>, i32)> {
    // 1. 실행
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("명령 실행 실패: {program}"))?;

    // 2. stdin 공급 (자식이 다 읽기 전 종료해도 교착 없게 별도 스레드, 끝나면 닫힘)
    let mut child_stdin = child.stdin.take().context("자식 stdin 없음")?;
    let input = stdin_bytes.to_vec();
    let feeder = std::thread::spawn(move || {
        let _ = child_stdin.write_all(&input);
    });

    // 3. 출력 수집
    let finished = child
        .wait_with_output()
        .with_context(|| format!("명령 출력 수집 실패: {program}"))?;
    let _ = feeder.join();
    Ok((finished.stdout, finished.status.code().unwrap_or(1)))
}

// 4. 지표 추출 ----------------------------------------------------------------
/// statusLine JSON 에서 한도·컨텍스트·캐시 지표만 추출 (경로·작업 폴더·PR 등은 읽지 않음).
/// 객체가 아니거나 지표가 하나도 없으면 None.
pub(crate) fn extract_statusline_snapshot(raw: &[u8]) -> Option<LimitSnapshot> {
    // 1. 값 접근 도구 (모든 필드는 없거나 null 일 수 있음)
    let value: Value = serde_json::from_slice(raw).ok()?;
    if !value.is_object() {
        return None;
    }
    let number = |path: &str| value.pointer(path).and_then(Value::as_f64);
    let integer = |path: &str| {
        value.pointer(path).and_then(|node| {
            node.as_i64()
                .or_else(|| node.as_f64().map(|float| float as i64))
        })
    };
    let text = |path: &str| {
        value
            .pointer(path)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let flag = |path: &str| value.pointer(path).and_then(Value::as_bool);

    // 2. 지표 구성
    let miss_cause = value
        .pointer("/prompt_cache/last_miss_cause/causes")
        .and_then(Value::as_array)
        .map(|causes| {
            causes
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<&str>>()
                .join(",")
        })
        .filter(|joined| !joined.is_empty());
    let snapshot = LimitSnapshot {
        session_id: text("/session_id").unwrap_or_default(),
        model: text("/model/id")
            .or_else(|| text("/model"))
            .unwrap_or_default(),
        five_hour_pct: number("/rate_limits/five_hour/used_percentage"),
        five_hour_resets_at: integer("/rate_limits/five_hour/resets_at"),
        seven_day_pct: number("/rate_limits/seven_day/used_percentage"),
        seven_day_resets_at: integer("/rate_limits/seven_day/resets_at"),
        context_pct: number("/context_window/used_percentage").or_else(|| {
            number("/context_window/remaining_percentage").map(|remaining| 100.0 - remaining)
        }),
        context_window_size: integer("/context_window/context_window_size"),
        context_input_tokens: integer("/context_window/total_input_tokens"),
        context_cache_read_tokens: integer("/context_window/current_usage/cache_read_input_tokens"),
        context_cache_write_tokens: integer(
            "/context_window/current_usage/cache_creation_input_tokens",
        ),
        cache_warm: flag("/prompt_cache/warm"),
        cache_caching_observed: flag("/prompt_cache/caching_observed"),
        cache_ttl: text("/prompt_cache/ttl"),
        cache_expires_at: integer("/prompt_cache/expires_at"),
        cache_requests: integer("/prompt_cache/requests"),
        cache_misses: integer("/prompt_cache/misses"),
        cache_hit_ratio: number("/prompt_cache/hit_ratio"),
        cache_last_miss_at: integer("/prompt_cache/last_miss_at"),
        cache_last_miss_cause: miss_cause,
        session_cost_usd: number("/cost/total_cost_usd"),
        ..LimitSnapshot::default()
    };

    // 3. 지표 없음 (식별 필드만 있는 본문)
    let identity_only = LimitSnapshot {
        session_id: snapshot.session_id.clone(),
        model: snapshot.model.clone(),
        ..LimitSnapshot::default()
    };
    if snapshot == identity_only {
        return None;
    }
    Some(snapshot)
}

// 5. 한 줄 구성 ---------------------------------------------------------------
// 있는 값만 `5h 42% | 7d 18% | ctx 63%` 형태 (ASCII, 색 코드 없음)
fn format_statusline_segment(snapshot: &LimitSnapshot) -> String {
    [
        ("5h", snapshot.five_hour_pct),
        ("7d", snapshot.seven_day_pct),
        ("ctx", snapshot.context_pct),
    ]
    .into_iter()
    .filter_map(|(label, percent)| percent.map(|used| format!("{label} {}%", used.round() as i64)))
    .collect::<Vec<String>>()
    .join(" | ")
}
