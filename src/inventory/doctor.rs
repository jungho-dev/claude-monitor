//! inventory/doctor.rs
//! 진단: 데이터 폴더, 포트, 중계 대상, 훅 등록, 허용 URL, 텔레메트리, 게이트웨이, projects 폴더.

use super::{DoctorFinding, HookEntry, Inventory, load::read_inventory};
use crate::core::config::{AppConfig, DB_FILE_NAME};
use crate::core::model::StatusSnapshot;
use crate::hooks::client::blocking_http_request;
use crate::net::admin::fetch_status_blocking;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

// 값 비교에만 쓰는 env 키 (값은 출력하지 않음)
const WATCHED_ENV_KEYS: [&str; 7] = [
    "CLAUDE_CODE_ENABLE_TELEMETRY",
    "OTEL_LOGS_EXPORTER",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_PROTOCOL",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "DISABLE_TELEMETRY",
];
const SERVE_HINT: &str = "rust-agents-proxy serve";

// 1. 진단 실행 ----------------------------------------------------------------
pub fn run_doctor(config: &AppConfig) -> anyhow::Result<Vec<DoctorFinding>> {
    let mut findings: Vec<DoctorFinding> = Vec::new();
    check_storage(config, &mut findings);
    check_ports(config, &mut findings);
    check_relay_target(config, &mut findings);

    let inventory = read_inventory(&config.claude_home)?;
    let watched = read_watched_env(config);
    for warning in &inventory.warnings {
        findings.push(DoctorFinding {
            level: "warn".to_string(),
            topic: "settings".to_string(),
            message: warning.clone(),
            fix_hint: String::new(),
        });
    }
    check_hook_registration(config, &inventory, &mut findings);
    check_telemetry(config, &watched, &mut findings);
    check_gateway(config, &watched, &mut findings);
    check_projects_dir(config, &mut findings);
    Ok(findings)
}

// 2. 저장소 점검 --------------------------------------------------------------
fn check_storage(config: &AppConfig, findings: &mut Vec<DoctorFinding>) {
    // 1. 데이터 폴더 쓰기
    let data_dir = &config.data_dir;
    if !data_dir.is_dir() {
        findings.push(DoctorFinding {
            level: "warn".to_string(),
            topic: "data-dir".to_string(),
            message: format!("데이터 폴더 없음: {}", data_dir.display()),
            fix_hint: format!("{SERVE_HINT} (첫 기동 때 폴더와 DB 생성)"),
        });
    } else {
        let probe = data_dir.join(format!(".rap-doctor-{}", std::process::id()));
        let writable = fs::write(&probe, b"probe").is_ok();
        let _ = fs::remove_file(&probe);
        findings.push(DoctorFinding {
            level: if writable { "ok" } else { "fail" }.to_string(),
            topic: "data-dir".to_string(),
            message: if writable {
                format!("데이터 폴더 쓰기 가능: {}", data_dir.display())
            } else {
                format!("데이터 폴더 쓰기 불가: {}", data_dir.display())
            },
            fix_hint: if writable {
                String::new()
            } else {
                "폴더 권한 확인 또는 rap.toml 의 data_dir 변경".to_string()
            },
        });
    }

    // 2. DB 파일
    let db_path = data_dir.join(DB_FILE_NAME);
    let exists = db_path.is_file();
    findings.push(DoctorFinding {
        level: if exists { "ok" } else { "warn" }.to_string(),
        topic: "database".to_string(),
        message: if exists {
            format!("DB 존재: {}", db_path.display())
        } else {
            format!("DB 없음: {}", db_path.display())
        },
        fix_hint: if exists {
            String::new()
        } else {
            format!("{SERVE_HINT} 또는 rust-agents-proxy index")
        },
    });
}

