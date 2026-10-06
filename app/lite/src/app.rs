//! 창 하나 — 탭(방)마다 칸 나누기, 칸마다 PTY. 그리기는 엔진(`kasa_gridview`), 박자는 macOS 디스플레이
//! 링크(다른 OS 는 주사율 타이머).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kasa_gridview::images::{inline_slot, InlineImages};
use kasa_gridview::overlay::{PaneOverlay, Selection};
use kasa_gridview::renderer::{is_wide_char, GridFonts, GridRenderer, PaneSlot};
use kasa_gridview::{CursorShape, Palette};
use kasa_pty::{PtyLayout, PtyOptions, PtySession, SplitDir};
use kasa_screen::{Cell, Color, ScreenUpdate};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::bench::{media_time, Bench, Step, Trace};
use crate::control::{self, Ctl};
use crate::keys::{self, Modes};
use crate::pane::{Pane, Tab};

pub enum UserEvent {
    Screen(String, ScreenUpdate),
    Ctl(Ctl),
    Tick,
}

/// 칸 둘레 여백(논리 px).
const PAD_X: f32 = 6.0;
const PAD_Y: f32 = 4.0;
/// 입력·출력 뒤 링크로 화면을 붙잡는 시간 — ProMotion 이 쉰 뒤 주사율을 내리지 않게(spikes/frame-pacing).
const HOLD: Duration = Duration::from_millis(1000);
/// 링크가 없는 플랫폼의 박자 하한.
const FALLBACK_FRAME: Duration = Duration::from_micros(8_333);
/// 내장 Nerd 기호 — 시스템 글꼴에 없는 아이콘(U+E000..F8FF·U+F0000..)의 마지막 그물.
const SYMBOLS_NERD_FONT_MONO: &[u8] = include_bytes!("../../../assets/fonts/SymbolsNerdFontMono-Regular.ttf");
const TAB_BG: [u8; 4] = [30, 36, 44, 255];
const TAB_ACTIVE_BG: [u8; 4] = [52, 62, 76, 255];
const DIVIDER: [u8; 4] = [80, 92, 110, 200];

enum Drag {
    Select(String),
    Divider { path: Vec<u8>, dir: SplitDir },
    Report { pane: String, button: u8 },
}

struct PaneRect {
    id: String,
    /// 칸 글자판 왼쪽 위(논리 px).
    x: f32,
    y: f32,
    cols: u16,
    rows: u16,
}

pub struct App {
    proxy: EventLoopProxy<UserEvent>,
    settings: crate::config::Settings,
    font_size: f32,
    window: Option<Arc<Window>>,
    grid: Option<GridRenderer>,
    images: InlineImages,
    tabs: Vec<Tab>,
    active: usize,
    panes: HashMap<String, Pane>,
    next_pane: u32,
    shared: Arc<Mutex<control::Shared>>,
    mods: ModifiersState,
    cursor_px: PhysicalPosition<f64>,
    focused: bool,
    hangul: kasa_ime::Composer,
    preedit: String,
    selection: Option<(String, Selection)>,
    drag: Option<Drag>,
    clipboard: Option<arboard::Clipboard>,
    dirty: bool,
    seq: u64,
    last_render: Instant,
    hold_until: Option<Instant>,
    /// 키를 받은 시각 — 그 키의 메아리가 처음 그려진 장에 묶어 계측한다.
    key_waiting: Vec<f64>,
    key_echoed: bool,
    #[cfg(target_os = "macos")]
    link: Option<crate::mac::Link>,
    trace: Option<Trace>,
    bench: Option<Bench>,
}

