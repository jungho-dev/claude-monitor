//! hooks/statusline_tests.rs
//! /hook/statusline 경로 시험: 저장, 중복 제거, 형식 오류.

use crate::hooks::run_hook_server;
use crate::net::admin::test_support::{
    build_test_config, build_test_context, post_raw, wait_until_listening,
};
use crate::transcript::test_support::TempDirGuard;
use serde_json::json;
use std::sync::Arc;

// 1. 저장과 중복 제거 ---------------------------------------------------------
#[tokio::test]
async fn statusline_route_stores_and_dedupes() {
    // 1. 훅 서버 기동
    let config = build_test_config("statusline-route");
    let port = config.ports.hooks;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let server = tokio::spawn(run_hook_server(Arc::clone(&ctx)));
    wait_until_listening(port).await;

    // 2. 첫 지표 저장
    let first = json!({
        "session_id": "sess-1",
        "model": "claude-opus-5-5",
        "five_hour_pct": 20.0,
        "five_hour_resets_at": 1_738_425_600,
        "context_pct": 8.0,
    })
    .to_string();
    let (status, body) = post_raw(port, "/hook/statusline", &[], first.as_bytes()).await;
    assert_eq!((status, body.as_str()), (200, "{}"));
    let rows = ctx.store.limit_snapshots_since("", 100).expect("조회");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session_id, "sess-1");
    assert_eq!(rows[0].five_hour_pct, Some(20.0));
    assert!(!rows[0].ts.is_empty());

    // 3. 같은 값은 1분 안에 다시 저장하지 않음
    let (status, _) = post_raw(port, "/hook/statusline", &[], first.as_bytes()).await;
    assert_eq!(status, 200);
    assert_eq!(
        ctx.store
            .limit_snapshots_since("", 100)
            .expect("조회")
            .len(),
        1
    );

    // 4. 값이 바뀌면 저장
    let changed = json!({
        "session_id": "sess-1",
        "model": "claude-opus-5-5",
        "five_hour_pct": 21.0,
        "five_hour_resets_at": 1_738_425_600,
        "context_pct": 9.0,
    })
    .to_string();
    let (status, _) = post_raw(port, "/hook/statusline", &[], changed.as_bytes()).await;
    assert_eq!(status, 200);
    let latest = ctx
        .store
        .latest_limit_snapshot_for_session("sess-1")
        .expect("세션 조회")
        .expect("행 있음");
    assert_eq!(latest.five_hour_pct, Some(21.0));
    assert_eq!(
        ctx.store
            .limit_snapshots_since("", 100)
            .expect("조회")
            .len(),
        2
    );

    // 5. 지표 없는 본문은 200 이지만 저장 없음
    let (status, body) =
        post_raw(port, "/hook/statusline", &[], br#"{"session_id":"sess-2"}"#).await;
    assert_eq!((status, body.as_str()), (200, "{}"));
    assert_eq!(
        ctx.store
            .limit_snapshots_since("", 100)
            .expect("조회")
            .len(),
        2
    );
    server.abort();
}

// 2. 형식 오류 ----------------------------------------------------------------
#[tokio::test]
async fn statusline_route_rejects_malformed_bodies() {
    let config = build_test_config("statusline-bad");
    let port = config.ports.hooks;
    let _cleanup = TempDirGuard(config.data_dir.clone());
    let ctx = build_test_context(config);
    let server = tokio::spawn(run_hook_server(Arc::clone(&ctx)));
    wait_until_listening(port).await;

    // 1. 해석 불가·비객체·형식 불일치·과대 문자열
    let oversized = json!({ "session_id": "x".repeat(600), "five_hour_pct": 1.0 }).to_string();
    let bodies: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"not json".to_vec(),
        b"[]".to_vec(),
        b"42".to_vec(),
        br#"{"five_hour_pct":"high"}"#.to_vec(),
        oversized.into_bytes(),
    ];
    for body in &bodies {
        let (status, _) = post_raw(port, "/hook/statusline", &[], body).await;
        assert_eq!(status, 400, "{}", String::from_utf8_lossy(body));
    }

    // 2. 저장 없음
    assert!(
        ctx.store
            .limit_snapshots_since("", 100)
            .expect("조회")
            .is_empty()
    );
    server.abort();
}
