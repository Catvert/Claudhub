# CLAUDE.md

Guide de ce dépôt. C'est le **seul** document d'architecture : quand un
changement touche la structure, il se met à jour dans le même commit. On y écrit
ce qui ne se devine pas en lisant le code — les pièges et les raisons — jamais
ce que le code dit déjà.

## Commandes

Tout passe par `nix-shell` via le `justfile` ; n'appelez `cargo` directement que
si les bibliothèques de `shell.nix` sont déjà dans le périmètre.

- `just` / `just run` — build debug et lancement
- `just check` / `just clippy` (`-D warnings`) / `just fmt` / `just test`
- `just check-server` — le serveur headless sans la feature `ui` : le portillon
  qui prouve qu'aucun module du cœur ne tire gpui ni Rune
- `just ci` — les quatre d'un coup
- Un test isolé : `nix-shell --quiet --run "cargo test watch"`

Le projet doit passer `cargo fmt --check`, `clippy --all-targets -- -D warnings`
et `cargo test` en permanence.

## Distribution

Le binaire release ne tourne que sur cette machine : compilé sous `nix-shell`,
il est lié contre la glibc du nix store, interpréteur ELF compris.
`tools/make_appimage.sh` (à lancer **hors** nix-shell, après un build release)
produit `Claudhub-x86_64.AppImage` (exige `fusermount`) et
`Claudhub-x86_64.run`, auto-extractrice, qui ne demande que `sh`, `tar` et
`gzip` — contenu adressé par empreinte dans `~/.cache`, anciens builds purgés.

Deux choses à ne pas défaire : les pilotes GPU (ICD Vulkan) viennent de
l'**hôte**, dont les chemins ferment le `--library-path` — embarquer un Mesa
casserait les machines NVIDIA ; et rien n'exporte `LD_LIBRARY_PATH`, si bien que
les sous-processus (`git`, `claude`, les shells) restent des programmes de
l'hôte. La machine cible fournit `git`, un pilote Vulkan, et l'agent.

**La CI ne construit que des versions** (`.github/workflows/release.yml`, tag
`v*` ou manuel). Pas à chaque commit : chaque jambe recompile l'arbre gpui
entier, et `just ci` le dit déjà ici. Mais elle relance les mêmes portes avant
d'empaqueter — la machine qui vérifie n'est pas celle qui livre, et la jambe
Windows compile du code qu'on ne compile jamais ici. Deux jambes : **Core
tests** (gratuite : `fmt`, tests et clippy du cœur sans `ui`, plus
`check-server`) et **Interface tests** (l'arbre entier, `clippy --all-targets`
et la campagne complète — les deux tiers des tests vivent derrière `ui`, dont la
table des raccourcis, où `KeyBinding::new` **panique** au démarrage sur une
touche illisible ; une version pouvait partir avec un binaire qui panique au
lancement). Elle tourne **à côté** des jambes d'empaquetage, et c'est `publish`
qui l'attend.

Ordre imposé : la jambe du serveur musl finit avant celle de Windows, qui lui
passe le binaire par `CLAUDHUB_EMBED_SERVER`. Le serveur est lié en statique
(musl) parce qu'il est copié dans une distribution dont on ne sait rien. La
jambe Linux passe par Nix et pousse dans Cachix (`CACHIX_AUTH_TOKEN`,
`CACHIX_CACHE`). Les fichiers sont attachés à une release **en brouillon**.

## Architecture

Trois couches, et une règle qui les sépare : **seule `src/ui/` connaît gpui, et
elle ne fait jamais d'entrée-sortie**.

