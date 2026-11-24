//! main.rs
//! claude-monitor 명령 진입점 (serve, tui, index, report, hook, status, doctor).

use chrono::{Duration, Local, NaiveDate};
use clap::{Parser, Subcommand};
use claude_monitor::core::config::{AppConfig, DB_FILE_NAME};
use claude_monitor::statuscheck::StatusOnceOptions;
use claude_monitor::store::Store;
use claude_monitor::{
    hooks, inventory, net, report, serve, statuscheck, statusline, transcript, tui,
};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::LazyLock;

// 1. 명령줄 -------------------------------------------------------------------
#[derive(Debug, Parser)]
#[command(name = "claude-monitor", version, about = "Claude Code 로컬 허브")]
struct CliArgs {
    /// 설정 파일 경로 (기본: ~/.config/claude-monitor/config.toml, CM_CONFIG 로도 지정)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// 상주 서버 실행 (관리·OTLP·훅·게이트웨이 포트)
    Serve,
    /// serve 분리 상주 보장 후 종료 (로그온 실행용, TUI 없음)
    Start,
    /// 실시간 관제 TUI 만 실행 (명령 없이 실행하면 serve 자동 상주 + TUI)
    Tui,
    /// 세션 기록 1회 색인
    Index,
    /// 일일 리포트 생성 (기본: 어제)
    Report {
        #[arg(long)]
        date: Option<NaiveDate>,
    },
    /// command 훅 클라이언트 (stdin JSON 을 훅 포트로 중계)
    Hook { route: String },
    /// statusLine 연동 (stdin 지표를 훅 포트로 전송하고 한 줄 출력)
    Statusline {
        /// 기존 statusline 명령 감싸기 (-- 뒤에 명령과 인자, stdout 그대로 통과)
        #[arg(long, requires = "command")]
        wrap: bool,
        /// 감싼 명령 출력 뒤에 우리 한 줄을 새 줄로 덧붙임
        #[arg(long, requires = "wrap")]
        append: bool,
        #[arg(last = true, requires = "wrap", value_name = "COMMAND")]
        command: Vec<String>,
    },
    /// 상주 서버 상태 출력 (--once: DB 의 최신 한도 스냅샷을 판정해 종료 코드로 알림, serve 불필요)
    Status {
        /// 한도 판정 1회: 종료 코드 0 정상, 10 경고 이상, 11 100% 이상, 20 알 수 없음, 30 오류
        #[arg(long)]
        once: bool,
        /// 경고 기준 퍼센트 (5시간·주간 중 높은 값, 기본 80)
        #[arg(
            long,
            requires = "once",
            default_value_t = statuscheck::DEFAULT_WARN_PCT,
            value_parser = statuscheck::parse_warn_pct
        )]
        warn: f64,
        /// 스냅샷이 이보다 오래되면 알 수 없음 처리 (분, 기본 360)
        #[arg(
            long,
            requires = "once",
            default_value_t = statuscheck::DEFAULT_MAX_AGE_MINUTES,
            value_parser = clap::value_parser!(u64).range(1..)
        )]
        max_age: u64,
        /// 결과 JSON 을 원자적으로 쓸 파일 (위젯·스크립트 연동, 지정할 때만 씀)
        #[arg(long, requires = "once", value_name = "PATH")]
        state_file: Option<PathBuf>,
    },
    /// 설정·포트·훅 등록 진단
    Doctor,
}

