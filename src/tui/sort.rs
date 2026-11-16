//! tui/sort.rs
//! 카드별 표 정렬: 정렬 상태, 열 종류 규칙(문자·숫자), 타입별 열 비교, 키·머리글 공용 정렬 변경.

use crate::core::model::LiveEvent;
use crate::inventory::{AgentEntry, DoctorFinding, HookEntry, McpEntry};
use crate::store::query::{
    AgentStatsRow, CacheBreakRow, HookStatRow, ModelStatsRow, ToolLatencyRow,
};
use crate::tui::cards::{CARD_COUNT, CardId};
use crate::tui::format::{short_model, short_tool};
use crate::tui::poller::live_event_timestamp;
use crate::tui::views::feed::feed_event_name;
use std::cmp::Ordering;

// 정렬 가능한 카드
pub const SORTABLE_CARDS: [CardId; 10] = [
    CardId::EventFeed,
    CardId::ToolTable,
    CardId::HookTable,
    CardId::ModelTable,
    CardId::CacheBreaks,
    CardId::AgentTable,
    CardId::SettingsHooks,
    CardId::SettingsMcp,
    CardId::SettingsAgents,
    CardId::SettingsDoctor,
];

// 1. 정렬 상태 ----------------------------------------------------------------
// 열 종류 (비교 규칙은 아래 타입별 비교 함수가 소유, Number = 숫자·시간 열은 내림차순, Text 는 오름차순에서 시작)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKind {
    Text,
    Number,
}

// 카드별 열 종류와 기본 정렬
#[derive(Debug, Clone, Copy)]
pub struct SortSpec {
    pub kinds: &'static [SortKind],
    pub default: SortState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SortState {
    pub column: usize,
    pub descending: bool,
}

// 정렬 변경 요청: 머리글 클릭, s 키, S 키
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortChange {
    SelectColumn(usize),
    NextColumn,
    ReverseDirection,
}

impl SortState {
    // 정렬 순환: 새 열은 숫자·시간 열이 내림차순, 문자 열이 오름차순에서 시작하고 같은 열은 처음 방향 -> 반대 방향 -> 카드 기본 정렬 (머리글 클릭·S 공용)
    // 기본 정렬 열은 기본 방향이 곧 꺼짐 상태라 두 방향만 번갈아 바뀜
    pub fn apply_change(&mut self, change: SortChange, spec: SortSpec) {
        let column_count = spec.kinds.len().max(1);

        // 1. 요청 해석 (s 는 다음 열의 처음 방향, 클릭·S 는 대상 열의 순환)
        let column = match change {
            SortChange::NextColumn => {
                self.column = (self.column + 1) % column_count;
                self.descending = first_descending(spec, self.column);
                return;
            }
            SortChange::SelectColumn(column) => column,
            SortChange::ReverseDirection => self.column,
        };
        if column >= column_count {
            return;
        }

        // 2. 다른 열은 그 열의 처음 방향에서 시작
        let first = first_descending(spec, column);
        if self.column != column {
            self.column = column;
            self.descending = first;
            return;
        }

        // 3. 같은 열: 처음 방향 -> 반대 방향, 반대 방향은 기본 정렬로 (이미 기본이면 처음 방향)
        if self.descending == first || *self == spec.default {
            self.descending = !self.descending;
        } else {
            *self = spec.default;
        }
    }

    pub fn sort_arrow(self) -> &'static str {
        if self.descending { "↓" } else { "↑" }
    }
}

// 열의 처음 정렬 방향 (숫자·시간은 큰·최근 값 먼저, 문자는 오름차순)
fn first_descending(spec: SortSpec, column: usize) -> bool {
    spec.kinds.get(column) == Some(&SortKind::Number)
}

