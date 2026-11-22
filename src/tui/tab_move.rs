//! tui/tab_move.rs
//! 탭 순서 이동: 탭을 누른 채 끌면 순서 미리보기, 놓으면 확정·저장, Esc 취소, < > 키 한 칸 이동.

use crate::tui::i18n::{self, Msg};
use crate::tui::state::{ActiveTab, AppState, TAB_COUNT, ToastLevel, UiCommand};
use ratatui::layout::Position;

// 탭을 누른 채 끄는 중 (baseline = 누르기 전 순서, Esc 가 이 순서로 되돌림)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabDrag {
    pub tab: ActiveTab,
    pub baseline: [ActiveTab; TAB_COUNT],
}

impl AppState {
    // 1. 끌기 미리보기 --------------------------------------------------------
    // 끄는 탭을 각 칸에 넣었을 때 그 탭이 차지할 가로 범위에 포인터가 들어가는 칸으로 옮김
    // 지금 칸이 포인터를 품으면 그대로 두어 폭이 다른 탭끼리 자리를 바꿔도 앞뒤로 흔들리지 않음
    pub fn preview_tab_move(&mut self, position: Position) {
        let Some(drag) = self.tab_drag else {
            return;
        };
        // 1. 직전 그리기의 탭 폭과 간격 (번호는 모두 한 자리라 폭은 순서와 무관, 시작 열도 순서와 무관)
        let rects = &self.hit_areas.tab_rects;
        if rects.len() != TAB_COUNT {
            return;
        }
        let start = rects[0].1.x;
        let gap = rects[1].1.x.saturating_sub(rects[0].1.right());
        let width_of = |tab: ActiveTab| {
            rects
                .iter()
                .find(|(drawn, _)| *drawn == tab)
                .map_or(0, |(_, rect)| rect.width)
        };

        // 2. 칸마다 끄는 탭의 가로 범위 계산, 포인터를 품는 첫 칸이 목표
        let others: Vec<ActiveTab> = self
            .tab_order
            .iter()
            .copied()
            .filter(|tab| *tab != drag.tab)
            .collect();
        let current = self.tab_order.iter().position(|tab| *tab == drag.tab);
        let width = width_of(drag.tab);
        let mut left = start;
        let mut target = None;
        for slot in 0..TAB_COUNT {
            if (left..left.saturating_add(width)).contains(&position.x) {
                if Some(slot) == current {
                    return;
                }
                target.get_or_insert(slot);
            }
            if let Some(next) = others.get(slot) {
                left = left.saturating_add(width_of(*next) + gap);
            }
        }

        // 3. 목표 칸에 끼워 넣은 순서로 교체
        let Some(target) = target else {
            return;
        };
        let mut order = others;
        order.insert(target, drag.tab);
        if let Ok(order) = <[ActiveTab; TAB_COUNT]>::try_from(order) {
            self.tab_order = order;
        }
    }

    // 2. 끌기 확정과 취소 -----------------------------------------------------
    // 놓음과 놓침(버튼 없는 움직임, 새 누름, 크기 변경)은 보이는 순서로 확정, 반환 = 순서가 바뀌어 저장이 필요한지
    pub fn finish_tab_drag(&mut self) -> bool {
        let Some(drag) = self.tab_drag.take() else {
            return false;
        };
        if drag.baseline == self.tab_order {
            return false;
        }
        self.show_toast(i18n::text(self.lang, Msg::ToastTabMoved), ToastLevel::Info);
        true
    }

    // Esc: 누르기 전 순서로 되돌림
    pub fn cancel_tab_drag(&mut self) {
        if let Some(drag) = self.tab_drag.take() {
            self.tab_order = drag.baseline;
        }
    }

