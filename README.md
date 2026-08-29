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
- **GitHub** — les pull requests ouvertes du dépôt : pour chacune, ce qu'elle
  vise, qui l'a ouverte, l'état de ses vérifications et la décision de revue ;
  celle de la branche affichée est marquée. Un bouton en ouvre une quand la
  branche n'en a pas — titre et description proposés d'après ses commits, et la
  branche poussée au passage si elle ne l'avait jamais été. La bascule d'à côté
  montre les exécutions de la CI de la branche, avec le journal de celle qui a
  échoué et de quoi le confier à un agent. Tout passe par `gh`, déjà
  authentifié : Claudhub n'a aucun jeton à tenir.
- **Défilement** — chaque panneau porte sa barre, et la molette y est lissée :
  un cran glisse en une fraction de seconde au lieu de sauter de trois lignes,
  ce qui garde sa place à l'œil quand on relit. Un pavé tactile reste
  directement attaché au doigt.
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

Lancé depuis un dépôt git, Claudhub l'ouvre — ou depuis n'importe où, si on le
lui nomme : `claudhub ~/projets/machin`. Les dépôts ouverts sont rouverts au
démarrage suivant.

**Une seule fenêtre par machine.** Relancer Claudhub pendant qu'il tourne ne
donne pas une deuxième fenêtre : le dossier demandé est passé à celle qui est
là, qui l'ouvre et revient au premier plan. `CLAUDHUB_ALLOW_MULTIPLE=1` pour
outrepasser — c'est ce que `just run` pose, afin qu'un build de développement ne
rende jamais la main à un Claudhub installé.

## Sous Windows : une fenêtre native, les workers dans WSL2

Claudhub se découpe en deux sous Windows : l'interface est un **exécutable
natif** — vraie fenêtre, DirectX, polices du système —, et tout ce qui touche
au dépôt tourne dans une **distribution WSL2**. C'est le modèle de VS Code
Remote et de Zed : git, la surveillance de fichiers, les bases de données et
la détection des agents s'exécutent là où vit le code, et ne traversent
jamais la frontière autrement que par des messages.

WSLg a été essayé d'abord, et écarté : le rendu y passe par un Vulkan émulé
sur D3D12 qui n'est pas à la hauteur.

**L'installation tient en deux gestes.** Téléchargez
`Claudhub-Setup-x86_64.exe` de la dernière [version](../../releases) et
lancez-le. L'installeur ne demande **aucun droit d'administrateur** : il pose
Claudhub dans `%LOCALAPPDATA%\Programs`, met son icône sur le bureau et dans le
menu Démarrer, ajoute « Ouvrir avec Claudhub » au menu contextuel d'un dossier,
et s'inscrit dans « Applications installées » avec de quoi désinstaller. Les
deux raccourcis et le menu contextuel se décochent dans l'assistant.

« Ouvrir avec Claudhub » sur un dossier ouvre ce dépôt dans la fenêtre déjà
là — elle revient au premier plan — plutôt que d'en lancer une deuxième.

Qui préfère un fichier unique et rien d'installé prend `Claudhub-x86_64.exe` :
c'est le même programme, sans les raccourcis — on le lance depuis ses
téléchargements et il n'écrit que ses réglages.

Dans les deux cas, un seul fichier : le serveur est **dans** l'exécutable, qui
le pose dans la distribution au premier démarrage — après avoir demandé
laquelle — puis s'y connecte. Rien à compiler, rien à copier à la main, rien à
garder à côté, et les mises à jour s'installent d'elles-mêmes : le serveur est
adressé par l'empreinte de son contenu.

Ce qu'il faut avoir : **WSL2** avec une distribution, et dedans **git** ainsi
que l'agent que vous pilotez (`claude`, ou un autre). Il n'y a plus rien à
installer côté graphique — ni pilote Vulkan, ni polices, ni serveur X : c'est
Windows qui dessine.

Les terminaux s'ouvrent eux aussi dans la distribution : leur émulation reste
locale, mais le shell — et donc l'agent — tourne là-bas, dans le worktree, et
voit le dépôt. Les chemins se traduisent d'un monde à l'autre aux quelques
endroits où c'est nécessaire : le sélecteur de dossier accepte
`\\wsl.localhost\<distribution>\home\…`, « Ouvrir avec Claudhub » traduit le
dossier sur lequel on a cliqué, et le bouton qui ouvre le coffre de notes le
rend à l'explorateur Windows.

