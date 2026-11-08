"""Rasterize the reviewed OpenCut SVG mark into a macOS application icon.

This intentionally supports the exact simple mark in brand/marks/icon.svg,
not arbitrary SVG. Keep the branded source and this deterministic raster together.
"""

from __future__ import annotations

import hashlib
import math
import struct
import sys
from pathlib import Path

from PIL import Image, ImageDraw


SOURCE = Path("brand/marks/icon.svg")
EXPECTED = "ab338b6515837b356b0c8aecb1196e78edb6b4f36157e81d33ab7810fb328016"
ROOT = Path("apps/desktop-launcher/assets")


def render() -> None:
    if hashlib.sha256(SOURCE.read_bytes()).hexdigest() != EXPECTED:
        raise SystemExit("OpenCut SVG changed; review and update the desktop icon rasterizer")

    dimension = 1024
    pixels = bytearray(dimension * dimension * 4)
    base = (244, 241, 243)
    gradients = (
        (0.0, 0.0, (79, 255, 255)),
        (512.0, 512.0, (247, 230, 144)),
        (0.0, 512.0, (185, 194, 242)),
        (512.0, 0.0, (255, 189, 221)),
    )
    for py in range(dimension):
        y = (py + 0.5) / 2
        for px in range(dimension):
            x = (px + 0.5) / 2
            color = list(base)
            for gx, gy, rgb in gradients:
                opacity = max(0.0, 1.0 - math.hypot(x - gx, y - gy) / 563.2)
                color = [color[i] * (1 - opacity) + rgb[i] * opacity for i in range(3)]
            offset = (py * dimension + px) * 4
            pixels[offset : offset + 4] = bytes((*[round(v) for v in color], 255))

    icon = Image.frombytes("RGBA", (dimension, dimension), bytes(pixels))
    # Super-sampled masks keep the clipped corners and logo edges smooth.
    aa = 2
    rounded = Image.new("L", (dimension * aa, dimension * aa))
    draw = ImageDraw.Draw(rounded)
    draw.rounded_rectangle((0, 0, dimension * aa - 1, dimension * aa - 1), radius=240 * aa, fill=255)
    rounded = rounded.resize((dimension, dimension), Image.Resampling.LANCZOS)
    icon.putalpha(rounded)

    mark = Image.new("L", (dimension * aa, dimension * aa))
    draw = ImageDraw.Draw(mark)
    pts = [(404, 193.993), (404, 317.007), (317.007, 404), (193.993, 404),
           (107, 317.007), (107, 193.993), (193.993, 107), (317.007, 107)]
    draw.polygon([(round(x * 2 * aa), round(y * 2 * aa)) for x, y in pts], fill=255)
    draw.rectangle((round(181.25 * 2 * aa), round(181.25 * 2 * aa),
                    round(329.75 * 2 * aa), round(329.75 * 2 * aa)), fill=0)
    mark = mark.resize((dimension, dimension), Image.Resampling.LANCZOS)
    icon.paste(Image.new("RGBA", icon.size, (0, 0, 0, 255)), mask=mark)

    ROOT.mkdir(parents=True, exist_ok=True)
    iconset = ROOT / "OpenCut.iconset"
    iconset.mkdir(exist_ok=True)
    for n in (16, 32, 128, 256, 512):
        icon.resize((n, n), Image.Resampling.LANCZOS).save(iconset / f"icon_{n}x{n}.png")
        icon.resize((n * 2, n * 2), Image.Resampling.LANCZOS).save(iconset / f"icon_{n}x{n}@2x.png")
    # ICNS supports PNG payloads directly. Write its documented chunk envelope
    # instead of relying on iconutil's host-specific iconset validation.
    chunks = (
        (b"icp4", "icon_16x16.png"),
        (b"icp5", "icon_32x32.png"),
        (b"icp6", "icon_32x32@2x.png"),
        (b"ic07", "icon_128x128.png"),
        (b"ic08", "icon_256x256.png"),
        (b"ic09", "icon_512x512.png"),
        (b"ic10", "icon_512x512@2x.png"),
        (b"ic11", "icon_16x16@2x.png"),
        (b"ic12", "icon_32x32@2x.png"),
        (b"ic13", "icon_128x128@2x.png"),
        (b"ic14", "icon_256x256@2x.png"),
    )
    payload = b"".join(code + struct.pack(">I", len(data) + 8) + data
                       for code, name in chunks for data in [(iconset / name).read_bytes()])
    (ROOT / "OpenCut.icns").write_bytes(b"icns" + struct.pack(">I", len(payload) + 8) + payload)
    if len(sys.argv) > 1 and sys.argv[1] == "--preview":
        icon.save(ROOT / "OpenCut-preview.png")


if __name__ == "__main__":
    render()
