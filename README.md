# Perch

Poste de travail pour la **revue de code** et le **pilotage d'agents de codage
en terminal**, organisé par **worktree git**. Écrit en Rust sur
[gpui](https://www.gpui.rs/) (le framework d'interface de Zed) avec les widgets
[gpui-component](https://github.com/longbridge/gpui-component) — la même pile
que le projet Aviary.

L'idée tient en une phrase : une fenêtre où l'on voit, pour chaque worktree,
ce qu'un agent a écrit, avec de quoi le relire, le valider et lui reparler,
sans quitter l'application.

## Ce que fait Perch

- **Worktrees** — une barre latérale liste les dépôts ouverts et leurs
  worktrees. Création (`<dépôt>-wt/<nom>`, la convention de l'outil `wt`),
  suppression, bascule instantanée de l'un à l'autre.
- **Terminaux multiplexés par worktree** — chaque worktree a son groupe
  d'onglets, lancés dans son répertoire. Émulation complète (alacritty), donc
  `vim`, `htop` et les interfaces plein écran fonctionnent. Un bouton lance
  directement l'agent de codage configuré dans un onglet neuf.
- **Revue** — quatre domaines de comparaison : modifications non indexées,
  index, tout le checkout contre HEAD, et la branche entière depuis sa
  divergence d'avec sa base (`base...HEAD`, donc sans le bruit de ce qui a
  atterri sur la base entre-temps). Diff coloré, numéroté des deux côtés.
- **Git** — indexer/dés-indexer un fichier ou un seul bloc, abandonner des
  modifications (avec confirmation, c'est la seule action que git ne rattrape
  pas), valider, récupérer, tirer en avance rapide, publier avec
  `--set-upstream`.
- **Branches** — liste locale et distante avec leur dernier commit et leur
  écart à l'amont, bascule, création, et création d'un worktree depuis une
  branche existante.
- **Rafraîchissement automatique** — le worktree affiché est surveillé. Un
  agent qui écrit des fichiers, un `git commit` tapé dans le terminal intégré :
  la revue suit sans qu'on lui demande.

Interface en **français et en anglais**, thèmes clair et sombre.

## Construire et lancer

Tout passe par `nix-shell`, qui fournit les bibliothèques système dont gpui a
besoin (Vulkan, Wayland/X11, fontconfig, freetype, dbus) :

```sh
just          # build debug + lancement
just release  # build optimisé + lancement
just check    # cargo check --all-targets
just test     # les tests unitaires
just clippy   # clippy -D warnings
just fmt
```

Sans Nix, les recettes retombent sur `cargo` nu ; à vous d'avoir les
bibliothèques dans le périmètre. gpui rend via Vulkan : sans `vulkan-loader`
accessible, rien ne s'affiche.

Lancé depuis un dépôt git, Perch l'ouvre. Les dépôts ouverts sont rouverts au
démarrage suivant.

## Réglages

`~/.config/perch/settings.json` (0600), toutes les clés facultatives :

| Clé | Défaut | Rôle |
| --- | --- | --- |
| `theme` | `dark` | `light`, `dark` ou `system` |
| `language` | `system` | `fr`, `en` ou `system` |
| `font_size` | 14 | taille de l'interface |
| `diff_context` | 3 | lignes de contexte autour d'un bloc |
| `terminal.shell` | vide | vide = le shell de connexion |
| `terminal.font_size` | 13 | |
| `terminal.scrollback` | 10000 | lignes d'historique par onglet |
| `terminal.agent_command` | `claude` | ce que lance le bouton « agent » |
| `repositories` | `[]` | dépôts rouverts au démarrage |

## Raccourcis

Tous passent par la touche système (Ctrl sous Linux et Windows, Cmd sous
macOS), parce que le reste du clavier appartient au programme qui tourne dans
le terminal.

| Raccourci | Action |
| --- | --- |
| `F5` / `Ctrl+R` | actualiser |
| `Ctrl+Maj+T` | nouveau terminal |
| `Ctrl+Maj+W` | fermer l'onglet de terminal |
| ``Ctrl+` `` | afficher/masquer le panneau des terminaux |
| `Ctrl+Tab` | onglet de terminal suivant |
| `Ctrl+Entrée` | valider |

## Ce qui n'y est pas encore

- Sélection à la souris et copier-coller dans le terminal.
- Coloration syntaxique du contenu des diffs (la grammaire `diff` de
  tree-sitter est déjà dans le graphe de dépendances, elle n'est pas branchée).
- Écran de préférences : le fichier de réglages s'édite à la main.
- Résolution de conflits — les fichiers en conflit sont signalés, pas outillés.
- Défilement virtualisé des très gros diffs : au-delà de quelques milliers de
  lignes, le rendu peine.

## Licence

Apache-2.0.
