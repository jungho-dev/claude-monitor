//! tui/views/common.rs
//! 탭 공용 그리기: 클릭·마우스 강조 카드, 안내 카드, DB 빈 상태, 표·텍스트 카드, 열 폭 결정, 정렬 머리글.

use crate::core::config::DB_FILE_NAME;
use crate::tui::cards::{
    CardId, CardKind, ColumnHit, HeaderHit, HeaderSpan, MAX_TABLE_COLUMNS, RowHit, ScrollHit,
    scroll_thumb,
};
use crate::tui::format::fit_display;
use crate::tui::layout::SplitAxis;
use crate::tui::sort::{SortState, sort_spec};
use crate::tui::state::{AppState, DbState, Panel};
use crate::tui::theme;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Padding, Paragraph, Row, Table};
use unicode_width::UnicodeWidthChar;

// 글자 열이 줄어드는 최소 폭 (드래그 하한 포함), 열 사이 간격
const MIN_COLUMN_WIDTH: u16 = 3;
const TABLE_COLUMN_GAP: u16 = 1;

// 표 열 맞춤: 숫자·짧은 열은 고정, 글자 열은 좁아지면 먼저 줄고(Shrink), 남는 폭은 맨 오른쪽 Grow 열이 가짐
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnFit {
    Fixed,
    Shrink,
    Grow,
}

// 열 명세 (머리글, 고정 열 최소 폭, 맞춤 규칙, 정렬)
// 글자 열의 폭은 머리글·가장 넓은 칸의 표시 폭(자연 폭)이 정함
pub type ColumnSpec = (&'static str, u16, ColumnFit, Alignment);

// 열 폭 계획 한 칸 (natural = 자연 폭, floor = 줄일 수 있는 최소 폭, user = 끌어 정한 폭)
#[derive(Debug, Clone, Copy)]
pub struct ColumnPlan {
    pub natural: u16,
    pub floor: u16,
    pub fit: ColumnFit,
    pub user: Option<u16>,
}

// 한 칸의 글자 조각들
pub type TableCell = Vec<Span<'static>>;

// 표 카드 그리기 입력 (머리글 1행 고정, rows 는 정렬을 마친 전체 행)
pub struct CardTable {
    pub card: CardId,
    pub title: String,
    pub hint: String,
    pub columns: Vec<ColumnSpec>,
    pub rows: Vec<Vec<TableCell>>,
}

// 텍스트 카드 그리기 입력 (header 는 스크롤되지 않는 머리글 줄, header_cells 는 그 줄의 정렬 열 구간)
pub struct TextCard {
    pub card: CardId,
    pub title: String,
    pub hint: String,
    pub header: Option<Line<'static>>,
    pub header_cells: Vec<HeaderSpan>,
    pub lines: Vec<Line<'static>>,
}

// 1. 클릭·마우스 강조 카드 ----------------------------------------------------
// 영역 등록, 선택 카드는 굵은 테두리·반전 제목, 마우스 아래는 밝은 외곽선
// tone = 평소 외곽선 색, 위험(COLOR_CRIT) 적색이면 선택·마우스 강조보다 우선
// hint = 우측 상단 보조 문구, 빈 문자열이거나 제목과 겹칠 폭이면 생략
pub fn interactive_card(
    state: &mut AppState,
    id: CardId,
    area: Rect,
    title: &str,
    hint: &str,
    tone: Color,
) -> Block<'static> {
    state.hit_areas.card_rects.push((id, area));

    // 1. 강조 단계: 클릭한 카드는 그 탭에서만 유효, 없으면 탭의 주 카드
    let focused = state.focused_card_id() == id;
    let hovered = state.hover_card == Some(id);
    let border = if tone == theme::COLOR_CRIT {
        tone
    } else if focused {
        theme::BORDER_FOCUS
    } else if hovered {
        theme::BORDER_HOVER
    } else {
        tone
    };

    // 2. 선택 카드는 색 외에도 선 굵기·제목 반전으로 구분
    let (border_type, title_style) = if focused {
        (
            BorderType::Thick,
            Style::default()
                .fg(theme::FG_ON_ACCENT)
                .bg(border)
                .add_modifier(Modifier::BOLD),
        )
    } else if hovered {
        (
            BorderType::Rounded,
            Style::default()
                .fg(theme::TITLE_HOVER)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            BorderType::Rounded,
            Style::default()
                .fg(theme::TITLE_IDLE)
                .add_modifier(Modifier::BOLD),
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border))
        .title(Span::styled(format!(" {title} "), title_style))
        .style(Style::default().bg(theme::BG_PANEL));

    // 3. 보조 문구
    let needed =
        Line::from(format!(" {title} ")).width() + Line::from(format!(" {hint} ")).width() + 4;
    if hint.is_empty() || needed > area.width as usize {
        return block;
    }
    block.title_top(Line::from(Span::styled(format!(" {hint} "), theme::STYLE_DIM)).right_aligned())
}

// 2. 패널 상태 안내 -----------------------------------------------------------
// Ready 가 아니면 (문구, 색), 준비되면 None (소유값이라 이후 state 변경과 충돌 없음)
pub fn panel_message<T>(panel: &Panel<T>) -> Option<(String, Color)> {
    match panel {
        Panel::Ready(_) => None,
        Panel::Pending => Some(("집계 대기 중".to_string(), theme::FG_DIM)),
        Panel::Failed(text) => Some((text.clone(), theme::COLOR_CRIT)),
    }
}

