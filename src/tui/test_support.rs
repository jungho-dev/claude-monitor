//! tui/test_support.rs
//! 테스트 전용 합성 설정·스냅샷 (환경 변수·파일 접근 없음).

use crate::core::config::{
    AppConfig, GatewayConfig, HookConfig, IndexerConfig, PortConfig, ReportConfig,
};
use crate::core::model::{
    ApiRequestRow, CounterSnapshot, HookEventRow, HookOutcome, LiveEvent, PortStatus,
    StatusSnapshot, ToolCallRow,
};
use crate::inventory::{AgentEntry, DoctorFinding, HookEntry, Inventory, McpEntry};
use crate::store::query::{
    AgentStatsRow, CacheBreakRow, HookStatRow, MinuteBucket, ModelStatsRow, OverviewStats,
    ToolLatencyRow,
};
use crate::tui::state::{AppState, DashboardSnapshot, DbState, Panel};
use chrono::{NaiveDate, Utc};
use std::path::PathBuf;

// 1. 합성 설정 ----------------------------------------------------------------
pub fn sample_config() -> AppConfig {
    AppConfig {
        data_dir: PathBuf::from("C:/rap-test-data"),
        claude_home: PathBuf::from("C:/rap-test-claude"),
        ports: PortConfig::default(),
        gateway: GatewayConfig::default(),
        hooks: HookConfig::default(),
        indexer: IndexerConfig::default(),
        report: ReportConfig::default(),
        source_path: None,
    }
}

// 2. 합성 화면 상태 -----------------------------------------------------------
pub fn sample_state_with_snapshot() -> AppState {
    let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
    let mut state = AppState::new(sample_config(), today);

    let status = StatusSnapshot {
        version: "0.1.0".to_string(),
        pid: 4242,
        started_at: "2026-11-15T00:00:00Z".to_string(),
        uptime_secs: 3_900,
        data_dir: "C:/rap-test-data".to_string(),
        claude_home: "C:/rap-test-claude".to_string(),
        ports: vec![
            PortStatus {
                name: "admin".to_string(),
                port: 47330,
                enabled: true,
                listening: true,
            },
            PortStatus {
                name: "otlp".to_string(),
                port: 47331,
                enabled: true,
                listening: true,
            },
            PortStatus {
                name: "gateway".to_string(),
                port: 47332,
                enabled: true,
                listening: true,
            },
            PortStatus {
                name: "hooks".to_string(),
                port: 47333,
                enabled: true,
                listening: false,
            },
        ],
        counters: CounterSnapshot::default(),
    };
    let overview = OverviewStats {
        sessions: 3,
        api_requests: 1_234,
        input_tokens: 2_300_000,
        output_tokens: 410_000,
        cache_read_tokens: 9_000_000,
        cache_creation_tokens: 700_000,
        cache_hit_ratio: 0.875,
        model_wait_p50_ms: Some(1_200),
        model_wait_p90_ms: Some(3_400),
        tool_calls: 880,
        tool_errors: 12,
        hook_denials: 3,
        hook_rewrites: 45,
        saved_ms: 189_000,
        lost_ms_estimate: 12_600,
        ..OverviewStats::default()
    };
    let minute = Utc::now().format("%Y-%m-%dT%H:%M").to_string();
    let events = vec![
        LiveEvent::Api(ApiRequestRow {
            ts: "2026-11-15T01:00:03Z".to_string(),
            model: "claude-sonnet-5-5".to_string(),
            agent: "main".to_string(),
            input_tokens: 1_200,
            output_tokens: 340,
            duration_ms: Some(2_100),
            ..ApiRequestRow::default()
        }),
        LiveEvent::Tool(ToolCallRow {
            ts: "2026-11-15T01:00:02Z".to_string(),
            tool_name: "Bash".to_string(),
            duration_ms: Some(85),
            ..ToolCallRow::default()
        }),
        LiveEvent::Hook(HookEventRow {
            ts: "2026-11-15T01:00:01Z".to_string(),
            route: "pretool".to_string(),
            guard: "cliguard".to_string(),
            outcome: HookOutcome::Rewrite,
            detail: "grep -> rg".to_string(),
            ..HookEventRow::default()
        }),
        LiveEvent::Notice {
            ts: "2026-11-15T01:00:00Z".to_string(),
            level: "info".to_string(),
            text: "색인 완료".to_string(),
        },
    ];

    state.snapshot = DashboardSnapshot {
        taken_at: Some(Utc::now()),
        db_state: DbState::Open,
        status: Panel::Ready(status),
        overview: Panel::Ready(overview.clone()),
        today: Panel::Ready(overview),
        minutes: Panel::Ready(vec![MinuteBucket {
            minute,
            api_requests: 7,
            output_tokens: 1_500,
            tool_calls: 4,
            hook_denials: 0,
        }]),
        events: Panel::Ready(events),
        ..DashboardSnapshot::default()
    };
    state
}

