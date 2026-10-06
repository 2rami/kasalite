//! 누르는 크롬 — 칸 머리 줄(번호·이름·나누기·확대·닫기)과 맨 아래 상태 줄(탭·칸 고르기 칩).
//!
//! 단추는 키캡이다: 얼굴 오른쪽 아래에 딱딱한 그림자가 붙고, 누르면 얼굴이 그림자 자리로 밀려
//! 그림자가 사라진다 — 카사텀 pixel 모양의 「누르면 줄어드는 어긋남」. 바깥 터미널이 kitty 그림을 받으면
//! 키캡 전체를 칸 픽셀 크기와 똑같은 PNG 로 정수배 도트(안티에일리어싱 없음)로 그린다. 못 받으면
//! 반블록 끝(`▐` `▌`)으로 반 칸 여백과 그림자를 낸다.
//!
//! 초점 칸 머리 줄은 강조색 바탕에 클래식 맥 활성 창처럼 가는 줄무늬를 깐다(kitty 면 한 칸 타일 그림을
//! 되풀이, 아니면 섹스턴트 도트 점선). 낮은 대비로 칠한 반블록·섹스턴트는 카사텀이 앞색 최소 대비(기본 2.5)로
//! 밝혀 버려서, 글자로 칠하는 도트는 대비가 넉넉한 곳에만 쓰고 키캡은 그림으로 그린다.
//!
//! 색은 kasaterm `docs/design.md` 의 어두운 팔레트·파랑 강조색 값이다.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthChar;

use crate::graphics::Graphics;
use crate::proto::{LayoutMsg, PaneRect};
use crate::render::Grids;

type Rgb = [u8; 3];

const SURFACE: Rgb = [26, 29, 35];
const SURFACE_HOVER: Rgb = [48, 56, 67];
const SURFACE_ACTIVE: Rgb = [60, 70, 84];
const BORDER: Rgb = [80, 92, 110];
const TEXT: Rgb = [236, 238, 243];
const TEXT_DIM: Rgb = [160, 166, 176];
const TEXT_MUTE: Rgb = [120, 126, 138];
const ACCENT: Rgb = [90, 140, 230];
const ON_ACCENT: Rgb = [255, 255, 255];
const DANGER: Rgb = [224, 88, 78];
const ATTENTION: Rgb = [250, 140, 42];
const BLACK: Rgb = [0, 0, 0];
const WHITE: Rgb = [255, 255, 255];

pub const DIVIDER: Rgb = BORDER;
pub const DIVIDER_FOCUS: Rgb = ACCENT;

fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

/// 초점 칸 머리 줄 바탕 — 강조색을 섞어 어느 칸이 키를 받는지 한눈에 보이게 한다.
fn focus_head() -> Rgb {
    lerp(SURFACE, ACCENT, 0.34)
}

/// 바깥 터미널이 24비트 색을 받는지. 모르면 256색으로 줄여 낸다(macOS Terminal.app 등).
fn truecolor() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let env = |k: &str| std::env::var(k).unwrap_or_default();
        let colorterm = env("COLORTERM").to_ascii_lowercase();
        let term = env("TERM");
        colorterm == "truecolor"
            || colorterm == "24bit"
            || matches!(env("TERM_PROGRAM").as_str(), "ghostty" | "WezTerm" | "kasaterm" | "iTerm.app" | "vscode")
            || term.contains("kitty")
            || term.contains("ghostty")
            || term.ends_with("-direct")
            || std::env::var_os("WT_SESSION").is_some()
    })
}

pub fn color(c: Rgb) -> Color {
    if truecolor() {
        Color::Rgb(c[0], c[1], c[2])
    } else {
        Color::Indexed(nearest_256(c))
    }
}

fn nearest_256(c: Rgb) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let near = |v: u8| LEVELS.iter().enumerate().min_by_key(|(_, l)| (**l as i32 - v as i32).abs()).map(|(i, _)| i).unwrap_or(0);
    let (r, g, b) = (near(c[0]), near(c[1]), near(c[2]));
    let cube = [LEVELS[r], LEVELS[g], LEVELS[b]];
    let avg = (c[0] as u32 + c[1] as u32 + c[2] as u32) / 3;
    let gi = ((avg.saturating_sub(8)) / 10).min(23) as u8;
    let gv = 8 + gi * 10;
    let dist = |a: Rgb| -> i32 { (0..3).map(|i| (a[i] as i32 - c[i] as i32).pow(2)).sum() };
    if dist([gv, gv, gv]) < dist(cube) {
        232 + gi
    } else {
        16 + 36 * r as u8 + 6 * g as u8 + b as u8
    }
}

/// 섹스턴트 한 글자. `dots` 는 칸을 2열×3행으로 나눈 자리 번호(1 2 / 3 4 / 5 6).
pub fn sextant(dots: &[u8]) -> char {
    let bits = dots.iter().fold(0u32, |acc, d| acc | 1 << (d - 1));
    match bits {
        0 => ' ',
        0b11_1111 => '█',
        0b01_0101 => '▌',
        0b10_1010 => '▐',
        // U+1FB00 부터 빈 칸·꽉 찬 칸·왼쪽 반·오른쪽 반(이미 블록 요소에 있는 넷)을 빼고 차례로 놓였다.
        n => char::from_u32(0x1FB00 + n - 1 - u32::from(n > 0b01_0101) - u32::from(n > 0b10_1010)).unwrap_or('█'),
    }
}