    // 3. 키보드 한 칸 이동 ----------------------------------------------------
    // < 는 왼쪽, > 는 오른쪽으로 지금 탭을 한 칸 옮기고 바로 저장, 끝에서는 무시
    pub fn move_current_tab(&mut self, delta: isize) -> UiCommand {
        let Some(slot) = self.tab_order.iter().position(|tab| *tab == self.tab) else {
            return UiCommand::None;
        };
        let Some(target) = slot
            .checked_add_signed(delta)
            .filter(|target| *target < TAB_COUNT)
        else {
            return UiCommand::None;
        };
        self.tab_order.swap(slot, target);
        self.show_toast(i18n::text(self.lang, Msg::ToastTabMoved), ToastLevel::Info);
        UiCommand::SaveState
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::i18n::Lang;
    use crate::tui::test_support::sample_state_with_tables;
    use crate::tui::views::render_frame;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    const SIZES: [(u16, u16); 3] = [(60, 16), (100, 30), (130, 40)];

    fn draw(state: &mut AppState, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_frame(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn send_mouse(state: &mut AppState, kind: MouseEventKind, column: u16, row: u16) -> UiCommand {
        state.handle_mouse_event(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn press_key(state: &mut AppState, code: KeyCode) -> UiCommand {
        state.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn tab_rect(state: &AppState, tab: ActiveTab) -> Rect {
        state
            .hit_areas
            .tab_rects
            .iter()
            .find(|(drawn, _)| *drawn == tab)
            .map(|(_, rect)| *rect)
            .unwrap()
    }

    fn drawn_order(state: &AppState) -> Vec<ActiveTab> {
        state
            .hit_areas
            .tab_rects
            .iter()
            .map(|(tab, _)| *tab)
            .collect()
    }

    // 탭 글자 영역 문자열 (영어 표시만 쓰므로 넓은 글자 뒷칸 처리 불필요)
    fn rect_text(buffer: &Buffer, rect: Rect) -> String {
        (rect.x..rect.right())
            .map(|x| buffer.cell((x, rect.y)).unwrap().symbol().to_string())
            .collect()
    }

    #[test]
    fn dragging_a_tab_onto_another_previews_the_order_and_release_commits_it() {
        for lang in [Lang::En, Lang::Ko] {
            for (width, height) in SIZES {
                let case = format!("{lang:?} {width}x{height}");
                let mut state = sample_state_with_tables();
                state.lang = lang;
                draw(&mut state, width, height);
                let overview = tab_rect(&state, ActiveTab::Overview);
                let tools = tab_rect(&state, ActiveTab::Tools);

                // 1. 누르면 그 탭으로 전환하고 끄기 대기, 도구 탭 위로 끄면 순서 미리보기
                send_mouse(
                    &mut state,
                    MouseEventKind::Down(MouseButton::Left),
                    overview.x + 1,
                    overview.y,
                );
                assert_eq!(state.tab, ActiveTab::Overview, "{case}");
                assert!(state.tab_drag.is_some(), "{case}");
                send_mouse(
                    &mut state,
                    MouseEventKind::Drag(MouseButton::Left),
                    tools.x + tools.width / 2,
                    tools.y,
                );
                assert_eq!(
                    state.tab_order[..2],
                    [ActiveTab::Tools, ActiveTab::Overview],
                    "{case}"
                );

                // 2. 다시 그리면 탭 바가 새 순서이고 번호는 자리 순서, 안내 줄에 놓기·취소
                let buffer = draw(&mut state, width, height);
                assert_eq!(
                    drawn_order(&state)[..2],
                    [ActiveTab::Tools, ActiveTab::Overview],
                    "{case}"
                );
                let first = rect_text(&buffer, tab_rect(&state, ActiveTab::Tools));
                assert!(first.trim_start().starts_with('1'), "{case} {first}");
                let hint_row: String = (0..width)
                    .map(|x| buffer.cell((x, height - 1)).unwrap().symbol().to_string())
                    .collect();
                let cancel: String = i18n::text(lang, Msg::HintCancel)
                    .split_whitespace()
                    .collect();
                let hint_row: String = hint_row.split_whitespace().collect();
                if width >= 100 {
                    assert!(hint_row.contains(&cancel), "{case} {hint_row}");
                }

                // 3. 놓으면 확정·저장 요청·알림
                let released = send_mouse(
                    &mut state,
                    MouseEventKind::Up(MouseButton::Left),
                    tools.x,
                    tools.y,
                );
                assert_eq!(released, UiCommand::SaveState, "{case}");
                assert!(state.tab_drag.is_none(), "{case}");
                assert_eq!(
                    state.toast.as_ref().map(|toast| toast.text.as_str()),
                    Some(i18n::text(lang, Msg::ToastTabMoved)),
                    "{case}"
                );

                // 4. 번호 키와 Tab 순환은 표시 순서를 따름
                press_key(&mut state, KeyCode::Char('1'));
                assert_eq!(state.tab, ActiveTab::Tools, "{case}");
                press_key(&mut state, KeyCode::Tab);
                assert_eq!(state.tab, ActiveTab::Overview, "{case}");
                press_key(&mut state, KeyCode::BackTab);
                assert_eq!(state.tab, ActiveTab::Tools, "{case}");
            }
        }
    }

    #[test]
    fn a_plain_tab_click_changes_no_order_and_a_following_key_still_works() {
        let mut state = sample_state_with_tables();
        draw(&mut state, 100, 30);
        let tools = tab_rect(&state, ActiveTab::Tools);
        send_mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            tools.x + 1,
            tools.y,
        );
        assert_eq!(state.tab, ActiveTab::Tools);

        // 놓기 전에 오는 키는 순서를 바꾸지 않은 누름을 끝내고 그대로 처리
        press_key(&mut state, KeyCode::Char('1'));
        assert_eq!(state.tab, ActiveTab::Overview);
        assert!(state.tab_drag.is_none());
        let released = send_mouse(
            &mut state,
            MouseEventKind::Up(MouseButton::Left),
            tools.x,
            tools.y,
        );
        assert_eq!(released, UiCommand::None);
        assert_eq!(state.tab_order, ActiveTab::ALL);
        assert!(state.toast.is_none());
    }

    #[test]
    fn esc_restores_the_order_from_before_the_press_and_a_lost_release_commits_the_preview() {
        // 1. Esc 취소: 종료하지 않고 원래 순서로
        let mut state = sample_state_with_tables();
        draw(&mut state, 100, 30);
        let overview = tab_rect(&state, ActiveTab::Overview);
        let hooks = tab_rect(&state, ActiveTab::Hooks);
        send_mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            overview.x + 1,
            overview.y,
        );
        send_mouse(
            &mut state,
            MouseEventKind::Drag(MouseButton::Left),
            hooks.x + 1,
            hooks.y,
        );
        assert_ne!(state.tab_order, ActiveTab::ALL);
        assert_eq!(press_key(&mut state, KeyCode::Char('2')), UiCommand::None);
        assert_eq!(state.tab, ActiveTab::Overview);
        assert_eq!(press_key(&mut state, KeyCode::Esc), UiCommand::None);
        assert!(!state.should_quit);
        assert!(state.tab_drag.is_none());
        assert_eq!(state.tab_order, ActiveTab::ALL);

        // 2. 창 밖에서 놓아 Up 을 놓치면 버튼 없는 움직임이 보이는 순서로 확정
        draw(&mut state, 100, 30);
        send_mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            overview.x + 1,
            overview.y,
        );
        send_mouse(
            &mut state,
            MouseEventKind::Drag(MouseButton::Left),
            hooks.x + 1,
            hooks.y,
        );
        let moved = state.tab_order;
        assert_eq!(
            send_mouse(&mut state, MouseEventKind::Moved, 0, 10),
            UiCommand::SaveState
        );
        assert!(state.tab_drag.is_none());
        assert_eq!(state.tab_order, moved);
        assert_eq!(moved[2], ActiveTab::Overview);
    }

    #[test]
    fn swapping_tabs_of_different_widths_settles_without_flipping_back() {
        // 에이전트(넓음)를 모델(좁음) 끝 칸으로 끌면 한 번만 자리를 바꾸고, 같은 자리·한 칸 안쪽 끌기에는 그대로
        let mut state = sample_state_with_tables();
        state.lang = Lang::Ko;
        draw(&mut state, 130, 40);
        let models = tab_rect(&state, ActiveTab::Models);
        let agents = tab_rect(&state, ActiveTab::Agents);
        assert!(agents.width > models.width);
        send_mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            agents.x + 2,
            agents.y,
        );
        let edge = models.right() - 1;
        send_mouse(
            &mut state,
            MouseEventKind::Drag(MouseButton::Left),
            edge,
            models.y,
        );
        assert_eq!(
            state.tab_order[3..5],
            [ActiveTab::Agents, ActiveTab::Models]
        );
        for column in [edge, edge + 1, edge + 2, models.x + 1] {
            draw(&mut state, 130, 40);
            send_mouse(
                &mut state,
                MouseEventKind::Drag(MouseButton::Left),
                column,
                models.y,
            );
            assert_eq!(
                state.tab_order[3..5],
                [ActiveTab::Agents, ActiveTab::Models],
                "{column}"
            );
        }

        // 모델이 있는 자리까지 끌면 다시 뒤로
        draw(&mut state, 130, 40);
        let moved_models = tab_rect(&state, ActiveTab::Models);
        send_mouse(
            &mut state,
            MouseEventKind::Drag(MouseButton::Left),
            moved_models.right() - 1,
            moved_models.y,
        );
        assert_eq!(state.tab_order, ActiveTab::ALL);
    }

    #[test]
    fn angle_bracket_keys_move_the_current_tab_one_slot_and_stop_at_the_ends() {
        let mut state = sample_state_with_tables();
        state.switch_tab(ActiveTab::Tools);
        assert_eq!(
            press_key(&mut state, KeyCode::Char('<')),
            UiCommand::SaveState
        );
        assert_eq!(
            state.tab_order[..2],
            [ActiveTab::Tools, ActiveTab::Overview]
        );
        assert_eq!(press_key(&mut state, KeyCode::Char('<')), UiCommand::None);
        assert_eq!(
            press_key(&mut state, KeyCode::Char('>')),
            UiCommand::SaveState
        );
        assert_eq!(state.tab_order, ActiveTab::ALL);

        state.switch_tab(ActiveTab::Skills);
        assert_eq!(press_key(&mut state, KeyCode::Char('>')), UiCommand::None);
        assert_eq!(state.tab_order, ActiveTab::ALL);
    }
}
