//! 키와 마우스를 칸 프로그램이 읽는 바이트로. 본판(kasaterm `input.rs`)의 규칙을 따른다.

use winit::event::{KeyEvent, MouseButton};
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// 칸 화면 상태 중 인코딩이 보는 것.
#[derive(Clone, Copy, Default)]
pub struct Modes {
    pub app_cursor: bool,
    pub alt_screen: bool,
}

/// 키 하나 → 바이트. 보낼 것이 없으면 None(수정키 단독 등).
pub fn encode(event: &KeyEvent, mods: ModifiersState, m: Modes) -> Option<Vec<u8>> {
    let bytes = match &event.logical_key {
        // Shift·Option+Enter 는 줄바꿈(LF) — claude code 는 맨 LF 를 입력창 줄바꿈으로 읽고, CR 은 제출이다.
        Key::Named(NamedKey::Enter) => {
            if mods.shift_key() || mods.alt_key() {
                b"\n".to_vec()
            } else {
                b"\r".to_vec()
            }
        }
        Key::Named(NamedKey::Backspace) => {
            if mods.alt_key() {
                b"\x1b\x7f".to_vec()
            } else {
                b"\x7f".to_vec()
            }
        }
        Key::Named(NamedKey::Tab) => {
            if mods.shift_key() {
                b"\x1b[Z".to_vec()
            } else {
                b"\t".to_vec()
            }
        }
        Key::Named(NamedKey::Escape) => b"\x1b".to_vec(),
        Key::Named(NamedKey::Delete) => {
            if mods.control_key() {
                b"\x1b[3;5~".to_vec()
            } else {
                b"\x1b[3~".to_vec()
            }
        }
        Key::Named(NamedKey::Insert) => b"\x1b[2~".to_vec(),
        Key::Named(NamedKey::Home) => b"\x1b[H".to_vec(),
        Key::Named(NamedKey::End) => b"\x1b[F".to_vec(),
        Key::Named(NamedKey::PageUp) => b"\x1b[5~".to_vec(),
        Key::Named(NamedKey::PageDown) => b"\x1b[6~".to_vec(),
        Key::Named(nk @ (NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::ArrowRight | NamedKey::ArrowLeft)) => {
            let letter = match nk {
                NamedKey::ArrowUp => 'A',
                NamedKey::ArrowDown => 'B',
                NamedKey::ArrowRight => 'C',
                _ => 'D',
            };
            arrow(letter, mods, m)
        }
        Key::Named(f) => match function_key(*f) {
            Some(b) => b,
            None => return event.text.as_ref().map(|t| t.as_bytes().to_vec()).filter(|b| !b.is_empty()),
        },
        Key::Character(c) if mods.control_key() && !mods.super_key() => control_char(c, mods)?,
        _ => {
            let t = event.text.as_ref()?;
            if t.is_empty() {
                return None;
            }
            t.as_bytes().to_vec()
        }
    };
    Some(bytes)
}

/// 셸 프롬프트에는 readline 제어문자를, 전체 화면 TUI(vim·less)에는 CSI 를 보낸다 — zsh 기본 bindkey
/// 에는 `^A`/`^E`·`ESC b`/`ESC f` 만 있고 CSI 수정 화살표가 없다.
fn arrow(letter: char, mods: ModifiersState, m: Modes) -> Vec<u8> {
    if mods.super_key() {
        return match letter {
            'D' if !m.alt_screen => b"\x01".to_vec(),
            'C' if !m.alt_screen => b"\x05".to_vec(),
            'D' => b"\x1b[H".to_vec(),
            'C' => b"\x1b[F".to_vec(),
            _ => format!("\x1b[{letter}").into_bytes(),
        };
    }
    if mods.control_key() {
        return format!("\x1b[1;5{letter}").into_bytes();
    }
    if mods.alt_key() {
        return match letter {
            'D' if !m.alt_screen => b"\x1bb".to_vec(),
            'C' if !m.alt_screen => b"\x1bf".to_vec(),
            _ => format!("\x1b[1;3{letter}").into_bytes(),
        };
    }
    if mods.shift_key() {
        return format!("\x1b[1;2{letter}").into_bytes();
    }
    if m.app_cursor {
        format!("\x1bO{letter}").into_bytes()
    } else {
        format!("\x1b[{letter}").into_bytes()
    }
}

