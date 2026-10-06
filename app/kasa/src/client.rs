//! 클라이언트 — 바깥 터미널을 날 모드로 잡고, 서버가 보낸 칸 화면을 그리고, 입력을 넘긴다.
//! 접두키·마우스 경계선·선택은 여기서 처리하고, 레이아웃을 바꾸는 일은 서버에 의도로 보낸다.

use std::io::{Stdout, Write};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};
use kasa_socket::transport::LocalStream;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
    EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    MouseButton, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::{execute, terminal};
use ratatui::layout::Rect;
use ratatui::Terminal;

use crate::chrome::{self, Action, Glyphs, Heads, Hit, HitBox, Painter, Pointer, StatusLine};
use crate::config::{self, Config};
use crate::graphics::{self, Graphics};
use crate::keys::{self, Button, Key, KeyInput, MouseKind};
use crate::paths;
use crate::proto::{self, ClientMsg, Command, Dir, LayoutMsg, ServerMsg};
use crate::render::{self, Grids, Selection};

enum Ev {
    Server(ServerMsg),
    ServerGone,
    Input(Event),
}

/// 접두키 뒤의 상태 줄 입력.
enum Prompt {
    RenameTab(String),
    /// 이 칸을 닫을지 묻는 중.
    ConfirmClose(String),
}

enum Drag {
    Divider { path: Vec<u8>, vertical_line: bool },
    Select,
    /// 마우스를 켠 칸에 넘기는 끌기 — 칸 밖으로 나가도 그 칸에 묶는다.
    Pane(String),
}

struct Ui {
    cfg: Config,
    tx: Sender<Vec<u8>>,
    layout: LayoutMsg,
    grids: Grids,
    prefix_armed: bool,
    prompt: Option<Prompt>,
    help: bool,
    scroll_mode: bool,
    selection: Option<Selection>,
    drag: Option<Drag>,
    /// 지난 프레임에 그린 누를 자리(칸 머리 줄·상태 줄).
    hits: Vec<HitBox>,
    hover: Option<Hit>,
    /// 누른 채 아직 안 뗀 단추 — 같은 단추 위에서 떼야 일한다.
    pressed: Option<Hit>,
    glyphs: Glyphs,
    message: Option<(String, Instant)>,
    /// 다음에 그릴 때 바깥 터미널로 낼 OSC 52.
    clipboard_out: Option<String>,
    graphics: Graphics,
    cell_px: Option<(u16, u16)>,
}

