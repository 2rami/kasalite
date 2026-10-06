//! 세션 서버의 협업 창구. 칸마다 `KASATERM_SOCKET_PATH` 를 이 서버의 제어 소켓으로 걸어 칸 안
//! `kasaterm-cli board/tell/done/summon` 이 이 세션으로 온다. 판 행·tell 장부·전달은 kasa-collab
//! 을 그대로 쓰고, 칸·탭을 만지는 일만 서버 이벤트 루프에 맡긴다(레이아웃 권한은 루프 한 곳).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use crossbeam_channel::{bounded, Sender};
use kasa_socket::backend::{
    ActivityEvent, Backend, PaneActivity, SplitDirection, SurfaceInfo, WorkspaceInfo,
};
use serde_json::{json, Value};

/// 루프에 맡기는 칸 조작. 답은 `reply` 로 온다.
pub enum Ctl {
    Split { from: Option<String>, right: Option<bool>, focus: bool, reply: Sender<Result<String>> },
    NewTab { focus: bool, reply: Sender<Result<String>> },
    Focus(String),
    /// 판·탭 이름이 바뀌었으니 클라이언트에 다시 알려라.
    Refresh,
}

/// 칸 하나에 대해 협업이 아는 것. 훅이 알려 오고 판 행이 읽는다.
#[derive(Default, Clone)]
pub struct PaneMeta {
    pub room: String,
    pub visible: bool,
    pub name: Option<String>,
    /// 이 칸을 만든(summon 한) 칸 — done 보고가 그리로 간다.
    pub parent: Option<String>,
    pub transcript: Option<PathBuf>,
    pub phase: Option<String>,
    pub attention: Option<(String, String)>,
    pub tool: Option<String>,
    pub done: Option<(String, String, u64)>,
}

#[derive(Default)]
pub struct Shared {
    pub panes: HashMap<String, PaneMeta>,
    /// 탭 이름(순서대로)과 지금 탭.
    pub tabs: Vec<String>,
    pub active_tab: usize,
    pub focus: String,
}

pub struct TuiBackend {
    pub shared: Arc<Mutex<Shared>>,
    ctl: Sender<Ctl>,
}

impl TuiBackend {
    pub fn new(shared: Arc<Mutex<Shared>>, ctl: Sender<Ctl>) -> Self {
        Self { shared, ctl }
    }

    fn ask(&self, make: impl FnOnce(Sender<Result<String>>) -> Ctl) -> Result<String> {
        let (tx, rx) = bounded(1);
        self.ctl.send(make(tx)).context("세션 서버가 닫혔다")?;
        rx.recv_timeout(Duration::from_secs(10)).context("세션 서버가 답하지 않았다")?
    }

    fn meta(&self, surface: &str) -> PaneMeta {
        self.shared.lock().unwrap().panes.get(surface).cloned().unwrap_or_default()
    }

    fn edit(&self, surface: &str, f: impl FnOnce(&mut PaneMeta)) {
        if let Some(m) = self.shared.lock().unwrap().panes.get_mut(surface) {
            f(m);
        }
        kasa_collab::board_service::poke();
    }

    fn surface_or_focus(&self, surface: Option<&str>) -> String {
        surface.map(str::to_owned).unwrap_or_else(|| self.shared.lock().unwrap().focus.clone())
    }

    fn surface_info(&self, id: &str) -> SurfaceInfo {
        let meta = self.meta(id);
        SurfaceInfo {
            id: id.to_string(),
            workspace_id: meta.room.clone(),
            title: meta.name.clone(),
            cwd: pane_cwd(id),
            character: meta.name,
        }
    }

    /// 칸에 붙은 대화(세션) id — claude 명령줄의 `--session-id`·`--resume` 이 앞서고, 없으면
    /// 시작 훅이 묶어 준 기록 파일 이름이다.
    fn session_of(&self, surface: &str) -> Option<String> {
        let pty = kasa_pty::lookup_session(surface)?;
        let table = kasa_pty::fresh_process_table();
        if let Some((kasa_pty::AgentKind::Claude, pid)) = pty.shell_pid().and_then(|s| kasa_pty::agent_pid_for_shell(&table, s)) {
            if let Some(sid) = kasa_pty::process_cmdline(pid).and_then(|c| session_flag(&c)) {
                return Some(sid);
            }
        }
        let path = self.meta(surface).transcript?;
        path.file_stem().and_then(|s| s.to_str()).filter(|s| is_uuid(s)).map(str::to_owned)
    }