// 3. 안내 카드 ----------------------------------------------------------------
// 문구는 가운데 정렬, 상태별 안내 한 줄을 아래에 덧붙임
pub fn render_notice_card(
    frame: &mut Frame,
    state: &mut AppState,
    id: CardId,
    area: Rect,
    title: &str,
    message: &str,
    color: Color,
) {
    let block = interactive_card(state, id, area, title, "", theme::BORDER_DIM);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let hint = match message {
        "집계 대기 중" => "첫 집계가 끝나면 자동 표시",
        "이벤트 없음" => "Claude Code 세션이 시작되면 실시간 표시",
        _ if color == theme::COLOR_CRIT => "r 키로 다시 시도",
        _ => "",
    };
    let headline = Span::styled(message.to_string(), Style::default().fg(color));
    render_empty_state(frame, inner, headline, hint);
}

// 4. DB 빈 상태 ---------------------------------------------------------------
// DB 가 열리지 않았으면 안내를 그리고 true (호출한 탭은 본문을 그리지 않음)
pub fn render_db_gate(frame: &mut Frame, state: &mut AppState, area: Rect) -> bool {
    let db_path = state.config.data_dir.join(DB_FILE_NAME);
    let (headline, detail, color) = match &state.snapshot.db_state {
        DbState::Open => return false,
        DbState::Pending => ("첫 집계 대기 중", String::new(), theme::FG_DIM),
        DbState::Missing => ("DB 없음", db_path.display().to_string(), theme::COLOR_WARN),
        DbState::Failed(text) => ("DB 열기 실패", text.clone(), theme::COLOR_CRIT),
    };
    let needs_serve = !matches!(state.snapshot.db_state, DbState::Pending);

    let block = interactive_card(state, CardId::Gate, area, "집계", "", theme::BORDER_DIM);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return true;
    }
    let mut lines = vec![Line::from(Span::styled(
        headline,
        Style::default().fg(color),
    ))];
    if !detail.is_empty() {
        lines.push(Line::from(Span::styled(detail, theme::STYLE_DIM)));
    }
    if needs_serve {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "serve 또는 index 실행 필요",
            theme::STYLE_ACCENT,
        )));
    }
    let top = inner.y + inner.height.saturating_sub(lines.len() as u16) / 2;
    let body = Rect::new(
        inner.x,
        top,
        inner.width,
        inner.height.saturating_sub(top - inner.y),
    );
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), body);
    true
}