// 3. 표 탭용 합성 집계 --------------------------------------------------------
pub fn sample_state_with_tables() -> AppState {
    let mut state = sample_state_with_snapshot();
    state.snapshot.tools = Panel::Ready(vec![
        ToolLatencyRow {
            tool_name: "Bash".to_string(),
            calls: 420,
            errors: 9,
            p50_ms: Some(85),
            p90_ms: Some(1_900),
            total_ms: 96_000,
        },
        ToolLatencyRow {
            tool_name: "Read".to_string(),
            calls: 1_300,
            errors: 0,
            p50_ms: Some(12),
            p90_ms: Some(40),
            total_ms: 21_000,
        },
        ToolLatencyRow {
            tool_name: "Edit".to_string(),
            calls: 80,
            errors: 1,
            p50_ms: None,
            p90_ms: None,
            total_ms: 4_000,
        },
    ]);
    state.snapshot.hooks = Panel::Ready(vec![
        HookStatRow {
            route: "pretool".to_string(),
            guard: "cliguard".to_string(),
            outcome: "rewrite".to_string(),
            detail: "grep -> rg".to_string(),
            count: 45,
            p50_ms: Some(3),
            p90_ms: Some(9),
            saved_ms: 189_000,
        },
        HookStatRow {
            route: "pretool".to_string(),
            guard: "remoteguard".to_string(),
            outcome: "deny".to_string(),
            detail: "git push".to_string(),
            count: 3,
            p50_ms: Some(4),
            p90_ms: Some(6),
            saved_ms: 0,
        },
    ]);
    state.snapshot.model_stats = Panel::Ready(vec![
        ModelStatsRow {
            model: "claude-sonnet-5-5".to_string(),
            requests: 900,
            sessions: 3,
            agent_types: 2,
            input_tokens: 1_500_000,
            output_tokens: 300_000,
            cache_read_tokens: 8_000_000,
            cache_creation_tokens: 500_000,
            cache_hit_ratio: 0.842,
            wait_p50_ms: Some(1_200),
            wait_p90_ms: Some(3_400),
            cost_usd_estimate: 12.345,
            last_ts: "2026-11-15T01:00:00Z".to_string(),
        },
        ModelStatsRow {
            model: "claude-haiku-4-5".to_string(),
            requests: 1_500,
            sessions: 5,
            agent_types: 4,
            input_tokens: 200_000,
            output_tokens: 90_000,
            cache_read_tokens: 1_000_000,
            cache_creation_tokens: 80_000,
            cache_hit_ratio: 0.9,
            wait_p50_ms: Some(300),
            wait_p90_ms: None,
            cost_usd_estimate: 0.75,
            last_ts: "2026-11-15T01:30:00Z".to_string(),
        },
    ]);
    state.snapshot.agent_stats = Panel::Ready(vec![
        AgentStatsRow {
            agent: "main".to_string(),
            runs: 3,
            requests: 700,
            sessions: 3,
            input_tokens: 900_000,
            output_tokens: 200_000,
            cache_read_tokens: 6_000_000,
            cache_creation_tokens: 300_000,
            cache_hit_ratio: 0.85,
            wait_p50_ms: Some(1_100),
            wait_p90_ms: Some(3_000),
            cost_usd_estimate: 9.5,
            tool_calls: 500,
            tool_errors: 4,
            top_model: "claude-sonnet-5-5".to_string(),
            last_ts: "2026-11-15T01:00:00Z".to_string(),
        },
        AgentStatsRow {
            agent: "worker".to_string(),
            runs: 12,
            requests: 1_700,
            sessions: 4,
            input_tokens: 800_000,
            output_tokens: 190_000,
            cache_read_tokens: 3_000_000,
            cache_creation_tokens: 280_000,
            cache_hit_ratio: 0.8,
            wait_p50_ms: Some(400),
            wait_p90_ms: None,
            cost_usd_estimate: 3.6,
            tool_calls: 380,
            tool_errors: 0,
            top_model: "claude-haiku-4-5".to_string(),
            last_ts: "2026-11-15T01:30:00Z".to_string(),
        },
    ]);
    state.inventory = Panel::Ready(Inventory {
        hooks: vec![
            HookEntry {
                event: "PreToolUse".to_string(),
                matcher: "Bash".to_string(),
                hook_type: "http".to_string(),
                target: "http://127.0.0.1:47333/pretool".to_string(),
                timeout_secs: Some(5),
                ..HookEntry::default()
            },
            HookEntry {
                event: "Notification".to_string(),
                hook_type: "command".to_string(),
                target: "notify.exe".to_string(),
                timeout_secs: Some(30),
                ..HookEntry::default()
            },
        ],
        mcp_servers: vec![
            McpEntry {
                name: "rust-fs-mcp".to_string(),
                transport: "stdio".to_string(),
                target: "rust-fs-mcp.exe".to_string(),
                denied: false,
            },
            McpEntry {
                name: "outline".to_string(),
                transport: "http".to_string(),
                target: "https://example.invalid".to_string(),
                denied: true,
            },
        ],
        agents: vec![
            AgentEntry {
                name: "worker".to_string(),
                model: "sonnet".to_string(),
                effort: "high".to_string(),
                max_turns: Some(40),
                ..AgentEntry::default()
            },
            AgentEntry {
                name: "tester".to_string(),
                model: "haiku".to_string(),
                effort: "medium".to_string(),
                ..AgentEntry::default()
            },
        ],
        env_keys: vec!["DEBUG".to_string()],
        ..Inventory::default()
    });
    state.doctor = Panel::Ready(vec![
        DoctorFinding {
            level: "ok".to_string(),
            topic: "토큰".to_string(),
            message: "정상".to_string(),
            fix_hint: String::new(),
        },
        DoctorFinding {
            level: "fail".to_string(),
            topic: "포트".to_string(),
            message: "47333 사용 중".to_string(),
            fix_hint: "다른 포트로 변경".to_string(),
        },
    ]);
    state.snapshot.cache_breaks = Panel::Ready(vec![CacheBreakRow {
        ts: "2026-11-15T01:00:00Z".to_string(),
        session_id: "s1".to_string(),
        agent: "main".to_string(),
        model: "claude-sonnet-5-5".to_string(),
        cache_creation_tokens: 120_000,
        previous_context_tokens: 118_000,
        gap_secs: 400,
        likely_cause: "TTL 만료".to_string(),
    }]);
    state.report = Panel::Ready(Some(
        "# 일일 리포트 2026-11-14\n\n## 요약\n- 요청 1,234건\n- 훅 재작성 45건\n\n| 모델 | 요청 |\n|---|---|\n| sonnet | 900 |\n"
            .to_string(),
    ));
    state
}

