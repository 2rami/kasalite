//! 세션 서버. 칸(PTY)·레이아웃·탭을 쥔다. 클라이언트는 그리고 입력을 넘길 뿐이고,
//! 「나눠 줘」 같은 의도만 보낸다 — 레이아웃 권한을 한 곳에만 둔다(2026-06 본판 데몬이
//! 레이아웃 권한을 둘로 나눴다가 죽었다).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};
use kasa_pty::{PtyLayout, PtyOptions, PtySession, SplitDir};
use kasa_screen::screen::{Cell, Color, ScreenUpdate};
use kasa_socket::transport::{LocalListener, LocalStream};
use unicode_width::UnicodeWidthChar;

use crate::collab::{self, Ctl, PaneMeta, Shared, TuiBackend};
use crate::keys::{self, Modes};
use crate::paths;
use crate::proto::{
    self, ClientMsg, Command, Dir, DividerMsg, LayoutMsg, PaneFrame, PaneRect, ServerMsg, SessionInfo, WCell, WColor,
};

/// 아래 한 줄은 탭 줄이다.
const STATUS_ROWS: u16 = 1;

/// 바깥 터미널이 남긴 정체. 서버가 물려받으면 칸의 자식이 그 터미널 전용 이스케이프를
/// 보낸다. 본판 칸의 협업 주소(`KASATERM_*`)와 claude 마커도 걷는다 — 칸에서 띄운 서버가
/// 그 칸의 정체로 모든 셸을 낳으면 안 된다.
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

fn scrub_env() {
    let kasaterm: Vec<String> = std::env::vars().map(|(k, _)| k).filter(|k| k.starts_with("KASATERM_")).collect();
    for k in SCRUB_ENV.iter().map(|s| s.to_string()).chain(kasaterm) {
        std::env::remove_var(k);
    }
    // 본판 칸 안에서 띄우면 그 칸의 shim 폴더가 PATH 맨 앞과 ZDOTDIR 에 남는다. 그대로 두면 칸의
    // `claude`·`kasaterm-cli` 가 본판 칸 정체를 찾다 실패한다.
    if let Some(path) = std::env::var_os("PATH") {
        let kept: Vec<_> = std::env::split_paths(&path).filter(|p| !is_kasaterm_shim(p)).collect();
        if let Ok(joined) = std::env::join_paths(kept) {
            std::env::set_var("PATH", joined);
        }
    }
    if std::env::var_os("ZDOTDIR").is_some_and(|d| is_kasaterm_shim(std::path::Path::new(&d))) {
        std::env::remove_var("ZDOTDIR");
    }
}

fn is_kasaterm_shim(p: &std::path::Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("kasaterm-shim-") || n.starts_with("kasaterm-lite-shim-"))
}

enum Ev {
    Conn(LocalStream),
    Msg(u64, ClientMsg),
    Gone(u64),
    Screen(String, ScreenUpdate),
    Clipboard(String),
    Ctl(Ctl),
}

struct Pane {
    session: Arc<PtySession>,
    modes: Modes,
    title: Option<String>,
    scrolled: u32,
    /// 칸 크기를 알려 둔 값 — 같으면 다시 안 부른다.
    size: (u16, u16),
}

struct Tab {
    name: Option<String>,
    auto_name: String,
    layout: PtyLayout,
    focus: String,
    zoomed: bool,
}

struct Client {
    tx: Sender<Vec<u8>>,
    attached: bool,
    size: (u16, u16),
}

struct Server {
    name: String,
    created_unix: u64,
    tabs: Vec<Tab>,
    active: usize,
    panes: HashMap<String, Pane>,
    clients: HashMap<u64, Client>,
    /// 칸 격자를 맞춘 창 크기 — 마지막으로 만진 클라이언트를 따른다(tmux `window-size latest`).
    size: (u16, u16),
    cell_px: Option<(u16, u16)>,
    next_pane: u32,
    ev_tx: Sender<Ev>,
    /// 협업 창구(제어 소켓 백엔드)와 함께 보는 칸·탭 사정.
    shared: Arc<std::sync::Mutex<Shared>>,
}

struct ClipboardToClients(std::sync::Mutex<Sender<Ev>>);

impl kasa_pty::ClipboardSink for ClipboardToClients {
    // 바깥 터미널 클립보드를 읽어 오려면 OSC 52 질의의 답을 기다려야 한다. 그 사이 칸이 멎지
    // 않게 빈 글로 답한다 — 붙여넣기는 바깥 터미널의 붙여넣기로 들어온다.
    fn load(&self) -> String {
        String::new()
    }
    fn store(&self, text: &str) {
        if let Ok(tx) = self.0.lock() {
            let _ = tx.send(Ev::Clipboard(text.to_string()));
        }
    }
}

