# CLAUDE.md

Guide de ce dépôt. On y écrit ce qui ne se lit **pas** dans le code : la carte
des modules, les règles qui traversent plusieurs fichiers, et les pièges dont
la violation ne produit aucune erreur. Le reste — pourquoi telle ligne est là,
ce qu'un piège a coûté — vit en commentaire à l'endroit qu'il concerne.

**Ce fichier tient sous mille lignes.** Il en a compté quatre mille, une section
par décision, et un guide qu'on ne relit plus ne guide personne. Un changement
n'a sa place ici que s'il déplace la structure ; sinon, le commentaire suffit.

## Commandes

Tout passe par `nix-shell` via le `justfile` ; n'appelez `cargo` directement que
si les bibliothèques de `shell.nix` sont déjà dans le périmètre.

- `just` / `just run` — build debug et lancement
- `just check` / `just clippy` (`-D warnings`) / `just fmt` / `just test`
- `just check-server` — le serveur headless sans la feature `ui` : le portillon
  qui prouve qu'aucun module du cœur ne tire gpui ni Rune
- `just ci` — les quatre d'un coup
- `just check-vendor` — **avant de poser un tag** : le `cargoHash` du paquet nix
  change à chaque changement de `Cargo.lock`, numéro de version compris, et
  aucune des quatre portes ne le voit. Ne compile rien
- Un test isolé : `nix-shell --quiet --run "cargo test watch"`

Le projet doit passer `cargo fmt --check`, `clippy --all-targets -- -D warnings`
et `cargo test` en permanence.

## Distribution

Le binaire release ne tourne que sur cette machine : compilé sous `nix-shell`,
il est lié contre la glibc du nix store, interpréteur ELF compris.
`tools/make_appimage.sh` (à lancer **hors** nix-shell, après un build release)
produit l'AppImage et une archive auto-extractrice qui ne demande que `sh`,
`tar` et `gzip`.

Deux choses à ne pas défaire : les pilotes GPU (ICD Vulkan) viennent de
l'**hôte**, dont les chemins ferment le `--library-path` — embarquer un Mesa
casserait les machines NVIDIA ; et rien n'exporte `LD_LIBRARY_PATH`, si bien que
les sous-processus (`git`, `claude`, les shells) restent des programmes de
l'hôte.

