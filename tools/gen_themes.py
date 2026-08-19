#!/usr/bin/env python3
"""Génère les thèmes de Perch au format de gpui-component.

Un thème complet compte une centaine de couleurs. Les écrire à la main pour
chaque palette serait une source d'erreurs muettes — une clé oubliée reprend
la valeur *claire* par défaut, et c'est une tache blanche au milieu d'un thème
sombre. On part donc d'une palette d'une quinzaine de couleurs, celles que les
auteurs de ces thèmes publient, et les rôles s'en déduisent ici.

    python3 tools/gen_themes.py

Les fichiers produits vont dans assets/themes/ et sont embarqués dans le
binaire.
"""

import json
import pathlib

OUT = pathlib.Path(__file__).resolve().parent.parent / "assets" / "themes"


def hex_to_rgb(value):
    value = value.lstrip("#")
    return tuple(int(value[i : i + 2], 16) for i in (0, 2, 4))


def rgb_to_hex(rgb):
    return "#" + "".join(f"{max(0, min(255, round(c))):02x}" for c in rgb)


def mix(a, b, t):
    """`t` = 0 rend `a`, `t` = 1 rend `b`."""
    ra, rb = hex_to_rgb(a), hex_to_rgb(b)
    return rgb_to_hex(tuple(x + (y - x) * t for x, y in zip(ra, rb)))


def alpha(color, a):
    return color + f"{round(a * 255):02x}"


class Palette:
    def __init__(self, name, mode, bg, surface, raised, fg, muted, accent,
                 red, green, yellow, blue, magenta, cyan, selection=None,
                 accent_fg=None):
        self.name = name
        self.mode = mode
        self.bg = bg
        self.surface = surface
        self.raised = raised
        self.fg = fg
        self.muted = muted
        self.accent = accent
        self.red = red
        self.green = green
        self.yellow = yellow
        self.blue = blue
        self.magenta = magenta
        self.cyan = cyan
        self.dark = mode == "dark"
        # La sélection doit rester lisible sous du texte coloré : elle est
        # tirée vers le fond plutôt que d'être l'accent pur.
        self.selection = selection or mix(accent, bg, 0.55 if self.dark else 0.65)
        self.accent_fg = accent_fg or (bg if not self.dark else self.contrast(accent))

    def contrast(self, color):
        r, g, b = hex_to_rgb(color)
        luma = (0.299 * r + 0.587 * g + 0.114 * b) / 255
        return "#101010" if luma > 0.55 else "#f8f8f8"

    def toward_bg(self, color, t):
        return mix(color, self.bg, t)

    def tint(self, color):
        """Fond discret d'un état sémantique."""
        return self.toward_bg(color, 0.78 if self.dark else 0.86)

    def on_tint(self, color):
        return mix(color, self.fg, 0.35)


