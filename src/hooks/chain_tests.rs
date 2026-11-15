//! hooks/chain_tests.rs
//! 체인·중계 시험: 가짜 가드 서버로 허용·거부·시한 초과·재작성 조합과 훅 서버 왕복 확인.

use crate::core::config::HookConfig;
use crate::core::context::AppContext;
use crate::core::model::HookOutcome;
use crate::hooks::chain::{
    FAIL_CLOSED_REASON, evaluate_pretool_chain, extract_guard_label, extract_legacy_names,
};
use crate::hooks::client::{blocking_http_request, post_hook_payload};
use crate::hooks::run_hook_server;
use crate::hooks::upstream::UpstreamClient;
use crate::net::admin::test_support::{build_test_config, build_test_context};
use crate::transcript::test_support::remove_temp_dir;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const CLI_GUARD_ALLOW: &str =
    "{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\"}}";
const CLI_GUARD_DENY: &str = "{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"\\n[cli-guard]\\n기존 CLI 차단됨 (감지: grep)\\n\"}}";

// 1. 가짜 가드 응답 -----------------------------------------------------------
#[derive(Clone)]
struct FakeReply {
    status: u16,
    body: String,
    delay_ms: u64,
}

// 2. 가짜 가드 서버 -----------------------------------------------------------
// 임시 포트에서 받은 본문을 기록하고 요청 순서대로 응답 (마지막 응답 반복)
async fn spawn_fake_guard(replies: Vec<FakeReply>) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("가짜 가드 바인드");
    let port = listener.local_addr().expect("주소").port();
    let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_outer = Arc::clone(&received);
    let replies_outer = Arc::new(replies);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let replies = Arc::clone(&replies_outer);
            let seen = Arc::clone(&seen_outer);
            tokio::spawn(async move {
                let service = service_fn(move |req: Request<Incoming>| {
                    let replies = Arc::clone(&replies);
                    let seen = Arc::clone(&seen);
                    async move {
                        let body = req
                            .into_body()
                            .collect()
                            .await
                            .map(|collected| collected.to_bytes())
                            .unwrap_or_default();
                        let position = {
                            let mut recorded = seen.lock().expect("기록 잠금");
                            recorded.push(String::from_utf8_lossy(&body).to_string());
                            recorded.len() - 1
                        };
                        let reply = replies[position.min(replies.len() - 1)].clone();
                        tokio::time::sleep(Duration::from_millis(reply.delay_ms)).await;
                        let mut response = Response::new(Full::new(Bytes::from(reply.body)));
                        *response.status_mut() =
                            StatusCode::from_u16(reply.status).expect("상태 코드");
                        Ok::<_, Infallible>(response)
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    (port, received)
}

// 3. 훅 설정 ------------------------------------------------------------------
fn hooks_for(port: u16, rewrite_enabled: bool, deadline_ms: u64) -> HookConfig {
    HookConfig {
        enabled: true,
        relay_target: format!("http://127.0.0.1:{port}"),
        rewrite_enabled,
        guard_deadline_ms: deadline_ms,
    }
}

// 4. 가드 입력 본문 -----------------------------------------------------------
fn bash_payload(command: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "session_id": "sess-1",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command, "description": "설명 유지"},
        "cwd": "C:/work",
    }))
    .expect("직렬화")
}

// 5. 허용 (재작성 없음) -------------------------------------------------------
#[tokio::test]
async fn chain_allow_without_rewrite_returns_guard_body() {
    let (port, received) = spawn_fake_guard(vec![FakeReply {
        status: 200,
        body: "{}".to_string(),
        delay_ms: 0,
    }])
    .await;
    let payload = bash_payload("grep foo a.txt");
    let result = evaluate_pretool_chain(
        &UpstreamClient::build_upstream_client(),
        &hooks_for(port, true, 2000),
        &payload,
    )
    .await;
    assert_eq!(result.outcome, HookOutcome::Allow);
    assert_eq!(&result.body[..], b"{}");
    assert_eq!(result.session_id, "sess-1");
    assert_eq!(result.saved_ms, None);
    let seen = received.lock().expect("잠금");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].as_bytes(), payload.as_slice());
}

