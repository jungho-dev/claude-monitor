//! tui/card_move.rs
//! 카드 위치 이동: 제목 길게 누름 시계, 이동 중 미리보기, 확정·취소, Shift+방향키 한 칸 이동.

use crate::tui::cards::{CARD_HOLD_TIME, CardId, CardMove, GridLineHit};
use crate::tui::i18n::{self, Msg};
use crate::tui::layout::grid::{GridLine, GridMove, GridPlan};
use crate::tui::layout::grid::{apply_drop, resolve_drop, shift_card};
use crate::tui::state::{AppState, ToastLevel, UiCommand};
use crossterm::event::KeyCode;
use ratatui::layout::{Position, Rect};
use std::time::{Duration, Instant};

impl AppState {
    // 1. 길게 누름 시계 -------------------------------------------------------
    // 제목을 누른 지 CARD_HOLD_TIME 이 지나면 이동 모드 진입 (마우스 움직임과 무관), 반환 = 상태가 바뀌었는지
    pub fn tick(&mut self, now: Instant) -> bool {
        let Some(press) = self.card_press else {
            return false;
        };
        if now.saturating_duration_since(press.started) < CARD_HOLD_TIME {
            return false;
        }
        self.card_press = None;

        // 격자 카드: 누른 순간 격자와 줄 보관
        let grid_tab = self
            .hit_areas
            .grid_lines
            .iter()
            .find(|line| line.cards.iter().any(|(placed, _)| *placed == press.card))
            .map(|line| line.tab);
        let Some(tab) = grid_tab else {
            return false;
        };
        let lines: Vec<GridLineHit> = self
            .hit_areas
            .grid_lines
            .iter()
            .filter(|line| line.tab == tab)
            .cloned()
            .collect();
        let baseline = self.card_layout.begin_grid_move(tab);
        self.grid_move = Some(GridMove {
            tab,
            card: press.card,
            baseline,
            lines,
        });
        self.card_move = Some(CardMove { card: press.card });
        true
    }

    // 이동 모드 진입까지 남은 시간 (이벤트 대기 시간 상한 계산용), 대기 중이 아니면 None
    pub fn press_remaining(&self, now: Instant) -> Option<Duration> {
        self.card_press.map(|press| {
            CARD_HOLD_TIME.saturating_sub(now.saturating_duration_since(press.started))
        })
    }

    // 2. 취소 -----------------------------------------------------------------
    // 대기 중 누름은 버리고, 이동 중이면 누르기 전 격자로 복원, 반환 = 상태가 바뀌었는지
    pub fn cancel_card_move(&mut self) -> bool {
        let pressed = self.card_press.take().is_some();
        let lifted = self.card_move.take().is_some();
        let Some(moving) = self.grid_move.take() else {
            return pressed || lifted;
        };
        self.card_layout
            .end_grid_move(moving.tab, Some(moving.baseline));
        true
    }

    // 3. 미리보기 -------------------------------------------------------------
    // 격자 카드 미리보기: 누른 순간 줄 기준 놓을 자리, 거부되면 미리보기 유지
    pub fn preview_grid_move(&mut self, position: Position) {
        let Some(moving) = self.grid_move.as_ref() else {
            return;
        };

        // 1. 누른 순간 줄의 놓을 자리
        let plan = GridPlan {
            lines: moving
                .lines
                .iter()
                .map(|line| GridLine {
                    row: line.row,
                    cards: line.cards.iter().map(|(card, _)| *card).collect(),
                    height: line.rect.height,
                })
                .collect(),
        };
        let line_rects: Vec<Rect> = moving.lines.iter().map(|line| line.rect).collect();
        let card_rects: Vec<(CardId, Rect)> = moving
            .lines
            .iter()
            .flat_map(|line| line.cards.iter().copied())
            .collect();
        let target = resolve_drop(&plan, &line_rects, &card_rects, moving.card, position);

        // 2. 누르기 전 격자에서 다시 계산
        let (tab, baseline) = (moving.tab, moving.baseline);
        let current = self.card_layout.grid_of(tab);
        let Some(moved) =
            apply_drop(&baseline, moving.card, target).filter(|moved| *moved != current)
        else {
            return;
        };

        // 3. 제자리 복원 (비율 포함)
        if moved == baseline {
            self.card_layout.end_grid_move(tab, Some(baseline));
            self.card_layout.begin_grid_move(tab);
            return;
        }
        self.card_layout.preview_grid(tab, moved);
    }

