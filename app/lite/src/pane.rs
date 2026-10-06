//! 칸 하나(PTY 세션 + 받은 화면)와 탭(칸 나누기 배치).

use std::sync::Arc;

use kasa_gridview::overlay::Selection;
use kasa_pty::{PtyLayout, PtySession};
use kasa_screen::screen::InlineImageView;
use kasa_screen::{Cell, Row, ScreenUpdate};

pub struct Pane {
    pub session: Arc<PtySession>,
    pub rows: Vec<Row>,
    pub cols: u16,
    pub cursor: (u16, u16),
    pub cursor_visible: bool,
    pub alt_screen: bool,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse_enabled: bool,
    pub mouse_sgr: bool,
    pub mouse_motion: bool,
    pub title: Option<String>,
    pub name: Option<String>,
    pub images: Vec<InlineImageView>,
    /// 탭 줄에 쓸 돌고 있는 프로그램 이름. 그릴 때마다 프로세스 표를 뒤지지 않게 칸·탭이 바뀔 때만 새로 뜬다.
    pub process: Option<String>,
    /// PTY 에 알린 크기 — 같으면 다시 안 부른다.
    pub size: (u16, u16),
}

impl Pane {
    pub fn new(session: Arc<PtySession>, size: (u16, u16)) -> Self {
        Self {
            session,
            rows: Vec::new(),
            cols: size.0,
            cursor: (0, 0),
            cursor_visible: true,
            alt_screen: false,
            app_cursor: false,
            bracketed_paste: false,
            mouse_enabled: false,
            mouse_sgr: false,
            mouse_motion: false,
            title: None,
            name: None,
            images: Vec::new(),
            process: None,
            size,
        }
    }

    /// 받은 화면 차분을 얹는다. 크기가 바뀐 장은 모든 줄을 싣고 온다.
    pub fn apply(&mut self, u: ScreenUpdate) {
        let rows = u.rows as usize;
        self.cols = u.cols;
        self.rows.resize_with(rows, || vec![Cell::blank(); u.cols as usize]);
        self.rows.truncate(rows);
        for (r, row) in u.dirty {
            if let Some(slot) = self.rows.get_mut(r as usize) {
                *slot = row;
            }
        }
        for row in &mut self.rows {
            if row.len() != u.cols as usize {
                row.resize(u.cols as usize, Cell::blank());
            }
        }
        self.cursor = (u.cursor_row, u.cursor_col);
        self.cursor_visible = u.cursor_visible;
        self.alt_screen = u.alt_screen;
        self.app_cursor = u.app_cursor;
        self.bracketed_paste = u.bracketed_paste;
        self.mouse_enabled = u.mouse_enabled;
        self.mouse_sgr = u.mouse_sgr;
        self.mouse_motion = u.mouse_motion;
        if u.title.is_some() {
            self.title = u.title;
        }
        self.images = u.inline_images;
    }

    /// 탭 이름에 쓸 말 — 붙인 이름, 셸이 알린 제목, 돌고 있는 프로그램 순.
    pub fn label(&self) -> String {
        if let Some(n) = self.name.as_ref().filter(|n| !n.is_empty()) {
            return n.clone();
        }
        if let Some(t) = self.title.as_ref().filter(|t| !t.is_empty()) {
            return t.clone();
        }
        self.process.clone().unwrap_or_else(|| "shell".into())
    }

    /// 선택한 칸들의 글. 넓은 글자 뒤의 빈칸(스페이서)은 빼고, 줄 끝 공백은 걷는다.
    pub fn selected_text(&self, sel: Selection) -> String {
        let ((c0, r0), (c1, r1)) = ordered(sel);
        let mut out = String::new();
        for r in r0..=r1 {
            let Some(row) = self.rows.get(r as usize) else { break };
            let from = if r == r0 { c0 as usize } else { 0 };
            let to = if r == r1 { (c1 as usize + 1).min(row.len()) } else { row.len() };
            let mut line = String::new();
            let mut skip = false;
            for cell in row.get(from..to).unwrap_or(&[]) {
                if skip {
                    skip = false;
                    if matches!(cell.ch, ' ' | '\0') {
                        continue;
                    }
                }
                if cell.ch != '\0' {
                    line.push(cell.ch);
                }
                skip = kasa_gridview::renderer::is_wide_char(cell.ch);
            }
            out.push_str(line.trim_end());
            let wrapped = row.last().is_some_and(|c| c.wrapped);
            if r != r1 && !wrapped {
                out.push('\n');
            }
        }
        out
    }
}

/// 읽는 순서로 (시작, 끝).
pub fn ordered(sel: Selection) -> ((u16, u16), (u16, u16)) {
    let (a, b) = (sel.anchor, sel.end);
    if (a.1, a.0) <= (b.1, b.0) {
        (a, b)
    } else {
        (b, a)
    }
}

pub struct Tab {
    pub layout: PtyLayout,
    pub focus: String,
}
