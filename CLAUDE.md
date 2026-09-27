# CLAUDE.md

Guide de ce dépôt. On y écrit ce qui ne se lit **pas** dans le code : la carte
des modules, les règles qui traversent plusieurs fichiers, et les pièges dont
la violation ne produit aucune erreur. Le reste — pourquoi telle ligne est là,
ce qu'un piège a coûté, ce qu'il y avait avant — vit en commentaire à l'endroit
qu'il concerne.

**Ce fichier tient sous mille lignes.** Un changement n'a sa place ici que s'il
déplace la structure ; sinon, le commentaire suffit.

## Commandes

Tout passe par `nix-shell` via le `justfile` ; n'appelez `cargo` directement que
si les bibliothèques de `shell.nix` sont déjà dans le périmètre.

- `just` / `just run` — build debug et lancement
- `just check` / `just clippy` (`-D warnings`) / `just fmt` / `just test`
- `just check-server` — le serveur headless sans la feature `ui` : le portillon
  qui prouve qu'aucun module du cœur ne tire gpui
- `just ci` — les quatre d'un coup
- `just check-vendor` — **avant de poser un tag** : le `cargoHash` du paquet nix
  change à chaque changement de `Cargo.lock`, numéro de version compris, et
  aucune des quatre portes ne le voit
- Un test isolé : `nix-shell --quiet --run "cargo test watch"`

Le projet doit passer `cargo fmt --check`, `clippy --all-targets -- -D warnings`
et `cargo test` en permanence.

## Distribution

- **Le binaire release ne tourne que sur cette machine** : compilé sous
  `nix-shell`, il est lié contre la glibc du nix store. `tools/make_appimage.sh`
  (**hors** nix-shell, après un build release) produit l'AppImage et une
  archive auto-extractrice. Les pilotes GPU (ICD Vulkan) viennent de
  l'**hôte** — embarquer un Mesa casserait les machines NVIDIA —, et rien
  n'exporte `LD_LIBRARY_PATH` : les sous-processus (`git`, `claude`, les
  shells) restent des programmes de l'hôte.
- **Le paquet nix** (`flake.nix`, `nix/package.nix`) pose les bibliothèques de
  rendu en **RPATH**, pour la même raison, et `devShells.default` **importe
  `shell.nix`** au lieu de recopier ses dépendances.
- **L'installeur Windows** (`tools/claudhub.iss`, Inno Setup) s'installe **sans
  droits d'administrateur** dans `%LOCALAPPDATA%\Programs` ; son `AppId` ne
  change **jamais** (deux GUID laissent deux Claudhub, dont un qu'on ne
  désinstalle plus) ; le script est en **UTF-8 avec BOM** (sinon mojibake,
  sans erreur) ; la version se lit dans l'exécutable, où `build.rs` a mis celle
  de `Cargo.toml`.
- **L'icône** : `assets/claudhub.svg`, engendrée en `.ico` **versionné** par
  `tools/make_icon.sh` (la jambe Windows n'a pas ImageMagick), posée en
  ressource par `build.rs`.
- **La CI ne construit que des versions** (`.github/workflows/release.yml`, tag
  `v*` ou manuel) et relance les portes avant d'empaqueter : **Core tests** (le
  cœur sans `ui`, plus `check-server`) et **Interface tests** (l'arbre entier,
  dont la table des raccourcis, où `KeyBinding::new` **panique** au démarrage
  sur une touche illisible). La jambe du serveur musl finit avant celle de
  Windows, qui l'embarque par `CLAUDHUB_EMBED_SERVER`. Les fichiers vont sur
  une release en brouillon.

## Architecture

Trois couches, et une règle qui les sépare : **seule `src/ui/` connaît gpui, et
elle ne fait jamais d'entrée-sortie**.

