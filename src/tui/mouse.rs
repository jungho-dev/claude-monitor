//! tui/mouse.rs
//! 마우스 입력 상태 전이: 호버 강조, 클릭 포커스·선택·정렬·버튼, 카드 경계·열 경계 드래그, 카드 제목 길게 누름, 휠 스크롤.

use crate::tui::cards::{
    CardPress, ColumnDrag, MAX_TABLE_COLUMNS, ScrollDrag, scroll_for_thumb, scroll_thumb,
};
use crate::tui::i18n::{self, Msg};
use crate::tui::layout::{SplitAxis, SplitDrag};
use crate::tui::sort::SortChange;
use crate::tui::state::{AppState, ToastLevel, UiCommand};
use crate::tui::tab_move::TabDrag;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;
use std::time::{Duration, Instant};

// 휠 한 칸에 움직일 줄 수, 열 경계 더블클릭 인정 간격
const WHEEL_STEP: isize = 3;
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);

// 끌어 움직인 것이 있었는지를 실행 요청으로 바꿈
fn save_request(saved: bool) -> UiCommand {
    if saved {
        UiCommand::SaveState
    } else {
        UiCommand::None
    }
}

impl AppState {
    // 1. 마우스 이벤트 분배 ---------------------------------------------------
    // 직전 그리기에서 등록한 영역으로 판정, 반환 = 실행 요청
    pub fn handle_mouse_event(&mut self, mouse: MouseEvent) -> UiCommand {
        self.handle_mouse_event_at(mouse, Instant::now())
    }

    // 시각을 주입하는 입구 (카드 제목 길게 누름·더블클릭 판정이 now 를 기준으로 함)
    pub fn handle_mouse_event_at(&mut self, mouse: MouseEvent, now: Instant) -> UiCommand {
        // 움직임 없이 오래 누른 뒤 처음 오는 이벤트도 이동 모드 진입 뒤에 처리
        self.tick(now);

        // 도움말이 열려 있으면 클릭(닫기)과 버튼 뗌만 받음
        if self.show_help && !matches!(mouse.kind, MouseEventKind::Down(_) | MouseEventKind::Up(_))
        {
            return UiCommand::None;
        }
        // 카드 이동 중: 버튼 없는 움직임·새 누름은 놓은 것으로 보고 미리보기 순서를 확정, 휠은 무시
        if self.card_move.is_some() {
            match mouse.kind {
                MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left) => {
                    return self.commit_card_move();
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => return UiCommand::None,
                _ => {}
            }
        }
        let position = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Moved => {
                // 버튼 없는 움직임 = 이미 뗌, 대기 중 누름은 일반 클릭으로 끝 (포커스는 누를 때 반영)
                // 경계·열·썸 끌기도 Up 과 같게 끝내고, 끌어 움직인 것이 있었으면 저장 요청
                self.card_press = None;
                let saved = self.end_drags(Some(now));
                self.track_hover(position);
                save_request(saved)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.card_move.is_some() {
                    self.preview_card_move(position);
                } else if self.tab_drag.is_some() {
                    self.preview_tab_move(position);
                } else if let Some(press) = self.card_press {
                    // 누른 칸을 벗어나 시간이 차기 전에 끌면 취소 (같은 칸의 흔들림은 유지)
                    if position != press.cell {
                        self.card_press = None;
                    }
                } else if self.scroll_drag.is_some() {
                    self.drag_scrollbar(position);
                } else if self.column_drag.is_some() {
                    self.drag_column(position);
                } else {
                    self.drag_split(position);
                }
                UiCommand::None
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // 시간이 차기 전에 놓으면 일반 클릭 (포커스는 누를 때 이미 반영)
                self.card_press = None;
                if self.card_move.is_some() {
                    return self.commit_card_move();
                }
                save_request(self.end_drags(Some(now)))
            }
            MouseEventKind::ScrollUp => {
                self.wheel_card(position, -WHEEL_STEP);
                UiCommand::None
            }
            MouseEventKind::ScrollDown => {
                self.wheel_card(position, WHEEL_STEP);
                UiCommand::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // 놓기를 놓친 이전 끌기는 새 누름보다 먼저 끝내고, 움직인 것이 있으면 저장도 함께 요청
                let saved = self.end_drags(None);
                let command = self.press_left(position, now);
                self.merge_save(saved, command)
            }
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
        let coord = match hit.axis {
            SplitAxis::Columns => i32::from(position.x),
            SplitAxis::Rows => i32::from(position.y),
        };
        if let Some((before, after)) = self.card_layout.drag_cut(&hit, coord - drag.grip) {
            self.split_drag = Some(SplitDrag {
                moved: true,
                ..drag
            });
            self.show_toast(
                &i18n::card_resized(self.lang, hit.axis, before, after),
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
    // 우선순위: 도움말 닫기, 스크롤바, 카드 제목, 카드 경계 잡기, 열 경계 잡기, 카드 포커스, 버튼, 탭, 머리글 정렬, 행 선택
    fn press_left(&mut self, position: Position, now: Instant) -> UiCommand {
        // 1. 도움말은 클릭 한 번에 닫기
        if self.show_help {
            self.show_help = false;
            return UiCommand::None;
        }

        // 놓기를 놓친 이전 누름이 남았으면 버림 (이동 중이던 순서는 입구에서 이미 확정)
        self.cancel_card_move();

        // 2. 스크롤바: 썸 잡기, 트랙 클릭은 한 화면 이동 (경계 잡기보다 우선)
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

        // 3. 카드 제목 글자 누름: 카드 포커스 후 길게 누름 대기 (카드 경계 잡기보다 우선, 이동 모드는 tick 이 진입)
        // 순서를 바꿀 수 있는 그룹이 이번 프레임에 그려진 카드만, 나머지 테두리는 계속 크기 조절
        let title_card = self
            .hit_areas
            .title_hits
            .iter()
            .find(|hit| hit.rect.contains(position))
            .map(|hit| hit.card);
        if let Some(card) = title_card
            && let Some((group, index)) = card.reorder_slot()
            && self
                .hit_areas
                .slot_groups
                .iter()
                .any(|slots| slots.group == group)
        {
            self.focused_card = Some((self.tab, card));
            self.card_press = Some(CardPress {
                card,
                group,
                index,
                started: now,
                cell: position,
            });
            return UiCommand::None;
        }

        // 4. 카드 경계 잡기 (행·머리글 클릭보다 우선)
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
                moved: false,
            });
            self.split_hover = Some((hit.group, hit.index));
            self.show_toast(i18n::text(self.lang, Msg::ToastSplitDrag), ToastLevel::Info);
            return UiCommand::None;
        }

        // 5. 열 경계 잡기: 머리글 정렬보다 우선, 경계 칸을 끌지 않고 떼 직후 다시 누르면 자동 폭 복원
        // 경계 옆 머리글 칸은 끌면 열 폭, 끌지 않고 떼면 그 열 정렬 (떼는 순간 end_drags 가 처리)
        let column_hit = self
            .hit_areas
            .column_hits
            .iter()
            .find(|hit| hit.grab.contains(position))
            .copied();
        if let Some(hit) = column_hit {
            self.focused_card = Some((self.tab, hit.card));
            let click_sort = self
                .hit_areas
                .header_cells
                .iter()
                .find(|header| header.rect.contains(position))
                .map(|header| header.column);
            let double_click = click_sort.is_none()
                && self.last_column_click.is_some_and(|(card, column, at)| {
                    card == hit.card
                        && column == hit.column
                        && now.saturating_duration_since(at) < DOUBLE_CLICK_WINDOW
                });
            self.last_column_click = None;
            if double_click {
                self.column_widths[hit.card as usize] = [None; MAX_TABLE_COLUMNS];
                self.show_toast(
                    i18n::text(self.lang, Msg::ToastColumnsAuto),
                    ToastLevel::Info,
                );
                return UiCommand::SaveState;
            }
            self.column_drag = Some(ColumnDrag {
                card: hit.card,
                column: hit.column,
                start_x: position.x,
                start_width: hit.width,
                moved: false,
                click_sort,
            });
            self.column_hover = Some((hit.card, hit.column));
            if click_sort.is_none() {
                self.show_toast(
                    i18n::text(self.lang, Msg::ToastColumnDrag),
                    ToastLevel::Info,
                );
            }
            return UiCommand::None;
        }

        // 6. 카드 클릭: 테두리 강조 대상 변경, 카드 안 행·머리글 처리는 아래에서 계속
        let clicked_card = self
            .hit_areas
            .card_rects
            .iter()
            .find(|(_, rect)| rect.contains(position))
            .map(|(card, _)| *card);
        if let Some(card) = clicked_card {
            self.focused_card = Some((self.tab, card));
        }

        // 7. 하단·날짜 버튼
        let button = self
            .hit_areas
            .button_rects
            .iter()
            .find(|(_, rect)| rect.contains(position))
            .map(|(action, _)| *action);
        if let Some(action) = button {
            return self.perform_action(action);
        }

        // 8. 탭 전환, 누른 채 끄면 탭 순서 이동 (놓을 때 end_drags 가 확정)
        let tab = self
            .hit_areas
            .tab_rects
            .iter()
            .find(|(_, rect)| rect.contains(position))
            .map(|(tab, _)| *tab);
        if let Some(tab) = tab {
            let command = self.switch_tab(tab);
            self.tab_drag = Some(TabDrag {
                tab,
                baseline: self.tab_order,
            });
            return command;
        }

        // 9. 표 머리글 클릭 정렬 (숫자·시간 내림 -> 오름, 문자 오름 -> 내림, 세 번째 기본, 선택 행은 같은 항목을 따라감)
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

        // 10. 표 본문 행 클릭: 선택 행 변경, 하단 상세 카드가 따라감
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
        self.show_toast(&i18n::column_width(self.lang, width), ToastLevel::Info);
    }

