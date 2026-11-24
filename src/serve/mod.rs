//! serve/mod.rs
//! 상주 실행: 포트 선점, 저장소·토큰 준비, 포트별 서버와 배경 작업 기동, 작업 감독, 종료 신호 대기.

use crate::core::config::{AppConfig, DB_FILE_NAME, TOKEN_FILE_NAME};
use crate::core::context::{AppContext, LIVE_CHANNEL_CAPACITY, LiveCounters};
use crate::core::model::LiveEvent;
use crate::hooks::client::blocking_http_request;
use crate::power::PowerState;
use crate::store::Store;
use crate::{hooks, net, power, report, transcript};
use anyhow::{Context, bail};
use chrono::{SecondsFormat, Utc};
use ring::rand::{SecureRandom, SystemRandom};
use std::collections::HashMap;
use std::fs;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, OnceLock};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};
use tokio::runtime::Builder;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinSet;

pub const SERVE_LOG_FILE_NAME: &str = "serve.log";
// 콘솔 없는 분리 프로세스 (DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
#[cfg(windows)]
const DETACHED_FLAGS: u32 = 0x0000_0008 | 0x0000_0200;
// 부모 Job 이탈 (Job 종료 시 동반 종료 방지, 불허 시 미적용 재시도)
#[cfg(windows)]
const BREAKAWAY_FLAG: u32 = 0x0100_0000;
const READY_TIMEOUT: Duration = Duration::from_secs(8);
// 거부된 루프백 connect 는 타임아웃까지 대기하므로 짧게 유지
const ADMIN_PROBE_TIMEOUT: Duration = Duration::from_millis(100);
const READY_HTTP_TIMEOUT: Duration = Duration::from_millis(500);
const READY_POLL_SLEEP: Duration = Duration::from_millis(50);

// 이 프로세스가 실제로 바인드한 서버 이름 (상태 응답의 수신 여부 근거)
pub(crate) static BOUND_LISTENER_NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();

// 이 프로세스 실행 파일의 빌드 식별자 "크기-수정시각ms" (serve·TUI 빌드 일치 판정)
// 실행 중 파일이 새 빌드로 바뀌어도 기동 당시 값을 유지하도록 main 첫 줄에서 강제 초기화,
// 메타데이터를 못 읽으면 빈 값이고 비교 쪽은 빈 값이면 판정을 생략
pub static BUILD_ID: LazyLock<String> = LazyLock::new(|| {
    let Ok(metadata) = std::env::current_exe().and_then(fs::metadata) else {
        return String::new();
    };
    let Ok(modified) = metadata.modified() else {
        return String::new();
    };
    let Ok(elapsed) = modified.duration_since(UNIX_EPOCH) else {
        return String::new();
    };
    format!("{}-{}", metadata.len(), elapsed.as_millis())
});

// 1. 선점 리스너 --------------------------------------------------------------
#[derive(Debug)]
struct PlannedListeners {
    admin: TcpListener,
    otlp: Option<TcpListener>,
    hooks: Option<TcpListener>,
    gateway: Option<TcpListener>,
}

// 2. 포트 선점 ----------------------------------------------------------------
// 리스너를 서버까지 유지, OTLP·게이트웨이 실패는 생략
fn bind_planned_listeners(config: &AppConfig) -> anyhow::Result<PlannedListeners> {
    let mut bound: HashMap<&'static str, TcpListener> = HashMap::new();
    for (name, port, enabled) in config.planned_ports() {
        if !enabled {
            continue;
        }
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => {
                bound.insert(name, listener);
            }
            Err(err) if matches!(name, "admin" | "hooks") => {
                bail!("{name} 포트 {port} 사용 불가 (다른 serve 실행 중일 수 있음): {err}")
            }
            Err(err) => eprintln!("[cm] {name} 포트 {port} 사용 불가, 해당 서버 생략: {err}"),
        }
    }
    let admin = bound.remove("admin").context("관리 리스너 없음")?;
    Ok(PlannedListeners {
        admin,
        otlp: bound.remove("otlp"),
        hooks: bound.remove("hooks"),
        gateway: bound.remove("gateway"),
    })
}

