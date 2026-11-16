//! core/config.rs
//! rap.toml 설정 로더와 데이터·Claude 홈 경로 결정.

use anyhow::{Context, bail};
use hyper::Uri;
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAME: &str = "rap.toml";
pub const DB_FILE_NAME: &str = "rap.sqlite";
pub const TOKEN_FILE_NAME: &str = "admin.token";
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

// 10. 설정 적재 ---------------------------------------------------------------
impl AppConfig {
    /// 명시 경로 > RAP_CONFIG > 데이터 폴더 rap.toml > 기본값 순 적재.
    pub fn load(explicit: Option<&Path>) -> anyhow::Result<Self> {
        // 1. 데이터 폴더
        let default_data_dir = match env::var_os("RAP_DATA_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => match env::var_os("LOCALAPPDATA") {
                Some(local) => PathBuf::from(local).join("rust-agents-proxy"),
                None => bail!("LOCALAPPDATA 와 RAP_DATA_DIR 모두 없음"),
            },
        };

        // 2. 설정 파일 탐색
        let candidate = match explicit {
            Some(path) => Some(path.to_path_buf()),
            None => match env::var_os("RAP_CONFIG") {
                Some(path) => Some(PathBuf::from(path)),
                None => {
                    let in_data_dir = default_data_dir.join(CONFIG_FILE_NAME);
                    in_data_dir.is_file().then_some(in_data_dir)
                }
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

        // 4. Claude 홈
        let claude_home = match file.claude_home {
            Some(home) => home,
            None => match env::var_os("CLAUDE_CONFIG_DIR") {
                Some(dir) => PathBuf::from(dir),
                None => match env::var_os("USERPROFILE") {
                    Some(profile) => PathBuf::from(profile).join(".claude"),
                    None => bail!("USERPROFILE 과 CLAUDE_CONFIG_DIR 모두 없음"),
                },
            },
        };

        Ok(Self {
            data_dir: file.data_dir.unwrap_or(default_data_dir),
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

// 11. 설정 검증 ---------------------------------------------------------------
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

    // 1. 예시 설정 파싱 -------------------------------------------------------
    #[test]
    fn parses_example_config() {
        let text = include_str!("../../rap.example.toml");
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
}