    // 8. 끌기 종료 ------------------------------------------------------------
    // 놓음과 놓침(버튼 없는 움직임, 새 누름, 탭 전환, 크기 변경)이 같은 방식으로 경계·열·썸 끌기를 끝냄
    // click_at 이 있으면 끌지 않고 뗀 클릭을 처리: 경계 옆 머리글 칸은 그 열 정렬, 경계 칸은 더블클릭 후보로 기록
    // 반환 = 끌어 움직인 것이 있어 저장이 필요한지
    pub fn end_drags(&mut self, click_at: Option<Instant>) -> bool {
        let split_moved = self.split_drag.take().is_some_and(|drag| drag.moved);
        self.scroll_drag = None;
        let mut column_moved = false;
        if let Some(drag) = self.column_drag.take() {
            column_moved = drag.moved;
            if drag.moved {
                self.last_column_click = None;
            } else if let Some(at) = click_at {
                match drag.click_sort {
                    Some(column) => {
                        self.last_column_click = None;
                        self.change_card_sort(drag.card, SortChange::SelectColumn(column));
                    }
                    None => self.last_column_click = Some((drag.card, drag.column, at)),
                }
            }
        }
        let tab_moved = self.finish_tab_drag();
        split_moved || column_moved || tab_moved
    }

    // 끌기 확정 저장과 다른 실행 요청을 합침, 다른 요청이 있으면 저장은 save_pending 에 남겨 이벤트 루프가 따로 보냄
    pub fn merge_save(&mut self, saved: bool, command: UiCommand) -> UiCommand {
        match (saved, command) {
            (false, command) => command,
            (true, UiCommand::None | UiCommand::SaveState) => UiCommand::SaveState,
            (true, command) => {
                self.save_pending = true;
                command
            }
        }
    }

