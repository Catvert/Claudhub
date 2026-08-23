#!/usr/bin/env python3
"""Rebuild the embedded Iosevka faces from the upstream packages.

The full faces weigh nine to eleven megabytes each, and there are four of
them: shipping them whole would add fifty megabytes to a seventy-five megabyte
binary, for coverage nobody reads in a diff. What is kept is what Claudhub
actually paints — Latin with its diacritics, Greek, Cyrillic, punctuation,
currencies, arrows, box drawing and block elements, plus the Nerd icons for
the monospace cut. A glyph left out is not an empty square on a machine that
has fonts: gpui falls back to the system's. It only costs on a bare one.

Run it outside nix-shell, after downloading the two packages:

    nix-shell -p python3Packages.fonttools --run "python3 tools/subset_fonts.py \
        --aile <IosevkaAile-{Regular,Bold}.ttf dir> \
        --mono <IosevkaNerdFontMono-{Regular,Bold}.ttf dir>"

Iosevka Aile comes from the upstream `PkgTTF-IosevkaAile` release; the
monospace cut comes from Nerd Fonts, whose patched files are what carry the
powerline and devicon glyphs a terminal prompt needs.
"""

import argparse
import pathlib

from fontTools import subset
from fontTools.ttLib import TTFont

# Latin and its diacritics, Greek, Cyrillic, punctuation, currencies, arrows,
# mathematical operators, technical symbols, box drawing, block elements,
# geometric shapes, miscellaneous symbols, dingbats, variation selectors.
#
# The symbols and the dingbats are there because of what a TUI writes: a check
# mark, a cross, and the four-pointed stars an agent's spinner cycles through
# (U+2722, U+2733, U+2736, U+273B) — and the bullet and the elbow it draws its
# transcript with (U+23FA, U+23BF). Left out, they were drawn by whatever font
# the system fell back to, at whatever width it pleased — which is what made a
# status line jitter left and right once a frame. The view now pins every run
# to its column, so the cost of a missing glyph is a shape that does not match
# its neighbours rather than a grid that moves; keeping them in is still the
# answer that looks right.
CORE = (
    "U+0000-024F,U+0300-036F,U+0370-03FF,U+0400-04FF,"
    "U+2000-206F,U+20A0-20BF,U+2190-21FF,U+2200-22FF,U+2300-23FF,"
    "U+2500-257F,U+2580-259F,U+25A0-25FF,U+2600-27BF,U+2900-29FF,U+2B00-2BFF,"
    "U+FE00-FE0F"
)
# Braille (TUI progress bars) and the private-use plane the Nerd patcher fills.
NERD = "U+2800-28FF,U+E000-F8FF"

AILE = "Iosevka Aile"
MONO = "Iosevka Nerd Font Mono"


def build(source: pathlib.Path, target: pathlib.Path, codes: str, family: str, style: str):
    subset.main(
        [
            str(source),
            f"--unicodes={codes}",
            "--layout-features=*",
            f"--output-file={target}",
        ]
    )
    # The Nerd patcher leaves the full family name in name ID 16 only; ID 1
    # says "Iosevka NFM". Which of the two a text system matches on is not
    # something to bet on — a name that misses resolves to a silent fallback —
    # so both are made to say the same thing.
    font = TTFont(target)
    names = font["name"]
    full = family if style == "Regular" else f"{family} {style}"
    for record in list(names.names):
        value = {1: family, 2: style, 4: full, 16: family, 17: style}.get(record.nameID)
        if value is not None:
            names.setName(value, record.nameID, record.platformID, record.platEncID, record.langID)
    font.save(target)
    print(f"{target}  {target.stat().st_size // 1024} KiB")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--aile", type=pathlib.Path, required=True,
                        help="directory holding IosevkaAile-Regular.ttf and -Bold.ttf")
    parser.add_argument("--mono", type=pathlib.Path, required=True,
                        help="directory holding IosevkaNerdFontMono-Regular.ttf and -Bold.ttf")
    parser.add_argument("--out", type=pathlib.Path,
                        default=pathlib.Path(__file__).parent.parent / "assets" / "fonts")
    args = parser.parse_args()

    build(args.aile / "IosevkaAile-Regular.ttf", args.out / "IosevkaAile.ttf", CORE, AILE, "Regular")
    build(args.aile / "IosevkaAile-Bold.ttf", args.out / "IosevkaAile-Bold.ttf", CORE, AILE, "Bold")
    build(args.mono / "IosevkaNerdFontMono-Regular.ttf", args.out / "IosevkaNerdFontMono.ttf",
          f"{CORE},{NERD}", MONO, "Regular")
    build(args.mono / "IosevkaNerdFontMono-Bold.ttf", args.out / "IosevkaNerdFontMono-Bold.ttf",
          f"{CORE},{NERD}", MONO, "Bold")


if __name__ == "__main__":
    main()
