//! tui/layout.rs
//! 카드 경계 드래그 크기 조절: 그룹별 경계 비율 보관, 카드 영역 분할, 드래그 영역 계산.

use ratatui::layout::Rect;

// 그룹당 최대 경계 수, 경계 비율 단위(만분율)
pub const MAX_CUTS: usize = 5;
const RATIO_SCALE: u32 = 10_000;
const GROUP_COUNT: usize = SplitGroup::ReportCols as usize + 1;

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
    GatewayRows,
    SettingsRows,
    ReportCols,
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

// 진행 중 드래그 (grip = 누른 지점과 경계의 거리)
#[derive(Debug, Clone, Copy)]
pub struct SplitDrag {
    pub group: SplitGroup,
    pub index: usize,
    pub grip: i32,
}

// 2. 카드 배치 상태 -----------------------------------------------------------
// 그룹별 경계 비율, None 이면 기본 배치
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CardLayout {
    ratios: [Option<[u16; MAX_CUTS]>; GROUP_COUNT],
}

impl CardLayout {
    // 카드 그룹 분할: 경계 위치 결정, 드래그 영역 등록, 카드 영역 반환
    // defaults = 기본 경계 위치(셀), mins = 카드별 최소 크기 (defaults 보다 1개 많음)
    pub fn split_cards(
        &self,
        hits: &mut Vec<SplitHit>,
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

        // 2. 카드 영역
        let mut panes = Vec::with_capacity(count + 1);
        let mut pane_start = 0u16;
        for pane_end in cuts[..count].iter().copied().chain([span_len]) {
            let pane_len = pane_end.saturating_sub(pane_start);
            panes.push(match axis {
                SplitAxis::Columns => Rect::new(origin + pane_start, area.y, pane_len, area.height),
                SplitAxis::Rows => Rect::new(area.x, origin + pane_start, area.width, pane_len),
            });
            pane_start = pane_end.saturating_add(gap).min(span_len);
        }

        // 3. 경계 드래그 영역 (이웃 카드 최소 크기 사이)
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
            hits.push(SplitHit {
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

        panes
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

    // 그룹 하나를 기본 배치로 복원
    pub fn reset_groups(&mut self, groups: &[SplitGroup]) {
        for group in groups {
            self.ratios[*group as usize] = None;
        }
    }

    // 조절한 그룹이 있는지
    pub fn has_custom_groups(&self, groups: &[SplitGroup]) -> bool {
        groups
            .iter()
            .any(|group| self.ratios[*group as usize].is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_rows(layout: &CardLayout, height: u16) -> (Vec<Rect>, Vec<SplitHit>) {
        let mut hits = Vec::new();
        let area = Rect::new(0, 10, 100, height);
        let panes = layout.split_cards(
            &mut hits,
            SplitGroup::OverviewRows,
            SplitAxis::Rows,
            area,
            &[height / 2, height - 6],
            &[5, 3, 3],
        );
        (panes, hits)
    }

    #[test]
    fn default_layout_keeps_given_cuts() {
        let (panes, hits) = split_rows(&CardLayout::default(), 30);
        let heights: Vec<u16> = panes.iter().map(|pane| pane.height).collect();
        assert_eq!(heights, [15, 9, 6]);
        assert_eq!(panes[2].y, 34);
        assert_eq!(hits.len(), 2);
        // 위 카드 아래 테두리 + 아래 카드 위 테두리
        assert_eq!(hits[0].grab, Rect::new(0, 24, 100, 2));
        assert_eq!(hits[0].line, Rect::new(0, 24, 100, 1));
    }

    #[test]
    fn dragged_cut_clamps_to_neighbor_minimum_and_keeps_ratio_on_resize() {
        let mut layout = CardLayout::default();
        let (_, hits) = split_rows(&layout, 30);

        // 아래 경계를 위로 끝까지: 가운데 카드 최소 3행에서 멈춤, 위 경계는 그대로
        assert_eq!(layout.drag_cut(&hits[1], 0), Some((3, 12)));
        let (panes, hits) = split_rows(&layout, 30);
        let heights: Vec<u16> = panes.iter().map(|pane| pane.height).collect();
        assert_eq!(heights, [15, 3, 12]);

        // 위 경계를 아래로 끝까지: 가운데 카드를 밀지 않음
        layout.drag_cut(&hits[0], 1000);
        let (panes, _) = split_rows(&layout, 30);
        assert_eq!(panes[0].height, 15);

        // 터미널 높이가 두 배가 되면 같은 비율
        let (panes, _) = split_rows(&layout, 60);
        let heights: Vec<u16> = panes.iter().map(|pane| pane.height).collect();
        assert_eq!(heights, [30, 6, 24]);
    }

    #[test]
    fn column_split_leaves_one_cell_gap_and_grabs_both_borders() {
        let mut hits = Vec::new();
        let area = Rect::new(2, 5, 81, 8);
        let panes = CardLayout::default().split_cards(
            &mut hits,
            SplitGroup::OverviewTrendCols,
            SplitAxis::Columns,
            area,
            &[44],
            &[20, 20],
        );
        assert_eq!(panes[0], Rect::new(2, 5, 44, 8));
        assert_eq!(panes[1], Rect::new(47, 5, 36, 8));
        assert_eq!(hits[0].line, Rect::new(46, 5, 1, 8));
        assert_eq!(hits[0].grab, Rect::new(45, 5, 3, 8));
        assert_eq!((hits[0].low, hits[0].high), (20, 60));
    }

    #[test]
    fn shrunk_terminal_keeps_leading_cards_at_minimum() {
        let mut layout = CardLayout::default();
        let (_, hits) = split_rows(&layout, 30);
        layout.drag_cut(&hits[0], 10 + 20);

        // 조절한 배치는 좁아져도 앞 카드부터 최소 크기 확보
        let (panes, _) = split_rows(&layout, 9);
        let heights: Vec<u16> = panes.iter().map(|pane| pane.height).collect();
        assert_eq!(heights, [5, 3, 1]);
    }

    #[test]
    fn reset_restores_default_cuts_for_listed_groups_only() {
        let mut layout = CardLayout::default();
        let (_, hits) = split_rows(&layout, 30);
        layout.drag_cut(&hits[0], 10 + 20);
        assert!(layout.has_custom_groups(&[SplitGroup::OverviewRows]));
        assert!(!layout.has_custom_groups(&[SplitGroup::ToolsRows]));

        layout.reset_groups(&[SplitGroup::ToolsRows]);
        assert!(layout.has_custom_groups(&[SplitGroup::OverviewRows]));

        layout.reset_groups(&[SplitGroup::OverviewRows]);
        assert_eq!(layout, CardLayout::default());
    }

    #[test]
    fn six_card_row_supports_five_cuts() {
        let mut hits = Vec::new();
        let area = Rect::new(0, 0, 100, 4);
        let layout = CardLayout::default();
        let panes = layout.split_cards(
            &mut hits,
            SplitGroup::OverviewKpiCols,
            SplitAxis::Columns,
            area,
            &[15, 31, 47, 63, 79],
            &[8; 6],
        );
        assert_eq!(panes.len(), 6);
        assert_eq!(hits.len(), 5);
        assert_eq!(panes[5].right(), 100);
    }
}
