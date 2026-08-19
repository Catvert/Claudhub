# Chaque recette lance cargo dans `nix-shell`, qui fournit les bibliothèques
# système du build (Vulkan, Wayland/X11, fontconfig, freetype, dbus).
# Repli sur cargo nu là où nix-shell n'existe pas (Windows, macOS sans Nix).
_cargo := if `command -v nix-shell >/dev/null 2>&1 && echo yes || echo no` == "yes" { "nix-shell --quiet --run" } else { "sh -c" }

default: run

run:
    {{_cargo}} "cargo run"

release:
    {{_cargo}} "cargo run --release"

check:
    {{_cargo}} "cargo check --all-targets"

test:
    {{_cargo}} "cargo test"

build:
    {{_cargo}} "cargo build --release"

fmt:
    {{_cargo}} "cargo fmt"

clippy:
    {{_cargo}} "cargo clippy --all-targets -- -D warnings"

clean:
    cargo clean

shell:
    nix-shell
