//! 칸 그림을 바깥 터미널로 넘긴다.
//!
//! 바깥 터미널이 kitty 그림 프로토콜을 알면 그림을 한 번만 보내고(`a=t`, 전송만) 가상 놓기
//! (`a=p,U=1`)를 건 뒤, 칸 자리에 `U+10EEEE` 자리표시 글자를 그린다. 자리표시는 글자라서
//! 칸 잘림·스크롤·경계선을 글과 똑같이 따른다. 그림 번호는 글자색(24비트)에, 상자 안
//! 행·열은 결합 문자 둘에 싣는다. 모르면 `[그림 640×480]` 글자를 그리고, 누르면 OS 보기로 연다.
//!
//! tmux 가 같은 길을 가며 밟은 함정(2026-03, tmux PR #5274)을 피한다: `a=T` 는 유령 놓기를
//! 남겨 `a=t` 를 쓰고, 256색 그림 번호는 SGR 확장색 머리와 부딪혀 트루컬러로만 싣는다.

use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

use base64::Engine;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

use crate::chrome::{self, IconKey};
use crate::proto::{ImageView, PaneRect};

const PLACEHOLDER: char = '\u{10EEEE}';

/// kitty `rowcolumn-diacritics.txt` — 순번이 곧 행·열 번호다.
static DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A,
    0x034B, 0x034C, 0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365,
    0x0366, 0x0367, 0x0368, 0x0369, 0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F,
    0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592, 0x0593, 0x0594, 0x0595, 0x0597,
    0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1, 0x05A8, 0x05A9,
    0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6,
    0x06D7, 0x06D8, 0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2,
    0x06E4, 0x06E7, 0x06E8, 0x06EB, 0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736,
    0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743, 0x0745, 0x0747, 0x0749, 0x074A,
    0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3, 0x0816, 0x0817,
    0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951,
    0x0953, 0x0954, 0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD,
    0x193A, 0x1A17, 0x1A75, 0x1A76, 0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C,
    0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71, 0x1B72, 0x1B73, 0x1CD0, 0x1CD1,
    0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4, 0x1DC5, 0x1DC6,
    0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF,
    0x1DE0, 0x1DE1, 0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1,
    0x20D4, 0x20D5, 0x20D6, 0x20D7, 0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0,
    0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2, 0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6,
    0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE, 0x2DEF, 0x2DF0,
    0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1,
    0xA8E0, 0xA8E1, 0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9,
    0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED, 0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2,
    0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1, 0xFE20, 0xFE21, 0xFE22, 0xFE23,
    0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186, 0x1D187, 0x1D188, 0x1D189,
    0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// 바깥 터미널에서 내린 그림을 지우지 않고 붙들어 두는 장수. 스크롤로 오가는 그림을
/// 다시 보내지 않게 몇 장은 남긴다.
const KEEP_HIDDEN: usize = 16;
/// base64 조각 크기(kitty 권장 상한).
const CHUNK: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Kitty,
    Text,
}

/// 바깥 터미널에 물어 안 것.
#[derive(Debug, Clone, Copy, Default)]
pub struct Probe {
    pub kitty: bool,
    pub cell_px: Option<(u16, u16)>,
}

/// 칸 그림 하나를 가리키는 이름. 같은 칸·같은 그림 번호·같은 파일이면 같은 그림이다.
type Key = (String, u64, String);

struct Sent {
    id: u32,
    /// 지금 걸린 가상 놓기 상자(열, 행).
    placed: Option<(u16, u16)>,
    last_seen: u64,
}

pub struct Graphics {
    pub mode: Mode,
    sent: HashMap<Key, Sent>,
    next_id: u32,
    frame: u64,
    /// 파일별 원본 픽셀 크기(글자 자리표시용).
    dims: HashMap<String, Option<(u32, u32)>>,
    /// 크롬 단추 그림. 몇 벌 안 되니 떠날 때까지 붙들어 둔다. 못 그리는 크기면 `None`.
    icons: HashMap<IconKey, Option<u32>>,
}

impl Graphics {
    pub fn new(mode: Mode) -> Self {
        Self { mode, sent: HashMap::new(), next_id: 1, frame: 0, dims: HashMap::new(), icons: HashMap::new() }
    }

