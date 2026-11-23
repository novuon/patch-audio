# The lock-screen logo, unlock.png, for Patch Dark and Patch Light: the
# Module alone (Patch's mark, docs/patch-style.md: a socket Unit with the
# red Unit plugged into it), on a transparent ground. No wordmark: a downloaded theme's unlock
# screen is no place to advertise an app. Omarchy draws it on the
# theme's background above the password field.
#
#   uv run --with pillow python unlock.py
import pathlib

from PIL import Image, ImageDraw

HERE = pathlib.Path(__file__).resolve().parent
INKS = {  # the socket Unit (Paper on Ink, Ink on Paper), the plug-in Unit
    "patch-dark": ("#f7f4ee", "#f0243b"),
    "patch-light": ("#141114", "#f0243b"),
}
PLYMOUTH = pathlib.Path("/usr/share/omarchy/default/plymouth")
W, H, SS = 800, 188, 4  # Omarchy's logo size; drawn 4x and scaled down


def module(d, x0, y0, x, ink, red):
    """The Module on unit `x` (the mark is 40x wide, 23x tall)."""
    clear = (0, 0, 0, 0)
    box = lambda l, t, r, b: [x0 + l * x, y0 + t * x, x0 + r * x, y0 + b * x]
    d.rounded_rectangle(box(0, 0, 23, 23), radius=1.2 * x, fill=ink)
    # The socket: 7x deep, 8x tall, on the shared centre, cut to the edge.
    d.rounded_rectangle(box(16, 7.5, 25, 15.5), radius=0.4 * x, fill=clear)
    # The plug: 8x long, 6x tall, 1x of air on its three sides.
    d.rounded_rectangle(box(17, 8.5, 26, 14.5), radius=0.4 * x, fill=red)
    d.rounded_rectangle(box(25, 4, 40, 19), radius=1.2 * x, fill=red)


def logo(ink, red):
    img = Image.new("RGBA", (W * SS, H * SS), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    x = 4 * SS
    module(d, 24 * SS, (H * SS - 23 * x) // 2, x, ink, red)
    # Centred: Omarchy centres the image, so the logo sits in its middle.
    mark = img.crop(img.getbbox())
    # As tall as the type in Omarchy's own logos.
    k = 104 * SS / mark.height
    mark = mark.resize((round(mark.width * k), round(mark.height * k)), Image.LANCZOS)
    framed = Image.new("RGBA", img.size, (0, 0, 0, 0))
    framed.paste(mark, ((img.width - mark.width) // 2, (img.height - mark.height) // 2))
    return framed.resize((W, H), Image.LANCZOS)


def colour(name, key):
    for line in (HERE / name / "colors.toml").read_text().splitlines():
        k, _, v = line.partition("=")
        if k.strip() == key:
            return v.strip().strip('"')
    raise KeyError(key)


def tinted(path, hex_, scale=1.0):
    img = Image.open(path).convert("RGBA")
    if scale != 1.0:
        img = img.resize((round(img.width * scale), round(img.height * scale)), Image.LANCZOS)
    flat = Image.new("RGBA", img.size, hex_)
    flat.putalpha(img.getchannel("A"))
    return flat


def preview(name, mark):
    """The unlock screen at 1920x1080, placed as Omarchy's own preview."""
    text = colour(name, "foreground")
    img = Image.new("RGBA", (1920, 1080), colour(name, "background"))
    img.alpha_composite(mark, (560, 446))
    img.alpha_composite(tinted(PLYMOUTH / "lock.png", text, 0.4), (769, 679))
    img.alpha_composite(tinted(PLYMOUTH / "entry.png", text), (817, 674))
    for i in range(4):
        img.alpha_composite(tinted(PLYMOUTH / "bullet.png", text, 0.5), (837 + 12 * i, 694))
    return img.convert("RGB")


for name, (ink, red) in INKS.items():
    mark = logo(ink, red)
    mark.save(HERE / name / "unlock.png")
    preview(name, mark).save(HERE / name / "preview-unlock.png")
    print(HERE / name)
