//! tui/test_support.rs
//! 테스트 전용 합성 설정·스냅샷 (환경 변수·파일 접근 없음).

use crate::core::config::{
    AppConfig, GatewayConfig, HookConfig, IndexerConfig, PortConfig, PowerConfig, ReportConfig,
};
use crate::core::model::{
    ApiRequestRow, CounterSnapshot, HookEventRow, HookOutcome, LiveEvent, PortStatus,
    StatusSnapshot, ToolCallRow,
};
use crate::inventory::{AgentEntry, DoctorFinding, HookEntry, Inventory, McpEntry, SkillEntry};
use crate::store::limits::LimitSnapshot;
use crate::store::metrics::{AttributionDimension, MetricAttributionRow};
use crate::store::query::{
    AgentRunSummary, AgentStatsRow, CacheBreakRow, HookStatRow, MinuteBucket, ModelStatsRow,
    OverviewStats, ProjectStatsRow, SkillStatsRow, ToolLatencyRow, UsageShare,
};
use crate::tui::i18n::Lang;
use crate::tui::state::{AppState, DashboardSnapshot, DbState, LimitsData, Panel};
use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, TimeZone, Utc};
use std::path::PathBuf;

// 1. 합성 설정 ----------------------------------------------------------------
pub fn sample_config() -> AppConfig {
    AppConfig {
        data_dir: PathBuf::from("C:/cm-test-data"),
        claude_home: PathBuf::from("C:/cm-test-claude"),
        ports: PortConfig::default(),
        gateway: GatewayConfig::default(),
        hooks: HookConfig::default(),
        indexer: IndexerConfig::default(),
        report: ReportConfig::default(),
        power: PowerConfig::default(),
        source_path: None,
    }
}

