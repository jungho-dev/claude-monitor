//! transcript/test_support.rs
//! 시험 전용 임시 폴더, 합성 기록 복사, 시험용 설정 구성.

use crate::core::config::{
    AppConfig, GatewayConfig, HookConfig, IndexerConfig, PortConfig, PowerConfig, ReportConfig,
};
use crate::store::Store;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

// 임시 폴더 삭제 재시도 횟수와 간격 (Windows 파일 잠금 대기, 분리 재시도 포함)
const REMOVE_ATTEMPTS: u32 = 20;
const REMOVE_RETRY_MS: u64 = 50;
const DEFERRED_REMOVE_ATTEMPTS: u32 = 300;
const DEFERRED_RETRY_MS: u64 = 100;

// 1. 시험 작업 공간 -----------------------------------------------------------
// _cleanup 은 마지막 필드: store 가 닫힌 뒤 폴더 삭제
pub struct TestWorkspace {
    pub config: AppConfig,
    pub store: Store,
    pub root: PathBuf,
    pub projects_dir: PathBuf,
    pub _cleanup: TempDirGuard,
}

// 2. 고유 임시 폴더 -----------------------------------------------------------
pub fn unique_temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rap-test-{label}-{}-{nanos}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("임시 폴더 생성");
    dir
}

// 3. 폴더 재귀 복사 -----------------------------------------------------------
pub fn copy_dir_recursive(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("대상 폴더 생성");
    for entry in fs::read_dir(from).expect("원본 폴더 읽기").flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir_recursive(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("파일 복사");
        }
    }
}

// 4. 합성 기록 작업 공간 ------------------------------------------------------
// 합성 fixture 를 임시 projects 폴더로 복사하고 임시 DB 를 연 설정 반환
pub fn prepare_fixture_workspace(label: &str) -> TestWorkspace {
    let root = unique_temp_dir(label);
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("transcript")
        .join("projects");
    let projects_dir = root.join("projects");
    copy_dir_recursive(&fixture_root, &projects_dir);

    let config = AppConfig {
        data_dir: root.join("data"),
        claude_home: root.join("claude"),
        ports: PortConfig::default(),
        gateway: GatewayConfig::default(),
        hooks: HookConfig::default(),
        indexer: IndexerConfig {
            projects_dir: Some(projects_dir.clone()),
            ..IndexerConfig::default()
        },
        report: ReportConfig::default(),
        power: PowerConfig::default(),
        source_path: None,
    };
    let store = Store::open_store(&root.join("data").join("rap.sqlite")).expect("DB 열기");
    TestWorkspace {
        config,
        store,
        _cleanup: TempDirGuard(root.clone()),
        root,
        projects_dir,
    }
}

// 열린 파일이 남아 있으면 잠시 뒤 재시도, 그래도 실패하면 분리 스레드가 이어서 재시도 (호출 전 Store 등을 drop)
pub fn remove_temp_dir(dir: &Path) {
    // 1. 즉시 재시도 (런타임이 남긴 작업이 상위 함수에서 끝날 때까지)
    for _ in 0..REMOVE_ATTEMPTS {
        match fs::remove_dir_all(dir) {
            Ok(()) => return,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => std::thread::sleep(Duration::from_millis(REMOVE_RETRY_MS)),
        }
    }

    // 2. 분리 스레드 재시도 (시험 런타임 종료 뒤 락 해제 대기)
    let pending = dir.to_path_buf();
    std::thread::spawn(move || {
        for _ in 0..DEFERRED_REMOVE_ATTEMPTS {
            match fs::remove_dir_all(&pending) {
                Ok(()) => return,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) => std::thread::sleep(Duration::from_millis(DEFERRED_RETRY_MS)),
            }
        }
        eprintln!("[rap-test] 임시 폴더 삭제 실패 {}", pending.display());
    });
}

// 6. 임시 폴더 정리 보증 ------------------------------------------------------
// Drop 시 폴더 삭제 (이 값보다 뒤에 선언한 Store·컨텍스트가 먼저 닫히도록 맨 앞에 둔다)
pub struct TempDirGuard(pub PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        remove_temp_dir(&self.0);
    }
}