// 2. 카드별 정렬 규칙 ---------------------------------------------------------
// 정렬하지 않는 카드는 None
pub fn sort_spec(card: CardId) -> Option<SortSpec> {
    let spec = match card {
        CardId::EventFeed => SortSpec {
            kinds: &[
                SortKind::Number,
                SortKind::Text,
                SortKind::Text,
                SortKind::Number,
                SortKind::Text,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
            ],
            default: SortState {
                column: 0,
                descending: true,
            },
        },
        CardId::ToolTable => SortSpec {
            kinds: &[
                SortKind::Text,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
            ],
            default: SortState {
                column: 5,
                descending: true,
            },
        },
        CardId::HookTable => SortSpec {
            kinds: &[
                SortKind::Text,
                SortKind::Text,
                SortKind::Text,
                SortKind::Text,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
            ],
            default: SortState {
                column: 4,
                descending: true,
            },
        },
        CardId::ModelTable => SortSpec {
            kinds: &[
                SortKind::Text,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
            ],
            default: SortState {
                column: 1,
                descending: true,
            },
        },
        CardId::AgentTable => SortSpec {
            kinds: &[
                SortKind::Text,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Text,
                SortKind::Number,
            ],
            default: SortState {
                column: 2,
                descending: true,
            },
        },
        CardId::CacheBreaks => SortSpec {
            kinds: &[
                SortKind::Number,
                SortKind::Text,
                SortKind::Text,
                SortKind::Number,
                SortKind::Number,
                SortKind::Number,
                SortKind::Text,
            ],
            default: SortState {
                column: 0,
                descending: true,
            },
        },
        CardId::SettingsHooks => SortSpec {
            kinds: &[
                SortKind::Text,
                SortKind::Text,
                SortKind::Text,
                SortKind::Text,
                SortKind::Text,
                SortKind::Number,
            ],
            default: SortState::default(),
        },
        CardId::SettingsMcp => SortSpec {
            kinds: &[
                SortKind::Text,
                SortKind::Text,
                SortKind::Text,
                SortKind::Text,
            ],
            default: SortState {
                column: 1,
                descending: false,
            },
        },
        CardId::SettingsAgents => SortSpec {
            kinds: &[
                SortKind::Text,
                SortKind::Text,
                SortKind::Text,
                SortKind::Number,
            ],
            default: SortState::default(),
        },
        CardId::SettingsDoctor => SortSpec {
            kinds: &[SortKind::Text, SortKind::Text, SortKind::Text],
            default: SortState::default(),
        },
        _ => return None,
    };
    Some(spec)
}

// 카드별 기본 정렬 상태 배열 (정렬하지 않는 카드는 0 번 열 오름차순)
pub fn default_card_sorts() -> [SortState; CARD_COUNT] {
    let mut sorts = [SortState::default(); CARD_COUNT];
    for card in SORTABLE_CARDS {
        if let Some(spec) = sort_spec(card) {
            sorts[card as usize] = spec.default;
        }
    }
    sorts
}

// 3. 행 정렬 ------------------------------------------------------------------
// 안정 정렬: 동률은 조회 순서 유지
pub fn sort_rows<T>(rows: &mut [T], state: SortState, compare: fn(&T, &T, usize) -> Ordering) {
    rows.sort_by(|left, right| {
        let order = compare(left, right, state.column);
        if state.descending {
            order.reverse()
        } else {
            order
        }
    });
}

// 문자 열 비교: 표시 문자열 기준, 대소문자 무시
pub fn compare_display_text(left: &str, right: &str) -> Ordering {
    left.chars()
        .flat_map(char::to_lowercase)
        .cmp(right.chars().flat_map(char::to_lowercase))
}

// 4. 열별 비교 (타입별) -------------------------------------------------------
// 숫자·시간은 값 비교, 없는 값은 가장 작게, 문자열은 표시 문자열 기준
pub fn compare_tool_row(left: &ToolLatencyRow, right: &ToolLatencyRow, column: usize) -> Ordering {
    match column {
        0 => compare_display_text(&short_tool(&left.tool_name), &short_tool(&right.tool_name)),
        1 => left.calls.cmp(&right.calls),
        2 => left.errors.cmp(&right.errors),
        3 => left.p50_ms.cmp(&right.p50_ms),
        4 => left.p90_ms.cmp(&right.p90_ms),
        5 => left.total_ms.cmp(&right.total_ms),
        _ => Ordering::Equal,
    }
}

