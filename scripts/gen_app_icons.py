# /// script
# requires-python = ">=3.10"
# dependencies = ["pillow"]
# ///
"""Generate the phone and watch app icons from the brand mark in src-tauri/icons/sona.svg.

The viewBox crops to the Mac icon's rounded square, so the glyph keeps its
proportions on the full-bleed square that iOS and watchOS mask themselves.
The phone gets three appearances: the ink tile, the glyph alone on a
transparent background for dark mode, and a grayscale copy the system tints.

Needs `rsvg-convert` (librsvg). Run with `uv run scripts/gen_app_icons.py`.
"""

import json
import pathlib
import re
import subprocess
import tempfile

from PIL import Image

REPO = pathlib.Path(__file__).resolve().parent.parent
BRAND = (REPO / "src-tauri/icons/sona.svg").read_text()
TILE = re.search(r'<path fill="(#[0-9a-fA-F]{6})" d="[^"]+"/>', BRAND)
GLYPH = BRAND[TILE.end() : BRAND.rindex("</svg>")].strip()
INK = TILE.group(1)
CATALOG = {"info": {"author": "xcode", "version": 1}}


def svg(background: str | None, mark: str) -> str:
    fill = f'<rect x="96" y="96" width="832" height="832" fill="{background}"/>' if background else ""
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="96 96 832 832">{fill}{mark}</svg>'


def render(work: pathlib.Path, name: str, source: str, mode: str) -> Image.Image:
    vector = work / f"{name}.svg"
    vector.write_text(source)
    raster = work / f"{name}.png"
    subprocess.run(["rsvg-convert", "-w", "1024", "-h", "1024", str(vector), "-o", str(raster)], check=True)
    return Image.open(raster).convert(mode)


def entry(filename: str, platform: str, appearance: str | None = None) -> dict:
    image = {"filename": filename, "idiom": "universal", "platform": platform, "size": "1024x1024"}
    if appearance is None:
        return image
    return {"appearances": [{"appearance": "luminosity", "value": appearance}], **image}


def icon_set(catalog: pathlib.Path, images: list[dict]) -> pathlib.Path:
    icons = catalog / "AppIcon.appiconset"
    icons.mkdir(parents=True, exist_ok=True)
    (catalog / "Contents.json").write_text(json.dumps(CATALOG, indent=2) + "\n")
    (icons / "Contents.json").write_text(json.dumps({"images": images, **CATALOG}, indent=2) + "\n")
    return icons


with tempfile.TemporaryDirectory() as scratch:
    work = pathlib.Path(scratch)
    light = render(work, "light", svg(INK, GLYPH), "RGB")
    dark = render(work, "dark", svg(None, GLYPH), "RGBA")
    tinted = render(work, "tinted", svg("#000000", GLYPH.replace("#fafafa", "#ffffff")), "L").convert("RGB")

phone = icon_set(
    REPO / "mobile/Sona/Assets.xcassets",
    [
        entry("AppIcon.png", "ios"),
        entry("AppIcon-Dark.png", "ios", "dark"),
        entry("AppIcon-Tinted.png", "ios", "tinted"),
    ],
)
watch = icon_set(REPO / "mobile/SonaWatch/Assets.xcassets", [entry("AppIcon.png", "watchos")])
light.save(phone / "AppIcon.png", optimize=True)
dark.save(phone / "AppIcon-Dark.png", optimize=True)
tinted.save(phone / "AppIcon-Tinted.png", optimize=True)
light.save(watch / "AppIcon.png", optimize=True)
