//! 칸 안 `kasaterm-cli` 의 창구 — 목록·나누기·새 탭·초점·이름·보내기만. 학생·판·tell 은 본판의 몫이다.
//! 칸·탭을 만지는 일은 창 이벤트 루프에 맡긴다(레이아웃 권한은 루프 한 곳).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Sender};
use kasa_socket::backend::{Backend, SplitDirection, SurfaceInfo, WorkspaceInfo};
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

pub enum Ctl {
    Split { from: Option<String>, right: bool, focus: bool, reply: Sender<Result<String>> },
    NewTab { focus: bool, reply: Sender<Result<String>> },
    Focus(String),
    Rename(String, Option<String>),
}

/// 루프가 바뀔 때마다 적어 두는 칸·탭 사정 — 소켓 스레드가 루프를 기다리지 않고 읽는다.
#[derive(Default)]
pub struct Shared {
    /// (칸 id, 탭 이름, 붙인 이름, 작업 폴더)
    pub panes: Vec<(String, String, Option<String>, Option<String>)>,
    pub tabs: Vec<String>,
    pub active_tab: usize,
    pub focus: String,
}

pub struct LiteBackend {
    pub shared: Arc<Mutex<Shared>>,
    proxy: Mutex<EventLoopProxy<UserEvent>>,
}

impl LiteBackend {
    pub fn new(shared: Arc<Mutex<Shared>>, proxy: EventLoopProxy<UserEvent>) -> Self {
        Self { shared, proxy: Mutex::new(proxy) }
    }

    fn post(&self, ctl: Ctl) -> Result<()> {
        self.proxy.lock().unwrap().send_event(UserEvent::Ctl(ctl)).ok().context("창이 닫혔다")
    }

    fn ask(&self, make: impl FnOnce(Sender<Result<String>>) -> Ctl) -> Result<String> {
        let (tx, rx) = bounded(1);
        self.post(make(tx))?;
        rx.recv_timeout(Duration::from_secs(10)).context("창이 답하지 않았다")?
    }

    fn info(&self, id: &str) -> SurfaceInfo {
        let s = self.shared.lock().unwrap();
        let row = s.panes.iter().find(|p| p.0 == id);
        SurfaceInfo {
            id: id.to_string(),
            workspace_id: row.map(|p| p.1.clone()).unwrap_or_default(),
            title: row.and_then(|p| p.2.clone()),
            cwd: row.and_then(|p| p.3.clone()),
            character: None,
        }
    }

    fn pty(&self, surface: Option<&str>) -> Result<Arc<kasa_pty::PtySession>> {
        let id = surface.map(str::to_owned).unwrap_or_else(|| self.shared.lock().unwrap().focus.clone());
        kasa_pty::lookup_session(&id).with_context(|| format!("칸 {id} 이(가) 없다"))
    }
}