// 5. 표 카드 ------------------------------------------------------------------
// 자연 폭 기반 열 폭 결정, 정렬 머리글·열 경계·본문 행 클릭 영역 등록, 선택 행 띠, 위치 표시, 오른쪽 거터 스크롤바
pub fn render_card_table(frame: &mut Frame, state: &mut AppState, area: Rect, table: CardTable) {
    let CardTable {
        card,
        title,
        hint,
        columns,
        rows,
    } = table;
    let sort = sort_spec(card).map(|_| state.card_sorts[card as usize]);

    // 1. 제목 (정렬 열과 방향 덧붙임)
    let title =
        match sort.and_then(|current| columns.get(current.column).map(|spec| (spec.0, current))) {
            Some((label, current)) => format!("{title} · 정렬 {label}{}", current.sort_arrow()),
            None => title,
        };
    let block = interactive_card(state, card, area, &title, &hint, theme::BORDER_DIM)
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    let total = rows.len();
    let visible = (inner.height as usize).saturating_sub(1);
    let selectable = card.kind() == CardKind::Selectable;

    // 2. 보기 상태 보정
    let view = &mut state.card_views[card as usize];
    view.total = total;
    view.visible = visible;
    view.scroll = view.scroll.min(total.saturating_sub(visible));
    view.selected = view.selected.min(total.saturating_sub(1));
    let (scroll, selected) = (view.scroll, view.selected);

    // 3. 열 폭 계획 (자연 폭 = 머리글·가장 넓은 칸, 숫자·고정 열은 선호 폭 이상으로 줄지 않음)
    let plans: Vec<ColumnPlan> = columns
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let label_width = Span::raw(spec.0).width() + usize::from(sort.is_some());
            let content_width = rows
                .iter()
                .map(|row| {
                    row.get(index)
                        .map_or(0, |cell| cell.iter().map(Span::width).sum::<usize>())
                })
                .max()
                .unwrap_or(0);
            let widest = u16::try_from(label_width.max(content_width)).unwrap_or(u16::MAX);
            let fixed = spec.2 == ColumnFit::Fixed;
            let natural = if fixed { spec.1.max(widest) } else { widest };
            ColumnPlan {
                natural,
                floor: if fixed {
                    natural
                } else {
                    MIN_COLUMN_WIDTH.min(natural)
                },
                fit: spec.2,
                user: state.column_widths[card as usize]
                    .get(index)
                    .copied()
                    .flatten(),
            }
        })
        .collect();
    let widths = resolve_column_widths(&plans, inner.width);
    let shown: Vec<usize> = (0..columns.len())
        .filter(|index| widths[*index] > 0)
        .collect();
    let shown_floors = shown
        .iter()
        .fold(0u16, |sum, index| sum.saturating_add(plans[*index].floor));
    let shown_gaps = (shown.len().saturating_sub(1)) as u16 * TABLE_COLUMN_GAP;

    // 4. 머리글 (정렬 열 화살표 유지, 정렬·열 경계 클릭 영역은 열 좌표와 동일)
    let mut header_cells: Vec<Cell<'static>> = Vec::new();
    let mut cursor = inner.x;
    for (position, index) in shown.iter().enumerate() {
        let spec = columns[*index];
        let width = widths[*index];
        let active = sort.is_some_and(|current| current.column == *index);
        let label = match sort {
            Some(current) if active && Span::raw(spec.0).width() + 1 > usize::from(width) => {
                let head = fit_display(spec.0, usize::from(width).saturating_sub(1));
                format!("{}{}", head.trim_end(), current.sort_arrow())
            }
            Some(current) if active => format!("{}{}", spec.0, current.sort_arrow()),
            _ if Span::raw(spec.0).width() > usize::from(width) => {
                fit_display(spec.0, usize::from(width))
                    .trim_end()
                    .to_string()
            }
            _ => spec.0.to_string(),
        };
        let cell = Cell::from(Line::from(label).alignment(spec.3));
        header_cells.push(if active {
            cell.style(theme::STYLE_ACCENT)
        } else {
            cell
        });
        if sort.is_some() && inner.height > 0 {
            state.hit_areas.header_cells.push(HeaderHit {
                card,
                column: *index,
                rect: Rect::new(cursor, inner.y, width, 1),
            });
        }

        // 열 경계 (마지막 보이는 열 오른쪽은 경계 없음, 폭 범위 = 다른 열 최소 폭을 남긴 만큼)
        if position + 1 < shown.len() && *index < MAX_TABLE_COLUMNS && inner.height > 0 {
            let others_floor = (shown_floors - plans[*index].floor).saturating_add(shown_gaps);
            state.hit_areas.column_hits.push(ColumnHit {
                card,
                column: *index,
                grab: Rect::new(cursor + width, inner.y, TABLE_COLUMN_GAP, 1),
                line: Rect::new(cursor + width, inner.y, TABLE_COLUMN_GAP, inner.height),
                width,
                min: plans[*index].floor,
                max: inner
                    .width
                    .saturating_sub(others_floor)
                    .max(plans[*index].floor),
            });
        }
        cursor += width + TABLE_COLUMN_GAP;
    }

    // 5. 본문 행 클릭 영역 (머리글 1행 제외)
    if selectable && visible > 0 {
        state.hit_areas.row_hits.push(RowHit {
            card,
            rect: Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1),
            offset: scroll,
            len: total,
        });
    }

    // 6. 표 그리기 (보이는 행만, 칸은 폭에 맞춰 끝 말줄임)
    let mut block = block;
    if total > visible && visible > 0 {
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(
                    " {}-{} / {} ",
                    scroll + 1,
                    (scroll + visible).min(total),
                    total
                ),
                theme::STYLE_DIM,
            ))
            .right_aligned(),
        );
    }
    let body_rows: Vec<Row<'static>> = rows
        .into_iter()
        .skip(scroll)
        .take(visible)
        .map(|row| {
            let cells: Vec<Cell<'static>> = row
                .into_iter()
                .enumerate()
                .filter(|(index, _)| widths.get(*index).is_some_and(|width| *width > 0))
                .map(|(index, spans)| {
                    let clipped = clip_spans_to_width(spans, usize::from(widths[index]));
                    Cell::from(Line::from(clipped).alignment(columns[index].3))
                })
                .collect();
            Row::new(cells).style(theme::STYLE_TEXT)
        })
        .collect();
    let constraints: Vec<Constraint> = shown
        .iter()
        .map(|index| Constraint::Length(widths[*index]))
        .collect();
    let drawn = Table::new(body_rows, constraints)
        .header(Row::new(header_cells).style(theme::STYLE_TABLE_HEADER))
        .block(block)
        .column_spacing(1);
    frame.render_widget(drawn, area);
    if total == 0 && inner.height > 1 {
        let headline = Span::styled(
            format!("선택 구간({})에 기록 없음", state.window.window_label()),
            theme::STYLE_TEXT,
        );
        let empty_area = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
        render_empty_state(frame, empty_area, headline, "w 키로 구간 변경");
    }

    // 7. 선택 행 띠 (머리글 행 제외)
    let body = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(1),
    );
    if selectable && total > 0 && (scroll..scroll + visible).contains(&selected) {
        let focused = state.focused_card_id() == card;
        let row_y = body.y + (selected - scroll) as u16;
        paint_selection_band(frame.buffer_mut(), area, row_y, focused);
    }

    // 8. 오른쪽 거터 스크롤바
    render_card_scrollbar(frame, state, card, area, body);
}

// 6. 텍스트 카드 --------------------------------------------------------------
// 줄 단위 스크롤, 고정 머리글 줄, 선택 줄 띠와 클릭 영역, 일반 문장 줄바꿈 (반환 = 입력 줄별 시작 행)
pub fn render_text_card(
    frame: &mut Frame,
    state: &mut AppState,
    area: Rect,
    spec: TextCard,
) -> Vec<usize> {
    let TextCard {
        card,
        title,
        hint,
        header,
        header_cells,
        lines,
    } = spec;
    let block = interactive_card(state, card, area, &title, &hint, theme::BORDER_DIM)
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    let header_line = header.filter(|_| inner.height > 0);
    let header_height = u16::from(header_line.is_some());
    let body = Rect::new(
        inner.x,
        inner.y + header_height,
        inner.width,
        inner.height - header_height,
    );
    let selectable = card.kind() == CardKind::Selectable;

    // 1. 줄 배치 (선택·머리글 카드는 줄 수 고정, 그 밖은 일반 문장 줄바꿈)
    let wrap_prose = !selectable && header_line.is_none();
    let (lines, row_starts) = layout_text_lines(lines, usize::from(body.width), wrap_prose);
    let total = lines.len();
    let visible = body.height as usize;

    // 2. 보기 상태 보정
    let view = &mut state.card_views[card as usize];
    view.total = total;
    view.visible = visible;
    view.scroll = view.scroll.min(total.saturating_sub(visible));
    view.selected = view.selected.min(total.saturating_sub(1));
    let (scroll, selected) = (view.scroll, view.selected);

    // 3. 행 클릭 영역과 머리글 열 클릭 영역
    if selectable && visible > 0 {
        state.hit_areas.row_hits.push(RowHit {
            card,
            rect: body,
            offset: scroll,
            len: total,
        });
    }
    if header_line.is_some() {
        for span in &header_cells {
            let room = inner.width.saturating_sub(span.start);
            let cell_width = span.width.min(room);
            if cell_width > 0 {
                state.hit_areas.header_cells.push(HeaderHit {
                    card,
                    column: span.column,
                    rect: Rect::new(inner.x + span.start, inner.y, cell_width, 1),
                });
            }
        }
    }

    // 4. 줄 그리기
    let mut block = block;
    if total > visible && visible > 0 {
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(
                    " {}-{} / {} ",
                    scroll + 1,
                    (scroll + visible).min(total),
                    total
                ),
                theme::STYLE_DIM,
            ))
            .right_aligned(),
        );
    }
    let shown: Vec<Line<'static>> = lines.into_iter().skip(scroll).take(visible).collect();
    frame.render_widget(block, area);
    if let Some(header_line) = header_line {
        frame.render_widget(
            Paragraph::new(header_line).style(theme::STYLE_TABLE_HEADER),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
    }
    frame.render_widget(Paragraph::new(shown), body);

    // 5. 선택 줄 띠
    if selectable && total > 0 && (scroll..scroll + visible).contains(&selected) {
        let focused = state.focused_card_id() == card;
        let row_y = body.y + (selected - scroll) as u16;
        paint_selection_band(frame.buffer_mut(), area, row_y, focused);
    }

    // 6. 오른쪽 거터 스크롤바
    render_card_scrollbar(frame, state, card, area, body);
    row_starts
}

