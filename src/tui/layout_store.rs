//! tui/layout_store.rs
//! TUI 상태 파일(tui.toml): 표시 언어·카드 순서·경계 비율·표 열 폭 저장과 복원, 임시 파일 후 이름 바꾸기 방식의 원자적 쓰기.

use crate::core::config::TUI_STATE_FILE_NAME;
use crate::store::metrics::AttributionDimension;
use crate::tui::cards::{CARD_COUNT, CardId, MAX_COLUMN_WIDTH, MAX_TABLE_COLUMNS};
use crate::tui::i18n::{self, Lang, StateIssue};
use crate::tui::layout::grid::{TabGrid, card_from_key, card_key, default_rows};
use crate::tui::layout::{CardLayout, MAX_SLOTS};
use crate::tui::state::{ATTRIBUTION_DIMENSIONS, ActiveTab, AppState, TAB_COUNT, ToastLevel};
use crate::tui::state_writer::StateSave;
use crate::tui::views::feed::FEED_COLUMNS;
use crate::tui::views::models::{AGENT_COLUMNS, CACHE_BREAK_COLUMNS, MODEL_COLUMNS};
use crate::tui::views::projects::{ATTRIBUTION_COLUMNS, PROJECT_COLUMNS};
use crate::tui::views::settings::{
    SETTINGS_AGENT_COLUMNS, SETTINGS_DOCTOR_COLUMNS, SETTINGS_HOOK_COLUMNS, SETTINGS_MCP_COLUMNS,
};
use crate::tui::views::skills::SKILL_COLUMNS;
use crate::tui::views::tables::{HOOK_COLUMNS, TOOL_COLUMNS};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const LAYOUT_VERSION: i64 = 1;
const BACKUP_SUFFIX: &str = ".bak";
const TEMP_SUFFIX: &str = ".tmp";
// 링크 사슬을 따라가는 최대 단계, 시작할 때 지우는 임시 파일의 최소 나이 (더 최근 것은 다른 프로세스의 진행 중 저장)
const MAX_LINK_DEPTH: usize = 16;
const STALE_TEMP_AGE: Duration = Duration::from_secs(600);
// 열 폭을 파일에 쓰는 표 카드 (열 폭 0 = 자동 폭)
const TABLE_CARDS: [CardId; 13] = [
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
    CardId::ProjectTable,
    CardId::AttributionTable,
    CardId::SkillTable,
];
// 예전 한 축 그룹 키 -> 탭과 예전 칸 번호 순서의 카드 (리포트만 목차·본문이 한 줄), 읽기만 하고 다시 쓰지 않음
const LEGACY_GROUPS: [(&str, ActiveTab, &[CardId]); 8] = [
    (
        "tools_rows",
        ActiveTab::Tools,
        &[CardId::ToolTable, CardId::ToolDetail],
    ),
    (
        "hooks_rows",
        ActiveTab::Hooks,
        &[CardId::HookTable, CardId::HookDetail],
    ),
    (
        "models_rows",
        ActiveTab::Models,
        &[
            CardId::ModelTable,
            CardId::ModelDetail,
            CardId::CacheBreaks,
            CardId::CacheTimer,
        ],
    ),
    (
        "agents_rows",
        ActiveTab::Agents,
        &[CardId::AgentTable, CardId::AgentDetail],
    ),
    (
        "settings_rows",
        ActiveTab::Settings,
        &[
            CardId::SettingsHooks,
            CardId::SettingsMcp,
            CardId::SettingsAgents,
            CardId::SettingsDoctor,
        ],
    ),
    (
        "report_cols",
        ActiveTab::Report,
        &[CardId::ReportOutline, CardId::ReportBody],
    ),
    (
        "projects_rows",
        ActiveTab::Projects,
        &[
            CardId::ProjectTable,
            CardId::ProjectDetail,
            CardId::AttributionTable,
        ],
    ),
    (
        "skills_rows",
        ActiveTab::Skills,
        &[CardId::SkillTable, CardId::SkillDetail],
    ),
];
// 예전 개요 그룹 키 (추이 순서만 이어받고 나머지는 알림 없이 버림)
const LEGACY_OVERVIEW_KEYS: [&str; 7] = [
    "overview_rows",
    "overview_compact_rows",
    "overview_kpi_cols",
    "overview_trend_cols",
    "overview_feed_cols",
    "overview_chart_rows",
    "overview_chart_cols",
];
// 없앤 카드 이름 ([grids] 에서 알림 없이 빼고 검사)
const RETIRED_CARD_KEYS: [&str; 3] = ["kpi_wait", "limits_card", "chart_limits"];

// 1. 파일 형식 ----------------------------------------------------------------
// 언어·귀속 차원은 최상위 문자열 (알 수 없는 값만 무시, 차원은 기본값이면 기록하지 않음)
// 탭 순서는 최상위 고정 이름 배열 (기본 순서면 기록하지 않고, 탭을 한 번씩 담지 않으면 무시, 나중에 더한 탭만 빠지면 뒤에 붙임)
// 그룹·카드는 고정 이름 키, 값은 정수 배열 (항목마다 길이·범위를 검사해 맞지 않는 항목만 무시)
// 격자는 탭 이름 키, 값은 줄별 카드 이름 배열 (기본 격자면 기록 안 함)
#[derive(Debug, Default, Serialize, Deserialize)]
struct LayoutFile {
    version: i64,
    #[serde(default)]
    language: Option<toml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attribution_dimension: Option<toml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tab_order: Option<toml::Value>,
    #[serde(default, skip_serializing)]
    orders: BTreeMap<String, Vec<i64>>,
    #[serde(default, skip_serializing)]
    ratios: BTreeMap<String, Vec<i64>>,
    #[serde(default)]
    column_widths: BTreeMap<String, Vec<i64>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    grids: BTreeMap<String, toml::Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    grid_ratios: BTreeMap<String, Vec<i64>>,
}

// 읽기 결과: 파일 없음, 읽은 내용, 읽을 수 없는 파일 (사유)
enum LayoutRead {
    Missing,
    Loaded(LayoutFile),
    Broken(StateIssue),
}

// 2. 표 카드 이름·열 수 -------------------------------------------------------
fn table_card_columns(card: CardId) -> Option<(&'static str, usize)> {
    match card {
        CardId::EventFeed => Some(("event_feed", FEED_COLUMNS.len())),
        CardId::ToolTable => Some(("tool_table", TOOL_COLUMNS.len())),
        CardId::HookTable => Some(("hook_table", HOOK_COLUMNS.len())),
        CardId::ModelTable => Some(("model_table", MODEL_COLUMNS.len())),
        CardId::CacheBreaks => Some(("cache_breaks", CACHE_BREAK_COLUMNS.len())),
        CardId::AgentTable => Some(("agent_table", AGENT_COLUMNS.len())),
        CardId::SettingsHooks => Some(("settings_hooks", SETTINGS_HOOK_COLUMNS.len())),
        CardId::SettingsMcp => Some(("settings_mcp", SETTINGS_MCP_COLUMNS.len())),
        CardId::SettingsAgents => Some(("settings_agents", SETTINGS_AGENT_COLUMNS.len())),
        CardId::SettingsDoctor => Some(("settings_doctor", SETTINGS_DOCTOR_COLUMNS.len())),
        CardId::ProjectTable => Some(("project_table", PROJECT_COLUMNS.len())),
        CardId::AttributionTable => Some(("attribution_table", ATTRIBUTION_COLUMNS.len())),
        CardId::SkillTable => Some(("skill_table", SKILL_COLUMNS.len())),
        _ => None,
    }
}

fn legacy_column_count(card: CardId) -> Option<usize> {
    match card {
        CardId::ToolTable => Some(6),
        CardId::HookTable => Some(8),
        CardId::ModelTable => Some(13),
        CardId::ProjectTable => Some(11),
        CardId::SkillTable => Some(9),
        _ => None,
    }
}

// 3. 현재 배치 -> 파일 내용 ---------------------------------------------------
// 기본 배치와 다른 항목만 기록
fn build_layout_file(
    layout: &CardLayout,
    column_widths: &[[Option<u16>; MAX_TABLE_COLUMNS]; CARD_COUNT],
    lang: Lang,
    dimension: AttributionDimension,
    tab_order: &[ActiveTab; TAB_COUNT],
) -> LayoutFile {
    let mut file = LayoutFile {
        version: LAYOUT_VERSION,
        language: Some(toml::Value::String(lang.code().to_string())),
        ..LayoutFile::default()
    };
    if dimension != ATTRIBUTION_DIMENSIONS[0] {
        file.attribution_dimension = toml::Value::try_from(dimension).ok();
    }
    if *tab_order != ActiveTab::ALL {
        let keys = tab_order
            .iter()
            .map(|tab| toml::Value::String(tab.tab_key().to_string()))
            .collect();
        file.tab_order = Some(toml::Value::Array(keys));
    }
    for card in TABLE_CARDS {
        let Some((key, columns)) = table_card_columns(card) else {
            continue;
        };
        let widths = &column_widths[card as usize][..columns];
        if widths.iter().all(Option::is_none) {
            continue;
        }
        let values = widths
            .iter()
            .map(|width| i64::from(width.unwrap_or(0)))
            .collect();
        file.column_widths.insert(key.to_string(), values);
    }
    for tab in ActiveTab::ALL {
        let grid = layout.grid_of(tab);
        if grid == default_rows(tab) {
            continue;
        }
        let rows: Vec<toml::Value> = grid
            .rows
            .iter()
            .filter(|cells| cells.iter().any(Option::is_some))
            .map(|cells| {
                toml::Value::Array(
                    cells
                        .iter()
                        .flatten()
                        .map(|card| toml::Value::String(card_key(*card).to_string()))
                        .collect(),
                )
            })
            .collect();
        file.grids
            .insert(tab.tab_key().to_string(), toml::Value::Array(rows));
    }
    for (key, ratios) in layout.grid_ratio_entries() {
        let values = ratios.iter().map(|ratio| i64::from(*ratio)).collect();
        file.grid_ratios.insert(key, values);
    }
    file
}