/// 도트로 칠할 글자 묶음.
#[derive(Debug, Clone, Copy)]
pub struct Glyphs {
    pub sextants: bool,
}

impl Glyphs {
    /// 설정이 없으면 바깥 터미널을 본다. 섹스턴트를 직접 그리거나(kitty·Ghostty·WezTerm·Windows Terminal·foot)
    /// 그 글자가 든 글꼴을 싣는(카사텀 Cascadia NF) 터미널만 고른다 — 나머지는 글꼴 대체에 맡기면 네모가 뜬다.
    pub fn detect(forced: Option<bool>) -> Self {
        if let Some(s) = forced {
            return Self { sextants: s };
        }
        let env = |k: &str| std::env::var(k).unwrap_or_default();
        let term = env("TERM");
        let sextants = matches!(env("TERM_PROGRAM").as_str(), "ghostty" | "WezTerm" | "kasaterm")
            || std::env::var_os("KITTY_WINDOW_ID").is_some()
            || std::env::var_os("WT_SESSION").is_some()
            || term.contains("kitty")
            || term.contains("ghostty")
            || term.starts_with("foot");
        Self { sextants }
    }

    /// 초점 머리 줄 줄무늬의 글자 대체 — 칸마다 가운데 왼쪽 도트 하나라 네모 점이 고르게 늘어선다.
    fn stripe(self) -> char {
        if self.sextants { sextant(&[3]) } else { '·' }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    SplitRight,
    SplitDown,
    Zoom,
    Close,
}

impl Action {
    /// 글자 대체(kitty 그림이 없을 때). 모양으로 뜻을 말한다 — 세로선 든 네모 = 옆으로, 가로선 = 아래로,
    /// 빈 네모 = 크게(창 최대화와 같은 약속), × = 닫기.
    fn glyph(self, toggled: bool) -> char {
        match self {
            Action::SplitRight => '◫',
            Action::SplitDown => '⊟',
            Action::Zoom if toggled => '▣',
            Action::Zoom => '□',
            Action::Close => '×',
        }
    }
}

/// 키캡 얼굴에 올릴 그림.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Sprite {
    /// 칸 단추. 참이면 켜진 상태(확대 중).
    Act(Action, bool),
    Number(usize),
    Plus,
    Help,
    /// 줄무늬 한 칸 타일(키캡이 아니다).
    Stripes,
}

impl Sprite {
    fn label(&self) -> String {
        match self {
            Sprite::Act(a, toggled) => a.glyph(*toggled).to_string(),
            Sprite::Number(n) => n.to_string(),
            Sprite::Plus => "+".into(),
            Sprite::Help => "?".into(),
            Sprite::Stripes => String::new(),
        }
    }

    /// 7줄 도트 그림(`#` 칠함). 줄 길이는 모두 같다.
    fn bitmap(&self) -> Vec<String> {
        let rows = |r: [&str; 7]| r.iter().map(|s| s.to_string()).collect();
        match self {
            Sprite::Act(Action::SplitRight, _) => rows(["#######", "#..#..#", "#..#..#", "#..#..#", "#..#..#", "#..#..#", "#######"]),
            Sprite::Act(Action::SplitDown, _) => rows(["#######", "#.....#", "#.....#", "#######", "#.....#", "#.....#", "#######"]),
            Sprite::Act(Action::Zoom, false) => rows(["###.###", "#.....#", "#.....#", ".......", "#.....#", "#.....#", "###.###"]),
            Sprite::Act(Action::Zoom, true) => rows(["..#.#..", "..#.#..", "###.###", ".......", "###.###", "..#.#..", "..#.#.."]),
            Sprite::Act(Action::Close, _) => rows(["#.....#", ".#...#.", "..#.#..", "...#...", "..#.#..", ".#...#.", "#.....#"]),
            Sprite::Plus => rows(["...#...", "...#...", "...#...", "#######", "...#...", "...#...", "...#..."]),
            Sprite::Help => rows([".###.", "#...#", "....#", "..##.", "..#..", ".....", "..#.."]),
            Sprite::Stripes => Vec::new(),
            Sprite::Number(n) => {
                let digits: Vec<[&str; 7]> = n.to_string().bytes().map(|b| DIGITS[(b - b'0') as usize]).collect();
                (0..7).map(|r| digits.iter().map(|d| d[r]).collect::<Vec<_>>().join(".")).collect()
            }
        }
    }
}

/// 5×7 도트 숫자.
const DIGITS: [[&str; 7]; 10] = [
    [".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###."],
    ["..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###."],
    [".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####"],
    ["####.", "....#", "....#", ".###.", "....#", "....#", "####."],
    ["...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#."],
    ["#####", "#....", "####.", "....#", "....#", "#...#", ".###."],
    ["..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###."],
    ["#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#..."],
    [".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###."],
    [".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##.."],
];

