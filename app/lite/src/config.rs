//! 라이트의 살림 폴더와 설정. 옛 v0.1 과 같은 자리(`~/.config/kasaterm-lite`)를 써서 설정이 이어진다.

use std::path::PathBuf;

use serde_json::Value;

/// 살림 뿌리. 검증 리그는 `KASALITE_ROOT`(옛 이름 `KASATERM_LITE_ROOT`)로 따로 띄운다.
pub fn root() -> PathBuf {
    for key in ["KASALITE_ROOT", "KASATERM_LITE_ROOT"] {
        if let Some(v) = std::env::var_os(key).filter(|v| !v.is_empty()) {
            return PathBuf::from(v);
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join(".config").join("kasaterm-lite")
}

pub fn socket_path() -> PathBuf {
    root().join("lite.sock")
}

pub struct Settings {
    /// 논리 px.
    pub font_size: f32,
    pub font_path: Option<String>,
    pub window: Option<(f64, f64)>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { font_size: 13.0, font_path: None, window: None }
    }
}

pub fn load() -> Settings {
    let mut s = Settings::default();
    let path = root().join("settings.json");
    let Some(v) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
        return s;
    };
    if let Some(f) = v.get("font_size").and_then(Value::as_f64).filter(|f| (6.0..=72.0).contains(f)) {
        s.font_size = f as f32;
    }
    s.font_path = v.get("font_path").and_then(Value::as_str).filter(|p| !p.is_empty()).map(str::to_owned);
    if let (Some(w), Some(h)) = (
        v.pointer("/window/width").and_then(Value::as_f64),
        v.pointer("/window/height").and_then(Value::as_f64),
    ) {
        s.window = Some((w.max(320.0), h.max(200.0)));
    }
    s
}

/// 글자 크기만 고쳐 쓴다 — 사람이 손으로 적은 다른 칸은 그대로 둔다.
pub fn save_font_size(size: f32) {
    let path = root().join("settings.json");
    let mut v = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    v["font_size"] = serde_json::json!(size);
    let _ = std::fs::create_dir_all(root());
    if let Ok(text) = serde_json::to_vec_pretty(&v) {
        let _ = std::fs::write(path, text);
    }
}
