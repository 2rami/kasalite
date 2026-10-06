//! 클라이언트 그리기 — 칸 격자·경계선·탭 줄을 ratatui 버퍼에 옮긴다. 앞뒤 화면 차분과
//! 넓은 글자 뒤 커서 옮기기는 ratatui 가 한다(넓은 글자 다음 칸은 차분에서 건너뛰고,
//! 이어지지 않는 칸마다 커서를 절대 좌표로 옮긴다).

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthChar;

use crate::keys::Modes;
use crate::proto::{self, LayoutMsg, PaneFrame, PaneRect, WCell, WColor};

/// 클라이언트가 들고 있는 칸 하나의 화면.
#[derive(Default)]
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Vec<WCell>>,
    pub cursor: (u16, u16),
    pub cursor_visible: bool,
    pub modes: Modes,
    pub title: Option<String>,
    pub scrolled: u32,
    pub images: Vec<proto::ImageView>,
}

impl Grid {
    pub fn apply(&mut self, f: PaneFrame) {
        if f.cols != self.cols || f.rows != self.rows {
            self.cols = f.cols;
            self.rows = f.rows;
            self.cells = vec![vec![WCell::BLANK; f.cols as usize]; f.rows as usize];
        }
        for (i, row) in f.dirty {
            if let Some(dst) = self.cells.get_mut(i as usize) {
                *dst = row;
                dst.resize(self.cols as usize, WCell::BLANK);
            }
        }
        self.cursor = f.cursor;
        self.cursor_visible = f.cursor_visible;
        self.modes = f.modes;
        if f.title.is_some() {
            self.title = f.title;
        }
        self.scrolled = f.scrolled;
        self.images = f.images;
    }

    /// 칸 좌표 두 점 사이의 글. 넓은 글자의 빈 뒤칸은 건너뛰고 줄 끝 공백은 걷는다.
    pub fn text_between(&self, a: (u16, u16), b: (u16, u16)) -> String {
        let (start, end) = if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) };
        let mut lines = Vec::new();
        for row in start.1..=end.1 {
            let Some(cells) = self.cells.get(row as usize) else { break };
            let from = if row == start.1 { start.0 as usize } else { 0 };
            let to = if row == end.1 { (end.0 as usize + 1).min(cells.len()) } else { cells.len() };
            let line: String = cells
                .get(from..to.max(from))
                .unwrap_or_default()
                .iter()
                .filter(|c| c.attrs & proto::SPACER == 0)
                .map(|c| c.ch)
                .collect();
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n")
    }
}

/// 선택 — 한 칸 안의 두 점(칸 좌표).
#[derive(Clone, Debug)]
pub struct Selection {
    pub pane: String,
    pub anchor: (u16, u16),
    pub head: (u16, u16),
}

impl Selection {
    fn contains(&self, col: u16, row: u16) -> bool {
        let (s, e) = if (self.anchor.1, self.anchor.0) <= (self.head.1, self.head.0) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        (row, col) >= (s.1, s.0) && (row, col) <= (e.1, e.0)
    }
}

