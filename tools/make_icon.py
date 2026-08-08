"""Draw The Editor's icon at each size it is actually displayed at.

Downscaling one large drawing turns thin strokes to mush at 16 px, which is
the size that matters most -- the taskbar and Explorer's list view. Each size
is drawn from the same proportions instead.
"""
from PIL import Image, ImageDraw

BG = (34, 38, 46)        # slate, near the dark theme's background
LINE = (122, 132, 148)   # muted "code"
CARET = (108, 182, 255)  # the accent blue used for links

def draw(size):
    # 4x supersample, then reduce: gives clean edges without blurring the
    # geometry, because the geometry is computed at the target size.
    s = size * 4
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    radius = s * 0.22
    d.rounded_rectangle([0, 0, s - 1, s - 1], radius=radius, fill=BG)

    # Two lines of "code" and a caret at the end of the second, which reads as
    # text-with-a-cursor even when it is twelve pixels across.
    pad = s * 0.22
    thickness = max(1, int(s * 0.085))
    gap = s * 0.20
    top = s * 0.34

    d.rounded_rectangle(
        [pad, top, pad + s * 0.40, top + thickness],
        radius=thickness / 2, fill=LINE)
    d.rounded_rectangle(
        [pad, top + gap, pad + s * 0.26, top + gap + thickness],
        radius=thickness / 2, fill=LINE)

    caret_x = pad + s * 0.32
    d.rounded_rectangle(
        [caret_x, top + gap - s * 0.06, caret_x + thickness * 1.1,
         top + gap + thickness + s * 0.06],
        radius=thickness / 2, fill=CARET)

    return img.resize((size, size), Image.LANCZOS)

sizes = [16, 24, 32, 48, 64, 128, 256]
images = [draw(n) for n in sizes]
images[0].save("assets/icon.ico", format="ICO",
               sizes=[(n, n) for n in sizes], append_images=images[1:])

# Raw RGBA for the window icon at runtime, so no PNG decoder is needed.
draw(64).save("assets/icon-64.png")
with open("assets/icon-64.rgba", "wb") as f:
    f.write(draw(64).tobytes())
print("wrote assets/icon.ico and assets/icon-64.rgba")