Le crate est une **bibliothèque et deux binaires** : `claudhub` (l'interface) et
`claudhub-server` (les mêmes workers, headless, derrière stdin/stdout — pour
WSL2 quand l'interface est un `.exe` Windows). La feature `ui`, active par
défaut, porte gpui, gpui-component, alacritty et tout ce qui s'affiche ; le
serveur se construit avec `--no-default-features`, et un module du cœur qui
toucherait à gpui — `tr!` compris — casse ce build.

```
build.rs        embarque le serveur musl (`CLAUDHUB_EMBED_SERVER`), et sous
                Windows l'icône et la version en ressource
src/
  lib.rs        les modules, l'i18n et `tr!` (feature `ui`)
  main.rs       le binaire de l'interface : le verrou d'instance, puis `ui::run`
  bin/server.rs le serveur headless
  cmdline.rs    découpe et recompose une ligne de commande (guillemets POSIX)
  wsl.rs        la distro : la lister, y installer le serveur, l'y lancer
  wslpath.rs    chemins Windows ⇄ distro WSL, textuel et pur
  commit_msg.rs le message de commit proposé : prompt, nettoyage, agent
  files.rs      lire, écrire (sous condition), ranger, éditeur externe
  instance.rs   une seule fenêtre par machine, et le dossier qu'on lui passe
  release.rs    une version plus récente est-elle publiée — pur, testé
  logging.rs    `env_logger` sur stderr, et un anneau de 2000 lignes en mémoire
  outside.rs    ce qu'on demande au monde hors du dépôt : HTTP, une commande
  github.rs     ce que `gh` répond : exécutions, jobs, fils de revue, PR — pur
  sentry.rs     les erreurs d'un projet Sentry : URL, réponses, filtre, prompt — pur
  wt.rs         le `wt.toml` d'un projet : questions, tâches, statut, URLs
  just.rs       les recettes du `justfile`, lues par `just --dump` (JSON)
  suite.rs      les suites de tests (Pest, Vitest, Jest) : relevé, filtre exact,
                run suivi, prompt d'un échec — pur
  canvas.rs     un nœud de l'accueil sur disque : l'en-tête, le nom — pur
  skill.rs      la skill Claude Code livrée : où, quelle version, l'installer
  agent.rs      les agents dans `/proc`, et le suivi (travaille, fini, attend)
  agent_hooks.rs  les hooks de Claude Code : la ligne shell, le fichier lu, la
                fusion dans `.claude/settings.local.json`
  db/           bases de données — `sqlx`, testable sans gpui
    mod.rs      connexions, schémas, résultats ; le choix du moteur
    scope.rs    quelles bases appartiennent au worktree regardé
    sql.rs      lire une requête comme du texte : tables, alias, clause
    link.rs     suivre une clé étrangère
    complete.rs ce que la console propose en tapant
    sqlite.rs / mysql.rs  les deux moteurs
  lsp/          le client de serveur de langage — un processus par worktree
    mod.rs      la session : poignée de main, requêtes en vol, ce qu'il pousse
    frame.rs    le cadrage `Content-Length` et JSON-RPC
    sync.rs     versions de document, édition à une plage (UTF-16)
    uri.rs      chemins ⇄ `file://`
  git/          couche git — sous-processus `git`, testable sans gpui
    mod.rs      exécution (stdin fermé, LC_ALL=C, pas de pager)
    repo.rs     découverte, worktrees, écritures (stage, commit, push…)
    status.rs   `status --porcelain=v2 -z` → index et worktree séparés
    branch.rs   branches, amont, divergence ; la base devinée, le point de départ
    diff.rs     `--numstat` et diff unifié → fichiers, hunks, lignes
    history.rs  `git log` → commits, et la disposition du graphe
    outline.rs  ce que l'accueil dit d'une branche : base, commits, écart
    tags.rs / stash.rs / search.rs (`git grep`) / snapshot.rs (point de relecture)
    submodule_tests.rs
  runtime/      les workers
    protocol.rs `Cmd` / `Evt` — des données, aucune logique, sérialisables
    mod.rs      les files (`queue_of`), des threads consommant les mêmes canaux
    executor.rs l'exécuteur tokio partagé, et le pont `block_on`
    watch.rs    surveillance de fichiers (notify), debounce 250 ms
    wire.rs     les trames du fil : postcard, longueur en tête, `PROTOCOL_VERSION`
    remote.rs   le client du fil : lance le serveur, trois threads de pont
  terminal/     émulation
    mod.rs      pty + `Term` alacritty derrière un `FairMutex`
    snapshot.rs grille → lignes et runs de style, sans tenir le verrou
    keys.rs / mouse.rs  frappe, clic et molette → octets
  ui/           tout gpui
    mod.rs      `run()`, `AssetSource`, polices, i18n
    app.rs      `ClaudhubApp` : l'état, la pompe d'événements, le chrome
    topbar.rs   la barre de titre de l'éditeur : menu, sélecteurs, glissement
    dock_layout.rs  l'aire unique : sa disposition, ses sièges, ses gestes
    panels.rs   les panneaux du dock, leur macro et leur registre
    rails.rs    les tool windows et leurs bandeaux — pur, testé
    home.rs     la page d'accueil du centre de l'éditeur
    overview.rs      l'accueil, vue Plan : l'arbre de chaque dépôt — pur
    overview_view.rs l'accueil peint : nœuds, liens, gestes du plan, notes
    focus.rs         l'accueil, vue Focus : quels worktrees, quelle vue, quelle
                     note principale — pur
    focus_view.rs    la barre latérale des worktrees, les tableaux et leurs vues
    summary_view.rs  les onglets d'un tableau, et son Accueil
    pr_view.rs       l'onglet PR : le formulaire, ou la PR, ses vérifications,
                     ses fils de revue et ses gestes
    changes_view.rs  le nœud « Modifications », la feuille de validation, la
                     revue d'un tableau
    canvas_view.rs   les notes de l'accueil : fichiers lus, écrits, déplacés
    context.rs       la fiche qu'un agent lit de son environnement — pur
    revive.rs        les terminaux qui survivent à la fenêtre — pur
    agents.rs        ce que disent les agents : bulles, hooks posés
    review.rs / diff_view.rs / refine.rs / highlight.rs / blade.rs
                     la revue, le diff virtualisé, les mots changés, la
                     coloration (tree-sitter, Blade)
    history_view.rs  l'historique et son graphe peint
    branches.rs / branch_picker.rs  le sélecteur de branches et ses gestes
    worktrees.rs / worktree_picker.rs / picker.rs  le sélecteur de worktrees
    base_select.rs   le sélecteur de base de comparaison
    worktree_ops.rs  création guidée, tâches du projet, intégration
    run.rs / run_view.rs  le widget d'exécution : environnement `wt`, recettes
    tags.rs / stashes.rs / conflicts.rs / merge.rs / merge_view.rs
    tests_view.rs    l'arbre des tests et le run suivi
    explorer.rs / tree.rs / file_icons.rs / preview.rs
                     l'explorateur, l'éditeur intégré et ses onglets, l'aperçu
    surface.rs / vim.rs / folds.rs / follow.rs / hunks.rs / jumps.rs
                     l'éditeur : harnais modal, vim, replis, Ctrl+clic,
                     gouttière, piste
    search.rs / search_view.rs / quick.rs / quick_view.rs / find.rs
                     recherche projet, sélecteur rapide, recherche d'un panneau
    db.rs / db_query.rs / sql_history.rs / sql_history_view.rs
    notes.rs / notes_view.rs / vault.rs  relecture annotée, TODO, coffre
    sentry.rs / github.rs / keyring.rs
    lsp.rs           le pont vers le serveur de langage
    terminal_view.rs
    server.rs        la mise en route du serveur WSL
    settings.rs / settings_view.rs  les réglages, et la page « Journal »
    store.rs / session.rs  ce qu'on retient par worktree, et où l'on en était
    dialogs.rs / notify.rs  les boutons d'un dialogue, les bulles
    inflight.rs / repos.rs  les écritures en vol, les dépôts ouverts — testés
    shortcuts.rs / shortcuts_view.rs / vscode.rs  les touches et leur aide
    motion.rs / scroll.rs  le lissage de la molette, les barres de défilement
    theme.rs / icons.rs
```

### La boucle Cmd/Evt

Le thread d'interface envoie des `Cmd` par `runtime::Handle::send`. Des threads
workers les consomment (`async_channel` est MPMC) et répondent par des `Evt`.
`ClaudhubApp::pump_events` les draine **par lots de 64**.

Ajouter une opération : une variante de `Cmd`, un bras dans `runtime::handle`,
une ou plusieurs variantes d'`Evt`, un bras dans `ClaudhubApp::handle_event`.
Jamais un appel à git depuis un `render` ou un gestionnaire de clic.

**Et un incrément à `wire::PROTOCOL_VERSION`** dès que la forme d'un message
change, retrait d'une variante compris : les deux bouts du fil sont installés
séparément et postcard est **positionnel**. En fusionnant deux branches qui
portent chacune le numéro suivant, git prend la ligne sans conflit : vérifier.

Toute écriture git est suivie d'une relecture du statut (`write_then_refresh`).
**`send` rend un `Ticket`, et `Done`/`Failed` le rapportent** : qui attend une
écriture précise l'attend par son ticket, jamais par « la prochaine réponse de
ce worktree et de cette action ».

### Les files

`queue_of` dit, du seul examen d'une commande, dans quelle file elle part — une
table qu'un test verrouille : une commande mal rangée n'échoue pas, elle attend.

- **Lectures** (trois workers) : statut, diff, branches, écritures locales.
- **Réseau** (un) : `fetch`, `pull`, `push`, Sentry, message rédigé par un
  agent. Un seul : deux `fetch` se disputeraient le verrou des références.
- **Hooks du projet** (un) : `wt new/rm/up/down`.
- **Fond** (un) : résumés, aperçus de l'accueil, agents, `wt`, `just`, `gh`.
  Ne doit jamais passer devant un diff qu'on vient de demander.
- **Bases** (deux) : déplier un schéma en demande plusieurs à la fois.
- **Recherche** (un) : une recherche se **remplace**, l'identifiant d'envoi trie.
- **Tests** (un) : un run se mesure en minutes ; le **relevé** des suites reste
  au fond.

Hors des files : la surveillance de fichiers, les serveurs de langage (une voie
chacun, l'ordre compte), et le **stop d'un run de tests**, un drapeau que le
worker sonde — rangé dans sa file, il arriverait après la mort qu'il demande.

### Le mode distant

`Handle` a deux modes, et les points d'envoi n'en savent rien. **Local** : les
files de ce processus. **Distant** : `runtime::remote::connect` lance
`claudhub-server` en enfant, et tout passe en trames sur ses stdin/stdout.

- **La poignée de main est hors des deux énumérations** (`wire::Hello`, champs
  jamais réordonnés).
- **`connect` ne bloque jamais l'appelant** : c'est le thread d'interface.
- **La mort du serveur est un événement** (`Evt::ServerLost`) ; la relance est
  manuelle.
- **Le plafond de trame vaut aux deux bouts** (256 Mo) ; une charge trop grosse
  est jetée et journalisée plutôt que de fermer le fil.
- **stdout du serveur appartient au fil** : un `println!` dans du code worker
  le corromprait.
- **Le manche reste vide tant que le serveur n'a pas répondu**
  (`HandleInner::Pending`) : sous Windows, les workers locaux feraient
  travailler `git.exe` sur des chemins inexistants.

Levier de test : `CLAUDHUB_SERVER_CMD`. `tests/server_wire.rs` exerce tout le
fil sous Linux, avec le vrai binaire.

### La cible Windows

Interface en `.exe` gpui natif, workers dans WSL2 ; **seuls les terminaux n'y
passent pas** (ConPTY local). Le binaire du serveur est **embarqué dans
l'exécutable** et installé dans la distro à la première ouverture
(`wsl::ensure_installed`), écrit dans l'entrée standard d'un `cat` lancé
là-bas.

- **`build.rs` écrit une constante** ; une variable posée sur un chemin absent
  est une erreur de compilation.
- **L'installation est adressée par le contenu** (`~/.claudhub/bin/<empreinte>`).
- **Rien ne passe par un shell de connexion** (`wsl.exe --exec`), d'où
  `wsl::probe`.
- **Le script d'installation n'a pas un seul guillemet** : la ligne traverse
  `CreateProcess` puis la reconstruction d'`argv` par `wsl.exe`.
- **L'exécutable est fenêtré** : chaque enfant de console reçoit
  `wsl::no_console` (`CREATE_NO_WINDOW`).
- **`wsl.exe --list` répond en UTF-16** avant `WSL_UTF8` ; `wsl::decode`.

Un dossier nommé en argument (`claudhub <chemin>`) l'emporte sur le répertoire
de lancement, et part à une fenêtre déjà ouverte (`instance.rs`) ; il n'arrive
au serveur que par son **répertoire de démarrage** (`server::connect_wsl`).

**Le fil ne transporte que des chemins Linux** ; la traduction (`wslpath`, pur)
n'existe qu'aux endroits où un chemin change de monde : sélecteur de dossier,
argument, coffre de notes, export CSV, dépôt dans l'explorateur, les retours.
Corollaire : **un chemin du serveur s'allonge par `wslpath::join`, jamais par
`Path::join`** — sous Windows, `/home/x` plus `notes` donne `/home/x\notes`, un
seul dossier créé sans erreur.

### Pourquoi le binaire `git` et non libgit2

Les credential helpers, `includeIf`, les hooks, la signature, les alias :
l'utilisateur attend *sa* configuration. Corollaires : `stdin` fermé et
`GIT_TERMINAL_PROMPT=0` (une invite bloquerait un worker), `GIT_EDITOR=true`,
`LC_ALL=C`, `GIT_OPTIONAL_LOCKS=0` (sinon `git status` réécrit `.git/index`,
qui est surveillé), et les formats `-z` partout où un chemin apparaît.
`git_opt` sert les lectures dont l'échec est la réponse normale ; `git_tolerant`
accepte un code borné (`diff --no-index` sort avec 1 dès qu'il diffère).

### La surveillance de fichiers

- **Ce qu'on surveille vient de `git ls-files`** : les dossiers portant un
  fichier suivi ou nouveau non ignoré, chacun **sans récursion**.
- **On ne réagit qu'aux événements qui changent le contenu**
  (`watch::changes_content`) : inotify signale chaque ouverture, et c'est nous
  qui ouvrons. `Any` et `Other` sont gardés (débordement de la file).
- **Poser les surveillances ne se fait jamais dans le thread d'interface.**
- **Sur un disque Windows monté par WSL, la surveillance ne marche pas et ne le
  dit pas** : `watch::on_windows_filesystem`, affiché dans la barre d'état.
- Dans un worktree lié, `.git` est un *fichier* qui pointe vers
  `<principal>/.git/worktrees/<nom>`.

## L'espace de travail, et le dock

**Une seule aire de dock** (`ui::dock_layout`) : trois zones d'outils autour d'un
centre de documents. **La gauche choisit, la droite se souvient, le bas s'étale,
le centre se lit** : à gauche fichiers, recherche, changements, branche, tests,
notes, conflits, remisages, tags ; à droite les bases et les requêtes jouées,
Sentry, GitHub ; en bas le run suivi, les branches et leur graphe, les
terminaux ; au centre le diff, l'éditeur, l'aperçu, les fichiers ouverts, les
consoles SQL.

- **Chaque bord a deux moitiés, et la fente suit le bord** (`Side::axis`) :
  `Half::Start` / `Half::End`, jamais « haut » et « bas ». **Le bas ne dessine
  qu'une moitié à la fois** (`Side::exclusive`, `rails::settle`) : appeler une
  moitié **déplace** l'autre, un troisième état en mémoire seulement
  (`displaced_panels`, jamais dans `folded_panels`), décidé **au geste**
  (`bring_half_forward`) et non au rendu.
- **Une moitié vide n'est pas un emplacement** (`dock_layout::edge`) ; `seats`
  lit un groupe seul comme la **première** moitié.
- **Une tool window ne se dépose pas au centre, un document pas à un bord**
  (`panels::regions_of`) ; la réponse se lit sur le **nom** — ce que
  `rails::tool` connaît est une tool window.
- **Un panneau assis à deux endroits a deux boutons**, et le bouton porte son
  ancre.
- **Les terminaux sont deux vues** (`ClaudhubTerminal`, `ClaudhubTerminalRight`,
  et « Exécution » pour les recettes) parce qu'une tool window *est* un nom ;
  `panel_name` est une méthode, `TerminalPanel::name_of` traduit l'emplacement.
- **Une tool window a un bouton, un document n'en a pas** ; `ui::rails` décide
  tout (ordre, lumière, sens d'une pression, zen).
- **Replier et retirer sont deux choses** : repliée, une vue garde son bouton ;
  retirée, elle n'en a plus, et le menu « Vues » est le seul chemin du retour
  (`rails::in_view_menu`). Un document ne se retire pas, il se **ferme**
  (`panels::closable_title`).
- **Il n'y a pas de seconde liste** : un bandeau se calcule à chaque frame
  depuis l'arbre (`dock_layout::seats`). Une tool window ne rejoint `TOOLS` que
  **par la fin** (`Alt+1`…`Alt+9` sont des rangs) ; `MAX_PER_RAIL`.
- **Les bandeaux sont peints par nous, hors de l'aire** : d'où
  `set_toggle_button_visible(false)` et `set_menu_button_visible(false)` ; à la
  place, `panels::fold_button`, qui passe par `rails::press`. Le milieu d'un
  bandeau porte la zone entière (`rails::zone_glyph`).
- **Un onglet du centre ne s'affiche que s'il a quelque chose à montrer**
  (`needed:` dans `panels!`), question posée à l'application.
- **Le panneau d'accueil du centre** (`EditorPanel`, peint par `ui::home`) ne
  demande rien à git, ne se ferme pas, et garde l'identifiant `ClaudhubEditor`.
- **Les Réglages sont une modale**, le formulaire une **entité enfant**.

La disposition est enregistrée dans `<config>/layout.json`. `LAYOUT_VERSION` la
fait écarter quand les panneaux changent de nom ; ce que le registre ne sait
pas bâtir est **élagué** (`app::prune`, `panels::is_registered`), et **seule une
feuille se juge sur son nom**. Les panneaux se déclarent par la macro `panels!`,
et `dock_layout` les bâtit **par ce registre**. **Ne renommez pas un identifiant
de panneau** (`ClaudhubCi`, `ClaudhubEditor`…) : il est écrit dans les
`layout.json` déjà enregistrés, et revient en « panel type is not registered ».

Sept pièges du dock :

- **Un panneau sans pile parente est verrouillé** (`is_locked`) : tout panneau
  est enveloppé, fût-ce dans une division d'un seul élément.
- **`toggle_dock` ne notifie pas l'aire**, seulement le dock intérieur.
- **Le dernier panneau de l'*aire* ne se déplace pas** (`is_last_panel`).
- **Les tailles d'une division se donnent toutes** : un `None` vaut cent pixels,
  et la pile répartit au prorata.
- **L'état se relit au moment d'écrire** : l'ouverture d'une zone est différée
  d'une frame.
- **`panel_handle` et `add_panel_view`, jamais `add_panel`** : un `Entity<P>` se
  convertit tout seul, et le panneau arrive sans onglet, ni titre, ni contenu.
  L'ajout-puis-déplacement passe par `panels::dock_panel_at`.
- **L'élagage d'une zone latérale passe par `DockState::new`** ; sans lui, un
  terminal de la zone basse revient en « panel type is not registered ».

**Un terminal s'ouvre du côté que le réglage dit** ; le `+` d'une barre
d'onglets ouvre dans **sa** vue, après le dernier onglet (`tab_bar_trailing`),
et le groupe rejoint est celui d'un frère **de la même vue** — sans frère, la
cible se dit quand même (`dock_layout::target_for`).

**Les terminaux survivent à la fenêtre** (`ui::revive`, `SavedTerminal`) : un
shell ou un agent vivant revient à sa place, Claude **dans sa conversation**
(`--resume` de la session que Claude écrit pour son pid, sinon celle des hooks,
sinon `--continue` s'il était seul), toujours suivi de `|| exec claude`. Un
onglet lancé sur une commande ne revient pas. Rien n'est écrit pour un worktree
dont les terminaux ne sont pas encore rouverts (`terminals_revived`).
**Les terminaux sont des panneaux**, retirés de `layout.json` avant écriture ;
un onglet lancé sur une commande est « occupé » tant qu'il vit (`sh -lc`
**exec** ce qu'on lui donne).

## L'accueil

**L'accueil est un écran** qui remplace la fenêtre entière, barre de titre
comprise, et a deux vues (`HomeMode`, retenu dans la session) : le **Focus**
(`ui::focus_view`, par défaut) et le **Plan** (`ui::overview_view`, disposé par
`ui::overview`, pur). Une **barre latérale** commune liste tous les projets
ouverts et leurs worktrees ; ce qu'on y choisit est ce que les deux vues
montrent. Un clic montre un worktree seul et le rend regardé (`active`, celui
de toute la fenêtre), `Ctrl`+clic l'ajoute à côté (`focus::shown_worktrees`).
Chaque ligne porte le signal du plus pressant de ses agents sur son bord gauche
(`edge_signal`, `worktree_doing`, `overview::loudest`). La barre se replie en
rail d'initiales. Les boutons de la fenêtre ne sont peints que là où c'est à
nous de le faire (`draws_window_buttons`).

**Le Focus : un tableau par worktree choisi**, côte à côte ; c'est la rangée qui
défile, jamais un tableau seul, et un tableau n'est **jamais plus étroit que ce
que sa vue met côte à côte** (`HOME_LEAST`…) — ce qui dépasse est coupé à son
bord. **Le titre d'un tableau est à lui et défile avec lui** (`board_title`) :
placé d'après ce que la rangée mesurait à chaque frame, il clignotait.

- **Des onglets et une seule vue** (`focus::View`, retenue par worktree) :
  Accueil, Git, Revue, PR, Tests, note principale, TODO, Terminaux. Des vues de types
  différents côte à côte ont été essayées et refusées.
- **L'Accueil** (`ui::summary_view`) : à gauche l'état en cartes (branche, PR
  et CI, ce qui attend un commit, revue, tâches, note principale, ce qui
  tourne), à droite **les terminaux eux-mêmes**, nus, un sous-onglet chacun
  (`focus::shown_terminal`). Le titre d'une carte ouvre son onglet ; un geste
  simple se fait sur la carte (une tâche cochée sur n'importe quel worktree :
  `toggle_task_in`).
- **Git, Revue, Tests et TODO sont les panneaux de l'éditeur eux-mêmes**, un
  exemplaire de chaque : vivants sur le worktree regardé seulement, et tant
  qu'aucune feuille ne peint les mêmes panneaux — une liste virtuelle peinte
  deux fois partagerait son défilement. La PR et la CI ne sont lues que pour le
  worktree regardé : l'état GitHub n'en suit qu'un.
- **L'onglet PR** (`ui::pr_view`) : sans PR, le formulaire est l'onglet (titre
  et corps d'après les commits, base de la revue) ; avec une PR, son état, ses
  vérifications, ses fils de revue — répondus et résolus d'ici par GraphQL,
  le corps en variable et jamais dans la requête — et prête / brouillon /
  fusionner. Chaque geste relit tout (`GithubState::pr_action_call`).
- **Une vue à deux côtés a son séparateur** (`two_sides`), dont l'état est
  rangé par tableau et par vue : un même séparateur peint dans deux tableaux
  à la fois se disputerait sa mesure.
- **L'onglet Git** a deux faces : les modifications (la feuille de validation
  posée dans le tableau) et l'historique (le panneau de l'éditeur — graphe,
  recherche, fichiers du commit — à côté du diff du commit choisi).
- **L'onglet Tests** montre par défaut les seuls tests écrits dans un fichier
  que la branche a touché — depuis sa base ou en cours (`branch_touched`,
  `tests_view::rows_among`) ; le même filtre existe, éteint, dans l'éditeur.
- **La note principale** est celle qu'on épingle (`pinned_note`), à défaut la
  plus récente (`focus::principal_note`).
- **Ce qu'un agent fait se lit pour tous les terminaux des worktrees vivants**
  sur les tableaux, pas seulement ceux du plan (`overview_at_work`) : un
  worktree masqué sur le plan emportait le signal de ses terminaux.
- Ne demande des frames que ce qui est à l'écran (`prepare_laid_out`).

**Le Plan : un arbre par dépôt**, de haut en bas — le nœud git, les checkouts,
leurs notes, leurs terminaux, les worktrees tirés de leur branche ; chaque
parent centré sur ses enfants.

- **Le zoom est une échelle, pas une disposition** : tout est en `rem`, prêté à
  l'échelle (`Scaled`) ; la grille d'un terminal se calcule sur la taille
  logique de la carte, jamais sur les pixels couverts (chaque ligne d'arrondi
  serait un `SIGWINCH`).
- **Ce qui est à l'écran ne bouge pas quand un nœud vient ou part**
  (`overview::hold`).
- **Déplacer un nœud emmène son sous-arbre** (`overview::Moved`) ; git,
  worktrees et notes sont retenus, les terminaux non.
- **Le bouton du milieu déplace le plan**, pris en phase de capture avant qu'un
  terminal n'y colle ; `Ctrl+Maj+V` pour coller.
- **Chaque nœud a les trois boutons d'une fenêtre** : replier, agrandir (90 % de
  l'écran à un zoom de un, `maximized_size`), fermer. Masqué (`Hand::hidden`)
  revient par « Masqués ». **Un dialogue ouvert depuis un nœud prend le focus**
  (`focus_dialog`, différé) : sinon ses boutons dispatchent depuis le terminal
  où la croix a été pressée, et OK ne fait rien.
- **Les liens** partent du côté qui fait face à l'enfant (`overview::attach`),
  en coude ; celui d'un agent au travail coule, celui d'un agent qui attend
  respire (`overview::at_work`, `tile_outline`). Ce qui décide est le **statut
  que Claude écrit pour son pid**, puis le mot des hooks, puis la devinette par
  le processeur. gpui n'a pas de décalage de tirets : `overview::dash_array`,
  recalculé à chaque image tant qu'un lien bouge (`tick_flow`).

**Les nœuds de l'accueil sont des fichiers** (`crate::canvas`,
`ui::canvas_view`) : un Markdown à en-tête plat par nœud, dans
`.claudhub/notes/` du checkout (**versionné**) ou dans le coffre du worktree
(privé). Le disque fait foi : la surveillance couvre `.claudhub/notes/`, et
l'accueil relit les projets affichés toutes les deux secondes ; la main n'y
écrit que si le fichier est toujours celui lu.

- **Une revue est un nœud et ses remarques** (`### chemin:ligne`,
  `canvas::findings`), résolues dans le fichier même, jamais versées dans les
  notes. Close, elle s'archive (`.claudhub/archive/`).
- **Une note ou un diagramme écrits par l'agent** : Claudhub nomme le fichier
  d'avance et donne le format entier (`canvas::generation_prompt`) ; le
  terminal cède la place au résultat, et n'est pas retenu (rouvert, il
  relancerait la demande).