pub fn color(c: WColor) -> Color {
    match c {
        WColor::Default => Color::Reset,
        WColor::Idx(i) => Color::Indexed(i),
        WColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn cell_style(c: &WCell) -> Style {
    let mut s = Style::default().fg(color(c.fg)).bg(color(c.bg));
    let mut m = Modifier::empty();
    if c.attrs & proto::BOLD != 0 {
        m |= Modifier::BOLD;
    }
    if c.attrs & proto::ITALIC != 0 {
        m |= Modifier::ITALIC;
    }
    if c.attrs & proto::UNDERLINE != 0 {
        m |= Modifier::UNDERLINED;
    }
    if c.attrs & proto::INVERSE != 0 {
        m |= Modifier::REVERSED;
    }
    if c.attrs & proto::DIM != 0 {
        m |= Modifier::DIM;
    }
    s = s.add_modifier(m);
    s
}

pub fn draw_pane(buf: &mut Buffer, area: Rect, r: &PaneRect, grid: Option<&Grid>, sel: Option<&Selection>) {
    let x0 = r.x.min(area.width);
    let y0 = r.y.min(area.height);
    let w = r.w.min(area.width - x0);
    let h = r.h.min(area.height - y0);
    for row in 0..h {
        let cells = grid.and_then(|g| g.cells.get(row as usize));
        let mut col = 0u16;
        while col < w {
            let Some(cell) = buf.cell_mut((x0 + col, y0 + row)) else { break };
            let wc = cells.and_then(|cs| cs.get(col as usize)).copied().unwrap_or(WCell::BLANK);
            let mut style = cell_style(&wc);
            if sel.is_some_and(|s| s.pane == r.id && s.contains(col, row)) {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let hidden = wc.attrs & (proto::HIDDEN | proto::SPACER) != 0;
            let wide = !hidden && UnicodeWidthChar::width(wc.ch).unwrap_or(1) > 1;
            cell.reset();
            cell.set_style(style);
            if hidden || wc.ch.is_control() {
                cell.set_char(' ');
            } else if wide && col + 1 >= w {
                // 칸 오른쪽 끝에 걸친 넓은 글자는 반만 그릴 수 없다.
                cell.set_char(' ');
            } else {
                cell.set_char(wc.ch);
            }
            if wide && col + 1 < w {
                // 뒤칸은 차분이 건너뛰지만, 바탕색은 같게 칠해 둔다.
                if let Some(next) = buf.cell_mut((x0 + col + 1, y0 + row)) {
                    next.reset();
                    next.set_style(style);
                    next.set_char(' ');
                }
                col += 2;
            } else {
                col += 1;
            }
        }
    }
}

const BORDER: Color = Color::Indexed(240);
const ACCENT: Color = Color::Indexed(75);

/// 분할선. 세로선·가로선을 칸 지도에 찍은 뒤 이웃과 이어지는 모양으로 글자를 고른다.
pub fn draw_borders(buf: &mut Buffer, area: Rect, layout: &LayoutMsg) {
    let (w, h) = (area.width as usize, area.height.saturating_sub(1) as usize);
    if w == 0 || h == 0 {
        return;
    }
    let mut v = vec![false; w * h];
    let mut hz = vec![false; w * h];
    for d in &layout.dividers {
        for i in 0..d.len {
            let (x, y) = if d.vertical_line { (d.at, d.start + i) } else { (d.start + i, d.at) };
            let (x, y) = (x as usize, y as usize);
            if x < w && y < h {
                if d.vertical_line {
                    v[y * w + x] = true;
                } else {
                    hz[y * w + x] = true;
                }
            }
        }
    }
    let focus = layout.panes.iter().find(|p| p.id == layout.focus);
    let any = |x: isize, y: isize| -> (bool, bool) {
        if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
            return (false, false);
        }
        let i = y as usize * w + x as usize;
        (v[i], hz[i])
    };
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !v[i] && !hz[i] {
                continue;
            }
            let (xi, yi) = (x as isize, y as isize);
            let up = (v[i] && { let a = any(xi, yi - 1); a.0 || a.1 }) || (hz[i] && any(xi, yi - 1).0);
            let down = (v[i] && { let a = any(xi, yi + 1); a.0 || a.1 }) || (hz[i] && any(xi, yi + 1).0);
            let left = (hz[i] && { let a = any(xi - 1, yi); a.0 || a.1 }) || (v[i] && any(xi - 1, yi).1);
            let right = (hz[i] && { let a = any(xi + 1, yi); a.0 || a.1 }) || (v[i] && any(xi + 1, yi).1);
            let ch = match (up, down, left, right) {
                (true, true, true, true) => '┼',
                (true, true, true, false) => '┤',
                (true, true, false, true) => '├',
                (true, false, true, true) => '┴',
                (false, true, true, true) => '┬',
                (true, false, true, false) => '┘',
                (true, false, false, true) => '└',
                (false, true, true, false) => '┐',
                (false, true, false, true) => '┌',
                (_, _, true, _) | (_, _, _, true) if hz[i] && !v[i] => '─',
                _ if v[i] => '│',
                _ => '─',
            };
            let touches_focus = focus.is_some_and(|f| {
                let (fx, fy, fw, fh) = (f.x as usize, f.y as usize, f.w as usize, f.h as usize);
                let beside = (x + 1 == fx || x == fx + fw) && y + 1 > fy && y < fy + fh + 1;
                let above_below = (y + 1 == fy || y == fy + fh) && x + 1 > fx && x < fx + fw + 1;
                beside || above_below
            });
            if let Some(cell) = buf.cell_mut((area.x + x as u16, area.y + y as u16)) {
                cell.reset();
                cell.set_char(ch);
                cell.set_fg(if touches_focus { ACCENT } else { BORDER });
            }
        }
    }
}

