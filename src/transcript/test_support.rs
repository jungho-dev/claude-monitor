//! transcript/test_support.rs
//! 시험 전용 임시 폴더, 합성 기록 복사, 시험용 설정 구성.

use crate::core::config::{
    AppConfig, GatewayConfig, HookConfig, IndexerConfig, PortConfig, ReportConfig,
};
use crate::store::Store;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

// 1. 시험 작업 공간 -----------------------------------------------------------
pub struct TestWorkspace {
    pub config: AppConfig,
    pub store: Store,
    pub root: PathBuf,
    pub projects_dir: PathBuf,
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
        source_path: None,
    };
    let store = Store::open_store(&root.join("data").join("rap.sqlite")).expect("DB 열기");
    TestWorkspace {
        config,
        store,
        root,
        projects_dir,
    }
}