/// 누를 수 있는 것.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    Tab(usize),
    NewTab,
    Help,
    /// 칸 머리 줄의 단추 아닌 자리 — 누르면 그 칸으로 초점.
    Head(String),
    /// 상태 줄의 칸 고르기 칩.
    Chip(String),
    /// 칸 단추. 칸이 `None` 이면 초점 칸(단추 줄을 끈 상태 줄 단추).
    Act(Option<String>, Action),
}

impl Hit {
    /// 누른 채로 있다가 떼야 일하는 것. 머리 줄은 누르는 즉시 초점을 옮긴다.
    pub fn is_button(&self) -> bool {
        !matches!(self, Hit::Head(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitBox {
    pub hit: Hit,
    pub y: u16,
    pub x0: u16,
    pub x1: u16,
}

/// 뒤에 그린 것이 위에 있다 — 머리 줄 위의 단추가 머리 줄보다 먼저 잡힌다.
pub fn hit_at(hits: &[HitBox], x: u16, y: u16) -> Option<&Hit> {
    hits.iter().rev().find(|h| h.y == y && x >= h.x0 && x < h.x1).map(|h| &h.hit)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Look {
    Normal,
    Hover,
    Pressed,
}

/// 마우스가 지금 어디 있고 무엇을 누르고 있는지.
#[derive(Debug, Clone, Copy, Default)]
pub struct Pointer<'a> {
    pub hover: Option<&'a Hit>,
    pub pressed: Option<&'a Hit>,
}

impl Pointer<'_> {
    fn look(&self, hit: &Hit) -> Look {
        let over = self.hover == Some(hit);
        match self.pressed {
            Some(p) if p == hit && over => Look::Pressed,
            // 누른 채 밖으로 끌면 뗄 때 일하지 않는다 — 평소 모습으로 그걸 알린다.
            Some(_) => Look::Normal,
            None if over => Look::Hover,
            None => Look::Normal,
        }
    }
}

/// 키캡 한 벌의 색. `base` 는 키캡이 앉은 바탕이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Paint {
    pub base: Rgb,
    pub face: Rgb,
    pub ink: Rgb,
}

impl Paint {
    fn shadow(&self) -> Rgb {
        lerp(self.base, BLACK, 0.55)
    }
}

/// 키캡 색. `lit` 은 켜진 단추(확대 중·초점 칸 칩), `danger` 는 올리면 위험색이 되는 닫기.
fn keycap_paint(base: Rgb, look: Look, lit: bool, danger: bool) -> Paint {
    let (mut face, mut ink) = if lit { (ACCENT, ON_ACCENT) } else { (lerp(base, TEXT, 0.2), TEXT) };
    if !lit && base == SURFACE {
        face = SURFACE_ACTIVE;
        ink = TEXT_DIM;
    }
    if look != Look::Normal {
        if danger {
            (face, ink) = (DANGER, ON_ACCENT);
        } else {
            (face, ink) = (lerp(face, TEXT, 0.18), if lit { ON_ACCENT } else { TEXT });
        }
    }
    // 눌린 얼굴은 그림자 자리로 내려앉으며 한 톤 어두워진다.
    if look == Look::Pressed {
        face = lerp(face, BLACK, 0.22);
    }
    Paint { base, face, ink }
}

fn style(fg: Rgb, bg: Rgb) -> Style {
    Style::default().fg(color(fg)).bg(color(bg))
}

fn put(buf: &mut Buffer, x: u16, y: u16, ch: char, st: Style) {
    if let Some(c) = buf.cell_mut((x, y)) {
        c.reset();
        c.set_char(ch);
        c.set_style(st);
    }
}

fn fill(buf: &mut Buffer, x: u16, y: u16, w: u16, bg: Rgb) {
    for i in 0..w {
        put(buf, x + i, y, ' ', style(TEXT, bg));
    }
}

/// 글을 `max` 칸까지 쓰고 다음 x 를 준다.
fn text(buf: &mut Buffer, x: u16, y: u16, s: &str, max: u16, st: Style) -> u16 {
    let (nx, _) = buf.set_stringn(x, y, s, max as usize, st);
    nx
}

pub fn text_width(s: &str) -> u16 {
    s.chars().map(|c| UnicodeWidthChar::width(c).unwrap_or(1) as u16).sum()
}

pub fn truncate(s: &str, max: usize) -> String {
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

/// 글자로 그리는 키캡: 반블록 끝 + 글 + 끝. `raised` 면 오른쪽 끝 반 칸이 그림자다. 차지한 폭을 준다.
fn text_keycap(buf: &mut Buffer, x: u16, y: u16, label: &str, p: Paint, look: Look, raised: bool) -> u16 {
    let pressed = raised && look == Look::Pressed;
    let left = if pressed { ' ' } else { '▐' };
    put(buf, x, y, left, style(p.face, p.base));
    let end = text(buf, x + 1, y, label, text_width(label), style(p.ink, p.face).add_modifier(Modifier::BOLD));
    // 눌림의 오른쪽 끝은 앞색 없이 바탕만 칠한다 — 앞색=바탕색 `█` 는 대비 보정 터미널이 밝혀 버린다.
    let (right, right_bg) = match (raised, pressed) {
        (true, true) => (' ', p.face),
        (true, false) => ('▌', p.shadow()),
        (false, _) => ('▌', p.base),
    };
    put(buf, end, y, right, style(p.face, right_bg));
    end + 1 - x
}

/// 키캡을 kitty 도트 그림으로, 못 하면 글자로 그린다.
pub struct Painter<'a> {
    pub glyphs: Glyphs,
    /// kitty 그림을 받는 바깥 터미널이면 그 창구와 글자 칸 픽셀 크기.
    pub icons: Option<(&'a mut Graphics, (u16, u16))>,
}

impl Painter<'_> {
    /// 키캡이 차지할 칸 수.
    fn width(&self, sprite: &Sprite, raised: bool) -> u16 {
        self.icons
            .as_ref()
            .and_then(|(_, px)| sprite_cells(sprite, raised, *px))
            .unwrap_or_else(|| text_width(&sprite.label()) + 2)
    }

    #[allow(clippy::too_many_arguments)]
    fn keycap(&mut self, buf: &mut Buffer, x: u16, y: u16, sprite: Sprite, paint: Paint, look: Look, raised: bool) -> u16 {
        if let Some((gfx, px)) = self.icons.as_mut() {
            if let Some(cells) = sprite_cells(&sprite, raised, *px) {
                let key = IconKey { sprite: sprite.clone(), paint, look, raised, cells, cell_px: *px };
                if let Some(id) = gfx.chrome_icon(&key) {
                    gfx.draw_chrome_icon(buf, x, y, cells, id, paint.base);
                    return cells;
                }
            }
        }
        text_keycap(buf, x, y, &sprite.label(), paint, look, raised)
    }

    /// `from..to` 칸에 초점 줄무늬를 깐다.
    fn stripes(&mut self, buf: &mut Buffer, from: u16, to: u16, y: u16, base: Rgb) {
        if let Some((gfx, px)) = self.icons.as_mut() {
            let paint = Paint { base, face: lerp(base, ACCENT, 0.55), ink: base };
            let key = IconKey { sprite: Sprite::Stripes, paint, look: Look::Normal, raised: false, cells: 1, cell_px: *px };
            if let Some(id) = gfx.chrome_icon(&key) {
                gfx.draw_chrome_tile(buf, from, y, to - from, id, base);
                return;
            }
        }
        for sx in from..to {
            put(buf, sx, y, self.glyphs.stripe(), style(ACCENT, base));
        }
    }
}

/// 칸 단추 한 칸 몫의 폭.
pub const BUTTON_W: u16 = 3;
const ALL_ACTIONS: [Action; 4] = [Action::SplitRight, Action::SplitDown, Action::Zoom, Action::Close];

/// 머리 줄 폭에 맞춰 남길 단추. 좁으면 나누기부터 걷고, 닫기는 끝까지 남긴다.
fn head_actions(w: u16, badge: u16) -> &'static [Action] {
    match w {
        w if w >= badge + 1 + 4 + BUTTON_W * 4 => &ALL_ACTIONS,
        w if w >= badge + 1 + 2 + BUTTON_W * 2 => &ALL_ACTIONS[2..],
        w if w >= badge + BUTTON_W => &ALL_ACTIONS[3..],
        _ => &[],
    }
}

pub struct Heads<'a> {
    pub layout: &'a LayoutMsg,
    pub grids: &'a Grids,
    pub pointer: Pointer<'a>,
}