/// 상태 줄에서 누를 수 있는 것. 접두키 없이 마우스만으로도 칸을 다룰 수 있게 한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Tab(usize),
    NewTab,
    SplitRight,
    SplitDown,
    Zoom,
    Close,
    Help,
}

/// 상태 줄의 누를 자리(무엇, 시작 열, 끝 열).
pub type StatusHits = Vec<(Hit, u16, u16)>;

pub struct StatusLine<'a> {
    pub layout: &'a LayoutMsg,
    /// 접두키 이름. 접두키를 끈 설정이면 `None`.
    pub prefix_label: Option<&'a str>,
    pub prefix_armed: bool,
    pub message: Option<&'a str>,
    pub scroll_mode: Option<u32>,
}

const BUTTONS: [(Hit, &str); 5] = [
    (Hit::SplitRight, " 옆으로 "),
    (Hit::SplitDown, " 아래로 "),
    (Hit::Zoom, " 확대 "),
    (Hit::Close, " 닫기 "),
    (Hit::Help, " ? "),
];

fn text_width(s: &str) -> u16 {
    s.chars().map(|c| UnicodeWidthChar::width(c).unwrap_or(1) as u16).sum()
}

pub fn draw_status(buf: &mut Buffer, area: Rect, s: &StatusLine) -> StatusHits {
    let y = area.y + area.height.saturating_sub(1);
    let base = Style::default().bg(Color::Indexed(236)).fg(Color::Indexed(250));
    for x in 0..area.width {
        if let Some(c) = buf.cell_mut((area.x + x, y)) {
            c.reset();
            c.set_style(base);
            c.set_char(' ');
        }
    }
    let right_edge = area.x + area.width;
    let mut x = area.x;
    let put = |buf: &mut Buffer, x: &mut u16, text: &str, style: Style| {
        let (nx, _) = buf.set_stringn(*x, y, text, right_edge.saturating_sub(*x) as usize, style);
        *x = nx;
    };
    let mut hits = Vec::new();
    put(buf, &mut x, &format!(" {} ", s.layout.session), base.fg(Color::Indexed(16)).bg(ACCENT).add_modifier(Modifier::BOLD));
    put(buf, &mut x, " ", base);
    for (i, name) in s.layout.tabs.iter().enumerate() {
        let label = format!(" {i} {} ", truncate(name, 18));
        let style = if i == s.layout.active_tab {
            base.fg(Color::Indexed(231)).bg(Color::Indexed(240)).add_modifier(Modifier::BOLD)
        } else {
            base
        };
        let start = x;
        put(buf, &mut x, &label, style);
        hits.push((Hit::Tab(i), start, x));
    }
    let start = x;
    put(buf, &mut x, " + ", base.fg(Color::Indexed(252)));
    hits.push((Hit::NewTab, start, x));
    if s.layout.zoomed {
        put(buf, &mut x, " [확대]", base.fg(Color::Indexed(214)));
    }

    // 오른쪽: 단추들, 그 왼쪽에 상태 글(스크롤·알림·접두키 대기).
    let button_style = base.fg(Color::Indexed(252)).bg(Color::Indexed(238));
    let buttons_w: u16 = BUTTONS.iter().map(|(_, t)| text_width(t) + 1).sum();
    let mut bx = right_edge.saturating_sub(buttons_w);
    if bx > x {
        for (hit, label) in BUTTONS {
            let start = bx;
            buf.set_string(bx, y, label, button_style);
            bx += text_width(label);
            hits.push((hit, start, bx));
            bx += 1;
        }
    }
    let note = match (s.scroll_mode, s.message, s.prefix_armed, s.prefix_label) {
        (Some(n), ..) => Some(format!("스크롤 ↑{n} · q 끝 ")),
        (None, Some(m), ..) => Some(format!("{m} ")),
        (None, None, true, Some(p)) => Some(format!("[{p}] 명령 대기 ")),
        (None, None, false, Some(p)) => Some(format!("{p} 접두키 ")),
        (None, None, _, None) => None,
    };
    if let Some(note) = note {
        let nw = text_width(&note);
        let limit = right_edge.saturating_sub(buttons_w + 1);
        if limit > x + nw {
            let style = if s.prefix_armed || s.scroll_mode.is_some() {
                base.fg(Color::Indexed(16)).bg(Color::Indexed(214))
            } else {
                base
            };
            buf.set_string(limit - nw, y, &note, style);
        }
    }
    hits
}

