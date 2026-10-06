# kasa tui 릴리스

굽기·게시는 dist(`dist-workspace.toml`)가 맡는다. `v*` 태그를 밀면 `.github/workflows/release.yml` 이
다섯 대상(macOS arm64·x86_64, Linux arm64·x86_64, Windows x86_64)을 굽고 GitHub 릴리스를 만든 뒤
Homebrew 탭과 npm 에 싣는다. 태그를 밀기 전엔 아무것도 게시되지 않는다.

## 처음 한 번

| 할 일 | 방법 |
|---|---|
| Homebrew 탭 레포 | `2rami/homebrew-tap`(공개, 빈 레포). dist 가 `Formula/kasa-tui.rb` 를 올린다 |
| `HOMEBREW_TAP_TOKEN` | 탭 레포에 쓸 수 있는 토큰 → `gh secret set HOMEBREW_TAP_TOKEN -R 2rami/kasalite` |
| `NPM_TOKEN` | npm 계정의 자동화(Automation) 토큰 → `gh secret set NPM_TOKEN -R 2rami/kasalite` |
| winget 첫 판 | 릴리스가 선 뒤 `komac new 2rami.kasa-tui --version 0.1.0 --urls <windows zip 주소> --submit` 로 `microsoft/winget-pkgs` 에 PR 을 한 번 손으로 낸다(클래식 PAT `public_repo`). 다음 판부터는 winget-releaser 액션이 이어 받는다 |

비밀 값은 레포·로그·문서에 적지 않는다. `gh secret set` 은 값을 표준 입력으로 받는다.

## 판 올리기

1. `app/kasa/Cargo.toml` 의 `version` 을 올린다.
2. 엔진을 올릴 거면 `Cargo.toml` 의 kasaterm `rev` 를 바꾸고 `cargo build -p kasa-tui` 로 확인한다.
3. 커밋·푸시 뒤 `git tag v<판> && git push origin v<판>`.

## 서명

첫 판은 서명하지 않는다(macOS 는 링커의 애드혹 서명만). brew·npm·셸 설치는 브라우저 다운로드가 아니라
격리 표시가 붙지 않아 그대로 실행된다. Developer ID 서명·공증은 다음 판에서 정한다.
Windows 는 SmartScreen 경고가 날 수 있다.

## 설치 확인

```sh
brew install 2rami/tap/kasa-tui
npm i -g kasa-tui
curl -fsSL https://github.com/2rami/kasalite/releases/latest/download/kasa-tui-installer.sh | sh
winget install 2rami.kasa-tui        # winget-pkgs PR 이 합쳐진 뒤
```

모두 `kasa` 명령을 깐다. `kasa tui` 로 연다.
