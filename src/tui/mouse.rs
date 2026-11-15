//! tui/mouse.rs
//! 마우스 입력 상태 전이: 호버 강조, 클릭 포커스·선택·정렬·버튼, 카드 경계·열 경계 드래그, 휠 스크롤.

use crate::tui::cards::{
    ColumnDrag, MAX_TABLE_COLUMNS, ScrollDrag, scroll_for_thumb, scroll_thumb,
};
use crate::tui::layout::{SplitAxis, SplitDrag};
use crate::tui::sort::SortChange;
use crate::tui::state::{AppState, ToastLevel, UiCommand};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;
use std::time::{Duration, Instant};

// 휠 한 칸에 움직일 줄 수, 열 경계 더블클릭 인정 간격
const WHEEL_STEP: isize = 3;
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);

impl AppState {
    // 1. 마우스 이벤트 분배 ---------------------------------------------------
    // 직전 그리기에서 등록한 영역으로 판정, 반환 = 실행 요청
    pub fn handle_mouse_event(&mut self, mouse: MouseEvent) -> UiCommand {
        // 도움말이 열려 있으면 클릭(닫기)과 버튼 뗌만 받음
        if self.show_help && !matches!(mouse.kind, MouseEventKind::Down(_) | MouseEventKind::Up(_))
        {
            return UiCommand::None;
        }
        let position = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Moved => {
                self.track_hover(position);
                UiCommand::None
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.scroll_drag.is_some() {
                    self.drag_scrollbar(position);
                } else if self.column_drag.is_some() {
                    self.drag_column(position);
                } else {
                    self.drag_split(position);
                }
                UiCommand::None
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.split_drag = None;
                self.scroll_drag = None;

                // 끌지 않고 떼 경계 클릭만 더블클릭 후보로 기록
                if let Some(drag) = self.column_drag.take() {
                    self.last_column_click = if drag.moved {
                        None
                    } else {
                        Some((drag.card, drag.column, Instant::now()))
                    };
                }
                UiCommand::None
            }
            MouseEventKind::ScrollUp => {
                self.wheel_card(position, -WHEEL_STEP);
                UiCommand::None
            }
            MouseEventKind::ScrollDown => {
                self.wheel_card(position, WHEEL_STEP);
                UiCommand::None
            }
            MouseEventKind::Down(MouseButton::Left) => self.press_left(position),
            _ => UiCommand::None,
        }
    }

    // 2. 호버 강조 ------------------------------------------------------------
    // 스크롤바 트랙 위면 썸만, 카드 경계·열 경계 위면 그 경계만, 아니면 마우스 아래 카드
    fn track_hover(&mut self, position: Position) {
        if self.show_help {
            return;
        }
        let track_card = self
            .hit_areas
            .scroll_hits
            .iter()
            .find(|hit| hit.track.contains(position))
            .map(|hit| hit.card);
        if track_card.is_some() {
            self.scroll_hover = track_card;
            self.hover_card = track_card;
            self.split_hover = None;
            self.column_hover = None;
            return;
        }
        self.scroll_hover = None;
        self.column_hover = self
            .hit_areas
            .column_hits
            .iter()
            .find(|hit| hit.grab.contains(position))
            .map(|hit| (hit.card, hit.column));
        let split = self
            .hit_areas
            .split_hits
            .iter()
            .find(|hit| hit.grab.contains(position))
            .map(|hit| (hit.group, hit.index));
        let card = if split.is_some() {
            None
        } else {
            self.hit_areas
                .card_rects
                .iter()
                .find(|(_, rect)| rect.contains(position))
                .map(|(card, _)| *card)
        };
        self.split_hover = split;
        self.hover_card = card;
    }

    // 3. 경계 드래그 ----------------------------------------------------------
    // 잡은 경계를 포인터 위치로 이동, 앞뒤 카드 크기를 알림으로 표시
    fn drag_split(&mut self, position: Position) {
        let Some(drag) = self.split_drag else {
            return;
        };
        let grabbed = self
            .hit_areas
            .split_hits
            .iter()
            .find(|hit| hit.group == drag.group && hit.index == drag.index)
            .copied();
        let Some(hit) = grabbed else {
            self.split_drag = None;
            return;
        };
        let (coord, unit) = match hit.axis {
            SplitAxis::Columns => (i32::from(position.x), "열"),
            SplitAxis::Rows => (i32::from(position.y), "행"),
        };
        if let Some((before, after)) = self.card_layout.drag_cut(&hit, coord - drag.grip) {
            self.show_toast(
                &format!("카드 크기 {before}{unit} : {after}{unit} (l 기본 배치)"),
                ToastLevel::Info,
            );
        }
    }

    // 4. 스크롤바 드래그 ------------------------------------------------------
    // 썸을 포인터 위치로 이동, 트랙 밖은 끝으로 제한 (선택 행은 유지)
    fn drag_scrollbar(&mut self, position: Position) {
        let Some(drag) = self.scroll_drag else {
            return;
        };
        let grabbed = self
            .hit_areas
            .scroll_hits
            .iter()
            .find(|hit| hit.card == drag.card)
            .copied();
        let Some(hit) = grabbed else {
            self.scroll_drag = None;
            return;
        };
        let thumb_top = i32::from(position.y) - i32::from(hit.track.y) - i32::from(drag.grip);
        let thumb_start = u16::try_from(thumb_top.max(0)).unwrap_or(u16::MAX);
        let scroll = scroll_for_thumb(hit.total, hit.visible, hit.track.height, thumb_start);
        self.set_card_scroll(drag.card, scroll);
    }

    // 5. 휠 스크롤 ------------------------------------------------------------
    // 마우스 아래 카드의 화면 위치만 이동 (선택 행은 유지)
    fn wheel_card(&mut self, position: Position, delta: isize) {
        let card = self
            .hit_areas
            .card_rects
            .iter()
            .find(|(_, rect)| rect.contains(position))
            .map(|(card, _)| *card);
        if let Some(card) = card {
            self.scroll_card_view(card, delta);
        }
    }

    // 6. 왼쪽 버튼 누름 -------------------------------------------------------
    // 우선순위: 도움말 닫기, 스크롤바, 카드 경계 잡기, 열 경계 잡기, 카드 포커스, 버튼, 탭, 머리글 정렬, 행 선택
    fn press_left(&mut self, position: Position) -> UiCommand {
        // 1. 도움말은 클릭 한 번에 닫기
        if self.show_help {
            self.show_help = false;
            return UiCommand::None;
        }

        // 2. 스크롤바: 썸 잡기, 트랙 클릭은 한 화면 이동 (경계 잡기보다 우선)
        self.scroll_drag = None;
        let track_hit = self
            .hit_areas
            .scroll_hits
            .iter()
            .find(|hit| hit.track.contains(position))
            .copied();
        if let Some(hit) = track_hit {
            self.focused_card = Some((self.tab, hit.card));
            let offset = position.y - hit.track.y;
            let page = hit.visible as isize;
            if let Some(thumb) = scroll_thumb(hit.total, hit.visible, hit.scroll, hit.track.height)
            {
                if offset < thumb.start {
                    self.scroll_card_view(hit.card, -page);
                } else if offset >= thumb.start + thumb.len {
                    self.scroll_card_view(hit.card, page);
                } else {
                    self.scroll_drag = Some(ScrollDrag {
                        card: hit.card,
                        grip: offset - thumb.start,
                    });
                }
            }
            return UiCommand::None;
        }

        // 3. 카드 경계 잡기 (행·머리글 클릭보다 우선)
        self.split_drag = None;
        let grabbed = self
            .hit_areas
            .split_hits
            .iter()
            .find(|hit| hit.grab.contains(position))
            .copied();
        if let Some(hit) = grabbed {
            let coord = match hit.axis {
                SplitAxis::Columns => position.x,
                SplitAxis::Rows => position.y,
            };
            let cut_at = hit.origin + hit.cuts[hit.index];
            self.split_drag = Some(SplitDrag {
                group: hit.group,
                index: hit.index,
                grip: i32::from(coord) - i32::from(cut_at),
            });
            self.split_hover = Some((hit.group, hit.index));
            self.show_toast(
                "카드 경계 드래그: 놓으면 고정, l 기본 배치",
                ToastLevel::Info,
            );
            return UiCommand::None;
        }

        // 4. 열 경계 잡기: 머리글 정렬보다 우선, 끌지 않고 떼 직후 다시 누르면 자동 폭 복원
        self.column_drag = None;
        let column_hit = self
            .hit_areas
            .column_hits
            .iter()
            .find(|hit| hit.grab.contains(position))
            .copied();
        if let Some(hit) = column_hit {
            self.focused_card = Some((self.tab, hit.card));
            let double_click = self.last_column_click.is_some_and(|(card, column, at)| {
                card == hit.card && column == hit.column && at.elapsed() < DOUBLE_CLICK_WINDOW
            });
            self.last_column_click = None;
            if double_click {
                self.column_widths[hit.card as usize] = [None; MAX_TABLE_COLUMNS];
                self.show_toast("열 폭 자동 복원", ToastLevel::Info);
                return UiCommand::None;
            }
            self.column_drag = Some(ColumnDrag {
                card: hit.card,
                column: hit.column,
                start_x: position.x,
                start_width: hit.width,
                moved: false,
            });
            self.column_hover = Some((hit.card, hit.column));
            self.show_toast(
                "열 경계 드래그: 열 폭 조절, 더블클릭 자동 폭",
                ToastLevel::Info,
            );
            return UiCommand::None;
        }

        // 5. 카드 클릭: 테두리 강조 대상 변경, 카드 안 행·머리글 처리는 아래에서 계속
        let clicked_card = self
            .hit_areas
            .card_rects
            .iter()
            .find(|(_, rect)| rect.contains(position))
            .map(|(card, _)| *card);
        if let Some(card) = clicked_card {
            self.focused_card = Some((self.tab, card));
        }

        // 6. 하단·날짜 버튼
        let button = self
            .hit_areas
            .button_rects
            .iter()
            .find(|(_, rect)| rect.contains(position))
            .map(|(action, _)| *action);
        if let Some(action) = button {
            return self.perform_action(action);
        }

        // 7. 탭 전환
        let tab = self
            .hit_areas
            .tab_rects
            .iter()
            .find(|(_, rect)| rect.contains(position))
            .map(|(tab, _)| *tab);
        if let Some(tab) = tab {
            return self.switch_tab(tab);
        }

        // 8. 표 머리글 클릭 정렬 (오름 -> 내림 -> 기본 순환, 선택 행은 같은 항목을 따라감)
        let header = self
            .hit_areas
            .header_cells
            .iter()
            .find(|hit| hit.rect.contains(position))
            .map(|hit| (hit.card, hit.column));
        if let Some((card, column)) = header {
            self.change_card_sort(card, SortChange::SelectColumn(column));
            return UiCommand::None;
        }

        // 9. 표 본문 행 클릭: 선택 행 변경, 하단 상세 카드가 따라감
        let row_hit = self
            .hit_areas
            .row_hits
            .iter()
            .find(|hit| hit.rect.contains(position))
            .copied();
        if let Some(hit) = row_hit {
            let index = hit.offset + usize::from(position.y - hit.rect.y);
            if index < hit.len {
                self.select_card_row(hit.card, index);
            }
        }
        UiCommand::None
    }

    // 7. 열 경계 드래그 -------------------------------------------------------
    // 왼쪽 열 폭 = 누른 순간 폭 + 포인터 이동량 (열 최소·최대 폭으로 제한), 움직이지 않은 클릭은 폭을 고정하지 않음
    fn drag_column(&mut self, position: Position) {
        let Some(drag) = self.column_drag else {
            return;
        };
        let grabbed = self
            .hit_areas
            .column_hits
            .iter()
            .find(|hit| hit.card == drag.card && hit.column == drag.column)
            .copied();
        let Some(hit) = grabbed else {
            self.column_drag = None;
            return;
        };
        if !drag.moved && position.x == drag.start_x {
            return;
        }
        let wanted = i32::from(drag.start_width) + i32::from(position.x) - i32::from(drag.start_x);
        let width = wanted.clamp(i32::from(hit.min), i32::from(hit.max.max(hit.min))) as u16;
        self.column_widths[drag.card as usize][drag.column] = Some(width);
        self.column_drag = Some(ColumnDrag {
            moved: true,
            ..drag
        });
        self.show_toast(
            &format!("열 폭 {width}칸 (더블클릭 자동 폭)"),
            ToastLevel::Info,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::cards::{CardId, ColumnHit, HeaderHit, ScrollHit, UiAction};
    use crate::tui::layout::SplitGroup;
    use crate::tui::sort::SortState;
    use crate::tui::state::ActiveTab;
    use crate::tui::test_support::{
        sample_state_with_long_report, sample_state_with_many_events, sample_state_with_many_tools,
        sample_state_with_snapshot, sample_state_with_tables,
    };
    use crate::tui::theme;
    use crate::tui::views::render_frame;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn draw(state: &mut AppState, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_frame(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(state: &mut AppState, column: u16, row: u16) -> UiCommand {
        state.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), column, row))
    }

    fn card_rect(state: &AppState, card: CardId) -> Rect {
        state
            .hit_areas
            .card_rects
            .iter()
            .find(|(id, _)| *id == card)
            .map(|(_, rect)| *rect)
            .unwrap()
    }

    fn scroll_hit(state: &AppState, card: CardId) -> ScrollHit {
        state
            .hit_areas
            .scroll_hits
            .iter()
            .find(|hit| hit.card == card)
            .copied()
            .unwrap()
    }

    fn drag_to(state: &mut AppState, column: u16, row: u16) {
        state.handle_mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left), column, row));
    }

    #[test]
    fn clicking_track_below_thumb_pages_down_by_visible() {
        let mut state = sample_state_with_many_tools(40);
        draw(&mut state, 100, 30);
        let hit = scroll_hit(&state, CardId::ToolTable);
        let table = CardId::ToolTable as usize;
        assert_eq!((hit.total, hit.scroll), (40, 0));

        // 썸 아래 트랙: 한 화면 아래, 선택 행은 그대로
        assert_eq!(
            click(&mut state, hit.track.x, hit.track.bottom() - 1),
            UiCommand::None
        );
        assert_eq!(state.card_views[table].scroll, hit.visible);
        assert_eq!(state.card_views[table].selected, 0);
        assert_eq!(
            state.focused_card,
            Some((ActiveTab::Tools, CardId::ToolTable))
        );
        assert!(state.scroll_drag.is_none());

        // 계속 눌러도 최대 스크롤에서 멈춤
        draw(&mut state, 100, 30);
        click(&mut state, hit.track.x, hit.track.bottom() - 1);
        draw(&mut state, 100, 30);
        click(&mut state, hit.track.x, hit.track.bottom() - 1);
        assert_eq!(state.card_views[table].scroll, 40 - hit.visible);
    }

    #[test]
    fn clicking_track_above_thumb_pages_up() {
        let mut state = sample_state_with_many_tools(40);
        let table = CardId::ToolTable as usize;
        state.card_views[table].scroll = 24;
        draw(&mut state, 100, 30);
        let hit = scroll_hit(&state, CardId::ToolTable);
        assert_eq!(hit.scroll, 24);

        click(&mut state, hit.track.x, hit.track.y);
        assert_eq!(state.card_views[table].scroll, 24 - hit.visible);
        assert!(state.scroll_drag.is_none());
    }

    #[test]
    fn dragging_thumb_to_bottom_reaches_max_scroll_and_clamps_past_track() {
        let mut state = sample_state_with_many_tools(40);
        draw(&mut state, 100, 30);
        let hit = scroll_hit(&state, CardId::ToolTable);
        let table = CardId::ToolTable as usize;

        // 썸(맨 위)을 잡으면 드래그 시작, 스크롤은 그대로
        click(&mut state, hit.track.x, hit.track.y);
        assert_eq!(
            state.scroll_drag,
            Some(ScrollDrag {
                card: CardId::ToolTable,
                grip: 0
            })
        );
        assert_eq!(state.card_views[table].scroll, 0);

        // 트랙 끝, 트랙 밖 아래로 끌어도 최대 스크롤
        drag_to(&mut state, hit.track.x, hit.track.bottom() - 1);
        assert_eq!(state.card_views[table].scroll, 40 - hit.visible);
        state.card_views[table].scroll = 3;
        drag_to(&mut state, hit.track.x, hit.track.bottom() + 10);
        assert_eq!(state.card_views[table].scroll, 40 - hit.visible);

        // 트랙 위로 끌면 처음으로, 버튼을 놓으면 드래그 종료
        drag_to(&mut state, hit.track.x, 0);
        assert_eq!(state.card_views[table].scroll, 0);
        state.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), hit.track.x, 0));
        assert!(state.scroll_drag.is_none());
    }

    #[test]
    fn dragging_thumb_keeps_the_grip_offset() {
        let mut state = sample_state_with_many_tools(40);
        let table = CardId::ToolTable as usize;
        state.card_views[table].scroll = 12;
        draw(&mut state, 100, 30);
        let hit = scroll_hit(&state, CardId::ToolTable);
        let track = hit.track.height;
        let thumb =
            crate::tui::cards::scroll_thumb(hit.total, hit.visible, hit.scroll, track).unwrap();

        // 썸 중간을 잡고 마우스를 두 칸 내리면 썸 시작도 두 칸 내려감
        let grab_row = hit.track.y + thumb.start + 1;
        click(&mut state, hit.track.x, grab_row);
        assert_eq!(state.scroll_drag.unwrap().grip, 1);
        drag_to(&mut state, hit.track.x, grab_row + 2);
        let expected = crate::tui::cards::scroll_for_thumb(40, hit.visible, track, thumb.start + 2);
        assert_eq!(state.card_views[table].scroll, expected);
        assert!(expected > 12);
    }

    #[test]
    fn scrollbar_drag_keeps_selected_row() {
        let mut state = sample_state_with_many_events(60);
        let feed = CardId::EventFeed as usize;
        state.card_views[feed].selected = 2;
        draw(&mut state, 130, 40);
        let hit = scroll_hit(&state, CardId::EventFeed);

        click(&mut state, hit.track.x, hit.track.y);
        drag_to(&mut state, hit.track.x, hit.track.bottom() - 1);
        assert_eq!(state.card_views[feed].scroll, 60 - hit.visible);
        assert_eq!(state.card_views[feed].selected, 2);
    }

    #[test]
    fn outline_scrollbar_click_beats_column_split_grab() {
        let mut state = sample_state_with_long_report(40);
        draw(&mut state, 110, 30);
        let hit = scroll_hit(&state, CardId::ReportOutline);
        let grab = state.hit_areas.split_hits[0].grab;

        // 트랙의 테두리 칸이 경계 잡기 영역에 들어 있어도 스크롤바가 이김
        assert!(grab.contains(Position::new(hit.track.right() - 1, hit.track.y)));
        click(&mut state, hit.track.right() - 1, hit.track.y);
        assert!(state.split_drag.is_none());
        assert!(state.scroll_drag.is_some());
        state.scroll_drag = None;
        click(&mut state, hit.track.x, hit.track.y);
        assert!(state.split_drag.is_none());
        assert!(state.scroll_drag.is_some());
        assert_eq!(
            state.focused_card,
            Some((ActiveTab::Report, CardId::ReportOutline))
        );

        // 남은 트랙 클릭은 목차 선택을 바꾸지 않음
        let outline = CardId::ReportOutline as usize;
        assert_eq!(state.card_views[outline].selected, 0);
    }

    #[test]
    fn hovering_track_highlights_thumb() {
        let mut state = sample_state_with_many_tools(40);
        draw(&mut state, 100, 30);
        let hit = scroll_hit(&state, CardId::ToolTable);
        let calm = draw(&mut state, 100, 30);
        assert_eq!(
            calm.cell((hit.track.x, hit.track.y)).unwrap().fg,
            theme::SCROLL_THUMB_FOCUS
        );

        // 트랙 위: 썸 호버색, 나머지 트랙은 밝은 ░, 경계·카드 호버는 따로 표시 안 함
        state.handle_mouse_event(mouse(MouseEventKind::Moved, hit.track.x, hit.track.y));
        assert_eq!(state.scroll_hover, Some(CardId::ToolTable));
        assert_eq!(state.hover_card, Some(CardId::ToolTable));
        assert_eq!(state.split_hover, None);
        let hovered = draw(&mut state, 100, 30);
        assert_eq!(
            hovered.cell((hit.track.x, hit.track.y)).unwrap().fg,
            theme::SCROLL_THUMB_HOVER
        );
        let rest = hovered.cell((hit.track.x, hit.track.bottom() - 1)).unwrap();
        assert_eq!(rest.symbol(), "░");
        assert_eq!(rest.fg, theme::SCROLL_TRACK_HOT);

        // 테두리 열은 썸·트랙 호버와 무관하게 포커스 테두리 유지
        let border = hovered
            .cell((hit.track.right() - 1, hit.track.bottom() - 1))
            .unwrap();
        assert_eq!(border.symbol(), "┃");
        assert_eq!(border.fg, theme::BORDER_FOCUS);

        // 벗어나면 원래 색
        state.handle_mouse_event(mouse(MouseEventKind::Moved, 1, 0));
        assert_eq!(state.scroll_hover, None);
        let left = draw(&mut state, 100, 30);
        assert_eq!(
            left.cell((hit.track.x, hit.track.y)).unwrap().fg,
            theme::SCROLL_THUMB_FOCUS
        );
    }

    #[test]
    fn unfocused_card_thumb_is_dim_and_drag_uses_active_color() {
        let mut state = sample_state_with_many_events(60);
        state.focused_card = Some((ActiveTab::Overview, CardId::KpiCache));
        draw(&mut state, 130, 40);
        let hit = scroll_hit(&state, CardId::EventFeed);
        let calm = draw(&mut state, 130, 40);
        assert_eq!(
            calm.cell((hit.track.x, hit.track.y)).unwrap().fg,
            theme::SCROLL_THUMB
        );

        // 드래그 중에는 포인터가 트랙 밖이어도 강조색 유지
        click(&mut state, hit.track.x, hit.track.y);
        drag_to(&mut state, 0, hit.track.y + 3);
        let dragged = draw(&mut state, 130, 40);
        let hit = scroll_hit(&state, CardId::EventFeed);
        let thumb_row = hit.track.y
            + crate::tui::cards::scroll_thumb(hit.total, hit.visible, hit.scroll, hit.track.height)
                .unwrap()
                .start;
        assert_eq!(
            dragged.cell((hit.track.x, thumb_row)).unwrap().fg,
            theme::SCROLL_THUMB_DRAG
        );
    }

    #[test]
    fn release_ends_scroll_drag_and_tab_switch_clears_it() {
        let mut state = sample_state_with_many_tools(40);
        draw(&mut state, 100, 30);
        let hit = scroll_hit(&state, CardId::ToolTable);

        click(&mut state, hit.track.x, hit.track.y);
        assert!(state.scroll_drag.is_some());
        state.handle_mouse_event(mouse(
            MouseEventKind::Up(MouseButton::Left),
            hit.track.x,
            hit.track.y,
        ));
        assert!(state.scroll_drag.is_none());

        click(&mut state, hit.track.x, hit.track.y);
        state.handle_mouse_event(mouse(MouseEventKind::Moved, hit.track.x, hit.track.y));
        assert!(state.scroll_drag.is_some());
        assert!(state.scroll_hover.is_some());
        state.switch_tab(ActiveTab::Hooks);
        assert!(state.scroll_drag.is_none());
        assert!(state.scroll_hover.is_none());
    }

    #[test]
    fn wheel_is_ignored_while_help_is_open() {
        let mut state = sample_state_with_many_tools(40);
        draw(&mut state, 100, 30);
        let table = card_rect(&state, CardId::ToolTable);
        let index = CardId::ToolTable as usize;

        state.show_help = true;
        state.handle_mouse_event(mouse(MouseEventKind::ScrollDown, table.x + 3, table.y + 2));
        assert_eq!(state.card_views[index].scroll, 0);
        drag_to(&mut state, table.x + 3, table.y + 2);
        assert!(state.toast.is_none());

        // 도움말을 닫으면 휠 동작 복구
        click(&mut state, table.x + 3, table.y + 2);
        assert!(!state.show_help);
        state.handle_mouse_event(mouse(MouseEventKind::ScrollDown, table.x + 3, table.y + 2));
        assert_eq!(state.card_views[index].scroll, 3);
    }

    #[test]
    fn clicking_a_tab_switches_tab_and_returns_its_load_request() {
        let mut state = sample_state_with_tables();
        draw(&mut state, 120, 40);
        let tools = state
            .hit_areas
            .tab_rects
            .iter()
            .find(|(tab, _)| *tab == ActiveTab::Tools)
            .map(|(_, rect)| *rect)
            .unwrap();
        assert_eq!(click(&mut state, tools.x + 1, tools.y), UiCommand::None);
        assert_eq!(state.tab, ActiveTab::Tools);

        draw(&mut state, 120, 40);
        let settings = state
            .hit_areas
            .tab_rects
            .iter()
            .find(|(tab, _)| *tab == ActiveTab::Settings)
            .map(|(_, rect)| *rect)
            .unwrap();
        assert_eq!(
            click(&mut state, settings.x, settings.y),
            UiCommand::LoadSettings
        );
        assert_eq!(state.tab, ActiveTab::Settings);
    }

    #[test]
    fn clicking_a_card_focuses_it_only_for_the_current_tab() {
        let mut state = sample_state_with_snapshot();
        draw(&mut state, 130, 40);
        let cache = card_rect(&state, CardId::KpiCache);
        click(&mut state, cache.x + 2, cache.y + 1);
        assert_eq!(
            state.focused_card,
            Some((ActiveTab::Overview, CardId::KpiCache))
        );
        assert_eq!(state.focused_card_id(), CardId::KpiCache);

        // 포커스 카드 테두리는 굵게
        let buffer = draw(&mut state, 130, 40);
        let corner = buffer.cell((cache.x, cache.y)).unwrap();
        assert_eq!(corner.symbol(), "┏");
        assert_eq!(corner.fg, theme::BORDER_FOCUS);

        state.switch_tab(ActiveTab::Tools);
        assert_eq!(state.focused_card_id(), CardId::ToolTable);
    }

    #[test]
    fn clicking_a_table_row_selects_and_highlights_it() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Tools;
        draw(&mut state, 100, 30);
        let hit = state
            .hit_areas
            .row_hits
            .iter()
            .find(|hit| hit.card == CardId::ToolTable)
            .copied()
            .unwrap();
        assert_eq!(hit.len, 3);

        // 둘째 행 클릭
        click(&mut state, hit.rect.x + 2, hit.rect.y + 1);
        assert_eq!(state.card_views[CardId::ToolTable as usize].selected, 1);

        // 빈 영역(행 수 밖)은 선택을 바꾸지 않음
        click(&mut state, hit.rect.x + 2, hit.rect.y + 6);
        assert_eq!(state.card_views[CardId::ToolTable as usize].selected, 1);

        let buffer = draw(&mut state, 100, 30);
        let selected_cell = buffer.cell((hit.rect.x + 2, hit.rect.y + 1)).unwrap();
        assert_eq!(selected_cell.bg, theme::BG_SELECTED);
        let other_cell = buffer.cell((hit.rect.x + 2, hit.rect.y)).unwrap();
        assert_ne!(other_cell.bg, theme::BG_SELECTED);
    }

    fn header_hit(state: &AppState, card: CardId, column: usize) -> HeaderHit {
        state
            .hit_areas
            .header_cells
            .iter()
            .find(|hit| hit.card == card && hit.column == column)
            .copied()
            .unwrap_or_else(|| panic!("{card:?} 열 {column} 머리글 없음"))
    }

    fn card_sort(state: &AppState, card: CardId) -> SortState {
        state.card_sorts[card as usize]
    }

    #[test]
    fn clicking_a_header_cell_sorts_tools_by_that_column() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Tools;
        draw(&mut state, 100, 30);
        let first = header_hit(&state, CardId::ToolTable, 0);
        click(&mut state, first.rect.x, first.rect.y);
        assert_eq!(card_sort(&state, CardId::ToolTable).column, 0);
        assert!(!card_sort(&state, CardId::ToolTable).descending);

        // 같은 머리글을 다시 누르면 방향 반전, 그리면 화살표가 따라감
        let buffer = draw(&mut state, 100, 30);
        assert_eq!(
            buffer
                .cell((first.rect.x + 4, first.rect.y))
                .unwrap()
                .symbol(),
            "↑"
        );
        click(&mut state, first.rect.x, first.rect.y);
        assert!(card_sort(&state, CardId::ToolTable).descending);
        let buffer = draw(&mut state, 100, 30);
        assert_eq!(
            buffer
                .cell((first.rect.x + 4, first.rect.y))
                .unwrap()
                .symbol(),
            "↓"
        );
    }

    #[test]
    fn header_click_sorts_every_table_card_ascending_first_then_descending() {
        // (탭, 카드, 기본과 다른 열)
        let cases = [
            (ActiveTab::Overview, CardId::EventFeed, 2),
            (ActiveTab::Tools, CardId::ToolTable, 1),
            (ActiveTab::Hooks, CardId::HookTable, 0),
            (ActiveTab::Models, CardId::ModelTable, 11),
            (ActiveTab::Models, CardId::CacheBreaks, 2),
            (ActiveTab::Agents, CardId::AgentTable, 10),
            (ActiveTab::Settings, CardId::SettingsHooks, 4),
            (ActiveTab::Settings, CardId::SettingsMcp, 3),
            (ActiveTab::Settings, CardId::SettingsAgents, 3),
            (ActiveTab::Settings, CardId::SettingsDoctor, 2),
        ];
        for (tab, card, column) in cases {
            let mut state = sample_state_with_tables();
            state.tab = tab;
            draw(&mut state, 170, 60);
            let hit = header_hit(&state, card, column);
            click(&mut state, hit.rect.x, hit.rect.y);
            let first = card_sort(&state, card);
            assert_eq!(first.column, column, "{card:?}");
            assert!(!first.descending, "{card:?}");

            draw(&mut state, 170, 60);
            click(&mut state, hit.rect.x, hit.rect.y);
            let second = card_sort(&state, card);
            assert_eq!(second.column, column, "{card:?}");
            assert!(second.descending, "{card:?}");

            // 세 번째 클릭은 카드 기본 정렬로 복원
            draw(&mut state, 170, 60);
            click(&mut state, hit.rect.x, hit.rect.y);
            let spec = crate::tui::sort::sort_spec(card).unwrap();
            assert_eq!(card_sort(&state, card), spec.default, "{card:?}");
        }
    }

    #[test]
    fn header_click_keeps_the_selected_item_and_scrolls_nothing_away() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Tools;
        draw(&mut state, 100, 30);
        let table = CardId::ToolTable as usize;

        // 합계 내림차순: Bash, Read, Edit 에서 Read(둘째 행) 선택
        let rows = state
            .hit_areas
            .row_hits
            .iter()
            .find(|hit| hit.card == CardId::ToolTable)
            .copied()
            .unwrap();
        click(&mut state, rows.rect.x + 2, rows.rect.y + 1);
        assert_eq!(state.card_views[table].selected, 1);

        // 호출 오름차순: Edit, Bash, Read 이라 Read 는 마지막 행
        let calls = header_hit(&state, CardId::ToolTable, 1);
        click(&mut state, calls.rect.x, calls.rect.y);
        assert_eq!(state.card_views[table].selected, 2);

        // 호출 내림차순: Read, Bash, Edit 이라 Read 는 첫 행
        click(&mut state, calls.rect.x, calls.rect.y);
        assert_eq!(state.card_views[table].selected, 0);
        let buffer = draw(&mut state, 100, 30);
        assert_eq!(
            buffer.cell((rows.rect.x + 2, rows.rect.y)).unwrap().bg,
            theme::BG_SELECTED
        );
        let text: String = (rows.rect.x..rows.rect.right())
            .map(|x| buffer.cell((x, rows.rect.y)).unwrap().symbol().to_string())
            .collect();
        assert!(text.contains("Read"));

        // 세 번째 클릭으로 기본 정렬(합계 내림차순)에 돌아와도 Read 선택 유지
        click(&mut state, calls.rect.x, calls.rect.y);
        assert_eq!(card_sort(&state, CardId::ToolTable).column, 5);
        assert_eq!(state.card_views[table].selected, 1);
    }

    #[test]
    fn wheel_scrolls_only_the_card_under_the_pointer() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Settings;
        draw(&mut state, 110, 36);
        let hooks = card_rect(&state, CardId::SettingsHooks);
        let mcp = card_rect(&state, CardId::SettingsMcp);
        for card in [CardId::SettingsHooks, CardId::SettingsMcp] {
            state.card_views[card as usize].total = 50;
            state.card_views[card as usize].visible = 5;
        }

        let wheel_down = |column, row| mouse(MouseEventKind::ScrollDown, column, row);
        state.handle_mouse_event(wheel_down(mcp.x + 3, mcp.y + 1));
        assert_eq!(state.card_views[CardId::SettingsMcp as usize].scroll, 3);
        assert_eq!(state.card_views[CardId::SettingsHooks as usize].scroll, 0);

        state.handle_mouse_event(wheel_down(hooks.x + 3, hooks.y + 1));
        assert_eq!(state.card_views[CardId::SettingsHooks as usize].scroll, 3);

        state.handle_mouse_event(mouse(MouseEventKind::ScrollUp, mcp.x + 3, mcp.y + 1));
        assert_eq!(state.card_views[CardId::SettingsMcp as usize].scroll, 0);

        // 카드 밖(앱 바)에서는 아무 카드도 움직이지 않음
        state.handle_mouse_event(wheel_down(1, 0));
        assert_eq!(state.card_views[CardId::SettingsHooks as usize].scroll, 3);
    }

    #[test]
    fn wheel_keeps_selected_row_and_ignores_static_cards() {
        let mut state = sample_state_with_snapshot();
        draw(&mut state, 130, 40);
        let feed = card_rect(&state, CardId::EventFeed);
        let feed_index = CardId::EventFeed as usize;
        state.card_views[feed_index].total = 40;
        state.card_views[feed_index].visible = 4;
        state.card_views[feed_index].selected = 2;

        state.handle_mouse_event(mouse(MouseEventKind::ScrollDown, feed.x + 2, feed.y + 1));
        assert_eq!(state.card_views[feed_index].scroll, 3);
        assert_eq!(state.card_views[feed_index].selected, 2);

        let kpi = card_rect(&state, CardId::KpiTools);
        state.handle_mouse_event(mouse(MouseEventKind::ScrollDown, kpi.x + 2, kpi.y + 1));
        assert_eq!(state.card_views[CardId::KpiTools as usize].scroll, 0);
    }

    #[test]
    fn hovering_a_card_or_split_handle_highlights_it_and_leaving_clears_it() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Tools;
        draw(&mut state, 100, 30);

        let table = card_rect(&state, CardId::ToolTable);
        state.handle_mouse_event(mouse(MouseEventKind::Moved, table.x + 3, table.y + 2));
        assert_eq!(state.hover_card, Some(CardId::ToolTable));
        assert_eq!(state.split_hover, None);

        // 경계 위: 경계만 강조, 카드 강조는 없음
        let hit = state.hit_areas.split_hits[0];
        state.handle_mouse_event(mouse(MouseEventKind::Moved, hit.grab.x + 4, hit.grab.y));
        assert_eq!(state.split_hover, Some((hit.group, hit.index)));
        assert_eq!(state.hover_card, None);

        // 손잡이 색: 평소에는 테두리 그대로, 호버 시 회색 손잡이
        let grip_x = hit.line.x + (hit.line.width - hit.line.width.min(6)) / 2;
        let hovered = draw(&mut state, 100, 30);
        assert_eq!(
            hovered.cell((grip_x, hit.line.y)).unwrap().fg,
            theme::SPLIT_GRIP_HOT
        );
        state.handle_mouse_event(mouse(MouseEventKind::Moved, 1, 0));
        assert_eq!(state.split_hover, None);
        let calm = draw(&mut state, 100, 30);
        let corner_fg = calm.cell((hit.line.x, hit.line.y)).unwrap().fg;
        let calm_grip = calm.cell((grip_x, hit.line.y)).unwrap();
        assert_eq!(calm_grip.fg, corner_fg);
        assert_ne!(calm_grip.fg, theme::SPLIT_GRIP_HOT);
    }

    #[test]
    fn idle_split_lines_leave_borders_and_gaps_untouched_and_drag_uses_drag_color() {
        let mut state = sample_state_with_snapshot();
        let calm = draw(&mut state, 130, 40);

        // 평소: KPI 카드 사이 빈 칸은 그린 글자 없음, 행 경계는 테두리 도형 그대로
        for hit in state.hit_areas.split_hits.clone() {
            for y in hit.line.y..hit.line.bottom() {
                for x in hit.line.x..hit.line.right() {
                    let cell = calm.cell((x, y)).unwrap();
                    assert_ne!(cell.fg, theme::SPLIT_GRIP_HOT, "{x},{y}");
                    if hit.axis == SplitAxis::Columns {
                        assert_eq!(cell.symbol(), " ", "{x},{y}");
                    }
                }
            }
        }

        // 드래그 중: 경계 전체가 드래그색 (세로 경계는 ┃ 로 길이 전체)
        let column_hit = state
            .hit_areas
            .split_hits
            .iter()
            .find(|hit| hit.axis == SplitAxis::Columns)
            .copied()
            .unwrap();
        click(&mut state, column_hit.grab.x, column_hit.grab.y + 1);
        assert_eq!(
            state.split_drag.map(|drag| drag.group),
            Some(column_hit.group)
        );
        let dragged = draw(&mut state, 130, 40);
        for y in column_hit.line.y..column_hit.line.bottom() {
            let cell = dragged.cell((column_hit.line.x, y)).unwrap();
            assert_eq!(cell.symbol(), "┃");
            assert_eq!(cell.fg, theme::SPLIT_GRIP_DRAG);
        }
    }

    #[test]
    fn hovered_card_border_uses_hover_color_unless_focused() {
        let mut state = sample_state_with_snapshot();
        draw(&mut state, 130, 40);
        let cache = card_rect(&state, CardId::KpiCache);
        state.handle_mouse_event(mouse(MouseEventKind::Moved, cache.x + 2, cache.y + 1));
        let buffer = draw(&mut state, 130, 40);
        let corner = buffer.cell((cache.x, cache.y)).unwrap();
        assert_eq!(corner.symbol(), "╭");
        assert_eq!(corner.fg, theme::BORDER_HOVER);
    }

    #[test]
    fn dragging_a_split_handle_resizes_cards_and_release_ends_the_drag() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Tools;
        draw(&mut state, 100, 30);
        let before = card_rect(&state, CardId::ToolDetail);
        let hit = state.hit_areas.split_hits[0];
        assert_eq!(hit.group, SplitGroup::ToolsRows);

        // 누르면 드래그 시작
        click(&mut state, hit.grab.x + 4, hit.grab.y);
        assert!(state.split_drag.is_some());

        // 위로 2행 이동하면 상세 카드가 2행 커짐
        state.handle_mouse_event(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            hit.grab.x + 4,
            hit.grab.y - 2,
        ));
        assert!(state.toast.as_ref().unwrap().text.contains("카드 크기"));
        draw(&mut state, 100, 30);
        let after = card_rect(&state, CardId::ToolDetail);
        assert_eq!(after.height, before.height + 2);

        // 놓으면 종료, 이후 드래그 이벤트는 무시
        state.handle_mouse_event(mouse(
            MouseEventKind::Up(MouseButton::Left),
            hit.grab.x + 4,
            hit.grab.y - 2,
        ));
        assert!(state.split_drag.is_none());
        state.handle_mouse_event(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            hit.grab.x + 4,
            hit.grab.y + 3,
        ));
        draw(&mut state, 100, 30);
        assert_eq!(card_rect(&state, CardId::ToolDetail).height, after.height);
    }

    #[test]
    fn drag_stops_at_neighbor_minimum_size() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Tools;
        draw(&mut state, 100, 30);
        let hit = state.hit_areas.split_hits[0];
        click(&mut state, hit.grab.x + 4, hit.grab.y);
        state.handle_mouse_event(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            hit.grab.x + 4,
            0,
        ));
        draw(&mut state, 100, 30);
        // 위 카드(표) 최소 5행
        assert_eq!(card_rect(&state, CardId::ToolTable).height, 5);

        state.handle_mouse_event(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            hit.grab.x + 4,
            200,
        ));
        draw(&mut state, 100, 30);
        // 아래 카드(상세) 최소 3행
        assert_eq!(card_rect(&state, CardId::ToolDetail).height, 3);
    }

    #[test]
    fn report_date_buttons_move_the_date_and_request_loads() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Report;
        draw(&mut state, 110, 30);
        let find_button = |state: &AppState, wanted: UiAction| {
            state
                .hit_areas
                .button_rects
                .iter()
                .find(|(action, _)| *action == wanted)
                .map(|(_, rect)| *rect)
                .unwrap()
        };
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let yesterday = chrono::NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        let earlier = chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();

        let next = find_button(&state, UiAction::ReportNext);
        assert_eq!(
            click(&mut state, next.x, next.y),
            UiCommand::LoadReport(today)
        );
        // 오늘이면 다음 버튼은 더 이동하지 않음
        assert_eq!(click(&mut state, next.x, next.y), UiCommand::None);

        let prev = find_button(&state, UiAction::ReportPrev);
        assert_eq!(
            click(&mut state, prev.x, prev.y),
            UiCommand::LoadReport(yesterday)
        );
        assert_eq!(
            click(&mut state, prev.x, prev.y),
            UiCommand::LoadReport(earlier)
        );

        let generate = find_button(&state, UiAction::ReportGenerate);
        assert_eq!(
            click(&mut state, generate.x, generate.y),
            UiCommand::GenerateReport(earlier)
        );
    }

    #[test]
    fn clicking_an_outline_entry_moves_the_report_body() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Report;
        draw(&mut state, 110, 30);
        let hit = state
            .hit_areas
            .row_hits
            .iter()
            .find(|hit| hit.card == CardId::ReportOutline)
            .copied()
            .unwrap();
        // 목차: 0 일일 리포트 제목, 1 요약
        assert_eq!(hit.len, 2);
        click(&mut state, hit.rect.x + 1, hit.rect.y + 1);
        assert_eq!(state.card_views[CardId::ReportOutline as usize].selected, 1);
        assert_eq!(state.card_views[CardId::ReportBody as usize].scroll, 2);
    }

    #[test]
    fn footer_buttons_run_actions_and_help_closes_on_next_click() {
        let mut state = sample_state_with_snapshot();
        draw(&mut state, 130, 40);
        let find_button = |state: &AppState, wanted: UiAction| {
            state
                .hit_areas
                .button_rects
                .iter()
                .find(|(action, _)| *action == wanted)
                .map(|(_, rect)| *rect)
                .unwrap()
        };

        let reindex = find_button(&state, UiAction::Reindex);
        assert_eq!(
            click(&mut state, reindex.x + 1, reindex.y),
            UiCommand::Reindex
        );

        let window = find_button(&state, UiAction::CycleWindow);
        assert_eq!(
            click(&mut state, window.x + 1, window.y),
            UiCommand::SetWindow(crate::tui::state::WindowKind::Day)
        );

        let pin = find_button(&state, UiAction::TogglePin);
        assert_eq!(
            click(&mut state, pin.x + 1, pin.y),
            UiCommand::SetTopmost(true)
        );
        assert!(state.always_on_top);

        let help = find_button(&state, UiAction::ShowHelp);
        click(&mut state, help.x + 1, help.y);
        assert!(state.show_help);
        // 도움말 중 클릭은 닫기만, 아래 버튼은 실행하지 않음
        assert_eq!(click(&mut state, reindex.x + 1, reindex.y), UiCommand::None);
        assert!(!state.show_help);

        let quit = find_button(&state, UiAction::Quit);
        click(&mut state, quit.x + 1, quit.y);
        assert!(state.should_quit);
    }

    #[test]
    fn layout_reset_button_restores_default_cards_of_current_tab() {
        // 전체 버튼이 들어가는 폭 (100열에서는 오른쪽 그룹을 지키려 [l] 칩이 빠짐)
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Tools;
        draw(&mut state, 120, 30);
        let default_height = card_rect(&state, CardId::ToolDetail).height;
        let hit = state.hit_areas.split_hits[0];
        click(&mut state, hit.grab.x + 4, hit.grab.y);
        state.handle_mouse_event(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            hit.grab.x + 4,
            hit.grab.y - 3,
        ));
        state.handle_mouse_event(mouse(
            MouseEventKind::Up(MouseButton::Left),
            hit.grab.x + 4,
            hit.grab.y - 3,
        ));
        draw(&mut state, 120, 30);
        assert_ne!(card_rect(&state, CardId::ToolDetail).height, default_height);

        let reset = state
            .hit_areas
            .button_rects
            .iter()
            .find(|(action, _)| *action == UiAction::ResetLayout)
            .map(|(_, rect)| *rect)
            .unwrap();
        click(&mut state, reset.x + 1, reset.y);
        draw(&mut state, 120, 30);
        assert_eq!(card_rect(&state, CardId::ToolDetail).height, default_height);
    }
    fn column_hit(state: &AppState, card: CardId, column: usize) -> ColumnHit {
        state
            .hit_areas
            .column_hits
            .iter()
            .find(|hit| hit.card == card && hit.column == column)
            .copied()
            .unwrap_or_else(|| panic!("{card:?} 열 {column} 경계 없음"))
    }

    fn release_at(state: &mut AppState, column: u16, row: u16) {
        state.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), column, row));
    }

    fn hooks_state() -> AppState {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Hooks;
        state
    }

    #[test]
    fn dragging_a_column_boundary_widens_it_and_narrows_the_slack_column() {
        let mut state = hooks_state();
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        let detail_before = column_hit(&state, CardId::HookTable, 3).width;
        let count_before = column_hit(&state, CardId::HookTable, 4).width;
        let default_sort = card_sort(&state, CardId::HookTable);

        // 경계를 누르면 드래그 시작, 오른쪽으로 4칸 이동
        click(&mut state, route.grab.x, route.grab.y);
        assert!(state.column_drag.is_some());
        drag_to(&mut state, route.grab.x + 4, route.grab.y);
        draw(&mut state, 200, 30);

        // 왼쪽 열이 넓어지고 상세(남는 폭) 열이 그만큼 좋아짐, 숫자 열은 그대로
        assert_eq!(
            column_hit(&state, CardId::HookTable, 0).width,
            route.width + 4
        );
        assert_eq!(
            column_hit(&state, CardId::HookTable, 3).width,
            detail_before - 4
        );
        assert_eq!(column_hit(&state, CardId::HookTable, 4).width, count_before);

        // 드래그는 정렬을 바꾸지 않고, 놓으면 끝남 (움직였으니 더블클릭 후보 없음)
        release_at(&mut state, route.grab.x + 4, route.grab.y);
        assert_eq!(card_sort(&state, CardId::HookTable), default_sort);
        assert!(state.column_drag.is_none());
        assert!(state.last_column_click.is_none());
        assert_eq!(
            state.column_widths[CardId::HookTable as usize][0],
            Some(route.width + 4)
        );

        // 왼쪽으로 되돌리면 다시 좁아짐
        let moved = column_hit(&state, CardId::HookTable, 0);
        click(&mut state, moved.grab.x, moved.grab.y);
        drag_to(&mut state, moved.grab.x - 2, moved.grab.y);
        draw(&mut state, 200, 30);
        assert_eq!(
            column_hit(&state, CardId::HookTable, 0).width,
            route.width + 2
        );
    }

    #[test]
    fn column_drag_clamps_to_minimum_and_numeric_columns_keep_content_width() {
        let mut state = hooks_state();
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        let count = column_hit(&state, CardId::HookTable, 4);

        // 글자 열: 최소 3칸에서 멈춤
        click(&mut state, route.grab.x, route.grab.y);
        drag_to(&mut state, 0, route.grab.y);
        draw(&mut state, 200, 30);
        assert_eq!(column_hit(&state, CardId::HookTable, 0).width, 3);
        let shrunk = state
            .hit_areas
            .header_cells
            .iter()
            .find(|hit| hit.card == CardId::HookTable && hit.column == 0);
        assert_eq!(shrunk.map(|hit| hit.rect.width), Some(3));
        release_at(&mut state, 0, route.grab.y);

        // 숫자 열: 내용 폭 미만으로는 내려가지 않고, 늘리는 것은 허용
        click(&mut state, count.grab.x, count.grab.y);
        drag_to(&mut state, count.grab.x - 30, count.grab.y);
        draw(&mut state, 200, 30);
        assert_eq!(column_hit(&state, CardId::HookTable, 4).width, count.width);
        drag_to(&mut state, count.grab.x + 3, count.grab.y);
        draw(&mut state, 200, 30);
        assert_eq!(
            column_hit(&state, CardId::HookTable, 4).width,
            count.width + 3
        );
    }

    #[test]
    fn header_click_near_a_boundary_sorts_but_the_boundary_itself_does_not() {
        let mut state = hooks_state();
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        let header = header_hit(&state, CardId::HookTable, 0);
        let default_sort = card_sort(&state, CardId::HookTable);
        assert_eq!(header.rect.right(), route.grab.x);

        // 경계 칸: 정렬 불변
        click(&mut state, route.grab.x, route.grab.y);
        release_at(&mut state, route.grab.x, route.grab.y);
        assert_eq!(card_sort(&state, CardId::HookTable), default_sort);

        // 경계 바로 왼쪽 칸(머리글 영역 끝): 정렬
        click(&mut state, route.grab.x - 1, header.rect.y);
        let sorted = card_sort(&state, CardId::HookTable);
        assert_eq!((sorted.column, sorted.descending), (0, false));
        assert!(state.column_drag.is_none());

        // 경계 바로 오른쪽 칸(다음 열 머리글 시작): 그 열 정렬
        let next = header_hit(&state, CardId::HookTable, 1);
        assert_eq!(next.rect.x, route.grab.x + 1);
        click(&mut state, next.rect.x, next.rect.y);
        assert_eq!(card_sort(&state, CardId::HookTable).column, 1);
    }

    #[test]
    fn double_click_on_a_boundary_and_the_l_key_reset_column_widths_to_automatic() {
        let mut state = hooks_state();
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        let auto_detail = column_hit(&state, CardId::HookTable, 3).width;
        let drag_route_by_five = |state: &mut AppState| {
            let hit = column_hit(state, CardId::HookTable, 0);
            click(state, hit.grab.x, hit.grab.y);
            drag_to(state, hit.grab.x + 5, hit.grab.y);
            release_at(state, hit.grab.x + 5, hit.grab.y);
            draw(state, 200, 30);
        };

        // 더블클릭: 끌지 않고 떼 뒤 다시 누름
        drag_route_by_five(&mut state);
        assert_eq!(
            column_hit(&state, CardId::HookTable, 0).width,
            route.width + 5
        );
        let grab = column_hit(&state, CardId::HookTable, 0).grab;
        click(&mut state, grab.x, grab.y);
        release_at(&mut state, grab.x, grab.y);
        assert!(state.last_column_click.is_some());
        click(&mut state, grab.x, grab.y);
        assert!(state.column_drag.is_none());
        assert!(
            state.column_widths[CardId::HookTable as usize]
                .iter()
                .all(Option::is_none)
        );
        draw(&mut state, 200, 30);
        assert_eq!(column_hit(&state, CardId::HookTable, 0).width, route.width);
        assert_eq!(column_hit(&state, CardId::HookTable, 3).width, auto_detail);

        // l 키: 현재 탭 표 카드만 복원, 다른 탭 카드 폭은 유지
        state.column_widths[CardId::ToolTable as usize][0] = Some(9);
        drag_route_by_five(&mut state);
        assert_eq!(
            column_hit(&state, CardId::HookTable, 0).width,
            route.width + 5
        );
        state.handle_key_event(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        draw(&mut state, 200, 30);
        assert_eq!(column_hit(&state, CardId::HookTable, 0).width, route.width);
        assert_eq!(state.column_widths[CardId::ToolTable as usize][0], Some(9));
    }

    #[test]
    fn column_boundary_shows_a_grip_only_on_hover_or_drag() {
        let mut state = hooks_state();
        let calm = draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);

        // 평소: 모든 열 경계 칸이 빈 칸 (손잡이가 남기는 찌꺼기 없음)
        let assert_blank = |buffer: &Buffer, state: &AppState| {
            assert!(!state.hit_areas.column_hits.is_empty());
            for hit in &state.hit_areas.column_hits {
                for y in hit.line.y..hit.line.bottom() {
                    let cell = buffer.cell((hit.line.x, y)).unwrap();
                    assert_eq!(cell.symbol(), " ", "{hit:?} {y}");
                }
            }
        };
        assert_blank(&calm, &state);

        // 호버: 머리글 칸 하나만 회색 손잡이
        state.handle_mouse_event(mouse(MouseEventKind::Moved, route.grab.x, route.grab.y));
        assert_eq!(state.column_hover, Some((CardId::HookTable, 0)));
        assert_eq!(state.split_hover, None);
        let hovered = draw(&mut state, 200, 30);
        let grip = hovered.cell((route.grab.x, route.grab.y)).unwrap();
        assert_eq!(grip.symbol(), "┃");
        assert_eq!(grip.fg, theme::SPLIT_GRIP_HOT);
        let below = hovered.cell((route.grab.x, route.grab.y + 1)).unwrap();
        assert_eq!(below.symbol(), " ");

        // 드래그: 표 높이 전체가 드래그색
        click(&mut state, route.grab.x, route.grab.y);
        drag_to(&mut state, route.grab.x + 2, route.grab.y);
        let dragged = draw(&mut state, 200, 30);
        let moved = column_hit(&state, CardId::HookTable, 0);
        for y in moved.line.y..moved.line.bottom() {
            let cell = dragged.cell((moved.line.x, y)).unwrap();
            assert_eq!(cell.symbol(), "┃", "{y}");
            assert_eq!(cell.fg, theme::SPLIT_GRIP_DRAG, "{y}");
        }

        // 놓고 벗어나면 다시 아무것도 그리지 않음
        release_at(&mut state, route.grab.x + 2, route.grab.y);
        state.handle_mouse_event(mouse(MouseEventKind::Moved, 1, 0));
        assert_eq!(state.column_hover, None);
        let idle = draw(&mut state, 200, 30);
        assert_blank(&idle, &state);
    }

    #[test]
    fn scrollbar_and_card_split_grabs_beat_column_boundaries() {
        let mut state = sample_state_with_many_tools(40);
        draw(&mut state, 100, 30);
        let track = scroll_hit(&state, CardId::ToolTable).track;
        let split = state.hit_areas.split_hits[0];
        let tool = column_hit(&state, CardId::ToolTable, 0);

        // 같은 칸을 덮는 가짜 열 경계를 앞에 두어도 스크롤바, 카드 경계 순으로 이김
        let overlap = |grab: Rect| ColumnHit { grab, ..tool };
        state
            .hit_areas
            .column_hits
            .insert(0, overlap(Rect::new(track.x, track.y, 1, 1)));
        click(&mut state, track.x, track.y);
        assert!(state.scroll_drag.is_some());
        assert!(state.column_drag.is_none());
        release_at(&mut state, track.x, track.y);

        state
            .hit_areas
            .column_hits
            .insert(0, overlap(Rect::new(split.grab.x + 4, split.grab.y, 1, 1)));
        click(&mut state, split.grab.x + 4, split.grab.y);
        assert!(state.split_drag.is_some());
        assert!(state.column_drag.is_none());

        // 진짜 레이아웃에서는 열 경계가 스크롤 트랙·카드 경계 잡기 영역과 겹치지 않음
        draw(&mut state, 100, 30);
        for hit in &state.hit_areas.column_hits {
            assert!(!hit.grab.intersects(track), "{hit:?}");
            assert!(!hit.grab.intersects(split.grab), "{hit:?}");
        }
    }
}