- **Un diagramme est une image et son nœud** ; le relevé ne transporte que
  l'**empreinte** des images (`files::picture_stamps`) — sinon les octets
  traverseraient WSL toutes les deux secondes.
- **Ce qu'un agent voit de son environnement** est une fiche Markdown
  (`ui::context`) réécrite dans le coffre et annoncée par `$CLAUDHUB_CONTEXT` ;
  **la skill** (`crate::skill`, `assets/skills/`) en apprend le format, et
  n'écrase ni ne retire jamais un fichier qui n'est pas le nôtre.
- **Le nœud « Modifications »** (`Node::Changes`) : le relevé de fond ne fait
  que **compter** ; la liste est le statut, demandé une fois par worktree tant
  que le compte ne bouge pas (`ensure_changes_read`), et c'est lui qui décide
  dès qu'il est frais (`has_changes`).
- **La feuille de validation** (`ReviewSheet`, entité enfant) est le panneau
  des changements et le diff **eux-mêmes** dans un dialogue ; sa taille se lit
  sur un `Rc<Cell>` partagé avec la fermeture du dialogue. Sur un tableau, pas
  de feuille : l'onglet Git.

## Le grain de l'interface

- **Les rayons montent à huit et douze** (`theme::apply`). **La carte, c'est le
  groupe d'onglets entier**. **Le masque de contenu de gpui est rectangulaire** :
  l'arrondi d'un élément ne rogne pas ses enfants — `panels::corner_cut`.
