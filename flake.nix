{
  description = "Claudhub — revue de code et pilotage d'agents, par worktree git";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      # Une seule architecture : la cible Linux est x86_64, et la cible Windows
      # ne se construit pas par nix.
      forAllSystems = nixpkgs.lib.genAttrs [ "x86_64-linux" ];
    in
    {
      packages = forAllSystems (system: {
        default = self.packages.${system}.claudhub;
        claudhub = nixpkgs.legacyPackages.${system}.callPackage ./nix/package.nix { };
      });

      # `shell.nix` reste la source de vérité : le justfile appelle `nix-shell`
      # et non `nix develop`, et deux listes de dépendances divergeraient au
      # premier ajout.
      devShells = forAllSystems (system: {
        default = import ./shell.nix { pkgs = nixpkgs.legacyPackages.${system}; };
      });

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt-tree);
    };
}
