//! `kasa` — 아무 터미널 안에서 도는 다중 칸 터미널(`kasa tui`)과 그 세션 서버.

mod client;
mod config;
mod graphics;
mod keys;
mod paths;
mod procinfo;
mod proto;
mod render;
mod server;

use anyhow::{bail, Result};

const USAGE: &str = "\
kasa — 아무 터미널 안에서 도는 다중 칸 터미널

사용법:
  kasa tui [-s 이름]     세션에 붙는다. 없으면 연다 (기본 이름 main)
  kasa attach [이름]     떨어진 세션에 다시 붙는다
  kasa ls                떠 있는 세션
  kasa kill <이름>       세션을 끝낸다
  kasa --version

세션 안에서는 접두키(기본 Ctrl-b) 다음 ? 로 단축키를 본다.
";

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("kasa: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("tui") => client::run(&session_flag(rest)?, true),
        Some("attach") | Some("a") => {
            let name = rest.first().cloned().unwrap_or_else(|| paths::DEFAULT_SESSION.into());
            client::run(&name, false)
        }
        Some("ls") | Some("list") => server::list(),
        Some("kill") => match rest.first() {
            Some(name) => server::kill(name),
            None => bail!("끝낼 세션 이름을 준다: kasa kill <이름>"),
        },
        Some("server") => {
            let mut name = paths::DEFAULT_SESSION.to_string();
            let mut size = (80u16, 24u16);
            let mut cwd = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-s" => name = it.next().cloned().unwrap_or(name),
                    "--size" => {
                        if let Some((c, r)) = it.next().and_then(|v| v.split_once('x')) {
                            size = (c.parse().unwrap_or(80), r.parse().unwrap_or(24));
                        }
                    }
                    "--cwd" => cwd = it.next().cloned(),
                    _ => {}
                }
            }
            server::run(&name, size, cwd)
        }
        Some("-V") | Some("--version") | Some("version") => {
            println!("kasa {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        None | Some("-h") | Some("--help") | Some("help") => {
            print!("{USAGE}");
            Ok(0)
        }
        Some(other) => bail!("모르는 명령 `{other}`\n\n{USAGE}"),
    }
}

fn session_flag(args: &[String]) -> Result<String> {
    let mut it = args.iter();
    let mut name = paths::DEFAULT_SESSION.to_string();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-s" | "--session" => match it.next() {
                Some(n) => name = n.clone(),
                None => bail!("-s 뒤에 세션 이름을 준다"),
            },
            other => bail!("모르는 인자 `{other}`"),
        }
    }
    if !paths::valid_session_name(&name) {
        bail!("세션 이름은 영문·숫자·-·_ 32자까지다: {name}");
    }
    Ok(name)
}
