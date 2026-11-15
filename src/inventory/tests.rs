//! inventory/tests.rs
//! 설정 목록·진단 시험: tests/fixtures/inventory 의 합성 Claude 홈 사용.

use super::{DoctorFinding, Inventory, load_inventory, run_doctor_checks};
use crate::core::config::{AppConfig, HookConfig, PortConfig};
use crate::core::model::{PortStatus, StatusSnapshot};
use crate::net::admin::test_support::build_test_config;
use crate::transcript::test_support::TempDirGuard;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

const SECRET_MARKERS: [&str; 9] = [
    "dummy-secret-value",
    "dummy-query-secret",
    "dummy-arg-secret",
    "dummy-mcp-key",
    "dummy-url-secret",
    "dummy-header-secret",
    "dummy-userinfo-secret",
    "dummy-hook-secret",
    "secret-arg",
];

// 1. 합성 홈 경로 -------------------------------------------------------------
fn fixture_home(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("inventory")
        .join(name)
}

// 2. 임시 데이터 폴더 ---------------------------------------------------------
fn temp_data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rap-inv-{name}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    fs::create_dir_all(&dir).expect("임시 폴더");
    dir
}

// 3. 합성 홈 복사 -------------------------------------------------------------
// 고정 포트 48381-48383 을 설정 포트로 치환한 json 사본
fn render_fixture_home(name: &str, ports: &PortConfig) -> PathBuf {
    let target = temp_data_dir(&format!("home-{name}"));
    let mut pending: Vec<(PathBuf, PathBuf)> = vec![(fixture_home(name), target.clone())];
    while let Some((source_dir, target_dir)) = pending.pop() {
        fs::create_dir_all(&target_dir).expect("폴더 생성");
        for entry in fs::read_dir(&source_dir).expect("폴더 읽기") {
            let entry = entry.expect("항목 읽기");
            let source_path = entry.path();
            let target_path = target_dir.join(entry.file_name());
            if source_path.is_dir() {
                pending.push((source_path, target_path));
            } else if source_path.extension().is_some_and(|ext| ext == "json") {
                let text = fs::read_to_string(&source_path).expect("json 읽기");
                let rendered = text
                    .replace("48381", &ports.otlp.to_string())
                    .replace("48382", &ports.gateway.to_string())
                    .replace("48383", &ports.hooks.to_string());
                fs::write(&target_path, rendered).expect("json 쓰기");
            } else {
                fs::copy(&source_path, &target_path).expect("파일 복사");
            }
        }
    }
    target
}

// 4. 진단용 설정 --------------------------------------------------------------
fn doctor_config(fixture: &str, data_dir: PathBuf, hooks_port: Option<u16>) -> AppConfig {
    let mut config = build_test_config("doctor");
    if let Some(port) = hooks_port {
        config.ports.hooks = port;
    }
    config.data_dir = data_dir;
    config.claude_home = render_fixture_home(fixture, &config.ports);
    config.hooks = HookConfig {
        relay_target: "http://127.0.0.1:1".to_string(),
        ..HookConfig::default()
    };
    config
}

// 5. 항목 조회 ----------------------------------------------------------------
fn pick<'a>(findings: &'a [DoctorFinding], topic: &str) -> Vec<&'a DoctorFinding> {
    findings
        .iter()
        .filter(|finding| finding.topic == topic)
        .collect()
}

// 6. 건강 확인 서버 -----------------------------------------------------------
// /health 에 200 ok 로 답하는 임시 서버 (포트 반환)
fn spawn_health_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("바인드");
    let port = listener.local_addr().expect("주소").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                let mut received: Vec<u8> = Vec::new();
                let mut buffer = [0u8; 1024];
                while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => received.extend_from_slice(&buffer[..count]),
                    }
                }
                if received.is_empty() {
                    return;
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
                let _ = stream.flush();
                while matches!(stream.read(&mut buffer), Ok(count) if count > 0) {}
            });
        }
    });
    port
}