pub fn run(name: &str, size: (u16, u16), cwd: Option<String>) -> Result<i32> {
    if !paths::valid_session_name(name) {
        bail!("세션 이름은 영문·숫자·-·_ 32자까지다: {name}");
    }
    scrub_env();
    paths::ensure_runtime_dir().context("실행 폴더")?;
    let sock = paths::socket_path(name);
    if LocalStream::connect(&sock).is_ok() {
        bail!("세션 {name} 이(가) 이미 떠 있다");
    }
    #[cfg(unix)]
    let _ = std::fs::remove_file(&sock);
    let listener = LocalListener::bind(&sock).with_context(|| format!("소켓 {}", sock.display()))?;
    let marker = paths::marker_path(name);
    std::fs::write(&marker, std::process::id().to_string())?;

    let (ev_tx, ev_rx) = unbounded::<Ev>();
    let shared = Arc::new(std::sync::Mutex::new(Shared::default()));
    let _collab = start_collab(name, shared.clone(), ev_tx.clone())?;
    kasa_pty::set_host_policy(kasa_pty::HostPolicy {
        term_program: None,
        last_login_dir: None,
        clipboard: Some(Arc::new(ClipboardToClients(std::sync::Mutex::new(ev_tx.clone())))),
    });
    {
        let tx = ev_tx.clone();
        std::thread::Builder::new().name("kasa-accept".into()).spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        if tx.send(Ev::Conn(s)).is_err() {
                            break;
                        }
                    }
                    Err(e) => eprintln!("[kasa] accept: {e}"),
                }
            }
        })?;
    }

    let mut srv = Server {
        name: name.to_string(),
        created_unix: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        tabs: Vec::new(),
        active: 0,
        panes: HashMap::new(),
        clients: HashMap::new(),
        size: (size.0.max(10), size.1.max(3)),
        cell_px: None,
        next_pane: 0,
        ev_tx,
        shared,
    };
    srv.new_tab(cwd)?;
    let code = srv.serve(ev_rx);
    let _ = std::fs::remove_file(&marker);
    let _ = std::fs::remove_file(paths::ctl_socket_path(name));
    #[cfg(unix)]
    let _ = std::fs::remove_file(&sock);
    Ok(code)
}