    fn row(&self, id: &str, table: &[(u32, u32, String)]) -> Result<Value> {
        let meta = self.meta(id);
        let pty = kasa_pty::lookup_session(id);
        let harness = pty
            .as_ref()
            .and_then(|p| p.shell_pid())
            .and_then(|pid| kasa_pty::agent_for_shell(table, pid))
            .map(|k| k.as_str().to_owned());
        let activity = meta.transcript.as_deref().map(|p| {
            kasa_agents::transcript::snapshot_from_tail(id, &read_tail(p, 256 * 1024), meta.phase.as_deref() != Some("start"))
        });
        let (status, reason) = match (&meta.attention, meta.phase.as_deref(), &harness) {
            (Some((kind, _)), ..) => ("waiting", format!("attention {kind}")),
            (None, Some("start" | "compact_start"), _) => ("working", "hook turn open".into()),
            (None, Some(_), _) => ("idle", "hook turn closed".into()),
            (None, None, Some(_)) => ("idle", "no turn yet".into()),
            (None, None, None) => ("unknown", "shell".into()),
        };
        let title = meta
            .name
            .clone()
            .or_else(|| activity.as_ref().map(|a| a.title.clone()).filter(|t| !t.is_empty()))
            .or_else(|| pty.as_ref().and_then(|p| p.osc_title()))
            .unwrap_or_default();
        let mut row = json!({
            "address": self.collab_pane_identity(id)?,
            "room_label": meta.room,
            "harness": harness,
            "title": title,
            "request": activity.as_ref().map(|a| a.last_prompt.clone()).unwrap_or_default(),
            "progress": meta.tool.clone().or_else(|| activity.as_ref().map(|a| a.last_reply.clone())).unwrap_or_default(),
            "status": status,
            "status_reason": reason,
            "place_state": if meta.visible { "visible" } else { "background" },
            "detached": false,
        });
        if let Some(name) = &meta.name {
            row["character"] = json!(name);
        }
        if let Some((kind, why)) = &meta.attention {
            row["attention_kind"] = json!(kind);
            row["waiting_for"] = json!(why);
        }
        if let Some((outcome, summary, at)) = &meta.done {
            row["done_outcome"] = json!(outcome);
            row["done_summary"] = json!(summary);
            row["done_at_ms"] = json!(at);
        }
        Ok(row)
    }
}