- **Une ligne de liste est un bandeau, pas une pastille** : `w_full`, aucun
  rayon, et le retrait porté par l'entrée — `uniform_list` **ignore les marges
  de ses entrées**. La gouttière de défilement est toujours réservée
  (`theme::scroll_gutter`).
- **Aucune hauteur de ligne ne s'écrit en dur** (`theme::row_height` et ses
  voisines) : une liste virtualisée réserve exactement ce qu'on lui annonce.
- **La gouttière est un cran de luminosité sous la carte** (`theme::gutter_of`),
  du côté où il y a de la place.
- **`Theme::tokens` est dérivé de `Theme::colors` une seule fois** : toute
  couleur écrite dans `theme::apply` doit être suivie du recalcul.
  `gpui-base` tient **sa propre copie** du thème (`Theme::sync_base`).
- **Les thèmes sont générés** (`tools/gen_themes.py`) : une clé absente reprend
  la valeur par défaut, qui est *claire*. Ils sont réécrits dans
  `<config>/themes/` à chaque démarrage — pour en modifier un, le copier sous
  un autre nom.

**Le fork de GPUI Kit** : seuls `gpui-base` et `gpui-component` sont patchés
(`Cargo.toml`), la façade `gpui-kit` et GPUI viennent de crates.io. Ce que la
série de commits ajoute — les crochets publics et les comportements, et comment
la rebaser — est dans [`docs/gpui-kit-migration.md`](docs/gpui-kit-migration.md).
Trois de ces comportements se perdent sans erreur si le fork est retiré : le
fond d'un run peint (`ShapedLine::paint` ne dessine que les glyphes), Tab qui
atteint le terminal hors surface modale, et les tailles d'une division lues
comme des parts. Les commits ont vocation à partir en PR.