impl Server {
    fn serve(&mut self, ev_rx: Receiver<Ev>) -> i32 {
        let mut next_client = 1u64;
        let mut last_names = Instant::now();
        loop {
            let ev = match ev_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(ev) => Some(ev),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                Err(_) => return 1,
            };
            match ev {
                Some(Ev::Conn(stream)) => {
                    let id = next_client;
                    next_client += 1;
                    if let Err(e) = self.add_client(id, stream) {
                        eprintln!("[kasa] client {id}: {e:#}");
                    }
                }
                Some(Ev::Msg(id, msg)) => {
                    if self.on_msg(id, msg) {
                        return 0;
                    }
                }
                Some(Ev::Gone(id)) => {
                    self.clients.remove(&id);
                }
                Some(Ev::Screen(pane, update)) => {
                    if update.eof {
                        self.pane_exited(&pane);
                        if self.tabs.is_empty() {
                            self.broadcast(&ServerMsg::Bye("모든 칸이 끝나 세션을 닫았다".into()));
                            // 작별 인사가 소켓에 닿을 틈을 준다.
                            std::thread::sleep(Duration::from_millis(100));
                            return 0;
                        }
                        self.broadcast_layout();
                    } else {
                        self.on_screen(&pane, update);
                    }
                }
                Some(Ev::Clipboard(text)) => self.broadcast(&ServerMsg::Clipboard(text)),
                Some(Ev::Ctl(ctl)) => self.on_ctl(ctl),
                None => {}
            }
            if last_names.elapsed() >= Duration::from_secs(1) {
                last_names = Instant::now();
                if self.refresh_tab_names() {
                    self.broadcast_layout();
                }
            }
        }
    }

    fn add_client(&mut self, id: u64, stream: LocalStream) -> Result<()> {
        let mut reader = stream.try_clone()?;
        let mut writer = stream;
        let (tx, rx) = unbounded::<Vec<u8>>();
        let ev = self.ev_tx.clone();
        std::thread::Builder::new().name(format!("kasa-read-{id}")).spawn(move || {
            loop {
                match proto::read_msg::<ClientMsg, _>(&mut reader) {
                    Ok(Some(msg)) => {
                        if ev.send(Ev::Msg(id, msg)).is_err() {
                            return;
                        }
                    }
                    _ => break,
                }
            }
            let _ = ev.send(Ev::Gone(id));
        })?;
        let ev = self.ev_tx.clone();
        std::thread::Builder::new().name(format!("kasa-write-{id}")).spawn(move || {
            use std::io::Write;
            while let Ok(buf) = rx.recv() {
                if writer.write_all(&buf).and_then(|_| writer.flush()).is_err() {
                    break;
                }
            }
            let _ = ev.send(Ev::Gone(id));
        })?;
        self.clients.insert(id, Client { tx, attached: false, size: self.size });
        Ok(())
    }

    /// 참이면 서버를 끝낸다.
    fn on_msg(&mut self, id: u64, msg: ClientMsg) -> bool {
        match msg {
            ClientMsg::Hello { protocol, cols, rows, cell_px } => {
                if protocol != proto::PROTOCOL {
                    self.send(id, &ServerMsg::Bye(format!(
                        "서버 판이 다르다(서버 {}, 클라이언트 {protocol}) — `kasa kill {}` 뒤 다시 붙어라",
                        proto::PROTOCOL,
                        self.name
                    )));
                    return false;
                }
                if let Some(c) = self.clients.get_mut(&id) {
                    c.attached = true;
                    c.size = (cols, rows);
                }
                self.set_cell_px(cell_px);
                self.take_size(id);
                self.send_layout(id);
                for pane in self.panes.keys().cloned().collect::<Vec<_>>() {
                    self.send_full(id, &pane);
                }
            }
            ClientMsg::Resize { cols, rows, cell_px } => {
                if let Some(c) = self.clients.get_mut(&id) {
                    c.size = (cols, rows);
                }
                self.set_cell_px(cell_px);
                self.take_size(id);
            }
            ClientMsg::Key(k) => {
                self.take_size(id);
                if let Some(p) = self.focused_pane() {
                    if p.scrolled > 0 {
                        p.session.scroll_to_bottom();
                    }
                    let _ = p.session.send_bytes(&keys::encode_key(k, &p.modes));
                }
            }
            ClientMsg::Paste(text) => {
                self.take_size(id);
                if let Some(p) = self.focused_pane() {
                    let _ = p.session.send_bytes(&keys::encode_paste(&text, &p.modes));
                }
            }
            ClientMsg::Mouse { pane, kind, col, row, mods } => {
                self.take_size(id);
                if let Some(p) = self.panes.get(&pane) {
                    if let Some(bytes) = keys::encode_mouse(kind, col, row, mods, &p.modes) {
                        let _ = p.session.send_bytes(&bytes);
                    }
                }
            }
            ClientMsg::Scroll { pane, lines } => {
                if let Some(p) = self.panes.get(&pane) {
                    if lines == 0 {
                        p.session.scroll_to_bottom();
                    } else {
                        p.session.scroll(lines);
                    }
                }
            }
            ClientMsg::Command(cmd) => return self.on_command(id, cmd),
            ClientMsg::Query => {
                let info = SessionInfo {
                    name: self.name.clone(),
                    pid: std::process::id(),
                    tabs: self.tabs.len(),
                    panes: self.panes.len(),
                    clients: self.clients.values().filter(|c| c.attached).count(),
                    created_unix: self.created_unix,
                };
                self.send(id, &ServerMsg::Info(info));
            }
            ClientMsg::Detach => {
                self.clients.remove(&id);
            }
        }
        false
    }

    fn on_command(&mut self, id: u64, cmd: Command) -> bool {
        self.take_size(id);
        match cmd {
            Command::Split { right } => {
                let Some(tab) = self.tabs.get(self.active) else { return false };
                let target = tab.focus.clone();
                let cwd = self.panes.get(&target).and_then(pane_cwd);
                match self.spawn_pane(cwd) {
                    Ok(new_id) => {
                        let dir = if right { SplitDir::Horizontal } else { SplitDir::Vertical };
                        let tab = &mut self.tabs[self.active];
                        tab.layout.split_leaf(&target, dir, new_id.clone());
                        tab.focus = new_id;
                        tab.zoomed = false;
                    }
                    Err(e) => eprintln!("[kasa] split: {e:#}"),
                }
            }
            Command::ClosePane => {
                if let Some(tab) = self.tabs.get(self.active) {
                    let target = tab.focus.clone();
                    // 셸을 죽이면 EOF 가 돌아와 `pane_exited` 가 레이아웃에서 걷는다.
                    if let Some(p) = self.panes.get(&target) {
                        p.session.terminate_local();
                    }
                    self.pane_exited(&target);
                    if self.tabs.is_empty() {
                        self.broadcast(&ServerMsg::Bye("모든 칸이 끝나 세션을 닫았다".into()));
                        std::thread::sleep(Duration::from_millis(100));
                        return true;
                    }
                }
            }
            Command::NewTab => {
                let cwd = self.focused_pane().and_then(|p| pane_cwd(p));
                if let Err(e) = self.new_tab(cwd) {
                    eprintln!("[kasa] new tab: {e:#}");
                }
            }
            Command::NextTab => {
                if !self.tabs.is_empty() {
                    self.active = (self.active + 1) % self.tabs.len();
                }
            }
            Command::PrevTab => {
                if !self.tabs.is_empty() {
                    self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
                }
            }
            Command::SelectTab(i) => {
                if i < self.tabs.len() {
                    self.active = i;
                }
            }
            Command::Focus(pane) => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if tab.layout.leaves().contains(&pane.as_str()) {
                        tab.focus = pane;
                    }
                }
            }
            Command::FocusDir(dir) => self.focus_dir(dir),
            Command::Zoom => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.zoomed = !tab.zoomed && tab.layout.leaves().len() > 1;
                }
            }
            Command::RenameTab(name) => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.name = Some(name).filter(|n| !n.trim().is_empty());
                }
            }
            Command::MoveDivider { path, pos } => {
                let (cols, rows) = self.pane_area();
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    // 선은 왼쪽(위쪽) 칸의 마지막 칸에 그린다 — 분할 경계는 그 다음 칸이다.
                    tab.layout.resize_divider(&path, pos.saturating_add(1), cols, rows);
                }
            }
            Command::KillServer => {
                self.broadcast(&ServerMsg::Bye(format!("세션 {} 을(를) 끝냈다", self.name)));
                std::thread::sleep(Duration::from_millis(100));
                return true;
            }
        }
        self.apply_sizes();
        self.broadcast_layout();
        false
    }

    fn focus_dir(&mut self, dir: Dir) {
        let rects = self.content_rects();
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Some(cur) = rects.iter().find(|r| r.id == tab.focus) else { return };
        let (cx, cy) = (cur.x as i32 + cur.w as i32 / 2, cur.y as i32 + cur.h as i32 / 2);
        let best = rects
            .iter()
            .filter(|r| r.id != cur.id)
            .filter(|r| match dir {
                Dir::Left => (r.x + r.w) as i32 <= cur.x as i32,
                Dir::Right => r.x as i32 >= (cur.x + cur.w) as i32,
                Dir::Up => (r.y + r.h) as i32 <= cur.y as i32,
                Dir::Down => r.y as i32 >= (cur.y + cur.h) as i32,
            })
            .min_by_key(|r| {
                let (rx, ry) = (r.x as i32 + r.w as i32 / 2, r.y as i32 + r.h as i32 / 2);
                let (along, across) = match dir {
                    Dir::Left | Dir::Right => ((rx - cx).abs(), (ry - cy).abs()),
                    Dir::Up | Dir::Down => ((ry - cy).abs(), (rx - cx).abs()),
                };
                along + across * 2
            });
        if let Some(r) = best {
            tab.focus = r.id.clone();
        }
    }

    fn spawn_pane(&mut self, cwd: Option<String>) -> Result<String> {
        let id = format!("%{}", self.next_pane);
        self.next_pane += 1;
        let (cols, rows) = self.pane_area();
        let session = Arc::new(PtySession::start(PtyOptions {
            cwd,
            cols: cols.max(2),
            rows: rows.max(1),
            pane_id: id.clone(),
            ..PtyOptions::default()
        })?);
        let rx = session.screens.clone();
        let ev = self.ev_tx.clone();
        let pane_id = id.clone();
        // 엔진의 화면 채널(256)이 차면 부분 손상 프레임이 버려지고 그 줄은 바뀔 때까지 다시
        // 안 온다. 그래서 받는 즉시 서버 큐로 옮겨 채널을 늘 비워 둔다.
        std::thread::Builder::new().name(format!("kasa-pane-{id}")).spawn(move || {
            while let Ok(u) = rx.recv() {
                let eof = u.eof;
                if ev.send(Ev::Screen(pane_id.clone(), u)).is_err() || eof {
                    return;
                }
            }
        })?;
        kasa_pty::register_session(&id, &session);
        self.panes.insert(
            id.clone(),
            Pane { session, modes: Modes::default(), title: None, scrolled: 0, size: (cols, rows) },
        );
        self.shared.lock().unwrap().panes.insert(id.clone(), PaneMeta::default());
        Ok(id)
    }

    fn new_tab(&mut self, cwd: Option<String>) -> Result<()> {
        let id = self.spawn_pane(cwd)?;
        self.tabs.push(Tab {
            name: None,
            auto_name: String::new(),
            layout: PtyLayout::single(id.clone()),
            focus: id,
            zoomed: false,
        });
        self.active = self.tabs.len() - 1;
        self.refresh_tab_names();
        self.apply_sizes();
        Ok(())
    }

    fn pane_exited(&mut self, pane: &str) {
        self.panes.remove(pane);
        self.shared.lock().unwrap().panes.remove(pane);
        kasa_pty::release_session(pane);
        let Some(ti) = self.tabs.iter().position(|t| t.layout.leaves().contains(&pane)) else { return };
        let tab = &mut self.tabs[ti];
        if tab.layout.leaves().len() <= 1 {
            self.tabs.remove(ti);
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            } else if ti < self.active {
                self.active -= 1;
            }
            return;
        }
        let order = tab.layout.leaves().iter().map(|s| s.to_string()).collect::<Vec<_>>();
        tab.layout.remove_leaf(pane);
        if tab.focus == pane {
            let at = order.iter().position(|p| p == pane).unwrap_or(0);
            let rest = tab.layout.leaves();
            tab.focus = rest.get(at.min(rest.len().saturating_sub(1))).map(|s| s.to_string()).unwrap_or_default();
        }
        tab.zoomed = false;
        self.apply_sizes();
    }

    fn focused_pane(&mut self) -> Option<&mut Pane> {
        let focus = self.tabs.get(self.active)?.focus.clone();
        self.panes.get_mut(&focus)
    }

    /// 바깥 터미널의 글자 칸 픽셀 크기를 엔진에 알린다. 칸 안 프로그램(`kitten icat`·Claude Code)은
    /// `TIOCGWINSZ` 의 픽셀 값으로 그림 칸 수를 재고, 0 이면 그리기를 포기한다. 값이 바뀌면 칸 크기를
    /// 다시 알려 픽셀 값이 실리게 한다.
    fn set_cell_px(&mut self, cell_px: Option<(u16, u16)>) {
        let Some((w, h)) = cell_px.filter(|(w, h)| *w > 0 && *h > 0) else { return };
        if self.cell_px == Some((w, h)) {
            return;
        }
        self.cell_px = Some((w, h));
        kasa_pty::set_cell_pixels(w as u32, h as u32);
        for p in self.panes.values_mut() {
            p.size = (0, 0);
        }
        self.apply_sizes();
    }

    /// 칸이 차지할 수 있는 넓이(탭 줄 빼고).
    fn pane_area(&self) -> (u16, u16) {
        (self.size.0, self.size.1.saturating_sub(STATUS_ROWS).max(1))
    }

    /// 마지막으로 만진 클라이언트의 크기로 칸을 맞춘다.
    fn take_size(&mut self, id: u64) {
        let Some(size) = self.clients.get(&id).filter(|c| c.attached).map(|c| c.size) else { return };
        if size != self.size {
            self.size = size;
            self.apply_sizes();
            self.broadcast_layout();
        }
    }

    /// 칸마다 경계선 몫을 뗀 글자 자리. 오른쪽·아래가 창 끝이 아니면 한 칸씩 내준다.
    fn tab_rects(&self, tab: &Tab) -> Vec<PaneRect> {
        let (cols, rows) = self.pane_area();
        if tab.zoomed {
            return vec![PaneRect { id: tab.focus.clone(), x: 0, y: 0, w: cols, h: rows }];
        }
        tab.layout
            .leaf_rects(cols, rows)
            .into_iter()
            .map(|(id, x, y, w, h)| {
                let w = if x + w < cols { w.saturating_sub(1) } else { w };
                let h = if y + h < rows { h.saturating_sub(1) } else { h };
                PaneRect { id, x, y, w: w.max(1), h: h.max(1) }
            })
            .collect()
    }

    fn content_rects(&self) -> Vec<PaneRect> {
        self.tabs.get(self.active).map(|t| self.tab_rects(t)).unwrap_or_default()
    }

    fn apply_sizes(&mut self) {
        let mut want: Vec<(String, u16, u16)> = Vec::new();
        for tab in &self.tabs {
            // 확대 중에도 다른 칸은 원래 자리 크기를 지킨다 — 되돌릴 때 다시 그리지 않게.
            let unzoomed = Tab { zoomed: false, name: None, auto_name: String::new(), layout: tab.layout.clone(), focus: tab.focus.clone() };
            for r in self.tab_rects(&unzoomed) {
                want.push((r.id, r.w, r.h));
            }
            if tab.zoomed {
                let (cols, rows) = self.pane_area();
                want.retain(|(id, ..)| *id != tab.focus);
                want.push((tab.focus.clone(), cols, rows));
            }
        }
        for (id, w, h) in want {
            if let Some(p) = self.panes.get_mut(&id) {
                if p.size != (w, h) {
                    p.size = (w, h);
                    let _ = p.session.resize(w, h);
                }
            }
        }
    }

    fn refresh_tab_names(&mut self) -> bool {
        let mut changed = false;
        let names: HashMap<String, String> = self
            .shared
            .lock()
            .unwrap()
            .panes
            .iter()
            .filter_map(|(id, m)| m.name.clone().map(|n| (id.clone(), n)))
            .collect();
        for tab in &mut self.tabs {
            let name = names
                .get(&tab.focus)
                .cloned()
                .or_else(|| self.panes.get(&tab.focus).and_then(|p| p.session.active_process_name()))
                // 로그인 셸은 `-zsh` 처럼 앞에 `-` 가 붙어 온다.
                .map(|n| n.trim_start_matches('-').to_string())
                .unwrap_or_else(|| "sh".into());
            if name != tab.auto_name {
                tab.auto_name = name;
                changed = true;
            }
        }
        changed
    }

    fn layout_msg(&self) -> LayoutMsg {
        let (cols, rows) = self.pane_area();
        let Some(tab) = self.tabs.get(self.active) else {
            return LayoutMsg { session: self.name.clone(), cols: self.size.0, rows: self.size.1, ..LayoutMsg::default() };
        };
        let dividers = if tab.zoomed {
            Vec::new()
        } else {
            tab.layout
                .dividers(cols, rows)
                .into_iter()
                .map(|d| DividerMsg {
                    path: d.path,
                    vertical_line: d.dir == SplitDir::Horizontal,
                    at: d.edge.saturating_sub(1),
                    start: d.span_start,
                    len: d.span_len,
                })
                .collect()
        };
        LayoutMsg {
            session: self.name.clone(),
            cols: self.size.0,
            rows: self.size.1,
            tabs: self.tabs.iter().map(|t| t.name.clone().unwrap_or_else(|| t.auto_name.clone())).collect(),
            active_tab: self.active,
            panes: self.tab_rects(tab),
            dividers,
            focus: tab.focus.clone(),
            zoomed: tab.zoomed,
        }
    }

    fn send(&self, id: u64, msg: &ServerMsg) {
        if let (Some(c), Ok(buf)) = (self.clients.get(&id), proto::encode(msg)) {
            let _ = c.tx.send(buf);
        }
    }

    fn broadcast(&self, msg: &ServerMsg) {
        let Ok(buf) = proto::encode(msg) else { return };
        for c in self.clients.values().filter(|c| c.attached) {
            let _ = c.tx.send(buf.clone());
        }
    }

    fn send_layout(&self, id: u64) {
        self.send(id, &ServerMsg::Layout(self.layout_msg()));
    }

    fn broadcast_layout(&self) {
        self.sync_shared();
        self.broadcast(&ServerMsg::Layout(self.layout_msg()));
    }

    /// 협업 창구가 보는 탭·초점·칸 자리를 지금 레이아웃에 맞춘다.
    fn sync_shared(&self) {
        let mut sh = self.shared.lock().unwrap();
        sh.tabs = self.tabs.iter().map(|t| t.name.clone().unwrap_or_else(|| t.auto_name.clone())).collect();
        sh.active_tab = self.active;
        sh.focus = self.tabs.get(self.active).map(|t| t.focus.clone()).unwrap_or_default();
        for (i, tab) in self.tabs.iter().enumerate() {
            let room = format!("{} · {}", self.name, sh.tabs[i]);
            for leaf in tab.layout.leaves() {
                if let Some(m) = sh.panes.get_mut(leaf) {
                    m.room = room.clone();
                    m.visible = i == self.active;
                }
            }
        }
    }

    fn on_ctl(&mut self, ctl: Ctl) {
        match ctl {
            Ctl::Split { from, right, focus, reply } => {
                let _ = reply.send(self.split_from(from, right, focus));
            }
            Ctl::NewTab { focus, reply } => {
                let before = self.active;
                let cwd = self.tabs.get(self.active).and_then(|t| self.panes.get(&t.focus)).and_then(pane_cwd);
                let made = self.new_tab(cwd).map(|_| self.tabs[self.active].focus.clone());
                if !focus {
                    self.active = before;
                }
                let _ = reply.send(made);
            }
            Ctl::Focus(pane) => {
                if let Some(i) = self.tabs.iter().position(|t| t.layout.leaves().contains(&pane.as_str())) {
                    self.active = i;
                    self.tabs[i].focus = pane;
                }
            }
            Ctl::Refresh => {
                self.refresh_tab_names();
            }
        }
        self.apply_sizes();
        self.broadcast_layout();
    }

    /// `from` 칸(없으면 초점 칸)을 나눈다. `right` 가 없으면 넓은 쪽으로. 새 칸은 `from` 을 부모로 안다.
    fn split_from(&mut self, from: Option<String>, right: Option<bool>, focus: bool) -> anyhow::Result<String> {
        let target = from
            .clone()
            .filter(|f| self.tabs.iter().any(|t| t.layout.leaves().contains(&f.as_str())))
            .or_else(|| self.tabs.get(self.active).map(|t| t.focus.clone()))
            .context("나눌 칸이 없다")?;
        let ti = self.tabs.iter().position(|t| t.layout.leaves().contains(&target.as_str())).context("칸이 탭에 없다")?;
        let right = right.unwrap_or_else(|| {
            let (cols, rows) = self.pane_area();
            self.tabs[ti]
                .layout
                .leaf_rects(cols, rows)
                .into_iter()
                .find(|(id, ..)| *id == target)
                // 글자 칸은 세로로 약 두 배 길다 — 폭이 높이의 두 배를 넘으면 옆으로.
                .map(|(_, _, _, w, h)| w as u32 >= h as u32 * 2)
                .unwrap_or(true)
        });
        let cwd = self.panes.get(&target).and_then(pane_cwd);
        let new_id = self.spawn_pane(cwd)?;
        let dir = if right { SplitDir::Horizontal } else { SplitDir::Vertical };
        let tab = &mut self.tabs[ti];
        tab.layout.split_leaf(&target, dir, new_id.clone());
        tab.zoomed = false;
        if focus {
            tab.focus = new_id.clone();
            self.active = ti;
        }
        if let Some(parent) = from {
            if let Some(m) = self.shared.lock().unwrap().panes.get_mut(&new_id) {
                m.parent = Some(parent);
            }
        }
        Ok(new_id)
    }

    fn send_full(&self, id: u64, pane: &str) {
        if let Some(p) = self.panes.get(pane) {
            let frame = to_frame(&p.session.full_snapshot(), p.modes, p.scrolled);
            self.send(id, &ServerMsg::Frame(frame));
        }
    }

    fn on_screen(&mut self, pane: &str, update: ScreenUpdate) {
        let Some(p) = self.panes.get_mut(pane) else { return };
        p.modes = Modes {
            app_cursor: update.app_cursor,
            bracketed_paste: update.bracketed_paste,
            mouse: update.mouse_enabled,
            mouse_sgr: update.mouse_sgr,
            mouse_motion: update.mouse_motion,
        };
        if update.title.is_some() {
            p.title = update.title.clone();
        }
        p.scrolled = p.session.view_state().0 as u32;
        let frame = to_frame(&update, p.modes, p.scrolled);
        self.broadcast(&ServerMsg::Frame(frame));
    }
}