// 4. 파일 내용 -> 현재 배치 ---------------------------------------------------
// 맞지 않는 항목(모르는 이름, 길이·범위 불일치, 열 폭 상한 초과)은 건너뛰고 건너뛴 개수를 반환
fn apply_layout_file(
    file: &LayoutFile,
    layout: &mut CardLayout,
    column_widths: &mut [[Option<u16>; MAX_TABLE_COLUMNS]; CARD_COUNT],
) -> usize {
    // 예전 한 축 그룹 순서 -> 격자 (줄 i = 카드 order[i], 리포트만 한 줄), 카드 번호의 순열이 아니면 None
    let legacy_grid = |key: &str, cards: &[CardId], values: &[i64]| -> Option<TabGrid> {
        let parsed: Option<Vec<usize>> = values
            .iter()
            .map(|value| usize::try_from(*value).ok())
            .collect();
        let mut order = parsed?;
        // 타이머가 생기기 전(3칸) 모델 순서는 타이머를 마지막 줄로
        if key == "models_rows" && order.len() == 3 {
            order.push(3);
        }
        let mut sorted = order.clone();
        sorted.sort_unstable();
        if !sorted.iter().copied().eq(0..cards.len()) {
            return None;
        }
        let mut grid = TabGrid::default();
        for (line, card) in order.iter().enumerate() {
            let (row, column) = if key == "report_cols" {
                (0, line)
            } else {
                (line, 0)
            };
            grid.rows[row][column] = Some(cards[*card]);
        }
        Some(grid)
    };

    let mut skipped = 0usize;
    // 예전 [orders]: [grids] 에 그 탭이 없을 때만 탭 격자로, 맞지 않는 값과 개요 키는 알림 없이 버림 (모르는 키만 건너뜀)
    for (key, values) in &file.orders {
        let legacy = LEGACY_GROUPS
            .iter()
            .find(|(name, _, _)| *name == key.as_str());
        let Some((_, tab, cards)) = legacy else {
            skipped += usize::from(!LEGACY_OVERVIEW_KEYS.contains(&key.as_str()));
            continue;
        };
        if file.grids.contains_key(tab.tab_key()) {
            continue;
        }
        if let Some(grid) = legacy_grid(key, cards, values) {
            layout.set_grid(*tab, grid);
        }
    }
    // 예전 개요 추이 순서: 예전 카드 1(요청 추이)·2(토큰 추이)가 적힌 순서로 추이 줄의 두 칸을 채움
    let trend_cards: Vec<CardId> = file
        .orders
        .get("overview_trend_cols")
        .into_iter()
        .flatten()
        .filter_map(|value| match *value {
            1 => Some(CardId::TrendRequests),
            2 => Some(CardId::TrendTokens),
            _ => None,
        })
        .collect();
    if trend_cards.len() == 2
        && trend_cards[0] != trend_cards[1]
        && !file.grids.contains_key(ActiveTab::Overview.tab_key())
    {
        let mut grid = layout.grid_of(ActiveTab::Overview);
        let trend_cells: Vec<(usize, usize)> = grid
            .rows
            .iter()
            .enumerate()
            .flat_map(|(row, cells)| {
                cells
                    .iter()
                    .enumerate()
                    .filter(|(_, cell)| {
                        matches!(cell, Some(CardId::TrendRequests | CardId::TrendTokens))
                    })
                    .map(move |(column, _)| (row, column))
            })
            .collect();
        for ((row, column), card) in trend_cells.iter().zip(&trend_cards) {
            grid.rows[*row][*column] = Some(*card);
        }
        layout.set_grid(ActiveTab::Overview, grid);
    }
    // 예전 [ratios]: <탭>_rows_<줄 수> (리포트는 report_cols_0) 격자 비율로 값 그대로, 맞지 않는 값과 개요 키는 알림 없이 버림
    for (key, values) in &file.ratios {
        let legacy = LEGACY_GROUPS
            .iter()
            .find(|(name, _, _)| *name == key.as_str());
        let Some((_, tab, cards)) = legacy else {
            skipped += usize::from(!LEGACY_OVERVIEW_KEYS.contains(&key.as_str()));
            continue;
        };
        if file.grids.contains_key(tab.tab_key()) {
            continue;
        }
        let mut ratios = values.clone();
        // 타이머가 생기기 전(3칸) 모델 비율은 새 경계를 구간 끝에
        if key == "models_rows" && ratios.len() == 2 {
            ratios.push(10_000);
        }
        let grid_key = if key == "report_cols" {
            "report_cols_0".to_string()
        } else {
            format!("{}_rows_{}", tab.tab_key(), cards.len())
        };
        layout.restore_grid_ratios(&grid_key, &ratios);
    }
    for (key, values) in &file.column_widths {
        let card = TABLE_CARDS
            .iter()
            .find(|card| table_card_columns(**card).is_some_and(|(name, _)| name == key.as_str()));
        let Some(card) = card else {
            skipped += 1;
            continue;
        };
        let columns = table_card_columns(*card).map_or(0, |(_, columns)| columns);
        let parsed: Option<Vec<Option<u16>>> = values
            .iter()
            .map(|value| match u16::try_from(*value) {
                Ok(0) => Some(None),
                Ok(width) if width <= MAX_COLUMN_WIDTH => Some(Some(width)),
                _ => None,
            })
            .collect();
        let legacy = legacy_column_count(*card);
        match parsed {
            Some(widths) if widths.len() == columns || Some(widths.len()) == legacy => {
                let slot = &mut column_widths[*card as usize];
                *slot = [None; MAX_TABLE_COLUMNS];
                slot[..widths.len()].copy_from_slice(&widths);
            }
            _ => skipped += 1,
        }
    }
    // 격자 먼저 (격자 비율 길이가 복원한 격자 기준)
    for (key, value) in &file.grids {
        let restored = ActiveTab::ALL
            .into_iter()
            .find(|tab| tab.tab_key() == key.as_str())
            .and_then(|tab| parse_stored_grid(tab, value).map(|grid| (tab, grid)));
        match restored {
            Some((tab, grid)) => layout.set_grid(tab, grid),
            None => skipped += 1,
        }
    }
    for (key, values) in &file.grid_ratios {
        if !layout.restore_grid_ratios(key, values) {
            skipped += 1;
        }
    }
    skipped
}

// [grids] 탭 항목 -> 격자, 모양이 틀리면 None (기본 격자에만 있는 카드는 기본 줄 끝에)
fn parse_stored_grid(tab: ActiveTab, value: &toml::Value) -> Option<TabGrid> {
    // 1. 줄별 카드 이름
    let stored_rows = value.as_array()?;
    if stored_rows.len() > MAX_SLOTS {
        return None;
    }
    let tab_cards = default_rows(tab);
    let mut rows: Vec<Vec<CardId>> = Vec::with_capacity(MAX_SLOTS);
    for stored_row in stored_rows {
        let mut row = stored_row
            .as_array()?
            .iter()
            .filter(|key| {
                !key.as_str()
                    .is_some_and(|name| RETIRED_CARD_KEYS.contains(&name))
            })
            .map(|key| key.as_str().and_then(card_from_key))
            .collect::<Option<Vec<CardId>>>()?;
        row.retain(|card| {
            tab_cards
                .rows
                .iter()
                .any(|cells| cells.contains(&Some(*card)))
        });
        if row.len() > MAX_SLOTS {
            return None;
        }
        if !row.is_empty() {
            rows.push(row);
        }
    }

    // 2. 중복 카드
    let mut seen = [false; CARD_COUNT];
    for card in rows.iter().flatten() {
        if seen[*card as usize] {
            return None;
        }
        seen[*card as usize] = true;
    }

    // 3. 빠진 기본 카드 (그 줄이 없으면 새 마지막 줄)
    let missing: Vec<(usize, CardId)> = default_rows(tab)
        .rows
        .iter()
        .enumerate()
        .flat_map(|(row, cells)| cells.iter().flatten().map(move |card| (row, *card)))
        .filter(|(_, card)| !seen[*card as usize])
        .collect();
    for (default_row, card) in missing {
        match rows.get_mut(default_row) {
            Some(row) => row.push(card),
            None => rows.push(vec![card]),
        }
    }

    // 4. 상한 확인 후 격자로
    if rows.len() > MAX_SLOTS || rows.iter().any(|row| row.len() > MAX_SLOTS) {
        return None;
    }
    let mut grid = TabGrid::default();
    for (row, cards) in rows.iter().enumerate() {
        for (column, card) in cards.iter().enumerate() {
            grid.rows[row][column] = Some(*card);
        }
    }
    Some(grid)
}

// 5. 파일 읽기·쓰기 -----------------------------------------------------------
fn read_layout_file(path: &Path) -> LayoutRead {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return LayoutRead::Missing,
        Err(error) => return LayoutRead::Broken(StateIssue::Detail(error.to_string())),
    };
    match toml::from_str::<LayoutFile>(&text) {
        Ok(file) if file.version == LAYOUT_VERSION => LayoutRead::Loaded(file),
        Ok(file) => LayoutRead::Broken(StateIssue::UnsupportedVersion(file.version)),
        Err(error) => LayoutRead::Broken(StateIssue::Detail(error.to_string())),
    }
}

// 읽을 수 없는 원본을 같은 폴더의 tui.toml.bak 으로 복사 (이전 .bak 덮어쓰기, 실패는 무시)
fn backup_state_file(path: &Path) {
    let Some(file_name) = path.file_name() else {
        return;
    };
    let mut backup_name = file_name.to_os_string();
    backup_name.push(BACKUP_SUFFIX);
    let _ = fs::copy(path, path.with_file_name(backup_name));
}

// 상태 파일이 심볼릭 링크면 링크 사슬을 끝까지 따라간 실제 파일 경로 (동기화 폴더로 연결한 사용자 구성), 링크가 아니면 받은 경로 그대로
// 대상이 아직 없는 끊어진 링크 사슬은 링크 값을 하나씩 따라가 마지막 자리를 돌려주고 중간 링크를 덮지 않음, 사슬이 MAX_LINK_DEPTH 를 넘으면 오류
fn resolve_state_target(path: &Path) -> io::Result<PathBuf> {
    if let Ok(resolved) = fs::canonicalize(path) {
        return Ok(resolved);
    }
    let mut current = path.to_path_buf();
    for _ in 0..MAX_LINK_DEPTH {
        let is_link =
            fs::symlink_metadata(&current).is_ok_and(|meta| meta.file_type().is_symlink());
        if !is_link {
            return Ok(current);
        }
        let link_value = fs::read_link(&current)?;
        current = match current.parent() {
            Some(dir) => dir.join(&link_value),
            None => link_value,
        };
    }
    Err(io::Error::new(
        ErrorKind::InvalidInput,
        format!(
            "symbolic link chain deeper than {MAX_LINK_DEPTH}: {}",
            path.display()
        ),
    ))
}

