//! core/config.rs
//! ~/.config/claude-monitor/ 설정 로더와 데이터·Claude 홈 경로 결정 (설정·DB·토큰·로그·리포트가 한 폴더).

use anyhow::{Context, bail};
use hyper::Uri;
use serde::Deserialize;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

pub const APP_DIR_NAME: &str = "claude-monitor";
pub const CONFIG_HOME_DIR_NAME: &str = ".config";
pub const CONFIG_FILE_NAME: &str = "config.toml";
pub const DB_FILE_NAME: &str = "cm.sqlite";
pub const TOKEN_FILE_NAME: &str = "admin.token";
pub const TUI_STATE_FILE_NAME: &str = "tui.toml";
const MAX_GUARD_DEADLINE_MS: u64 = 4500;
const MIN_GAP_THRESHOLD_SECS: u64 = 15;

// 1. 실행 설정 ----------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub data_dir: PathBuf,
    pub claude_home: PathBuf,
    pub ports: PortConfig,
    pub gateway: GatewayConfig,
    pub hooks: HookConfig,
    pub indexer: IndexerConfig,
    pub report: ReportConfig,
    pub power: PowerConfig,
    pub source_path: Option<PathBuf>,
}

// 2. 포트 설정 ----------------------------------------------------------------
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PortConfig {
    pub admin: u16,
    pub otlp: u16,
    pub gateway: u16,
    pub hooks: u16,
}

// 3. 게이트웨이 설정 ----------------------------------------------------------
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewayConfig {
    pub enabled: bool,
    pub upstream: String,
    pub upstream_timeout_secs: u64,
}

// 4. 훅 설정 ------------------------------------------------------------------
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HookConfig {
    pub enabled: bool,
    pub relay_target: String,
    pub rewrite_enabled: bool,
    pub guard_deadline_ms: u64,
}

// 5. 기록 색인 설정 -----------------------------------------------------------
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexerConfig {
    pub enabled: bool,
    pub projects_dir: Option<PathBuf>,
    pub interval_secs: u64,
    pub backfill_days: u32,
}

// 6. 일일 리포트 설정 ---------------------------------------------------------
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReportConfig {
    pub enabled: bool,
    pub dir: Option<PathBuf>,
    pub run_hour: u32,
}

// 7. 최신 대기 대응 설정 ------------------------------------------------------
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PowerConfig {
    pub enabled: bool,
    pub keep_awake: bool,
    pub awake_grace_secs: u64,
    pub gap_threshold_secs: u64,
    pub hint_window_secs: u64,
}

// 8. 설정 파일 형태 -----------------------------------------------------------
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConfigFile {
    data_dir: Option<PathBuf>,
    claude_home: Option<PathBuf>,
    ports: PortConfig,
    gateway: GatewayConfig,
    hooks: HookConfig,
    indexer: IndexerConfig,
    report: ReportConfig,
    power: PowerConfig,
}

// 9. 기본값 -------------------------------------------------------------------
impl Default for PortConfig {
    fn default() -> Self {
        Self {
            admin: 47330,
            otlp: 47331,
            gateway: 47332,
            hooks: 47333,
        }
    }
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            upstream: "https://api.anthropic.com".to_string(),
            upstream_timeout_secs: 900,
        }
    }
}

impl Default for HookConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            relay_target: "http://127.0.0.1:47321".to_string(),
            rewrite_enabled: true,
            guard_deadline_ms: 3000,
        }
    }
}

impl Default for IndexerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            projects_dir: None,
            interval_secs: 30,
            backfill_days: 7,
        }
    }
}

impl Default for ReportConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: None,
            run_hour: 0,
        }
    }
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            keep_awake: true,
            awake_grace_secs: 600,
            gap_threshold_secs: 60,
            hint_window_secs: 1800,
        }
    }
}

// 10. 환경 입력 ---------------------------------------------------------------
/// 설정 적재가 읽는 환경 변수 값. 테스트가 실제 환경 대신 값을 주입함.
#[derive(Debug, Default)]
pub struct ConfigEnv {
    pub data_dir: Option<OsString>,
    pub config: Option<OsString>,
    pub home: Option<OsString>,
    pub user_profile: Option<OsString>,
    pub claude_config_dir: Option<OsString>,
}

