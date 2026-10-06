//! 세션 소켓 자리. 세션 하나가 서버 프로세스 하나이고, 소켓 이름이 곧 세션 이름이다.

use std::path::PathBuf;

pub const DEFAULT_SESSION: &str = "main";

/// 이 사용자만 들어갈 수 있는 실행 폴더. 유닉스 소켓 경로는 104바이트 한도라 짧게 둔다.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KASA_TUI_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    #[cfg(unix)]
    {
        if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
            return PathBuf::from(xdg).join("kasa");
        }
        let uid = unsafe { libc::getuid() };
        std::env::temp_dir().join(format!("kasa-{uid}"))
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("kasa")
            .join("run")
    }
}

pub fn ensure_runtime_dir() -> std::io::Result<PathBuf> {
    let dir = runtime_dir();
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

/// 세션의 소켓. 윈도우에서는 `kasa_socket::transport` 가 마지막 이름을 named pipe 이름으로
/// 쓴다 — 파이프 이름 공간은 기계 전체라 사용자 이름을 섞는다.
pub fn socket_path(session: &str) -> PathBuf {
    #[cfg(unix)]
    {
        runtime_dir().join(format!("{session}.sock"))
    }
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
        runtime_dir().join(format!("kasa-tui-{user}-{session}"))
    }
}

/// 세션 표식 파일 — 윈도우는 파이프를 훑을 수 없어, `kasa ls` 가 이것으로 세션을 찾는다.
pub fn marker_path(session: &str) -> PathBuf {
    runtime_dir().join(format!("{session}.session"))
}

/// 표식이 남은 세션 이름들(살았는지는 붙어 봐야 안다).
pub fn known_sessions() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(runtime_dir()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".session")).map(str::to_owned))
        .collect();
    names.sort();
    names
}

pub fn valid_session_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 32 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}