## Les sous-systèmes

Un paragraphe par sujet : ce qui décide, où ça vit, et le piège qui ne se voit
pas. Le détail est en commentaire dans le module nommé.

**La vue de diff** (`diff_view.rs`) — deux listes virtualisées par
`uniform_list`, et quatre contraintes tiennent ensemble : `.h(LINE_HEIGHT)`
explicite, `.whitespace_nowrap()`, `ListHorizontalSizingBehavior::Unconstrained`
avec `with_width_from_item`, et **pas de `w_full` sur une entrée**. Ce qui se
déduit d'un diff est calculé une fois dans `Rendered`. Le repli des lignes
longues se fait **à la colonne** (hauteur calculable) et bascule sur
`v_virtual_list` ; la largeur mesurée est **toujours celle de la frame
d'avant**. `Ctrl` rend les symboles cliquables, repeint par un
`on_modifiers_changed` sur la **racine**.

**Le cherry-pick** vit dans le graphe des branches ; le clic droit dans le diff
d'un commit reprend **ce que ce commit a fait à ce fichier**
(`repo::take_from_commit`, patch par l'entrée standard, jamais un fichier
temporaire). Les deux relisent le statut **même en échec**
(`write_then_refresh_anyway`) : un pick qui conflit a écrit ses marqueurs.