/// 칸마다 머리 줄을 그리고 누를 자리를 준다.
pub fn draw_heads(buf: &mut Buffer, area: Rect, h: &Heads, painter: &mut Painter) -> Vec<HitBox> {
    let mut hits = Vec::new();
    for p in &h.layout.panes {
        if !p.head || p.y == 0 {
            continue;
        }
        let y = area.y + p.y - 1;
        if y >= area.y + area.height {
            continue;
        }
        let number = h.layout.tab_panes.iter().position(|(id, _)| *id == p.id).unwrap_or(0) + 1;
        let x0 = area.x + p.x;
        let w = p.w.min(area.width.saturating_sub(p.x));
        draw_head(buf, x0, y, w, number, p, h, painter, &mut hits);
    }
    hits
}

#[allow(clippy::too_many_arguments)]
fn draw_head(buf: &mut Buffer, x0: u16, y: u16, w: u16, number: usize, p: &PaneRect, h: &Heads, painter: &mut Painter, hits: &mut Vec<HitBox>) {
    let focused = p.id == h.layout.focus;
    let head_hit = Hit::Head(p.id.clone());
    let base = match (focused, h.pointer.hover == Some(&head_hit)) {
        (true, _) => focus_head(),
        (false, true) => SURFACE_HOVER,
        (false, false) => SURFACE,
    };
    fill(buf, x0, y, w, base);
    hits.push(HitBox { hit: head_hit, y, x0, x1: x0 + w });

    // 번호 배지 — 초점 칸은 강조색으로 채워 어느 칸이 키를 받는지 한눈에.
    let badge = Sprite::Number(number);
    let badge_w = painter.width(&badge, false);
    let mut x = x0;
    if w >= badge_w {
        let paint = if focused { Paint { base, face: ACCENT, ink: ON_ACCENT } } else { Paint { base, face: SURFACE_ACTIVE, ink: TEXT_DIM } };
        x += painter.keycap(buf, x, y, badge, paint, Look::Normal, false);
    }

    let actions = head_actions(w, badge_w);
    let margin = u16::from(w >= 40);
    let bx0 = (x0 + w).saturating_sub(BUTTON_W * actions.len() as u16 + margin);

    // 이름, 남으면 칸 제목을 흐리게.
    let room = bx0.saturating_sub(x + 1);
    if room >= 2 {
        x += 1;
        let name_style = if focused { style(TEXT, base).add_modifier(Modifier::BOLD) } else { style(TEXT_MUTE, base) };
        x = text(buf, x, y, &truncate(&p.name, room as usize), room, name_style);
        let title = h.grids.get(&p.id).and_then(|g| g.title.as_deref()).map(str::trim).filter(|t| !t.is_empty() && *t != p.name);
        if let Some(t) = title {
            let left = bx0.saturating_sub(x + 4);
            if left >= 6 {
                let dim = style(if focused { TEXT_DIM } else { TEXT_MUTE }, base);
                x = text(buf, x, y, " · ", 3, dim);
                x = text(buf, x, y, &truncate(t, left as usize - 1), left, dim);
            }
        }
    }
    if focused {
        let (from, to) = (x + 1, bx0.saturating_sub(1));
        if to >= from + 3 {
            painter.stripes(buf, from, to, y, base);
        }
    }

    let mut bx = bx0;
    for &a in actions {
        let hit = Hit::Act(Some(p.id.clone()), a);
        let look = h.pointer.look(&hit);
        let toggled = a == Action::Zoom && h.layout.zoomed;
        let paint = keycap_paint(base, look, toggled, a == Action::Close);
        let w = painter.keycap(buf, bx, y, Sprite::Act(a, toggled), paint, look, true);
        hits.push(HitBox { hit, y, x0: bx, x1: bx + w });
        bx += BUTTON_W;
    }
}