// 시작할 때 중단된 저장이 남긴 tui.toml.<pid>.tmp 삭제 (파일만, 실패는 무시), 링크 폴더와 실제 파일 폴더 둘 다
// 오래된 것만 지움: 다른 프로세스가 지금 쓰는 임시 파일(최근 수정)과 이 프로세스 pid 의 파일은 남김
fn remove_stale_temp_files(path: &Path) {
    remove_stale_temp_files_in(path);
    if let Ok(target) = resolve_state_target(path)
        && target != path
    {
        remove_stale_temp_files_in(&target);
    }
}

// 한 파일 이름 기준으로 그 폴더의 오래된 임시 파일 삭제
fn remove_stale_temp_files_in(path: &Path) {
    let (Some(dir), Some(file_name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let prefix = format!("{}.", file_name.to_string_lossy());
    let own_pid = std::process::id().to_string();
    let now = SystemTime::now();
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name
            .strip_prefix(prefix.as_str())
            .and_then(|rest| rest.strip_suffix(TEMP_SUFFIX))
        else {
            continue;
        };
        let is_file = entry.file_type().is_ok_and(|kind| kind.is_file());
        if !is_file || pid.is_empty() || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }

        // 수정 시각을 알 수 없거나 미래이거나 최근이면 살아 있는 저장으로 보고 남김
        let age = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok());
        if pid != own_pid && age.is_some_and(|age| age >= STALE_TEMP_AGE) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

// 같은 폴더의 임시 파일에 쓰고 디스크까지 밀어 낸(sync_all) 뒤 이름을 바꿔 교체 (쓰기 도중 중단돼도 기존 파일은 온전), 실패하면 임시 파일 삭제
// 심볼릭 링크는 먼저 실제 파일로 풀어, 임시 파일은 실제 파일 폴더에 만들고 링크가 아닌 실제 파일 위로 바꿔 링크를 유지
pub(crate) fn write_layout_atomic(path: &Path, text: &str) -> io::Result<()> {
    let target = resolve_state_target(path)?;
    let Some(dir) = target.parent() else {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "no parent directory",
        ));
    };
    fs::create_dir_all(dir)?;
    let file_name = target.file_name().map_or_else(
        || TUI_STATE_FILE_NAME.to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temp_path = dir.join(format!("{file_name}.{}{TEMP_SUFFIX}", std::process::id()));
    let written = fs::File::create(&temp_path).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    if let Err(error) = written {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temp_path, &target) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}

impl AppState {
    // 6. 시작할 때 한 번 복원 -------------------------------------------------
    // 파일이 없으면 영어·기본 배치, 읽을 수 없는 파일이나 맞지 않는 항목(알 수 없는 언어 포함)은 무시하고 알림 한 번
    pub fn load_tui_state(&mut self) {
        remove_stale_temp_files(&self.state_file);
        match read_layout_file(&self.state_file) {
            LayoutRead::Missing => {}
            LayoutRead::Broken(issue) => {
                // 첫 저장이 덮기 전에 원본 보존
                backup_state_file(&self.state_file);
                self.show_toast(
                    &i18n::state_unreadable(self.lang, TUI_STATE_FILE_NAME, &issue),
                    ToastLevel::Info,
                );
            }
            LayoutRead::Loaded(file) => {
                // 예전 [orders]·[ratios] 는 이관 뒤 다시 쓰지 않으므로 첫 저장이 덮기 전에 원본 보존
                if !file.orders.is_empty() || !file.ratios.is_empty() {
                    backup_state_file(&self.state_file);
                }
                let mut skipped =
                    apply_layout_file(&file, &mut self.card_layout, &mut self.column_widths);
                match file
                    .language
                    .as_ref()
                    .map(|value| value.as_str().and_then(Lang::from_code))
                {
                    None => {}
                    Some(Some(lang)) => self.lang = lang,
                    Some(None) => skipped += 1,
                }
                // 차원은 탭이 순환하는 목록 안의 값만 복원
                match file.attribution_dimension.as_ref().map(|value| {
                    value
                        .as_str()
                        .and_then(AttributionDimension::parse)
                        .filter(|dimension| ATTRIBUTION_DIMENSIONS.contains(dimension))
                }) {
                    None => {}
                    Some(Some(dimension)) => self.attribution_dimension = dimension,
                    Some(None) => skipped += 1,
                }
                // 탭 순서는 고정 이름을 겹침 없이 담은 배열만 복원 (나중에 더한 탭만 빠진 이전 파일은 뒤에 붙임)
                match file.tab_order.as_ref().map(|value| {
                    let mut tabs: Vec<ActiveTab> = value
                        .as_array()?
                        .iter()
                        .map(|key| {
                            let key = key.as_str()?;
                            ActiveTab::ALL.into_iter().find(|tab| tab.tab_key() == key)
                        })
                        .collect::<Option<_>>()?;
                    for later in ActiveTab::LATER_TABS {
                        if !tabs.contains(&later) {
                            tabs.push(later);
                        }
                    }
                    <[ActiveTab; TAB_COUNT]>::try_from(tabs)
                        .ok()
                        .filter(|order| ActiveTab::ALL.iter().all(|tab| order.contains(tab)))
                }) {
                    None => {}
                    Some(Some(order)) => self.tab_order = order,
                    Some(None) => skipped += 1,
                }
                if skipped > 0 {
                    self.show_toast(
                        &i18n::state_entries_skipped(self.lang, TUI_STATE_FILE_NAME, skipped),
                        ToastLevel::Info,
                    );
                }
            }
        }
    }