impl Backend for TuiBackend {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        let s = self.shared.lock().unwrap();
        Ok(s.tabs.iter().map(|t| WorkspaceInfo { id: t.clone(), name: t.clone() }).collect())
    }
    fn current_workspace(&self) -> Result<Option<WorkspaceInfo>> {
        let s = self.shared.lock().unwrap();
        Ok(s.tabs.get(s.active_tab).map(|t| WorkspaceInfo { id: t.clone(), name: t.clone() }))
    }
    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        let ids: Vec<String> = self.shared.lock().unwrap().panes.keys().cloned().collect();
        Ok(ids.iter().map(|id| self.surface_info(id)).collect())
    }
    fn focus_surface(&self, surface_id: &str) -> Result<()> {
        self.ctl.send(Ctl::Focus(surface_id.to_string())).context("세션 서버가 닫혔다")
    }
    fn split_surface(&self, direction: SplitDirection, focus: bool, from: Option<&str>) -> Result<SurfaceInfo> {
        let right = match direction {
            SplitDirection::Left | SplitDirection::Right => Some(true),
            SplitDirection::Up | SplitDirection::Down => Some(false),
            SplitDirection::Auto => None,
        };
        let from = from.map(str::to_owned);
        let id = self.ask(|reply| Ctl::Split { from, right, focus, reply })?;
        Ok(self.surface_info(&id))
    }
    fn new_tab(&self, _outer: Option<&str>, focus: bool) -> Result<SurfaceInfo> {
        let id = self.ask(|reply| Ctl::NewTab { focus, reply })?;
        Ok(self.surface_info(&id))
    }
    fn rename_surface(&self, surface_id: &str, title: &str) -> Result<()> {
        let title = title.trim().to_string();
        self.edit(surface_id, |m| m.name = (!title.is_empty()).then_some(title));
        let _ = self.ctl.send(Ctl::Refresh);
        Ok(())
    }
    fn send_text(&self, surface_id: Option<&str>, text: &str) -> Result<()> {
        let id = self.surface_or_focus(surface_id);
        let pty = kasa_pty::lookup_session(&id).with_context(|| format!("칸 {id} 이(가) 없다"))?;
        // 줄바꿈은 Enter 다 — 터미널의 Enter 는 CR 이고, 엔진도 CR 이 와야 「쓰던 글」 표시를 거둔다.
        pty.send_bytes(text.replace("\r\n", "\r").replace('\n', "\r").as_bytes())
    }
    fn send_key(&self, surface_id: Option<&str>, key: &str) -> Result<()> {
        let id = self.surface_or_focus(surface_id);
        let pty = kasa_pty::lookup_session(&id).with_context(|| format!("칸 {id} 이(가) 없다"))?;
        pty.send_bytes(&key_bytes(key).with_context(|| format!("모르는 키 {key}"))?)
    }
    fn notify(&self, _surface_id: &str, _title: &str, _body: &str) -> Result<()> {
        Ok(())
    }
    fn attention(&self, surface_id: &str, reason: &str) -> Result<()> {
        self.attention_kind(surface_id, "permission", reason)
    }
    fn attention_kind(&self, surface_id: &str, kind: &str, reason: &str) -> Result<()> {
        let (kind, reason) = (kind.to_string(), reason.to_string());
        self.edit(surface_id, |m| m.attention = Some((kind, reason)));
        Ok(())
    }
    fn turn(&self, surface_id: &str, phase: &str, _permission_mode: &str) -> Result<()> {
        let phase = phase.to_string();
        self.edit(surface_id, |m| {
            if phase == "start" {
                m.done = None;
                m.tool = None;
            }
            m.attention = None;
            m.phase = Some(phase);
        });
        Ok(())
    }
    fn agent_status(&self, surface_id: &str, phase: &str, _kind: &str, _key: &str, label: &str) -> Result<()> {
        let label = (phase != "end" && !label.is_empty()).then(|| label.to_string());
        self.edit(surface_id, |m| m.tool = label);
        Ok(())
    }
    fn bind_transcript(&self, surface_id: &str, path: &str) -> Result<()> {
        let path = PathBuf::from(path);
        self.edit(surface_id, |m| m.transcript = Some(path));
        Ok(())
    }
    fn pane_done(&self, surface_id: &str, outcome: &str, summary: &str) -> Result<()> {
        let at = kasa_socket::tell::now_ms();
        let (o, s) = (outcome.to_string(), summary.to_string());
        self.edit(surface_id, |m| m.done = Some((o, s, at)));
        let meta = self.meta(surface_id);
        let Some(parent) = meta.parent else { return Ok(()) };
        let who = meta.name.unwrap_or_else(|| surface_id.to_string());
        let mark = if outcome == "succeeded" { "완료" } else { "실패" };
        let body = if summary.is_empty() {
            format!("[{mark}] {who}({surface_id})")
        } else {
            format!("[{mark}] {who}({surface_id}) — {summary}")
        };
        let params = json!({ "message_id": kasa_socket::tell::new_message_id(), "surface_id": parent, "body": body });
        if let Err(e) = self.collab_tell(&params) {
            eprintln!("[done] {surface_id} → {parent} 보고 전달 못 함: {e:#}");
        }
        Ok(())
    }
    fn collab_board_source(&self) -> Result<Value> {
        let table = kasa_pty::process_table_shared();
        let ids: Vec<String> = self.shared.lock().unwrap().panes.keys().cloned().collect();
        let mut panes = Vec::new();
        for id in ids {
            if kasa_pty::lookup_session(&id).is_some() {
                panes.push(self.row(&id, &table)?);
            }
        }
        let mut source = kasa_collab::board_service::local_source(panes, true)?;
        source["source_kind"] = json!("tui");
        source["capabilities"] = json!(["live_places", "rooms", "transcript_summary", "done_reports"]);
        Ok(source)
    }
    fn collab_snapshot(&self, params: &Value) -> Result<Value> {
        kasa_collab::board_service::snapshot(params)
    }
    fn collab_changes(&self, params: &Value) -> Result<Value> {
        kasa_collab::board_service::changes(params)
    }
    fn collab_inspect(&self, params: &Value) -> Result<Value> {
        kasa_collab::board_service::inspect(self, params)
    }
    fn collab_pane_identity(&self, surface_id: &str) -> Result<Value> {
        if kasa_pty::lookup_session(surface_id).is_none() {
            bail!("managed place no longer exists");
        }
        kasa_collab::board_service::address(surface_id, self.session_of(surface_id).as_deref())
    }
    fn collab_tell(&self, params: &Value) -> Result<Value> {
        // 전달은 kasa-collab 전달 루프가 0.5초마다 장부를 훑어 한다.
        kasa_collab::tell_service::submit(self, params, || Ok(()))
    }
    fn collab_tell_status(&self, params: &Value) -> Result<Value> {
        kasa_collab::tell_service::status(params)
    }
    fn collab_tell_identity(&self, surface_id: &str) -> Result<Value> {
        let pty = kasa_pty::lookup_session(surface_id).context("live PTY unavailable")?;
        let shell = pty.shell_pid().context("live process identity unavailable")?;
        let table = kasa_pty::fresh_process_table();
        let (kind, pid) = kasa_pty::agent_pid_for_shell(&table, shell).context("target is a shell or unknown process")?;
        let mut address = self.collab_pane_identity(surface_id)?;
        if address.get("session_id").and_then(Value::as_str).unwrap_or_default().is_empty() {
            bail!("full current {} session unavailable; tell withheld", kind.as_str());
        }
        address["agent_pid"] = json!(pid);
        address["harness"] = json!(kind.as_str());
        Ok(address)
    }
    fn pane_activity_log(&self, surface_id: &str, limit: usize) -> Result<Vec<ActivityEvent>> {
        let path = self.meta(surface_id).transcript.context("이 칸에 묶인 대화 기록이 없다")?;
        Ok(kasa_agents::transcript::activity_from_tail(&read_tail(&path, 512 * 1024), limit))
    }
    fn collab_board(&self) -> Result<Vec<PaneActivity>> {
        let ids: Vec<String> = self.shared.lock().unwrap().panes.keys().cloned().collect();
        Ok(ids
            .iter()
            .filter_map(|id| {
                let meta = self.meta(id);
                let path = meta.transcript?;
                let mut a = kasa_agents::transcript::snapshot_from_tail(id, &read_tail(&path, 256 * 1024), meta.phase.as_deref() != Some("start"));
                a.character = meta.name;
                Some(a)
            })
            .collect())
    }
}

