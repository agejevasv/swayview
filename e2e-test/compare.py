"""Compares each format's screenshot with the XRGB8888 one, pixel by pixel.

Formats with fewer bits per channel round colors, so they may differ by up to
half a step of their coarsest channel.
"""

import sys
from pathlib import Path

import numpy as np

TOLERANCE = {"rgba4444": 9, "rgbx4444": 9, "rgba5551": 5, "rgbx5551": 5, "rgb565": 5}


def load(path):
    """A binary PPM, as grim writes it, as an array of RGB rows."""
    data = path.read_bytes()
    magic, w, h, maxval, pixels = data.split(maxsplit=4)
    assert magic == b"P6" and maxval == b"255"
    return np.frombuffer(pixels, np.uint8).reshape(int(h), int(w), 3).astype(int)


out = Path(sys.argv[1])
base = load(out / "xrgb8888" / "screenshot.ppm")
for run in sorted(p for p in out.iterdir() if (p / "screenshot.ppm").exists()):
    img = load(run / "screenshot.ppm")
    if img.shape != base.shape:
        print(f"{run.name}: screenshot size differs")
        continue
    tolerance = TOLERANCE.get(run.name, 1)
    diff = np.abs(img - base).max(axis=2)
    off = np.argwhere(diff > tolerance)
    if len(off) == 0:
        print(f"{run.name}: matches xrgb8888 (within {tolerance}, at most {diff.max()} off)")
    else:
        y, x = off[0]
        print(f"{run.name}: DIFFERS from xrgb8888 in {len(off)} pixels, by up to {diff.max()}, first at {x},{y}")