// 11. 경로 결정 ---------------------------------------------------------------
/// USERPROFILE, HOME 순으로 비어 있지 않은 값을 홈으로 골라 <홈>/.config/claude-monitor 를 돌려줌 (설정·데이터 공용 폴더).
pub fn resolve_home_app_dir(
    home: Option<OsString>,
    user_profile: Option<OsString>,
) -> Option<PathBuf> {
    let home_dir = [user_profile, home]
        .into_iter()
        .flatten()
        .find(|dir| !dir.is_empty())?;
    Some(
        PathBuf::from(home_dir)
            .join(CONFIG_HOME_DIR_NAME)
            .join(APP_DIR_NAME),
    )
}

/// 공용 폴더 아래 config.toml 경로.
pub fn resolve_home_config_path(
    home: Option<OsString>,
    user_profile: Option<OsString>,
) -> Option<PathBuf> {
    resolve_home_app_dir(home, user_profile).map(|dir| dir.join(CONFIG_FILE_NAME))
}

/// CM_DATA_DIR 이 있으면 그 값, 없으면 공용 폴더 <홈>/.config/claude-monitor, 홈도 없으면 None.
pub fn resolve_data_dir(
    data_dir_env: Option<OsString>,
    home: Option<OsString>,
    user_profile: Option<OsString>,
) -> Option<PathBuf> {
    match data_dir_env {
        Some(dir) => Some(PathBuf::from(dir)),
        None => resolve_home_app_dir(home, user_profile),
    }
}

/// TUI 상태 파일(tui.toml) 경로: 데이터 폴더와 무관하게 홈 공용 폴더(config.toml 옆), 홈을 알 수 없을 때만 데이터 폴더.
pub fn resolve_tui_state_path(
    data_dir: &Path,
    home: Option<OsString>,
    user_profile: Option<OsString>,
) -> PathBuf {
    match resolve_home_app_dir(home, user_profile) {
        Some(app_dir) => app_dir.join(TUI_STATE_FILE_NAME),
        None => data_dir.join(TUI_STATE_FILE_NAME),
    }
}

// 12. 설정 적재 ---------------------------------------------------------------
impl AppConfig {
    /// 명시 경로 > CM_CONFIG > ~/.config/claude-monitor/config.toml > 기본값 순 적재.
    pub fn load(explicit: Option<&Path>) -> anyhow::Result<Self> {
        let env_inputs = ConfigEnv {
            data_dir: env::var_os("CM_DATA_DIR"),
            config: env::var_os("CM_CONFIG"),
            home: env::var_os("HOME"),
            user_profile: env::var_os("USERPROFILE"),
            claude_config_dir: env::var_os("CLAUDE_CONFIG_DIR"),
        };
        Self::load_with_env(explicit, &env_inputs)
    }

