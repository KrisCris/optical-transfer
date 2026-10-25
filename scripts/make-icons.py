#!/usr/bin/env python3
"""Rasterise the app icon into the PNGs that platforms insist on.

icons/icon.svg is the source of truth, but iOS ignores SVG icons entirely (it
wants a PNG apple-touch-icon, or it falls back to a letter on a grey tile), and
Android's installer wants PNGs in the manifest. The SVG is a handful of
rectangles, so this redraws the same geometry with Pillow rather than pulling
in an SVG renderer. Keep the two in step by hand.

The outputs are committed; run this again only when the icon changes. Needs
Pillow, which the shipped app does not.
"""

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw

ICONS = Path(__file__).resolve().parent.parent / "web" / "app" / "icons"

BACKGROUND = "#101418"
GREEN = "#3ddc84"
WHITE = "#f2f5f8"

# (x, y, size, corner radius, colour) on the SVG's 192-unit canvas.
SHAPES = [
    *[(x, y, 46, 6, GREEN) for x, y in ((34, 34), (112, 34), (34, 112))],
    *[(x, y, 18, 3, BACKGROUND) for x, y in ((48, 48), (126, 48), (48, 126))],
    *[(x, y, 18, 3, WHITE) for x, y in ((112, 112), (140, 140), (112, 140), (140, 112))],
]

# Drawn this many times larger, then downsampled, for anti-aliased edges.
SUPERSAMPLE = 8


def render(size: int, *, corner: float, scale: float) -> Image.Image:
    """corner is the outer radius in SVG units (0 for full bleed); scale shrinks
    the glyph about the centre, for masks that crop the edges."""
    big = size * SUPERSAMPLE
    unit = big / 192
    image = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle((0, 0, big - 1, big - 1), radius=corner * unit, fill=BACKGROUND)
    for x, y, side, radius, colour in SHAPES:
        left = (96 + (x - 96) * scale) * unit
        top = (96 + (y - 96) * scale) * unit
        extent = side * scale * unit
        draw.rounded_rectangle(
            (left, top, left + extent, top + extent), radius=radius * scale * unit, fill=colour
        )
    return image.resize((size, size), Image.LANCZOS)


def main() -> None:
    # iOS rounds the corners itself and fills transparency with black, so its
    # icon is full bleed.
    render(180, corner=0, scale=1).convert("RGB").save(ICONS / "apple-touch-icon.png", optimize=True)
    for size in (192, 512):
        render(size, corner=42, scale=1).save(ICONS / f"icon-{size}.png", optimize=True)
    # A maskable icon may be cropped to a circle of 40% radius; at 0.85 the
    # corners of the outer squares stay inside it.
    render(512, corner=0, scale=0.85).convert("RGB").save(ICONS / "icon-maskable-512.png", optimize=True)


if __name__ == "__main__":
    main()
