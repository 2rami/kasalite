#!/usr/bin/env bash
# 실행 파일의 ELF 아키텍처로 이름을 정하므로 --bin 교차 빌드도 잘못 표시하지 않는다.
# 실행 파일 하나 — 칸 안 `kasaterm-cli` 는 같은 바이너리로 가는 링크라 따로 싣지 않는다.
# GPU 렌더 노드가 없는 기계(에이전트 VM)에선 엔진이 알아서 CPU 로 그린다. 그래서 libEGL·Mesa 를 요구하지 않는다.
#
#   scripts/lite/package-linux.sh [--bin <이미 구운 실행 파일>] [--out <dir>]
set -euo pipefail

BIN=""
OUT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) BIN="$2"; shift ;;
    --out) OUT="$2"; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
  shift
done

abs() { mkdir -p "$(dirname "$1")" && echo "$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; }
[[ -n "$BIN" ]] && BIN="$(abs "$BIN")"
[[ -n "$OUT" ]] && OUT="$(abs "$OUT")"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
READELF="${READELF:-$(command -v readelf || command -v greadelf || true)}"
[[ -n "$READELF" ]] || { echo "error: readelf (binutils) is required" >&2; exit 1; }
OUT="${OUT:-$ROOT/dist}"
ICON="$ROOT/assets/LiteIcon.iconset/icon_256x256.png"
[[ -f "$ICON" && $(wc -c < "$ICON") -gt 1000 ]] || { echo "error: $ICON 이 없거나 LFS 포인터다(git lfs pull)" >&2; exit 1; }
FONT="$ROOT/assets/fonts/SymbolsNerdFontMono-Regular.ttf"
[[ $(wc -c < "$FONT") -gt 100000 ]] || { echo "error: 내장 Nerd 글꼴이 LFS 포인터다(git lfs pull)" >&2; exit 1; }

VERSION="$(grep -m1 '^version' app/lite/Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
if [[ -z "$BIN" ]]; then
  cargo build --locked --release -p kasalite --bin kasaterm-lite
  TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
  [[ -n "${CARGO_BUILD_TARGET:-}" ]] && TARGET_DIR="$TARGET_DIR/$CARGO_BUILD_TARGET"
  BIN="$TARGET_DIR/release/kasaterm-lite"
fi
[[ -x "$BIN" ]] || { echo "error: $BIN 이 없다" >&2; exit 1; }

MACHINE="$(LC_ALL=C "$READELF" -h "$BIN" | sed -n 's/^[[:space:]]*Machine:[[:space:]]*//p')"
case "$MACHINE" in
  'Advanced Micro Devices X86-64') ARCH=x86_64; DEB_ARCH=amd64 ;;
  AArch64) ARCH=aarch64; DEB_ARCH=arm64 ;;
  *) echo "error: unsupported ELF machine: $MACHINE" >&2; exit 1 ;;
esac
NAME="KasaLite-$VERSION-linux-$ARCH"
LIBC_VERSION="$("$READELF" --version-info "$BIN" | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' | cut -d_ -f2 | sort -Vu | tail -1)"
STAGE="$(mktemp -d)"
trap 'rm -rf "${STAGE:?}"' EXIT
ROOTFS="$STAGE/$NAME"
mkdir -p "$ROOTFS/bin" "$ROOTFS/share/applications" "$ROOTFS/share/icons/hicolor/256x256/apps"
cp "$BIN" "$ROOTFS/bin/kasaterm-lite"
ln -s kasaterm-lite "$ROOTFS/bin/kasaterm-cli"
cp "$ICON" "$ROOTFS/share/icons/hicolor/256x256/apps/kasalite.png"
# 파일 이름이 desktop id 다 — Wayland 셸은 창의 app_id(`kasaterm-lite`, app.rs APP_ID)와 이 이름을 맞춰 아이콘을 찾는다.
cat > "$ROOTFS/share/applications/kasaterm-lite.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=KasaLite
Name[ko]=카사라이트
Comment=Light terminal — splits, tabs, Korean IME
Exec=kasaterm-lite
Icon=kasalite
Terminal=false
Categories=System;TerminalEmulator;
StartupWMClass=kasaterm-lite
DESKTOP

mkdir -p "$OUT"
tar -C "$STAGE" -czf "$OUT/$NAME.tar.gz" "$NAME"
echo "built $OUT/$NAME.tar.gz"

if command -v dpkg-deb >/dev/null; then
  DEB="$STAGE/deb"
  mkdir -p "$DEB/DEBIAN" "$DEB/usr"
  cp -a "$ROOTFS/." "$DEB/usr/"
  # winit·softbuffer·xkbcommon 은 dlopen 하므로 X11·Wayland 의존성을 직접 적는다.
  # libegl1 은 GPU 경로에서만 쓰므로 Recommends.
  cat > "$DEB/DEBIAN/control" <<CONTROL
Package: kasalite
Version: $VERSION
Architecture: $DEB_ARCH
Maintainer: 2rami <https://github.com/2rami/kasalite>
Depends: libc6 (>= $LIBC_VERSION), libwayland-client0, libxkbcommon0, libx11-6, libx11-xcb1, libxcb1, libxcursor1, libxi6, libxrandr2, libxkbcommon-x11-0
Recommends: libegl1, fonts-noto-cjk, fonts-jetbrains-mono
Section: x11
Priority: optional
Homepage: https://github.com/2rami/kasalite
Description: KasaLite — light terminal (splits, tabs, Korean IME)
 Draws with kasaterm's terminal engine. On machines without a GPU render node
 it paints on the CPU through Wayland shared memory or X11.
CONTROL
  dpkg-deb --root-owner-group --build "$DEB" "$OUT/$NAME.deb" >/dev/null
  echo "built $OUT/$NAME.deb"
fi