    /// load 와 같은 순서로 적재하되 환경 값을 인자로 받음.
    pub fn load_with_env(explicit: Option<&Path>, env_inputs: &ConfigEnv) -> anyhow::Result<Self> {
        // 1. 데이터 폴더 기본값 (CM_DATA_DIR > 홈 공용 폴더, 설정 파일 data_dir 이 우선)
        let default_data_dir = resolve_data_dir(
            env_inputs.data_dir.clone(),
            env_inputs.home.clone(),
            env_inputs.user_profile.clone(),
        );

        // 2. 설정 파일 탐색
        let candidate = match explicit {
            Some(path) => Some(path.to_path_buf()),
            None => match &env_inputs.config {
                Some(path) => Some(PathBuf::from(path)),
                None => resolve_home_config_path(
                    env_inputs.home.clone(),
                    env_inputs.user_profile.clone(),
                )
                .filter(|path| path.is_file()),
            },
        };
        let (file, source_path) = match candidate {
            Some(path) => {
                let text = fs::read_to_string(&path)
                    .with_context(|| format!("설정 파일 읽기 실패: {}", path.display()))?;
                let parsed: ConfigFile = toml::from_str(&text)
                    .with_context(|| format!("설정 파일 형식 오류: {}", path.display()))?;
                (parsed, Some(path))
            }
            None => (ConfigFile::default(), None),
        };

        // 3. 값 검증
        validate_config_file(&file)?;

        // 4. Claude 홈 (설정 파일 claude_home > CLAUDE_CONFIG_DIR > USERPROFILE/.claude > HOME/.claude, 빈 값은 없는 것으로 봄)
        let claude_home = match file.claude_home {
            Some(home) => home,
            None => {
                let from_env = env_inputs
                    .claude_config_dir
                    .iter()
                    .filter(|dir| !dir.is_empty())
                    .map(PathBuf::from)
                    .next();
                let from_home = [&env_inputs.user_profile, &env_inputs.home]
                    .into_iter()
                    .flatten()
                    .find(|dir| !dir.is_empty())
                    .map(|dir| PathBuf::from(dir).join(".claude"));
                match from_env.or(from_home) {
                    Some(dir) => dir,
                    None => bail!("CLAUDE_CONFIG_DIR, USERPROFILE, HOME 모두 없음"),
                }
            }
        };

        // 5. 데이터 폴더 확정
        let Some(data_dir) = file.data_dir.or(default_data_dir) else {
            bail!("USERPROFILE, HOME, CM_DATA_DIR 모두 없음");
        };

        Ok(Self {
            data_dir,
            claude_home,
            ports: file.ports,
            gateway: file.gateway,
            hooks: file.hooks,
            indexer: file.indexer,
            report: file.report,
            power: file.power,
            source_path,
        })
    }

    /// 기동 대상 포트 (이름, 번호, 사용 여부), 관리·OTLP·훅·게이트웨이 순.
    pub fn planned_ports(&self) -> Vec<(&'static str, u16, bool)> {
        vec![
            ("admin", self.ports.admin, true),
            ("otlp", self.ports.otlp, true),
            ("hooks", self.ports.hooks, self.hooks.enabled),
            ("gateway", self.ports.gateway, self.gateway.enabled),
        ]
    }

    /// 기록 색인 대상 projects 폴더.
    pub fn projects_dir(&self) -> PathBuf {
        match &self.indexer.projects_dir {
            Some(dir) => dir.clone(),
            None => self.claude_home.join("projects"),
        }
    }

    /// 일일 리포트 출력 폴더.
    pub fn report_dir(&self) -> PathBuf {
        match &self.report.dir {
            Some(dir) => dir.clone(),
            None => self.data_dir.join("reports"),
        }
    }
}