/// 협업 창구를 세운다: 칸 shim(훅·claude·kasaterm-cli), 제어 소켓 서버, 판 수집기, tell 전달.
/// 돌려준 것을 쥐고 있어야 수집기가 돈다.
fn start_collab(
    name: &str,
    shared: Arc<std::sync::Mutex<Shared>>,
    ev_tx: Sender<Ev>,
) -> Result<(Arc<dyn kasa_socket::Backend>, Option<kasa_collab::board_service::CollectorGuard>)> {
    let shim = paths::runtime_dir().join(format!("{name}-shim"));
    if let Err(e) = collab::install_shims(&shim) {
        eprintln!("[kasa] 칸 shim 을 못 깔았다(협업 훅 없이 간다): {e:#}");
    } else {
        std::env::set_var("KASATERM_TMUX_SHIM_DIR", &shim);
    }
    let ctl_path = paths::ctl_socket_path(name);
    #[cfg(unix)]
    let _ = std::fs::remove_file(&ctl_path);
    std::env::set_var("KASATERM_SOCKET_PATH", &ctl_path);
    kasa_collab::tell_service::set_storage_root(paths::runtime_dir().join(format!("{name}-collab")));
    let (ctl_tx, ctl_rx) = unbounded::<Ctl>();
    std::thread::Builder::new().name("kasa-ctl".into()).spawn(move || {
        while let Ok(c) = ctl_rx.recv() {
            if ev_tx.send(Ev::Ctl(c)).is_err() {
                return;
            }
        }
    })?;
    let backend: Arc<dyn kasa_socket::Backend> = Arc::new(TuiBackend::new(shared, ctl_tx));
    kasa_socket::server::Server::bind(&ctl_path)
        .with_context(|| format!("제어 소켓 {}", ctl_path.display()))?
        .spawn(backend.clone());
    let label = kasa_collab::env::machine_label();
    let guard = match kasa_collab::board_service::local_id() {
        Ok(machine_id) => kasa_collab::board_service::register_with_config(
            backend.clone(),
            kasa_collab::board_service::CollectorConfig {
                machine_id,
                label,
                journal_path: Some(paths::runtime_dir().join(format!("{name}.board.json"))),
                remote_enabled: false,
                source_override: None,
                machines: None,
            },
        )
        .map_err(|e| eprintln!("[kasa] 판 수집기를 못 세웠다: {e:#}"))
        .ok(),
        Err(e) => {
            eprintln!("[kasa] 기계 id 를 못 읽었다(판 없이 간다): {e:#}");
            None
        }
    };
    kasa_collab::delivery::spawn(Arc::downgrade(&backend))?;
    Ok((backend, guard))
}