    /// 이번 프레임에 보일 그림을 바깥 터미널에 준비시키고(보내기·놓기·지우기) 그 바이트를 낸다.
    /// ratatui 가 자리표시를 그리기 전에 불러야 한다.
    pub fn prepare(&mut self, visible: &[(&str, &ImageView)], out: &mut impl Write) {
        if self.mode != Mode::Kitty {
            return;
        }
        self.frame += 1;
        let mut buf = Vec::new();
        for (pane, v) in visible {
            let key = (pane.to_string(), v.id, v.path.clone());
            if !self.sent.contains_key(&key) {
                let id = self.alloc_id();
                match transmit(id, &v.path) {
                    Some(bytes) => buf.extend_from_slice(&bytes),
                    None => continue,
                }
                self.sent.insert(key.clone(), Sent { id, placed: None, last_seen: 0 });
            }
            let s = self.sent.get_mut(&key).expect("방금 넣었다");
            s.last_seen = self.frame;
            if s.placed != Some((v.cols, v.rows)) {
                buf.extend_from_slice(
                    format!("\x1b_Ga=p,U=1,i={},p=1,c={},r={},q=2\x1b\\", s.id, v.cols, v.rows).as_bytes(),
                );
                s.placed = Some((v.cols, v.rows));
            }
        }
        let mut hidden: Vec<(Key, u64)> =
            self.sent.iter().filter(|(_, s)| s.last_seen != self.frame).map(|(k, s)| (k.clone(), s.last_seen)).collect();
        if hidden.len() > KEEP_HIDDEN {
            hidden.sort_by_key(|(_, seen)| *seen);
            for (k, _) in hidden.iter().take(hidden.len() - KEEP_HIDDEN) {
                if let Some(s) = self.sent.remove(k) {
                    buf.extend_from_slice(format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", s.id).as_bytes());
                }
            }
        }
        if !buf.is_empty() {
            let _ = out.write_all(&buf);
            let _ = out.flush();
        }
    }

    /// 떠날 때 보낸 그림을 바깥 터미널에서 지운다.
    pub fn clear_all(&mut self, out: &mut impl Write) {
        if self.mode != Mode::Kitty {
            return;
        }
        let mut buf = Vec::new();
        for id in self.sent.values().map(|s| s.id).chain(self.icons.values().flatten().copied()) {
            buf.extend_from_slice(format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").as_bytes());
        }
        self.sent.clear();
        self.icons.clear();
        let _ = out.write_all(&buf);
        let _ = out.flush();
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        // 24비트 안에서 돈다(글자색에 싣는다). 0 은 「그림 없음」이라 건너뛴다.
        self.next_id = if self.next_id >= 0x00FF_FFFF { 1 } else { self.next_id + 1 };
        id
    }

    /// 칸 그림을 버퍼에 그린다 — kitty 면 자리표시, 아니면 글자 표식.
    pub fn draw(&mut self, buf: &mut Buffer, rect: &PaneRect, pane: &str, images: &[ImageView]) {
        for v in images {
            let (clip_r0, clip_c0, clip_w, clip_h) = v.clip.unwrap_or((v.row, v.col, v.cols, v.rows));
            let visible = |r: i32, c: i32| {
                r >= 0
                    && c >= 0
                    && r < rect.h as i32
                    && c < rect.w as i32
                    && r >= clip_r0
                    && r < clip_r0 + clip_h as i32
                    && c >= clip_c0 as i32
                    && c < clip_c0 as i32 + clip_w as i32
            };
            match self.mode {
                Mode::Kitty => {
                    let key = (pane.to_string(), v.id, v.path.clone());
                    let Some(s) = self.sent.get(&key) else { continue };
                    let fg = Color::Rgb((s.id >> 16) as u8, (s.id >> 8) as u8, s.id as u8);
                    for br in 0..v.rows.min(DIACRITICS.len() as u16) {
                        for bc in 0..v.cols.min(DIACRITICS.len() as u16) {
                            let (r, c) = (v.row + br as i32, v.col as i32 + bc as i32);
                            if !visible(r, c) {
                                continue;
                            }
                            let Some(cell) = buf.cell_mut((rect.x + c as u16, rect.y + r as u16)) else { continue };
                            cell.reset();
                            cell.set_symbol(&placeholder(br, bc));
                            cell.set_style(Style::default().fg(fg));
                        }
                    }
                }
                Mode::Text => {
                    let label = match self.dims(&v.path) {
                        Some((w, h)) => format!("[그림 {w}×{h}]"),
                        None => "[그림]".to_string(),
                    };
                    // 상자의 보이는 첫 행에 싣는다.
                    let Some(r) = (v.row..v.row + v.rows as i32).find(|r| visible(*r, v.col as i32)) else { continue };
                    let room = (rect.w as i32 - v.col as i32).max(0) as usize;
                    let style = Style::default().fg(Color::Indexed(75)).add_modifier(Modifier::UNDERLINED);
                    buf.set_stringn(rect.x + v.col, rect.y + r as u16, &label, room.min(v.cols as usize), style);
                }
            }
        }
    }

    /// 크롬 단추 그림의 번호. 처음 보는 그림이면 그려 보내고 가상 놓기까지 건다 — `term.draw` 안에서
    /// 불려도 이 바이트가 그 프레임의 자리표시보다 먼저 나간다(같은 stdout 버퍼).
    pub fn chrome_icon(&mut self, key: &IconKey) -> Option<u32> {
        if self.mode != Mode::Kitty {
            return None;
        }
        if let Some(id) = self.icons.get(key) {
            return *id;
        }
        let made = chrome::keycap_png(key).map(|png| {
            let id = self.alloc_id();
            let mut bytes = transmit_png(id, &png);
            bytes.extend_from_slice(format!("\x1b_Ga=p,U=1,i={id},p=1,c={},r=1,q=2\x1b\\", key.cells).as_bytes());
            let mut out = std::io::stdout();
            let _ = out.write_all(&bytes);
            id
        });
        self.icons.insert(key.clone(), made);
        made
    }

    /// 단추 그림 자리표시를 한 줄 `cells` 칸에 그린다. 투명한 도트 사이로 `bg` 가 비친다.
    pub fn draw_chrome_icon(&self, buf: &mut Buffer, x: u16, y: u16, cells: u16, id: u32, bg: [u8; 3]) {
        let fg = Color::Rgb((id >> 16) as u8, (id >> 8) as u8, id as u8);
        for c in 0..cells {
            let Some(cell) = buf.cell_mut((x + c, y)) else { continue };
            cell.reset();
            cell.set_symbol(&placeholder(0, c));
            cell.set_style(Style::default().fg(fg).bg(chrome::color(bg)));
        }
    }

    /// 한 칸 타일 그림을 `cells` 칸에 되풀이한다 — 칸마다 같은 (0, 0) 자리표시를 쓴다.
    pub fn draw_chrome_tile(&self, buf: &mut Buffer, x: u16, y: u16, cells: u16, id: u32, bg: [u8; 3]) {
        let fg = Color::Rgb((id >> 16) as u8, (id >> 8) as u8, id as u8);
        let tile = placeholder(0, 0);
        for c in 0..cells {
            let Some(cell) = buf.cell_mut((x + c, y)) else { continue };
            cell.reset();
            cell.set_symbol(&tile);
            cell.set_style(Style::default().fg(fg).bg(chrome::color(bg)));
        }
    }

    fn dims(&mut self, path: &str) -> Option<(u32, u32)> {
        *self.dims.entry(path.to_string()).or_insert_with(|| image::image_dimensions(path).ok())
    }
}

fn placeholder(row: u16, col: u16) -> String {
    let mut s = String::with_capacity(12);
    s.push(PLACEHOLDER);
    s.push(char::from_u32(DIACRITICS[row as usize]).unwrap_or('\u{0305}'));
    s.push(char::from_u32(DIACRITICS[col as usize]).unwrap_or('\u{0305}'));
    s
}

/// 그림 파일을 PNG 로 맞춰 `a=t`(전송만) 조각들로. 읽지 못하면 `None`.
fn transmit(id: u32, path: &str) -> Option<Vec<u8>> {
    let raw = std::fs::read(path).ok()?;
    let png = if raw.starts_with(b"\x89PNG\r\n\x1a\n") {
        raw
    } else {
        let img = image::load_from_memory(&raw).ok()?;
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).ok()?;
        out.into_inner()
    };
    Some(transmit_png(id, &png))
}

fn transmit_png(id: u32, png: &[u8]) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let chunks: Vec<&[u8]> = b64.as_bytes().chunks(CHUNK).collect();
    let mut out = Vec::with_capacity(b64.len() + chunks.len() * 32);
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        if i == 0 {
            out.extend_from_slice(format!("\x1b_Ga=t,f=100,t=d,i={id},q=2,m={more};").as_bytes());
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
        }
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// 바깥 터미널에 kitty 그림 지원과 글자 칸 픽셀 크기를 묻는다. 날 모드에서, 입력 읽기 스레드를
/// 띄우기 전에 부른다 — 답을 그 스레드가 먼저 먹으면 안 된다.
///
/// kitty 질의(`a=q`)·창 픽셀(`CSI 14 t`)·글자 칸 픽셀(`CSI 16 t`) 뒤에 DA1 을 붙여, DA1 답이 오면
/// 기다림을 끝낸다. 모르는 터미널은 앞의 셋을 무시하고 DA1 만 답한다.
#[cfg(unix)]
pub fn probe() -> Probe {
    if let Some(forced) = std::env::var("KASA_TUI_KITTY").ok() {
        let mut p = probe_raw();
        p.kitty = forced == "1";
        return p;
    }
    let mut p = probe_raw();
    // iTerm2 는 `a=q` 에 답하지만 글자 칸 자리표시는 확인되지 않았다 — 실측 전까지 글자로 둔다.
    let iterm = std::env::var("TERM_PROGRAM").is_ok_and(|v| v == "iTerm.app")
        || std::env::var("LC_TERMINAL").is_ok_and(|v| v == "iTerm2");
    if iterm {
        p.kitty = false;
    }
    p
}

#[cfg(unix)]
fn probe_raw() -> Probe {
    let mut out = std::io::stdout();
    let query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[14t\x1b[16t\x1b[c";
    if out.write_all(query).and_then(|_| out.flush()).is_err() {
        return Probe::default();
    }
    let mut got = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_millis(400);
    let fd = libc::STDIN_FILENO;
    while std::time::Instant::now() < deadline {
        let left = deadline.saturating_duration_since(std::time::Instant::now()).as_millis() as i32;
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let n = unsafe { libc::poll(&mut pfd, 1, left.max(1)) };
        if n <= 0 {
            break;
        }
        let mut chunk = [0u8; 512];
        let r = unsafe { libc::read(fd, chunk.as_mut_ptr() as *mut libc::c_void, chunk.len()) };
        if r <= 0 {
            break;
        }
        got.extend_from_slice(&chunk[..r as usize]);
        if da1_done(&got) {
            break;
        }
    }
    parse_probe(&got)
}

#[cfg(windows)]
pub fn probe() -> Probe {
    // Windows Terminal 은 kitty 그림을 모른다(1.24 기준, sixel 만). 픽셀 크기는 crossterm 이 못 준다.
    Probe { kitty: std::env::var("KASA_TUI_KITTY").is_ok_and(|v| v == "1"), cell_px: None }
}

fn da1_done(bytes: &[u8]) -> bool {
    let s = String::from_utf8_lossy(bytes);
    s.find("\x1b[?").is_some_and(|i| s[i..].contains('c'))
}

fn parse_probe(bytes: &[u8]) -> Probe {
    let s = String::from_utf8_lossy(bytes);
    let kitty = s.contains("\x1b_Gi=31;OK");
    // CSI 6 ; h ; w t — 글자 칸 픽셀. 없으면 CSI 4 ; h ; w t(창 픽셀)를 칸 수로 나눈다.
    let report = |kind: &str| -> Option<(u32, u32)> {
        let start = s.find(&format!("\x1b[{kind};"))? + 3 + kind.len();
        let end = s[start..].find('t')? + start;
        let (h, w) = s[start..end].split_once(';')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    };
    let cell_px = report("6").map(|(w, h)| (w as u16, h as u16)).or_else(|| {
        let (w, h) = report("4")?;
        let (cols, rows) = crossterm_size();
        (cols > 0 && rows > 0).then(|| ((w / cols as u32) as u16, (h / rows as u32) as u16))
    });
    Probe { kitty, cell_px: cell_px.filter(|(w, h)| *w > 0 && *h > 0) }
}

fn crossterm_size() -> (u16, u16) {
    ratatui::crossterm::terminal::size().unwrap_or((0, 0))
}

/// 그림 파일을 OS 기본 보기로 연다(글자 자리표시를 누르면).
pub fn open_externally(path: &str) {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut cmd = std::process::Command::new("xdg-open");
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    let _ = cmd
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kitty_ok_and_cell_size() {
        let p = parse_probe(b"\x1b_Gi=31;OK\x1b\\\x1b[6;34;16t\x1b[?62;22c");
        assert!(p.kitty);
        assert_eq!(p.cell_px, Some((16, 34)));
        let none = parse_probe(b"\x1b[?1;2c");
        assert!(!none.kitty);
        assert_eq!(none.cell_px, None);
        assert!(da1_done(b"\x1b[?62;22c"));
        assert!(!da1_done(b"\x1b_Gi=31;OK\x1b\\"));
    }

    #[test]
    fn placeholder_carries_row_and_column() {
        let s = placeholder(1, 2);
        let cs: Vec<u32> = s.chars().map(|c| c as u32).collect();
        assert_eq!(cs, [0x10EEEE, 0x030D, 0x030E]);
    }

    #[test]
    fn transmit_chunks_end_with_m0() {
        let dir = std::env::temp_dir().join(format!("kasa-gfx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([255, 0, 0, 255]));
        img.save(&path).unwrap();
        let bytes = String::from_utf8(transmit(9, path.to_str().unwrap()).unwrap()).unwrap();
        assert!(bytes.starts_with("\x1b_Ga=t,f=100,t=d,i=9,q=2,m="));
        assert!(bytes.contains("m=0;"));
        assert!(!bytes.contains("a=T"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