// 13. 설정 검증 ---------------------------------------------------------------
fn validate_config_file(file: &ConfigFile) -> anyhow::Result<()> {
    // 1. 포트 (0 과 중복 거부)
    let ports = &file.ports;
    let all_ports = [ports.admin, ports.otlp, ports.gateway, ports.hooks];
    for (index, port) in all_ports.iter().enumerate() {
        if *port == 0 || all_ports[index + 1..].contains(port) {
            bail!("포트 값 오류 (0 또는 중복): {port}");
        }
    }

    // 2. 시간 제한과 리포트 시각
    if file.gateway.upstream_timeout_secs == 0 {
        bail!("gateway.upstream_timeout_secs 는 1 이상");
    }
    if file.report.run_hour > 23 {
        bail!("report.run_hour 범위 오류: {}", file.report.run_hour);
    }
    if file.power.gap_threshold_secs < MIN_GAP_THRESHOLD_SECS {
        bail!(
            "power.gap_threshold_secs 는 {MIN_GAP_THRESHOLD_SECS} 이상: {}",
            file.power.gap_threshold_secs
        );
    }

    // 3. 가드 제한 시간 (0 은 전 호출 거부, 상한은 훅 제한 안 두 번 호출)
    let deadline_ms = file.hooks.guard_deadline_ms;
    if deadline_ms == 0 || deadline_ms > MAX_GUARD_DEADLINE_MS {
        bail!("hooks.guard_deadline_ms 범위 오류 (1~{MAX_GUARD_DEADLINE_MS}): {deadline_ms}");
    }

    // 4. 중계 대상 (평문 http 는 루프백만, 자기 포트 거부)
    let target = &file.hooks.relay_target;
    let parsed_target = target.parse::<Uri>();
    let loopback = match &parsed_target {
        Ok(uri) => {
            uri.scheme_str() == Some("http")
                && matches!(uri.host(), Some("127.0.0.1" | "localhost" | "[::1]"))
        }
        Err(_) => false,
    };
    if !loopback {
        bail!("hooks.relay_target 은 http://127.0.0.1:<포트> 형태의 루프백만 허용: {target}");
    }
    if let Some(relay_port) = parsed_target.ok().and_then(|uri| uri.port_u16())
        && all_ports.contains(&relay_port)
    {
        bail!("hooks.relay_target 이 자기 포트를 가리킴 (훅 중계 순환): {relay_port}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    // 1. 예시 설정 파싱 -------------------------------------------------------
    #[test]
    fn parses_example_config() {
        let text = include_str!("../../docs/config.example.toml");
        let parsed: ConfigFile = toml::from_str(text).expect("예시 설정 파싱");
        assert_eq!(parsed.ports.hooks, 47333);
        assert_eq!(parsed.hooks.relay_target, "http://127.0.0.1:47321");
        assert!(parsed.gateway.enabled);
    }

    // 2. 포트 0·중복 거부 -----------------------------------------------------
    #[test]
    fn rejects_zero_and_duplicate_ports() {
        let zero = ConfigFile {
            ports: PortConfig {
                otlp: 0,
                ..PortConfig::default()
            },
            ..ConfigFile::default()
        };
        let message = format!("{:#}", validate_config_file(&zero).expect_err("0 거부"));
        assert!(message.contains("포트 값 오류"), "{message}");

        let duplicate = ConfigFile {
            ports: PortConfig {
                hooks: 47330,
                ..PortConfig::default()
            },
            ..ConfigFile::default()
        };
        let message = format!(
            "{:#}",
            validate_config_file(&duplicate).expect_err("중복 거부")
        );
        assert!(message.contains("47330"), "{message}");
        validate_config_file(&ConfigFile::default()).expect("기본값 통과");
    }

    // 3. 상류 제한 시간 0 거부 ------------------------------------------------
    #[test]
    fn rejects_zero_upstream_timeout() {
        let file = ConfigFile {
            gateway: GatewayConfig {
                upstream_timeout_secs: 0,
                ..GatewayConfig::default()
            },
            ..ConfigFile::default()
        };
        let message = format!("{:#}", validate_config_file(&file).expect_err("0 거부"));
        assert!(message.contains("upstream_timeout_secs"), "{message}");
    }

    // 4. 리포트 시각 범위 -----------------------------------------------------
    #[test]
    fn rejects_run_hour_over_23() {
        let file = ConfigFile {
            report: ReportConfig {
                run_hour: 24,
                ..ReportConfig::default()
            },
            ..ConfigFile::default()
        };
        let message = format!("{:#}", validate_config_file(&file).expect_err("24 거부"));
        assert!(message.contains("run_hour"), "{message}");
        let edge = ConfigFile {
            report: ReportConfig {
                run_hour: 23,
                ..ReportConfig::default()
            },
            ..ConfigFile::default()
        };
        validate_config_file(&edge).expect("23 통과");
    }

    // 5. 비루프백 중계 대상 거부 ----------------------------------------------
    #[test]
    fn rejects_non_loopback_relay_target() {
        for target in [
            "http://10.0.0.5:47321",
            "https://127.0.0.1:47321",
            "http://127.0.0.1@evil.test:47321",
            "not a uri",
        ] {
            let file = ConfigFile {
                hooks: HookConfig {
                    relay_target: target.to_string(),
                    ..HookConfig::default()
                },
                ..ConfigFile::default()
            };
            let message = format!("{:#}", validate_config_file(&file).expect_err(target));
            assert!(
                message.contains("hooks.relay_target"),
                "{target}: {message}"
            );
        }
    }

    // 6. 루프백 중계 대상 허용 ------------------------------------------------
    #[test]
    fn accepts_default_relay_target() {
        validate_config_file(&ConfigFile::default()).expect("기본 중계 대상 통과");
        for target in [
            "http://localhost:47321",
            "http://[::1]:47321",
            "http://127.0.0.1:47321/",
        ] {
            let file = ConfigFile {
                hooks: HookConfig {
                    relay_target: target.to_string(),
                    ..HookConfig::default()
                },
                ..ConfigFile::default()
            };
            validate_config_file(&file).expect(target);
        }
    }

    // 7. 알 수 없는 키 거부 ---------------------------------------------------
    #[test]
    fn rejects_unknown_key() {
        assert!(toml::from_str::<ConfigFile>("[ports]\nadmn = 1").is_err());
        assert!(toml::from_str::<ConfigFile>("unknown_top = 1").is_err());
    }

    // 8. 기동 대상 포트 -------------------------------------------------------
    #[test]
    fn planned_ports_follow_enabled_flags() {
        let mut config = AppConfig {
            data_dir: PathBuf::from("data"),
            claude_home: PathBuf::from("claude"),
            ports: PortConfig::default(),
            gateway: GatewayConfig::default(),
            hooks: HookConfig::default(),
            indexer: IndexerConfig::default(),
            report: ReportConfig::default(),
            power: PowerConfig::default(),
            source_path: None,
        };
        let planned = config.planned_ports();
        let names: Vec<&str> = planned.iter().map(|(name, _, _)| *name).collect();
        assert_eq!(names, ["admin", "otlp", "hooks", "gateway"]);
        assert_eq!(planned[0], ("admin", 47330, true));
        assert_eq!(planned[2], ("hooks", 47333, true));
        assert_eq!(planned[3], ("gateway", 47332, true));

        config.hooks.enabled = false;
        config.gateway.enabled = false;
        let planned = config.planned_ports();
        assert!(!planned[2].2);
        assert!(!planned[3].2);
    }

    // 9. 가드 제한 시간 범위 --------------------------------------------------
    #[test]
    fn rejects_out_of_range_guard_deadline() {
        for deadline_ms in [0, MAX_GUARD_DEADLINE_MS + 1, 10_000] {
            let file = ConfigFile {
                hooks: HookConfig {
                    guard_deadline_ms: deadline_ms,
                    ..HookConfig::default()
                },
                ..ConfigFile::default()
            };
            let message = format!(
                "{:#}",
                validate_config_file(&file).expect_err("범위 밖 거부")
            );
            assert!(
                message.contains("guard_deadline_ms"),
                "{deadline_ms}: {message}"
            );
        }
        for deadline_ms in [1, 3000, MAX_GUARD_DEADLINE_MS] {
            let file = ConfigFile {
                hooks: HookConfig {
                    guard_deadline_ms: deadline_ms,
                    ..HookConfig::default()
                },
                ..ConfigFile::default()
            };
            validate_config_file(&file).expect("범위 안 통과");
        }
    }

    // 10. 자기 포트 중계 대상 거부 --------------------------------------------
    #[test]
    fn rejects_relay_target_on_own_port() {
        for target in [
            "http://127.0.0.1:47330",
            "http://127.0.0.1:47331/",
            "http://localhost:47332",
            "http://127.0.0.1:47333",
        ] {
            let file = ConfigFile {
                hooks: HookConfig {
                    relay_target: target.to_string(),
                    ..HookConfig::default()
                },
                ..ConfigFile::default()
            };
            let message = format!("{:#}", validate_config_file(&file).expect_err(target));
            assert!(message.contains("자기 포트"), "{target}: {message}");
        }
    }

    // 11. 대기 감지 기준 하한 -------------------------------------------------
    #[test]
    fn rejects_gap_threshold_below_minimum() {
        let short = ConfigFile {
            power: PowerConfig {
                gap_threshold_secs: MIN_GAP_THRESHOLD_SECS - 1,
                ..PowerConfig::default()
            },
            ..ConfigFile::default()
        };
        let message = format!(
            "{:#}",
            validate_config_file(&short).expect_err("하한 미만 거부")
        );
        assert!(message.contains("gap_threshold_secs"), "{message}");
        let edge = ConfigFile {
            power: PowerConfig {
                gap_threshold_secs: MIN_GAP_THRESHOLD_SECS,
                ..PowerConfig::default()
            },
            ..ConfigFile::default()
        };
        validate_config_file(&edge).expect("하한 통과");
    }

    // 12. 홈 설정 경로 결정 ---------------------------------------------------
    #[test]
    fn resolves_home_config_path_from_user_profile_then_home() {
        let suffix = Path::new(".config")
            .join("claude-monitor")
            .join("config.toml");
        assert_eq!(
            resolve_home_config_path(Some(OsString::from("H")), Some(OsString::from("U"))),
            Some(PathBuf::from("U").join(&suffix))
        );
        assert_eq!(
            resolve_home_config_path(Some(OsString::from("H")), None),
            Some(PathBuf::from("H").join(&suffix))
        );
        assert_eq!(
            resolve_home_config_path(Some(OsString::from("H")), Some(OsString::new())),
            Some(PathBuf::from("H").join(&suffix))
        );
        assert_eq!(resolve_home_config_path(Some(OsString::new()), None), None);
        assert_eq!(resolve_home_config_path(None, None), None);
    }

    // 13. 데이터 폴더 결정 (설정 파일과 같은 공용 폴더) -----------------------
    #[test]
    fn resolves_data_dir_from_env_then_home_app_dir() {
        let app_dir = Path::new(".config").join("claude-monitor");
        assert_eq!(
            resolve_data_dir(
                Some(OsString::from("D:/x")),
                Some(OsString::from("H")),
                Some(OsString::from("U"))
            ),
            Some(PathBuf::from("D:/x"))
        );
        assert_eq!(
            resolve_data_dir(None, Some(OsString::from("H")), Some(OsString::from("U"))),
            Some(PathBuf::from("U").join(&app_dir))
        );
        assert_eq!(
            resolve_data_dir(None, Some(OsString::from("H")), None),
            Some(PathBuf::from("H").join(&app_dir))
        );
        assert_eq!(resolve_data_dir(None, None, None), None);
        assert_eq!(
            resolve_data_dir(None, Some(OsString::from("H")), None),
            resolve_home_config_path(Some(OsString::from("H")), None)
                .and_then(|path| path.parent().map(Path::to_path_buf))
        );
    }

    // 14. 설정 파일 탐색 순서 -------------------------------------------------
    #[test]
    fn load_prefers_explicit_then_env_then_home_config() {
        let root = create_temp_root("order");
        let home = root.join("home");
        let home_config = home
            .join(".config")
            .join("claude-monitor")
            .join("config.toml");
        fs::create_dir_all(home_config.parent().expect("부모 폴더")).expect("홈 설정 폴더 생성");
        fs::write(&home_config, "[ports]\nadmin = 41001\n").expect("홈 설정 생성");
        let env_config = root.join("env.toml");
        fs::write(&env_config, "[ports]\nadmin = 41002\n").expect("환경 설정 생성");
        let explicit_config = root.join("explicit.toml");
        fs::write(&explicit_config, "[ports]\nadmin = 41003\n").expect("명시 설정 생성");
        let env_inputs = ConfigEnv {
            data_dir: Some(root.join("data").into_os_string()),
            config: Some(env_config.clone().into_os_string()),
            home: Some(home.clone().into_os_string()),
            claude_config_dir: Some(root.join("claude").into_os_string()),
            ..ConfigEnv::default()
        };

        let explicit = AppConfig::load_with_env(Some(&explicit_config), &env_inputs).expect("명시");
        assert_eq!(explicit.ports.admin, 41003);
        assert_eq!(explicit.source_path, Some(explicit_config));

        let from_env = AppConfig::load_with_env(None, &env_inputs).expect("환경 변수");
        assert_eq!(from_env.ports.admin, 41002);
        assert_eq!(from_env.source_path, Some(env_config));

        let home_only = ConfigEnv {
            config: None,
            ..env_inputs
        };
        let from_home = AppConfig::load_with_env(None, &home_only).expect("홈 설정");
        assert_eq!(from_home.ports.admin, 41001);
        assert_eq!(from_home.source_path, Some(home_config));
        fs::remove_dir_all(&root).expect("임시 폴더 정리");
    }

    // 15. 홈 설정이 없으면 기본값 (데이터 폴더의 다른 toml 은 읽지 않음) ------
    #[test]
    fn load_uses_defaults_without_home_config_and_ignores_data_dir_toml() {
        let root = create_temp_root("defaults");
        let data_dir = root.join("data");
        fs::create_dir_all(&data_dir).expect("데이터 폴더 생성");
        fs::write(data_dir.join("other.toml"), "[ports]\nadmin = 41009\n").expect("다른 설정");
        let home = root.join("home");
        fs::create_dir_all(&home).expect("홈 폴더 생성");
        let with_home = ConfigEnv {
            data_dir: Some(data_dir.clone().into_os_string()),
            home: Some(home.into_os_string()),
            claude_config_dir: Some(root.join("claude").into_os_string()),
            ..ConfigEnv::default()
        };

        let loaded = AppConfig::load_with_env(None, &with_home).expect("기본값");

        assert_eq!(loaded.ports.admin, 47330);
        assert_eq!(loaded.source_path, None);
        assert_eq!(loaded.data_dir, data_dir);

        let without_home = ConfigEnv {
            home: None,
            ..with_home
        };
        let loaded = AppConfig::load_with_env(None, &without_home).expect("홈 없음");
        assert_eq!(loaded.source_path, None);
        fs::remove_dir_all(&root).expect("임시 폴더 정리");
    }

    // 16. 입력 없음 거부 ------------------------------------------------------
    // Claude 홈 오류와 데이터 폴더 오류를 각각 그 경로로만 확인
    #[test]
    fn load_rejects_missing_data_dir_inputs() {
        // Claude 홈은 CLAUDE_CONFIG_DIR 로 정해져 데이터 폴더 단계까지 가고, 홈·CM_DATA_DIR 가 없어 거기서 실패
        let env_inputs = ConfigEnv {
            claude_config_dir: Some(OsString::from("C:/claude-test")),
            ..ConfigEnv::default()
        };
        let message = format!(
            "{:#}",
            AppConfig::load_with_env(None, &env_inputs).expect_err("데이터 폴더 입력 없음")
        );
        assert!(message.contains("CM_DATA_DIR"), "{message}");
        assert!(!message.contains("CLAUDE_CONFIG_DIR"), "{message}");
    }

    #[test]
    fn load_rejects_missing_claude_home_inputs() {
        let env_inputs = ConfigEnv {
            data_dir: Some(OsString::from("C:/data-test")),
            ..ConfigEnv::default()
        };
        let message = format!(
            "{:#}",
            AppConfig::load_with_env(None, &env_inputs).expect_err("Claude 홈 입력 없음")
        );
        assert!(message.contains("CLAUDE_CONFIG_DIR"), "{message}");
        assert!(!message.contains("CM_DATA_DIR"), "{message}");
    }

    // 16-1. Claude 홈 결정 순서 -----------------------------------------------
    #[test]
    fn claude_home_falls_back_to_home_dot_claude_when_only_home_is_set() {
        // Linux·macOS 처럼 HOME 만 있는 환경
        let env_inputs = ConfigEnv {
            home: Some(OsString::from("/home/tester")),
            ..ConfigEnv::default()
        };
        let loaded = AppConfig::load_with_env(None, &env_inputs).expect("HOME 만");
        assert_eq!(
            loaded.claude_home,
            Path::new("/home/tester").join(".claude")
        );
        assert_eq!(
            loaded.data_dir,
            Path::new("/home/tester")
                .join(".config")
                .join("claude-monitor")
        );
    }

    #[test]
    fn claude_home_prefers_config_dir_then_user_profile_then_home_and_skips_empty_values() {
        let all = ConfigEnv {
            data_dir: Some(OsString::from("C:/data-test")),
            home: Some(OsString::from("H")),
            user_profile: Some(OsString::from("U")),
            claude_config_dir: Some(OsString::from("C")),
            ..ConfigEnv::default()
        };
        let claude_home = |env_inputs: &ConfigEnv| {
            AppConfig::load_with_env(None, env_inputs)
                .expect("Claude 홈")
                .claude_home
        };
        assert_eq!(claude_home(&all), PathBuf::from("C"));

        let without_config_dir = ConfigEnv {
            claude_config_dir: Some(OsString::new()),
            ..all
        };
        assert_eq!(
            claude_home(&without_config_dir),
            Path::new("U").join(".claude")
        );

        let without_profile = ConfigEnv {
            user_profile: Some(OsString::new()),
            ..without_config_dir
        };
        assert_eq!(
            claude_home(&without_profile),
            Path::new("H").join(".claude")
        );
    }

    // 17. 기본 데이터 폴더는 설정 파일 폴더와 같음 ----------------------------
    #[test]
    fn load_defaults_data_dir_to_home_app_dir() {
        let root = create_temp_root("shared-dir");
        let home = root.join("home");
        let env_inputs = ConfigEnv {
            home: Some(home.clone().into_os_string()),
            claude_config_dir: Some(root.join("claude").into_os_string()),
            ..ConfigEnv::default()
        };

        let loaded = AppConfig::load_with_env(None, &env_inputs).expect("기본 폴더");

        assert_eq!(loaded.data_dir, home.join(".config").join("claude-monitor"));
        assert_eq!(
            loaded.report_dir(),
            home.join(".config").join("claude-monitor").join("reports")
        );
        fs::remove_dir_all(&root).expect("임시 폴더 정리");
    }

    // 18. TUI 상태 파일 경로 (데이터 폴더와 무관하게 홈 공용 폴더) ------------
    #[test]
    fn tui_state_path_follows_the_home_app_dir_not_the_data_dir() {
        let home_state = Path::new("U")
            .join(".config")
            .join("claude-monitor")
            .join("tui.toml");

        // 홈이 있으면 데이터 폴더가 무엇이든 홈 공용 폴더
        assert_eq!(
            resolve_tui_state_path(
                Path::new("D:/data"),
                Some(OsString::from("H")),
                Some(OsString::from("U"))
            ),
            home_state
        );
        assert_eq!(
            resolve_tui_state_path(Path::new("D:/other"), None, Some(OsString::from("U"))),
            home_state
        );
        assert_eq!(
            resolve_tui_state_path(Path::new("D:/data"), Some(OsString::from("H")), None),
            Path::new("H")
                .join(".config")
                .join("claude-monitor")
                .join("tui.toml")
        );

        // 빈 홈 값은 없는 것으로 보고 데이터 폴더로 대체
        assert_eq!(
            resolve_tui_state_path(
                Path::new("D:/data"),
                Some(OsString::new()),
                Some(OsString::new())
            ),
            Path::new("D:/data").join("tui.toml")
        );
        assert_eq!(
            resolve_tui_state_path(Path::new("D:/data"), None, None),
            Path::new("D:/data").join("tui.toml")
        );
    }

    // 19. CM_DATA_DIR 로 데이터 폴더를 옮겨도 상태 파일은 config.toml 옆 ------
    #[test]
    fn tui_state_path_stays_next_to_config_when_data_dir_is_overridden() {
        let root = create_temp_root("state-path");
        let home = root.join("home");
        let moved_data = root.join("moved-data");
        let env_inputs = ConfigEnv {
            data_dir: Some(moved_data.clone().into_os_string()),
            user_profile: Some(home.clone().into_os_string()),
            claude_config_dir: Some(root.join("claude").into_os_string()),
            ..ConfigEnv::default()
        };

        let loaded = AppConfig::load_with_env(None, &env_inputs).expect("데이터 폴더 이동");
        assert_eq!(loaded.data_dir, moved_data);

        let state_path = resolve_tui_state_path(
            &loaded.data_dir,
            env_inputs.home.clone(),
            env_inputs.user_profile.clone(),
        );
        assert_eq!(
            state_path,
            home.join(".config").join("claude-monitor").join("tui.toml")
        );
        assert!(!state_path.starts_with(&moved_data));
        fs::remove_dir_all(&root).expect("임시 폴더 정리");
    }

    // 20. 임시 폴더 -----------------------------------------------------------
    fn create_temp_root(label: &str) -> PathBuf {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let root = env::temp_dir().join(format!(
            "cm-config-{label}-{}-{nanos}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("임시 폴더 생성");
        root
    }
}
