#!/usr/bin/env python3
"""KasaLite 의 Windows 브랜딩 에셋(app.ico · 설치 화면 2종)을 아이콘에서 만든다.

레포에 들어온 원본은 본판 kasaterm 의 것이라 물방울 로고와 "kasaterm" 글자가
박혀 있었다 — 라이트 설치본에서 그게 뜨면 다른 앱을 까는 것처럼 보인다.
아이콘을 갈아 끼울 때 이 스크립트를 다시 돌리면 셋이 함께 따라온다.

    python3 scripts/windows/make-brand-assets.py

Pillow 가 필요하다. 산출물은 레포에 커밋되므로 빌드 때는 돌지 않는다.
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[2]
ICONSET = ROOT / "assets" / "LiteIcon.iconset"
WIX = ROOT / "app" / "kasaterm" / "wix"

# WiX 가 고정으로 요구하는 치수. 다른 크기를 주면 설치 화면이 깨진다.
BANNER = (493, 58)
DIALOG = (493, 312)
# 원본에서 잰 값 — 왼쪽 패널 폭과 그 바탕색.
PANEL_W = 164
PANEL_BG = (37, 44, 53)

BOLD = "/System/Library/Fonts/Supplemental/Arial Bold.ttf"
REGULAR = "/System/Library/Fonts/Supplemental/Arial.ttf"


def icon(size: int) -> Image.Image:
    return Image.open(ICONSET / "icon_512x512.png").convert("RGBA").resize(
        (size, size), Image.LANCZOS
    )


def centered(draw: ImageDraw.ImageDraw, cx: int, y: int, text: str, font, fill) -> None:
    left, top, right, bottom = draw.textbbox((0, 0), text, font=font)
    draw.text((cx - (right - left) / 2 - left, y - top), text, font=font, fill=fill)


def write_ico() -> None:
    # 16·32 는 ICONSET 의 실물을 쓴다 — 512 를 줄이면 작은 크기에서 뭉갠다.
    sizes = [16, 32, 48, 64, 128, 256]
    base = icon(256)
    small = {
        16: Image.open(ICONSET / "icon_16x16.png").convert("RGBA"),
        32: Image.open(ICONSET / "icon_32x32.png").convert("RGBA"),
    }
    frames = [small.get(s) or icon(s) for s in sizes]
    out = ROOT / "assets" / "app.ico"
    base.save(out, format="ICO", sizes=[(s, s) for s in sizes], append_images=frames)
    # build.rs(exe 아이콘)와 WiX(제어판 아이콘)가 각자 다른 경로에서 읽는다.
    (WIX / "app.ico").write_bytes(out.read_bytes())
    print(f"wrote {out.relative_to(ROOT)} + wix/app.ico")


def write_dialog() -> None:
    img = Image.new("RGB", DIALOG, (255, 255, 255))
    draw = ImageDraw.Draw(img)
    draw.rectangle([0, 0, PANEL_W - 1, DIALOG[1]], fill=PANEL_BG)

    art = icon(110)
    img.paste(art, (PANEL_W // 2 - 55, 78), art)
    centered(draw, PANEL_W // 2, 198, "KasaLite", ImageFont.truetype(BOLD, 24), (255, 255, 255))
    centered(draw, PANEL_W // 2, 230, "terminal", ImageFont.truetype(REGULAR, 13), (150, 160, 172))

    img.save(WIX / "Dialog.bmp", format="BMP")
    print("wrote wix/Dialog.bmp")


def write_banner() -> None:
    img = Image.new("RGB", BANNER, (255, 255, 255))
    art = icon(44)
    img.paste(art, (BANNER[0] - 56, 7), art)
    img.save(WIX / "Banner.bmp", format="BMP")
    print("wrote wix/Banner.bmp")


if __name__ == "__main__":
    write_ico()
    write_dialog()
    write_banner()
