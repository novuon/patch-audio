#!/usr/bin/env python3
# Move an image's red (and a video made from it) onto Patch's brand red.
#
# Used once, 2026-10-28, when the brand red became #f0243b (docs/brand.md):
# every wallpaper, intro, theme preview and remote screenshot went through
# it. Only red hues move. Greys, violet, indigo and amber stay exactly as
# they were, so the art is untouched apart from its red.
#
# Each image is measured first: the median hue of its saturated reds is
# moved onto the brand's hue (OKLCH 23.0°) and chroma rises by the brand's
# ratio over the old red, keeping lightness. A fixed shift was tried and
# overshot to orange on wallpapers whose red already sat near the new hue.
# The shift becomes a 65³ 3D LUT that ffmpeg applies; an intro takes its
# wallpaper's LUT, so the last frame still lands on the wallpaper.
#
#   uv run --with numpy --with pillow python themes/recolour.py IMAGE [VIDEO...]
#
# Writes the files in place (keep the originals in git). Needs ffmpeg.
import os
import shutil
import subprocess
import sys
import tempfile

import numpy as np
from PIL import Image

N = 65
TARGET_HUE = 23.0                 # #f0243b in OKLCH
CHROMA = 0.2317 / 0.2130          # #f0243b over the old #e43055
M1 = np.array([[0.4122214708, 0.5363325363, 0.0514459929],
               [0.2119034982, 0.6806995451, 0.1073969566],
               [0.0883024619, 0.2817188376, 0.6299787005]])
M2 = np.array([[0.2104542553, 0.7936177850, -0.0040720468],
               [1.9779984951, -2.4285922050, 0.4505937099],
               [0.0259040371, 0.7827717662, -0.8086757660]])


def to_lab(rgb):
    lin = np.where(rgb <= 0.04045, rgb / 12.92, ((rgb + 0.055) / 1.055) ** 2.4)
    return np.cbrt(lin @ M1.T) @ M2.T


def to_rgb(lab):
    return ((lab @ np.linalg.inv(M2).T) ** 3) @ np.linalg.inv(M1).T


def encode(lin):
    lin = np.clip(lin, 0, 1)
    return np.where(lin <= 0.0031308, 12.92 * lin, 1.055 * lin ** (1 / 2.4) - 0.055)


def lch(rgb):
    lab = to_lab(rgb)
    return lab[:, 0], np.hypot(lab[:, 1], lab[:, 2]), np.degrees(np.arctan2(lab[:, 2], lab[:, 1]))


def lut(image):
    """The image's LUT, or None when it has no red to move."""
    _, c, h = lch(np.asarray(Image.open(image).convert("RGB")).reshape(-1, 3) / 255)
    red = (c > 0.10) & (h > -25) & (h < 45)
    if red.sum() < 200:
        return None
    centre = float(np.median(h[red]))
    g = np.linspace(0, 1, N)
    b, gg, r = np.meshgrid(g, g, g, indexing="ij")    # .cube order: red fastest
    rgb = np.stack([r, gg, b], -1).reshape(-1, 3)
    l, c, h = lch(rgb)
    # Full effect within 18° of the image's red, none past 40°; greys stay.
    w = np.clip((40 - np.abs((h - centre + 180) % 360 - 180)) / 22, 0, 1)
    w = w * w * (3 - 2 * w) * np.clip(c / 0.06, 0, 1)
    c2, h2 = c * (1 + w * (CHROMA - 1)), np.radians(h + w * (TARGET_HUE - centre))
    lin = lambda cc: to_rgb(np.stack([l, cc * np.cos(h2), cc * np.sin(h2)], -1))
    fits = lambda x: np.all((x >= -1e-5) & (x <= 1 + 1e-5), 1)
    lo, hi = np.zeros_like(c2), c2.copy()             # out of gamut: least chroma lost
    for _ in range(20):
        mid = (lo + hi) / 2
        ok = fits(lin(mid))
        lo, hi = np.where(ok, mid, lo), np.where(ok, hi, mid)
    cf = np.where(fits(lin(c2)), c2, lo)
    print(f"{image}: red {centre:.1f}° -> {TARGET_HUE}°")
    return np.where(w[:, None] > 0, encode(lin(cf)), rgb)


def ffmpeg(src, cube, args, suffix):
    fd, tmp = tempfile.mkstemp(suffix=suffix, dir=os.path.dirname(os.path.abspath(src)))
    os.close(fd)
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", src,
                    "-vf", f"lut3d=file={cube}:interp=tetrahedral" + ("" if suffix == ".png" else ",format=yuv420p"),
                    *args, tmp], check=True)
    shutil.copymode(src, tmp)
    os.replace(tmp, src)


def main():
    image, videos = sys.argv[1], sys.argv[2:]
    table = lut(image)
    if table is None:
        print(f"{image}: no red")
        return
    with tempfile.NamedTemporaryFile("w", suffix=".cube") as cube:
        cube.write(f"LUT_3D_SIZE {N}\n")
        cube.writelines("%.6f %.6f %.6f\n" % tuple(o) for o in table)
        cube.flush()
        ffmpeg(image, cube.name, ["-pix_fmt", "rgb24"], ".png")
        for video in videos:
            # CRF 18 keeps the intros at their original size.
            ffmpeg(video, cube.name, ["-c:v", "libx264", "-preset", "slow", "-crf", "18",
                                      "-profile:v", "high", "-movflags", "+faststart", "-an"], ".mp4")


if __name__ == "__main__":
    main()
