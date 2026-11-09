//! 명령줄 인자. alacritty 와 같은 꼴(`-e`·`-T`)이라 다른 터미널을 부르던 바로가기를 이름만 바꿔 옮길 수 있다.

use std::ffi::OsString;

pub const USAGE: &str = "\
사용: kasaterm-lite [-T 제목] [-e 명령 [인자…]]

  -e, --command 명령 [인자…]  첫 칸에서 셸 대신 이 명령을 띄운다. 뒤의 인자는 전부 명령의 것이다.
                              명령이 끝나면 그 칸이 닫히고, 마지막 칸이면 창도 닫힌다.
  -T, --title 제목            창 제목(기본 KasaLite)
  -h, --help                  이 도움말
  -V, --version               판 번호
";

#[derive(Debug, Default, PartialEq)]
pub struct Launch {
    /// 첫 칸에 띄울 argv. 비면 셸.
    pub command: Vec<OsString>,
    pub title: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    Run(Launch),
    Help,
    Version,
}

/// `args` 는 실행 파일 이름을 뺀 나머지.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Parsed, String> {
    let mut launch = Launch::default();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let s = arg.to_string_lossy();
        match s.as_ref() {
            "-e" | "--command" => {
                launch.command = it.by_ref().collect();
                if launch.command.is_empty() {
                    return Err(format!("{s} 뒤에 띄울 명령이 없다"));
                }
            }
            "-T" | "--title" => {
                let t = it.next().ok_or_else(|| format!("{s} 뒤에 제목이 없다"))?;
                launch.title = Some(t.to_string_lossy().into_owned());
            }
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            other => {
                if let Some(t) = other.strip_prefix("--title=") {
                    launch.title = Some(t.to_string());
                } else if other.starts_with("-psn_") {
                    // 옛 macOS LaunchServices 가 Finder 로 띄울 때 붙이던 프로세스 번호 — 사람이 준 인자가 아니다.
                } else {
                    return Err(format!("알 수 없는 인자: {other}"));
                }
            }
        }
    }
    Ok(Parsed::Run(launch))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Parsed, String> {
        parse(args.iter().map(OsString::from))
    }

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn no_args_runs_the_shell() {
        assert_eq!(p(&[]), Ok(Parsed::Run(Launch::default())));
    }

    #[test]
    fn everything_after_e_belongs_to_the_command() {
        let got = p(&["-T", "에이전트 셸", "-e", "watch", "-T", "x", "--help"]);
        assert_eq!(
            got,
            Ok(Parsed::Run(Launch { command: os(&["watch", "-T", "x", "--help"]), title: Some("에이전트 셸".into()) }))
        );
    }

    #[test]
    fn title_forms() {
        assert_eq!(p(&["--title=a b"]), Ok(Parsed::Run(Launch { command: vec![], title: Some("a b".into()) })));
        assert!(p(&["-T"]).is_err());
    }

    #[test]
    fn e_without_a_command_is_an_error() {
        assert!(p(&["-e"]).is_err());
        assert!(p(&["--command"]).is_err());
    }

    #[test]
    fn unknown_flags_fail_but_launchservices_noise_is_ignored() {
        assert!(p(&["--config-file", "x"]).is_err());
        assert_eq!(p(&["-psn_0_12345"]), Ok(Parsed::Run(Launch::default())));
    }

    #[test]
    fn help_and_version() {
        assert_eq!(p(&["-h"]), Ok(Parsed::Help));
        assert_eq!(p(&["--version"]), Ok(Parsed::Version));
    }
}