fn pane_cwd(p: &Pane) -> Option<String> {
    p.session
        .reported_cwd()
        .or_else(|| p.session.shell_pid().and_then(crate::procinfo::process_cwd))
        .map(|c| c.to_string_lossy().into_owned())
}

fn wcolor(c: &Color) -> WColor {
    match c {
        Color::Default => WColor::Default,
        Color::Idx(i) => WColor::Idx(*i),
        Color::Rgb(r, g, b) => WColor::Rgb(*r, *g, *b),
    }
}

/// 엔진 셀 한 줄을 선 낱말로. 넓은 글자 뒤 칸은 엔진이 공백으로 넘기므로(문자로는 진짜
/// 공백과 구분이 안 된다) 앞 글자의 폭으로 가려 `SPACER` 를 붙인다.
fn wire_row(row: &[Cell]) -> Vec<WCell> {
    let mut out = Vec::with_capacity(row.len());
    let mut spacer_next = false;
    for c in row {
        let mut attrs = 0u8;
        if c.bold {
            attrs |= proto::BOLD;
        }
        if c.italic {
            attrs |= proto::ITALIC;
        }
        if c.underline {
            attrs |= proto::UNDERLINE;
        }
        if c.inverse {
            attrs |= proto::INVERSE;
        }
        if c.dim {
            attrs |= proto::DIM;
        }
        if c.hidden {
            attrs |= proto::HIDDEN;
        }
        if spacer_next || c.leading_wide_spacer {
            attrs |= proto::SPACER;
        }
        spacer_next = !spacer_next && UnicodeWidthChar::width(c.ch).unwrap_or(1) > 1;
        let ch = if c.ch == '\0' { ' ' } else { c.ch };
        out.push(WCell { ch, fg: wcolor(&c.fg), bg: wcolor(&c.bg), attrs });
    }
    out
}