// 3. 상주 실행 ----------------------------------------------------------------
pub fn run_serve(config: AppConfig) -> anyhow::Result<()> {
    // 1. 게이트웨이 상류 검증 (포트 선점 전 빠른 실패)
    if config.gateway.enabled {
        net::gateway::validate_gateway_upstream(&config.gateway)?;
    }

    // 2. 포트 선점
    let PlannedListeners {
        admin: admin_listener,
        otlp: otlp_listener,
        hooks: hooks_listener,
        gateway: gateway_listener,
    } = bind_planned_listeners(&config)?;
    let mut bound_names: Vec<&'static str> = vec!["admin"];
    if otlp_listener.is_some() {
        bound_names.push("otlp");
    }
    if hooks_listener.is_some() {
        bound_names.push("hooks");
    }
    if gateway_listener.is_some() {
        bound_names.push("gateway");
    }
    let _ = BOUND_LISTENER_NAMES.set(bound_names);

    // 3. 저장소
    let store = Store::open_store(&config.data_dir.join(DB_FILE_NAME))?;

    // 4. 관리 토큰 (OS 난수, 실행마다 교체)
    let mut token_bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut token_bytes)
        .map_err(|_| anyhow::anyhow!("OS 난수 생성 실패"))?;
    let admin_token: String = token_bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let token_path = config.data_dir.join(TOKEN_FILE_NAME);
    fs::write(&token_path, &admin_token)
        .with_context(|| format!("관리 토큰 저장 실패: {}", token_path.display()))?;

    // 5. 공유 상태
    let (live_tx, _) = broadcast::channel(LIVE_CHANNEL_CAPACITY);
    let ctx = Arc::new(AppContext {
        config,
        store,
        live_tx,
        counters: LiveCounters::default(),
        started_at: Utc::now(),
        admin_token,
        power: PowerState::default(),
    });
    if ctx.config.power.enabled
        && let Err(err) = power::spawn_power_monitor(Arc::clone(&ctx))
    {
        eprintln!("[cm] 대기 감시 스레드 생성 실패 (serve 계속): {err}");
    }

    // 6. 게이트웨이 전용 런타임 (훅·색인 부하와 스트리밍 중계 분리)
    let mut gateway_exit_rx: Option<oneshot::Receiver<String>> = None;
    if let Some(gateway_listener) = gateway_listener {
        let gateway_ctx = Arc::clone(&ctx);
        let (gateway_exit_tx, receiver) = oneshot::channel::<String>();
        gateway_exit_rx = Some(receiver);
        thread::Builder::new()
            .name("cm-gateway".to_string())
            .spawn(move || {
                let exit_reason = match Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("cm-gateway-worker")
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => {
                        match runtime.block_on(net::gateway::run_gateway_server_on(
                            gateway_ctx,
                            gateway_listener,
                        )) {
                            Ok(()) => "서버가 반환됨".to_string(),
                            Err(err) => format!("{err:#}"),
                        }
                    }
                    Err(err) => format!("런타임 생성 실패: {err}"),
                };
                eprintln!("[cm] 게이트웨이 종료: {exit_reason}");
                let _ = gateway_exit_tx.send(exit_reason);
            })
            .context("게이트웨이 스레드 생성 실패")?;
    }

    // 7. 주 런타임 (관리, OTLP, 훅, 색인, 리포트)
    let runtime = Builder::new_multi_thread()
        .thread_name("cm-main")
        .enable_all()
        .build()
        .context("주 런타임 생성 실패")?;
    runtime.block_on(async move {
        let mut tasks: JoinSet<(&'static str, anyhow::Result<()>)> = JoinSet::new();
        let admin_server = net::admin::run_admin_server_on(Arc::clone(&ctx), admin_listener);
        tasks.spawn(async move { ("admin", admin_server.await) });
        if let Some(otlp_listener) = otlp_listener {
            let otlp_server = net::otlp::run_otlp_server_on(Arc::clone(&ctx), otlp_listener);
            tasks.spawn(async move { ("otlp", otlp_server.await) });
        }
        if let Some(hooks_listener) = hooks_listener {
            let hook_server = hooks::run_hook_server_on(Arc::clone(&ctx), hooks_listener);
            tasks.spawn(async move { ("hooks", hook_server.await) });
        }
        if ctx.config.indexer.enabled {
            let indexer = transcript::run_indexer_loop(Arc::clone(&ctx));
            tasks.spawn(async move { ("indexer", indexer.await) });
        }
        if ctx.config.report.enabled {
            let scheduler = report::run_report_scheduler(Arc::clone(&ctx));
            tasks.spawn(async move { ("report", scheduler.await) });
        }
        eprintln!(
            "[cm] serve 시작: admin {} / otlp {} / hooks {} / gateway {} ({})",
            ctx.config.ports.admin,
            ctx.config.ports.otlp,
            ctx.config.ports.hooks,
            ctx.config.ports.gateway,
            if ctx.config.gateway.enabled {
                "on"
            } else {
                "off"
            },
        );

        // 8. 종료 신호와 작업 감독
        let notice_ctx = Arc::clone(&ctx);
        let notify = move |text: &str| {
            eprintln!("[cm] {text}");
            let _ = notice_ctx.live_tx.send(LiveEvent::Notice {
                ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                level: "warn".to_string(),
                text: text.to_string(),
            });
        };
        let outcome: anyhow::Result<()> = tokio::select! {
            Ok(()) = tokio::signal::ctrl_c() => {
                eprintln!("[cm] 종료 신호 수신");
                Ok(())
            }
            supervised = supervise(tasks, gateway_exit_rx, notify) => supervised,
        };
        outcome
    })
}

