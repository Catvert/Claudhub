{
  lib,
  rustPlatform,
  makeDesktopItem,
  copyDesktopItems,
  pkg-config,
  clang,
  wild,
  cmake,
  patchelf,
  openssl,
  dbus,
  zstd,
  wayland,
  libxkbcommon,
  libGL,
  fontconfig,
  freetype,
  vulkan-loader,
  libx11,
  libxcb,
  libxcursor,
  libxi,
  libxrandr,
  git,
}:

let
  # Chargées à l'exécution et non liées à l'édition de liens : gpui ouvre son
  # pilote Vulkan par `dlopen`, et le choix Wayland/X11 se fait au démarrage.
  # Elles vont donc dans le RPATH plutôt que dans une variable d'environnement
  # — `LD_LIBRARY_PATH` passerait aussi aux sous-processus, or Claudhub lance
  # `git`, `claude` et les shells de l'utilisateur, qui sont des programmes de
  # l'hôte et n'ont rien à faire de nos bibliothèques.
  runtimeLibs = [
    wayland
    libxkbcommon
    libGL
    vulkan-loader
    fontconfig
    freetype
    libx11
    libxcb
    libxcursor
    libxi
    libxrandr
  ];
in
rustPlatform.buildRustPackage {
  pname = "claudhub";
  # Lue dans Cargo.toml : écrite en dur, elle a laissé la 0.5.0 sortir sous un
  # chemin de store nommé 0.4.0 — aucune des quatre portes ne la voyait.
  version = (lib.importTOML ../Cargo.toml).package.version;

  src = lib.cleanSource ../.;

  # Le verrou contient huit dépôts git (gpui et ses satellites chez
  # zed-industries, le fork gpui-component, wt) : `fetchCargoVendor` les
  # rassemble sous un seul hash, là où `cargoLock.outputHashes` en demanderait
  # un par dépôt.
  #
  # Il change à **chaque** changement de `Cargo.lock` — le vendor en emporte
  # une copie —, donc y compris quand seul le numéro de version de Claudhub
  # bouge. Un `just ci` ne le dit pas : la porte qui le voit est `nix build`,
  # et elle n'est dans aucune des quatre. C'est ce qui a laissé la 0.2.1 sortir
  # avec un hash périmé.
  cargoHash = "sha256-vmjk6c+RJoDwXD1URcAa44tN7BzSaGtM0AaannmPs0o=";

  nativeBuildInputs = [
    pkg-config
    clang
    wild
    cmake
    patchelf
    copyDesktopItems
  ];

  buildInputs = [
    openssl
    dbus
    zstd
  ]
  ++ runtimeLibs;

  # Le même couple que `shell.nix` : lier l'arbre gpui avec l'éditeur de liens
  # par défaut se compte en minutes.
  env = {
    RUSTFLAGS = "-C linker=clang -C link-arg=--ld-path=wild";
    # Le crate openssl-sys compile sa propre copie s'il ne trouve rien ; ici il
    # y a une openssl du store, et deux openssl dans une closure est un bug qui
    # ne se voit qu'au premier CVE.
    OPENSSL_NO_VENDOR = 1;
  };

  # Trois tests lancent le vrai `git` — celui de `watch`, qui prouve toute la
  # chaîne de surveillance, et ceux de l'installation d'un plugin. Ils posent
  # leur propre `user.name`, donc le binaire suffit : ils tournent dans le bac
  # à sable plutôt que d'être sautés.
  nativeCheckInputs = [ git ];

  # Le logo, celui dont sortent aussi l'AppImage et l'icône Windows : un tracé
  # monochrome sur fond transparent — ce qu'était `icons/git-branch.svg` ici —
  # disparaît sur un dock de la même teinte.
  postInstall = ''
    install -Dm644 assets/claudhub.svg \
      $out/share/icons/hicolor/scalable/apps/claudhub.svg
  '';

  # `dlopen` ne regarde pas les dépendances déclarées : ce qui n'est pas dans
  # le RPATH n'est trouvé qu'avec un `LD_LIBRARY_PATH` posé par l'appelant,
  # c'est-à-dire jamais.
  postFixup = ''
    patchelf --add-rpath "${lib.makeLibraryPath runtimeLibs}" $out/bin/claudhub
  '';

  desktopItems = [
    (makeDesktopItem {
      name = "claudhub";
      desktopName = "Claudhub";
      comment = "Review and drive coding agents across git worktrees";
      exec = "claudhub";
      icon = "claudhub";
      terminal = false;
      categories = [ "Development" ];
    })
  ];

  meta = {
    description = "Poste de travail pour la revue de code et le pilotage d'agents de codage en terminal, par worktree git";
    homepage = "https://github.com/Catvert/Claudhub";
    license = lib.licenses.asl20;
    mainProgram = "claudhub";
    platforms = lib.platforms.linux;
  };
}