// 7. 카드 스크롤바 ------------------------------------------------------------
// 오른쪽 거터(테두리 바로 안쪽 열)가 ░ 트랙, 썸은 █, 그린 경우만 클릭 영역 등록
// body = 스크롤되는 본문 행 범위 (표는 머리글 행 제외)
pub fn render_card_scrollbar(
    frame: &mut Frame,
    state: &mut AppState,
    card: CardId,
    area: Rect,
    body: Rect,
) {
    // 1. 그릴 조건 (고정 카드·내용이 들어가는 카드는 없음)
    if card.kind() == CardKind::Static || body.is_empty() || area.width < 4 {
        return;
    }
    let view = state.card_views[card as usize];
    let Some(thumb) = scroll_thumb(view.total, view.visible, view.scroll, body.height) else {
        return;
    };

    // 2. 색: 트랙은 호버·드래그만 밝게, 썸은 드래그, 호버, 포커스 카드, 평소 순
    let dragging = state.scroll_drag.is_some_and(|drag| drag.card == card);
    let hovered = state.scroll_hover == Some(card);
    let track_color = if dragging || hovered {
        theme::SCROLL_TRACK_HOT
    } else {
        theme::SCROLL_TRACK
    };
    let thumb_color = if dragging {
        theme::SCROLL_THUMB_DRAG
    } else if hovered {
        theme::SCROLL_THUMB_HOVER
    } else if state.focused_card_id() == card {
        theme::SCROLL_THUMB_FOCUS
    } else {
        theme::SCROLL_THUMB
    };

    // 3. 거터 쓰기 (테두리 열은 건드리지 않음)
    let gutter_x = area.right() - 2;
    let buf = frame.buffer_mut();
    for y in body.y..body.bottom() {
        buf[(gutter_x, y)].set_symbol("░").set_fg(track_color);
    }
    let thumb_top = body.y + thumb.start;
    for y in thumb_top..thumb_top + thumb.len {
        buf[(gutter_x, y)].set_symbol("█").set_fg(thumb_color);
    }

    // 4. 클릭 영역 등록 (거터와 테두리 2열)
    state.hit_areas.scroll_hits.push(ScrollHit {
        card,
        track: Rect::new(gutter_x, body.y, 2, body.height),
        total: view.total,
        visible: view.visible,
        scroll: view.scroll,
    });
}

// 8. 카드 경계 드래그 손잡이 --------------------------------------------------
// 평소에는 아무것도 그리지 않고, 마우스 아래·드래그 중인 경계만 강조
pub fn render_split_handles(frame: &mut Frame, state: &AppState) {
    let dragging = state.split_drag.map(|drag| (drag.group, drag.index));
    let buf = frame.buffer_mut();
    for hit in &state.hit_areas.split_hits {
        let key = Some((hit.group, hit.index));
        let dragged = dragging == key;
        let active = dragged || (dragging.is_none() && state.split_hover == key);
        let line = hit.line;
        if !active || line.is_empty() {
            continue;
        }
        let color = if dragged {
            theme::SPLIT_GRIP_DRAG
        } else {
            theme::SPLIT_GRIP_HOT
        };
        match hit.axis {
            // 카드 사이 빈 칸 세로 손잡이
            SplitAxis::Columns => {
                for y in line.y..line.bottom() {
                    buf[(line.x, y)].set_symbol("┃").set_fg(color);
                }
            }
            // 위 카드 아래 테두리 가로 손잡이
            SplitAxis::Rows => {
                buf.set_style(line, Style::default().fg(color));
                let grip_w = line.width.min(6);
                let grip_x = line.x + (line.width - grip_w) / 2;
                for x in grip_x..grip_x + grip_w {
                    buf[(x, line.y)]
                        .set_symbol("━")
                        .set_fg(color)
                        .set_style(Style::default().add_modifier(Modifier::BOLD));
                }
            }
        }
    }
}