/// 도트 키캡 그림 하나를 가리키는 이름.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IconKey {
    pub sprite: Sprite,
    pub paint: Paint,
    pub look: Look,
    /// 그림자 있는(누르는) 키캡인지.
    pub raised: bool,
    pub cells: u16,
    pub cell_px: (u16, u16),
}

/// 키캡 얼굴 높이(도트): 테두리 둘 + 빛·그늘 줄 둘 + 그림 7.
const FACE_H: u32 = 11;
/// 그림 좌우 여백(도트).
const FACE_PAD: u32 = 2;
const SHADOW: u32 = 1;

/// 도트 한 변의 픽셀. 칸 단추(글자 3칸) 안에 키캡과 그림자가 드는 가장 큰 정수배.
fn dot_px((cw, ch): (u16, u16)) -> Option<u32> {
    let action_w = 7 + 2 * FACE_PAD + 2 + SHADOW;
    let d = (cw as u32 * BUTTON_W as u32 / action_w).min(ch as u32 / (FACE_H + SHADOW));
    (d > 0).then_some(d)
}

fn face_w(sprite: &Sprite) -> u32 {
    sprite.bitmap().first().map(|r| r.len() as u32).unwrap_or(0) + 2 * FACE_PAD + 2
}

/// kitty 그림으로 그릴 때 키캡이 차지할 칸 수. 칸 단추는 늘 `BUTTON_W` 다 — 누를 자리가 고르게.
fn sprite_cells(sprite: &Sprite, raised: bool, px: (u16, u16)) -> Option<u16> {
    let d = dot_px(px)?;
    if matches!(sprite, Sprite::Act(..)) {
        return Some(BUTTON_W);
    }
    let need = (face_w(sprite) + if raised { SHADOW } else { 0 }) * d;
    Some(need.div_ceil(px.0 as u32).max(1) as u16)
}

