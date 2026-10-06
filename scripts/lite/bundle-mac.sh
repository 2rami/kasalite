#!/usr/bin/env bash
# KasaLite.app (와 dmg) 를 굽는다. 실행 파일 하나 — 칸 안 `kasaterm-cli` 는 같은 바이너리로 가는 링크라
# 따로 싣지 않는다. 번들 id·실행 파일 이름·살림 폴더는 옛 v0.1 그대로라 설정이 이어진다.
#
#   scripts/lite/bundle-mac.sh [--debug] [--dmg] [--target <triple>] [--out <dir>]
set -euo pipefail

PROFILE=release
DMG=0
TARGET=""
OUT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --debug) PROFILE=debug ;;
    --dmg) DMG=1 ;;
    --target) TARGET="$2"; shift ;;
    --out) OUT="$2"; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
  shift
done

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
OUT="${OUT:-$ROOT/dist}"
ICON="$ROOT/assets/LiteIcon.icns"
[[ -f "$ICON" && $(wc -c < "$ICON") -gt 1000 ]] || { echo "error: assets/LiteIcon.icns 가 없거나 LFS 포인터다(git lfs pull)" >&2; exit 1; }
FONT="$ROOT/assets/fonts/SymbolsNerdFontMono-Regular.ttf"
[[ $(wc -c < "$FONT") -gt 100000 ]] || { echo "error: 내장 Nerd 글꼴이 LFS 포인터다(git lfs pull)" >&2; exit 1; }

VERSION="$(grep -m1 '^version' app/lite/Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
ARGS=(-p kasalite --bin kasaterm-lite)
[[ "$PROFILE" == release ]] && ARGS+=(--release)
[[ -n "$TARGET" ]] && ARGS+=(--target "$TARGET")
cargo build "${ARGS[@]}"
TDIR="${CARGO_TARGET_DIR:-$ROOT/target}"
BIN="$TDIR/${TARGET:+$TARGET/}$PROFILE/kasaterm-lite"
[[ -x "$BIN" ]] || { echo "error: $BIN 이 없다" >&2; exit 1; }

mkdir -p "$OUT"
APP="$OUT/KasaLite.app"
STAGE="$OUT/.KasaLite.app.$$"
trap 'rm -rf "$STAGE"' EXIT
rm -rf "$STAGE"
mkdir -p "$STAGE/Contents/MacOS" "$STAGE/Contents/Resources"
cp "$BIN" "$STAGE/Contents/MacOS/kasaterm-lite"
cp "$ICON" "$STAGE/Contents/Resources/LiteIcon.icns"
cat > "$STAGE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>KasaLite</string>
    <key>CFBundleDisplayName</key>
    <string>카사라이트</string>
    <key>CFBundleIdentifier</key>
    <string>com.kasa.kasaterm.lite</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleExecutable</key>
    <string>kasaterm-lite</string>
    <key>CFBundleIconFile</key>
    <string>LiteIcon</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
    <key>CADisableMinimumFrameDurationOnPhone</key>
    <true/>
</dict>
</plist>
PLIST
plutil -lint "$STAGE/Contents/Info.plist" >/dev/null

SIGN_ID="${KASALITE_SIGN_ID:-}"
if [[ -n "$SIGN_ID" ]] && security find-identity -p codesigning 2>/dev/null | grep -q "$SIGN_ID"; then
  codesign --force --options runtime --timestamp --sign "$SIGN_ID" "$STAGE"
else
  codesign --force --timestamp=none --sign - "$STAGE"
fi
codesign --verify --strict "$STAGE"

rm -rf "$APP"
mv "$STAGE" "$APP"
trap - EXIT
echo "$APP ($(du -sh "$APP" | cut -f1))"

if [[ "$DMG" == 1 ]]; then
  ARCH="${TARGET%%-*}"; ARCH="${ARCH:-$(uname -m)}"
  DMG_PATH="$OUT/KasaLite-$VERSION-macos-$ARCH.dmg"
  SRC="$OUT/.dmg-src.$$"
  rm -rf "$SRC" "$DMG_PATH"
  mkdir -p "$SRC"
  cp -R "$APP" "$SRC/"
  ln -s /Applications "$SRC/Applications"
  # -srcfolder 가 /Applications 링크를 따라가 크기를 부풀린다 — 크기를 직접 준다.
  MB=$(( $(du -sm "$APP" | cut -f1) + 20 ))
  hdiutil create -volname "KasaLite" -srcfolder "$SRC" -fs HFS+ -format UDZO -imagekey zlib-level=9 -size "${MB}m" "$DMG_PATH" >/dev/null
  rm -rf "$SRC"
  echo "$DMG_PATH ($(du -sh "$DMG_PATH" | cut -f1))"
fi