def colors(p):
    return {
        "accent.background": p.raised,
        "accent.foreground": p.fg,
        "accordion.background": p.bg,
        "background": p.bg,
        "base.blue": p.blue,
        "base.blue.light": mix(p.blue, "#ffffff", 0.6),
        "base.cyan": p.cyan,
        "base.cyan.light": mix(p.cyan, "#ffffff", 0.6),
        "base.green": p.green,
        "base.green.light": mix(p.green, "#ffffff", 0.6),
        "base.magenta": p.magenta,
        "base.magenta.light": mix(p.magenta, "#ffffff", 0.6),
        "base.red": p.red,
        "base.red.light": mix(p.red, "#ffffff", 0.6),
        "base.yellow": p.yellow,
        "base.yellow.light": mix(p.yellow, "#ffffff", 0.6),
        "bearish.background": p.red,
        "border": p.raised,
        "bullish.background": p.green,
        "caret": p.accent,
        "chart_1": mix(p.blue, "#ffffff", 0.35),
        "chart_2": p.blue,
        "chart_3": p.cyan,
        "chart_4": p.magenta,
        "chart_5": p.green,
        "danger.background": p.red,
        "danger.active.background": p.toward_bg(p.red, 0.25),
        "danger.foreground": p.contrast(p.red),
        "danger.hover.background": alpha(p.red, 0.9),
        "description_list_label.background": p.surface,
        "description_list_label.foreground": p.fg,
        "drag_border": p.accent,
        "drop_target.background": alpha(p.accent, 0.25),
        "foreground": p.fg,
        "group_box.background": p.surface,
        "group_box.foreground": p.fg,
        "info.background": p.blue,
        "info.active.background": p.toward_bg(p.blue, 0.25),
        "info.foreground": p.contrast(p.blue),
        "info.hover.background": alpha(p.blue, 0.9),
        "input.border": p.raised,
        "link.foreground": p.accent,
        "link.active.foreground": p.toward_bg(p.accent, 0.2),
        "link.hover.foreground": mix(p.accent, "#ffffff", 0.25),
        "list.background": p.bg,
        "list.active.background": alpha(p.accent, 0.22),
        "list.active.border": p.accent,
        "list.even.background": alpha(p.surface, 0.8),
        "list.head.background": alpha(p.surface, 0.8),
        "list.hover.background": p.raised,
        "muted.background": p.raised,
        "muted.foreground": p.muted,
        "overlay": alpha("#000000" if not p.dark else "#ffffff", 0.04),
        "popover.background": p.surface,
        "popover.foreground": p.fg,
        "primary.background": p.accent,
        "primary.active.background": p.toward_bg(p.accent, 0.2),
        "primary.foreground": p.accent_fg,
        "primary.hover.background": mix(p.accent, "#ffffff", 0.12),
        "progress_bar.background": p.accent,
        "ring": p.accent,
        "scrollbar.background": alpha(p.surface, 0.0),
        "scrollbar.thumb.background": alpha(p.muted, 0.7),
        "scrollbar.thumb.hover.background": p.muted,
        "secondary.background": p.surface,
        "secondary.active.background": p.raised,
        "secondary.foreground": p.fg,
        "secondary.hover.background": mix(p.surface, p.raised, 0.5),
        "selection.background": p.selection,
        "sidebar.background": p.surface,
        "sidebar.border": p.raised,
        "sidebar.foreground": p.fg,
        "sidebar.accent.background": p.raised,
        "sidebar.accent.foreground": p.fg,
        "sidebar.primary.background": p.accent,
        "sidebar.primary.foreground": p.accent_fg,
        "skeleton.background": p.raised,
        "slider.bar.background": p.accent,
        "slider.thumb.background": p.bg,
        "success.background": p.green,
        "success.active.background": p.toward_bg(p.green, 0.25),
        "success.foreground": p.contrast(p.green),
        "success.hover.background": alpha(p.green, 0.9),
        "switch.background": p.raised,
        "tab.background": alpha(p.bg, 0.0),
        "tab.foreground": p.muted,
        "tab.active.background": p.bg,
        "tab.active.foreground": p.fg,
        "tab_bar.background": p.surface,
        "tab_bar.segmented.background": p.surface,
        "table.background": p.bg,
        "table.active.background": alpha(p.accent, 0.22),
        "table.active.border": p.accent,
        "table.even.background": alpha(p.surface, 0.8),
        "table.head.background": alpha(p.surface, 0.8),
        "table.head.foreground": p.muted,
        "table.hover.background": p.raised,
        "table.row.border": alpha(p.raised, 0.7),
        "tiles.background": p.surface,
        "title_bar.background": p.surface,
        "title_bar.border": p.raised,
        "warning.background": p.yellow,
        "warning.active.background": p.toward_bg(p.yellow, 0.25),
        "warning.foreground": p.contrast(p.yellow),
        "warning.hover.background": alpha(p.yellow, 0.9),
        "window.border": p.raised,
    }


def highlight(p):
    """Coloration du code.

    Le rôle de chaque famille suit la convention que ces thèmes partagent :
    mot-clé coloré, chaîne verte, commentaire éteint, type distinct de la
    fonction. C'est ce qui rend un fichier lisible dans un thème qu'on ne
    connaît pas encore.
    """
    return {
        "editor.foreground": p.fg,
        "editor.background": p.bg,
        "editor.active_line.background": p.surface,
        "editor.line_number": p.muted,
        "editor.active_line_number": p.fg,
        "conflict": p.yellow,
        "created": p.green,
        "created.background": p.tint(p.green),
        "deleted.background": p.tint(p.red),
        "modified": p.yellow,
        "modified.background": p.tint(p.yellow),
        "error.background": p.tint(p.red),
        "error.border": p.toward_bg(p.red, 0.4),
        "warning.background": p.tint(p.yellow),
        "warning.border": p.toward_bg(p.yellow, 0.4),
        "info.background": p.tint(p.blue),
        "info.border": p.toward_bg(p.blue, 0.4),
        "success.background": p.tint(p.green),
        "hidden": p.muted,
        "hint": p.toward_bg(p.cyan, 0.3),
        "hint.background": p.tint(p.cyan),
        "hint.border": p.toward_bg(p.cyan, 0.4),
        "predictive": p.muted,
        "syntax": {
            "attribute": {"color": p.yellow},
            "boolean": {"color": p.magenta},
            "comment": {"color": p.muted, "font_style": "italic"},
            "comment.doc": {"color": mix(p.muted, p.fg, 0.3), "font_style": "italic"},
            "constant": {"color": p.cyan},
            "constructor": {"color": p.yellow},
            "embedded": {"color": p.fg},
            "function": {"color": p.blue},
            "keyword": {"color": p.magenta},
            "link_text": {"color": p.blue},
            "link_uri": {"color": p.cyan},
            "number": {"color": p.cyan},
            "property": {"color": p.red},
            "string": {"color": p.green},
            "string.escape": {"color": p.cyan},
            "string.regex": {"color": p.cyan},
            "string.special": {"color": p.cyan},
            "string.special.symbol": {"color": p.cyan},
            "tag": {"color": p.red},
            "text.literal": {"color": p.green},
            "title": {"color": p.yellow, "font_weight": 700},
            "type": {"color": p.yellow},
            "variable.special": {"color": p.red},
        },
    }