fn pane_cwd(id: &str) -> Option<String> {
    let pty = kasa_pty::lookup_session(id)?;
    pty.reported_cwd()
        .or_else(|| pty.shell_pid().and_then(crate::procinfo::process_cwd))
        .map(|p| p.to_string_lossy().into_owned())
}

fn read_tail(path: &Path, max: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).into_owned();
    // 잘린 첫 줄은 버린다 — 반쪽 JSON 줄이다.
    if start > 0 {
        text.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_default()
    } else {
        text
    }
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().enumerate().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
}

fn session_flag(cmdline: &str) -> Option<String> {
    let words: Vec<&str> = cmdline.split_whitespace().collect();
    ["--session-id", "--resume", "-r"].iter().find_map(|flag| {
        words.iter().enumerate().find_map(|(i, w)| {
            if w == flag {
                words.get(i + 1).filter(|s| is_uuid(s)).map(|s| s.to_string())
            } else {
                w.strip_prefix(&format!("{flag}=")).filter(|s| is_uuid(s)).map(str::to_owned)
            }
        })
    })
}

/// `kasaterm-cli send-key`·`tell --key` 의 키 이름.
fn key_bytes(key: &str) -> Option<Vec<u8>> {
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

/// 칸 shim 폴더 — `kasaterm-cli`(이 바이너리로 가는 링크), 훅을 얹는 `claude`, 훅 스크립트.
pub fn install_shims(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let settings = kasa_collab::hooks::install(dir)?;
    // 엔진이 `KASATERM_TMUX_SHIM_DIR` 를 보면 PATH 앞에 이 폴더를 붙이고 zsh 의 ZDOTDIR 를 여기로
    // 돌린다. 사용자 rc 가 PATH 를 다시 짜도(brew·~/.local/bin) 이 폴더의 claude·kasaterm-cli 가
    // 이기게, 사용자 rc 를 먼저 읽고 마지막에 다시 앞에 붙인다.
    let quoted = dir.display().to_string().replace('"', "\\\"");
    for (name, body) in [
        (".zshenv", "[ -f \"${HOME}/.zshenv\" ] && source \"${HOME}/.zshenv\"\n".to_string()),
        (".zprofile", "[ -f \"${HOME}/.zprofile\" ] && source \"${HOME}/.zprofile\"\n".to_string()),
        (".zshrc", format!("[ -f \"${{HOME}}/.zshrc\" ] && source \"${{HOME}}/.zshrc\"\nexport PATH=\"{quoted}:${{PATH}}\"\n")),
        (".zlogin", "[ -f \"${HOME}/.zlogin\" ] && source \"${HOME}/.zlogin\"\n".to_string()),
    ] {
        std::fs::write(dir.join(name), body)?;
    }
    #[cfg(unix)]
    {
        let exe = std::env::current_exe()?;
        let link = dir.join("kasaterm-cli");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&exe, &link)?;
        kasa_collab::hooks::write_claude_wrapper(dir, &settings)?;
    }
    #[cfg(windows)]
    {
        let exe = std::env::current_exe()?;
        let _ = std::fs::copy(&exe, dir.join("kasaterm-cli.exe"));
        let _ = settings;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_session_flag_and_keys() {
        let sid = "0e9a3d85-ce26-46ad-aa93-e211fcbfa8c7";
        assert_eq!(session_flag(&format!("node claude --resume {sid} --model x")), Some(sid.into()));
        assert_eq!(session_flag(&format!("claude --session-id={sid}")), Some(sid.into()));
        assert_eq!(session_flag("claude --resume nope"), None);
        assert_eq!(key_bytes("Enter").unwrap(), b"\r");
        assert_eq!(key_bytes("ctrl+c").unwrap(), [3]);
        assert!(key_bytes("hyper+x").is_none());
    }
}