// 2. 합성 화면 상태 -----------------------------------------------------------
// 기존 한국어 문구 단언을 위해 한국어로 고정 (영어는 테스트가 lang 를 바꾸어 검사)
pub fn sample_state_with_snapshot() -> AppState {
    let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
    let mut state = AppState::new(sample_config(), today);
    state.lang = Lang::Ko;

    let status = StatusSnapshot {
        version: "0.1.0".to_string(),
        build_id: "1-1".to_string(),
        pid: 4242,
        started_at: "2026-11-15T00:00:00Z".to_string(),
        uptime_secs: 3_900,
        data_dir: "C:/cm-test-data".to_string(),
        claude_home: "C:/cm-test-claude".to_string(),
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
            avg_ms: Some(85),
            max_ms: Some(1_900),
        },
        ToolLatencyRow {
            tool_name: "Read".to_string(),
            calls: 1_300,
            errors: 0,
            p50_ms: Some(12),
            p90_ms: Some(40),
            total_ms: 21_000,
            avg_ms: Some(12),
            max_ms: Some(40),
        },
        ToolLatencyRow {
            tool_name: "Edit".to_string(),
            calls: 80,
            errors: 1,
            p50_ms: None,
            p90_ms: None,
            total_ms: 4_000,
            avg_ms: None,
            max_ms: None,
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
            avg_ms: Some(3),
            max_ms: Some(9),
            total_ms: 135,
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
            avg_ms: Some(4),
            max_ms: Some(6),
            total_ms: 12,
        },
    ]);
    state.snapshot.model_stats = Panel::Ready(vec![
        ModelStatsRow {
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
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
            wait_avg_ms: Some(1_200),
            wait_max_ms: Some(3_400),
            wait_total_ms: 1_080_000,
            cost_usd_estimate: 12.345,
            last_ts: "2026-11-15T01:00:00Z".to_string(),
            first_ts: "2026-11-14T22:00:00Z".to_string(),
            agents: vec![
                usage_share("main", 600, 0, 9.1, "2026-11-15T01:00:00Z"),
                usage_share("worker", 300, 0, 3.245, "2026-11-15T00:40:00Z"),
            ],
        },
        ModelStatsRow {
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
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
            wait_avg_ms: Some(300),
            wait_max_ms: None,
            wait_total_ms: 450_000,
            cost_usd_estimate: 0.75,
            last_ts: "2026-11-15T01:30:00Z".to_string(),
            first_ts: "2026-11-14T23:00:00Z".to_string(),
            agents: vec![usage_share(
                "worker",
                1_500,
                0,
                0.75,
                "2026-11-15T01:30:00Z",
            )],
        },
    ]);
    state.snapshot.agent_stats = Panel::Ready(vec![
        AgentStatsRow {
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
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
            wait_avg_ms: Some(1_100),
            wait_max_ms: Some(3_000),
            wait_total_ms: 770_000,
            cost_usd_estimate: 9.5,
            tool_calls: 500,
            tool_errors: 4,
            top_model: "claude-sonnet-5-5".to_string(),
            last_ts: "2026-11-15T01:00:00Z".to_string(),
            first_ts: "2026-11-14T22:00:00Z".to_string(),
            models: vec![usage_share(
                "claude-sonnet-5-5",
                700,
                0,
                9.5,
                "2026-11-15T01:00:00Z",
            )],
            top_tools: vec![
                usage_share("Bash", 300, 4, 0.0, "2026-11-15T01:00:00Z"),
                usage_share("Read", 200, 0, 0.0, "2026-11-15T00:59:00Z"),
            ],
            recent_runs: vec![AgentRunSummary {
                run: "sess-main-1".to_string(),
                model: "claude-sonnet-5-5".to_string(),
                requests: 240,
                output_tokens: 70_000,
                cost_usd_estimate: 3.2,
                first_ts: "2026-11-15T00:10:00Z".to_string(),
                last_ts: "2026-11-15T01:00:00Z".to_string(),
            }],
        },
        AgentStatsRow {
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
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
            wait_avg_ms: Some(400),
            wait_max_ms: None,
            wait_total_ms: 680_000,
            cost_usd_estimate: 3.6,
            tool_calls: 380,
            tool_errors: 0,
            top_model: "claude-haiku-4-5".to_string(),
            last_ts: "2026-11-15T01:30:00Z".to_string(),
            first_ts: "2026-11-14T23:00:00Z".to_string(),
            models: vec![
                usage_share("claude-sonnet-5-5", 200, 0, 2.85, "2026-11-15T00:40:00Z"),
                usage_share("claude-haiku-4-5", 1_500, 0, 0.75, "2026-11-15T01:30:00Z"),
            ],
            top_tools: vec![usage_share("Edit", 380, 0, 0.0, "2026-11-15T01:30:00Z")],
            recent_runs: vec![AgentRunSummary {
                run: "worker#a1b2c3d4".to_string(),
                model: "claude-haiku-4-5".to_string(),
                requests: 140,
                output_tokens: 15_000,
                cost_usd_estimate: 0.3,
                first_ts: "2026-11-15T01:20:00Z".to_string(),
                last_ts: "2026-11-15T01:30:00Z".to_string(),
            }],
        },
    ]);
    state.snapshot.projects = Panel::Ready(vec![
        ProjectStatsRow {
            project: "claude-monitor".to_string(),
            git_branch: "main".to_string(),
            requests: 900,
            sessions: 3,
            input_tokens: 1_200_000,
            output_tokens: 300_000,
            cache_read_tokens: 6_000_000,
            cache_creation_5m_tokens: 200_000,
            cache_creation_1h_tokens: 100_000,
            cost_usd_estimate: 8.4,
            wait_avg_ms: None,
            wait_max_ms: None,
            wait_total_ms: 0,
            last_ts: "2026-11-15T01:30:00Z".to_string(),
            first_ts: "2026-11-14T22:00:00Z".to_string(),
            models: vec![usage_share(
                "claude-sonnet-5-5",
                900,
                0,
                8.4,
                "2026-11-15T01:30:00Z",
            )],
            agents: vec![
                usage_share("main", 600, 0, 6.0, "2026-11-15T01:30:00Z"),
                usage_share("worker", 300, 0, 2.4, "2026-11-15T01:00:00Z"),
            ],
            top_sessions: vec![usage_share(
                "0f9e8d7c-1234-5678-9abc-def012345678",
                500,
                0,
                5.0,
                "2026-11-15T01:30:00Z",
            )],
        },
        ProjectStatsRow {
            project: "claude-monitor".to_string(),
            git_branch: "feature/projects-tab".to_string(),
            requests: 400,
            sessions: 2,
            input_tokens: 500_000,
            output_tokens: 120_000,
            cache_read_tokens: 2_500_000,
            cache_creation_5m_tokens: 90_000,
            cache_creation_1h_tokens: 0,
            cost_usd_estimate: 3.1,
            last_ts: "2026-11-15T01:10:00Z".to_string(),
            ..ProjectStatsRow::default()
        },
        ProjectStatsRow {
            project: "rust-fs-mcp".to_string(),
            git_branch: String::new(),
            requests: 100,
            sessions: 1,
            input_tokens: 80_000,
            output_tokens: 20_000,
            cache_read_tokens: 400_000,
            cache_creation_5m_tokens: 10_000,
            cache_creation_1h_tokens: 0,
            cost_usd_estimate: 0.5,
            last_ts: "2026-11-15T00:50:00Z".to_string(),
            ..ProjectStatsRow::default()
        },
    ]);
    let attribution_row = |name: &str, cost_usd: f64| MetricAttributionRow {
        name: name.to_string(),
        cost_usd,
        input_tokens: 40_000,
        output_tokens: 9_000,
        cache_read_tokens: 300_000,
        cache_creation_tokens: 12_000,
    };
    state.snapshot.attribution = Panel::Ready(vec![
        (
            AttributionDimension::Skill,
            vec![
                attribution_row("frontend-tui", 1.25),
                attribution_row("languages-rust", 0.75),
            ],
        ),
        (
            AttributionDimension::McpServer,
            vec![attribution_row("serena-mcp", 0.4)],
        ),
        (AttributionDimension::Plugin, Vec::new()),
        (
            AttributionDimension::Agent,
            vec![attribution_row("agent-planner", 2.0)],
        ),
        (
            AttributionDimension::Model,
            vec![attribution_row("claude-sonnet-5-5", 3.5)],
        ),
    ]);
    state.snapshot.skills = Panel::Ready(vec![
        SkillStatsRow {
            skill: "frontend-tui".to_string(),
            calls: 6,
            errors: 1,
            sessions: 2,
            first_ts: "2026-11-14T22:10:00Z".to_string(),
            last_ts: "2026-11-15T01:20:00Z".to_string(),
            agents: vec![
                usage_share("main", 4, 0, 0.0, "2026-11-15T01:20:00Z"),
                usage_share("worker", 2, 1, 0.0, "2026-11-15T00:30:00Z"),
            ],
            recent: vec![
                usage_share("main", 1, 0, 0.0, "2026-11-15T01:20:00Z"),
                usage_share("worker", 1, 1, 0.0, "2026-11-15T00:30:00Z"),
            ],
        },
        SkillStatsRow {
            skill: "core-response".to_string(),
            calls: 3,
            errors: 0,
            sessions: 3,
            first_ts: "2026-11-14T23:00:00Z".to_string(),
            last_ts: "2026-11-15T01:00:00Z".to_string(),
            agents: vec![usage_share("main", 3, 0, 0.0, "2026-11-15T01:00:00Z")],
            recent: vec![usage_share("main", 1, 0, 0.0, "2026-11-15T01:00:00Z")],
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
        skills: vec![
            SkillEntry {
                name: "frontend-tui".to_string(),
                description_chars: 180,
                path: "C:/cm-test-claude/skills/frontend-tui/SKILL.md".to_string(),
            },
            SkillEntry {
                name: "core-response".to_string(),
                description_chars: 150,
                path: "C:/cm-test-claude/skills/core-response/SKILL.md".to_string(),
            },
            SkillEntry {
                name: "utils-table".to_string(),
                description_chars: 210,
                path: "C:/cm-test-claude/skills/utils-table/SKILL.md".to_string(),
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

// 3-1. 분포 항목 한 줄 --------------------------------------------------------
pub fn usage_share(name: &str, count: u64, errors: u64, cost: f64, last_ts: &str) -> UsageShare {
    UsageShare {
        name: name.to_string(),
        count,
        errors,
        output_tokens: count as i64 * 300,
        cost_usd_estimate: cost,
        last_ts: last_ts.to_string(),
    }
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

// 5. 한도·캐시 카드용 합성 입력 -----------------------------------------------
// 시계를 이 값으로 고정해 카운트다운 문구 단언이 흔들리지 않게 함
pub fn fixed_limits_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 19, 10, 0, 0).unwrap()
}

// now 기준: 5시간 창 70% (리셋 3시간 뒤), 7일 창 35%, 컨텍스트 62%,
// 캐시 4분 12초 남음 (적중 82%, 미스 9/50), 스냅샷은 12초 전
pub fn sample_limits_data(now: DateTime<Utc>) -> LimitsData {
    let stamp = |offset_secs: i64| {
        (now + Duration::seconds(offset_secs)).to_rfc3339_opts(SecondsFormat::Millis, true)
    };
    let five_hour_resets_at = (now + Duration::hours(3)).timestamp();
    let seven_day_resets_at = (now + Duration::days(6)).timestamp();
    let earlier = LimitSnapshot {
        ts: stamp(-3_600),
        session_id: "sess-1".to_string(),
        five_hour_pct: Some(50.0),
        five_hour_resets_at: Some(five_hour_resets_at),
        seven_day_pct: Some(34.0),
        seven_day_resets_at: Some(seven_day_resets_at),
        ..LimitSnapshot::default()
    };
    let latest = LimitSnapshot {
        ts: stamp(-12),
        five_hour_pct: Some(70.0),
        seven_day_pct: Some(35.0),
        context_pct: Some(62.0),
        cache_warm: Some(true),
        cache_caching_observed: Some(true),
        cache_ttl: Some("5m".to_string()),
        cache_expires_at: Some((now + Duration::seconds(252)).timestamp()),
        cache_requests: Some(50),
        cache_misses: Some(9),
        cache_hit_ratio: Some(0.82),
        cache_last_miss_at: Some((now - Duration::seconds(180)).timestamp()),
        cache_last_miss_cause: Some("ttl_expired".to_string()),
        ..earlier.clone()
    };
    LimitsData {
        latest: Some(latest),
    }
}

// 개요·모델 탭 공용: 시계 고정, 스냅샷은 마지막 수집 12초 전
pub fn sample_state_with_limits(lang: Lang) -> AppState {
    let mut state = sample_state_with_tables();
    state.lang = lang;
    let now = fixed_limits_now();
    state.clock_override = Some(now);
    state.snapshot.taken_at = Some(now);
    state.snapshot.limits = Panel::Ready(sample_limits_data(now));
    state
}
