# Claudhub

Poste de travail pour la **revue de code** et le **pilotage d'agents de codage
en terminal**, organisé par **worktree git**. Écrit en Rust sur
[gpui](https://www.gpui.rs/) (le framework d'interface de Zed) avec les widgets
[gpui-component](https://github.com/longbridge/gpui-component) — la même pile
que le projet Aviary.

L'idée tient en une phrase : une fenêtre où l'on voit, pour chaque worktree,
ce qu'un agent a écrit, avec de quoi le relire, le valider et lui reparler,
sans quitter l'application.

## Ce que fait Claudhub

- **Worktrees** — une barre latérale liste les dépôts ouverts et leurs
  worktrees. Création (`<dépôt>-wt/<nom>`, la convention de l'outil `wt`),
  suppression, bascule instantanée de l'un à l'autre.
- **Terminaux multiplexés par worktree** — chaque worktree a son groupe
  d'onglets, lancés dans son répertoire. Émulation complète (alacritty), donc
  `vim`, `htop` et les interfaces plein écran fonctionnent. Sélection à la
  souris (glisser, double-clic pour un mot, triple-clic pour une ligne), copie
  et collage — encadré par les séquences de « collage entre crochets » quand le
  programme les comprend, pour qu'un texte multiligne collé ne s'exécute pas
  tout seul. Un bouton lance directement l'agent de codage configuré dans un
  onglet neuf.
- **Revue** — deux domaines : les modifications en cours, et la branche entière
  depuis sa divergence d'avec une base **que l'on choisit** — la branche
  d'intégration est devinée au départ, le sélecteur (avec recherche) permet de
  comparer à `dev`, à une autre branche de travail ou à une distante (`base...HEAD`, donc sans le bruit de ce qui a
  atterri sur la base entre-temps). Le code est coloré par tree-sitter des deux
  côtés — une ligne supprimée d'après l'ancienne version du fichier, une ligne
  ajoutée d'après la nouvelle — et numéroté des deux côtés. L'affichage est
  virtualisé : un diff de plusieurs milliers de lignes défile sans peiner. À
  l'ouverture d'un worktree, Claudhub se place sur le domaine où il y a quelque
  chose à lire — un worktree propre s'ouvre sur la revue de sa branche.

  Les modifications en cours tiennent dans **une seule liste, avec une case par
  fichier** : la cocher indexe, la décocher retire de l'index, et c'est ce qui
  est coché qui part au commit. Les fichiers suivis et ceux qui ne le sont pas
  encore forment deux groupes, chacun avec sa case pour tout prendre d'un coup.
  Un fichier dont une partie seulement est indexée porte les deux codes de git
  (`MM`) et la mention « partiel » — l'unique cas où une case à cocher mentirait.
- **Git** — indexer/dés-indexer un fichier ou un seul bloc, abandonner des
  modifications (avec confirmation, c'est la seule action que git ne rattrape
  pas), valider, récupérer, tirer en avance rapide, publier avec
  `--set-upstream`.
- **Historique** — la liste des commits avec son graphe dessiné : une couleur
  par colonne, des courbes pour les rattachements de branche, les étiquettes de
  branches et de tags. Toutes les branches ou seulement la courante ; cliquer
  un commit affiche son diff, comparé à son premier parent.
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

Lancé depuis un dépôt git, Claudhub l'ouvre. Les dépôts ouverts sont rouverts au
démarrage suivant.

## Sous Windows : par WSL2

Claudhub est une application Linux ; sous Windows elle se lance **depuis WSL2**,
et WSLg l'affiche comme une fenêtre Windows ordinaire — barre de titre,
Alt+Tab, presse-papiers partagé. Il n'y a pas de serveur X tiers à installer.
Il n'y a pas non plus de version native : la détection des agents lit `/proc`,
les terminaux sont de vrais pty, et le build passe par Nix.

Prérequis :

- **Windows 11**, ou Windows 10 21H2 et plus avec WSL installé depuis le
  Microsoft Store — l'ancien WSL2 intégré à Windows 10 n'a pas WSLg.
- Un pilote **Vulkan** dans la distribution : gpui rend par Vulkan et n'a pas
  de repli. `sudo apt install mesa-vulkan-drivers libvulkan1 vulkan-tools`,
  puis `vulkaninfo --summary` doit nommer un pilote — `Microsoft Direct3D12`
  (le GPU, par Mesa/dozen) ou `llvmpipe` (le rendu logiciel, lent mais
  suffisant pour relire du code).
- Quelques **polices** : une distribution WSL nue en a presque aucune, et les
  listes de familles des réglages s'en ressentent
  (`sudo apt install fonts-jetbrains-mono fonts-dejavu`).

Et une règle qui n'est pas un détail de confort : **gardez vos dépôts dans le
système de fichiers Linux** (`~/projets/…`), jamais sous `/mnt/c`. Sur les
disques Windows montés par WSL, `inotify` ne remonte aucun événement — la revue
cesse de se rafraîchir toute seule, sans erreur — et `git status` y est
plusieurs fois plus lent. Claudhub reconnaît le cas et l'affiche dans la barre
d'état, mais le remède est de déplacer le dépôt. Les éditeurs Windows y accèdent
par `\\wsl$\<distribution>\home\…`, et VS Code s'y branche nativement.

## Réglages

`~/.config/claudhub/settings.json` (0600), toutes les clés facultatives :

| Clé | Défaut | Rôle |
| --- | --- | --- |
| `theme` | `dark` | `light`, `dark` ou `system` |
| `language` | `system` | `fr`, `en` ou `system` |
| `font_size` | 14 | taille de l'interface |
| `diff_context` | 3 | lignes de contexte autour d'un bloc |
| `terminal.shell` | vide | vide = le shell de connexion |
| `terminal.font_size` | 13 | |
| `terminal.scrollback` | 10000 | lignes d'historique par onglet |
| `terminal.agents` | un profil `claude` | profils d'agent : `name`, `command`, `args`, `env` |
| `terminal.default_agent` | le premier | profil lancé par le bouton « agent » |
| `external_editor` | vide | commande avec `{path}` et `{line}` |
| `update_with_rebase` | `false` | mettre à jour depuis la base par rebase |
| `sentry_org` | vide | organisation Sentry ; le projet se règle par dépôt |
| `sentry_token` | vide | jeton d'API, à défaut de `SENTRY_TOKEN` |
| `repositories` | `[]` | dépôts rouverts au démarrage |

`terminal.agent_command` a été remplacé par `terminal.agents` ; s'il est encore
là, il est repris en profil au premier lancement puis effacé.

L'`env` d'un profil est ce qui porte le modèle — `{"ANTHROPIC_MODEL": "…"}` —
plutôt qu'un réglage à part : Claudhub ne parle à aucune API, il lance un agent
qui, lui, a le dépôt entre les mains.

`~/.config/claudhub/state.json` (0600) range à côté ce qui n'est pas une
préférence : par worktree, la base de comparaison choisie, les dossiers repliés
et les remarques de relecture ; par dépôt, son projet Sentry.

Le jeton Sentry se lit **d'abord dans `SENTRY_TOKEN`**. Le fichier de réglages
est en 0600, ce qui ne fait pas de lui un coffre : préférez l'environnement.

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
| `Ctrl+1` / `Ctrl+2` | modifications / revue de branche |
| `Ctrl+H` | afficher/masquer l'historique |
| `Ctrl+S` | enregistrer le fichier ouvert |
| `Ctrl+Maj+N` | annoter les lignes sélectionnées |
| `Ctrl+Maj+K` | poser une question à l'agent sur la sélection |
| `Ctrl+Maj+E` | envoyer les remarques non traitées à l'agent |
| `Ctrl+Maj+C` | copier la sélection du terminal |
| `Ctrl+Maj+V` | coller dans le terminal |
| `Ctrl+Maj+A` | tout sélectionner dans le terminal |

## Ce qui n'y est pas encore

- Résolution de conflits — les fichiers en conflit sont signalés, pas outillés.
- Recherche dans un diff ou dans l'historique d'un terminal.
- Les langages sans grammaire embarquée s'affichent en texte nu. PHP est lié en
  direct ; les autres viennent de gpui-component.
- L'historique s'arrête à deux mille commits et ne se recherche pas.

## Licence

Apache-2.0.
