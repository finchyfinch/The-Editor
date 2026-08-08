"""Draw The Editor's icon at each size it is actually displayed at.

Downscaling one large drawing turns thin strokes to mush at 16 px, which is the
size that matters most -- the taskbar and Explorer's list view. Every size is
drawn from the same proportions instead, so the 16 px version is a deliberate
drawing rather than a blurred 256 px one.

The mark is a burst: a bright core with rays radiating out of it. Long and
short rays alternate, because twelve rays of equal length read as a circle once
they are small, and the alternation keeps the star shape legible. The ray count
and the core size are the two numbers worth playing with.
"""

import math

from PIL import Image, ImageDraw, ImageFilter

BG = (24, 27, 34)          # near-black slate, so it sits on any wallpaper
CORE = (255, 236, 190)     # hot centre
MID = (255, 168, 64)       # the body of the burst
RAY = (255, 122, 48)       # rays, a shade deeper so they read against the core

RAYS = 12
LONG = 0.39                # long ray length, as a fraction of the icon
SHORT = 0.26               # short ray length


def draw(size):
    """One icon at `size` pixels, drawn 4x and reduced for clean edges."""
    s = size * 4
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    centre = s / 2

    d.rounded_rectangle([0, 0, s - 1, s - 1], radius=s * 0.22, fill=BG)

    # Rays first, so the core is drawn over their inner ends and the join is
    # hidden rather than showing as a seam.
    width = max(1, int(s * 0.055))
    for i in range(RAYS):
        angle = (2 * math.pi / RAYS) * i - math.pi / 2
        reach = (LONG if i % 2 == 0 else SHORT) * s
        # Start outside the core, or short rays vanish underneath it.
        inner = s * 0.13
        d.line(
            [
                centre + math.cos(angle) * inner,
                centre + math.sin(angle) * inner,
                centre + math.cos(angle) * reach,
                centre + math.sin(angle) * reach,
            ],
            fill=RAY,
            width=width,
        )

    # A soft halo under the core, which is what makes it read as light rather
    # than as a dot with sticks attached.
    halo = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(halo).ellipse(
        [centre - s * 0.26, centre - s * 0.26, centre + s * 0.26, centre + s * 0.26],
        fill=MID + (150,),
    )
    img.alpha_composite(halo.filter(ImageFilter.GaussianBlur(s * 0.05)))

    d = ImageDraw.Draw(img)
    d.ellipse(
        [centre - s * 0.19, centre - s * 0.19, centre + s * 0.19, centre + s * 0.19],
        fill=MID,
    )
    d.ellipse(
        [centre - s * 0.10, centre - s * 0.10, centre + s * 0.10, centre + s * 0.10],
        fill=CORE,
    )

    return img.resize((size, size), Image.LANCZOS)


SIZES = [16, 24, 32, 48, 64, 128, 256]
images = [draw(n) for n in SIZES]

# Save from the *largest* image. Pillow skips any requested size bigger than
# the one `save` was called on, without saying so, so saving from the 16 px
# drawing writes a one-image .ico and Explorer's tile view gets a scaled-up
# 16 px icon. Nothing warns you; the file just quietly contains one entry.
images[-1].save(
    "assets/icon.ico",
    format="ICO",
    sizes=[(n, n) for n in SIZES],
    append_images=images[:-1],
)
_written = Image.open("assets/icon.ico")
assert len(_written.ico.entry) == len(SIZES), (
    f"icon.ico has {len(_written.ico.entry)} images, expected {len(SIZES)}"
)

# Raw RGBA for the window icon at runtime, so no PNG decoder is needed.
draw(64).save("assets/icon-64.png")
with open("assets/icon-64.rgba", "wb") as f:
    f.write(draw(64).tobytes())

print("wrote assets/icon.ico and assets/icon-64.rgba")
