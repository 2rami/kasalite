# KasaLite

터미널만 남긴 [kasaterm](https://github.com/2rami/kasaterm) 고정판. 캐릭터·보드·펫·서버·자동 업데이트·세션 복원이
없고, pane 나누기·탭·한글 IME·`kasaterm-cli`·헤더 진행 바만 있다. 설정은 색 하나(⌘,).

kasaterm 을 kasaterm 안에서 고치다 보면 앱을 굽고 껐다 켤 때마다 그 안의 세션이 죽는다.
이 앱은 한 번 굽고 **다시 굽지 않는** 용도다 — Ghostty 처럼.

## 받기

[릴리스](https://github.com/2rami/kasalite/releases)에서 macOS `.dmg` · Windows `.msi`.
자동 업데이트가 없으니 새 판은 같은 자리에서 받아 덮어쓴다.

## 빌드

```sh
scripts/build-lite-app.sh --install   # dist/KasaLite.app → ~/Applications/KasaLite.app
pwsh scripts\windows\package.ps1      # dist\kasalite-v<판>-windows-x86_64.msi (Windows)
```

태그(`vX.Y.Z`)를 밀면 `.github/workflows/release.yml` 이 둘 다 구워 릴리스에 붙인다.
태그 전에 Windows 빌드만 보려면 Actions 에서 이 워크플로를 수동 실행한다 — 맥에서는
윈도우 타깃 크로스 컴파일이 `ring` 의 C 코드에서 막혀 미리 볼 수 없다.

살림은 `~/.config/kasaterm-lite/` (설정·창 크기·소켓 `lite.sock`). 본판 kasaterm 과 같이 떠도 서로 안 보인다.

바깥에서 CLI 를 부르려면 `KASATERM_SOCKET_PATH=~/.config/kasaterm-lite/lite.sock kasaterm-cli …`. pane 안에서는 자동.

## 본판과의 관계

같은 코드에 런타임 모드 하나를 얹은 것이다(`ViewerLaunch::lite`). 본판 kasaterm 의 수정을 따라가려면
`app/kasaterm`·`crates/` 를 옮겨 오면 되고, 라이트 전용 분기는 `self.lite` / `crate::lite_mode()` 로 찾는다.

MIT
