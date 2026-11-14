"""Renders the app icon's PNGs, its macOS .iconset and its .icns.

usage: python3 -m pip install cairosvg && python3 assets/render.py

Everything here comes from assets/icon.svg (the tile) and assets/icon-mono.svg
(the mark alone), so the icon is edited in one place. The .icns is what the
DMG's volume icon uses (macos/dmg.sh); the PNGs are for docs and other
platforms. On Haiku the icon is a vector one instead — see haiku/mkicon.py.
"""
import os
import shutil
import subprocess

import cairosvg

here = os.path.dirname(os.path.abspath(__file__))
icon = os.path.join(here, "icon.svg")
mono = os.path.join(here, "icon-mono.svg")


def png(svg, path, w, h=None):
    cairosvg.svg2png(url=svg, write_to=path, output_width=w, output_height=h or w)
    print(f">> {os.path.relpath(path, here)}  {w}x{h or w}")


for size in (16, 32, 64, 128, 256, 512, 1024):
    png(icon, os.path.join(here, f"icon-{size}.png"), size)

# The banner logo: a 64px square, inset 2px like Temino's other agent marks,
# so the mark fills the cell instead of floating in it.
S, M = 60.0, 2.0
BW, BH = 304.0, 260.0  # icon-mono.svg's viewBox
sx, sy = S / BW, S / BH
scaled = os.path.join(here, ".icon-mono-64.svg")
with open(scaled, "w") as f:
    f.write(
        '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">'
        f'<g transform="translate({M - 104 * sx:.4f} {M - 126 * sy:.4f}) scale({sx:.6f} {sy:.6f})">'
        '<path d="M140 162 L254 254 L140 346" fill="none" stroke="#ffffff" stroke-width="72"'
        ' stroke-linecap="round" stroke-linejoin="round"/>'
        '<rect x="312" y="276" width="96" height="110" rx="26" fill="#ffffff"/>'
        "</g></svg>\n"
    )
png(scaled, os.path.join(here, "icon-mono-64.png"), 64)
os.remove(scaled)

# macOS: the .iconset iconutil wants, then the .icns itself.
def iconset():
    out = os.path.join(here, "HaikuCode.iconset")
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    for size in (16, 32, 128, 256, 512):
        png(icon, os.path.join(out, f"icon_{size}x{size}.png"), size)
        png(icon, os.path.join(out, f"icon_{size}x{size}@2x.png"), size * 2)
    icns = os.path.join(here, "haikucode.icns")
    subprocess.run(["iconutil", "-c", "icns", out, "-o", icns], check=True)
    shutil.rmtree(out)
    print(f">> {os.path.relpath(icns, here)}  {os.path.getsize(icns)} bytes")


if shutil.which("iconutil"):
    iconset()
else:
    print("!! iconutil not found: skipping haikucode.icns (it needs macOS)")