// 9. 선택 행 띠 ---------------------------------------------------------------
// 왼쪽 여백 열부터 거터 직전까지 한 줄, 포커스 카드는 진한 바탕·흰 글자, 아니면 바탕만
fn paint_selection_band(buf: &mut Buffer, area: Rect, row_y: u16, focused: bool) {
    let band = Rect::new(area.x + 1, row_y, area.width.saturating_sub(3), 1);
    let style = if focused {
        theme::STYLE_SELECTED_ROW
    } else {
        theme::STYLE_SELECTED_ROW_IDLE
    };
    buf.set_style(band, style);
}

// 10. 빈 상태 안내 ------------------------------------------------------------
// 영역 가운데에 제목 한 줄과 흐린 안내 한 줄 (높이 1이면 제목만)
pub fn render_empty_state(frame: &mut Frame, rect: Rect, headline: Span<'static>, hint: &str) {
    if rect.is_empty() {
        return;
    }
    let mut lines = vec![Line::from(headline)];
    if !hint.is_empty() && rect.height >= 2 {
        lines.push(Line::from(Span::styled(hint.to_string(), theme::STYLE_DIM)));
    }
    let used = (lines.len() as u16).min(rect.height);
    let body = Rect::new(rect.x, rect.y + (rect.height - used) / 2, rect.width, used);
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), body);
}

// 11. 표 열 폭 결정 -----------------------------------------------------------
// 자연 폭(또는 끌어 정한 폭)으로 시작, 모자라면 끌지 않은 글자 열부터 가장 넓은 순서로 최소 폭까지 줄이고 끈 열은 마지막
// 그래도 모자라면 맨 오른쪽 열부터 숨김 (숫자 열은 줄지 않음), 남는 폭은 맨 오른쪽 Grow 열이 가짐 (끌어 정한 열은 받지 않음)
pub fn resolve_column_widths(plans: &[ColumnPlan], total: u16) -> Vec<u16> {
    let total = u32::from(total);
    for shown in (1..=plans.len()).rev() {
        // 1. 보이는 열의 시작 폭과 사이 간격 합
        let mut widths: Vec<u16> = plans[..shown]
            .iter()
            .map(|plan| match plan.user {
                Some(user) => user.max(plan.floor),
                None => plan.natural,
            })
            .collect();
        let gaps = (shown as u32 - 1) * u32::from(TABLE_COLUMN_GAP);
        let used = widths.iter().map(|width| u32::from(*width)).sum::<u32>() + gaps;

        // 2. 부족분은 가장 넓은 열부터 한 칸씩 줄임 (끌지 않은 열 먼저)
        if used > total {
            let mut deficit = used - total;
            for dragged in [false, true] {
                while deficit > 0 {
                    let widest = (0..shown)
                        .filter(|index| {
                            plans[*index].user.is_some() == dragged
                                && widths[*index] > plans[*index].floor
                        })
                        .max_by_key(|index| widths[*index]);
                    let Some(index) = widest else {
                        break;
                    };
                    widths[index] -= 1;
                    deficit -= 1;
                }
            }
            if deficit > 0 {
                continue;
            }
        }

        // 3. 남는 폭은 맨 오른쪽 Grow 열 (끌어 정한 열이면 그대로 둠)
        let absorber = (0..shown)
            .rev()
            .find(|index| plans[*index].fit == ColumnFit::Grow)
            .filter(|index| plans[*index].user.is_none());
        if let Some(index) = absorber {
            widths[index] += total.saturating_sub(used) as u16;
        }

        // 4. 숨긴 열은 폭 0
        widths.resize(plans.len(), 0);
        return widths;
    }
    vec![0; plans.len()]
}

// 12. 칸 말줄임 ---------------------------------------------------------------
// 넓이를 넘으면 끝에 … 하나 (마지막으로 자른 조각의 스타일), 넘지 않으면 그대로
pub fn clip_spans_to_width(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(Span::width).sum();
    if total <= width {
        return spans;
    }
    if width == 0 {
        return Vec::new();
    }
    let limit = width - 1;
    let mut clipped: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    let mut last_style = Style::default();
    for span in spans {
        last_style = span.style;
        let mut kept = String::new();
        let mut cut = false;
        for letter in span.content.chars() {
            let letter_width = UnicodeWidthChar::width(letter).unwrap_or(0);
            if used + letter_width > limit {
                cut = true;
                break;
            }
            kept.push(letter);
            used += letter_width;
        }
        if !kept.is_empty() {
            clipped.push(Span::styled(kept, span.style));
        }
        if cut {
            break;
        }
    }
    clipped.push(Span::styled("…", last_style));
    clipped
}

// 13. 정렬 헤더 줄 ------------------------------------------------------------
// 고정 폭 열 하나 (column = 정렬 열 번호, lead = 앞 간격 칸 수, right = 오른쪽 정렬)
pub struct HeaderCell {
    pub column: usize,
    pub label: &'static str,
    pub width: usize,
    pub lead: usize,
    pub right: bool,
}

