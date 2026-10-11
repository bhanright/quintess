"""
Builds the icon font the app uses where Segoe Fluent Icons is missing
(Linux, and Windows 10 without the font): assets/fonts/quintess-icons.otf.

    python tools/make_icon_font.py MaterialIconsOutlined-Regular.otf MaterialIconsOutlined-Regular.codepoints

The source is Google's Material Icons (Outlined), Apache License 2.0, from
https://github.com/google/material-design-icons/tree/master/font (the .otf
and its .codepoints file). The app names its icons by Segoe Fluent Icons
code points (see `icon` in crates/quintess-app/src/theme.rs), so this keeps
only the glyphs the app shows and moves each to the code point of the Segoe
icon it stands in for. Then no drawing code changes between the two fonts.

Requires fontTools (pip install fonttools).
"""
import pathlib
import sys

from fontTools import subset
from fontTools.ttLib import TTFont

ROOT = pathlib.Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "assets" / "fonts" / "quintess-icons.otf"

# Segoe Fluent Icons code point -> Material Icons name, in the order of the
# `icon` module in theme.rs. Keep the two lists in step.
SEGOE_TO_MATERIAL = {
    0xE710: "add",
    0xE711: "close",
    0xE712: "more_horiz",
    0xE721: "search",
    0xE7A7: "undo",
    0xE7A6: "redo",
    0xE8A3: "zoom_in",
    0xE71F: "zoom_out",
    0xE9A6: "swap_horiz",  # fit width
    0xE740: "fit_screen",  # fit page
    0xE76B: "chevron_left",
    0xE76C: "chevron_right",
    0xE708: "dark_mode",
    0xE706: "light_mode",
    0xE8B0: "near_me",  # pointer
    0xED5F: "pan_tool",
    0xE8AC: "edit_note",
    0xE7E6: "highlight",
    0xE8DC: "format_underlined",
    0xEDE0: "strikethrough_s",
    0xE70B: "sticky_note_2",
    0xE8D2: "text_fields",
    0xE739: "crop_square",  # rectangle
    0xEA3A: "radio_button_unchecked",  # ellipse
    0xE738: "horizontal_rule",  # line
    0xE72A: "north_east",  # arrow
    0xEDFB: "draw",
    0xE8D3: "approval",  # stamp
    0xE71B: "link",
    0xE727: "hide_source",  # redact
    0xEE56: "history_edu",  # sign
    0xE946: "manage_search",  # inspect
    0xE91B: "image",
    0xECC6: "straighten",  # measure
    0xE81E: "layers",
    0xE929: "input",  # form field
    0xE73A: "check_box",
    0xE8A9: "library_books",  # pages
    0xE728: "bookmarks",
    0xE90A: "chat_bubble_outline",  # comments
    0xF0E3: "assignment",  # form
    0xE723: "attach_file",
    0xF0E2: "grid_view",  # organize
    0xE8A0: "view_sidebar",
    0xE90D: "vertical_split",
    0xE8A5: "description",  # document
    0xE72E: "lock",
    0xE8EC: "sell",  # tags
}


def read_codepoints(path):
    """Name -> code point, from the font's .codepoints file ("add e145")."""
    names = {}
    for line in pathlib.Path(path).read_text(encoding="utf-8").splitlines():
        parts = line.split()
        if len(parts) == 2:
            names.setdefault(parts[0], int(parts[1], 16))
    return names


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    source, codepoints_file = sys.argv[1], sys.argv[2]
    codepoints = read_codepoints(codepoints_file)
    missing = [name for name in SEGOE_TO_MATERIAL.values() if name not in codepoints]
    if missing:
        sys.exit(f"not in the source font: {', '.join(missing)}")

    font = TTFont(source)
    source_cmap = font.getBestCmap()
    glyph_for_segoe = {segoe: source_cmap[codepoints[name]] for segoe, name in SEGOE_TO_MATERIAL.items()}

    # Keep only the glyphs used, without the ligature tables that let the
    # source font turn words such as "search" into icons.
    options = subset.Options()
    options.layout_features = []
    options.name_IDs = ["*"]
    options.notdef_outline = True
    subsetter = subset.Subsetter(options)
    subsetter.populate(glyphs=sorted(set(glyph_for_segoe.values())))
    subsetter.subset(font)

    # Point the Segoe code points at the kept glyphs, and nothing else.
    for table in font["cmap"].tables:
        if table.isUnicode():
            table.cmap = dict(glyph_for_segoe)

    # A name of its own, so it never passes for the original.
    for record in font["name"].names:
        if record.nameID in (1, 16):
            record.string = "Quintess Icons"
        elif record.nameID in (3, 4):
            record.string = "Quintess Icons Regular"
        elif record.nameID == 6:
            record.string = "QuintessIcons-Regular"
    if "CFF " in font:
        cff = font["CFF "].cff
        cff.fontNames = ["QuintessIcons-Regular"]
        top = cff.topDictIndex[0]
        top.FullName = "Quintess Icons Regular"
        top.FamilyName = "Quintess Icons"

    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    font.save(OUTPUT)
    print(f"wrote {OUTPUT.relative_to(ROOT)} with {len(glyph_for_segoe)} icons ({OUTPUT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
