//! KasaLite — 칸 나누기·탭·한글·kitty/iTerm2 그림만 남긴 가볍고 빠른 GUI 터미널. 칸 그리기·PTY·VT 는
//! kasaterm 엔진 크레이트(`kasa-gridview`·`kasa-pty`)를 그대로 쓴다.
//!
//! 같은 바이너리를 `kasaterm-cli` 라는 이름으로 부르면 칸 안 제어 CLI 로 돈다(칸 shim 이 그 링크를 둔다).

mod app;
mod bench;
mod config;
mod control;
mod keys;
#[cfg(target_os = "macos")]
mod mac;
mod pane;

use winit::event_loop::EventLoop;

/// 바깥(본판 칸·다른 터미널)이 남긴 정체. 물려받으면 칸의 셸·claude 가 그 바깥의 칸인 척한다.
const SCRUB_ENV: &[&str] = &[
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "KITTY_PUBLIC_KEY",
    "GHOSTTY_BIN_DIR",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_SHELL_FEATURES",
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "WT_SESSION",
    "WT_PROFILE_ID",
    "VTE_VERSION",
    "KONSOLE_VERSION",
    "WEZTERM_PANE",
    "WEZTERM_EXECUTABLE",
    "ALACRITTY_WINDOW_ID",
    "TMUX",
    "TMUX_PANE",
    "STY",
    "CMUX_SOCKET_PATH",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_TEAMMATE_MODE",
    "CLAUDE_CODE_FORK_SUBAGENT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDECODE",
];

/// 라이트가 스스로 읽는 `KASATERM_*` — 나머지(본판 칸 주소·shim)는 칸에 물려주지 않는다.
const KEEP_KASATERM: &[&str] = &[
    "KASATERM_LITE_ROOT",
    "KASATERM_NO_FOCUS",
    "KASATERM_P3_ROOT",
    "KASATERM_PIXEL_FORMAT",
    "KASATERM_CELL_TIGHTEN",
];

fn scrub_env() {
    let drop: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| (k.starts_with("KASATERM_") && !KEEP_KASATERM.contains(&k.as_str())) || SCRUB_ENV.contains(&k.as_str()))
        .collect();
    for k in drop {
        std::env::remove_var(k);
    }
    // 본판 칸 안에서 띄우면 그 칸의 shim 폴더가 PATH 맨 앞과 ZDOTDIR 에 남는다.
    let is_shim = |p: &std::path::Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("kasaterm-shim-") || n.starts_with("kasaterm-lite-shim-"))
    };
    if let Some(path) = std::env::var_os("PATH") {
        let kept: Vec<_> = std::env::split_paths(&path).filter(|p| !is_shim(p)).collect();
        if let Ok(joined) = std::env::join_paths(kept) {
            std::env::set_var("PATH", joined);
        }
    }
    if std::env::var_os("ZDOTDIR").is_some_and(|d| is_shim(std::path::Path::new(&d))) {
        std::env::remove_var("ZDOTDIR");
    }
}

fn main() {
    let argv0 = std::env::args_os().next().unwrap_or_default();
    let called = std::path::Path::new(&argv0).file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
    if called == "kasaterm-cli" {
        kasa_socket::cli::main();
        return;
    }
    scrub_env();
    let _ = std::fs::create_dir_all(config::root());
    let el = match EventLoop::<app::UserEvent>::with_user_event().build() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("[lite] 이벤트 루프를 못 세웠다: {e}");
            std::process::exit(1);
        }
    };
    let mut app = app::App::new(el.create_proxy());
    if let Err(e) = el.run_app(&mut app) {
        eprintln!("[lite] {e}");
        std::process::exit(1);
    }
}
