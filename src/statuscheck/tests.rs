//! statuscheck/tests.rs
//! status --once 판정·종료 코드·상태 파일 테스트.

use super::{
    EXIT_ERROR, EXIT_LIMIT, EXIT_OK, EXIT_UNKNOWN, EXIT_WARN, StatusLevel, StatusOnceOptions,
    evaluate_snapshot, execute_status_once, parse_warn_pct, write_state_atomic,
};
use crate::core::config::DB_FILE_NAME;
use crate::store::Store;
use crate::store::limits::LimitSnapshot;
use crate::transcript::test_support::unique_temp_dir;
use chrono::{DateTime, Duration, SecondsFormat, TimeZone, Utc};
use std::fs;
use std::path::{Path, PathBuf};

fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap()
}

fn options_with(state_file: Option<PathBuf>) -> StatusOnceOptions {
    StatusOnceOptions {
        warn_pct: 80.0,
        max_age_minutes: 360,
        state_file,
    }
}

// 기준 시각에서 minutes_ago 분 전에 찍힌 스냅샷 (창 resets_at 은 기준 시각 + 1 시간)
fn snapshot_at(minutes_ago: i64, five_hour: Option<f64>, seven_day: Option<f64>) -> LimitSnapshot {
    let now = fixed_now();
    LimitSnapshot {
        ts: (now - Duration::minutes(minutes_ago)).to_rfc3339_opts(SecondsFormat::Millis, true),
        session_id: "sess-1".to_string(),
        five_hour_pct: five_hour,
        five_hour_resets_at: five_hour.map(|_| now.timestamp() + 3_600),
        seven_day_pct: seven_day,
        seven_day_resets_at: seven_day.map(|_| now.timestamp() + 86_400),
        ..LimitSnapshot::default()
    }
}

// 스냅샷을 담은 임시 DB 경로 (폴더는 테스트 끝에 직접 지움)
fn db_with(label: &str, snapshots: &[LimitSnapshot]) -> (PathBuf, PathBuf) {
    let dir = unique_temp_dir(label);
    let db_path = dir.join(DB_FILE_NAME);
    let store = Store::open_store(&db_path).expect("저장소 열기");
    for snapshot in snapshots {
        store.insert_limit_snapshot(snapshot).expect("기록");
    }
    drop(store);
    (dir, db_path)
}

fn run_on(snapshots: &[LimitSnapshot], label: &str) -> (i32, StatusLevel) {
    let (dir, db_path) = db_with(label, snapshots);
    let run = execute_status_once(&db_path, &options_with(None), fixed_now());
    assert!(run.error.is_none(), "{:?}", run.error);
    fs::remove_dir_all(&dir).ok();
    (run.exit_code, run.report.level)
}

// 1. 종료 코드 수준 -----------------------------------------------------------
#[test]
fn each_level_maps_to_its_exit_code() {
    // 1. 정상
    let ok = run_on(&[snapshot_at(1, Some(50.0), Some(10.0))], "sc-ok");
    assert_eq!(ok, (EXIT_OK, StatusLevel::Ok));

    // 2. 경고 기준 직전과 기준 값
    let below = run_on(&[snapshot_at(1, Some(79.9), None)], "sc-below");
    assert_eq!(below, (EXIT_OK, StatusLevel::Ok));
    let warn = run_on(&[snapshot_at(1, Some(80.0), None)], "sc-warn");
    assert_eq!(warn, (EXIT_WARN, StatusLevel::Warn));

    // 3. 100% 직전과 100%
    let almost = run_on(&[snapshot_at(1, Some(99.9), None)], "sc-almost");
    assert_eq!(almost, (EXIT_WARN, StatusLevel::Warn));
    let limit = run_on(&[snapshot_at(1, Some(100.0), None)], "sc-limit");
    assert_eq!(limit, (EXIT_LIMIT, StatusLevel::Limit));

    // 4. 두 창 중 높은 값
    let higher = run_on(&[snapshot_at(1, Some(10.0), Some(95.0))], "sc-higher");
    assert_eq!(higher, (EXIT_WARN, StatusLevel::Warn));
    let seven_limit = run_on(&[snapshot_at(1, Some(85.0), Some(100.0))], "sc-seven");
    assert_eq!(seven_limit, (EXIT_LIMIT, StatusLevel::Limit));

    // 5. 한 창만 있어도 판정
    let only_seven = run_on(&[snapshot_at(1, None, Some(81.0))], "sc-only7");
    assert_eq!(only_seven, (EXIT_WARN, StatusLevel::Warn));
}