impl App {
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        let settings = crate::config::load();
        Self {
            proxy,
            font_size: settings.font_size,
            settings,
            window: None,
            grid: None,
            images: InlineImages::default(),
            tabs: Vec::new(),
            active: 0,
            panes: HashMap::new(),
            next_pane: 0,
            shared: Arc::new(Mutex::new(control::Shared::default())),
            mods: ModifiersState::empty(),
            cursor_px: PhysicalPosition::new(0.0, 0.0),
            focused: true,
            hangul: kasa_ime::Composer::new(),
            preedit: String::new(),
            selection: None,
            drag: None,
            clipboard: arboard::Clipboard::new().ok(),
            dirty: true,
            seq: 0,
            last_render: Instant::now(),
            hold_until: None,
            key_waiting: Vec::new(),
            key_echoed: false,
            #[cfg(target_os = "macos")]
            link: None,
            trace: None,
            bench: None,
        }
    }

    // ---- 칸·탭 ----------------------------------------------------------------------------------

    fn spawn_pane(&mut self, cwd: Option<String>, size: (u16, u16)) -> anyhow::Result<String> {
        let id = format!("%{}", self.next_pane);
        self.next_pane += 1;
        let session = Arc::new(PtySession::start(PtyOptions {
            cwd,
            cols: size.0.max(2),
            rows: size.1.max(1),
            pane_id: id.clone(),
            ..PtyOptions::default()
        })?);
        let rx = session.screens.clone();
        let proxy = self.proxy.clone();
        let pane_id = id.clone();
        // 엔진의 화면 채널(256)이 차면 부분 손상 장이 버려지고 그 줄은 바뀔 때까지 다시 안 온다 — 받는 즉시
        // 창 루프로 옮겨 채널을 늘 비워 둔다.
        std::thread::Builder::new().name(format!("lite-pane-{id}")).spawn(move || {
            while let Ok(u) = rx.recv() {
                let eof = u.eof;
                if proxy.send_event(UserEvent::Screen(pane_id.clone(), u)).is_err() || eof {
                    return;
                }
            }
        })?;
        kasa_pty::register_session(&id, &session);
        self.panes.insert(id.clone(), Pane::new(session, size));
        Ok(id)
    }

    fn focused_id(&self) -> Option<String> {
        self.tabs.get(self.active).map(|t| t.focus.clone())
    }

    fn focused_cwd(&self) -> Option<String> {
        let id = self.focused_id()?;
        let p = self.panes.get(&id)?;
        p.session.reported_cwd().map(|c| c.to_string_lossy().into_owned())
    }

    fn new_tab(&mut self) -> anyhow::Result<String> {
        let cwd = self.focused_cwd();
        let (cols, rows) = self.grid_cells();
        let id = self.spawn_pane(cwd, (cols, rows.saturating_sub(1).max(1)))?;
        self.tabs.push(Tab { layout: PtyLayout::single(id.clone()), focus: id.clone() });
        self.active = self.tabs.len() - 1;
        self.selection = None;
        self.apply_sizes();
        self.sync_shared();
        Ok(id)
    }

    fn split(&mut self, from: Option<String>, right: bool, focus: bool) -> anyhow::Result<String> {
        let ti = match &from {
            Some(f) => self.tabs.iter().position(|t| t.layout.leaves().contains(&f.as_str())),
            None => Some(self.active),
        }
        .ok_or_else(|| anyhow::anyhow!("칸 {} 이(가) 없다", from.clone().unwrap_or_default()))?;
        let target = from.unwrap_or_else(|| self.tabs[ti].focus.clone());
        let cwd = self.panes.get(&target).and_then(|p| p.session.reported_cwd()).map(|c| c.to_string_lossy().into_owned());
        let new_id = self.spawn_pane(cwd, (10, 5))?;
        let dir = if right { SplitDir::Horizontal } else { SplitDir::Vertical };
        if !self.tabs[ti].layout.split_leaf(&target, dir, new_id.clone()) {
            if let Some(p) = self.panes.remove(&new_id) {
                p.session.terminate_local();
            }
            kasa_pty::release_session(&new_id);
            anyhow::bail!("칸 {target} 을 더 나눌 자리가 없다");
        }
        if focus {
            self.tabs[ti].focus = new_id.clone();
            self.active = ti;
        }
        self.apply_sizes();
        self.sync_shared();
        Ok(new_id)
    }

    fn pane_exited(&mut self, id: &str, el: &ActiveEventLoop) {
        self.panes.remove(id);
        kasa_pty::release_session(id);
        if self.selection.as_ref().is_some_and(|(p, _)| p == id) {
            self.selection = None;
        }
        if let Some(ti) = self.tabs.iter().position(|t| t.layout.leaves().contains(&id)) {
            let tab = &mut self.tabs[ti];
            if tab.layout.leaves().len() <= 1 {
                self.tabs.remove(ti);
                if self.active >= self.tabs.len() {
                    self.active = self.tabs.len().saturating_sub(1);
                } else if ti < self.active {
                    self.active -= 1;
                }
            } else {
                let order: Vec<String> = tab.layout.leaves().iter().map(|s| s.to_string()).collect();
                tab.layout.remove_leaf(id);
                if tab.focus == id {
                    let at = order.iter().position(|p| p == id).unwrap_or(0);
                    let rest = tab.layout.leaves();
                    tab.focus = rest.get(at.min(rest.len().saturating_sub(1))).map(|s| s.to_string()).unwrap_or_default();
                }
            }
        }
        if self.tabs.is_empty() {
            self.quit(el);
            return;
        }
        self.apply_sizes();
        self.sync_shared();
        self.dirty = true;
    }

    fn close_focused(&mut self) {
        if let Some(p) = self.focused_id().and_then(|id| self.panes.get(&id)) {
            p.session.terminate_local();
        }
    }

    fn focus_step(&mut self, delta: i32) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let leaves: Vec<String> = tab.layout.leaves().iter().map(|s| s.to_string()).collect();
        let at = leaves.iter().position(|l| *l == tab.focus).unwrap_or(0) as i32;
        let n = leaves.len() as i32;
        tab.focus = leaves[((at + delta).rem_euclid(n)) as usize].clone();
        self.flush_hangul();
        self.sync_shared();
        self.dirty = true;
    }

    fn switch_tab(&mut self, i: usize) {
        if i < self.tabs.len() && i != self.active {
            self.flush_hangul();
            self.active = i;
            self.selection = None;
            self.sync_shared();
            self.dirty = true;
        }
    }

    fn sync_shared(&mut self) {
        for p in self.panes.values_mut() {
            p.process = p.session.active_process_name();
        }
        let mut s = self.shared.lock().unwrap();
        s.tabs = (0..self.tabs.len()).map(|i| format!("tab-{}", i + 1)).collect();
        s.active_tab = self.active;
        s.focus = self.focused_id().unwrap_or_default();
        s.panes = self
            .tabs
            .iter()
            .enumerate()
            .flat_map(|(i, t)| t.layout.leaves().into_iter().map(move |id| (i, id.to_string())))
            .map(|(i, id)| {
                let p = self.panes.get(&id);
                let name = p.and_then(|p| p.name.clone());
                let cwd = p.and_then(|p| p.session.reported_cwd()).map(|c| c.to_string_lossy().into_owned());
                (id, format!("tab-{}", i + 1), name, cwd)
            })
            .collect();
    }

    // ---- 자리 계산 --------------------------------------------------------------------------------

    fn metrics(&self) -> (f32, f32, f32) {
        let g = self.grid.as_ref().expect("grid");
        (g.cell_w, g.cell_h, g.scale)
    }

    /// 창 전체의 글자 칸 수(여백 뺀).
    fn grid_cells(&self) -> (u16, u16) {
        let (Some(g), Some(w)) = (self.grid.as_ref(), self.window.as_ref()) else { return (80, 24) };
        let size = w.inner_size();
        let (lw, lh) = (size.width as f32 / g.scale, size.height as f32 / g.scale);
        (
            (((lw - 2.0 * PAD_X) / g.cell_w).floor() as u16).max(10),
            (((lh - 2.0 * PAD_Y) / g.cell_h).floor() as u16).max(3),
        )
    }

    fn tab_rows(&self) -> u16 {
        u16::from(self.tabs.len() > 1)
    }

    /// 칸 영역(탭 줄 아래)의 크기.
    fn pane_area(&self) -> (u16, u16) {
        let (c, r) = self.grid_cells();
        (c, r.saturating_sub(self.tab_rows()).max(1))
    }

    /// 칸마다 경계선 몫을 뗀 글자 자리. 오른쪽·아래가 끝이 아니면 한 칸씩 내준다.
    fn cell_rects(layout: &PtyLayout, area: (u16, u16)) -> Vec<(String, u16, u16, u16, u16)> {
        let (cols, rows) = area;
        layout
            .leaf_rects(cols, rows)
            .into_iter()
            .map(|(id, x, y, w, h)| {
                let w = if x + w < cols { w.saturating_sub(1) } else { w };
                let h = if y + h < rows { h.saturating_sub(1) } else { h };
                (id, x, y, w.max(1), h.max(1))
            })
            .collect()
    }

    fn pane_rects(&self) -> Vec<PaneRect> {
        let Some(tab) = self.tabs.get(self.active) else { return Vec::new() };
        let (cw, ch, _) = self.metrics();
        let top = PAD_Y + self.tab_rows() as f32 * ch;
        Self::cell_rects(&tab.layout, self.pane_area())
            .into_iter()
            .map(|(id, x, y, w, h)| PaneRect { id, x: PAD_X + x as f32 * cw, y: top + y as f32 * ch, cols: w, rows: h })
            .collect()
    }

    fn apply_sizes(&mut self) {
        let area = self.pane_area();
        let mut want = Vec::new();
        for tab in &self.tabs {
            want.extend(Self::cell_rects(&tab.layout, area).into_iter().map(|(id, _, _, w, h)| (id, w, h)));
        }
        if let Some((cw, ch, s)) = self.grid.as_ref().map(|g| (g.cell_w, g.cell_h, g.scale)) {
            kasa_pty::set_cell_pixels((cw * s).round() as u32, (ch * s).round() as u32);
        }
        for (id, w, h) in want {
            if let Some(p) = self.panes.get_mut(&id) {
                if p.size != (w, h) {
                    p.size = (w, h);
                    let _ = p.session.resize(w, h);
                }
            }
        }
        self.dirty = true;
    }

    /// 창 px → (칸 id, 열, 행). 칸 밖이면 None.
    fn hit_pane(&self, px: PhysicalPosition<f64>) -> Option<(String, u16, u16)> {
        let (cw, ch, s) = self.metrics();
        let (x, y) = (px.x as f32 / s, px.y as f32 / s);
        self.pane_rects().into_iter().find_map(|r| {
            let (w, h) = (r.cols as f32 * cw, r.rows as f32 * ch);
            (x >= r.x && x < r.x + w && y >= r.y && y < r.y + h)
                .then(|| (r.id, ((x - r.x) / cw) as u16, ((y - r.y) / ch) as u16))
        })
    }

    /// 칸 안 셀 좌표(칸 밖으로 끌면 가장자리에 붙인다).
    fn cell_in(&self, pane: &str, px: PhysicalPosition<f64>) -> Option<(u16, u16)> {
        let (cw, ch, s) = self.metrics();
        let r = self.pane_rects().into_iter().find(|r| r.id == pane)?;
        let col = ((px.x as f32 / s - r.x) / cw).floor().clamp(0.0, r.cols.saturating_sub(1) as f32) as u16;
        let row = ((px.y as f32 / s - r.y) / ch).floor().clamp(0.0, r.rows.saturating_sub(1) as f32) as u16;
        Some((col, row))
    }

    fn hit_divider(&self, px: PhysicalPosition<f64>) -> Option<(Vec<u8>, SplitDir)> {
        let tab = self.tabs.get(self.active)?;
        let (cw, ch, s) = self.metrics();
        let (x, y) = (px.x as f32 / s, px.y as f32 / s);
        let top = PAD_Y + self.tab_rows() as f32 * ch;
        let (cols, rows) = self.pane_area();
        tab.layout.dividers(cols, rows).into_iter().find_map(|d| {
            let span0 = d.span_start as f32;
            let span1 = (d.span_start + d.span_len) as f32;
            let hit = match d.dir {
                SplitDir::Horizontal => {
                    let lx = PAD_X + (d.edge as f32 - 0.5) * cw;
                    (x - lx).abs() <= cw * 0.6 && y >= top + span0 * ch && y <= top + span1 * ch
                }
                SplitDir::Vertical => {
                    let ly = top + (d.edge as f32 - 0.5) * ch;
                    (y - ly).abs() <= ch * 0.6 && x >= PAD_X + span0 * cw && x <= PAD_X + span1 * cw
                }
            };
            hit.then(|| (d.path.clone(), d.dir))
        })
    }

    // ---- 그리기 -----------------------------------------------------------------------------------

    fn render(&mut self) {
        if self.grid.is_none() || self.tabs.is_empty() {
            return;
        }
        let t0 = Instant::now();
        let palette = self.grid.as_ref().unwrap().palette;
        let rects = self.pane_rects();
        let focus = self.focused_id().unwrap_or_default();
        let tab_row = self.tab_bar_row(&palette);
        let (cw, ch, s) = self.metrics();
        let multi = rects.len() > 1;
        let area = self.pane_area();
        let top = PAD_Y + self.tab_rows() as f32 * ch;
        let g = self.grid.as_mut().unwrap();
        g.clear_chrome();
        g.maintain_atlas();
        if let Some(row) = &tab_row {
            g.rect(0.0, 0.0, g.surface_size().0 as f32 / s, PAD_Y + ch, TAB_BG);
            let rows = std::slice::from_ref(row);
            g.draw_cells(&[PaneSlot {
                rows,
                origin_px: (PAD_X * s, PAD_Y * s),
                font_scale: 1.0,
                dim: false,
                links: Vec::new(),
                default_fg: palette.fg,
                source: None,
            }]);
        }
        let slots: Vec<PaneSlot> = rects
            .iter()
            .filter_map(|r| {
                let p = self.panes.get(&r.id)?;
                Some(PaneSlot {
                    rows: &p.rows,
                    origin_px: (r.x * s, r.y * s),
                    font_scale: 1.0,
                    dim: multi && r.id != focus,
                    links: Vec::new(),
                    default_fg: palette.fg,
                    source: None,
                })
            })
            .collect();
        g.draw_cells(&slots);
        drop(slots);
        if let Some(tab) = self.tabs.get(self.active) {
            for d in tab.layout.dividers(area.0, area.1) {
                match d.dir {
                    SplitDir::Horizontal => {
                        let x = PAD_X + (d.edge as f32 - 0.5) * cw;
                        g.rect(x, top + d.span_start as f32 * ch, 1.0, d.span_len as f32 * ch, DIVIDER);
                    }
                    SplitDir::Vertical => {
                        let y = top + (d.edge as f32 - 0.5) * ch;
                        g.rect(PAD_X + d.span_start as f32 * cw, y, d.span_len as f32 * cw, 1.0, DIVIDER);
                    }
                }
            }
        }
        let mut inline = Vec::new();
        for r in &rects {
            let Some(p) = self.panes.get(&r.id) else { continue };
            for v in &p.images {
                let key = format!("inline:{}:{}:{}", r.id, v.id, v.path);
                inline.push(inline_slot(v, key, (r.x, r.y), (cw, ch), p.rows.len(), 0));
            }
        }
        self.images.paint(g, &inline);
        for r in &rects {
            let Some(p) = self.panes.get(&r.id) else { continue };
            let sel = self.selection.as_ref().filter(|(id, _)| *id == r.id).map(|(_, s)| *s);
            let is_focus = r.id == focus;
            if !is_focus && sel.is_none() {
                continue;
            }
            let (row, col) = p.cursor;
            let wide = p.rows.get(row as usize).and_then(|l| l.get(col as usize)).is_some_and(|c| is_wide_char(c.ch));
            kasa_gridview::overlay::paint(
                g,
                &PaneOverlay {
                    cell_w: cw,
                    cell_h: ch,
                    pad_x: r.x,
                    pad_y: r.y,
                    cursor_row: row,
                    cursor_col: col,
                    cursor_w: if wide { 2 } else { 1 },
                    // 창이 초점을 잃으면 빈 테로 — 어디에 칠지는 보이되 지금 치는 창이 아님을 알린다.
                    cursor_shape: if self.focused { CursorShape::Block } else { CursorShape::Frame },
                    cursor_thickness: 2.0,
                    cursor_color: palette.cursor,
                    cursor_visible: is_focus && p.cursor_visible,
                    cols: r.cols,
                    blink_on: true,
                    preedit: if is_focus { self.preedit.clone() } else { String::new() },
                    preedit_row: row,
                    preedit_col: col,
                    font_scale: 1.0,
                    selection: sel,
                    suggestion: String::new(),
                },
            );
        }
        self.seq += 1;
        #[cfg(target_os = "macos")]
        crate::mac::set_frame_seq(self.seq);
        let t_keys = (self.key_echoed && !self.key_waiting.is_empty()).then(|| std::mem::take(&mut self.key_waiting));
        match g.render(time_secs(), true, |_, _, _, _, _, _| {}) {
            Ok(_) => {}
            Err(e) => {
                let size = g.surface_size();
                g.resize(size.0, size.1);
                eprintln!("[lite] 그리기 실패: {e:#}");
            }
        }
        if let Some(t) = self.trace.as_mut() {
            t.present(self.seq, t0.elapsed().as_secs_f64() * 1e3);
            for k in t_keys.into_iter().flatten() {
                t.key(k, self.seq);
            }
        }
        self.key_echoed = false;
        self.dirty = false;
        self.last_render = Instant::now();
        self.update_ime_area();
    }

    /// 탭이 둘 이상일 때 맨 위 한 줄. 셀로 그려 칸 글자와 같은 글꼴·박자를 탄다.
    fn tab_bar_row(&self, palette: &Palette) -> Option<Vec<Cell>> {
        if self.tabs.len() < 2 {
            return None;
        }
        let (cols, _) = self.grid_cells();
        let mut row = Vec::with_capacity(cols as usize);
        for (i, tab) in self.tabs.iter().enumerate() {
            let label = self.panes.get(&tab.focus).map(|p| p.label()).unwrap_or_default();
            let text = format!(" {} {} ", i + 1, truncate(&label, 18));
            let active = i == self.active;
            let bg = if active { TAB_ACTIVE_BG } else { TAB_BG };
            for c in text.chars() {
                row.push(Cell {
                    ch: c,
                    fg: if active { rgb(palette.fg) } else { Color::Rgb(150, 158, 170) },
                    bg: rgb(bg),
                    bold: active,
                    ..Cell::blank()
                });
                if is_wide_char(c) {
                    row.push(Cell { ch: '\0', bg: rgb(bg), ..Cell::blank() });
                }
            }
            row.push(Cell { ch: ' ', bg: rgb(TAB_BG), ..Cell::blank() });
        }
        row.truncate(cols as usize);
        Some(row)
    }

    fn tab_at(&self, col: u16) -> Option<usize> {
        let mut x = 0u16;
        for (i, tab) in self.tabs.iter().enumerate() {
            let label = self.panes.get(&tab.focus).map(|p| p.label()).unwrap_or_default();
            let w: u16 = format!(" {} {} ", i + 1, truncate(&label, 18)).chars().map(|c| if is_wide_char(c) { 2 } else { 1 }).sum::<u16>() + 1;
            if col < x + w {
                return Some(i);
            }
            x += w;
        }
        None
    }

    fn update_ime_area(&self) {
        let (Some(w), Some(id)) = (self.window.as_ref(), self.focused_id()) else { return };
        let Some(p) = self.panes.get(&id) else { return };
        let Some(r) = self.pane_rects().into_iter().find(|r| r.id == id) else { return };
        let (cw, ch, _) = self.metrics();
        let x = r.x + p.cursor.1 as f32 * cw;
        let y = r.y + p.cursor.0 as f32 * ch;
        w.set_ime_cursor_area(winit::dpi::LogicalPosition::new(x, y), LogicalSize::new(cw, ch));
    }

    // ---- 박자 -------------------------------------------------------------------------------------

    fn holding(&self) -> bool {
        self.hold_until.is_some_and(|h| Instant::now() < h)
    }

    /// 화면이 바뀌었다. 붙잡는 중이 아니면 바로 그리고(키 메아리가 박자를 안 기다린다) 박자를 붙잡는다.
    /// 붙잡는 중이면 다음 박자에 실린다.
    fn frame_wanted(&mut self) {
        self.dirty = true;
        #[cfg(target_os = "macos")]
        if self.link.is_some() {
            if !self.holding() {
                self.render();
            }
            self.hold_until = Some(Instant::now() + HOLD);
            if let Some(l) = &self.link {
                l.set_running(true);
            }
            return;
        }
        if self.last_render.elapsed() >= FALLBACK_FRAME {
            self.render();
        }
    }

    fn on_tick(&mut self) {
        // 붙잡는 동안은 바뀐 게 없어도 매 박자 낸다 — ProMotion 은 장이 끊기면 주사율을 내리고, 그 뒤 첫 키가
        // 낮은 주사율의 다음 박자를 기다린다(spikes/frame-pacing).
        if self.dirty || self.holding() {
            self.render();
        }
        #[cfg(target_os = "macos")]
        if !self.holding() && !self.dirty {
            if let Some(l) = &self.link {
                l.set_running(false);
            }
        }
    }

    // ---- 입력 -------------------------------------------------------------------------------------

    fn host_mod(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.mods.super_key()
        } else {
            self.mods.control_key() && self.mods.shift_key()
        }
    }

    fn send_focused(&mut self, bytes: &[u8]) {
        let Some(id) = self.focused_id() else { return };
        if let Some(p) = self.panes.get(&id) {
            if p.session.view_state().0 > 0 {
                p.session.scroll_to_bottom();
            }
            let _ = p.session.send_bytes(bytes);
        }
        self.selection = None;
    }

    fn flush_hangul(&mut self) {
        if let Some(s) = self.hangul.flush() {
            self.send_focused(s.as_bytes());
        }
        if !self.preedit.is_empty() {
            self.preedit.clear();
            self.dirty = true;
        }
    }

    fn shortcut(&mut self, event: &KeyEvent, el: &ActiveEventLoop) -> bool {
        if !self.host_mod() {
            // 윈도우 터미널 식 나누기(Alt+Shift+= / -)도 받는다.
            if self.mods.alt_key() && self.mods.shift_key() {
                if let Key::Character(c) = &event.logical_key {
                    match c.as_str() {
                        "+" | "=" => return self.split(None, true, true).is_ok(),
                        "_" | "-" => return self.split(None, false, true).is_ok(),
                        _ => {}
                    }
                }
            }
            return false;
        }
        let shift = self.mods.shift_key() && cfg!(target_os = "macos");
        let Key::Character(c) = &event.logical_key else { return false };
        let c = c.to_lowercase();
        match c.as_str() {
            "d" if shift => {
                let _ = self.split(None, false, true);
            }
            "d" => {
                let _ = self.split(None, true, true);
            }
            "e" if !cfg!(target_os = "macos") => {
                let _ = self.split(None, false, true);
            }
            "t" => {
                let _ = self.new_tab();
            }
            "w" => self.close_focused(),
            "q" if cfg!(target_os = "macos") => self.quit(el),
            "c" => self.copy_selection(),
            "v" => self.paste(),
            "[" | "{" if shift => self.switch_tab((self.active + self.tabs.len() - 1) % self.tabs.len().max(1)),
            "]" | "}" if shift => self.switch_tab((self.active + 1) % self.tabs.len().max(1)),
            "[" => self.focus_step(-1),
            "]" => self.focus_step(1),
            "=" | "+" => self.zoom(1.0),
            "-" | "_" => self.zoom(-1.0),
            "0" => self.zoom(0.0),
            d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() => {
                let n = (d.as_bytes()[0] - b'0') as usize;
                self.switch_tab(if n == 9 { self.tabs.len() - 1 } else { n.saturating_sub(1) });
            }
            _ => return false,
        }
        self.dirty = true;
        true
    }

    fn zoom(&mut self, step: f32) {
        self.font_size = if step == 0.0 { crate::config::Settings::default().font_size } else { (self.font_size + step).clamp(8.0, 40.0) };
        if let Some(g) = self.grid.as_mut() {
            g.set_font_size(self.font_size);
        }
        crate::config::save_font_size(self.font_size);
        self.apply_sizes();
    }

    fn copy_selection(&mut self) {
        let Some((id, sel)) = self.selection.clone() else { return };
        let Some(text) = self.panes.get(&id).map(|p| p.selected_text(sel)) else { return };
        if let Some(cb) = self.clipboard.as_mut() {
            let _ = cb.set_text(text);
        }
    }

    fn paste(&mut self) {
        let Some(text) = self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()) else { return };
        let bracketed = self.focused_id().and_then(|id| self.panes.get(&id)).is_some_and(|p| p.bracketed_paste);
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if bracketed {
            let mut b = b"\x1b[200~".to_vec();
            b.extend_from_slice(text.as_bytes());
            b.extend_from_slice(b"\x1b[201~");
            self.send_focused(&b);
        } else {
            self.send_focused(text.as_bytes());
        }
    }

    fn on_key(&mut self, event: KeyEvent, el: &ActiveEventLoop) {
        if event.state != ElementState::Pressed {
            return;
        }
        if self.shortcut(&event, el) {
            return;
        }
        self.key_waiting.push(media_time());
        // macOS: OS IME 를 끄고 두벌식 자모를 우리 조합기로 받는다 — NSTextInputContext 가 첫 키를 먹는
        // 문제를 피하는 본판과 같은 길이다. 다른 OS 는 OS IME(Ime::Preedit/Commit)가 조합한다.
        #[cfg(target_os = "macos")]
        if !self.mods.control_key() && !self.mods.super_key() {
            if let Some(t) = event.text.as_ref().filter(|t| t.chars().count() == 1) {
                let c = t.chars().next().unwrap();
                if (0x3130..=0x318F).contains(&(c as u32)) {
                    if let Some(commit) = self.hangul.feed(c) {
                        self.send_focused(commit.as_bytes());
                    }
                    self.preedit = self.hangul.preedit().unwrap_or_default();
                    self.frame_wanted();
                    return;
                }
            }
            if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
                self.preedit = self.hangul.preedit().unwrap_or_default();
                self.frame_wanted();
                return;
            }
        }
        if !self.preedit.is_empty() && cfg!(not(target_os = "macos")) {
            return;
        }
        let modes = self
            .focused_id()
            .and_then(|id| self.panes.get(&id))
            .map(|p| Modes { app_cursor: p.app_cursor, alt_screen: p.alt_screen })
            .unwrap_or_default();
        let Some(bytes) = keys::encode(&event, self.mods, modes) else { return };
        let mut out = Vec::new();
        if let Some(s) = self.hangul.flush() {
            out.extend_from_slice(s.as_bytes());
            self.preedit.clear();
        }
        out.extend_from_slice(&bytes);
        self.send_focused(&out);
    }

    fn on_ime(&mut self, ime: Ime) {
        match ime {
            Ime::Preedit(text, _) => {
                self.preedit = text;
                self.frame_wanted();
            }
            Ime::Commit(text) => {
                self.preedit.clear();
                self.send_focused(text.as_bytes());
                self.frame_wanted();
            }
            Ime::Enabled | Ime::Disabled => {
                self.preedit.clear();
            }
        }
    }

    fn mouse_report(&self, pane: &str, button: u8, cell: (u16, u16), press: bool, motion: bool) {
        let Some(p) = self.panes.get(pane) else { return };
        let bytes = if p.mouse_sgr {
            Some(keys::mouse_sgr(button, cell.0, cell.1, press, motion, self.mods))
        } else {
            keys::mouse_x10(button, cell.0, cell.1, press, motion)
        };
        if let Some(b) = bytes {
            let _ = p.session.send_bytes(&b);
        }
    }

    fn on_mouse(&mut self, state: ElementState, button: MouseButton) {
        let px = self.cursor_px;
        if state == ElementState::Released {
            match self.drag.take() {
                Some(Drag::Report { pane, button }) => {
                    if let Some(cell) = self.cell_in(&pane, px) {
                        self.mouse_report(&pane, button, cell, false, false);
                    }
                }
                Some(Drag::Select(pane)) => {
                    if self.selection.as_ref().is_some_and(|(p, s)| *p == pane && s.anchor == s.end) {
                        self.selection = None;
                        self.dirty = true;
                    }
                }
                _ => {}
            }
            self.frame_wanted();
            return;
        }
        let (_, ch, s) = self.metrics();
        if self.tab_rows() > 0 && (px.y as f32 / s) < PAD_Y + ch {
            let (cw, _, _) = self.metrics();
            let col = ((px.x as f32 / s - PAD_X) / cw).max(0.0) as u16;
            if let Some(i) = self.tab_at(col) {
                self.switch_tab(i);
                self.frame_wanted();
            }
            return;
        }
        if button == MouseButton::Left {
            if let Some((path, dir)) = self.hit_divider(px) {
                self.drag = Some(Drag::Divider { path, dir });
                return;
            }
        }
        let Some((id, col, row)) = self.hit_pane(px) else { return };
        if self.focused_id().as_deref() != Some(id.as_str()) {
            self.flush_hangul();
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.focus = id.clone();
            }
            self.sync_shared();
        }
        let reports = self.panes.get(&id).is_some_and(|p| p.mouse_enabled) && !self.mods.shift_key();
        if reports {
            if let Some(b) = keys::button_code(button) {
                self.mouse_report(&id, b, (col, row), true, false);
                self.drag = Some(Drag::Report { pane: id, button: b });
            }
        } else if button == MouseButton::Left {
            self.selection = Some((id.clone(), Selection { anchor: (col, row), end: (col, row) }));
            self.drag = Some(Drag::Select(id));
        } else if button == MouseButton::Middle {
            self.paste();
        }
        self.frame_wanted();
    }

    fn on_cursor_moved(&mut self, px: PhysicalPosition<f64>) {
        self.cursor_px = px;
        match &self.drag {
            Some(Drag::Select(pane)) => {
                let pane = pane.clone();
                if let Some(cell) = self.cell_in(&pane, px) {
                    if let Some((_, sel)) = self.selection.as_mut() {
                        if sel.end != cell {
                            sel.end = cell;
                            self.frame_wanted();
                        }
                    }
                }
            }
            Some(Drag::Divider { path, dir }) => {
                let (path, dir) = (path.clone(), *dir);
                let (cw, ch, s) = self.metrics();
                let top = PAD_Y + self.tab_rows() as f32 * ch;
                let pos = match dir {
                    SplitDir::Horizontal => ((px.x as f32 / s - PAD_X) / cw).round(),
                    SplitDir::Vertical => ((px.y as f32 / s - top) / ch).round(),
                }
                .max(1.0) as u16;
                let (cols, rows) = self.pane_area();
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if tab.layout.resize_divider(&path, pos, cols, rows) {
                        self.apply_sizes();
                        self.frame_wanted();
                    }
                }
            }
            Some(Drag::Report { pane, button }) => {
                let (pane, button) = (pane.clone(), *button);
                if let Some(cell) = self.cell_in(&pane, px) {
                    self.mouse_report(&pane, button, cell, true, true);
                }
            }
            None => {
                let cursor = if self.hit_divider(px).is_some() {
                    winit::window::CursorIcon::ColResize
                } else {
                    winit::window::CursorIcon::Text
                };
                if let Some(w) = &self.window {
                    w.set_cursor(cursor);
                }
            }
        }
    }

    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        let (_, ch, s) = self.metrics();
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => (y * 3.0).round() as i32,
            MouseScrollDelta::PixelDelta(p) => (p.y as f32 / s / ch).round() as i32,
        };
        if lines == 0 {
            return;
        }
        let Some((id, col, row)) = self.hit_pane(self.cursor_px) else { return };
        let Some(p) = self.panes.get(&id) else { return };
        if p.mouse_enabled {
            let button = if lines > 0 { 64 } else { 65 };
            for _ in 0..lines.unsigned_abs().min(10) {
                self.mouse_report(&id, button, (col, row), true, false);
            }
        } else if p.alt_screen {
            let arrow: &[u8] = match (lines > 0, p.app_cursor) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            for _ in 0..lines.unsigned_abs().min(10) {
                let _ = p.session.send_bytes(arrow);
            }
        } else {
            p.session.scroll(lines);
        }
    }

    fn on_ctl(&mut self, ctl: Ctl) {
        match ctl {
            Ctl::Split { from, right, focus, reply } => {
                let _ = reply.send(self.split(from, right, focus));
            }
            Ctl::NewTab { focus, reply } => {
                let before = self.active;
                let r = self.new_tab();
                if !focus {
                    self.active = before;
                    self.sync_shared();
                }
                let _ = reply.send(r);
            }
            Ctl::Focus(id) => {
                if let Some(ti) = self.tabs.iter().position(|t| t.layout.leaves().contains(&id.as_str())) {
                    self.active = ti;
                    self.tabs[ti].focus = id;
                    self.sync_shared();
                }
            }
            Ctl::Rename(id, name) => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.name = name;
                }
                self.sync_shared();
            }
        }
        self.frame_wanted();
    }

    fn run_bench(&mut self, el: &ActiveEventLoop) {
        let Some(steps) = self.bench.as_mut().map(Bench::due) else { return };
        for step in steps {
            match step {
                Step::Send(b) => self.send_focused(&b),
                Step::FloodStart => {
                    if let Some(t) = self.trace.as_mut() {
                        t.flood();
                    }
                }
                Step::FloodEnd => {
                    if let Some(t) = self.trace.as_mut() {
                        t.flood();
                    }
                }
                Step::Key => {
                    #[cfg(target_os = "macos")]
                    if let Some(w) = &self.window {
                        crate::mac::inject_key(w, "a", 0);
                    }
                }
                Step::Capture(path) => {
                    if let Some(g) = self.grid.as_mut() {
                        g.capture_next = Some(path);
                    }
                    self.render();
                }
                Step::Quit => self.quit(el),
            }
        }
    }

    fn quit(&mut self, el: &ActiveEventLoop) {
        if let Some(t) = self.trace.as_mut() {
            std::thread::sleep(Duration::from_millis(120));
            t.flush();
        }
        for p in self.panes.values() {
            p.session.terminate_local();
        }
        el.exit();
    }
}