**La revue** (`review.rs`) — `DiffRange` n'a ni `Unstaged` ni `Staged` : une
case par fichier. `app::initial_range` choisit au **premier** statut, ensuite la
portée appartient à l'utilisateur. La base vient de git (`branch::guess_base`) :
la branche dont celle-ci a le moins divergé, les égalités départagées par
**là où part le travail** (`branch::start_point` : `dev`/`develop`, sinon la
branche d'intégration). Elle **n'est pas enregistrée** : le magasin garde ce
qu'on a choisi, jamais une devinette.

**« Depuis ma dernière relecture »** (`DiffRange::Since`, `git::snapshot`) — le
point est l'état du disque, construit dans un **index à nous**
(`GIT_INDEX_FILE`, jamais celui de l'utilisateur), puis `commit-tree` et une
ref `refs/claudhub/review/<empreinte>`. Le diff compare **deux arbres** :
`git diff <point>` lirait supprimé un fichier non suivi des deux côtés.

**L'explorateur** (`explorer.rs`, `tree.rs`) — l'arbre vient d'un seul appel git.
`tree` rend des **indices** ; construire l'arbre et le plier sont deux gestes.
Le curseur est un **chemin**. L'arbre retient ce qu'on a **ouvert**, la revue ce
qu'on a **fermé** (`tree::Folds`) ; une recherche a son **second** ensemble de
replis.

**L'éditeur** (`explorer.rs`, `surface.rs`) — un onglet par fichier, la barre du
dock. Police, taille et hauteur de ligne se disent **explicitement**, sinon il
hérite de la police proportionnelle et ignore le zoom. L'écriture est
**conditionnelle** (`files::write`, empreinte) : un agent écrit dans les mêmes
fichiers.

**La gouttière** (`hunks.rs`) — le texte de base lu une fois
(`repo::head_blob`), la comparaison refaite **en mémoire** à chaque frappe
(patience, écrite ici). Le saut de ligne final est une ligne (`split('\n')`,
jamais `lines()`).

**Le mode vim** (`vim.rs`, `surface.rs`) — désactivé par défaut : ses liaisons
sont des **lettres nues**. La machine ne connaît aucun type de gpui. L'écoute
est en **phase de capture** ; ce qui arrive par une liaison (`Ctrl+V`, `Enter`)
s'attrape comme une **action** ; le caractère lu est `key_char`.

**Les surfaces de code** (`surface.rs`) — une surface se **nomme**
(`Surface::File(chemin)` / `Query` / `Text(champ)`), elle ne se possède pas. Les
quatre champs de rédaction sont des `EditorState` habillés en texte brut
(`plain_editor`) ; deux sont dans un dialogue, leurs décorations se
rafraîchissent en tête du rendu de la racine (`sync_text_surfaces`). **Échap y
est une action** (`Cancel`) : sans `vim_escape`, sortir du mode insertion
fermait le dialogue.

**Le terminal** (`terminal/`) — **ne jamais dessiner sous le verrou de la
grille**, d'où l'instantané. Sur l'accueil, un terminal est une **vue en
cache** (`Entity::cached`) : ce qui le change du dehors notifie (`set_canvas`).
Chaque run est posé **à sa colonne** en absolu. Le redimensionnement attend que
la main s'arrête, mais **lâcher la poignée est un événement** (écouteur sur la
fenêtre). **Le texte nu passe par l'input handler, jamais par la frappe** :
`key_bytes` ne rend que les touches spéciales — rompre ce maillon double
chaque lettre ou perd le ê d'une touche morte, sans erreur.

**La recherche** — `Ctrl+F` cherche dans le panneau où l'on vient de
**cliquer** (`panels::pane_root`, en capture) ; tout panneau qui y répond porte
la même loupe (`find::find_button`). `Ctrl+Maj+F` demande à `git grep` (`-E`,
pas `-P`), trois plafonds dits. Une occurrence s'ouvre dans l'onglet d'aperçu
de l'éditeur et **laisse le clavier dans la liste** ; les mots cherchés
s'allument sur la couche des occurrences (`surface::hit_pattern`).

**Le sélecteur rapide** (`quick.rs`, `quick_view.rs`) — un modal, deux questions
(`Ctrl+Maj+F`, `Maj Maj`/`Ctrl+P`), un champ pour les deux, liés une **seconde**
fois contre son propre contexte. Le côté texte n'a aucun état : il écrit dans
`search_input` et peint `search.rows` (`Ctrl+Entrée` passe au panneau). Le côté
fichiers est un **classement** pur. **`Maj Maj` n'est pas une liaison** : il se
lit sur `on_modifiers_changed` de la racine, toute autre frappe rompt la série
(`quick::DoubleTap`, en capture) ; il se tait sur l'accueil.

**Les bases** (`db/`, `db.rs`, `db_query.rs`) — `sqlx`. **Une console est un
document**, un panneau chacune ; un clic sur une table réutilise celle où l'on
est. L'identifiant d'envoi est compté pour la **fenêtre**. `db::Cell` est un
`Option<String>`. Un `DECIMAL` se décode par `try_get_unchecked`. Une connexion
par requête. Le tri enveloppe la requête (`db::order_by`, par **rang**).

**Sentry** (`sentry.rs`, `ui/sentry.rs`) — le projet appartient au dépôt,
l'organisation et le jeton à la machine. **Une réponse se lit champ par champ,
jamais en la désérialisant dans une structure** : `#[serde(default)]` ne fait
rien d'un champ présent à `null`, et un seul `null` jetait toute la pile. Le
jeton est substitué **dans le worker** (`outside::Cap`) ; `keyring:` se résout
**côté interface** (`ui/keyring.rs`), la session de bureau étant du côté
Windows. Le corps d'une erreur est une liste virtuelle, colorée **une fois**.

**Rien n'est lu avant que le panneau ne soit peint** (`ensure_sentry`,
`ensure_github`, `ensure_history`) ; changer de worktree **oublie** au lieu de
relire ; ce qui a été lu est signé par le compte et le projet, jamais par un
drapeau.