fn function_key(k: NamedKey) -> Option<Vec<u8>> {
    let s: &[u8] = match k {
        NamedKey::F1 => b"\x1bOP",
        NamedKey::F2 => b"\x1bOQ",
        NamedKey::F3 => b"\x1bOR",
        NamedKey::F4 => b"\x1bOS",
        NamedKey::F5 => b"\x1b[15~",
        NamedKey::F6 => b"\x1b[17~",
        NamedKey::F7 => b"\x1b[18~",
        NamedKey::F8 => b"\x1b[19~",
        NamedKey::F9 => b"\x1b[20~",
        NamedKey::F10 => b"\x1b[21~",
        NamedKey::F11 => b"\x1b[23~",
        NamedKey::F12 => b"\x1b[24~",
        _ => return None,
    };
    Some(s.to_vec())
}

/// Ctrl+글자 → C0 제어문자. Ctrl+Option 은 ESC 를 앞에 붙인다.
fn control_char(c: &str, mods: ModifiersState) -> Option<Vec<u8>> {
    let ch = c.chars().next()?.to_ascii_lowercase();
    let b = match ch {
        'a'..='z' => ch as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '7' => 0x1f,
        '/' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    };
    let mut out = Vec::with_capacity(2);
    if mods.alt_key() {
        out.push(0x1b);
    }
    out.push(b);
    Some(out)
}

/// SGR(1006) 마우스 보고. `button` 0 왼쪽·1 가운데·2 오른쪽·64/65 휠, `motion` 은 끌기(+32).
pub fn mouse_sgr(button: u8, col: u16, row: u16, press: bool, motion: bool, mods: ModifiersState) -> Vec<u8> {
    let mut b = button as u32;
    if motion {
        b += 32;
    }
    if mods.shift_key() {
        b += 4;
    }
    if mods.alt_key() {
        b += 8;
    }
    if mods.control_key() {
        b += 16;
    }
    format!("\x1b[<{b};{};{}{}", col + 1, row + 1, if press { 'M' } else { 'm' }).into_bytes()
}

/// 옛 X10 보고 — SGR 을 안 켠 앱용. 좌표가 223 을 넘으면 못 싣는다.
pub fn mouse_x10(button: u8, col: u16, row: u16, press: bool, motion: bool) -> Option<Vec<u8>> {
    if col > 222 || row > 222 {
        return None;
    }
    let mut b = if press { button as u32 } else { 3 };
    if motion {
        b += 32;
    }
    Some(vec![0x1b, b'[', b'M', (32 + b) as u8, (33 + col) as u8, (33 + row) as u8])
}

pub fn button_code(b: MouseButton) -> Option<u8> {
    match b {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        _ => None,
    }
}

/// `kasaterm-cli send-key` 이름 → 바이트.
pub fn named_key(key: &str) -> Option<Vec<u8>> {
    let k = key.trim().to_ascii_lowercase();
    let named: Option<&[u8]> = match k.as_str() {
        "enter" | "return" => Some(b"\r"),
        "tab" => Some(b"\t"),
        "escape" | "esc" => Some(b"\x1b"),
        "backspace" => Some(b"\x7f"),
        "up" => Some(b"\x1b[A"),
        "down" => Some(b"\x1b[B"),
        "right" => Some(b"\x1b[C"),
        "left" => Some(b"\x1b[D"),
        "space" => Some(b" "),
        _ => None,
    };
    if let Some(b) = named {
        return Some(b.to_vec());
    }
    let (mods, last) = k.rsplit_once('+')?;
    let c = last.chars().next().filter(|_| last.chars().count() == 1)?;
    match mods {
        "ctrl" | "c" if c.is_ascii_lowercase() => Some(vec![c as u8 - b'a' + 1]),
        "alt" | "meta" | "m" => Some(format!("\x1b{c}").into_bytes()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_reports_and_named_keys() {
        assert_eq!(mouse_sgr(0, 4, 2, true, false, ModifiersState::empty()), b"\x1b[<0;5;3M");
        assert_eq!(mouse_sgr(64, 0, 0, true, false, ModifiersState::empty()), b"\x1b[<64;1;1M");
        assert_eq!(mouse_sgr(0, 4, 2, false, true, ModifiersState::empty()), b"\x1b[<32;5;3m");
        assert_eq!(mouse_x10(0, 0, 0, true, false).unwrap(), [0x1b, b'[', b'M', 32, 33, 33]);
        assert_eq!(named_key("Enter").unwrap(), b"\r");
        assert_eq!(named_key("ctrl+c").unwrap(), [3]);
        assert!(named_key("hyper+x").is_none());
        assert_eq!(control_char("c", ModifiersState::empty()).unwrap(), [3]);
        assert_eq!(control_char("[", ModifiersState::empty()).unwrap(), [0x1b]);
        assert_eq!(arrow('A', ModifiersState::empty(), Modes { app_cursor: true, alt_screen: false }), b"\x1bOA");
        assert_eq!(arrow('D', ModifiersState::ALT, Modes::default()), b"\x1bb");
    }
}