// 4. 스크롤바용 긴 목록 -------------------------------------------------------
// 도구 탭에 도구 count 종, 개요 탭에 이벤트 count 건, 리포트 탭에 제목 count 개
pub fn sample_state_with_many_tools(count: usize) -> AppState {
    let mut state = sample_state_with_tables();
    state.tab = crate::tui::state::ActiveTab::Tools;
    state.snapshot.tools = Panel::Ready(
        (0..count)
            .map(|index| ToolLatencyRow {
                tool_name: format!("Tool{index:03}"),
                calls: index as u64 + 1,
                total_ms: 100,
                ..ToolLatencyRow::default()
            })
            .collect(),
    );
    state
}

pub fn sample_state_with_many_events(count: usize) -> AppState {
    let mut state = sample_state_with_snapshot();
    state.snapshot.events = Panel::Ready(
        (0..count)
            .rev()
            .map(|index| LiveEvent::Notice {
                ts: format!("2026-11-15T01:{:02}:{:02}Z", index / 60, index % 60),
                level: "info".to_string(),
                text: format!("알림 {index}"),
            })
            .collect(),
    );
    state
}

pub fn sample_state_with_long_report(count: usize) -> AppState {
    let mut state = sample_state_with_tables();
    state.tab = crate::tui::state::ActiveTab::Report;
    let mut markdown = String::from("# 일일 리포트\n");
    for index in 0..count {
        markdown.push_str(&format!("\n## 항목 {index}\n본문\n"));
    }
    state.report = Panel::Ready(Some(markdown));
    state
}