// 3. 포트 점검 ----------------------------------------------------------------
fn check_ports(config: &AppConfig, findings: &mut Vec<DoctorFinding>) {
    // 관리 서버 상태 (OTLP·게이트웨이 소유 확인용, 필요할 때 한 번만 조회)
    let mut serve_status: Option<Option<StatusSnapshot>> = None;
    for (name, port, enabled) in config.planned_ports() {
        if !enabled {
            continue;
        }
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let listening = TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok();

        // 선택 서버는 수신 중이어도 rap 소유 여부 확인
        if listening && matches!(name, "otlp" | "gateway") {
            let status =
                serve_status.get_or_insert_with(|| fetch_status_blocking(config.ports.admin).ok());
            let rap_owned = status.as_ref().map(|snapshot| {
                snapshot
                    .ports
                    .iter()
                    .any(|row| row.name == name && row.listening)
            });
            let (level, message, hint) = match rap_owned {
                Some(true) => ("ok", format!("{name} 포트 {port} 수신 중"), String::new()),
                Some(false) => (
                    "warn",
                    format!(
                        "{name} 포트 {port} 를 다른 프로세스가 점유 (serve 는 바인드 실패로 서버 생략)"
                    ),
                    "점유 프로세스 종료 또는 rap.toml 의 ports 변경 후 serve 재시작".to_string(),
                ),
                None => (
                    "warn",
                    format!("{name} 포트 {port} 수신 중이나 rap 소유 확인 불가 (serve 미응답)"),
                    SERVE_HINT.to_string(),
                ),
            };
            findings.push(DoctorFinding {
                level: level.to_string(),
                topic: format!("port-{name}"),
                message,
                fix_hint: hint,
            });
            continue;
        }
        findings.push(DoctorFinding {
            level: if listening { "ok" } else { "warn" }.to_string(),
            topic: format!("port-{name}"),
            message: if listening {
                format!("{name} 포트 {port} 수신 중")
            } else {
                format!("{name} 포트 {port} 수신 안 함")
            },
            fix_hint: if listening {
                String::new()
            } else {
                SERVE_HINT.to_string()
            },
        });
    }
}

// 4. 중계 대상 점검 -----------------------------------------------------------
fn check_relay_target(config: &AppConfig, findings: &mut Vec<DoctorFinding>) {
    let target = config.hooks.relay_target.trim_end_matches('/');
    let Some(rest) = target.strip_prefix("http://") else {
        findings.push(DoctorFinding {
            level: "warn".to_string(),
            topic: "relay-target".to_string(),
            message: format!("relay_target 은 http:// 만 지원: {target}"),
            fix_hint: "rap.toml 의 hooks.relay_target 을 http://127.0.0.1:<포트> 로 수정"
                .to_string(),
        });
        return;
    };
    let host_port = rest.split('/').next().unwrap_or("");
    let reply = blocking_http_request(
        host_port,
        "GET",
        "/health",
        b"",
        Duration::from_millis(1000),
    );
    let (level, message, hint) = match reply {
        Ok((200, _)) => (
            "ok",
            format!("relay_target {target} /health 응답"),
            String::new(),
        ),
        Ok((status, _)) => (
            "warn",
            format!("relay_target {target} /health 상태 {status}"),
            "기존 훅 서버 상태 확인".to_string(),
        ),
        Err(err) => (
            "warn",
            format!("relay_target {target} 응답 없음: {err:#}"),
            "기존 훅 서버 기동 확인 (SessionStart 훅 hook-server-ensure)".to_string(),
        ),
    };
    findings.push(DoctorFinding {
        level: level.to_string(),
        topic: "relay-target".to_string(),
        message,
        fix_hint: hint,
    });
}

// 5. 비교 대상 env 읽기 -------------------------------------------------------
// settings.json env 중 WATCHED_ENV_KEYS 만 보관 (값은 비교 전용)
fn read_watched_env(config: &AppConfig) -> HashMap<String, String> {
    let mut watched: HashMap<String, String> = HashMap::new();
    let Ok(text) = fs::read_to_string(config.claude_home.join("settings.json")) else {
        return watched;
    };
    let Ok(root) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else {
        return watched;
    };
    let Some(Value::Object(env)) = root.get("env") else {
        return watched;
    };
    for key in WATCHED_ENV_KEYS {
        let value = match env.get(key) {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Number(number)) => number.to_string(),
            Some(Value::Bool(flag)) => flag.to_string(),
            _ => continue,
        };
        watched.insert(key.to_string(), value);
    }
    watched
}

