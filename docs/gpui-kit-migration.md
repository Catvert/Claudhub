# GPUI Kit migration

Claudhub uses the published `gpui-kit` 0.7.0 facade. GPUI and its platform
crates come from the matching published `gpui-pre` family, which Kit pins
exactly (`=0.3.7`). Only `gpui-base` and `gpui-component` are overridden by
the Claudhub fork.

The UI remains optional: `--no-default-features` builds the headless server
without GPUI. The application keeps its embedded fonts and icons; Kit's
optional asset re-export is disabled (the component layer still depends on
`gpui-kit-assets` internally). `tree-sitter-languages` preserves the
existing language coverage. Claudhub still registers its PHP queries for
Blade HTML/SQL injections and class constants, plus Nix, Dockerfile and Just.
Its Rust edition can remain 2021 while Kit uses edition 2024.

## What 0.7.0 changed in Claudhub

Read against the release notes of 0.6.1 to 0.7.0
([v0.7.0](https://github.com/longbridge/gpui-kit/releases/tag/v0.7.0)):

- **Window startup** goes through `gpui_kit::open_window`, which wraps the view
  in Base's `Root` (`src/ui/mod.rs`).
- **Kit hosts the dialog, sheet and notification layers itself**, as siblings
  of the application view. The root view no longer re-emits them. It had a
  consequence nothing reports: a dialog stopped being a descendant of
  `ClaudhubApp`'s element, so the action handlers on that element — the
  palette's `QuickUp`/`QuickOpen`, the review sheet's diff keys — no longer
  heard what a dialog dispatched. They moved to `app::Frame`, a `RootPlugin`
  whose `decorate` wraps the whole `Root`; the key context and the focus
  handle stay on the view (see the comment on `ClaudhubApp::frame`).
- **A dialog's `Fn` is no longer called from inside the root view's render**,
  so reading the application there is no longer the double-lease panic. The
  child entities built for that reason stay: they also carry state across
  frames.
- **`Theme::update`** replaces the manual token recomputation and
  `Theme::sync_base` in `theme::apply`.
- **`DockArea::select_panel`** (0.6.2) replaces the "move a panel onto its own
  index" workaround in `panels.rs`.
- **The tiles canvas is gone** (0.6.2): the `PaneRef::Tiles` arms went with it.

## Why the fork is still needed

The fork's 34 commits were rebased from v0.6.0 onto the exact upstream v0.7.0
tag (`0c830f4d257e69fdd17200650533ab4ca9a40cc0`). Upstream now covers part
of three of them, which were reduced rather than dropped:

- *Refuse a panel that is not the area's own*: upstream refuses it too (#3197);
  the commit keeps only its regression test.
- *A resize handle on the seam*: upstream sized the grab band correctly and
  moved a dock's handle inside the dock (#3200). What remains is
  `ResizeHandle::reach`, which carries the band across the gutter a skin puts
  between the docks and the centre.
- *Leave Tab to the focused view*: Tab navigation moved from `gpui-component`'s
  `Root` to `gpui-base`'s; the guard followed it, and reads the focus trap
  that dialogs and sheets now both register.

The selection-kept-on-blur and block-caret patches were adapted to upstream's
multi-cursor rendering (0.6.1).

| Retained extension | Claudhub caller / behavior |
| --- | --- |
| `DockSkin::set_tab_variant`, `set_panel_style_at`, `set_menu_button_visible` | `src/ui/app.rs`: segmented tabs, region-specific chrome and controls |
| `Panel::regions` / `DockRegions` | `src/ui/panels.rs`: documents stay in the center, tool windows on the edges |
| `Panel::tab_bar_trailing` / `TabBar::trailing` | `src/ui/panels.rs`: new-terminal button follows the final tab |
| `set_cursor_hidden`, `set_caret_block` and painted decoration backgrounds | `src/ui/surface.rs`: Vim block cursor and normal-mode caret behavior |
| `fold_candidates`, `set_folded` | `src/ui/surface.rs`: programmatic folding outside the gutter |
| `scroll_size`, `wrapped_row_count` | `src/ui/surface.rs`: scrolling limits and content-driven editor height |
| `WindowExt::focus_dialog` | `src/ui/dialogs.rs`, `src/ui/settings_view.rs`: put the keyboard back on a dialog |
| `Settings::on_select` | `src/ui/settings_view.rs`: remember the selected settings page |
| `ResizeHandle::reach` | the dock skin: grab the divider in the gutter beside a dock |
| `MessageScrollerState::list_state` | `src/ui/chat_view.rs`: the transcript's wheel eased (`scroll::wheel_capture`) rather than let jump |
| `gpui_shell::scroll_hook::observe_scroll_areas` (on `claudhub-v0.7.0-gpui-fast` only, with `gpui-shell` itself) | `src/ui/scripts.rs`: the scripts' lists and scroll areas get Claudhub's wheel smoothing |
| `gpui_shell::Capabilities::any_http_request` | `src/ui/scripts.rs`: every script may `fetch` any HTTP or HTTPS address, with no grant per host |

The fork also preserves behavior without a new call site: dock card geometry
and spacing, resize targets across panel gutters, split sizes surviving layout
reconciliation, moving the last side panel, hiding emptied dock regions,
visible selection while a menu has focus, truncated menu labels, scrollbar
hit testing through overlays, smart-case search, and Tab/Shift+Tab reaching
the terminal outside modal surfaces, and a divider's resting hairline in the
`resizable.handle` colour the application projects into base — transparent
in Claudhub, which parts its cards by a gutter and shows only the pill. Gutter-mark hooks remain part of the
series as well.

Upstream 0.7.0 adds editor range decorations
(`RangeDecorationCollection`, `RangeDecorationStyle::Fill`) that may make the
fork's "painted decoration backgrounds" patch unnecessary; moving
`surface.rs` onto them is left for a change of its own.

## Reproducible dependency source

The fork is [Catvert/gpui-kit](https://github.com/Catvert/gpui-kit). The
series for 0.7.0 is on branch `claudhub-v0.7.0`; Cargo patches pin its tip
`59b5176bc9d431741f5f3cb9a58d4588978e6094` for both layers, with no local
path dependency. The 0.6.0 series stays on `claudhub-v0.6.0` and the fork's
`master`. The unchanged `gpui-component-macros` and `gpui-kit-assets`
packages follow the fork through its workspace dependencies. Kit itself and
GPUI remain registry packages.

For the next upgrade, rebase the fork's commits onto the new release tag,
check which public hooks and behavior fixes upstream now provides, and remove
or reduce superseded patches. Update both patch revisions together,
regenerate `Cargo.lock`, and refresh `nix/package.nix`'s `cargoHash` — with a
fake hash first: see `just check-vendor`.

Linux validation on 2026-09-28: the fork's `gpui-base` (1240) and
`gpui-component` (570) library tests; for Claudhub, `just ci` (formatting,
Clippy with warnings denied, 1164 tests with UI enabled, the headless server
check), 457 tests without default features, and the Nix vendor build with the
refreshed `cargoHash`.

Validation commands:

```sh
just ci
nix-shell --quiet --run 'cargo test --no-default-features'
just check-vendor
```

A Linux compile and test run does not validate Windows/macOS or interactive
rendering. The useful manual checks are keyboard shortcuts inside dialogs
(the palette's arrows and Enter, the review sheet), dock dragging/resizing
(including dragging the bottom dock below its minimum, which now closes it),
terminal Tab/Shift+Tab and the trailing + button, Vim carets/folds, and
settings-page focus restoration.
