"""
Makes the website's icons and its social preview picture from the program's
logo (assets/ in the repository root, made by tools/make_icons.py).

    python site/tools/make_images.py

Writes into site/public: favicon.ico, assets/img/{mark.png, icon-192.png,
icon-512.png, apple-touch-icon.png, og.png}. Requires Pillow and fontTools.
"""
import io
import pathlib

from fontTools.ttLib import TTFont
from fontTools.varLib import instancer
from PIL import Image, ImageDraw, ImageFont

SITE = pathlib.Path(__file__).resolve().parents[1]
REPOSITORY = SITE.parent
PUBLIC = SITE / "public"
IMAGES = PUBLIC / "assets" / "img"

INK = (10, 10, 15)
TEXT = (236, 235, 242)
MUTED = (162, 161, 176)
VIOLET = (124, 92, 255)
LINE = (38, 38, 48)


def font(weight: int, width: int, size: int) -> ImageFont.FreeTypeFont:
    """Archivo at a fixed weight and width, cut from the site's variable font."""
    variable = TTFont(PUBLIC / "assets" / "fonts" / "archivo-wdth.woff2")
    static = instancer.instantiateVariableFont(variable, {"wght": weight, "wdth": width})
    static.flavor = None
    buffer = io.BytesIO()
    static.save(buffer)
    buffer.seek(0)
    return ImageFont.truetype(buffer, size)


def mono(size: int) -> ImageFont.FreeTypeFont:
    variable = TTFont(PUBLIC / "assets" / "fonts" / "jetbrains-mono.woff2")
    static = instancer.instantiateVariableFont(variable, {"wght": 450})
    static.flavor = None
    buffer = io.BytesIO()
    static.save(buffer)
    buffer.seek(0)
    return ImageFont.truetype(buffer, size)


def crop_marks(draw: ImageDraw.ImageDraw, box: tuple[int, int, int, int], color, gap: int = 12, length: int = 14) -> None:
    left, top, right, bottom = box
    for x, y, dx, dy in [(left, top, -1, -1), (right, top, 1, -1), (left, bottom, -1, 1), (right, bottom, 1, 1)]:
        draw.line([(x + dx * gap, y), (x + dx * (gap + length), y)], fill=color, width=2)
        draw.line([(x, y + dy * gap), (x, y + dy * (gap + length))], fill=color, width=2)


def social_preview(icon: Image.Image) -> Image.Image:
    width, height = 1200, 630
    image = Image.new("RGB", (width, height), INK)
    draw = ImageDraw.Draw(image)
    # The faint layout grid.
    for x in range(60, width, 90):
        draw.line([(x, 0), (x, height)], fill=(20, 20, 27), width=1)
    box = (72, 72, width - 72, height - 72)
    crop_marks(draw, box, (70, 70, 84))
    tile = icon.resize((150, 150), Image.LANCZOS)
    image.paste(tile, (110, 110), tile)
    draw.text((290, 128), "quintess", font=font(700, 112, 64), fill=TEXT)
    draw.text((292, 210), "free · open source · windows", font=mono(24), fill=MUTED)
    headline = font(800, 125, 84)
    draw.text((108, 318), "PDFs, without", font=headline, fill=TEXT)
    draw.text((108, 410), "the friction.", font=headline, fill=TEXT)
    draw.text((width - 112, height - 112), "quintess.cc", font=mono(24), fill=VIOLET, anchor="rs")
    # A folded corner, as on the logo's page.
    fold = 70
    draw.polygon([(width - fold, 0), (width, 0), (width, fold)], fill=INK)
    draw.polygon([(width - fold, 0), (width - fold, fold), (width, fold)], fill=VIOLET)
    return image


def main() -> None:
    IMAGES.mkdir(parents=True, exist_ok=True)
    icon = Image.open(REPOSITORY / "assets" / "quintess.png").convert("RGBA")
    mark = Image.open(REPOSITORY / "assets" / "quintess-mark.png").convert("RGBA")
    small_mark = mark.copy()
    small_mark.thumbnail((80, 96), Image.LANCZOS)
    small_mark.save(IMAGES / "mark.png", optimize=True)
    for name, size in [("icon-192.png", 192), ("icon-512.png", 512), ("apple-touch-icon.png", 180)]:
        picture = icon.resize((size, size), Image.LANCZOS)
        if name == "apple-touch-icon.png":
            # iOS rounds the corners itself; give it a full square.
            square = Image.new("RGBA", (size, size), INK + (255,))
            square.alpha_composite(picture)
            picture = square
        picture.save(IMAGES / name, optimize=True)
    # The favicon comes from the program's own icon file, whose small sizes
    # were drawn separately for sharpness.
    program_icon = Image.open(REPOSITORY / "assets" / "quintess.ico")
    program_icon.save(PUBLIC / "favicon.ico", sizes=[(16, 16), (32, 32), (48, 48)])
    social_preview(icon).save(IMAGES / "og.png", optimize=True)
    print("wrote site images")


if __name__ == "__main__":
    main()
