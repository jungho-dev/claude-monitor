//! tui/cards.rs
//! 카드 식별자·종류, 카드별 스크롤·선택 상태, 클릭 동작, 마우스 적중 영역 모음.

use crate::tui::layout::SplitHit;
use crate::tui::state::ActiveTab;
use ratatui::layout::Rect;

pub const CARD_COUNT: usize = CardId::ReportOutline as usize + 1;

// 1. 카드 식별자 --------------------------------------------------------------
// 클릭·마우스 강조 대상 카드 (앱 바·탭 바·하단 바·도움말 제외)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardId {
    ServerStatus,
    KpiRequests,
    KpiTokens,
    KpiCache,
    KpiWait,
    KpiTools,
    KpiHooks,
    SavingsEffect,
    TrendRequests,
    TrendTokens,
    EventFeed,
    EventDetail,
    // 탭 공용 안내 (DB 없음, 집계 대기)
    Gate,
    ToolTable,
    ToolDetail,
    HookTable,
    HookDetail,
    ModelTable,
    ModelDetail,
    CacheBreaks,
    AgentTable,
    AgentDetail,
    SettingsHooks,
    SettingsMcp,
    SettingsAgents,
    SettingsDoctor,
    ReportBody,
    ReportOutline,
}

// 휠·키 동작 구분: 고정, 화면 위치만 이동, 선택 행과 화면 위치 함께 이동
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardKind {
    Static,
    Scrollable,
    Selectable,
}

impl CardId {
    pub fn kind(self) -> CardKind {
        match self {
            Self::EventFeed
            | Self::ToolTable
            | Self::HookTable
            | Self::ModelTable
            | Self::CacheBreaks
            | Self::AgentTable
            | Self::ReportOutline => CardKind::Selectable,
            Self::EventDetail
            | Self::ToolDetail
            | Self::HookDetail
            | Self::ModelDetail
            | Self::AgentDetail
            | Self::SettingsHooks
            | Self::SettingsMcp
            | Self::SettingsAgents
            | Self::SettingsDoctor
            | Self::ReportBody => CardKind::Scrollable,
            _ => CardKind::Static,
        }
    }
}

// 2. 카드별 보기 상태 ---------------------------------------------------------
// scroll = 첫 표시 행, selected = 선택 행, visible·total = 직전 그리기에서 기록한 값
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CardView {
    pub scroll: usize,
    pub selected: usize,
    pub visible: usize,
    pub total: usize,
}

// 키 이동 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMotion {
    Lines(isize),
    Pages(isize),
    Home,
    End,
}

// 3. 클릭 동작 ----------------------------------------------------------------
// 하단 바 버튼, 리포트 날짜 버튼, 단축키가 같이 쓰는 동작
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    Quit,
    ShowHelp,
    Reindex,
    CycleWindow,
    RefreshView,
    TogglePin,
    ResetLayout,
    ReportPrev,
    ReportNext,
    ReportGenerate,
}

// 4. 마우스 적중 영역 ---------------------------------------------------------
// 정렬 머리글 셀 (card = 정렬 대상 카드)
#[derive(Debug, Clone, Copy)]
pub struct HeaderHit {
    pub card: CardId,
    pub column: usize,
    pub rect: Rect,
}

// 텍스트 카드 머리글 줄 안의 열 구간 (start = 본문 왼쪽 기준 칸)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderSpan {
    pub column: usize,
    pub start: u16,
    pub width: u16,
}

// 선택 가능한 표의 본문 영역: 클릭 행 = offset + (클릭 y - rect.y)
#[derive(Debug, Clone, Copy)]
pub struct RowHit {
    pub card: CardId,
    pub rect: Rect,
    pub offset: usize,
    pub len: usize,
}

// 스크롤바 트랙 적중 영역 (거터+테두리 2열, 값은 그릴 때의 보기 상태)
#[derive(Debug, Clone, Copy)]
pub struct ScrollHit {
    pub card: CardId,
    pub track: Rect,
    pub total: usize,
    pub visible: usize,
    pub scroll: usize,
}

// 진행 중 썸 드래그 (grip = 누른 지점과 썸 시작 칸의 거리)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollDrag {
    pub card: CardId,
    pub grip: u16,
}

// 열 폭을 조절할 수 있는 표의 최대 열 수 (카드별 끌어 정한 폭 배열 길이)
pub const MAX_TABLE_COLUMNS: usize = 14;

// 표 머리글 두 칸 사이 경계 (column = 왼쪽 열 번호, width = 그릴 때 왼쪽 열 폭)
// grab = 잡는 칸 (머리글 행의 열 사이 한 칸), line = 끄는 동안 그리는 세로선
// min·max = 끌어서 정할 수 있는 왼쪽 열 폭 범위
#[derive(Debug, Clone, Copy)]
pub struct ColumnHit {
    pub card: CardId,
    pub column: usize,
    pub grab: Rect,
    pub line: Rect,
    pub width: u16,
    pub min: u16,
    pub max: u16,
}