fn to_frame(u: &ScreenUpdate, modes: Modes, scrolled: u32) -> PaneFrame {
    PaneFrame {
        pane: u.pane_id.clone(),
        cols: u.cols,
        rows: u.rows,
        dirty: u.dirty.iter().map(|(i, row)| (*i, wire_row(row))).collect(),
        cursor: (u.cursor_row, u.cursor_col),
        cursor_visible: u.cursor_visible,
        modes,
        title: u.title.clone(),
        scrolled,
        images: u
            .inline_images
            .iter()
            .map(|v| proto::ImageView {
                id: v.id,
                path: v.path.clone(),
                row: v.row,
                col: v.col,
                cols: v.cols,
                rows: v.rows,
                clip: v.clip.as_ref().map(|c| (c.row, c.col, c.cols, c.rows)),
            })
            .collect(),
    }
}

/// `kasa ls` — 표식이 남은 세션마다 붙어 묻는다. 답이 없으면 죽은 표식이라 치운다.
pub fn list() -> Result<i32> {
    let mut any = false;
    for name in paths::known_sessions() {
        match query(&name) {
            Some(info) => {
                any = true;
                let since = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs().saturating_sub(info.created_unix))
                    .unwrap_or(0);
                println!(
                    "{}: 탭 {} · 칸 {} · 붙은 창 {} · {} 전부터 (pid {})",
                    info.name,
                    info.tabs,
                    info.panes,
                    info.clients,
                    human_age(since),
                    info.pid
                );
            }
            None => {
                let _ = std::fs::remove_file(paths::marker_path(&name));
                #[cfg(unix)]
                let _ = std::fs::remove_file(paths::socket_path(&name));
            }
        }
    }
    if !any {
        println!("떠 있는 세션이 없다 — `kasa tui` 로 연다");
    }
    Ok(0)
}