// 4. 작업 감독 ----------------------------------------------------------------
/// 관리·훅 종료와 panic 은 serve 중단, 그 외 종료는 알림 후 계속 대기.
async fn supervise(
    mut tasks: JoinSet<(&'static str, anyhow::Result<()>)>,
    mut gateway_exit_rx: Option<oneshot::Receiver<String>>,
    notify: impl Fn(&str),
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            joined = tasks.join_next() => {
                // 1. 종료 작업 판별 (panic 은 중단)
                let (name, outcome) = match joined {
                    Some(Ok(finished)) => finished,
                    Some(Err(err)) => {
                        eprintln!("[cm] 작업 panic: {err}");
                        return Err(anyhow::anyhow!("작업 panic 으로 serve 중단: {err}"));
                    }
                    None => {
                        eprintln!("[cm] 작업 종료: 실행 중인 작업 없음");
                        return Err(anyhow::anyhow!("작업 종료로 serve 중단: 실행 중인 작업 없음"));
                    }
                };
                let reason = match outcome {
                    Ok(()) => "작업이 반환됨".to_string(),
                    Err(err) => format!("{err:#}"),
                };

                // 2. 필수 작업은 중단, 선택 작업은 알림 후 계속
                if matches!(name, "admin" | "hooks") {
                    eprintln!("[cm] 작업 종료: {name}: {reason}");
                    return Err(anyhow::anyhow!("작업 종료로 serve 중단: {name}: {reason}"));
                }
                notify(&format!("{name} 종료 (serve 계속): {reason}"));
            }
            gateway_exit = async {
                match gateway_exit_rx.as_mut() {
                    Some(receiver) => receiver.await,
                    None => std::future::pending::<Result<String, oneshot::error::RecvError>>().await,
                }
            } => {
                gateway_exit_rx = None;
                let reason = gateway_exit.unwrap_or_else(|_| "스레드 비정상 종료".to_string());
                notify(&format!("게이트웨이 종료 (serve 계속): {reason}"));
            }
        }
    }
}

// 5. 배경 상주 기동 -----------------------------------------------------------
/// 관리 포트 미수신 시 같은 exe 를 serve 로 분리 실행, 기동 여부 반환.
pub fn ensure_background_serve(
    config: &AppConfig,
    config_path: Option<&Path>,
) -> anyhow::Result<bool> {
    // 1. 이미 상주 중 (다른 빌드면 재시작 안내, 상태 조회 실패는 판정할 수 없어 안내 생략)
    let admin_addr = SocketAddr::from(([127, 0, 0, 1], config.ports.admin));
    let running = TcpStream::connect_timeout(&admin_addr, ADMIN_PROBE_TIMEOUT).is_ok();
    let stale_pid = match running.then(|| net::admin::fetch_status_blocking(config.ports.admin)) {
        Some(Ok(status)) if !BUILD_ID.is_empty() && status.build_id != *BUILD_ID => {
            Some(status.pid)
        }
        _ => None,
    };
    if let Some(pid) = stale_pid {
        eprintln!(
            "[cm] 상주 중인 serve(pid {pid})가 이 실행 파일과 다른 빌드임. serve 를 종료한 뒤 다시 시작해야 새 빌드로 수집함"
        );
    }
    if running {
        return Ok(false);
    }

    // 2. 로그 파일 (기동마다 새로 작성)
    fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("데이터 폴더 생성 실패: {}", config.data_dir.display()))?;
    let log_path = config.data_dir.join(SERVE_LOG_FILE_NAME);
    let log_file = fs::File::create(&log_path)
        .with_context(|| format!("serve 로그 생성 실패: {}", log_path.display()))?;

    // 3. 분리 프로세스 실행 (Job 이탈 우선, 거부 시 이탈 없이)
    let exe_path = std::env::current_exe().context("실행 파일 경로 확인 실패")?;
    let mut spawned = None;
    #[cfg(windows)]
    let flag_sets = [DETACHED_FLAGS | BREAKAWAY_FLAG, DETACHED_FLAGS];
    #[cfg(not(windows))]
    let flag_sets = [0u32];
    for flags in flag_sets {
        let mut command = Command::new(&exe_path);
        if let Some(path) = config_path {
            command.arg("--config").arg(path);
        }
        command
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log_file.try_clone()?));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(flags);
        }
        #[cfg(not(windows))]
        let _ = flags;
        match command.spawn() {
            Ok(child) => {
                spawned = Some(child);
                break;
            }
            Err(err) => eprintln!("[cm] serve 분리 실행 재시도 (flags {flags:#x}): {err}"),
        }
    }
    let Some(mut child) = spawned else {
        bail!("serve 기동 실패: {}", exe_path.display());
    };

    // 4. 관리 서버 응답 대기
    wait_until_serve_ready(admin_addr, READY_TIMEOUT, &log_path, || {
        child
            .try_wait()
            .ok()
            .flatten()
            .map(|status| status.to_string())
    })?;
    Ok(true)
}