// 6. 명시 허용 + 재작성 -------------------------------------------------------
#[tokio::test]
async fn chain_allow_with_rewrite_returns_updated_input() {
    let (port, received) = spawn_fake_guard(vec![FakeReply {
        status: 200,
        body: CLI_GUARD_ALLOW.to_string(),
        delay_ms: 0,
    }])
    .await;
    let result = evaluate_pretool_chain(
        &UpstreamClient::build_upstream_client(),
        &hooks_for(port, true, 2000),
        &bash_payload("cat README.md"),
    )
    .await;
    assert_eq!(result.outcome, HookOutcome::Rewrite);
    assert_eq!(result.saved_ms, Some(4200));
    assert_eq!(result.detail, "cat");
    let body: Value = serde_json::from_slice(&result.body).expect("JSON");
    let specific = &body["hookSpecificOutput"];
    assert_eq!(specific["hookEventName"], "PreToolUse");
    assert_eq!(specific["permissionDecision"], "allow");
    assert_eq!(specific["permissionDecisionReason"], "[rap] rewrite");
    assert_eq!(
        specific["updatedInput"]["command"],
        "bat --plain --paging=never README.md"
    );
    assert_eq!(specific["updatedInput"]["description"], "설명 유지");
    let context = specific["additionalContext"].as_str().expect("문맥");
    assert!(context.contains("[rap] 명령 자동 변환: cat → bat --plain --paging=never"));
    assert!(context.contains("가드 재검사 통과"));

    // 가드에는 재작성된 명령과 나머지 필드가 전달됨
    let seen = received.lock().expect("잠금");
    let forwarded: Value = serde_json::from_str(&seen[0]).expect("전달 JSON");
    assert_eq!(
        forwarded["tool_input"]["command"],
        "bat --plain --paging=never README.md"
    );
    assert_eq!(forwarded["cwd"], "C:/work");
    assert_eq!(forwarded["session_id"], "sess-1");
}

// 7. 판정 없음 + 재작성 + 가드 문맥 -------------------------------------------
#[tokio::test]
async fn chain_rewrite_merges_guard_context_and_keys() {
    let guard_body = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": "가드 안내",
            "extraKey": "유지",
        },
        "systemMessage": "유지",
    })
    .to_string();
    let (port, _) = spawn_fake_guard(vec![FakeReply {
        status: 200,
        body: guard_body,
        delay_ms: 0,
    }])
    .await;
    let result = evaluate_pretool_chain(
        &UpstreamClient::build_upstream_client(),
        &hooks_for(port, true, 2000),
        &bash_payload("ls -la"),
    )
    .await;
    assert_eq!(result.outcome, HookOutcome::Rewrite);
    let body: Value = serde_json::from_slice(&result.body).expect("JSON");
    assert_eq!(body["systemMessage"], "유지");
    assert_eq!(body["hookSpecificOutput"]["extraKey"], "유지");
    assert!(
        body["hookSpecificOutput"]
            .get("permissionDecision")
            .is_none()
    );
    let context = body["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("문맥");
    assert!(context.starts_with("가드 안내\n[rap] 명령 자동 변환: ls → eza"));
    assert_eq!(
        body["hookSpecificOutput"]["updatedInput"]["command"],
        "eza -la ."
    );
}

// 8. 거부 (재작성 없음) -------------------------------------------------------
#[tokio::test]
async fn chain_deny_returns_guard_body_unchanged() {
    let (port, _) = spawn_fake_guard(vec![FakeReply {
        status: 200,
        body: CLI_GUARD_DENY.to_string(),
        delay_ms: 0,
    }])
    .await;
    let result = evaluate_pretool_chain(
        &UpstreamClient::build_upstream_client(),
        &hooks_for(port, true, 2000),
        &bash_payload("grep foo a.txt"),
    )
    .await;
    assert_eq!(result.outcome, HookOutcome::Deny);
    assert_eq!(result.guard, "cli-guard");
    assert_eq!(result.detail, "grep");
    assert_eq!(&result.body[..], CLI_GUARD_DENY.as_bytes());
}