// 7. 훅·env·허용 목록 ---------------------------------------------------------
#[test]
fn inventory_reads_hooks_env_keys_and_allowlist() {
    let inventory = load_inventory(&fixture_home("home_registered")).expect("적재");
    assert!(inventory.warnings.is_empty(), "{:?}", inventory.warnings);
    assert_eq!(inventory.hooks.len(), 4);

    let chain = inventory
        .hooks
        .iter()
        .find(|hook| hook.target.ends_with("/hook/pretool-chain"))
        .expect("chain 훅");
    assert_eq!(chain.event, "PreToolUse");
    assert_eq!(chain.matcher, "^Bash$");
    assert_eq!(chain.hook_type, "http");
    assert_eq!(chain.timeout_secs, Some(10));
    assert_eq!(chain.on_failure, "block");
    assert!(!chain.is_async);

    let search = inventory
        .hooks
        .iter()
        .find(|hook| hook.target.contains("search-augment"))
        .expect("relay 훅");
    assert_eq!(
        search.target,
        "http://127.0.0.1:48383/hook/relay/search-augment"
    );
    assert_eq!(search.if_condition, "Bash(rg *)");
    assert_eq!(search.on_failure, "continue");

    let session = inventory
        .hooks
        .iter()
        .find(|hook| hook.event == "SessionStart")
        .expect("command 훅");
    assert_eq!(session.hook_type, "command");
    assert_eq!(session.target, "bun ensure.ts");
    assert_eq!(session.matcher, "");

    let notify = inventory
        .hooks
        .iter()
        .find(|hook| hook.event == "Notification")
        .expect("notify 훅");
    assert_eq!(notify.target, "python.exe notify.py");
    assert!(notify.is_async);
    assert_eq!(notify.timeout_secs, Some(30));

    assert!(
        inventory
            .env_keys
            .contains(&"SAMPLE_SECRET_TOKEN".to_string())
    );
    assert_eq!(
        inventory.http_hook_allowlist,
        vec!["http://127.0.0.1:48383/hook/*".to_string()]
    );
}

// 8. MCP 서버 -----------------------------------------------------------------
#[test]
fn inventory_reads_mcp_servers_without_secrets() {
    let inventory = load_inventory(&fixture_home("home_registered")).expect("적재");
    assert_eq!(inventory.mcp_servers.len(), 4);
    let by_name = |name: &str| {
        inventory
            .mcp_servers
            .iter()
            .find(|server| server.name == name)
            .expect("서버")
    };
    let alpha = by_name("alpha-fs");
    assert_eq!(alpha.transport, "stdio");
    assert_eq!(alpha.target, "C:/tools/alpha-fs.exe");
    assert!(!alpha.denied);
    let beta = by_name("beta-remote");
    assert_eq!(beta.transport, "http");
    assert_eq!(beta.target, "https://example.test:8443");
    let web = by_name("web-tool");
    assert_eq!(web.transport, "stdio");
    assert!(web.denied);
    let blocked = by_name("blocked-docs");
    assert!(blocked.denied);
    assert_eq!(blocked.target, "");
}

// 9. 에이전트·스킬 ------------------------------------------------------------
#[test]
fn inventory_reads_agent_and_skill_frontmatter() {
    let inventory = load_inventory(&fixture_home("home_registered")).expect("적재");
    assert_eq!(inventory.agents.len(), 2);
    assert_eq!(inventory.agents[0].name, "alpha");
    assert_eq!(inventory.agents[0].model, "sonnet");
    assert_eq!(inventory.agents[0].effort, "high");
    assert_eq!(inventory.agents[0].max_turns, Some(200));
    assert_eq!(inventory.agents[1].name, "beta");
    assert_eq!(inventory.agents[1].model, "opus");
    assert_eq!(inventory.agents[1].effort, "");
    assert_eq!(inventory.agents[1].max_turns, None);

    assert_eq!(inventory.skills.len(), 2);
    assert_eq!(inventory.skills[0].name, "demo");
    assert_eq!(
        inventory.skills[0].description_chars,
        "Short description".len()
    );
    assert_eq!(inventory.skills[1].name, "folded");
    assert_eq!(
        inventory.skills[1].description_chars,
        "two lines of text".len()
    );
}

// 10. 비밀값 미포함 -----------------------------------------------------------
#[test]
fn inventory_json_carries_no_secret_values() {
    let inventory: Inventory = load_inventory(&fixture_home("home_registered")).expect("적재");
    let text = serde_json::to_string(&inventory).expect("직렬화");
    for marker in SECRET_MARKERS {
        assert!(!text.contains(marker), "비밀 표식 노출: {marker}");
    }
}