**GitHub** (`github.rs`, `ui/github.rs`) — par `gh`, jamais l'API : la CLI est
déjà authentifiée. Tout passe par `outside::Cap::Shell` sous `Caller::Github`.
Les exécutions reviennent en lignes (`gh --template`), une PR en JSON lu champ
par champ. `printf "%.0f"` sur l'identifiant : un modèle Go formate un nombre
JSON en flottant (`3.2494024323e+10`). `gh pr list` et non `gh pr view` (sans PR,
`view` sort en erreur). Ce qui tourne se relit toutes les dix secondes, **mais
seulement peint**. Une PR s'ouvre dans un worktree par le dialogue de création
(`worktree_from_branch`), après un `fetch` explicite — jamais `gh pr checkout` ;
la PR d'un fork est récupérée dans `pr/<n>`.

**`outside.rs`** — une liste fermée de ce qu'on fait hors du dépôt, chacun avec
sa file. Le système d'extension est le `justfile`, le `wt.toml` et les commandes
des réglages ; il n'y a pas de plugins.

**L'instance unique** (`instance.rs`) — le dossier part à la fenêtre déjà
ouverte par une socket locale (`GenericNamespaced` : aucun fichier, pas de
verrou périmé). **Rien ici ne peut empêcher la fenêtre de s'ouvrir** ;
`CLAUDHUB_ALLOW_MULTIPLE` (posé par le `justfile`). **Le nom porte
l'utilisateur.** **Ce qui arrive n'est pas de confiance** : lu en UTF-8, refusé
sinon. Un dossier qui arrive pendant que le serveur WSL démarre attend la
poignée de main (`pending_handoff`). Sous Windows, `AllowSetForegroundWindow`
**côté secondaire**.

**Les remisages** (`git/stash.rs`) — la pile est **celle du dépôt** ;
`stash@{0}` est une **position** que le terminal d'à côté décale, donc chaque
geste porte l'empreinte que la ligne montrait. Un `pop` qui conflit relit le
statut.

**Les notes** (`notes.rs`, `vault.rs`) — une note retient des numéros de ligne,
un côté et l'**extrait** ; `notes::relocate` la replace, sinon elle est dite
**décalée** et reste. Le dossier est la source de vérité. On n'efface que ce qui
porte notre marque (`files::is_ours`). L'envoi à l'agent passe par le terminal,
en collage encadré, le retour chariot dans un **second** envoi.

**Les raccourcis** (`shortcuts.rs`) — une seule table (`table!`) donne
`bind_keys` et l'aide. Deux prédicats : sous Linux `secondary` **est** Ctrl,
donc une lettre seule passe par `WINDOW_PREDICATE`, qui exclut le terminal. Les
tool windows se replient par `Alt+N` (gpui retire le Maj d'un caractère sans
casse). `KeyBinding::new` **panique** sur ce qu'il ne sait pas lire
(`valid_keys`). `Ctrl+Tab` parcourt les onglets de ce qu'on regarde ; **changer
le prédicat d'une liaison, pas ses touches**, sinon la personnalisation part.
Le clavier de VS Code se reprend (`vscode.rs`), lu depuis le thread
d'interface ; un import n'éteint jamais un raccourci.

**Le lissage de la molette** (`motion.rs`) — on laisse gpui sauter, on lit où
il a atterri, on **remet** le décalage d'avant et on y va progressivement ;
l'écouteur est sur un ancêtre **non défilant**. Le saut se **lit**
(`Axis::jump`).

**La coloration** (`highlight.rs`, `blade.rs`) — les plages doivent être
**triées et disjointes**, les décalages en **octets**. Un fragment reçoit de
quoi être reconnu (`prologue` : sans `<?php`, PHP lit tout comme du HTML).
`SyntaxHighlighter::new` compile les requêtes — **jamais dans un `render`**.

**Le serveur de langage** (`lsp/`, `ui/lsp.rs`) — une session par worktree, une
**voie** et non une file. Le `languageId` n'est pas l'extension
(`lsp::language_of`). Le fil ne transporte que des chemins **absolus**. Un saut
de définition qui ne trouve rien retombe sur `git grep`. La légende des jetons
sémantiques est traduite (`lsp::theme_name`), un nom inconnu ne rendant aucun
style.

**La piste** (`jumps.rs`) — une place est un fichier *ou* un document du centre ;
déplier une tool window n'est **pas** un déplacement. Une piste par worktree ;
un nouveau saut jette ce qui était devant. Boutons 4 et 5 de la souris.