// 9. 재작성 + 거부 ------------------------------------------------------------
#[tokio::test]
async fn chain_rewrite_then_guard_deny_keeps_deny() {
    let (port, received) = spawn_fake_guard(vec![FakeReply {
        status: 200,
        body: CLI_GUARD_DENY.to_string(),
        delay_ms: 0,
    }])
    .await;
    let result = evaluate_pretool_chain(
        &UpstreamClient::build_upstream_client(),
        &hooks_for(port, true, 2000),
        &bash_payload("cat a.txt && grep x b.txt"),
    )
    .await;
    assert_eq!(result.outcome, HookOutcome::Deny);
    assert_eq!(&result.body[..], CLI_GUARD_DENY.as_bytes());
    assert_eq!(result.saved_ms, None);
    let seen = received.lock().expect("잠금");
    assert!(seen[0].contains("bat --plain --paging=never a.txt && grep x b.txt"));
}

// 10. 최상위 block 판정 -------------------------------------------------------
#[tokio::test]
async fn chain_top_level_block_counts_as_deny() {
    let block_body = "{\"decision\":\"block\",\"reason\":\"[remote-guard] 차단\"}";
    let (port, _) = spawn_fake_guard(vec![FakeReply {
        status: 200,
        body: block_body.to_string(),
        delay_ms: 0,
    }])
    .await;
    let result = evaluate_pretool_chain(
        &UpstreamClient::build_upstream_client(),
        &hooks_for(port, true, 2000),
        &bash_payload("cat a.txt"),
    )
    .await;
    assert_eq!(result.outcome, HookOutcome::Deny);
    assert_eq!(result.guard, "remote-guard");
    assert_eq!(&result.body[..], block_body.as_bytes());
}

// 11. 시한 초과 ---------------------------------------------------------------
#[tokio::test]
async fn chain_timeout_fails_closed() {
    let (port, _) = spawn_fake_guard(vec![FakeReply {
        status: 200,
        body: "{}".to_string(),
        delay_ms: 800,
    }])
    .await;
    let result = evaluate_pretool_chain(
        &UpstreamClient::build_upstream_client(),
        &hooks_for(port, true, 100),
        &bash_payload("cat a.txt"),
    )
    .await;
    assert_eq!(result.outcome, HookOutcome::Error);
    assert_eq!(result.guard, "rap");
    let body: Value = serde_json::from_slice(&result.body).expect("JSON");
    assert_eq!(body["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(
        body["hookSpecificOutput"]["permissionDecisionReason"],
        FAIL_CLOSED_REASON
    );
}

// 12. 비정상 응답 -------------------------------------------------------------
#[tokio::test]
async fn chain_bad_guard_replies_fail_closed() {
    let bad_replies = [
        (500u16, "{}"),
        (200u16, "not json"),
        (200u16, ""),
        (200u16, "[1,2]"),
    ];
    for (status, body) in bad_replies {
        let (port, _) = spawn_fake_guard(vec![FakeReply {
            status,
            body: body.to_string(),
            delay_ms: 0,
        }])
        .await;
        let result = evaluate_pretool_chain(
            &UpstreamClient::build_upstream_client(),
            &hooks_for(port, true, 2000),
            &bash_payload("grep x y"),
        )
        .await;
        assert_eq!(result.outcome, HookOutcome::Error, "응답 {status} {body:?}");
        assert_eq!(result.guard, "rap");
    }
}

// 13. 가드 연결 불가 ----------------------------------------------------------
#[tokio::test]
async fn chain_unreachable_guard_fails_closed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("바인드");
    let port = listener.local_addr().expect("주소").port();
    drop(listener);
    let result = evaluate_pretool_chain(
        &UpstreamClient::build_upstream_client(),
        &hooks_for(port, true, 1000),
        &bash_payload("cat a.txt"),
    )
    .await;
    assert_eq!(result.outcome, HookOutcome::Error);
    assert_eq!(result.detail, "cat");
}