// 훅 판정 순서는 표시 언어와 무관 (모르는 값, 거부, 오류, 재작성, 허용)
fn outcome_rank(outcome: &str) -> u8 {
    match outcome {
        "deny" => 1,
        "error" => 2,
        "rewrite" => 3,
        "allow" => 4,
        _ => 0,
    }
}

pub fn compare_hook_row(left: &HookStatRow, right: &HookStatRow, column: usize) -> Ordering {
    match column {
        0 => compare_display_text(&left.route, &right.route),
        1 => compare_display_text(&left.guard, &right.guard),
        2 => outcome_rank(&left.outcome).cmp(&outcome_rank(&right.outcome)),
        3 => compare_display_text(&left.detail, &right.detail),
        4 => left.count.cmp(&right.count),
        5 => left.p50_ms.cmp(&right.p50_ms),
        6 => left.p90_ms.cmp(&right.p90_ms),
        7 => left.saved_ms.cmp(&right.saved_ms),
        _ => Ordering::Equal,
    }
}

pub fn compare_model_row(left: &ModelStatsRow, right: &ModelStatsRow, column: usize) -> Ordering {
    match column {
        0 => compare_display_text(short_model(&left.model), short_model(&right.model)),
        1 => left.requests.cmp(&right.requests),
        2 => left.sessions.cmp(&right.sessions),
        3 => left.agent_types.cmp(&right.agent_types),
        4 => left.input_tokens.cmp(&right.input_tokens),
        5 => left.output_tokens.cmp(&right.output_tokens),
        6 => left.cache_read_tokens.cmp(&right.cache_read_tokens),
        7 => left.cache_creation_tokens.cmp(&right.cache_creation_tokens),
        8 => left.cache_hit_ratio.total_cmp(&right.cache_hit_ratio),
        9 => left.wait_p50_ms.cmp(&right.wait_p50_ms),
        10 => left.wait_p90_ms.cmp(&right.wait_p90_ms),
        11 => left.cost_usd_estimate.total_cmp(&right.cost_usd_estimate),
        12 => left.last_ts.cmp(&right.last_ts),
        _ => Ordering::Equal,
    }
}

pub fn compare_agent_row(left: &AgentStatsRow, right: &AgentStatsRow, column: usize) -> Ordering {
    match column {
        0 => compare_display_text(&left.agent, &right.agent),
        1 => left.runs.cmp(&right.runs),
        2 => left.requests.cmp(&right.requests),
        3 => left.sessions.cmp(&right.sessions),
        4 => left.input_tokens.cmp(&right.input_tokens),
        5 => left.output_tokens.cmp(&right.output_tokens),
        6 => left.cache_read_tokens.cmp(&right.cache_read_tokens),
        7 => left.cost_usd_estimate.total_cmp(&right.cost_usd_estimate),
        8 => left.wait_p50_ms.cmp(&right.wait_p50_ms),
        9 => left.wait_p90_ms.cmp(&right.wait_p90_ms),
        10 => left.tool_calls.cmp(&right.tool_calls),
        11 => left.tool_errors.cmp(&right.tool_errors),
        12 => compare_display_text(short_model(&left.top_model), short_model(&right.top_model)),
        13 => left.last_ts.cmp(&right.last_ts),
        _ => Ordering::Equal,
    }
}

pub fn compare_cache_break_row(
    left: &CacheBreakRow,
    right: &CacheBreakRow,
    column: usize,
) -> Ordering {
    match column {
        0 => left.ts.cmp(&right.ts),
        1 => compare_display_text(&left.agent, &right.agent),
        2 => compare_display_text(short_model(&left.model), short_model(&right.model)),
        3 => left.cache_creation_tokens.cmp(&right.cache_creation_tokens),
        4 => left
            .previous_context_tokens
            .cmp(&right.previous_context_tokens),
        5 => left.gap_secs.cmp(&right.gap_secs),
        6 => compare_display_text(&left.likely_cause, &right.likely_cause),
        _ => Ordering::Equal,
    }
}