// 6. 기동 완료 대기 -----------------------------------------------------------
/// 관리 서버 /health ok 응답까지 대기 (준비 완료 뒤에만 응답), 자식 종료 시 중단.
fn wait_until_serve_ready(
    admin_addr: SocketAddr,
    limit: Duration,
    log_path: &Path,
    mut child_exit_status: impl FnMut() -> Option<String>,
) -> anyhow::Result<()> {
    let started = Instant::now();
    while started.elapsed() < limit {
        // 1. 응답 확인 (포트 선점만으로는 통과 불가)
        let reply = blocking_http_request(
            &admin_addr.to_string(),
            "GET",
            "/health",
            b"",
            READY_HTTP_TIMEOUT,
        );
        if matches!(reply, Ok((200, body)) if body.as_slice() == b"ok") {
            return Ok(());
        }

        // 2. 자식 조기 종료
        if let Some(status) = child_exit_status() {
            bail!(
                "serve 가 즉시 종료됨 ({status}), 로그: {}",
                log_path.display()
            );
        }
        thread::sleep(READY_POLL_SLEEP);
    }
    bail!(
        "serve 기동 대기 시간 초과 ({:.1}초), 로그: {}",
        limit.as_secs_f32(),
        log_path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::{
        SERVE_LOG_FILE_NAME, bind_planned_listeners, ensure_background_serve, supervise,
        wait_until_serve_ready,
    };
    use crate::net::admin::test_support::build_test_config;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::oneshot;
    use tokio::task::JoinSet;

    // 1. 관리 포트 선점 실패 --------------------------------------------------
    #[test]
    fn bind_planned_listeners_rejects_busy_admin_before_other_ports() {
        let config = build_test_config("serve-admin-busy");
        let busy = TcpListener::bind(("127.0.0.1", config.ports.admin)).expect("포트 점유");
        let err = bind_planned_listeners(&config).expect_err("관리 포트 거부");
        let message = format!("{err:#}");
        assert!(
            message.contains(&config.ports.admin.to_string()),
            "{message}"
        );
        assert!(
            message.contains("다른 serve 실행 중일 수 있음"),
            "{message}"
        );

        // OTLP 포트는 아직 잡히지 않음
        let otlp = TcpListener::bind(("127.0.0.1", config.ports.otlp)).expect("OTLP 포트 미점유");
        drop(otlp);
        drop(busy);
    }

    // 2. 필수·선택 포트 구분 --------------------------------------------------
    #[test]
    fn bind_planned_listeners_rejects_busy_hooks_and_skips_busy_optional_ports() {
        // 1. 모두 비어 있음
        let config = build_test_config("serve-ports");
        let bound = bind_planned_listeners(&config).expect("빈 포트 통과");
        assert!(bound.otlp.is_some());
        assert!(bound.hooks.is_some());
        assert!(bound.gateway.is_some());
        drop(bound);

        // 2. 훅 포트 점유는 중단
        let hooks_busy = TcpListener::bind(("127.0.0.1", config.ports.hooks)).expect("포트 점유");
        let err = bind_planned_listeners(&config).expect_err("점유 훅 포트 거부");
        let message = format!("{err:#}");
        assert!(message.contains("hooks"), "{message}");
        assert!(
            message.contains(&config.ports.hooks.to_string()),
            "{message}"
        );
        drop(hooks_busy);

        // 3. OTLP 포트 점유는 생략
        let otlp_busy = TcpListener::bind(("127.0.0.1", config.ports.otlp)).expect("포트 점유");
        let bound = bind_planned_listeners(&config).expect("OTLP 생략 후 통과");
        assert!(bound.otlp.is_none());
        assert!(bound.hooks.is_some());
        drop(bound);
        drop(otlp_busy);

        // 4. 켜진 게이트웨이 포트 점유도 생략
        let gateway_busy =
            TcpListener::bind(("127.0.0.1", config.ports.gateway)).expect("게이트웨이 포트 점유");
        let bound = bind_planned_listeners(&config).expect("게이트웨이 생략 후 통과");
        assert!(bound.gateway.is_none());
        assert!(bound.otlp.is_some());
        drop(bound);
        drop(gateway_busy);
        let bound = bind_planned_listeners(&config).expect("해제 후 통과");
        assert!(bound.gateway.is_some());
    }

    // 3. 꺼진 서버 포트 제외 --------------------------------------------------
    #[test]
    fn bind_skips_disabled_hooks_and_gateway() {
        let mut config = build_test_config("serve-disabled");
        config.hooks.enabled = false;
        config.gateway.enabled = false;
        let hooks_busy = TcpListener::bind(("127.0.0.1", config.ports.hooks)).expect("포트 점유");
        let gateway_busy =
            TcpListener::bind(("127.0.0.1", config.ports.gateway)).expect("게이트웨이 포트 점유");
        let bound = bind_planned_listeners(&config).expect("꺼진 포트는 검사 제외");
        assert!(bound.hooks.is_none());
        assert!(bound.gateway.is_none());
        assert!(bound.otlp.is_some());
        drop(bound);
        drop(hooks_busy);
        drop(gateway_busy);
    }

    // 4. 훅 오류 종료 ---------------------------------------------------------
    #[tokio::test]
    async fn supervise_ends_on_hooks_error() {
        let mut tasks: JoinSet<(&'static str, anyhow::Result<()>)> = JoinSet::new();
        tasks.spawn(async { ("hooks", Err(anyhow::anyhow!("훅 바인드 실패"))) });
        let err = supervise(tasks, None, |_| {})
            .await
            .expect_err("훅 오류로 종료");
        let message = format!("{err:#}");
        assert!(message.contains("hooks"), "{message}");
        assert!(message.contains("훅 바인드 실패"), "{message}");
    }

    // 5. 선택 작업 종료 유지 --------------------------------------------------
    #[tokio::test]
    async fn supervise_keeps_running_when_otlp_task_ends() {
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let mut tasks: JoinSet<(&'static str, anyhow::Result<()>)> = JoinSet::new();
        tasks.spawn(async { ("otlp", Ok(())) });
        tasks.spawn(async move {
            let _ = release_rx.await;
            ("hooks", Err(anyhow::anyhow!("훅 종료")))
        });
        let notices: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&notices);
        let supervised = supervise(tasks, None, move |text| {
            sink.lock().expect("알림 잠금").push(text.to_string());
        });
        tokio::pin!(supervised);

        // 1. OTLP 종료 후에도 반환하지 않음
        let waited = tokio::time::timeout(Duration::from_millis(200), &mut supervised).await;
        assert!(waited.is_err(), "OTLP 종료로 serve 가 끝나면 안 됨");
        assert!(
            notices
                .lock()
                .expect("알림 잠금")
                .iter()
                .any(|text| text.contains("otlp"))
        );

        // 2. 훅 종료는 중단
        release_tx.send(()).expect("훅 종료 신호");
        let err = supervised.await.expect_err("훅 종료로 중단");
        assert!(format!("{err:#}").contains("훅 종료"));
    }

    // 6. 게이트웨이 종료 알림 -------------------------------------------------
    #[tokio::test]
    async fn supervise_ignores_gateway_exit_but_notifies() {
        let mut tasks: JoinSet<(&'static str, anyhow::Result<()>)> = JoinSet::new();
        tasks.spawn(async {
            std::future::pending::<()>().await;
            ("hooks", Ok(()))
        });
        let (gateway_tx, gateway_rx) = oneshot::channel::<String>();
        gateway_tx
            .send("업스트림 형식 오류".to_string())
            .expect("게이트웨이 종료 신호");
        let notices: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&notices);
        let supervised = supervise(tasks, Some(gateway_rx), move |text| {
            sink.lock().expect("알림 잠금").push(text.to_string());
        });
        tokio::pin!(supervised);
        let waited = tokio::time::timeout(Duration::from_millis(200), &mut supervised).await;
        assert!(waited.is_err(), "게이트웨이 종료로 serve 가 끝나면 안 됨");
        let recorded = notices.lock().expect("알림 잠금");
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].contains("게이트웨이"), "{recorded:?}");
        assert!(recorded[0].contains("업스트림 형식 오류"), "{recorded:?}");
    }

    // 7. panic 종료 -----------------------------------------------------------
    #[tokio::test]
    async fn supervise_treats_panic_as_fatal() {
        let mut tasks: JoinSet<(&'static str, anyhow::Result<()>)> = JoinSet::new();
        tasks.spawn(async { panic!("의도한 panic") });
        let err = supervise(tasks, None, |_| {})
            .await
            .expect_err("panic 으로 중단");
        assert!(format!("{err:#}").contains("panic"));
    }

    // 8. 상주 확인 시 기동 생략 -----------------------------------------------
    #[test]
    fn background_serve_returns_false_when_admin_answers() {
        let config = build_test_config("serve-background");
        let admin_listener =
            TcpListener::bind(("127.0.0.1", config.ports.admin)).expect("관리 포트 점유");
        let started = ensure_background_serve(&config, None).expect("상주 확인");
        assert!(!started);
        assert!(!config.data_dir.join(SERVE_LOG_FILE_NAME).exists());
        drop(admin_listener);
    }

    // 9. 선점만 된 관리 포트 --------------------------------------------------
    #[test]
    fn wait_ready_rejects_bound_port_that_never_answers() {
        // 1. 수락 없이 선점만 한 포트 (connect 는 백로그로 성공)
        let silent = TcpListener::bind(("127.0.0.1", 0)).expect("포트 선점");
        let admin_addr: SocketAddr = silent.local_addr().expect("주소");

        // 2. 응답 없으면 제한 시간 초과
        let err = wait_until_serve_ready(
            admin_addr,
            Duration::from_millis(800),
            Path::new("serve.log"),
            || None,
        )
        .expect_err("선점만으로 준비 완료 불가");
        let message = format!("{err:#}");
        assert!(message.contains("대기 시간 초과"), "{message}");
        drop(silent);
    }

    // 10. 자식 조기 종료 ------------------------------------------------------
    #[test]
    fn wait_ready_stops_when_child_exits() {
        let silent = TcpListener::bind(("127.0.0.1", 0)).expect("포트 선점");
        let admin_addr: SocketAddr = silent.local_addr().expect("주소");
        let err = wait_until_serve_ready(
            admin_addr,
            Duration::from_secs(5),
            Path::new("serve.log"),
            || Some("exit code: 1".to_string()),
        )
        .expect_err("자식 종료로 중단");
        let message = format!("{err:#}");
        assert!(message.contains("즉시 종료됨"), "{message}");
        assert!(message.contains("exit code: 1"), "{message}");
        drop(silent);
    }

    // 11. 응답하는 관리 포트 --------------------------------------------------
    #[test]
    fn wait_ready_accepts_health_ok() {
        // 1. /health 에 200 ok 로 답하는 임시 서버
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("포트 선점");
        let admin_addr: SocketAddr = listener.local_addr().expect("주소");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                // 요청 헤더 끝까지 수신 후 응답, 잔여 입력 비움 (RST 방지)
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
                let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
                while matches!(stream.read(&mut buffer), Ok(read) if read > 0) {}
            }
        });

        // 2. 응답 즉시 준비 완료
        wait_until_serve_ready(
            admin_addr,
            Duration::from_secs(5),
            Path::new("serve.log"),
            || None,
        )
        .expect("응답 시 준비 완료");
    }

    // 12. 빌드 식별자 ---------------------------------------------------------
    #[test]
    fn build_id_is_the_running_executable_size_and_modified_millis() {
        let exe_path = std::env::current_exe().expect("실행 파일 경로");
        let metadata = std::fs::metadata(exe_path).expect("실행 파일 메타데이터");
        let modified_ms = metadata
            .modified()
            .expect("수정 시각")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("기원 이후")
            .as_millis();
        assert_eq!(
            *super::BUILD_ID,
            format!("{}-{modified_ms}", metadata.len())
        );
    }
}
