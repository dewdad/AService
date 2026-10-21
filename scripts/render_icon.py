"""Render AService's vector mark to the raster formats used by native apps.

Requires Pillow: ``python -m pip install Pillow``. Keep this drawing and the SVG
template in sync when the mark changes.
"""
from io import BytesIO
from pathlib import Path

from PIL import Image, ImageDraw

ASSETS = Path(__file__).resolve().parents[1] / "assets"
SCALE = 8
NAVY = "#141f30"
TEAL = "#5ddcb7"

svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128">
  <rect width="128" height="128" rx="28" fill="{NAVY}"/>
  <g fill="none" stroke="{TEAL}" stroke-width="9" stroke-linecap="round">
    <circle cx="46" cy="72" r="20"/>
    <path d="M66 52v40M103 27 81 100"/>
  </g>
</svg>
'''
(ASSETS / "aservice.svg").write_text(svg, encoding="utf-8")

size = 128 * SCALE
image = Image.new("RGBA", (size, size), (0, 0, 0, 0))
draw = ImageDraw.Draw(image)
scale = lambda value: round(value * SCALE)
draw.rounded_rectangle(
    (0, 0, size - 1, size - 1), radius=scale(28), fill=NAVY
)
stroke = scale(9)
draw.ellipse(
    (scale(26), scale(52), scale(66), scale(92)), outline=TEAL, width=stroke
)
for start, end in (((66, 52), (66, 92)), ((103, 27), (81, 100))):
    p1 = tuple(scale(v) for v in start)
    p2 = tuple(scale(v) for v in end)
    draw.line((p1, p2), fill=TEAL, width=stroke)
    for point in (p1, p2):
        x, y = point
        draw.ellipse((x - stroke // 2, y - stroke // 2, x + stroke // 2, y + stroke // 2), fill=TEAL)

def raster(edge: int) -> Image.Image:
    return image.resize((edge, edge), Image.Resampling.LANCZOS)


png = raster(512)
png.save(ASSETS / "aservice.png", optimize=True)
raster(1024).save(
    ASSETS / "aservice.ico",
    format="ICO",
    sizes=[(16, 16), (20, 20), (24, 24), (32, 32), (40, 40), (48, 48), (64, 64), (128, 128), (256, 256)],
)

# ICNS uses PNG-backed chunks for modern macOS icon sizes.
types = [(16, b"icp4"), (32, b"icp5"), (64, b"icp6"), (128, b"ic07"), (256, b"ic08"), (512, b"ic09"), (1024, b"ic10")]
chunks = []
for edge, chunk_type in types:
    stream = BytesIO()
    raster(edge).save(stream, format="PNG", optimize=True)
    data = stream.getvalue()
    chunks.append(chunk_type + (len(data) + 8).to_bytes(4, "big") + data)
body = b"".join(chunks)
(ASSETS / "aservice.icns").write_bytes(b"icns" + (len(body) + 8).to_bytes(4, "big") + body)