// 14. 가드 자체 입력 교체 -----------------------------------------------------
#[tokio::test]
async fn chain_guard_updated_input_drops_rewrite() {
    let guard_body = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": {"command": "echo 가드변경"},
        }
    })
    .to_string();
    let (port, _) = spawn_fake_guard(vec![FakeReply {
        status: 200,
        body: guard_body.clone(),
        delay_ms: 0,
    }])
    .await;
    let result = evaluate_pretool_chain(
        &UpstreamClient::build_upstream_client(),
        &hooks_for(port, true, 2000),
        &bash_payload("cat a.txt"),
    )
    .await;
    assert_eq!(result.outcome, HookOutcome::Allow);
    assert_eq!(&result.body[..], guard_body.as_bytes());
}

// 15. 판정 없음 + 재작성 ------------------------------------------------------
#[tokio::test]
async fn chain_no_decision_rewrite_omits_permission_decision() {
    let bare_bodies = [
        "{}".to_string(),
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecisionReason": "사유만 있음",
            }
        })
        .to_string(),
    ];
    for guard_body in bare_bodies {
        let (port, _) = spawn_fake_guard(vec![FakeReply {
            status: 200,
            body: guard_body.clone(),
            delay_ms: 0,
        }])
        .await;
        let result = evaluate_pretool_chain(
            &UpstreamClient::build_upstream_client(),
            &hooks_for(port, true, 2000),
            &bash_payload("ls && rm -rf ./build"),
        )
        .await;
        assert_eq!(result.outcome, HookOutcome::Rewrite, "가드 {guard_body}");
        assert_eq!(result.saved_ms, Some(4200));
        let body: Value = serde_json::from_slice(&result.body).expect("JSON");
        let specific = body["hookSpecificOutput"]
            .as_object()
            .expect("hookSpecificOutput");
        assert!(!specific.contains_key("permissionDecision"));
        assert!(!specific.contains_key("permissionDecisionReason"));
        assert_eq!(specific["hookEventName"], "PreToolUse");
        assert_eq!(
            specific["updatedInput"]["command"],
            "eza . && rm -rf ./build"
        );
        assert_eq!(specific["updatedInput"]["description"], "설명 유지");
        let context = specific["additionalContext"].as_str().expect("문맥");
        assert!(context.contains("[rap] 명령 자동 변환: ls → eza"));
    }
}

// 16. 확인 요청 + 재작성 ------------------------------------------------------
#[tokio::test]
async fn chain_ask_with_rewrite_keeps_ask_and_updated_input() {
    let guard_body = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "ask",
            "permissionDecisionReason": "확인 필요",
            "additionalContext": "가드 안내",
        },
        "systemMessage": "유지",
    })
    .to_string();
    let (port, received) = spawn_fake_guard(vec![FakeReply {
        status: 200,
        body: guard_body,
        delay_ms: 0,
    }])
    .await;
    let result = evaluate_pretool_chain(
        &UpstreamClient::build_upstream_client(),
        &hooks_for(port, true, 2000),
        &bash_payload("cat README.md"),
    )
    .await;
    assert_eq!(result.outcome, HookOutcome::Rewrite);
    assert_eq!(result.saved_ms, Some(4200));
    let body: Value = serde_json::from_slice(&result.body).expect("JSON");
    assert_eq!(body["systemMessage"], "유지");
    let specific = &body["hookSpecificOutput"];
    assert_eq!(specific["permissionDecision"], "ask");
    assert_eq!(specific["permissionDecisionReason"], "확인 필요");
    assert_eq!(
        specific["updatedInput"]["command"],
        "bat --plain --paging=never README.md"
    );
    let context = specific["additionalContext"].as_str().expect("문맥");
    assert!(
        context.starts_with("가드 안내\n[rap] 명령 자동 변환: cat → bat --plain --paging=never")
    );

    // 가드는 재작성된 명령을 평가함
    let seen = received.lock().expect("잠금");
    assert!(seen[0].contains("bat --plain --paging=never README.md"));
}