// 6. 훅 등록·허용 URL 점검 ----------------------------------------------------
fn check_hook_registration(
    config: &AppConfig,
    inventory: &Inventory,
    findings: &mut Vec<DoctorFinding>,
) {
    // 1. rap 훅 등록 여부
    let hooks_port = config.ports.hooks;
    let prefixes = [
        format!("http://127.0.0.1:{hooks_port}/"),
        format!("http://localhost:{hooks_port}/"),
    ];
    let rap_hooks: Vec<&HookEntry> = inventory
        .hooks
        .iter()
        .filter(|hook| {
            hook.hook_type == "http"
                && prefixes
                    .iter()
                    .any(|prefix| hook.target.starts_with(prefix))
        })
        .collect();
    if rap_hooks.is_empty() {
        let snippet = format!(
            "{{\"matcher\":\"^Bash$\",\"hooks\":[{{\"type\":\"http\",\"url\":\"http://127.0.0.1:{hooks_port}/hook/pretool-chain\",\"timeout\":10,\"onFailure\":\"block\"}}]}}"
        );
        findings.push(DoctorFinding {
            level: "warn".to_string(),
            topic: "hook-registration".to_string(),
            message: format!("settings.json 에 rap 훅 포트 {hooks_port} 등록 없음"),
            fix_hint: format!(
                "settings.json hooks.PreToolUse 에서 ^Bash$ 항목 ({}/hook/pretool-guard) 을 다음 항목으로 교체 (onFailure block, timeout 10 유지): {snippet}",
                config.hooks.relay_target.trim_end_matches('/')
            ),
        });
    } else {
        findings.push(DoctorFinding {
            level: "ok".to_string(),
            topic: "hook-registration".to_string(),
            message: format!("rap 훅 등록 {}건 (포트 {hooks_port})", rap_hooks.len()),
            fix_hint: String::new(),
        });
    }

    // 2. 기존 가드 직접 등록 잔존 (병렬 실행 시 재작성이 거부에 짐)
    let chain_registered = rap_hooks
        .iter()
        .any(|hook| hook.event == "PreToolUse" && hook.target.ends_with("/hook/pretool-chain"));
    let legacy_left = inventory.hooks.iter().any(|hook| {
        hook.event == "PreToolUse"
            && hook.matcher.contains("Bash")
            && hook.target.ends_with("/hook/pretool-guard")
    });
    if chain_registered && legacy_left {
        findings.push(DoctorFinding {
            level: "warn".to_string(),
            topic: "hook-registration".to_string(),
            message: "Bash 에 pretool-chain 과 pretool-guard 직접 등록이 함께 있음 (병렬 실행이라 가드 거부가 재작성을 이김)".to_string(),
            fix_hint: "PreToolUse ^Bash$ 의 pretool-guard 항목 제거".to_string(),
        });
    }

    // 3. 훅 포트 미수신 (등록된 pretool-chain 이 모든 Bash 를 차단)
    if chain_registered {
        let hooks_alive = config.hooks.enabled
            && matches!(
                blocking_http_request(
                    &format!("127.0.0.1:{hooks_port}"),
                    "GET",
                    "/health",
                    b"",
                    Duration::from_millis(1000),
                ),
                Ok((200, body)) if body.as_slice() == b"ok"
            );
        if !hooks_alive {
            findings.push(DoctorFinding {
                level: "fail".to_string(),
                topic: "hook-registration".to_string(),
                message: "훅 포트 미수신: 등록된 pretool-chain 이 모든 Bash 를 차단함".to_string(),
                fix_hint: "serve 기동 또는 settings.json 등록 제거".to_string(),
            });
        }
    }

    // 4. 허용 URL 목록
    let required = format!("http://127.0.0.1:{hooks_port}/hook/*");
    let sample = format!("http://127.0.0.1:{hooks_port}/hook/pretool-chain");
    let allowed = inventory.http_hook_allowlist.iter().any(|entry| {
        entry == &required
            || entry
                .strip_suffix('*')
                .is_some_and(|prefix| sample.starts_with(prefix))
    });
    let add_hint = format!("settings.json allowedHttpHookUrls 에 \"{required}\" 추가");
    let restricted = inventory.http_hook_allowlist_defined;
    if rap_hooks.is_empty() {
        findings.push(DoctorFinding {
            level: "warn".to_string(),
            topic: "http-allowlist".to_string(),
            message: "rap 훅 미등록 (허용 URL 필요 여부 판단 불가)".to_string(),
            fix_hint: if allowed || !restricted {
                String::new()
            } else {
                add_hint
            },
        });
    } else if !restricted {
        // 미지정 목록은 HTTP 훅 제한 없음
        findings.push(DoctorFinding {
            level: "ok".to_string(),
            topic: "http-allowlist".to_string(),
            message: "allowedHttpHookUrls 미지정 (HTTP 훅 제한 없음)".to_string(),
            fix_hint: String::new(),
        });
    } else if allowed {
        findings.push(DoctorFinding {
            level: "ok".to_string(),
            topic: "http-allowlist".to_string(),
            message: "allowedHttpHookUrls 에 rap 훅 URL 포함".to_string(),
            fix_hint: String::new(),
        });
    } else if inventory.http_hook_allowlist.is_empty() {
        // 빈 배열은 모든 HTTP 훅 차단
        findings.push(DoctorFinding {
            level: "fail".to_string(),
            topic: "http-allowlist".to_string(),
            message:
                "allowedHttpHookUrls 가 빈 배열이라 모든 HTTP 훅 차단 (rap 훅이 실행되지 않음)"
                    .to_string(),
            fix_hint: add_hint,
        });
    } else {
        findings.push(DoctorFinding {
            level: "fail".to_string(),
            topic: "http-allowlist".to_string(),
            message: "rap 훅이 등록됐으나 allowedHttpHookUrls 에 URL 없음 (훅이 실행되지 않음)"
                .to_string(),
            fix_hint: add_hint,
        });
    }
}

