# kasalite

[kasaterm](https://github.com/2rami/kasaterm) 의 터미널 엔진 위에 세운 가벼운 터미널 둘을 담는다.

- **`kasa tui`** — 앱을 깔지 않고 아무 터미널(Ghostty·iTerm2·Windows Terminal·SSH 너머 서버) 안에서
  tmux 처럼 도는 다중 칸 터미널. 칸 나누기·탭·마우스·스크롤백·한글·kitty 그림을 지원한다.
- **새 카사라이트** — 120Hz 로 가볍게 도는 바닐라 GUI 터미널(만드는 중).

엔진(PTY·VT·스크롤백·kitty 그림·화면 낱말)의 원본은 kasaterm 레포 `crates/` 다. 여기서는 커밋 하나를
고정해 git 의존으로 받는다(`Cargo.toml` 의 `[workspace.dependencies]`). 설계는 kasaterm
[`docs/terminal-engine.md`](https://github.com/2rami/kasaterm/blob/main/docs/terminal-engine.md).

## kasa tui

```sh
kasa tui              # 세션 main 에 붙는다. 없으면 연다
kasa tui -s work      # 다른 이름의 세션
kasa attach [이름]     # 떨어진 세션에 다시 붙는다
kasa ls               # 떠 있는 세션
kasa kill <이름>       # 세션을 끝낸다
```

창을 닫거나 SSH 가 끊겨도 세션 서버와 칸은 산다. 다시 `kasa attach` 로 붙는다.

### 단축키

접두키(기본 `Ctrl-b`) 다음에 누른다. 세션 안에서 `접두키 ?` 로 같은 표를 본다.

| 키 | 하는 일 |
|---|---|
| `%` `\|` | 옆으로 나누기 |
| `"` `-` | 아래로 나누기 |
| 화살표 · `h` `j` `k` `l` | 칸 옮기기 |
| `o` | 다음 칸 |
| `z` | 칸 확대 / 되돌리기 |
| `x` | 칸 닫기 |
| `c` | 새 탭 |
| `n` `p` `0`–`9` | 탭 옮기기 |
| `,` | 탭 이름 바꾸기 |
| `[` | 스크롤 (`q` 로 끝) |
| `d` | 떨어지기 |
| 접두키 두 번 | 접두키를 칸에 보내기 |

접두키 없이 마우스로도 된다: 아래 줄의 `+`(새 탭)·`옆으로`·`아래로`·`확대`·`닫기`·`?` 단추를 누른다.
칸을 눌러 초점, 경계선을 끌어 크기, 탭 줄을 눌러 탭, 휠로 스크롤백. 끌어서 고른 글은 OSC 52 로
바깥 터미널 클립보드에 들어간다. 칸 안 프로그램이 마우스를 켰으면 그 프로그램에 넘기고, `Shift` 를
누르고 끌면 그래도 고른다.

### 설정

`~/.config/kasa/tui.conf`:

```text
prefix = C-a
```

환경 변수 `KASA_TUI_PREFIX=C-a` 가 앞선다. tmux 안에서 돌릴 때는 접두키를 tmux 와 다르게 둔다.
`prefix = none` 이면 접두키를 끄고 모든 키를 칸에 보낸다(칸 다루기는 단추로).

카사텀 본판의 ⌘D·⌘T 같은 단축키는 쓰지 않는다 — ⌘ 조합은 바깥 터미널(Ghostty·iTerm2)이 먼저
자기 단축키로 잡아 칸까지 오지 않는다.

## 빌드

```sh
cargo build --release -p kasa-tui     # target/release/kasa
```

엔진을 고치면서 함께 볼 때는 kasaterm 작업 트리로 의존을 돌린다(레포에 넣지 않는 파일):

```toml
# patch.toml
[patch."https://github.com/2rami/kasaterm"]
kasa-pty = { path = "../kasaterm/crates/kasa-pty" }
kasa-screen = { path = "../kasaterm/crates/kasa-screen" }
kasa-socket = { path = "../kasaterm/crates/kasa-socket" }
```

```sh
cargo --config patch.toml build -p kasa-tui
```

## 옛 카사라이트(v0.1)

본판을 통째로 복사해 터미널만 남긴 옛 판은 [`legacy-v0.1`](https://github.com/2rami/kasalite/tree/legacy-v0.1)
가지와 `v0.1.0` 태그에 그대로 있다. 릴리스의 `.dmg`·`.msi` 도 그 판이다.

## 라이선스

MIT
