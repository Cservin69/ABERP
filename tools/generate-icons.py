#!/usr/bin/env python3
"""
generate-icons.py — regenerate the Tauri icon set under apps/aberp-ui/icons/.

WHY THIS EXISTS
---------------
Tauri 2 fails *silently* on missing or malformed app icons: tauri-build
succeeds, the window opens, but the WebView never initialises and the
operator sees a blank white screen with no error in the logs. The 2026-05-30
prod cutover hit this exact failure mode. To prevent fresh-clone regressions,
the repo now ships a placeholder icon set generated from a single source
image by this script.

PLACEHOLDER vs. REAL BRANDING
-----------------------------
The icons committed to the repo today are a *placeholder*: dark navy
background (#1a2332), centred white "ABERP" wordmark in a system sans
typeface. They are intentionally simple — the goal is "professional, not
amateur," not final branding.

TO REPLACE WITH ÁBEN BRANDING
-----------------------------
1. Drop a square (≥1024×1024 recommended), PNG-with-transparency logo at
   `tools/source-logo.png`.
2. Run `python3 tools/generate-icons.py`.
3. Rebuild aberp-ui (`cargo build --release --bin aberp-ui --features production`).
4. Commit the regenerated files under `apps/aberp-ui/icons/`.

If `tools/source-logo.png` is absent, the script regenerates the placeholder
from scratch using PIL drawing primitives (no external font files required).

OUTPUTS
-------
Exactly the set referenced by Tauri at build time. tauri.conf.json
currently lists only `icons/icon.icns` (bundle.active = false today), but
the full set is kept for the day bundle.active flips to true.

Outputs written to apps/aberp-ui/icons/:
  - 32x32.png
  - 128x128.png
  - 128x128_2x.png   (256×256; underscore form per repo convention since PR-9-2)
  - icon.png         (128×128, used as the generic icon)
  - icon.icns        (macOS multi-resolution; built via /usr/bin/iconutil)
  - icon.ico         (Windows multi-resolution: 16/32/48/256)

REQUIREMENTS
------------
- Python 3.9+
- Pillow (`pip3 install --break-system-packages Pillow`)
- macOS `iconutil` (ships in /usr/bin/iconutil; the script falls back to
  emitting the .iconset directory if iconutil is absent)

USAGE
-----
  python3 tools/generate-icons.py
  python3 tools/generate-icons.py --source path/to/custom-logo.png
  python3 tools/generate-icons.py --help
"""

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:
    print(
        "[fail] Pillow not installed. Run:\n"
        "    pip3 install --break-system-packages Pillow",
        file=sys.stderr,
    )
    sys.exit(2)


REPO_ROOT = Path(__file__).resolve().parent.parent
ICONS_DIR = REPO_ROOT / "apps" / "aberp-ui" / "icons"
DEFAULT_SOURCE = REPO_ROOT / "tools" / "source-logo.png"

PLACEHOLDER_BG = (26, 35, 50, 255)        # #1a2332 — dark navy
PLACEHOLDER_FG = (255, 255, 255, 255)     # white wordmark
PLACEHOLDER_TEXT = "ABERP"
MASTER_SIZE = 1024                         # work from a 1024² master then resize


def find_sans_font() -> Path | None:
    """Locate a clean sans-serif TTF available on macOS or Linux."""
    candidates = [
        "/System/Library/Fonts/HelveticaNeue.ttc",
        "/System/Library/Fonts/Helvetica.ttc",
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/Library/Fonts/Arial Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
    ]
    for path in candidates:
        if Path(path).exists():
            return Path(path)
    return None