// 17. 보류 + 재작성 -----------------------------------------------------------
#[tokio::test]
async fn chain_defer_with_rewrite_rechecks_original_command() {
    // 1. 첫 응답 보류, 둘째 응답 허용
    let defer_body = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "defer",
        }
    })
    .to_string();
    let second_body = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": "원본 재검사",
        }
    })
    .to_string();
    let (port, received) = spawn_fake_guard(vec![
        FakeReply {
            status: 200,
            body: defer_body,
            delay_ms: 0,
        },
        FakeReply {
            status: 200,
            body: second_body.clone(),
            delay_ms: 0,
        },
    ])
    .await;
    let result = evaluate_pretool_chain(
        &UpstreamClient::build_upstream_client(),
        &hooks_for(port, true, 2000),
        &bash_payload("cat a.txt"),
    )
    .await;

    // 2. 둘째 응답 본문 그대로 반환
    assert_eq!(result.outcome, HookOutcome::Allow);
    assert_eq!(&result.body[..], second_body.as_bytes());
    assert_eq!(result.saved_ms, None);

    // 3. 가드는 재작성본 다음 원본을 평가함
    let seen = received.lock().expect("잠금");
    assert_eq!(seen.len(), 2);
    let first: Value = serde_json::from_str(&seen[0]).expect("첫 전달 JSON");
    let second: Value = serde_json::from_str(&seen[1]).expect("둘째 전달 JSON");
    assert_eq!(
        first["tool_input"]["command"],
        "bat --plain --paging=never a.txt"
    );
    assert_eq!(second["tool_input"]["command"], "cat a.txt");
}

// 18. 확인 요청 (재작성 없음) -------------------------------------------------
#[tokio::test]
async fn chain_ask_without_rewrite_returns_guard_body_unchanged() {
    let guard_body = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "ask",
            "permissionDecisionReason": "확인 필요",
        }
    })
    .to_string();
    let (port, _) = spawn_fake_guard(vec![FakeReply {
        status: 200,
        body: guard_body.clone(),
        delay_ms: 0,
    }])
    .await;
    let result = evaluate_pretool_chain(
        &UpstreamClient::build_upstream_client(),
        &hooks_for(port, true, 2000),
        &bash_payload("rm -rf ./build"),
    )
    .await;
    assert_eq!(result.outcome, HookOutcome::Allow);
    assert_eq!(&result.body[..], guard_body.as_bytes());
}

// 19. 재작성 비활성·비 Bash ---------------------------------------------------
#[tokio::test]
async fn chain_skips_rewrite_when_disabled_or_not_bash() {
    let (port, received) = spawn_fake_guard(vec![FakeReply {
        status: 200,
        body: "{}".to_string(),
        delay_ms: 0,
    }])
    .await;
    let upstream = UpstreamClient::build_upstream_client();
    let disabled = evaluate_pretool_chain(
        &upstream,
        &hooks_for(port, false, 2000),
        &bash_payload("cat a"),
    )
    .await;
    assert_eq!(disabled.outcome, HookOutcome::Allow);

    let write_payload = serde_json::to_vec(&json!({
        "session_id": "s",
        "tool_name": "Write",
        "tool_input": {"command": "cat a"},
    }))
    .expect("직렬화");
    let not_bash =
        evaluate_pretool_chain(&upstream, &hooks_for(port, true, 2000), &write_payload).await;
    assert_eq!(not_bash.outcome, HookOutcome::Allow);
    let seen = received.lock().expect("잠금");
    assert!(seen[0].contains("\"cat a\""));
    assert_eq!(seen[1].as_bytes(), write_payload.as_slice());
}

// 20. 라벨·명령 이름 추출 -----------------------------------------------------
#[test]
fn guard_label_and_legacy_names_are_extracted() {
    assert_eq!(extract_guard_label("\n[cli-guard]\n본문"), "cli-guard");
    assert_eq!(extract_guard_label("[Bad Label] x"), "");
    assert_eq!(extract_guard_label("라벨 없음"), "");
    assert_eq!(
        extract_legacy_names("[cli-guard]\n차단됨 (감지: ls)\nls → eza"),
        "ls"
    );
    assert_eq!(
        extract_legacy_names("(감지: git diff, cat)"),
        "git diff,cat"
    );
    assert_eq!(extract_legacy_names("감지: curl https://x?token=1"), "");
    assert_eq!(extract_legacy_names("감지 항목 없음"), "");
}