pub fn run(session: &str, create: bool) -> Result<i32> {
    if !paths::valid_session_name(session) {
        bail!("세션 이름은 영문·숫자·-·_ 32자까지다: {session}");
    }
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let stream = match LocalStream::connect(&paths::socket_path(session)) {
        Ok(s) => s,
        Err(_) if create => {
            spawn_server(session, (cols, rows))?;
            LocalStream::connect(&paths::socket_path(session)).context("새 세션 서버에 붙지 못했다")?
        }
        Err(_) => bail!("세션 {session} 이(가) 없다 — `kasa ls` 로 보거나 `kasa tui -s {session}` 로 연다"),
    };
    let mut writer = stream.try_clone()?;
    let mut reader = stream;
    let (guard, probe) = TermGuard::enter()?;
    let cell_px = probe.cell_px.or_else(|| cell_px_from_ioctl(cols, rows));
    let cfg = Config::load();
    let hello = ClientMsg::Hello { protocol: proto::PROTOCOL, cols, rows, cell_px, heads: cfg.buttons };
    proto::write_msg(&mut writer, &hello)?;

    let (ev_tx, ev_rx) = unbounded::<Ev>();
    let (out_tx, out_rx) = unbounded::<Vec<u8>>();
    {
        let ev = ev_tx.clone();
        std::thread::Builder::new().name("kasa-sock-read".into()).spawn(move || {
            while let Ok(Some(msg)) = proto::read_msg::<ServerMsg, _>(&mut reader) {
                if ev.send(Ev::Server(msg)).is_err() {
                    return;
                }
            }
            let _ = ev.send(Ev::ServerGone);
        })?;
        std::thread::Builder::new().name("kasa-sock-write".into()).spawn(move || {
            while let Ok(buf) = out_rx.recv() {
                if writer.write_all(&buf).and_then(|_| writer.flush()).is_err() {
                    break;
                }
            }
        })?;
    }

    // 입력 스레드보다 먼저 만든다 — 터미널을 만들며 커서 위치를 묻는데, 그 답을 입력 스레드가
    // 먼저 읽어 가면 질의가 시간 초과로 실패한다.
    let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    term.clear()?;
    {
        let ev = ev_tx.clone();
        std::thread::Builder::new().name("kasa-input".into()).spawn(move || loop {
            match event::read() {
                Ok(e) => {
                    if ev.send(Ev::Input(e)).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        })?;
    }
    let glyphs = Glyphs::detect(cfg.sextants);
    let mut ui = Ui {
        cfg,
        tx: out_tx,
        layout: LayoutMsg { session: session.to_string(), ..LayoutMsg::default() },
        grids: Grids::new(),
        prefix_armed: false,
        prompt: None,
        help: false,
        scroll_mode: false,
        selection: None,
        drag: None,
        hits: Vec::new(),
        hover: None,
        pressed: None,
        glyphs,
        message: None,
        clipboard_out: None,
        graphics: Graphics::new(if probe.kitty { graphics::Mode::Kitty } else { graphics::Mode::Text }),
        cell_px,
    };
    let outcome = ui.event_loop(&mut term, &ev_rx);
    ui.graphics.clear_all(&mut std::io::stdout());
    drop(term);
    guard.leave();
    match outcome {
        Ok(Exit::Detached) => {
            println!("[세션 {session} 에서 떨어졌다 — 다시 붙기: kasa attach {session}]");
            Ok(0)
        }
        Ok(Exit::Bye(msg)) => {
            println!("[{msg}]");
            Ok(0)
        }
        Ok(Exit::Lost) => {
            println!("[세션 서버와 끊겼다]");
            Ok(1)
        }
        Err(e) => Err(e),
    }
}

enum Exit {
    Detached,
    Bye(String),
    Lost,
}

/// 바깥 터미널 모드를 잡고, 끝날 때(패닉 포함) 되돌린다.
struct TermGuard {
    kitty_keys: bool,
}

impl TermGuard {
    fn enter() -> Result<(Self, graphics::Probe)> {
        terminal::enable_raw_mode()?;
        // crossterm 이 입력을 읽기 시작하기 전에 묻는다 — 답을 crossterm 이 먹으면 못 받는다.
        let probe = graphics::probe();
        // kitty 키보드 프로토콜을 아는 바깥 터미널이면 켠다 — 그래야 Shift+Enter 가 Enter 와
        // 갈려 들어와 claude 의 줄바꿈으로 넘길 수 있다.
        let kitty_keys = terminal::supports_keyboard_enhancement().unwrap_or(false);
        let mut out = std::io::stdout();
        execute!(out, terminal::EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste, EnableFocusChange)?;
        if kitty_keys {
            execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
        }
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal(kitty_keys);
            prev(info);
        }));
        Ok((Self { kitty_keys }, probe))
    }

    fn leave(self) {
        restore_terminal(self.kitty_keys);
    }
}

fn restore_terminal(kitty_keys: bool) {
    let mut out = std::io::stdout();
    if kitty_keys {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        out,
        DisableFocusChange,
        DisableBracketedPaste,
        DisableMouseCapture,
        terminal::LeaveAlternateScreen,
        ratatui::crossterm::cursor::Show
    );
    let _ = terminal::disable_raw_mode();
}

