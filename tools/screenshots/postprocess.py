#!/usr/bin/env python3
"""Turn the harness's raw window captures into the pictures the site uses.

  tools/screenshots/postprocess.py <raw-dir> <out-dir>

- crops the invisible window border (a few pixels of desktop wallpaper bleed in at the left, right and bottom);
- pixelates the one place that shows the adapter's full Bluetooth address (the Adapter popup), a real hardware identifier.
Everything else is left exactly as the app drew it.
"""
import pathlib, sys
from PIL import Image

LEFT, TOP, RIGHT, BOTTOM = 8, 1, 8, 8           # border of a 1260x740 capture of the window, in pixels
ADDRESS = {"02-adapter.png": (20, 76, 135, 92)}  # the "28:84:85:86:xx:xx" line, in raw coordinates


def process(src: pathlib.Path, dst: pathlib.Path) -> None:
    im = Image.open(src).convert("RGB")
    box = ADDRESS.get(src.name)
    if box:
        patch = im.crop(box)
        small = patch.resize((max(1, patch.width // 9), max(1, patch.height // 9)), Image.BILINEAR)
        im.paste(small.resize(patch.size, Image.NEAREST), box)
    w, h = im.size
    im.crop((LEFT, TOP, w - RIGHT, h - BOTTOM)).save(dst, optimize=True)


if __name__ == "__main__":
    raw, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    for f in sorted(raw.glob("*.png")):
        process(f, out / f.name)
        print("ok ", f.name)