fn human_age(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}초"),
        60..=3599 => format!("{}분", secs / 60),
        3600..=86399 => format!("{}시간", secs / 3600),
        _ => format!("{}일", secs / 86400),
    }
}

pub fn query(name: &str) -> Option<SessionInfo> {
    let mut s = LocalStream::connect(&paths::socket_path(name)).ok()?;
    proto::write_msg(&mut s, &ClientMsg::Query).ok()?;
    loop {
        match proto::read_msg::<ServerMsg, _>(&mut s).ok()?? {
            ServerMsg::Info(info) => return Some(info),
            _ => continue,
        }
    }
}

pub fn kill(name: &str) -> Result<i32> {
    let mut s = LocalStream::connect(&paths::socket_path(name)).with_context(|| format!("세션 {name} 이(가) 없다"))?;
    proto::write_msg(&mut s, &ClientMsg::Command(Command::KillServer))?;
    println!("세션 {name} 을(를) 끝냈다");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(ch: char) -> Cell {
        Cell {
            ch,
            fg: Color::Default,
            bg: Color::Default,
            bold: false,
            italic: false,
            underline: false,
            inverse: false,
            dim: false,
            hidden: false,
            wrapped: false,
            leading_wide_spacer: false,
        }
    }

    #[test]
    fn spots_kasaterm_shim_dirs() {
        assert!(is_kasaterm_shim(std::path::Path::new("/var/folders/x/T/kasaterm-shim-39679")));
        assert!(is_kasaterm_shim(std::path::Path::new("/tmp/kasaterm-lite-shim-12")));
        assert!(!is_kasaterm_shim(std::path::Path::new("/Users/kasa/.local/bin")));
    }

    #[test]
    fn wide_glyph_marks_the_next_cell_as_spacer() {
        let row = wire_row(&[cell('한'), cell(' '), cell(' '), cell('a'), cell('글'), cell(' ')]);
        let spacer: Vec<bool> = row.iter().map(|c| c.attrs & proto::SPACER != 0).collect();
        assert_eq!(spacer, [false, true, false, false, false, true]);
    }
}