// 7. 텔레메트리 점검 ----------------------------------------------------------
fn check_telemetry(
    config: &AppConfig,
    watched: &HashMap<String, String>,
    findings: &mut Vec<DoctorFinding>,
) {
    // 1. 필수 항목 비교
    let otlp_port = config.ports.otlp;
    let endpoint_ok = watched
        .get("OTEL_EXPORTER_OTLP_ENDPOINT")
        .is_some_and(|value| {
            value.contains(&format!("127.0.0.1:{otlp_port}"))
                || value.contains(&format!("localhost:{otlp_port}"))
        });
    let mut problems: Vec<&str> = Vec::new();
    if watched
        .get("CLAUDE_CODE_ENABLE_TELEMETRY")
        .map(String::as_str)
        != Some("1")
    {
        problems.push("CLAUDE_CODE_ENABLE_TELEMETRY=1 필요");
    }
    if watched.get("OTEL_LOGS_EXPORTER").map(String::as_str) != Some("otlp") {
        problems.push("OTEL_LOGS_EXPORTER=otlp 필요");
    }
    if !endpoint_ok {
        problems.push("OTEL_EXPORTER_OTLP_ENDPOINT 이 rap otlp 포트를 가리키지 않음");
    }
    if watched
        .get("OTEL_EXPORTER_OTLP_PROTOCOL")
        .map(String::as_str)
        != Some("http/json")
    {
        problems.push("OTEL_EXPORTER_OTLP_PROTOCOL=http/json 필요");
    }
    let disable_set = matches!(
        watched.get("DISABLE_TELEMETRY").map(String::as_str),
        Some("1") | Some("true")
    );

    // 2. 판정
    let disable_note =
        "DISABLE_TELEMETRY 가 켜져 있음: OTLP 내보내기를 막는지는 미확인 (unverified)";
    if problems.is_empty() && !disable_set {
        findings.push(DoctorFinding {
            level: "ok".to_string(),
            topic: "telemetry".to_string(),
            message: "텔레메트리 env 가 rap otlp 포트를 가리킴".to_string(),
            fix_hint: String::new(),
        });
    } else if problems.is_empty() {
        findings.push(DoctorFinding {
            level: "warn".to_string(),
            topic: "telemetry".to_string(),
            message: format!("텔레메트리 env 는 맞음, 단 {disable_note}"),
            fix_hint: "serve 실행 후 status 의 otlp_batches 증가로 수신 확인".to_string(),
        });
    } else {
        let snippet = format!(
            "\"env\": {{\"CLAUDE_CODE_ENABLE_TELEMETRY\":\"1\",\"OTEL_LOGS_EXPORTER\":\"otlp\",\"OTEL_EXPORTER_OTLP_ENDPOINT\":\"http://127.0.0.1:{otlp_port}\",\"OTEL_EXPORTER_OTLP_PROTOCOL\":\"http/json\"}}"
        );
        let suffix = if disable_set {
            format!(" / {disable_note}")
        } else {
            String::new()
        };
        findings.push(DoctorFinding {
            level: "warn".to_string(),
            topic: "telemetry".to_string(),
            message: format!("{}{suffix}", problems.join(", ")),
            fix_hint: format!("settings.json env 에 추가: {snippet}"),
        });
    }
}