// 피드 열: 시각, 종류, 이름, 소요, 에이전트, 입력, 출력, 캐시읽기 (화면에 보이는 값 기준)
pub fn compare_event_row(left: &LiveEvent, right: &LiveEvent, column: usize) -> Ordering {
    // 종류는 표시 언어와 무관한 고정 순서 (API, 도구, 알림, 훅)
    let kind_rank = |event: &LiveEvent| match event {
        LiveEvent::Api(_) => 0,
        LiveEvent::Tool(_) => 1,
        LiveEvent::Notice { .. } => 2,
        LiveEvent::Hook(_) => 3,
    };
    let agent_label = |event: &LiveEvent| match event {
        LiveEvent::Api(row) => row.agent.clone(),
        LiveEvent::Tool(row) => row.agent.clone(),
        _ => String::new(),
    };
    let token_count = |event: &LiveEvent, slot: usize| match event {
        LiveEvent::Api(row) => Some(match slot {
            0 => row.input_tokens,
            1 => row.output_tokens,
            _ => row.cache_read_tokens,
        }),
        _ => None,
    };
    let duration = |event: &LiveEvent| match event {
        LiveEvent::Api(row) => row.duration_ms,
        LiveEvent::Tool(row) => row.duration_ms,
        LiveEvent::Hook(row) => row.saved_ms.or(row.duration_ms),
        LiveEvent::Notice { .. } => None,
    };
    match column {
        0 => live_event_timestamp(left).cmp(live_event_timestamp(right)),
        1 => kind_rank(left).cmp(&kind_rank(right)),
        2 => compare_display_text(&feed_event_name(left), &feed_event_name(right)),
        3 => duration(left).cmp(&duration(right)),
        4 => compare_display_text(&agent_label(left), &agent_label(right)),
        5 => token_count(left, 0).cmp(&token_count(right, 0)),
        6 => token_count(left, 1).cmp(&token_count(right, 1)),
        7 => token_count(left, 2).cmp(&token_count(right, 2)),
        _ => Ordering::Equal,
    }
}

// 5. 설정 카드 열 비교 --------------------------------------------------------
pub fn compare_hook_entry(left: &HookEntry, right: &HookEntry, column: usize) -> Ordering {
    match column {
        0 => compare_display_text(&left.event, &right.event),
        1 => compare_display_text(&left.matcher, &right.matcher),
        2 => compare_display_text(&left.hook_type, &right.hook_type),
        3 => compare_display_text(&left.target, &right.target),
        4 => left.is_async.cmp(&right.is_async),
        5 => left.timeout_secs.cmp(&right.timeout_secs),
        _ => Ordering::Equal,
    }
}

pub fn compare_mcp_entry(left: &McpEntry, right: &McpEntry, column: usize) -> Ordering {
    match column {
        0 => left.denied.cmp(&right.denied),
        1 => compare_display_text(&left.name, &right.name),
        2 => compare_display_text(&left.transport, &right.transport),
        3 => compare_display_text(&left.target, &right.target),
        _ => Ordering::Equal,
    }
}

pub fn compare_agent_entry(left: &AgentEntry, right: &AgentEntry, column: usize) -> Ordering {
    match column {
        0 => compare_display_text(&left.name, &right.name),
        1 => compare_display_text(&left.model, &right.model),
        2 => compare_display_text(&left.effort, &right.effort),
        3 => left.max_turns.cmp(&right.max_turns),
        _ => Ordering::Equal,
    }
}