// 11. 상위 폴더 .claude.json 과 형식 오류 -------------------------------------
#[test]
fn inventory_falls_back_to_parent_claude_json_and_warns() {
    let inventory = load_inventory(&fixture_home("parent_fallback").join("home")).expect("적재");
    assert_eq!(inventory.mcp_servers.len(), 1);
    assert_eq!(inventory.mcp_servers[0].name, "parent-server");
    assert_eq!(inventory.mcp_servers[0].transport, "stdio");
    assert!(
        inventory
            .warnings
            .iter()
            .any(|warning| warning.contains("hooks.Stop"))
    );
}

// 12. 깨진 JSON 과 없는 홈 ----------------------------------------------------
#[test]
fn inventory_reports_broken_and_missing_files_as_warnings() {
    let broken = load_inventory(&fixture_home("broken_json")).expect("적재");
    assert!(broken.hooks.is_empty());
    assert!(
        broken
            .warnings
            .iter()
            .any(|warning| warning.contains("settings.json 형식 오류"))
    );

    let missing = load_inventory(&fixture_home("does_not_exist")).expect("적재");
    assert!(missing.hooks.is_empty());
    assert!(
        missing
            .warnings
            .iter()
            .any(|warning| warning.contains("settings.json 읽기 실패"))
    );
}

// 13. args 없는 command 훅 대상 -----------------------------------------------
#[test]
fn inventory_command_target_keeps_only_script_name() {
    let home = temp_data_dir("command-target");
    let _cleanup = TempDirGuard(home.clone());
    fs::write(
        home.join("settings.json"),
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"bun run C:/x/y.ts --token=abc"},{"type":"command","command":"C:/tools/plain.exe --flag value"}]}]}}"#,
    )
    .expect("설정 쓰기");
    let inventory = load_inventory(&home).expect("적재");
    assert_eq!(inventory.hooks[0].target, "bun y.ts");
    assert_eq!(inventory.hooks[1].target, "plain.exe");
}

// 14. URL 대상 축약 -----------------------------------------------------------
#[test]
fn inventory_url_targets_drop_userinfo_query_and_fragment() {
    let home = temp_data_dir("url-target");
    let _cleanup = TempDirGuard(home.clone());
    fs::write(
        home.join("settings.json"),
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"http","url":"http://user:pw@localhost:9/hook/a?k=v#f"},{"type":"http","url":"http://localhost:9"},{"type":"http","url":"http://localhost:9?k=v"}]}]}}"#,
    )
    .expect("설정 쓰기");
    fs::write(
        home.join(".claude.json"),
        r#"{"mcpServers":{"remote":{"type":"http","url":"https://user:pw@example.test/mcp/v1?key=v#f"},"bare":{"type":"http","url":"https://example.test"}}}"#,
    )
    .expect("MCP 설정 쓰기");
    let inventory = load_inventory(&home).expect("적재");
    assert_eq!(inventory.hooks[0].target, "http://localhost:9/hook/a");
    assert_eq!(inventory.hooks[1].target, "http://localhost:9");
    assert_eq!(inventory.hooks[2].target, "http://localhost:9");
    let remote = inventory
        .mcp_servers
        .iter()
        .find(|server| server.name == "remote")
        .expect("원격 서버");
    assert_eq!(remote.target, "https://example.test");
    let bare = inventory
        .mcp_servers
        .iter()
        .find(|server| server.name == "bare")
        .expect("경로 없는 서버");
    assert_eq!(bare.target, "https://example.test");
}

// 15. 정상 등록 홈 진단 -------------------------------------------------------
#[test]
fn doctor_registered_home_passes_settings_checks() {
    let data_dir = temp_data_dir("registered");
    let config = doctor_config(
        "home_registered",
        data_dir.clone(),
        Some(spawn_health_server()),
    );
    let _cleanup = (
        TempDirGuard(data_dir.clone()),
        TempDirGuard(config.claude_home.clone()),
    );
    let findings = run_doctor_checks(&config).expect("진단");
    assert!(
        findings.iter().all(|finding| finding.level != "fail"),
        "{findings:?}"
    );
    assert_eq!(pick(&findings, "data-dir")[0].level, "ok");
    assert_eq!(pick(&findings, "database")[0].level, "warn");
    assert_eq!(pick(&findings, "port-admin")[0].level, "warn");
    assert_eq!(
        pick(&findings, "port-admin")[0].fix_hint,
        "rust-agents-proxy serve"
    );
    assert!(pick(&findings, "port-gateway").is_empty());
    assert_eq!(pick(&findings, "relay-target")[0].level, "warn");
    assert_eq!(pick(&findings, "hook-registration")[0].level, "ok");
    assert_eq!(pick(&findings, "http-allowlist")[0].level, "ok");
    assert_eq!(pick(&findings, "telemetry")[0].level, "ok");
    assert_eq!(pick(&findings, "gateway")[0].level, "ok");
    assert_eq!(pick(&findings, "projects-dir")[0].level, "ok");
}

