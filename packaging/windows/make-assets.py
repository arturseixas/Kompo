#!/usr/bin/env python3
"""Regenerates the installer/app artwork from the logo (needs Pillow): python3 packaging/windows/make-assets.py

Writes packaging/windows/{kompo.ico,banner.bmp,dialog.bmp} and crates/kompo-app/assets/icon.png.
The logo is the same as packaging/icon.svg (dark rounded square, frame, blue play triangle).
"""
import pathlib
from PIL import Image, ImageDraw, ImageFont

ROOT = pathlib.Path(__file__).resolve().parents[2]
BG, FG, ACCENT, MUTED = "#141416", "#e8e8ea", "#4c8dff", "#8a8a90"
SS = 4  # supersampling


def logo(size: int) -> Image.Image:
    """Renders the 512-unit logo at `size` px with transparent corners."""
    s = size * SS / 512
    img = Image.new("RGBA", (size * SS, size * SS), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([0, 0, 512 * s - 1, 512 * s - 1], radius=112 * s, fill=BG)
    d.rounded_rectangle([96 * s, 150 * s, 416 * s, 362 * s], radius=22 * s, outline=FG, width=round(22 * s))
    d.polygon([(215 * s, 200 * s), (215 * s, 312 * s), (313 * s, 256 * s)], fill=ACCENT)
    for x0, x1 in ((96, 66), (416, 446)):
        d.line([(x0 * s, 256 * s), (x1 * s, 256 * s)], fill=MUTED, width=round(14 * s))
        for x in (x1,):
            r = 7 * s
            d.ellipse([x * s - r, 256 * s - r, x * s + r, 256 * s + r], fill=MUTED)
    return img.resize((size, size), Image.LANCZOS)


def font(px: int, bold=False):
    for name in ("DejaVuSans-Bold.ttf", "DejaVuSans.ttf", "LiberationSans-Regular.ttf"):
        try:
            return ImageFont.truetype(name if not bold or "Bold" in name else name, px)
        except OSError:
            continue
    return ImageFont.load_default()


# application / Windows icon (all sizes in one .ico) and the window icon embedded in the executable
sizes = [16, 24, 32, 48, 64, 128, 256]
logo(256).save(ROOT / "packaging/windows/kompo.ico", sizes=[(n, n) for n in sizes])
(ROOT / "crates/kompo-app/assets").mkdir(parents=True, exist_ok=True)
logo(256).save(ROOT / "crates/kompo-app/assets/icon.png")

# banner (493x58): light background so the dialog title (black) stays readable, logo on the right
banner = Image.new("RGB", (493, 58), "#ffffff")
banner.paste(logo(44), (493 - 44 - 10, 7), logo(44))
ImageDraw.Draw(banner).line([(0, 57), (493, 57)], fill="#d8d8dc")
banner.save(ROOT / "packaging/windows/banner.bmp")

# welcome / finish side panel (493x312): dark 164px strip with the logo and the name
dlg = Image.new("RGB", (493, 312), "#ffffff")
dd = ImageDraw.Draw(dlg)
dd.rectangle([0, 0, 163, 311], fill=BG)
dlg.paste(logo(96), (34, 70), logo(96))
name = "Kompo"
f = font(26, bold=True)
w = dd.textlength(name, font=f)
dd.text(((164 - w) / 2, 180), name, font=f, fill=FG)
sub = "Video editor"
f2 = font(12)
w2 = dd.textlength(sub, font=f2)
dd.text(((164 - w2) / 2, 216), sub, font=f2, fill=MUTED)
dlg.save(ROOT / "packaging/windows/dialog.bmp")
print("ok")
