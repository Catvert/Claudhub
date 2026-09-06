# GPUI Kit 0.6.0 migration

Claudhub uses the published `gpui-kit` 0.6.0 facade. GPUI and its platform
crates come from the matching published `gpui-pre` 0.3.x family (0.3.3 in
`Cargo.lock`), replacing the Zed Git dependencies. Only `gpui-base` and
`gpui-component` are overridden by the Claudhub fork.

The UI remains optional: `--no-default-features` builds the headless server
without GPUI. The application keeps its embedded fonts and icons; Kit's
optional asset re-export is disabled (the component layer still depends on
`gpui-kit-assets` internally). `tree-sitter-languages` preserves the
existing language coverage. Claudhub still registers its PHP queries for
Blade HTML/SQL injections and class constants, plus Nix, Dockerfile and Just.
The application already used separate Input, Textarea and Editor APIs before
this migration. Its Rust edition can remain 2021 while Kit uses edition 2024.

## Why the fork is still needed

Compared against the exact upstream [v0.6.0 tag](https://github.com/longbridge/gpui-kit/releases/tag/v0.6.0)
(`94a313a72a2513aee2780240cd322d552b2395f0`). The old Cargo.toml comment about
one tab-style patch was stale: the locked fork revision
`0f754153e14ca1aecfe27fd9eecf1276b540b5f2` had **34 commits**, including
follow-up fixes, above upstream `0e2fb7ac`. All 34 rebased without conflicts
onto v0.6.0; their combined diff still changes 29 files. The release still lacks the extension points listed below, so this upgrade
retains the existing series.

Several absent APIs are directly required by Claudhub:

| Retained extension | Claudhub caller / behavior |
| --- | --- |
| `DockSkin::set_tab_variant`, `set_panel_style_at`, `set_menu_button_visible` | `src/ui/app.rs`: segmented tabs, region-specific chrome and controls |
| `Panel::regions` / `DockRegions` | `src/ui/panels.rs`: documents stay in the center, tool windows on the edges |
| `Panel::tab_bar_trailing` / `TabBar::trailing` | `src/ui/panels.rs`: new-terminal button follows the final tab |
| `set_cursor_hidden`, `set_caret_block` and painted decoration backgrounds | `src/ui/surface.rs`: Vim block cursor and normal-mode caret behavior |
| `fold_candidates`, `set_folded` | `src/ui/surface.rs`: programmatic folding outside the gutter |
| `scroll_size`, `wrapped_row_count` | `src/ui/surface.rs`: scrolling limits and content-driven editor height |
| `WindowExt::focus_dialog` | `src/ui/settings_view.rs`: restore keyboard focus after changing settings pages |
| `Settings::on_select` | `src/ui/settings_view.rs`: remember the selected settings page |

The fork also preserves behavior without a new call site: dock card geometry
and spacing, resize targets on panel seams, split sizes surviving layout
reconciliation, rejection of cross-area drops, moving the last side panel,
hiding emptied dock regions, visible selection while a menu has focus,
truncated menu labels, scrollbar hit testing through overlays, smart-case
search, and Tab/Shift+Tab reaching the terminal outside modal surfaces.
Gutter-mark hooks remain part of the existing patch series as well.

Removing the fork today would both break compilation and lose these
interactions. Upstreaming these changes is still the route to removing it;
changing the version number alone cannot replace them.

## Reproducible dependency source

The fork is now [Catvert/gpui-kit](https://github.com/Catvert/gpui-kit). Its
default branch, `master`, contains the rebased series, merged at
`78aee01972ca9b845794d536e3691d718bc89e3d`. The original `claudhub` branch
is preserved. Cargo patches pin the series tip
`475a83b50844c2a209f76e73d72b20c6fe023c9d` for both layers; there
are no local path dependencies. The unchanged `gpui-component-macros` and
`gpui-kit-assets` packages follow the fork through its workspace dependencies.
Kit itself and GPUI remain registry packages.

For the next upgrade, rebase the fork's commits onto the new release tag,
check which public hooks and behavior fixes upstream now provides, and remove
superseded patches. Update both patch revisions together, regenerate
`Cargo.lock`, and refresh `nix/package.nix`'s `cargoHash`.

Linux validation on 2026-09-06: all-targets check, Clippy with warnings denied,
871 tests with UI enabled, 279 tests without default features, the headless
server check and formatting passed. Cargo also fetched and checked the exact
published fork revision after the temporary local overrides were removed.
The Nix vendor build passed with the refreshed `cargoHash`.

Validation commands:

```sh
just check
just clippy
just test
nix-shell --quiet --run 'cargo test --no-default-features'
just check-server
just fmt-check
just check-vendor
```

A Linux compile and test run does not validate Windows/macOS or interactive
rendering. The useful manual checks are dock dragging/resizing, terminal
Tab/Shift+Tab and the trailing + button, Vim carets/folds, and settings-page
focus restoration.
