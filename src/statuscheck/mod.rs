//! statuscheck/mod.rs
//! `status --once`: DB 의 최신 한도 스냅샷을 읽기 전용으로 판정해 종료 코드·JSON 한 줄·선택적 상태 파일로 내보냄 (serve 없이 동작).

#[cfg(test)]
mod tests;

use crate::core::config::{AppConfig, DB_FILE_NAME};
use crate::store::Store;
use crate::store::limits::LimitSnapshot;
use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

// 1. 기본값과 종료 코드 -------------------------------------------------------
pub const DEFAULT_WARN_PCT: f64 = 80.0;
pub const DEFAULT_MAX_AGE_MINUTES: u64 = 360;
pub const EXIT_OK: i32 = 0;
pub const EXIT_WARN: i32 = 10;
pub const EXIT_LIMIT: i32 = 11;
pub const EXIT_UNKNOWN: i32 = 20;
pub const EXIT_ERROR: i32 = 30;
const STATUS_SOURCE: &str = "statusline";
const TEMP_SUFFIX: &str = ".tmp";
// 허용 나이 상한 (약 100년): 시간 연산 넘침 방지
const MAX_AGE_CAP_MINUTES: i64 = 52_560_000;

// 2. 판정 수준 ----------------------------------------------------------------
// ok < warn (경고 비율 이상) < limit (100 이상), unknown = 스냅샷 없음·오래됨·유효한 창 없음, error = 데이터·설정·DB·상태 파일 오류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusLevel {
    Ok,
    Warn,
    Limit,
    Unknown,
    Error,
}

// 3. 옵션 ---------------------------------------------------------------------
#[derive(Debug, Clone, PartialEq)]
pub struct StatusOnceOptions {
    pub warn_pct: f64,
    pub max_age_minutes: u64,
    pub state_file: Option<PathBuf>,
}

// 4. 출력 한 줄 (stdout 과 상태 파일 공용) ------------------------------------
// *_pct 는 만료되지 않은 창의 값만 (resets_at 이 지난 창은 null, resets_at 은 그대로), resets_at 은 Unix epoch 초
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusReport {
    pub level: StatusLevel,
    pub five_hour_pct: Option<f64>,
    pub five_hour_resets_at: Option<i64>,
    pub seven_day_pct: Option<f64>,
    pub seven_day_resets_at: Option<i64>,
    pub snapshot_ts: Option<String>,
    pub source: &'static str,
}

// 5. 실행 결과 ----------------------------------------------------------------
#[derive(Debug, Clone, PartialEq)]
pub struct StatusRun {
    pub report: StatusReport,
    pub exit_code: i32,
    pub error: Option<String>,
}

// 6. 경고 비율 인자 해석 (clap value_parser) ----------------------------------
// 0 초과 100 이하의 유한한 숫자만 허용
pub fn parse_warn_pct(text: &str) -> Result<f64, String> {
    let value: f64 = text
        .trim()
        .parse()
        .map_err(|_| format!("숫자가 아님: {text}"))?;
    if !value.is_finite() || value <= 0.0 || value > 100.0 {
        return Err(format!("0 초과 100 이하여야 함: {text}"));
    }
    Ok(value)
}

// 7. 수준 -> 종료 코드 --------------------------------------------------------
pub fn level_exit_code(level: StatusLevel) -> i32 {
    match level {
        StatusLevel::Ok => EXIT_OK,
        StatusLevel::Warn => EXIT_WARN,
        StatusLevel::Limit => EXIT_LIMIT,
        StatusLevel::Unknown => EXIT_UNKNOWN,
        StatusLevel::Error => EXIT_ERROR,
    }
}

// 8. 스냅샷 판정 --------------------------------------------------------------
// 스냅샷 시각을 읽을 수 없으면 오류 문자열, 그 밖에는 수준을 정한 보고서
pub fn evaluate_snapshot(
    snapshot: Option<&LimitSnapshot>,
    now: DateTime<Utc>,
    warn_pct: f64,
    max_age_minutes: u64,
) -> Result<StatusReport, String> {
    // 1. 스냅샷 없음
    let Some(snapshot) = snapshot else {
        return Ok(StatusReport {
            level: StatusLevel::Unknown,
            five_hour_pct: None,
            five_hour_resets_at: None,
            seven_day_pct: None,
            seven_day_resets_at: None,
            snapshot_ts: None,
            source: STATUS_SOURCE,
        });
    };

    // 2. 스냅샷 시각 해석
    let taken_at = DateTime::parse_from_rfc3339(&snapshot.ts)
        .map_err(|error| format!("스냅샷 시각 해석 실패 ({}): {error}", snapshot.ts))?
        .with_timezone(&Utc);

    // 3. 창별 유효 값 (유한한 값이고 resets_at 이 지나지 않은 창만)
    let live_pct = |pct: Option<f64>, resets_at: Option<i64>| {
        let expired = resets_at.is_some_and(|reset| reset <= now.timestamp());
        pct.filter(|value| value.is_finite() && !expired)
    };
    let five_hour_pct = live_pct(snapshot.five_hour_pct, snapshot.five_hour_resets_at);
    let seven_day_pct = live_pct(snapshot.seven_day_pct, snapshot.seven_day_resets_at);

    // 4. 수준 (오래된 스냅샷과 유효한 창 없음은 unknown, 미래 시각은 최신으로 봄)
    let max_age_cap = i64::try_from(max_age_minutes)
        .unwrap_or(MAX_AGE_CAP_MINUTES)
        .min(MAX_AGE_CAP_MINUTES);
    let max_age = Duration::minutes(max_age_cap);
    let stale = now - taken_at > max_age;
    let peak = five_hour_pct
        .into_iter()
        .chain(seven_day_pct)
        .fold(None, |peak: Option<f64>, value| {
            Some(peak.map_or(value, |current| current.max(value)))
        });
    let level = match peak {
        _ if stale => StatusLevel::Unknown,
        None => StatusLevel::Unknown,
        Some(value) if value >= 100.0 => StatusLevel::Limit,
        Some(value) if value >= warn_pct => StatusLevel::Warn,
        Some(_) => StatusLevel::Ok,
    };
    Ok(StatusReport {
        level,
        five_hour_pct,
        five_hour_resets_at: snapshot.five_hour_resets_at,
        seven_day_pct,
        seven_day_resets_at: snapshot.seven_day_resets_at,
        snapshot_ts: Some(snapshot.ts.clone()),
        source: STATUS_SOURCE,
    })
}