// 열 간격이 모두 한 칸인 표의 머리글 (specs = 머리글, 폭, 오른쪽 정렬 여부, 열 번호는 순서)
pub fn build_uniform_sort_header(
    specs: &[(&'static str, usize, bool)],
    sort: SortState,
) -> (Line<'static>, Vec<HeaderSpan>) {
    let cells: Vec<HeaderCell> = specs
        .iter()
        .enumerate()
        .map(|(column, (label, width, right))| HeaderCell {
            column,
            label,
            width: *width,
            lead: 1,
            right: *right,
        })
        .collect();
    build_sort_header(&cells, sort)
}

// 정렬 열에 화살표를 붙인 머리글 줄과 열 클릭 구간 (글자가 폭을 넘치면 이웃 간격 한 칸을 씁음)
pub fn build_sort_header(
    cells: &[HeaderCell],
    sort: SortState,
) -> (Line<'static>, Vec<HeaderSpan>) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut hits: Vec<HeaderSpan> = Vec::new();
    let mut cursor = 0usize;
    let mut owed = 0usize;
    for (position, cell) in cells.iter().enumerate() {
        // 1. 글자 (정렬 열은 화살표)
        let active = cell.column == sort.column;
        let text = if active {
            format!("{}{}", cell.label, sort.sort_arrow())
        } else {
            cell.label.to_string()
        };
        let text_width = Span::raw(text.as_str()).width();
        let overflow = text_width.saturating_sub(cell.width);

        // 2. 앞 간격 (앞 열이 넘친 칸과 오른쪽 정렬 넘침만큼 줄임)
        let mut lead = if position == 0 { 0 } else { cell.lead };
        lead = lead.saturating_sub(owed);
        owed = 0;
        if cell.right {
            lead = lead.saturating_sub(overflow);
        }
        if lead > 0 {
            spans.push(Span::raw(" ".repeat(lead)));
            cursor += lead;
        }

        // 3. 칸 채움과 클릭 구간
        let gap_fill = " ".repeat(cell.width.saturating_sub(text_width));
        let padded = if cell.right {
            format!("{gap_fill}{text}")
        } else {
            format!("{text}{gap_fill}")
        };
        if !cell.right {
            owed = overflow;
        }
        hits.push(HeaderSpan {
            column: cell.column,
            start: cursor as u16,
            width: cell.width.max(text_width) as u16,
        });
        cursor += Span::raw(padded.as_str()).width();
        spans.push(if active {
            Span::styled(padded, theme::STYLE_ACCENT)
        } else {
            Span::raw(padded)
        });
    }
    (Line::from(spans), hits)
}

// 14. 텍스트 줄 배치 ----------------------------------------------------------
// 박스 표 줄은 끝 말줄임, 일반 줄은 wrap_prose 일 때 줄바꿈 (반환 = 줄 목록, 입력 줄별 시작 행)
pub fn layout_text_lines(
    lines: Vec<Line<'static>>,
    width: usize,
    wrap_prose: bool,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let mut laid_out: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    let mut row_starts: Vec<usize> = Vec::with_capacity(lines.len());
    for line in lines {
        row_starts.push(laid_out.len());

        // 1. 폭 이내 줄
        if width == 0 || line.width() <= width {
            laid_out.push(line);
            continue;
        }

        // 2. 박스 표 줄 (줄 내용 유지, 오른쪽 끝 …)
        let first_letter = line
            .spans
            .iter()
            .flat_map(|span| span.content.chars())
            .find(|letter| !letter.is_whitespace());
        if matches!(first_letter, Some('\u{2500}'..='\u{257F}')) {
            let Line {
                spans,
                style,
                alignment,
            } = line;
            laid_out.push(Line {
                spans: clip_spans_to_width(spans, width),
                style,
                alignment,
            });
            continue;
        }

        // 3. 일반 줄
        if wrap_prose {
            laid_out.extend(wrap_styled_line(line, width));
        } else {
            laid_out.push(line);
        }
    }
    (laid_out, row_starts)
}

// 15. 스타일 유지 줄바꿈 ------------------------------------------------------
// 공백 뒤 줄나눔, 긴 낱말은 글자 단위, 이어지는 줄은 앞 공백·불릿만큼 들여쓰기
fn wrap_styled_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let Line {
        spans,
        style,
        alignment,
    } = line;

    // 1. 글자별 (글자, 스타일, 표시 폭)
    let letters: Vec<(char, Style, usize)> = spans
        .iter()
        .flat_map(|span| {
            span.content.chars().map(move |letter| {
                (
                    letter,
                    span.style,
                    UnicodeWidthChar::width(letter).unwrap_or(0),
                )
            })
        })
        .collect();

    // 2. 이어지는 줄 들여쓰기 (앞 공백 + 불릿, 폭의 절반 이하)
    let leading_spaces = letters
        .iter()
        .take_while(|(letter, _, _)| *letter == ' ')
        .count();
    let bullet_extra = match letters.get(leading_spaces) {
        Some(('\u{2022}', _, _)) => 2,
        _ => 0,
    };
    let hang = (leading_spaces + bullet_extra).min(width / 2);
    let hang_cells: Vec<(char, Style, usize)> = vec![(' ', Style::default(), 1); hang];

    // 3. 행 채움 (넘치면 마지막 공백 뒤에서, 없으면 현재 위치에서 나눠 다음 줄로)
    let mut rows: Vec<Vec<(char, Style, usize)>> = Vec::new();
    let mut row: Vec<(char, Style, usize)> = Vec::new();
    let mut row_width = 0usize;
    let mut break_at: Option<usize> = None;
    for cell in letters {
        if cell.0 == ' ' && row_width + cell.2 > width && row.len() > hang {
            rows.push(std::mem::replace(&mut row, hang_cells.clone()));
            row_width = hang;
            break_at = None;
            continue;
        }
        while cell.0 != ' ' && row_width + cell.2 > width && row.len() > hang {
            let split = break_at.filter(|at| *at > hang).unwrap_or(row.len());
            let carried = row.split_off(split);
            rows.push(std::mem::replace(&mut row, hang_cells.clone()));
            row.extend(carried);
            row_width = row.iter().map(|(_, _, shown)| *shown).sum();
            break_at = None;
        }
        if cell.0 == ' ' && row.len() >= hang {
            break_at = Some(row.len() + 1);
        }
        row_width += cell.2;
        row.push(cell);
    }
    if row.len() > hang || rows.is_empty() {
        rows.push(row);
    }

    // 4. 같은 스타일 글자를 조각으로 묶어 줄 생성
    rows.into_iter()
        .map(|cells| {
            let mut pieces: Vec<Span<'static>> = Vec::new();
            let mut piece = String::new();
            let mut piece_style = Style::default();
            for (letter, letter_style, _) in cells {
                if !piece.is_empty() && letter_style != piece_style {
                    pieces.push(Span::styled(std::mem::take(&mut piece), piece_style));
                }
                piece_style = letter_style;
                piece.push(letter);
            }
            if !piece.is_empty() {
                pieces.push(Span::styled(piece, piece_style));
            }
            Line {
                spans: pieces,
                style,
                alignment,
            }
        })
        .collect()
}

// 16. 열 경계 손잡이 ----------------------------------------------------------
// 평소에는 아무것도 그리지 않고, 마우스 아래는 머리글 칸 하나·끌는 동안은 표 높이 전체 세로선
pub fn render_column_handles(frame: &mut Frame, state: &AppState) {
    let dragging = state.column_drag.map(|drag| (drag.card, drag.column));
    let buf = frame.buffer_mut();
    for hit in &state.hit_areas.column_hits {
        let key = Some((hit.card, hit.column));
        let dragged = dragging == key;
        let hovered = dragging.is_none() && state.column_hover == key;
        if !dragged && !hovered {
            continue;
        }
        let (color, line) = if dragged {
            (theme::SPLIT_GRIP_DRAG, hit.line)
        } else {
            (theme::SPLIT_GRIP_HOT, hit.grab)
        };
        for y in line.y..line.bottom() {
            buf[(line.x, y)].set_symbol("┃").set_fg(color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::query::HookStatRow;
    use crate::tui::state::ActiveTab;
    use crate::tui::test_support::sample_state_with_tables;
    use crate::tui::views::render_frame;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn prose_wraps_at_word_boundaries_without_losing_words() {
        let source = "alpha beta gamma delta epsilon zeta eta theta";
        let (rows, starts) = layout_text_lines(vec![Line::from(source)], 16, true);
        assert!(rows.len() > 1);
        assert_eq!(starts, vec![0]);
        for row in &rows {
            assert!(row.width() <= 16, "{}", line_text(row));
        }
        let words: Vec<String> = rows
            .iter()
            .flat_map(|row| {
                line_text(row)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(words.join(" "), source);
    }

    #[test]
    fn bullet_lines_wrap_with_hanging_indent_and_keep_styles() {
        let bullet = Line::from(vec![
            Span::styled("  \u{2022} ", theme::STYLE_OK),
            Span::styled("가나다 라마바 사아자 차카타 파하", theme::STYLE_TEXT),
        ]);
        let (rows, _) = layout_text_lines(vec![bullet], 20, true);
        assert!(rows.len() > 1);
        assert!(line_text(&rows[0]).starts_with("  \u{2022} 가나다"));
        assert_eq!(rows[0].spans[0].style, theme::STYLE_OK);
        for row in &rows[1..] {
            assert!(line_text(row).starts_with("    "), "{}", line_text(row));
            assert!(!line_text(row).starts_with("     "), "{}", line_text(row));
            assert!(row.width() <= 20);
        }
    }

    #[test]
    fn box_table_lines_are_clipped_with_ellipsis_not_wrapped() {
        let rule = format!("┌{}┐", "─".repeat(30));
        let (rows, starts) =
            layout_text_lines(vec![Line::from(rule), Line::from("short")], 12, true);
        assert_eq!(rows.len(), 2);
        assert_eq!(starts, vec![0, 1]);
        assert_eq!(rows[0].width(), 12);
        assert!(line_text(&rows[0]).ends_with('…'));
        assert_eq!(line_text(&rows[1]), "short");
    }

    #[test]
    fn fixed_row_cards_keep_one_row_per_line() {
        let long = "word ".repeat(20);
        let (rows, starts) = layout_text_lines(vec![Line::from(long), Line::from("x")], 10, false);
        assert_eq!(rows.len(), 2);
        assert_eq!(starts, vec![0, 1]);
    }

    #[test]
    fn start_rows_follow_wrapped_row_counts() {
        let lines = vec![
            Line::from("short"),
            Line::from("one two three four five six seven eight nine ten"),
            Line::from("tail"),
        ];
        let (rows, starts) = layout_text_lines(lines, 12, true);
        assert_eq!(starts[0], 0);
        assert_eq!(starts[1], 1);
        assert!(starts[2] > 2);
        assert_eq!(starts[2], rows.len() - 1);
    }

    fn plan(natural: u16, fit: ColumnFit, user: Option<u16>) -> ColumnPlan {
        ColumnPlan {
            natural,
            floor: if fit == ColumnFit::Fixed {
                natural
            } else {
                MIN_COLUMN_WIDTH.min(natural)
            },
            fit,
            user,
        }
    }

    // 텍스트 열 둘, 끝에 숫자 열 하나 (자연 폭 10, 20, 6)
    fn sample_plans() -> Vec<ColumnPlan> {
        vec![
            plan(10, ColumnFit::Shrink, None),
            plan(20, ColumnFit::Grow, None),
            plan(6, ColumnFit::Fixed, None),
        ]
    }

    #[test]
    fn wide_card_gives_text_its_natural_width_and_last_grow_column_takes_the_slack() {
        let widths = resolve_column_widths(&sample_plans(), 100);
        // 10 + 20 + 6 + 간격 2 = 38, 남는 62 은 맨 오른쪽 Grow 열이 가짐
        assert_eq!(widths, vec![10, 82, 6]);

        // 정확히 맞으면 그대로
        assert_eq!(resolve_column_widths(&sample_plans(), 38), vec![10, 20, 6]);
    }

    #[test]
    fn narrow_card_shrinks_widest_text_first_and_never_touches_numbers() {
        // 38 -> 26: 부족 12 은 글자 열이 가장 넓은 순서로 부담, 숫자 열 6 유지
        let widths = resolve_column_widths(&sample_plans(), 26);
        assert_eq!(widths[2], 6);
        assert_eq!(widths[0] + widths[1] + widths[2] + 2, 26);
        assert!(widths[1] >= widths[0], "{widths:?}");
        assert!(widths[0] < 10 || widths[1] < 20, "{widths:?}");

        // 글자 열이 최소 폭에 닿아도 모자라면 맨 오른쪽 열(숫자)부터 숨김
        let widths = resolve_column_widths(&sample_plans(), 9);
        assert_eq!(widths[2], 0);
        assert_eq!(widths[0] + widths[1] + 1, 9);
        assert!(widths[0] >= 3 && widths[1] >= 3, "{widths:?}");
    }

    #[test]
    fn dragged_widths_are_kept_clamped_to_the_floor_and_shrunk_last() {
        // 끌어 정한 폭은 자연 폭보다 넓어도 유지, 남는 폭은 끌지 않은 Grow 열이 줄여 받음
        let mut plans = sample_plans();
        plans[0].user = Some(15);
        assert_eq!(resolve_column_widths(&plans, 100), vec![15, 77, 6]);

        // 숫자 열은 내용 폭 미만으로 지정해도 내용 폭, 글자 열은 최소 3
        plans[0].user = Some(1);
        plans[2].user = Some(2);
        assert_eq!(resolve_column_widths(&plans, 100), vec![3, 89, 6]);

        // 자동 열이 더 줄일 수 있는 동안 끌어 정한 열은 폭 유지 (자동 열이 최소에 닿으면 그때 줄임)
        let mut plans = sample_plans();
        plans[0].user = Some(10);
        let widths = resolve_column_widths(&plans, 26);
        assert_eq!(widths, vec![10, 8, 6]);
        let widths = resolve_column_widths(&plans, 16);
        assert_eq!(widths.iter().sum::<u16>() + 2, 16);
        assert!(widths[0] <= 10 && widths[1] == 3, "{widths:?}");

        // Grow 열 자체를 끌어 정했으면 남는 폭을 받지 않아 오른쪽이 비어 있음
        let mut plans = sample_plans();
        plans[1].user = Some(12);
        assert_eq!(resolve_column_widths(&plans, 100), vec![10, 12, 6]);
    }

    // 긴 글자가 들어 있는 훅 표 한 행 (자연 폭이 큰 경로·가드·상세)
    fn hooks_state_with_long_text() -> AppState {
        let mut state = sample_state_with_tables();
        state.tab = ActiveTab::Hooks;
        state.snapshot.hooks = Panel::Ready(vec![HookStatRow {
            route: "PreToolUse:mcp__rust-fs-mcp__file-read".to_string(),
            guard: "cli-guard-with-a-long-name".to_string(),
            outcome: "deny".to_string(),
            detail: "git push origin main --force-with-lease".to_string(),
            count: 1_234,
            p50_ms: Some(18),
            p90_ms: Some(23),
            saved_ms: 8_400,
        }]);
        state
    }

    // 버퍼를 줄 단위 문자열로 (넓은 글자 뒤 빈 칸은 건너뛀)
    fn render_rows(state: &mut AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_frame(frame, state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut rows = Vec::new();
        for y in 0..height {
            let mut row = String::new();
            let mut skip = 0usize;
            for x in 0..width {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let symbol = buffer.cell((x, y)).map_or(" ", |cell| cell.symbol());
                skip = Span::raw(symbol).width().saturating_sub(1);
                row.push_str(symbol);
            }
            rows.push(row);
        }
        rows
    }

    #[test]
    fn wide_table_card_shows_every_text_cell_without_ellipsis() {
        let mut state = hooks_state_with_long_text();
        let rows = render_rows(&mut state, 200, 30);
        let row = rows
            .iter()
            .find(|row| row.contains("PreToolUse:"))
            .unwrap_or_else(|| panic!("행 없음: {rows:#?}"));
        for text in [
            "PreToolUse:mcp__rust-fs-mcp__file-read",
            "cli-guard-with-a-long-name",
            "git push origin main --force-with-lease",
            "1,234",
            "18ms",
            "23ms",
            "8.4s",
        ] {
            assert!(row.contains(text), "{text}: {row}");
        }
        assert!(!row.contains('…'), "{row}");
    }

    #[test]
    fn narrow_table_card_truncates_text_before_it_touches_numbers() {
        let mut state = hooks_state_with_long_text();
        let rows = render_rows(&mut state, 70, 30);
        let row = rows
            .iter()
            .find(|row| row.contains("18ms"))
            .unwrap_or_else(|| panic!("행 없음: {rows:#?}"));
        assert!(row.contains('…'), "{row}");
        assert!(
            !row.contains("git push origin main --force-with-lease"),
            "{row}"
        );
        for number in ["1,234", "18ms", "23ms", "8.4s"] {
            assert!(row.contains(number), "{number}: {row}");
        }
        assert!(!state.hit_areas.column_hits.is_empty());
    }

    #[test]
    fn table_columns_never_overflow_the_card_at_any_width() {
        for width in 60..=210u16 {
            let mut state = hooks_state_with_long_text();
            render_rows(&mut state, width, 24);
            let card = state
                .hit_areas
                .card_rects
                .iter()
                .find(|(id, _)| *id == CardId::HookTable)
                .map(|(_, rect)| *rect)
                .unwrap();
            for hit in &state.hit_areas.header_cells {
                assert!(hit.rect.right() <= card.right() - 2, "{width}: {hit:?}");
            }
            for hit in &state.hit_areas.column_hits {
                assert!(hit.grab.right() <= card.right() - 2, "{width}: {hit:?}");
            }
        }
    }
}