Et une règle qui n'est pas un détail de confort : **gardez vos dépôts dans le
système de fichiers Linux** (`~/projets/…`), jamais sous `/mnt/c`. Sur les
disques Windows montés par WSL, `inotify` ne remonte aucun événement — la
revue cesse de se rafraîchir toute seule, sans erreur — et `git status` y est
plusieurs fois plus lent. Claudhub reconnaît le cas et l'affiche dans la barre
d'état, mais le remède est de déplacer le dépôt.

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

La liste complète est dans l'application : **`F1`**, ou le menu ☰ ›
« Raccourcis clavier ». Elle est engendrée à partir des liaisons elles-mêmes,
donc elle ne peut pas mentir. Les principaux :

| Raccourci | Action |
| --- | --- |
| `F1` | afficher les raccourcis |
| `F5` / `Ctrl+R` | actualiser |
| `Ctrl+1` … `Ctrl+9` | aller au n-ième worktree |
| `Ctrl+B` | afficher/masquer la barre latérale |
| `Ctrl+Maj+R` / `Ctrl+Maj+U` / `Ctrl+Maj+P` | récupérer / tirer / publier |
| `Ctrl+Entrée` | valider |
| `Ctrl+Maj+I` | indexer ou dés-indexer le fichier ouvert |
| `↑` `↓` | bloc modifié précédent / suivant |
| `←` `→` | fichier précédent / suivant |
| `Début` `Fin` `Page↑` `Page↓` | parcourir le fichier |
| `Ctrl+C` / `Ctrl+Maj+C` | copier le code / le patch |
| `Ctrl+S` / `Ctrl+W` | enregistrer / fermer l'éditeur |
| `Ctrl+F` | chercher dans le panneau où l'on vient de cliquer |
| `Ctrl+G` / `Ctrl+Maj+G` | occurrence suivante / précédente |
| `Échap` | fermer la recherche |
| `Ctrl+Maj+N` | annoter les lignes sélectionnées |
| `Ctrl+Maj+K` | poser une question à l'agent sur la sélection |
| `Ctrl+Maj+E` | envoyer les remarques non traitées à l'agent |
| `Ctrl+Maj+T` / `Ctrl+Maj+W` | nouveau terminal / fermer l'onglet |
| ``Ctrl+` `` / `Ctrl+Tab` | afficher les terminaux / onglet suivant |
| `Ctrl+Maj+C` `Ctrl+Maj+V` `Ctrl+Maj+A` | copier, coller, tout sélectionner (terminal) |

Une liaison qui s'écrit avec la touche système et **une seule lettre** ne vaut
pas dans le terminal : `Ctrl+R` y reste la recherche arrière du shell, `Ctrl+S`
son XOFF. Ce qui demande Maj vaut partout, comme dans tout terminal.

### Mode vim

Désactivé par défaut ; Réglages › Clavier. Il ne remplace pas l'éditeur — il
donne la main gauche à la relecture : `j`/`k` d'une ligne à l'autre, `h`/`l`
d'un fichier à l'autre, `]c`/`[c` d'un bloc modifié au suivant, `gg`/`G`,
`Ctrl+D`/`Ctrl+U`, `y` pour copier, `/` puis `n`/`N` pour chercher. Les mêmes
touches parcourent l'arborescence du projet. Ce sont des lettres nues : elles
ne valent que là où ni un champ de saisie ni un terminal n'a le focus.

## Ce qui n'y est pas encore

- Résolution de conflits — les fichiers en conflit sont signalés, pas outillés.
- Recherche dans l'historique d'un terminal : `Ctrl+F` y appartient au
  programme qui tourne.
- Le défilement de l'éditeur intégré n'est pas lissé : sa poignée est privée
  dans gpui-component. Celui du terminal non plus — la grille d'alacritty se
  compte en lignes entières.
- Les langages sans grammaire embarquée s'affichent en texte nu. PHP est lié en
  direct ; les autres viennent de gpui-component.
- L'historique s'arrête à deux mille commits.

## Crédits

Les icônes d'interface viennent de [Lucide](https://lucide.dev) (ISC) ; les
logos de langages et d'outils, de [simple-icons](https://simpleicons.org)
(CC0), dans `assets/icons/lang/`. Les marques restent la propriété de leurs
titulaires : Claudhub s'en sert comme repères visuels dans ses listes de
fichiers.

## Licence

Apache-2.0.
