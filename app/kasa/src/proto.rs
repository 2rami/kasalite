//! 서버와 클라이언트 사이 낱말. 길이 머리(u32 LE) + bincode 한 덩어리가 한 메시지다.
//!
//! 엔진의 `ScreenUpdate` 를 그대로 싣지 않고 여기 낱말로 옮긴다 — 엔진 구조체는 serde
//! 속성(`skip_serializing_if`)이 bincode 와 안 맞고, 엔진을 올릴 때마다 붙어 있던 클라이언트와
//! 서버가 서로 못 알아듣는 일이 없어야 해서다.

use std::io::{Read, Write};

use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::keys::{KeyInput, Modes, MouseKind};

/// 서버와 클라이언트가 같은 바이너리에서 나오지만, 판을 올린 뒤 옛 서버에 새 클라이언트가
/// 붙는 일은 생긴다. 낱말이 바뀌면 올린다.
pub const PROTOCOL: u32 = 3;

const MAX_FRAME: usize = 64 << 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMsg {
    /// 붙는 클라이언트의 첫 메시지. `cell_px` 는 바깥 터미널 글자 한 칸의 픽셀 크기(모르면 없음).
    /// `heads` 면 칸마다 맨 위 한 줄을 머리 줄(이름·단추)로 비워 달라는 뜻이다.
    Hello { protocol: u32, cols: u16, rows: u16, cell_px: Option<(u16, u16)>, heads: bool },
    Resize { cols: u16, rows: u16, cell_px: Option<(u16, u16)> },
    /// 초점 칸에 키 하나.
    Key(KeyInput),
    /// 초점 칸에 붙여넣기.
    Paste(String),
    /// 칸 안 좌표의 마우스 사건. 칸이 마우스 모드를 켰을 때만 클라이언트가 보낸다.
    Mouse { pane: String, kind: MouseKind, col: u16, row: u16, mods: u8 },
    /// 칸의 스크롤백 보기를 움직인다(+ 위로). 0 이면 맨 아래로.
    Scroll { pane: String, lines: i32 },
    Command(Command),
    /// 붙지 않고 묻기만 하는 연결(`kasa ls`).
    Query,
    Detach,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    /// 초점 칸을 나눈다. `right` 면 옆으로, 아니면 아래로.
    Split { right: bool },
    ClosePane,
    NewTab,
    NextTab,
    PrevTab,
    SelectTab(usize),
    Focus(String),
    FocusDir(Dir),
    /// 초점 칸을 탭 전체로 키우거나 되돌린다.
    Zoom,
    RenameTab(String),
    /// 분할선 `path` 를 칸 좌표 `pos` 로 옮긴다(`PtyLayout::resize_divider`).
    MoveDivider { path: Vec<u8>, pos: u16 },
    KillServer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMsg {
    Layout(LayoutMsg),
    Frame(PaneFrame),
    /// 칸이 OSC 52 로 클립보드에 쓴 글 — 클라이언트가 바깥 터미널로 다시 낸다.
    Clipboard(String),
    Info(SessionInfo),
    /// 서버가 이 클라이언트를 놓는다(세션 끝·다른 판·kill). 사람에게 보일 까닭을 싣는다.
    Bye(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub pid: u32,
    pub tabs: usize,
    pub panes: usize,
    pub clients: usize,
    pub created_unix: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LayoutMsg {
    pub session: String,
    /// 서버가 칸을 맞춘 창 크기(마지막으로 만진 클라이언트의 크기).
    pub cols: u16,
    pub rows: u16,
    pub tabs: Vec<String>,
    pub active_tab: usize,
    /// 지금 탭에서 보이는 칸들. 확대 중이면 초점 칸 하나.
    pub panes: Vec<PaneRect>,
    /// 지금 탭의 모든 칸(번호 순서, id·이름). 확대 중에도 칸 고르기 칩이 다른 칸을 보여 준다.
    pub tab_panes: Vec<(String, String)>,
    pub dividers: Vec<DividerMsg>,
    pub focus: String,
    pub zoomed: bool,
}

/// 칸이 그리는 글자 자리. 머리 줄이 있으면 그 줄은 `y - 1` 에 있고 여기 넓이에 들지 않는다.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaneRect {
    pub id: String,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub head: bool,
    /// 머리 줄·칸 고르기 칩에 쓸 이름(학생 이름, 없으면 도는 프로그램).
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DividerMsg {
    pub path: Vec<u8>,
    /// 옆으로 나눈 분할(세로선)이면 참.
    pub vertical_line: bool,
    /// 선이 놓인 열(세로선) 또는 행(가로선).
    pub at: u16,
    pub start: u16,
    pub len: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WColor {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;
pub const UNDERLINE: u8 = 4;
pub const INVERSE: u8 = 8;
pub const DIM: u8 = 16;
pub const HIDDEN: u8 = 32;
/// 넓은 글자 뒤의 빈 칸, 또는 줄 끝에서 넓은 글자가 다음 줄로 넘어가며 남긴 빈 칸.
pub const SPACER: u8 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WCell {
    pub ch: char,
    pub fg: WColor,
    pub bg: WColor,
    pub attrs: u8,
}

impl WCell {
    pub const BLANK: WCell = WCell { ch: ' ', fg: WColor::Default, bg: WColor::Default, attrs: 0 };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneFrame {
    pub pane: String,
    pub cols: u16,
    pub rows: u16,
    /// 바뀐 행만. 크기가 바뀌었거나 처음 보내는 칸이면 모든 행.
    pub dirty: Vec<(u16, Vec<WCell>)>,
    pub cursor: (u16, u16),
    pub cursor_visible: bool,
    pub modes: Modes,
    pub title: Option<String>,
    /// 스크롤백을 올려 보고 있는 줄 수(0 = 살아 있는 끝).
    pub scrolled: u32,
    /// 지금 칸에 보이는 그림 전부(OSC 1337·kitty). 프레임마다 통째로 온다.
    pub images: Vec<ImageView>,
}

/// 칸에 보이는 그림 하나. 좌표는 칸 안 글자 칸이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageView {
    /// 칸 안에서 그림을 가리키는 번호 — 자리를 옮겨도 같다.
    pub id: u64,
    /// 서버 기계의 임시 파일(PNG·JPEG). 클라이언트는 같은 기계에서 돈다.
    pub path: String,
    /// 상자 맨 위 행. 스크롤로 위가 잘리면 음수.
    pub row: i32,
    pub col: u16,
    pub cols: u16,
    pub rows: u16,
    /// 상자 안에서 실제로 보이는 칸(행, 열, 폭, 높이). 없으면 상자 전체.
    pub clip: Option<(i32, u16, u16, u16)>,
}

pub fn write_msg<T: Serialize, W: Write>(w: &mut W, msg: &T) -> Result<()> {
    let body = bincode::serialize(msg)?;
    w.write_all(&(body.len() as u32).to_le_bytes())?;
    w.write_all(&body)?;
    w.flush()?;
    Ok(())
}

pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>> {
    let body = bincode::serialize(msg)?;
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// 다음 메시지. 상대가 깔끔히 닫았으면 `None`.
pub fn read_msg<T: DeserializeOwned, R: Read>(r: &mut R) -> Result<Option<T>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        bail!("메시지가 너무 크다({len} 바이트)");
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).context("메시지 본문")?;
    Ok(Some(bincode::deserialize(&body).context("메시지 해석")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let msg = ServerMsg::Frame(PaneFrame {
            pane: "%3".into(),
            cols: 2,
            rows: 1,
            dirty: vec![(0, vec![WCell { ch: '한', fg: WColor::Rgb(1, 2, 3), bg: WColor::Idx(4), attrs: BOLD }, WCell { attrs: SPACER, ..WCell::BLANK }])],
            cursor: (0, 1),
            cursor_visible: true,
            modes: Modes { app_cursor: true, ..Modes::default() },
            title: Some("vim".into()),
            scrolled: 0,
            images: vec![ImageView { id: 7, path: "/tmp/a.png".into(), row: -2, col: 3, cols: 10, rows: 5, clip: None }],
        });
        let mut buf = Vec::new();
        write_msg(&mut buf, &msg).unwrap();
        let back: ServerMsg = read_msg(&mut buf.as_slice()).unwrap().unwrap();
        let ServerMsg::Frame(f) = back else { panic!() };
        assert_eq!(f.dirty[0].1[0].ch, '한');
        assert!(f.modes.app_cursor);
        assert_eq!(f.images[0].row, -2);
        assert!(read_msg::<ServerMsg, _>(&mut &[][..]).unwrap().is_none());
    }
}