#[test]
fn the_warn_threshold_option_moves_the_boundary() {
    let (dir, db_path) = db_with("sc-warn-opt", &[snapshot_at(1, Some(60.0), None)]);
    let strict = StatusOnceOptions {
        warn_pct: 60.0,
        ..options_with(None)
    };
    let run = execute_status_once(&db_path, &strict, fixed_now());
    assert_eq!(run.exit_code, EXIT_WARN);
    let lenient = StatusOnceOptions {
        warn_pct: 90.0,
        ..options_with(None)
    };
    let run = execute_status_once(&db_path, &lenient, fixed_now());
    assert_eq!(run.exit_code, EXIT_OK);
    fs::remove_dir_all(&dir).ok();
}

// 2. 만료된 창 무시 -----------------------------------------------------------
#[test]
fn an_expired_window_is_ignored_and_reported_as_null() {
    // 1. 5시간 창이 99% 였지만 resets_at 이 지남 -> 주간 값으로 판정
    let now = fixed_now();
    let mut expired = snapshot_at(1, Some(99.0), Some(20.0));
    expired.five_hour_resets_at = Some(now.timestamp() - 10);
    let (dir, db_path) = db_with("sc-expired", &[expired]);
    let run = execute_status_once(&db_path, &options_with(None), now);
    assert_eq!(run.exit_code, EXIT_OK);
    assert_eq!(run.report.five_hour_pct, None);
    assert_eq!(run.report.five_hour_resets_at, Some(now.timestamp() - 10));
    assert_eq!(run.report.seven_day_pct, Some(20.0));
    fs::remove_dir_all(&dir).ok();

    // 2. resets_at 이 정확히 지금이어도 만료
    let mut edge = snapshot_at(1, Some(99.0), None);
    edge.five_hour_resets_at = Some(now.timestamp());
    let (dir, db_path) = db_with("sc-edge", &[edge]);
    let run = execute_status_once(&db_path, &options_with(None), now);
    assert_eq!(run.exit_code, EXIT_UNKNOWN);
    fs::remove_dir_all(&dir).ok();

    // 3. 두 창 모두 만료면 유효한 값이 없어 unknown
    let mut both = snapshot_at(1, Some(99.0), Some(99.0));
    both.five_hour_resets_at = Some(now.timestamp() - 1);
    both.seven_day_resets_at = Some(now.timestamp() - 1);
    let (dir, db_path) = db_with("sc-both", &[both]);
    let run = execute_status_once(&db_path, &options_with(None), now);
    assert_eq!(run.exit_code, EXIT_UNKNOWN);
    assert_eq!(run.report.level, StatusLevel::Unknown);
    fs::remove_dir_all(&dir).ok();

    // 4. resets_at 없는 창은 값 그대로 판정
    let mut no_reset = snapshot_at(1, Some(90.0), None);
    no_reset.five_hour_resets_at = None;
    let (dir, db_path) = db_with("sc-noreset", &[no_reset]);
    let run = execute_status_once(&db_path, &options_with(None), now);
    assert_eq!(run.exit_code, EXIT_WARN);
    fs::remove_dir_all(&dir).ok();
}

// 3. 오래된 스냅샷과 스냅샷 없음 ----------------------------------------------
#[test]
fn stale_or_missing_snapshots_are_unknown() {
    // 1. 360분 경계: 360분은 유효, 361분은 오래됨 (값은 그대로 출력)
    let fresh = run_on(&[snapshot_at(360, Some(90.0), None)], "sc-age-ok");
    assert_eq!(fresh, (EXIT_WARN, StatusLevel::Warn));
    let (dir, db_path) = db_with("sc-stale", &[snapshot_at(361, Some(90.0), None)]);
    let run = execute_status_once(&db_path, &options_with(None), fixed_now());
    assert_eq!(run.exit_code, EXIT_UNKNOWN);
    assert_eq!(run.report.five_hour_pct, Some(90.0));
    assert!(run.report.snapshot_ts.is_some());

    // 2. --max-age 로 기준 변경
    let tight = StatusOnceOptions {
        max_age_minutes: 5,
        ..options_with(None)
    };
    let run = execute_status_once(&db_path, &tight, fixed_now());
    assert_eq!(run.exit_code, EXIT_UNKNOWN);
    let wide = StatusOnceOptions {
        max_age_minutes: 1_000,
        ..options_with(None)
    };
    let run = execute_status_once(&db_path, &wide, fixed_now());
    assert_eq!(run.exit_code, EXIT_WARN);
    fs::remove_dir_all(&dir).ok();

    // 3. 스냅샷 없음 (DB 는 있음)
    let empty = run_on(&[], "sc-empty");
    assert_eq!(empty, (EXIT_UNKNOWN, StatusLevel::Unknown));

    // 4. 한도 값이 없는 행만 있어도 스냅샷 없음과 같음
    let without = run_on(&[snapshot_at(1, None, None)], "sc-novalues");
    assert_eq!(without, (EXIT_UNKNOWN, StatusLevel::Unknown));

    // 5. 미래 시각(시계 오차)은 최신으로 봄
    let future = run_on(&[snapshot_at(-30, Some(10.0), None)], "sc-future");
    assert_eq!(future, (EXIT_OK, StatusLevel::Ok));
}

