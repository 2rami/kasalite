//! 키·마우스·붙여넣기를 칸이 기대하는 바이트로 바꾼다. 바깥 터미널과도, 화면 그리기와도
//! 무관한 순수 변환이라 서버가 칸의 모드(커서 키·마우스·브래킷 붙여넣기)를 보고 부른다.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

pub const SHIFT: u8 = 1;
pub const ALT: u8 = 2;
pub const CTRL: u8 = 4;
pub const SUPER: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyInput {
    pub key: Key,
    pub mods: u8,
}

/// 칸이 켠 입력 모드. 칸의 마지막 화면 갱신에서 읽는다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modes {
    /// DECCKM — 화살표를 `ESC O A` 로 보낸다(vim·readline·claude).
    pub app_cursor: bool,
    /// DECSET 2004.
    pub bracketed_paste: bool,
    /// DECSET 1000/1002/1003 중 하나라도.
    pub mouse: bool,
    /// DECSET 1006.
    pub mouse_sgr: bool,
    /// DECSET 1003 — 버튼 없이 움직여도 알려 달라는 앱.
    pub mouse_motion: bool,
}

/// xterm 수정키 매개변수(`CSI 1 ; m X` 의 m).
fn mod_param(mods: u8) -> u8 {
    1 + (mods & (SHIFT | ALT | CTRL | SUPER))
}

pub fn encode_key(k: KeyInput, modes: &Modes) -> Vec<u8> {
    let m = k.mods;
    let csi_letter = |letter: u8, ss3_plain: bool| -> Vec<u8> {
        if m & (SHIFT | ALT | CTRL | SUPER) == 0 {
            if ss3_plain {
                vec![0x1b, b'O', letter]
            } else {
                vec![0x1b, b'[', letter]
            }
        } else {
            format!("\x1b[1;{}{}", mod_param(m), letter as char).into_bytes()
        }
    };
    let csi_tilde = |n: u8| -> Vec<u8> {
        if m & (SHIFT | ALT | CTRL | SUPER) == 0 {
            format!("\x1b[{n}~").into_bytes()
        } else {
            format!("\x1b[{n};{}~", mod_param(m)).into_bytes()
        }
    };
    let alt_prefix = |mut body: Vec<u8>| -> Vec<u8> {
        if m & ALT != 0 {
            body.insert(0, 0x1b);
        }
        body
    };
    match k.key {
        Key::Char(c) => {
            if m & CTRL != 0 {
                if let Some(b) = ctrl_byte(c) {
                    return alt_prefix(vec![b]);
                }
            }
            let mut buf = [0u8; 4];
            alt_prefix(c.encode_utf8(&mut buf).as_bytes().to_vec())
        }
        // claude 는 kitty 키보드 협상을 안 하고 맨 LF(Ctrl+J 와 같은 바이트)를 줄바꿈으로
        // 읽는다. 맨 Enter 는 CR 로 보내 제출한다. 본판 칸과 같은 규칙이다.
        Key::Enter if m & (SHIFT | ALT) != 0 => b"\n".to_vec(),
        Key::Enter => b"\r".to_vec(),
        Key::Tab if m & SHIFT != 0 => b"\x1b[Z".to_vec(),
        Key::Tab => alt_prefix(b"\t".to_vec()),
        Key::BackTab => b"\x1b[Z".to_vec(),
        Key::Backspace if m & CTRL != 0 => alt_prefix(vec![0x08]),
        Key::Backspace => alt_prefix(vec![0x7f]),
        Key::Esc => alt_prefix(vec![0x1b]),
        Key::Up => csi_letter(b'A', modes.app_cursor),
        Key::Down => csi_letter(b'B', modes.app_cursor),
        Key::Right => csi_letter(b'C', modes.app_cursor),
        Key::Left => csi_letter(b'D', modes.app_cursor),
        Key::Home => csi_letter(b'H', modes.app_cursor),
        Key::End => csi_letter(b'F', modes.app_cursor),
        Key::Insert => csi_tilde(2),
        Key::Delete => csi_tilde(3),
        Key::PageUp => csi_tilde(5),
        Key::PageDown => csi_tilde(6),
        Key::F(n @ 1..=4) => csi_letter(b'P' + (n - 1), true),
        Key::F(n) => match n {
            5 => csi_tilde(15),
            6 => csi_tilde(17),
            7 => csi_tilde(18),
            8 => csi_tilde(19),
            9 => csi_tilde(20),
            10 => csi_tilde(21),
            11 => csi_tilde(23),
            12 => csi_tilde(24),
            _ => Vec::new(),
        },
    }
}