// 진행 중 열 경계 드래그 (start_x·start_width = 누른 순간 포인터 열과 왼쪽 열 폭, moved = 실제로 끌었는지)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnDrag {
    pub card: CardId,
    pub column: usize,
    pub start_x: u16,
    pub start_width: u16,
    pub moved: bool,
}

// 매 프레임 그리기에서 다시 채우는 클릭 영역
#[derive(Debug, Default, Clone)]
pub struct UiHitAreas {
    pub tab_rects: Vec<(ActiveTab, Rect)>,
    pub button_rects: Vec<(UiAction, Rect)>,
    pub header_cells: Vec<HeaderHit>,
    pub column_hits: Vec<ColumnHit>,
    pub row_hits: Vec<RowHit>,
    pub split_hits: Vec<SplitHit>,
    pub scroll_hits: Vec<ScrollHit>,
    pub card_rects: Vec<(CardId, Rect)>,
}

// 5. 스크롤바 썸 --------------------------------------------------------------
// 썸 위치·길이 (트랙 안 칸 단위)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollThumb {
    pub start: u16,
    pub len: u16,
}

// 보기 상태 -> 썸, 내용이 들어가거나 트랙이 2칸 미만이면 None
pub fn scroll_thumb(
    total: usize,
    visible: usize,
    scroll: usize,
    track: u16,
) -> Option<ScrollThumb> {
    if visible == 0 || total <= visible || track < 2 {
        return None;
    }

    // 1. 길이: 보이는 비율, 1칸 이상 트랙 미만 (끝에서도 움직일 여유 유지)
    let track_len = usize::from(track);
    let len = ((visible * track_len + total / 2) / total).clamp(1, track_len - 1);

    // 2. 시작: 스크롤 비율을 남은 칸에 반올림 대응
    let free = track_len - len;
    let max_scroll = total - visible;
    let start = (scroll.min(max_scroll) * free + max_scroll / 2) / max_scroll;
    Some(ScrollThumb {
        start: start as u16,
        len: len as u16,
    })
}

// 썸 시작 칸 -> 스크롤 (scroll_thumb 의 역변환, 끝점 보존)
pub fn scroll_for_thumb(total: usize, visible: usize, track: u16, thumb_start: u16) -> usize {
    let Some(thumb) = scroll_thumb(total, visible, 0, track) else {
        return 0;
    };
    let free = usize::from(track - thumb.len);
    let max_scroll = total - visible;
    (usize::from(thumb_start).min(free) * max_scroll + free / 2) / free
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_thumb_matches_proportion_and_hits_both_ends() {
        let top = scroll_thumb(50, 10, 0, 10).unwrap();
        let middle = scroll_thumb(50, 10, 20, 10).unwrap();
        let bottom = scroll_thumb(50, 10, 40, 10).unwrap();
        assert_eq!((top.len, middle.len, bottom.len), (2, 2, 2));
        assert_eq!((top.start, middle.start, bottom.start), (0, 4, 8));

        // 범위를 넘는 스크롤은 끝으로 제한
        assert_eq!(scroll_thumb(50, 10, 999, 10).unwrap().start, 8);
    }

    #[test]
    fn scroll_thumb_keeps_room_to_move_when_almost_everything_is_visible() {
        let thumb = scroll_thumb(10, 9, 0, 4).unwrap();
        assert_eq!(thumb.len, 3);
        assert_eq!(scroll_thumb(10, 9, 1, 4).unwrap().start, 1);
    }

    #[test]
    fn scroll_thumb_none_when_content_fits() {
        assert_eq!(scroll_thumb(10, 10, 0, 8), None);
        assert_eq!(scroll_thumb(5, 10, 0, 8), None);
        assert_eq!(scroll_thumb(50, 0, 0, 8), None);
        assert_eq!(scroll_thumb(50, 10, 0, 1), None);
    }

    #[test]
    fn scroll_for_thumb_round_trips_endpoints_and_is_monotonic() {
        for (total, visible, track) in [(50usize, 10usize, 10u16), (200, 15, 15), (17, 9, 9)] {
            let max_scroll = total - visible;
            let mut previous = 0usize;
            for scroll in 0..=max_scroll {
                let thumb = scroll_thumb(total, visible, scroll, track).unwrap();
                let back = scroll_for_thumb(total, visible, track, thumb.start);
                assert!(back >= previous, "{total}/{visible}/{track} at {scroll}");
                previous = back;
            }
            assert_eq!(scroll_for_thumb(total, visible, track, 0), 0);
            let last = scroll_thumb(total, visible, max_scroll, track).unwrap();
            assert_eq!(
                scroll_for_thumb(total, visible, track, last.start),
                max_scroll
            );
            // 트랙 밖으로 끌어도 최대 스크롤
            assert_eq!(scroll_for_thumb(total, visible, track, 999), max_scroll);
        }
        assert_eq!(scroll_for_thumb(5, 10, 8, 3), 0);
    }
}