/// 서버를 이 터미널과 떨어진 프로세스로 띄우고 소켓이 설 때까지 기다린다.
fn spawn_server(session: &str, size: (u16, u16)) -> Result<()> {
    let dir = paths::ensure_runtime_dir()?;
    let exe = std::env::current_exe()?;
    let log = std::fs::File::create(dir.join(format!("{session}.log")))?;
    let cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned());
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("server").arg("-s").arg(session).arg("--size").arg(format!("{}x{}", size.0, size.1));
    if let Some(cwd) = &cwd {
        cmd.arg("--cwd").arg(cwd);
    }
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 새 세션으로 떼어 낸다 — 바깥 터미널이 닫혀 SIGHUP 이 와도 서버와 칸은 산다.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS};
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    cmd.spawn().context("세션 서버를 띄우지 못했다")?;
    let sock = paths::socket_path(session);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if LocalStream::connect(&sock).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    bail!("세션 서버가 5초 안에 서지 않았다 — 기록: {}", dir.join(format!("{session}.log")).display())
}

/// 유닉스에서는 `TIOCGWINSZ` 가 창 픽셀을 함께 준다. 모르는 터미널은 0 을 준다.
fn cell_px_from_ioctl(cols: u16, rows: u16) -> Option<(u16, u16)> {
    let ws = terminal::window_size().ok()?;
    (ws.width > 0 && ws.height > 0 && cols > 0 && rows > 0).then(|| (ws.width / cols, ws.height / rows))
}

fn key_input(k: &KeyEvent) -> Option<KeyInput> {
    let mut mods = 0u8;
    if k.modifiers.contains(KeyModifiers::SHIFT) {
        mods |= keys::SHIFT;
    }
    if k.modifiers.contains(KeyModifiers::ALT) {
        mods |= keys::ALT;
    }
    if k.modifiers.contains(KeyModifiers::CONTROL) {
        mods |= keys::CTRL;
    }
    if k.modifiers.contains(KeyModifiers::SUPER) {
        mods |= keys::SUPER;
    }
    let key = match k.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Esc => Key::Esc,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Insert => Key::Insert,
        KeyCode::Delete => Key::Delete,
        KeyCode::F(n) => Key::F(n),
        _ => return None,
    };
    Some(KeyInput { key, mods })
}

fn is_prefix(k: &KeyInput, prefix: &KeyInput) -> bool {
    let norm = |key: Key| match key {
        Key::Char(c) => Key::Char(c.to_ascii_lowercase()),
        other => other,
    };
    norm(k.key) == norm(prefix.key) && (k.mods & !keys::SHIFT) == prefix.mods
}

fn osc52(text: &str) -> String {
    use base64::Engine;
    format!("\x1b]52;c;{}\x07", base64::engine::general_purpose::STANDARD.encode(text))
}

impl Ui {
    fn send(&self, msg: ClientMsg) {
        if let Ok(buf) = proto::encode(&msg) {
            let _ = self.tx.send(buf);
        }
    }

    fn command(&self, cmd: Command) {
        self.send(ClientMsg::Command(cmd));
    }

