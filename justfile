# Chaque recette lance cargo dans `nix-shell`, qui fournit les bibliothèques
# système du build (Vulkan, Wayland/X11, fontconfig, freetype, dbus).
# Repli sur cargo nu là où nix-shell n'existe pas (Windows, macOS sans Nix).
_cargo := if `command -v nix-shell >/dev/null 2>&1 && echo yes || echo no` == "yes" { "nix-shell --quiet --run" } else { "sh -c" }

default: run

# `CLAUDHUB_ALLOW_MULTIPLE` : sans lui, lancer une deuxième fois pendant qu'un
# Claudhub est ouvert donnerait la main à celui-là et rendrait aussitôt la main
# — le build qu'on vient de faire ne s'afficherait nulle part, sans rien dire.
# Pour éprouver l'instance unique elle-même, lancer le binaire directement.
run:
    {{_cargo}} "CLAUDHUB_ALLOW_MULTIPLE=1 cargo run --bin claudhub"

release:
    {{_cargo}} "CLAUDHUB_ALLOW_MULTIPLE=1 cargo run --release --bin claudhub"

check:
    {{_cargo}} "cargo check --all-targets"

# Le portillon du serveur headless : prouve qu'aucun module du cœur ne tire
# gpui. C'est ce build (sans la feature `ui`) qui part dans la distro WSL2.
check-server:
    {{_cargo}} "cargo check --no-default-features --bin claudhub-server"

test:
    {{_cargo}} "cargo test"

# Les portes, dans l'ordre où la CI les lance. À passer avant de poser un tag :
# ce sont exactement les mêmes, et les découvrir dans la CI coûte un build gpui
# complet par essai.
ci: fmt-check clippy test check-server

# La porte qu'aucune des quatre ne voit : le `cargoHash` du paquet nix change à
# chaque changement de `Cargo.lock`, **y compris quand seul le numéro de version
# bouge** — le vendor en emporte une copie. Rien ne le signale, et c'est ce qui
# a laissé la 0.2.1 sortir avec un hash périmé.
#
# Ne compile rien : seul le vendor est bâti, quelques minutes contre la
# demi-heure d'un `nix build` entier. Hors de `ci`, qui doit tourner là où il
# n'y a pas de nix.
check-vendor:
    nix build .#claudhub.cargoDeps --no-link

fmt-check:
    {{_cargo}} "cargo fmt --check"

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