# Palettes publiées par les auteurs de chaque thème. Seule la répartition des
# rôles est de nous.
PALETTES = [
    Palette("One Dark", "dark",
            bg="#282c34", surface="#21252b", raised="#3b4048", fg="#abb2bf",
            muted="#7f848e", accent="#61afef", red="#e06c75", green="#98c379",
            yellow="#e5c07b", blue="#61afef", magenta="#c678dd", cyan="#56b6c2"),
    Palette("One Light", "light",
            bg="#fafafa", surface="#f0f0f0", raised="#dcdcdc", fg="#383a42",
            muted="#8a8f98", accent="#4078f2", red="#e45649", green="#50a14f",
            yellow="#c18401", blue="#4078f2", magenta="#a626a4", cyan="#0184bc"),
    Palette("Nord", "dark",
            bg="#2e3440", surface="#3b4252", raised="#4c566a", fg="#eceff4",
            muted="#7b88a1", accent="#88c0d0", red="#bf616a", green="#a3be8c",
            yellow="#ebcb8b", blue="#81a1c1", magenta="#b48ead", cyan="#8fbcbb"),
    Palette("Dracula", "dark",
            bg="#282a36", surface="#21222c", raised="#44475a", fg="#f8f8f2",
            muted="#6272a4", accent="#bd93f9", red="#ff5555", green="#50fa7b",
            yellow="#f1fa8c", blue="#8be9fd", magenta="#ff79c6", cyan="#8be9fd"),
    Palette("Synthwave '84", "dark",
            bg="#262335", surface="#241b2f", raised="#495495", fg="#f8f8f2",
            muted="#848bbd", accent="#ff7edb", red="#fe4450", green="#72f1b8",
            yellow="#fede5d", blue="#36f9f6", magenta="#ff7edb", cyan="#36f9f6"),
    Palette("Tokyo Night", "dark",
            bg="#1a1b26", surface="#16161e", raised="#2f3549", fg="#c0caf5",
            muted="#565f89", accent="#7aa2f7", red="#f7768e", green="#9ece6a",
            yellow="#e0af68", blue="#7aa2f7", magenta="#bb9af7", cyan="#7dcfff"),
    Palette("Gruvbox Dark", "dark",
            bg="#282828", surface="#32302f", raised="#504945", fg="#ebdbb2",
            muted="#928374", accent="#83a598", red="#fb4934", green="#b8bb26",
            yellow="#fabd2f", blue="#83a598", magenta="#d3869b", cyan="#8ec07c"),
    Palette("Gruvbox Light", "light",
            bg="#fbf1c7", surface="#f2e5bc", raised="#d5c4a1", fg="#3c3836",
            muted="#7c6f64", accent="#076678", red="#9d0006", green="#79740e",
            yellow="#b57614", blue="#076678", magenta="#8f3f71", cyan="#427b58"),
    Palette("Catppuccin Mocha", "dark",
            bg="#1e1e2e", surface="#181825", raised="#313244", fg="#cdd6f4",
            muted="#7f849c", accent="#89b4fa", red="#f38ba8", green="#a6e3a1",
            yellow="#f9e2af", blue="#89b4fa", magenta="#cba6f7", cyan="#94e2d5"),
    Palette("Catppuccin Latte", "light",
            bg="#eff1f5", surface="#e6e9ef", raised="#ccd0da", fg="#4c4f69",
            muted="#8c8fa1", accent="#1e66f5", red="#d20f39", green="#40a02b",
            yellow="#df8e1d", blue="#1e66f5", magenta="#8839ef", cyan="#179299"),
    Palette("Solarized Dark", "dark",
            bg="#002b36", surface="#073642", raised="#0f4b58", fg="#93a1a1",
            muted="#586e75", accent="#268bd2", red="#dc322f", green="#859900",
            yellow="#b58900", blue="#268bd2", magenta="#d33682", cyan="#2aa198"),
    Palette("Solarized Light", "light",
            bg="#fdf6e3", surface="#eee8d5", raised="#ddd6c1", fg="#586e75",
            muted="#93a1a1", accent="#268bd2", red="#dc322f", green="#859900",
            yellow="#b58900", blue="#268bd2", magenta="#d33682", cyan="#2aa198"),
]


def slug(name):
    keep = [c.lower() if c.isalnum() else "-" for c in name]
    out = "".join(keep)
    while "--" in out:
        out = out.replace("--", "-")
    return out.strip("-")


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for palette in PALETTES:
        document = {
            "name": palette.name,
            "author": "Perch",
            "themes": [
                {
                    "name": palette.name,
                    "mode": palette.mode,
                    "colors": colors(palette),
                    "highlight": highlight(palette),
                }
            ],
        }
        path = OUT / f"perch-{slug(palette.name)}.json"
        path.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
        print(path.name)


if __name__ == "__main__":
    main()
