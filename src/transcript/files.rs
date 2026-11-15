//! transcript/files.rs
//! projects 폴더의 최근 변경 jsonl 파일 탐색.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

// 1. 기록 파일 ----------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct TranscriptFile {
    pub path: PathBuf,
    pub project: String,
    pub size: i64,
    pub modified_ms: i64,
    pub is_subagent: bool,
}

// 2. 파일 탐색 ----------------------------------------------------------------
// cutoff_ms 이후 수정된 jsonl 만, 경로순 정렬
pub fn discover_transcript_files(root: &Path, cutoff_ms: i64) -> Vec<TranscriptFile> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];

    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };

            // 1. 하위 폴더
            if meta.is_dir() {
                pending.push(path);
                continue;
            }

            // 2. 대상 확장자·수정 시각
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|elapsed| elapsed.as_millis() as i64)
                .unwrap_or(0);
            if modified_ms < cutoff_ms {
                continue;
            }

            // 3. 프로젝트·하위 에이전트 판별
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let depth = relative.components().count();
            let project = match (depth > 1, relative.components().next()) {
                (true, Some(first)) => first.as_os_str().to_string_lossy().into_owned(),
                _ => String::new(),
            };
            let is_subagent = relative
                .components()
                .any(|part| part.as_os_str() == "subagents");
            found.push(TranscriptFile {
                path,
                project,
                size: meta.len() as i64,
                modified_ms,
                is_subagent,
            });
        }
    }

    found.sort_by(|left, right| left.path.cmp(&right.path));
    found
}