    fn flash(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now()));
    }

    fn event_loop(&mut self, term: &mut Terminal<CrosstermBackend<Stdout>>, rx: &Receiver<Ev>) -> Result<Exit> {
        loop {
            let first = match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(ev) => Some(ev),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                Err(_) => return Ok(Exit::Lost),
            };
            // 쌓인 사건을 다 먹고 한 번만 그린다 — 출력 폭주 때 프레임마다 그리면 밀린다.
            let mut batch: Vec<Ev> = first.into_iter().collect();
            while batch.len() < 4096 {
                match rx.try_recv() {
                    Ok(ev) => batch.push(ev),
                    Err(_) => break,
                }
            }
            for ev in batch {
                if let Some(exit) = self.handle(ev) {
                    return Ok(exit);
                }
            }
            if self.message.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(3)) {
                self.message = None;
            }
            self.draw(term)?;
        }
    }

    fn handle(&mut self, ev: Ev) -> Option<Exit> {
        match ev {
            Ev::ServerGone => return Some(Exit::Lost),
            Ev::Server(msg) => match msg {
                ServerMsg::Layout(l) => {
                    if self.selection.as_ref().is_some_and(|s| !l.panes.iter().any(|p| p.id == s.pane)) {
                        self.selection = None;
                    }
                    if matches!(&self.prompt, Some(Prompt::ConfirmClose(id)) if !l.tab_panes.iter().any(|(p, _)| p == id)) {
                        self.prompt = None;
                    }
                    // 확대 중에 가려진 칸도 남긴다 — 서버는 되돌릴 때 그 칸을 다시 보내지 않는다.
                    self.grids.retain(|id, _| l.tab_panes.iter().any(|(p, _)| p == id) || l.tabs.len() > 1);
                    self.layout = l;
                }
                ServerMsg::Frame(f) => {
                    self.grids.entry(f.pane.clone()).or_default().apply(f);
                }
                ServerMsg::Clipboard(text) => self.clipboard_out = Some(text),
                ServerMsg::Info(_) => {}
                ServerMsg::Bye(msg) => return Some(Exit::Bye(msg)),
            },
            Ev::Input(e) => return self.on_input(e),
        }
        None
    }

    fn on_input(&mut self, e: Event) -> Option<Exit> {
        match e {
            Event::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => return self.on_key(&k),
            Event::Key(_) => {}
            Event::Paste(text) => {
                if let Some(Prompt::RenameTab(buf)) = &mut self.prompt {
                    buf.push_str(text.lines().next().unwrap_or_default());
                } else {
                    self.send(ClientMsg::Paste(text));
                }
            }
            Event::Mouse(m) => self.on_mouse(m),
            Event::Resize(cols, rows) => {
                // 글자 크기를 바꾸면(⌘+) 칸 수와 함께 칸 픽셀도 바뀐다.
                if let Some(px) = cell_px_from_ioctl(cols, rows) {
                    self.cell_px = Some(px);
                }
                self.send(ClientMsg::Resize { cols, rows, cell_px: self.cell_px });
            }
            Event::FocusGained | Event::FocusLost => {}
        }
        None
    }

    fn focused_rows(&self) -> u16 {
        self.layout.panes.iter().find(|p| p.id == self.layout.focus).map(|p| p.h).unwrap_or(24)
    }

    fn on_key(&mut self, k: &KeyEvent) -> Option<Exit> {
        let Some(input) = key_input(k) else { return None };
        if self.help {
            self.help = false;
            return None;
        }
        if let Some(prompt) = self.prompt.take() {
            match prompt {
                Prompt::ConfirmClose(id) => {
                    if matches!(input.key, Key::Char('y') | Key::Char('Y')) {
                        self.close_pane(id);
                    }
                }
                Prompt::RenameTab(mut buf) => match input.key {
                    Key::Enter => self.command(Command::RenameTab(buf)),
                    Key::Esc => {}
                    Key::Backspace => {
                        buf.pop();
                        self.prompt = Some(Prompt::RenameTab(buf));
                    }
                    Key::Char(c) if input.mods & keys::CTRL == 0 => {
                        buf.push(c);
                        self.prompt = Some(Prompt::RenameTab(buf));
                    }
                    _ => self.prompt = Some(Prompt::RenameTab(buf)),
                },
            }
            return None;
        }
        if self.scroll_mode {
            let page = (self.focused_rows() / 2).max(1) as i32;
            let pane = self.layout.focus.clone();
            let lines = match input.key {
                Key::Up | Key::Char('k') => 1,
                Key::Down | Key::Char('j') => -1,
                Key::PageUp | Key::Char('b') => page,
                Key::PageDown | Key::Char('f') | Key::Char(' ') => -page,
                Key::Char('u') if input.mods & keys::CTRL != 0 => page,
                Key::Char('d') if input.mods & keys::CTRL != 0 => -page,
                Key::Home | Key::Char('g') => 1_000_000,
                Key::End | Key::Char('G') | Key::Char('q') | Key::Esc => {
                    self.scroll_mode = false;
                    0
                }
                _ => return None,
            };
            self.send(ClientMsg::Scroll { pane, lines });
            return None;
        }
        if self.prefix_armed {
            self.prefix_armed = false;
            return self.prefix_command(input);
        }
        if self.cfg.prefix.is_some_and(|p| is_prefix(&input, &p)) {
            self.prefix_armed = true;
            return None;
        }
        self.selection = None;
        self.send(ClientMsg::Key(input));
        None
    }

    fn prefix_command(&mut self, input: KeyInput) -> Option<Exit> {
        if self.cfg.prefix.is_some_and(|p| is_prefix(&input, &p)) {
            self.send(ClientMsg::Key(input));
            return None;
        }
        let Key::Char(c) = input.key else {
            match input.key {
                Key::Left => self.command(Command::FocusDir(Dir::Left)),
                Key::Right => self.command(Command::FocusDir(Dir::Right)),
                Key::Up => self.command(Command::FocusDir(Dir::Up)),
                Key::Down => self.command(Command::FocusDir(Dir::Down)),
                Key::PageUp => {
                    self.scroll_mode = true;
                    let rows = self.focused_rows() as i32;
                    self.send(ClientMsg::Scroll { pane: self.layout.focus.clone(), lines: rows / 2 });
                }
                _ => {}
            }
            return None;
        };
        match c {
            '%' | '|' | '\\' => self.command(Command::Split { right: true }),
            '"' | '-' => self.command(Command::Split { right: false }),
            'h' => self.command(Command::FocusDir(Dir::Left)),
            'l' => self.command(Command::FocusDir(Dir::Right)),
            'k' => self.command(Command::FocusDir(Dir::Up)),
            'j' => self.command(Command::FocusDir(Dir::Down)),
            'o' => {
                if let Some(i) = self.layout.panes.iter().position(|p| p.id == self.layout.focus) {
                    let next = self.layout.panes[(i + 1) % self.layout.panes.len()].id.clone();
                    self.command(Command::Focus(next));
                }
            }
            'z' => self.command(Command::Zoom),
            'x' => self.prompt = Some(Prompt::ConfirmClose(self.layout.focus.clone())),
            'c' => self.command(Command::NewTab),
            'n' => self.command(Command::NextTab),
            'p' => self.command(Command::PrevTab),
            '0'..='9' => self.command(Command::SelectTab(c as usize - '0' as usize)),
            ',' => self.prompt = Some(Prompt::RenameTab(String::new())),
            '[' => self.scroll_mode = true,
            'd' => {
                self.send(ClientMsg::Detach);
                return Some(Exit::Detached);
            }
            '?' => self.help = true,
            _ => {
                let label = self.cfg.prefix.as_ref().map(config::key_label).unwrap_or_default();
                self.flash(format!("모르는 명령 {label}-{c}"));
            }
        }
        None
    }

    fn pane_at(&self, x: u16, y: u16) -> Option<&proto::PaneRect> {
        self.layout.panes.iter().find(|p| x >= p.x && x < p.x + p.w && y >= p.y && y < p.y + p.h)
    }

    fn divider_at(&self, x: u16, y: u16) -> Option<&proto::DividerMsg> {
        self.layout.dividers.iter().find(|d| {
            if d.vertical_line {
                x == d.at && y >= d.start && y < d.start + d.len
            } else {
                y == d.at && x >= d.start && x < d.start + d.len
            }
        })
    }

    fn mouse_mods(m: &MouseEvent) -> u8 {
        let mut mods = 0;
        if m.modifiers.contains(KeyModifiers::SHIFT) {
            mods |= keys::SHIFT;
        }
        if m.modifiers.contains(KeyModifiers::ALT) {
            mods |= keys::ALT;
        }
        if m.modifiers.contains(KeyModifiers::CONTROL) {
            mods |= keys::CTRL;
        }
        mods
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        let mods = Self::mouse_mods(&m);
        let button = |b: MouseButton| match b {
            MouseButton::Left => Button::Left,
            MouseButton::Right => Button::Right,
            MouseButton::Middle => Button::Middle,
        };
        let status_row = terminal::size().map(|(_, r)| r.saturating_sub(1)).unwrap_or(u16::MAX);
        if matches!(m.kind, MouseEventKind::Moved | MouseEventKind::Drag(_)) {
            self.hover = chrome::hit_at(&self.hits, x, y).cloned();
        }
        match m.kind {
            MouseEventKind::Down(b) => {
                self.selection = None;
                if self.help {
                    self.help = false;
                    return;
                }
                if let Some(hit) = chrome::hit_at(&self.hits, x, y).cloned() {
                    if b == MouseButton::Left {
                        self.hover = Some(hit.clone());
                        if hit.is_button() {
                            self.pressed = Some(hit);
                        } else {
                            self.fire(hit);
                        }
                    }
                    return;
                }
                if y == status_row {
                    return;
                }
                if b == MouseButton::Left {
                    if let Some(d) = self.divider_at(x, y) {
                        self.drag = Some(Drag::Divider { path: d.path.clone(), vertical_line: d.vertical_line });
                        return;
                    }
                }
                let Some(p) = self.pane_at(x, y).cloned() else { return };
                if p.id != self.layout.focus {
                    self.command(Command::Focus(p.id.clone()));
                }
                let (col, row) = (x - p.x, y - p.y);
                if self.graphics.mode == graphics::Mode::Text && b == MouseButton::Left {
                    let hit = self.grids.get(&p.id).and_then(|g| {
                        g.images.iter().find(|v| {
                            (row as i32) >= v.row
                                && (row as i32) < v.row + v.rows as i32
                                && col >= v.col
                                && col < v.col + v.cols
                        })
                    });
                    if let Some(v) = hit {
                        graphics::open_externally(&v.path);
                        return;
                    }
                }
                // Shift 를 누르면 마우스를 켠 칸에서도 선택한다(다른 터미널들과 같은 약속).
                let to_pane = self.grids.get(&p.id).is_some_and(|g| g.modes.mouse) && mods & keys::SHIFT == 0;
                if to_pane {
                    self.send(ClientMsg::Mouse { pane: p.id.clone(), kind: MouseKind::Down(button(b)), col, row, mods });
                    self.drag = Some(Drag::Pane(p.id));
                } else if b == MouseButton::Left {
                    self.selection = Some(Selection { pane: p.id, anchor: (col, row), head: (col, row) });
                    self.drag = Some(Drag::Select);
                }
            }
            MouseEventKind::Drag(b) => match &self.drag {
                Some(Drag::Divider { path, vertical_line }) => {
                    let pos = if *vertical_line { x } else { y };
                    self.command(Command::MoveDivider { path: path.clone(), pos });
                }
                Some(Drag::Select) => {
                    if let Some(sel) = &mut self.selection {
                        if let Some(p) = self.layout.panes.iter().find(|p| p.id == sel.pane) {
                            let col = x.clamp(p.x, p.x + p.w - 1) - p.x;
                            let row = y.clamp(p.y, p.y + p.h - 1) - p.y;
                            sel.head = (col, row);
                        }
                    }
                }
                Some(Drag::Pane(id)) => {
                    if let Some(p) = self.layout.panes.iter().find(|p| &p.id == id) {
                        let col = x.clamp(p.x, p.x + p.w - 1) - p.x;
                        let row = y.clamp(p.y, p.y + p.h - 1) - p.y;
                        self.send(ClientMsg::Mouse { pane: id.clone(), kind: MouseKind::Drag(button(b)), col, row, mods });
                    }
                }
                None => {}
            },
            MouseEventKind::Up(_) if self.pressed.is_some() => {
                let pressed = self.pressed.take();
                if chrome::hit_at(&self.hits, x, y) == pressed.as_ref() {
                    if let Some(hit) = pressed {
                        self.fire(hit);
                    }
                }
            }
            MouseEventKind::Up(b) => match self.drag.take() {
                Some(Drag::Select) => {
                    let text = self.selection.as_ref().and_then(|s| {
                        (s.anchor != s.head).then(|| self.grids.get(&s.pane).map(|g| g.text_between(s.anchor, s.head)))?
                    });
                    match text.filter(|t| !t.is_empty()) {
                        Some(t) => {
                            let n = t.chars().count();
                            self.clipboard_out = Some(t);
                            self.flash(format!("{n}자 복사"));
                        }
                        None => self.selection = None,
                    }
                }
                Some(Drag::Pane(id)) => {
                    if let Some(p) = self.layout.panes.iter().find(|p| p.id == id) {
                        let col = x.clamp(p.x, p.x + p.w - 1) - p.x;
                        let row = y.clamp(p.y, p.y + p.h - 1) - p.y;
                        self.send(ClientMsg::Mouse { pane: id, kind: MouseKind::Up(button(b)), col, row, mods });
                    }
                }
                _ => {}
            },
            MouseEventKind::Moved => {
                if let Some(p) = self.pane_at(x, y) {
                    if self.grids.get(&p.id).is_some_and(|g| g.modes.mouse_motion) {
                        let msg = ClientMsg::Mouse { pane: p.id.clone(), kind: MouseKind::Moved, col: x - p.x, row: y - p.y, mods };
                        self.send(msg);
                    }
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown | MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
                let Some(p) = self.pane_at(x, y).cloned() else { return };
                let kind = match m.kind {
                    MouseEventKind::ScrollUp => MouseKind::WheelUp,
                    MouseEventKind::ScrollDown => MouseKind::WheelDown,
                    MouseEventKind::ScrollLeft => MouseKind::WheelLeft,
                    _ => MouseKind::WheelRight,
                };
                let grid = self.grids.get(&p.id);
                if grid.is_some_and(|g| g.modes.mouse) && !self.scroll_mode {
                    self.send(ClientMsg::Mouse { pane: p.id, kind, col: x - p.x, row: y - p.y, mods });
                } else if matches!(kind, MouseKind::WheelUp | MouseKind::WheelDown) {
                    let lines = if kind == MouseKind::WheelUp { 3 } else { -3 };
                    if grid.is_some_and(|g| g.scrolled == 0) && lines < 0 {
                        return;
                    }
                    self.send(ClientMsg::Scroll { pane: p.id, lines });
                }
            }
        }
    }

    /// 단추를 뗐을 때(머리 줄은 누를 때) 할 일. 다른 칸의 단추면 그 칸으로 초점을 옮긴 뒤 한다 —
    /// 서버는 한 연결의 메시지를 차례대로 처리하니 초점이 먼저 선다.
    fn fire(&mut self, hit: Hit) {
        match hit {
            Hit::Tab(i) => self.command(Command::SelectTab(i)),
            Hit::NewTab => self.command(Command::NewTab),
            Hit::Help => self.help = true,
            Hit::Head(id) | Hit::Chip(id) => {
                if id != self.layout.focus {
                    self.command(Command::Focus(id));
                }
            }
            Hit::Act(target, action) => {
                let id = target.unwrap_or_else(|| self.layout.focus.clone());
                if action == Action::Close {
                    if matches!(&self.prompt, Some(Prompt::ConfirmClose(p)) if *p == id) {
                        self.prompt = None;
                        self.close_pane(id);
                    } else {
                        self.prompt = Some(Prompt::ConfirmClose(id));
                    }
                    return;
                }
                if id != self.layout.focus {
                    self.command(Command::Focus(id));
                }
                self.command(match action {
                    Action::SplitRight => Command::Split { right: true },
                    Action::SplitDown => Command::Split { right: false },
                    _ => Command::Zoom,
                });
            }
        }
    }

    fn close_pane(&mut self, id: String) {
        if id != self.layout.focus {
            self.command(Command::Focus(id));
        }
        self.command(Command::ClosePane);
    }

    /// 칸을 사람에게 부를 이름(「2 claude」).
    fn pane_label(&self, id: &str) -> String {
        match self.layout.tab_panes.iter().position(|(p, _)| p == id) {
            Some(i) => format!("{} {}", i + 1, self.layout.tab_panes[i].1),
            None => id.to_string(),
        }
    }

    fn draw(&mut self, term: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        if let Some(text) = self.clipboard_out.take() {
            let mut out = std::io::stdout();
            let _ = out.write_all(osc52(&text).as_bytes());
            let _ = out.flush();
        }
        let prefix_label = self.cfg.prefix.as_ref().map(config::key_label);
        let prompt_text = match &self.prompt {
            Some(Prompt::RenameTab(buf)) => Some(format!("탭 이름: {buf}_  (Enter 확인 · Esc 취소)")),
            Some(Prompt::ConfirmClose(id)) => Some(format!("칸 {} 을(를) 닫을까요? y / n · 닫기 한 번 더", self.pane_label(id))),
            None => None,
        };
        let message = prompt_text.or_else(|| self.message.as_ref().map(|(m, _)| m.clone()));
        let scroll = self.scroll_mode.then(|| self.grids.get(&self.layout.focus).map(|g| g.scrolled).unwrap_or(0));
        let mut hits = Vec::new();
        let pointer = Pointer { hover: self.hover.as_ref(), pressed: self.pressed.as_ref() };
        let icon_px = self.cell_px.filter(|_| self.graphics.mode == graphics::Mode::Kitty);
        {
            let visible: Vec<(&str, &proto::ImageView)> = self
                .layout
                .panes
                .iter()
                .filter_map(|p| self.grids.get(&p.id).map(|g| (p, g)))
                .flat_map(|(p, g)| g.images.iter().map(move |v| (p.id.as_str(), v)))
                .collect();
            self.graphics.prepare(&visible, &mut std::io::stdout());
        }
        let graphics = &mut self.graphics;
        term.draw(|f| {
            let area = f.area();
            let panes_area = Rect { height: area.height.saturating_sub(1), ..area };
            let buf = f.buffer_mut();
            for p in &self.layout.panes {
                let grid = self.grids.get(&p.id);
                render::draw_pane(buf, panes_area, p, grid, self.selection.as_ref());
                if let Some(g) = grid.filter(|g| !g.images.is_empty()) {
                    graphics.draw(buf, p, &p.id, &g.images);
                }
            }
            render::draw_borders(buf, area, &self.layout);
            let mut painter = Painter { glyphs: self.glyphs, icons: icon_px.map(|px| (&mut *graphics, px)) };
            if self.cfg.buttons {
                let heads = Heads { layout: &self.layout, grids: &self.grids, pointer };
                hits = chrome::draw_heads(buf, panes_area, &heads, &mut painter);
            }
            hits.extend(chrome::draw_status(
                buf,
                area,
                &StatusLine {
                    layout: &self.layout,
                    buttons: self.cfg.buttons,
                    pointer,
                    prefix_label: prefix_label.as_deref(),
                    prefix_armed: self.prefix_armed,
                    message: message.as_deref(),
                    scroll_mode: scroll,
                },
                &mut painter,
            ));
            if self.help {
                render::draw_help(buf, area, prefix_label.as_deref(), self.cfg.buttons);
            }
            // 진짜 커서를 초점 칸의 커서 자리에 둔다 — 바깥 터미널의 한글 조합 글자가 거기 뜬다.
            let focus = self.layout.panes.iter().find(|p| p.id == self.layout.focus);
            if let (Some(p), Some(g)) = (focus, self.grids.get(&self.layout.focus)) {
                let (row, col) = g.cursor;
                if g.cursor_visible && g.scrolled == 0 && !self.help && self.prompt.is_none() && col < p.w && row < p.h {
                    let (cx, cy) = (p.x + col, p.y + row);
                    if cx < panes_area.width && cy < panes_area.height {
                        f.set_cursor_position((cx, cy));
                    }
                }
            }
        })?;
        self.hits = hits;
        Ok(())
    }
}