**Le paquet nix** (`flake.nix`, `nix/package.nix`) est l'autre voie : `nix
build`, ou l'entrée `claudhub` d'une configuration NixOS. Deux choses valent
d'être sues. Les bibliothèques de rendu sont posées en **RPATH** et non en
`LD_LIBRARY_PATH`, pour la raison qui précède — un `LD_LIBRARY_PATH` suivrait
les sous-processus. Et `devShells.default` **importe `shell.nix`** au lieu de
recopier ses dépendances : le justfile appelle `nix-shell`, et deux listes
divergeraient au premier ajout.

**L'installeur Windows** (`tools/claudhub.iss`, Inno Setup) est le troisième
paquet, et il ne remplace pas le `.exe` nu : il ajoute ce qu'un fichier
téléchargé ne peut pas avoir — l'icône du bureau, le menu Démarrer, « Ouvrir
avec Claudhub » sur un dossier, et de quoi désinstaller. Quatre choses à ne pas
défaire. Il s'installe **sans droits d'administrateur** dans
`%LOCALAPPDATA%\Programs` : Claudhub n'écrit rien hors du profil, et un
exécutable posé dans Program Files ne serait plus remplaçable sans une invite
UAC à chaque mise à jour. L'`AppId` ne change **jamais** — c'est par lui que
Windows reconnaît une installation déjà là, et deux GUID laissent deux Claudhub
dans la liste des applications, dont un que plus rien ne désinstalle. Le script
est en **UTF-8 avec BOM**, sans quoi Inno 6 le lit comme de l'ANSI et les
messages français arrivent en mojibake, sans erreur de compilation. Et la
version n'est écrite nulle part : elle se lit dans l'exécutable, où `build.rs`
a mis celle de `Cargo.toml`.

**L'icône** vient de `assets/claudhub.svg`, engendrée en `.ico` par
`tools/make_icon.sh` — un fichier **versionné**, la jambe Windows n'ayant pas
ImageMagick. `build.rs` la pose en ressource dans le `.exe` ; sans elle les
raccourcis, la barre des tâches et l'explorateur montrent l'icône générique de
Windows. Le logo porte une **tuile**, et pas seulement le glyphe `git-branch` :
un tracé monochrome sur fond transparent disparaît sur une barre des tâches de
la même teinte. L'AppImage et le paquet nix en sortent aussi.

**La CI ne construit que des versions** (`.github/workflows/release.yml`, tag
`v*` ou manuel) : chaque jambe recompile l'arbre gpui entier, et `just ci` le dit
déjà ici. Elle relance les mêmes portes avant d'empaqueter — la machine qui
vérifie n'est pas celle qui livre, et la jambe Windows compile du code qu'on ne
compile jamais ici. Deux jambes : **Core tests** (le cœur sans `ui`, plus
`check-server`) et **Interface tests** (l'arbre entier — les deux tiers des tests
vivent derrière `ui`, dont la table des raccourcis, où `KeyBinding::new`
**panique** au démarrage sur une touche illisible). Ordre imposé : la jambe du
serveur musl finit avant celle de Windows, qui lui passe le binaire par
`CLAUDHUB_EMBED_SERVER`. Les fichiers sont attachés à une release en brouillon.

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
build.rs        embarque le serveur musl, s'il y en a un (`CLAUDHUB_EMBED_SERVER`),
                et sous Windows l'icône et la version en ressource
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
  outside.rs    ce qu'on demande au monde hors du dépôt : HTTP, une commande
  sentry.rs     les erreurs d'un projet Sentry : les URL, ce que l'API rend,
                le filtre, et ce qui part à l'agent — pur, testé
  wt.rs         le `wt.toml` d'un projet : questions, tâches, statut, URLs
  just.rs       les recettes du `justfile`, lues par `just --dump` (JSON)
  suite.rs      les suites de tests d'un worktree — Pest, Vitest, Jest,
                détectés par leur binaire et fusionnés : relevé par l'outil
                lui-même, le filtre exact qui relance un test, le run suivi
                sur les deux flux dont le compte (JUnit ou JSON) colore les
                lignes — règles vérifiées sur Pest 4, Vitest 4, Jest 30
  git/          couche git — sous-processus `git`, testable sans gpui
    mod.rs      exécution (stdin fermé, LC_ALL=C, pas de pager)
    repo.rs     découverte, worktrees, écritures (stage, commit, push…)
    status.rs   `status --porcelain=v2 -z` → index et worktree séparés
    branch.rs   `for-each-ref` → branches, amont, divergence
    diff.rs     `--numstat` et diff unifié → fichiers, hunks, lignes
    history.rs  `git log` → commits, et la disposition du graphe
    tags.rs     les tags : lecture, création, publication, suppression
    stash.rs    la pile des remisages : la lire, y poser, en reprendre
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
    rails.rs        les tool windows et leurs trois bandeaux — pur, testé
    dock_layout.rs  l'aire unique : sa disposition, ses sièges, ses gestes
    diff_view.rs    la vue de diff, virtualisée
    refine.rs       les mots qui changent entre deux versions d'une ligne — pur
    history_view.rs l'historique et son graphe peint
    tags.rs         le panneau des tags, et les quatre gestes sur un tag
    stashes.rs      le panneau des remisages, et les gestes sur un remisage
    tests_view.rs   les deux panneaux des tests : l'arbre repliable multi-
                    lanceurs (pastilles par test, filtre des échecs, replis
                    par défaut) et le run suivi — « tout lancer » est une
                    campagne, une commande par lanceur fusionnée en un compte ;
                    l'onglet n'existe que si un lanceur existe, les verdicts
                    persistent par worktree dans le magasin
    highlight.rs    coloration tree-sitter d'un diff
    blade.rs        les vues Blade : surcouche du diff, coloriseur de l'éditeur
    panels.rs       les panneaux du dock, leur macro et leur registre
    base_select.rs  le sélecteur de base de comparaison — ce qu'une entrée dit
    branches.rs     ce que le sélecteur de branches liste, et les gestes sur une
    branch_picker.rs   le sélecteur de branches : le filtre, la liste, les actions —
                       une surface, deux sièges : le popover de la barre de titre
                       et la colonne gauche de l'historique, quand il est assez large
    worktree_picker.rs le sélecteur de worktrees : le filtre, la liste, les actions
    worktrees.rs    ce que le sélecteur de worktrees liste — pur, testé
    picker.rs       ce que les deux sélecteurs partagent : le pas du curseur — pur
    review.rs / terminal_view.rs
    server.rs       la mise en route du serveur WSL
    settings.rs     les réglages et leur global
    settings_view.rs le formulaire, et la page « Journal »
    tree.rs         chemins → arborescence repliable, en indices
    file_icons.rs   l'icône et la teinte d'un fichier, d'après son nom
    explorer.rs     l'explorateur, l'éditeur intégré et ses onglets
    preview.rs      regarder un fichier au lieu de l'éditer : images et SVG
    db.rs           l'arbre des bases : connexion, base, table, colonne
    db_query.rs     les consoles SQL — une par onglet : ce qu'elle montre, ce
                    qu'elle attend, ses complétions et sa table de résultats
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
    follow.rs       les mots qu'on suit au Ctrl+clic : où est le pointeur — pur
    hunks.rs        la gouttière de l'éditeur, et la comparaison de lignes — pur
    merge.rs        la fusion à trois voies : ce que chaque côté a fait — pur
    merge_view.rs   les trois colonnes, et le clic qui tranche
    sentry.rs       les deux vues Sentry : la liste et l'erreur qu'on lit
    ci.rs           les exécutions de la branche, lues par `gh`
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
workers les consomment (`async_channel` est MPMC) et répondent par des `Evt`.
`ClaudhubApp::pump_events` les draine **par lots de 64** : un `update_in` par
événement forcerait un cycle d'effets à chaque fois.

Ajouter une opération : une variante de `Cmd`, un bras dans `runtime::handle`,
une ou plusieurs variantes d'`Evt`, un bras dans `ClaudhubApp::handle_event`.
Jamais un appel à git depuis un `render` ou un gestionnaire de clic — la plus
rapide des commandes coûte déjà une frame.

**Et un incrément à `wire::PROTOCOL_VERSION`** dès que la forme d'un message
change, retrait d'une variante compris : les deux bouts du fil sont installés
séparément et postcard est **positionnel**. Piège payé deux fois en fusionnant :
quand deux branches portent chacune le numéro au suivant, git prend la ligne sans
conflit et le fil se retrouve avec deux protocoles sous un seul numéro.

Toute écriture git est suivie d'une relecture du statut (`write_then_refresh`).

### Les sept files

`queue_of` dit, du seul examen d'une commande, dans quelle file elle part. Une
table qu'un test verrouille — une commande mal rangée n'échoue jamais, elle
attend, et c'est la panne qu'on ne diagnostique pas.

- **Lectures** (trois workers) : statut, diff, branches, écritures locales.
  C'est ce qu'une frame attend.
- **Réseau** (un worker) : `fetch`, `pull`, `push`, l'API de Sentry, message de
  commit rédigé par un agent. Un seul, parce que deux `fetch` sur le même dépôt
  se disputeraient le verrou des références.
- **Hooks du projet** (un worker) : `wt new/rm/up/down`. Pas avec les lectures
  (un `up` démarre des conteneurs), pas avec le réseau.
- **Fond** (un worker) : résumés, agents, relevé de `wt`, ses questions et les
  liens de son `[open] source` (des shells du projet), recettes d'un `justfile`,
  `gh` des exécutions CI. Ne doit jamais passer devant un diff qu'on vient
  de demander.
- **Bases** (deux workers) : deux, parce que déplier un schéma en demande
  plusieurs à la fois et qu'ils attendent une socket.
- **Recherche** (un worker) : une recherche se **remplace** — c'est
  l'identifiant d'envoi qui trie.
- **Tests** (un worker) : un run — Pest, Vitest ou Jest — se mesure en
  minutes ; avec le fond il affamerait les résumés, avec les hooks il ferait
  attendre un `wt up`. Le **relevé** des suites reste au fond, exprès :
  relire la liste ne doit pas attendre derrière un run.

Hors des files, trois voies directes : la surveillance de fichiers, remise au
thread du surveillant ; les serveurs de langage, remis à leur hôte ; et le
**stop d'un run de tests**, posé en drapeau que le worker sonde — rangé dans
sa file, il arriverait après la mort qu'il demande.

### Le mode distant

`Handle` a deux modes, et les points d'envoi de la vue n'en savent rien.
**Local** : les files de ce processus. **Distant** :
`runtime::remote::connect` lance `claudhub-server` en enfant et tout passe par
des trames sur ses stdin/stdout (`runtime::wire` — postcard, longueur en tête).

- **La poignée de main est hors des deux énumérations** (`wire::Hello`, champs
  jamais réordonnés) : elle doit se relire depuis n'importe quelle version.
- **`connect` ne bloque jamais l'appelant** : un `wsl.exe` froid met des
  secondes, et c'est le thread d'interface qui appelle.
- **La mort du serveur est un événement** (`Evt::ServerLost`), jamais un
  silence. La relance est **manuelle**.
- **Le plafond de trame vaut aux deux bouts** (256 Mo), et une charge trop
  grosse est **jetée et journalisée** : perdre un événement vaut mieux que
  fermer le fil.
- **stdout du serveur appartient au fil.** Un `println!` dans du code worker le
  corromprait ; les traces vont sur stderr.
- **Le manche reste vide tant que le serveur n'a pas répondu**
  (`HandleInner::Pending`) plutôt que de retomber sur les workers locaux : sous
  Windows ceux-ci feraient travailler `git.exe` sur des chemins inexistants.

Levier de test : `CLAUDHUB_SERVER_CMD`. `tests/server_wire.rs` exerce tout le
fil sous Linux, à chaque `cargo test`, avec le vrai binaire.

### La cible Windows

Interface en `.exe` gpui natif (DirectX), workers dans WSL2, et **seuls les
terminaux n'y passent pas** : leur pty reste local (ConPTY).

**Le binaire du serveur est embarqué dans l'exécutable** et installé dans la
distro à la première ouverture (`wsl::ensure_installed`) : `build.rs` le met dans
le `.exe` d'après `CLAUDHUB_EMBED_SERVER`, et l'installation écrit ces octets
dans l'**entrée standard** d'un `cat` lancé là-bas — ni partage réseau, ni chemin
à traduire, ni bit d'exécution perdu.

- **`build.rs` écrit une constante, il ne lit rien à l'exécution.** Une variable
  posée sur un chemin absent est une **erreur de compilation**.
- **L'installation est adressée par le contenu** (`~/.claudhub/bin/<empreinte>`),
  jamais par un numéro de version.
- **Rien ne passe par un shell de connexion** (`wsl.exe --exec`), d'où
  `wsl::probe`, qui demande une fois où est le foyer et quel shell appartient à
  l'utilisateur.
- **Le script d'installation n'a pas un seul guillemet**, délibérément : la ligne
  traverse `CreateProcess` puis la reconstruction d'`argv` par `wsl.exe`.
- **L'exécutable est un programme *fenêtré*** (`windows_subsystem = "windows"`).
  Corollaire : un processus sans console en fait **créer une** à chaque enfant de
  console, d'où `wsl::no_console` (`CREATE_NO_WINDOW`).
- **`wsl.exe --list` répond en UTF-16** avant `WSL_UTF8` ; `wsl::decode` gère les
  deux.

**Un dossier nommé en argument** (`claudhub <chemin>`) l'emporte sur le
répertoire de lancement — et si une fenêtre est déjà ouverte, il lui est passé
plutôt qu'ouvert ici (`instance.rs`) — c'est ce que passe « Ouvrir avec Claudhub » du menu
contextuel, le répertoire courant d'un verbe de l'explorateur étant celui que
l'explorateur avait, pas celui sur lequel on a cliqué. Il n'arrive au serveur
que par son **répertoire de démarrage** (`server::connect_wsl`) : le manche est
vide tant que le serveur n'a pas répondu, et tout ce qu'on lui enverrait avant
serait jeté.

**Le fil ne transporte que des chemins Linux**, et la traduction n'existe qu'aux
six endroits où un chemin change de monde : le sélecteur de dossier, le dossier
nommé en argument, le coffre de notes, la cible d'un export CSV, ce qu'un
glissement dépose dans l'explorateur, et les deux retours. `wslpath` est pur et testé sous Linux.
`settings.json` et `state.json` restent côté Windows mais contiennent des chemins
Linux.

Corollaire : **un chemin du serveur s'allonge par `wslpath::join`**, jamais par
`Path::join`, qui écrit le séparateur de la machine où il tourne — sous Windows
`/home/x` plus `notes` donne `/home/x\notes`, un seul dossier dont le nom porte
une contre-oblique. Rien ne le signale : le dossier est créé, sous un nom que
personne ne tape. C'est ce qui a donné un coffre de notes
`/mnt/c/…/notes\Projet\worktree`. Sous Linux les deux rendent le même octet,
et c'est ce qui rend la règle testable ici.

### Pourquoi le binaire `git` et non libgit2

Les credential helpers, `includeIf`, les hooks, la signature, les alias :
l'utilisateur attend *sa* configuration. Le coût est un `fork` par commande,
invisible à cette échelle.

Corollaires : `stdin` fermé et `GIT_TERMINAL_PROMPT=0` (sinon une invite de mot
de passe bloque un worker pour toujours), `GIT_EDITOR=true`, `LC_ALL=C` pour que
les messages d'erreur soient reconnaissables, `GIT_OPTIONAL_LOCKS=0` (sinon
`git status` réécrit `.git/index`, qui est surveillé), et les formats `-z`
partout où un chemin apparaît — un fichier peut contenir un saut de ligne.

`git_opt` existe pour les lectures dont l'échec est la réponse normale ;
`git_tolerant` accepte un code borné, ce dont `diff --no-index` a besoin — il
sort avec **1** dès qu'il trouve une différence.

### La surveillance de fichiers

**Ce qu'on surveille vient de `git ls-files`** : les dossiers contenant un
fichier suivi ou nouveau non ignoré, chacun **sans récursion**. Un projet Laravel
a quarante mille répertoires dont sept cents portent du code. Un dossier créé
plus tard est signalé par son parent.

**On ne réagit qu'aux événements qui changent le contenu**
(`watch::changes_content`) : inotify signale chaque **ouverture**, et c'est nous
qui les ouvrons — `git status` lisait le worktree, chaque lecture produisait un
événement, chaque événement un `git status`. `Any` et `Other` sont gardés : c'est
ainsi que `notify` signale un débordement de sa file.

**Poser les surveillances ne se fait jamais dans le thread d'interface** :
c'était une demi-seconde de fenêtre figée par changement de worktree.

**Sur un disque Windows monté par WSL, la surveillance ne marche pas et ne le dit
pas.** `notify` pose ses surveillances sur drvfs sans erreur et ne livre jamais
un événement, d'où `watch::on_windows_filesystem`, que la barre d'état affiche.
Pas de repli par sondage.

Dans un worktree lié, `.git` est un *fichier* qui pointe vers
`<principal>/.git/worktrees/<nom>`.

## L'espace de travail, et le dock

**Une seule aire de dock** (`ui::dock_layout`) : trois zones d'outils autour d'un
centre de documents, la forme que tout éditeur gardant ses utilisateurs dans une
fenêtre finit par prendre. Il y a eu neuf écrans, chacun sa propre aire, puis un
accueil qui les réunissait ; le double système coûtait quatre chemins pour
« montre-moi ça », neuf panneaux par terminal et deux par fichier — un panneau
n'appartient qu'à une aire à la fois — et neuf listes de vues recopiées.

La répartition se lit comme une phrase : **la gauche choisit, la droite se
souvient, le bas s'étale, le centre se lit.** À gauche ce qui désigne — les
fichiers, la recherche, les changements, la branche, les tests dans le haut ;
les notes, les conflits, les remisages, les tags dans le bas ; à droite ce qui
dit où l'on en est — les bases, les requêtes déjà jouées ; en bas ce qui a
besoin de la largeur — le run suivi, l'historique et son graphe, les terminaux ;
au centre ce qu'on relit — le diff, l'éditeur, l'aperçu, et chaque fichier ouvert
et chaque console SQL.

**La droite est un seul groupe**, les deux autres bords en ont deux : rien n'y
démarre dans la moitié basse, et une moitié vide n'est pas un emplacement mais
une bande qui ne dit rien et qu'on ne remplit qu'en y traînant quelque chose
(`dock_layout::edge`). Elle existe toujours — un panneau qu'on y dépose la
fabrique — et `seats` lit un groupe seul comme la moitié haute, si bien que le
bandeau de droite garde une seule série.

**Une tool window a un bouton, un document n'en a pas** — la différence n'est
pas dans la nature du panneau mais dans sa place. `ui::rails` est pur et décide
tout : l'ordre des boutons, lequel est allumé, ce qu'une pression veut dire, ce
que le zen replie et rend.

**Replier et retirer sont deux choses**, et le bandeau est là où ça se voit :
une vue repliée garde son bouton — cette pression est ce qui la ramène —, une
vue retirée n'en a aucun, et c'est tout ce que retirer veut dire. Le menu
« Vues » de la barre de titre est le seul chemin du retour, donc il liste
exactement ce qu'un bandeau peut porter (`rails::in_view_menu` : pas les vues
situationnelles, dont le bouton va et vient avec le contenu — sauf déjà
retirées). Les deux états ont partagé un seul ensemble, et ça faisait que
chaque geste défaisait l'autre : la pression qui rangeait une vue la marquait
comme « je ne m'en sers pas », donc le bouton qu'on venait de presser devait
disparaître. Un **document** ne se retire pas, il se **ferme** : sa croix est
peinte par le panneau (`panels::closable_title`), le dock n'en dessine aucune.

**Il n'y a pas de seconde liste.** Un bandeau se calcule à chaque frame depuis
l'arbre du dock (`dock_layout::seats`) et rien n'en est gardé : un panneau
traîné d'un bord à l'autre change de bandeau sans qu'on tienne rien à jour.
`Tool::home` ne dit que le point de départ. Une tool window ne rejoint `TOOLS`
que **par la fin**, `Alt+1` à `Alt+9` étant des rangs de ce tableau ; un bandeau
ne défile pas, et `MAX_PER_RAIL` l'y tient.

**Les bandeaux sont peints par nous, hors de l'aire**, et c'est ce qui permet à
une zone de se replier à rien pendant que ses boutons restent. L'affordance du
dock est un chevron dans une barre d'onglets : il nomme la zone d'à côté et s'en
va avec elle, si bien que ce qu'il cachait ne revenait que par la barre d'un
autre groupe. D'où `set_toggle_button_visible(false)`.

**Un onglet du centre ne s'affiche que s'il a quelque chose à montrer**
(`needed:` dans la macro `panels!`). La règle ne valait que sur l'accueil, les
autres écrans ayant chacun un centre à eux dont l'état vide *était* l'écran ; le
centre est maintenant toujours celui-là, donc elle vaut partout. La question se
pose à l'**application** et non au panneau — un panneau ignore quelle région le
tient, le registre en rebâtit un d'après un nom seul.


**Le panneau d'accueil du centre** (`EditorPanel`) n'est plus une contrainte du
moteur — `add_panel_view` sait fendre la racine d'une région vide, et fabrique
même une zone absente. Il reste parce qu'une fenêtre qui s'ouvre sur rien ne dit
rien de ce qu'elle sait faire : au premier démarrage, diff, console et aperçu
sont tous conditionnels, donc absents.

**Les Réglages sont une modale** et le **multiplexeur un état** : « tous les
worktrees », bascule de la barre d'état, puisque c'était déjà son seul écart
(`terminals_everywhere`). Le formulaire des réglages est une **entité enfant** et
non une fermeture : `open_dialog` retient un `Fn` rappelé depuis le rendu de la
racine, où lire l'entité racine est une panique.

La disposition est enregistrée dans `<config>/layout.json`, une seule.
`LAYOUT_VERSION` la fait écarter quand les panneaux changent de nom, et c'est le
**seul** numéro consulté à la lecture. Ce qu'une disposition relue porte et que
le registre ne sait pas bâtir est **élagué** (`app::prune`,
`panels::is_registered`) — et **seule une feuille se juge sur son nom**, un
conteneur portant celui du dock. Les panneaux se déclarent au registre par la
macro `panels!`, et `dock_layout` les bâtit **par ce registre** : deux listes
divergeaient au premier ajout.

Sept pièges du dock, tous rencontrés :

- **Un panneau sans pile parente est verrouillé** (`is_locked`) : tout panneau
  doit être enveloppé, fût-ce dans une division d'un seul élément.
- **`toggle_dock` ne notifie pas l'aire**, seulement le dock intérieur — or
  c'est l'aire qu'on observe pour enregistrer.
- **Le dernier panneau de l'*aire* ne se déplace pas** (`is_last_panel`) : le
  drapeau se comptait par arbre, or chaque zone est le sien.
- **Les tailles d'une division se donnent toutes** : un `None` vaut cent pixels
  dans l'état enregistré, et la pile répartit **au prorata**.
- **L'état se relit au moment d'écrire**, pas à l'appel : l'ouverture d'une zone
  est différée d'une frame.
- **`panel_handle` et `add_panel_view`, jamais `add_panel`** : un `Entity<P>` se
  convertit tout seul et le dock l'accepte sans rien dire, mais sans onglet, ni
  titre, ni contenu. C'est l'échec silencieux de la refonte du dock. Et
  l'ajout-puis-déplacement passe par `panels::dock_panel_at`, qui rend à la
  région l'onglet que son premier groupe montrait.
- **L'élagage d'une zone latérale passe par `DockState::new`** et ses quatre
  lecteurs : les champs sont privés, mais démonter et remonter suffit. Sans lui
  un terminal de la zone basse revient en « panel type is not registered » à
  chaque démarrage — son contenu est un processus, rien ne le rebâtit.

**Les terminaux sont des panneaux** : un par terminal, rendant sa propre
`Entity<TerminalView>`. La place se garde par l'invisibilité, pas par un
déménagement. Ils sont retirés de `layout.json` avant écriture — leur contenu est
un **processus**. Fermer un onglet dont une commande tourne se fait confirmer
(`Terminal::busy`, lu dans `/proc/<pid>/stat`) ; un onglet **lancé sur une
commande** — agent, tâche `wt`, recette `just` — est occupé tant qu'il vit, et ça
ne se déduit pas du processus : `sh -lc` **exec** ce qu'on lui donne, si bien que
l'enfant du pty tient le groupe de premier plan comme un shell à son invite.

### Le grain de l'interface

**Les rayons montent à huit et douze** dans `theme::apply`. **La carte, c'est le
groupe d'onglets entier**, barre comprise, peinte par le fork. **Le masque de
contenu de gpui est rectangulaire** : l'arrondi d'un élément ne rogne que son
propre fond, jamais ses enfants — d'où `panels::corner_cut`, qui redécoupe les
deux coins du bas par-dessus le contenu.

**Une ligne de liste est un bandeau, pas une pastille** : ce qu'une ligne
sélectionnée désigne est la **ligne**, pas son texte. D'où `w_full` sur l'entrée,
aucun rayon, et l'entrée qui porte son propre retrait — `uniform_list` **ignore
les marges de ses entrées**. Le bandeau va **sous** la barre de défilement, ce
qu'il porte non (`theme::scroll_gutter`), la gouttière étant toujours réservée.

**Aucune hauteur de ligne ne s'écrit en dur** (`theme::row_height` et ses
voisines) : une hauteur figée déborde dès qu'on grossit la police, et c'est pire
dans les listes virtualisées, qui réservent exactement ce qu'on leur annonce.

**`Theme::tokens` est dérivé de `Theme::colors` une seule fois**, à l'application
de la palette : toute couleur écrite dans `theme::apply` doit être suivie du
recalcul, sans quoi elle ne se voit nulle part et rien ne le signale.
`gpui-base` tient **sa propre copie** du thème, que `Theme::sync_base` projette.

**Les thèmes sont générés** par `tools/gen_themes.py` : une clé absente ne
provoque pas d'erreur, elle reprend la valeur par défaut, qui est *claire*. Le
registre de gpui-component ne se charge **que depuis un répertoire**, qu'il
surveille ; les thèmes sont donc écrits dans `<config>/themes/` au démarrage, et
réécrits à chaque fois — pour en modifier un, le copier sous un autre nom.

**Le fork de gpui-component** (voir `Cargo.toml`) est vingt commits au-dessus
de leur `main`, chacun payé par un symptôme :

1. le `TabVariant` que `DockSkin` fait passer jusqu'au `TabBar` ;
2. les coins en boîte bordée réservés au variant classique ;
3. le groupe lu comme une carte hors variant classique ;
4. la même gouttière entre les **zones** du dock ;
5. `split_gap`, pris en **rembourrage** et non en `gap` — ce cadre n'a qu'un
   enfant, et une marge aurait faussé les tailles que le redimensionnement
   distribue ;
6. **une sélection reste peinte quand le focus part à un menu** ;
7. **un contrôle peut cacher son caret sans se désactiver**
   (`set_cursor_hidden`), ce que demande un éditeur modal ;
8. **le fond d'un run est peint** — `ShapedLine::paint` ne dessine que les
   glyphes, si bien qu'un `background_color` était **invisible en silence** ;
9. **une application peut replier sans passer par la gouttière**
   (`fold_candidates`, `set_folded`), `display_map` étant privé ;
10. **l'éditeur dit jusqu'où il défile** (`scroll_size`) : la mesure du dernier
    rendu, celle contre laquelle `set_scroll_offset` rogne ;
11. **un contrôle peut demander un caret en bloc** (`set_caret_block`) ;
12. **ce qu'une division laisse en trop va à un slot qu'aucune taille ne fixe**
    — c'était le dernier slot montré, celui qu'un appelant venait d'épingler ;
13. **une aire de dock refuse un panneau qui n'est pas le sien** : deux aires
    peuvent être à l'écran en même temps, et l'onglet glissé de l'une à l'autre
    y était inséré quand même ;
14. **une application peut marquer la gouttière** (`set_gutter_marks`) : les
    deux crochets existants pendaient à `InputEditorStyle`, que `Input::render`
    reconstruit **à chaque frame** ;
15. **le seul panneau d'une zone latérale se déplace quand même** : `alone` se
    lisait par arbre, et chaque zone est le sien ;
16. **et la zone qu'il laisse cesse d'être peinte** : un dock garde sa taille une
    fois vidé, laissant une bande morte le long du bord ;
17. **l'entrée d'un menu tronque son libellé** : la boîte est plafonnée
    (`max_width`), la ligne ne l'était pas — un libellé long gardait sa largeur
    entière, mille cent pixels dans une boîte de deux cents, peinte à travers la
    bordure. Rien ne le repliait non plus, une ligne de menu ayant la hauteur
    d'une ligne.
18. **les tailles d'une division sont des parts, pas des mesures** : le dock
    les redonne à chaque réconciliation — un changement d'onglet en est une —
    et l'état les reprenait brutes, sans les remettre à l'échelle du conteneur
    déjà mesuré ; une colonne de 420 px dessinée à 508 y retombait au premier
    clic dans une barre d'onglets.

19. **un dialogue peut reprendre le focus** (`focus_dialog`) : un dialogue à
    pages perd le sien quand le champ de la page précédente meurt — et avec lui
    Entrée, Échap et les actions que ses boutons dispatchent.

20. **une barre d'onglets sans préfixe garde le retrait du préfixe** : les
    boutons de dock se rembourrent de deux, et chaque onglet est décalé par
    celui d'avant — le premier était donc la seule chose de la fenêtre à
    toucher le bord de sa propre carte, et ça se lisait comme un défaut
    d'alignement plutôt que comme un onglet. Ça ne se voit qu'avec une barre
    sans bouton de tête, ce qui est exactement notre cas : les zones se
    replient depuis les bandeaux.

Les commits ont vocation à partir en PR.

## Les sous-systèmes

Un paragraphe par sujet : ce qui décide, où ça vit, et le piège qui ne se voit
pas. Le détail est en commentaire dans le module nommé.

**La vue de diff** (`diff_view.rs`) — les deux listes sont virtualisées par
`uniform_list`, et quatre contraintes tiennent ensemble : `.h(LINE_HEIGHT)`
explicite, `.whitespace_nowrap()`, `ListHorizontalSizingBehavior::Unconstrained`
avec `with_width_from_item`, et **pas de `w_full` sur une entrée**. Tout ce qui
se déduit d'un diff est calculé une fois dans `Rendered`, jamais dans la
fermeture de rendu. Le repli des lignes longues n'existe qu'en deux colonnes, se
fait **à la colonne** et non aux mots — c'est ce qui rend la hauteur calculable —
et bascule sur `v_virtual_list`. La largeur mesurée n'existe pas à la première
frame et est **toujours celle de la frame d'avant**, d'où le `canvas` de
`diff_laid_out`. **`Ctrl` rend les symboles cliquables** : les plages de mots ne
sont posées que tant que la touche est tenue — c'est ce qui met la main sur un
mot et nulle part ailleurs —, et le repeint qui les installe vient d'un
`on_modifiers_changed` sur la **racine**, un changement de modificateur étant un
événement clavier qui remonte depuis ce qui a le focus. Le mot survolé est
souligné par une **troisième couche** de style (`highlight::underline`, après la
grammaire et les occurrences), et c'est l'entrée survolée qui efface le
soulignement d'une autre — un texte est l'enfant de sa ligne, donc il parle en
premier.

**La revue** (`review.rs`) — `DiffRange` n'a ni `Unstaged` ni `Staged` : la
distinction est un détail de plomberie git, restitué par une case à cocher par
fichier. `rows_for` est la seule vraie décision, libre et testée. `app::
initial_range` choisit au **premier** statut ; ensuite la portée appartient à
l'utilisateur (`range_chosen`). La base vient de git (`branch::default_base`),
jamais d'un `main` codé en dur.

**L'explorateur** (`explorer.rs`, `tree.rs`) — l'arbre vient d'un seul appel git,
jamais d'un parcours de disque. `tree` ne connaît que des chemins et rend des
**indices** : la même feuille apparaît dans le sous-arbre de chacun de ses
parents. **Construire l'arbre et le plier sont deux gestes**, et les confondre
coûtait un cinquième de seconde par chevron. Le curseur est un **chemin**, pas un
indice. L'arbre s'ouvre fermé et retient ce qu'on a **ouvert** ; la revue s'ouvre
grande et retient ce qu'on a **fermé** (`tree::Folds`). Git s'arrête au dossier
qu'il exclut en entier (`--directory`) et son contenu se lit au chevron
(`files::read_dir`).

**L'éditeur** (`explorer.rs`, `surface.rs`) — un jeu d'éditeurs par worktree, un
onglet par fichier, et c'est la **barre du dock**. Le panneau qui se peint *est*
le fichier qu'on lit (`show_file`). Police, taille et hauteur de ligne se disent
**explicitement** : sinon l'éditeur hérite de la police proportionnelle et d'un
`line_height` calé sur le rem, donc sourd au zoom. L'écriture est
**conditionnelle** (`files::write`, empreinte) : un agent écrit dans les mêmes
fichiers.

**La gouttière** (`hunks.rs`) — le texte de base est lu une fois
(`repo::head_blob`) et la comparaison refaite **en mémoire** à chaque frappe :
`git diff` compare le fichier sur le disque, or le tampon qui compte est celui
sous le curseur. La comparaison est *patience*, écrite ici. Le saut de ligne
final est une ligne (`split('\n')`, jamais `lines()`). Sans base, aucune marque.
Le rétablissement est une **édition**, pas un `git apply`.

**Le mode vim** (`vim.rs`, `surface.rs`) — désactivé par défaut, et il faut que
ça le reste : ses liaisons sont des **lettres nues**. La machine ne connaît aucun
type de gpui : on lui donne le texte et le curseur, elle rend l'édition. L'écoute
est en **phase de capture** sur un ancêtre — après les liaisons, avant que la
plateforme ne livre le caractère. Ce qui arrive par une **liaison** et non par une
frappe (`Ctrl+V`, `Enter`, `Backspace`) s'attrape comme une **action**. Le
caractère lu est celui que la frappe a *produit* (`key_char`), pas la touche. Le
curseur bloc et les occurrences de recherche sont peints par nous, en couches de
décoration créées **une fois** avec la surface.

**Les surfaces de code** (`surface.rs`) — une surface se **nomme**, elle ne se
possède pas (`Surface::File(chemin)` / `Surface::Query`) : l'état reste où il
vit. Un fichier se nomme par son **chemin**, pas par « l'onglet actif » — le dock
en montre deux dès qu'on ouvre un split, et une clé de lissage unique poussait
deux éditeurs à la fois.

**Le terminal** (`terminal/`) — `alacritty_terminal` fournit le parseur, la
grille et le pty. Le verrou de la grille est partagé avec la boucle d'E/S :
**ne jamais dessiner sous ce verrou**, d'où l'instantané. Une police à chasse
fixe ne suffit pas à aligner les colonnes : chaque run est posé **à sa colonne**
en absolu, et un caractère mesuré hors grille reçoit une case à lui. Les lignes
de l'historique sont numérotées **négativement**. Le redimensionnement attend que
la main s'arrête, l'attente repartant à chaque changement. Une ligne de terminal
ne se laisse pas comprimer (`flex_shrink_0`). **Le texte nu passe par l'input
handler, jamais par la frappe** : `key_bytes` ne rend que les touches spéciales
et les combinaisons, `on_key` consomme ce qu'il rend, et la plateforme livre à
`TerminalInputHandler` ce que le clavier a composé — le ê d'un ^ mort, le @
d'AltGr+2, qui sous Windows n'existent que dans le `WM_CHAR` d'un keydown non
consommé. Rompre un des trois maillons double chaque lettre ou la perd, sans
erreur.

**La recherche** — `Ctrl+F` cherche dans le panneau où l'on vient de **cliquer**
(`panels::pane_root`, en capture). Là où la liste est libre de son ordre elle
**filtre** ; là où l'ordre porte du sens elle **saute**. `Ctrl+Maj+F` demande à
`git grep` ce que le worktree contient — jamais un parcours à nous, et pas
ripgrep : la question a été mesurée, l'écart est sous le seuil de perception et
`rg` n'est promis par aucune machine cible. `-E` et non `-P` (PCRE est une option
de compilation de git). Trois plafonds, et chacun est dit. La recherche est
amortie par un **compteur de frappes** relu à l'échéance, pas par un drapeau.

**Les bases** (`db/`, `db.rs`, `db_query.rs`) — un seul pilote, `sqlx`.
**Une console est un document**, un panneau chacune, comme un fichier ouvert :
il y en avait une parce que le centre était un emplacement unique, l'aire de
dock a donné la barre d'onglets. Un clic sur une table **réutilise** la console
où l'on est — parcourir un schéma à la souris laisserait dix onglets — et l'on
en ajoute en le demandant : le bouton du panneau des bases, `Ctrl+Maj+Q`, ou
« interroger dans un nouvel onglet ». L'identifiant d'envoi est compté pour la
**fenêtre** : c'est tout ce que la réponse rapporte, et deux consoles comptant
depuis un prendraient chacune les lignes de l'autre.
`db::Cell` est un `Option<String>` : `NULL` n'est pas la chaîne « NULL ». Un
`DECIMAL` se décode par `try_get_unchecked`, seul de tous les types. Une
connexion par requête, jamais gardée. La console est une **fenêtre** sur le
résultat, pas « la page *n* » ; le tri est fait par le moteur en enveloppant la
requête (`db::order_by`, par **rang**), et ce qu'on ne sait pas envelopper n'est
pas triable. Un identifiant d'envoi, et non la requête, écarte le résultat en
retard. Le motif de portée (`db::scope`) sépare les bases d'un worktree ; rien
n'est masqué en silence.

**Sentry** (`sentry.rs`, `ui/sentry.rs`) — deux panneaux sur un état : la liste
à gauche, l'erreur au centre — c'est le geste du reste de la fenêtre, et un seul
panneau empilait les deux, si bien que choisir une erreur poussait hors de vue
la liste où l'on choisissait. Le **projet appartient au dépôt** et se choisit
dans la barre du panneau ; l'organisation et le jeton appartiennent à la machine
et sont dans les réglages — cinq worktrees d'un même code ont les mêmes erreurs,
deux dépôts d'une même organisation non. Ce qui décide vit dans `sentry.rs`, pur
et testé sur des fixtures : les URL, les formes que l'API rend, le filtre, le
prompt. Le jeton ne traverse jamais le script d'un `Debug` : `{secret}` est
substitué **dans le worker** (`outside::Cap`), et `$SENTRY_TOKEN` est lu dans son
environnement. Un identifiant d'envoi écarte la réponse en retard, comme la
console SQL.

**La CI** (`ui/ci.rs`) — les exécutions de la branche, par `gh` et non par l'API
GitHub : la CLI est déjà authentifiée, c'est un programme que l'utilisateur
installe comme les agents du terminal, et Claudhub n'a ni jeton à tenir ni OAuth
à parcourir. Le formatage est demandé à `gh --template` : le JSON qu'on
analyserait serait une forme de plus à suivre. Un piège y est écrit et ce n'est
pas un raffinement — `printf "%.0f"` sur l'identifiant, un modèle Go formatant un
nombre JSON en flottant, si bien que `{{.databaseId}}` rend `3.2494024323e+10`,
que `gh run view` ne reconnaît pas, et rien ne le dit avant le premier clic.

**Il y a eu un système de plugins**, et Sentry en était le portillon
d'acceptation : mille lignes de Rust contre deux cent soixante-dix de Rune, et
l'API tenait. Ce qu'il a coûté est ce qui l'a fait retirer — Rune, un hôte de
script, un vocabulaire de vue, un installeur par `git clone`, une page de
réglages, et un second format de paquet — pour deux panneaux qu'on écrivait
soi-même. Ce qui en reste est `outside.rs`, la moitié qui n'a jamais parlé de
script : une liste fermée de ce qu'on fait dehors, chacun avec sa file. Le
troisième niveau du système d'extension redevient le `justfile`, le `wt.toml` et
les commandes déclarées dans les réglages.

**L'instance unique** (`instance.rs`) — « Ouvrir avec Claudhub » est sur
*chaque* dossier, et un deuxième clic donnait un deuxième processus : deux
fenêtres, deux serveurs WSL, deux jeux de surveillances sur les mêmes worktrees.
Le dossier part donc à la fenêtre déjà ouverte, par une socket locale — tube
nommé sous Windows, socket abstraite sous Linux, `GenericNamespaced` des deux
côtés parce qu'aucun des deux ne laisse de fichier : un nom libéré à la mort du
processus n'a pas d'histoire de verrou périmé. Quatre choses. **Rien ici ne peut
empêcher la fenêtre de s'ouvrir** : toute panne retombe sur « vous êtes seul »,
ce que le programme faisait avant ce module, et `CLAUDHUB_ALLOW_MULTIPLE` le dit
exprès — le `justfile` le pose, sans quoi `just run` rendrait la main à un
Claudhub installé sans rien afficher. **Le nom porte l'utilisateur**, les deux
espaces de noms étant à l'échelle de la machine. **Ce qui arrive n'est pas de
confiance** : une socket abstraite Linux n'a aucun contrôle d'accès, la charge
est donc lue en UTF-8 et refusée sinon, jamais passée à
`OsStr::from_encoded_bytes_unchecked` dont elle ne tient pas le contrat — ce
qu'un programme local y gagne est de faire remonter la fenêtre et ouvrir un
dossier, ce que le geste fait déjà. Et **le rang de sélection
retombe à zéro** quand un dossier arrive : `pick_worktree` note un répertoire de
lancement `SELECTION_CHOSEN`, qui ne déplace pas un `SELECTION_CHOSEN` déjà là —
la réponse arriverait et ne changerait rien.

Un dossier qui arrive alors que le serveur WSL démarre encore **attend la
poignée de main** (`pending_handoff`) : le manche est vide tant que le serveur
n'a pas répondu, et une commande jetée là est un clic qui n'a rien fait.

Sous Windows, remonter la fenêtre demande le consentement de celui qui a le
premier plan : c'est le processus lancé par l'explorateur qui l'a, et la fenêtre
à remonter est à un autre. D'où `AllowSetForegroundWindow` **côté secondaire**,
avant de passer le chemin ; sans lui, `SetForegroundWindow` là-bas ne fait que
clignoter un bouton de la barre des tâches.

**Les remisages** (`git/stash.rs`, `ui/stashes.rs`) — `refs/stash` vit dans le
`.git` commun : la pile est **celle du dépôt**, la même vue de tous les
worktrees, d'où une liste rangée sous le dépôt principal comme celle des tags.
Et `stash@{0}` est une **position**, que le terminal d'à côté décale : chaque
geste porte l'empreinte que la ligne montrait, et la couche git refuse dès que
le nom ne la désigne plus — git n'accepte le nom que sous cette forme (`drop`
sur une empreinte est refusé). Un `pop` qui **conflit** est un échec qui a
changé le worktree : le statut se relit dans les deux branches.

**Les notes** (`notes.rs`, `vault.rs`) — une note retient des **numéros de
ligne**, un côté et l'**extrait** : `diff_selection` est invalidé par chaque
rechargement de diff. `notes::relocate` la replace, et une note qui ne se
retrouve pas est dite **décalée** et **reste dans la liste**. Le dossier est la
source de vérité — un fichier Markdown par note, relisible, donc `ui::vault` rend
du texte et le relit sans toucher au disque. On n'efface que ce qui porte notre
marque, et sur sa **valeur** (`files::is_ours`). L'envoi passe par le terminal,
en collage encadré, le retour chariot dans un **second** envoi.

**Les raccourcis** (`shortcuts.rs`) — une seule table (`table!`) donne
`bind_keys` et la fenêtre d'aide. Deux prédicats et pas un seul : sous Linux
`secondary` **est** Ctrl, donc une lettre seule passe par `WINDOW_PREDICATE`, qui
exclut le terminal. Les tool windows se replient par `Alt+N` et non
`Ctrl+Maj+N` : gpui *retire* le Maj quand la touche est un caractère sans
casse. Une touche que
gpui lit n'est pas une touche qui existe (`valid_keys`) ; `KeyBinding::new`
**panique** sur ce qu'elle ne sait pas lire, et `init` tourne au démarrage.

**Le lissage de la molette** (`motion.rs`) — on n'empêche pas gpui de sauter (il
n'y a pas de capture pour la molette) : on le laisse faire, on lit où il a
atterri, on **remet** le décalage d'avant et on y va progressivement. D'où
l'écouteur sur un ancêtre **non défilant**. Le saut se **lit**, il ne se
recalcule pas (`Axis::jump`) — au bord, l'écart est tout le mouvement. Quatre
surfaces n'y passent pas et chacune a sa raison, écrite sur place.

**La coloration** (`highlight.rs`, `blade.rs`) — c'est le *code* qu'on relit, pas
la grammaire `diff`. Deux invariants que gpui ne vérifie pas et dont la violation
est silencieuse : les plages doivent être **triées et disjointes**, et les
décalages sont en **octets**. Un fragment reçoit d'abord de quoi être reconnu
(`prologue` — sans `<?php`, PHP lit tout comme du HTML). Une vue Blade est du
HTML avant d'être du PHP, d'où une surcouche ; ce qui est écrit dans une autre
langue est rendu à sa grammaire. `SyntaxHighlighter::new` compile les requêtes —
des dizaines de millisecondes — donc **jamais dans un `render`**, et gardé d'un
fichier à l'autre.

**Le serveur de langage** (`lsp/`, `ui/lsp.rs`) — une session par worktree, et
c'est une **voie**, pas une file : un serveur vit des heures et **l'ordre
compte**. Un seul propriétaire par session. Du JSON brut sur le fil, `lsp-types`
étant sous la feature `ui`. Le `languageId` n'est pas l'extension
(`lsp::language_of`). Le fil ne transporte que des chemins **absolus** — une URI
est absolue ou elle n'est rien —, et l'oubli ne provoque aucune erreur : les
diagnostics reviennent sous un chemin que l'éditeur n'emploie pas. **Un saut de définition
qui ne trouve rien retombe sur `git grep`** (`search_view::search_for_definition`) :
serveur coupé, langue non servie, symbole inconnu — c'est le même geste pour la
main, et une seule occurrence se suit, plusieurs ouvrent l'écran Recherche. La
légende des jetons sémantiques est traduite dans le vocabulaire de nos thèmes
(`lsp::theme_name`), un nom inconnu ne rendant **aucun** style.

**La piste** (`jumps.rs`) — une place est un fichier *ou* un document du centre,
et c'est une seule piste. Un document ne porte que son nom ; la console SQL est
l'exception, son état étant ce qu'un geste **remplace**. Déplier une tool window
n'est **pas** un déplacement : montrer l'historique à côté de ce qu'on lit ne
change pas où l'on est, et une flèche arrière qui replierait une zone serait
imprévisible. Une piste par worktree. Le départ est lu
au moment du saut. Un nouveau saut jette ce qui était devant. Les boutons 4 et 5
de la souris sont la même piste (`MouseButton::Navigate`).

**Le magasin** (`store.rs`) — les réglages disent comment Claudhub s'affiche, le
magasin dit où l'on en est. Écrit depuis le thread d'interface, ce qui déroge à
la règle des entrées-sorties : elle vise les commandes git, pas la préférence
qu'on range. Un seul point d'écriture, et les ensembles sont **triés** avant —
un `HashSet` sérialisé dans un ordre différent fait un fichier qui change sans
que rien n'ait changé. Une entrée retient son **dépôt**, sans quoi une entrée
absente ne se distinguerait pas d'une entrée d'un dépôt pas encore ouvert.

**Les réglages** (`settings.rs`) — un **global gpui** et non un champ de
`ClaudhubApp` : le formulaire déclare chaque champ par des fermetures qui ne
reçoivent qu'un `App`. L'écriture est différée d'une demi-seconde. Ce qui dépend
d'un réglage se **relit à chaque rendu**, jamais à la construction. La page
demandée passe par l'**identifiant** du formulaire, `default_selected_index`
n'étant lu qu'à la création de l'état. Le formulaire est une **entité enfant**
bâtie une fois : `open_dialog` retient un `Fn` rappelé depuis le rendu de la
racine, et lire l'entité racine de là est une panique.

**Le journal** (`logging.rs`) — notre `log::Log` par-dessus celui d'`env_logger`,
un anneau de deux mille lignes. Une application graphique n'a pas de console sous
sa fenêtre. Trois étages : `git::report` file chaque commande à `debug` (une qui
**traîne** passe à `info`), `runtime::handle` nomme chaque commande par
`Cmd::name` — un match exhaustif, là où un nom tiré de `Debug` formaterait la
charge —, et `fail` est le seul `warn`. **Le journal est en anglais**, et un test
relit les sources pour le garder.

**Les agents** (`agent.rs`) — la détection passe par `/proc` et non par nos
onglets, ce qui fixe la cible Windows à WSL2. Le relevé ne dit pas qu'un agent
travaille : c'est la **différence** entre deux relevés (`agent::Tracker`).
`parse_cpu_ticks` repart de la **dernière parenthèse fermante**, le nom du
programme pouvant contenir espaces et parenthèses. Les marqueurs de session de
l'agent qui nous a lancés sont effacés au démarrage
(`agent::disinherit_session`), sinon un `claude` ouvert dans un onglet se croit
la sous-session de celui d'à côté.

**`wt`** — une dépendance, pas un sous-processus : parser la sortie de sa CLI
reviendrait à lire ce qui est fait pour un humain. Sa CLI reste derrière la
caractéristique `cli`, que Claudhub n'active pas. Le `wt.toml` d'un projet ajoute
des actions **sans que Claudhub les connaisse** — `[tasks.*]`, `[[prompt]]`,
`[status] up`, `[open]`, `[lsp.<nom>]` : c'est le vrai système d'extension. Les
questions se demandent en **boucle**, un `[[prompt]]` ayant un `when` qui dépend
d'une réponse précédente ; `wt::Phase` décide lesquels s'appliquent.

**Les conflits** (`merge.rs`, `merge_view.rs`) — « smart » n'est pas un réglage,
c'est ce que la comparaison rend : le fichier est relu depuis l'index (`:1:`,
`:2:`, `:3:`) et comparé deux fois, et ce qu'**un seul** côté a touché est pris
sans rien demander. Ce que rend l'algorithme est ce que rend git, jusqu'aux
découpages qui paraissent grossiers. **`--ours` et `--theirs` s'inversent pendant
un rebase** : la traduction se fait dans la couche git, une fois. Le blob se lit
octet pour octet (`git_blob`), `git` rognant les sauts de ligne finaux.

**Ce qui tient lieu de système d'extension**, du moins cher au plus cher : le
`justfile` du dépôt, s'il en a un — ses recettes sont le bouton « Lancer » de
la barre de titre, lues par `just --dump --dump-format json` et jamais par un
parseur à nous ; le `wt.toml` du projet ; des commandes déclarées dans les
réglages. Il y a eu un quatrième niveau — un panneau écrit en Rune — et il est
parti avec ce qu'il coûtait ; voir « Il y a eu un système de plugins ». Des
extensions wasm à la Zed avaient été écartées avant lui, pour la raison qui l'a
emporté à son tour : deux panneaux livrés ne paient ni un WIT ni un deuxième
format de paquet.

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
- **Lire l'entité racine depuis une fermeture de rendu est une panique.**
  `open_dialog` retient un `Fn` rappelé **à chaque frame** depuis le rendu de la
  vue racine ; le contenu d'un popover tourne **dans** `ClaudhubApp::render`. Ce
  qui doit relire l'application est donc une **entité enfant**, dont le `render`
  a lieu après que la fermeture du parent a rendu la main — la règle dont vivent
  les panneaux du dock. Depuis un **clic**, tout est libre
  (`settings_view::with_app`).
- **Un `Dialog` ne peint pas ses boutons** : `on_ok` et `on_cancel` n'installent
  que les rappels d'Entrée et d'Échap. `ui::dialogs::confirm` rend le pied de
  page, et ses boutons **dispatchent les mêmes actions que les touches**.
- **Le champ d'un dialogue prend le focus, et de façon différée**
  (`ui::dialogs::focus_field`) : un menu contextuel **rend le focus** à ce qui
  l'avait quand il se referme, après le gestionnaire qui a ouvert le dialogue.
  Un focus posé tout de suite perd la course, et le dialogue s'ouvre sans
  clavier — ni frappe, ni Entrée. Rien ne s'abonne au `PressEnter` du champ :
  l'Entrée d'un champ d'une ligne **remonte déjà** à la liaison `Confirm` du
  dialogue, et l'abonnement en ferait une deuxième.
- **`key_context` prend un identifiant, pas un prédicat.** Passer
  `"Claudhub && !Dialog"` fait boucler le parseur et déborder la pile au premier
  rendu. L'expression va dans le troisième argument de `KeyBinding::new`. Et un
  contexte doit être déclaré **sur le même nœud** que celui avec lequel il se
  combine : `depth_of` évalue chaque identifiant contre un seul niveau.
- **`div()` est un *bloc*, pas une boîte flex** : `Style::default()` porte
  `Display::Block`, où les propriétés flex d'un enfant sont **ignorées** — un
  `flex_1()` y prend la hauteur du contenu, et le `size_full()` d'en dessous se
  résout alors contre une hauteur indéfinie, donc zéro. Un conteneur dont un
  enfant réclame la place restante s'écrit `v_flex()` / `h_flex()`.
- **Tout ce qu'une opération a à dire est une bulle, en haut à droite**
  (`ui::notify`, `ClaudhubApp::announce`) : la barre d'état n'en porte plus
  rien. Un point d'appel sans fenêtre passe par la file `pending_notes`, vidée
  en tête du rendu de la racine — `push_notification` réclame un `&mut Window`.
- Les raccourcis passent par `secondary-` : le reste du clavier appartient au
  programme du terminal.
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
bulle qui le rapporte.

## Tests

Les couches `git`, `terminal`, `runtime` et `sentry` sont testables sans contexte
gpui, et c'est là que sont les tests. Ils portent sur les formats que nous
parsons — sortie porcelain, diff unifié, séquences de touches — parce que c'est
là que se trouvent les régressions silencieuses : un chemin renommé mal découpé
produit une liste plausible mais fausse.

Le même motif partout : la décision vit dans un module **pur**, devant la vue qui
la peint — `notes.rs`, `sql_history.rs`, `inflight.rs`, `vim.rs`, `motion.rs`,
`jumps.rs`, `folds.rs`, `notify.rs`, `search.rs`, `merge.rs`, `hunks.rs`,
`refine.rs`, `db/scope.rs`, `db/link.rs`.

`watch::tests::a_real_write_reaches_the_receiver` est le seul test qui touche le
système de fichiers ; il prouve la chaîne complète de la surveillance.