    // 4. 확정 -----------------------------------------------------------------
    // 격자가 바뀌었을 때만 알림과 저장 요청 (제자리에 놓으면 아무 일 없음)
    pub fn commit_card_move(&mut self) -> UiCommand {
        // 1. 미리보기 격자, 누르기 전 격자로 복원
        self.card_move = None;
        let Some(moving) = self.grid_move.take() else {
            return UiCommand::None;
        };
        let committed = self.card_layout.grid_of(moving.tab);
        self.card_layout
            .end_grid_move(moving.tab, Some(moving.baseline));

        // 2. 바뀐 격자만 확정·알림·저장 (비율 초기화 한 번)
        if committed == moving.baseline {
            return UiCommand::None;
        }
        self.card_layout.set_grid(moving.tab, committed);
        self.show_toast(i18n::text(self.lang, Msg::ToastCardMoved), ToastLevel::Info);
        UiCommand::SaveState
    }

    // 5. Shift+방향키 한 칸 이동 ----------------------------------------------
    // 이번 프레임에 그려진 격자 카드만 대상 (안내 카드만 보이는 탭은 배치를 바꾸지 않음), 격자가 바뀔 때만 확정·저장
    pub fn shift_focused_card(&mut self, card: CardId, code: KeyCode) -> UiCommand {
        // 1. 이번 프레임 격자 카드
        let drawn_tab = self
            .hit_areas
            .grid_lines
            .iter()
            .find(|line| line.cards.iter().any(|(placed, _)| *placed == card))
            .map(|line| line.tab);
        let Some(tab) = drawn_tab else {
            return UiCommand::None;
        };
        let drawn: Vec<CardId> = self
            .hit_areas
            .grid_lines
            .iter()
            .filter(|line| line.tab == tab)
            .flat_map(|line| line.cards.iter().map(|(placed, _)| *placed))
            .collect();

        // 2. 격자 이동
        let grid = self.card_layout.grid_of(tab);
        let Some(moved) = shift_card(&grid, card, code, &drawn).filter(|moved| *moved != grid)
        else {
            return UiCommand::None;
        };
        self.card_layout.set_grid(tab, moved);
        self.show_toast(i18n::text(self.lang, Msg::ToastCardMoved), ToastLevel::Info);
        UiCommand::SaveState
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::cards::UiHitAreas;
    use crate::tui::layout::grid::{CardSlot, SlotHeight, card_key, default_rows};
    use crate::tui::layout::{CardLayout, SplitGroup};
    use crate::tui::state::ActiveTab;
    use crate::tui::test_support::sample_config;
    use crate::tui::theme;
    use crate::tui::views::common::interactive_card;
    use chrono::NaiveDate;
    use crossterm::event::{KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn overview_state() -> AppState {
        AppState::new(
            sample_config(),
            NaiveDate::from_ymd_opt(2026, 11, 15).unwrap(),
        )
    }

    // 개요 기본 격자 카드 조건 (추이 줄 rank 2 reserve 5, 차트 줄 rank 3 reserve 9)
    fn overview_slots() -> Vec<CardSlot> {
        let grid = default_rows(ActiveTab::Overview);
        let mut slots = Vec::new();
        for (row, cells) in grid.rows.iter().enumerate() {
            let (min_w, min_h, height, rank, reserve) = match row {
                0 => (20, 3, SlotHeight::Fixed(3), 0, 0),
                1 => (8, 3, SlotHeight::Fixed(3), 0, 0),
                2 => (16, 5, SlotHeight::Fixed(5), 2, 5),
                3 => (21, 6, SlotHeight::Fixed(10), 3, 9),
                _ => (20, 3, SlotHeight::Fill, 0, 0),
            };
            for card in cells.iter().flatten() {
                slots.push(CardSlot {
                    card: *card,
                    min_w,
                    min_h,
                    height,
                    weight: 1,
                    rank,
                    reserve,
                });
            }
        }
        slots
    }

    // 테스트 전용 격자 그리기 (카드마다 테두리 카드)
    fn draw_grid(state: &mut AppState, width: u16, height: u16) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                state.hit_areas = UiHitAreas::default();
                let layout = state.card_layout;
                let placed = layout.arrange_tab(
                    &mut state.hit_areas,
                    ActiveTab::Overview,
                    frame.area(),
                    &overview_slots(),
                );
                for (card, rect) in placed {
                    let block =
                        interactive_card(state, card, rect, card_key(card), "", theme::BORDER_DIM);
                    frame.render_widget(block, rect);
                }
            })
            .unwrap();
    }

    fn overview_rows(state: &AppState) -> Vec<Vec<CardId>> {
        state
            .card_layout
            .grid_of(ActiveTab::Overview)
            .rows
            .iter()
            .map(|cells| cells.iter().flatten().copied().collect::<Vec<CardId>>())
            .filter(|row| !row.is_empty())
            .collect()
    }

    fn shift_key(state: &mut AppState, card: CardId, code: KeyCode) -> UiCommand {
        state.focused_card = Some((ActiveTab::Overview, card));
        state.handle_key_event(KeyEvent::new(code, KeyModifiers::SHIFT))
    }

    fn pointer(
        state: &mut AppState,
        kind: MouseEventKind,
        at: Position,
        now: Instant,
    ) -> UiCommand {
        let event = MouseEvent {
            kind,
            column: at.x,
            row: at.y,
            modifiers: KeyModifiers::NONE,
        };
        state.handle_mouse_event_at(event, now)
    }

    fn drawn_rect(state: &AppState, card: CardId) -> Rect {
        state
            .hit_areas
            .card_rects
            .iter()
            .find(|(placed, _)| *placed == card)
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("{card:?} not drawn"))
    }

    // 카드가 놓인 화면 줄 영역
    fn line_rect(state: &AppState, card: CardId) -> Rect {
        state
            .hit_areas
            .grid_lines
            .iter()
            .find(|line| line.cards.iter().any(|(placed, _)| *placed == card))
            .map(|line| line.rect)
            .unwrap_or_else(|| panic!("{card:?} line not drawn"))
    }

    // 130x40 에 그리고 제목을 400ms 눌러 이동 모드, 반환 = 누른 칸
    fn lift_card(state: &mut AppState, card: CardId, base: Instant) -> Position {
        draw_grid(state, 130, 40);
        let title = state
            .hit_areas
            .title_hits
            .iter()
            .find(|hit| hit.card == card)
            .map(|hit| Position::new(hit.rect.x + 1, hit.rect.y))
            .unwrap_or_else(|| panic!("{card:?} title not drawn"));
        let down = MouseEventKind::Down(MouseButton::Left);
        assert_eq!(pointer(state, down, title, base), UiCommand::None);
        assert!(state.card_press.is_some());
        assert!(state.tick(base + CARD_HOLD_TIME));
        let moving = state.grid_move.as_ref().expect("grid move");
        assert_eq!((moving.tab, moving.card), (ActiveTab::Overview, card));
        assert_eq!(state.card_move.map(|lifted| lifted.card), Some(card));
        title
    }

    fn drag_grid(state: &mut AppState, at: Position, now: Instant) {
        let drag = MouseEventKind::Drag(MouseButton::Left);
        assert_eq!(pointer(state, drag, at, now), UiCommand::None);
    }

    fn release_grid(state: &mut AppState, at: Position, now: Instant) -> UiCommand {
        let command = pointer(state, MouseEventKind::Up(MouseButton::Left), at, now);
        assert!(state.grid_move.is_none());
        assert!(state.card_move.is_none());
        command
    }

    // 60 폭: 차트 줄 2 + 1
    #[test]
    fn grid_render_records_drawn_lines_and_no_width_grab_on_a_wrapped_row() {
        let mut state = overview_state();
        draw_grid(&mut state, 60, 40);
        let lines: Vec<(usize, usize)> = state
            .hit_areas
            .grid_lines
            .iter()
            .map(|line| (line.row, line.cards.len()))
            .collect();
        assert_eq!(lines, [(0, 1), (1, 6), (2, 2), (3, 2), (3, 1), (4, 2)]);
        for line in &state.hit_areas.grid_lines {
            assert_eq!(line.tab, ActiveTab::Overview);
            assert!(
                line.cards
                    .iter()
                    .all(|(_, rect)| line.rect.union(*rect) == line.rect),
                "{line:?}"
            );
        }
        let col_grabs = |row: u8| {
            state
                .hit_areas
                .split_hits
                .iter()
                .filter(|hit| hit.group == SplitGroup::GridCols(ActiveTab::Overview, row))
                .count()
        };
        assert_eq!(col_grabs(1), 5);
        assert_eq!(col_grabs(2), 1);
        assert_eq!(col_grabs(3), 0);
    }

    #[test]
    fn shift_arrows_move_a_drawn_grid_card_in_every_direction_and_save() {
        let mut state = overview_state();
        draw_grid(&mut state, 130, 40);
        let base = overview_rows(&state);

        // 좌우: 줄 안 교환
        assert_eq!(
            shift_key(&mut state, CardId::KpiTokens, KeyCode::Right),
            UiCommand::SaveState
        );
        assert_eq!(
            overview_rows(&state)[1][..3],
            [CardId::KpiRequests, CardId::KpiCache, CardId::KpiTokens]
        );
        draw_grid(&mut state, 130, 40);
        assert_eq!(
            shift_key(&mut state, CardId::KpiTokens, KeyCode::Left),
            UiCommand::SaveState
        );
        assert_eq!(overview_rows(&state), base);

        // 아래: 자기 새 줄, 다음은 아래 줄 끝 합류
        draw_grid(&mut state, 130, 40);
        assert_eq!(
            shift_key(&mut state, CardId::KpiTokens, KeyCode::Down),
            UiCommand::SaveState
        );
        let rows = overview_rows(&state);
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[2], [CardId::KpiTokens]);
        draw_grid(&mut state, 130, 40);
        assert_eq!(
            shift_key(&mut state, CardId::KpiTokens, KeyCode::Down),
            UiCommand::SaveState
        );
        assert_eq!(
            overview_rows(&state)[2],
            [base[2].clone(), vec![CardId::KpiTokens]].concat()
        );

        // 위: 위에 새 줄, 다음은 위 줄 끝 합류
        draw_grid(&mut state, 130, 40);
        assert_eq!(
            shift_key(&mut state, CardId::KpiTokens, KeyCode::Up),
            UiCommand::SaveState
        );
        assert_eq!(overview_rows(&state)[2], [CardId::KpiTokens]);
        draw_grid(&mut state, 130, 40);
        assert_eq!(
            shift_key(&mut state, CardId::KpiTokens, KeyCode::Up),
            UiCommand::SaveState
        );
        let kpi_rest: Vec<CardId> = base[1]
            .iter()
            .copied()
            .filter(|card| *card != CardId::KpiTokens)
            .collect();
        assert_eq!(
            overview_rows(&state)[1],
            [kpi_rest, vec![CardId::KpiTokens]].concat()
        );

        // 맨 위 혼자인 카드의 위: 변화 없음
        draw_grid(&mut state, 130, 40);
        let before = overview_rows(&state);
        assert_eq!(
            shift_key(&mut state, CardId::ServerStatus, KeyCode::Up),
            UiCommand::None
        );
        assert_eq!(overview_rows(&state), before);
    }

    #[test]
    fn shift_right_without_a_drawn_neighbor_changes_nothing_and_saves_nothing() {
        // 이벤트 없음: 상세 카드 없이 그림
        let mut state = overview_state();
        let slots: Vec<CardSlot> = overview_slots()
            .into_iter()
            .filter(|slot| slot.card != CardId::EventDetail)
            .collect();
        let layout = state.card_layout;
        layout.arrange_tab(
            &mut state.hit_areas,
            ActiveTab::Overview,
            Rect::new(0, 0, 130, 40),
            &slots,
        );
        assert!(line_rect(&state, CardId::EventFeed).height > 0);
        assert_eq!(
            shift_key(&mut state, CardId::EventFeed, KeyCode::Right),
            UiCommand::None
        );
        assert_eq!(state.card_layout, CardLayout::default());
        assert!(state.toast.is_none());
    }

    #[test]
    fn shift_arrows_ignore_a_grid_card_not_drawn_this_frame() {
        let mut state = overview_state();
        draw_grid(&mut state, 130, 12);
        assert!(state.hit_areas.grid_lines.iter().all(|line| line.row != 2));
        assert_eq!(
            shift_key(&mut state, CardId::TrendTokens, KeyCode::Right),
            UiCommand::None
        );
        assert_eq!(state.card_layout, CardLayout::default());

        // 격자 없는 프레임: 무시
        state.hit_areas = UiHitAreas::default();
        assert_eq!(
            shift_key(&mut state, CardId::KpiTokens, KeyCode::Right),
            UiCommand::None
        );
        assert_eq!(state.card_layout, CardLayout::default());
    }

    #[test]
    fn l_key_restores_the_default_grid_and_its_ratios() {
        let mut state = overview_state();
        draw_grid(&mut state, 130, 40);
        assert_eq!(
            shift_key(&mut state, CardId::EventFeed, KeyCode::Up),
            UiCommand::SaveState
        );
        assert!(
            state
                .card_layout
                .restore_grid_ratios("overview_rows_5", &[1_000, 2_000, 3_000, 4_000])
        );
        assert_ne!(state.card_layout, CardLayout::default());

        state.focused_card = None;
        assert_eq!(
            state.handle_key_event(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)),
            UiCommand::SaveState
        );
        assert_eq!(
            state.card_layout.grid_of(ActiveTab::Overview),
            default_rows(ActiveTab::Overview)
        );
        assert_eq!(state.card_layout, CardLayout::default());
    }

    #[test]
    fn grid_drag_into_another_row_joins_it_and_release_saves() {
        let mut state = overview_state();
        let base = Instant::now();
        lift_card(&mut state, CardId::EventDetail, base);
        let trend = drawn_rect(&state, CardId::TrendRequests);
        let trend_line = line_rect(&state, CardId::TrendRequests);
        assert!(trend_line.height >= 3);

        // 추이 카드 왼쪽 안: 그 카드 앞에 합류
        let drop = Position::new(trend.x + 1, trend_line.y + 2);
        drag_grid(&mut state, drop, base + Duration::from_millis(450));
        let rows = overview_rows(&state);
        let joined = rows
            .iter()
            .find(|row| row.contains(&CardId::TrendRequests))
            .unwrap();
        assert!(
            joined
                .windows(2)
                .any(|pair| pair == [CardId::EventDetail, CardId::TrendRequests]),
            "{rows:?}"
        );
        assert_eq!(
            release_grid(&mut state, drop, base + Duration::from_millis(460)),
            UiCommand::SaveState
        );
        assert!(state.toast.is_some());

        // 다음 프레임: 같은 줄
        draw_grid(&mut state, 130, 40);
        assert_eq!(
            drawn_rect(&state, CardId::EventDetail).y,
            drawn_rect(&state, CardId::TrendRequests).y
        );
    }

    #[test]
    fn grid_move_through_a_new_row_into_an_existing_row_keeps_unchanged_row_ratios() {
        let mut state = overview_state();
        let base = Instant::now();
        assert!(
            state
                .card_layout
                .restore_grid_ratios("overview_rows_5", &[1_000, 2_000, 3_000, 4_000])
        );
        assert!(
            state
                .card_layout
                .restore_grid_ratios("overview_cols_1", &[1_000, 2_000, 3_000, 4_000, 5_000])
        );
        assert!(
            state
                .card_layout
                .restore_grid_ratios("overview_cols_3", &[2_500, 5_000])
        );
        let kept = state.card_layout.grid_ratio_entries();
        lift_card(&mut state, CardId::EventDetail, base);
        let chart_line = line_rect(&state, CardId::ChartActivity);
        let trend = drawn_rect(&state, CardId::TrendRequests);
        let trend_line = line_rect(&state, CardId::TrendRequests);
        assert!(trend_line.height >= 3);

        // 차트 줄 위 테두리: 미리보기 줄 추가
        let border = Position::new(chart_line.x + 3, chart_line.y);
        drag_grid(&mut state, border, base + Duration::from_millis(450));
        assert_eq!(overview_rows(&state).len(), 6);

        // 추이 줄 안에 놓기: 줄 수 복귀
        let drop = Position::new(trend.x + 1, trend_line.y + 2);
        drag_grid(&mut state, drop, base + Duration::from_millis(460));
        assert_eq!(
            release_grid(&mut state, drop, base + Duration::from_millis(470)),
            UiCommand::SaveState
        );
        let rows = overview_rows(&state);
        assert_eq!(rows.len(), 5);
        assert_eq!(
            rows[2],
            [
                CardId::EventDetail,
                CardId::TrendRequests,
                CardId::TrendTokens
            ]
        );

        // 구성이 그대로인 줄의 비율 유지
        let entries = state.card_layout.grid_ratio_entries();
        assert_eq!(entries, kept);
    }

    #[test]
    fn grid_drop_on_a_top_or_bottom_border_creates_a_row_there() {
        let base = Instant::now();

        // 위 테두리: 차트 줄 위 새 줄
        let mut state = overview_state();
        lift_card(&mut state, CardId::EventFeed, base);
        let chart_line = line_rect(&state, CardId::ChartActivity);
        let drop = Position::new(chart_line.x + 3, chart_line.y);
        drag_grid(&mut state, drop, base + Duration::from_millis(450));
        assert_eq!(
            release_grid(&mut state, drop, base + Duration::from_millis(460)),
            UiCommand::SaveState
        );
        let rows = overview_rows(&state);
        let chart = rows
            .iter()
            .position(|row| row.contains(&CardId::ChartActivity))
            .unwrap();
        assert_eq!(rows[chart - 1], [CardId::EventFeed]);
        assert_eq!(rows.len(), 6);

        // 아래 테두리: 서버 상태 줄 아래 새 줄
        let mut state = overview_state();
        lift_card(&mut state, CardId::KpiTokens, base);
        let status_line = line_rect(&state, CardId::ServerStatus);
        let drop = Position::new(status_line.x + 3, status_line.bottom() - 1);
        drag_grid(&mut state, drop, base + Duration::from_millis(450));
        assert_eq!(
            release_grid(&mut state, drop, base + Duration::from_millis(460)),
            UiCommand::SaveState
        );
        let rows = overview_rows(&state);
        assert_eq!(rows[0], [CardId::ServerStatus]);
        assert_eq!(rows[1], [CardId::KpiTokens]);
        assert_eq!(rows.len(), 6);
    }

    #[test]
    fn grid_drop_that_empties_a_row_removes_the_row() {
        let mut state = overview_state();
        let base = Instant::now();
        let before = overview_rows(&state);
        lift_card(&mut state, CardId::ServerStatus, base);
        let feed_line = line_rect(&state, CardId::EventFeed);
        assert!(feed_line.height >= 3);

        // 피드 줄 오른쪽 끝 안: 마지막 카드 뒤
        let drop = Position::new(feed_line.right() - 2, feed_line.y + 1);
        drag_grid(&mut state, drop, base + Duration::from_millis(450));
        assert_eq!(
            release_grid(&mut state, drop, base + Duration::from_millis(460)),
            UiCommand::SaveState
        );
        let rows = overview_rows(&state);
        assert_eq!(rows.len(), before.len() - 1);
        assert_eq!(rows[0], before[1]);
        assert_eq!(
            rows.last().unwrap()[..],
            [CardId::EventFeed, CardId::EventDetail, CardId::ServerStatus]
        );
        draw_grid(&mut state, 130, 40);
        assert_eq!(
            drawn_rect(&state, CardId::ServerStatus).y,
            drawn_rect(&state, CardId::EventFeed).y
        );
    }

    #[test]
    fn grid_drop_of_a_seventh_card_into_a_full_row_keeps_the_preview() {
        let mut state = overview_state();
        let base = Instant::now();
        assert_eq!(
            overview_rows(&state)[1].len(),
            crate::tui::layout::MAX_SLOTS
        );
        lift_card(&mut state, CardId::EventFeed, base);
        let kpi_line = line_rect(&state, CardId::KpiRequests);
        let chart_line = line_rect(&state, CardId::ChartActivity);
        drag_grid(
            &mut state,
            Position::new(chart_line.x + 3, chart_line.y),
            base + Duration::from_millis(450),
        );
        let preview = state.card_layout.grid_of(ActiveTab::Overview);
        assert_ne!(preview, default_rows(ActiveTab::Overview));

        // 여섯 장 줄 안: 거부, 미리보기 그대로
        drag_grid(
            &mut state,
            Position::new(kpi_line.x + 3, kpi_line.y + 1),
            base + Duration::from_millis(460),
        );
        assert_eq!(state.card_layout.grid_of(ActiveTab::Overview), preview);
        assert!(state.grid_move.is_some());

        // Esc: 누르기 전 격자
        assert_eq!(
            state.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            UiCommand::None
        );
        assert!(state.grid_move.is_none());
        assert!(state.card_move.is_none());
        assert_eq!(state.card_layout, CardLayout::default());
    }

    #[test]
    fn grid_move_cancel_paths_restore_the_baseline_grid_and_ratios_exactly() {
        let base = Instant::now();
        let later = base + Duration::from_millis(500);
        for cancel in ["esc", "resize", "tab"] {
            let mut state = overview_state();
            assert!(
                state
                    .card_layout
                    .restore_grid_ratios("overview_rows_5", &[1_000, 2_000, 3_000, 4_000])
            );
            let before = state.card_layout;
            lift_card(&mut state, CardId::EventDetail, base);
            let chart_line = line_rect(&state, CardId::ChartActivity);
            let drop = Position::new(chart_line.x + 3, chart_line.y);
            drag_grid(&mut state, drop, base + Duration::from_millis(450));
            assert_ne!(state.card_layout, before, "{cancel}");

            // 취소 경로별 복원
            let command = match cancel {
                "esc" => state.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
                "resize" => state.handle_resize(),
                _ => state.switch_tab(ActiveTab::Tools),
            };
            assert_eq!(command, UiCommand::None, "{cancel}");
            assert_eq!(state.card_layout, before, "{cancel}");
            assert!(state.grid_move.is_none(), "{cancel}");
            assert!(state.card_move.is_none(), "{cancel}");
            assert!(state.card_press.is_none(), "{cancel}");

            // 늦게 온 놓기
            let up = MouseEventKind::Up(MouseButton::Left);
            assert_eq!(
                pointer(&mut state, up, drop, later + Duration::from_millis(10)),
                UiCommand::None,
                "{cancel}"
            );
            assert_eq!(state.card_layout, before, "{cancel}");
        }
    }

    // 놓기 놓침(버튼 없는 움직임)과 새 누름은 놓은 것과 같이 미리보기 격자를 확정
    #[test]
    fn grid_lost_release_and_new_press_commit_the_preview_like_a_release() {
        let base = Instant::now();
        let later = base + Duration::from_millis(500);
        let away = Position::new(3, 3);
        for ending in [
            MouseEventKind::Moved,
            MouseEventKind::Down(MouseButton::Left),
        ] {
            let mut state = overview_state();
            lift_card(&mut state, CardId::EventDetail, base);
            let chart_line = line_rect(&state, CardId::ChartActivity);
            let drop = Position::new(chart_line.x + 3, chart_line.y);
            drag_grid(&mut state, drop, base + Duration::from_millis(450));
            let preview = state.card_layout.grid_of(ActiveTab::Overview);
            assert_ne!(preview, default_rows(ActiveTab::Overview), "{ending:?}");

            // 확정: 알림과 저장 요청, 그 누름은 새 대기를 만들지 않음
            assert_eq!(
                pointer(&mut state, ending, away, later),
                UiCommand::SaveState,
                "{ending:?}"
            );
            assert_eq!(
                state.card_layout.grid_of(ActiveTab::Overview),
                preview,
                "{ending:?}"
            );
            assert!(state.grid_move.is_none(), "{ending:?}");
            assert!(state.card_move.is_none(), "{ending:?}");
            assert!(state.card_press.is_none(), "{ending:?}");
            assert!(state.toast.is_some(), "{ending:?}");

            // 늦게 온 놓기는 아무 일도 하지 않음
            let up = MouseEventKind::Up(MouseButton::Left);
            assert_eq!(
                pointer(&mut state, up, drop, later + Duration::from_millis(10)),
                UiCommand::None,
                "{ending:?}"
            );
            assert_eq!(
                state.card_layout.grid_of(ActiveTab::Overview),
                preview,
                "{ending:?}"
            );
        }
    }

    // 자기 줄 위 테두리(제목 줄)는 줄 안, 위 줄 아래 테두리는 두 줄 사이 새 줄, 자기 줄 아래 테두리는 아래 새 줄
    #[test]
    fn grid_title_row_slide_reorders_inside_the_row_and_border_drops_create_rows() {
        let base = Instant::now();
        let chart_row = |state: &AppState| {
            overview_rows(state)
                .into_iter()
                .find(|row| row.contains(&CardId::ChartTools))
                .unwrap()
        };

        // 1. 세 장 차트 줄의 제목 줄을 따라 밀기: 이웃 앞뒤로만, 새 줄 없음
        let mut state = overview_state();
        let row_count = overview_rows(&state).len();
        lift_card(&mut state, CardId::ChartActivity, base);
        let tokens = drawn_rect(&state, CardId::ChartTokens);
        let tools = drawn_rect(&state, CardId::ChartTools);
        let title_y = line_rect(&state, CardId::ChartActivity).y;
        drag_grid(
            &mut state,
            Position::new(tokens.right() - 2, title_y),
            base + Duration::from_millis(450),
        );
        assert_eq!(
            chart_row(&state),
            [
                CardId::ChartTokens,
                CardId::ChartActivity,
                CardId::ChartTools
            ]
        );
        assert_eq!(overview_rows(&state).len(), row_count);
        drag_grid(
            &mut state,
            Position::new(tools.right() - 2, title_y),
            base + Duration::from_millis(460),
        );
        assert_eq!(
            chart_row(&state),
            [
                CardId::ChartTokens,
                CardId::ChartTools,
                CardId::ChartActivity
            ]
        );
        assert_eq!(overview_rows(&state).len(), row_count);
        let at = Position::new(tools.right() - 2, title_y);
        assert_eq!(
            release_grid(&mut state, at, base + Duration::from_millis(470)),
            UiCommand::SaveState
        );
        assert_eq!(overview_rows(&state).len(), row_count);

        // 2. 위 줄(추이) 아래 테두리: 두 줄 사이 새 줄
        let mut state = overview_state();
        lift_card(&mut state, CardId::ChartActivity, base);
        let trend_line = line_rect(&state, CardId::TrendRequests);
        let drop = Position::new(trend_line.x + 3, trend_line.bottom() - 1);
        drag_grid(&mut state, drop, base + Duration::from_millis(450));
        assert_eq!(
            release_grid(&mut state, drop, base + Duration::from_millis(460)),
            UiCommand::SaveState
        );
        let rows = overview_rows(&state);
        let trend = rows
            .iter()
            .position(|row| row.contains(&CardId::TrendRequests))
            .unwrap();
        assert_eq!(rows[trend + 1], [CardId::ChartActivity]);
        assert_eq!(rows[trend + 2], [CardId::ChartTokens, CardId::ChartTools]);
        assert_eq!(rows.len(), row_count + 1);

        // 3. 자기 줄 아래 테두리: 아래 새 줄
        let mut state = overview_state();
        lift_card(&mut state, CardId::ChartActivity, base);
        let chart_line = line_rect(&state, CardId::ChartActivity);
        let drop = Position::new(chart_line.x + 3, chart_line.bottom() - 1);
        drag_grid(&mut state, drop, base + Duration::from_millis(450));
        assert_eq!(
            release_grid(&mut state, drop, base + Duration::from_millis(460)),
            UiCommand::SaveState
        );
        let rows = overview_rows(&state);
        let chart = rows
            .iter()
            .position(|row| row.contains(&CardId::ChartTools))
            .unwrap();
        assert_eq!(rows[chart], [CardId::ChartTokens, CardId::ChartTools]);
        assert_eq!(rows[chart + 1], [CardId::ChartActivity]);
        assert_eq!(rows.len(), row_count + 1);
    }

    #[test]
    fn grid_release_at_the_start_or_back_over_its_own_card_saves_nothing() {
        let base = Instant::now();
        let mut state = overview_state();
        let title = lift_card(&mut state, CardId::EventDetail, base);
        assert_eq!(
            release_grid(&mut state, title, base + Duration::from_millis(450)),
            UiCommand::None
        );
        assert_eq!(state.card_layout, CardLayout::default());
        assert!(state.toast.is_none());

        // 다른 줄에서 자기 카드 위로 복귀
        let mut state = overview_state();
        lift_card(&mut state, CardId::EventDetail, base);
        let own = drawn_rect(&state, CardId::EventDetail);
        let chart_line = line_rect(&state, CardId::ChartActivity);
        drag_grid(
            &mut state,
            Position::new(chart_line.x + 3, chart_line.y),
            base + Duration::from_millis(450),
        );
        assert_ne!(
            state.card_layout.grid_of(ActiveTab::Overview),
            default_rows(ActiveTab::Overview)
        );
        let back = Position::new(own.x + 2, own.y + 1);
        drag_grid(&mut state, back, base + Duration::from_millis(460));
        assert_eq!(
            state.card_layout.grid_of(ActiveTab::Overview),
            default_rows(ActiveTab::Overview)
        );
        assert_eq!(
            release_grid(&mut state, back, base + Duration::from_millis(470)),
            UiCommand::None
        );
        assert_eq!(state.card_layout, CardLayout::default());
        assert!(state.toast.is_none());
    }
}