// 16. 구형 등록 홈 진단 -------------------------------------------------------
#[test]
fn doctor_legacy_home_prints_exact_settings_snippets() {
    let data_dir = temp_data_dir("legacy");
    let config = doctor_config("home_legacy", data_dir.clone(), None);
    let _cleanup = (
        TempDirGuard(data_dir.clone()),
        TempDirGuard(config.claude_home.clone()),
    );
    let findings = run_doctor_checks(&config).expect("진단");
    let hooks_port = config.ports.hooks;
    let otlp_port = config.ports.otlp;

    let registration = pick(&findings, "hook-registration");
    assert_eq!(registration[0].level, "warn");
    assert!(registration[0].fix_hint.contains(&format!(
        "{{\"matcher\":\"^Bash$\",\"hooks\":[{{\"type\":\"http\",\"url\":\"http://127.0.0.1:{hooks_port}/hook/pretool-chain\",\"timeout\":10,\"onFailure\":\"block\"}}]}}"
    )));

    let allowlist = pick(&findings, "http-allowlist");
    assert_eq!(allowlist[0].level, "warn");
    assert!(
        allowlist[0]
            .fix_hint
            .contains(&format!("http://127.0.0.1:{hooks_port}/hook/*"))
    );

    let telemetry = pick(&findings, "telemetry");
    assert_eq!(telemetry[0].level, "warn");
    assert!(telemetry[0].message.contains("DISABLE_TELEMETRY"));
    assert!(telemetry[0].message.contains("unverified"));
    assert!(telemetry[0].fix_hint.contains(&format!(
        "\"env\": {{\"CLAUDE_CODE_ENABLE_TELEMETRY\":\"1\",\"OTEL_LOGS_EXPORTER\":\"otlp\",\"OTEL_EXPORTER_OTLP_ENDPOINT\":\"http://127.0.0.1:{otlp_port}\",\"OTEL_EXPORTER_OTLP_PROTOCOL\":\"http/json\"}}"
    )));

    let gateway = pick(&findings, "gateway");
    assert_eq!(gateway[0].level, "fail");
    assert!(gateway[0].message.contains("gateway.enabled=false"));

    assert_eq!(pick(&findings, "projects-dir")[0].level, "warn");
    assert_eq!(pick(&findings, "settings")[0].level, "warn");
}

// 17. 허용 목록 미지정 --------------------------------------------------------
#[test]
fn doctor_accepts_registered_hook_without_allowlist_key() {
    let data_dir = temp_data_dir("no-allowlist");
    let config = doctor_config("home_no_allowlist", data_dir.clone(), None);
    let _cleanup = (
        TempDirGuard(data_dir.clone()),
        TempDirGuard(config.claude_home.clone()),
    );
    let findings = run_doctor_checks(&config).expect("진단");
    assert_eq!(pick(&findings, "http-allowlist")[0].level, "ok");
    assert!(
        pick(&findings, "http-allowlist")[0]
            .message
            .contains("미지정")
    );
    assert!(
        pick(&findings, "hook-registration")
            .iter()
            .any(|finding| finding.level == "warn" && finding.message.contains("pretool-guard"))
    );
}

// 18. 게이트웨이 사용 + haiku 미지정 ------------------------------------------
#[test]
fn doctor_warns_gateway_without_haiku_model() {
    let data_dir = temp_data_dir("gateway-on");
    let mut config = doctor_config("home_registered", data_dir.clone(), None);
    let _cleanup = (
        TempDirGuard(data_dir.clone()),
        TempDirGuard(config.claude_home.clone()),
    );
    config.gateway.enabled = true;
    let findings = run_doctor_checks(&config).expect("진단");
    let gateway = pick(&findings, "gateway");
    assert_eq!(gateway.len(), 1);
    assert_eq!(gateway[0].level, "warn");
    assert!(gateway[0].message.contains("ANTHROPIC_DEFAULT_HAIKU_MODEL"));
    assert_eq!(pick(&findings, "port-gateway").len(), 1);
}