// 21. 훅 서버 기동 ------------------------------------------------------------
// 임시 포트 훅 서버를 띄우고 /health 응답까지 대기
async fn start_hook_server(
    label: &str,
    relay_target_port: u16,
) -> (Arc<AppContext>, JoinHandle<anyhow::Result<()>>) {
    // 1. 임시 설정과 서버 기동
    let mut config = build_test_config(label);
    config.hooks = hooks_for(relay_target_port, true, 2000);
    let hooks_port = config.ports.hooks;
    let ctx = build_test_context(config);
    let server = tokio::spawn(run_hook_server(Arc::clone(&ctx)));

    // 2. 건강 확인 대기 (기동 실패 즉시 중단)
    let mut healthy = false;
    for _ in 0..50 {
        assert!(!server.is_finished(), "훅 서버 기동 실패");
        let probe = tokio::task::spawn_blocking(move || {
            blocking_http_request(
                &format!("127.0.0.1:{hooks_port}"),
                "GET",
                "/health",
                b"",
                Duration::from_millis(500),
            )
        })
        .await
        .expect("조인");
        if matches!(&probe, Ok((200, body)) if body.as_slice() == b"ok") {
            healthy = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(healthy, "훅 서버 /health 응답 없음");
    (ctx, server)
}

// 22. 훅 기록 반영 대기 -------------------------------------------------------
// 비동기 기록이 expected 건에 이를 때까지 대기 후 건수 반환
async fn wait_for_hook_events(ctx: &Arc<AppContext>, expected: i64) -> i64 {
    let mut stored = 0i64;
    for _ in 0..50 {
        stored = ctx
            .store
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM hook_events", [], |row| row.get(0))
            })
            .expect("조회");
        if stored >= expected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    stored
}

// 23. 훅 서버 왕복 ------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hook_server_round_trip_records_events() {
    // 1. 가짜 가드와 훅 서버 기동
    let (guard_port, _) = spawn_fake_guard(vec![FakeReply {
        status: 200,
        body: "{}".to_string(),
        delay_ms: 0,
    }])
    .await;
    let (ctx, server) = start_hook_server("hooks-chain", guard_port).await;
    let hooks_port = ctx.config.ports.hooks;
    let data_dir = ctx.config.data_dir.clone();

    // 2. 체인·중계·잘못된 경로 요청
    let chain_payload = bash_payload("cat README.md");
    let (chain_status, chain_reply) = tokio::task::spawn_blocking(move || {
        post_hook_payload(hooks_port, "pretool-chain", &chain_payload)
    })
    .await
    .expect("조인")
    .expect("체인 요청");
    assert_eq!(chain_status, 200);
    let chain_json: Value = serde_json::from_slice(&chain_reply).expect("JSON");
    assert_eq!(
        chain_json["hookSpecificOutput"]["updatedInput"]["command"],
        "bat --plain --paging=never README.md"
    );
    let (relay_status, relay_reply) = tokio::task::spawn_blocking(move || {
        post_hook_payload(hooks_port, "relay/codeguard", b"{\"session_id\":\"s2\"}")
    })
    .await
    .expect("조인")
    .expect("중계 요청");
    assert_eq!(relay_status, 200);
    assert_eq!(relay_reply, b"{}");
    let (bad_status, _) = tokio::task::spawn_blocking(move || {
        blocking_http_request(
            &format!("127.0.0.1:{hooks_port}"),
            "POST",
            "/hook/relay/Bad_Route",
            b"{}",
            Duration::from_secs(2),
        )
    })
    .await
    .expect("조인")
    .expect("잘못된 경로 요청");
    assert_eq!(bad_status, 400);
    assert!(post_hook_payload(hooks_port, "Bad/../x", b"{}").is_err());

    // 3. 비동기 기록 반영 확인
    let stored = wait_for_hook_events(&ctx, 2).await;
    assert_eq!(stored, 2);
    let (route_count, leaked): (i64, i64) = ctx
        .store
        .with_conn(|conn| {
            let routes: i64 = conn.query_row(
                "SELECT COUNT(*) FROM hook_events WHERE route IN ('pretool-chain', 'relay/codeguard')",
                [],
                |row| row.get(0),
            )?;
            let leaked: i64 = conn.query_row(
                "SELECT COUNT(*) FROM hook_events WHERE detail LIKE '%README%' OR guard LIKE '%README%'",
                [],
                |row| row.get(0),
            )?;
            Ok((routes, leaked))
        })
        .expect("조회");
    assert_eq!(route_count, 2);
    assert_eq!(leaked, 0);
    let counters = ctx.counters.snapshot_counters();
    assert_eq!(counters.hook_calls, 2);
    assert_eq!(counters.hook_rewrites, 1);
    assert_eq!(counters.saved_ms, 4200);

    // 4. 정리 (DB 닫은 뒤 폴더 제거)
    server.abort();
    let _ = server.await;
    drop(ctx);
    remove_temp_dir(&data_dir);
}

