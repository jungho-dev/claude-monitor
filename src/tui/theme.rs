//! tui/theme.rs
//! 다크 테마 팔레트: 서피스 단계, 텍스트, 상태·이벤트 종류·스크롤바·분할선 색.
//! 외곽선 단계: 평소 240 둥근선, 호버 67 둥근선, 포커스 39 굵은선·반전 제목, 경보 203 최우선.

use ratatui::style::{Color, Modifier, Style};

// 1. 서피스·외곽선 ------------------------------------------------------------
pub const BG_BASE: Color = Color::Indexed(233);
pub const BG_PANEL: Color = Color::Indexed(235);
pub const BG_BAR: Color = Color::Indexed(236);
pub const BG_HEADER: Color = Color::Indexed(237);
pub const BG_TONAL: Color = Color::Indexed(238);
pub const BG_SELECTED: Color = Color::Indexed(24);
pub const BG_SELECTED_IDLE: Color = Color::Indexed(237);
pub const GAUGE_TRACK: Color = Color::Indexed(236);
pub const BORDER_DIM: Color = Color::Indexed(240);
pub const BORDER_FOCUS: Color = Color::Indexed(39);
// 마우스 아래 카드: 기본 외곽선과 선택 강조 사이 밝기
pub const BORDER_HOVER: Color = Color::Indexed(67);

// 2. 텍스트 -------------------------------------------------------------------
pub const FG_TEXT: Color = Color::Indexed(252);
pub const FG_STRONG: Color = Color::Indexed(231);
pub const FG_DIM: Color = Color::Indexed(247);
// 구분 기호·단위 전용, 필수 정보에는 쓰지 않음
pub const FG_FAINT: Color = Color::Indexed(243);
pub const FG_ACCENT: Color = Color::Indexed(81);
pub const FG_ON_ACCENT: Color = Color::Indexed(16);
// 리포트 박스 표 선(┌─┬─┐ │ ├─┼─┤ └─┴─┘) 전용 뮤트 색
pub const FG_TABLE_LINE: Color = Color::Indexed(241);
// 포커스·호버가 아닌 카드 제목, 호버 카드 제목, 단축키 글자, 비활성 탭 이름
pub const TITLE_IDLE: Color = Color::Indexed(250);
pub const TITLE_HOVER: Color = FG_TEXT;
pub const HINT_KEY: Color = Color::Indexed(250);
pub const TAB_IDLE_NAME: Color = Color::Indexed(248);

// 3. 상태 색 ------------------------------------------------------------------
pub const COLOR_OK: Color = Color::Indexed(78);
pub const COLOR_WARN: Color = Color::Indexed(220);
pub const COLOR_CRIT: Color = Color::Indexed(203);
// 오류 알림 채움 바탕 (흰 글자와 함께)
pub const COLOR_CRIT_BG: Color = Color::Indexed(160);

// 4. 이벤트 종류·차트 색 ------------------------------------------------------
pub const COLOR_API: Color = Color::Indexed(81);
pub const COLOR_TOOL: Color = Color::Indexed(177);
pub const COLOR_HOOK: Color = Color::Indexed(215);
pub const COLOR_NOTICE: Color = Color::Indexed(250);
pub const CHART_REQUESTS: Color = Color::Indexed(74);
pub const CHART_TOKENS: Color = Color::Indexed(147);

// 5. 스크롤바·분할선 색 -------------------------------------------------------
// 트랙: 평소·포커스, 호버·드래그 중
pub const SCROLL_TRACK: Color = Color::Indexed(238);
pub const SCROLL_TRACK_HOT: Color = Color::Indexed(241);
// 썸: 평소, 포커스 카드, 트랙 호버, 드래그 중 (외곽선 포커스색과 겹치지 않는 회색 계열)
pub const SCROLL_THUMB: Color = Color::Indexed(245);
pub const SCROLL_THUMB_FOCUS: Color = Color::Indexed(250);
pub const SCROLL_THUMB_HOVER: Color = Color::Indexed(254);
pub const SCROLL_THUMB_DRAG: Color = Color::Indexed(231);
// 분할선: 호버, 드래그 중 (활성일 때만 그림)
pub const SPLIT_GRIP_HOT: Color = Color::Indexed(250);
pub const SPLIT_GRIP_DRAG: Color = Color::Indexed(231);