    // 7. 확정 때마다 저장 요청 만들기 -----------------------------------------
    // 확정 이벤트 한 번에 파일 내용 전체를 만들어 저장 요청 하나로 돌려줌 (파일 쓰기는 입력 스레드가 아니라 저장 스레드가 함, 실패는 UiMessage)
    pub fn build_state_save(&self) -> Result<StateSave, String> {
        let file = build_layout_file(
            &self.card_layout,
            &self.column_widths,
            self.lang,
            self.attribution_dimension,
            &self.tab_order,
        );
        toml::to_string(&file)
            .map(|text| StateSave {
                path: self.state_file.clone(),
                text,
            })
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::layout::grid::{DropTarget, apply_drop};
    use crate::tui::test_support::sample_config;
    use chrono::NaiveDate;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DIR_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    // 테스트마다 따로 쓰는 임시 폴더 (끝나면 지움)
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let sequence = DIR_SEQUENCE.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir()
                .join(format!("cm-layout-store-{}-{sequence}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    impl AppState {
        // 테스트 전용: 저장 요청을 만들어 같은 스레드에서 바로 씀 (저장 스레드 없이 파일 결과와 실패 알림을 확인)
        fn save_tui_state(&mut self) {
            let outcome = self.build_state_save().and_then(|save| {
                write_layout_atomic(&save.path, &save.text).map_err(|error| error.to_string())
            });
            if let Err(detail) = outcome {
                self.show_toast(
                    &i18n::state_save_failed(self.lang, &detail),
                    ToastLevel::Error,
                );
            }
        }
    }

    // 기존 한국어 알림 단언을 위해 한국어로 고정
    fn state_in(dir: &TempDir) -> AppState {
        let mut state = AppState::new(
            sample_config(),
            NaiveDate::from_ymd_opt(2026, 11, 15).unwrap(),
        );
        state.state_file = dir.0.join(TUI_STATE_FILE_NAME);
        state.lang = Lang::Ko;
        state
    }

    fn customize(state: &mut AppState) {
        let settings = apply_drop(
            &default_rows(ActiveTab::Settings),
            CardId::SettingsDoctor,
            DropTarget::RowAbove(CardId::SettingsHooks),
        )
        .unwrap();
        state.card_layout.set_grid(ActiveTab::Settings, settings);
        let report = apply_drop(
            &default_rows(ActiveTab::Report),
            CardId::ReportOutline,
            DropTarget::After(CardId::ReportBody),
        )
        .unwrap();
        state.card_layout.set_grid(ActiveTab::Report, report);
        assert!(
            state
                .card_layout
                .restore_grid_ratios("tools_rows_2", &[6_000])
        );
        assert!(
            state
                .card_layout
                .restore_grid_ratios("overview_cols_1", &[1_000, 3_000, 5_000, 7_000, 9_000])
        );
        state.column_widths[CardId::ToolTable as usize][1] = Some(14);
        state.column_widths[CardId::ToolTable as usize][3] = Some(9);
        state.column_widths[CardId::AgentTable as usize][13] = Some(21);
    }

    // 탭 격자의 줄별 카드 (빈 줄 제외)
    fn grid_rows(state: &AppState, tab: ActiveTab) -> Vec<Vec<CardId>> {
        state
            .card_layout
            .grid_of(tab)
            .rows
            .iter()
            .map(|cells| cells.iter().flatten().copied().collect::<Vec<CardId>>())
            .filter(|row| !row.is_empty())
            .collect()
    }

    // 바꾼 설정 격자 (예전 settings_rows = [3, 0, 1, 2])
    fn doctor_first() -> Vec<Vec<CardId>> {
        vec![
            vec![CardId::SettingsDoctor],
            vec![CardId::SettingsHooks],
            vec![CardId::SettingsMcp],
            vec![CardId::SettingsAgents],
        ]
    }

    #[test]
    fn save_then_load_round_trips_orders_ratios_and_column_widths() {
        let dir = TempDir::new();
        let mut saved = state_in(&dir);
        customize(&mut saved);
        saved.save_tui_state();
        assert!(saved.toast.is_none(), "저장 실패 알림: {:?}", saved.toast);

        let text = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        assert!(text.contains("version = 1"), "{text}");
        assert!(text.contains("settings_doctor"), "{text}");
        assert!(text.contains("tool_table"), "{text}");

        let mut loaded = state_in(&dir);
        loaded.load_tui_state();
        assert!(loaded.toast.is_none(), "복원 알림: {:?}", loaded.toast);
        assert_eq!(loaded.card_layout, saved.card_layout);
        assert_eq!(loaded.column_widths, saved.column_widths);
        assert_eq!(grid_rows(&loaded, ActiveTab::Settings), doctor_first());
        assert!(loaded.card_layout.grid_ratio_entries().contains(&(
            "overview_cols_1".to_string(),
            vec![1_000, 3_000, 5_000, 7_000, 9_000]
        )));
        assert_eq!(
            loaded.column_widths[CardId::ToolTable as usize][..4],
            [None, Some(14), None, Some(9)]
        );
    }

    #[test]
    fn saved_file_text_keys_entries_by_stable_names_with_the_version_first() {
        let dir = TempDir::new();
        let mut state = state_in(&dir);
        customize(&mut state);
        state.save_tui_state();
        let text = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        let expected = "version = 1\n\
            language = \"ko\"\n\
            \n\
            [column_widths]\n\
            agent_table = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 21]\n\
            tool_table = [0, 14, 0, 9, 0, 0, 0, 0]\n\
            \n\
            [grids]\n\
            report = [[\"report_body\", \"report_outline\"]]\n\
            settings = [[\"settings_doctor\"], [\"settings_hooks\"], [\"settings_mcp\"], [\"settings_agents\"]]\n\
            \n\
            [grid_ratios]\n\
            overview_cols_1 = [1000, 3000, 5000, 7000, 9000]\n\
            tools_rows_2 = [6000]\n";
        assert_eq!(text, expected);
    }

    #[test]
    fn default_layout_writes_only_the_version_and_reset_overwrites_custom_entries() {
        let dir = TempDir::new();
        let mut state = state_in(&dir);
        customize(&mut state);
        state.save_tui_state();

        for tab in ActiveTab::ALL {
            state.card_layout.reset_tab(tab);
        }
        state.column_widths = [[None; MAX_TABLE_COLUMNS]; CARD_COUNT];
        state.save_tui_state();
        let text = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        assert!(text.contains("version = 1"), "{text}");
        assert!(!text.contains("settings_doctor"), "{text}");
        assert!(!text.contains("tool_table"), "{text}");

        let mut loaded = state_in(&dir);
        loaded.load_tui_state();
        assert!(loaded.toast.is_none());
        assert_eq!(loaded.card_layout, CardLayout::default());
    }

    #[test]
    fn tab_order_round_trips_by_stable_names_and_only_a_full_permutation_is_restored() {
        // 1. 기본 순서는 기록하지 않고, 바꾼 순서는 최상위 이름 배열로 표보다 앞에 기록
        let dir = TempDir::new();
        let mut state = state_in(&dir);
        state.save_tui_state();
        let text = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        assert!(!text.contains("tab_order"), "{text}");

        state.tab_order.swap(0, 7);
        customize(&mut state);
        state.save_tui_state();
        let text = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        let expected = "tab_order = [\"projects\", \"tools\", \"hooks\", \"models\", \"agents\", \"settings\", \"report\", \"overview\", \"skills\"]";
        assert!(text.contains(expected), "{text}");
        assert!(
            text.find("tab_order") < text.find("[column_widths]"),
            "{text}"
        );

        let mut loaded = state_in(&dir);
        loaded.load_tui_state();
        assert!(loaded.toast.is_none(), "{:?}", loaded.toast);
        assert_eq!(loaded.tab_order, state.tab_order);

        // 1-1. 스킬 탭 이전에 저장한 8개 순서는 그대로 두고 새 탭을 끝에 붙임 (알림 없음)
        fs::write(
            dir.0.join(TUI_STATE_FILE_NAME),
            "version = 1\ntab_order = [\"projects\", \"tools\", \"hooks\", \"models\", \"agents\", \"settings\", \"report\", \"overview\"]\n",
        )
        .unwrap();
        let mut legacy = state_in(&dir);
        legacy.load_tui_state();
        assert!(legacy.toast.is_none(), "{:?}", legacy.toast);
        assert_eq!(legacy.tab_order, state.tab_order);

        // 2. 겹치거나 빠지거나 모르는 이름이면 기본 순서 유지, 건너뛴 항목 알림 한 번
        for broken in [
            "[\"tools\", \"tools\", \"hooks\", \"models\", \"agents\", \"settings\", \"report\", \"projects\"]",
            "[\"tools\", \"overview\"]",
            "[\"tools\", \"overview\", \"hooks\", \"models\", \"agents\", \"settings\", \"report\", \"gate\"]",
            "\"tools\"",
        ] {
            fs::write(
                dir.0.join(TUI_STATE_FILE_NAME),
                format!("version = 1\ntab_order = {broken}\n"),
            )
            .unwrap();
            let mut loaded = state_in(&dir);
            loaded.load_tui_state();
            assert_eq!(loaded.tab_order, ActiveTab::ALL, "{broken}");
            assert!(loaded.toast.is_some(), "{broken}");
        }
    }

    #[test]
    fn missing_file_keeps_defaults_without_a_toast() {
        let dir = TempDir::new();
        let mut state = state_in(&dir);
        state.load_tui_state();
        assert!(state.toast.is_none());
        assert_eq!(state.card_layout, CardLayout::default());
        assert!(!dir.0.join(TUI_STATE_FILE_NAME).exists());
    }

    #[test]
    fn malformed_file_is_ignored_with_one_toast_and_never_panics() {
        for body in [
            "이건 toml 이 아님 = = [",
            "version = 2\n[orders]\nsettings_rows = [3, 0, 1, 2]\n",
            "orders = 5\n",
            "version = \"1\"\n",
        ] {
            let dir = TempDir::new();
            fs::write(dir.0.join(TUI_STATE_FILE_NAME), body).unwrap();
            let mut state = state_in(&dir);
            state.load_tui_state();
            let toast = state
                .toast
                .as_ref()
                .unwrap_or_else(|| panic!("알림 없음: {body}"));
            assert!(toast.text.contains("읽기 실패"), "{}", toast.text);
            assert_eq!(toast.level, ToastLevel::Info);
            assert_eq!(state.card_layout, CardLayout::default(), "{body}");
        }
    }

    // 카드가 늘기 전(3칸) 배치 파일: 모델 순서는 타이머를 마지막 줄로, 비율은 새 경계를 구간 끝에 둔 격자 비율로, 다음 저장은 격자 형식
    #[test]
    fn a_three_card_file_from_before_the_new_cards_migrates_without_a_skip_toast() {
        let dir = TempDir::new();
        let body = "version = 1\n\
            [orders]\n\
            models_rows = [2, 0, 1]\n\
            overview_trend_cols = [1, 0, 2]\n\
            [ratios]\n\
            models_rows = [2000, 6000]\n\
            overview_trend_cols = [3000, 6500]\n";
        fs::write(dir.0.join(TUI_STATE_FILE_NAME), body).unwrap();
        let mut state = state_in(&dir);
        state.load_tui_state();

        assert!(state.toast.is_none(), "{:?}", state.toast);
        assert_eq!(
            grid_rows(&state, ActiveTab::Models),
            vec![
                vec![CardId::CacheBreaks],
                vec![CardId::ModelTable],
                vec![CardId::ModelDetail],
                vec![CardId::CacheTimer],
            ]
        );
        // 추이 순서 [1, 0, 2] 는 요청 추이·토큰 추이 순서라 기본 격자, 개요 비율은 버림
        assert_eq!(
            state.card_layout.grid_of(ActiveTab::Overview),
            default_rows(ActiveTab::Overview)
        );
        assert_eq!(
            state.card_layout.grid_ratio_entries(),
            [("models_rows_4".to_string(), vec![2_000, 6_000, 10_000])]
        );

        // 다음 저장은 격자 형식으로 쓰고, 다시 읽어도 같음
        state.save_tui_state();
        let text = fs::read_to_string(&state.state_file).unwrap();
        assert!(
            text.contains("models = [[\"cache_breaks\"], [\"model_table\"], [\"model_detail\"], [\"cache_timer\"]]"),
            "{text}"
        );
        assert!(
            text.contains("models_rows_4 = [2000, 6000, 10000]"),
            "{text}"
        );
        assert!(
            !text.contains("[orders]") && !text.contains("[ratios]"),
            "{text}"
        );
        let mut reloaded = state_in(&dir);
        reloaded.load_tui_state();
        assert!(reloaded.toast.is_none(), "{:?}", reloaded.toast);
        assert_eq!(reloaded.card_layout, state.card_layout);
    }

    #[test]
    fn mismatched_entries_are_skipped_while_valid_entries_still_apply() {
        let dir = TempDir::new();
        let body = "version = 1\n\
            [orders]\n\
            settings_rows = [3, 0, 1, 2]\n\
            tools_rows = [0, 1, 2]\n\
            report_cols = [1, 1]\n\
            overview_rows = [0, 1, 2, 3]\n\
            unknown_group = [0, 1]\n\
            [ratios]\n\
            models_rows = [2000]\n\
            tools_rows = [6000]\n\
            [column_widths]\n\
            tool_table = [0, 12]\n\
            hook_table = [0, 0, 0, 0, 0, 0, 0, 20, 0]\n\
            agent_table = [-5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]\n\
            overview_chart = [1]\n";
        fs::write(dir.0.join(TUI_STATE_FILE_NAME), body).unwrap();
        let mut state = state_in(&dir);
        state.load_tui_state();

        // 맞는 항목만 반영: 설정 순서, 도구 비율, 훅 열 폭
        assert_eq!(grid_rows(&state, ActiveTab::Settings), doctor_first());
        assert_eq!(
            state.card_layout.grid_ratio_entries(),
            [("tools_rows_2".to_string(), vec![6_000])]
        );
        assert_eq!(state.column_widths[CardId::HookTable as usize][7], Some(20));

        // 예전 키의 길이·중복이 맞지 않는 값과 개요 키는 알림 없이 버림
        for tab in [ActiveTab::Tools, ActiveTab::Report, ActiveTab::Overview] {
            assert_eq!(state.card_layout.grid_of(tab), default_rows(tab), "{tab:?}");
        }
        assert!(
            state.column_widths[CardId::ToolTable as usize]
                .iter()
                .all(Option::is_none)
        );
        assert!(
            state.column_widths[CardId::AgentTable as usize]
                .iter()
                .all(Option::is_none)
        );

        // 알림은 한 번, 건너뛴 항목 수 표시 (모르는 그룹 키와 열 폭 세 항목)
        let toast = state.toast.as_ref().unwrap();
        assert!(toast.text.contains("일부 항목 무시 4건"), "{}", toast.text);
    }

    // 예전 [orders]·[ratios] 만 있는 파일: 격자·격자 비율로 이관, 알림 없음, 다시 쓰지 않음, .bak 은 첫 저장 전 한 번
    #[test]
    fn a_legacy_orders_and_ratios_file_migrates_into_grids_with_one_backup() {
        let dir = TempDir::new();
        let path = dir.0.join(TUI_STATE_FILE_NAME);
        let body = r#"version = 1
language = "ko"
attribution_dimension = "agent"
tab_order = ["overview", "tools", "hooks", "skills", "models", "agents", "projects", "settings", "report"]
[orders]
models_rows = [0, 2, 1, 3]
overview_trend_cols = [0, 3, 2, 1]
settings_rows = [2, 0, 3, 1]
[ratios]
agents_rows = [3929]
hooks_rows = [6923]
models_rows = [2321, 5893, 7679]
overview_rows = [702, 1579, 3333]
overview_trend_cols = [1107, 4760, 7159]
projects_rows = [3929, 6250]
report_cols = [959]
settings_rows = [3077, 4615, 7692]
skills_rows = [6607]
tools_rows = [8036]
[column_widths]
event_feed = [18, 20, 46, 0, 0, 21, 8, 0]
hook_table = [14, 0, 0, 0, 0, 0, 0, 0]
model_table = [17, 0, 0, 0, 0, 12, 0, 172, 0, 65, 10, 26, 0]
tool_table = [0, 7, 0, 0, 0, 0]
"#;
        fs::write(&path, body).unwrap();
        let mut state = fresh_state_in(&dir);
        state.load_tui_state();

        // 1. 알림 없음, 언어·귀속 차원·탭 순서·열 폭 유지
        assert!(state.toast.is_none(), "{:?}", state.toast);
        assert_eq!(state.lang, Lang::Ko);
        assert_eq!(
            Some(state.attribution_dimension),
            AttributionDimension::parse("agent")
        );
        assert_eq!(
            state.tab_order,
            [
                ActiveTab::Overview,
                ActiveTab::Tools,
                ActiveTab::Hooks,
                ActiveTab::Skills,
                ActiveTab::Models,
                ActiveTab::Agents,
                ActiveTab::Projects,
                ActiveTab::Settings,
                ActiveTab::Report,
            ]
        );
        for (card, expected) in [
            (CardId::EventFeed, &[18, 20, 46, 0, 0, 21, 8, 0][..]),
            (CardId::HookTable, &[14, 0, 0, 0, 0, 0, 0, 0][..]),
            (
                CardId::ModelTable,
                &[17, 0, 0, 0, 0, 12, 0, 172, 0, 65, 10, 26, 0][..],
            ),
            (CardId::ToolTable, &[0, 7, 0, 0, 0, 0][..]),
        ] {
            let widths: Vec<u16> = state.column_widths[card as usize][..expected.len()]
                .iter()
                .map(|width| width.unwrap_or(0))
                .collect();
            assert_eq!(widths, expected, "{card:?}");
        }

        // 2. 격자: 줄 i = 예전 카드 order[i], 추이 줄은 토큰 추이·요청 추이 순서
        assert_eq!(
            grid_rows(&state, ActiveTab::Models),
            vec![
                vec![CardId::ModelTable],
                vec![CardId::CacheBreaks],
                vec![CardId::ModelDetail],
                vec![CardId::CacheTimer],
            ]
        );
        assert_eq!(
            grid_rows(&state, ActiveTab::Settings),
            vec![
                vec![CardId::SettingsAgents],
                vec![CardId::SettingsHooks],
                vec![CardId::SettingsDoctor],
                vec![CardId::SettingsMcp],
            ]
        );
        let trend_row = grid_rows(&state, ActiveTab::Overview)
            .into_iter()
            .find(|row| row.contains(&CardId::TrendRequests))
            .unwrap();
        assert_eq!(trend_row, [CardId::TrendTokens, CardId::TrendRequests]);

        // 3. 격자 비율: 값 그대로, 개요 비율은 버림
        let mut entries = state.card_layout.grid_ratio_entries();
        entries.sort();
        let mut expected: Vec<(String, Vec<u16>)> = [
            ("tools_rows_2", vec![8036]),
            ("hooks_rows_2", vec![6923]),
            ("skills_rows_2", vec![6607]),
            ("agents_rows_2", vec![3929]),
            ("projects_rows_3", vec![3929, 6250]),
            ("models_rows_4", vec![2321, 5893, 7679]),
            ("settings_rows_4", vec![3077, 4615, 7692]),
            ("report_cols_0", vec![959]),
        ]
        .into_iter()
        .map(|(key, values)| (key.to_string(), values))
        .collect();
        expected.sort();
        assert_eq!(entries, expected);

        // 4. 첫 저장: 예전 표·키 없음, .bak 은 원본
        state.save_tui_state();
        assert!(state.toast.is_none(), "{:?}", state.toast);
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("[orders]"), "{text}");
        assert!(!text.contains("[ratios]"), "{text}");
        for key in [
            "tools_rows",
            "hooks_rows",
            "models_rows",
            "agents_rows",
            "settings_rows",
            "report_cols",
            "projects_rows",
            "skills_rows",
            "overview_rows",
            "overview_trend_cols",
        ] {
            assert!(!text.contains(&format!("{key} = ")), "{key}: {text}");
        }
        assert_eq!(fs::read_to_string(backup_of(&dir)).unwrap(), body);

        // 5. 두 번째 읽기·저장: 같은 배치, .bak 그대로
        let mut next_run = fresh_state_in(&dir);
        next_run.load_tui_state();
        assert!(next_run.toast.is_none(), "{:?}", next_run.toast);
        assert_eq!(next_run.card_layout, state.card_layout);
        next_run.save_tui_state();
        assert_eq!(fs::read_to_string(backup_of(&dir)).unwrap(), body);
    }

    // 언어 저장·복원 테스트용: 기본 언어(영어)로 만든 상태
    fn fresh_state_in(dir: &TempDir) -> AppState {
        let mut state = AppState::new(
            sample_config(),
            NaiveDate::from_ymd_opt(2026, 11, 15).unwrap(),
        );
        state.state_file = dir.0.join(TUI_STATE_FILE_NAME);
        state
    }

    #[test]
    fn language_round_trips_through_the_state_file_next_to_the_version() {
        let dir = TempDir::new();
        let mut saved = fresh_state_in(&dir);
        assert_eq!(saved.lang, Lang::En);
        saved.save_tui_state();
        let text = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        assert!(
            text.starts_with("version = 1\nlanguage = \"en\"\n"),
            "{text}"
        );
        assert!(
            !text.contains("_rows") && !text.contains("_table"),
            "{text}"
        );

        saved.perform_action(crate::tui::cards::UiAction::ToggleLanguage);
        saved.save_tui_state();
        let text = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        assert!(
            text.starts_with("version = 1\nlanguage = \"ko\"\n"),
            "{text}"
        );

        let mut loaded = fresh_state_in(&dir);
        assert_eq!(loaded.lang, Lang::En);
        loaded.load_tui_state();
        assert_eq!(loaded.lang, Lang::Ko);
        assert!(loaded.toast.is_none(), "{:?}", loaded.toast);
    }

    #[test]
    fn attribution_dimension_is_saved_only_when_changed_and_restored_on_load() {
        // 1. 기본 차원(스킬)은 기록하지 않음
        let dir = TempDir::new();
        let mut saved = fresh_state_in(&dir);
        saved.save_tui_state();
        let text = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        assert!(!text.contains("attribution_dimension"), "{text}");

        // 2. d 키로 바꾼 차원은 저장 요청이 나가고 다음 시작에 복원
        saved.tab = crate::tui::state::ActiveTab::Projects;
        let command = saved.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('d'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(command, crate::tui::state::UiCommand::SaveState);
        assert_eq!(saved.attribution_dimension, ATTRIBUTION_DIMENSIONS[1]);
        saved.save_tui_state();
        let text = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        assert!(
            text.contains("attribution_dimension = \"mcp_server\""),
            "{text}"
        );
        let mut loaded = fresh_state_in(&dir);
        loaded.load_tui_state();
        assert_eq!(loaded.attribution_dimension, ATTRIBUTION_DIMENSIONS[1]);
        assert!(loaded.toast.is_none(), "{:?}", loaded.toast);

        // 3. 모르는 값과 탭 목록 밖 차원(query_source)은 무시하고 알림 한 번
        for value in ["\"bogus\"", "\"query_source\"", "3"] {
            fs::write(
                dir.0.join(TUI_STATE_FILE_NAME),
                format!("version = 1\nattribution_dimension = {value}\n"),
            )
            .unwrap();
            let mut ignored = fresh_state_in(&dir);
            ignored.load_tui_state();
            assert_eq!(
                ignored.attribution_dimension, ATTRIBUTION_DIMENSIONS[0],
                "{value}"
            );
            assert!(ignored.toast.is_some(), "{value}");
        }
    }

    #[test]
    fn language_and_layout_save_together_and_reset_keeps_the_language() {
        let dir = TempDir::new();
        let mut state = fresh_state_in(&dir);
        customize(&mut state);
        state.lang = Lang::Ko;
        state.save_tui_state();

        // 배치 초기화 뒤 저장해도 언어 키는 유지
        for tab in ActiveTab::ALL {
            state.card_layout.reset_tab(tab);
        }
        state.column_widths = [[None; MAX_TABLE_COLUMNS]; CARD_COUNT];
        state.save_tui_state();
        let text = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        assert!(text.contains("language = \"ko\""), "{text}");
        assert!(!text.contains("settings_doctor"), "{text}");

        let mut loaded = fresh_state_in(&dir);
        loaded.load_tui_state();
        assert_eq!(loaded.lang, Lang::Ko);
        assert_eq!(loaded.card_layout, CardLayout::default());
    }

    #[test]
    fn a_file_without_a_language_key_keeps_english() {
        let dir = TempDir::new();
        fs::write(
            dir.0.join(TUI_STATE_FILE_NAME),
            "version = 1\n[orders]\nsettings_rows = [3, 0, 1, 2]\n",
        )
        .unwrap();
        let mut state = fresh_state_in(&dir);
        state.load_tui_state();
        assert_eq!(state.lang, Lang::En);
        assert!(state.toast.is_none(), "{:?}", state.toast);
        assert_eq!(grid_rows(&state, ActiveTab::Settings), doctor_first());
    }

    #[test]
    fn an_unknown_language_value_is_ignored_with_one_info_toast_and_english() {
        for value in ["\"fr\"", "\"EN\"", "\"\"", "5", "[\"ko\"]"] {
            let dir = TempDir::new();
            let body = format!(
                "version = 1\nlanguage = {value}\n[orders]\nsettings_rows = [3, 0, 1, 2]\n"
            );
            fs::write(dir.0.join(TUI_STATE_FILE_NAME), body).unwrap();
            let mut state = fresh_state_in(&dir);
            state.load_tui_state();

            assert_eq!(state.lang, Lang::En, "{value}");
            let toast = state
                .toast
                .as_ref()
                .unwrap_or_else(|| panic!("no toast: {value}"));
            assert_eq!(toast.level, ToastLevel::Info, "{value}");
            assert_eq!(
                toast.text, "tui.toml skipped 1 entries, using defaults",
                "{value}"
            );
            // 다른 항목은 그대로 적용
            assert_eq!(
                grid_rows(&state, ActiveTab::Settings),
                doctor_first(),
                "{value}"
            );
        }
    }

    #[test]
    fn an_unknown_language_in_a_korean_session_keeps_the_current_language() {
        let dir = TempDir::new();
        fs::write(
            dir.0.join(TUI_STATE_FILE_NAME),
            "version = 1\nlanguage = \"fr\"\n",
        )
        .unwrap();
        let mut state = state_in(&dir);
        state.load_tui_state();
        assert_eq!(state.lang, Lang::Ko);
        assert!(
            state
                .toast
                .as_ref()
                .unwrap()
                .text
                .contains("일부 항목 무시 1건")
        );
    }

    #[test]
    fn state_file_issues_follow_the_loaded_language() {
        let dir = TempDir::new();
        fs::write(dir.0.join(TUI_STATE_FILE_NAME), "version = 2\n").unwrap();
        let mut state = fresh_state_in(&dir);
        state.load_tui_state();
        assert_eq!(
            state.toast.as_ref().unwrap().text,
            "tui.toml read failed, using defaults (unsupported version 2)"
        );
        assert_eq!(state.lang, Lang::En);
    }

    #[test]
    fn atomic_write_replaces_the_file_and_leaves_no_temp_file() {
        let dir = TempDir::new();
        let path = dir.0.join(TUI_STATE_FILE_NAME);
        write_layout_atomic(&path, "version = 1\n").unwrap();
        write_layout_atomic(&path, "version = 1\n# 둘째\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "version = 1\n# 둘째\n");

        let names: Vec<String> = fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [TUI_STATE_FILE_NAME.to_string()]);
    }

    #[test]
    fn write_failure_shows_an_error_toast_and_removes_the_temp_file() {
        let dir = TempDir::new();
        // 대상 경로가 폴더라 이름 바꾸기가 실패함
        fs::create_dir_all(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        let mut state = state_in(&dir);
        customize(&mut state);
        state.save_tui_state();

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.level, ToastLevel::Error);
        assert!(
            toast.text.starts_with("화면 상태 저장 실패"),
            "{}",
            toast.text
        );
        let leftovers: Vec<String> = fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn missing_state_dir_is_created_on_first_save() {
        let dir = TempDir::new();
        let mut state = state_in(&dir);
        state.state_file = dir
            .0
            .join("nested")
            .join("claude-monitor")
            .join(TUI_STATE_FILE_NAME);
        customize(&mut state);
        state.save_tui_state();
        assert!(state.toast.is_none(), "{:?}", state.toast);
        assert!(state.state_file.is_file());
    }

    // 파일 심볼릭 링크 만들기 (권한이 없으면 오류를 돌려주고 테스트가 사유를 남긴 채 건너뜀)
    #[cfg(unix)]
    fn make_file_symlink(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn make_file_symlink(target: &Path, link: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    #[cfg(not(any(unix, windows)))]
    fn make_file_symlink(_target: &Path, _link: &Path) -> io::Result<()> {
        Err(io::Error::new(ErrorKind::Unsupported, "no symlink support"))
    }

    // 파일 수정 시각을 시작 정리 기준(10분)보다 오래전으로 돌림
    fn age_file(path: &Path) {
        let old = SystemTime::now() - STALE_TEMP_AGE - Duration::from_secs(60);
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }

    fn file_names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn saving_through_a_symlink_keeps_the_link_and_writes_the_target() {
        let dir = TempDir::new();
        let synced = dir.0.join("synced");
        let home = dir.0.join("home");
        fs::create_dir_all(&synced).unwrap();
        fs::create_dir_all(&home).unwrap();
        let target = synced.join(TUI_STATE_FILE_NAME);
        let link = home.join(TUI_STATE_FILE_NAME);
        fs::write(&target, "version = 1\n").unwrap();
        if let Err(error) = make_file_symlink(&target, &link) {
            eprintln!("skip symlink save test: symlink not created ({error})");
            return;
        }

        // 시작: 링크 폴더와 실제 파일 폴더의 남은 임시 파일을 모두 정리
        let stale_target = synced.join(format!("{TUI_STATE_FILE_NAME}.4242{TEMP_SUFFIX}"));
        let stale_link = home.join(format!("{TUI_STATE_FILE_NAME}.4243{TEMP_SUFFIX}"));
        fs::write(&stale_target, "x").unwrap();
        fs::write(&stale_link, "x").unwrap();
        age_file(&stale_target);
        age_file(&stale_link);
        let mut state = state_in(&dir);
        state.state_file = link.clone();
        state.load_tui_state();
        assert!(!stale_target.exists() && !stale_link.exists());
        assert!(state.toast.is_none(), "{:?}", state.toast);

        // 저장: 링크는 그대로 같은 대상을 가리키고 실제 파일에 새 내용
        customize(&mut state);
        state.save_tui_state();
        assert!(state.toast.is_none(), "{:?}", state.toast);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::canonicalize(fs::read_link(&link).unwrap()).unwrap(),
            fs::canonicalize(&target).unwrap()
        );
        let saved = fs::read_to_string(&target).unwrap();
        assert!(saved.contains("settings_doctor"), "{saved}");
        assert_eq!(fs::read_to_string(&link).unwrap(), saved);
        assert_eq!(file_names_in(&synced), [TUI_STATE_FILE_NAME.to_string()]);
        assert_eq!(file_names_in(&home), [TUI_STATE_FILE_NAME.to_string()]);

        // 다음 시작이 링크를 통해 같은 배치를 복원
        let mut next_run = state_in(&dir);
        next_run.state_file = link.clone();
        next_run.load_tui_state();
        assert_eq!(next_run.card_layout, state.card_layout);
    }

    #[test]
    fn saving_through_a_dangling_symlink_creates_the_target_and_keeps_the_link() {
        let dir = TempDir::new();
        let synced = dir.0.join("synced");
        let home = dir.0.join("home");
        fs::create_dir_all(&synced).unwrap();
        fs::create_dir_all(&home).unwrap();
        let target = synced.join(TUI_STATE_FILE_NAME);
        let link = home.join(TUI_STATE_FILE_NAME);
        if let Err(error) = make_file_symlink(&target, &link) {
            eprintln!("skip dangling symlink save test: symlink not created ({error})");
            return;
        }

        let mut state = state_in(&dir);
        state.state_file = link.clone();
        customize(&mut state);
        state.save_tui_state();
        assert!(state.toast.is_none(), "{:?}", state.toast);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::read_to_string(&target)
                .unwrap()
                .contains("settings_doctor")
        );
        assert_eq!(file_names_in(&synced), [TUI_STATE_FILE_NAME.to_string()]);
    }

    // 끊어진 링크 사슬: link0 -> link1 -> ... -> link<count-1> -> target(없음), 반환 = (처음 링크, 사슬 모든 링크)
    fn make_dangling_chain(
        dir: &Path,
        count: usize,
        target: &Path,
    ) -> Option<(PathBuf, Vec<PathBuf>)> {
        let links: Vec<PathBuf> = (0..count)
            .map(|index| dir.join(format!("link{index}")))
            .collect();
        for (index, link) in links.iter().enumerate() {
            let value = links.get(index + 1).map_or(target, PathBuf::as_path);
            if let Err(error) = make_file_symlink(value, link) {
                eprintln!("skip symlink chain test: symlink not created ({error})");
                return None;
            }
        }
        Some((links[0].clone(), links))
    }

    #[test]
    fn saving_through_a_multi_level_dangling_chain_creates_the_final_target_and_keeps_every_link() {
        let dir = TempDir::new();
        let synced = dir.0.join("synced");
        let links = dir.0.join("links");
        fs::create_dir_all(&synced).unwrap();
        fs::create_dir_all(&links).unwrap();
        let target = synced.join(TUI_STATE_FILE_NAME);
        let Some((first, chain)) = make_dangling_chain(&links, 5, &target) else {
            return;
        };

        let mut state = state_in(&dir);
        state.state_file = first;
        customize(&mut state);
        state.save_tui_state();
        assert!(state.toast.is_none(), "{:?}", state.toast);

        // 모든 중간 링크가 그대로 링크이고, 실제 파일은 마지막 대상 자리에만 생김
        for link in &chain {
            assert!(
                fs::symlink_metadata(link).unwrap().file_type().is_symlink(),
                "{}",
                link.display()
            );
        }
        assert!(
            fs::read_to_string(&target)
                .unwrap()
                .contains("settings_doctor")
        );
        assert_eq!(file_names_in(&synced), [TUI_STATE_FILE_NAME.to_string()]);
        assert_eq!(file_names_in(&links).len(), chain.len());
    }

    #[test]
    fn a_link_chain_deeper_than_the_limit_fails_the_save_instead_of_overwriting_a_link() {
        let dir = TempDir::new();
        let links = dir.0.join("links");
        fs::create_dir_all(&links).unwrap();
        let target = dir.0.join("never").join(TUI_STATE_FILE_NAME);
        let Some((first, chain)) = make_dangling_chain(&links, MAX_LINK_DEPTH + 4, &target) else {
            return;
        };

        let error = write_layout_atomic(&first, "version = 1\n").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(!target.exists());
        for link in &chain {
            assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
        }
    }

    #[test]
    fn a_column_width_above_the_cap_skips_that_entry_and_counts_one() {
        let dir = TempDir::new();
        let body = "version = 1\n\
            [column_widths]\n\
            tool_table = [0, 65535, 0, 0, 0, 0, 0, 0]\n\
            hook_table = [0, 0, 0, 0, 0, 0, 0, 20, 0]\n";
        fs::write(dir.0.join(TUI_STATE_FILE_NAME), body).unwrap();
        let mut state = state_in(&dir);
        state.load_tui_state();

        assert!(
            state.column_widths[CardId::ToolTable as usize]
                .iter()
                .all(Option::is_none)
        );
        assert_eq!(state.column_widths[CardId::HookTable as usize][7], Some(20));
        let toast = state.toast.as_ref().unwrap();
        assert!(toast.text.contains("일부 항목 무시 1건"), "{}", toast.text);
    }

    #[test]
    fn the_column_width_cap_itself_is_accepted_and_one_cell_above_is_skipped() {
        let mut layout = CardLayout::default();
        let mut widths = [[None; MAX_TABLE_COLUMNS]; CARD_COUNT];
        let build = |value: i64| {
            let mut file = LayoutFile {
                version: LAYOUT_VERSION,
                ..LayoutFile::default()
            };
            file.column_widths
                .insert("tool_table".to_string(), vec![0, value, 0, 0, 0, 0, 0, 0]);
            file
        };
        let cap = i64::from(MAX_COLUMN_WIDTH);
        assert_eq!(apply_layout_file(&build(cap), &mut layout, &mut widths), 0);
        assert_eq!(
            widths[CardId::ToolTable as usize][1],
            Some(MAX_COLUMN_WIDTH)
        );
        assert_eq!(
            apply_layout_file(&build(cap + 1), &mut layout, &mut widths),
            1
        );
    }

    fn backup_of(dir: &TempDir) -> PathBuf {
        dir.0.join(format!("{TUI_STATE_FILE_NAME}{BACKUP_SUFFIX}"))
    }

    #[test]
    fn an_unreadable_file_is_copied_to_bak_before_the_first_save_overwrites_it() {
        for body in [
            "이건 toml 이 아님 = = [",
            "version = 2\n[orders]\nsettings_rows = [3, 0, 1, 2]\n",
            "version = \"1\"\n",
        ] {
            let dir = TempDir::new();
            fs::write(dir.0.join(TUI_STATE_FILE_NAME), body).unwrap();
            // 이전 백업 덮어쓰기 대상
            fs::write(backup_of(&dir), "old backup").unwrap();
            let mut state = state_in(&dir);
            state.load_tui_state();
            assert_eq!(fs::read_to_string(backup_of(&dir)).unwrap(), body);

            // 첫 저장이 원본을 덮어도 백업은 그대로
            customize(&mut state);
            state.save_tui_state();
            let saved = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
            assert!(saved.contains("version = 1"), "{saved}");
            assert_eq!(fs::read_to_string(backup_of(&dir)).unwrap(), body);

            // 새로 시작해 정상 파일을 읽으면 백업을 다시 만들지 않음
            let mut next_run = state_in(&dir);
            next_run.load_tui_state();
            assert_eq!(fs::read_to_string(backup_of(&dir)).unwrap(), body);
        }
    }

    #[test]
    fn missing_or_valid_files_make_no_bak() {
        let dir = TempDir::new();
        let mut state = state_in(&dir);
        state.load_tui_state();
        assert!(!backup_of(&dir).exists());

        customize(&mut state);
        state.save_tui_state();
        let mut next_run = state_in(&dir);
        next_run.load_tui_state();
        assert!(next_run.toast.is_none(), "{:?}", next_run.toast);
        assert!(!backup_of(&dir).exists());
    }

    #[test]
    fn atomic_write_goes_through_a_created_temp_file_and_fails_cleanly_when_it_cannot_be_created() {
        let dir = TempDir::new();
        let path = dir.0.join(TUI_STATE_FILE_NAME);
        write_layout_atomic(&path, "version = 1\n").unwrap();

        // 임시 파일 자리를 폴더가 차지하면 생성 실패, 기존 파일은 그대로
        let temp_dir = dir.0.join(format!(
            "{TUI_STATE_FILE_NAME}.{}{TEMP_SUFFIX}",
            std::process::id()
        ));
        fs::create_dir_all(&temp_dir).unwrap();
        assert!(write_layout_atomic(&path, "version = 1\n# 새 내용\n").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "version = 1\n");
    }

    #[test]
    fn startup_removes_stale_leftover_temp_files_but_keeps_live_and_unrelated_files() {
        let dir = TempDir::new();
        let leftovers = [
            format!("{TUI_STATE_FILE_NAME}.123{TEMP_SUFFIX}"),
            format!("{TUI_STATE_FILE_NAME}.99999{TEMP_SUFFIX}"),
        ];
        let keepers = [
            format!("{TUI_STATE_FILE_NAME}{BACKUP_SUFFIX}"),
            format!("{TUI_STATE_FILE_NAME}.abc{TEMP_SUFFIX}"),
            format!("{TUI_STATE_FILE_NAME}.{TEMP_SUFFIX}"),
            format!("config.toml.123{TEMP_SUFFIX}"),
            "notes.txt".to_string(),
        ];
        // 다른 프로세스가 방금 쓰기 시작한 임시 파일(최근 수정)과 이 프로세스 pid 의 파일은 오래됐어도 남김
        let live_other = format!("{TUI_STATE_FILE_NAME}.4242{TEMP_SUFFIX}");
        let own = format!("{TUI_STATE_FILE_NAME}.{}{TEMP_SUFFIX}", std::process::id());
        for name in leftovers.iter().chain(keepers.iter()) {
            fs::write(dir.0.join(name), "x").unwrap();
        }
        fs::write(dir.0.join(&live_other), "x").unwrap();
        fs::write(dir.0.join(&own), "x").unwrap();
        for name in leftovers.iter().chain([&own]) {
            age_file(&dir.0.join(name));
        }
        // 폴더는 이름이 맞아도 지우지 않음
        let temp_dir = dir.0.join(format!("{TUI_STATE_FILE_NAME}.7{TEMP_SUFFIX}"));
        fs::create_dir_all(&temp_dir).unwrap();

        let mut state = state_in(&dir);
        state.load_tui_state();
        for name in &leftovers {
            assert!(!dir.0.join(name).exists(), "{name}");
        }
        for name in keepers.iter().chain([&live_other, &own]) {
            assert!(dir.0.join(name).exists(), "{name}");
        }
        assert!(temp_dir.is_dir());
        assert!(state.toast.is_none(), "{:?}", state.toast);

        // 없는 폴더는 오류 없이 지나감
        state.state_file = dir.0.join("nowhere").join(TUI_STATE_FILE_NAME);
        state.load_tui_state();
        assert!(state.toast.is_none(), "{:?}", state.toast);
    }

    #[test]
    fn a_new_state_defaults_to_the_data_dir_file_and_save_load_follow_state_file_only() {
        let data = TempDir::new();
        let home = TempDir::new();
        let mut config = sample_config();
        config.data_dir = data.0.clone();
        let mut state = AppState::new(config, NaiveDate::from_ymd_opt(2026, 11, 15).unwrap());
        assert_eq!(state.state_file, data.0.join(TUI_STATE_FILE_NAME));

        // 실행 진입점이 홈 공용 폴더로 바꾼 뒤에는 데이터 폴더에 아무것도 쓰지 않음
        state.state_file = home.0.join(TUI_STATE_FILE_NAME);
        customize(&mut state);
        state.save_tui_state();
        assert!(state.toast.is_none(), "{:?}", state.toast);
        assert!(home.0.join(TUI_STATE_FILE_NAME).is_file());
        assert!(!data.0.join(TUI_STATE_FILE_NAME).exists());

        let mut loaded = state_in(&home);
        loaded.config.data_dir = data.0.clone();
        loaded.load_tui_state();
        assert_eq!(loaded.card_layout, state.card_layout);
    }

    #[test]
    fn event_feed_and_settings_column_widths_round_trip_under_their_stable_keys() {
        let dir = TempDir::new();
        let mut saved = state_in(&dir);
        saved.column_widths[CardId::EventFeed as usize][2] = Some(30);
        saved.column_widths[CardId::EventFeed as usize][7] = Some(11);
        saved.column_widths[CardId::SettingsHooks as usize][3] = Some(40);
        saved.column_widths[CardId::SettingsMcp as usize][1] = Some(18);
        saved.column_widths[CardId::SettingsAgents as usize][0] = Some(22);
        saved.column_widths[CardId::SettingsDoctor as usize][2] = Some(60);
        saved.save_tui_state();
        assert!(saved.toast.is_none(), "저장 실패 알림: {:?}", saved.toast);

        let text = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        for entry in [
            "event_feed = [0, 0, 30, 0, 0, 0, 0, 11]",
            "settings_hooks = [0, 0, 0, 40, 0, 0]",
            "settings_mcp = [0, 18, 0, 0]",
            "settings_agents = [22, 0, 0, 0]",
            "settings_doctor = [0, 0, 60]",
        ] {
            assert!(text.contains(entry), "{entry}: {text}");
        }

        let mut loaded = state_in(&dir);
        loaded.load_tui_state();
        assert!(loaded.toast.is_none(), "복원 알림: {:?}", loaded.toast);
        assert_eq!(loaded.column_widths, saved.column_widths);
    }

    #[test]
    fn invalid_widths_for_the_event_feed_and_settings_cards_are_skipped_and_counted() {
        let dir = TempDir::new();
        let body = "version = 1\n\
            [column_widths]\n\
            event_feed = [0, 0, 0, 0, 0, 0, 0]\n\
            settings_hooks = [0, 0, 0, 513, 0, 0]\n\
            settings_mcp = [0, -3, 0, 0]\n\
            settings_agents = [0, 0, 0, 9]\n\
            settings_doctor = [0, 0, 0, 0]\n";
        fs::write(dir.0.join(TUI_STATE_FILE_NAME), body).unwrap();
        let mut state = state_in(&dir);
        state.load_tui_state();

        // 길이 불일치, 상한 초과, 음수는 항목째 무시하고 맞는 항목만 반영
        for card in [
            CardId::EventFeed,
            CardId::SettingsHooks,
            CardId::SettingsMcp,
            CardId::SettingsDoctor,
        ] {
            assert!(
                state.column_widths[card as usize]
                    .iter()
                    .all(Option::is_none),
                "{card:?}"
            );
        }
        assert_eq!(
            state.column_widths[CardId::SettingsAgents as usize][..4],
            [None, None, None, Some(9)]
        );
        let toast = state.toast.as_ref().unwrap();
        assert!(toast.text.contains("일부 항목 무시 4건"), "{}", toast.text);
    }

    #[test]
    fn build_state_save_carries_the_state_file_path_and_the_full_text() {
        let dir = TempDir::new();
        let mut state = state_in(&dir);
        customize(&mut state);
        let save = state.build_state_save().unwrap();
        assert_eq!(save.path, dir.0.join(TUI_STATE_FILE_NAME));
        assert!(save.text.starts_with("version = 1\nlanguage = \"ko\"\n"));
        assert!(save.text.contains("settings_doctor"), "{}", save.text);
        // 요청을 만드는 것만으로는 파일을 쓰지 않음
        assert!(!dir.0.join(TUI_STATE_FILE_NAME).exists());
    }

    #[test]
    fn table_card_keys_and_column_counts_fit_the_column_width_storage() {
        let mut keys = Vec::new();
        for card in TABLE_CARDS {
            let (key, columns) = table_card_columns(card).unwrap();
            assert!((1..=MAX_TABLE_COLUMNS).contains(&columns), "{key}");
            keys.push(key);
        }
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), TABLE_CARDS.len());
        assert!(table_card_columns(CardId::ToolDetail).is_none());
        // 열 폭 저장 대상은 탭 배치 초기화 대상 표와 같음
        let mut reset_targets: Vec<CardId> = crate::tui::state::ActiveTab::ALL
            .iter()
            .flat_map(|tab| tab.table_cards().iter().copied())
            .collect();
        reset_targets.sort_by_key(|card| *card as usize);
        let mut stored = TABLE_CARDS.to_vec();
        stored.sort_by_key(|card| *card as usize);
        assert_eq!(stored, reset_targets);
    }

    #[test]
    fn project_cards_round_trip_widths_and_order_under_their_stable_keys() {
        let dir = TempDir::new();
        let mut saved = state_in(&dir);
        saved.column_widths[CardId::ProjectTable as usize][1] = Some(24);
        saved.column_widths[CardId::AttributionTable as usize][0] = Some(30);
        let mut projects = TabGrid::default();
        projects.rows[0][0] = Some(CardId::AttributionTable);
        projects.rows[1][0] = Some(CardId::ProjectTable);
        projects.rows[2][0] = Some(CardId::ProjectDetail);
        saved.card_layout.set_grid(ActiveTab::Projects, projects);
        saved.save_tui_state();
        assert!(saved.toast.is_none(), "저장 실패 알림: {:?}", saved.toast);

        let text = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        for entry in [
            "project_table = [0, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]",
            "attribution_table = [30, 0, 0, 0, 0, 0]",
            "projects = [[\"attribution_table\"], [\"project_table\"], [\"project_detail\"]]",
        ] {
            assert!(text.contains(entry), "{entry}: {text}");
        }

        let mut loaded = state_in(&dir);
        loaded.load_tui_state();
        assert!(loaded.toast.is_none(), "복원 알림: {:?}", loaded.toast);
        assert_eq!(loaded.column_widths, saved.column_widths);
        assert_eq!(loaded.card_layout, saved.card_layout);

        // 길이·범위가 맞지 않는 항목은 항목째 무시
        let body = "version = 1\n\
            [column_widths]\n\
            project_table = [0, 0, 0]\n\
            attribution_table = [0, 513, 0, 0, 0, 0]\n";
        fs::write(dir.0.join(TUI_STATE_FILE_NAME), body).unwrap();
        let mut skipped = state_in(&dir);
        skipped.load_tui_state();
        for card in [CardId::ProjectTable, CardId::AttributionTable] {
            assert!(
                skipped.column_widths[card as usize]
                    .iter()
                    .all(Option::is_none),
                "{card:?}"
            );
        }
    }

    #[test]
    fn column_widths_of_the_old_column_counts_load_as_a_prefix_without_a_notice() {
        let dir = TempDir::new();
        let body = "version = 1\n\
            [column_widths]\n\
            tool_table = [0, 7, 0, 0, 0, 0]\n\
            hook_table = [14, 0, 0, 0, 0, 0, 0, 0]\n\
            model_table = [17, 0, 0, 0, 0, 12, 0, 172, 0, 65, 10, 26, 0]\n";
        fs::write(dir.0.join(TUI_STATE_FILE_NAME), body).unwrap();
        let mut state = state_in(&dir);
        state.load_tui_state();

        assert!(state.toast.is_none(), "{:?}", state.toast);
        let tool = &state.column_widths[CardId::ToolTable as usize];
        assert_eq!(tool[1], Some(7));
        assert_eq!(tool[6..8], [None, None]);
        assert_eq!(state.column_widths[CardId::HookTable as usize][0], Some(14));
        let model = &state.column_widths[CardId::ModelTable as usize];
        assert_eq!(model[0], Some(17));
        assert_eq!(model[13], None);
    }

    #[test]
    fn grids_and_grid_ratios_round_trip_and_a_default_grid_writes_neither_table() {
        // 1. 기본 격자는 두 표 모두 없음
        let dir = TempDir::new();
        let mut state = state_in(&dir);
        state.save_tui_state();
        let text = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        assert!(!text.contains("[grids]"), "{text}");
        assert!(!text.contains("[grid_ratios]"), "{text}");

        // 2. 바꾼 격자와 비율
        let moved = apply_drop(
            &default_rows(ActiveTab::Overview),
            CardId::EventDetail,
            DropTarget::After(CardId::ChartTools),
        )
        .unwrap();
        state.card_layout.set_grid(ActiveTab::Overview, moved);
        assert!(
            state
                .card_layout
                .restore_grid_ratios("overview_cols_3", &[2_000, 4_000, 6_000])
        );
        assert!(
            state
                .card_layout
                .restore_grid_ratios("overview_rows_5", &[1_000, 2_000, 4_000, 6_000])
        );
        state.save_tui_state();
        assert!(state.toast.is_none(), "{:?}", state.toast);
        let text = fs::read_to_string(dir.0.join(TUI_STATE_FILE_NAME)).unwrap();
        for entry in [
            "[grids]\noverview = [[\"server_status\"], [\"kpi_requests\", \"kpi_tokens\", \"kpi_cache\", \"kpi_tools\", \"kpi_hooks\", \"savings_effect\"], [\"trend_requests\", \"trend_tokens\"], [\"chart_activity\", \"chart_tokens\", \"chart_tools\", \"event_detail\"], [\"event_feed\"]]\n",
            "[grid_ratios]\noverview_cols_3 = [2000, 4000, 6000]\noverview_rows_5 = [1000, 2000, 4000, 6000]\n",
        ] {
            assert!(text.contains(entry), "{entry}: {text}");
        }

        // 3. 복원
        let mut loaded = state_in(&dir);
        loaded.load_tui_state();
        assert!(loaded.toast.is_none(), "{:?}", loaded.toast);
        assert_eq!(loaded.card_layout, state.card_layout);
    }

    #[test]
    fn grid_ratios_apply_after_grids_and_missing_cards_join_their_default_row() {
        // 1. 새 표 없는 파일은 지금과 같게 적재
        let dir = TempDir::new();
        let path = dir.0.join(TUI_STATE_FILE_NAME);
        fs::write(
            &path,
            "version = 1\n[orders]\nsettings_rows = [3, 0, 1, 2]\n",
        )
        .unwrap();
        let mut plain = state_in(&dir);
        plain.load_tui_state();
        assert!(plain.toast.is_none(), "{:?}", plain.toast);
        assert_eq!(
            plain.card_layout.grid_of(ActiveTab::Overview),
            default_rows(ActiveTab::Overview)
        );
        assert_eq!(grid_rows(&plain, ActiveTab::Settings), doctor_first());

        // 2. 비율 표가 앞에 적혀도 격자 뒤에 적용, 빠진 두 카드는 기본 줄 끝과 새 마지막 줄
        let body = "version = 1\n\
            [grid_ratios]\n\
            overview_cols_3 = [2000, 4000, 6000]\n\
            \n\
            [grids]\n\
            overview = [[\"server_status\"], [\"kpi_requests\", \"kpi_tokens\", \"kpi_cache\", \"kpi_tools\", \"kpi_hooks\"], [\"trend_requests\", \"trend_tokens\"], [\"chart_activity\", \"chart_tokens\", \"chart_tools\", \"event_feed\"]]\n";
        fs::write(&path, body).unwrap();
        let mut state = state_in(&dir);
        state.load_tui_state();
        assert!(state.toast.is_none(), "{:?}", state.toast);
        let grid = state.card_layout.grid_of(ActiveTab::Overview);
        let rows: Vec<Vec<CardId>> = grid
            .rows
            .iter()
            .map(|cells| cells.iter().flatten().copied().collect::<Vec<CardId>>())
            .filter(|row| !row.is_empty())
            .collect();
        assert_eq!(
            rows[1],
            [
                CardId::KpiRequests,
                CardId::KpiTokens,
                CardId::KpiCache,
                CardId::KpiTools,
                CardId::KpiHooks,
                CardId::SavingsEffect,
            ]
        );
        assert_eq!(rows[3].last(), Some(&CardId::EventFeed));
        assert_eq!(rows[4], [CardId::EventDetail]);
        assert_eq!(rows.len(), 5);
        assert!(
            state
                .card_layout
                .grid_ratio_entries()
                .contains(&("overview_cols_3".to_string(), vec![2_000, 4_000, 6_000]))
        );
    }

    #[test]
    fn each_invalid_grid_entry_is_skipped_with_exactly_one_notice() {
        let dir = TempDir::new();
        let notice = i18n::state_entries_skipped(Lang::Ko, TUI_STATE_FILE_NAME, 1);
        for entry in [
            "[grids]\noverview = [[\"no_such_card\"]]\n",
            "[grids]\noverview = [[\"server_status\"], [\"server_status\"]]\n",
            "[grids]\noverview = [[\"kpi_requests\"], [\"kpi_tokens\"], [\"kpi_cache\"], [\"savings_effect\"], [\"kpi_tools\"], [\"kpi_hooks\"], [\"server_status\"]]\n",
            "[grids]\noverview = [[\"server_status\", \"kpi_requests\", \"kpi_tokens\", \"kpi_cache\", \"savings_effect\", \"kpi_tools\", \"kpi_hooks\"]]\n",
            "[grids]\nnowhere = [[\"server_status\"]]\n",
            "[grids]\noverview = \"server_status\"\n",
            "[grids]\noverview = [\"server_status\"]\n",
            "[grids]\noverview = [[1]]\n",
            "[grid_ratios]\noverview_cols_1 = [5000]\n",
        ] {
            fs::write(
                dir.0.join(TUI_STATE_FILE_NAME),
                format!("version = 1\n{entry}"),
            )
            .unwrap();
            let mut state = state_in(&dir);
            state.load_tui_state();
            let toast = state
                .toast
                .as_ref()
                .unwrap_or_else(|| panic!("no notice: {entry}"));
            assert_eq!(toast.text, notice, "{entry}");
            assert_eq!(state.card_layout, CardLayout::default(), "{entry}");
        }
    }

    #[test]
    fn a_card_of_another_tab_in_a_stored_grid_is_dropped_without_a_notice() {
        let dir = TempDir::new();
        fs::write(
            dir.0.join(TUI_STATE_FILE_NAME),
            "version = 1\n[grids]\ntools = [[\"model_table\", \"tool_table\"]]\n",
        )
        .unwrap();
        let mut state = state_in(&dir);
        state.load_tui_state();
        assert!(state.toast.is_none(), "{:?}", state.toast);
        let rows = grid_rows(&state, ActiveTab::Tools);
        assert!(!rows.concat().contains(&CardId::ModelTable), "{rows:?}");
        assert!(rows.concat().contains(&CardId::ToolTable), "{rows:?}");
    }
}
