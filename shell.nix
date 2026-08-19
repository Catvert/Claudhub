{ pkgs ? import <nixpkgs> {} }:

let
  runtimeLibs = with pkgs; [
    wayland
    libxkbcommon
    libGL
    fontconfig
    freetype
    libx11
    libxcursor
    libxi
    libxrandr
    libxcb
    # gpui rend via blade (Vulkan)
    vulkan-loader
  ];
in
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [ pkg-config clang wild cmake ];
  # git : Perch pilote le binaire `git` en sous-processus plutôt que de lier
  # libgit2, pour hériter exactement de la configuration de l'utilisateur
  # (credential helpers, hooks, includeIf, signature GPG/SSH).
  buildInputs = runtimeLibs ++ (with pkgs; [ openssl dbus zstd wl-clipboard git ]);

  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
  RUSTFLAGS = "-C linker=clang -C link-arg=--ld-path=wild";
}