/// 키캡 칸(`cells` × 1줄)과 똑같은 픽셀 크기의 PNG. 칸이 너무 작아 도트가 안 서면 `None`.
pub fn keycap_png(k: &IconKey) -> Option<Vec<u8>> {
    let d = dot_px(k.cell_px)?;
    if k.sprite == Sprite::Stripes {
        return stripes_png(k, d);
    }
    let (wpx, hpx) = (k.cell_px.0 as u32 * k.cells as u32, k.cell_px.1 as u32);
    let (wd, hd) = (wpx / d, hpx / d);
    let fw = face_w(&k.sprite);
    let s = if k.raised { SHADOW } else { 0 };
    if wd < fw + s || hd < FACE_H + s {
        return None;
    }
    let (ox, oy) = ((wpx - wd * d) / 2, (hpx - hd * d) / 2);
    let mut img = image::RgbaImage::new(wpx, hpx);
    let mut dot = |x: u32, y: u32, c: Rgb| {
        for py in 0..d {
            for px in 0..d {
                img.put_pixel(ox + x * d + px, oy + y * d + py, image::Rgba([c[0], c[1], c[2], 255]));
            }
        }
    };
    let pressed = k.raised && k.look == Look::Pressed;
    let (mut fx, mut fy) = ((wd - fw - s) / 2, (hd - FACE_H - s) / 2);
    if pressed {
        fx += s;
        fy += s;
    }
    // 모서리 도트 하나를 깎은 네모.
    let inside = |x: u32, y: u32, x0: u32, y0: u32| {
        let (l, r, t, b) = (x == x0, x == x0 + fw - 1, y == y0, y == y0 + FACE_H - 1);
        !((l || r) && (t || b))
    };
    if k.raised && !pressed {
        let shadow = k.paint.shadow();
        for y in fy + s..fy + s + FACE_H {
            for x in fx + s..fx + s + fw {
                if inside(x, y, fx + s, fy + s) {
                    dot(x, y, shadow);
                }
            }
        }
    }
    let face = k.paint.face;
    let (outline, light, shade) = (lerp(face, BLACK, 0.5), lerp(face, WHITE, 0.16), lerp(face, BLACK, 0.16));
    for y in fy..fy + FACE_H {
        for x in fx..fx + fw {
            if !inside(x, y, fx, fy) {
                continue;
            }
            let edge = x == fx || x == fx + fw - 1 || y == fy || y == fy + FACE_H - 1;
            let c = match () {
                _ if edge => outline,
                _ if y == fy + 1 => light,
                _ if y == fy + FACE_H - 2 => shade,
                _ => face,
            };
            dot(x, y, c);
        }
    }
    let (ix, iy) = (fx + 1 + FACE_PAD, fy + 2);
    for (r, row) in k.sprite.bitmap().iter().enumerate() {
        for (c, b) in row.bytes().enumerate() {
            if b == b'#' {
                dot(ix + c as u32, iy + r as u32, k.paint.ink);
            }
        }
    }
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// 줄무늬 한 칸 타일: 키캡 얼굴 높이 안에 한 도트 줄을 한 도트 걸러 다섯 가닥. 가로는 칸 끝까지 칠해
/// 타일을 이어 붙여도 이음매가 없다.
fn stripes_png(k: &IconKey, d: u32) -> Option<Vec<u8>> {
    let (wpx, hpx) = (k.cell_px.0 as u32, k.cell_px.1 as u32);
    let hd = hpx / d;
    if hd < FACE_H {
        return None;
    }
    let top = (hpx - hd * d) / 2 + (hd - FACE_H) / 2 * d;
    let c = k.paint.face;
    let mut img = image::RgbaImage::new(wpx, hpx);
    for line in 0..5 {
        let y0 = top + (1 + line * 2) * d;
        for y in y0..(y0 + d).min(hpx) {
            for x in 0..wpx {
                img.put_pixel(x, y, image::Rgba([c[0], c[1], c[2], 255]));
            }
        }
    }
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// 상태 줄에 실을 것.
pub struct StatusLine<'a> {
    pub layout: &'a LayoutMsg,
    /// 단추 줄을 켰으면 오른쪽에 칸 고르기 칩, 껐으면 예전 칸 단추.
    pub buttons: bool,
    pub pointer: Pointer<'a>,
    /// 접두키 이름. 접두키를 끈 설정이면 `None`.
    pub prefix_label: Option<&'a str>,
    pub prefix_armed: bool,
    pub message: Option<&'a str>,
    pub scroll_mode: Option<u32>,
}

const FALLBACK_BUTTONS: [(Action, &str); 4] =
    [(Action::SplitRight, "옆으로"), (Action::SplitDown, "아래로"), (Action::Zoom, "확대"), (Action::Close, "닫기")];

/// 상태 줄 오른쪽에 놓을 것 하나.
enum Item {
    /// 키캡 하나(+ 뒤에 붙는 이름).
    Key { hit: Hit, sprite: Sprite, lit: bool, name: Option<String> },
    /// 글 단추(단추 줄을 껐을 때의 예전 칸 단추).
    Word { hit: Hit, label: &'static str },
}

pub fn draw_status(buf: &mut Buffer, area: Rect, s: &StatusLine, painter: &mut Painter) -> Vec<HitBox> {
    let y = area.y + area.height.saturating_sub(1);
    let base = SURFACE;
    fill(buf, area.x, y, area.width, base);
    let right_edge = area.x + area.width;
    let mut hits = Vec::new();
    let mut x = area.x;

    let session = format!(" {} ", s.layout.session);
    x = text(buf, x, y, &session, right_edge - x, style(ON_ACCENT, ACCENT).add_modifier(Modifier::BOLD));
    x = text(buf, x, y, " ", right_edge.saturating_sub(x), style(TEXT, base));
    for (i, name) in s.layout.tabs.iter().enumerate() {
        let hit = Hit::Tab(i);
        let label = format!(" {i} {} ", truncate(name, 18));
        let st = if i == s.layout.active_tab {
            style(TEXT, SURFACE_ACTIVE).add_modifier(Modifier::BOLD)
        } else if s.pointer.look(&hit) != Look::Normal {
            style(TEXT, SURFACE_HOVER)
        } else {
            style(TEXT_DIM, base)
        };
        let start = x;
        x = text(buf, x, y, &label, right_edge.saturating_sub(x), st);
        hits.push(HitBox { hit, y, x0: start, x1: x });
    }
    let plus_w = painter.width(&Sprite::Plus, true);
    if x + 1 + plus_w < right_edge {
        let hit = Hit::NewTab;
        let look = s.pointer.look(&hit);
        let w = painter.keycap(buf, x + 1, y, Sprite::Plus, keycap_paint(base, look, false, false), look, true);
        hits.push(HitBox { hit, y, x0: x + 1, x1: x + 1 + w });
        x += 1 + w;
    }
    if s.layout.zoomed {
        x = text(buf, x, y, " 확대 중", right_edge.saturating_sub(x), style(ATTENTION, base));
    }

    // 오른쪽에서부터: ? 단추, 그 왼쪽에 칸 칩(또는 예전 칸 단추), 남은 자리에 알림 글.
    let mut items = vec![Item::Key { hit: Hit::Help, sprite: Sprite::Help, lit: false, name: None }];
    let help_w = painter.width(&Sprite::Help, true) + 1;
    let room = right_edge.saturating_sub(x + help_w + 2);
    let picks = if s.buttons {
        chips(s.layout, room, painter)
    } else {
        FALLBACK_BUTTONS.iter().map(|(a, l)| Item::Word { hit: Hit::Act(None, *a), label: l }).collect()
    };
    let item_w = |it: &Item, p: &Painter| match it {
        Item::Key { sprite, name, .. } => p.width(sprite, true) + name.as_ref().map(|n| text_width(n) + 1).unwrap_or(0) + 1,
        Item::Word { label, .. } => text_width(label) + 3,
    };
    let picks_w: u16 = picks.iter().map(|it| item_w(it, painter)).sum();
    if !picks.is_empty() && x + picks_w + help_w < right_edge {
        items.splice(0..0, picks);
    }
    let total: u16 = items.iter().map(|it| item_w(it, painter)).sum();
    let start = right_edge.saturating_sub(total);
    let mut right = start;
    if start > x {
        let mut cx = start;
        for it in items {
            let w = item_w(&it, painter);
            match it {
                Item::Key { hit, sprite, lit, name } => {
                    let look = s.pointer.look(&hit);
                    let kw = painter.keycap(buf, cx, y, sprite, keycap_paint(base, look, lit, false), look, true);
                    if let Some(n) = name {
                        let st = if lit { style(TEXT, base).add_modifier(Modifier::BOLD) } else { style(TEXT_DIM, base) };
                        text(buf, cx + kw, y, &n, text_width(&n), st);
                    }
                    hits.push(HitBox { hit, y, x0: cx, x1: cx + w - 1 });
                }
                Item::Word { hit, label } => {
                    let look = s.pointer.look(&hit);
                    let danger = matches!(hit, Hit::Act(_, Action::Close));
                    let kw = text_keycap(buf, cx, y, label, keycap_paint(base, look, false, danger), look, true);
                    hits.push(HitBox { hit, y, x0: cx, x1: cx + kw });
                }
            }
            cx += w;
        }
    } else {
        right = right_edge;
    }

    let note = match (s.scroll_mode, s.message, s.prefix_armed, s.prefix_label) {
        (Some(n), ..) => Some(format!(" 스크롤 ↑{n} · q 끝 ")),
        (None, Some(m), ..) => Some(format!(" {m} ")),
        (None, None, true, Some(p)) => Some(format!(" [{p}] 명령 대기 ")),
        (None, None, false, Some(p)) => Some(format!("{p} 접두키 ")),
        (None, None, _, None) => None,
    };
    if let Some(note) = note {
        let nw = text_width(&note);
        let loud = s.prefix_armed || s.scroll_mode.is_some() || s.message.is_some();
        // 알림(닫을까요?·복사)은 칩보다 앞선다 — 칩 자리를 덮어서라도 보인다.
        let limit = if loud && right < x + nw + 1 { right_edge.saturating_sub(help_w) } else { right.saturating_sub(1) };
        if limit > x + nw {
            let st = if s.prefix_armed || s.scroll_mode.is_some() {
                style(BLACK, ATTENTION)
            } else if s.message.is_some() {
                style(TEXT, SURFACE_ACTIVE)
            } else {
                style(TEXT_MUTE, base)
            };
            let nx = limit - nw;
            hits.retain(|h| h.x1 <= nx || h.x0 >= limit);
            text(buf, nx, y, &note, nw, st);
        }
    }
    hits
}

/// 칸 고르기 칩: 번호 키캡 + 이름. `room` 안에 이름까지 다 들어가면 이름을 붙이고, 아니면 번호만.
fn chips(layout: &LayoutMsg, room: u16, painter: &Painter) -> Vec<Item> {
    if layout.tab_panes.len() < 2 {
        return Vec::new();
    }
    let make = |named: bool| -> Vec<Item> {
        layout
            .tab_panes
            .iter()
            .enumerate()
            .map(|(i, (id, name))| Item::Key {
                hit: Hit::Chip(id.clone()),
                sprite: Sprite::Number(i + 1),
                lit: *id == layout.focus,
                name: named.then(|| truncate(name, 10)),
            })
            .collect()
    };
    let width = |v: &[Item]| -> u16 {
        v.iter()
            .map(|it| match it {
                Item::Key { sprite, name, .. } => painter.width(sprite, true) + name.as_ref().map(|n| text_width(n) + 1).unwrap_or(0) + 1,
                Item::Word { label, .. } => text_width(label) + 3,
            })
            .sum()
    };
    let named = make(true);
    if width(&named) <= room {
        return named;
    }
    let bare = make(false);
    if width(&bare) <= room {
        return bare;
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sextant_codes_skip_the_block_halves() {
        assert_eq!(sextant(&[1]), '\u{1FB00}');
        assert_eq!(sextant(&[2, 3, 5]), '\u{1FB14}');
        assert_eq!(sextant(&[1, 3, 5]), '▌');
        assert_eq!(sextant(&[2, 4, 6]), '▐');
        assert_eq!(sextant(&[1, 3, 4, 5, 6]), '\u{1FB3A}');
        assert_eq!(sextant(&[2, 3, 4, 5, 6]), '\u{1FB3B}');
        assert_eq!(sextant(&[1, 2, 5, 6]), '\u{1FB30}');
    }

    #[test]
    fn nearest_256_hits_cube_and_greys() {
        assert_eq!(nearest_256([0, 0, 0]), 16);
        assert_eq!(nearest_256([255, 255, 255]), 231);
        assert_eq!(nearest_256([28, 28, 28]), 234);
    }

    #[test]
    fn press_shifts_the_face_into_its_shadow() {
        let area = Rect::new(0, 0, 6, 1);
        let p = keycap_paint(SURFACE, Look::Normal, false, false);
        let mut buf = Buffer::empty(area);
        text_keycap(&mut buf, 0, 0, "×", p, Look::Normal, true);
        assert_eq!(buf[(0, 0)].symbol(), "▐");
        assert_eq!(buf[(2, 0)].symbol(), "▌");
        assert_eq!(buf[(2, 0)].bg, color(p.shadow()));
        let p = keycap_paint(SURFACE, Look::Pressed, false, false);
        text_keycap(&mut buf, 0, 0, "×", p, Look::Pressed, true);
        assert_eq!(buf[(0, 0)].symbol(), " ");
        assert_eq!(buf[(2, 0)].symbol(), " ");
        assert_eq!(buf[(2, 0)].bg, color(p.face));
    }

    #[test]
    fn pressed_look_needs_the_pointer_still_over_it() {
        let a = Hit::Act(Some("%1".into()), Action::Close);
        let b = Hit::Head("%1".into());
        assert_eq!(Pointer { hover: Some(&a), pressed: Some(&a) }.look(&a), Look::Pressed);
        assert_eq!(Pointer { hover: Some(&b), pressed: Some(&a) }.look(&a), Look::Normal);
        assert_eq!(Pointer { hover: Some(&a), pressed: None }.look(&a), Look::Hover);
    }

    #[test]
    fn buttons_win_over_the_head_they_sit_on() {
        let hits = vec![
            HitBox { hit: Hit::Head("%1".into()), y: 0, x0: 0, x1: 40 },
            HitBox { hit: Hit::Act(Some("%1".into()), Action::Close), y: 0, x0: 37, x1: 40 },
        ];
        assert_eq!(hit_at(&hits, 38, 0), Some(&Hit::Act(Some("%1".into()), Action::Close)));
        assert_eq!(hit_at(&hits, 5, 0), Some(&Hit::Head("%1".into())));
        assert_eq!(hit_at(&hits, 5, 1), None);
    }

    #[test]
    fn keycap_png_fills_its_cells_with_whole_dots() {
        let paint = keycap_paint(focus_head(), Look::Normal, false, true);
        let k = IconKey { sprite: Sprite::Act(Action::Close, false), paint, look: Look::Normal, raised: true, cells: 3, cell_px: (17, 43) };
        let img = image::load_from_memory(&keycap_png(&k).unwrap()).unwrap().to_rgba8();
        assert_eq!((img.width(), img.height()), (51, 43));
        // 정수배 도트 — 알파는 0 아니면 255 뿐(안티에일리어싱 없음).
        assert!(img.pixels().all(|p| p.0[3] == 0 || p.0[3] == 255));
        assert_eq!(dot_px((17, 43)), Some(3));
        assert_eq!(dot_px((8, 17)), Some(1));
        assert!(keycap_png(&IconKey { cell_px: (4, 8), ..k }).is_none());
        // 두 자리 번호는 한 자리보다 넓다.
        let one = sprite_cells(&Sprite::Number(3), true, (17, 43)).unwrap();
        let two = sprite_cells(&Sprite::Number(12), true, (17, 43)).unwrap();
        assert!(two > one);
    }

    #[test]
    fn digits_are_five_by_seven() {
        for d in DIGITS {
            assert!(d.iter().all(|r| r.len() == 5));
        }
        let rows = Sprite::Number(10).bitmap();
        assert_eq!(rows.len(), 7);
        assert!(rows.iter().all(|r| r.len() == 11));
    }
}