#[test]
fn the_newest_snapshot_wins_across_sessions() {
    let mut older = snapshot_at(30, Some(95.0), None);
    older.session_id = "sess-old".to_string();
    let newer = snapshot_at(2, Some(10.0), None);
    let latest = run_on(&[older, newer], "sc-newest");
    assert_eq!(latest, (EXIT_OK, StatusLevel::Ok));
}

#[test]
fn a_db_from_before_the_limit_table_exits_20_instead_of_30() {
    // 이전 버전이 만든 DB 흉내 (한도 스냅샷 표만 없음)
    let (dir, db_path) = db_with("sc-oldschema", &[]);
    let store = Store::open_store(&db_path).expect("저장소 열기");
    store
        .with_conn(|conn| conn.execute_batch("DROP TABLE limit_snapshots"))
        .expect("표 삭제");
    drop(store);
    let run = execute_status_once(&db_path, &options_with(None), fixed_now());
    assert!(run.error.is_none(), "{:?}", run.error);
    assert_eq!(run.exit_code, EXIT_UNKNOWN);
    assert_eq!(run.report.level, StatusLevel::Unknown);
    fs::remove_dir_all(&dir).ok();
}

// 4. 오류 -> 30 ---------------------------------------------------------------
#[test]
fn a_missing_db_exits_30_without_creating_the_db_or_its_folder() {
    let dir = unique_temp_dir("sc-nodb");
    let db_path = dir.join("nested").join(DB_FILE_NAME);
    let run = execute_status_once(&db_path, &options_with(None), fixed_now());
    assert_eq!(run.exit_code, EXIT_ERROR);
    assert_eq!(run.report.level, StatusLevel::Error);
    assert!(run.error.as_deref().is_some_and(|text| text.contains("DB")));
    assert!(!db_path.exists());
    assert!(!dir.join("nested").exists());
    assert!(!dir.join(format!("{DB_FILE_NAME}-wal")).exists());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_bad_snapshot_time_or_a_non_database_file_exits_30() {
    // 1. 해석할 수 없는 스냅샷 시각
    let mut broken = snapshot_at(1, Some(10.0), None);
    broken.ts = "어제 오후".to_string();
    let (dir, db_path) = db_with("sc-badts", &[broken]);
    let run = execute_status_once(&db_path, &options_with(None), fixed_now());
    assert_eq!(run.exit_code, EXIT_ERROR);
    assert_eq!(run.report.level, StatusLevel::Error);
    fs::remove_dir_all(&dir).ok();

    // 2. DB 가 아닌 파일
    let dir = unique_temp_dir("sc-garbage");
    let db_path = dir.join(DB_FILE_NAME);
    fs::write(&db_path, "이건 sqlite 가 아님 ".repeat(100)).unwrap();
    let run = execute_status_once(&db_path, &options_with(None), fixed_now());
    assert_eq!(run.exit_code, EXIT_ERROR);
    assert!(run.error.is_some());
    fs::remove_dir_all(&dir).ok();
}

// 5. 출력 JSON ----------------------------------------------------------------
#[test]
fn the_report_serializes_the_documented_fields_in_order() {
    let report = evaluate_snapshot(
        Some(&snapshot_at(1, Some(85.5), Some(12.0))),
        fixed_now(),
        80.0,
        360,
    )
    .unwrap();
    let text = serde_json::to_string(&report).unwrap();
    assert!(!text.contains('\n'));
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    // 키 순서는 직렬화 문자열 기준 (Value 는 키를 정렬함)
    let names = [
        "level",
        "five_hour_pct",
        "five_hour_resets_at",
        "seven_day_pct",
        "seven_day_resets_at",
        "snapshot_ts",
        "source",
    ];
    let positions: Vec<usize> = names
        .iter()
        .map(|name| text.find(&format!("\"{name}\":")).expect(name))
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    assert_eq!(value.as_object().unwrap().len(), names.len());
    assert_eq!(value["level"], "warn");
    assert_eq!(value["source"], "statusline");
    assert_eq!(value["five_hour_pct"], 85.5);
    assert_eq!(value["snapshot_ts"], "2026-11-19T11:59:00.000Z");

    // 값 없는 보고서는 null 로 같은 키를 유지
    let unknown = evaluate_snapshot(None, fixed_now(), 80.0, 360).unwrap();
    let value: serde_json::Value = serde_json::to_value(&unknown).unwrap();
    assert_eq!(value["level"], "unknown");
    assert!(value["five_hour_pct"].is_null() && value["snapshot_ts"].is_null());
}

// 6. 상태 파일 ----------------------------------------------------------------
fn file_names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn the_state_file_holds_the_printed_json_and_is_replaced_atomically() {
    let (dir, db_path) = db_with("sc-state", &[snapshot_at(1, Some(85.0), Some(10.0))]);
    let state_file = dir.join("out").join("status.json");
    let options = options_with(Some(state_file.clone()));

    // 1. 폴더가 없어도 만들어 씀, 내용은 출력 JSON 한 줄
    let run = execute_status_once(&db_path, &options, fixed_now());
    assert_eq!(run.exit_code, EXIT_WARN);
    let text = fs::read_to_string(&state_file).unwrap();
    assert_eq!(
        text,
        format!("{}\n", serde_json::to_string(&run.report).unwrap())
    );
    assert_eq!(file_names_in(&dir.join("out")), ["status.json".to_string()]);

    // 2. 다시 실행하면 같은 파일을 교체하고 임시 파일을 남기지 않음
    let later = execute_status_once(&db_path, &options, fixed_now() + Duration::minutes(400));
    assert_eq!(later.exit_code, EXIT_UNKNOWN);
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_file).unwrap()).unwrap();
    assert_eq!(value["level"], "unknown");
    assert_eq!(file_names_in(&dir.join("out")), ["status.json".to_string()]);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_error_run_still_writes_an_error_state_and_a_failed_write_exits_30() {
    // 1. DB 없음이어도 상태 파일에 level=error 기록
    let dir = unique_temp_dir("sc-state-err");
    let state_file = dir.join("status.json");
    let options = options_with(Some(state_file.clone()));
    let run = execute_status_once(&dir.join(DB_FILE_NAME), &options, fixed_now());
    assert_eq!(run.exit_code, EXIT_ERROR);
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_file).unwrap()).unwrap();
    assert_eq!(value["level"], "error");
    assert!(!dir.join(DB_FILE_NAME).exists());
    fs::remove_dir_all(&dir).ok();

    // 2. 상태 파일 자리를 폴더가 차지하면 쓰기 실패: 종료 코드 30, 판정 보고서는 유지, 임시 파일 없음
    let (dir, db_path) = db_with("sc-state-fail", &[snapshot_at(1, Some(50.0), None)]);
    let blocked = dir.join("blocked.json");
    fs::create_dir_all(&blocked).unwrap();
    let run = execute_status_once(&db_path, &options_with(Some(blocked)), fixed_now());
    assert_eq!(run.exit_code, EXIT_ERROR);
    assert_eq!(run.report.level, StatusLevel::Ok);
    assert!(
        run.error
            .as_deref()
            .is_some_and(|text| text.contains("상태 파일"))
    );
    let leftovers: Vec<String> = file_names_in(&dir)
        .into_iter()
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn atomic_write_replaces_content_and_rejects_a_path_without_a_file_name() {
    let dir = unique_temp_dir("sc-atomic");
    let path = dir.join("status.json");
    write_state_atomic(&path, "첫째\n").unwrap();
    write_state_atomic(&path, "둘째\n").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "둘째\n");
    assert_eq!(file_names_in(&dir), ["status.json".to_string()]);
    assert!(write_state_atomic(Path::new(""), "x").is_err());
    fs::remove_dir_all(&dir).ok();
}

// 7. 인자 해석 ----------------------------------------------------------------
#[test]
fn warn_pct_accepts_only_positive_values_up_to_100() {
    assert_eq!(parse_warn_pct("80"), Ok(80.0));
    assert_eq!(parse_warn_pct(" 92.5 "), Ok(92.5));
    assert_eq!(parse_warn_pct("100"), Ok(100.0));
    for bad in ["0", "-5", "100.1", "abc", "", "NaN", "inf"] {
        assert!(parse_warn_pct(bad).is_err(), "{bad}");
    }
}