// 19. 수신 포트와 중계 대상 응답 ----------------------------------------------
#[test]
fn doctor_detects_listening_port_and_healthy_relay_target() {
    // 1. /health 에 200 으로 답하는 임시 서버
    let port = spawn_health_server();

    // 2. 진단
    let data_dir = temp_data_dir("listening");
    let mut config = doctor_config("home_registered", data_dir.clone(), Some(port));
    let _cleanup = (
        TempDirGuard(data_dir.clone()),
        TempDirGuard(config.claude_home.clone()),
    );
    config.hooks.relay_target = format!("http://127.0.0.1:{port}/");
    let findings = run_doctor_checks(&config).expect("진단");
    assert_eq!(pick(&findings, "port-hooks")[0].level, "ok");
    assert_eq!(
        pick(&findings, "relay-target")[0].level,
        "ok",
        "{:?}",
        pick(&findings, "relay-target")
    );
    assert!(
        findings.iter().all(|finding| finding.level != "fail"),
        "{findings:?}"
    );
}

// 20. 없는 데이터 폴더 --------------------------------------------------------
#[test]
fn doctor_warns_when_data_dir_is_missing() {
    let data_dir = std::env::temp_dir().join(format!(
        "rap-inv-missing-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let _ = fs::remove_dir_all(&data_dir);
    let config = doctor_config("home_registered", data_dir, None);
    let _cleanup = TempDirGuard(config.claude_home.clone());
    let findings = run_doctor_checks(&config).expect("진단");
    assert_eq!(pick(&findings, "data-dir")[0].level, "warn");
    assert!(pick(&findings, "data-dir")[0].fix_hint.contains("serve"));
}

// 21. 체인 등록 + 훅 미수신 ---------------------------------------------------
#[test]
fn doctor_fails_when_chain_registered_but_hooks_disabled() {
    // 1. 훅 꺼짐 (훅 포트는 진단 대상 제외)
    let data_dir = temp_data_dir("hooks-disabled");
    let mut config = doctor_config("home_registered", data_dir.clone(), None);
    let _cleanup = (
        TempDirGuard(data_dir.clone()),
        TempDirGuard(config.claude_home.clone()),
    );
    config.hooks.enabled = false;
    let findings = run_doctor_checks(&config).expect("진단");
    let registration = pick(&findings, "hook-registration");
    assert!(registration.iter().any(|finding| finding.level == "ok"));
    assert!(registration.iter().any(|finding| {
        finding.level == "fail"
            && finding.message.contains("훅 포트 미수신")
            && finding.fix_hint.contains("serve 기동")
    }));
    assert!(pick(&findings, "port-hooks").is_empty());

    // 2. 훅 켜짐이어도 포트가 닫혀 있으면 실패
    config.hooks.enabled = true;
    let findings = run_doctor_checks(&config).expect("진단");
    assert!(
        pick(&findings, "hook-registration")
            .iter()
            .any(|finding| finding.level == "fail")
    );
    assert_eq!(pick(&findings, "port-hooks")[0].level, "warn");
}

// 22. 빈 허용 목록 ------------------------------------------------------------
#[test]
fn doctor_fails_when_allowlist_is_empty_array() {
    let data_dir = temp_data_dir("empty-allowlist");
    let config = doctor_config("home_empty_allowlist", data_dir.clone(), None);
    let _cleanup = (
        TempDirGuard(data_dir.clone()),
        TempDirGuard(config.claude_home.clone()),
    );
    let findings = run_doctor_checks(&config).expect("진단");
    let allowlist = pick(&findings, "http-allowlist");
    assert_eq!(allowlist[0].level, "fail");
    assert!(
        allowlist[0].message.contains("빈 배열"),
        "{:?}",
        allowlist[0]
    );
}

// 23. 불일치 허용 목록 --------------------------------------------------------
#[test]
fn doctor_fails_when_allowlist_lacks_rap_url() {
    let data_dir = temp_data_dir("mismatched-allowlist");
    let config = doctor_config("home_mismatched_allowlist", data_dir.clone(), None);
    let _cleanup = (
        TempDirGuard(data_dir.clone()),
        TempDirGuard(config.claude_home.clone()),
    );
    let findings = run_doctor_checks(&config).expect("진단");
    let allowlist = pick(&findings, "http-allowlist");
    assert_eq!(allowlist[0].level, "fail");
    assert!(
        allowlist[0].message.contains("URL 없음"),
        "{:?}",
        allowlist[0]
    );
}

// 24. 상태 응답 서버 ----------------------------------------------------------
// /api/status 에 주어진 포트 행으로 답하는 임시 관리 서버 (포트 반환)
fn spawn_status_server(ports: Vec<PortStatus>) -> u16 {
    let snapshot = StatusSnapshot {
        ports,
        ..StatusSnapshot::default()
    };
    let body = serde_json::to_vec(&snapshot).expect("상태 직렬화");
    let listener = TcpListener::bind("127.0.0.1:0").expect("바인드");
    let port = listener.local_addr().expect("주소").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let body = body.clone();
            std::thread::spawn(move || {
                // 요청 헤더 끝까지 수신 (미수신 잔여는 닫을 때 RST 유발)
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();

                // 상대 종료까지 잔여 입력 비움
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(300)));
                while matches!(stream.read(&mut buffer), Ok(read) if read > 0) {}
            });
        }
    });
    port
}