fn truncate(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(1);
        if width + cw > max {
            out.push('…');
            break;
        }
        width += cw;
        out.push(c);
    }
    out
}

pub const HELP: &[(&str, &str)] = &[
    ("%  |", "옆으로 나누기"),
    ("\"  -", "아래로 나누기"),
    ("화살표 hjkl", "칸 옮기기"),
    ("o", "다음 칸"),
    ("z", "칸 확대/되돌리기"),
    ("x", "칸 닫기"),
    ("c", "새 탭"),
    ("n  p  0-9", "탭 옮기기"),
    (",", "탭 이름 바꾸기"),
    ("[", "스크롤 (q 로 끝)"),
    ("d", "떨어지기(세션은 남는다)"),
    ("접두키 두 번", "접두키를 칸에 보내기"),
];

pub fn draw_help(buf: &mut Buffer, area: Rect, prefix_label: Option<&str>) {
    let w = 48u16.min(area.width);
    let h = (HELP.len() as u16 + 4).min(area.height);
    let x0 = area.x + (area.width - w) / 2;
    let y0 = area.y + (area.height.saturating_sub(h)) / 2;
    let style = Style::default().bg(Color::Indexed(235)).fg(Color::Indexed(252));
    for y in 0..h {
        for x in 0..w {
            if let Some(c) = buf.cell_mut((x0 + x, y0 + y)) {
                c.reset();
                c.set_style(style);
                c.set_char(' ');
            }
        }
    }
    let head = match prefix_label {
        Some(p) => format!("{p} 다음에 누른다 · 아래 줄 단추도 된다"),
        None => "접두키를 껐다 — 아래 줄 단추를 누른다".to_string(),
    };
    buf.set_stringn(x0 + 2, y0 + 1, head, (w - 4) as usize, style.add_modifier(Modifier::BOLD));
    for (i, (k, d)) in HELP.iter().enumerate() {
        let y = y0 + 2 + i as u16;
        if y >= y0 + h - 1 {
            break;
        }
        buf.set_stringn(x0 + 2, y, k, 14, style.fg(ACCENT));
        buf.set_stringn(x0 + 16, y, d, (w - 18) as usize, style);
    }
}

/// 그릴 칸들의 격자 묶음.
pub type Grids = HashMap<String, Grid>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::DividerMsg;

    fn layout_two_by_one() -> LayoutMsg {
        // 왼쪽 칸 하나, 오른쪽은 위아래 둘. 세로선은 9열, 가로선은 오른쪽의 4행.
        LayoutMsg {
            session: "t".into(),
            cols: 20,
            rows: 10,
            tabs: vec!["a".into()],
            active_tab: 0,
            panes: vec![
                PaneRect { id: "%0".into(), x: 0, y: 0, w: 9, h: 9 },
                PaneRect { id: "%1".into(), x: 10, y: 0, w: 10, h: 4 },
                PaneRect { id: "%2".into(), x: 10, y: 5, w: 10, h: 4 },
            ],
            dividers: vec![
                DividerMsg { path: vec![], vertical_line: true, at: 9, start: 0, len: 9 },
                DividerMsg { path: vec![1], vertical_line: false, at: 4, start: 10, len: 10 },
            ],
            focus: "%0".into(),
            zoomed: false,
        }
    }

    #[test]
    fn borders_join_into_a_tee() {
        let area = Rect::new(0, 0, 20, 10);
        let mut buf = Buffer::empty(area);
        draw_borders(&mut buf, area, &layout_two_by_one());
        assert_eq!(buf[(9, 0)].symbol(), "│");
        assert_eq!(buf[(9, 4)].symbol(), "├");
        assert_eq!(buf[(12, 4)].symbol(), "─");
    }

    #[test]
    fn selection_text_skips_wide_spacers() {
        let mut g = Grid::default();
        let row = |s: &str| {
            let mut v = Vec::new();
            for ch in s.chars() {
                v.push(WCell { ch, ..WCell::BLANK });
                if UnicodeWidthChar::width(ch) == Some(2) {
                    v.push(WCell { attrs: proto::SPACER, ..WCell::BLANK });
                }
            }
            v
        };
        g.cells = vec![row("한글 ok   "), row("둘째 줄")];
        assert_eq!(g.text_between((0, 0), (3, 1)), "한글 ok\n둘째");
    }
}