fn rgb(c: [u8; 4]) -> Color {
    Color::Rgb(c[0], c[1], c[2])
}

fn truncate(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.pop();
        out.push('…');
    }
    out
}

fn time_secs() -> f32 {
    use std::sync::OnceLock;
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_secs_f32()
}

struct ClipboardSink;

impl kasa_pty::ClipboardSink for ClipboardSink {
    fn load(&self) -> String {
        arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default()
    }
    fn store(&self, text: &str) {
        if let Ok(mut c) = arboard::Clipboard::new() {
            let _ = c.set_text(text.to_string());
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn new_events(&mut self, _el: &ActiveEventLoop, _cause: StartCause) {}

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let (w, h) = self.settings.window.unwrap_or((1100.0, 700.0));
        let attrs = Window::default_attributes()
            .with_title("KasaLite")
            .with_inner_size(LogicalSize::new(w, h))
            .with_active(std::env::var_os("KASALITE_NO_FOCUS").is_none() && std::env::var_os("KASATERM_NO_FOCUS").is_none());
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[lite] 창을 못 만들었다: {e}");
                el.exit();
                return;
            }
        };
        let fonts = GridFonts { primary: self.settings.font_path.clone(), bundled: vec![SYMBOLS_NERD_FONT_MONO] };
        let grid = match GridRenderer::new(window.clone(), self.font_size, fonts) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[lite] 그리기를 못 세웠다: {e:#}");
                el.exit();
                return;
            }
        };
        window.set_ime_allowed(cfg!(not(target_os = "macos")));
        kasa_pty::set_host_policy(kasa_pty::HostPolicy {
            term_program: Some(("kasaterm".into(), env!("CARGO_PKG_VERSION").into())),
            last_login_dir: None,
            clipboard: Some(Arc::new(ClipboardSink)),
        });
        self.grid = Some(grid);
        self.window = Some(window.clone());
        #[cfg(target_os = "macos")]
        {
            self.link = crate::mac::Link::new(&window, self.proxy.clone());
        }
        control::start(self.shared.clone(), self.proxy.clone());
        self.trace = Trace::from_env();
        self.bench = Bench::from_env();
        #[cfg(target_os = "macos")]
        if std::env::var_os("KASALITE_BENCH").is_some() {
            crate::mac::float_for_bench(&window);
        }
        if let Err(e) = self.new_tab() {
            eprintln!("[lite] 셸을 못 띄웠다: {e:#}");
            el.exit();
            return;
        }
        self.render();
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::Screen(id, u) => {
                if u.eof {
                    self.pane_exited(&id, el);
                    return;
                }
                let focused = self.focused_id().as_deref() == Some(id.as_str());
                let visible = self.tabs.get(self.active).is_some_and(|t| t.layout.leaves().contains(&id.as_str()));
                let title = u.title.is_some();
                if let Some(p) = self.panes.get_mut(&id) {
                    p.apply(u);
                }
                if title {
                    self.sync_shared();
                }
                // 친 키의 메아리는 박자를 안 기다리고 바로 그린다 — 붙잡는 중에도. 박자에 싣는 것은 남이
                // 흘리는 출력뿐이다(spikes/frame-pacing: 미루면 키→화면이 한 박자 늘었다).
                if focused && !self.key_waiting.is_empty() {
                    self.key_echoed = true;
                    self.render();
                    self.hold_until = Some(Instant::now() + HOLD);
                } else if visible || (title && self.tabs.len() > 1) {
                    self.frame_wanted();
                }
            }
            UserEvent::Ctl(c) => self.on_ctl(c),
            UserEvent::Tick => self.on_tick(),
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        if self.grid.is_none() {
            return;
        }
        match ev {
            WindowEvent::CloseRequested => self.quit(el),
            WindowEvent::Resized(size) => {
                if let Some(g) = self.grid.as_mut() {
                    g.resize(size.width.max(1), size.height.max(1));
                }
                self.apply_sizes();
                self.render();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(g) = self.grid.as_mut() {
                    g.set_scale(scale_factor as f32);
                    g.set_font_size(self.font_size);
                }
                self.apply_sizes();
            }
            WindowEvent::Focused(f) => {
                self.focused = f;
                if !f {
                    self.flush_hangul();
                }
                self.frame_wanted();
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } => self.on_key(event, el),
            WindowEvent::Ime(ime) => self.on_ime(ime),
            WindowEvent::CursorMoved { position, .. } => self.on_cursor_moved(position),
            WindowEvent::MouseInput { state, button, .. } => self.on_mouse(state, button),
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::RedrawRequested => self.render(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.run_bench(el);
        let mut wake: Option<Instant> = self.bench.as_ref().and_then(Bench::next_at);
        #[cfg(target_os = "macos")]
        let linked = self.link.is_some();
        #[cfg(not(target_os = "macos"))]
        let linked = false;
        if self.dirty && !linked {
            let due = self.last_render + FALLBACK_FRAME;
            if Instant::now() >= due {
                self.render();
            } else {
                wake = Some(wake.map_or(due, |w| w.min(due)));
            }
        }
        el.set_control_flow(match wake {
            Some(w) => ControlFlow::WaitUntil(w),
            None => ControlFlow::Wait,
        });
    }
}
