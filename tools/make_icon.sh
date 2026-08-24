#!/usr/bin/env bash
# Engendre assets/claudhub.ico à partir de assets/claudhub.svg.
#
# Le .ico est **versionné**, pas construit en CI : la jambe Windows n'a ni
# ImageMagick ni rsvg, et une icône est le genre de fichier qu'on regarde avant
# de le livrer. Ce script n'est donc à relancer que si le logo change.
#
# Sept tailles, et c'est Windows qui choisit : 16 pour l'explorateur en liste,
# 32 pour le bureau, 48 pour les grandes icônes, 256 pour la vignette et la
# page « Applications ». Une taille absente est interpolée par le système, et
# une interpolation de 256 vers 16 rend un pâté.
#
#   tools/make_icon.sh        (hors nix-shell : il y entre seul)
set -euo pipefail

PROJ=$(cd "$(dirname "$0")/.." && pwd)
SVG=$PROJ/assets/claudhub.svg
ICO=$PROJ/assets/claudhub.ico

# `-background none` avant `-density` : l'ordre compte, ImageMagick applique
# ces réglages au *chargement* du SVG, pas après.
nix-shell --quiet -p imagemagick --run "
  magick -background none -density 600 '$SVG' \
    -define icon:auto-resize=256,128,64,48,32,24,16 '$ICO'
"

ls -lh "$ICO"
