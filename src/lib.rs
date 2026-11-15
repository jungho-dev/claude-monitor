//! lib.rs
//! Claude Code 로컬 허브 (OTLP 수집, 기록 색인, 훅 가속, API 게이트웨이, TUI) 라이브러리 루트.

// 1. 공개 모듈 ----------------------------------------------------------------
pub mod core;
pub mod hooks;
pub mod inventory;
pub mod net;
pub mod report;
pub mod serve;
pub mod store;
pub mod transcript;
pub mod tui;
