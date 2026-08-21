#!/usr/bin/env bash
# Construit target/appimage/Claudhub-x86_64.AppImage à partir du binaire release.
#
# Le binaire compilé sous nix-shell est lié contre la glibc du nix store, avec
# un interpréteur ELF codé en dur : il ne tourne sur aucune autre machine tel
# quel. L'AppImage embarque donc la closure complète — glibc et ld-linux
# compris — et son AppRun lance le loader embarqué explicitement. Les pilotes
# GPU (ICD Vulkan), eux, doivent venir de l'hôte : leurs chemins usuels sont en
# fin de --library-path, après les nôtres.
#
# À lancer depuis la racine du dépôt, HORS nix-shell (le script y entre seul) :
#   tools/make_appimage.sh
set -euo pipefail

PROJ=$(cd "$(dirname "$0")/.." && pwd)
cd "$PROJ"
BIN=$PROJ/target/release/claudhub
OUT=$PROJ/target/appimage
APPDIR=$OUT/Claudhub.AppDir
LIBDIR=$APPDIR/usr/lib
RUNTIME_URL=https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64

[ -f "$BIN" ] || { echo "pas de binaire release : lancer 'just build' d'abord" >&2; exit 1; }

mkdir -p "$OUT"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$LIBDIR"

# --- binaire, strippé (copie ; l'original n'est pas touché)
cp "$BIN" "$APPDIR/usr/bin/claudhub"
strip "$APPDIR/usr/bin/claudhub"