// 6. 공용 스타일 --------------------------------------------------------------
pub const STYLE_TEXT: Style = Style::new().fg(FG_TEXT);
pub const STYLE_DIM: Style = Style::new().fg(FG_DIM);
pub const STYLE_FAINT: Style = Style::new().fg(FG_FAINT);
pub const STYLE_TABLE_LINE: Style = Style::new().fg(FG_TABLE_LINE);
pub const STYLE_STRONG: Style = Style::new().fg(FG_STRONG).add_modifier(Modifier::BOLD);
pub const STYLE_ACCENT: Style = Style::new().fg(FG_ACCENT).add_modifier(Modifier::BOLD);
pub const STYLE_OK: Style = Style::new().fg(COLOR_OK);
pub const STYLE_WARN: Style = Style::new().fg(COLOR_WARN);
pub const STYLE_CRIT: Style = Style::new().fg(COLOR_CRIT);
pub const STYLE_TABLE_HEADER: Style = Style::new().fg(FG_DIM).add_modifier(Modifier::BOLD);
pub const STYLE_SELECTED_ROW: Style = Style::new().bg(BG_SELECTED).fg(FG_STRONG);
// 포커스가 아닌 카드의 선택 행 (바탕만 칠해 종류별 글자색 유지)
pub const STYLE_SELECTED_ROW_IDLE: Style = Style::new().bg(BG_SELECTED_IDLE);

// 7. 훅 판정 표시 -------------------------------------------------------------
// 판정 문자열 -> 글자 스타일 (거부 적색 굵게, 재작성 녹색 굵게, 오류 노랑, 허용 흐림)
pub fn outcome_style(outcome: &str) -> Style {
    match outcome {
        "deny" => STYLE_CRIT.add_modifier(Modifier::BOLD),
        "rewrite" => STYLE_OK.add_modifier(Modifier::BOLD),
        "error" => STYLE_WARN,
        _ => STYLE_DIM,
    }
}

// 판정 문자열 -> 한글 라벨
pub fn outcome_label(outcome: &str) -> &'static str {
    match outcome {
        "allow" => "허용",
        "deny" => "거부",
        "rewrite" => "재작성",
        "error" => "오류",
        _ => "-",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_style_marks_deny_rewrite_and_error_and_dims_the_rest() {
        assert_eq!(outcome_style("deny").fg, Some(COLOR_CRIT));
        assert!(outcome_style("deny").add_modifier.contains(Modifier::BOLD));
        assert_eq!(outcome_style("rewrite").fg, Some(COLOR_OK));
        assert!(
            outcome_style("rewrite")
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(outcome_style("error"), STYLE_WARN);
        assert_eq!(outcome_style("allow"), STYLE_DIM);
        assert_eq!(outcome_style("anything"), STYLE_DIM);
    }

    #[test]
    fn outcome_label_translates_known_outcomes_to_korean() {
        assert_eq!(outcome_label("allow"), "허용");
        assert_eq!(outcome_label("deny"), "거부");
        assert_eq!(outcome_label("rewrite"), "재작성");
        assert_eq!(outcome_label("error"), "오류");
        assert_eq!(outcome_label(""), "-");
        assert_eq!(outcome_label("unknown"), "-");
    }

    #[test]
    fn scroll_thumb_colors_never_reuse_the_focus_border_color() {
        for color in [
            SCROLL_THUMB,
            SCROLL_THUMB_FOCUS,
            SCROLL_THUMB_HOVER,
            SCROLL_THUMB_DRAG,
            SCROLL_TRACK,
            SCROLL_TRACK_HOT,
            SPLIT_GRIP_HOT,
            SPLIT_GRIP_DRAG,
        ] {
            assert_ne!(color, BORDER_FOCUS);
            assert_ne!(color, FG_ACCENT);
        }
    }
}
