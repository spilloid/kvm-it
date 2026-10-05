#!/usr/bin/env python3
"""Red-arrow tours: callouts drawn on the finished screenshots, so the docs pictures explain themselves.

  tools/screenshots/annotate.py <final-dir> <out-dir>

Coordinates are those of the post-processed (cropped) 1244x731 pictures. Each callout is a label in a white box with a red
border and a red arrow to the thing it names. Re-run after the screenshots change and check every picture by eye.
"""
import math, pathlib, sys
from PIL import Image, ImageDraw, ImageFont

RED = (222, 50, 40)
FONT = next(p for p in ("/usr/share/fonts/liberation-sans-fonts/LiberationSans-Bold.ttf",
                        "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf") if pathlib.Path(p).exists())

# file -> output name -> list of (label, (label x, label y), (arrow tip x, arrow tip y))
TOURS = {
    "01-overview.png": ("tour-overview.png", [
        ("1  Adapter", (24, 118), (62, 58)),
        ("2  Target USB", (150, 118), (211, 58)),
        ("3  Video", (322, 118), (366, 58)),
        ("4  Capture", (460, 118), (524, 58)),
        ("5  Keys \u00b7 Type \u00b7 Scripts", (610, 118), (690, 58)),
    ]),
    "08-capturing.png": ("tour-capture.png", [
        ("Ctrl+Alt+Esc gives you\nyour keyboard back", (770, 112), (700, 58)),
        ("Red frame: every key and click\ngoes to the target", (340, 620), (352, 718)),
    ]),
    "09-flash.png": ("tour-flash.png", [
        ("The firmware ships\nwith the app", (905, 245), (680, 340)),
        ("Your board's COM port\n(picked for you)", (905, 330), (800, 389)),
        ("One click.\nPairing is kept.", (905, 450), (443, 492)),
    ]),
}


def arrow(d: ImageDraw.ImageDraw, a, b, width=5):
    d.line([a, b], fill=RED, width=width)
    ang = math.atan2(b[1] - a[1], b[0] - a[0])
    for s in (-0.45, 0.45):  # arrow head
        d.line([b, (b[0] - 22 * math.cos(ang + s), b[1] - 22 * math.sin(ang + s))], fill=RED, width=width)


def callout(im: Image.Image, label: str, at, tip) -> None:
    d = ImageDraw.Draw(im)
    f = ImageFont.truetype(FONT, 19)
    box = d.multiline_textbbox(at, label, font=f, spacing=3)
    pad = 9
    r = (box[0] - pad, box[1] - pad, box[2] + pad, box[3] + pad)
    # arrow from the nearest edge of the box to the tip
    cx = min(max(tip[0], r[0] + 10), r[2] - 10)
    start = (cx, r[1]) if tip[1] < r[1] else (cx, r[3]) if tip[1] > r[3] else (r[0] if tip[0] < r[0] else r[2], (r[1] + r[3]) // 2)
    arrow(d, start, tip)
    d.rounded_rectangle(r, radius=9, fill=(255, 255, 255), outline=RED, width=3)
    d.multiline_text(at, label, font=f, fill=(150, 20, 15), spacing=3)


if __name__ == "__main__":
    src, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    for name, (dest, items) in TOURS.items():
        im = Image.open(src / name).convert("RGB")
        for label, at, tip in items:
            callout(im, label, at, tip)
        im.save(out / dest, optimize=True)
        print("ok ", dest)
