//! tui/layout.rs
//! 카드 배치: 그룹별 경계 비율·카드 순서 보관, 카드 영역 분할, 경계 드래그 영역 계산.

use crate::tui::cards::{SlotGroupHit, UiHitAreas};
use ratatui::layout::Rect;

// 그룹당 최대 경계 수와 칸 수, 경계 비율 단위(만분율)
pub const MAX_CUTS: usize = 5;
pub const MAX_SLOTS: usize = MAX_CUTS + 1;
const RATIO_SCALE: u32 = 10_000;
pub const GROUP_COUNT: usize = SplitGroup::ReportCols as usize + 1;
// 순서를 바꾸지 않은 칸 배치 (칸 번호 = 카드 번호)
pub const IDENTITY_ORDER: [u8; MAX_SLOTS] = [0, 1, 2, 3, 4, 5];

// 1. 분할 그룹과 드래그 영역 --------------------------------------------------
// 한 축으로 나란한 카드 묶음
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitGroup {
    OverviewRows,
    // 이벤트 피드를 줄이지 않고도 들어가는 낮은 화면
    OverviewCompactRows,
    OverviewKpiCols,
    OverviewTrendCols,
    OverviewFeedCols,
    ToolsRows,
    HooksRows,
    ModelsRows,
    AgentsRows,
    SettingsRows,
    ReportCols,
}

impl SplitGroup {
    pub const ALL: [Self; GROUP_COUNT] = [
        Self::OverviewRows,
        Self::OverviewCompactRows,
        Self::OverviewKpiCols,
        Self::OverviewTrendCols,
        Self::OverviewFeedCols,
        Self::ToolsRows,
        Self::HooksRows,
        Self::ModelsRows,
        Self::AgentsRows,
        Self::SettingsRows,
        Self::ReportCols,
    ];