/// Ctrl 과 함께 누른 글자의 C0 바이트. 대응이 없으면 `None`(글자 그대로 보낸다).
fn ctrl_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '@' | ' ' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '-' | '7' | '/' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Button {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseKind {
    Down(Button),
    Up(Button),
    Drag(Button),
    Moved,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
}

/// 칸 안 좌표(0부터)의 마우스 사건을 칸이 켠 보고 형식으로. 칸이 받을 준비가 없는
/// 사건(마우스 모드가 꺼졌거나, 1003 없이 맨 이동)은 `None`.
pub fn encode_mouse(kind: MouseKind, col: u16, row: u16, mods: u8, modes: &Modes) -> Option<Vec<u8>> {
    if !modes.mouse {
        return None;
    }
    let btn_code = |b: Button| match b {
        Button::Left => 0u16,
        Button::Middle => 1,
        Button::Right => 2,
    };
    let (mut code, release) = match kind {
        MouseKind::Down(b) => (btn_code(b), false),
        MouseKind::Up(b) => (btn_code(b), true),
        MouseKind::Drag(b) => (btn_code(b) + 32, false),
        MouseKind::Moved if modes.mouse_motion => (3 + 32, false),
        MouseKind::Moved => return None,
        MouseKind::WheelUp => (64, false),
        MouseKind::WheelDown => (65, false),
        MouseKind::WheelLeft => (66, false),
        MouseKind::WheelRight => (67, false),
    };
    if mods & SHIFT != 0 {
        code += 4;
    }
    if mods & ALT != 0 {
        code += 8;
    }
    if mods & CTRL != 0 {
        code += 16;
    }
    if modes.mouse_sgr {
        let tail = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{tail}", col + 1, row + 1).into_bytes());
    }
    // X10 형식은 좌표를 한 바이트(32 더하기)에 싣는다 — 223 을 넘는 칸은 못 보낸다.
    if col > 222 || row > 222 {
        return None;
    }
    let code = if release { 3 + (code & !3) } else { code };
    Some(vec![0x1b, b'[', b'M', (code + 32) as u8, (col + 33) as u8, (row + 33) as u8])
}

/// 붙여넣기. 칸이 브래킷을 켰을 때만 감싼다 — 안 켠 앱에는 그 바이트가 글자로 들어간다.
/// 감쌀 때는 본문 속 ESC 를 걷어, 붙여넣은 글이 브래킷을 스스로 닫지 못하게 한다.
pub fn encode_paste(text: &str, modes: &Modes) -> Vec<u8> {
    if modes.bracketed_paste {
        let body: String = text.chars().filter(|c| *c != '\x1b').collect();
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(body.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(key: Key, mods: u8) -> KeyInput {
        KeyInput { key, mods }
    }

    #[test]
    fn arrows_follow_cursor_key_mode() {
        let normal = Modes::default();
        let app = Modes { app_cursor: true, ..Modes::default() };
        assert_eq!(encode_key(k(Key::Up, 0), &normal), b"\x1b[A");
        assert_eq!(encode_key(k(Key::Up, 0), &app), b"\x1bOA");
        assert_eq!(encode_key(k(Key::Left, CTRL), &app), b"\x1b[1;5D");
        assert_eq!(encode_key(k(Key::Right, SHIFT | ALT), &normal), b"\x1b[1;4C");
    }

    #[test]
    fn control_and_alt_chars() {
        let m = Modes::default();
        assert_eq!(encode_key(k(Key::Char('c'), CTRL), &m), [0x03]);
        assert_eq!(encode_key(k(Key::Char('C'), CTRL | SHIFT), &m), [0x03]);
        assert_eq!(encode_key(k(Key::Char('x'), ALT), &m), b"\x1bx");
        assert_eq!(encode_key(k(Key::Char('한'), 0), &m), "한".as_bytes());
        assert_eq!(encode_key(k(Key::Char(' '), CTRL), &m), [0]);
    }

    #[test]
    fn shift_enter_is_a_newline_for_claude() {
        let m = Modes::default();
        assert_eq!(encode_key(k(Key::Enter, 0), &m), b"\r");
        assert_eq!(encode_key(k(Key::Enter, SHIFT), &m), b"\n");
        assert_eq!(encode_key(k(Key::Tab, SHIFT), &m), b"\x1b[Z");
    }

    #[test]
    fn function_keys() {
        let m = Modes::default();
        assert_eq!(encode_key(k(Key::F(1), 0), &m), b"\x1bOP");
        assert_eq!(encode_key(k(Key::F(5), 0), &m), b"\x1b[15~");
        assert_eq!(encode_key(k(Key::F(2), SHIFT), &m), b"\x1b[1;2Q");
        assert_eq!(encode_key(k(Key::Delete, CTRL), &m), b"\x1b[3;5~");
    }

    #[test]
    fn mouse_only_when_the_pane_asked() {
        let off = Modes::default();
        assert_eq!(encode_mouse(MouseKind::Down(Button::Left), 0, 0, 0, &off), None);
        let sgr = Modes { mouse: true, mouse_sgr: true, ..Modes::default() };
        assert_eq!(encode_mouse(MouseKind::Down(Button::Left), 4, 2, 0, &sgr).unwrap(), b"\x1b[<0;5;3M");
        assert_eq!(encode_mouse(MouseKind::Up(Button::Left), 4, 2, 0, &sgr).unwrap(), b"\x1b[<0;5;3m");
        assert_eq!(encode_mouse(MouseKind::WheelDown, 0, 0, CTRL, &sgr).unwrap(), b"\x1b[<81;1;1M");
        assert_eq!(encode_mouse(MouseKind::Moved, 0, 0, 0, &sgr), None);
        let x10 = Modes { mouse: true, ..Modes::default() };
        assert_eq!(encode_mouse(MouseKind::Up(Button::Right), 0, 0, 0, &x10).unwrap(), [0x1b, b'[', b'M', 35, 33, 33]);
    }

    #[test]
    fn paste_wraps_only_when_bracketed() {
        let plain = Modes::default();
        assert_eq!(encode_paste("a\nb", &plain), b"a\rb");
        let br = Modes { bracketed_paste: true, ..Modes::default() };
        assert_eq!(encode_paste("a\x1b[201~b", &br), b"\x1b[200~a[201~b\x1b[201~");
    }
}