// 2. 진입점 -------------------------------------------------------------------
fn main() -> ExitCode {
    // 빌드 식별자는 실행 파일이 교체되기 전 기동 시점 값으로 고정
    LazyLock::force(&serve::BUILD_ID);
    let args = CliArgs::parse();
    let config = match AppConfig::load(args.config.as_deref()) {
        Ok(config) => config,
        Err(err) => {
            // status --once 는 설정 오류도 종료 코드 30 (위젯·스크립트가 단계를 구분)
            if matches!(args.command, Some(CliCommand::Status { once: true, .. })) {
                let code = statuscheck::run_status_config_error(&format!("{err:#}"));
                return ExitCode::from(u8::try_from(code).unwrap_or(1));
            }
            eprintln!("설정 오류: {err:#}");
            return ExitCode::from(2);
        }
    };

    // 명령 없음: serve 분리 상주 보장 후 TUI (TUI 종료 후에도 serve 유지)
    let Some(command) = args.command else {
        if let Err(err) = serve::ensure_background_serve(&config, args.config.as_deref()) {
            eprintln!("serve 자동 기동 실패: {err:#}");
        }
        return match tui::run_tui(&config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("오류: {err:#}");
                ExitCode::from(1)
            }
        };
    };

    let outcome: anyhow::Result<i32> = match command {
        CliCommand::Serve => serve::run_serve(config).map(|_| 0),
        CliCommand::Start => {
            serve::ensure_background_serve(&config, args.config.as_deref()).map(|_| 0)
        }
        CliCommand::Tui => tui::run_tui(&config).map(|_| 0),
        CliCommand::Index => Store::open_store(&config.data_dir.join(DB_FILE_NAME))
            .and_then(|store| transcript::index_transcripts_once(&config, &store))
            .map(|summary| {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&summary).unwrap_or_default()
                );
                0
            }),
        CliCommand::Report { date } => {
            let report_date =
                date.unwrap_or_else(|| (Local::now() - Duration::days(1)).date_naive());
            Store::open_store(&config.data_dir.join(DB_FILE_NAME))
                .and_then(|store| report::write_daily_report(&config, &store, report_date))
                .map(|path| {
                    println!("{}", path.display());
                    0
                })
        }
        CliCommand::Hook { route } => hooks::run_hook_client(&config, &route),
        CliCommand::Statusline {
            append, command, ..
        } => statusline::run_statusline(&config, &command, append),
        CliCommand::Status {
            once: true,
            warn,
            max_age,
            state_file,
        } => Ok(statuscheck::run_status_once(
            &config,
            &StatusOnceOptions {
                warn_pct: warn,
                max_age_minutes: max_age,
                state_file,
            },
        )),
        CliCommand::Status { .. } => {
            net::admin::fetch_status_blocking(config.ports.admin).map(|status| {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&status).unwrap_or_default()
                );
                0
            })
        }
        CliCommand::Doctor => inventory::run_doctor_checks(&config).map(|findings| {
            let mut failed = false;
            for finding in &findings {
                failed |= finding.level == "fail";
                println!("[{}] {}: {}", finding.level, finding.topic, finding.message);
                if !finding.fix_hint.is_empty() {
                    println!("    -> {}", finding.fix_hint);
                }
            }
            i32::from(failed)
        }),
    };

    match outcome {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("오류: {err:#}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CliArgs, CliCommand};
    use clap::Parser;
    use std::path::PathBuf;

    // 1. statusline 인자 해석 -------------------------------------------------
    #[test]
    fn statusline_parses_wrap_and_rejects_dangling_flags() {
        // 1. 감싸기: -- 뒤 명령과 인자 보존
        let args = CliArgs::try_parse_from([
            "claude-monitor",
            "statusline",
            "--wrap",
            "--append",
            "--",
            "bash",
            "-c",
            "echo hi",
        ])
        .expect("감싸기 해석");
        let Some(CliCommand::Statusline {
            wrap,
            append,
            command,
        }) = args.command
        else {
            panic!("statusline 명령 기대");
        };
        assert!(wrap && append);
        assert_eq!(command, ["bash", "-c", "echo hi"]);

        // 2. 단독 실행
        let args = CliArgs::try_parse_from(["claude-monitor", "statusline"]).expect("단독 해석");
        let Some(CliCommand::Statusline {
            wrap,
            append,
            command,
        }) = args.command
        else {
            panic!("statusline 명령 기대");
        };
        assert!(!wrap && !append && command.is_empty());

        // 3. 명령 없는 --wrap 과 --wrap 없는 --append 는 오류
        assert!(CliArgs::try_parse_from(["claude-monitor", "statusline", "--wrap"]).is_err());
        assert!(CliArgs::try_parse_from(["claude-monitor", "statusline", "--append"]).is_err());
    }

    // 2. status --once 인자 해석 ----------------------------------------------
    #[test]
    fn status_once_parses_defaults_and_options() {
        // 1. 기본값
        let args = CliArgs::try_parse_from(["claude-monitor", "status", "--once"]).expect("기본");
        let Some(CliCommand::Status {
            once,
            warn,
            max_age,
            state_file,
        }) = args.command
        else {
            panic!("status 명령 기대");
        };
        assert!(once && state_file.is_none());
        assert_eq!((warn, max_age), (80.0, 360));

        // 2. 모든 옵션
        let args = CliArgs::try_parse_from([
            "claude-monitor",
            "status",
            "--once",
            "--warn",
            "92.5",
            "--max-age",
            "15",
            "--state-file",
            "out/status.json",
        ])
        .expect("옵션");
        let Some(CliCommand::Status {
            warn,
            max_age,
            state_file,
            ..
        }) = args.command
        else {
            panic!("status 명령 기대");
        };
        assert_eq!((warn, max_age), (92.5, 15));
        assert_eq!(state_file, Some(PathBuf::from("out/status.json")));

        // 3. 단독 status 는 기존 동작 (once 꺼짐)
        let args = CliArgs::try_parse_from(["claude-monitor", "status"]).expect("단독");
        assert!(matches!(
            args.command,
            Some(CliCommand::Status { once: false, .. })
        ));
    }

    #[test]
    fn status_once_options_require_once_and_reject_bad_values() {
        for bad in [
            ["status", "--warn", "80"].as_slice(),
            &["status", "--max-age", "60"],
            &["status", "--state-file", "a.json"],
            &["status", "--once", "--warn", "0"],
            &["status", "--once", "--warn", "101"],
            &["status", "--once", "--warn", "abc"],
            &["status", "--once", "--max-age", "0"],
        ] {
            let argv = std::iter::once("claude-monitor").chain(bad.iter().copied());
            assert!(CliArgs::try_parse_from(argv).is_err(), "{bad:?}");
        }
    }
}