Le crate est une **bibliothèque et deux binaires** : `claudhub` (l'interface) et
`claudhub-server` (les mêmes workers, headless, derrière stdin/stdout — destiné
à WSL2 quand l'interface est un `.exe` Windows). La feature `ui`, active par
défaut, porte gpui, gpui-component, alacritty, Rune et tout ce qui s'affiche ;
le serveur se construit avec `--no-default-features`, et un module du cœur qui
toucherait à gpui — `tr!` compris — casse ce build. C'est la règle des trois
couches, vérifiée par le compilateur.

```
build.rs        embarque le serveur musl, s'il y en a un (`CLAUDHUB_EMBED_SERVER`)
src/
  lib.rs        les modules, l'i18n et `tr!` (feature `ui`)
  main.rs       le binaire de l'interface — trois lignes
  bin/server.rs le serveur headless
  cmdline.rs    découpe et recompose une ligne de commande (guillemets POSIX)
  wsl.rs        la distro : la lister, y installer le serveur, l'y lancer
  wslpath.rs    chemins Windows ⇄ distro WSL, textuel et pur
  commit_msg.rs le message de commit proposé : prompt, nettoyage, agent
  files.rs      lire, écrire (sous condition), ranger, éditeur externe
  db/           bases de données — `sqlx`, asynchrone, testable sans gpui
    mod.rs      connexions, schémas, résultats ; le choix du moteur
    scope.rs    quelles bases appartiennent au worktree regardé — motifs
    sql.rs      lire une requête comme du texte : ses tables, leurs alias, la clause
    link.rs     suivre une clé étrangère : la table d'une colonne, le littéral
    complete.rs ce que la console propose en tapant — classé, jamais deviné
    sqlite.rs   en lecture seule, schéma lu par les pragmas
    mysql.rs    MySQL et MariaDB, par `information_schema`
  logging.rs    `env_logger` sur stderr, et un anneau de 2000 lignes en mémoire
  lsp/          le client de serveur de langage — un processus par worktree
    mod.rs      la session : poignée de main, requêtes en vol, ce qu'il pousse
    frame.rs    le cadrage `Content-Length` et JSON-RPC
    sync.rs     versions de document, et l'édition à une plage (UTF-16)
    uri.rs      chemins ⇄ `file://`
  plugin/       les plugins : des panneaux dont le contenu est un script
    mod.rs      les modules, et `in_worktree` (un chemin qu'un script nomme)
    view.rs     l'arbre de vue en données — le vocabulaire borné
    manifest.rs le `plugin.toml`, ses panneaux, et la découverte
    install.rs  installer, mettre à jour, retirer — `git clone`, `git pull`
    caps.rs     ce qu'un plugin peut faire dehors : les données, et le worker
    host.rs     la machine Rune (feature `plugins`) — le seul module qui la voit
    loaded.rs   un plugin tel que la fenêtre le tient : script, état, arbres
  wt.rs         le `wt.toml` d'un projet : questions, tâches, statut, URLs
  git/          couche git — sous-processus `git`, testable sans gpui
    mod.rs      exécution (stdin fermé, LC_ALL=C, pas de pager)
    repo.rs     découverte, worktrees, écritures (stage, commit, push…)
    status.rs   `status --porcelain=v2 -z` → index et worktree séparés
    branch.rs   `for-each-ref` → branches, amont, divergence
    diff.rs     `--numstat` et diff unifié → fichiers, hunks, lignes
    history.rs  `git log` → commits, et la disposition du graphe
    tags.rs     les tags : lecture, création, publication, suppression
    search.rs   `git grep` : les arguments, le parsage, les plafonds
  agent.rs      les agents dans `/proc`, et le suivi qui dit lesquels travaillent
  runtime/      les workers
    protocol.rs `Cmd` / `Evt` — des données, aucune logique, sérialisables
    mod.rs      six files (`queue_of`), des threads consommant les mêmes canaux
    executor.rs l'exécuteur tokio partagé, et le pont `block_on`
    watch.rs    surveillance de fichiers (notify), debounce 250 ms
    wire.rs     les trames du fil : postcard, longueur en tête, `PROTOCOL_VERSION`
    remote.rs   le client du fil : lance le serveur, trois threads de pont
  terminal/     émulation
    mod.rs      pty + `Term` alacritty derrière un `FairMutex`
    snapshot.rs grille → lignes et runs de style, sans tenir le verrou
    keys.rs     frappe gpui → octets (séquences xterm)
    mouse.rs    clic et molette → octets, quand le programme les demande
  ui/           tout gpui
    mod.rs      `run()`, `AssetSource`, polices, i18n
    app.rs      `ClaudhubApp` : l'état, la pompe d'événements, le chrome
    topbar.rs   la barre de titre : le menu, les sélecteurs worktree et branche
    repos.rs        les dépôts ouverts et ceux qui manquent — sans gpui, testé
    inflight.rs     les écritures en vol, et ce que la barre en dit — testé
    workspace.rs    les sept écrans, leur dock et la barre qui les choisit
    multiplexer.rs  l'écran de tous les terminaux — le nom d'un projet
    diff_view.rs    la vue de diff, virtualisée
    history_view.rs l'historique et son graphe peint
    tags.rs         le panneau des tags, et les quatre gestes sur un tag
    highlight.rs    coloration tree-sitter d'un diff
    blade.rs        les vues Blade : surcouche du diff, coloriseur de l'éditeur
    branches.rs     ce que le sélecteur de branches liste, et les gestes sur une
    branch_picker.rs   le sélecteur de branches : le filtre, la liste, les actions
    worktree_picker.rs le sélecteur de worktrees : le filtre, la liste, les actions
    review.rs / terminal_view.rs
    server.rs       la mise en route du serveur WSL
    settings.rs     les réglages et leur global
    settings_view.rs le formulaire, et la page « Journal »
    tree.rs         chemins → arborescence repliable, en indices
    file_icons.rs   l'icône et la teinte d'un fichier, d'après son nom
    explorer.rs     l'explorateur, l'éditeur intégré et ses onglets
    db.rs           l'arbre des bases : connexion, base, table, colonne
    db_query.rs     la console SQL, ses complétions et sa table de résultats
    sql_history.rs  les requêtes déjà jouées : dédup, portée, jours — pur
    sql_history_view.rs  le panneau « Historique » et ses gestes
    search.rs       les lignes de la recherche projet — pur
    search_view.rs  la loupe : le champ, la liste, l'aperçu
    conflicts.rs    les conflits et le garde-fou d'une opération à mi-chemin
    worktree_ops.rs création guidée, tâches du projet, intégration
    store.rs        ce qu'on retient par worktree : base, replis, notes
    session.rs      où l'on en était — et la règle du worktree qui s'ouvre
    dialogs.rs      les deux boutons d'un dialogue, que gpui-component ne peint
                    que pour un `AlertDialog`
    notes.rs        le modèle des notes, leur ancrage et leur prompt — pur
    notes_view.rs   les gestes de la relecture annotée et son panneau
    vault.rs        notes, suivi de relecture et TODO en Markdown — pur
    lsp.rs          le pont vers le serveur de langage
    jumps.rs        la piste : d'où l'on vient, où l'on repart — pur
    folds.rs        quels replis un niveau de repli ferme — pur
    hunks.rs        la gouttière de l'éditeur, et la comparaison de lignes — pur
    merge.rs        la fusion à trois voies : ce que chaque côté a fait — pur
    merge_view.rs   les trois colonnes, et le clic qui tranche
    plugin_view.rs  peint l'arbre d'un plugin, et tient son script en vie
    find.rs         la recherche d'un panneau, et son routage
    motion.rs       le lissage de la molette — pur
    vim.rs          les modes de vim de l'éditeur — pur
    surface.rs      une surface de code : le harnais modal, la molette, le zoom
    scroll.rs       la barre de défilement d'un panneau, et son lissage
    shortcuts.rs    les actions, leurs touches, et l'aide qui en sort
    shortcuts_view.rs  la fenêtre d'aide, en deux colonnes
    theme.rs / icons.rs
```

### La boucle Cmd/Evt

Le thread d'interface envoie des `Cmd` par `runtime::Handle::send`. Des threads
workers les consomment (`async_channel` est MPMC, ils partagent le récepteur) et
répondent par des `Evt`. `ClaudhubApp::pump_events` les draine **par lots de
64** dans une tâche gpui de premier plan : un `update_in` par événement forcerait
un cycle d'effets à chaque fois.

Ajouter une opération : une variante de `Cmd`, un bras dans `runtime::handle`,
une ou plusieurs variantes d'`Evt`, un bras dans `ClaudhubApp::handle_event`.
Jamais un appel à git depuis un `render` ou un gestionnaire de clic — la plus
rapide des commandes coûte déjà une frame.

**Et un incrément à `wire::PROTOCOL_VERSION`** dès que la forme d'un message
change, retrait d'une variante compris : les deux bouts du fil sont installés
séparément et postcard est **positionnel**. Piège payé deux fois en fusionnant :
quand deux branches portent chacune le numéro au suivant, git prend la ligne sans
conflit et le fil se retrouve avec deux protocoles sous un seul numéro.

### Les six files

`queue_of` dit, du seul examen d'une commande, dans quelle file elle part. Une
table qu'un test verrouille — une commande mal rangée n'échoue jamais, elle
attend, et c'est la panne qu'on ne diagnostique pas.

- **Lectures** (trois workers) : statut, diff, branches, écritures locales.
  C'est ce qu'une frame attend.
- **Réseau** (un worker) : `fetch`, `pull`, `push`, HTTP d'un plugin, message de
  commit rédigé par un agent — dix à trente secondes. Un seul worker parce que
  deux `fetch` sur le même dépôt se disputeraient le verrou des références.
- **Hooks du projet** (un worker) : `wt new/rm/up/down`. Pas avec les lectures
  (un `up` démarre des conteneurs), pas avec le réseau (un `wt up` y retenait
  tout ce qui se compte en secondes).
- **Fond** (un worker) : résumés, agents, relevé de `wt`, commandes shell d'un
  plugin. Ne doit jamais passer devant un diff qu'on vient de demander.
- **Bases** (deux workers) : ni les lectures (un `SELECT` malheureux emporterait
  un worker sur trois), ni le réseau. Deux, parce que déplier un schéma en
  demande plusieurs à la fois et qu'ils attendent une socket.
- **Recherche** (un worker) : `git grep` coûte une seconde. Un seul, parce
  qu'une recherche se **remplace** — c'est l'identifiant d'envoi qui trie.

Hors des files, deux voies directes : la surveillance de fichiers, remise au
thread du surveillant, et les serveurs de langage, remis à leur hôte.

### Le mode distant

`Handle` a deux modes, et les points d'envoi de la vue n'en savent rien.
**Local** : les files de ce processus. **Distant** : `runtime::remote::connect`
lance `claudhub-server` en enfant et tout passe par des trames sur ses
stdin/stdout (`runtime::wire` — postcard, longueur en tête, un `flush` par
trame) ; c'est le serveur qui refait le tri entre ses files.

Points qui ne se devinent pas :

- **La poignée de main est hors des deux énumérations** (`wire::Hello`, champs
  jamais réordonnés) : elle doit se relire depuis n'importe quelle version. Le
  lecteur la traduit en `Evt::ServerHello`, qui porte ce que la vue ne peut pas
  savoir de sa machine : le `cwd` du serveur, son appartenance à WSL, ses
  `/etc/shells`. Ces derniers vont dans un **statique** de `settings.rs`, le
  formulaire déclarant ses champs par des fermetures qui ne reçoivent qu'un `App`.
- **`connect` ne bloque jamais l'appelant** : un `wsl.exe` froid met des
  secondes, et c'est le thread d'interface qui appelle. Tout ce qui suit arrive
  en événements.
- **La mort du serveur est un événement** (`Evt::ServerLost`), jamais un
  silence. La relance est **manuelle** — un serveur qui meurt en boucle se
  relancerait en boucle — et repasse les dépôts ouverts au serveur neuf.
- **Le plafond de trame vaut aux deux bouts** (256 Mo). Sans cela l'écrivain
  produisait une trame que le lecteur refuse : le fil mourait pour une charge
  trop grosse, ce qui se lit « serveur perdu » et non « ce diff est énorme ». La
  charge est **jetée et journalisée** — perdre un événement vaut mieux que fermer
  le fil, et la vue repose d'elle-même ce qu'elle attend.
- **stdout du serveur appartient au fil.** Un `println!` dans du code worker le
  corromprait ; les traces vont sur stderr, que le client pompe dans les nôtres
  (`target: "claudhub_server"`).
- **Le manche reste vide tant que le serveur n'a pas répondu**
  (`HandleInner::Pending`) plutôt que de retomber sur les workers locaux : sous
  Windows ceux-ci feraient travailler `git.exe` sur des chemins qui n'existent
  pas. Les commandes émises avant sont jetées.

Levier de test : `CLAUDHUB_SERVER_CMD` (par exemple
`target/debug/claudhub-server`) — tout le fil s'exerce sous Linux.
`tests/server_wire.rs` le fait à chaque `cargo test`, avec le vrai binaire.

### La cible Windows

Interface en `.exe` gpui natif (DirectX), workers dans WSL2, et **seuls les
terminaux n'y passent pas** : leur pty reste local (ConPTY). WSLg a été essayé
d'abord et rendait mal.

**Le binaire du serveur est embarqué dans l'exécutable** et installé dans la
distro à la première ouverture (`wsl::ensure_installed`) : `build.rs` le met dans
le `.exe` d'après `CLAUDHUB_EMBED_SERVER`, et l'installation écrit ces octets
dans l'**entrée standard** d'un `cat` lancé là-bas — ni partage réseau, ni chemin
à traduire, ni bit d'exécution perdu. Le chemin voisin reste en repli
(`wsl::bundled_server`) pour le build de développement.

- **`build.rs` écrit une constante, il ne lit rien à l'exécution.** Sans la
  variable, `EMBEDDED` vaut `None`. Une variable posée sur un chemin absent est
  une **erreur de compilation** : un exécutable livré sans son serveur est ce
  qu'on rend impossible. Le chemin est écrit par `{:?}` — sous Windows il est
  plein d'antislashs.
- **L'installation est adressée par le contenu** (`~/.claudhub/bin/<empreinte>`),
  jamais par un numéro de version : une mise à jour s'installe d'elle-même, deux
  `.exe` cohabitent, une version de développement se comporte comme les autres.
- **Rien ne passe par un shell de connexion.** `wsl.exe --exec` lance le
  programme directement : ce qui s'y écrit est un chemin absolu, jamais un `~`.
  D'où `wsl::probe`, qui demande une fois où est le foyer **et quel shell
  appartient à l'utilisateur** — un terminal ouvert par `--exec` n'a pas de shell
  pour interroger `$SHELL`.
- **Le script d'installation n'a pas un seul guillemet**, délibérément : la ligne
  traverse `CreateProcess` puis la reconstruction d'`argv` par `wsl.exe`. Un test
  le verrouille.
- **L'exécutable est un programme *fenêtré*** (`windows_subsystem = "windows"`,
  `main.rs`) : par défaut un binaire Rust est un programme de console, et
  Windows ouvrait une fenêtre noire à côté de l'interface, y déversait
  `env_logger`, et la fermer tuait l'application. Ce qu'elle montrait est ce à
  quoi sert la page « Journal ». Corollaire immédiat : un processus sans console
  à hériter en fait **créer une** à chaque enfant de console, et `wsl.exe` en
  est un — d'où `wsl::no_console` (`CREATE_NO_WINDOW`), sur le manche à
  `wsl.exe` comme sur le lancement du serveur. Sans lui, la fenêtre retirée du
  démarrage revenait en un clignotement par commande.
- **`wsl.exe --list` répond en UTF-16** avant `WSL_UTF8` ; `wsl::decode` gère les
  deux.

**Le fil ne transporte que des chemins Linux**, et la traduction n'existe qu'aux
cinq endroits où un chemin change de monde : le sélecteur de dossier (en
refusant le dépôt d'une *autre* distribution), le coffre de notes, la cible d'un
export CSV, ce qu'un glissement dépose dans l'explorateur, et les deux retours
(ouverture du coffre, chemin d'un export annoncé) qui refont le chemin inverse. `wslpath` est pur et testé sous Linux.

**Les mêmes réglages, deux mondes.** `settings.json` et `state.json` restent côté
Windows — c'est l'état de cette fenêtre — mais contiennent des chemins Linux. La
liste de shells et le shell de connexion viennent du serveur.

Toute écriture git est suivie d'une relecture du statut (`write_then_refresh`).

### Pourquoi le binaire `git` et non libgit2

Les credential helpers, `includeIf`, les hooks, la signature, les alias :
l'utilisateur attend *sa* configuration. Le coût est un `fork` par commande,
invisible à cette échelle.

Corollaires : `stdin` fermé et `GIT_TERMINAL_PROMPT=0` (sinon une invite de mot
de passe bloque un worker pour toujours), `GIT_EDITOR=true` (un
`rebase --continue` ouvre un éditeur), `LC_ALL=C` pour que les messages d'erreur
soient reconnaissables, `GIT_OPTIONAL_LOCKS=0` (sinon `git status` rafraîchit le
cache de `stat` dans `.git/index`, qui est surveillé), et les formats `-z`
partout où un chemin apparaît — un fichier peut contenir un saut de ligne.

`git_opt` existe pour les lectures dont l'échec est la réponse normale ;
`git_tolerant` accepte un code borné, ce dont `diff --no-index` a besoin — il
sort avec **1** dès qu'il trouve une différence, ce qui est le cas normal pour un
fichier non versionné, et le lire par `git` l'affichait vide.

### La surveillance de fichiers

Deux règles, et les enfreindre se paie en fenêtre figée puis en rafraîchissement
en boucle.

**Ce qu'on surveille vient de `git ls-files --cached --others
--exclude-standard`** : les dossiers contenant un fichier suivi ou nouveau non
ignoré, chacun **sans récursion**. Un projet Laravel a quarante mille répertoires
dont sept cents portent du code ; le reste est `vendor/`, `node_modules/` et
`storage/`, qu'un serveur de développement réécrit sans arrêt. Un dossier créé
plus tard est signalé par son parent.

**On ne réagit qu'aux événements qui changent le contenu**
(`watch::changes_content`). inotify signale chaque **ouverture** — `Access(Open)`
— et c'est nous qui les ouvrons : `git status` lisait le worktree, chaque lecture
produisait un événement, chaque événement un `git status`. `Any` et `Other` sont
gardés : c'est ainsi que `notify` signale un débordement de sa file.

**Poser les surveillances ne se fait jamais dans le thread d'interface** :
c'était une demi-seconde de fenêtre figée par changement de worktree. La vue
envoie `Cmd::Watch`/`Cmd::WatchDir`, que `Handle::send` remet directement au
thread du surveillant. Ce qui en revient est un `Evt::FilesChanged`, un **lot**
de chemins par fenêtre de regroupement. Le surveillant vit dans le runtime : le
disque qu'il regarde est celui du serveur quand les workers sont dans WSL.

**Sur un disque Windows monté par WSL, la surveillance ne marche pas et ne le dit
pas.** `notify` pose ses surveillances sur drvfs sans erreur et ne livre jamais
un événement. D'où `watch::on_windows_filesystem`, que la barre d'état affiche.
Pas de repli par sondage : `git status` y coûte déjà plusieurs fois plus cher.

Dans un worktree lié, `.git` est un *fichier* qui pointe vers
`<principal>/.git/worktrees/<nom>` : c'est là que vivent son `HEAD` et son
`index`.

### La vue de diff

**Les deux listes sont virtualisées** — celle des fichiers (une revue de branche
en touche des centaines) et celle des lignes (un diff d'agent fait des milliers
de lignes). C'est `uniform_list` (gpui) et non `v_virtual_list` : toutes les
entrées ont la même hauteur, il trouve l'intervalle visible par une division, et
c'est le seul des deux qui sache défiler horizontalement.

Quatre contraintes tiennent ensemble, et en relâcher une casse une autre :

- **`.h(LINE_HEIGHT)` explicite** sur chaque entrée : la liste réserve la hauteur
  d'un seul item mesuré.
- **`.whitespace_nowrap()`** : sans cela le texte est shapé à la largeur du
  viewport pendant le rendu et à largeur infinie pendant la mesure.
- **`ListHorizontalSizingBehavior::Unconstrained` + `with_width_from_item`** avec
  `Rendered::longest_row`, sans quoi le défilement s'arrête à la largeur de la
  première ligne.
- **pas de `w_full` sur une entrée**, mais un `min_w(content_width)` : `w_full`
  étire l'entrée et il n'y a plus rien à révéler.

Tout ce qui se déduit d'un diff — mise à plat, coloration, patchs d'indexation,
largeur de gouttière — est calculé une fois dans `diff_view::Rendered`, à
l'arrivée du diff, et rangé derrière un `Rc`. La fermeture de rendu est appelée
pour chaque ligne visible à chaque frame : elle ne doit rien y calculer.

**Deux colonnes, une seule référence.** La vue côte à côte (`split_rows`) n'est
qu'un autre agencement de la liste unifiée : ses entrées ne portent que des
**indices dans `rows`**. La copie ramène donc la sélection à la liste unifiée
(`unified_span`), qui seule porte l'ordre du fichier. Corollaire : les indices de
`diff_selection` désignent la liste **affichée**, donc basculer de mode abandonne
la sélection.

**Le repli des lignes longues** (`Settings::diff_wrap`, vrai par défaut) n'existe
**qu'en deux colonnes** : en une seule la ligne a toute la largeur. Il se fait
**à la colonne, comme un terminal, et non aux espaces** — c'est ce qui rend la
hauteur d'une entrée calculable avant de la peindre, la police étant à chasse
fixe et `wrapped_lines` une division. Le shaper de gpui coupe aux mots, et une
hauteur devinée qui ne tombe pas juste laisse les entrées se recouvrir.

- **`v_virtual_list` remplace `uniform_list` ici, et là seulement** : les entrées
  n'ont plus la même hauteur. Le vecteur est reconstruit à chaque frame, ce qui
  ne coûte qu'une division par entrée, `Rendered::row_chars` ayant déjà compté
  les caractères une fois pour toutes.
- **Une seconde poignée de défilement** (`diff_wrap_scroll`). Les deux listes
  n'étant jamais affichées ensemble, tout ce qui vise « la » liste passe par
  `diff_base_handle` et `reveal_diff_row`.
- **Une paire fait la hauteur de sa plus haute moitié**, l'autre complétant avec
  des lignes vides.
- **Le texte est découpé, ses styles avec** (`char_span`, `slice_runs`). Le
  découpage se compte en **caractères** — en octets, une ligne accentuée se
  couperait au milieu d'un caractère et paniquerait — et les plages rendues
  restent **triées et disjointes**, l'invariant que gpui ne vérifie pas.
- **La largeur mesurée n'existe pas à la première frame**, et rien ne redessine
  tout seul. D'où `window.request_animation_frame` tant que la mesure manque,
  borné à quelques frames, et `ClaudhubApp::diff_width` qui retient la dernière
  largeur connue.
- **Et elle est toujours celle de la frame d'avant** : tout ce qui précède est
  calculé au rendu, donc la frame qui suit un redimensionnement — un panneau
  qu'on zoome, une poignée qu'on lâche — est peinte à une taille que la vue n'a
  plus, et **rien ne demande la suivante**, une fenêtre ne se redessinant que sur
  un événement. Le prochain était le balayage de fond, deux secondes plus tard.
  Un `canvas` mesure donc le cadre **après** la mise en page
  (`ClaudhubApp::diff_laid_out`), et une largeur qui a bougé depuis la dernière
  demande la frame qui repeint juste. Ça s'arrête de soi-même : la frame d'après,
  la largeur ne bouge plus.

Ce que le repli ne corrige pas : `page_rows` (Ctrl+D/U) compte des entrées et non
des lignes visibles.

**« Tout le fichier » est un contexte, pas un mode** : `git diff` n'a pas
d'option pour cela, donc `Settings::context_lines` demande `WHOLE_FILE_CONTEXT`,
que git ramène de lui-même. Basculer relit le fichier.

**Les quatre déplacements existent en touches et en boutons** (`step_diff_hunk`,
`step_file`), le **même code** dessous : les flèches appartiennent à qui a le
focus, donc à personne après un clic dans un terminal. `NAVIGATION_PREDICATE`
exclut les champs de saisie, les terminaux et les couches flottantes.

**Aller au hunk suivant défile toujours, et dit où l'on est.** `scroll_to_item`
**ne fait rien quand la cible est déjà à l'écran**, ce qui est juste pour une
flèche et faux pour un pas de hunk : la sélection sautait de quatre cents lignes
et la vue de rien. Le hunk demandé est donc **centré** (`reveal_diff_hunk`), là
où l'œil est déjà et où l'on voit ce qui précède la modification autant que ce
qui la suit — **sauf s'il est plus haut que la vue**, auquel cas il va en haut :
centré, ses premières lignes passeraient au-dessus de l'écran, d'où on ne les
rattrape pas en lisant vers le bas. La mesure se fait en **entrées**, comme
`page_rows`, le choix n'étant qu'entre deux placements. La liste repliée n'a pas
de variante stricte — sauf pour `Center`, qu'elle applique inconditionnellement,
donc qui ne demande rien. C'est `Top` qui demande les deux temps, et son chemin
non strict **n'est pas un haut** : une entrée sous la vue est amenée à son
**bord bas**, et c'est là que l'en-tête `@@` d'un long hunk se retrouvait, sur la
dernière ligne de l'écran. On défile donc d'abord **au-delà de la fin**, la cible
est alors au-dessus de la vue, et la seule branche qui reste est celle qui épingle
une entrée en haut ; le débordement est rogné dans le même prepaint. Et **un filet dans la marge marque le hunk courant** :
une **bordure** et non une bande enfant, donc au même endroit sur un en-tête qui
a du rembourrage et sur une ligne qui n'en a pas ; toujours présente,
transparente hors du hunk courant — une largeur qui apparaît décalerait la ligne.

Haut/bas vont d'une **modification** à la suivante et **débordent sur le fichier
voisin**. Un débordement ne peut pas poser lui-même la sélection : le diff du
voisin n'arrive qu'après la commande git. Le geste est noté
(`ReviewState::pending_jump`) et consommé à l'arrivée. Ce drapeau est réservé au
clavier : ouvrir un fichier à la souris l'efface.

**La liste suit le fichier ouvert**, avec une poignée de défilement **par
domaine** (« Revue » et « Modifications » sont affichés en même temps). Le
défilement est non strict : un clic sur un fichier déjà visible ne fait pas
sauter la liste.

### Une seule liste pour l'index et les modifications

`DiffRange` n'a ni `Unstaged` ni `Staged` : la distinction est un détail de
plomberie git, restitué par **une case à cocher par fichier**. Cocher appelle
`git add`, décocher `git restore --staged`.

Deux endroits où cette simplification pourrait mentir : l'**indexation
partielle** (`MM`), d'où `FileRow::partial` et la mention « partiel », qu'un test
verrouille ; et les **fichiers non suivis**, qui forment leur propre groupe.

**Un bouton d'une ligne consomme son clic.** La ligne entière est cliquable — et
**reprend le focus** — et la case, la coche de relecture et la corbeille sont ses
enfants. Sans `stop_propagation`, la corbeille était inutilisable : le dialogue
s'ouvrait, prenait le focus, puis le clic finissait de remonter et la ligne le
lui reprenait ; Échap et les boutons du pied envoient une action, et une action
se distribue sur le nœud qui a le focus.

La liste est une **arborescence de dossiers** repliable, avec un bouton vers la
liste plate. Trois points : les **dossiers sans embranchement sont fusionnés** ;
la **liste plate reste la référence** (l'arbre n'est qu'un affichage, et un
fichier caché par un repli compte quand même) ; la **case d'un dossier porte tout
son sous-arbre**, d'où `DirRow::paths`.

**Elle s'indente comme l'explorateur, et par le même code** (`theme::
indent_guides`, `theme::chevron_space`) : un retrait proportionnel seul ne
disait rien ici, la ligne d'un dossier commençant par un chevron que celle d'un
fichier n'a pas — la case d'un fichier se retrouvait **à gauche** de celle du
dossier qui le porte, et l'imbrication se lisait à l'envers. Le fichier réserve
donc la place du chevron, en mode arbre seulement — à plat ce serait une colonne
de rien —, et les niveaux au-dessus portent leurs filets.

Corollaire : le diff va de HEAD au répertoire de travail, index compris.
Indexer un hunk isolé sur un fichier *déjà partiellement indexé* peut donc
échouer — `git apply --cached` refuse un patch qui ne s'applique pas.

Le statut est lu avec `--untracked-files=all` : sans cela un dossier entièrement
nouveau apparaît comme une seule entrée qu'on ne peut ni lire ni indexer, et un
worktree d'agent en crée. La suppression passe par `git clean` et non
`remove_file` : il refuse ce qui est suivi.

### Valider, et pousser dans le même geste

Deux boutons sous le champ de message, celui de PhpStorm : « Valider », et
« Valider et pousser ».

- **Un drapeau et non deux commandes** (`Cmd::Commit { push }`), le précédent
  étant `CreateTag` : le commit est local et le push est un aller-retour, donc
  les deux partiraient dans **deux files** que rien n'ordonne — le push pouvant
  alors devancer le commit et envoyer la branche telle qu'elle était.
  `queue_of` lit le drapeau, et la paire voyage dans la file réseau.
- **Une action à elle** (`Action::CommitPush`) : c'est ce qui fait tourner le
  bouton qui a été pressé et non son voisin, et ce qui permet à la barre d'état
  de dire lequel des deux gestes tourne.
- **Ce qui est rapporté est le compte rendu du push**, quand il en écrit un :
  c'est la moitié qu'on a attendue, et la ligne du commit est déjà dans
  l'historique qui suit.
- **Deux boutons et non un menu sur le premier** : les deux gestes se font une
  douzaine de fois par jour, et cacher l'un derrière un chevron coûterait un
  clic à celui qui finit le travail. Le second est en contour — ce qu'il ajoute
  est un aller-retour, et le fond plein revient au geste qui vaut toujours.

### Proposer un message de commit

Le bouton donne le diff indexé à un agent et met sa réponse dans le champ.
`src/commit_msg.rs` tient le prompt et le nettoyage ; le reste est un
sous-processus.

**Un programme, pas une API.** `Settings::commit_message_command`, par défaut
`claude -p --model sonnet`, est une ligne de commande déjà installée et
authentifiée. Une clé d'API aurait sa propre authentification, ses quotas et son
format d'erreur, pour rédiger une ligne de résumé. Le réglage vide fait
disparaître le bouton.

- **Le diff part par l'entrée standard** : une ligne de commande a une longueur
  maximale. Les trois flux passent par des threads, comme
  `git::wait_with_timeout` — un tube plein bloque celui qui écrit.
- **Les sujets des derniers commits partent avec** (`history::recent_subjects`) :
  la convention d'un dépôt — la langue, la personne du verbe, les préfixes — ne
  se devine pas, et une consigne écrite dans le prompt l'imposerait à tous.
- **Le diff est tronqué à `MAX_DIFF`, sur une frontière de caractère**, et la
  coupe est dite dans le prompt.
- **La réponse est nettoyée** (`clean`) : un modèle encadre volontiers sa réponse
  d'un bloc de code, et ce sont des caractères qui finiraient dans l'historique.
- **File réseau, et un délai à lui** : deux minutes (`commit_msg::TIMEOUT`), les
  trente secondes de `git` étant ici un échec quasi certain.
- **Le message retrouve le champ qui l'a demandé** : `Evt::CommitMessage` porte
  son worktree et `suggesting_message` retient lequel attend.

### L'explorateur de projet

**L'arbre vient d'un seul appel git** — `ls-files --cached --others
--exclude-standard` —, jamais d'un parcours de disque : quarante mille
répertoires ouverts un par un coûteraient un appel système chacun pour arriver
aux sept cents qui portent du code.

**Et git s'arrête au dossier qu'il exclut en entier** (`--directory
--no-empty-directory`, le second appel) : sans cela un Laravel rendait **cent
cinquante-quatre mille** chemins ignorés pour huit mille utiles, soit une
seconde de commande — et l'arbre s'ouvrant **replié**, on payait tout cela pour
peindre une dizaine de lignes. C'est cinq cent vingt entrées, dont soixante-neuf
dossiers, et dix millisecondes. Les trois coûts chiffrés plus bas suivent :
l'arbre passe de cent soixante mille nœuds à neuf mille, `dimmed` n'a plus
trente mille feuilles à visiter sous `vendor/`, et la recherche de fichiers —
qui rebâtit un arbre à chaque frappe — cesse d'être le point « pas optimisé et
qui se sait ».

Ce que ça demande, et ce que ça ne demande pas :

- **Un chemin sans rien dessous est un fichier**, pour un arbre bâti de chemins.
  `vendor/` se peignait donc avec une icône de fichier : `git::Files::dirs` dit
  lesquels sont des dossiers, et `tree::Tree::with_dirs` en fait un nœud qui
  **porte son propre indice** — c'est le seul que `vendor/` ait, et griser un
  dossier demande si tout ce qu'il porte est exclu. Le slash final se lit sur le
  **texte** : `PathBuf` le jette, et `vendor` et `vendor/` sont le même chemin
  une fois analysés.
- **Un dossier reste dans cette liste une fois lu**, sans quoi il se dessine
  deux fois — la ligne du dossier que ses fichiers font, et la ligne du fichier
  qu'il paraît être. Ce qui en sort est l'autre liste, `unexplored`, celle qui
  dit ce qu'un chevron a encore à aller chercher.
- **Le contenu se lit par un `readdir` et non par git** (`files::read_dir`), et
  c'est **exact** : git ne descend jamais dans un dossier exclu — le lui
  redemander répond `vendor/` —, donc rien n'y peut être réinclus. Deux
  millisecondes pour les sept cents entrées d'un `node_modules/`. La règle du
  paragraphe précédent n'est pas enfreinte : ce qu'elle interdit est de
  **parcourir** quarante mille répertoires pour trouver les sept cents qui
  portent du code, pas de lire un niveau, à un geste, là où git a renoncé.
- **Ce qui est ouvert et inconnu se fait lire** (`read_open_dirs`) : un chevron
  et une liste qui vient de tout remplacer sont la même question. Un `git add`
  ne referme donc pas le `vendor/` qu'on lisait, et une session reprise
  redemande ce que `expanded` retenait. Le balayage parcourt `expanded` — la
  poignée de dossiers ouverts à la main —, jamais l'arbre.
- **Rien ne surveille là-dedans** : la règle des quarante mille répertoires vaut
  ici plus qu'ailleurs, et un `composer install` se voit en repliant.
- **Les deux listes se refont triées à chaque arrivée**, jamais complétées par
  la fin : `excluded_flags` parcourt deux listes triées de front, et une queue
  hors d'ordre cesse **en silence** de griser tout ce qui la suit. Les exclus
  sont donc **gardés** (`ignored_paths`) et non redéduits des drapeaux — une
  insertion décale tous les indices d'après.

**`ui::tree` ne connaît que des chemins et rend des indices.** Deux listes s'en
servent — la revue et l'explorateur — et elles n'affichent pas la même chose.
Rendre des indices n'est pas un détail : la même feuille apparaît dans le
sous-arbre de chacun de ses parents, et un explorateur de quarante mille fichiers
ferait des centaines de milliers de clones de `PathBuf` par reconstruction.

**L'arbre de l'explorateur est construit une fois**, à l'arrivée de la liste, et
rangé derrière un `Rc`. La liste de revue se reconstruit au rendu : quelques
centaines d'entrées le permettent, des dizaines de milliers non.

**Construire l'arbre et le plier sont deux gestes** (`tree::Tree::new` et
`Tree::rows`), et les confondre coûtait un cinquième de seconde par chevron.
Ce dépôt en donne la mesure : `target/` porte **cent treize mille** fichiers
ignorés, et ouvrir un dossier refaisait tout le travail que la liste n'avait pas
changé. Trois coûts, mesurés sur cette liste :

- **L'arbre lui-même, vingt millisecondes** — un nœud de `BTreeMap` par
  dossier. Un repli ne change rien de cet arbre, seulement les lignes qui en
  sortent : `rows` coûte 0,1 ms replié, 2,5 ms tout ouvert.
- **Le gris de ce que `.gitignore` écarte, deux cents millisecondes.** Un
  dossier n'est gris que si **tout** ce qu'il porte l'est, donc le calcul visite
  chaque feuille de chaque dossier affiché — et chaque visite était une
  **recherche binaire dans une liste de chemins**. C'est désormais un drapeau
  par fichier (`excluded_flags`, une passe sur deux listes triées, à l'arrivée
  de la liste) : 0,2 ms.
- **La liste plate, trois millisecondes par frame** : le rendu la donnait à la
  fermeture de la liste virtualisée en la **clonant**. Un `Rc`.

Le reste suit la même règle : `row_of` — appelé à chaque flèche — parcourt les
lignes sans allouer, là où il en clonait un `PathBuf` par ligne. Ce qui n'est pas
optimisé et se sait : chaque frappe dans la recherche de fichiers rebâtit un
arbre du sous-ensemble, soit une vingtaine de millisecondes sur une liste
pareille.

**Il se parcourt au clavier** (contexte `ClaudhubExplorer`, que
`NAVIGATION_PREDICATE` exclut). Corollaire : un clic dans la liste de revue
**reprend le focus**.

**Le curseur est un chemin, pas un indice** : l'arbre se reconstruit à chaque
repli et à chaque frappe de recherche. Le chercher coûte un parcours par geste,
ce qu'un geste peut payer et pas une frame. **Ouvert et sous le curseur sont deux
choses**, et se voient différemment.

**Les filets d'indentation ne sont pas une décoration** : à six niveaux, plus
rien ne dit à quel dossier une ligne appartient. Ils imposent une hauteur de
ligne explicite (`theme::row_height`), étant en `h_full`. Ils vivent dans
`theme` et non ici : les deux arbres de la fenêtre — celui-ci et la liste de
revue — sont le même geste, et leur couleur est **passée** et non relue,
la fermeture d'une liste virtualisée tournant pour chaque ligne visible.

`reveal_open_file` est le « scroll from source » de PhpStorm, et il n'est **pas**
automatique : une liste de quarante mille entrées qui saute à chaque clic est un
mouvement de trop.

**L'arbre s'ouvre fermé, et c'est une polarité et non une graine.** Une revue se
lit grande ouverte, donc elle retient ce qu'on a **fermé** ; l'explorateur porte
le worktree entier, donc il retient ce qu'on a **ouvert**. D'où `tree::Folds`,
`OpenBut` et `ShutBut`, et un test par polarité. Retenir l'exception garde les
deux ensembles petits. Corollaire : « tout replier » est un `clear`, et « tout
déplier ici » et « tout replier ici » ont échangé leurs corps.

**Ce que `.gitignore` écarte est montré, mais en gris** — la convention de
PhpStorm, et la seule chose qui empêche `vendor/` de se lire comme une partie du
projet. `Settings::show_ignored_files` est vrai par défaut. Trois points : **git
dit lesquels** (`--ignored` est un *filtre*, d'où les deux appels de
`repo::list_files`, la seconde liste triée et interrogée par recherche binaire) ;
**un dossier n'est gris que si tout ce qu'il porte l'est** — ce qu'un dossier
que personne n'a ouvert répond par son seul indice ; **le calcul se fait à la
reconstruction, jamais au rendu** (`Explorer::dimmed`), un dossier demandant de
vérifier toutes ses feuilles.

Les chemins sont **ramenés dans le worktree** (`files::inside`) avant toute
opération : un `../` saisi dans un dialogue de renommage en sortirait.

**Une ligne est un bandeau et non une pastille** — voir « Le grain de
l'interface », la règle vaut pour toutes les listes de la fenêtre, et c'est ici
qu'elle a été comprise : une colonne étroite de noms de longueurs très inégales,
où un fond qui s'arrête au bout du nom se lit comme le nom qui est sélectionné
plutôt que comme la **ligne** où l'on est — celle sur laquelle le clic droit, la
coche et le dépôt vont agir.

### Faire glisser dans l'arbre

Deux glissements, une seule mécanique : gpui traduit le dépôt d'un fichier venu
du bureau en un **glissement interne** portant des `ExternalPaths`, si bien que
les fichiers de l'explorateur de Windows ou de Linux et une ligne de notre arbre
se reçoivent avec les mêmes écouteurs (`accepts_drops`).

- **Ce qui vient du dehors est une copie, jamais un déplacement**
  (`files::Op::Import`) : le fichier déposé appartient à quelqu'un d'autre — un
  téléchargement, l'export d'un outil de dessin — et un geste de la main n'a pas
  à vider le dossier d'où il vient. L'exception est un chemin **déjà dans ce
  worktree**, déposé depuis un gestionnaire de fichiers ouvert sur le projet :
  là, le geste est celui que l'arbre fait tout seul, et il déplace.
- **Rien n'est jamais écrasé** : un dépôt sur un nom qui existe est **refusé** et
  le dit. Ce que la main désigne en une seconde peut être une heure de travail,
  et renommer est à un clic droit.
- **Un déplacement est un `Op::Rename`** : c'est ce que déplacer *est*, et cela
  n'a rien coûté au protocole. Ce que `move_within` refuse — pur et testé — est
  ce qui ne veut rien dire : reposer une chose où elle est, et faire entrer un
  dossier dans lui-même ou dans l'un de ses enfants, seul des trois à faire des
  dégâts.
- **Une ligne de fichier accepte un dépôt et le remet à son dossier**
  (`drop_dir`) : viser la ligne propre d'un dossier, dans une liste où un dossier
  de trente fichiers est une ligne sur trente-et-une, est une précision que
  personne ne devrait avoir à avoir. Le panneau entier en accepte un aussi, pour
  la **racine** : un projet dont la racine porte trois fichiers laisse la colonne
  vide, et y viser une ligne serait viser rien. Son cadre est **toujours
  réservé** — une bordure qui apparaît décalerait toutes les lignes d'un pixel.
- **Un dossier fermé s'ouvre quand le glissement s'attarde dessus**
  (`DRAG_HOVER`, 600 ms) : c'est le geste de tout gestionnaire de fichiers, et le
  différé est ce qui rend la traversée gratuite. Le piège est que `on_drag_move`
  se déclenche à **chaque** mouvement de la souris, où qu'elle soit — sans le
  test des bornes, chaque ligne du panneau se croirait sous la main.
- **La couleur d'une cible n'est pas celle du survol** : les deux sont à l'écran
  en même temps, et un `drag_over` est appliqué **après** le `hover` comme après
  le fond de la ligne ouverte.
- **Le fantôme est peint par nous** (`DraggedEntry`, qui est à la fois la charge
  et la vue) : la plateforme ne dessine rien pour un glissement interne, et un
  glissement que rien ne suit se lit comme un clic qui n'a pas pris.
- **Le chemin déposé change de monde** comme la cible d'un export CSV : sous
  Windows il vient de *cette* machine et les workers sont dans la distribution.
  Ce que la distribution ne peut pas atteindre est refusé et dit — c'est la
  cinquième frontière de `wslpath`.

### L'icône d'un fichier

Trois listes en portent, parce que le geste central de Claudhub est de parcourir
des listes de fichiers.

**Un glyphe par langage, pas par famille.** Les icônes Lucide ne connaissent que
des catégories : tout le code y serait le même `file-code`. Les marques viennent
de **simple-icons** (CC0), dans `assets/icons/lang/` — un dossier à part, licence
et dessin différents. Ce sont des repères visuels, pas une revendication.

**La teinte vient de la coloration syntaxique du thème**, pas d'une palette à
nous et **pas des couleurs de marque** : un logo noir sur fond sombre est un
trou. `keys_of` verrouille l'existence des noms de style dans tous les thèmes
livrés. La teinte n'a donc **aucun sens sémantique**.

Quatre passes, de la plus précise à la plus large : le **nom entier**
(`package.json` est de npm avant d'être du JSON), les **familles d'outils**
(`.eslintrc`, `eslint.config.js`), les **doubles extensions** (`.blade.php`),
puis l'**extension**.

Les deux grandes tables sont **triées et interrogées par recherche binaire**, et
un test le vérifie : un ordre cassé ne provoque aucune erreur, il fait rater des
entrées. Un autre test vérifie que chaque icône nommée est sur le disque **et
embarquée** — un fichier présent mais non embarqué donne une case vide, seulement
en release.

### Le serveur de langage

L'éditeur de gpui-component porte déjà un `Lsp` — trois providers et un
`DiagnosticSet` — avec popovers, soulignements et gouttière. Ce qui manquait est
le **client**, et le chemin d'un provider appelé dans une frame à un worker qui
répond trente millisecondes plus tard.

**Un bouton « LSP » par worktree, éteint par défaut** : un serveur tient des
centaines de mégaoctets, et douze worktrees ouverts ne sont pas douze worktrees
qu'on édite. L'état vit dans le magasin (`state.json`) et non dans les réglages.
Le bouton est dans la **barre de l'éditeur** — une action va où se fait le geste
dont elle est la fin. Il porte quatre états et non deux : éteint, en démarrage,
prêt (avec le compte), en échec avec la raison. Et il n'apparaît **que si quelque
chose sert ce fichier**.

**Qui sert quoi.** Les serveurs se déclarent dans le `wt.toml` du projet
(`[lsp.<nom>]`) **et** dans les réglages (`Settings::lsp`) ; le projet l'emporte
pour un même langage. `lsp::pick` prend la **plus longue extension qui
correspond**, si bien qu'une vue Blade va à `blade.php` et non à `php`. Trois
entrées sont livrées — PHPantom, rust-analyzer, typescript-language-server — et
aucun binaire n'est **téléchargé**.

**Le `languageId` n'est pas l'extension**, et c'est le piège que PHP cachait :
`php` est son propre nom, si bien que deviner marchait partout où on regardait.
Un `.ts` annoncé `ts` est refusé, et `.tsx` est un **autre langage**. D'où
`lsp::language_of` ; `language_id` d'une déclaration l'emporte.

**Une session par worktree**, et ouvrir un fichier d'un autre langage la
remplace.

- **C'est une voie, pas une file.** Un serveur vit des heures, tient un état et
  pousse des messages qu'on n'a pas demandés. Et **l'ordre compte** : un
  `didChange` doit atteindre le serveur avant la complétion qui en dépend, ce que
  plusieurs workers sur un canal partagé ne promettent pas.
- **Un seul propriétaire par session.** Le fil de session reçoit les ordres de la
  vue et les messages du serveur sur le même canal : pas de verrou, et aucun
  moyen d'écrire une requête au milieu d'une autre. Le processus meurt avec la
  session, toujours.
- **Du JSON brut sur le fil.** `lsp-types` est sous la feature `ui`, et postcard
  ne sait pas relire un `Value`. Les `Cmd`/`Evt` portent donc des chaînes.
- **Une requête ne reste jamais en attente** : un canal à un coup rangé sous un
  identifiant qui ne recule jamais — le motif de la console SQL. Le cœur expire
  au bout de quinze secondes, la mort d'une session échoue tout ce qu'elle
  portait, et une complétion est **annulée** (`$/cancelRequest`) par celle qui la
  remplace.
- **L'édition est calculée, pas devinée** : `lsp::sync` prend le préfixe et le
  suffixe communs. L'unité est le **code UTF-16**, ni l'octet ni le caractère.
  Les changements sont **groupés** (120 ms).
- **Les providers ne sont posés que pour ce que le serveur sait faire**, et
  reposés à l'arrivée des capacités, la première pose ayant eu lieu avant qu'on
  les connaisse.
- **Un serveur qui demande quelque chose doit être servi**
  (`client/registerCapability`, `workspace/configuration`, une barre de
  progression) : le reste reçoit l'erreur que la spécification prévoit. Ne rien
  répondre est la seule chose qui ne doit pas arriver.

**Les actions de code** (`Ctrl+.`) demandent trois choses qu'un client naïf rate :
**une action se résout avant de s'appliquer** (`codeAction/resolve` — un `data`
sans `edit` veut dire « je calculerai si tu choisis ») ; **`workspace/applyEdit`
est une requête du serveur** qui traverse jusqu'à la vue, seule à tenir le tampon
(`Evt::LspApplyEdit` → `Cmd::LspApplied`), et le serveur attend son oui ou son
non ; **ce qui touche un autre fichier est refusé**, et le refus se dit — toute
écriture repart avec l'empreinte de ce qu'on avait lu ; et **les modifications
s'appliquent à l'envers**, leurs positions décrivant toutes le texte tel qu'il
est maintenant.

**Les jetons sémantiques** sont la coloration que le serveur donne, là où
tree-sitter ne donne que celle de la grammaire. Quatre points :

- **La légende est traduite dans le vocabulaire de nos thèmes** (`lsp::theme_name`,
  deux tests). Un jeton ne porte qu'un **indice** dans la légende du serveur, et
  l'éditeur résout ce *nom* contre le thème ; nos palettes parlent tree-sitter,
  qui n'a ni `parameter` ni `enumMember`. Un nom que le thème ignore ne rend
  aucun style — échec **silencieux**, indiscernable d'un serveur muet. Un nom
  pointé est gratuit (`function.method`) ; un nom **vide** est délibéré.
- **L'ordre est celui du serveur**, puisque c'est à lui que les indices
  renvoient. Une entrée déplacée recolore tout un fichier de travers.
- **`full` ou `range`, selon ce que le serveur annonce**, pas selon le nom du
  trait : PHPantom déclare `full: true, range: false`.
- **Un flux vide est souvent la bonne réponse** : PHPantom en `contextual`
  n'émet que ce qu'une grammaire ne peut pas savoir.

Les **modificateurs** (`deprecated`, `readonly`, `static`) sont reçus et mappés
sur rien : il n'y a rien à quoi les accrocher pour l'instant.

Ce que la v1 ne fait pas : seul le fichier ouvert dans l'éditeur est synchronisé
(rien dans la vue de diff, qui n'est pas un document), pas de renommage, pas de
formatage, pas de recherche de symboles.

**Le fil ne transporte que des chemins absolus, et c'est le seul endroit de la
fenêtre où c'est vrai** : une URI est absolue ou elle n'est rien, et
`file://app/User.php` se lit « l'hôte `app` ». La jonction se fait aux quatre
frontières de `ui::lsp` (`full`, `local`). L'oubli ne provoque aucune erreur :
les diagnostics reviennent classés sous un chemin que l'éditeur n'emploie pas,
donc invisibles. Un test tient l'aller-retour.

**Ce que le serveur raconte va dans le journal** (`target: "lsp"`), stderr
compris. `$/progress` fait exception et remonte en `Evt::LspBusy`, dans
l'infobulle du bouton.

**Le test qui compte ne lance pas de processus** : `Session::run` prend ce dans
quoi il écrit et ce qu'il entend. `tests/lsp_phpantom.rs` fait l'autre moitié et
**se saute** quand le binaire n'est pas installé. Ce qu'il vérifie est la
**mécanique**, jamais le contenu.

### Aller quelque part, et revenir

**`gd` ne passe pas par le `GoToDefinition` de gpui-component** : leur action lit
`hover_definition`, que seul un survol avec la touche système remplit, si bien
qu'au clavier elle ne fait **rien**, en silence. La requête est la nôtre, posée
au caret ; la **première** réponse est prise, choisir demanderait une liste.

**`Ctrl+O` et `Ctrl+I` sont des liaisons, pas des touches de vim**, déclarées
sous `ClaudhubEditor` : une liaison s'exécute **avant** l'écouteur en phase de
capture par lequel les touches de vim passent, donc elles marchent le mode
éteint. Un contexte à lui plutôt qu'`Input` : les champs de recherche en sont, et
n'ont pas de piste. `F12` complète.

**Une place est un fichier *ou* un écran** (`jumps::Place`), et c'est une seule
piste. Le geste qui manquait est celui qui traverse : on lit une erreur dans
Sentry, on ouvre la ligne qu'elle nomme, et l'éditeur s'ouvre sur un autre écran
— d'où l'on ne revenait pas. Deux pistes auraient fait deux « retour » qui ne
font pas la même chose, et aucune n'aurait su défaire un mouvement commencé sur
l'une et fini sur l'autre.

- **Un écran ne porte que son nom.** Ce qu'on y retrouve — l'issue qu'un plugin
  tient ouverte, la liste d'une revue — est **son** état, qui n'a pas bougé
  pendant l'absence. Le recopier dans la piste en ferait une seconde copie, qui
  vieillirait toute seule.
- **La console SQL est l'exception que cette règle gagne** (`Place::Query`) :
  son état est précisément ce qu'un geste **remplace** — suivre une clé
  étrangère, ouvrir une table —, si bien que revenir à « l'écran des bases »
  reviendrait à la requête qui a pris la place de celle qu'on lisait. C'est la
  requête **envoyée** et non le texte en cours de frappe : elle seule a produit
  ce qui était à l'écran, et elle seule peut le remettre.
- **Un pas en arrière rejoue la requête**, là où la reprise de session ne la
  rejoue pas : un pas est un geste demandé maintenant, et ce vers quoi on
  revient est le **résultat**. Il n'est pas classé dans l'historique — la
  requête y est déjà, une ligne plus haut, et un `×2` compte les fois où une
  question a été posée, pas les fois où on est repassé devant.
- **`here()` répond l'écran, sauf quand un fichier est ouvert *et* affiché** —
  un fichier resté derrière un autre écran n'est pas où le geste se fait — **ou
  qu'une requête a été envoyée**.
- **Ce qui s'inscrit est ce que le code a décidé pour vous** : ouvrir un fichier
  (`open_at`), ouvrir la console d'une table, suivre une clé étrangère
  (`run_db_sql`), le script d'un plugin, une page de réglages qu'un panneau
  demande — `travel_to` ou `record_step`, jamais `enter_workspace` seul.
- **Changer d'écran au bouton s'inscrit** : les deux groupes de la barre d'état
  et de la barre de titre passent par `travel_to`. Un écran qu'on quitte au clic
  est un endroit où l'on faisait quelque chose, et `Ctrl+O` — ou le bouton 4 —
  est ce qui y ramène. **Le clavier reste dehors** (`Alt+4`, `Ctrl+Maj+F`,
  l'entrée « Réglages » du menu) : une touche se défait en pressant celle d'où
  l'on vient, et la piste se remplirait de ce qu'on n'a jamais voulu défaire.
- **Deux fois la même place à la suite n'en font qu'une.** Revenir à Sentry à la
  main puis ouvrir une seconde frame l'écrirait deux fois, et l'un des deux pas
  en arrière ne changerait **rien à l'écran** — le seul pas qu'on ne distingue
  pas d'un bouton cassé.
- **Les boutons 4 et 5 de la souris sont la même piste** : gpui les rend en
  `MouseButton::Navigate`, X11 depuis les boutons 8 et 9, Wayland depuis
  `BTN_SIDE` et `BTN_EXTRA`. L'écouteur est sur la racine, en phase de remontée
  — rien en dessous ne les écoute, terminal compris, qui ne passe que le bouton
  gauche au programme. `Alt+←` et `Alt+→` les doublent au clavier, hors du
  terminal.
- **Les deux flèches sont dans la barre de titre**, seul chrome qui traverse les
  écrans que la piste traverse. Celles de la barre du fichier restent, sur la
  même piste.
- **La piste est un module pur** (`ui::jumps`), et ce qui s'y teste est ce qui ne
  se devine pas — ce qu'un nouveau saut fait à ce qu'on avait rembobiné.
- **Une piste par worktree**, et elle s'en va avec lui. Un pas ne fait donc
  jamais changer de worktree : c'est le mouvement le plus lourd de la fenêtre, et
  le plus surprenant à défaire tout seul.
- **Le départ est lu au moment du saut**, jamais repris dans la piste : c'est la
  différence entre une piste et une liste de signets. Un pas en arrière réécrit
  de même l'endroit qu'il quitte.
- **Un nouveau saut jette ce qui était devant.**
- **La reprise de session ne s'inscrit pas** : revenir où l'on était n'est pas un
  saut qu'on doit pouvoir défaire.
- **Un atterrissage centre, une motion non** (`Place`). `Nearest` ne défile que
  le minimum ; un saut laissé au minimum posait le symbole sur la **dernière**
  ligne du panneau, or lire une définition c'est lire ce qu'il y a autour.
- **Un caret peut attendre d'être révélé** : le fichier qui vient d'être ouvert
  installe un `EditorState` que rien n'a mis en page — ni lignes visibles ni
  hauteur de ligne, donc défiler jusqu'au caret est une division par rien. La
  révélation est gardée (`Editing::reveal_at`) et retentée à chaque frame jusqu'à
  ce qu'une mesure existe, bornée comme la première largeur du diff.
- **Atterrir donne le focus à l'éditeur** : les touches de la piste vivent sous
  `ClaudhubEditor`, et `enter_workspace` n'en donne que si l'écran **change**.
  Ouvrir depuis l'arbre ne passe pas par là — parcourir au clavier doit laisser
  les flèches à l'arbre.
- **Un saut vers un autre fichier ne pose pas son caret lui-même** : le texte
  n'arrive qu'un aller-retour plus tard. Le geste est noté (`explorer::Pending`),
  et il porte son worktree.

Les deux flèches de la barre du fichier sont ces deux touches. Elles sont
**toujours là**, éteintes quand il n'y a nulle part où aller.

### Lire et retoucher un fichier

`InputState::code_editor(langue).line_number(true)` fournit la coloration,
l'auto-indentation, les numéros et la recherche. **La police, la taille et la
hauteur de ligne se disent explicitement** : sans la première, l'éditeur hérite
de la police proportionnelle de l'interface ; sans la troisième, il garde le
`line_height(Rems(1.25))` d'`Input`, calé sur le rem et donc **sourd à la taille
du texte** — zoomé, les glyphes grandissent, les lignes non. Au-delà de
`files::MAX_LINES`, l'ouverture est **refusée** avec un renvoi vers l'éditeur
externe.

**Et l'éditeur ne porte pas de carte à lui** (`appearance(false)`, les deux
surfaces de code) : `Input` peint un fond, un rayon et une bordure, si bien
qu'au milieu de la carte du groupe d'onglets il dessinait une boîte arrondie
encadrée dans un cadre — et son fond est `editor.background`, la couleur même
dont la carte est peinte. Il n'en restait que le filet gris, qui recousait ce
que le dock venait de découdre. Le code est **sur la carte**, comme le diff
d'à côté. Corollaire pour la console SQL : c'est le conteneur qui porte la
couture avec la grille (`border_b`), la poignée de redimensionnement ne
peignant rien au repos.

**L'écriture est conditionnelle** (`files::write`, `expect`) : un agent écrit
dans les mêmes fichiers pendant qu'on les relit. Corollaire : après un
enregistrement réussi, l'empreinte retenue suit ce qu'on vient d'écrire.

L'éditeur **a son écran** (« Édition »). Ouvrir un fichier **bascule dessus** :
le geste vient de l'explorateur mais aussi d'une ligne de diff.

**Un jeu d'éditeurs par worktree** (`ClaudhubApp::editings`, `explorer::Editors`),
et `editing()` rend celui qui est à l'écran. Deux corollaires : l'abonnement qui
allume « non enregistré » **capture son worktree et son chemin** — marquer « le
courant » poserait l'étoile sur le mauvais onglet — et les éditeurs d'un worktree
qui s'en va s'en vont avec lui.

**Un onglet par fichier, et c'est la barre du dock** (`panels::FilePanel`), comme
pour les terminaux : une barre à nous serait un second chrome peint sous celui
que la fenêtre a déjà, et c'est ce qui permet de glisser un fichier dans un
split. Un panneau et non six — un fichier ne se lit que sur l'écran « Édition ».

- **Le panneau qui se peint *est* le fichier qu'on lit.** Un groupe d'onglets
  n'en dessine qu'un ; être dessiné dit donc lequel est à l'écran. C'est un fait
  qu'on lit dans la frame plutôt qu'un événement qu'il faut attraper, et tout ce
  qui en découle — le document ouvert du serveur de langage, ce que la session
  retient — part de là (`show_file`).
- **`EditorPanel` n'est plus que l'état vide** : il faut bien que le groupe
  tienne quelque chose quand rien n'est ouvert — un centre vide n'a nulle part où
  déposer le premier fichier —, et il s'efface dès qu'un onglet arrive.
- **Rouvrir un fichier réutilise son onglet**, son contenu remplacé : c'est ce
  que fait un enregistrement suivi d'une relecture, et deux onglets sur un
  fichier est la seule chose qu'une barre ne doit pas faire pousser.
- **Une session tient un seul document ouvert côté LSP** : l'onglet qu'on quitte
  est fermé, celui qui arrive est ouvert. C'est l'invariant d'avant les onglets,
  gardé tel quel.
- **Fermer est idempotent** : la croix, `Ctrl+W` et le retrait du panneau
  repassent par `close_file`, et retirer le panneau rappelle `on_removed`.
- **Les onglets sont élagués de `layout.json`** comme les terminaux, et pour une
  raison d'un cran différente : leur contenu est un **texte lu sur le disque**,
  qu'un constructeur ne peut pas aller chercher sans un aller-retour — un onglet
  relu serait un cadre vide. Ce qui était ouvert est retenu là où l'est le reste
  de l'endroit où l'on en était : dans le magasin.
- **Ils se redemandent un par un** (`read_next_file`) : trois workers servent les
  lectures, donc les réponses reviennent dans l'ordre du disque — et cet ordre
  serait celui des onglets. Celui qu'on regardait est ramené devant **à la fin**,
  chaque ouverture faisant du sien l'onglet affiché.

### Ce que la gouttière de l'éditeur marque

Une bande dans la gouttière dit ce qui a bougé depuis `HEAD` — verte pour un
ajout, bleue pour une modification, un moignon sur la frontière pour une
suppression —, et un clic ouvre ce qui était là avec de quoi le rétablir. C'est
le geste de PhpStorm, et il vaut d'abord pour ce qu'il évite : ouvrir l'écran
Git pour savoir si la ligne qu'on relit est de soi.

**Elle répond pendant qu'on tape, et c'est ce qui décide de tout le reste.**
`git diff` compare le fichier **sur le disque**, or le tampon qui compte est
celui sous le curseur. Le texte de base est donc lu une fois
(`repo::head_blob`, `git show HEAD:./<chemin>`) et la comparaison refaite **en
mémoire** à chaque frappe — `ui::hunks`, pur et testé, du même ordre de coût que
la coloration et très en dessous d'une frame.

- **Une différence de lignes écrite ici, sans crate** : la comparaison est
  *patience* — s'ancrer sur les lignes qui n'apparaissent qu'une fois de chaque
  côté, recommencer entre les ancres. Un script d'édition glouton est libre
  d'apparier l'accolade fermante d'une fonction avec celle de la suivante, et la
  marque enjambe alors deux choses qui n'ont rien à voir. Là où **rien n'est
  unique** — une pile d'accolades, un tableau de lignes jumelles —, le passage
  entier fait **un** hunk : c'est moins fin, et au moins c'est vrai.
- **Le saut de ligne final est une ligne** (`split('\n')`, jamais `lines()`) :
  sinon ajouter la newline manquante ne se verrait nulle part.
- **Sans base, aucune marque** — et non « tout est neuf » : un fichier que git
  ne suit pas n'est pas un fichier qu'on a modifié, et le peindre en vert du
  haut en bas dit autre chose que ce que la gouttière veut dire.
- **La bande vit dans la marge que la gouttière avait déjà**, et ne lui coûte
  aucune largeur. Une colonne à elle éloignait le texte des numéros de dix
  pixels de plus — par-dessus les dix-huit de la colonne de repli, ça se lit
  comme une erreur —, et la réserver ou non selon qu'il y a des marques aurait
  fait glisser le document à la frappe qui annule la dernière modification. Son
  bord droit est celui de la gouttière, donc la bande touche quand même le
  premier caractère.
- **Le renderer est reposé, pas relu** : celui que l'élément appelle ne reçoit
  qu'une ligne — aucun `App` où aller chercher quoi que ce soit —, donc tout ce
  qu'il répond est **capturé**. C'est payable parce que ça ne se refait qu'à un
  geste (une édition, un clic), jamais à une frame. Les couleurs font exception
  et se lisent dans le `canvas` au moment de peindre : un changement de thème
  n'est signalé à personne, et une bande capturée dans l'ancienne palette y
  resterait jusqu'à la frappe suivante.
- **La base est redemandée quand une écriture git aboutit** (`action_done`), pas
  à chaque rafraîchissement de statut : un commit déplace `HEAD` sous les
  fichiers ouverts et les marques doivent se taire, mais un statut arrive à
  chaque fichier que le surveillant voit bouger et cela coûterait un `git show`
  par onglet.
- **Ce qu'un clic ouvre est un bandeau, pas un popover ancré à la ligne** : les
  lignes de l'éditeur vont et viennent avec le défilement, donc une ancre sur
  l'une est une ancre que le premier cran de molette emporte — c'est la décision
  déjà prise pour une ligne annotée (voir « Annoter une relecture »). Le
  bandeau défile au lieu de grandir : un hunk peut faire deux cents lignes, et
  pousser l'éditeur hors du panneau reviendrait à retirer le fichier pour en
  montrer un morceau.
- **Le rétablissement est une édition, pas un `git apply`** : le tampon porte du
  travail non enregistré, git ne voit que le disque, et `repo::apply_patch`
  écrit de toute façon dans l'**index**. Passer par l'éditeur met le geste dans
  le même journal de transactions que le reste — `u` le reprend — et laisse le
  fichier modifié pour l'enregistrement qu'on fera.
- **Ce qui se teste est la plage d'octets**, et c'est là que ça se trompe en
  silence : rejouer tous les hunks à l'envers doit rendre la base au saut de
  ligne près. La newline appartient à la ligne qui la précède, sauf pour un
  passage qui atteint la fin du fichier, où il n'y en a pas après — un span
  faux d'une newline emporte une ligne voisine.

Ce que ça a coûté au fork : le quatorzième commit, la gouttière n'ayant aucun
crochet applicatif (voir « Le grain de l'interface »).

L'éditeur externe se déclare par une commande avec `{path}` et `{line}`. Il est
lancé **détaché**, ses sorties jetées : un éditeur graphique ne rend la main
qu'à sa fermeture.

### Quel domaine de revue s'ouvre

`app::initial_range` choisit, au **premier** statut d'un worktree : ouvrir sur un
domaine vide alors que l'autre est plein fait croire que Claudhub ne voit rien.
Un worktree d'agent est propre et n'a que des commits à relire, d'où le repli sur
la revue de branche, qui attend que la base soit connue (`None` plutôt que
trancher). Ensuite la portée appartient à l'utilisateur, d'où `range_chosen` — un
rafraîchissement de statut, et il en arrive un à chaque écriture, ne doit jamais
reprendre la main.

Les onglets « Modifications » et « Index » portent leur compte ; les deux autres
portent sur des commits et leur compte coûterait une commande git de plus par
rafraîchissement.

`review::rows_for` est la seule vraie décision de cette vue, libre et testée : le
statut est la source pour les modifications en cours (lui seul distingue index et
répertoire de travail), `--numstat` pour ce qui parle de commits.

**`ReviewState::files` est une table par domaine** : une seule liste ferait
clignoter l'un des deux panneaux quand l'autre se recharge. Chaque panneau
demande la sienne au rendu (`ensure_files`), avec un garde `pending_files` — sans
lui, chaque frame relancerait la commande. Une liste ne se vide jamais avant
d'avoir de quoi la remplacer.

`ReviewState::range` désigne le domaine du **fichier ouvert en dernier**, quel
que soit le panneau d'où le clic vient.

Le diff est au **centre** : les zones latérales occupent toute la hauteur, et le
diff à droite couperait les terminaux en deux.

### Le graphe d'historique

`git::history` lit les commits **et** calcule la disposition : `git log --graph`
produit un dessin en caractères qu'il faudrait re-parser. L'algorithme est celui
de tous les visualiseurs — une liste de rails, chacun attendant un commit ; les
rails libérés sont réutilisés avant d'en ouvrir de nouveaux.

`layout` rend **exactement autant d'entrées que de commits** : la vue les affiche
côte à côte, et un décalage d'une ligne ferait pointer chaque trait sur le
mauvais commit.

Le graphe est peint par un `canvas` **par ligne**, ce qui permet à la liste de
rester virtualisée. La puce est peinte en dernier pour recouvrir les courbes.

Sélectionner un commit remplit la liste des fichiers sous le graphe. Le module
s'appelle `history` et non `log` : un module `log` masquerait la bibliothèque de
journalisation.

L'historique se charge au **rendu** de son onglet et non à la construction, d'où
`history_pending`, sans lequel chaque frame relancerait la commande.

### Les tags

Un tag est le seul objet git qu'une relecture crée **exprès**. Le panneau est un
onglet **à côté de « Historique »** : un tag nomme un commit, et le geste qu'on
fait après avoir trouvé le commit à marquer est juste là.

**Le local et le distant sont deux savoirs différents.** Lister les tags est une
lecture de `refs/tags` ; savoir si `origin` a un tag est un `ls-remote`. Tant que
le bouton globe n'a pas été pressé, **rien ne dit quoi que ce soit du distant**.

- **Le message décide du genre** : un message donné fait un tag annoté, sans
  message un tag léger — c'est la distinction de git, et un interrupteur dirait
  la même chose deux fois.
- **Une création qui pousse est *une* commande** (`Cmd::CreateTag { push }`),
  seul endroit qui déroge à « un geste, une commande » : deux commandes
  partiraient dans **deux files**, que rien n'ordonne, et le push pourrait partir
  avant que le tag existe. Un test verrouille les six commandes et leur file.
- **Le commit visé est celui qu'on lit** : le sélectionné dans l'historique, HEAD
  sinon.
- **Le nom est validé sous le champ** (`tags::is_valid_name`, pur et testé) : le
  refus de git arrive après la fermeture du dialogue.
- **Un tag annoté rapporte le commit qu'il marque** (`%(*objectname:short)`).
- **`ls-remote` liste un tag annoté deux fois** — l'objet, puis le commit sous
  `^{}` : `parse_remote` le sait.
- **Un push de tag écrit `refs/tags/<nom>` en entier** : une branche et un tag
  peuvent porter le même nom.

**Supprimer ici et sur origin sont deux entrées de menu**, pas un drapeau : ce
sont deux regrets différents, et les deux passent par une confirmation.

La lecture est gardée par `loaded` et non par « la liste est vide » — un dépôt
sans tag redemanderait à chaque frame — et un rafraîchissement **oublie** ce
qu'on savait du distant.

### Les réglages

Ils vivent dans un **global gpui** (`settings::SettingsStore`) et non dans
`ClaudhubApp` : le formulaire de gpui-component déclare chaque champ par un
couple lire/écrire dont les fermetures ne reçoivent qu'un `App`, sans accès à
l'entité racine. `Settings::global(cx)` lit, `Settings::update_global` écrit.

`update_global` ré-applique le thème à chaque modification, même celles qui ne le
concernent pas : trier les champs coûterait plus que un `refresh_windows` de
trop. L'écriture du fichier est **différée d'une demi-seconde** — un champ émet
une valeur par frappe. Pas de bouton « Appliquer » : choisir une police sans en
voir l'effet serait choisir à l'aveugle.

**Un écran et non un dialogue.** Une modale couvrait ce qu'on réglait et ne
pouvait pas rester ouverte à côté de son effet. Trois conséquences :

- **La fermeture de rendu tourne à chaque frame.** Ce que le formulaire demandait
  au système est donc lu **une fois et retenu** (`settings_view::Environment`) :
  énumérer les polices et faire un `stat` par ligne d'`/etc/shells` au rythme des
  images est une entrée-sortie au milieu d'une frame. Le registre des thèmes se
  relit à chaque fois — il est surveillé. Le cache est vidé à l'arrivée du
  serveur distant.
- **La page demandée passe par l'identifiant du formulaire.**
  `default_selected_index` n'est lu qu'à la **création** de l'état, lequel vit
  aussi longtemps que l'`id`. `open_settings_at` incrémente donc un compteur qui
  entre dans l'`id` ; `Page::First` veut dire « là où vous en étiez ».
- **L'écran des réglages n'a pas de colonne de gauche** : le formulaire a sa
  propre barre latérale de pages.

Les familles proposées viennent de `cx.text_system().all_font_names()`, filtrées
par convention de nommage pour les champs à chasse fixe : gpui n'expose pas cette
propriété de façon portable. La liste rate donc des familles, et le fichier reste
modifiable à la main.

Corollaire pour les vues : ce qui dépend d'un réglage se **relit à chaque
rendu**, jamais à la construction. `TerminalView::sync_font` en est l'exemple —
changer la police invalide la géométrie mesurée, et il faut effacer les bornes
retenues pour que le pty soit redimensionné.

### Le journal

La page « Journal » montre ce que Claudhub a écrit depuis son démarrage : une
application graphique n'a pas de console sous sa fenêtre, et sans elle il faut
reproduire le problème avant d'avoir le droit de le regarder.

`logging::init` installe **notre** `log::Log` par-dessus celui d'`env_logger` :
il lui passe l'enregistrement puis en garde une copie. Un `format` d'`env_logger`
reçoit un puits d'octets et ce qui va être **imprimé**, quand la page veut le
niveau et la cible tels quels.

**Trois étages, trois niveaux** :

- `git::report` file **chaque** commande à `debug` — commande, répertoire, durée
  —, échecs compris. `debug` et non `warn` pour l'échec : `git_opt` existe pour
  les lectures dont l'échec est normal. L'exception est une commande qui
  **traîne** : passé la seconde, elle explique une interface qui semble bloquée,
  et elle passe à `info`.
- `runtime::handle` nomme **chaque commande** et la chronomètre. Le nom vient de
  `Cmd::name`, un match exhaustif : ajouter une variante sans la nommer ne
  compile pas, là où un nom tiré de `Debug` coûterait le formatage de la charge —
  un `WriteFile` porte un fichier entier. Une commande qui a produit un `Done` ou
  un `Failed` est une **écriture** et passe à `info` avec sa durée.
- `done` et `fail` disent **ce qui a été fait** — la première ligne seulement, un
  `git push` écrivant un paragraphe. `fail` est le seul `warn` de la couche, et
  c'est ce qui lui donne sa valeur : l'opération est connue, donc on sait que
  quelqu'un l'attendait.

Côté `wt`, `capturing` **journalise toutes les lignes** et n'en rend qu'une : un
`wt new` raconte toute sa séquence, et c'est le seul compte rendu de ce que les
hooks ont fait.

- **Un anneau de deux mille lignes**, pas une liste qui croît.
- **Le filtre d'`env_logger` s'applique aussi à la copie** : `set_max_level` ne
  borne que par niveau, c'est `enabled` qui applique les directives par cible de
  `CLAUDHUB_LOG`.
- **Un compteur, pas la longueur de l'anneau** (`logging::written`) : la longueur
  cesse de bouger dès qu'il est plein, et la page se rend à chaque frame.
- **Rien ne réveille la vue quand une ligne est écrite** : un worker journalise,
  il ne touche pas à gpui. La page suit les frames que le reste provoque, et le
  balayage de fond en amène une toutes les deux secondes.
- **Deux cents lignes peintes au plus**, et c'est dit : la liste vit dans une
  page qui défile d'un bloc, elle n'est pas virtualisée. Le filtre s'applique
  **avant** la coupe. Le bouton « Copier » prend tout ce que le filtre a gardé.

**Le journal est en anglais, et un test le garde**
(`logging::tests::nothing_speaks_french_in_the_journal`, qui relit les sources et
n'inspecte que les littéraux d'un `log::`). La règle passait inaperçue tant que
ces lignes n'atteignaient qu'une console.

Le niveau affiché vit dans `ClaudhubApp` et non dans les réglages : c'est une
posture de lecture. Il s'ouvre sur `Trace`.

### Les polices embarquées

`install_fonts` les enregistre au démarrage, et c'est ce qui fait qu'un
`DEFAULT_UI_FONT` est une promesse et non un pari : **Iosevka Aile** pour
l'interface, **Iosevka Nerd Font Mono** pour l'éditeur, les diffs et le terminal
— la coupe `Mono`, l'Iosevka nue ayant un `@` d'une cellule et demie qui
décalerait la grille. Inter et JetBrains Mono restent embarquées derrière.

- **Les fontes sont sous-ensemblées** par `tools/subset_fonts.py`, seule façon de
  les refaire : les faces complètes pèsent neuf à onze mégaoctets pièce.
- **Un glyphe absent du sous-ensemble n'est pas un carré vide** sur une machine
  pourvue : gpui retombe sur les polices du système. L'arbitrage ne vaut que
  parce que le repli existe.
- **Le patcheur de Nerd Fonts ne met le nom complet que dans le champ 16** ; le
  champ 1 dit « Iosevka NFM ». Le script renomme les deux.
- **Les fontes ne passent pas par `rust-embed`** mais par `include_bytes!` : le
  motif d'`Assets` ne couvre que `icons/**/*.svg`. C'est aussi pourquoi
  `NotoColorEmoji.ttf`, présent mais nommé nulle part, ne coûte rien.

### Ce qui tourne, et ce qu'une opération a répondu

`ClaudhubApp::running` est l'ensemble des écritures en vol, et `start` remplace
`git.send` partout où le geste en est une.

- **La clé est exactement la paire que le worker renverra** : `write_then_refresh`
  réémet le worktree et l'action qu'on lui a donnés. C'est ce qui empêche
  l'unique panne d'un indicateur d'attente — un bouton qui tourne pour toujours
  en dit moins qu'un bouton qui n'a jamais tourné.
- **Deux moments vident tout** (`clear_running`) : la mort du serveur distant et
  l'arrivée d'un neuf.
- **`wt up` et `wt down` ne nomment pas de worktree** dans leur réponse, d'où
  `wt_pending`, un seul à la fois.
- **Ce qui part d'un menu n'a pas de bouton à faire tourner** : un menu se ferme
  au clic, donc l'intégration, le rebase, un `wt rm` s'annoncent dans la **barre
  d'état**, nommés et non comptés. La liste est triée sur la **clé** i18n — un
  `HashSet` s'itère dans un ordre différent à chaque frame, et l'ordre des mots
  ne doit pas dépendre de la langue.
- **La décision est hors de la vue** (`ui::inflight::InFlight`), testée.
- **`Action::running_key` a un joker assumé** : un `Stage` qui finit en dix
  millisecondes afficherait un message que personne n'a le temps de lire. Un test
  vérifie que chaque action nommée a ses deux messages dans les deux catalogues.

**Deux endroits pour une réponse, et le partage est tout le sujet de
`ui::notify`.** Un `git pull` répond `Updating …`, `Fast-forward`, puis une ligne
par fichier : versé dans une barre d'une ligne, ce texte ne se tronque pas, il se
replie et se peint par-dessus la fenêtre.

- **La barre garde une ligne**, toujours : `headline` prend la **première** ligne
  non vide — git mène par ce qu'il a fait — et à défaut le libellé de succès.
- **Une bulle porte ce qui se lit** : la liste de fichiers d'un pull, les
  références qu'un push a bougées, la raison d'un refus de fusion.
- **La décision est hors de la vue et testée.** Trois règles : tout échec a sa
  bulle ; une sortie de plus d'une ligne a la sienne ; rien d'autre n'en a.
- **Un échec ne s'efface pas tout seul** (`autohide` faux).
- **Un seul libellé pour tous les échecs** (`notify-failed`) : ce qui a échoué
  est dans le corps, git nommant l'opération lui-même.
- **Toute opération distante a sa bulle**, même sur une ligne
  (`notify::always_worth_reading`) : elle a coûté des secondes, on a fait autre
  chose pendant ce temps, et sa réponse se lit **après**.
- **Ce que git a écrit sur stderr en fait partie** (`git::git_reporting`, pour
  les commandes réseau seules) : `push` et `fetch` écrivent tout leur compte
  rendu là et leur stdout est vide. stderr passe **en premier**.

### Le magasin d'état

Les réglages disent comment Claudhub s'affiche ; le magasin (`store::StateStore`,
`<config>/state.json`) dit où l'on en est — la base de comparaison d'un worktree,
ses replis, le prochain numéro de note, les worktrees épinglés, où l'on en était
en fermant. Deux fichiers
et non un : le premier se modifie à la main, le second compterait des centaines
de lignes par dépôt.

Pas de SQLite : le magasin fait moins d'un kilo-octet, et une base achèterait
l'écriture partielle, la requête et la concurrence, dont rien ici n'a besoin.

Il est écrit **depuis le thread d'interface**, ce qui déroge à « `src/ui/` ne fait
jamais d'entrée-sortie » : c'est le précédent de `settings.rs` et la même raison.
La règle vise les commandes git, pas la préférence qu'on range.

- **La valeur relue l'emporte sur celle que git devine** : `ensure_review` remet
  ce que le magasin avait, et les deux endroits qui proposent un `default_base`
  testent `is_none()`.
- **Une entrée retient son dépôt** (`WorktreeState::repo`). La purge se fait quand
  git vient d'énumérer les worktrees ; sans ce champ, une entrée absente ne se
  distinguerait pas d'une entrée d'un dépôt pas encore ouvert.
- **Un seul point d'écriture** (`persist_review`). Les replis sont triés avant
  d'être écrits : un `HashSet` sérialisé dans un ordre différent ferait un
  fichier qui change sans que rien n'ait changé.

### Chercher dans un panneau

`Ctrl+F` cherche dans le panneau où l'on vient de **cliquer** — le dock pose le
focus sur l'onglet actif de *chaque* zone, et rien là-dedans ne dit laquelle on
regarde. `panels::pane_root` note donc le panneau touché, en phase de
**capture** : une ligne de diff comme une case à cocher consomment leur clic.

**Un seul geste, deux comportements.** Là où la liste est libre de son ordre, la
recherche **filtre**. Là où l'ordre porte du sens, elle **saute** : le **diff**
surligne par-dessus la coloration (`highlight::overlay`) et `Ctrl+G` va d'une
occurrence à l'autre — seul panneau qui affiche un compte, une occurrence pouvant
être à quatre mille lignes ; l'**historique** éteint ce qui ne correspond pas
plutôt que de le retirer, ses traits reliant une ligne à ses voisines.

- **La casse est déduite de la requête** : minuscules = insensible.
- **Les décalages sont en octets**, et `find_all` compare caractère à caractère
  plutôt que de chercher dans un `to_lowercase()`, qui change la longueur en
  octets de certains caractères.
- **Une recherche ignore les replis**, sans quoi elle paraîtrait n'avoir rien
  trouvé.

Les occurrences du diff sont calculées à chaque changement de requête et à chaque
arrivée de diff, **jamais au rendu** ; `DiffSearch::valid` les invalide.
`highlight::overlay` pose un fond sans toucher aux couleurs de texte, en découpant
aux frontières des deux découpages, et rend des plages **triées et disjointes**.

Le terminal n'a pas de recherche : `Ctrl+F` y appartient au programme qui tourne.

### Chercher dans tout le projet

`Ctrl+Maj+F`, l'écran « Recherche ». Autre geste que le précédent : `Ctrl+F`
filtre une liste que Claudhub tient déjà, celle-ci demande à git ce que le
worktree **contient**. Le premier répond en une frame, le second en une seconde.

**`git grep`, jamais un parcours à nous** (`git::search`) : il sait déjà ce qui
appartient au projet — l'index sans un `readdir`, `--untracked` pour ce qui est
neuf, `.gitignore` qui laisse `vendor/` dehors. Il est threadé et écarte les
binaires (`-I`).

**Et non ripgrep, la question ayant été posée et mesurée.** Sur un Laravel de
8 200 fichiers, `git grep` répond en 36 ms (59 en regex), `rg` en 15 : deux à
quatre fois plus rapide, et les deux si loin sous le seuil de perception que
l'écart ne s'affiche nulle part. Ce que l'échange coûterait est réel : `rg` n'est
pas promis par la machine cible, et sous WSL il faudrait l'avoir **dans la
distro**. Par défaut les deux ne cherchent pas la même chose — `rg` a trouvé 97
fichiers de plus (un **sous-module**, où `git grep` ne descend pas, ce qui le
rend cohérent avec l'arbre du projet) et 15 de moins (des fichiers **cachés**).
Le jour où le coût se verrait, la sortie serait le crate `grep`, pas le programme.

- **`-E` et non `-P`** : PCRE est une option de compilation de git, absente de
  plusieurs paquets — une recherche qui échoue chez l'utilisateur et pas chez
  nous est la pire espèce de fonction.
- **Trois plafonds, et chacun est dit** : une ligne coupée à 400 octets sur une
  **frontière de caractère** (un fichier minifié fait une « ligne » de deux
  mégaoctets), cent occurrences par fichier, deux mille au total. La barre **dit**
  que la liste est écourtée.
- **La recherche est amortie, et ce qu'on amortit est ce qu'on lit** : à quarante
  millisecondes la recherche, partir à chaque lettre serait payable ; ce qui ne
  le serait pas, c'est la liste qui se reconstruit, resélectionne et relit un
  aperçu à chaque fois. Trois cents millisecondes — une pause entre deux mots.
  L'amortissement est un **compteur de frappes** relu à l'échéance et non le
  drapeau qu'emploient les autres différés : leur drapeau tire à cadence fixe, ce
  qui est juste pour ce qui coûte une milliseconde et faux pour une seconde.
- **Sous deux caractères, rien ne part tout seul.** `Enter` cherche quand même —
  un geste a le droit de coûter cher.
- **Une sélection faite dans une surface de code part avec le raccourci**
  (`open_search`, `search::seed`), comme dans PhpStorm : on surligne un appel, on
  demande où il est fait ailleurs, et retaper ce qui est déjà sous le curseur est
  le geste que le raccourci existe pour épargner. La recherche **part** — un
  champ portant une question au-dessus d'une liste qui en répond une autre est
  l'état que l'amortissement passe son temps à éviter — mais le caret **reste
  dans le champ**, son texte sélectionné : le geste demandait les résultats, pas
  que la liste prenne le clavier. Quatre points :
  - **La sélection se lit avant de changer d'écran** : `enter_workspace` déplace
    le focus, et c'est le focus qui dit dans quelle surface la main était.
  - **La surface est celle qui a le focus**, pas « le fichier qu'on édite » : le
    dock en montre deux dès qu'on ouvre un split, et la console SQL en est une
    aussi. À défaut, l'écran où l'on est.
  - **Le mode de vim décide de ce que « sélectionné » veut dire**, et c'est le
    piège : le mode **normal** laisse en permanence dans l'éditeur une plage
    d'**un caractère** — le curseur bloc est écrit comme une sélection
    (`vim::block`) —, si bien que lire la plage seule amorcerait chaque
    recherche avec la lettre sous le caret. Le mode normal est donc refusé,
    le mode **bloc** est demandé à vim (un rectangle est une plage par ligne,
    que l'éditeur ne sait pas tenir) et tout le reste — les modes visuels,
    l'insertion, et toute sélection faite à la souris, vim allumé ou non — est
    la plage de l'éditeur. Le mode ne veut rien dire quand `vim_mode` est
    éteint : rien ne conduit la machine et elle dit « normal » par-dessus une
    sélection de souris.
  - **Trois refus** (`search::seed`, pur et testé) : rien de sélectionné, plus
    d'une ligne (`git grep` cherche *dans* une ligne, donc un terme à deux
    lignes ne peut que ne rien trouver), et plus long que `MAX_SEED` — refusé et
    non tronqué, une requête coupée étant une requête que personne n'a demandée.
- **Seule une recherche demandée prend le focus** : les réponses qui arrivent
  pendant la frappe doivent laisser le caret où il est.
- **La liste prend le focus quand les résultats arrivent** — `InputState` lie les
  flèches lui-même, plus profond dans la pile de contextes. Le retour au champ
  est `Ctrl+Maj+F`. D'où `ClaudhubSearch`, que `NAVIGATION_PREDICATE` et
  `VIM_PREDICATE` excluent.
- **L'aperçu est un aller-retour de plus** (`Cmd::ReadPreview`), et une commande
  à lui plutôt que `ReadFile`, qui *ouvre l'éditeur*. Le fichier n'est **pas**
  relu d'une occurrence à l'autre du même fichier, et la ligne est **relue à
  l'arrivée** sur la sélection du moment.
- **Ouvrir passe par `open_at`/`jump_to`**, donc la piste retient d'où l'on
  venait.
- **Les résultats d'un worktree qu'on a quitté sont gardés et dits tels** : une
  recherche vaut des minutes, mais montrer sans le dire les occurrences d'un
  autre projet est la seule chose que ce panneau ne doit pas faire. Ouvrir depuis
  ces résultats est refusé, et le refus se dit.

**La coloration de l'aperçu est celle du diff, un étage plus haut** :
`highlight::DocumentHighlights` calcule les styles **une fois**, à l'arrivée du
contenu, et les découpe par ligne. Une vue Blade passe par
`blade::document_styles`.

**Et la liste des occurrences est colorée elle aussi**
(`highlight::HitHighlights`), comme PhpStorm : ce qu'on lit dans cette colonne
est du code, et une colonne de gris ne dit pas si l'occurrence est dans un
commentaire, une chaîne ou un appel. Quatre points :

- **Une ligne est parsée seule**, et non jointe aux autres du même fichier :
  ce qui l'entoure n'est pas là, et l'occurrence suivante peut être quatre cents
  lignes plus bas. Un guillemet non refermé sur l'une colorerait sinon en chaîne
  toutes celles d'en dessous. Une grammaire tree-sitter se relève d'un fragment
  mal formé ; c'est ce qui rend l'approximation payable, et le coût est borné par
  `search::MAX_HITS`.
- **Une grammaire par langage, gardée d'un fichier à l'autre** :
  `SyntaxHighlighter::new` compile les requêtes — des dizaines de millisecondes —
  et une recherche qui touche cent fichiers la paierait cent fois. **Blade est
  de la règle**, et c'est là qu'elle a été enfreinte : `blade::document_styles`
  bâtissait un `BladeHighlighter` — donc une grammaire PHP — par appel, soit
  **une par ligne** et non une par fichier. Deux mille occurrences dans des vues
  coûtaient une minute de fenêtre gelée, la seule panne de ce genre : tout est
  coloré juste, rien n'échoue, et cela ne se voit qu'à l'horloge. D'où
  `BladeHighlighter::document_styles`, qui prend un highlighter qu'on garde, et
  un test qui borne le temps d'une liste pleine.
- **Le prologue vaut ici aussi** : sans `<?php`, une ligne de PHP est du texte
  HTML et rien ne sort. Ses propres lignes ne comptent pas dans les décalages.
- **Les décalages décrivent le texte *rogné*** — la ligne est affichée sans son
  indentation de tête, faute de quoi une occurrence à six niveaux ne montrerait
  qu'une colonne de rien. Décalés de l'indentation, les styles peindraient le
  mauvais mot. L'occurrence est surlignée **par-dessus** (`highlight::overlay`),
  comme dans l'aperçu.

Ce que la v1 ne fait pas : elle ne cherche que dans le worktree affiché, et il n'y
a pas de remplacement — écrire dans des fichiers que personne n'a ouverts est ce
que la règle de l'empreinte interdit ailleurs.

### Les barres de défilement, la molette, les hauteurs

Tout panneau qui défile porte une barre (`ui::scroll`) : une liste virtualisée ne
dit rien d'elle-même, et rien ne distingue « il reste trois lignes » de « il en
reste trois mille ». Elle se pose **par-dessus** le contenu, en absolu, d'où le
`relative` du conteneur.

Quatre points, tous constatés à l'écran — une barre qui ne se peint pas ne
provoque aucune erreur :

- **`min_h_0` et `min_w_0` sur le conteneur** : élément flex, dont la taille
  minimale vaut celle du contenu. Sans eux, la barre se peint trois cents pixels
  à droite du panneau.
- **`overflow_hidden`**, pour la même raison en aval.
- **`scrollbar()` de gpui-component plutôt qu'un enfant `Scrollbar` nu** :
  l'extension enveloppe la barre d'une couche absolue calée sur les quatre bords
  ; posée nue, elle ne reçoit pas de bornes utilisables.
- **L'identifiant va au conteneur** : la couche s'appelle toujours
  `scrollbar_layer`, et sans parent identifié les panneaux partageraient l'état
  d'une seule barre.

**`ScrollbarShow::Always`** : ni `Scrolling`, qui l'efface deux secondes après le
dernier cran, ni `Hover`, qui ne la montre qu'une fois le pointeur **sur la
barre**, laquelle est invisible.

Les panneaux non virtualisés prennent leur poignée de `ClaudhubApp::scroll_of`,
une table plutôt qu'un champ par panneau — créée au rendu, elle remettrait la
liste en haut à chaque frame.

**Le lissage de la molette** (`ui::motion`) rejoue en 160 ms le saut de trois
hauteurs de ligne que gpui applique d'un coup. Le principe tient en une
inversion : **on n'empêche pas gpui de sauter** — il n'y a pas de phase de
capture pour la molette. On le laisse faire, on lit où il a atterri, on **remet**
le décalage d'avant, et on y va progressivement. D'où la place de l'écouteur : sur
un ancêtre **non défilant**, donc après le gestionnaire interne dans la phase de
remontée — exactement le conteneur de la barre. `ClaudhubApp::scrolled` pose les
deux d'un même geste, avec **une seule clé**.

- **Le saut se lit, il ne se recalcule pas.** gpui ne rogne rien au moment de la
  molette et capture sa hauteur de ligne **sous le style de l'élément qui
  défile**, quand notre écouteur lit la hauteur ambiante : sur le diff, trois
  lignes d'écart font deux ou trois pixels. **Au bord, ils sont tout le
  mouvement** — la destination y est rognée, la provenance non, et la vue
  reculait de trois pixels à chaque cran. D'où `Axis::jump`.
- **Un pavé tactile n'est pas une molette** : `ScrollDelta::Pixels` passe tel
  quel et annule la transition.
- **Un saut demandé par le code gagne** : `advance` compare ce qu'il trouve à ce
  qu'il avait écrit lui-même.
- **La liste change de taille pendant le mouvement** : la destination est reprise
  à chaque frame.
- **Les deux axes sont répartis comme gpui les répartit.**

`Axis` ne contient aucun type de gpui, et c'est ce qui le rend testable.

Trois surfaces n'y passent pas : le **diff** garde son écouteur (le zoom et le
lissage veulent tous deux rendre le saut, et deux écouteurs le rendraient deux
fois) ; l'**éditeur intégré** aussi, et rien n'y ressemble aux autres —
`InputState::on_scroll_wheel` **consomme** l'événement dès que l'offset a bougé,
d'où un écouteur de fenêtre en phase de **capture** et un `canvas` qui enregistre
les bornes ; son offset relu est **en retard d'une frame** (`set_scroll_offset`
s'applique à la mise en page suivante), d'où `ScrollMotion::owned` ; et sa course
se **lit** (`scroll_size` moins `input_bounds`, dixième commit du fork) et ne se
déduit pas du nombre de lignes — le domaine que `visible_row_range` annonce
compte deux lignes de trop, et un plafond trop bas est ce contre quoi chaque
frame rogne : la molette s'arrêtait avant la fin du fichier, et un cran donné
après que la barre y était arrivée remontait la vue ; le **terminal**
non plus, son défilement étant le `display_offset` de la grille alacritty, compté
en lignes entières.

**Et la table de résultats est la quatrième, pour une raison d'un cran
différente** : elle se couvre elle-même d'un `ScrollableMask`, qui prend la
molette en phase de **capture** et la consomme. L'écouteur d'un ancêtre — la
phase de remontée, sur quoi toute l'inversion repose — n'était donc jamais
appelé, et la grille sautait ses trois lignes d'un coup pendant que tout le
reste de la fenêtre glissait. `smoothed` enregistre donc son propre écouteur de
capture **avant** que la table ne peigne — la capture suit l'ordre de peinture,
donc le premier enfant enregistré passe le premier —, consomme l'événement et
pousse la motion lui-même (`ScrollMotion::push`, le chemin de l'éditeur). Ce
qu'il laisse au masque est délibéré : le **pavé tactile**, déjà continu, et
tout ce qui est **horizontal** — la grille défile aussi en largeur, et le
verrou d'axe du masque y vaut mieux que notre lissage. **Au bord vertical
l'événement remonte**, comme le masque le fait lui-même.

**Aucune hauteur de ligne ne s'écrit en dur** : `theme::row_height` /
`tall_row_height` / `bar_height` / `toolbar_height` les déduisent de la taille du
texte. Une hauteur figée déborde dès qu'on grossit la police — et c'est pire dans
les listes virtualisées, qui réservent exactement ce qu'on leur annonce : la
ligne suivante est recouverte, pas repoussée.

**Le zoom** règle deux zones séparément — les diffs et le terminal — parce que
grossir la sortie d'un agent ne doit pas déplacer le code relu à côté.
L'éditeur prend la taille du **diff** : c'est du code des deux côtés, jamais
affiché en même temps.

### Copier depuis un diff

Ce qui sort du presse-papiers est du **code** : ni `+`/`-`, ni numéros de ligne,
ni en-tête `@@`. `Ctrl+Maj+C` donne l'autre forme, un extrait de patch — le vrai
signe moins de l'affichage n'y a pas sa place.

La sélection se fait au clic, s'étend au glissement ou au Maj+clic, `Ctrl+A`
prend tout, et `Rendered::copy_text` est libre et testée. Sans sélection,
`Ctrl+C` prend le fichier entier.

Un clic sur une ligne **prend le focus** : sans cela le `Ctrl+C` qui suit part au
programme du terminal. Pour la même raison, `COPY_PREDICATE` exclut `Input` et
`ClaudhubTerminal`.

### Annoter une relecture, et le coffre

Une note porte sur une plage de lignes, et on la renvoie à l'agent qui les a
écrites. Le modèle et tout ce qui se teste sont dans `notes.rs` ; `notes_view.rs`
n'est que de la plomberie.

**Une note ne peut pas s'accrocher à la sélection** : `diff_selection` est un
couple d'indices dans la liste *affichée*, invalidé par la bascule de mode, par
un changement de contexte et par tout rechargement du diff — c'est-à-dire par
chaque écriture de fichier. On retient donc des **numéros de ligne**, un **côté**
(`Old`/`New` : commenter du code supprimé a un sens) et **l'extrait** lui-même.

`notes::relocate` replace la note à chaque arrivée de diff : aux numéros retenus
si le texte qui s'y trouve est celui qu'on a cité ; sinon par recherche de
l'extrait ; sinon la note est dite **décalée** et **reste dans la liste**. Une
note perdue en silence est pire que pas de note.

- **L'ancrage est arrêté au moment du geste**, pas à la validation du dialogue.
- **Les marqueurs de gouttière sont calculés en amont** (`notes::marks`, dans un
  `Rc`), jamais dans la fermeture d'`uniform_list`. Deux vecteurs, un par
  disposition.
- **Le dialogue n'est pas un popover** : la ligne annotée appartient à une liste
  virtualisée, et le moindre défilement emporterait l'ancre.

**L'envoi passe par le terminal, jamais par une API.** Claudhub compose un prompt
(`notes::prompt`, libre et testé) et le livre à l'agent, qui a le dépôt entre les
mains. Le texte part en **collage encadré** (`Terminal::paste`), sans quoi un
message multiligne arrive comme autant de commandes validées ; le **retour
chariot part dans un second envoi**, après un court silence, un TUI pouvant
avaler un `\r` arrivé dans la foulée ; et s'il n'y a pas d'onglet d'agent, on en
ouvre un et l'envoi est **différé** (`AGENT_WARMUP`), rien dans un pty ne disant
« je suis prêt ». Les notes envoyées passent à `sent`, pas à `done` : c'est la
relecture de la réponse qui les clôt.

**Le dossier est la source de vérité.** Une note vit dans un **dossier de
fichiers Markdown**, un fichier par note, sous `Settings::notes_dir` (vide :
`<config>/notes`). Pointée sur un coffre Obsidian, la relecture s'y lit et s'y
relie. Ce qu'on corrige dans Obsidian revient au chargement suivant, et c'est ce
qui décide du reste : le format doit être **relu** et pas seulement écrit, d'où
le frontmatter plat de `ui::vault` et l'extrait cité dans un bloc de code dont la
clôture est **calculée** — un diff de Markdown contient des accents graves.

- **`ui::vault` ne touche pas au disque** : il rend du texte et le relit, donc il
  se teste. Les fichiers passent par un worker — un coffre vit souvent sur un
  disque synchronisé, parfois sur un montage drvfs.
- **On n'efface que ce qui porte notre marque**, et sur sa **valeur**
  (`files::is_ours` : `note` et `review`) : `claudhub: todo` porte la marque sans
  nous appartenir, et une note supprimée emportait sinon la liste de tâches.
  `sync_notes` aligne le dossier sur la liste **entière**.
- **On ne réécrit pas ce qui n'a pas changé** : un coffre se synchronise.
- **Le nom d'un fichier ne porte que l'identifiant et le fichier relu**, jamais
  les numéros de ligne, qui glissent — les liens du coffre pointeraient dans le
  vide.
- **Rien ne s'écrit avant d'avoir lu** (`notes_loaded`), et rien ne s'écrit pour
  un worktree qu'on n'a pas annoté (`notes_on_disk`).
- **Le coffre est surveillé comme le worktree** (`Cmd::WatchDir`). Un dossier qui
  n'existe pas n'entre pas dans `watched`, et c'est l'écriture qui dit qu'il est
  là — `Evt::VaultWritten` ne porte pas de contenu mais le fait que le dossier
  existe désormais.
- **La reprise de l'ancien magasin passe par le même chemin que l'installation
  neuve**, comme `migrate_agents`.

Le magasin garde `next_note`, qui ne se déduit pas des fichiers : une note
supprimée libérerait son numéro, et une note déjà envoyée y serait désignée par
un numéro valant pour une autre.

**Le panneau « Notes » est le tiers du bas de la colonne de revue**, pas un
onglet : il ne dit pas ce qu'il y a à lire, il dit où l'on en est, et cela se lit
**pendant** qu'on choisit un fichier. Des **sections repliables** — tâches, note
libre, remarques, fichiers relus — et non trois panneaux ni des sous-onglets. Le
repli est porté par le **titre** (un clic sur « envoyer tout » remonterait
replier ce qu'on vient d'agir), il ne se persiste pas, et une section vide se
réduit à une ligne grise, trois sections partageant ce défilement.

La barre porte le **chemin du coffre** et de quoi l'ouvrir (`file://`). **Le
prompt se voit avant de partir** : ce qui entre dans un terminal ne se rattrape
pas, et c'est aussi où l'on ajoute d'une phrase ce que les notes ne disent pas.

**`TODO.md` est le seul fichier du coffre que Claudhub n'écrit pas en entier.**

- **Cocher retourne un caractère** à une ligne connue (`vault::toggle_task`) : le
  reste est recopié tel quel, donc la prose que l'agent met entre les tâches
  survit.
- **L'écriture est conditionnelle**, comme celle d'un fichier ouvert.
- **Claudhub ne le crée pas tout seul** : il naît de la première tâche ajoutée.
  Le fichier posé explique son propre format — il finit dans un coffre.
- **Une tâche s'ajoute après la dernière**, pas à la fin du fichier
  (`vault::append_task`) : un agent écrit sous sa liste.
- **Tout s'y édite en place** : une ligne de saisie en bas ajoute, un clic sur un
  libellé le remplace par sa saisie, un libellé vidé **supprime**. Le `+` de
  l'en-tête ne fait que donner le focus à la ligne du bas.
- **Deux zones de saisie et non une** (`task_input`, `task_edit_input`).
- **Perdre le focus valide** : `InputState` n'a pas d'événement d'échappement.
- **Les trois gestes passent par `rewrite_todo`**, pur, qui rend `None` quand la
  ligne visée n'est plus ce qu'elle était.

**`NOTES.md`, la note libre**, s'édite sur place sans bouton « enregistrer ».
Elle n'a **pas de frontmatter**, ce qui la met hors de portée de la purge ;
**vide, elle n'existe pas** (`files::write_vault_file` efface, sous la même
empreinte) ; l'écriture est **différée d'une seconde** ; et il y a **une seule
zone de saisie pour tous les worktrees**, **jamais rechargée pendant qu'on y
écrit** — le coffre est relu à chaque écriture, et remettre le texte du disque
sous les doigts déplacerait le curseur.

**Marquer un fichier relu** : une coche par ligne, et **un clic sur un dossier
vaut pour tout son sous-arbre**, replié compris. Une coche et non une case, celle
de la liste voulant déjà dire « indexer » ; elle vit à droite, et une ligne relue
prend un **fond vert** avec son nom éteint — la sélection passe devant le vert.
**Le volume retenu est ce qui périme la coche** : `vault::Reviewed` garde `+n −m`
au moment du clic, et `Evt::DiffFiles` purge ce qui ne vaut plus. Le suivi vit
dans le coffre en cases à cocher Markdown (`vault::INDEX`), donc **décocher là
rend le fichier à relire ici** ; le titre d'une section est la clé du domaine et
non son libellé traduit.

### Ce que l'agent sait de Claudhub

Trois variables posées par `TerminalGroup::open` sur tous les onglets :
`CLAUDHUB_WORKTREE`, `CLAUDHUB_NOTES_DIR`, `CLAUDHUB_TODO`. Un shell les voit
aussi, et un agent lancé à côté n'a qu'à les recopier.

**Pas de serveur MCP, et c'est une décision.** Les notes sont déjà des fichiers
Markdown : un serveur qui les exposerait serait un habillage typé de `cat` et de
`write`. MCP ne gagnerait son prix que sur ce qu'un fichier ne dit pas — l'état
vivant de la fenêtre — et rien ne le demande.

Le prompt (`notes-prompt-outro`) dit **où répondre et à quoi ne pas toucher** :
dans le corps, jamais dans le frontmatter. Les variables n'y sont pas
développées, ce qui garde `notes::prompt` pur.

**Les profils d'agent** : `Settings::terminal.agents` est une liste — nom,
commande, arguments, environnement — et `default_agent` désigne celui que le
bouton lance. **L'environnement est ce qui porte le modèle** (`ANTHROPIC_MODEL`) :
« configurer plusieurs modèles » n'appelle aucune dépendance HTTP.

- **`command` et `args` sont séparés**, et une ligne se découpe par
  `settings::split_command`, qui honore les guillemets — `split_whitespace`
  cassait sur tout chemin contenant une espace. L'aller-retour avec
  `join_command` est testé.
- **`migrate_agents` ne fait quelque chose que si `agents` est vide**, et
  `agent_command` est vidé après coup.
- **`Cmd::ScanAgents` prend la liste entière des programmes** : un agent lancé
  depuis un terminal à côté compte autant.
- **La clé d'état d'une ligne de la table porte le nombre de profils**
  (`claudhub-agent-{n}-{i}`) : sans le compte, supprimer le premier laisserait
  les champs de la ligne 0 remplis avec l'ancien. Renommer ne change pas le
  compte, donc les champs gardent leur curseur.

### La répartition du chrome

La barre d'outils ne porte que des **actions**. L'écran regardé, ce qui tourne et
ce qu'une opération vient de répondre décrivent *où l'on est* : ils vivent dans
la barre d'état.

**Le worktree et la branche sont remontés** : ce sont les deux sélecteurs qui
pilotent tout le reste, et un sélecteur est une action. **Les panneaux « Dépôts »
et « Branches » n'existent plus** — supprimés, pas masqués. La revue et Sentry
n'ont donc plus de colonne de gauche.

- **Le nom du dépôt précède celui du worktree**, en gris : deux worktrees appelés
  `main` dans deux dépôts est le cas courant.
- **Le menu du worktree reste un bouton** (`…`) : un clic droit a besoin d'une
  ligne où atterrir, et il n'y en a plus dans la barre.

### Les deux sélecteurs

Ce sont les deux gestes les plus fréquents de la fenêtre, et ils ont longtemps
été des `PopupMenu`. Cette forme avait un plafond, et il tenait en deux faits :

- **un menu n'a pas de champ de filtre** — un dépôt à deux cents branches se
  fait défiler, pas chercher ;
- **un sous-menu dans un menu qui défile est rogné par le défilement dont il a
  besoin.** gpui capture le masque de contenu d'un `deferred` au prépaint, si
  bien que le popup ouvert depuis une ligne est coupé au bord de la fenêtre de
  défilement. Le commentaire est dans gpui-component même. C'est ce qui gardait
  le sélecteur de branches à deux gestes là où celui de PhpStorm en a douze.

Ce sont donc deux popovers à nous (`ui::branch_picker`, `ui::worktree_picker`),
**en deux temps** — la forme du popup de branches de PhpStorm, et pour la même
raison : le second temps **remplace** le premier dans la même surface, si bien
que rien n'est jamais imbriqué et que rien ne peut être rogné.

- **Le premier temps est la liste filtrée.** Un clic sur une ligne fait le geste
  courant — basculer sur la branche, choisir le worktree —, qui doit rester à un
  clic. Le `…` du bout de ligne ouvre le second temps.
- **Le second temps est la liste des actions**, chacune avec son nom écrit.
- **Le `…` est peint sur toutes les lignes**, pas sur la survolée : un contrôle
  qui apparaît sous le pointeur déplace ce qui est à côté, et ce qui est à côté
  ici est le compte qu'on était en train de lire.
- **Une action indisponible reste affichée, grisée.** Ce qu'on veut savoir en
  arrivant là est *pourquoi* un geste ne se fait pas — la branche est déployée
  ailleurs, elle n'a pas d'amont —, et une ligne absente ne dit rien.

**Pourquoi le contenu du popover est une entité et non un élément.** La
fermeture de `Popover::content` tourne **dans** `ClaudhubApp::render` : y lire
l'entité racine est la panique documentée plus bas. Le contenu est donc
l'entité du sélecteur, et le `render` d'une vue enfant a lieu **après** que la
fermeture du parent a rendu la main — la règle dont vivent les panneaux du dock.
C'est ce qui permet aux sélecteurs de relire l'application à chaque image, donc
de ne jamais montrer une liste périmée.

- **Les flèches sont prises en phase de capture** sur un ancêtre du champ : un
  `InputState` d'une ligne lie Haut et Bas aux extrémités de son texte, et
  laissées remonter elles n'atteindraient jamais la liste. Échap n'est pas
  touché — il appartient au popover, qui est ce qu'on s'attend à voir fermer.
  Le curseur **enjambe les en-têtes** de groupe et **boucle**.
- **Ouvrir remet le premier temps et vide le filtre** (`on_open_change`, un clic
  et non un rendu) : un filtre resté de la fois d'avant se lit comme un dépôt
  qui aurait perdu ses branches.
- **Les couleurs sont lues une fois** (`Look`) et passées à la fermeture de la
  liste virtualisée, qui tourne pour chaque ligne visible à chaque image.
- **Un en-tête n'a pas la hauteur d'une entrée**, d'où `v_virtual_list` et non
  `uniform_list` — la bascule du repli du diff et de la vue de fusion, pour la
  même raison. Un en-tête est une règle qui porte un nom, pas une ligne de
  liste ; une entrée fait deux lignes de texte et presque rien autour. Une liste
  dont chaque entrée est deux fois trop haute en montre quatre là où elle en
  montrait sept, et ce qu'on vient y faire est comparer.
- **Les groupes se replient, et c'est la ligne entière qui replie** : un chevron
  fait huit pixels, et ce qu'on vise est le groupe. Fermer les distantes est ce
  qu'on fait d'un dépôt dont `origin` porte cent branches que personne n'a
  déployées. **L'en-tête reste** quand le groupe est fermé — cet en-tête *est* le
  repli, et le retirer ne laisserait pas de retour. Le repli ne survit pas à la
  fenêtre : c'est une posture de lecture, pas une préférence. Et **un filtre
  ignore les replis**, la règle de toute liste repliable ici : une requête qui a
  trouvé quelque chose et n'affiche rien se lit comme une requête qui n'a rien
  trouvé.
- **Les entrées sont en retrait sous leur en-tête**, de la largeur du chevron :
  une colonne plate sous un titre se lit comme une liste qui a un titre au-dessus
  d'elle, pas comme les branches de ce groupe ou les checkouts de ce dépôt.
- **Les dépôts introuvables sont en bas** de la liste des worktrees, avec ce que
  git a répondu : c'est le seul endroit d'où on peut les retirer. Ils
  disparaissent dès qu'on filtre — ils n'ont ni branche ni nom sur quoi chercher,
  et les cacher derrière une requête les rendrait irretirables.
- **Une branche déjà déployée ailleurs est grisée et dit chez qui** — git refuse
  deux extractions.

**Ce qu'on peut demander d'une branche**, dans l'ordre du second temps : basculer
dessus, une nouvelle branche depuis celle-ci, un worktree depuis celle-ci,
comparer avec la branche courante, fusionner dedans, rebaser la courante dessus,
mettre à jour depuis l'amont, publier, renommer, supprimer, supprimer sur
`origin`. Quatre d'entre elles ont demandé des commandes qui n'existaient pas —
`RenameBranch`, `PushBranch`, `UpdateBranch`, `DeleteRemoteBranch` —, et aucune
n'a demandé une `Action` de plus : renommer est une écriture de branche
(`Action::Branch`), publier est un `Push`, mettre à jour est un `Pull`, et ces
messages disent déjà la bonne chose.

- **Ce qui parle à un distant part dans la file réseau**, le reste dans les
  lectures : un renommage déplace une référence locale et coûte une
  milliseconde, et le faire attendre derrière un `push` est la panne qu'on ne
  diagnostique pas. Un test verrouille les six.
- **`branch_written` est le `write_then_refresh` de cet étage** : ce que ces
  commandes changent n'est pas le statut d'un worktree mais la liste que le
  sélecteur affiche, et une liste qui porte encore la branche qu'on vient de
  supprimer est la seule chose que ce menu ne doit pas faire. Aucun worktree
  n'est nommé — une branche appartient au dépôt, et celle qu'on écrit est
  souvent tenue par un checkout que la fenêtre n'a jamais ouvert.
- **Un nom distant n'est pas une branche**, et c'est tout `repo::checkout` :
  `git switch origin/feat` échoue, la référence n'étant pas quelque chose que
  HEAD puisse viser. Ce que cliquer `origin/feat` veut dire est la branche
  locale qui la suit, que `--track` crée et qui existe peut-être déjà sous le
  nom court. Le local est cherché **en premier** : un nom local porte lui aussi
  des barres obliques (`wt/essai`).
- **Mettre à jour est deux commandes et non une**, et laquelle s'applique n'est
  pas une préférence : **git refuse de fetcher dans une branche déployée**, donc
  une branche tenue par un worktree se met à jour depuis l'intérieur
  (`pull --ff-only`) et toute autre par le refspec (`fetch origin b:b`), qui
  déplace la référence sans arbre de travail. Fast-forward des deux côtés : une
  branche sur laquelle personne ne se tient ne reçoit pas un commit de fusion
  d'une entrée de menu.
- **`split_remote` est pur et testé** : il décide le refspec, et une coupe fausse
  y va chercher ce qui existe sous un autre nom.
- **« Comparer » pose la base de la revue et va sur l'écran Git** : la revue de
  branche *est* cette comparaison, et elle garde la base choisie, par worktree.

**Ce qu'on peut demander d'un worktree** est `ClaudhubApp::worktree_actions`,
**une table lue deux fois** : le `…` de la barre la plie en `PopupMenu`, le
second temps du sélecteur la peint en lignes. Deux listes auraient divergé au
premier ajout, et la divergence est du genre silencieux — il manquerait à l'un
des deux menus une entrée dont personne ne remarque l'absence. Elle porte ce
qu'une ligne offrait (mettre à jour, intégrer, `wt up`/`down`/`rm`, les tâches du
projet, retirer le dépôt) et ce qu'une ligne ne pouvait pas offrir : travailler
ici, copier le chemin, ouvrir le dossier. **Et une entrée qui ne parle pas du
worktree mais de la fenêtre** : l'épingler.

### Les worktrees épinglés

Un popover est la bonne forme pour douze checkouts qu'on parcourt ; c'est la
mauvaise pour les deux ou trois entre lesquels on fait l'aller-retour toute la
journée. Une **épingle** dit lesquels, et ceux-là deviennent un `ButtonGroup`
dans la barre du haut : un clic au lieu d'un popover et d'un filtre.

Ils sont **devant le sélecteur de worktree** et non derrière — après les deux
flèches de la piste, avant les deux sélecteurs. Une épingle est le plus court
chemin vers le geste que le sélecteur fait, et ce qu'on met devant est ce vers
quoi la main va d'abord ; le sélecteur reste juste derrière, pour les checkouts
qu'on n'a pas épinglés.

- **L'épingle est une entrée de `worktree_actions`**, pas un geste à part : la
  table est déjà lue deux fois — le `…` de la barre et le second temps du
  sélecteur — et une épingle posée ailleurs aurait manqué à l'un des deux.
  L'entrée dit « épingler » ou « retirer » selon l'état, d'où le `&App` que la
  table prend désormais.
- **La liste vit dans le magasin** (`Store::pinned`), pas dans les réglages :
  c'est où l'on travaille cette semaine, pas une préférence qu'on écrit à la
  main. Et **une liste plate, tous dépôts confondus** — c'est ce que la rangée
  *est*, le passage du checkout d'un projet à celui d'un autre.
- **Une nouvelle épingle va en dernier** : la rangée se lit de gauche à droite,
  et une épingle qui pousserait les autres déplacerait le bouton qu'on visait.
- **Une épingle que personne ne réclame est gardée et non peinte** : un dépôt
  qu'on n'a pas encore ouvert n'est pas un dépôt dont les épingles s'oublient —
  la règle des notes du magasin. Ce que `forget_missing` retire est un checkout
  dont git vient de dire qu'il n'est plus là, et l'épingle part avec lui : elle
  ne porte pas de dépôt à elle sur quoi être jugée plus tard.
- **Plein contre contour**, la polarité de la fenêtre : l'état « sélectionné »
  d'un groupe en contour est un fond à quelques pour cent du sien.
- **Rien n'est peint quand rien n'est épinglé** : un groupe vide prendrait aux
  sélecteurs la largeur qu'il faut pour dire qu'une fonction existe.
- **Le nom du dépôt précède celui du checkout et se fond quand il le répète**
  (`project_label`, celui du multiplexeur) : deux worktrees appelés `main` dans
  deux dépôts est le cas pour lequel la mention existe.
- **Un bouton porte l'agent et le `+n −m`**, les deux choses qu'on regarde avant
  de basculer et les deux que porte déjà la ligne du sélecteur. Elles viennent
  du **balayage de fond**, donc une épingle dit d'un checkout qu'on n'a pas
  ouvert ce qu'elle dit des autres : l'agent toutes les deux secondes, le volume
  un relevé sur cinq. La pastille est **sans le nom du programme**
  (`topbar::agent_dot`) — le bouton dit déjà quel checkout c'est, et les noms
  prendraient à la rangée la largeur qu'elle a à donner ; la ligne du sélecteur
  a la place et les dit.
- **L'épingle se pose depuis la ligne du sélecteur**, où un bouton est peint sur
  **toutes** les lignes, épinglées ou non — la règle du `…` d'à côté. C'est la
  même bascule que l'entrée de `worktree_actions`, sortie là où l'œil est déjà :
  on décide d'épingler en parcourant la liste, pas après avoir ouvert un menu
  sur une ligne. Et **le popover reste ouvert**, seul geste de ce panneau qui ne
  le ferme pas : épingler n'est pas aller quelque part.

`Ctrl+1` à `Ctrl+9` **ne suivent pas les épingles** : elles désignent les
worktrees dans l'ordre affiché, et une touche dont le sens changerait selon
qu'on a épinglé quelque chose n'est mémorisable dans aucun des deux états.

**La barre du haut *est* la barre de titre de la fenêtre**
(`gpui_component::TitleBar`). Ce n'est pas un raffinement :
`TitleBar::title_bar_options()` demande à la plateforme de ne pas en dessiner
une, et tant que rien ne la remplaçait, la fenêtre Windows n'avait plus de quoi
être déplacée ni fermée. `ui::run` part de `TitleBar::window_options()` et non de
`Default`, qui pose aussi `app_owns_titlebar_drag` — sans lui, la plateforme et
notre barre se disputent le double-clic. **Les boutons posés dans la zone de
déplacement restent cliquables** (gpui redistribue les messages souris de zone
non cliente) ; les boutons de la fenêtre sont **hors** de cette zone.

**Une action va où se fait le geste dont elle est la fin** : `fetch`, `pull` et
`push` vivent dans la barre de « Modifications ». Les terminaux se basculent
depuis le **coin en bas à droite**, à l'angle sur lequel ils s'ouvrent, et le
**« + » du dernier onglet est à côté** — le même bouton, avec son menu de
profils : les terminaux masqués, il n'y a plus de barre d'onglets d'où en
demander un, et les montrer pour cela seul est un geste de trop. Ce
bouton porte son nom et suit la polarité des deux sélecteurs qui l'encadrent —
**plein** quand les terminaux sont là, en contour sinon : un `ghost` marqué
« sélectionné » est un fond à quelques pour cent de celui de la barre, invisible
sur la moitié des thèmes.

**La barre du diff est en trois groupes**, et ce n'est pas de la mise en forme :
la taille minimale d'un élément flex vaut celle de son contenu, si bien qu'un
chemin long poussait les boutons hors de la barre. Le chemin porte `min_w_0` — il
est le seul à céder. Un seul bouton porte son mot, « Éditer », et il est **avec
les quatre déplacements** et non avec les bascules de droite : on parcourt les
modifications, quelque chose cloche, on ouvre le fichier là où il est — le geste
qui écrit est la fin de ce parcours, et parmi des icônes qui parlent toutes de
lecture, celle-là vaut d'être lue. **Annoter n'est pas dans cette barre** : c'est
un geste du clic droit, là où la sélection est.

**`pull` et `push` portent leur compte et s'allument** : la barre d'état dit *où
l'on est*, le bouton dit *ce qu'il y a à faire*.

**Toute entrée de menu porte une icône** : un menu se parcourt à la verticale et
se choisit au geste. Deux entrées qui font la même chose sur deux objets portent
la même. Un nom d'icône qui ne désigne aucun fichier ne provoque **aucune
erreur**, il peint un vide.

### Le grain de l'interface

Ce qui datait n'était pas une couleur mais une **géométrie** : des rectangles
cousus bord à bord et les rayons par défaut de gpui-component.

**Les rayons montent à huit et douze** dans `theme::apply`, et seulement si la
palette ne s'en occupe pas. **La carte, c'est le groupe d'onglets entier** —
barre comprise —, peinte par le fork : `TabGroupSkin::frame` s'arrondit hors
variant classique, et les splits s'espacent d'une gouttière de quatre pixels.
`panels::pane_root` ne dessine donc **plus rien**. La barre est **sur la carte**,
et la pastille active porte un ton surélevé (`tab_active` = `secondary`).

**Le masque de contenu de gpui est rectangulaire** : l'arrondi d'un élément ne
rogne que son propre fond, jamais ses enfants — d'où le `rounded_b` du fond de
`pane_root`. Corollaire : la moitié **fixe** d'une division est celle du bas,
jamais celle du haut.

**Tout panneau passe par `panels::pane_frame`**, pas seulement ceux que la macro
`panels!` fabrique : c'est lui qui peint le fond arrondi du bas.

**Et qui redécoupe les deux coins du bas, par-dessus le contenu**
(`panels::corner_cut`). Le fond arrondi n'a le dernier mot que sur ce qui est
peint **avant** lui : un panneau dont le contenu peint sa propre surface
ramenait le coin carré — l'éditeur remplit un rectangle opaque de la hauteur du
panneau sous ses numéros de ligne, la table de résultats un par ligne —, et
aucun arrondi ne peut atteindre l'intérieur d'un enfant. Ce qu'on peint est le
**dehors exact du quart de cercle**, dans la couleur de la gouttière, et non un
petit carré de cette couleur : un carré couvrirait aussi ce qui tombe *dedans* —
le dernier centimètre de course d'une barre de défilement à droite — et ne
vaudrait que là où les deux couleurs se trouvent égales. Le quart est une
Bézier cubique au kappa usuel : un arc elliptique dirait la même chose avec deux
drapeaux dont le sens dépend du sens de l'axe y.

**La vue racine rembourre le dock**, des mêmes quatre pixels que le fork met
entre les cartes — mais **huit sur les côtés**, et c'est la règle et non un
oubli : la barre de titre et la barre d'état se ferment chacune par une bordure,
si bien que quatre pixels y **se lisent** comme une marge, quand les mêmes quatre
contre le bord nu de la fenêtre ne se lisent pas du tout. Des nombres égaux ne
paraissaient égaux nulle part.

**La poignée de redimensionnement ne peint rien au repos.** `gpui-base` tient
**sa propre copie** du thème, et `theme::apply` doit la projeter
(`Theme::sync_base`) après ses retouches ; la projection reconstruit la copie à
partir de zéro, donc l'extinction de la poignée vient **après** elle. C'est pour
écrire dans ce global que `gpui-base` est une dépendance directe.

**Une ligne de liste est un bandeau, pas une pastille.** Ce qu'une ligne
sélectionnée désigne est la **ligne**, pas son texte : c'est sur elle que le clic
droit, la case à cocher, la coche de relecture et le dépôt d'un fichier vont
agir. Un fond qui s'arrête au bout du nom dit autre chose — et il le dit
différemment d'une ligne à l'autre, la longueur des noms étant ce qui varie le
plus dans une colonne. D'où `w_full` sur l'entrée, **aucun rayon**, et pas de
`px_1` sur la liste : c'est ce que peignent PhpStorm, VS Code et tout ce qui
montre une liste de fichiers. Les huit listes de la fenêtre le font — arbre du
projet, revue et index, arbre des bases, tags, historique SQL, résultats de
recherche, historique git, liste d'un plugin — et l'entrée porte alors son propre
retrait horizontal, `uniform_list` **ignorant les marges de ses entrées**, dont
il calcule la taille.

Deux exceptions, et elles disent la règle : une **pastille** dans un dialogue
(le choix d'un `[[prompt]]` de `wt`), qui est une carte et non une colonne ; et
tout ce qui n'est pas une ligne de liste — badges, mentions, extraits — qui garde
son rayon.

**Et le bandeau va sous la barre de défilement, ce qu'il porte non**
(`theme::scroll_gutter`). La barre est peinte **par-dessus** le contenu et non à
côté — voir « Les barres de défilement » —, donc une ligne qui va jusqu'au bord
passe dessous, et ce qui vit au bout d'une ligne est précisément ce qu'on lit
d'un coup d'œil : la coche de relecture, la lettre du statut, le `+n −m`. Le
fond, lui, traverse : c'est lui qui dit sur quelle ligne on est. La gouttière est
**toujours réservée**, la barre ne se peignant que lorsqu'il y a de quoi
défiler : un rembourrage qui arriverait avec elle décalerait le bout de chaque
ligne le jour où la liste gagne un élément — et le rendrait le jour où l'on en
indexe un. La largeur est celle du composant (`Scrollbar::width`), pas un nombre
à nous. Les panneaux qui défilent d'un bloc — conflits, notes — la portent sur
leur conteneur, n'ayant pas de lignes à qui la donner.

**Les onglets sont des pastilles** (`TabVariant::Segmented`) : le variant par
défaut du dock a un rayon **codé en dur à zéro**. D'où le **fork** (voir
`Cargo.toml`), seize commits au-dessus de leur `main` :

1. le `TabVariant` que `DockSkin` fait passer jusqu'au `TabBar` ;
2. les coins en boîte bordée réservés au variant classique ;
3. le groupe lu comme une carte hors variant classique ;
4. la même gouttière entre les **zones** du dock ;
5. `split_gap`, pris en **rembourrage** dans chaque case sauf la première — un
   `gap` CSS n'espaçait rien, ce cadre n'ayant qu'un enfant, et une marge aurait
   faussé les tailles que le redimensionnement distribue ;
6. **une sélection reste peinte quand le focus part à un menu** — sinon un clic
   droit ouvre son menu par-dessus un texte qui n'a plus l'air sélectionné, sur
   le seul geste pour lequel la sélection est justement conservée ;
7. **un contrôle peut cacher son caret sans se désactiver**
   (`set_cursor_hidden`), ce que demande un éditeur modal ;
8. **le fond d'un run est peint** — `ShapedLine::paint` ne dessine que les
   glyphes, et le `background_color` appartient à `paint_background`, que rien
   n'appelait : une `TextDecoration` qui en pose un était **invisible en
   silence**, ce qui empêchait le curseur bloc de vim et l'éclat d'un yank de
   s'afficher ;
9. **une application peut replier sans passer par la gouttière**
   (`fold_candidates`, `set_folded`, …), `display_map` étant privé ;
10. **l'éditeur dit jusqu'où il défile** (`scroll_size`) : c'est la mesure du
    dernier rendu, celle-là même contre laquelle `set_scroll_offset` rogne, et
    la déduire du nombre de lignes rate un repli de ligne, un repli de code et
    la place gardée sous la dernière ;
11. **un contrôle peut demander un caret en bloc** (`set_caret_block`), large
    d'un caractère, haut d'une ligne, dans sa couleur et sans clignoter ;
12. **ce qu'une division laisse en trop va à un slot qu'aucune taille ne fixe**
    — c'était le dernier slot montré, celui-là même qu'un appelant venait
    d'épingler, si bien qu'un panneau détaché avec sa taille se dessinait à tout
    autre chose et que l'état gardait une part que rien à l'écran ne portait ;
13. **une aire de dock refuse un panneau qui n'est pas le sien** : deux aires
    peuvent être à l'écran en même temps — celle du multiplexeur est peinte
    **dans** un panneau de celle de l'écran —, et un onglet glissé de l'une à
    l'autre arrivait dans
    `move_panel` sous un identifiant que l'aire réceptrice n'a jamais
    enregistré. Il y était **inséré quand même** : un panneau dans un arbre qui
    n'a pas sa vue, dans lequel le reconcile déclenché par l'insertion entre
    aussitôt — l'assertion de `views_of` en debug, avant même qu'on ait peint,
    et en release un groupe dont l'indice actif a glissé en silence. Le refus
    est aussi ce que doit faire un dépôt qui ne peut pas atterrir : l'onglet
    revient d'où il vient.
14. **une application peut marquer la gouttière** (`set_gutter_marks`) : elle
    est peinte **dans** l'élément, et les deux crochets qu'elle avait — le
    renderer de chevron, le fond — pendent à `InputEditorStyle`, que
    `Input::render` reconstruit depuis le thème **à chaque frame**. Ce qu'une
    application y écrivait avait disparu à la suivante, si bien qu'une bande de
    modifications n'avait nulle part où vivre. D'où un renderer posé sur
    l'**état**, appelé une fois par ligne **tampon** visible, dont les éléments
    sont disposés au bord droit de la gouttière — dans la **marge** qui sépare
    déjà les numéros du texte, et non dans une colonne à eux : celle-ci
    éloignait le texte des numéros que quelque chose soit marqué ou non. Le
    parcours est celui des numéros de ligne, et il le faut : `offset_y`
    s'accumule depuis les `wrapped_lines` de chaque ligne, donc une marque
    tombe où tombe son numéro — la déduire de l'indice dériverait dès qu'une
    ligne se replie, en silence et seulement dans les fichiers assez longs pour
    le faire.
15. **le seul panneau d'une zone latérale se déplace quand même** : `alone` se
    lisait par arbre, et chaque zone d'une aire est le sien, si bien qu'un
    panneau seul à gauche se déclarait le dernier du dock — ni déplaçable, ni
    fermable. Ce que le drapeau garde est qu'on ne vide pas le dock, et le dock
    est l'aire entière : il se compte donc une fois sur le centre et toutes les
    zones, puis descend inchangé — être déplaçable est un fait sur l'aire, pas
    sur le nombre de voisins qu'une division donnée porte. Deux tests, un par
    moitié de la règle.
16. **et la zone qu'il laisse cesse d'être peinte** : un dock garde sa taille et
    son arbre une fois vidé, si bien que le cadre réservait encore la largeur de
    la colonne autour d'un groupe qui ne portait plus rien — une bande morte le
    long du bord. Vider une zone latérale *est* la fermer, ce que le commit
    précédent suppose déjà, et la question était déjà répondue : `is_empty`
    demande si quelque chose y serait dessiné. `render_dock` la pose à son tour,
    donc la poignée de redimensionnement s'en va avec le cadre — il n'y a plus
    rien à redimensionner — et masquer le dernier panneau visible d'une zone dit
    désormais la même chose.

Les commits ont vocation à partir en PR.

**`PanelStyle::TabBar` et non `Auto`** : `Auto` rend un titre plat dès qu'un
groupe n'a qu'un panneau, soit deux chromes pour une même fenêtre.

**`Theme::tokens` est dérivé de `Theme::colors` une seule fois**, à l'application
de la palette, et les composants récents lisent `tokens` : toute couleur écrite
dans `theme::apply` doit être suivie du recalcul, sans quoi elle ne se voit nulle
part et rien ne le signale.

### Les thèmes

Une douzaine de palettes livrées, couleurs de leurs auteurs, répartition des
rôles de nous. Elles sont **générées** par `tools/gen_themes.py` : un thème
complet compte une centaine de clés, et une clé absente ne provoque pas d'erreur
— elle reprend la valeur par défaut, qui est *claire*, soit une tache blanche au
milieu d'un thème sombre. Deux tests : chaque fichier doit se lire, et aucun ne
doit avoir moins de clés que les autres.

Le registre de gpui-component ne se charge **que depuis un répertoire**, qu'il
surveille. Les thèmes sont donc embarqués puis écrits dans `<config>/themes/` au
démarrage — effet de bord heureux, le même répertoire accueille ceux de
l'utilisateur. Corollaire : les `claudhub-*.json` sont réécrits à chaque
démarrage, donc pour en modifier un il faut le copier sous un autre nom.

Deux réglages : `theme` dit s'il fait clair ou sombre (le système peut décider),
`light_theme` et `dark_theme` disent *quelle* palette porte chaque apparence.
C'est la structure de `Theme` lui-même. Le chargement du registre est asynchrone,
d'où la ré-application dans le rappel de `watch_dir`.

### Les sous-applications

Claudhub fait quatre métiers qui n'ont presque rien en commun : relire un diff,
retoucher un fichier, interroger une base, dépouiller une erreur. Tant qu'ils
partageaient une fenêtre, chacun payait la place des trois autres.

Sept **écrans** (`ui::workspace::Workspace`) : Git, Édition, Recherche, Bases,
Sentry, Réglages, Multiplexeur, atteints par `Alt+1` à `Alt+7`. Le premier s'appelle « Git » et
non « Revue » — il porte aussi l'historique, les branches, les conflits et le
commit — mais sa clé de disposition **reste `review`** : c'est par elle qu'une
disposition enregistrée se relit.

**Deux écrans ne sont pas dans cette barre-là** : les Réglages et le
Multiplexeur, où l'on ne va pas travailler — on change comment le reste se
comporte, ou on regarde ce qui tourne partout avant de repartir vers le worktree
qu'on y a trouvé. Ils forment un **groupe de deux boutons à l'extrémité droite de
la barre de titre** (`Workspace::ASIDE`), et non deux icônes posées côte à côte :
un groupe dit qu'ils sont le même genre de détour. `Workspace::working()` est
`ALL` moins ces deux-là, si bien que les deux listes ne peuvent pas diverger —
et l'indice que rend un groupe de boutons est un indice dans ce qu'il
**affiche**, jamais dans `ALL`. Là comme dans la barre d'état, l'écran courant
est **plein** et l'autre en contour. Le multiplexeur **porte son nom** — une
icône seule est un rébus qu'on apprend au lieu de le lire —, l'engrenage non :
c'est la seule icône de la fenêtre qui n'a pas besoin d'être glosée, les réglages
étant derrière un engrenage dans toutes les applications, et le mot ne ferait
qu'y prendre de la largeur aux sélecteurs. Les deux gardent leur infobulle.

**Et les deux sont des bascules** : pressé une seconde fois, l'un ou l'autre rend
le dernier écran de **travail** (`ClaudhubApp::worked_in`), celui-là même où
« travailler ici » renvoie un terminal — tout ce qui quitte le détour passe par
la même porte. C'est ce que ces deux écrans sont : un détour — qu'est-ce qui
tourne, comment le reste se comporte —, et un détour se reprend par le bouton
qui l'a ouvert. Une seule réponse pour le groupe, et jamais un écran où l'on ne
peut pas travailler, ces deux-là ne s'inscrivant jamais dans `worked_in`. Le
clavier reste dehors comme partout ailleurs : `Alt+6` et `Alt+7` se défont en
pressant la touche d'où l'on vient. Le retour **s'inscrit sur la piste**, comme
l'aller.

**Les boutons portent leur nom** : six icônes en rang sont un rébus qu'on apprend
au lieu de le lire, et une infobulle est un nom qu'il faut aller demander.

**Un dock par écran**, et non un dock dont le centre change : chacun a ses
panneaux et ses tailles. Les six sont **construits au démarrage** — un dock se
bâtit avec `window`, et le faire au rendu créerait des entités au milieu d'une
frame.

**Une seule vue est partout : les terminaux**, seul panneau instancié une fois
**par dock** (un panneau n'appartient qu'à une aire à la fois).

- **Rien à faire en changeant d'écran que de changer de dock** : l'état vit dans
  `ClaudhubApp`, pas dans les panneaux.
- **Rien, sauf le focus** (`focus_workspace`) : le focus restait où l'écran
  précédent l'avait laissé, le plus souvent un terminal, à qui appartiennent
  alors les flèches et la copie. L'éditeur et la console sont nommés un par un ;
  ailleurs c'est le manche de la racine. Un champ qu'on ne voit pas n'est jamais
  visé — l'écran « Édition » rend alors le focus à l'arbre du projet.
- **Un geste qui ouvre quelque chose emmène sur son écran** : ouvrir un fichier
  bascule sur « Édition », une console sur « Bases ».
- **Les tailles d'une division se donnent toutes**, et ici ça se paie vraiment :
  un `None` vaut cent pixels dans l'état enregistré, et la pile répartit **au
  prorata** — un centre décrit `[None, 220]` s'affiche à 31/69 au lieu de 76/24.
  La disposition d'un écran jamais ouvert n'est jamais mesurée, si bien que le
  défaut mal écrit se voyait sur trois écrans sur quatre. Les largeurs se donnent
  sur celle du **centre**.
- **Le choix de l'écran vit dans la barre d'état**, à gauche, et non dans une
  barre à lui : les deux se suivaient, hautes de trente pixels à elles deux, et
  disaient la même chose — *où* l'on est. La barre passe à
  `theme::toolbar_height` et est peinte par la **vue racine**, un panneau
  pouvant se masquer.
- **L'écran actif est plein, les autres en contour** : l'état « sélectionné »
  d'un `ButtonGroup` en contour est invisible sur la moitié des thèmes.

L'écran qu'on regardait revient à l'ouverture, retenu dans `layout.json`.

### Le multiplexeur

Les terminaux sont des panneaux de dock, un panneau n'appartient qu'à une aire à
la fois, et chaque aire ne montre que le worktree regardé : ce qui tourne dans
les onze autres est vivant et invisible. C'est exactement l'état dans lequel on
laisse une demi-douzaine d'agents, et la question qui n'avait aucune vue est
« lequel a fini ». Elle est la seule qui **traverse les worktrees**.

**Cet écran n'est donc pas une vue : c'est un dock qui ne porte que les
terminaux.** Un terminal a déjà une face par écran — la même
`Entity<TerminalView>`, un panneau chacune —, et celle du multiplexeur diffère
par exactement deux choses (`Workspace::shows_every_worktree`) : elle se montre
quel que soit le worktree regardé, et son onglet dit à quel projet elle
appartient. Tout le reste — divisions, poignées, onglets qu'on glisse, zoom, la
croix, le renommage — est celui du dock, inchangé.

- **Aucun chrome à nous** : pas de panneau, donc pas de barre d'onglets au-dessus
  de celle des terminaux, ni de titre répétant ce que le bouton de la barre de
  titre vient de dire. `install_default_layout` **rend la main tout de suite**
  pour cet écran : il n'y a pas de disposition à poser, et un centre vide est ce
  qu'une aire neuve est déjà.
- **Le premier terminal y prend tout le centre** : ailleurs il s'ouvre en bande
  sous le contenu de l'écran, ici il n'y a pas de contenu à ménager, et diviser
  une racine vide l'aurait épinglé en bas avec du vide au-dessus.
- **Le dépôt et le worktree se fondent en un quand ils disent la même chose**
  (`project_label`) : un checkout principal est d'ordinaire un dossier au nom de
  son dépôt, et « nixos / nixos » est un mot perdu sur une barre d'onglets. Deux
  worktrees appelés `main` dans deux dépôts est le cas pour lequel la mention
  existe, et c'est justement celui où les deux diffèrent.
- **Les terminaux d'un même worktree restent voisins** : un nouvel onglet rejoint
  ceux de son worktree, comme sur les autres écrans. L'aire mélange les projets,
  la barre reste lisible.
- **Le « + » du dernier onglet ouvre sur *ce* worktree** ici, et sur celui qu'on
  regarde partout ailleurs : une barre qui mélange les projets n'a pas de
  « worktree courant » qu'on puisse y lire.
- **Le clic droit d'un onglet gagne « travailler ici »** (`work_in_worktree`) :
  le worktree devient celui qu'on regarde, les terminaux sont rendus visibles
  s'ils étaient masqués, et l'écran redevient le dernier écran de **travail**
  (`ClaudhubApp::worked_in` — ni les Réglages ni le multiplexeur ne s'y
  inscrivent, ce sont les deux écrans qu'on ne fait que regarder). Ailleurs,
  l'entrée serait une façon d'aller où l'on est.

Deux formes ont précédé celle-ci, et ce qu'elles ont coûté vaut d'être su. Une
**grille de tuiles** peinte par nous était une image de dock, sans rien de ce
qu'un dock fait. **Une aire par projet**, empilées sous des titres, faisait de la
frontière entre projets une affaire de disposition — au prix d'une page entre les
rangées de laquelle on ne pouvait rien réarranger, d'un glissement d'une aire à
l'autre qui invitait à un dépôt qu'il refusait ensuite, et du chrome ci-dessus.
Il en reste le **treizième commit du fork** : une aire de dock refuse un panneau
qui n'est pas le sien, ce qui reste vrai de toute fenêtre où deux aires
coexistent.

- **Les terminaux sont vivants, et cela coûte un redimensionnement** : ce sont
  les mêmes vues que les autres écrans montrent, donc on y tape ; et une
  `TerminalView` ajuste son pty à la place qu'on lui donne, donc entrer sur cet
  écran redimensionne tout ce qui s'y trouve et en sortir les remet. C'est ce que
  fait tmux quand on zoome un volet. Aucun pty n'est dessiné deux fois dans une
  frame : une seule aire est à l'écran à la fois.
- **Le pty n'est prévenu qu'au relâchement** : l'attente de
  `TerminalView::request_size` **repart à chaque changement**, ce qui est toute
  la différence entre « un redimensionnement toutes les 150 ms tant que la main
  bouge » et « un quand elle s'arrête ». L'attente est bornée (`MAX_DEFERRALS`,
  trois secondes) : une taille qui changerait sans fin laisserait sinon le
  programme sur une géométrie périmée sans que rien ne le dise.
- **Et pendant ce temps la vue dit la géométrie qui vient** : sous la grille de
  l'ancienne taille, rognée, un terminal a l'air figé. La pastille
  `colonnes × lignes` est toute la réponse, celle de n'importe quel gestionnaire
  de fenêtres pavant.
- **La molette s'arrête au terminal** (`cx.stop_propagation()`) : rien au-dessus
  n'a à recevoir un cran qu'un terminal a traduit en flèches, en rapport à un
  programme ou en zoom.

### Le dock

La disposition appartient à `gpui_component::dock` : chaque zone est une
**entité à part** (`ui/panels.rs`), le dock ne sachant déplacer que des entités.
Les panneaux ne portent aucun état : ils délèguent à `ClaudhubApp`, dont ils ne
gardent qu'une référence **faible** — forte, elle formerait un cycle — et
qu'ils observent.

Rendre depuis un `update` sur `ClaudhubApp` est licite : le rendu d'une vue
enfant a lieu *après* que la fermeture de rendu du parent a rendu la main.

Six pièges, tous rencontrés :

- **Un panneau sans pile parente est verrouillé** (`is_locked`) : tout panneau
  doit être enveloppé, fût-ce dans une division d'un seul élément.
- **`toggle_dock` ne notifie pas l'aire**, seulement le dock intérieur.
- **Le dernier panneau de l'*aire* ne se déplace pas** (`is_last_panel`) : ce
  qu'il garde, c'est qu'on ne vide pas le dock, et le dock est l'aire entière.
  Longtemps le drapeau se lisait **par arbre**, or chaque zone est le sien : un
  panneau seul dans la zone de gauche se croyait le dernier qui soit et ne
  pouvait ni se déplacer ni se fermer — sur l'écran de recherche, la liste des
  résultats était clouée à gauche sans moyen de la mettre à côté de l'aperçu.
  Emmener le seul panneau d'une zone latérale vers le centre vide cette zone,
  ce qui *est* la fermer, et il reste où le reposer — le centre. **Et la zone
  vidée n'est plus peinte du tout** (seizième commit) : cadre, largeur réservée
  et poignée de redimensionnement partent avec le dernier panneau, sans quoi il
  reste une bande morte le long du bord. La disparition d'un panneau passe par
  `Panel::visible`.
- **Les tailles d'une division se donnent toutes** (voir ci-dessus).
- **L'état se relit au moment d'écrire**, pas à l'appel : l'ouverture d'une zone
  est différée d'une frame.
- **Le zoom d'un panneau est un bouton, pas une entrée de menu**
  (`zoom_in_toolbar`). Le bouton `…` reste affiché malgré tout —
  `TabPanel::render_toolbar` le pose sans condition —, d'où l'entrée qu'il porte.
- **Le repli d'une zone est éteint** (`set_toggle_button_visible(false)`) : le
  dock pose, dans la barre du groupe le plus proche de chaque bord, un bouton
  qui replie la zone voisine — et c'est le seul chrome de la fenêtre qui dise
  quelque chose de faux. Nos panneaux ne sont pas un meuble fixé au bord d'un
  écran, ce sont des onglets qu'on glisse où l'on veut ; « replier la zone de
  gauche » nomme donc un rangement que personne n'a fait, et ce que le bouton
  masquait ne revenait que par un bouton posé sur la barre d'un **autre**
  groupe. Le réglage vient du fork, il n'a rien coûté de plus.

**L'écran Git s'ouvre sur « Modifications », quel que soit l'onglet qu'on
regardait en fermant** (`app::open_on`, appelé à la lecture de `layout.json`) :
un groupe retient l'onglet affiché, ce qui est juste pour l'historique qu'on
relisait et faux pour la question à laquelle cet écran répond — le tableau d'un
plugin n'est pas « ce qu'il y a à relire ». **Au démarrage seulement**, jamais à
chaque visite de l'écran : dans une session l'onglet appartient à qui l'a
cliqué, la règle de la base de revue (`range_chosen`).

La disposition est enregistrée dans `<config>/layout.json` — **une par écran**,
plus le nom de celui qu'on regardait —, à part des réglages. `LAYOUT_VERSION` la
fait écarter quand les panneaux changent de nom. Ce qu'une disposition relue
porte encore et que le registre ne sait pas bâtir est **élagué**
(`app::prune`) — et **seule une feuille se juge sur son nom** : un conteneur
porte celui du dock (`TabPanel`, `StackPanel`), qui n'est pas des nôtres, si bien
que le juger arrêtait la descente à la racine et n'élaguait jamais rien. Le
symptôme est précisément ce que l'élagage évite : « the `ClaudhubMultiplexer`
panel type is not registered » en travers de l'écran, à chaque démarrage, pour un
panneau disparu le jour où cet écran est devenu le dock des terminaux. Un centre
qu'il ne reste rien vaut « non restauré », donc la disposition par défaut. Les panneaux se déclarent au
registre (`panels::register`), sans quoi une disposition relue ne saurait pas les
fabriquer — le symptôme est un « panel type is not registered » écrit en travers
de l'écran, qui revient à chaque démarrage et qu'une réinitialisation de la vue
ne fait que différer. La déclaration est donc **engendrée par la macro
`panels!`** (`register_generated`) : deux listes divergeaient au premier ajout,
et c'est ce qui était arrivé aux tags et à l'historique SQL.

**Et ce qui est relu passe par le même savoir** : `panels::is_registered` — que
`declare_panel` remplit au fur et à mesure des déclarations, donc sans seconde
liste — et tout nom que le registre ne connaît pas est **élagué** de la
disposition relue. Non pas une liste de coupables connus : un plugin désinstallé
entre deux sessions laisse son nom derrière lui, et un panneau **à nous** qui
disparaît aussi — celui du multiplexeur, le jour où cet écran a cessé d'être une
vue pour devenir le dock des terminaux. `LAYOUT_VERSION` ne répond à cela qu'en
jetant l'agencement de tous les écrans, ce qui est cher payé pour un nom mort.

**Les terminaux dans le dock** : un panneau par terminal, la barre du dock *étant*
la rangée d'onglets, ce qui permet de glisser un terminal dans un split.

- **Un panneau par terminal et par écran qui en porte**, rendant la **même**
  `Entity<TerminalView>` — il n'y a toujours qu'un pty. `OpenTerminal::panels` les
  range dans l'ordre de `Workspace::terminal_hosts`, et **pas** de
  `Workspace::ALL` : le multiplexeur n'en dock aucun, si bien qu'un rang pris
  dans `ALL` désignerait le panneau du voisin. D'où `panel_on`, qui rend `None`
  là-bas.
- **La place se garde par l'invisibilité, pas par un déménagement** : retirer
  puis reposer les panneaux aurait perdu leur place à chaque bascule de worktree.
- **Le premier terminal d'un écran ouvre son emplacement** sous le **dernier
  slot** d'une rangée horizontale, et sous le centre entier à défaut ; les
  suivants rejoignent son groupe d'onglets, `activate: true`. La colonne de
  listes de la revue vit dans le centre et non dans un dock, si bien qu'un
  terminal ouvert sous toute la rangée remontait la liste des fichiers avec le
  diff : on lit une liste **à côté** d'un terminal, jamais au-dessus. Prendre le
  dernier slot le dit en une règle qui donne aussi la bonne réponse là où le
  centre ne porte qu'une colonne.
- **La taille demandée pour ce slot n'est tenue que depuis le fork** : celui qui
  absorbe ce qu'une division laisse était le **dernier** slot montré, donc celui
  qu'on venait d'épingler — les 260 pixels du terminal se dessinaient à la
  moitié de ce que le centre avait. La croissance va désormais au dernier slot
  **qu'aucune taille ne fixe** (douzième commit du fork).
- **`panel_handle` et `add_panel_view`, jamais `add_panel`** : un `Entity<P>` se
  convertit tout seul en `PanelView` et le dock l'accepte sans rien dire, mais
  sans onglet, ni titre, ni contenu. C'est l'échec silencieux de la refonte du
  dock.
- **Et l'ajout-puis-déplacement passe par `panels::dock_panel_at`** :
  `add_panel_view` ne prend pas de cible — il pose le panneau dans le **premier
  groupe d'onglets** du centre et l'y **active**, et le `move_panel` qui suit
  l'en retire aussitôt. Retirer l'onglet qu'un groupe affiche laisse son index
  actif un cran après la fin, et le rognage retombe sur le **dernier** onglet —
  sur l'écran Git, le tableau du plugin CI, sur lequel chaque session s'ouvrait
  malgré `app::open_on` : le terminal de session traverse ce groupe à chaque
  démarrage. Le helper note l'onglet affiché avant l'ajout et le rend après le
  déplacement ; il ne le rend pas quand la cible rejoint ce groupe même, ni
  quand il n'y a pas de cible — le panneau qu'on vient d'ouvrir est alors celui
  qu'on regarde. Le défaut restait invisible tant que le dernier onglet était
  « Conflits », que son invisibilité faisait remplacer au rendu par le premier
  visible.
- **Le nom de l'onglet vient de l'application, pas de la vue** : il peut être
  donné à la main, et les six panneaux doivent dire la même chose. Un nom vide
  rend au programme le sien.
- **La croix est peinte par `Panel::title`**, la peau du dock ne dessinant aucun
  bouton de fermeture par onglet. Le clic est **consommé**, sans quoi la croix
  commencerait par amener au premier plan ce qu'elle s'apprête à fermer.
- **Le bouton de la molette ferme aussi** (`close_on_middle_click`, les onglets
  de fichier et de terminal) : c'est le geste de tout navigateur et de tout
  éditeur, et le seul qui ferme sans viser une croix grande comme un point. Un
  `on_aux_click` et non un `on_mouse_down` — un clic est une pression **et** un
  relâchement sur le même onglet, donc une molette pressée par erreur se reprend
  en lâchant ailleurs, ce qu'un bouton du milieu n'a aucun autre moyen de faire,
  n'ayant pas de glissement. C'est aussi ce qui porte le bouton : gpui envoie à
  cet écouteur **tous** les clics qui ne sont pas le gauche, le droit compris, et
  le droit appartient au menu de renommage. Il se pose sur la rangée **avant**
  que le menu contextuel ne l'enveloppe : un `ContextMenu` n'est pas un élément
  interactif, et il n'y a plus rien où accrocher un clic une fois qu'il est
  passé. Un terminal occupé pose sa question comme sous la croix — ce qui meurt
  avec ne dépend pas du geste qui l'a demandé.
- **Fermer un onglet tue le pty et ses autres faces** ; la fermeture est
  **différée**, `on_removed` étant appelé depuis l'édition du dock lui-même.
- **Une commande qui tourne se fait confirmer** (`ask_close_terminal`) : fermer
  envoie SIGHUP, et ce qui meurt avec est un build à moitié fait, une migration à
  moitié appliquée, un agent au milieu d'une tâche — le seul geste de cette
  fenêtre que git ne rattrape pas. Un shell à son invite se ferme sans un mot.
  **Ce qui le dit est le groupe de processus au premier plan du pty**
  (`Terminal::busy`, lu dans `/proc/<pid>/stat` : un shell à l'invite *est* ce
  groupe, un shell qui lance un travail l'a cédé) — et un **onglet d'agent** est
  occupé tant qu'il vit, son enfant *étant* la commande, sans invite où revenir.
  Ce qui distingue les deux est le **profil** avec lequel l'onglet a été lancé
  (`Launch::agent`) et surtout **pas** « on a nommé un programme » : le shell
  aussi est nommé, chaque onglet lançant celui des réglages — lu ainsi, tout
  terminal se disait occupé.
  Le parsage repart de la **dernière parenthèse fermante**, comme
  `agent::parse_cpu_ticks`, et il est testé. Hors Linux, seule la seconde moitié
  vaut. La question se pose aux deux **gestes** — la croix, `Ctrl+W` — et jamais
  dans `on_removed` : l'onglet y est déjà parti, et un dialogue annulé laisserait
  un pty vivant que plus rien ne montre. Un worktree qui s'en va n'en pose pas
  non plus.
- **Les terminaux sont retirés de `layout.json` avant écriture** (`app::prune`) :
  leur contenu est un **processus**. Un groupe vidé s'en va avec, et la taille
  correspondante est retirée de la pile — les tailles d'un `Stack` sont
  positionnelles.
- **Le « + » suit le dernier onglet** et voyage dans le **titre du dernier
  terminal**, celui du worktree et non du groupe. C'est l'application qu'on
  interroge pour savoir qui est le dernier, jamais le groupe — il est au milieu
  de son propre rendu, et c'est une panique déjà payée deux fois.
- **Un panneau de terminal ne lit pas l'application à sa construction** : il est
  bâti au milieu d'un `update`, donc l'entité est sortie de la table. Sa
  visibilité initiale lui est **donnée** par l'appelant.
- **La session ouvre un terminal, sur le worktree où l'on *atterrit*** — le
  premier seulement (`terminal_started`) : en ouvrir un par worktree visité
  laisserait une douzaine de shells derrière une après-midi. Et **jamais sur une
  sélection bouche-trou** (`session::session_terminal_due`, pur et testé) : tant
  que les dépôts s'énumèrent, la fenêtre montre le premier checkout de celui qui
  a répondu en premier (`SELECTION_FALLBACK`), et y ouvrir le terminal posait un
  shell dans un projet que la session n'a jamais demandé — puis, le drapeau
  étant mis, refusait au worktree mémorisé celui qui lui revenait. Un rang
  inférieur à celui de la session attend donc les dépôts encore attendus
  (`opening_repos`) ; quand il n'en reste aucun, le bouche-trou **est** la
  réponse et le terminal s'y ouvre.
- **Seul `Cmd::OpenRepo` se compte** : `OpenIfRepo` ne répond **rien** quand le
  dossier de lancement n'est pas un dépôt, et l'attendre serait attendre
  toujours.
- **La bascule ouvre quand il n'y a rien à montrer** : le drapeau est vrai par
  défaut et aucun terminal n'existe au démarrage.
- **Et elle s'éteint quand il n'y a plus rien** (`terminals_on_screen`) : le
  drapeau dit « non masqués » et reste vrai quand on a fermé le dernier onglet à
  la main, si bien que le bouton s'allumait au-dessus d'un coin vide — la seule
  chose qu'une bascule ne doit jamais dire. C'est une **lecture**, rien n'est
  écrit : le drapeau reste l'intention, et la pression suivante ouvre un terminal
  comme elle le fait déjà au démarrage. Sur le multiplexeur, ce sont les
  terminaux de **tous** les worktrees qui comptent.
- **Ouvrir un terminal le montre** (`open_terminal`) : le panneau a pu être
  masqué, et un pty installé derrière un panneau masqué est un processus auquel
  personne ne peut répondre — le `+` de la barre d'état paraissait cassé, la
  tâche du projet tournait hors de vue. C'est `set_panel_visible` et non
  `show_terminal_panel`, qui en ouvre un quand il n'y en a pas : d'ici, ce
  serait ouvrir le terminal qu'on est en train d'ouvrir. Les appelants qui
  demandaient les deux ne demandent plus que l'ouverture — le premier
  `Ctrl+Maj+T` en ouvrait deux.
- **Montrer un terminal n'est pas lui donner le focus** (`reveal_terminal` vs
  `focus_terminal`) : un message livré dans un onglet caché est un message que
  personne ne voit arriver, mais `Ctrl+T` veut les deux.
- **La fenêtre s'ouvre avec le focus posé** (`window.focus` à la fin de
  `ClaudhubApp::new`) : sans focus, aucun contexte n'est sur la pile, donc
  **aucun** raccourci ne se résolvait avant le premier clic.
- **Ce qui retire du focus de l'arbre doit dire où il va** : un `FocusHandle` que
  plus personne ne rend ne résout plus aucune liaison, et *tous* les raccourcis
  restent morts. Masquer rend le focus à la racine ; fermer le rend au terminal
  voisin.

**Masquer une vue** est la seule entrée du menu `…` d'un panneau : tout le reste
vit dans la barre du panneau, et masquer parle de sa place dans la fenêtre. On
revient par le **menu principal**, sous-menu « Vues » — une vue masquée n'a plus
d'onglet, donc plus rien à cliquer.

- **`Panel::visible`, jamais une zone d'accueil repliée** : `TabPanel::visible`
  rend faux quand aucun onglet n'est visible, et `StackPanel` l'honore.
- **L'état vit dans `ClaudhubApp::hidden_panels`, sa copie dans les réglages** :
  les panneaux observent l'application, et `Settings::update_global` ne notifie
  personne. C'est `Settings::hidden_panels` qui survit à la fermeture, et non
  `layout.json`, que `LAYOUT_VERSION` jette au premier renommage.
- **Chaque panneau met sa visibilité en cache** : `Panel::visible` est appelé
  pendant la construction de la disposition, donc au milieu de
  `ClaudhubApp::new`, où lire l'entité racine est une panique.
- **La liste est celle de l'écran courant** : masquer « Console SQL » depuis la
  revue ne ferait rien voir changer.
- **Elle est bâtie sur les constantes `Panel::NAME`**, pas sur des littéraux. Les
  conflits n'y sont pas — leur visibilité se décide toute seule.
- **Les lignes du sous-menu sont des `PopupMenuItem::element`** : `PopupMenu::
  confirm` referme le menu après chaque entrée, donc la ligne consomme son clic
  et l'entrée ne le voit jamais. Un `checked` aurait de toute façon menti, étant
  figé à la construction.

### Le balayage de fond

Le sélecteur de worktree dit, pour chacun, ce qu'il a en chantier (`+n −m`) et si
un agent y travaille — deux informations qui portent sur des worktrees **qu'on
n'a pas ouverts**. D'où un balayage périodique, dans sa **propre file**, qui ne
doit jamais passer devant un diff.

Deux périodes : les agents se lisent dans `/proc` sans lancer de processus,
toutes les deux secondes ; le résumé coûte **deux commandes git par worktree**
(`--numstat` ignore ce qu'il ne suit pas, `status` voit les fichiers nouveaux
sans savoir ce qu'ils contiennent), donc un relevé sur cinq.

**Le fetch automatique bat sur la même horloge**, en minutes
(`Settings::auto_fetch_minutes`, dix par défaut, zéro pour rien) : sans lui, « en
retard de trois commits » n'apparaît qu'après un fetch manuel.

- **Un horodatage, pas un compte de tics** (`last_auto_fetch`) : le balayage bat
  toutes les deux secondes et le réglage se donne en minutes.
- **Un dépôt et non un worktree** : les références distantes sont partagées.
- **`Cmd::AutoFetch` ne dit rien quand il aboutit** ; `Evt::Fetched` ne porte pas
  un résultat mais une **occasion** — relire le statut du worktree affiché.
- **File réseau**, comme le fetch manuel.

`agent::scan` est **Linux seulement**, par un `cfg` explicite : le parcours
compile partout et échouerait en silence à l'ouverture de `/proc`. C'est aussi ce
qui fixe la cible Windows à WSL2.

La détection passe par `/proc` et non par nos onglets : on lance un agent depuis
Claudhub, mais aussi depuis un terminal à côté. Le worktree le plus profond
l'emporte, faute de quoi un worktree imbriqué se verrait attribuer les agents de
son parent.

Le relevé ne dit pas qu'un agent travaille : c'est la **différence** entre deux
relevés. `agent::Tracker` retient le précédent, et il vit dans le cœur — c'est la
seule décision qui se teste. « En cours » veut dire **a consommé du processeur
depuis le relevé précédent** : approximation assumée, rien dans un processus ne
disant « je réfléchis ». Le seuil (`AGENT_BUSY_TICKS`) écarte le clignotement
d'un curseur.

`parse_cpu_ticks` repart de la **dernière parenthèse fermante** de
`/proc/<pid>/stat` : le nom du programme est le deuxième champ, entre
parenthèses, et il peut contenir des espaces et des parenthèses. Un test le
verrouille.

### Intégrer un worktree, et `wt.toml`

`git/repo.rs` sait fusionner, rebaser, abandonner et reprendre. **`--ours` et
`--theirs` s'inversent pendant un rebase**, git rejouant nos commits par-dessus
les leurs : `repo::resolve` traduit le drapeau, la vue parlant de « la nôtre » au
sens de l'utilisateur. **L'opération en cours vit dans `Status`** : elle se lit au
même moment et change la lecture de tout le reste ; `pending_in` est libre, seul
`git rev-parse --git-dir` coûte un fork — dans un worktree lié les marqueurs
vivent ailleurs.

**Intégrer s'exécute depuis le dépôt principal**, et le worker vérifie d'abord
qu'il est propre et positionné sur la base : la vue ne connaît l'état d'un
checkout que s'il a été ouvert. Une fois la fusion faite, Claudhub propose de
retirer le worktree et sa branche — `wt` conserve délibérément la branche.

Le panneau « Conflits » n'apparaît que quand il y a de quoi le remplir. Ses deux
boutons — garder la nôtre, garder la leur — tranchent un fichier d'un geste ;
cliquer son nom ouvre les trois volets, ci-dessous.

**`wt` est une dépendance, pas un sous-processus** : le dépôt est le nôtre, et
parser la sortie de sa CLI — alignée, colorée, traduite — reviendrait à lire ce
qui est fait pour un humain. Sa CLI reste derrière la caractéristique `cli`, que
Claudhub n'active pas — sans quoi il paierait ratatui, clap et skim pour créer un
dossier. Elle vient du **dépôt distant**, `branch = "main"` ; `Cargo.lock` fige
le commit.

Ce que cela donne : **le `wt.toml` d'un projet ajoute des actions à Claudhub sans
que Claudhub les connaisse** — `[tasks.*]`, `[[prompt]]`, `[status] up`,
`[open]`, `[lsp.<nom>]`.

- **Tout appel à `ops::App` part dans un worker**, sur la file des hooks.
- **Les questions se demandent en boucle** (`Cmd::WtQuestions` → `Evt` → …) : un
  `[[prompt]]` a un `when` qui peut dépendre d'une réponse précédente. L'absence
  de nouvelle question déclenche l'opération.
- **Les tâches partent dans un onglet de terminal**, pas dans un panneau de
  sortie : elles sont interactives, colorées, parfois longues. `wt::task` rend
  les commandes, le terminal les lance dans un `sh -lc`. Ce qui tient une
  comptabilité passe par la bibliothèque.

**`wt.Phase`** — `New`, `Up`, `Task` — décide quels `[[prompt]]` s'appliquent.
Longtemps Claudhub n'en connaissait qu'une, et cela se voyait sur les projets
dont les tenants se choisissent à **chaque** démarrage.

- **C'est le worker qui amorce les réponses**, au premier tour seulement, depuis
  ce que le worktree a retenu (`wt::saved_answers`) : c'est ce qui empêche un `wt
  up` de reposer ses questions. Les réponses **reviennent** avec les questions.
- **Un compteur de tour, pas une comparaison des réponses** : l'amorçage les rend
  différentes par construction. `round` ne recule jamais.
- **Les questions d'une tâche sont celles qu'elle nomme**, filtre « déjà répondu »
  compris. Leurs réponses deviennent les **arguments**, dans l'ordre de la tâche
  et non des réponses — un hook qui les reçoit dans le mauvais ordre agit sur
  autre chose sans le dire.
- **Deux drapeaux et non un** (`has_new_prompts`, `has_up_prompts`).

Le dialogue rend le `detail` d'une option — la phrase sur laquelle le choix se
fait. **Un choix simple s'affiche en lignes tant qu'il est court** (six options),
un menu déroulant cachant le détail derrière un clic qui valide déjà. **Un choix
multiple gagne un champ de recherche** passé huit options, les options venant
souvent d'une commande shell. **Le filtre masque des lignes, il ne touche jamais
à la réponse** — une recherche qui décoche en silence est la façon la plus sûre
de cloner les mauvaises bases.

Le relevé de `[status] up` et de `[open]` est une commande shell par worktree :
**file de fond uniquement**.

### Trancher un conflit à trois volets

Ce qu'un conflit coûte n'est pas de le corriger, c'est de le lire. Git laisse le
fichier avec ses marqueurs en travers, et un marqueur dit **ce qui n'est pas
d'accord** sans jamais dire ce que chaque côté a *fait*. D'où trois colonnes —
la nôtre, le résultat, la leur — et une décision qui est un clic dans la
gouttière entre deux colonnes — ou du texte tapé dans la colonne du milieu, là
où aucun côté ne convient. C'est le geste de PhpStorm, au centre de l'écran Git,
**à la place du diff** : le diff d'un fichier non fusionné ne montre que les
marqueurs.

La colonne du milieu **s'écrit** : ce qui suit dit comment, et pourquoi c'est
par bloc.

**« Smart » n'est pas un réglage, c'est ce que la comparaison rend.** Le fichier
est relu depuis l'index — `:1:`, `:2:`, `:3:` (`repo::stages`) — et comparé deux
fois, la base contre la nôtre et la base contre la leur. Ce que les deux
laissent tranquille est stable ; ce qu'**un seul** côté a touché est pris sans
rien demander ; ce que les deux ont touché différemment est un conflit, et c'est
la seule chose que la vue demande. Un fichier où git a écrit vingt marqueurs se
réduit à trois décisions.

- **La comparaison est celle de la gouttière** (`hunks::regions`, patience),
  d'où l'extraction du primitif : deux appels ici, un là-bas, un seul
  algorithme. Le savoir vit dans `ui::merge`, pur et testé.
- **Ce que rend l'algorithme est ce que rend git**, jusqu'aux découpages qui
  paraissent trop grossiers : deux modifications sur des lignes **voisines** ne
  sont séparées par aucune ligne dont les trois versions conviennent, donc elles
  ne font qu'un conflit — et git en écrit une seule paire de marqueurs. Une vue
  qui les séparerait proposerait une décision que git n'honorera pas.
- **Un côté pris est un bouton allumé, et deux boutons font « les deux »**,
  dans l'ordre où on les a pressés. C'est une bascule et non un choix exclusif :
  deux fonctions ajoutées au même endroit se gardent toutes les deux, et se
  dépresser est la façon de revenir en arrière.
- **Tant qu'un conflit n'a pas de réponse, la colonne du milieu ne montre
  rien** : y afficher l'un des deux côtés se lirait comme une décision déjà
  prise. Le fond rouge s'éteint dès qu'un côté est pris.
- **La colonne du milieu s'écrit, et elle s'écrit par bloc.** Deux boutons
  répondent à presque tout ; ce à quoi ils ne répondent pas est le cas où les
  deux côtés ont ajouté quelque chose à la **même** ligne, où aucune
  combinaison de « prendre ce côté » n'écrit ce que le fichier veut. Un
  `EditorState` s'ouvre alors **dans** le bloc — double-clic sur le résultat, ou
  le bouton de la barre — amorcé de ce que la colonne disait, et ce qu'on y tape
  est une réponse comme les autres : un conflit qui porte du texte est tranché.
  - **Par bloc et non un tampon sur tout le fichier**, et c'est ce qui rend la
    chose possible : trois colonnes alignées et un éditeur vivant sur toute la
    hauteur ne peuvent pas s'accorder ligne à ligne — replis, retours à la
    ligne, défilement propre. Un bloc, lui, n'a qu'une hauteur, et la vue la
    connaît.
  - **D'où `v_virtual_list` et non `uniform_list`** : le bloc en cours d'édition
    est **une** entrée, aussi haute qu'il le faut, quand les autres sont des
    lignes. Un éditeur est un élément, il ne se découpe pas sur les lignes qu'il
    couvre. C'est la même bascule que le repli du diff, pour la même raison.
  - **L'entrée est aussi haute que le plus grand des trois** — les deux côtés et
    ce qui est tapé : une hauteur trop courte fait peindre l'entrée suivante
    par-dessus les dernières lignes de l'éditeur. Elle grandit donc à la frappe,
    les lignes étant reconstruites à chaque changement (un parcours du fichier
    par frappe, l'ordre de la gouttière, très en dessous d'une frame).
  - **Les boutons et l'éditeur sont deux façons de répondre à la même
    question** : presser un côté **jette** ce qui a été tapé et referme
    l'éditeur. Deux réponses pour un bloc, dont une invisible, est l'état dans
    lequel on résout un fichier qu'on n'a pas lu.
  - **Vider l'éditeur, c'est ne rien prendre**, et non prendre une ligne vide :
    un document se découpe sur les sauts de ligne, donc `""` serait une ligne
    blanche dans le fichier. C'est ainsi qu'on laisse tomber un passage dont les
    deux versions sont à jeter.
  - **N'importe quel bloc s'écrit**, pas seulement un conflit : une fusion
    automatique avec laquelle on n'est pas d'accord est précisément ce qu'une
    colonne éditable sert à corriger. Une gomme la rend au calcul.
  - Et l'éditeur externe reste, pour ce qui se finit ailleurs : le disque porte
    encore les marqueurs, qui sont ce qu'on lit pour ça.
- **Ce qui reste à trancher est écrit**, dans la barre, en mots et pas seulement
  en couleur — « 3 conflits à trancher », puis « 5 conflits réglés » sur fond de
  succès quand il n'en reste aucun. Un fichier laissé à moitié est un fichier
  qu'on écrit sans l'avoir fini de lire, et « Résoudre » reste éteint tant qu'il
  en reste un.
- **On passe d'un conflit à l'autre**, et par les mêmes touches qu'ailleurs :
  la vue ayant pris la place du diff, `step_diff_hunk` — les flèches, `j`/`k`,
  `]c`/`[c` — y va de conflit en conflit, en **bouclant** (ce qu'on parcourt est
  une poignée d'endroits dans un fichier, et une flèche qui cesse de répondre au
  dernier se lit comme cassée). Deux boutons la doublent dans la barre, et un
  troisième va au prochain **non tranché** — « le premier encore ouvert » et non
  « le suivant vers le bas », la liste qu'on parcourt étant une liste qui
  raccourcit. Un filet dans la marge dit où l'on est.
- **Rien n'est écrit avant qu'on le demande** : « Résoudre » écrit le résultat
  et l'indexe d'un seul geste (`repo::resolve_with`), et le bouton est éteint
  tant qu'un conflit attend. L'écriture est inconditionnelle, seule de ce
  programme : ce qui est sur le disque est la version aux marqueurs, et
  l'écraser est tout le geste.
- **Les stages 2 et 3 s'inversent pendant un rebase**, comme `--ours` : la
  traduction se fait dans la couche git, une fois, et la vue parle de « la
  nôtre » au sens de l'utilisateur d'un bout à l'autre.
- **Ce qui n'a pas trois colonnes est refusé et le dit** : un fichier binaire —
  `git_blob` rend une erreur plutôt qu'une conversion approximative, dont les
  octets finiraient dans le fichier résolu — et un conflit où un côté a
  **supprimé** le fichier. Les deux boutons de la ligne restent la sortie.
- **Le blob se lit octet pour octet** (`git_blob`) : `git` rogne les sauts de
  ligne finaux, ce qui est juste pour la sortie d'une commande et faux pour un
  fichier — le saut mangé serait une modification que personne n'a faite, et il
  atterrirait dans le fichier résolu.
- **La vue s'ouvre sur le premier conflit**, pas en haut du fichier, et une
  flèche y ramène : c'est « le premier encore ouvert » et non « le suivant vers
  le bas », la liste qu'on parcourt étant une liste qui raccourcit.
- **Un conflit réglé ailleurs referme la vue** (`status_arrived`) : les deux
  boutons de la ligne, un abandon, un `git checkout` dans le terminal d'à côté —
  ce qui resterait à l'écran serait l'index tel qu'il était, et chaque bouton
  agirait sur un conflit terminé.
- **Pas de défilement horizontal** : trois colonnes se partagent une largeur,
  donc chacune est étroite et une ligne trop longue est coupée. Ce qu'on lit ici
  est quel côté prendre, pas la fin d'une ligne.

### L'exécuteur asynchrone

`runtime::executor` tient un runtime tokio multi-thread, démarré au premier
usage. Claudhub reste un programme à threads — un `fork` bloque de toute façon —
et cet exécuteur **s'ajoute** à côté, pour les bibliothèques sans interface
bloquante. La première est `sqlx` ; ce qu'il apporte est un **vrai délai** (un
futur qu'on laisse tomber s'annule).

- **Le pont est `block_on`, et il est à un seul endroit** : le worker qui traite
  la commande. C'est ce qui garde `runtime::handle` synchrone et pur.
- **Jamais depuis le thread d'interface.**
- **Deux threads, et non le nombre de cœurs** : ce qui tourne dessus attend une
  socket. C'est aussi ce qui borne la concurrence vers un serveur.

### Les bases de données

Deux surfaces : un **arbre** à gauche — connexion, base, table, colonne — et une
**console SQL** au centre. C'est l'explorateur de PhpStorm.

**Un seul pilote, `sqlx`**, pour SQLite comme pour MySQL et MariaDB, et le même
modèle pour un troisième moteur : une variante d'`Engine`, un module à côté, rien
à changer au protocole ni aux vues.

- **`NULL` n'est pas la chaîne « NULL »** : `db::Cell` est un `Option<String>`, et
  une colonne `TEXT` contient couramment le mot. Les confondre se paie trois fois
  — la grille, l'export CSV, la copie.
- **Un `DECIMAL` se décode sans vérification de type, et c'est le seul.** Les
  types numériques de `sqlx` vivent derrière `bigdecimal` et `rust_decimal`, que
  Claudhub n'active pas : sans elles la voie vérifiée refuse, et une table de
  prix s'affichait en `<?>`. La valeur voyage en **texte** dans les deux
  protocoles, donc `try_get_unchecked` est ici exact — et cela évite qu'un
  `DECIMAL(20,4)` passe par un `f64`.
- **Une connexion par requête, jamais gardée** : un `connect` coûte quelques
  millisecondes en local, et un panneau qui tient une connexion occupe un
  descripteur et découvre la coupure au pire moment.
- **Un délai qui annule vraiment** : `tokio::time::timeout` laisse tomber le
  futur et le pilote ferme la connexion. Le délai enveloppe le **geste entier**,
  une introspection enchaînant plusieurs requêtes.
- **SQLite est ouvert en lecture seule** : c'est le moteur qui refuse, ce qui
  vaut mieux qu'un filtre à nous sur le texte. Pour MySQL, la seule barrière qui
  tienne est celle du compte de connexion.

**Les connexions se déclarent dans les réglages** (`Settings::databases`), pas
dans le magasin : une connexion n'appartient pas à un dépôt. Trois choses sur ce
formulaire, chacune venant d'un essai raté : la table est un `SettingItem::render`
et non un `new`, seule de la fenêtre — un item ordinaire donne quatre cents
pixels au champ, taillés pour une case à cocher ; **`min_w_0` sur chaque champ
élastique**, sans quoi une ligne étroite pousse ses voisins dehors ; et le moteur
se choisit par **deux boutons, plein contre contour**, l'état « sélectionné » de
deux boutons de même variante ne se lisant pas.

**Le mot de passe voyage dans la `Cmd`**, ce qui déroge à la règle des secrets, et
la dérogation est bornée : `db::Connection` a un `Debug` **écrit à la main** qui
le masque. Le faire relire au worker coûterait un identifiant à faire voyager et
— l'écriture étant différée — une connexion qu'on vient de saisir interrogée avec
ce qu'elle contenait avant.

L'arbre :

- **Chaque niveau se charge à son dépliage**, avec un `Load` à quatre états
  (`Idle`, `Loading`, `Ready`, `Failed`) : confondre « pas encore demandé » et
  « en route » relance la commande à chaque frame.
- **L'échec vit dans l'événement, pas dans la barre d'état** : les `Evt::Db*`
  portent un `DbResult`, si bien qu'une erreur s'affiche **sous le nœud qui l'a
  demandée**.
- **Le filtre indexe ce qui est déjà ouvert, jamais ce qui ne l'est pas** : taper
  trois lettres ne doit pas ouvrir une connexion vers une production. « Tout
  indexer » est explicite et **ne retente jamais** ce qui a échoué.
- **Les colonnes d'une base se lisent d'un coup** (`Cmd::DbAllColumns`) : une
  commande par table ferait trois cents connexions sur un schéma Laravel.
- **Une entrée ne porte que des indices**, comme `ui::tree`. L'arbre a son propre
  contexte clavier (`ClaudhubDb`).

**Les bases d'un worktree.** Un projet qui **clone ses bases par worktree** voyait
les quatre-vingts du dépôt principal mélangées aux trois de la branche relue. Un
**motif déclaré sur la connexion** (`db::Connection::scope`, `wt_{slug}_*`) les
sépare ; `db::scope` est pur et testé. Quatre règles :

- **Un motif dont une variable ne se résout pas est écarté** : le checkout
  principal n'a pas de slug, et le résoudre en chaîne vide donnerait `wt__*`.
  Variables : `{worktree}`, `{slug}` (absent sur le principal — c'est ce qui rend
  le motif inerte là-bas), `{branch}`.
- **Aucun motif applicable montre tout** : un scope qui ne sait pas décider ne
  doit jamais être la raison qu'une base disparaisse.
- **Rien n'est masqué en silence** : la barre porte le compte des bases écartées
  et la bascule, **qui n'apparaît que si une connexion déclare un motif**.
- **L'indexation suit le scope**, et la recherche non plus n'y va pas.

Corollaires : le filtre s'applique à l'**affichage**, les entrées ne portant que
des indices ; et le motif n'entre **pas** dans `Connection::key`, qui dit quelle
connexion c'est et non ce qu'on en montre.

### La console SQL

**Elle est le centre de l'écran des bases** (`ConsolePanel`), et **une seule à la
fois** : la place centrale est unique, et deux consoles demanderaient une barre
d'onglets à nous.

**Une fenêtre sur le résultat, et non « la page *n* »** : elle commence à
`offset`, compte `shown` lignes, et **grandit** quand le défilement atteint le bas
(`load_more`). Prolonger n'appelle donc **pas** `refresh`, qui remettrait le
défilement en haut.

- **La pagination se fait en lisant, pas en réécrivant la requête** : ajouter un
  `LIMIT` demanderait de comprendre ce que l'utilisateur a écrit, ce qui est le
  plus sûr moyen de lui faire exécuter autre chose que ce qu'il lit.
- **Le tri est fait par le moteur, jamais sur la page** — trier en mémoire
  mentirait dès la deuxième page. `db::order_by` **enveloppe** la requête
  (`SELECT * FROM (…) AS claudhub_result ORDER BY 3 DESC`) et ordonne par le
  **rang** : un rang ne se cite pas, et une colonne calculée s'appelle
  `count(*)`. La parenthèse fermante est sur sa propre ligne, hors de portée d'un
  `--` terminant la requête. Ce qu'on ne sait pas envelopper n'est **pas triable
  du tout** (`db::can_order`) plutôt que trié faux.
- **L'enchaînement du tri est le nôtre**, pas celui de gpui-component, qui part
  du décroissant et vit dans un état qu'un `refresh` reconstruit. La flèche suit
  le geste et non la réponse.
- **Un identifiant d'envoi, et non la requête, écarte le résultat en retard** :
  changer de page, trier et prolonger rejouent le **même texte**.
- **La sélection de cellules est la nôtre** : celle de gpui-component n'en connaît
  qu'une à la fois, or ce qu'on copie est presque toujours une colonne ou un
  bloc. `Results::selection` garde une **ancre et un curseur**. Deux corollaires
  : la colonne est déclarée sans rembourrage (`Column::p_0`) pour que ce soit
  **notre** élément qui remplisse la cellule, et un clic **prend le focus**.
- **Une clé étrangère se suit jusqu'à sa ligne** : la cellule est teintée comme
  la colonne dans l'arbre, et le clic avec la touche système y va. Quatre points,
  tous dans `db::link`, pur et testé :
  - **Le moteur ne dit pas de quelle table vient une colonne de résultat** —
    `sqlx` ne garde que le nom, le rang et le type. La provenance se lit dans la
    **requête**, croisée avec les clés étrangères du schéma indexé. Quand deux
    tables portent la même colonne vers deux cibles, **rien n'est offert**.
  - **Le balayage n'est pas un parseur**, mais il saute les chaînes, les
    identifiants cités et les commentaires.
  - **Un nombre part nu, le reste est cité** : SQLite compare un `INTEGER` à
    `'42'` par type et ne trouve rien. L'apostrophe est doublée, et l'antislash
    aussi sur MySQL.
  - **Les liens sont calculés à l'arrivée des lignes**, jamais au rendu, et
    recalculés quand l'index du schéma arrive — un recalcul, pas un `refresh`.
- **La requête qu'un geste remplace est sur la piste** : ouvrir une table et
  suivre une clé étrangère écrivent un pas, et `Alt+←` — ou le bouton 4 de la
  souris — rejoue celle d'avant. Voir « Aller quelque part, et revenir ».
- **Le presse-papiers prend des tabulations, le fichier des virgules** : un
  presse-papiers se **colle**, un fichier s'**ouvre**. Une cellule seule sort
  telle quelle. Un clic droit **hors** de la sélection la remplace, dedans il la
  garde.
- **L'export rejoue la requête et écrit au fil de l'eau**, tri en vigueur
  compris : exporter l'affiché n'exporterait qu'une fenêtre, et tout charger
  ferait tenir un million de lignes dans le tas. Délai à lui
  (`db::EXPORT_TIMEOUT`).
- **La durée est mesurée dans le worker** : depuis la vue, elle comprendrait
  l'attente dans la file.
- **L'éditeur et la grille se partagent la hauteur, et le partage se règle.**
- **L'éditeur est une surface de code comme celle des fichiers** (`ui::surface`)
  : mêmes modes vim, même curseur bloc, même molette lissée, même zoom. Et
  **police, taille et hauteur de ligne se disent explicitement**, comme partout
  ailleurs : sans elles la console héritait de la police proportionnelle de
  l'interface et du `line_height(Rems(1.25))` d'`Input`, sourd à la taille du
  texte — zoomé, les glyphes grandissaient et les lignes non.
- **La table est une entité créée une fois** : la reconstruire perdrait les
  largeurs réglées à la souris. Les largeurs de départ viennent des cinquante
  premières lignes et ne sont **pas** recalculées quand la fenêtre grandit. Sa
  molette passe par `smoothed` et non `scrolled`, la table peignant ses propres
  barres — et `smoothed` la lui prend **en capture**, son masque consommant
  l'événement avant qu'un ancêtre ne le voie (voir « Le lissage de la molette »).
- **Tout geste de la table repart vers l'application en différé**
  (`Results::report`, référence **faible**) : la table appelle son délégué au
  milieu d'un `update` sur elle-même, et l'application répond en le remplaçant.
- **Le fournisseur de complétions filtre et classe lui-même** : la liste de
  gpui-component affiche ce qu'on lui rend, dans l'ordre où on le rend, sans rien
  écarter. Le remplacement est donné en clair (`text_edit`), la plage de repli
  englobant le `users.` d'une colonne qualifiée. Voir « Ce que la console
  propose ».
- **`Ctrl+Entrée` est la touche de la requête**, et la console déclare son
  contexte (`ClaudhubQuery`) que la liaison du commit exclut explicitement.
  `COPY_PREDICATE` exclut la console, et `QUERY_COPY_PREDICATE` les champs.

**Aucun cache de schéma sur le disque** : le magasin fait un kilo-octet et se lit
à la main, et un schéma indexé pèserait mille fois plus.

### Ce que la console propose

`db::complete`, pur et testé, décide ; `SqlCompletions` traduit en
`CompletionItem` et ne décide de rien. Le savoir qui le nourrit est celui des
liens de la grille, monté d'un cran : **`db::sql` lit la requête** — les tables
qu'elle nomme après `FROM` et `JOIN`, l'alias qu'elle leur donne, et ce que le
curseur attend là où il est. Un balayage grossier et non un analyseur, comme
`link` l'assumait déjà : chaînes, identifiants cités et commentaires écartés,
une sous-requête ne nomme rien.

- **La requête est lue, pas seulement le mot qu'on tape.** Un schéma Laravel de
  trois cents tables porte des milliers de noms de colonnes, et une liste coupée
  à cinquante **avant** d'être classée est la façon dont la colonne qu'on
  cherchait n'apparaît jamais. Les colonnes des tables que la requête nomme
  passent donc devant, et la coupe vient après le classement.
- **Classer, pas restreindre** : une requête à moitié tapée n'a pas encore de
  `FROM`, et écarter ce qui n'appartient pas à la clause retirerait ce vers quoi
  on va. Seul un qualifieur explicite restreint — `u.` ne propose que les
  colonnes de cette table-là, et **rien du tout** s'il ne nomme rien plutôt que
  tout le schéma sous un nom qu'il n'a pas.
- **Un alias répond là où le nom de table répond** : on écrit `u.` bien plus
  souvent que `users.`. C'est l'autre moitié de `sources_in`, dont `link`
  profite au passage.
- **Le mot qu'on est en train de taper n'est pas un alias**, et c'est le piège
  qui ne se voit pas : dans `FROM users j`, le `j` d'un `JOIN` qu'on commence
  nommerait la table `j`, et toute condition proposée depuis là serait écrite
  contre un nom qui ne survivra pas à la frappe suivante. L'identifiant entier
  est retiré du texte que le balayage lit, la moitié après le curseur comprise.
- **Un nom que deux tables de la requête portent sort qualifié** (`u.id`) : un
  `id` nu dans une jointure est ce que le moteur refuse comme ambigu, donc le
  compléter tel quel écrirait une requête qui ne tourne pas. Le libellé reste le
  nom nu — c'est lui que le préfixe filtre — et la mention grise dit d'où il
  vient.
- **La jointure s'écrit toute seule à partir d'une clé étrangère.**
  `SchemaIndex::foreign_keys` est déjà indexé pour les liens de la grille, et
  c'est le geste qui vaut le détour. **Trois formes, une par endroit où le
  curseur peut être**, et chacune commence par le mot qu'on y taperait, si bien
  que le préfixe les filtre comme le reste : `ON …` après un `JOIN` dont la
  table est nommée, `posts ON …` après un `JOIN` nu, et `JOIN posts ON …`
  ailleurs. La table jointe est écrite à gauche de la condition, quel que soit
  le côté qui porte la clé : c'est celle qu'on ajoute, et l'œil lit la condition
  depuis elle.
- **Un préfixe passe toujours devant une sous-séquence** (`crat` trouvant
  `created_at`), et **une seule lettre ne cherche que des préfixes** : toute
  chaîne est une sous-séquence d'une lettre, et la liste ne dirait plus rien. Le
  tri est stable, donc l'ordre du schéma survit à l'intérieur d'un rang.
- **La comparaison est en octets, insensible à la casse ASCII**, jamais par
  `to_lowercase()` : il change la longueur en octets de certains caractères,
  et c'est ainsi qu'un décalage tombe au milieu d'un caractère.
- **Aucun `filter_text` n'est renvoyé** : le menu du fork souligne sa
  **longueur** depuis le début du libellé, si bien que le remplir — ce que LSP
  demande — souligne chaque entrée en entier. Laissé vide, le souligné vaut la
  longueur du mot tapé, qui est ce qu'un match par préfixe revendique.

Ce qui n'y est pas : la complétion d'une **valeur** (`WHERE status = '…'`), qui
demanderait un aller-retour vers le serveur à chaque frappe.

**L'historique des requêtes** vit dans un fichier à lui
(`<config>/sql_history.json`) : le magasin se réécrit en entier toutes les
demi-secondes pendant qu'on tape. Il est sérialisé dans le thread d'interface et
**écrit en fond**. C'est le **tiers du bas de la colonne**, comme les notes sur
l'écran de revue et pour la même raison : il ne dit pas ce qu'il y a à
interroger, il dit où l'on en est — ce qui a déjà été demandé et ce que ça a
répondu —, et cela se lit **pendant** qu'on écrit la requête suivante. Derrière
un onglet, il était à un clic du schéma, si bien qu'on n'y allait qu'après avoir
retapé ce qui s'y trouvait déjà. Une rangée et non une seconde colonne : la
console garde toute la largeur qu'elle avait, ce qui était la raison de
l'onglet.

- **La même requête rejouée ne fait pas une seconde ligne** : la ligne remonte,
  compte un passage (`×4`) et **prend le résultat frais**. Les blancs sont
  normalisés.
- **Seul ce qu'un geste a demandé est classé** (`QueryState::record`) : changer de
  page, trier et prolonger rejouent le même texte.
- **Une requête qui échoue est classée aussi**, avec la première ligne du message.
- **Le plafond est par worktree** (`PER_WORKTREE`).
- **Une entrée nomme sa connexion, elle ne la décrit pas** (`Connection::key`,
  donc sans mot de passe) ; une connexion supprimée laisse la ligne en le disant.
- **Un clic charge, un double-clic exécute**, et le focus part à l'éditeur.
- **Vider n'oublie que ce qui est affiché.**

La liste est virtualisée par `v_virtual_list` : ses lignes n'ont pas la même
hauteur, et les en-têtes de jour sont **dans** la liste. Le groupement, la portée,
la déduplication et la recherche sont dans `ui::sql_history`, pur et testé.

### Un dépôt qui n'est plus là, et quel worktree s'ouvre

Un dépôt mémorisé qui ne s'ouvre plus reste affiché, en bas du sélecteur, en
erreur — avec ce que git a répondu et de quoi le retirer.

- **`Evt::RepoUnavailable` et non un `Failed`** : ce n'est pas une opération qui a
  échoué mais un dépôt qui manque. **Mémorisé**, il doit rester visible ;
  **désigné à l'instant**, il n'a qu'à se dire dans la barre d'état — en garder
  une trace ferait une relique d'une faute de frappe.
- **Une liste à part** (`ClaudhubApp::unavailable`) et non un drapeau sur
  `RepoState` : tout ce qui parcourt `repos` suppose un dépôt qui existe.
- **Un bouton, pas une entrée de menu** : c'est la seule chose qu'on puisse faire
  d'une ligne pareille. Sur un dépôt ouvert, le même geste est la dernière entrée
  du menu du worktree, derrière un séparateur.
- **Le magasin d'état n'est pas purgé** : ses notes attendent le jour où on le
  rouvre.

`runtime::open_repo` retient le checkout d'où l'ouverture vient, et non le premier
de la liste. Mais `opened_at` ne dit pas à lui seul « lancé ici » : trois
candidats se départagent par un **rang** (`session::pick_worktree`, libre et
testé) — le checkout qui contient le dossier de lancement, puis celui de la
session précédente, puis le premier. Le rang rend l'ordre des réponses sans
importance. Corollaire : la sélection a lieu **même quand le dépôt était déjà
ouvert**, cette réponse étant la seule qui nomme le checkout profond.

**Où l'on en était** (`store::Place`, écrit par `ui::session`) : l'écran, les
onglets de fichiers, la connexion de la console et le texte de sa requête. **On
remet ce qu'on avait choisi, jamais ce qu'on avait obtenu** — pas de diff, pas de
grille, un `SELECT` rejoué à l'arrivée étant une requête vers un serveur que
personne n'a demandé à joindre.

**Et c'est par worktree**, ce qui est l'autre moitié de la même idée : aller
voir une chose dans un autre projet et revenir coûtait le fichier, l'écran et la
requête — quatre gestes pour retrouver un état que personne n'avait quitté
exprès. `store::Session` ne garde plus que **quel** worktree, ce qui est
précisément ce qui n'appartient à aucun d'eux. `layout.json` garde la place des
panneaux — c'est la géométrie de la *fenêtre*, la même sur tous les projets — et
l'écran sur lequel elle s'ouvre, qui est ce qui répond avant qu'aucun dépôt ne
l'ait fait.

Trois moments, et ils ne sont pas les mêmes (`settle_place`) :

- **L'écran et la console** se remettent à chaque arrivée : il y en a un
  exemplaire pour toute la fenêtre, donc le worktree qu'on quitte vient d'écrire
  par-dessus. La console est **vidée** avant d'être remise — laisser la requête
  du projet précédent sous le curseur est la seule chose qu'une place par
  worktree ne doit pas faire.
- **Les onglets** ne se remettent qu'à la **première** arrivée : le panneau d'un
  fichier se masque tout seul quand la racine d'édition change et revient avec
  elle — la réponse des terminaux —, si bien qu'un worktree déjà visité a encore
  ses fichiers, là où on les a laissés.
- **Rien du tout tant que la sélection est un bouche-trou** : le temps que les
  dépôts répondent, la fenêtre montre le premier checkout de celui qui a répondu
  en premier, et y rejouer une place ouvrirait des fichiers dans un projet que la
  session n'a jamais demandé. C'est `session_terminal_due`, la règle que le
  premier terminal suit déjà — d'où les **deux** points d'appel : le geste, et le
  moment où le dernier dépôt répond.

- **La connexion est nommée par sa clé**, pas recopiée.
- **Rouvrir n'est pas un geste** : l'écran qui revient est celui de la place,
  posé un cran plus tôt, d'où `take_restored_editing` et `reopen_db_console`
  (`start_db_console` sans ses deux effets de bord).
- **L'écriture se fait aux gestes, pas à la fermeture** : une fenêtre se ferme
  aussi par un plantage. Changer d'écran en est un.
- **Rien ne s'écrit pendant qu'une place se remet** (`settling`), ni tant que les
  fichiers sont en route (`restoring_files`) : les trois moitiés se remplacent
  l'une après l'autre — `enter_workspace` écrit, et c'est la première —, et une
  frappe dans l'éditeur de requête atterrit bien avant le dernier fichier. Une
  liste d'onglets vide écrite là les perdrait tous.
- **Seul un écran de travail est retenu** : les réglages et le multiplexeur sont
  des détours, et revenir sur un projet ne doit pas remettre dans le détour qu'on
  a pris en le quittant. C'est la règle de `worked_in`. Un écran devenu vide —
  le seul plugin qu'il portait a été désinstallé — n'est pas non plus un écran où
  revenir.
- **La place de la fenêtre entière devient celle de son worktree** : `state.json`
  écrit avant ce changement porte ses trois clés au niveau de la session, que le
  `flatten` relit là, et `migrate_place` les verse une fois dans l'entrée — le
  chemin de `migrate_sentry` et de la reprise des notes. L'entrée créée ainsi ne
  porte pas de dépôt, ce que la purge lit comme « pas à moi d'oublier ».

**La base de la revue** vient de git — `origin/HEAD`, puis `init.defaultBranch`,
puis les noms usuels *qui existent vraiment* (`branch::default_base`) — jamais
d'un nom supposé, un `main` codé en dur produisant un `unknown revision` sur la
moitié des dépôts. Ce n'est qu'un point de départ : le choix est **propre au
worktree**. Choisir une base bascule sur la revue de branche, sans quoi le
sélecteur paraîtrait ne pas marcher. Le statut arrive avant les branches :
`Evt::Branches` repropage donc la base aux revues déjà ouvertes.

### La coloration syntaxique

C'est le *code* qu'on relit : les lignes sont colorées avec la grammaire du
fichier, pas avec la grammaire `diff`.

Un hunk n'étant pas un fichier, `highlight.rs` reconstruit les deux versions —
ancienne (contexte + supprimées), nouvelle (contexte + ajoutées) —, colore chacune
en un seul appel, puis redistribue ligne par ligne. Une ligne de contexte
appartient aux deux : la seconde passe **remplace** la première (`target.clear()`).

Deux invariants que gpui ne vérifie pas et dont la violation est silencieuse :
les plages doivent être **triées et disjointes** (`with_highlights` les convertit
en longueurs de runs consécutives, en les parcourant dans l'ordre donné), et les
décalages sont en **octets**. Les deux sont verrouillés par
`diff_view::tests::highlight_runs_stay_sorted_and_disjoint`.

**Un fragment reçoit d'abord de quoi être reconnu** (`highlight::prologue`). PHP
l'impose : sans `<?php`, sa grammaire lit *tout* le fragment comme du texte HTML.
Le prologue n'est ajouté que s'il manque — un fichier Blade dont le hunk porte du
HTML l'attend déjà.

**Une vue Blade est du HTML avant d'être du PHP** (`ui::blade`). Aucune grammaire
tree-sitter n'en est publiée : la grammaire colore ce qu'elle sait lire, puis
`blade::overlay` repasse dessus les directives, les échos, les commentaires et
les **balises de composant**. Trois conséquences : un `.blade.php` ne reçoit
**jamais** de prologue ; un rôle Blade se traduit en style par une **liste de
noms**, du plus juste au plus sûrement présent, nos thèmes ne définissant ni
`punctuation` ni `operator` ; `blade::tests::every_scope_resolves_to_a_colour` le
vérifie.

- **Un nom de composant pointé appartient à la surcouche** : la grammaire HTML lit
  `<x-layout.app>` comme la balise `x-layout` et l'attribut `.app`, et le nom se
  coupe en deux couleurs. `blade::component` repeint le nom entier, dans la
  couleur d'une balise.
- **Le corps d'un bloc `@php` est rendu à la grammaire** : `blade::mask_php`
  change `@php` en `<?` et `@endphp` en `?>`, **complétés d'espaces jusqu'au même
  nombre d'octets** — c'est ce qui rend le procédé sûr, chaque décalage désignant
  encore le même caractère. Deux gardes : `@php(…)` est une instruction et n'est
  pas masqué, et l'état de commentaire suit le masquage.
- **Ce qui est écrit dans une autre langue est rendu à sa grammaire**
  (`blade::Tint`) : l'argument d'une directive et le corps d'un écho sont du
  **PHP**, la valeur d'un attribut Alpine du **JavaScript**. Chaque fragment est
  analysé à part, avec ce qu'il faut devant : `<?php ` pour le PHP, rien pour du
  JavaScript sauf une paire de parenthèses quand la valeur commence par une
  accolade — `x-data="{ tab: 1 }"` est une *expression*, et un programme
  JavaScript qui commence par une accolade est un **bloc**.

Sept points sur ce dernier, les trois derniers étant ce qui le rend payable :

- **C'est tout ou rien, et les deux cas rendent l'essai sans risque.** Si la
  grammaire a dit quelque chose, ce qu'elle n'a pas nommé prend la couleur du
  texte ordinaire — laisser ces octets à la couleur de la valeur faisait revenir
  `prevEditId !==` en vert au milieu d'un JavaScript coloré. Si elle n'a **rien**
  dit, le fragment garde la couleur unique qu'il avait, ce qui est exactement ce
  qui se passait avant.
- **Un fragment PHP reçoit un point-virgule** : sans lui, une expression nue rend
  un nœud `ERROR` et la requête ne trouve rien.
- **La requête PHP livrée ne nomme pas un cas d'énumération**, réparé au-dessus
  d'elle (`highlight::PHP_CONSTANTS`) : elle ne nomme une constante que
  capitalisée, et `ActionColor::Success` ne correspondait à **rien**. Le nœud ne
  porte pas de noms de champs, d'où l'ancre.
- **`:name="…"` est lu comme du PHP, et c'est une convention** : sur une balise
  de composant c'est une propriété Blade, sur une balise ordinaire le `x-bind`
  d'Alpine, et les départager demanderait de savoir quelle balise est ouverte.
  Alpine a une orthographe qui le dit (`x-bind:class`). `wire:` n'est pas touché.
- **Une valeur peut tenir sur deux lignes** : c'est la seconde chose que
  `blade::State` reporte, à côté du commentaire ouvert.
- **Les grammaires sont construites une fois et gardées** : compiler les requêtes
  d'une grammaire coûte des dizaines de millisecondes.
- **Dans l'éditeur, on ne peint que ce qui est demandé, et on le retient** :
  `styles` est appelé pour chaque groupe de lignes visible à chaque frame, et
  peindre les douze cents fragments d'une vue coûte soixante-dix millisecondes.
  Seuls les quarante à l'écran sont analysés, chacun gardé, indexé par son début.
  Ce qui invalide le lot : une édition, que `refresh` signale, et un changement de
  thème, que **personne** ne signale — d'où un **témoin**, la couleur d'un nom
  redemandée à chaque appel.

**Ce qui commence par une arobase n'est pas toujours une directive.** La
**liaison d'événement** d'Alpine (`@click.prevent="…"`) a exactement la même forme
qu'`@if` : c'est le `=` qui les départage, rien d'autre. La peindre en directive
était faux deux fois — couleur de mot-clé au milieu d'une balise, et **effacement**
de la couleur d'attribut que la grammaire avait donnée. Et l'**échappement
`@{{ … }}`**, dont seule l'arobase est consommée.

**Et tout cela vaut aussi dans l'éditeur**, ce qui a demandé un second chemin : la
surcouche sert le diff, qui peint lui-même ; l'éditeur demande des plages stylées
à un coloriseur. La couture est `gpui_base::input::InputHighlighter`, qu'installe
`set_highlighter_factory`. Quatre points :

- **L'édition incrémentale est jetée et le document reparsé entier** : une édition
  décrit un changement par rapport au texte **masqué**, et les deux s'accordent
  jusqu'à la frappe qui achève un `@php` — quatre octets loin du curseur changent
  alors de sens. Au-delà de `blade::MAX_BYTES`, on laisse la grammaire seule.
- **Les plages rendues doivent couvrir la fenêtre demandée en entier**, triées et
  disjointes. `blade::merge` parcourt donc la **fenêtre** et non les plages de la
  grammaire ; parcourir celles-ci perdrait en silence une plage Blade tombée dans
  un trou. Les plages voisines de même style sont fusionnées.
- **Les replis sont à refaire** : la règle est celle de gpui-component mais la
  fonction est privée à son adaptateur. D'où `tree-sitter` en dépendance directe,
  à sa version — deux crates étrangères ne s'accorderaient sur aucun type.
- **`ensure_highlighter_factory` laisse le nôtre en place** : c'est le rendu de
  l'`Input` qui installe celui par défaut, et il ne le fait qu'à défaut.

**PHP n'est pas dans les grammaires que gpui-component embarque** :
`highlight::register_languages` le déclare dans le registre partagé au démarrage,
à appeler avant tout rendu. **Nix passe par le même chemin** — c'est avec quoi ce
dépôt se construit — et sa grammaire apporte ses injections bash. Ce qui n'est
**pas** enregistré est la requête `locals`, que le crate ne expose sous aucune
constante.

**L'injection HTML est recopiée chez nous** (`highlight::HTML_INJECTION`) :
`tree_sitter_php::INJECTIONS_QUERY` ne couvre que phpdoc et les heredocs, le HTML
vivant dans `queries/injections-text.scm`, qu'aucune constante n'expose. Tant
qu'on ne le passait pas, **toute vue arrivait grise, balises comprises** — une
injection qui ne trouve pas sa grammaire ne produit aucune erreur.

`SyntaxHighlighter::new` compile les requêtes — près de quarante millisecondes
pour JavaScript. Jamais dans un `render`, et **une seule instance pour les deux
passes**.

### Le terminal

`alacritty_terminal` fournit le parseur VTE, la grille, l'historique et le pty.
Claudhub écrit `keys::key_bytes` et `snapshot::capture`. Le rendu est du texte —
des `StyledText` — et non un canevas : gpui garde le façonnage, les ligatures et
les écritures complexes qu'un rendu cellule par cellule aurait fallu réécrire.

Le verrou de la grille est partagé avec la boucle d'E/S : **ne jamais dessiner
sous ce verrou**, d'où l'instantané.

**Mais une police à chasse fixe ne suffit pas à aligner les colonnes**, et c'est
le piège que ce rendu a payé : un caractère que la police n'a pas est dessiné par
celle sur laquelle le système se rabat, dont l'avance n'a aucune raison de faire
une cellule. Façonné au milieu d'un run, il pousse tout ce qui le suit — le
sablier de l'agent parcourt quatre dingbats que le sous-ensemble d'Iosevka laisse
dehors, et la ligne de statut entière se décalait gauche-droite à chaque image.

- **Chaque run est posé à sa colonne** (`line_boxes`), en absolu, et non mis bout
  à bout : un run ne peut plus décaler son voisin, et le curseur — déjà peint à
  `colonne × largeur` — retrouve le texte qu'il désigne.
- **Un caractère mesuré hors grille reçoit une case à lui** (`chunks`), large
  d'une cellule, qui rogne ce qui dépasse. `TextSystem::advance` résout le glyphe
  dans **cette** police et échoue quand elle ne l'a pas : cet échec *est* la
  question. La réponse est mise en cache — la grille visible est parcourue à
  chaque image —, et `sync_font` la vide.
- **Le fond appartient à la case et non au run** : un rectangle couvre alors
  exactement les cellules qu'il doit, quelle que soit la largeur du glyphe.
- **Un caractère large est un run à lui seul** (`snapshot::capture`), et c'est ce
  qui rend la largeur d'un run lisible dans son texte : partout ailleurs une
  colonne est un caractère, donc la vue découpe caractère par caractère sans
  table de largeurs. Ce qui échappe à la règle — des signes combinants — épingle
  le run **en un bloc** : son intérieur peut glisser, ce qui le suit non.
- **Le sous-ensemble des polices porte désormais les symboles et les dingbats**
  (`tools/subset_fonts.py`, `.ttf` régénérés) : l'épinglage tient la grille,
  avoir le glyphe est ce qui le rend joli. Iosevka les dessine tous à une
  cellule ; le prix est un mégaoctet et demi de plus.
- **Le retrait des blancs de fin est une coupe en octets sur un compte en
  cellules** : elle se ramène dans le run avant de compter, sinon un run tombé
  entièrement après la coupe se retire les octets du voisin — une espace
  inversée sur une ligne par ailleurs vide mettait la coupe à zéro, et le
  compte débordait par en dessous.

**La molette n'a pas le même sens selon l'écran.** Dans l'écran secondaire — un
agent, `less`, `vim` — il n'y a pas d'historique : elle se traduit en flèches,
trois lignes par cran. Ailleurs, elle déplace l'affichage.

**Les lignes de l'historique sont numérotées négativement** : le parcours commence
à `-display_offset`. Les ramener par un `max(0)` les écrasait toutes sur l'indice
0, où elles s'accumulaient — l'écran paraissait « s'effacer » à chaque cran.
`snapshot::viewport_line` fait la translation, cellules et curseur compris.

Les fractions de ligne sont **accumulées** (`take_lines`) : un pavé tactile en
envoie par dixièmes. La conversion se fait sur la hauteur d'une **cellule**, pas
sur celle du texte ambiant.

**Un programme peut demander la souris**, et il faut la lui donner
(`terminal::mouse`) : sans cela un agent qui écoute recevait des déplacements de
curseur au lieu d'un défilement. Quatre décisions :

- **Maj est la sortie de secours** : un programme qui prend la souris prend aussi
  la sélection.
- **Seul le bouton gauche part au programme** : le milieu colle la sélection
  primaire, le droit ouvre notre menu.
- **Un déplacement n'est rapporté qu'au changement de cellule** : un geste de la
  main traverse dix cellules en cent événements.
- **Le format d'origine abandonne au-delà de la 223e colonne** plutôt que d'y
  rogner : un clic rapporté sur la mauvaise cellule est pire que pas de clic.
  D'où SGR (`1006`).

**Les marqueurs de session de l'agent qui nous a lancés sont effacés au
démarrage** (`agent::disinherit_session`, premier geste de `main`) : sinon un
`claude` ouvert dans un onglet se croit la sous-session de celui d'à côté et
cesse d'enregistrer sa transcription. La liste est explicite et non un balayage
de `CLAUDE_CODE_*`, qui emporterait la configuration de l'utilisateur. Effacer
dans notre propre environnement, pas dans celui du pty.

**Le redimensionnement attend que la main s'arrête, et il est planchérisé** :
un glissement passe par toutes les largeurs intermédiaires, et comme un shell
redessine son invite *en place*, ses redessins s'empilent au lieu de se
remplacer. L'attente **repart à chaque changement** et non toutes les 150 ms —
voir « Le multiplexeur », où un glissement déplace une douzaine de ptys —, elle
est bornée par `MAX_DEFERRALS`, et ce qui se peint pendant ce temps est la
pastille `colonnes × lignes`. Le plancher (`grid_size`) sert la même cause :
sous vingt colonnes, la moindre invite fait déborder l'historique.

**Une ligne de terminal a une hauteur fixe, ne revient pas à la ligne, et ne se
laisse pas comprimer** : sans le premier point une ligne trop large est repliée
par gpui et la grille ne correspond plus. Le troisième (`flex_shrink_0`) n'est
pas un raffinement : la boîte est en `size_full`, donc dès que la grille a plus
de lignes qu'il n'en tient — tout un rétrécissement, le pty n'étant prévenu qu'à
l'arrêt de la main, et tout un démarrage, dont le 80×24 atterrit dans la place
qu'il trouve — flexbox reprend la hauteur sur **chaque** enfant. Les glyphes sont
alors peints écrasés, et cela se lit comme un terminal qui étirerait sa propre
image. Une ligne qui garde sa hauteur déborde simplement par le bas, ce que
`overflow_hidden` rogne : exactement ce que fait une fenêtre qu'on
redimensionne. La géométrie étant mesurée après la mise en page, la grille reste
trop large pendant une frame après chaque rétrécissement.

Le curseur est un rectangle translucide **posé par-dessus** la grille : l'inversion
demanderait de redessiner le glyphe à l'envers. Il ne clignote pas — cela
réveillerait l'interface deux fois par seconde et par onglet.

La sélection est un attribut de style de `Segment`, comme le gras : la fusion des
runs la prend alors en compte toute seule.

### Les raccourcis clavier

**Une seule table décrit chaque liaison** (`shortcuts::table!`), et c'est d'elle
que sortent `bind_keys` et la fenêtre d'aide : deux listes auraient divergé au
premier ajout, et une aide qui ment est pire qu'une absence d'aide. Un test
vérifie que chaque clé i18n existe dans les deux catalogues, un autre que chaque
touche se lit — `KeyBinding::new` **panique** sur ce qu'elle ne sait pas lire, et
`init` tourne au démarrage.

**Deux prédicats, et pas un seul.** Sous Linux, `secondary` **est** Ctrl : une
liaison sur `secondary-r` prend au shell sa recherche arrière. Ce qui s'écrit avec
la touche système et une **seule lettre** passe par `WINDOW_PREDICATE`, qui exclut
le terminal ; ce qui demande Maj ou une touche de fonction vaut partout
(`PREDICATE`). C'est la convention que les terminaux ont fixée.

**`Ctrl+Maj+F` appartient à la recherche projet**, et « tout le fichier » a
déménagé sur `Ctrl+Maj+X` — seule liaison qu'on ait déplacée, `Ctrl+Maj+F` étant
ce que PhpStorm, VS Code et Eclipse lient tous à « chercher dans les fichiers ».
Un test verrouille qu'aucune touche n'est déclarée deux fois sous le même
prédicat.

**Une seule lettre est prise quand même**, et l'exception dit la règle : `Ctrl+T`
masque les terminaux, et ce qu'il masque est le terminal dans lequel on tape. Il
avait `Ctrl+\`` comme sortie de secours ; cet accent grave **n'existe pas sur un
clavier AZERTY**.

**Les écrans s'atteignent par `Alt+1` à `Alt+7`, et pas par `Ctrl+Maj+1`** : gpui
*retire* le Maj des modificateurs quand la touche est un caractère sans casse, si
bien qu'un `secondary-shift-1` arrive comme `ctrl-&` selon la disposition et ne se
déclenche jamais, en silence. `Ctrl+1` à `Ctrl+9` désignent des **worktrees**,
dans l'ordre affiché.

**Tout se reconfigure**, page « Clavier ». Ce que `settings.json` garde n'est pas
la table mais ce qui s'en écarte (`Settings::shortcuts`, id → touches) : une
version qui ajoute un raccourci n'a besoin de rien, et un champ vidé
**désactive** la liaison — la ligne reste, si bien qu'on voit ce qu'on a éteint.

- **L'id d'une liaison est sa famille et ses touches par défaut** (`review:j`) :
  le défaut ne bouge jamais, donc l'id survit à la personnalisation. L'action ne
  conviendrait pas — la moitié en ont deux — et les touches seules non plus.
- **On ne remplace pas une liaison, on refait le trousseau** : le keymap de gpui
  n'accepte que des ajouts et c'est le dernier qui gagne.
- **D'où l'instantané** (`BaseKeymap`) : vider le keymap emporte aussi les
  liaisons de gpui-component, et rien de public ne les repose. `init` en garde une
  copie prise **avant** les nôtres, ce qui fixe l'ordre de `ui::run`.
- **Une touche que gpui lit n'est pas une touche qui existe** : `Keystroke::parse`
  accepte `ctrl-nonsense` sans broncher, et la liaison ne se déclenche jamais.
  `valid_keys` exige en plus un seul caractère ou un nom de `NAMED_KEYS`, écrit à
  la main parce que la liste de gpui est *négative* et privée.
- **On appuie sur la touche plutôt que de l'écrire** : la capture passe par un
  **intercepteur de frappes**, qui tourne *avant* le keymap et où
  `stop_propagation` arrête la distribution net — une zone focalisable aurait
  demandé un contexte exclu de **chacun** des huit prédicats. `stroke_syntax`
  écrit la frappe comme la table l'écrirait, et un test verrouille l'aller-retour.
- **Ce qui ne se lit pas n'est pas écrit** ; le garde-fou est doublé côté
  installation, qui saute et journalise plutôt que de paniquer au démarrage.
- **Deux touches identiques sous le même prédicat sont signalées.**

L'aide (`shortcuts::sheet`) montre les touches **en vigueur** et **réunit sur une
ligne** les plusieurs façons de faire un même geste : c'est le geste qu'on
cherche, pas la touche.

### Le mode vim

Désactivé par défaut, et il faut que ça le reste : ses liaisons sont des
**lettres nues**. Un seul réglage (`Settings::vim_mode`) allume deux mécanismes
qui n'ont rien en commun.

**Hors de l'éditeur, ce sont des liaisons**, et une liaison ne connaît pas de
mode : `j`/`k` d'un **bloc modifié** au suivant, `h`/`l` d'un fichier à l'autre,
`gg`/`G`, `Ctrl+D`/`Ctrl+U`, `y`, `/` puis `n`/`N`. La touche système descend
d'une ligne. `]c`/`[c` restent à côté.

**Le réglage n'ajoute pas de liaisons, il allume un contexte** : `bind_keys`
s'appelle une fois au démarrage, les liaisons vim sont posées
inconditionnellement sous un prédicat qui exige `ClaudhubVim`, et la vue racine
déclare ce contexte. Corollaire qui ne se devine pas : **`ClaudhubVim` doit être
déclaré sur le même nœud que l'identifiant avec lequel il se combine** —
`KeyBindingContextPredicate::depth_of` évalue chaque identifiant contre un seul
niveau de la pile, si bien que `ClaudhubExplorer && ClaudhubVim` ne se rencontre
jamais quand l'un est sur l'arbre et l'autre sur la racine.

**Dans l'éditeur, ce sont des modes** (`ui::vim`), et la machine **ne connaît
aucun type de gpui** : on lui donne le texte et le curseur, elle rend l'édition à
appliquer. Une quarantaine de tests décrivent ce que fait chaque touche.

C'est un **sous-ensemble exprès** : ce qu'une main tape sans y penser. Ce qui n'y
est pas est ce qu'un éditeur fait déjà mieux (registres nommés, macros, marques)
ou ce que Claudhub a ailleurs.

**Et « l'éditeur » est deux panneaux** : le fichier qu'on retouche et la console
SQL. Les deux étaient bâtis sur le même `EditorState` depuis toujours — c'est de
là que viennent la coloration, les numéros de ligne et les complétions —, mais
tout ce qui l'entoure était **soudé à `Editing`**, l'état d'un fichier ouvert, si
bien que la console n'avait ni modes, ni curseur bloc, ni molette lissée, ni
zoom. `ui::surface` est ce harnais monté d'un cran.

- **Une surface se nomme, elle ne se possède pas** (`Surface::File(chemin)` /
  `Surface::Query`) : l'état reste où il vivait — celui d'un fichier dans son
  `Editing`, celui de la console dans `ClaudhubApp` —, et chaque méthode prend le
  nom et va le chercher. Le tenir derrière une entité commune aurait demandé de
  sortir l'éditeur d'un fichier de l'onglet qui le porte.
- **Et un fichier se nomme par son chemin, pas par « l'onglet actif »** : le
  dock en montre deux à la fois dès qu'on ouvre un split, `show_file` tourne une
  fois par panneau et par frame, si bien que l'onglet actif **alterne** entre
  les deux sans qu'on ait touché à rien. Un cran de molette atterrissait alors
  sur celui qui avait été peint en dernier, et le lissage — rangé sous une clé
  unique — était avancé depuis les rendus des **deux** panneaux : une seule
  motion poussait deux éditeurs, ce qui se lit comme deux fichiers qui défilent
  ensemble. D'où `editing_at`, ce qu'un **geste** atteint, là où `editing()`
  reste ce que le reste de la fenêtre appelle « le fichier qu'on édite ».
- **Le harnais est un `VimHost`** : la machine vim et les trois couches de
  décoration, créées **une fois** avec la surface et dans l'ordre qui décide qui
  gagne là où elles se recouvrent.
- **Une clé de lissage par surface et non une pour toutes** — le fichier la tire
  de son chemin : elles défilent indépendamment, et une motion partagée donne à
  l'une la destination de l'autre.
- **La console n'a pas les commandes qui nomment un fichier.** `:w`, `:q` et `gd`
  sont celles de l'éditeur — une requête n'a ni chemin où écrire, ni onglet à
  fermer, ni définition à suivre. Elles sont **laissées tomber** plutôt que
  pliées sur ce qui leur ressemblerait : `:w` qui lancerait la requête est un
  geste réseau que personne n'a demandé.
- **Elle déclare `ClaudhubEditorVim` et pas `ClaudhubEditor`** : de ce second nom
  on ne veut que ce qu'il achète, `Ctrl+R` restant le rétablissement au lieu de
  devenir le rafraîchissement.

**Le mode bloc** (`Ctrl+V`) est une **colonne**, seul mode dont la sélection n'est
pas un morceau de texte.

- **`I`, `A` et `c` se répètent sur toutes les lignes du bloc**, et c'est le geste
  qui vaut le mode. On tape sur la ligne du haut, et c'est l'`Esc` qui écrit sur
  les autres. `A` **complète d'espaces** une ligne trop courte, `I` la **saute**.
- **Les lignes visées sont retenues par leur numéro, pas par leur position** : ce
  qu'on tape en haut décale ce qui est en dessous. La répétition **abandonne**
  sur un saut de ligne, un clic ailleurs, une correction en arrière.
- **Un rectangle, ce sont plusieurs coupes et une seule édition** (`splice`) —
  donc **une** transaction, ce qu'un `u` doit reprendre d'un coup.
- **Le registre n'a pas de largeur** : un `p` remet le bloc **d'un seul tenant**.
- **La sélection est peinte par nous** (une troisième
  `TextDecorationCollection`) : celle de l'éditeur est un **seul** morceau de
  texte et allumerait les colonnes que le bloc laisse de côté.
- **`Ctrl+V` n'arrive jamais comme une frappe** : l'`Input` le lie à `Paste`, et
  une liaison passe **avant** l'écouteur en phase de capture. C'est donc
  l'**action** qu'on attrape, en capture sur le même ancêtre ; en mode insertion
  elle passe. `Ctrl+Q` fait la même chose sans détour.
- **La colonne désirée donne sa largeur au bloc**, `$` compris, d'où la remise à
  zéro en entrant dans un mode visuel.

**Les objets de texte** valent après un opérateur et en mode visuel, jamais seuls
— `i` et `a` sont d'abord les deux façons d'entrer en insertion. **Un objet de
mot ne traverse pas un saut de ligne** ; **les paires se comptent par
imbrication** ; **les guillemets d'une ligne s'apparient dans l'ordre**, seule
lecture qui n'ait besoin de savoir ni ce qu'est une échappement ni ce qu'est un
commentaire.

- **L'écoute est en phase de capture, sur un ancêtre de l'éditeur**, et cette
  place *est* le mécanisme : un écouteur de touche s'exécute **après** les
  liaisons — ce qui laisse `Ctrl+S` à la fenêtre — mais **avant** que la
  plateforme ne livre le caractère au champ. Consommer l'événement empêche un `d`
  nu d'être tapé ; le laisser passer fait du mode insertion un éditeur ordinaire.
- **Le caractère lu est celui que la frappe a *produit*** (`key_char`), pas la
  touche : c'est ce qui met `$`, `^` et `0` là où il faut sur un AZERTY.
- **Une sélection faite à la souris est une sélection visuelle** (`Vim::adopt`).
  Rien n'annonce un glissement : la sélection de la souris et la nôtre arrivent
  par la même porte, `selected_range()`. Ce qui les distingue est ce que vim a
  **écrit en dernier**, relu depuis l'éditeur (`VimHost::selection_at`) — relu et
  non retenu du `Change`, l'éditeur ramenant une plage aux frontières de
  caractère. Sans cela ce qui était surligné et ce que l'opérateur suivant
  prenait étaient deux choses différentes : le mode restait normal, le curseur
  bloc se posait au **début** de la plage — le mode normal lit le caret de
  l'éditeur, et le caret d'une sélection est son début —, et `c`, `d` ou `y`
  agissaient sur le caractère qui était dessous. Quatre points :
  - **Toujours *charwise*** : la souris désigne un morceau de texte, et revenir
    dans `V` ou `Ctrl+V` couvrirait plus que ce qui est allumé.
  - **La tête est le bout qu'on tirait**, lu dans `EditorState::cursor` — la
    seule chose qui distingue une sélection grandie vers la droite d'une grandie
    vers la gauche, et ce dont `o`, `h` et `l` repartent. La tête de vim est
    **inclusive**, la plage de l'éditeur non.
  - **Une plage vide rend le mode normal** : un clic simple, et une sélection
    laissée à l'écran alors qu'elle n'y est plus.
  - **Le mode insertion garde la souris pour lui** : sélectionner un mot pour
    taper par-dessus est ce que fait tout éditeur.
  Le tout vit dans `sync_block_cursor`, qui tourne déjà à chaque frame pour la
  même raison — un clic déplace le caret sans nous le dire —, et le texte n'y
  est copié qu'une fois. Deux exceptions le traversent : `u` et `Ctrl+R`, qui
  sont **différés** par gpui et **remettent la sélection de la transaction
  défaite** — une sélection que vim n'a pas écrite et que la frame suivante
  prendrait pour une main, d'où `absorb_selection`. Et la console SQL l'a par la
  même porte que le reste du harnais.
- **Le curseur bloc est peint par nous.** Il a d'abord été la **sélection** du
  caractère sous le curseur, et c'était faux deux fois : une sélection ne s'écrit
  qu'à une frappe, donc un fichier qui vient de s'ouvrir n'avait aucun curseur ;
  et elle se peint dans le `selection` du thème, à quelques pour cent du fond. Une
  décoration est peinte **par-dessus** la sélection et dans nos couleurs. Le
  découpage se compte en **caractères**.
- **Le bloc se redemande à chaque frame, et se recalcule rarement** :
  `VimHost::cursor_at` retient le mode, la sélection, la tête et la longueur du
  texte pour lesquels il a été calculé, `value()` recopiant le fichier entier.
  La tête y est pour son propre compte : `o` échange les deux bouts d'une
  sélection visuelle sans changer la plage, et le bloc doit changer de bout
  avec elle.
- **Le curseur dit le mode par sa couleur** (`vim_mode_colour`), table partagée
  avec la pastille de la barre du fichier. Le glyphe prend celle des deux
  extrémités du thème qui **contraste** (`ink_on`).
- **Là où le bloc n'a rien à couvrir, le caret *est* le bloc**
  (`set_caret_block`, onzième commit du fork) : une ligne vide et la fin du
  fichier n'ont pas de caractère à peindre, et le trait clignotant qu'on y
  rendait disait le mode insertion sur une ligne qui n'y était pas. `cursor`
  répond une plage **vide** à ces endroits et `None` au seul mode insertion —
  c'est exactement la distinction qu'il fallait.
- **Un yank s'allume une fraction de seconde** — le seul geste de vim qui ne
  change rien à l'écran. Le module pur rend la plage (`Change::flash`), et pour un
  **yank seulement**. Teinte d'une occurrence de recherche, trois cents
  millisecondes, collection créée **après** celle du curseur pour que le bloc
  reste visible dessous, et minuteur **remplacé** et non empilé.
- **Le caret s'éteint hors du mode insertion** (`set_cursor_hidden`, septième
  commit du fork) : le style de l'éditeur est reconstruit à **chaque** rendu, donc
  un caret transparent ne survivrait pas à la frame suivante, et `disabled` grise
  le texte avec. Le clignotement est arrêté avec lui. La consigne est **relue à
  chaque rendu**.
- **`Ctrl+E` et `Ctrl+Y` bougent la page, pas le caret.** Conséquence :
  `Ctrl+Y` n'est plus le rétablissement, donc le redo n'a plus que `Ctrl+R`, que
  la fenêtre prenait pour un rafraîchissement. D'où `ClaudhubEditorVim`, un
  **second nom** de contexte et non `ClaudhubEditor && ClaudhubVim`.
- **Le préfixe `z` fait deux métiers, et aucun ne touche au texte** : `zz`/`zt`/
  `zb` placent la ligne **sans déplacer le caret** — c'est ce qui les distingue de
  `z.` et `z-`, qui seraient deux réponses là où `Response` n'en porte qu'une, et
  qui ne sont pas là. `zc`/`zo`/`za`/`zM`/`zR`/`zm`/`zr` replient : où commence un
  repli est la réponse de la grammaire, et tout ce qui se décide ici est
  **lesquels** fermer, le plus intérieur pour les trois premiers.
- **Un niveau de repli est une profondeur d'imbrication**, relue dans les plages
  que l'éditeur donne à plat (`ui::folds`, pur, testé — la seule partie qui se
  trompe en silence). Chaque changement de niveau **repart de tout ouvert**, la
  carte des replis ne sachant pas rouvrir par addition.
- **`j` et `k` enjambent un repli fermé** (`folds::step`) : un repli cache ce qui
  est **entre** ses deux lignes — celle qui le commence et celle qui le ferme
  restent à l'écran —, et un caret posé au milieu est un curseur que personne ne
  voit. Un repli coûte **un pas**, quelle que soit sa hauteur, et en sortir peut
  faire entrer dans celui qui le contient. Les replis fermés sont **redemandés à
  chaque frappe** : la gouttière en ferme aussi, `zc` n'est pas la seule voie. Les
  autres motions n'en tiennent pas compte, à dessein : `G`, `gg` et une recherche
  nomment la ligne qu'on veut.
- **`Enter` et `Backspace` sont des liaisons de l'éditeur, pas des frappes** :
  une liaison passe **avant** l'écouteur en phase de capture, exactement comme
  `Ctrl+V`, et l'action est donc attrapée sur le même ancêtre en capture
  (`vim_named_key`). Sans cela une ligne `:w` ou `/foo` se tapait et ne
  s'exécutait **jamais** — la seule panne où tout paraît marcher jusqu'au geste
  qui compte — et `Backspace` en mode normal **supprimait**, la seule frappe
  d'un éditeur modal qui ne doit rien détruire. En mode insertion les deux
  passent. Un `Enter` modifié est laissé à qui il appartient : `Ctrl+Entrée` est
  la touche de la requête.
- **La ligne de statut est au *pied* de l'éditeur**, comme dans vim, et c'est le
  seul endroit où le mode se dit désormais. Le placement n'est pas une révérence
  à l'habitude : une ligne `:` est une ligne qu'on **écrit**, et une ligne qu'on
  écrit en haut d'un panneau — au-dessus du fichier, à côté de son chemin et de
  ses boutons — ne se lit pas comme telle, elle se lit comme une étiquette. La
  pastille a donc quitté la barre du haut : deux endroits qui disent la même
  chose en font un de trop, et ce qui dit le mode là où l'œil est reste la
  **couleur du curseur bloc**. Le `pending` est à droite, où vim le met, et la
  ligne en cours de frappe **prend la place** du mode — tant qu'on écrit une
  commande, ce qu'on écrit est tout ce qu'il y a à dire.
- **`/` et `Ctrl+F` cherchent la même chose.** Deux gestes pour une question,
  et deux moteurs qui s'ignorent en font deux réponses : on tapait un motif dans
  la barre, `n` ne le connaissait pas ; on tapait `/motif`, rien ne s'allumait.
  Le motif est donc **partagé** — poussé dans vim avant chaque frappe
  (`Vim::set_search`, comme le registre et les replis), et reposé dans la
  session de l'éditeur après (`set_search_query`). Trois conséquences :
  - **`n` continue une recherche tapée dans la barre**, même refermée, et
    `Ctrl+F` s'ouvre sur ce que `/` cherchait.
  - **Les occurrences restent allumées** (`hlsearch`), et `Échap` les éteint —
    le `:nohlsearch` que toute configuration vim lie à cette touche. Une
    recherche qui ne fait que déplacer le caret oblige à presser `n` pour
    savoir s'il y avait autre chose, alors que la réponse est à l'écran depuis
    le début.
  - **Elles sont peintes par nous**, sur une quatrième couche de décoration, et
    non en ouvrant le panneau de recherche de l'éditeur : l'ouvrir **prend le
    focus**, et le focus est ce dont `n` et `N` ont besoin pour rester où ils
    sont. La couleur est celle d'une occurrence de `Ctrl+F` (`find::
    highlight_color`) et le calcul est le sien (`find::find_all`, en octets).
    Celle **sous le caret** est allumée plus vif, ce qui ne coûte rien — le
    caret est à nous, donc « l'occurrence courante » est une comparaison et non
    un état à tenir. La couche s'éteint tant que la barre est ouverte : deux
    couches de la même couleur sur les mêmes mots font une troisième couleur.
  Le parcours n'est refait qu'au changement de motif ou de texte, jamais à la
  frame.
- **`;` et `,` rejouent le dernier `f`, `F`, `t` ou `T`** : une recherche dans
  la ligne qui ne vaut qu'une fois n'est pas ce à quoi elle sert. Ce sont des
  **motions** et non des commandes — elles vivent dans `parse_motion`, donc `d;`
  et `3;` marchent sans rien de plus. Le piège est le `t` : le caret est déjà
  posé une case avant le caractère qu'on lui a dit de ne pas franchir, si bien
  que « jusqu'au prochain » est exactement là où il se trouve — un rejeu qui ne
  commencerait pas par enjamber ce caractère resterait sur place. `,` a le même
  piège à l'envers.
- **`.` rejoue la dernière modification**, et c'est ce qui rend un opérateur
  payant à taper une fois. Ce qui s'enregistre n'est pas une touche mais une
  **commande** : `3dw` en fait quatre, `cwfoo` et son `Esc` six, et le tampon se
  vide dès que ce qui s'y trouvait s'avère ne rien changer — une motion, un
  changement de mode, un défilement. Six points :
  - **Le texte tapé en insertion est *déduit*, pas enregistré** : à l'entrée on
    retient où l'insertion commence et la longueur du texte, à l'`Esc` on lit ce
    qu'il y a entre les deux — la paire de `BlockInsert`, lue comme elle. C'est
    la seule façon d'emporter ce que l'éditeur a ajouté de lui-même :
    l'auto-indentation, une complétion, une paire de délimiteurs fermée pour
    vous. Rejouer les lettres les redemanderait une seconde fois.
  - **Un rejeu est *une* édition** : les touches sont rejouées sur une **copie**
    du texte, et ce que la vue reçoit est la plus courte édition qui mène de
    l'un à l'autre (`minimal_edit`, préfixe et suffixe communs — le calcul de
    `lsp::sync`). Sans quoi un `.` serait autant de transactions que de frappes,
    et `u` autant de pressions.
  - **L'`Esc` du rejeu est un vrai `Esc`** et non un mode posé à la main : c'est
    lui qui recule le caret sur le dernier caractère tapé, et lui qui répète une
    insertion en bloc sur les autres lignes — `Ctrl+V j I` est trois touches et
    une commande, d'où le tampon qu'un mode visuel garde ouvert.
  - **Un `.` ne s'enregistre pas lui-même**, sans quoi il serait sa propre
    dernière modification.
  - **Rien ne s'invente** : une insertion qu'on a quittée à la souris ou
    rebroussée en arrière ne se dépose pas, et une modification faite sur une
    **sélection de souris** non plus — il n'existe aucune frappe qui décrive un
    glissement, et déposer `d` seul donnerait un rejeu qui attend une motion et
    ne fait rien. C'est la modification d'avant qui reste.
  - **En insertion, un point est un point** : `.` appartient au seul mode
    normal, et l'interception le sait — comme elle sait qu'après `d` ou `f`
    c'est le caractère attendu, et dans une ligne `/` un point.
- **`u` et `Ctrl+R` sont rendus à l'éditeur**, seul à savoir ce qu'était la
  dernière transaction (`Command::Undo`).
- **Le défilement est repris à la main quand la tête remonte** :
  `set_selected_range` défile vers la **fin** de ce qu'on lui donne.
- **Le registre est interne, sauf réglage** (`Settings::vim_clipboard`, éteint par
  défaut). Le module pur **dit** ce qu'il vient d'arracher (`Change::yank`) et
  **accepte** qu'on lui pose un registre. Le presse-papiers n'est lu qu'au moment
  d'un `p`, et le drapeau « lignes entières » se relit sur le saut de ligne final.
- **`w` a l'exception de vim** : `dw` sur le dernier mot d'une ligne s'arrête au
  bout de la ligne. C'est dans l'opérateur, pas dans la motion.

Le mode courant et ce qui est en train d'être tapé s'affichent dans la **barre du
fichier**, là où l'œil est déjà.

### Ce qui tient lieu de système d'extension

Quatre niveaux, du moins cher au plus cher :

1. **Le `wt.toml` du projet** — tâches, questions, statuts, URLs, serveurs de
   langage. Claudhub les affiche sans les connaître, et cela n'a coûté que la
   dépendance à `wt`. C'est le vrai système d'extension.
2. **Des commandes déclarées dans les réglages** — profils d'agent, message de
   commit, connexions aux bases, serveurs de langage, réglages et secrets d'un
   plugin. Pour ce qui n'est pas propre à un projet.
3. **Un panneau écrit en Rune** — voir « Les plugins ». Le niveau que les deux
   précédents ne couvrent pas : une **vue**, avec son état et ses gestes. Ouvert
   par un constat de compte plus que par une envie de généralité.
4. **Des extensions wasm, à la Zed — toujours écarté.** Un script rechargé à
   chaud fait ce que le point 3 demandait, sans WIT, sans chaîne d'outils croisée
   et sans deuxième format de paquet.

### Les plugins

Une vue de Claudhub, déshabillée, ne contient presque rien qui parle de son sujet.
Sentry : chercher du JSON derrière un jeton, retenir un réglage par dépôt,
peindre une liste maître/détail avec un extrait de code, composer un prompt et le
remettre à l'agent. Mille lignes de Rust pour cinq capacités génériques, celles
dont auraient besoin GitHub Issues, un tableau de CI, un flux de logs.

Un plugin est donc le partage que ce dépôt fait déjà six fois — `notes.rs` devant
`notes_view.rs`, `sql_history.rs` devant `sql_history_view.rs`, `inflight.rs`,
`vim.rs`, `motion.rs` — poussé d'un cran : **un script rend un arbre de vue en
données, et la vue le peint**. Ce sont des panneaux, dans le dock d'un écran
qu'il nomme, rechargés pendant que la fenêtre tourne.

**Trois étages :**

- `plugin/view.rs` et `plugin/manifest.rs` sont des données. Ni gpui, ni Rune.
- `plugin/caps.rs` est ce qu'un plugin peut faire au monde extérieur. Des données
  aussi, exécutées par un worker — qui est parfois un worker du serveur WSL.
  **C'est pourquoi il ne porte pas Rune** : le binaire headless doit exécuter les
  requêtes d'un plugin sans moteur de script dedans, et `just check-server` le
  prouve.
- `plugin/host.rs` est la machine Rune, derrière la feature `plugins` qu'`ui`
  allume. Le script tourne côté interface ; seules ses entrées-sorties traversent
  le fil.

**Le contrat du script tient en trois fonctions** :

```
pub async fn init(worktree)                 -> Result<état>
pub fn view(état, panneau)                  -> nœud
pub async fn update(état, action, charge)   -> Result<état>
```

`view` est **synchrone et pure** : c'est ce qui permet de l'appeler chaque fois
que l'état bouge, et c'est la règle de toute la fenêtre. L'arbre est rangé
derrière un `Rc`, et le panneau ne fait que le lire.

**Plusieurs panneaux pour un script.** Un plugin déclare autant de panneaux qu'il
veut (`[[panel]]` : `id`, `title`, `icon`, `place`), et le manifeste sans tableau
garde sa forme d'avant — un `title` de premier niveau vaut un panneau nommé
`main`. Sentry livré en a deux : la liste des erreurs à gauche, celle qu'on a
ouverte au centre. C'est le geste de tout le reste de la fenêtre, et un panneau
unique ne pouvait que les empiler.

- **Un script, un état, plusieurs lectures** : une `init`, un `update`, un jeu de
  réglages. Ce qui distingue les panneaux est le **second argument de `view`**,
  qui porte l'identifiant du manifeste — jamais le nom du dock, qui est notre
  comptabilité.
- **Tous sont repeints à chaque assiette** : peindre le visible seul laisserait
  l'autre montrer ce que l'état disait avant le geste.
- **`place` choisit entre deux groupes existants** — la colonne de listes ou la
  moitié large — et n'en crée pas un troisième : réserver une place coûterait à
  la revue sa largeur qu'un plugin soit installé ou non. Ce n'est que le point de
  **départ**, le dock laissant glisser un panneau où l'on veut.
- **Un écran sans colonne à nous en gagne une** si un plugin la demande, et
  seulement alors.
- **Le nom dans le dock est `ClaudhubPlugin:<plugin>/<panneau>`.**
- **Deux panneaux d'un même identifiant sont refusés** plutôt que rendus uniques :
  ils partageraient l'arbre, la poignée de défilement et les champs, et rien
  n'échouerait — le panneau se peindrait deux fois.
- **Une clé écrite après un `[[panel]]` appartient à ce panneau**, et c'est le
  piège de TOML : un `capabilities` placé sous le tableau cesse d'être celui du
  plugin, qui se voit refuser sa première requête sans que rien ne dise pourquoi.
  D'où `deny_unknown_fields` sur les deux déclarations.

**Le vocabulaire de vue est borné exprès** : colonne, rangée, section repliable,
texte en quatre rôles, extrait de code, liste, bouton, lien, champ de saisie,
pastille, encart, table de deux colonnes, barre de proportion, filet, remplissage,
état vide, roue. Assez pour un maître/détail, délibérément pas assez pour être un second
framework. `List` passe par `uniform_list` à `theme::row_height`, donc une
entrée fait deux étages au plus.

**Cinq de ces noms sont venus de Sentry, et aucun ne parle de Sentry** — c'est
le même portillon que la première fois. Une erreur ne se diagnostique pas d'une
trace seule : ce qui la rend lisible est ce qu'il y a autour — depuis quand,
combien de fois, sur quelle release, ce qui s'est passé la seconde d'avant —, et
aucune de ces cinq choses ne se disait avec un texte et une rangée.

- **`Badge` et `Callout` portent un `Tone`**, cinq niveaux de gravité et **pas
  une couleur** : un plugin dit ce qu'une valeur *veut dire*, le thème dit à quoi
  ça ressemble. C'est la règle que `TextStyle` suit déjà un cran plus bas. Un
  ton inconnu vaut `Neutral` — une gravité qu'on ne connaît pas n'est pas une
  erreur de script.
- **`Fields` est la seule forme qu'une rangée de textes ne donnait pas** : les
  valeurs s'alignent entre elles, et c'est ce qui rend huit d'entre elles
  lisibles d'un coup d'œil plutôt que huit phrases.
- **`Meter` porte un pourcentage entier** et non un flottant, pour que l'arbre
  reste comparable : tout le vocabulaire est `Eq`, et c'est ce qui permet
  d'écrire le rendu d'un plugin dans un test.
- **`Fill` est ce qui fait d'une liste un panneau de liste** et non un bloc dans
  une colonne. Sans lui, une liste doit se donner une hauteur d'avance, et seize
  lignes était le nombre choisi : au-delà elle défilait **dans** une colonne qui
  défilait aussi — deux barres pour un geste, dont aucune n'était celle du
  panneau —, et en deçà le panneau finissait sur une bande de rien. C'est le
  **script** qui le dit et non le panneau qui le devine : un plugin sait lequel
  de ses blocs est celui qu'on lit, et aucune lecture de l'arbre ne le dit. Un
  arbre qui remplit ne reçoit **pas** le défilement du panneau : ce qui est
  au-dessus est un en-tête, et un en-tête ne défile pas. Seuls les enfants
  directs de la colonne racine comptent — un `Fill` enfoui dans une section est
  une section qui grandit, et les deux se disputeraient les mêmes pixels.
- **Une liste porte sa barre et sa molette lissée**, comme les huit autres de la
  fenêtre, et elle n'avait ni l'une ni l'autre : une liste virtualisée ne dit
  rien d'elle-même, et un cran de trois lignes appliqué d'un coup était le seul
  endroit de la fenêtre où la molette ne glissait pas. La poignée appartient à
  la **fenêtre** — `ClaudhubApp::plugin_lists`, rangée sous le panneau *et*
  l'identifiant de la liste —, pour la raison qui fait vivre là l'`InputState`
  d'un champ : un arbre se reconstruit à chaque geste, et une poignée créée là
  remettrait la liste en haut à chaque fois.
- **Une section dit dans quel sens elle s'ouvre, le panneau retient
  l'exception.** Le repli est une posture de lecture — il change plusieurs fois
  dans une session et ne se persiste pas —, mais *où il commence* est une
  affirmation sur le contenu : une répartition qu'on regarde une fois appartient
  derrière son titre, une trace non. Ce que `plugin_folded` garde est donc ce
  qui **diffère** de ce que le script a demandé, la polarité de `tree::Folds` un
  étage plus haut, et c'est ce qui garde l'ensemble petit.
- **`Link` n'est pas un bouton** : le geste d'un bouton repart au script, et un
  script n'a aucune capacité qui atteigne un navigateur. C'est ce qui rendait
  mort le « Ouvrir dans Sentry » de la première version — un bouton dont
  l'action était vide, donc désactivé, donc un bouton qui ne fait rien.

**Un `Code` est coloré**, quand il nomme son langage, et il porte de quoi être
numéroté (`start_line`) et de quoi marquer la ligne dont on parle (`mark`).

- **Le texte part nu, les numéros sont une colonne du panneau** : un texte qui
  les porte ne se copie pas sans eux, et coller un extrait dans un fichier est ce
  qu'on en fait. Le prompt qui part à l'agent, lui, les remet — il arrive sans
  gouttière, et « la ligne 46 » se compterait à la main.
- **La coloration se fait à l'arrivée de l'arbre, jamais au rendu**
  (`ClaudhubApp::colour_plugin_code`) : c'est la règle de tout le module, et
  parser coûte des millisecondes là où un panneau se repeint à chaque frame. Le
  cache est **indexé par le texte** — deux cadres du même fichier citent des
  lignes différentes, et un chemin ne les distinguerait pas — et le thème en est
  le **témoin** : un changement de palette n'est signalé à personne, la leçon que
  le cache de `ui::blade` a déjà payée.
- **Un extrait est un fragment** : ce qui le précède n'est pas là, exactement
  comme une ligne de résultat de recherche, et c'est la reprise d'erreur d'une
  grammaire qui rend l'analyse isolée payable (`DocumentHighlights::for_language`).
  Le prologue PHP vaut ici aussi, et ses lignes ne comptent pas dans les
  décalages.
- **Un langage qu'on ne nomme pas reste gris** : deviner d'après le texte est
  ce qui fait peindre une transcription de shell en Rust.

**Un champ de saisie en fait partie**, et il a failli ne pas en être. Il avait été
écarté au motif qu'un `InputState` doit vivre d'une frame à l'autre et qu'un arbre
reconstruit à volonté ne peut pas le posséder. C'est exact et cela ne conclut
rien : ce n'est pas l'arbre qui le possède mais la **fenêtre**, par
`use_keyed_state`. Deux règles : l'`id` doit être **stable**, et la valeur
**amorce** le champ sans le suivre ensuite.

**Ajouter un nom au vocabulaire peut casser un plugin installé** : un script fait
`use claudhub::*`, si bien que chaque fonction réserve son nom. C'est arrivé
quatre fois — `text`, `state`, `field`, et `badge`, qui était le helper du plugin
CI — et cela ne se corrige pas, cela se sait. Ce qui
rend la chose supportable : Rune **le dit** en nommant les deux définitions,
l'erreur s'affiche dans le panneau, et une compilation ratée garde la machine qui
marchait.

Ce qu'il n'y a **pas** : aucune recherche dans le panneau d'un plugin, `Ctrl+F`
cherchant dans une liste dont nous tenons l'ordre.

- **Un `Ref<str>` et jamais un `String`, dans chaque fonction hôte.** C'est le
  seul piège qui ne se voit pas à la lecture : Rune passe un argument en le
  **prenant**, si bien qu'une fonction déclarée `|t: String|` sort le champ de
  l'objet d'où il venait — `item(run.title, …)` vidait `run.title`, le premier
  `view` marchait et le second échouait sur « value is moved ». Les fonctions
  asynchrones convertissent en propriétaire **avant** de construire leur futur.
- **Trois variantes de protocole, et trois seulement** : postcard est positionnel
  et `PROTOCOL_VERSION` se dit à la poignée de main, donc un plugin qui pourrait
  ajouter un message casserait le fil pour tous les autres. `Cmd::PluginCall`
  porte une capacité à charge fermée, `Evt::PluginResult` la ramène. Ajouter une
  **capacité** est un changement de Claudhub, versionné une fois ; ajouter un
  plugin n'est pas un changement du fil.
- **La file se lit sur la capacité, pas sur la variante** : un appel HTTP part au
  réseau, une commande shell au fond. Un test la verrouille.
- **Une requête ne reste jamais en attente** : canal à un coup sous un
  identifiant qui ne recule jamais, et trois choses le vident — la réponse, le
  rechargement, et le balayage de fond qui expire au-delà de quatre-vingt-dix
  secondes.
- **Le rechargement redémarre le plugin** : recompiler donne de nouvelles formes
  aux mêmes noms. `init` est rejouée, ce qu'on veut après avoir modifié le code
  qui va chercher. Une compilation **ratée garde la machine qui marchait** — un
  éditeur enregistre au milieu d'un mot.
- **L'erreur s'affiche dans le panneau**, pas dans la barre d'état : ce sont les
  diagnostics de Rune tels quels, qui nomment la ligne.
- **Une surveillance par dossier de plugin, et non une sur leur parent** :
  `Cmd::WatchDir` est **sans récursion**, si bien que surveiller
  `<config>/plugins` seul voit un plugin apparaître et ne voit jamais son
  `main.rn` changer. Le parent est gardé quand même — c'est lui qui remarque une
  installation.
- **Le panneau d'un plugin neuf attend un redémarrage ; son script, non.** Un
  panneau doit être dans le registre du dock **avant** que `layout.json` ne soit
  relu. La **liste**, elle, se relit tout de suite : le registre est derrière un
  verrou et non un `OnceLock`. Le nom du panneau est un `&'static str` fuité,
  borné par le nombre d'installations d'une session.
- **Un plugin dit ce dont il ne peut pas se passer** (`required`), et tant que ces
  champs sont vides il n'est **pas allumé** : pas de panneau, et pas non plus
  l'écran qui ne portait que lui. Atterrir sur une vue vide et devoir en déduire
  que la faute est un champ blanc ailleurs est la pire façon de l'apprendre. Un
  réglage et un secret sont la même chose ici.
- **Un écran qui ne porte que des plugins disparaît de la barre** quand aucun
  n'est utilisable (`screen_has_content`). Trois moments demandent de quitter un
  écran devenu vide (`leave_empty_workspace`).
- **Les capacités sont déclarées et non devinées** : un plugin qui atteint autre
  chose que ce que son `plugin.toml` liste se le voit refuser avant que rien ne
  parte.
- **Un chemin qu'un script nomme est un chemin *dans* le worktree**
  (`plugin::in_worktree`, pur et testé). Un script rend ce que son API lui a
  donné : Sentry nomme une frame d'après la racine de l'application **déployée**
  et livre avec son séparateur de tête ce qu'il n'a pas su relativiser —
  `/vendor/laravel/…`. Joint tel quel, un chemin absolu **remplace** la racine, et
  le panneau répondait « No such file or directory » d'un chemin que personne
  n'avait écrit. Le séparateur est retiré, et un `..` est **refusé**.
- **Une clé de défilement par panneau, et non une pour tous.** La clé nomme la
  barre **et** le lissage, et un plugin maître/détail met deux panneaux à
  l'écran en même temps : partagée, chaque frame avançait une seule motion
  depuis deux rendus et contre deux offsets différents, si bien que le détail
  ou bien sautait là où la liste en était, ou bien ne bougeait pas du tout —
  c'est ce dernier qu'on voyait. La règle est celle que `ui::surface` paie déjà
  pour ses deux surfaces de code.
- **Un identifiant par nœud, tiré de son rang dans le parcours.** gpui clé un
  élément à état par son identifiant parmi ses frères, et tous les extraits de
  code s'appelaient `plugin-code` : le second prenait en silence le défilement
  du premier, et les dix extraits d'une trace défilaient ensemble. Le parcours
  est en profondeur d'abord et l'arbre est reconstruit à l'identique entre deux
  gestes, donc le rang est stable d'une frame à l'autre — ce qu'un identifiant
  doit être.
- **Ouvrir un fichier atterrit sur la ligne** (`jump_to`, `Landing::Position`),
  et non en haut : un cadre nomme une ligne, et l'ouvrir au début du fichier
  laisse chercher à la main ce que la trace venait de dire. C'est le geste des
  résultats de recherche, et c'est celui qui avait le plus besoin de la piste —
  le panneau qu'on quitte est sur un autre écran.
- **Ce qui attend se voit, et ça tourne.** La roue est celle de gpui-component :
  un glyphe posé à côté d'un titre se lit comme une décoration, et c'est ce
  qu'était l'ancienne, ce dépôt n'ayant aucune animation par ailleurs.
- **L'attente se montre là où la réponse va atterrir**, c'est-à-dire dans tous
  les panneaux **sauf celui d'où le geste est parti** (`Plugin::waiting_in`). Les
  autres sont périmés par construction ; celui où l'on a cliqué est ce qu'on
  regarde et ce dont on vient de se servir, et l'effacer retire au geste son
  propre contexte. Un premier chargement n'a pas d'exception à faire.

**Les secrets ne passent pas par le script** : il nomme celui qu'il veut, et c'est
le **worker** qui le substitue dans un en-tête portant `{secret}`. Le jeton voyage
en `Secret`, dont le `Debug` masque la valeur. Les valeurs vivent dans les
réglages, jamais dans le manifeste, qui est un fichier qu'on recopie.

**Le trousseau est l'endroit par défaut** : `keep_secret` range la valeur dans le
trousseau du système et ne laisse dans `settings.json` qu'une **référence**
(`keyring:sentry.token`). Une valeur qui **nomme** déjà où elle vit (`$NOM`,
`keyring:…`) est enregistrée telle quelle. Une valeur vidée efface l'entrée.
Trois points : **à la perte du focus, jamais à la frappe** (un aller-retour vers
le trousseau peut demander de le déverrouiller) ; **en tâche de fond** ; et **un
échec se dit**, le repli vers le fichier étant ce qu'il faut à une machine sans
trousseau mais un jeton en clair devant être annoncé. La page porte sous chaque
champ un mot disant **où la valeur est vraiment**.

Trois formes, pour trois questions différentes :

- **le jeton en clair**, écrit en `0600` par `write_private`, qui répare aussi un
  fichier laissé plus permissif. Cela protège des autres comptes, pas de ce qui
  tourne sous le vôtre — agents compris. **Sous Windows il n'y a pas
  d'équivalent.**
- **`$NOM`**, lu dans l'environnement **du worker** — sous Windows, celui de la
  distribution WSL.
- **`keyring:compte`** ou `keyring:service/compte`, résolu **côté interface** :
  un trousseau appartient à une session de bureau, qui est celle de Windows quand
  les workers sont dans WSL. C'est aussi ce qui garde `keyring` — donc zbus —
  hors du binaire musl. La lecture est **mise en cache**, vidée dès que les
  réglages changent ; un échec est **journalisé** et non rendu en secret vide, le
  placeholder resterait sinon dans l'en-tête.

**Sentry a servi de portillon d'acceptation et l'a passé.** Mille lignes de Rust
contre deux cents de Rune, au prix de quatre ajouts au vocabulaire dont aucun ne
parle de Sentry : un **lecteur JSON** (Rune n'en a pas ; ce qu'il garde, ce sont
les formes telles qu'elles arrivent — Sentry écrit un compte en **chaîne** dans
la liste et en **nombre** ailleurs) ; un **`join`** (le `Vec` de Rune n'a rien qui
fasse un texte d'une liste de lignes) ; un **état par dépôt** (`state` /
`set_state`) ; et **`worktree_for(nom, prompt)`**, qui crée un worktree et remet
un texte à l'agent qui y atterrit — les deux dans un seul effet parce que le
prompt doit attendre que `wt` ait fini ses hooks.

**`set_state` écrit sur place autant qu'il envoie un effet** : les effets sont
drainés quand le script **rend la main**, si bien que sans l'écriture immédiate,
la ligne suivante qui relit `state` obtiendrait ce qui était là avant. Et la copie
durable est celle de l'application, **qu'il faut rendre** : `configure_plugins`
repose sur chaque plugin ce que le magasin garde du dépôt regardé, au même titre
que ses réglages, et cette passe rejoue à chaque changement de worktree. C'était
la moitié qui manquait — un projet choisi tenait jusqu'à la fermeture de la
fenêtre, et le lendemain le panneau redemandait lequel.

Ce que le portage a aussi révélé : un script ne peut pas nommer un de ses helpers
comme une fonction du vocabulaire — Rune refuse, et le dit clairement.

Ce qui reste vrai de Sentry : Claudhub le lit et ne lui envoie jamais rien ; le
code cité vient de l'**événement**, puisque c'est le code déployé au moment de
l'erreur ; la pile entière part au prompt mais le code des seules frames
`in_app` ; les deux formes de pile selon le SDK sont lues **toutes les deux**,
un client qui n'en lit qu'une affichant une trace vide sur la moitié des projets ;
et **l'événement d'une issue se lit par une liste**, `events/latest/` ayant quitté
l'API et répondant 404 — la route est org-scopée et il faut `full=true`.

**Ce qu'un clic sur une erreur demande est deux allers-retours** : l'événement,
qui décrit *cette* occurrence, et `issues/{id}/tags/`, qui décrit **toutes** les
autres. Le second est le seul renseignement de la page qui dise si le bug est
partout ou chez un seul, et son échec ne vaut que « pas de barres » — la trace,
elle, est arrivée.

- **La trace est renversée** : l'ordre de Sentry est celui de l'appel, et ce
  qu'on vient chercher est la ligne qui a levé. Elle est reconstruite plutôt que
  parcourue à rebours — la longueur d'un `Vec` de Rune est **non signée**, et
  `len() - 1` sur une pile vide déborde par en dessous au lieu de rendre `-1`.
  Le même piège fait échouer une comparaison entre un rang tiré de `len()` et le
  `-1` de « rien de choisi » : les compteurs sont donc écrits à la main.
- **Le filtre porte sur la liste qu'on a, pas sur la requête** : la requête est
  un réglage, et une frappe ne doit pas coûter un aller-retour. Corollaire : ce
  qu'un clic rend est un indice dans la liste **affichée**, quand ce que l'état
  retient est le rang dans la liste **entière** — sans quoi effacer le filtre
  ouvrirait une autre erreur que celle qu'on lisait.
- **Le numéro court est un bouton et non une ligne de texte** : `ACETICS-4Q` est
  la référence qu'on emporte ailleurs — un nom de branche, un message de commit,
  un message à quelqu'un —, et un panneau de plugin n'a pas de sélection de texte
  où aller la prendre à la main. D'où `Effect::Copy`, la cinquième chose qu'un
  script demande à Claudhub de faire chez lui.
- **La répartition s'ouvre repliée** : sept tags dépliés poussent la trace hors
  de l'écran, qui est ce qu'on est venu lire.
- **Le fil d'Ariane est ce que la trace ne porte pas** : une requête, un SQL, un
  log. Les douze derniers, dans l'ordre où ils se sont produits.
- **`now()` et `epoch()` sont les deux seules choses que l'hôte devait** : lire
  l'horloge, et lire une date ISO-8601. « Il y a cinq heures » s'écrit dans le
  script, qui est ce qui parle une langue.

**Un `init` qui échoue supprime la vue entière**, donc tout ce qui aurait permis
de s'en sortir : un script rattrape ce qu'il sait montrer et le range dans son
état, et ne rend une erreur que pour ce dont il n'a rien à dire. La même règle
vaut un cran plus bas — une issue dont l'événement a expiré est l'affaire d'une
ligne, pas du panneau.

**Pourquoi Rune.** Pur Rust, donc rien à négocier avec la jambe musl ni avec
Windows, là où un Lua vendorerait du C ; une syntaxe proche de celle qu'on lit à
côté ; et surtout **l'asynchrone de première classe**, l'argument décisif — chaque
capacité est un aller-retour sur `Cmd`/`Evt`, et un script doit pouvoir écrire
`shell(…).await`. Rhai ne sait pas faire d'asynchrone du tout, Lua le ferait à la
main. Ce que ça coûte : une 0.x qui a déjà cassé entre mineures.

**Le test qui compte ne lance aucun processus et n'ouvre aucune fenêtre** : un
script compilé depuis une chaîne, une capacité répondue à la main, et l'arbre
comparé à celui qu'on attend. Le plugin livré y passe aussi : un fichier embarqué
qui ne se lit pas ne provoque **aucune erreur**.

### Installer un plugin, l'éditer, le mettre à jour

**Le binaire `git` et pas une archive** : un plugin est un dossier de fichiers
texte, la mise à jour est un `pull`, et l'auteur publie en poussant. Les
credential helpers de l'utilisateur marchent sans que nous sachions qu'ils
existent. **Pas de dépôt central** : un registre, c'est un serveur, un espace de
noms, une politique de modération et un modèle de confiance, pour installer ce
qu'une URL nomme déjà.

**Une commande et une seule** (`Cmd::PluginManage`), dont la file se lit sur
l'opération : un clone et un `pull` au réseau, effacer un dossier avec les
lectures.

- **Le nom du dossier est arrêté avant que quoi que ce soit ne parte** :
  `install::dir_of` refuse un `..`, ce nom finissant dans un `git clone` et, pour
  une désinstallation, dans un `remove_dir_all`.
- **Un dépôt sans manifeste n'est pas un plugin**, et le clone est défait sur
  place.
- **Le nom est suggéré par l'URL et reste modifiable** : deux plugins peuvent être
  publiés depuis des dépôts appelés `claudhub-plugin`.
- **`pull --ff-only`** : un plugin qu'on a édité a des commits à lui, et une
  fusion laissée à mi-chemin est un état dont on ne sort pas depuis ici.
- **Éteindre un plugin ne l'empêche pas de compiler** : cela coûte une
  milliseconde et achète le seul renseignement qu'on veut d'un plugin qu'on vient
  d'installer.

**L'édition passe par l'écran « Édition », et cela n'a coûté aucune plomberie** :
l'éditeur range ce qu'il tient par une **racine**, et le dossier d'un plugin est
une autre racine (`editing_root`, remise à zéro dès qu'on choisit un worktree).
Enregistrer écrit le fichier, la surveillance se déclenche, le script recompile.

Un `.rn` est coloré avec la grammaire de **Rust**, convention assumée : la syntaxe
de Rune est celle de Rust à dessein.

**La page des réglages gère, le panneau diagnostique**, et le partage n'est pas
qu'éditorial : la fermeture de rendu du formulaire tourne **à l'intérieur** du
rendu de `ClaudhubApp`, donc y lire l'entité racine est une panique. Un
gestionnaire de clic, lui, s'exécute une fois cet emprunt rendu — d'où
`settings_view::with_app`, un handle faible dans un global, et la règle qui va
avec : **depuis un clic, jamais depuis un rendu**.

**Une installation passe par la mécanique des écritures en vol** (`Action::Plugin`)
et **une désinstallation se confirme** : c'est le seul geste d'ici que git ne
rattrape pas.

Le plugin livré s'appelle **« CI »** et lit les exécutions de GitHub Actions par
`gh`. Comme les thèmes, son dossier est **réécrit à chaque démarrage**. Le
formatage est demandé à `gh --template` plutôt que fait dans le script — Rune n'a
pas de lecteur JSON, et en écrire un ferait du plugin un sujet d'analyse
syntaxique au lieu d'un tableau de CI.

## Conventions gpui

Elles viennent d'Aviary, et les enfreindre produit des bugs silencieux.

- L'état qui survit à une frame vit dans un `Entity<T>` créé **une fois** dans le
  constructeur. Un `InputState` recréé dans `render` perd le curseur, la
  sélection et le texte dès la première frappe.
- `cx.listener(...)` pour les gestionnaires qui mutent la vue ; les souscriptions
  se font dans le constructeur, jamais dans `render`.
- `let theme = cx.theme().clone();` dès qu'une fermeture de rendu aura besoin de
  `&mut cx` — `cx.theme()` emprunte.
- `Theme::change` réinitialise les couleurs : toute palette s'applique **après**,
  puis `cx.refresh_windows()`.
- La vue racine doit ré-émettre les couches de `Root` (dialogues, notifications)
  à la fin de son `render`.
- **La fermeture d'`open_dialog` ne doit rien toucher à `ClaudhubApp`.** C'est un
  `Fn` que `Root` retient et rappelle **à chaque frame**, depuis le rendu de la
  vue racine, donc au milieu de l'emprunt : un `update` là-dedans panique, et un
  `read` aussi. Un état que le dialogue affiche *et* modifie vit donc dans une
  **entité à lui** ; ce qui ne s'exécute qu'au clic est libre.
- **Un `Dialog` ne peint pas ses boutons** : `on_ok` et `on_cancel` n'installent
  que les rappels d'Entrée et d'Échap, la rangée n'étant rendue que par un
  `AlertDialog`, qui ne prend ni champ ni liste. `ui::dialogs::confirm` rend donc
  le pied de page, et ses boutons **dispatchent les mêmes actions que les
  touches**.
- **`key_context` prend un identifiant, pas un prédicat.** Passer
  `"Claudhub && !Dialog"` fait boucler le parseur et déborder la pile au premier
  rendu. L'expression va dans le troisième argument de `KeyBinding::new`.
- Les raccourcis passent par `secondary-` : le reste du clavier appartient au
  programme du terminal — sauf les flèches nues et, mode vim allumé, la rangée de
  repos.
- gpui rend via Vulkan sur Linux : `vulkan-loader` doit être dans
  `LD_LIBRARY_PATH`, ce dont `shell.nix` se charge.

## Interface bilingue

Toute chaîne visible passe par `tr!` (défini dans `main.rs`), qui rend un
`SharedString` — pas un `String` : les catalogues compilés donnent des
`Cow::Borrowed(&'static str)`, et une frame en rend des centaines.

`assets/i18n/{fr,en}.json`, objets plats, clés en kebab-case préfixées par
domaine. Les deux catalogues doivent avoir les mêmes clés et les mêmes
substitutions `%{…}` : `ui::i18n_tests` le vérifie.

**Le code est en anglais, la documentation en français.** Commentaires, noms et
messages d'erreur du cœur sont en anglais — c'est la langue de gpui, de git et
des dépendances qu'on lit à côté ; ce fichier, le README et le `justfile` restent
en français. Un message d'erreur d'un worker remonte donc en anglais dans la
barre d'état.

Corollaire d'un renommage : l'index du coffre s'appelle `Review.md`
(`vault::INDEX`) et non plus `Relecture.md`, que `vault::LEGACY_INDEX` **relit
sans jamais l'écrire** — perdre en silence les coches d'une relecture existante
coûterait plus cher que de porter une constante.

## Tests

Les couches `git`, `terminal`, `runtime` et `plugin` sont testables sans contexte
gpui, et c'est là que sont les tests. Ils portent sur les formats que nous parsons
— sortie porcelain, diff unifié, séquences de touches — parce que c'est là que se
trouvent les régressions silencieuses : un chemin renommé mal découpé produit une
liste plausible mais fausse.

Le même motif partout : la décision vit dans un module **pur**, devant la vue qui
la peint — `notes.rs`, `sql_history.rs`, `inflight.rs`, `vim.rs`, `motion.rs`,
`jumps.rs`, `folds.rs`, `notify.rs`, `search.rs`, `db/scope.rs`, `db/link.rs`,
`db/sql.rs`, `db/complete.rs`.

`watch::tests::a_real_write_reaches_the_receiver` est le seul test qui touche le
système de fichiers ; il prouve la chaîne complète de la surveillance.