// 9. DB 조회와 판정 -----------------------------------------------------------
// DB 파일이 없으면 만들지 않고 오류 (읽기 전용 연결만 사용)
// 한도 스냅샷 표가 아직 없는 DB(이전 버전 serve)는 스냅샷 없음과 같이 unknown
fn load_status_report(
    db_path: &Path,
    options: &StatusOnceOptions,
    now: DateTime<Utc>,
) -> anyhow::Result<StatusReport> {
    if !db_path.is_file() {
        anyhow::bail!("DB 파일 없음: {}", db_path.display());
    }
    let store = Store::open_read_only(db_path)?;
    let has_table: bool = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'limit_snapshots')",
                [],
                |row| row.get(0),
            )
        })
        .context("DB 표 확인 실패")?;
    let snapshot = if has_table {
        store
            .latest_limit_snapshot()
            .context("한도 스냅샷 조회 실패")?
    } else {
        None
    };
    evaluate_snapshot(
        snapshot.as_ref(),
        now,
        options.warn_pct,
        options.max_age_minutes,
    )
    .map_err(anyhow::Error::msg)
}

// 10. 상태 파일 원자적 쓰기 ---------------------------------------------------
// 같은 폴더의 임시 파일에 쓰고 디스크까지 밀어 낸 뒤 이름을 바꿔 교체 (읽는 쪽은 옛 내용이나 새 내용만 봄), 실패하면 임시 파일 삭제
pub fn write_state_atomic(path: &Path, text: &str) -> io::Result<()> {
    let Some(file_name) = path.file_name() else {
        return Err(io::Error::new(ErrorKind::InvalidInput, "no file name"));
    };
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let temp_path = dir.join(format!(
        "{}.{}{TEMP_SUFFIX}",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    let written = fs::File::create(&temp_path).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    if let Err(error) = written {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}

// 11. 판정 실행 (출력 제외) ---------------------------------------------------
// 오류는 level=error 보고서와 종료 코드 30, 상태 파일은 오류 수준도 기록 (위젯이 오래된 값을 믿지 않도록), 파일 쓰기 실패는 종료 코드 30
pub fn execute_status_once(
    db_path: &Path,
    options: &StatusOnceOptions,
    now: DateTime<Utc>,
) -> StatusRun {
    // 1. 판정
    let (report, mut error) = match load_status_report(db_path, options, now) {
        Ok(report) => (report, None),
        Err(failure) => (error_report(), Some(format!("{failure:#}"))),
    };
    let mut exit_code = level_exit_code(report.level);

    // 2. 상태 파일 (선택)
    if let Some(state_file) = &options.state_file {
        let text = serde_json::to_string(&report).unwrap_or_default();
        if let Err(failure) = write_state_atomic(state_file, &format!("{text}\n")) {
            exit_code = EXIT_ERROR;
            error.get_or_insert_with(|| {
                format!("상태 파일 쓰기 실패 ({}): {failure}", state_file.display())
            });
        }
    }
    StatusRun {
        report,
        exit_code,
        error,
    }
}

// 12. 오류 보고서 -------------------------------------------------------------
pub fn error_report() -> StatusReport {
    StatusReport {
        level: StatusLevel::Error,
        five_hour_pct: None,
        five_hour_resets_at: None,
        seven_day_pct: None,
        seven_day_resets_at: None,
        snapshot_ts: None,
        source: STATUS_SOURCE,
    }
}

// 13. 명령 진입 ---------------------------------------------------------------
// stdout 에 JSON 한 줄, 오류 사유는 stderr, 종료 코드 반환
pub fn run_status_once(config: &AppConfig, options: &StatusOnceOptions) -> i32 {
    let run = execute_status_once(&config.data_dir.join(DB_FILE_NAME), options, Utc::now());
    println!("{}", serde_json::to_string(&run.report).unwrap_or_default());
    if let Some(message) = &run.error {
        eprintln!("오류: {message}");
    }
    run.exit_code
}

// 14. 설정 오류 진입 (설정을 읽지 못해 DB 위치를 모를 때) ---------------------
pub fn run_status_config_error(message: &str) -> i32 {
    println!(
        "{}",
        serde_json::to_string(&error_report()).unwrap_or_default()
    );
    eprintln!("설정 오류: {message}");
    EXIT_ERROR
}