    // 9. 크기 변경 ------------------------------------------------------------
    // 맞추어 그린 영역이 모두 바뀜으니 카드 이동 취소와 강조·끌기 해제, 끌어 움직인 경계·열 폭이 있었으면 저장 요청
    pub fn handle_resize(&mut self) -> UiCommand {
        self.cancel_card_move();
        let saved = self.end_drags(None);
        self.split_hover = None;
        self.column_hover = None;
        self.hover_card = None;
        self.scroll_hover = None;
        save_request(saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::cards::{CardId, ColumnHit, HeaderHit, ScrollHit, UiAction};
    use crate::tui::layout::{CardLayout, SplitGroup};
    use crate::tui::sort::{SortKind, SortState};
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

        // 놓기를 놓쳐도 버튼 없는 움직임이 썸 끌기를 끝내고, 트랙 위 강조는 따로 유지
        click(&mut state, hit.track.x, hit.track.y);
        assert!(state.scroll_drag.is_some());
        assert_eq!(
            state.handle_mouse_event(mouse(MouseEventKind::Moved, hit.track.x, hit.track.y)),
            UiCommand::None
        );
        assert!(state.scroll_drag.is_none());
        assert!(state.scroll_hover.is_some());

        click(&mut state, hit.track.x, hit.track.y);
        assert!(state.scroll_drag.is_some());
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
    fn header_click_sorts_numbers_descending_first_and_text_ascending_first_on_every_table_card() {
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
            click(&mut state, hit.rect.x + hit.rect.width / 2, hit.rect.y);
            let spec = crate::tui::sort::sort_spec(card).unwrap();
            let number_first = spec.kinds[column] == SortKind::Number;
            let first = card_sort(&state, card);
            assert_eq!(first.column, column, "{card:?}");
            assert_eq!(first.descending, number_first, "{card:?}");

            draw(&mut state, 170, 60);
            click(&mut state, hit.rect.x + hit.rect.width / 2, hit.rect.y);
            let second = card_sort(&state, card);
            assert_eq!(second.column, column, "{card:?}");
            assert_eq!(second.descending, !number_first, "{card:?}");

            // 세 번째 클릭은 카드 기본 정렬로 복원
            draw(&mut state, 170, 60);
            click(&mut state, hit.rect.x + hit.rect.width / 2, hit.rect.y);
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

        // 호출 내림차순 (숫자 열 첫 클릭): Read, Bash, Edit 이라 Read 는 첫 행
        let calls = header_hit(&state, CardId::ToolTable, 1);
        click(
            &mut state,
            calls.rect.x + calls.rect.width / 2,
            calls.rect.y,
        );
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

        // 호출 오름차순: Edit, Bash, Read 이라 Read 는 마지막 행
        click(
            &mut state,
            calls.rect.x + calls.rect.width / 2,
            calls.rect.y,
        );
        assert_eq!(state.card_views[table].selected, 2);

        // 세 번째 클릭으로 기본 정렬(합계 내림차순)에 돌아와도 Read 선택 유지
        draw(&mut state, 100, 30);
        click(
            &mut state,
            calls.rect.x + calls.rect.width / 2,
            calls.rect.y,
        );
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

        // 기본 날짜는 오늘이라 다음 버튼은 이동하지 않음
        assert_eq!(state.report_date, today);
        let next = find_button(&state, UiAction::ReportNext);
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
        assert_eq!(
            click(&mut state, next.x, next.y),
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
            UiCommand::SetWindow(crate::tui::state::WindowKind::Hour)
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
    fn language_chip_click_switches_language_and_requests_a_save() {
        let mut state = sample_state_with_snapshot();
        state.lang = crate::tui::i18n::Lang::En;
        draw(&mut state, 130, 40);
        let chip = state
            .hit_areas
            .button_rects
            .iter()
            .find(|(action, _)| *action == UiAction::ToggleLanguage)
            .map(|(_, rect)| *rect)
            .unwrap();
        assert_eq!(click(&mut state, chip.x + 1, chip.y), UiCommand::SaveState);
        assert_eq!(state.lang, crate::tui::i18n::Lang::Ko);
        assert_eq!(state.toast.as_ref().unwrap().text, "언어: 한국어");

        // 다시 그린 뒤 같은 칩을 누르면 영어로
        draw(&mut state, 130, 40);
        let chip = state
            .hit_areas
            .button_rects
            .iter()
            .find(|(action, _)| *action == UiAction::ToggleLanguage)
            .map(|(_, rect)| *rect)
            .unwrap();
        click(&mut state, chip.x + 1, chip.y);
        assert_eq!(state.lang, crate::tui::i18n::Lang::En);
    }

    #[test]
    fn drag_toasts_follow_the_active_language() {
        let mut state = sample_state_with_tables();
        state.lang = crate::tui::i18n::Lang::En;
        state.handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        draw(&mut state, 130, 40);
        let hit = state.hit_areas.split_hits[0];
        click(&mut state, hit.grab.x, hit.grab.y);
        assert_eq!(
            state.toast.as_ref().unwrap().text,
            "Dragging card edge: release to keep, l resets layout"
        );
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
        click(&mut state, route.line.x, route.line.y);
        assert!(state.column_drag.is_some());
        drag_to(&mut state, route.line.x + 4, route.line.y);
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
        release_at(&mut state, route.line.x + 4, route.line.y);
        assert_eq!(card_sort(&state, CardId::HookTable), default_sort);
        assert!(state.column_drag.is_none());
        assert!(state.last_column_click.is_none());
        assert_eq!(
            state.column_widths[CardId::HookTable as usize][0],
            Some(route.width + 4)
        );

        // 왼쪽으로 되돌리면 다시 좁아짐
        let moved = column_hit(&state, CardId::HookTable, 0);
        click(&mut state, moved.line.x, moved.line.y);
        drag_to(&mut state, moved.line.x - 2, moved.line.y);
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
        click(&mut state, route.line.x, route.line.y);
        drag_to(&mut state, 0, route.line.y);
        draw(&mut state, 200, 30);
        assert_eq!(column_hit(&state, CardId::HookTable, 0).width, 3);
        let shrunk = state
            .hit_areas
            .header_cells
            .iter()
            .find(|hit| hit.card == CardId::HookTable && hit.column == 0);
        assert_eq!(shrunk.map(|hit| hit.rect.width), Some(3));
        release_at(&mut state, 0, route.line.y);

        // 숫자 열: 내용 폭 미만으로는 내려가지 않고, 늘리는 것은 허용
        click(&mut state, count.line.x, count.line.y);
        drag_to(&mut state, count.line.x - 30, count.line.y);
        draw(&mut state, 200, 30);
        assert_eq!(column_hit(&state, CardId::HookTable, 4).width, count.width);
        drag_to(&mut state, count.line.x + 3, count.line.y);
        draw(&mut state, 200, 30);
        assert_eq!(
            column_hit(&state, CardId::HookTable, 4).width,
            count.width + 3
        );
    }

    #[test]
    fn header_cells_beside_a_boundary_drag_the_width_or_sort_on_a_plain_click() {
        let mut state = hooks_state();
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        let header = header_hit(&state, CardId::HookTable, 0);
        let next = header_hit(&state, CardId::HookTable, 1);
        let default_sort = card_sort(&state, CardId::HookTable);
        assert_eq!(header.rect.right(), route.line.x);
        assert_eq!(next.rect.x, route.line.x + 1);

        // 잡는 칸 = 왼쪽 머리글 끝 칸, 경계 칸, 오른쪽 머리글 첫 칸
        assert_eq!(route.grab, Rect::new(route.line.x - 1, route.line.y, 3, 1));

        // 경계 칸: 누르고 떼도 정렬 불변, 더블클릭 후보만 남음
        click(&mut state, route.line.x, route.line.y);
        release_at(&mut state, route.line.x, route.line.y);
        assert_eq!(card_sort(&state, CardId::HookTable), default_sort);
        assert!(state.last_column_click.is_some());

        // 경계 바로 왼쪽 칸: 누르면 끌기 대기, 끌지 않고 떼면 그 열 정렬 (폭 불변, 더블클릭 후보 지움)
        click(&mut state, route.line.x - 1, header.rect.y);
        assert!(state.column_drag.is_some());
        assert_eq!(card_sort(&state, CardId::HookTable), default_sort);
        release_at(&mut state, route.line.x - 1, header.rect.y);
        let sorted = card_sort(&state, CardId::HookTable);
        assert_eq!((sorted.column, sorted.descending), (0, false));
        assert!(state.column_drag.is_none());
        assert!(state.last_column_click.is_none());
        assert!(
            state.column_widths[CardId::HookTable as usize]
                .iter()
                .all(Option::is_none)
        );

        // 경계 바로 오른쪽 칸: 끌지 않고 떼면 그 열 정렬
        click(&mut state, next.rect.x, next.rect.y);
        release_at(&mut state, next.rect.x, next.rect.y);
        assert_eq!(card_sort(&state, CardId::HookTable).column, 1);

        // 같은 칸에서 끌면 정렬 없이 왼쪽 열 폭이 바뀜
        let before = card_sort(&state, CardId::HookTable);
        click(&mut state, next.rect.x, next.rect.y);
        drag_to(&mut state, next.rect.x + 2, next.rect.y);
        release_at(&mut state, next.rect.x + 2, next.rect.y);
        assert_eq!(card_sort(&state, CardId::HookTable), before);
        assert_eq!(
            state.column_widths[CardId::HookTable as usize][0],
            Some(route.width + 2)
        );
    }

    #[test]
    fn double_click_on_a_boundary_and_the_l_key_reset_column_widths_to_automatic() {
        let mut state = hooks_state();
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        let auto_detail = column_hit(&state, CardId::HookTable, 3).width;
        let drag_route_by_five = |state: &mut AppState| {
            let hit = column_hit(state, CardId::HookTable, 0);
            click(state, hit.line.x, hit.line.y);
            drag_to(state, hit.line.x + 5, hit.line.y);
            release_at(state, hit.line.x + 5, hit.line.y);
            draw(state, 200, 30);
        };

        // 더블클릭: 끌지 않고 떼 뒤 다시 누름
        drag_route_by_five(&mut state);
        assert_eq!(
            column_hit(&state, CardId::HookTable, 0).width,
            route.width + 5
        );
        let grab = column_hit(&state, CardId::HookTable, 0).line;
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
    fn column_boundary_shows_a_faint_mark_at_rest_and_a_full_grip_on_hover_or_drag() {
        let mut state = hooks_state();
        let calm = draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        assert!(route.line.height >= 2);

        // 평소: 경계 칸은 머리글 행에만 흐린 │, 본문 행은 빈 칸 (손잡이가 남기는 찌꺼기 없음)
        let assert_calm = |buffer: &Buffer, state: &AppState| {
            assert!(!state.hit_areas.column_hits.is_empty());
            for hit in &state.hit_areas.column_hits {
                let mark = buffer.cell((hit.line.x, hit.line.y)).unwrap();
                assert_eq!(mark.symbol(), "│", "{hit:?}");
                assert_eq!(mark.fg, theme::FG_FAINT, "{hit:?}");
                for y in hit.line.y + 1..hit.line.bottom() {
                    let cell = buffer.cell((hit.line.x, y)).unwrap();
                    assert_eq!(cell.symbol(), " ", "{hit:?} {y}");
                }
            }
        };
        assert_calm(&calm, &state);

        // 호버 (경계 옆 머리글 칸 포함): 머리글과 본문 행 전체가 회색 ┃
        for x in [route.grab.x, route.line.x, route.grab.right() - 1] {
            state.handle_mouse_event(mouse(MouseEventKind::Moved, x, route.line.y));
            assert_eq!(state.column_hover, Some((CardId::HookTable, 0)), "{x}");
            assert_eq!(state.split_hover, None);
            let hovered = draw(&mut state, 200, 30);
            for y in route.line.y..route.line.bottom() {
                let grip = hovered.cell((route.line.x, y)).unwrap();
                assert_eq!(grip.symbol(), "┃", "{x} {y}");
                assert_eq!(grip.fg, theme::SPLIT_GRIP_HOT, "{x} {y}");
            }
        }

        // 드래그: 표 높이 전체가 드래그색
        click(&mut state, route.line.x, route.line.y);
        drag_to(&mut state, route.line.x + 2, route.line.y);
        let dragged = draw(&mut state, 200, 30);
        let moved = column_hit(&state, CardId::HookTable, 0);
        for y in moved.line.y..moved.line.bottom() {
            let cell = dragged.cell((moved.line.x, y)).unwrap();
            assert_eq!(cell.symbol(), "┃", "{y}");
            assert_eq!(cell.fg, theme::SPLIT_GRIP_DRAG, "{y}");
        }

        // 놓고 벗어나면 평소 표시로 돌아감
        release_at(&mut state, route.line.x + 2, route.line.y);
        state.handle_mouse_event(mouse(MouseEventKind::Moved, 1, 0));
        assert_eq!(state.column_hover, None);
        let idle = draw(&mut state, 200, 30);
        assert_calm(&idle, &state);
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

    fn settings_state() -> AppState {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Settings;
        draw(&mut state, 110, 36);
        state
    }

    fn title_pos(state: &AppState, card: CardId) -> (u16, u16) {
        let rect = state
            .hit_areas
            .title_hits
            .iter()
            .find(|hit| hit.card == card)
            .map(|hit| hit.rect)
            .unwrap_or_else(|| panic!("{card:?} 제목 영역 없음"));
        (rect.x + 1, rect.y)
    }

    fn slot_rects(state: &AppState, group: SplitGroup) -> Vec<Rect> {
        state
            .hit_areas
            .slot_groups
            .iter()
            .find(|slots| slots.group == group)
            .map(|slots| slots.slots.clone())
            .unwrap_or_else(|| panic!("{group:?} 칸 배치 없음"))
    }

    fn after(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    fn press_at(state: &mut AppState, (column, row): (u16, u16), now: Instant) -> UiCommand {
        state.handle_mouse_event_at(
            mouse(MouseEventKind::Down(MouseButton::Left), column, row),
            now,
        )
    }

    fn drag_at(state: &mut AppState, column: u16, row: u16, now: Instant) -> UiCommand {
        state.handle_mouse_event_at(
            mouse(MouseEventKind::Drag(MouseButton::Left), column, row),
            now,
        )
    }

    fn up_at(state: &mut AppState, column: u16, row: u16, now: Instant) -> UiCommand {
        state.handle_mouse_event_at(
            mouse(MouseEventKind::Up(MouseButton::Left), column, row),
            now,
        )
    }

    fn settings_order(state: &AppState) -> [u8; 4] {
        let order = state.card_layout.order_of(SplitGroup::SettingsRows);
        [order[0], order[1], order[2], order[3]]
    }

    #[test]
    fn title_press_released_before_400ms_is_a_plain_click_that_only_focuses() {
        let mut state = settings_state();
        let base = Instant::now();
        let title = title_pos(&state, CardId::SettingsMcp);

        assert_eq!(press_at(&mut state, title, base), UiCommand::None);
        assert_eq!(
            state.focused_card,
            Some((ActiveTab::Settings, CardId::SettingsMcp))
        );
        assert!(state.card_press.is_some());
        assert!(state.split_drag.is_none());

        // 399ms: 아직 이동 모드 아님, 놓으면 포커스만 남음
        assert!(!state.tick(after(base, 399)));
        assert!(state.card_move.is_none());
        assert_eq!(
            up_at(&mut state, title.0, title.1, after(base, 399)),
            UiCommand::None
        );
        assert!(state.card_press.is_none());
        assert!(state.card_move.is_none());
        assert_eq!(state.card_layout, CardLayout::default());
        assert!(state.toast.is_none());
        assert_eq!(
            state.focused_card,
            Some((ActiveTab::Settings, CardId::SettingsMcp))
        );
    }

    #[test]
    fn title_hold_for_400ms_enters_move_mode_without_any_mouse_motion() {
        let mut state = settings_state();
        let base = Instant::now();
        let title = title_pos(&state, CardId::SettingsMcp);
        press_at(&mut state, title, base);

        // 남은 시간은 줄어들다 400ms 에서 0, 시계만으로 이동 모드 진입
        assert_eq!(
            state.press_remaining(after(base, 100)),
            Some(Duration::from_millis(300))
        );
        assert!(!state.tick(after(base, 399)));
        assert!(state.tick(after(base, 400)));
        let moving = state.card_move.expect("이동 모드");
        assert_eq!(moving.card, CardId::SettingsMcp);
        assert_eq!((moving.group, moving.index), (SplitGroup::SettingsRows, 1));
        assert_eq!(moving.target, 1);
        assert_eq!(moving.baseline[..4], [0, 1, 2, 3]);
        assert!(state.card_press.is_none());
        assert_eq!(state.press_remaining(after(base, 401)), None);
        // 이미 진입했으면 상태 변화 없음
        assert!(!state.tick(after(base, 900)));
    }

    #[test]
    fn dragging_before_the_hold_time_cancels_the_press_without_move_or_resize() {
        let mut state = settings_state();
        let base = Instant::now();
        let title = title_pos(&state, CardId::SettingsMcp);
        let slots = slot_rects(&state, SplitGroup::SettingsRows);
        press_at(&mut state, title, base);

        drag_at(&mut state, title.0, slots[3].y + 1, after(base, 100));
        assert!(state.card_press.is_none());
        assert!(state.card_move.is_none());
        assert!(state.split_drag.is_none());
        assert!(state.column_drag.is_none());
        assert_eq!(state.card_layout, CardLayout::default());

        // 취소한 뒤 시간이 흘러도 이동 모드로 가지 않고, 놓아도 저장 요청 없음
        assert!(!state.tick(after(base, 600)));
        assert!(state.card_move.is_none());
        assert_eq!(
            up_at(&mut state, title.0, slots[3].y + 1, after(base, 650)),
            UiCommand::None
        );
        assert_eq!(state.card_layout, CardLayout::default());
        // 크기 조절 알림도 나오지 않음
        assert!(state.toast.is_none());
    }

    #[test]
    fn dragging_in_move_mode_previews_insertion_and_release_commits_and_saves() {
        let mut state = settings_state();
        let base = Instant::now();
        let slots = slot_rects(&state, SplitGroup::SettingsRows);
        let title = title_pos(&state, CardId::SettingsHooks);
        press_at(&mut state, title, base);
        assert!(state.tick(after(base, 400)));

        // 둘째 칸 -> 셋째 칸 -> 되돌아 첫 칸: 삽입 이동이라 사이 카드가 밀림
        drag_at(&mut state, 20, slots[2].y + 1, after(base, 450));
        assert_eq!(settings_order(&state), [1, 2, 0, 3]);
        assert_eq!(state.card_move.unwrap().target, 2);
        drag_at(&mut state, 20, slots[1].y + 1, after(base, 460));
        assert_eq!(settings_order(&state), [1, 0, 2, 3]);
        drag_at(&mut state, 20, slots[0].y + 1, after(base, 470));
        assert_eq!(settings_order(&state), [0, 1, 2, 3]);

        // 칸 밖(앱 바)은 마지막 목표 유지
        drag_at(&mut state, 20, slots[3].y + 1, after(base, 480));
        assert_eq!(settings_order(&state), [1, 2, 3, 0]);
        drag_at(&mut state, 20, 0, after(base, 490));
        assert_eq!(settings_order(&state), [1, 2, 3, 0]);
        assert_eq!(state.card_move.unwrap().target, 3);

        // 휠은 이동 중 무시 (순서·이동 모드 그대로)
        state.handle_mouse_event_at(
            mouse(MouseEventKind::ScrollDown, 20, slots[1].y + 1),
            after(base, 495),
        );
        assert_eq!(settings_order(&state), [1, 2, 3, 0]);
        assert!(state.card_move.is_some());

        // 놓으면 확정: 알림과 저장 요청
        assert_eq!(
            up_at(&mut state, 20, 0, after(base, 500)),
            UiCommand::SaveState
        );
        assert!(state.card_move.is_none());
        assert_eq!(settings_order(&state), [1, 2, 3, 0]);
        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.text, "카드 위치 변경 (l 기본 배치)");
        assert_eq!(toast.level, ToastLevel::Info);

        // 다음 프레임: 위 카드가 마지막 칸 영역에 그려짐
        draw(&mut state, 110, 36);
        assert_eq!(card_rect(&state, CardId::SettingsHooks), slots[3]);
        assert_eq!(card_rect(&state, CardId::SettingsMcp), slots[0]);
    }

    #[test]
    fn releasing_on_the_original_slot_changes_nothing_and_requests_no_save() {
        let mut state = settings_state();
        let base = Instant::now();
        let slots = slot_rects(&state, SplitGroup::SettingsRows);
        let title = title_pos(&state, CardId::SettingsAgents);
        press_at(&mut state, title, base);
        state.tick(after(base, 400));
        drag_at(&mut state, 20, slots[0].y + 1, after(base, 420));
        drag_at(&mut state, 20, slots[2].y + 1, after(base, 430));
        assert_eq!(
            up_at(&mut state, 20, slots[2].y + 1, after(base, 440)),
            UiCommand::None
        );
        assert!(state.card_move.is_none());
        assert_eq!(state.card_layout, CardLayout::default());
        assert!(state.toast.is_none());
    }

    #[test]
    fn first_event_after_the_hold_time_starts_the_move_even_without_a_tick() {
        let mut state = settings_state();
        let base = Instant::now();
        let slots = slot_rects(&state, SplitGroup::SettingsRows);
        let title = title_pos(&state, CardId::SettingsHooks);
        press_at(&mut state, title, base);

        // 이벤트 루프가 tick 을 못 돌렸어도 450ms 에 온 드래그는 이동으로 처리
        drag_at(&mut state, 20, slots[1].y + 1, after(base, 450));
        assert!(state.card_press.is_none());
        assert_eq!(settings_order(&state), [1, 0, 2, 3]);
        assert_eq!(
            up_at(&mut state, 20, slots[1].y + 1, after(base, 460)),
            UiCommand::SaveState
        );
    }

    #[test]
    fn escape_restores_the_order_from_before_the_press() {
        let mut state = settings_state();
        let base = Instant::now();
        let slots = slot_rects(&state, SplitGroup::SettingsRows);
        let title = title_pos(&state, CardId::SettingsHooks);
        press_at(&mut state, title, base);
        state.tick(after(base, 400));
        drag_at(&mut state, 20, slots[3].y + 1, after(base, 420));
        assert_eq!(settings_order(&state), [1, 2, 3, 0]);

        state.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(state.card_move.is_none());
        assert_eq!(state.card_layout, CardLayout::default());
        assert!(!state.should_quit);

        // 취소 뒤 남은 끌기·놓기는 아무 일도 하지 않음
        drag_at(&mut state, 20, slots[1].y + 1, after(base, 430));
        assert_eq!(
            up_at(&mut state, 20, slots[1].y + 1, after(base, 440)),
            UiCommand::None
        );
        assert_eq!(state.card_layout, CardLayout::default());

        // 크기 변경 등 외부 취소 경로도 같은 복원
        press_at(&mut state, title, after(base, 500));
        state.tick(after(base, 900));
        drag_at(&mut state, 20, slots[2].y + 1, after(base, 910));
        assert_ne!(state.card_layout, CardLayout::default());
        assert!(state.cancel_card_move());
        assert_eq!(state.card_layout, CardLayout::default());
        assert!(!state.cancel_card_move());
    }

    fn moved_at(state: &mut AppState, column: u16, row: u16, now: Instant) -> UiCommand {
        state.handle_mouse_event_at(mouse(MouseEventKind::Moved, column, row), now)
    }

    #[test]
    fn a_lost_button_release_is_committed_by_the_first_moved_event_and_saved() {
        let mut state = settings_state();
        let base = Instant::now();
        let slots = slot_rects(&state, SplitGroup::SettingsRows);
        let title = title_pos(&state, CardId::SettingsHooks);
        press_at(&mut state, title, base);
        state.tick(after(base, 400));
        drag_at(&mut state, 20, slots[2].y + 1, after(base, 420));
        assert_eq!(settings_order(&state), [1, 2, 0, 3]);

        // 창 밖에서 놓아 Up 이 오지 않은 경우: 버튼 없는 움직임 = 뗌, 미리보기 순서를 그대로 확정
        assert_eq!(
            moved_at(&mut state, 5, 5, after(base, 430)),
            UiCommand::SaveState
        );
        assert!(state.card_move.is_none());
        assert_eq!(settings_order(&state), [1, 2, 0, 3]);
        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.text, "카드 위치 변경 (l 기본 배치)");
        assert_eq!(toast.level, ToastLevel::Info);

        // 확정 뒤 늦게 온 Up 은 아무 일도 하지 않음
        assert_eq!(up_at(&mut state, 5, 5, after(base, 440)), UiCommand::None);
        assert_eq!(settings_order(&state), [1, 2, 0, 3]);
    }

    #[test]
    fn moved_in_move_mode_without_a_change_commits_nothing_and_requests_no_save() {
        let mut state = settings_state();
        let base = Instant::now();
        let slots = slot_rects(&state, SplitGroup::SettingsRows);
        let title = title_pos(&state, CardId::SettingsAgents);
        press_at(&mut state, title, base);
        state.tick(after(base, 400));
        drag_at(&mut state, 20, slots[0].y + 1, after(base, 410));
        drag_at(&mut state, 20, slots[2].y + 1, after(base, 420));

        // 원래 칸으로 돌아온 상태에서 뗌: 알림도 저장도 없음
        assert_eq!(
            moved_at(&mut state, 5, 5, after(base, 430)),
            UiCommand::None
        );
        assert!(state.card_move.is_none());
        assert_eq!(state.card_layout, CardLayout::default());
        assert!(state.toast.is_none());
    }

    #[test]
    fn a_new_press_in_move_mode_commits_the_preview_like_a_release() {
        let mut state = settings_state();
        let base = Instant::now();
        let slots = slot_rects(&state, SplitGroup::SettingsRows);
        let title = title_pos(&state, CardId::SettingsHooks);
        press_at(&mut state, title, base);
        state.tick(after(base, 400));
        drag_at(&mut state, 20, slots[3].y + 1, after(base, 420));
        assert_eq!(settings_order(&state), [1, 2, 3, 0]);

        // Up 이 오지 않고 새 누름이 오면 확정(알림·저장), 그 누름은 확정에 쓰이고 새 대기를 만들지 않음
        assert_eq!(
            press_at(&mut state, (3, 3), after(base, 450)),
            UiCommand::SaveState
        );
        assert!(state.card_move.is_none());
        assert!(state.card_press.is_none());
        assert_eq!(settings_order(&state), [1, 2, 3, 0]);
        assert_eq!(
            state.toast.as_ref().unwrap().text,
            "카드 위치 변경 (l 기본 배치)"
        );

        // 순서가 그대로면 새 누름도 아무것도 요청하지 않음
        let mut unchanged = settings_state();
        press_at(&mut unchanged, title, base);
        unchanged.tick(after(base, 400));
        assert_eq!(
            press_at(&mut unchanged, (3, 3), after(base, 450)),
            UiCommand::None
        );
        assert!(unchanged.card_move.is_none());
        assert_eq!(unchanged.card_layout, CardLayout::default());
        assert!(unchanged.toast.is_none());
    }

    #[test]
    fn moved_during_a_pending_press_ends_it_as_a_plain_click_and_never_enters_move_mode() {
        let mut state = settings_state();
        let base = Instant::now();
        let title = title_pos(&state, CardId::SettingsMcp);
        press_at(&mut state, title, base);
        assert!(state.card_press.is_some());

        // 버튼 없는 움직임: 대기 중 누름 종료, 포커스는 누를 때 이미 반영
        assert_eq!(
            moved_at(&mut state, title.0 + 1, title.1, after(base, 100)),
            UiCommand::None
        );
        assert!(state.card_press.is_none());
        assert_eq!(
            state.focused_card,
            Some((ActiveTab::Settings, CardId::SettingsMcp))
        );

        // 400ms 가 지나도 이동 모드로 가지 않음
        assert!(!state.tick(after(base, 400)));
        assert!(state.card_move.is_none());
        assert_eq!(state.card_layout, CardLayout::default());
        assert!(state.toast.is_none());
        assert_eq!(state.press_remaining(after(base, 400)), None);
    }

    #[test]
    fn a_key_other_than_escape_in_a_timer_started_move_mode_does_not_leave_the_ui_stuck() {
        let base = Instant::now();
        let plain = |code: KeyCode| KeyEvent::new(code, KeyModifiers::NONE);
        for code in [
            KeyCode::Char('q'),
            KeyCode::Char('2'),
            KeyCode::Char('l'),
            KeyCode::Tab,
            KeyCode::Down,
            KeyCode::Enter,
        ] {
            // 놓기를 놓치고 움직임도 없이 400ms: 시계만으로 이동 모드 진입
            let mut state = settings_state();
            let title = title_pos(&state, CardId::SettingsMcp);
            press_at(&mut state, title, base);
            assert!(state.tick(after(base, 400)));

            // 키는 종료·탭 전환·배치 변경 없이 무시, 이동 모드 유지
            assert_eq!(
                state.handle_key_event(plain(code)),
                UiCommand::None,
                "{code:?}"
            );
            assert!(state.card_move.is_some(), "{code:?}");
            assert!(!state.should_quit, "{code:?}");
            assert_eq!(state.tab, ActiveTab::Settings, "{code:?}");
            assert_eq!(state.card_layout, CardLayout::default(), "{code:?}");

            // 다음 버튼 없는 움직임이 놓은 것으로 보고 끝냄 (순서 그대로라 저장 없음)
            assert_eq!(
                moved_at(&mut state, 5, 5, after(base, 500)),
                UiCommand::None
            );
            assert!(state.card_move.is_none(), "{code:?}");

            // 이동 모드가 끝난 뒤에는 일반 키가 다시 동작
            state.handle_key_event(plain(KeyCode::Char('3')));
            assert_eq!(state.tab, ActiveTab::Hooks, "{code:?}");
        }

        // Esc 는 놓은 뒤 오는 이벤트 없이도 즉시 취소
        let mut state = settings_state();
        let title = title_pos(&state, CardId::SettingsMcp);
        press_at(&mut state, title, base);
        assert!(state.tick(after(base, 400)));
        state.handle_key_event(plain(KeyCode::Char('x')));
        state.handle_key_event(plain(KeyCode::Esc));
        assert!(state.card_move.is_none());
        assert!(!state.should_quit);
        assert_eq!(state.card_layout, CardLayout::default());

        // 키 뒤 새 누름은 확정에 쓰이고 새 대기를 만들지 않음
        let mut state = settings_state();
        press_at(&mut state, title, base);
        assert!(state.tick(after(base, 400)));
        state.handle_key_event(plain(KeyCode::Char('x')));
        assert_eq!(
            press_at(&mut state, (3, 3), after(base, 450)),
            UiCommand::None
        );
        assert!(state.card_move.is_none());
        assert!(state.card_press.is_none());
    }

    #[test]
    fn same_cell_jitter_keeps_the_press_while_another_cell_cancels_it() {
        let mut state = settings_state();
        let base = Instant::now();
        let title = title_pos(&state, CardId::SettingsMcp);
        press_at(&mut state, title, base);
        assert_eq!(
            state.card_press.map(|press| press.cell),
            Some(Position::new(title.0, title.1))
        );

        // 같은 칸의 끌기(떨림)는 대기 유지, 400ms 에 이동 모드 진입
        assert_eq!(
            drag_at(&mut state, title.0, title.1, after(base, 100)),
            UiCommand::None
        );
        assert!(state.card_press.is_some());
        assert!(state.split_drag.is_none());
        drag_at(&mut state, title.0, title.1, after(base, 300));
        assert!(state.card_press.is_some());
        assert!(state.tick(after(base, 400)));
        let moving = state.card_move.expect("이동 모드");
        assert_eq!(moving.card, CardId::SettingsMcp);

        // 다른 칸으로 끌면 대기 취소: 가로 한 칸 옆도 취소
        let mut other = settings_state();
        press_at(&mut other, title, base);
        drag_at(&mut other, title.0 + 1, title.1, after(base, 100));
        assert!(other.card_press.is_none());
        assert!(!other.tick(after(base, 400)));
        assert!(other.card_move.is_none());
        assert_eq!(other.card_layout, CardLayout::default());
    }

    #[test]
    fn title_press_does_not_grab_the_split_on_a_lower_card_but_the_rest_of_the_border_does() {
        let mut state = settings_state();
        let base = Instant::now();
        let mcp = card_rect(&state, CardId::SettingsMcp);
        let split = state
            .hit_areas
            .split_hits
            .iter()
            .find(|hit| hit.group == SplitGroup::SettingsRows && hit.index == 0)
            .copied()
            .unwrap();
        let title = title_pos(&state, CardId::SettingsMcp);

        // 제목 글자는 경계 잡기 영역 안이지만 길게 누름이 이김
        assert!(split.grab.contains(Position::new(title.0, title.1)));
        press_at(&mut state, title, base);
        assert!(state.split_drag.is_none());
        assert!(state.card_press.is_some());
        up_at(&mut state, title.0, title.1, after(base, 50));

        // 같은 테두리 줄의 제목 밖은 계속 크기 조절
        press_at(&mut state, (mcp.right() - 6, mcp.y), after(base, 100));
        assert!(state.card_press.is_none());
        assert_eq!(
            state.split_drag.map(|drag| (drag.group, drag.index)),
            Some((SplitGroup::SettingsRows, 0))
        );
    }

    #[test]
    fn title_without_a_drawn_slot_group_falls_back_to_a_normal_click() {
        // 이벤트 패널이 준비되지 않으면 피드는 안내 카드(칸 분할 없음)
        let mut state = sample_state_with_snapshot();
        state.snapshot.events = crate::tui::state::Panel::Pending;
        draw(&mut state, 130, 40);
        assert!(
            state
                .hit_areas
                .slot_groups
                .iter()
                .all(|slots| slots.group != SplitGroup::OverviewFeedCols)
        );
        let title = title_pos(&state, CardId::EventFeed);
        press_at(&mut state, title, Instant::now());
        assert!(state.card_press.is_none());
        // 기존 동작 그대로: 아래 카드의 위 테두리는 줄 경계 크기 조절 잡기
        assert_eq!(
            state.split_drag.map(|drag| drag.group),
            Some(SplitGroup::OverviewRows)
        );
        state.split_drag = None;

        // 순서 대상이 아닌 서버 상태 카드 제목도 일반 클릭
        let status = title_pos(&state, CardId::ServerStatus);
        press_at(&mut state, status, Instant::now());
        assert!(state.card_press.is_none());
        assert_eq!(
            state.focused_card,
            Some((ActiveTab::Overview, CardId::ServerStatus))
        );
    }

    fn shift_press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    #[test]
    fn shift_arrows_do_nothing_when_the_focused_cards_group_is_not_drawn_this_frame() {
        // 데이터베이스 안내 카드만 보이는 도구 탭: 포커스는 탭의 주 카드(도구 표), 그룹은 그려지지 않음
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Tools;
        state.snapshot.db_state = crate::tui::state::DbState::Missing;
        draw(&mut state, 100, 30);
        assert_eq!(state.focused_card_id(), CardId::ToolTable);
        assert!(
            state
                .hit_areas
                .slot_groups
                .iter()
                .all(|slots| slots.group != SplitGroup::ToolsRows)
        );
        for code in [KeyCode::Up, KeyCode::Down, KeyCode::Left, KeyCode::Right] {
            assert_eq!(state.handle_key_event(shift_press(code)), UiCommand::None);
        }
        assert_eq!(state.card_layout, CardLayout::default());
        assert!(state.toast.is_none());

        // 이벤트 패널이 준비되지 않아 피드가 안내 카드인 개요 탭도 같음
        let mut feed = sample_state_with_snapshot();
        feed.snapshot.events = crate::tui::state::Panel::Pending;
        draw(&mut feed, 130, 40);
        assert_eq!(feed.focused_card_id(), CardId::EventFeed);
        for code in [KeyCode::Left, KeyCode::Right] {
            assert_eq!(feed.handle_key_event(shift_press(code)), UiCommand::None);
        }
        assert_eq!(feed.card_layout, CardLayout::default());
        assert!(feed.toast.is_none());

        // 그려진 그룹의 카드는 그대로 이동
        let mut drawn = sample_state_with_tables();
        drawn.tab = ActiveTab::Tools;
        draw(&mut drawn, 100, 30);
        assert!(
            drawn
                .hit_areas
                .slot_groups
                .iter()
                .any(|slots| slots.group == SplitGroup::ToolsRows)
        );
        assert_eq!(
            drawn.handle_key_event(shift_press(KeyCode::Down)),
            UiCommand::SaveState
        );
        assert_eq!(
            drawn.card_layout.order_of(SplitGroup::ToolsRows)[..2],
            [1, 0]
        );
    }

    #[test]
    fn resize_during_a_moved_column_drag_requests_a_save_and_keeps_the_dragged_width() {
        let mut state = hooks_state();
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        click(&mut state, route.line.x, route.line.y);
        drag_to(&mut state, route.line.x + 3, route.line.y);
        assert!(state.column_drag.is_some_and(|drag| drag.moved));
        let dragged = state.column_widths[CardId::HookTable as usize][0];
        assert!(dragged.is_some());
        state.hover_card = Some(CardId::HookTable);
        state.column_hover = Some((CardId::HookTable, 0));

        // 끌기 상태 해제, 끌어 정한 폭은 저장 요청으로 보존
        assert_eq!(state.handle_resize(), UiCommand::SaveState);
        assert!(state.column_drag.is_none());
        assert!(state.column_hover.is_none());
        assert!(state.hover_card.is_none());
        assert_eq!(state.column_widths[CardId::HookTable as usize][0], dragged);

        // 늦게 오는 Up 은 저장 재요청 없음
        assert_eq!(
            state.handle_mouse_event(mouse(
                MouseEventKind::Up(MouseButton::Left),
                route.line.x + 3,
                route.line.y
            )),
            UiCommand::None
        );
    }

    #[test]
    fn resize_during_a_moved_split_drag_requests_a_save() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Tools;
        draw(&mut state, 100, 30);
        let hit = state.hit_areas.split_hits[0];
        click(&mut state, hit.grab.x + 4, hit.grab.y);
        drag_to(&mut state, hit.grab.x + 4, hit.grab.y - 2);
        assert!(state.split_drag.is_some_and(|drag| drag.moved));
        let ratios = state.card_layout.custom_ratios(SplitGroup::ToolsRows);
        assert!(ratios.is_some());
        state.split_hover = Some((SplitGroup::ToolsRows, 0));

        assert_eq!(state.handle_resize(), UiCommand::SaveState);
        assert!(state.split_drag.is_none());
        assert!(state.split_hover.is_none());
        assert_eq!(
            state.card_layout.custom_ratios(SplitGroup::ToolsRows),
            ratios
        );
    }

    #[test]
    fn resize_without_a_moved_drag_requests_nothing_and_clears_the_gesture_state() {
        // 끌지 않고 잡기만 한 열 경계·카드 경계
        let mut state = hooks_state();
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        click(&mut state, route.line.x, route.line.y);
        assert!(state.column_drag.is_some_and(|drag| !drag.moved));
        assert_eq!(state.handle_resize(), UiCommand::None);
        assert!(state.column_drag.is_none());

        let mut tools = sample_state_with_tables();
        tools.tab = ActiveTab::Tools;
        draw(&mut tools, 100, 30);
        let hit = tools.hit_areas.split_hits[0];
        click(&mut tools, hit.grab.x + 4, hit.grab.y);
        assert!(tools.split_drag.is_some_and(|drag| !drag.moved));
        assert_eq!(tools.handle_resize(), UiCommand::None);
        assert!(tools.split_drag.is_none());

        // 스크롤바 썸 끌기와 잡담만된 강조도 지움
        tools.scroll_drag = Some(ScrollDrag {
            card: CardId::ToolTable,
            grip: 0,
        });
        tools.scroll_hover = Some(CardId::ToolTable);
        assert_eq!(tools.handle_resize(), UiCommand::None);
        assert!(tools.scroll_drag.is_none());
        assert!(tools.scroll_hover.is_none());
    }

    #[test]
    fn resize_cancels_a_card_move_and_restores_the_order_without_saving() {
        let mut state = settings_state();
        let base = Instant::now();
        let slots = slot_rects(&state, SplitGroup::SettingsRows);
        let title = title_pos(&state, CardId::SettingsHooks);
        press_at(&mut state, title, base);
        state.tick(after(base, 400));
        drag_at(&mut state, 20, slots[3].y + 1, after(base, 420));
        assert_eq!(settings_order(&state), [1, 2, 3, 0]);

        assert_eq!(state.handle_resize(), UiCommand::None);
        assert!(state.card_move.is_none());
        assert_eq!(state.card_layout, CardLayout::default());
    }

    #[test]
    fn split_and_column_drags_request_a_layout_save_only_when_something_moved() {
        // 경계를 끌어 놓으면 저장 요청, 끌지 않고 놓으면 없음
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Tools;
        draw(&mut state, 100, 30);
        let hit = state.hit_areas.split_hits[0];
        click(&mut state, hit.grab.x + 4, hit.grab.y);
        assert_eq!(
            state.handle_mouse_event(mouse(
                MouseEventKind::Up(MouseButton::Left),
                hit.grab.x + 4,
                hit.grab.y
            )),
            UiCommand::None
        );
        click(&mut state, hit.grab.x + 4, hit.grab.y);
        drag_to(&mut state, hit.grab.x + 4, hit.grab.y - 2);
        assert_eq!(
            state.handle_mouse_event(mouse(
                MouseEventKind::Up(MouseButton::Left),
                hit.grab.x + 4,
                hit.grab.y - 2
            )),
            UiCommand::SaveState
        );

        // 열 경계: 끌면 저장, 끌지 않은 클릭은 없음, 더블클릭 자동 폭은 저장
        let mut state = hooks_state();
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        click(&mut state, route.line.x, route.line.y);
        drag_to(&mut state, route.line.x + 3, route.line.y);
        let up = mouse(
            MouseEventKind::Up(MouseButton::Left),
            route.line.x + 3,
            route.line.y,
        );
        assert_eq!(state.handle_mouse_event(up), UiCommand::SaveState);

        draw(&mut state, 200, 30);
        let moved = column_hit(&state, CardId::HookTable, 0);
        click(&mut state, moved.line.x, moved.line.y);
        assert_eq!(
            state.handle_mouse_event(mouse(
                MouseEventKind::Up(MouseButton::Left),
                moved.line.x,
                moved.line.y
            )),
            UiCommand::None
        );
        assert_eq!(
            click(&mut state, moved.line.x, moved.line.y),
            UiCommand::SaveState
        );
        assert!(
            state.column_widths[CardId::HookTable as usize]
                .iter()
                .all(Option::is_none)
        );
    }

    #[test]
    fn a_lost_release_is_ended_by_the_first_moved_event_for_column_drags() {
        let mut state = hooks_state();
        let base = Instant::now();
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        let grab = (route.line.x, route.line.y);

        // 끌어 움직인 뒤 Up 없이 버튼 없는 움직임: 끌기 종료, 폭 유지, 저장 요청 (더블클릭 후보는 없음)
        press_at(&mut state, grab, base);
        drag_at(&mut state, grab.0 + 3, grab.1, after(base, 50));
        let dragged = state.column_widths[CardId::HookTable as usize][0];
        assert!(dragged.is_some());
        assert_eq!(
            moved_at(&mut state, grab.0 + 6, grab.1, after(base, 60)),
            UiCommand::SaveState
        );
        assert!(state.column_drag.is_none());
        assert!(state.last_column_click.is_none());
        assert_eq!(state.column_widths[CardId::HookTable as usize][0], dragged);

        // 이후 움직임과 늦게 온 Up 은 저장을 다시 요청하지 않음
        assert_eq!(
            moved_at(&mut state, grab.0 + 7, grab.1, after(base, 70)),
            UiCommand::None
        );
        assert_eq!(
            up_at(&mut state, grab.0 + 7, grab.1, after(base, 80)),
            UiCommand::None
        );

        // 끌지 않고 잡기만 했으면 저장 없이 끝나고 더블클릭 후보만 남음
        draw(&mut state, 200, 30);
        let moved = column_hit(&state, CardId::HookTable, 0);
        press_at(&mut state, (moved.line.x, moved.line.y), after(base, 500));
        assert_eq!(
            moved_at(&mut state, 0, 0, after(base, 520)),
            UiCommand::None
        );
        assert!(state.column_drag.is_none());
        assert!(state.last_column_click.is_some());
    }

    #[test]
    fn a_lost_release_is_ended_by_the_first_moved_event_for_split_and_scroll_drags() {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Tools;
        let base = Instant::now();
        draw(&mut state, 100, 30);
        let hit = state.hit_areas.split_hits[0];

        press_at(&mut state, (hit.grab.x + 4, hit.grab.y), base);
        drag_at(&mut state, hit.grab.x + 4, hit.grab.y - 2, after(base, 50));
        assert!(state.split_drag.is_some_and(|drag| drag.moved));
        assert_eq!(
            moved_at(&mut state, hit.grab.x + 4, hit.grab.y - 3, after(base, 60)),
            UiCommand::SaveState
        );
        assert!(state.split_drag.is_none());

        // 끌지 않은 카드 경계 잡기는 저장 없이 끝남
        draw(&mut state, 100, 30);
        let again = state.hit_areas.split_hits[0];
        press_at(
            &mut state,
            (again.grab.x + 4, again.grab.y),
            after(base, 500),
        );
        assert!(state.split_drag.is_some_and(|drag| !drag.moved));
        assert_eq!(
            moved_at(&mut state, 0, 0, after(base, 510)),
            UiCommand::None
        );
        assert!(state.split_drag.is_none());

        // 스크롤바 썸 끌기는 저장할 것이 없고 끝나기만 함
        let mut tools = sample_state_with_many_tools(40);
        draw(&mut tools, 100, 30);
        let thumb = scroll_hit(&tools, CardId::ToolTable);
        press_at(&mut tools, (thumb.track.x, thumb.track.y), base);
        assert!(tools.scroll_drag.is_some());
        assert_eq!(
            moved_at(
                &mut tools,
                thumb.track.x,
                thumb.track.y + 2,
                after(base, 20)
            ),
            UiCommand::None
        );
        assert!(tools.scroll_drag.is_none());
    }

    #[test]
    fn a_new_press_resets_every_drag_state_and_commits_a_moved_one() {
        let mut state = hooks_state();
        let base = Instant::now();
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        let grab = (route.line.x, route.line.y);
        press_at(&mut state, grab, base);
        drag_at(&mut state, grab.0 + 3, grab.1, after(base, 50));
        let dragged = state.column_widths[CardId::HookTable as usize][0];
        state.scroll_drag = Some(ScrollDrag {
            card: CardId::HookTable,
            grip: 0,
        });

        // Up 이 오지 않고 다른 곳을 누름: 이전 끌기를 모두 끝내고 움직인 폭은 저장 요청
        assert_eq!(
            press_at(&mut state, (0, 0), after(base, 100)),
            UiCommand::SaveState
        );
        assert!(state.column_drag.is_none());
        assert!(state.scroll_drag.is_none());
        assert!(state.split_drag.is_none());
        assert_eq!(state.column_widths[CardId::HookTable as usize][0], dragged);
        assert!(!state.save_pending);

        // 다른 실행 요청이 있는 누름(설정 탭)은 그 요청을 돌려주고 저장은 save_pending 으로 남김
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        press_at(&mut state, (route.line.x, route.line.y), after(base, 200));
        drag_at(&mut state, route.line.x + 2, route.line.y, after(base, 250));
        let settings = state
            .hit_areas
            .tab_rects
            .iter()
            .find(|(tab, _)| *tab == ActiveTab::Settings)
            .map(|(_, rect)| *rect)
            .unwrap();
        assert_eq!(
            press_at(&mut state, (settings.x, settings.y), after(base, 300)),
            UiCommand::LoadSettings
        );
        assert!(state.column_drag.is_none());
        assert!(state.save_pending);
    }

    #[test]
    fn a_tab_switch_commits_a_moved_drag_instead_of_dropping_it() {
        let mut state = hooks_state();
        let base = Instant::now();
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        press_at(&mut state, (route.line.x, route.line.y), base);
        drag_at(&mut state, route.line.x + 3, route.line.y, after(base, 50));
        let dragged = state.column_widths[CardId::HookTable as usize][0];
        assert!(dragged.is_some());

        assert_eq!(state.switch_tab(ActiveTab::Overview), UiCommand::SaveState);
        assert!(state.column_drag.is_none());
        assert_eq!(state.column_widths[CardId::HookTable as usize][0], dragged);

        // 끌지 않고 잡기만 한 열 경계는 저장 없이 해제, 적재 요청이 있는 탭은 저장을 save_pending 으로 남김
        state.switch_tab(ActiveTab::Hooks);
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        press_at(&mut state, (route.line.x, route.line.y), after(base, 400));
        assert_eq!(state.switch_tab(ActiveTab::Overview), UiCommand::None);
        assert!(state.column_drag.is_none());
        assert!(!state.save_pending);

        state.switch_tab(ActiveTab::Hooks);
        draw(&mut state, 200, 30);
        let route = column_hit(&state, CardId::HookTable, 0);
        press_at(&mut state, (route.line.x, route.line.y), after(base, 600));
        drag_at(&mut state, route.line.x + 2, route.line.y, after(base, 650));
        assert_eq!(
            state.switch_tab(ActiveTab::Settings),
            UiCommand::LoadSettings
        );
        assert!(state.save_pending);
    }

    #[test]
    fn a_title_press_dragged_across_cells_before_400ms_leaves_widths_and_order_unchanged() {
        let mut state = settings_state();
        let base = Instant::now();
        let title = title_pos(&state, CardId::SettingsHooks);
        press_at(&mut state, title, base);

        // 시간이 차기 전에 다른 칸으로 끎: 누름 취소, 이후 끌기도 열 폭이나 카드 경계에 영향 없음
        drag_at(&mut state, title.0 + 4, title.1, after(base, 100));
        drag_at(&mut state, title.0 + 8, title.1 + 1, after(base, 150));
        assert!(state.card_press.is_none());
        assert!(state.card_move.is_none());
        assert!(state.column_drag.is_none());
        assert!(state.split_drag.is_none());
        assert!(
            state
                .column_widths
                .iter()
                .all(|widths| widths.iter().all(Option::is_none))
        );
        assert_eq!(state.card_layout, CardLayout::default());

        // 400ms 가 지나도 이동 모드로 가지 않고 놓아도 저장 요청 없음
        assert!(!state.tick(after(base, 500)));
        assert_eq!(
            up_at(&mut state, title.0 + 8, title.1 + 1, after(base, 520)),
            UiCommand::None
        );
        assert!(state.toast.is_none());
    }
}