// 25. 닫힌 포트 번호 ----------------------------------------------------------
// 바인드 후 곧바로 해제한 임시 포트 번호
fn reserve_closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("바인드");
    listener.local_addr().expect("주소").port()
}

// 26. 점유된 OTLP 포트 소유 판정 ----------------------------------------------
#[test]
fn doctor_reports_foreign_owned_otlp_port() {
    // 1. 다른 프로세스가 OTLP 포트 점유, serve 는 otlp 생략 상태로 응답
    let foreign = TcpListener::bind("127.0.0.1:0").expect("포트 점유");
    let otlp_port = foreign.local_addr().expect("주소").port();
    let status_port = spawn_status_server(vec![PortStatus {
        name: "otlp".to_string(),
        port: otlp_port,
        enabled: true,
        listening: false,
    }]);
    let data_dir = temp_data_dir("foreign-otlp");
    let mut config = doctor_config("home_registered", data_dir.clone(), None);
    let _cleanup = (
        TempDirGuard(data_dir.clone()),
        TempDirGuard(config.claude_home.clone()),
    );
    config.ports.admin = status_port;
    config.ports.otlp = otlp_port;

    // 2. 수신 중이어도 경고
    let findings = run_doctor_checks(&config).expect("진단");
    let otlp = pick(&findings, "port-otlp");
    assert_eq!(otlp[0].level, "warn");
    assert!(otlp[0].message.contains("다른 프로세스"), "{:?}", otlp[0]);
    drop(foreign);
}

// 27. rap 소유 OTLP 포트 ------------------------------------------------------
#[test]
fn doctor_accepts_rap_owned_otlp_port_and_flags_unverified_owner() {
    // 1. serve 가 otlp 를 잡았다고 응답
    let held = TcpListener::bind("127.0.0.1:0").expect("포트 점유");
    let otlp_port = held.local_addr().expect("주소").port();
    let status_port = spawn_status_server(vec![PortStatus {
        name: "otlp".to_string(),
        port: otlp_port,
        enabled: true,
        listening: true,
    }]);
    let data_dir = temp_data_dir("rap-otlp");
    let mut config = doctor_config("home_registered", data_dir.clone(), None);
    let _cleanup = (
        TempDirGuard(data_dir.clone()),
        TempDirGuard(config.claude_home.clone()),
    );
    config.ports.otlp = otlp_port;
    config.ports.admin = status_port;
    let findings = run_doctor_checks(&config).expect("진단");
    assert_eq!(pick(&findings, "port-otlp")[0].level, "ok");

    // 2. serve 미응답이면 소유 확인 불가
    config.ports.admin = reserve_closed_port();
    let findings = run_doctor_checks(&config).expect("진단");
    let otlp = pick(&findings, "port-otlp");
    assert_eq!(otlp[0].level, "warn");
    assert!(otlp[0].message.contains("소유 확인 불가"), "{:?}", otlp[0]);
    drop(held);
}