impl Backend for LiteBackend {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        let s = self.shared.lock().unwrap();
        Ok(s.tabs.iter().map(|t| WorkspaceInfo { id: t.clone(), name: t.clone() }).collect())
    }
    fn current_workspace(&self) -> Result<Option<WorkspaceInfo>> {
        let s = self.shared.lock().unwrap();
        Ok(s.tabs.get(s.active_tab).map(|t| WorkspaceInfo { id: t.clone(), name: t.clone() }))
    }
    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        let ids: Vec<String> = self.shared.lock().unwrap().panes.iter().map(|p| p.0.clone()).collect();
        Ok(ids.iter().map(|id| self.info(id)).collect())
    }
    fn focus_surface(&self, surface_id: &str) -> Result<()> {
        self.post(Ctl::Focus(surface_id.to_string()))
    }
    fn split_surface(&self, direction: SplitDirection, focus: bool, from: Option<&str>) -> Result<SurfaceInfo> {
        let right = !matches!(direction, SplitDirection::Up | SplitDirection::Down);
        let from = from.map(str::to_owned);
        let id = self.ask(|reply| Ctl::Split { from, right, focus, reply })?;
        Ok(self.info(&id))
    }
    fn new_tab(&self, _outer: Option<&str>, focus: bool) -> Result<SurfaceInfo> {
        let id = self.ask(|reply| Ctl::NewTab { focus, reply })?;
        Ok(self.info(&id))
    }
    fn rename_surface(&self, surface_id: &str, title: &str) -> Result<()> {
        let title = title.trim();
        self.post(Ctl::Rename(surface_id.to_string(), (!title.is_empty()).then(|| title.to_string())))
    }
    fn send_text(&self, surface_id: Option<&str>, text: &str) -> Result<()> {
        // 줄바꿈은 Enter(CR)다.
        self.pty(surface_id)?.send_bytes(text.replace("\r\n", "\r").replace('\n', "\r").as_bytes())
    }
    fn send_key(&self, surface_id: Option<&str>, key: &str) -> Result<()> {
        let bytes = crate::keys::named_key(key).with_context(|| format!("모르는 키 {key}"))?;
        self.pty(surface_id)?.send_bytes(&bytes)
    }
}

/// 소켓을 열고 칸이 물려받을 환경(`KASATERM_SOCKET_PATH`·shim 폴더)을 건다. 실패해도 터미널은 돈다.
pub fn start(shared: Arc<Mutex<Shared>>, proxy: EventLoopProxy<UserEvent>) {
    let root = crate::config::root();
    let sock = crate::config::socket_path();
    let shim = root.join("shim");
    if let Err(e) = install_shim(&shim) {
        eprintln!("[lite] 칸 shim 을 못 깔았다(칸 안 kasaterm-cli 없이 간다): {e:#}");
    } else {
        std::env::set_var("KASATERM_TMUX_SHIM_DIR", &shim);
    }
    std::env::set_var("KASATERM_SOCKET_PATH", &sock);
    let backend: Arc<dyn Backend> = Arc::new(LiteBackend::new(shared, proxy));
    match kasa_socket::server::Server::bind(&sock) {
        Ok(server) => {
            server.spawn(backend);
        }
        Err(e) => eprintln!("[lite] 제어 소켓 {} 을 못 열었다: {e:#}", sock.display()),
    }
}

/// 칸 shim — `kasaterm-cli`(이 바이너리로 가는 링크)와 zsh rc 넷. 엔진이 `KASATERM_TMUX_SHIM_DIR` 를 보고
/// PATH 앞에 이 폴더를 붙이고 ZDOTDIR 을 여기로 돌린다. 사용자 rc 를 먼저 읽고 마지막에 다시 앞에 붙여,
/// rc 가 PATH 를 다시 짜도 이 폴더의 `kasaterm-cli` 가 이긴다.
fn install_shim(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let quoted = dir.display().to_string().replace('"', "\\\"");
    for (name, body) in [
        (".zshenv", "[ -f \"${HOME}/.zshenv\" ] && source \"${HOME}/.zshenv\"\n".to_string()),
        (".zprofile", "[ -f \"${HOME}/.zprofile\" ] && source \"${HOME}/.zprofile\"\n".to_string()),
        (".zshrc", format!("[ -f \"${{HOME}}/.zshrc\" ] && source \"${{HOME}}/.zshrc\"\nexport PATH=\"{quoted}:${{PATH}}\"\n")),
        (".zlogin", "[ -f \"${HOME}/.zlogin\" ] && source \"${HOME}/.zlogin\"\n".to_string()),
    ] {
        std::fs::write(dir.join(name), body)?;
    }
    let exe = std::env::current_exe()?;
    #[cfg(unix)]
    {
        let link = dir.join("kasaterm-cli");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&exe, &link)?;
    }
    #[cfg(windows)]
    {
        let _ = std::fs::copy(&exe, dir.join("kasaterm-cli.exe"));
    }
    Ok(())
}