# --- closure des bibliothèques, résolue dans l'environnement du shell.nix.
# Graines : le binaire (ldd est transitif) + tout LD_LIBRARY_PATH — les
# bibliothèques chargées par dlopen (wayland, vulkan-loader…) n'apparaissent
# pas dans ldd. La glibc part au complet : les modules nss doivent être ceux
# de NOTRE libc, et c'est /etc/nsswitch.conf de l'hôte qui les choisira.
nix-shell --quiet --run '
  set -euo pipefail
  LIBDIR="'"$LIBDIR"'"
  declare -A copied
  copy_lib() {
    local base; base=$(basename "$1")
    [ -n "${copied[$base]:-}" ] && return 0
    copied[$base]=1
    cp -L "$1" "$LIBDIR/$base"
  }
  seeds=("'"$APPDIR"'/usr/bin/claudhub")
  IFS=: read -ra dirs <<< "$LD_LIBRARY_PATH"
  for d in "${dirs[@]}"; do
    for f in "$d"/*.so*; do [ -e "$f" ] && seeds+=("$f"); done
  done
  for s in "${seeds[@]}"; do
    case "$s" in */usr/bin/claudhub) ;; *) copy_lib "$s" ;; esac
    while read -r p; do [ -n "$p" ] && copy_lib "$p"; done \
      < <(ldd "$s" 2>/dev/null | awk "/=>/ && \$3 ~ /^\// {print \$3}")
  done
  GLIBC=$(dirname "$(ldd "'"$BIN"'" | awk "/libc\.so\.6/ {print \$3}")")
  for f in "$GLIBC"/*.so*; do copy_lib "$f"; done
  [ -f "$LIBDIR/ld-linux-x86-64.so.2" ] || { echo "ld-linux manquant" >&2; exit 1; }
'

# --- AppRun : le loader embarqué, nos bibliothèques d'abord, l'hôte ensuite.
# --library-path ne s'exporte pas : les sous-processus (git, claude, shells)
# restent des programmes de l'hôte lancés avec les bibliothèques de l'hôte.
cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/lib/ld-linux-x86-64.so.2" \
  --library-path "$HERE/usr/lib:/usr/lib/x86_64-linux-gnu:/usr/lib64:/usr/lib:/run/opengl-driver/lib" \
  "$HERE/usr/bin/claudhub" "$@"
EOF
chmod +x "$APPDIR/AppRun"

cat > "$APPDIR/claudhub.desktop" <<'EOF'
[Desktop Entry]
Name=Claudhub
Comment=Review and drive coding agents across git worktrees
Exec=claudhub
Icon=claudhub
Type=Application
Categories=Development;
Terminal=false
EOF

# Par nix-shell comme le squashfs plus bas : ImageMagick n'est pas dans le
# shell du projet, et la machine qui construit — un runner de CI, notamment —
# n'a aucune raison de l'avoir, ni sous ce nom-là.
nix-shell --quiet -p imagemagick --run \
  "magick -background none -density 300 '$PROJ/assets/icons/git-branch.svg' \
    -resize 256x256 -gravity center -extent 256x256 '$APPDIR/claudhub.png'"
ln -sf claudhub.png "$APPDIR/.DirIcon"

# --- empaquetage : squashfs + runtime type 2 concaténés — c'est exactement ce
# que fait appimagetool, absent du nixpkgs épinglé. Le runtime trouve le
# squashfs par la taille de son propre ELF, la concaténation nue suffit.
[ -f "$OUT/runtime-x86_64" ] || wget -q "$RUNTIME_URL" -O "$OUT/runtime-x86_64"
nix-shell --quiet -p squashfsTools --run \
  "mksquashfs '$APPDIR' '$OUT/claudhub.squashfs' -root-owned -noappend -comp zstd -Xcompression-level 19 -b 1M -quiet"
cat "$OUT/runtime-x86_64" "$OUT/claudhub.squashfs" > "$OUT/Claudhub-x86_64.AppImage"
chmod +x "$OUT/Claudhub-x86_64.AppImage"
rm -f "$OUT/claudhub.squashfs"

# --- variante sans FUSE : archive auto-extractrice. L'AppImage exige un
# fusermount sur la cible, qui manque plus souvent qu'on ne croit ; le .run ne
# demande que sh, tar et gzip. Il se déballe une fois dans ~/.cache — le
# contenu est adressé par son empreinte, un nouveau build se déballe à côté et
# les anciens sont purgés — puis exec l'AppRun déballé, donc lancement
# instantané dès le deuxième.
tar -czf "$OUT/claudhub.tar.gz" -C "$APPDIR" .
PAYLOAD_HASH=$(sha256sum "$OUT/claudhub.tar.gz" | cut -c1-16)
{
  cat <<EOF
#!/bin/sh
set -e
SELF=\$(readlink -f "\$0")
CACHE="\${XDG_CACHE_HOME:-\$HOME/.cache}/claudhub-bundle"
DEST="\$CACHE/$PAYLOAD_HASH"
if [ ! -x "\$DEST/AppRun" ]; then
  rm -rf "\$DEST.tmp"
  mkdir -p "\$DEST.tmp"
  tail -n +__LINES__ "\$SELF" | gzip -dc | tar -x -C "\$DEST.tmp"
  if ! mv -T "\$DEST.tmp" "\$DEST" 2>/dev/null; then
    [ -x "\$DEST/AppRun" ] || mv "\$DEST.tmp" "\$DEST"
    rm -rf "\$DEST.tmp"
  fi
  for d in "\$CACHE"/*; do
    [ "\$d" = "\$DEST" ] || rm -rf "\$d"
  done
fi
exec "\$DEST/AppRun" "\$@"
EOF
} > "$OUT/stub.sh"
# la ligne où commence le payload : celles du stub, plus une
sed -i "s/__LINES__/$(( $(wc -l < "$OUT/stub.sh") + 1 ))/" "$OUT/stub.sh"
cat "$OUT/stub.sh" "$OUT/claudhub.tar.gz" > "$OUT/Claudhub-x86_64.run"
chmod +x "$OUT/Claudhub-x86_64.run"
rm -f "$OUT/stub.sh" "$OUT/claudhub.tar.gz"

ls -lh "$OUT/Claudhub-x86_64.AppImage" "$OUT/Claudhub-x86_64.run"
