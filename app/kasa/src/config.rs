//! 사용자 설정. `~/.config/kasa/tui.conf` 의 `키 = 값` 줄과 환경 변수(앞선다)만 읽는다.
//!
//! ```text
//! prefix = C-a
//! ```

use crate::keys::{Key, KeyInput, ALT, CTRL};

#[derive(Debug, Clone)]
pub struct Config {
    /// `None` 이면 접두키를 끈다 — 키는 전부 칸으로 가고, 칸 다루기는 상태 줄 단추로 한다.
    pub prefix: Option<KeyInput>,
}

impl Default for Config {
    fn default() -> Self {
        Self { prefix: Some(KeyInput { key: Key::Char('b'), mods: CTRL }) }
    }
}

impl Config {
    pub fn load() -> Self {
        let mut cfg = Config::default();
        let file = config_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        for line in file.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            if k.trim() == "prefix" {
                if let Some(p) = parse_prefix(v.trim().trim_matches('"')) {
                    cfg.prefix = p;
                }
            }
        }
        if let Some(p) = std::env::var("KASA_TUI_PREFIX").ok().and_then(|v| parse_prefix(&v)) {
            cfg.prefix = p;
        }
        cfg
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(std::path::PathBuf::from(x).join("kasa").join("tui.conf"));
    }
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.map(|h| std::path::PathBuf::from(h).join(".config").join("kasa").join("tui.conf"))
}

/// `none` 이면 접두키를 끈다. 못 읽으면 `None`(설정을 그대로 둔다).
fn parse_prefix(s: &str) -> Option<Option<KeyInput>> {
    if s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("off") {
        return Some(None);
    }
    parse_key(s).map(Some)
}

/// `C-b`·`C-a`·`C-Space`·`M-a` 꼴.
pub fn parse_key(s: &str) -> Option<KeyInput> {
    let mut mods = 0u8;
    let mut rest = s;
    loop {
        if let Some(r) = rest.strip_prefix("C-") {
            mods |= CTRL;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("M-") {
            mods |= ALT;
            rest = r;
        } else {
            break;
        }
    }
    let key = match rest {
        "Space" | "space" => Key::Char(' '),
        r if r.chars().count() == 1 => Key::Char(r.chars().next()?.to_ascii_lowercase()),
        _ => return None,
    };
    (mods != 0).then_some(KeyInput { key, mods })
}

/// 상태 줄에 보일 이름(`^B`).
pub fn key_label(k: &KeyInput) -> String {
    let base = match k.key {
        Key::Char(' ') => "Space".to_string(),
        Key::Char(c) => c.to_ascii_uppercase().to_string(),
        _ => "?".to_string(),
    };
    match (k.mods & CTRL != 0, k.mods & ALT != 0) {
        (true, true) => format!("^M-{base}"),
        (true, false) => format!("^{base}"),
        (false, true) => format!("M-{base}"),
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prefix_names() {
        assert_eq!(parse_key("C-a"), Some(KeyInput { key: Key::Char('a'), mods: CTRL }));
        assert_eq!(parse_key("C-Space"), Some(KeyInput { key: Key::Char(' '), mods: CTRL }));
        assert_eq!(parse_key("M-x").map(|k| k.mods), Some(ALT));
        assert_eq!(parse_key("a"), None);
        assert_eq!(key_label(&parse_key("C-b").unwrap()), "^B");
        assert_eq!(parse_prefix("none"), Some(None));
        assert_eq!(parse_prefix("C-a").map(|p| p.is_some()), Some(true));
    }
}