def draw_placeholder(size: int = MASTER_SIZE) -> Image.Image:
    """Synthesise the placeholder ABERP icon from primitives."""
    img = Image.new("RGBA", (size, size), PLACEHOLDER_BG)
    draw = ImageDraw.Draw(img)

    # Slight rounded-square mask: real app icons are rounded but macOS does the
    # rounding itself for .icns / .ico. Here we draw a 1px inner border so the
    # icon reads as "intentional," not "default".
    border_inset = max(2, size // 64)
    draw.rectangle(
        (border_inset, border_inset, size - border_inset - 1, size - border_inset - 1),
        outline=(60, 75, 100, 255),
        width=max(1, size // 256),
    )

    # Wordmark.
    font_path = find_sans_font()
    target_width = int(size * 0.72)
    if font_path is None:
        # Fallback: PIL's bitmap default — readable but small. Better than crashing.
        font = ImageFont.load_default()
        text_bbox = draw.textbbox((0, 0), PLACEHOLDER_TEXT, font=font)
        text_w = text_bbox[2] - text_bbox[0]
        text_h = text_bbox[3] - text_bbox[1]
        x = (size - text_w) // 2
        y = (size - text_h) // 2 - text_bbox[1]
        draw.text((x, y), PLACEHOLDER_TEXT, fill=PLACEHOLDER_FG, font=font)
        return img

    # Binary-search a font size that fits target_width.
    lo, hi, best = 8, size, 8
    while lo <= hi:
        mid = (lo + hi) // 2
        font = ImageFont.truetype(str(font_path), mid)
        bbox = draw.textbbox((0, 0), PLACEHOLDER_TEXT, font=font)
        text_w = bbox[2] - bbox[0]
        if text_w <= target_width:
            best = mid
            lo = mid + 1
        else:
            hi = mid - 1

    font = ImageFont.truetype(str(font_path), best)
    bbox = draw.textbbox((0, 0), PLACEHOLDER_TEXT, font=font)
    text_w = bbox[2] - bbox[0]
    text_h = bbox[3] - bbox[1]
    x = (size - text_w) // 2 - bbox[0]
    y = (size - text_h) // 2 - bbox[1]
    draw.text((x, y), PLACEHOLDER_TEXT, fill=PLACEHOLDER_FG, font=font)
    return img


def load_or_make_master(source: Path) -> Image.Image:
    if source.exists():
        print(f"[info] using source logo: {source}")
        img = Image.open(source).convert("RGBA")
        if img.width != img.height:
            print(
                f"[warn] source is {img.width}x{img.height} (non-square); "
                f"centre-cropping to square",
                file=sys.stderr,
            )
            side = min(img.width, img.height)
            left = (img.width - side) // 2
            top = (img.height - side) // 2
            img = img.crop((left, top, left + side, top + side))
        if img.width < MASTER_SIZE:
            print(
                f"[warn] source is {img.width}x{img.height} (<{MASTER_SIZE}); "
                f"upscaling — quality may suffer",
                file=sys.stderr,
            )
        return img.resize((MASTER_SIZE, MASTER_SIZE), Image.LANCZOS)
    print("[info] no source logo found — generating placeholder")
    return draw_placeholder(MASTER_SIZE)


def write_png(master: Image.Image, size: int, out_path: Path) -> None:
    img = master.resize((size, size), Image.LANCZOS)
    img.save(out_path, format="PNG", optimize=True)
    print(f"[ ok ] wrote {out_path.relative_to(REPO_ROOT)} ({size}x{size})")


def write_ico(master: Image.Image, out_path: Path) -> None:
    sizes = [(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
    master.save(out_path, format="ICO", sizes=sizes)
    print(f"[ ok ] wrote {out_path.relative_to(REPO_ROOT)} (multi-res ICO)")


def write_icns(master: Image.Image, out_path: Path) -> None:
    """Build a .icns via macOS iconutil, falling back to the .iconset dir."""
    iconset_dir = out_path.with_suffix(".iconset")
    if iconset_dir.exists():
        shutil.rmtree(iconset_dir)
    iconset_dir.mkdir(parents=True)

    # Apple-mandated icns variant set.
    variants = [
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
        (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"),
        (1024, "icon_512x512@2x.png"),
    ]
    for size, name in variants:
        img = master.resize((size, size), Image.LANCZOS)
        img.save(iconset_dir / name, format="PNG", optimize=True)

    iconutil = shutil.which("iconutil") or "/usr/bin/iconutil"
    if not Path(iconutil).exists():
        print(
            f"[warn] iconutil not found; left .iconset at {iconset_dir} "
            f"and skipped {out_path.name}. Run on macOS or supply a prebuilt .icns.",
            file=sys.stderr,
        )
        return

    try:
        subprocess.run(
            [iconutil, "-c", "icns", str(iconset_dir), "-o", str(out_path)],
            check=True,
            capture_output=True,
        )
        print(f"[ ok ] wrote {out_path.relative_to(REPO_ROOT)} (iconutil)")
    finally:
        shutil.rmtree(iconset_dir, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Regenerate the Tauri icon set for apps/aberp-ui."
    )
    parser.add_argument(
        "--source",
        type=Path,
        default=DEFAULT_SOURCE,
        help=f"path to source logo PNG (default: {DEFAULT_SOURCE.relative_to(REPO_ROOT)})",
    )
    args = parser.parse_args()

    ICONS_DIR.mkdir(parents=True, exist_ok=True)
    master = load_or_make_master(args.source)

    write_png(master, 32, ICONS_DIR / "32x32.png")
    write_png(master, 128, ICONS_DIR / "128x128.png")
    write_png(master, 256, ICONS_DIR / "128x128_2x.png")
    write_png(master, 128, ICONS_DIR / "icon.png")
    write_ico(master, ICONS_DIR / "icon.ico")
    write_icns(master, ICONS_DIR / "icon.icns")

    print()
    print(f"[done] icon set regenerated at {ICONS_DIR.relative_to(REPO_ROOT)}/")
    print("[next] cargo build --release --bin aberp-ui --features production")
    return 0


if __name__ == "__main__":
    sys.exit(main())