    // 배치 파일에서 그룹을 가리키는 고정 이름 (열거형 번호는 쓰지 않음)
    pub fn group_key(self) -> &'static str {
        match self {
            Self::OverviewRows => "overview_rows",
            Self::OverviewCompactRows => "overview_compact_rows",
            Self::OverviewKpiCols => "overview_kpi_cols",
            Self::OverviewTrendCols => "overview_trend_cols",
            Self::OverviewFeedCols => "overview_feed_cols",
            Self::ToolsRows => "tools_rows",
            Self::HooksRows => "hooks_rows",
            Self::ModelsRows => "models_rows",
            Self::AgentsRows => "agents_rows",
            Self::SettingsRows => "settings_rows",
            Self::ReportCols => "report_cols",
        }
    }

    // 그룹을 이루는 칸(카드) 수: 뷰의 기본 경계 수 + 1 과 같아야 함
    pub fn card_count(self) -> usize {
        match self {
            Self::OverviewRows => 4,
            Self::OverviewCompactRows => 3,
            Self::OverviewKpiCols => 6,
            Self::OverviewTrendCols => 3,
            Self::ModelsRows => 3,
            Self::SettingsRows => 4,
            Self::OverviewFeedCols
            | Self::ToolsRows
            | Self::HooksRows
            | Self::AgentsRows
            | Self::ReportCols => 2,
        }
    }

    pub fn axis(self) -> SplitAxis {
        match self {
            Self::OverviewKpiCols
            | Self::OverviewTrendCols
            | Self::OverviewFeedCols
            | Self::ReportCols => SplitAxis::Columns,
            _ => SplitAxis::Rows,
        }
    }

    // 칸 하나가 카드 한 장인 그룹만 순서를 바꿈 (개요 줄 그룹의 칸은 카드 묶음)
    pub fn is_reorderable(self) -> bool {
        !matches!(self, Self::OverviewRows | Self::OverviewCompactRows)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAxis {
    // 좌우 카드, 사이 빈 칸 1열
    Columns,
    // 상하 카드, 테두리 맞닿음
    Rows,
}

// 경계 하나의 드래그 영역 (위치는 origin 기준 셀)
#[derive(Debug, Clone, Copy)]
pub struct SplitHit {
    pub group: SplitGroup,
    pub index: usize,
    pub axis: SplitAxis,
    // 마우스로 잡는 영역, 손잡이 표시 줄
    pub grab: Rect,
    pub line: Rect,
    // 분할 구간 시작 좌표와 길이
    pub origin: u16,
    pub span_len: u16,
    // 그룹 전체 경계 위치, 이웃 카드 범위, 이 경계의 이동 범위
    pub cuts: [u16; MAX_CUTS],
    pub prev_end: u16,
    pub next_cut: u16,
    pub low: u16,
    pub high: u16,
}

// 진행 중 드래그 (grip = 누른 지점과 경계의 거리, moved = 실제로 끌었는지)
#[derive(Debug, Clone, Copy)]
pub struct SplitDrag {
    pub group: SplitGroup,
    pub index: usize,
    pub grip: i32,
    pub moved: bool,
}

// 2. 카드 순서 계산 -----------------------------------------------------------
// 삽입 이동: baseline 에서 card 를 빼 target 칸에 끼워 넣음 (사이 카드는 한 칸씩 밀림, count 밖 칸은 그대로)
pub fn insert_card_order(
    baseline: &[u8; MAX_SLOTS],
    count: usize,
    card: u8,
    target: usize,
) -> [u8; MAX_SLOTS] {
    let mut order = *baseline;
    let count = count.min(MAX_SLOTS);
    let Some(from) = baseline[..count].iter().position(|placed| *placed == card) else {
        return order;
    };
    let target = target.min(count.saturating_sub(1));
    if from < target {
        order.copy_within(from + 1..=target, from);
    } else if from > target {
        order.copy_within(target..from, target + 1);
    }
    order[target] = card;
    order
}

// 3. 카드 배치 상태 -----------------------------------------------------------
// 그룹별 경계 비율과 칸별 카드 번호 (orders[칸] = 그 칸에 놓인 카드), None 이면 기본 배치
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CardLayout {
    ratios: [Option<[u16; MAX_CUTS]>; GROUP_COUNT],
    orders: [Option<[u8; MAX_SLOTS]>; GROUP_COUNT],
}

impl CardLayout {
    // 카드 그룹 분할: 경계 위치 결정, 드래그 영역·칸 배치 등록, 카드 영역 반환
    // 반환 = 카드 번호 순서의 영역 (카드 k 는 지금 놓인 칸의 영역, 칸 크기·경계 비율·칸 최소 크기는 칸에 남음)
    // defaults = 기본 경계 위치(셀), mins = 칸별 최소 크기 (defaults 보다 1개 많음)
    pub fn split_cards(
        &self,
        hit_areas: &mut UiHitAreas,
        group: SplitGroup,
        axis: SplitAxis,
        area: Rect,
        defaults: &[u16],
        mins: &[u16],
    ) -> Vec<Rect> {
        debug_assert_eq!(mins.len(), defaults.len() + 1);
        let (origin, span_len, gap) = match axis {
            SplitAxis::Columns => (area.x, area.width, 1),
            SplitAxis::Rows => (area.y, area.height, 0),
        };
        let count = defaults.len().min(MAX_CUTS);

        // 1. 경계 위치 (조절한 그룹만 비율·최소 크기 적용)
        let ratios = self.ratios[group as usize];
        let mut cuts = [0u16; MAX_CUTS];
        let mut pane_start = 0u16;
        for (idx, default_cut) in defaults.iter().take(count).enumerate() {
            cuts[idx] = match ratios {
                Some(ratio) => {
                    let scaled = u32::from(ratio[idx]) * u32::from(span_len);
                    let wanted = ((scaled + RATIO_SCALE / 2) / RATIO_SCALE) as u16;
                    let tail_min: u16 = mins[idx + 1..].iter().map(|min| min + gap).sum();
                    let low = pane_start.saturating_add(mins[idx]);
                    wanted
                        .min(span_len.saturating_sub(tail_min))
                        .max(low)
                        .min(span_len)
                }
                None => (*default_cut).min(span_len),
            };
            pane_start = cuts[idx].saturating_add(gap);
        }

        // 2. 칸 영역 (칸 순서)
        let mut slots = Vec::with_capacity(count + 1);
        let mut pane_start = 0u16;
        for pane_end in cuts[..count].iter().copied().chain([span_len]) {
            let pane_len = pane_end.saturating_sub(pane_start);
            slots.push(match axis {
                SplitAxis::Columns => Rect::new(origin + pane_start, area.y, pane_len, area.height),
                SplitAxis::Rows => Rect::new(area.x, origin + pane_start, area.width, pane_len),
            });
            pane_start = pane_end.saturating_add(gap).min(span_len);
        }

        // 3. 카드 순서 적용: 카드 k 가 놓인 칸의 영역을 카드 번호 자리에 담음
        let order = self.order_of(group);
        let mut by_card = slots.clone();
        let mut cards = Vec::with_capacity(slots.len());
        for (slot, rect) in slots.iter().enumerate() {
            let placed = usize::from(order[slot]);
            let card = if placed < slots.len() { placed } else { slot };
            by_card[card] = *rect;
            cards.push(card);
        }
        if group.is_reorderable() {
            hit_areas.slot_groups.push(SlotGroupHit {
                group,
                axis,
                slots,
                cards,
            });
        }

        // 4. 경계 드래그 영역 (이웃 칸 최소 크기 사이)
        for (index, cut) in cuts[..count].iter().copied().enumerate() {
            let prev_end = if index == 0 { 0 } else { cuts[index - 1] + gap };
            let next_cut = if index + 1 < count {
                cuts[index + 1]
            } else {
                span_len
            };
            let low = prev_end.saturating_add(mins[index]);
            let high = next_cut.saturating_sub(gap + mins[index + 1]);
            if low > high || cut == 0 {
                continue;
            }
            // 최소 크기 밖 기본 경계는 첫 드래그에 튀지 않게 범위 확장
            let (low, high) = (low.min(cut), high.max(cut));
            let (grab, line) = match axis {
                // 빈 칸과 양옆 카드 테두리
                SplitAxis::Columns => (
                    Rect::new(origin + cut - 1, area.y, 3, area.height),
                    Rect::new(origin + cut, area.y, 1, area.height),
                ),
                // 위 카드 아래 테두리, 아래 카드 위 테두리
                SplitAxis::Rows => (
                    Rect::new(area.x, origin + cut - 1, area.width, 2),
                    Rect::new(area.x, origin + cut - 1, area.width, 1),
                ),
            };
            hit_areas.split_hits.push(SplitHit {
                group,
                index,
                axis,
                grab: grab.intersection(area),
                line: line.intersection(area),
                origin,
                span_len,
                cuts,
                prev_end,
                next_cut,
                low,
                high,
            });
        }

        by_card
    }

    // 경계 이동: pointer = 경계가 놓일 화면 좌표, 그룹 전체를 비율로 보관
    // 반환 = 경계 앞뒤 카드 크기
    pub fn drag_cut(&mut self, hit: &SplitHit, pointer: i32) -> Option<(u16, u16)> {
        if hit.span_len == 0 || hit.low > hit.high {
            return None;
        }
        let offset = pointer - i32::from(hit.origin);
        let cut = offset.clamp(i32::from(hit.low), i32::from(hit.high)) as u16;
        let mut cuts = hit.cuts;
        cuts[hit.index] = cut;
        let span_len = u32::from(hit.span_len);
        self.ratios[hit.group as usize] =
            Some(cuts.map(|cut| ((u32::from(cut) * RATIO_SCALE + span_len / 2) / span_len) as u16));

        let gap = match hit.axis {
            SplitAxis::Columns => 1,
            SplitAxis::Rows => 0,
        };
        Some((cut - hit.prev_end, hit.next_cut.saturating_sub(cut + gap)))
    }

    // 칸별 카드 번호 (기본 배치는 칸 번호 = 카드 번호)
    pub fn order_of(&self, group: SplitGroup) -> [u8; MAX_SLOTS] {
        self.orders[group as usize].unwrap_or(IDENTITY_ORDER)
    }

    // 카드가 놓인 칸 번호
    pub fn slot_of(&self, group: SplitGroup, card: usize) -> Option<usize> {
        let order = self.order_of(group);
        order[..group.card_count()]
            .iter()
            .position(|placed| usize::from(*placed) == card)
    }

    // 칸별 카드 번호 지정 (기본 배치와 같으면 보관하지 않음)
    pub fn set_order(&mut self, group: SplitGroup, order: [u8; MAX_SLOTS]) {
        self.orders[group as usize] = (order != IDENTITY_ORDER).then_some(order);
    }

    // 미리보기·확정용 삽입 이동: baseline 순서에서 card 를 target 칸으로
    pub fn move_card_to_slot(
        &mut self,
        group: SplitGroup,
        baseline: &[u8; MAX_SLOTS],
        card: usize,
        target: usize,
    ) {
        let moved = insert_card_order(baseline, group.card_count(), card as u8, target);
        self.set_order(group, moved);
    }

    // 배치 파일 저장용: 바꾼 순서, 바꾼 경계 비율 (그룹의 카드 수 길이)
    pub fn custom_order(&self, group: SplitGroup) -> Option<Vec<u8>> {
        self.orders[group as usize].map(|order| order[..group.card_count()].to_vec())
    }

    pub fn custom_ratios(&self, group: SplitGroup) -> Option<Vec<u16>> {
        self.ratios[group as usize].map(|ratio| ratio[..group.card_count() - 1].to_vec())
    }

    // 배치 파일 복원용: 길이·범위·중복이 맞을 때만 반영, 반환 = 반영 여부
    pub fn restore_order(&mut self, group: SplitGroup, values: &[i64]) -> bool {
        let count = group.card_count();
        if !group.is_reorderable() || values.len() != count {
            return false;
        }
        let mut order = IDENTITY_ORDER;
        let mut seen = [false; MAX_SLOTS];
        for (slot, value) in values.iter().enumerate() {
            let Ok(card) = usize::try_from(*value) else {
                return false;
            };
            if card >= count || seen[card] {
                return false;
            }
            seen[card] = true;
            order[slot] = card as u8;
        }
        self.set_order(group, order);
        true
    }

    pub fn restore_ratios(&mut self, group: SplitGroup, values: &[i64]) -> bool {
        if values.len() != group.card_count() - 1 {
            return false;
        }
        let mut ratios = [0u16; MAX_CUTS];
        for (slot, value) in values.iter().enumerate() {
            let Ok(ratio) = u16::try_from(*value) else {
                return false;
            };
            if u32::from(ratio) > RATIO_SCALE {
                return false;
            }
            ratios[slot] = ratio;
        }
        self.ratios[group as usize] = Some(ratios);
        true
    }

    // 그룹을 기본 배치(경계 비율·카드 순서)로 복원
    pub fn reset_groups(&mut self, groups: &[SplitGroup]) {
        for group in groups {
            self.ratios[*group as usize] = None;
            self.orders[*group as usize] = None;
        }
    }

    // 조절하거나 순서를 바꾼 그룹이 있는지
    pub fn has_custom_groups(&self, groups: &[SplitGroup]) -> bool {
        groups.iter().any(|group| {
            self.ratios[*group as usize].is_some() || self.orders[*group as usize].is_some()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_rows(layout: &CardLayout, height: u16) -> (Vec<Rect>, UiHitAreas) {
        let mut hit_areas = UiHitAreas::default();
        let area = Rect::new(0, 10, 100, height);
        let panes = layout.split_cards(
            &mut hit_areas,
            SplitGroup::OverviewRows,
            SplitAxis::Rows,
            area,
            &[height / 2, height - 6],
            &[5, 3, 3],
        );
        (panes, hit_areas)
    }

    fn split_tools(layout: &CardLayout) -> (Vec<Rect>, UiHitAreas) {
        let mut hit_areas = UiHitAreas::default();
        let panes = layout.split_cards(
            &mut hit_areas,
            SplitGroup::ToolsRows,
            SplitAxis::Rows,
            Rect::new(0, 4, 100, 30),
            &[22],
            &[5, 3],
        );
        (panes, hit_areas)
    }

    fn split_settings(layout: &CardLayout) -> (Vec<Rect>, UiHitAreas) {
        let mut hit_areas = UiHitAreas::default();
        let panes = layout.split_cards(
            &mut hit_areas,
            SplitGroup::SettingsRows,
            SplitAxis::Rows,
            Rect::new(0, 4, 100, 40),
            &[12, 20, 28],
            &[4; 4],
        );
        (panes, hit_areas)
    }

    #[test]
    fn default_layout_keeps_given_cuts() {
        let (panes, hit_areas) = split_rows(&CardLayout::default(), 30);
        let heights: Vec<u16> = panes.iter().map(|pane| pane.height).collect();
        assert_eq!(heights, [15, 9, 6]);
        assert_eq!(panes[2].y, 34);
        assert_eq!(hit_areas.split_hits.len(), 2);
        // 위 카드 아래 테두리 + 아래 카드 위 테두리
        assert_eq!(hit_areas.split_hits[0].grab, Rect::new(0, 24, 100, 2));
        assert_eq!(hit_areas.split_hits[0].line, Rect::new(0, 24, 100, 1));
        // 카드 묶음 줄 그룹은 순서를 바꾸지 않으므로 칸 배치를 등록하지 않음
        assert!(hit_areas.slot_groups.is_empty());
    }

    #[test]
    fn dragged_cut_clamps_to_neighbor_minimum_and_keeps_ratio_on_resize() {
        let mut layout = CardLayout::default();
        let (_, hit_areas) = split_rows(&layout, 30);

        // 아래 경계를 위로 끝까지: 가운데 카드 최소 3행에서 멈춤, 위 경계는 그대로
        assert_eq!(layout.drag_cut(&hit_areas.split_hits[1], 0), Some((3, 12)));
        let (panes, hit_areas) = split_rows(&layout, 30);
        let heights: Vec<u16> = panes.iter().map(|pane| pane.height).collect();
        assert_eq!(heights, [15, 3, 12]);

        // 위 경계를 아래로 끝까지: 가운데 카드를 밀지 않음
        layout.drag_cut(&hit_areas.split_hits[0], 1000);
        let (panes, _) = split_rows(&layout, 30);
        assert_eq!(panes[0].height, 15);

        // 터미널 높이가 두 배가 되면 같은 비율
        let (panes, _) = split_rows(&layout, 60);
        let heights: Vec<u16> = panes.iter().map(|pane| pane.height).collect();
        assert_eq!(heights, [30, 6, 24]);
    }

    #[test]
    fn column_split_leaves_one_cell_gap_and_grabs_both_borders() {
        let mut hit_areas = UiHitAreas::default();
        let area = Rect::new(2, 5, 81, 8);
        let panes = CardLayout::default().split_cards(
            &mut hit_areas,
            SplitGroup::OverviewTrendCols,
            SplitAxis::Columns,
            area,
            &[44],
            &[20, 20],
        );
        let hits = &hit_areas.split_hits;
        assert_eq!(panes[0], Rect::new(2, 5, 44, 8));
        assert_eq!(panes[1], Rect::new(47, 5, 36, 8));
        assert_eq!(hits[0].line, Rect::new(46, 5, 1, 8));
        assert_eq!(hits[0].grab, Rect::new(45, 5, 3, 8));
        assert_eq!((hits[0].low, hits[0].high), (20, 60));
    }

    #[test]
    fn shrunk_terminal_keeps_leading_cards_at_minimum() {
        let mut layout = CardLayout::default();
        let (_, hit_areas) = split_rows(&layout, 30);
        layout.drag_cut(&hit_areas.split_hits[0], 10 + 20);

        // 조절한 배치는 좁아져도 앞 카드부터 최소 크기 확보
        let (panes, _) = split_rows(&layout, 9);
        let heights: Vec<u16> = panes.iter().map(|pane| pane.height).collect();
        assert_eq!(heights, [5, 3, 1]);
    }

    #[test]
    fn reset_restores_default_cuts_for_listed_groups_only() {
        let mut layout = CardLayout::default();
        let (_, hit_areas) = split_rows(&layout, 30);
        layout.drag_cut(&hit_areas.split_hits[0], 10 + 20);
        assert!(layout.has_custom_groups(&[SplitGroup::OverviewRows]));
        assert!(!layout.has_custom_groups(&[SplitGroup::ToolsRows]));

        layout.reset_groups(&[SplitGroup::ToolsRows]);
        assert!(layout.has_custom_groups(&[SplitGroup::OverviewRows]));

        layout.reset_groups(&[SplitGroup::OverviewRows]);
        assert_eq!(layout, CardLayout::default());
    }

    #[test]
    fn six_card_row_supports_five_cuts() {
        let mut hit_areas = UiHitAreas::default();
        let area = Rect::new(0, 0, 100, 4);
        let layout = CardLayout::default();
        let panes = layout.split_cards(
            &mut hit_areas,
            SplitGroup::OverviewKpiCols,
            SplitAxis::Columns,
            area,
            &[15, 31, 47, 63, 79],
            &[8; 6],
        );
        assert_eq!(panes.len(), 6);
        assert_eq!(hit_areas.split_hits.len(), 5);
        assert_eq!(panes[5].right(), 100);
        assert_eq!(hit_areas.slot_groups[0].slots.len(), 6);
    }

    #[test]
    fn insertion_move_shifts_the_cards_between_source_and_target() {
        let base = IDENTITY_ORDER;
        // 앞 카드를 뒤로: 사이 카드는 한 칸씩 앞으로
        assert_eq!(insert_card_order(&base, 4, 0, 2), [1, 2, 0, 3, 4, 5]);
        // 뒤 카드를 앞으로: 사이 카드는 한 칸씩 뒤로
        assert_eq!(insert_card_order(&base, 4, 3, 1), [0, 3, 1, 2, 4, 5]);
        // 제자리, 인접 교환, 칸 수 밖 목표는 마지막 칸으로 제한
        assert_eq!(insert_card_order(&base, 4, 2, 2), base);
        assert_eq!(insert_card_order(&base, 2, 0, 1), [1, 0, 2, 3, 4, 5]);
        assert_eq!(insert_card_order(&base, 3, 0, 9), [1, 2, 0, 3, 4, 5]);
        // 이미 바뀐 순서에서도 카드 번호로 찾아 이동 (카드 3 은 지금 둘째 칸)
        let moved = [0, 3, 1, 2, 4, 5];
        assert_eq!(insert_card_order(&moved, 4, 3, 3), [0, 1, 2, 3, 4, 5]);
        // 범위 밖 카드는 그대로
        assert_eq!(insert_card_order(&base, 2, 4, 0), base);
    }

    #[test]
    fn order_permutation_maps_each_card_to_the_slot_it_occupies() {
        let (default_panes, _) = split_tools(&CardLayout::default());
        let mut layout = CardLayout::default();
        layout.move_card_to_slot(SplitGroup::ToolsRows, &IDENTITY_ORDER, 0, 1);

        // 카드 0(표)이 아래 칸으로, 카드 1(상세)이 위 칸으로
        assert_eq!(layout.order_of(SplitGroup::ToolsRows)[..2], [1, 0]);
        assert_eq!(layout.slot_of(SplitGroup::ToolsRows, 0), Some(1));
        assert_eq!(layout.slot_of(SplitGroup::ToolsRows, 1), Some(0));
        let (panes, hit_areas) = split_tools(&layout);
        assert_eq!(panes[0], default_panes[1]);
        assert_eq!(panes[1], default_panes[0]);

        // 칸 기록: 칸 영역은 그대로, 각 칸의 카드 번호만 바뀜
        let recorded = &hit_areas.slot_groups[0];
        assert_eq!(recorded.group, SplitGroup::ToolsRows);
        assert_eq!(recorded.slots, default_panes);
        assert_eq!(recorded.cards, [1, 0]);
    }

    #[test]
    fn slot_sizes_and_cuts_stay_unchanged_after_a_reorder_and_a_resize() {
        let mut layout = CardLayout::default();
        let (_, hit_areas) = split_settings(&layout);
        layout.drag_cut(&hit_areas.split_hits[1], 4 + 24);
        let (_, before) = split_settings(&layout);
        let slot_rects = before.slot_groups[0].slots.clone();
        let hit_cuts: Vec<[u16; MAX_CUTS]> = before.split_hits.iter().map(|hit| hit.cuts).collect();

        layout.move_card_to_slot(SplitGroup::SettingsRows, &IDENTITY_ORDER, 3, 0);
        assert_eq!(layout.order_of(SplitGroup::SettingsRows)[..4], [3, 0, 1, 2]);
        let (panes, after) = split_settings(&layout);
        assert_eq!(after.slot_groups[0].slots, slot_rects);
        assert_eq!(after.slot_groups[0].cards, [3, 0, 1, 2]);
        let after_cuts: Vec<[u16; MAX_CUTS]> =
            after.split_hits.iter().map(|hit| hit.cuts).collect();
        assert_eq!(after_cuts, hit_cuts);
        // 카드 3 은 첫 칸, 카드 0 은 둘째 칸 영역
        assert_eq!(panes[3], slot_rects[0]);
        assert_eq!(panes[0], slot_rects[1]);
        assert_eq!(panes[1], slot_rects[2]);
        assert_eq!(panes[2], slot_rects[3]);
        // 영역은 겹치지 않고 위에서 아래로 이어짐
        let mut heights: Vec<u16> = slot_rects.iter().map(|rect| rect.height).collect();
        heights.sort_unstable();
        let mut by_card: Vec<u16> = panes.iter().map(|rect| rect.height).collect();
        by_card.sort_unstable();
        assert_eq!(heights, by_card);
    }

    #[test]
    fn reset_clears_orders_together_with_ratios() {
        let mut layout = CardLayout::default();
        layout.move_card_to_slot(SplitGroup::ModelsRows, &IDENTITY_ORDER, 0, 2);
        assert!(layout.has_custom_groups(&[SplitGroup::ModelsRows]));
        layout.reset_groups(&[SplitGroup::ToolsRows]);
        assert!(layout.has_custom_groups(&[SplitGroup::ModelsRows]));
        layout.reset_groups(&[SplitGroup::ModelsRows]);
        assert_eq!(layout, CardLayout::default());
        assert_eq!(layout.order_of(SplitGroup::ModelsRows), IDENTITY_ORDER);

        // 순서를 제자리로 되돌리면 기본 배치와 같음
        layout.move_card_to_slot(SplitGroup::ModelsRows, &IDENTITY_ORDER, 0, 2);
        let moved = layout.order_of(SplitGroup::ModelsRows);
        layout.move_card_to_slot(SplitGroup::ModelsRows, &moved, 0, 0);
        assert_eq!(layout, CardLayout::default());
    }

    #[test]
    fn group_metadata_is_unique_and_matches_axis_and_reorder_scope() {
        let mut keys: Vec<&str> = SplitGroup::ALL
            .iter()
            .map(|group| group.group_key())
            .collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), GROUP_COUNT);
        for (position, group) in SplitGroup::ALL.iter().enumerate() {
            assert_eq!(*group as usize, position);
            assert!((2..=MAX_SLOTS).contains(&group.card_count()));
        }
        assert!(!SplitGroup::OverviewRows.is_reorderable());
        assert!(!SplitGroup::OverviewCompactRows.is_reorderable());
        let reorderable = SplitGroup::ALL
            .iter()
            .filter(|group| group.is_reorderable())
            .count();
        assert_eq!(reorderable, 9);
        assert_eq!(SplitGroup::ReportCols.axis(), SplitAxis::Columns);
        assert_eq!(SplitGroup::OverviewKpiCols.axis(), SplitAxis::Columns);
        assert_eq!(SplitGroup::SettingsRows.axis(), SplitAxis::Rows);
    }

    #[test]
    fn restore_rejects_wrong_length_duplicates_and_out_of_range_values() {
        let mut layout = CardLayout::default();
        assert!(layout.restore_order(SplitGroup::SettingsRows, &[3, 2, 1, 0]));
        assert_eq!(layout.order_of(SplitGroup::SettingsRows)[..4], [3, 2, 1, 0]);
        assert_eq!(
            layout.custom_order(SplitGroup::SettingsRows),
            Some(vec![3, 2, 1, 0])
        );

        let mut layout = CardLayout::default();
        assert!(!layout.restore_order(SplitGroup::SettingsRows, &[0, 1, 2]));
        assert!(!layout.restore_order(SplitGroup::SettingsRows, &[0, 1, 1, 2]));
        assert!(!layout.restore_order(SplitGroup::SettingsRows, &[0, 1, 2, 4]));
        assert!(!layout.restore_order(SplitGroup::SettingsRows, &[0, 1, 2, -1]));
        assert!(!layout.restore_order(SplitGroup::OverviewRows, &[0, 1, 2, 3]));
        assert!(!layout.restore_ratios(SplitGroup::ToolsRows, &[3_000, 5_000]));
        assert!(!layout.restore_ratios(SplitGroup::ToolsRows, &[10_001]));
        assert!(!layout.restore_ratios(SplitGroup::ToolsRows, &[-1]));
        assert_eq!(layout, CardLayout::default());

        assert!(layout.restore_ratios(SplitGroup::ToolsRows, &[6_000]));
        assert_eq!(
            layout.custom_ratios(SplitGroup::ToolsRows),
            Some(vec![6_000])
        );
        let (panes, _) = split_tools(&layout);
        assert_eq!(panes[0].height, 18);
    }
}