// 24. 중계 상류 불가 ----------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_unreachable_returns_502_and_records_event() {
    // 1. 닫힌 포트를 상류로 둔 훅 서버 기동
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("바인드");
    let dead_port = listener.local_addr().expect("주소").port();
    drop(listener);
    let (ctx, server) = start_hook_server("hooks-relay-502", dead_port).await;
    let hooks_port = ctx.config.ports.hooks;
    let data_dir = ctx.config.data_dir.clone();

    // 2. 중계 요청 (502 와 안내문)
    let (status, reply) = tokio::task::spawn_blocking(move || {
        post_hook_payload(hooks_port, "relay/x", b"{\"session_id\":\"s3\"}")
    })
    .await
    .expect("조인")
    .expect("중계 요청");
    assert_eq!(status, 502);
    assert!(String::from_utf8_lossy(&reply).contains("relay 실패"));

    // 3. 기록 1건과 오류 판정 확인
    let stored = wait_for_hook_events(&ctx, 1).await;
    assert_eq!(stored, 1);
    let outcome: String = ctx
        .store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT outcome FROM hook_events WHERE route = 'relay/x'",
                [],
                |row| row.get(0),
            )
        })
        .expect("판정 조회");
    assert_eq!(outcome, "error");

    // 4. 정리
    server.abort();
    let _ = server.await;
    drop(ctx);
    remove_temp_dir(&data_dir);
}

// 25. 과대 가드 응답 ----------------------------------------------------------
#[tokio::test]
async fn oversized_guard_reply_fails_closed() {
    // 1. 8 MiB 상한을 넘는 가드 응답
    let oversized_body = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "additionalContext": "x".repeat(8 * 1024 * 1024 + 1),
        }
    })
    .to_string();
    let (port, _) = spawn_fake_guard(vec![FakeReply {
        status: 200,
        body: oversized_body,
        delay_ms: 0,
    }])
    .await;

    // 2. 거부 본문으로 fail-closed
    let result = evaluate_pretool_chain(
        &UpstreamClient::build_upstream_client(),
        &hooks_for(port, true, 10_000),
        &bash_payload("grep x y"),
    )
    .await;
    assert_eq!(result.outcome, HookOutcome::Error);
    assert_eq!(result.guard, "rap");
    let body: Value = serde_json::from_slice(&result.body).expect("JSON");
    assert_eq!(body["hookSpecificOutput"]["permissionDecision"], "deny");
}

// 26. 보류 후 재검사 거부 -----------------------------------------------------
#[tokio::test]
async fn chain_defer_then_recheck_deny_keeps_deny() {
    let defer_body = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "defer",
        }
    })
    .to_string();
    let (port, _) = spawn_fake_guard(vec![
        FakeReply {
            status: 200,
            body: defer_body,
            delay_ms: 0,
        },
        FakeReply {
            status: 200,
            body: CLI_GUARD_DENY.to_string(),
            delay_ms: 0,
        },
    ])
    .await;
    let result = evaluate_pretool_chain(
        &UpstreamClient::build_upstream_client(),
        &hooks_for(port, true, 2000),
        &bash_payload("cat a.txt"),
    )
    .await;
    assert_eq!(result.outcome, HookOutcome::Deny);
    assert_eq!(result.guard, "cli-guard");
    assert_eq!(&result.body[..], CLI_GUARD_DENY.as_bytes());
}