// 진단 수준은 심각한 순서(실패, 경고, 정상, 기타)가 오름차순
pub fn compare_doctor_finding(
    left: &DoctorFinding,
    right: &DoctorFinding,
    column: usize,
) -> Ordering {
    let severity = |finding: &DoctorFinding| match finding.level.as_str() {
        "fail" => 0,
        "warn" => 1,
        "ok" => 2,
        _ => 3,
    };
    match column {
        0 => severity(left).cmp(&severity(right)),
        1 => compare_display_text(&left.topic, &right.topic),
        2 => compare_display_text(&left.message, &right.message),
        _ => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_row(name: &str, calls: u64, p50: Option<i64>) -> ToolLatencyRow {
        ToolLatencyRow {
            tool_name: name.to_string(),
            calls,
            p50_ms: p50,
            ..ToolLatencyRow::default()
        }
    }

    fn tool_spec() -> SortSpec {
        sort_spec(CardId::ToolTable).unwrap()
    }

    #[test]
    fn next_column_starts_numbers_descending_and_text_ascending_and_wraps() {
        let mut state = SortState {
            column: 1,
            descending: true,
        };
        let mut visited = Vec::new();
        for _ in 0..tool_spec().kinds.len() {
            state.apply_change(SortChange::NextColumn, tool_spec());
            visited.push((state.column, state.descending));
        }
        assert_eq!(
            visited,
            [
                (2, true),
                (3, true),
                (4, true),
                (5, true),
                (0, false),
                (1, true)
            ]
        );
    }

    #[test]
    fn reverse_direction_on_a_number_column_cycles_descending_ascending_default() {
        let mut state = SortState {
            column: 1,
            descending: true,
        };
        assert_eq!(state.sort_arrow(), "↓");
        state.apply_change(SortChange::ReverseDirection, tool_spec());
        assert_eq!((state.column, state.descending), (1, false));
        assert_eq!(state.sort_arrow(), "↑");

        // 세 번째는 카드 기본 정렬 (도구: 합계 내림차순)
        state.apply_change(SortChange::ReverseDirection, tool_spec());
        assert_eq!(state, tool_spec().default);
    }

    #[test]
    fn reverse_direction_on_a_text_column_cycles_ascending_descending_default() {
        let spec = tool_spec();
        let mut state = SortState {
            column: 0,
            descending: false,
        };
        state.apply_change(SortChange::ReverseDirection, spec);
        assert_eq!((state.column, state.descending), (0, true));
        state.apply_change(SortChange::ReverseDirection, spec);
        assert_eq!(state, spec.default);
    }

    #[test]
    fn number_column_clicks_run_descending_ascending_then_the_card_default() {
        // 도구 표 호출 수 열 (숫자): 기본 정렬은 다른 열
        let spec = tool_spec();
        assert_eq!(spec.kinds[1], SortKind::Number);
        let mut state = spec.default;
        state.apply_change(SortChange::SelectColumn(1), spec);
        assert_eq!((state.column, state.descending), (1, true));
        state.apply_change(SortChange::SelectColumn(1), spec);
        assert_eq!((state.column, state.descending), (1, false));
        state.apply_change(SortChange::SelectColumn(1), spec);
        assert_eq!(state, spec.default);

        // 다시 누르면 처음부터
        state.apply_change(SortChange::SelectColumn(1), spec);
        assert_eq!((state.column, state.descending), (1, true));
    }

    #[test]
    fn time_column_clicks_run_latest_first_oldest_first_then_the_card_default() {
        // 모델 표 마지막 시각 열 (시간): 기본 정렬은 요청 수 열
        let spec = sort_spec(CardId::ModelTable).unwrap();
        assert_eq!(spec.kinds[12], SortKind::Number);
        let mut state = spec.default;
        state.apply_change(SortChange::SelectColumn(12), spec);
        assert_eq!((state.column, state.descending), (12, true));
        state.apply_change(SortChange::SelectColumn(12), spec);
        assert_eq!((state.column, state.descending), (12, false));
        state.apply_change(SortChange::SelectColumn(12), spec);
        assert_eq!(state, spec.default);

        // 이벤트 피드 시각 열: 기본이 최근 먼저라 오름과 내림만 번갈아 바뀜
        let feed = sort_spec(CardId::EventFeed).unwrap();
        let mut state = feed.default;
        state.apply_change(SortChange::SelectColumn(0), feed);
        assert_eq!((state.column, state.descending), (0, false));
        state.apply_change(SortChange::SelectColumn(0), feed);
        assert_eq!(state, feed.default);
    }

    #[test]
    fn text_column_clicks_run_ascending_descending_then_the_card_default() {
        let spec = tool_spec();
        assert_eq!(spec.kinds[0], SortKind::Text);
        let mut state = spec.default;
        state.apply_change(SortChange::SelectColumn(0), spec);
        assert_eq!((state.column, state.descending), (0, false));
        state.apply_change(SortChange::SelectColumn(0), spec);
        assert_eq!((state.column, state.descending), (0, true));
        state.apply_change(SortChange::SelectColumn(0), spec);
        assert_eq!(state, spec.default);

        // 다시 누르면 처음부터
        state.apply_change(SortChange::SelectColumn(0), spec);
        assert_eq!((state.column, state.descending), (0, false));
    }

    #[test]
    fn a_different_header_starts_in_its_own_first_direction() {
        let spec = tool_spec();
        let mut state = SortState {
            column: 0,
            descending: true,
        };
        state.apply_change(SortChange::SelectColumn(4), spec);
        assert_eq!((state.column, state.descending), (4, true));
        state.apply_change(SortChange::SelectColumn(4), spec);
        assert_eq!((state.column, state.descending), (4, false));

        // 순환 중간에 다른 열을 누르면 그 열이 처음 방향부터 (숫자는 내림, 문자는 오름)
        state.apply_change(SortChange::SelectColumn(1), spec);
        assert_eq!((state.column, state.descending), (1, true));
        state.apply_change(SortChange::SelectColumn(0), spec);
        assert_eq!((state.column, state.descending), (0, false));

        // 범위 밖 열은 무시
        state.apply_change(SortChange::SelectColumn(99), spec);
        assert_eq!((state.column, state.descending), (0, false));
    }

    #[test]
    fn default_column_alternates_between_its_two_directions() {
        // 기본이 내림차순인 숫자 열: 내림(기본) -> 오름 -> 내림(기본)
        let spec = tool_spec();
        let mut state = spec.default;
        state.apply_change(SortChange::SelectColumn(5), spec);
        assert_eq!((state.column, state.descending), (5, false));
        state.apply_change(SortChange::SelectColumn(5), spec);
        assert_eq!(state, spec.default);
        state.apply_change(SortChange::SelectColumn(5), spec);
        assert_eq!((state.column, state.descending), (5, false));

        // 기본이 오름차순인 열 (설정 훅 표): 오름(기본) -> 내림 -> 오름(기본)
        let hooks = sort_spec(CardId::SettingsHooks).unwrap();
        let mut state = hooks.default;
        state.apply_change(SortChange::SelectColumn(0), hooks);
        assert_eq!((state.column, state.descending), (0, true));
        state.apply_change(SortChange::SelectColumn(0), hooks);
        assert_eq!(state, hooks.default);
    }

    #[test]
    fn every_sortable_card_has_a_spec_and_a_valid_default_column() {
        for card in SORTABLE_CARDS {
            let spec = sort_spec(card).unwrap();
            assert!(spec.default.column < spec.kinds.len(), "{card:?}");
        }
        let defaults = default_card_sorts();
        let tool_default = defaults[CardId::ToolTable as usize];
        assert_eq!((tool_default.column, tool_default.descending), (5, true));
        let feed_default = defaults[CardId::EventFeed as usize];
        assert_eq!((feed_default.column, feed_default.descending), (0, true));
        let hook_default = defaults[CardId::HookTable as usize];
        assert_eq!((hook_default.column, hook_default.descending), (4, true));
        for card in [CardId::ModelTable, CardId::AgentTable] {
            let default = defaults[card as usize];
            assert_eq!(
                (default.column, default.descending),
                (card_requests_column(card), true)
            );
        }
        assert!(sort_spec(CardId::ToolDetail).is_none());
    }

    fn card_requests_column(card: CardId) -> usize {
        if card == CardId::ModelTable { 1 } else { 2 }
    }

    #[test]
    fn numbers_sort_numerically_and_text_sorts_by_display_string() {
        let mut rows = vec![
            tool_row("Bash", 9, Some(40)),
            tool_row("read", 100, None),
            tool_row("Edit", 25, Some(10)),
        ];

        // 숫자 열: 9 < 25 < 100 (문자열 비교였다면 100 < 25 < 9)
        sort_rows(
            &mut rows,
            SortState {
                column: 1,
                descending: false,
            },
            compare_tool_row,
        );
        let names: Vec<&str> = rows.iter().map(|row| row.tool_name.as_str()).collect();
        assert_eq!(names, ["Bash", "Edit", "read"]);

        // 없는 지연은 가장 작게
        sort_rows(
            &mut rows,
            SortState {
                column: 3,
                descending: false,
            },
            compare_tool_row,
        );
        let names: Vec<&str> = rows.iter().map(|row| row.tool_name.as_str()).collect();
        assert_eq!(names, ["read", "Edit", "Bash"]);

        // 문자 열은 대소문자 무시
        sort_rows(
            &mut rows,
            SortState {
                column: 0,
                descending: false,
            },
            compare_tool_row,
        );
        let names: Vec<&str> = rows.iter().map(|row| row.tool_name.as_str()).collect();
        assert_eq!(names, ["Bash", "Edit", "read"]);
    }

    #[test]
    fn equal_values_keep_the_previous_order() {
        let mut rows = vec![
            tool_row("a", 5, None),
            tool_row("b", 5, None),
            tool_row("c", 9, None),
        ];
        sort_rows(
            &mut rows,
            SortState {
                column: 1,
                descending: true,
            },
            compare_tool_row,
        );
        let names: Vec<&str> = rows.iter().map(|row| row.tool_name.as_str()).collect();
        assert_eq!(names, ["c", "a", "b"]);
    }

    #[test]
    fn model_and_agent_rows_compare_floats_durations_and_text() {
        let low = ModelStatsRow {
            model: "claude-b".to_string(),
            cost_usd_estimate: 1.5,
            wait_p50_ms: None,
            cache_hit_ratio: 0.2,
            ..ModelStatsRow::default()
        };
        let high = ModelStatsRow {
            model: "claude-A".to_string(),
            cost_usd_estimate: 12.0,
            wait_p50_ms: Some(300),
            cache_hit_ratio: 0.9,
            ..ModelStatsRow::default()
        };
        assert_eq!(compare_model_row(&low, &high, 11), Ordering::Less);
        assert_eq!(compare_model_row(&low, &high, 9), Ordering::Less);
        assert_eq!(compare_model_row(&low, &high, 8), Ordering::Less);
        assert_eq!(compare_model_row(&low, &high, 0), Ordering::Greater);

        // 접두사 유무 혼합 이름
        let bare = ModelStatsRow {
            model: "zeta".to_string(),
            ..ModelStatsRow::default()
        };
        let prefixed = ModelStatsRow {
            model: "claude-zz".to_string(),
            ..ModelStatsRow::default()
        };
        assert_eq!(compare_model_row(&bare, &prefixed, 0), Ordering::Less);
        assert_eq!(compare_model_row(&prefixed, &bare, 0), Ordering::Greater);

        let first = AgentStatsRow {
            agent: "main".to_string(),
            tool_calls: 9,
            ..AgentStatsRow::default()
        };
        let second = AgentStatsRow {
            agent: "worker".to_string(),
            tool_calls: 100,
            ..AgentStatsRow::default()
        };
        assert_eq!(compare_agent_row(&first, &second, 10), Ordering::Less);
        assert_eq!(compare_agent_row(&first, &second, 0), Ordering::Less);
    }

    #[test]
    fn doctor_level_orders_by_severity_not_alphabet() {
        let finding = |level: &str| DoctorFinding {
            level: level.to_string(),
            ..DoctorFinding::default()
        };
        assert_eq!(
            compare_doctor_finding(&finding("fail"), &finding("ok"), 0),
            Ordering::Less
        );
        assert_eq!(
            compare_doctor_finding(&finding("warn"), &finding("fail"), 0),
            Ordering::Greater
        );
    }
}
