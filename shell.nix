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

  # Lier avec wild via le linker plutôt que via RUSTFLAGS : RUSTFLAGS entre dans
  # le hash des artefacts, si bien qu'un build dans le shell et un build hors du
  # shell (éditeur, agents) dupliquaient tout le graphe gpui dans target/.
  clangWild = pkgs.writeShellScript "clang-wild" ''
    exec ${pkgs.clang}/bin/clang --ld-path=${pkgs.wild}/bin/wild "$@"
  '';
in
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [ pkg-config clang wild cmake ];
  # git : Claudhub pilote le binaire `git` en sous-processus plutôt que de lier
  # libgit2, pour hériter exactement de la configuration de l'utilisateur
  # (credential helpers, hooks, includeIf, signature GPG/SSH).
  buildInputs = runtimeLibs ++ (with pkgs; [ openssl dbus zstd wl-clipboard git ]);

  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER = clangWild;
}