**Le magasin** (`store.rs`) — où l'on en est, écrit depuis le thread
d'interface. Un seul point d'écriture, et les ensembles **triés** avant. Une
entrée retient son **dépôt**. **Un champ qui change de forme change de nom** :
l'ancien format, relu sous le même nom, ferait échouer tout le fichier.

**Les réglages** (`settings.rs`) — un **global gpui**, écrit une demi-seconde
après. Ce qui dépend d'un réglage se **relit à chaque rendu**. Le formulaire est
une entité enfant ; sa page est retenue par nous (`Page`, redonnée sur un
identifiant neuf, `on_select` du fork).

**Le journal** (`logging.rs`) — un anneau de deux mille lignes. `git::report`
file chaque commande à `debug` (à `info` quand elle traîne), `runtime::handle`
nomme chaque commande par `Cmd::name` (un match, jamais `Debug`). **Le journal
est en anglais**, un test le vérifie.

**Les agents** (`agent.rs`) — détectés par `/proc`, pas par nos onglets. Le
relevé ne dit pas qu'un agent travaille : c'est la **différence** entre deux
relevés (`agent::Tracker`). `parse_cpu_ticks` repart de la **dernière
parenthèse**. Les marqueurs de session hérités sont effacés au démarrage
(`agent::disinherit_session`). **Un agent dit lui-même qu'il a fini ou qu'il
attend** (`agent_hooks`) : une ligne de shell POSIX écrit un fichier par
session dans `~/.claudhub/agents/`, relu par le relevé et **jamais surveillé** ;
les hooks sont fusionnés dans `.claude/settings.local.json`, jamais sur un
fichier suivi, cachés par `info/exclude`.

**`wt`** — une dépendance, pas un sous-processus (sa CLI derrière la feature
`cli`, désactivée). Les questions se demandent en **boucle** (`wt::Phase`), un
`[[prompt]]` pouvant dépendre d'une réponse précédente. **La création**
(`worktree_ops.rs`) a un premier écran commun aux dépôts avec ou sans
`wt.toml` : nouvelle branche ou existante, le nom, le point de départ, et
l'aperçu de ce qui sera fait (`plan_creation`, pur). Le point de départ par
défaut est celui du dernier worktree du dépôt, sinon `branch::start_point`, lu
avec les branches.

**Les conflits** (`merge.rs`, `merge_view.rs`) — le fichier relu depuis l'index
(`:1:`, `:2:`, `:3:`), comparé deux fois ; ce qu'**un seul** côté a touché est
pris d'office. **`--ours` et `--theirs` s'inversent pendant un rebase** : traduit
une fois, dans la couche git. Le blob se lit octet pour octet (`git_blob`).

## Conventions gpui

Elles viennent d'Aviary, et les enfreindre produit des bugs silencieux.

- L'état qui survit à une frame vit dans un `Entity<T>` créé **une fois** : un
  `InputState` recréé dans `render` perd le texte à la première frappe.
- `cx.listener(...)` pour ce qui mute la vue ; les souscriptions dans le
  constructeur, jamais dans `render`.
- `let theme = cx.theme().clone();` dès qu'une fermeture aura besoin de
  `&mut cx`.
- `Theme::change` réinitialise les couleurs : toute palette s'applique
  **après**, puis `cx.refresh_windows()`.
- La vue racine ré-émet les couches de `Root` à la fin de son `render`.
- **Lire l'entité racine depuis une fermeture de rendu est une panique** :
  `open_dialog` rappelle son `Fn` à chaque frame depuis le rendu de la racine,
  un popover tourne dans `ClaudhubApp::render`. Ce qui doit relire
  l'application est une **entité enfant**. Depuis un clic, tout est libre.
- **Un `Dialog` ne peint pas ses boutons** : `ui::dialogs::confirm` rend le
  pied, dont les boutons **dispatchent les mêmes actions que les touches**.
- **Le champ d'un dialogue prend le focus de façon différée**
  (`ui::dialogs::focus_field`) : un menu qui se referme rend le focus après le
  gestionnaire. Rien ne s'abonne au `PressEnter` d'un champ d'une ligne : il
  remonte déjà à `Confirm`.
- **`key_context` prend un identifiant, pas un prédicat** (`"A && !B"` fait
  déborder la pile). Un contexte se déclare **sur le même nœud** que celui avec
  lequel il se combine.
- **`div()` est un bloc, pas une boîte flex** : un `flex_1()` y est ignoré, et
  le `size_full()` d'en dessous vaut zéro. Un conteneur dont un enfant réclame
  la place restante s'écrit `v_flex()` / `h_flex()`.
- **Tout ce qu'une opération a à dire est une bulle, en haut à droite**
  (`ui::notify`, `announce`) ; sans fenêtre, par `pending_notes`. Bornée deux
  fois : quatorze lignes, et un plafond au tiers de la fenêtre posé sur le
  **corps**.
- **Un handle de focus survit à l'élément qui le portait**, et la fenêtre
  devient sourde à toutes nos liaisons ; `app::reclaim_stranded_focus`, en tête
  de rendu, est le filet.
- **Tout ce qui se clique dans la barre de titre passe par `topbar::actions()`**,
  qui consomme la pression : sinon le déplacement de fenêtre avale le
  relâchement, et le clic n'a lieu qu'une fois sur deux.
- Les raccourcis passent par `secondary-` : le reste du clavier appartient au
  programme du terminal.
- gpui rend via Vulkan sur Linux : `vulkan-loader` doit être dans
  `LD_LIBRARY_PATH`, ce dont `shell.nix` se charge.

## Interface bilingue

Toute chaîne visible passe par `tr!`, qui rend un `SharedString`.
`assets/i18n/{fr,en}.json`, objets plats, clés en kebab-case préfixées par
domaine ; les deux catalogues ont les mêmes clés et les mêmes substitutions
`%{…}` (`ui::i18n_tests`). Une clé que plus rien n'utilise se retire.

**Le code est en anglais, la documentation en français** : commentaires, noms
et messages d'erreur du cœur en anglais ; ce fichier, le README et le
`justfile` en français.

## Tests

Les couches `git`, `terminal`, `runtime` et `sentry` sont testables sans gpui,
et c'est là que sont les tests : ils portent sur les formats que nous parsons,
là où une régression donne une liste plausible mais fausse. Le même motif
partout : **la décision vit dans un module pur, devant la vue qui la peint**
(`notes.rs`, `focus.rs`, `inflight.rs`, `vim.rs`, `motion.rs`, `jumps.rs`,
`merge.rs`, `hunks.rs`, `quick.rs`, `db/scope.rs`…).
`watch::tests::a_real_write_reaches_the_receiver` est le seul test qui touche
le système de fichiers ; `tests/server_wire.rs` lance le vrai serveur.