// 8. 게이트웨이 점검 ----------------------------------------------------------
fn check_gateway(
    config: &AppConfig,
    watched: &HashMap<String, String>,
    findings: &mut Vec<DoctorFinding>,
) {
    // 1. 기본 URL 이 꺼진 게이트웨이를 가리킴
    let gateway_port = config.ports.gateway;
    let points_at_gateway = watched.get("ANTHROPIC_BASE_URL").is_some_and(|value| {
        value.contains(&format!("127.0.0.1:{gateway_port}"))
            || value.contains(&format!("localhost:{gateway_port}"))
    });
    if points_at_gateway && !config.gateway.enabled {
        findings.push(DoctorFinding {
            level: "fail".to_string(),
            topic: "gateway".to_string(),
            message: format!(
                "ANTHROPIC_BASE_URL 이 게이트웨이 포트 {gateway_port} 를 가리키나 gateway.enabled=false (API 호출 연결 실패)"
            ),
            fix_hint: "rap.toml 에 [gateway] enabled = true 후 serve 재시작, 또는 settings.json env 의 ANTHROPIC_BASE_URL 제거".to_string(),
        });
    }

    // 2. 게이트웨이 사용 시 백그라운드 모델
    if config.gateway.enabled {
        let haiku_set = watched
            .get("ANTHROPIC_DEFAULT_HAIKU_MODEL")
            .is_some_and(|value| !value.is_empty());
        if haiku_set {
            findings.push(DoctorFinding {
                level: "ok".to_string(),
                topic: "gateway".to_string(),
                message: "게이트웨이 사용, ANTHROPIC_DEFAULT_HAIKU_MODEL 지정됨".to_string(),
                fix_hint: String::new(),
            });
        } else {
            findings.push(DoctorFinding {
                level: "warn".to_string(),
                topic: "gateway".to_string(),
                message: "게이트웨이 사용 중 ANTHROPIC_DEFAULT_HAIKU_MODEL 미지정 (사용자 지정 기본 URL 경유 시 백그라운드 작업이 메인 모델 사용)".to_string(),
                fix_hint: "settings.json env 에 \"ANTHROPIC_DEFAULT_HAIKU_MODEL\": \"<haiku 모델 ID>\" 추가".to_string(),
            });
        }
    } else if !points_at_gateway {
        findings.push(DoctorFinding {
            level: "ok".to_string(),
            topic: "gateway".to_string(),
            message: "게이트웨이 비활성, 기본 URL 이 게이트웨이를 가리키지 않음".to_string(),
            fix_hint: String::new(),
        });
    }
}

// 9. projects 폴더 점검 -------------------------------------------------------
fn check_projects_dir(config: &AppConfig, findings: &mut Vec<DoctorFinding>) {
    let projects_dir = config.projects_dir();
    let exists = projects_dir.is_dir();
    findings.push(DoctorFinding {
        level: if exists { "ok" } else { "warn" }.to_string(),
        topic: "projects-dir".to_string(),
        message: if exists {
            format!("projects 폴더 존재: {}", projects_dir.display())
        } else {
            format!("projects 폴더 없음: {}", projects_dir.display())
        },
        fix_hint: if exists {
            String::new()
        } else {
            "rap.toml 의 claude_home 또는 indexer.projects_dir 확인".to_string()
        },
    });
}
