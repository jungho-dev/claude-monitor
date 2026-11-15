//! main.rs
//! rust-agents-proxy 명령 진입점 (serve, tui, index, report, hook, status, doctor).

use chrono::{Duration, Local, NaiveDate};
use clap::{Parser, Subcommand};
use rust_agents_proxy::core::config::{AppConfig, DB_FILE_NAME};
use rust_agents_proxy::store::Store;
use rust_agents_proxy::{hooks, inventory, net, report, serve, transcript, tui};
use std::path::PathBuf;
use std::process::ExitCode;

// 1. 명령줄 -------------------------------------------------------------------
#[derive(Debug, Parser)]
#[command(name = "rust-agents-proxy", version, about = "Claude Code 로컬 허브")]
struct CliArgs {
    /// rap.toml 경로 (기본: 데이터 폴더)
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
    /// 상주 서버 상태 출력
    Status,
    /// 설정·포트·훅 등록 진단
    Doctor,
}

// 2. 진입점 -------------------------------------------------------------------
fn main() -> ExitCode {
    let args = CliArgs::parse();
    let config = match AppConfig::load(args.config.as_deref()) {
        Ok(config) => config,
        Err(err) => {
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
        CliCommand::Status => net::admin::fetch_status_blocking(config.ports.admin).map(|status| {
            println!(
                "{}",
                serde_json::to_string_pretty(&status).unwrap_or_default()
            );
            0
        }),
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
