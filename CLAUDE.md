# CLAUDE.md

Guide de ce dépôt pour Claude Code et pour tout contributeur. C'est le **seul**
document d'architecture : quand un changement touche la structure, il se met à
jour dans le même commit.

## Commandes

Tout passe par `nix-shell` via le `justfile` ; n'appelez `cargo` directement
que si les bibliothèques de `shell.nix` sont déjà dans le périmètre.

- `just` / `just run` — build debug et lancement
- `just check` / `just clippy` (`-D warnings`) / `just fmt` / `just test`
- `just check-server` — le serveur headless sans la feature `ui` : c'est le
  portillon qui prouve qu'aucun module du cœur ne tire gpui
- Un test isolé : `nix-shell --quiet --run "cargo test watch"`

Le projet doit passer `cargo fmt --check`, `clippy --all-targets -- -D warnings`
et `cargo test` en permanence.

## Distribution

Le binaire release ne tourne que sur cette machine : compilé sous `nix-shell`,
il est lié contre la glibc du nix store, interpréteur ELF compris.
`tools/make_appimage.sh` (à lancer **hors** nix-shell, après un build release)
produit dans `target/appimage/` deux formes du même contenu — la closure
complète, glibc et `ld-linux` avec, lancée par un `AppRun` qui invoque le
loader embarqué explicitement : `Claudhub-x86_64.AppImage`, qui exige un
`fusermount` sur la cible, et `Claudhub-x86_64.run`, auto-extractrice, qui ne
demande que `sh`, `tar` et `gzip` — elle se déballe dans `~/.cache` une fois
par build (contenu adressé par empreinte, anciens builds purgés) et lance
instantanément ensuite. Deux choses à ne pas défaire : les pilotes GPU (ICD
Vulkan) viennent de l'**hôte**, dont les chemins ferment le `--library-path` —
embarquer un Mesa casserait les machines NVIDIA ; et rien n'exporte
`LD_LIBRARY_PATH`, si bien que les sous-processus (`git`, `claude`, les shells
du terminal) restent des programmes de l'hôte avec les bibliothèques de
l'hôte. La machine cible fournit `git`, un pilote Vulkan, et l'agent.

**La CI ne construit que des versions** (`.github/workflows/release.yml`,
déclenché par un tag `v*` ou à la main). Pas à chaque commit : chacune de ces
jambes recompile l'arbre gpui entier, et ce que la CI vérifierait, `just ci` le
dit déjà sur la machine de développement.

**Mais elle relance les mêmes portes avant d'empaqueter**, et ce n'est pas une
redite : la machine qui vérifie et la machine qui livre ne sont pas la même, et
la jambe Windows compile du code que la machine de développement ne compile
jamais. Deux jambes, parce qu'elles n'ont pas le même prix. **« Core tests »**
est gratuite — `fmt --check`, les tests et le clippy du cœur sans la feature
`ui`, plus `check-server` — et c'est elle qui prouve qu'aucun module du cœur
ne tire gpui. **« Interface tests »** compile l'arbre entier pour lancer
`clippy --all-targets` et la campagne complète : les deux tiers des tests
vivent derrière la feature `ui` — les catalogues i18n, la table des raccourcis
dont `KeyBinding::new` **panique** au démarrage sur une touche illisible, les
clés de thème, les plages triées et disjointes du diff — et ne tournaient
nulle part ailleurs que sur la machine de développement. Une version pouvait
donc partir avec un binaire qui panique au lancement.

Elle tourne **à côté** des jambes d'empaquetage et non devant : un build gpui
complet est de toute façon le chemin le plus long, et c'est `publish` qui
l'attend — rien n'atteint une page de release sans elle. Elle produit deux livraisons, et **chacune est un fichier
unique** : l'AppImage et le `.run` ci-dessus pour Linux, et pour Windows un
`.exe` qui **porte le serveur en lui** (voir « La cible Windows »). D'où
l'ordre des jambes : celle du serveur musl doit finir avant que celle de
Windows ne compile, puisqu'elle lui passe le binaire par
`CLAUDHUB_EMBED_SERVER`. Le serveur est lié en statique (musl) parce qu'il est
copié dans une distribution dont on ne sait rien.

La jambe Linux passe par Nix et pousse ce qu'elle construit dans Cachix
(`CACHIX_AUTH_TOKEN` en secret, le nom du cache dans la variable
`CACHIX_CACHE`), ce qui évite de reconstruire la closure à chaque version.
Les fichiers sont attachés à une release **en brouillon** : c'est la dernière
occasion de relire ce qui part avant de le rendre public.

## Architecture

Trois couches, et une règle qui les sépare : **seule `src/ui/` connaît gpui, et
elle ne fait jamais d'entrée-sortie**.

Le crate est une **bibliothèque et deux binaires** : `claudhub` (l'interface)
et `claudhub-server` (les mêmes workers, headless, derrière stdin/stdout —
destiné à tourner dans WSL2 quand l'interface est un `.exe` Windows). La
feature `ui`, active par défaut, porte gpui, gpui-component, alacritty et tout
ce qui s'affiche ; le serveur se construit avec `--no-default-features`, et un
module du cœur qui toucherait à gpui — `tr!` compris, le macro n'existe que
sous `ui` — casse ce build. C'est la règle des trois couches, vérifiée par le
compilateur.

```
build.rs        embarque le serveur musl dans l'exécutable, s'il y en a un
                à embarquer (`CLAUDHUB_EMBED_SERVER`)
src/
  lib.rs        les modules, l'i18n et `tr!` (feature `ui`)
  main.rs       le binaire de l'interface — trois lignes
  bin/server.rs le serveur headless (le transport arrive avec `runtime::wire`)
  cmdline.rs    découpe et recompose une ligne de commande (guillemets POSIX)
  wsl.rs        la distro : la lister, y installer le serveur, l'y lancer,
                et la ligne de commande d'un terminal
  wslpath.rs    chemins Windows ⇄ distro WSL, textuel et pur — n'existe
                qu'aux bords : sélecteurs de fichiers, ouverture du coffre
  commit_msg.rs le message de commit proposé : prompt, nettoyage, agent
  files.rs      lire, écrire (sous condition), ranger, éditeur externe
  db/           bases de données — `sqlx`, asynchrone, testable sans gpui
    mod.rs      connexions, schémas, résultats ; le choix du moteur
    scope.rs    quelles bases appartiennent au worktree regardé — motifs
    sqlite.rs   en lecture seule, schéma lu par les pragmas
    mysql.rs    MySQL et MariaDB, par `information_schema`
  logging.rs    `env_logger` sur stderr, et un anneau de deux mille lignes
                en mémoire, que la page « Journal » lit
  lsp/          le client de serveur de langage — un processus par worktree
    mod.rs      la session : poignée de main, requêtes en vol, ce qu'il pousse
    frame.rs    le cadrage `Content-Length` et JSON-RPC
    sync.rs     versions de document, et l'édition à une plage (UTF-16)
    uri.rs      chemins ⇄ `file://`
  sentry.rs     issues et traces Sentry, testées sur fixture
  wt.rs         le `wt.toml` d'un projet : questions, tâches, statut, URLs
  git/          couche git — sous-processus `git`, testable sans gpui
    mod.rs      exécution des commandes (stdin fermé, LC_ALL=C, pas de pager)
    repo.rs     découverte, worktrees, écritures (stage, commit, push…)
    status.rs   `status --porcelain=v2 -z` → index et worktree séparés
    branch.rs   `for-each-ref` → branches, amont, divergence
    diff.rs     `--numstat` et diff unifié → fichiers, hunks, lignes
    history.rs  `git log` → commits, et la disposition du graphe
    tags.rs     les tags : lecture, création, publication, suppression
  agent.rs      les agents dans `/proc`, et le suivi qui dit lesquels
                travaillent
  runtime/      les workers
    protocol.rs `Cmd` / `Evt` — des données, aucune logique, sérialisables
    mod.rs      cinq files (`queue_of`), des threads consommant les mêmes
                canaux, et le surveillant de fichiers en sixième voie
    executor.rs l'exécuteur tokio partagé, et le pont `block_on`
    watch.rs    surveillance de fichiers (notify), debounce 250 ms,
                limitée aux dossiers que git connaît
    wire.rs     les trames du fil UI ↔ serveur : postcard, longueur en
                tête, `PROTOCOL_VERSION`
    remote.rs   le client du fil : lance `claudhub-server`, trois threads
                de pont, la mort du serveur en `Evt::ServerLost`
  terminal/     émulation
    mod.rs      pty + `Term` alacritty derrière un `FairMutex`
    snapshot.rs grille → lignes et runs de style, sans tenir le verrou
    keys.rs     frappe gpui → octets (séquences xterm)
    mouse.rs    clic et molette → octets, quand le programme les demande
  ui/           tout gpui
    mod.rs      `run()`, `AssetSource`, polices, i18n
    app.rs      `ClaudhubApp` : l'état, la pompe d'événements, le chrome
    topbar.rs   la barre de titre : le menu, et les sélecteurs de worktree
                et de branche qui pilotent les vues
    repos.rs        les dépôts ouverts et ceux qui manquent, et ce qu'on
                    leur demande — sans gpui, donc testé
    inflight.rs     les écritures en vol, et ce que la barre en dit —
                    sans gpui, donc testé
    workspace.rs   les cinq écrans, leur dock et la barre qui les choisit
    diff_view.rs   la vue de diff, virtualisée
    history_view.rs  l'historique et son graphe peint
    tags.rs         le panneau des tags, et les quatre gestes sur un tag
    highlight.rs   coloration tree-sitter d'un diff
    blade.rs        les vues Blade : la surcouche du diff, et le coloriseur
                    de l'éditeur
    branches.rs     ce que le sélecteur de branches liste, et les deux
                    gestes sur une branche — sans panneau
    review.rs / terminal_view.rs
    server.rs       la mise en route du serveur WSL : la distro qu'on
                    demande, l'installation, l'état qu'en dit la barre
    settings.rs     les réglages et leur global
    settings_view.rs  le formulaire, bâti sur `gpui_component::setting`,
                    et la page « Journal »
    tree.rs         chemins → arborescence repliable, en indices
    file_icons.rs   l'icône et la teinte d'un fichier, d'après son nom
                    (marques dans assets/icons/lang/, CC0)
    explorer.rs     l'explorateur de projet et l'éditeur intégré
    sentry_view.rs  les issues, leur trace, et de quoi les confier
    db.rs           l'arbre des bases : connexion, base, table, colonne
    db_query.rs     la console SQL, ses complétions et sa table de résultats
    sql_history.rs  les requêtes déjà jouées : dédup, portée, jours — pur
    sql_history_view.rs  le panneau « Historique » et ses gestes
    conflicts.rs    les conflits et le garde-fou d'une opération à mi-chemin
    worktree_ops.rs création guidée, tâches du projet, intégration
    store.rs        ce qu'on retient par worktree : base, replis, notes
    session.rs      où l'on en était : le worktree, le fichier ouvert, la
                    console — et la règle, testée, du worktree qui s'ouvre
    dialogs.rs      les deux boutons d'un dialogue, que gpui-component ne
                    peint que pour un `AlertDialog`
    notes.rs        le modèle des notes, leur ancrage et leur prompt
    notes_view.rs   les gestes de la relecture annotée et son panneau
    vault.rs        les notes, le suivi de relecture et la liste de tâches
                    en Markdown, rendus et relus — aucune entrée-sortie ici
    lsp.rs          le pont vers le serveur de langage : les providers de
                    l'éditeur, les diagnostics, et le bouton « LSP »
    jumps.rs        la piste de l'éditeur : d'où l'on vient, où l'on
                    repart — sans rien de gpui dedans
    folds.rs        quels replis un niveau de repli ferme — sans gpui dedans
    find.rs         la recherche d'un panneau, et son routage
    motion.rs       le lissage de la molette, sans rien de gpui dedans
    vim.rs          les modes de vim de l'éditeur : motions, opérateurs,
                    registres — sans rien de gpui dedans
    scroll.rs       la barre de défilement d'un panneau, et son lissage
    shortcuts.rs    les actions, leurs touches, et l'aide qui en sort
    shortcuts_view.rs  la fenêtre d'aide, en deux colonnes
    theme.rs / icons.rs
```

### La boucle Cmd/Evt

Le thread d'interface envoie des `Cmd` par `runtime::Handle::send`. Trois
threads workers les consomment (`async_channel` est un canal MPMC, ils partagent
le même récepteur) et répondent par des `Evt`. `ClaudhubApp::pump_events` les
draine par lots de 64 dans une tâche gpui de premier plan : un `update_in` par
événement forcerait un cycle d'effets à chaque fois.

Ajouter une opération, c'est : une variante de `Cmd`, un bras dans
`runtime::handle`, une ou plusieurs variantes d'`Evt`, un bras dans
`ClaudhubApp::handle_event`. Jamais un appel à git depuis un `render` ou un
gestionnaire de clic — la plus rapide des commandes coûte déjà une frame.
**Et un coup d'incrément à `wire::PROTOCOL_VERSION`** dès que la forme d'un
message change : les deux bouts du fil sont installés séparément, et postcard
est positionnel — un désaccord doit se dire à la poignée de main, pas en
charabia au premier diff.

### Les cinq files

`queue_of` dit, du seul examen d'une commande, dans quelle file elle part.
C'est une table qu'on lit d'un bout à l'autre, et un test la verrouille — une
commande rangée dans la mauvaise file n'échoue jamais, elle attend, et c'est
la panne qu'on ne diagnostique pas.

- **Les lectures** (trois workers) : statut, diff, branches, et les écritures
  locales, toutes en millisecondes. C'est ce qu'une frame attend.
- **Le réseau** (un worker) : `fetch`, `pull`, `push`, Sentry, et le message
  de commit qu'un agent rédige — dix à trente secondes. Un seul worker parce
  que deux `fetch` sur le même dépôt se disputeraient le verrou des références
  sans rien accélérer.
- **Les hooks du projet** (un worker) : `wt new`, `wt rm`, `wt up`, `wt down`.
  Ils ont eu la file du réseau, pour la bonne raison — un `post_new` installe
  des dépendances, un `up` démarre des conteneurs, et les mettre avec les
  lectures figerait la revue le temps d'un `composer install`. Mais le réseau
  n'a qu'un worker : un `wt up` retenait derrière lui tout ce qui se compte en
  secondes, c'est-à-dire exactement le symptôme qui avait fait sortir le réseau
  de la file des lectures. Le verrou des références, lui, ne dit rien des hooks
  d'un projet, qui ne touchent à rien de git.
- **Le fond** (un worker) : les résumés, les agents, le relevé de `wt`. Il ne
  doit jamais passer devant un diff qu'on vient de demander.
- **Les bases** (deux workers) : ni celle des lectures — un `SELECT`
  malheureux y emporterait un worker sur trois et le diff attendrait derrière
  —, ni celle du réseau. Deux, parce que déplier un schéma en demande plusieurs
  à la fois et qu'ils attendent une socket, pas un cœur.

Et hors des files, deux voies directes : la surveillance de fichiers, remise au
thread du surveillant, et les serveurs de langage, remis à leur hôte — voir
« Le serveur de langage ».

### Le mode distant

`Handle` a deux modes, et les soixante-dix points d'envoi de la vue n'en
savent rien. **Local** : les files de ce processus, comme toujours.
**Distant** : `runtime::remote::connect` lance `claudhub-server` en enfant et
tout passe par des trames sur ses stdin/stdout (`runtime::wire` — postcard,
longueur en tête, un `flush` par trame) ; c'est le serveur qui refait le tri
entre ses files à l'arrivée. Le serveur est le même `runtime::spawn` derrière
un fil — `handle()`, l'exécuteur et les workers ne savent pas qu'ils sont
loin. C'est l'architecture de la cible Windows : l'interface en `.exe` natif,
les workers dans la distro WSL2, seuls les terminaux n'y passant pas
(`wsl.exe` depuis ConPTY).

Quatre points qui ne se devinent pas :

- **La poignée de main est hors des deux grandes énumérations**
  (`wire::Hello`, champs jamais réordonnés) : c'est elle qui détecte un
  désaccord de version, elle doit se relire depuis n'importe quelle version.
  Le lecteur du client la traduit en `Evt::ServerHello` — le seul flux que la
  vue draine — qui porte ce que la vue ne peut pas savoir de sa machine à
  elle : le `cwd` du serveur (c'est lui qui vaut « lancé depuis son projet »),
  son appartenance à WSL, ses `/etc/shells`. Ces derniers vont dans un
  **statique** de `settings.rs` et non dans `ClaudhubApp` : le formulaire
  déclare ses champs par des fermetures qui ne reçoivent qu'un `App`, ce qui
  est déjà la raison d'être du global des réglages. Ils arrivent filtrés par
  leur existence **là-bas**, que nous ne pouvons pas vérifier d'ici.
- **`connect` ne bloque jamais l'appelant** : un `wsl.exe` froid met des
  secondes, et c'est le thread d'interface qui appelle. Tout ce qui suit le
  lancement — poignée de main comprise — arrive en événements.
- **La mort du serveur est un événement** (`Evt::ServerLost`, synthétisé par
  le lecteur), jamais un silence : la barre d'état le dit et porte le bouton
  de relance. La relance est **manuelle** — un serveur qui meurt en boucle se
  relancerait en boucle — et repasse les dépôts ouverts au serveur neuf.
- **Le plafond de trame vaut aux deux bouts.** Le lecteur refuse au-delà de
  256 Mo — passé cette taille, les quatre octets de longueur n'en étaient pas
  une —, et l'écrivain honore le même plafond. Sans cela il envoyait ce qu'on
  lui donnait, si bien qu'une trame que le lecteur refuse était une trame que
  l'écrivain avait produite : le fil mourait pour une charge seulement trop
  grosse, ce qui se lit « serveur perdu » et non « ce diff est énorme ». La
  charge est **jetée et journalisée**, comme un chemin non-UTF-8, plutôt que
  remontée en erreur : perdre un événement vaut mieux que fermer le fil pour
  tout le monde, et la vue repose d'elle-même ce qu'elle attend. C'est aussi ce
  qui rend sûre la longueur sur quatre octets, qui tronquerait au-delà de
  quatre gigaoctets.
- **stdout du serveur appartient au fil.** Un `println!` dans du code worker
  corromprait le flux ; les traces vont sur stderr, que le client pompe dans
  les nôtres (`target: "claudhub_server"`).

Le levier de test est `CLAUDHUB_SERVER_CMD` (une ligne de commande, par
exemple `target/debug/claudhub-server`) : tout le fil s'exerce sous Linux,
sans Windows ni WSL. `tests/server_wire.rs` le fait à chaque `cargo test`,
avec le vrai binaire. Il l'emporte sur la mise en route automatique, ce qui
en fait aussi la sortie de secours quand celle-ci se trompe.

### La cible Windows

L'interface est un `.exe` gpui natif (DirectX), les workers tournent dans une
distribution WSL2, et **seuls les terminaux n'y passent pas** : leur pty reste
local — ConPTY — et c'est ce qui tourne dedans qui traverse. WSLg a été
essayé d'abord et rendait mal ; c'est ce qui a décidé de la découpe.

**Le binaire du serveur est embarqué dans l'exécutable**, et installé dans la
distro à la première ouverture (`wsl::ensure_installed`). L'utilisateur n'a
rien à faire et n'a qu'un fichier : `build.rs` compile le binaire musl dans le
`.exe` d'après `CLAUDHUB_EMBED_SERVER`, et l'installation se fait en écrivant
ces octets **dans l'entrée standard** d'un `cat` lancé là-bas — ni partage
réseau, ni chemin à traduire, ni bit d'exécution perdu par un zip.

Il a d'abord été *livré à côté* de l'exécutable, ce qui marchait et laissait
deux fichiers dans une archive dont un que personne ne savait quoi faire ;
surtout, rien n'empêchait de garder un vieux serveur à côté d'une interface
neuve — la poignée de main l'aurait refusé, mais c'est une panne qu'on peut
supprimer au lieu de la diagnostiquer. Le chemin voisin reste en **repli**
(`wsl::bundled_server`), pour le build de développement, qui n'a pas de
binaire musl à embarquer.

Six points qui ne se devinent pas :

- **`build.rs` écrit une constante, il ne lit rien à l'exécution.** Sans la
  variable, `EMBEDDED` vaut `None` et tout se passe comme avant ; avec, elle
  vaut les octets. Une variable posée sur un chemin qui n'existe pas est une
  **erreur de compilation** et non un repli silencieux : elle a été posée
  exprès, et un exécutable livré sans son serveur est précisément ce qu'on
  cherche à rendre impossible. Le chemin est écrit par `{:?}`, qui rend un
  littéral échappé — sous Windows il est plein d'antislashs, et les recopier
  tels quels donnerait des séquences d'échappement au milieu du chemin.

- **L'installation est adressée par le contenu**, jamais par un numéro de
  version : l'empreinte du binaire nomme son dossier
  (`~/.claudhub/bin/<empreinte>`). Une mise à jour s'installe donc d'elle-même,
  deux `.exe` différents cohabitent, et une version de développement — qui n'a
  pas de numéro — se comporte comme les autres. C'est le motif de
  `tools/make_appimage.sh`, et la purge garde le dossier courant.
- **Rien ne passe par un shell de connexion.** `wsl.exe --exec` lance le
  programme directement : ce qui s'y écrit est un chemin absolu, jamais un `~`
  que personne ne développerait. D'où `wsl::probe`, qui demande une fois pour
  toutes où est le foyer de l'utilisateur **et quel shell lui appartient** —
  ce dernier parce qu'un terminal ouvert par `--exec` n'a pas de shell pour
  interroger `$SHELL`, et que c'est justement un shell qu'on veut y lancer.
- **Le script d'installation n'a pas un seul guillemet**, délibérément : la
  ligne traverse `CreateProcess` puis la reconstruction d'`argv` par
  `wsl.exe`, et chaque guillemet y est une occasion de se faire manger. Un
  test le verrouille.
- **`wsl.exe --list` répond en UTF-16** sur les versions d'avant `WSL_UTF8` ;
  lu comme de l'UTF-8, cela donne un nom sur deux caractères. `wsl::decode`
  gère les deux, et un test le vérifie sur un nom accentué.
- **Le manche reste vide tant que le serveur n'a pas répondu**
  (`HandleInner::Pending`), plutôt que de retomber sur les workers locaux :
  sous Windows, ceux-ci feraient travailler `git.exe` sur des chemins qui
  n'existent pas, en silence et à côté de la plaque. Les commandes émises
  avant sont jetées — la vue les repose d'elle-même.

**Le fil ne transporte que des chemins Linux**, et la traduction n'existe
qu'aux quatre endroits où un chemin change de monde : le sélecteur de dossier
(`\\wsl.localhost\…` ou `C:\…` → `/…`, en refusant le dépôt d'une *autre*
distribution, qui s'ouvrirait vide) ; le coffre de notes (`notes_dir` rend un
chemin du serveur, un coffre déjà pointé sur `/home/…` passant tel quel) ; la
cible d'un export CSV, choisie ici et écrite là-bas ; et les deux retours —
l'ouverture du coffre dans l'explorateur, le chemin d'un export qu'on
annonce — qui refont le chemin inverse, sans quoi l'utilisateur lirait
`/mnt/c/…` d'un fichier qu'il ira chercher dans son explorateur. `wslpath`
est pur et testé sous Linux.

**Les mêmes réglages, deux mondes.** `settings.json` et `state.json` restent
côté Windows — c'est l'état de cette fenêtre-là — mais contiennent des chemins
Linux, puisque c'est ce que le fil transporte. La liste de shells du
formulaire et le shell de connexion des terminaux viennent, eux, du serveur :
deux statiques dans `settings.rs`, parce que le formulaire déclare ses champs
par des fermetures qui ne reçoivent qu'un `App`.

Toute écriture git est suivie d'une relecture du statut (`write_then_refresh`),
pour que la vue n'ait pas à savoir quelle commande touche quoi.

### Pourquoi le binaire `git` et non libgit2

Les credential helpers, `includeIf`, les hooks, la signature, les alias :
l'utilisateur attend *sa* configuration. Une réimplémentation en couvrirait la
moitié. Le coût est un `fork` par commande, invisible à cette échelle.

Corollaires à respecter : `stdin` fermé et `GIT_TERMINAL_PROMPT=0` (sinon une
invite de mot de passe bloque un worker pour toujours), `LC_ALL=C` pour que les
messages d'erreur soient reconnaissables, et les formats `-z` partout où un
chemin apparaît — un fichier peut contenir un saut de ligne.

### La surveillance de fichiers

Deux règles, et les enfreindre se paie en fenêtre figée puis en rafraîchissement
en boucle.

**Ce qu'on surveille vient de `git ls-files --cached --others
--exclude-standard`** : les dossiers contenant un fichier suivi ou un fichier
nouveau non ignoré, chacun **sans récursion**. Surveiller le worktree en bloc
prend un appel système par répertoire, et un projet Laravel en a quarante mille
dont sept cents contiennent du code — le reste est `vendor/`, `node_modules/`
et surtout `storage/`, que Laravel n'ignore pas dossier par dossier et qu'un
serveur de développement réécrit sans arrêt. Chacune de ces écritures
produisait un réveil, donc un `git status`, donc un rechargement de la revue.
Un dossier créé plus tard est signalé par son parent, ce qui suffit à
déclencher le rafraîchissement qui le découvrira.

**On ne réagit qu'aux événements qui changent le contenu**
(`watch::changes_content`). inotify signale chaque **ouverture** de fichier —
`Access(Open)` — et c'est nous qui les ouvrons : `git status` lisait le
worktree, chaque lecture produisait un événement, chaque événement déclenchait
un `git status`. Une boucle à plein régime, quelques centaines de `git status`
par minute, invisible tant que la liste ne se vidait pas entre deux réponses —
et devenue un clignotement le jour où elle s'est vidée. Les métadonnées sont
écartées pour la même raison ; `Any` et `Other` sont gardés, c'est ainsi que
`notify` signale un débordement de sa file.

Dans le même esprit, toutes les commandes git tournent avec
`GIT_OPTIONAL_LOCKS=0` : `git status` rafraîchit sinon le cache de `stat` qu'il
garde dans `.git/index`, qui est justement l'un des fichiers surveillés.

**Poser les surveillances ne se fait jamais dans le thread d'interface** :
c'était une demi-seconde de fenêtre figée à chaque changement de worktree.
La vue envoie `Cmd::Watch`/`Cmd::WatchDir`, que `Handle::send` remet
directement au thread du surveillant — la sixième voie, hors des files : la
pose est déjà différée, la faire attendre derrière un diff n'aurait pas de
sens. Ce qui en revient est un `Evt::FilesChanged`, un **lot** de chemins par
fenêtre de regroupement, sur le même canal que tout le reste — un seul flux à
faire passer sur un fil, local ou distant. Le surveillant vit dans le runtime
et non dans la vue : c'est le disque du worktree qu'il regarde, et ce disque
est celui du serveur quand les workers tournent dans WSL. Corollaire à
connaître : la surveillance n'est pas effective au retour de l'appel — sans
importance, puisque la sélection d'un worktree déclenche de toute façon une
lecture du statut.

**Sur un disque Windows monté par WSL, la surveillance ne marche pas et ne le
dit pas.** `notify` pose ses surveillances sur drvfs (`/mnt/c`) sans erreur et
ne livre jamais un événement : les écritures ont lieu côté Windows, le noyau WSL
n'a rien à traduire. C'est le seul échec silencieux de cette couche, d'où
`watch::on_windows_filesystem`, que la barre d'état affiche. Pas de repli par
sondage : `git status` coûte déjà plusieurs fois plus cher sur ces montages, et
le mettre sur un minuteur ferait payer en permanence ce que déplacer le dépôt
vers `~` supprime d'un coup.

Dans un worktree lié, `.git` est un *fichier* qui pointe vers
`<principal>/.git/worktrees/<nom>` : c'est là que vivent son `HEAD` et son
`index`, et les surveiller au mauvais endroit revient à ne rien surveiller.

### La vue de diff

**La liste des fichiers est virtualisée elle aussi** : une revue de branche en
touche couramment plusieurs centaines, et reconstruire autant de lignes — deux
boutons chacune — à chaque frame suffit à faire tomber l'interface à quelques
images par seconde.

Un diff de relecture d'agent fait couramment plusieurs milliers de lignes.
L'affichage repose donc sur `uniform_list` (gpui), et non sur le
`v_virtual_list` de gpui-component : toutes les entrées ont exactement la même
hauteur, `uniform_list` trouve l'intervalle visible par une division au lieu de
parcourir un vecteur de tailles, et surtout c'est le seul des deux qui sache
défiler horizontalement. La seule exception est la vue à deux colonnes
repliée, où les entrées n'ont plus la même hauteur et où il n'y a plus rien à
défiler en largeur — voir « Le repli des lignes longues ».

Quatre contraintes tiennent ensemble, et en relâcher une casse une autre :

- **`.h(LINE_HEIGHT)` explicite** sur chaque entrée. Une hauteur mesurée
  dépendrait du texte, et la liste réserve la hauteur d'un seul item mesuré.
- **`.whitespace_nowrap()`**. Sans cela le texte est shapé à la largeur du
  viewport pendant le rendu réel mais à largeur infinie pendant la mesure : les
  lignes longues passent sur deux lignes et débordent de la place réservée.
- **`ListHorizontalSizingBehavior::Unconstrained` + `with_width_from_item`**.
  La largeur défilable vient d'un seul item ; `Rendered::longest_row` désigne
  la ligne la plus large, sans quoi le défilement s'arrête à la largeur de la
  première ligne du fichier — presque toujours courte.
- **pas de `w_full` sur une entrée**, mais un `min_w(content_width)`. `w_full`
  étire l'entrée à la largeur disponible et il n'y a plus rien à révéler ;
  `min_w` laisse la largeur intrinsèque remonter tout en garantissant que le
  fond coloré d'une ligne modifiée traverse toute la vue.

Tout ce qui se déduit d'un diff — mise à plat, coloration, patchs
d'indexation, largeur de gouttière — est calculé une fois dans
`diff_view::Rendered`, à l'arrivée du diff, et rangé derrière un `Rc`. La
fermeture de rendu est appelée pour chaque ligne visible à chaque frame,
animation de molette comprise : elle ne doit rien y calculer.

**Deux colonnes, une seule référence.** La vue côte à côte (`SplitRow`,
`split_rows`) n'est qu'un autre agencement de la liste unifiée : ses entrées ne
portent que des **indices dans `rows`**, jamais du texte ni des styles à elles.
La copie ramène donc toujours la sélection à la liste unifiée
(`unified_span`), qui seule porte l'ordre du fichier — appariées, une
suppression et l'ajout qui lui répond tiennent sur une même entrée, et il faut
bien décider laquelle vient d'abord. Conséquence à ne pas oublier : les indices
de `diff_selection` désignent la liste **affichée**, donc basculer de mode
abandonne la sélection.

Sans repli, chaque colonne est taillée pour la plus longue ligne **du
fichier** : le défilement horizontal emmène alors les deux colonnes ensemble et
garde les versions en regard. C'est aussi ce qui rendait la vue pénible, une
seule ligne longue donnant sa largeur à tout le fichier — d'où le repli, qui
est le défaut.

### Le repli des lignes longues

`Settings::diff_wrap`, vrai par défaut, et **en deux colonnes seulement** :
c'est là que ça se joue, une colonne ne faisant que la moitié de la vue. En une
seule colonne la ligne dispose de toute la largeur, et le repli n'aurait pas de
quoi se justifier.

**Le repli se fait à la colonne, comme dans un terminal, et non aux espaces.**
C'est ce qui rend la hauteur d'une entrée calculable **avant** de la peindre :
la police du diff est à chasse fixe, un caractère vaut une colonne, et
`wrapped_lines` est une division. Le shaper de gpui, lui, coupe aux mots — une
hauteur devinée qui ne tombe pas juste laisserait les entrées se recouvrir,
puisqu'une liste virtualisée réserve exactement ce qu'on lui annonce.

Cinq points qui ne se devinent pas :

- **`v_virtual_list` remplace `uniform_list` ici, et là seulement.** Les
  entrées n'ont plus la même hauteur — une ligne longue en occupe trois, celle
  d'en face une seule —, et c'est le seul cas où parcourir un vecteur de
  tailles vaut son prix. Le vecteur est reconstruit à chaque frame : il ne
  coûte qu'une division par entrée, `Rendered::row_chars` ayant déjà compté les
  caractères de chaque ligne une fois pour toutes. C'est ce champ qui évite de
  reparcourir le texte du fichier à chaque changement de largeur — un
  glissement de séparateur en produit un par image.
- **Une seconde poignée de défilement** (`diff_wrap_scroll`) : `v_virtual_list`
  ne sait défiler qu'avec la sienne. Les deux listes n'étant jamais affichées
  ensemble, tout ce qui vise « la » liste passe par `diff_base_handle` et
  `reveal_diff_row`, qui choisissent — viser la mauvaise ferait défiler une
  liste qui n'est pas là.
- **Une paire fait la hauteur de sa plus haute moitié.** Les deux versions
  restent en regard, ce qui est tout l'intérêt de cette vue ; la moitié la plus
  courte complète avec des lignes vides.
- **Le texte est découpé, ses styles avec.** `char_span` et `slice_runs`
  ramènent la coloration et les surlignages de recherche au début de chaque
  tranche. Le découpage se compte en **caractères** — en octets, une ligne
  accentuée se couperait une colonne trop tôt et au milieu d'un caractère, ce
  qui panique — et les plages rendues restent **triées et disjointes**,
  l'invariant que gpui ne vérifie pas.
- **La marge de note appartient à l'entrée, pas aux colonnes.** L'oublier fait
  déborder la ligne de trois pixels, et le repli ayant supprimé la barre
  horizontale, rien ne le révélerait.
- **La largeur mesurée n'existe pas à la première frame**, et rien ne
  redessine tout seul. Les bornes d'une vue ne valent quelque chose qu'une
  fois la mise en page faite : au tout premier diff, le repli calculerait ses
  colonnes sur zéro, et l'affichage resterait faux jusqu'au prochain
  événement — le balayage de fond, deux secondes plus tard. D'où
  `window.request_animation_frame` tant que la mesure manque, borné à quelques
  frames pour qu'un panneau rétréci à zéro ne fasse pas tourner l'interface à
  plein régime, et `ClaudhubApp::diff_width` qui retient la dernière largeur
  connue — c'est elle qui fait que les diffs suivants s'ouvrent d'emblée à la
  bonne largeur.

Ce que le repli ne corrige pas : `page_rows` (Ctrl+D/U) compte des entrées et
non des lignes visibles, donc une page déborde un peu quand les lignes se
replient.

**« Tout le fichier » est un contexte, pas un mode.** `git diff` n'a pas
d'option pour cela : `Settings::context_lines` demande un contexte plus grand
que n'importe quel fichier (`WHOLE_FILE_CONTEXT`), que git ramène de lui-même à
ce qui existe. Basculer relit donc le fichier — les lignes élidées ne sont
nulle part en mémoire.

**La barre du diff porte les quatre mêmes déplacements en boutons** : bloc
précédent et suivant, fichier précédent et suivant. Les flèches appartiennent à
qui a le focus, donc à personne après un clic dans un terminal ; les boutons
sont toujours là, et leurs infobulles nomment les touches pour qui préfère ne
pas lâcher le clavier. C'est le **même code** dessous (`step_diff_hunk`,
`step_file`) : deux façons de faire un geste qui n'aboutiraient pas au même
endroit seraient une de trop.

**Les flèches sont les seules touches qui ne passent pas par la touche
système**, et c'est ce qui les rend délicates : elles appartiennent d'abord à
qui a le focus. `NAVIGATION_PREDICATE` exclut donc les champs de saisie, les
terminaux et les couches flottantes, exactement comme la copie.

**Aller au hunk suivant défile toujours, et dit où l'on est.** Deux choses qui
vont ensemble, et qui manquaient toutes les deux :

- `scroll_to_item` de gpui **ne fait rien quand la cible est déjà à l'écran**,
  ce qui est juste pour une flèche qui descend d'une ligne — la vue ne doit pas
  sauter sous l'œil — et faux pour un pas de hunk : la touche déplaçait la
  sélection de quatre cents lignes et la vue pas du tout. Le hunk demandé va
  donc **en haut de la vue** (`reveal_diff_row_strict`), là où on le lit. La
  liste repliée n'a pas de variante stricte — `scroll_strict` est codé en dur
  dans gpui-component —, d'où les deux temps : on remet la liste en haut, la
  cible est alors forcément sous la vue, et la stratégie `Top` doit la
  remonter. Les deux ont lieu avant la peinture, rien ne clignote.
- **Un filet dans la marge marque le hunk courant**, toutes ses lignes
  comprises. Sans lui, une touche qui ne déplace rien de visible se lit comme
  une touche morte. C'est une **bordure** et non une bande enfant : elle est
  hors du rembourrage, donc au même endroit sur un en-tête, qui en a, et sur
  une ligne, qui n'en a pas ; toujours présente, transparente quand ce n'est
  pas le hunk courant — une largeur qui apparaît décalerait toute la ligne.
  `Rendered::hunk_of` dit à quel hunk une entrée appartient, dans les deux
  dispositions, et il est calculé une fois par frame et non par ligne.

Haut/bas vont d'une **modification** à la suivante — c'est le geste de la
relecture, les lignes de contexte entre deux hunks n'ayant rien à montrer — et
**débordent sur le fichier voisin** une fois le dernier hunk passé. La touche
système descend à la ligne, Maj étend la sélection ; gauche/droite changent de
fichier directement. L'ordre des fichiers est celui qui est **affiché**, celui
que l'œil suit, replis compris, et la revue bute à ses deux bouts plutôt que de
boucler.

**La liste suit le fichier ouvert.** Elle a une poignée de défilement **par
domaine** — « Revue » et « Modifications » sont affichés en même temps, et une
seule poignée les ferait défiler ensemble —, et `reveal_file` l'amène sur le
fichier à chaque ouverture. L'indice est celui de la liste *affichée*, dossiers
compris et sans ce qu'un repli cache : c'est cette liste-là que la vue
virtualise. Le défilement est non strict, si bien qu'un clic sur un fichier
déjà visible ne fait pas sauter la liste sous les yeux ; seule une flèche qui
change de fichier la déplace vraiment.

Un débordement ne peut pas poser lui-même la sélection : le diff du fichier
voisin n'arrive qu'après la commande git. Le geste est donc noté
(`ReviewState::pending_jump`) et consommé à l'arrivée du diff — par le premier
hunk en descendant, par le dernier en remontant, là où la lecture s'arrête. Ce
drapeau est réservé au clavier : ouvrir un fichier à la souris l'efface, sans
quoi un clic hériterait d'un saut armé plus tôt.

### Une seule liste pour l'index et les modifications

`DiffRange` n'a plus de `Unstaged` ni de `Staged` : la distinction est un
détail de plomberie git, et la vue la restitue par **une case à cocher par
fichier** plutôt que par deux listes qu'il faut recoudre mentalement. Cocher
appelle `git add`, décocher `git restore --staged`, et ce qui est coché part au
commit.

Deux endroits où cette simplification pourrait mentir, et ce qui l'en empêche :

- **L'indexation partielle** (`MM`). La case seule laisserait croire que tout
  le fichier part. La ligne affiche donc les deux codes de git et la mention
  « partiel » ; `FileRow::partial` est ce qui la déclenche, et un test la
  verrouille.
- **Les fichiers non suivis**, dont cocher ne veut pas dire la même chose que
  pour un fichier déjà suivi : ils forment leur propre groupe.

**Un bouton d'une ligne consomme son clic.** La ligne entière est cliquable —
elle ouvre le fichier et **reprend le focus**, pour que les flèches reviennent à
la revue — et la case à cocher, la coche de relecture et la corbeille sont ses
enfants. Sans `stop_propagation`, cocher ouvrait aussi le fichier, et surtout la
corbeille était inutilisable : le dialogue de confirmation s'ouvrait, prenait le
focus, puis le clic finissait de remonter et la ligne le lui reprenait — Échap
et les deux boutons du pied de page envoient tous une action, et une action se
distribue sur le nœud qui a le focus. Le dialogue restait ouvert et sourd. C'est
la même règle que celle des lignes de menu, et le même piège : ce qui casse
n'est pas le geste qu'on vient de faire mais celui d'après.

La liste est une **arborescence de dossiers**, repliable, comme celle de
PhpStorm — un bouton bascule vers la liste plate, et le choix est persistant.
Trois points la font tenir :

- **Les dossiers sans embranchement sont fusionnés** : `app/Http/Livewire/Forms`
  tient sur une ligne. Sans cela, un projet Laravel coûte six niveaux
  d'indentation avant le premier fichier.
- **La liste plate reste la référence.** L'arbre n'est qu'un affichage :
  `tree_rows` la transforme, mais le compte des fichiers indexés et les cases
  des groupes travaillent sur elle. Un fichier caché par un dossier replié
  compte quand même.
- **La case d'un dossier porte tout son sous-arbre**, y compris ce que le repli
  cache — d'où `DirRow::paths`, calculé à la construction de l'arbre. Cocher un
  dossier fermé doit indexer ce qu'il contient, pas ce qu'on en voit.

Corollaire à connaître : le diff affiché va de HEAD au répertoire de travail,
index compris. Indexer un hunk isolé sur un fichier *déjà partiellement
indexé* peut donc échouer — `git apply --cached` refuse un patch qui ne
s'applique pas —, et le message le dit.

### Proposer un message de commit

Le bouton à côté de « Valider » donne le diff indexé à un agent et met sa
réponse dans le champ. `src/commit_msg.rs` en tient tout ce qui se teste — le
prompt et le nettoyage de la réponse —, le reste est un sous-processus.

**Un programme, pas une API.** C'est la décision de cadrage de Claudhub, la
même que pour l'agent du terminal : `Settings::commit_message_command`, par
défaut `claude -p --model sonnet`, est une ligne de commande que l'utilisateur
a déjà installée et authentifiée. Une clé d'API et un client HTTP à nous
auraient leur propre authentification, leurs propres quotas et leur propre
format d'erreur, pour rédiger une ligne de résumé. Le réglage vide fait
disparaître le bouton plutôt que d'offrir un geste qui échouera.

Six points qui ne se devinent pas :

- **Le diff part par l'entrée standard**, jamais en argument : une ligne de
  commande a une longueur maximale, environnement compris, et un diff de
  relecture d'agent la frôle. Les trois flux passent par des threads, comme
  dans `git::wait_with_timeout` et pour la même raison — un tube plein bloque
  celui qui écrit, et attendre la fin avant de lire est l'interblocage
  classique.
- **Les sujets des derniers commits partent avec** (`history::recent_subjects`).
  La convention d'un dépôt ne se devine pas : la langue d'abord, mais aussi la
  personne du verbe et les préfixes que l'équipe s'est donnés. Une consigne
  écrite dans le prompt les imposerait à tous les dépôts.
- **Le diff est tronqué à `MAX_DIFF`**, sur une frontière de caractère : en
  octets nus, un diff accentué se couperait au milieu d'un caractère et la
  tranche ne serait plus de l'UTF-8. La coupe est dite dans le prompt.
- **La réponse est nettoyée avant d'entrer dans le champ** (`clean`). Un modèle
  encadre volontiers sa réponse d'un bloc de code ou de guillemets malgré la
  consigne, et ce sont des caractères qui finiraient tels quels dans
  l'historique du dépôt.
- **File réseau, et un délai à lui.** Un agent qui rédige met dix à trente
  secondes : c'est le profil qui a fait sortir le réseau de la file des
  lectures. Le délai de `git` — trente secondes — serait ici un échec quasi
  certain, d'où les deux minutes de `commit_msg::TIMEOUT`.
- **Le message retrouve le champ qui l'a demandé.** `Evt::CommitMessage` porte
  son worktree, et `ClaudhubApp::suggesting_message` retient lequel attend : on
  change de worktree pendant les vingt secondes d'attente, et poser ce
  message-là dans le champ d'un autre serait le pire des services.

### L'explorateur de projet

**L'arbre vient d'un seul appel git** — `ls-files --cached --others
--exclude-standard` —, jamais d'un parcours de disque. C'est déjà ce que fait
la surveillance de fichiers, et pour la même raison : un projet Laravel a
quarante mille répertoires, et les ouvrir un par un coûterait un appel système
chacun pour arriver aux sept cents qui portent du code.

**`ui::tree` ne connaît que des chemins et rend des indices.** Deux listes s'en
servent — la revue et l'explorateur — et elles n'affichent pas les mêmes
choses : cases à cocher et volumes d'un côté, statut git de l'autre. Rendre des
indices plutôt que des valeurs n'est pas un détail : la même feuille apparaît
dans le sous-arbre de chacun de ses dossiers parents, et un explorateur de
quarante mille fichiers ferait sinon des centaines de milliers de clones de
`PathBuf` par reconstruction.

**L'arbre de l'explorateur est construit une fois**, à l'arrivée de la liste et
à chaque repli, et rangé derrière un `Rc`. La liste de revue, elle, se
reconstruit au rendu : quelques centaines d'entrées le permettent, des dizaines
de milliers non.

**Il se parcourt au clavier**, comme celui de PhpStorm : haut et bas d'une
ligne à l'autre de la liste *affichée*, droite pour déplier, gauche pour
replier ou remonter au dossier parent, Entrée pour ouvrir. D'où un contexte à
lui, `ClaudhubExplorer`, que `NAVIGATION_PREDICATE` exclut : les flèches nues
appartiennent sinon à la relecture du diff, et deux jeux de liaisons sur la
même touche ne se départageraient pas. Corollaire : un clic dans la liste de
revue **reprend le focus**, sans quoi les flèches continueraient de parcourir
l'arbre après qu'on a ouvert un fichier ailleurs.

**Le curseur est un chemin, pas un indice.** L'arbre se reconstruit à chaque
repli, à chaque frappe de recherche et à chaque relecture de la liste : un
indice y désignerait une autre ligne d'une fois sur l'autre. Le chercher coûte
un parcours par geste, ce qu'un geste peut payer et pas une frame.

**Ouvert et sous le curseur sont deux choses**, et se voient différemment : on
parcourt l'arbre au clavier sans quitter le fichier qu'on relit, et ne montrer
que l'un des deux perdrait l'autre.

**Les filets d'indentation ne sont pas une décoration** : à six niveaux — le
cas courant sur un projet Laravel — plus rien ne dit à quel dossier une ligne
appartient. Ils imposent une hauteur de ligne explicite (`theme::row_height`),
les filets étant en `h_full`.

`reveal_open_file` est le « scroll from source » de PhpStorm, et il n'est
**pas** automatique : une liste de quarante mille entrées qui saute toute seule
à chaque clic dans la revue est un mouvement de trop. Il déplie les ancêtres —
il suffit de les nommer, une chaîne fusionnée restant un ancêtre de ce qu'elle
contient.

**L'arbre s'ouvre fermé, et c'est une polarité et non une graine.** Les deux
listes qui se servent de `ui::tree` ne partent pas du même endroit : une revue
fait quelques dizaines de fichiers et se lit grande ouverte, donc elle retient
ce qu'on a **fermé** ; l'explorateur porte le worktree entier, où tout déplié
est une liste que personne ne parcourt, donc il retient ce qu'on a **ouvert**.
D'où `tree::Folds`, `OpenBut` et `ShutBut`, et un test par polarité.

Retenir l'exception et non l'état est ce qui garde les deux ensembles petits :
amorcer celui de l'explorateur avec tous les répertoires du projet en ferait un
ensemble de la taille de l'arbre, reconstruit à chaque repli — et fermer
seulement le premier niveau ne marcherait pas non plus, puisque déplier `app/`
cascaderait alors jusqu'aux feuilles. Corollaire pour les gestes : « tout
replier » n'est plus un parcours mais un `clear`, et ce sont « tout déplier
ici » et « tout replier ici » qui ont échangé leurs corps.

**Ce que `.gitignore` écarte est montré, mais montré en gris** — la convention
de PhpStorm, et la seule chose qui empêche `vendor/` de se lire comme une
partie du projet. `Settings::show_ignored_files` est donc vrai par défaut. Le
prix est dit dans l'aide du réglage et ne se devine pas : la liste change
d'ordre de grandeur, un projet Laravel passant de quelques centaines de
fichiers à ce que `node_modules/` et `vendor/` contiennent. La surveillance de
fichiers, elle, n'en voit rien — c'est un autre appel, et il garde son
`--exclude-standard`.

Trois points sur le grisé :

- **git dit lesquels, on ne le devine pas.** `--ignored` est un *filtre* et non
  un ajout, d'où les deux appels de `repo::list_files` — qui rend désormais les
  deux listes plutôt que leur union seule. La seconde est **triée**, et c'est
  par recherche binaire qu'on l'interroge.
- **Un dossier n'est gris que si tout ce qu'il porte l'est.** `vendor/` l'est,
  `app/` avec un fichier de log ignoré dedans ne l'est pas — griser un dossier
  qui contient du code qu'on cherche serait un contresens.
- **Le calcul se fait à la reconstruction de l'arbre, jamais au rendu.** Une
  feuille coûte une recherche binaire, mais un dossier demande de vérifier
  toutes ses feuilles, et `vendor/` en porte trente mille. `Explorer::dimmed`
  est un drapeau par ligne, calculé avec les lignes ; c'est un prix qu'un geste
  paie et qu'une frame ne peut pas.

### L'icône d'un fichier

Trois listes en portent — l'explorateur, les modifications, la revue de branche
— parce que le geste central de Claudhub est de parcourir des listes de
fichiers.

**Un glyphe par langage, pas un glyphe par famille.** Les icônes de Lucide
livrées avec Claudhub ne connaissent que des catégories : tout le code y serait
le même `file-code`, et une revue Laravel — du PHP, des vues Blade, du Vue, du
CSS, du SQL, trois fichiers de configuration — n'y gagnerait rien. Les marques
viennent donc de **simple-icons** (CC0), rangées dans `assets/icons/lang/` : un
dossier à part, parce qu'elles n'ont ni la même licence ni le même dessin — les
nôtres sont des traits, celles-ci des aplats. Les marques restent la propriété
de leurs titulaires ; ce sont des repères visuels, pas une revendication.

**La teinte vient de la coloration syntaxique du thème**, pas d'une palette à
nous et **pas des couleurs de marque**. Les noms de style existent dans tous
les thèmes livrés — `keys_of` le verrouille —, ils s'accordent au diff affiché
à côté, et ils suivent l'apparence claire ou sombre sans qu'on s'en occupe. Une
couleur de marque figée, elle, disparaît sur la moitié des thèmes : un logo
noir sur fond sombre est un trou. La teinte n'a donc **aucun sens sémantique** :
un `.rs` n'est pas « un type ». C'est une convention d'affichage, et un thème
qui ne définirait pas un nom de style retombe sur le suivant de la liste, puis
sur la couleur du texte.

Quatre passes, de la plus précise à la plus large : le **nom entier**
(`Dockerfile`, `.gitignore`, et surtout `package.json` qui est de npm avant
d'être du JSON, `Cargo.toml` qui est de Rust avant d'être du TOML), les
**familles d'outils** qui se déclinent (`.eslintrc`, `.eslintrc.json`,
`eslint.config.js`), les **doubles extensions** (`.blade.php` est du balisage
avant d'être du PHP, comme le dit déjà la coloration), puis l'**extension**.

Les deux grandes tables sont **triées et interrogées par recherche binaire**,
et un test vérifie qu'elles le sont : un ordre cassé ne provoque aucune erreur,
il fait rater des entrées, et une entrée ratée n'est qu'une icône générique de
plus — ce que personne ne remarque. Un autre test vérifie que chaque icône
nommée est sur le disque **et embarquée** : les marques vivent dans un
sous-dossier que le motif de `rust-embed` doit couvrir, et un fichier présent
mais non embarqué donnerait une case vide, seulement en release.

Trois glyphes manquaient au jeu de Lucide — `file-code`, `file-json`,
`database` — et sont redessinés sur sa grille.

### Le serveur de langage

L'éditeur intégré sait désormais compléter, survoler, aller à la définition et
souligner ce qu'un serveur de langage lui reproche. **Presque rien de tout cela
n'est écrit ici** : l'éditeur de gpui-component porte déjà un `Lsp` — trois
providers et un `DiagnosticSet` — avec les popovers, les soulignements et la
gouttière qui vont avec. Ce qui manquait était le **client**, et le chemin qui
va d'un provider appelé dans une frame à un worker qui répond trente
millisecondes plus tard.

**Un bouton « LSP » par worktree, éteint par défaut.** Un serveur tient des
centaines de mégaoctets sur un gros projet, et douze worktrees ouverts ne sont
pas douze worktrees qu'on édite. L'état vit dans le magasin (`state.json`) et
non dans les réglages : c'est où l'on en est, comme la base de comparaison. Le
bouton est dans la **barre de l'éditeur** et non dans le menu du worktree — une
action va où se fait le geste dont elle est la fin, et c'est là qu'on lit son
effet. Il porte quatre états et non deux : éteint, en démarrage, prêt — avec le
compte de ce que le serveur a trouvé — et en échec, avec la raison en infobulle.
Un serveur qui ne démarre pas doit le dire ; il n'a pas à ressembler à un serveur
qui réfléchit. Et il n'apparaît **que si quelque chose sert ce fichier** : un
bouton qui ne sait répondre que « pas de serveur pour un Markdown » est pire que
pas de bouton.

**Qui sert quoi.** Les serveurs se déclarent dans le `wt.toml` du projet
(`[lsp.<nom>]`, premier niveau du système d'extension) **et** dans les réglages
(`Settings::lsp`, deuxième niveau) ; le projet l'emporte pour un même langage —
la machine dit ce qu'elle a installé, le projet dit ce que son code veut. Le
fichier ouvert choisit : `lsp::pick` prend la **plus longue extension qui
correspond**, si bien qu'une vue Blade va à l'entrée qui dit `blade.php` et non
à celle qui dit `php`.

**Trois entrées sont livrées** — PHPantom sur PHP et Blade, rust-analyzer sur
Rust, typescript-language-server sur TypeScript et JavaScript : les langages de
ce dépôt et de ceux qu'on y relit, et rien de plus. Aucun binaire n'est
**téléchargé** ; ce sont des programmes que l'utilisateur installe, comme les
agents du terminal, et un serveur absent le dit sur son bouton plutôt que de
faire chercher cent mégaoctets à une interface.

**Le `languageId` n'est pas l'extension**, et c'est le piège que PHP cachait :
`php` est son propre nom, si bien que deviner à partir du suffixe marchait
partout où on regardait. Un `.ts` annoncé `ts` est un document que
typescript-language-server refuse, et `.tsx` est un **autre langage** que `.ts`
pour le protocole. D'où `lsp::language_of`, une table des identifiants usuels
que `language_for` consulte quand la déclaration n'en dit rien — c'est aussi ce
qui permet à une seule entrée d'en servir quatre. Une déclaration peut toujours
le dire elle-même, `language_id` l'emporte, et ce que la table ignore retombe
sur l'extension, ce qui n'est pas pire qu'avant.

**Une session par worktree**, et ouvrir un fichier d'un autre langage la
remplace : un worktree est un projet, et deux serveurs répondraient deux fois à
la même question.

Sept points qui ne se devinent pas :

- **C'est une voie, pas une file.** Un serveur de langage vit des heures, tient
  un état — quels documents sont ouverts, à quelle version — et pousse des
  messages qu'on n'a pas demandés. Il est donc remis directement à son hôte par
  `Handle::send`, comme les ordres du surveillant, et pour une raison de plus :
  **l'ordre compte**. Un `didChange` doit atteindre le serveur avant la
  complétion qui en dépend, ce que plusieurs workers sur un canal partagé ne
  promettent pas.
- **Un seul propriétaire par session.** Le fil de session reçoit les ordres de
  la vue et les messages du serveur sur le même canal : pas de verrou, et aucun
  moyen d'écrire une requête au milieu d'une autre. Le processus meurt avec la
  session, toujours — un serveur oublié derrière un worktree qu'on ne regarde
  plus est une fuite que rien d'autre ne rattraperait.
- **Du JSON brut sur le fil.** `lsp-types` est sous la feature `ui` — un module
  du cœur qui le tirerait casserait le build du serveur — et postcard,
  positionnel, ne sait pas relire un `Value`. Les `Cmd`/`Evt` portent donc des
  chaînes, la vue seule type ce qu'elle lit, et les deux bouts du fil n'ont pas
  à s'accorder sur une version du crate. `PROTOCOL_VERSION` passe à 4.
- **Une requête ne reste jamais en attente.** Un provider rend une `Task` ;
  celle-ci se résout par un canal à un coup (`async_channel::bounded(1)`) rangé
  sous un identifiant qui ne recule jamais — le motif de la console SQL. Rien
  ne l'y laisse : le cœur expire une requête au bout de quinze secondes, la
  mort d'une session échoue tout ce qu'elle portait, et une complétion est
  **annulée** (`$/cancelRequest`) par celle qui la remplace. Une `Task` que
  personne ne résout, c'est un popover qui tourne pour toujours.
- **L'édition est calculée, pas devinée.** Le serveur dit s'il veut le document
  entier ou des plages ; pour les plages, `lsp::sync` prend le préfixe et le
  suffixe communs des deux textes. L'unité est le **code UTF-16**, ni l'octet ni
  le caractère : un `é` vaut une colonne pour deux octets, un emoji deux
  colonnes pour quatre — compter les octets décale toute ligne accentuée,
  compter les caractères décale tout emoji. Et les changements sont **groupés**
  (120 ms) : une frappe émet un événement, et envoyer chacun d'eux ferait
  cinquante allers-retours pour une ligne tapée.
- **Les providers ne sont posés que pour ce que le serveur sait faire**, ce que
  dit sa poignée de main. Un provider de survol posé sur un serveur qui n'en a
  pas, c'est un aller-retour par pause du pointeur pour s'entendre dire non. Ils
  sont donc posés une seconde fois à l'arrivée des capacités, la première ayant
  eu lieu avant qu'on les connaisse.
- **Un serveur qui demande quelque chose doit être servi.** `client/register-`
  `Capability`, `workspace/configuration`, la création d'une barre de
  progression : chacune a une réponse qui ne coûte rien, et le reste reçoit
  l'erreur que la spécification prévoit. Ne rien répondre est la seule chose qui
  ne doit pas arriver — un serveur qui attend son accusé de réception s'arrête
  là.

**Les actions de code sont là aussi** — `Ctrl+.` ou le clic droit, gpui-base les
liant déjà —, et trois choses les font marcher qu'un client naïf rate :

- **Une action se résout avant de s'appliquer.** Un serveur a le droit de rendre
  des titres et de ne calculer la modification que pour celui qu'on choisit :
  c'est ce que veut dire un `data` sans `edit`, et c'est exactement ce que
  PHPantom fait de ses trois refactorisations. Ne pas appeler
  `codeAction/resolve` là donne un correctif qui ne fait rien, ce qui se lit
  comme cassé et non comme absent.
- **`workspace/applyEdit` est une requête du serveur**, et elle traverse jusqu'à
  la vue : elle seule tient le tampon et sait quel fichier est ouvert
  (`Evt::LspApplyEdit` → `Cmd::LspApplied`). La session n'attend rien pendant ce
  temps ; le serveur, lui, attend son oui ou son non, et ne rien répondre le
  laisse bloqué. Beaucoup de correctifs passent par là plutôt que par l'action
  elle-même.
- **Ce qui touche un autre fichier est refusé**, et le refus se dit. Toute
  écriture dans Claudhub repart avec l'empreinte de ce qu'on avait lu — c'est ce
  qui empêche d'effacer le travail d'un agent — et appliquer des modifications
  à l'aveugle dans des fichiers que personne n'a ouverts serait le seul endroit
  où cette règle tomberait. Un renommage à l'échelle du projet n'est donc pas
  pour cette version.
- **Les modifications s'appliquent à l'envers.** Leurs positions décrivent
  toutes le texte tel qu'il est maintenant : appliquées dans l'ordre de lecture,
  la première déplace ce que la seconde désigne. Les trier en ordre décroissant
  est ce qui fait que chacune tombe encore juste.

**Les jetons sémantiques sont la coloration que le serveur donne**, là où
tree-sitter ne donne que celle de la grammaire. La grammaire voit `$user->name`
et sait dire « accès à un membre » ; elle n'a jamais ouvert la classe, donc elle
ne sait pas si c'est une propriété, une méthode, ni si elle est dépréciée. Le
serveur, lui, a résolu le type. Ce qu'on y gagne en PHP : un paramètre distingué
d'une variable locale, un appel statique d'un appel d'instance, un symbole
déprécié signalé.

Quatre points, et le premier est le seul endroit où cette fonction peut échouer :

- **La légende est traduite dans le vocabulaire de nos thèmes.** Un jeton ne
  porte qu'un **indice** dans la légende du serveur — `parameter`, `enumMember`,
  `typeParameter` —, et l'éditeur résout ce *nom* contre le thème au moment de
  peindre. Nos palettes parlent tree-sitter, qui n'a ni `parameter` ni
  `enumMember` : un nom que le thème ignore ne rend aucun style, et un jeton sans
  style ne change rien à l'écran. C'est un échec **silencieux**, indiscernable
  d'un serveur qui n'aurait rien envoyé. `lsp::theme_name` fait donc la
  traduction, et deux tests la verrouillent — l'un contre les douze thèmes
  livrés, l'autre contre les deux de gpui-component. Un nom pointé est gratuit :
  `function.method` est exact là où un thème distingue les méthodes et retombe
  sur `function` ailleurs. Un nom **vide** est délibéré : `variable` est laissé à
  la grammaire, nos thèmes ne donnant aucune couleur propre aux variables.
- **L'ordre est celui du serveur**, puisque c'est à lui que les indices
  renvoient. Une entrée déplacée recolore tout un fichier de travers, sans erreur
  nulle part.
- **`full` ou `range`, selon ce que le serveur annonce.** L'éditeur demande le
  document entier — il met le résultat en cache et le fenêtre au moment de
  peindre, si bien qu'un défilement ne coûte rien — mais PHPantom déclare
  `full: true, range: false`, et lui demander une plage n'obtient qu'un refus.
  C'est la capacité qui choisit la requête, pas le nom du trait.
- **Un flux vide est souvent la bonne réponse.** PHPantom est réglé par défaut
  sur `contextual` : il n'émet que ce qu'une grammaire ne peut pas savoir —
  paramètres, `@template`, références dépréciées, accès statiques. Un fichier
  sans un seul paramètre répond zéro jeton, et c'est juste. Le test contre le
  vrai binaire a dû se donner un paramètre pour avoir quelque chose à vérifier.

Les **modificateurs** (`deprecated`, `readonly`, `static`) sont reçus et ne sont
mappés sur rien : ils voudraient un style à eux — un barré pour le premier — et
il n'y a rien à quoi l'accrocher pour l'instant.

Ce que la v1 ne fait pas, et c'est délibéré : seul le fichier ouvert dans
l'éditeur est synchronisé (rien dans la vue de diff, qui n'est pas un document
et dont le texte n'est pas celui du fichier), pas de renommage, pas de
formatage, pas de recherche de symboles — il n'y a pas de vue pour les porter.

**Le fil ne transporte que des chemins absolus, et c'est le seul endroit de la
fenêtre où c'est vrai.** Partout ailleurs un fichier s'appelle par son chemin
*dans* le worktree — c'est ce que git imprime, et chaque commande y joint la
racine au dernier moment. Le protocole n'a pas de racine à joindre : une URI est
absolue ou elle n'est rien, et `file://app/User.php` se lit « l'hôte `app`, le
fichier `/User.php` ». La jonction se fait donc aux quatre frontières de
`ui::lsp` (`full`, `local`) et jamais dans le cœur, à qui on remet un chemin
déjà fait. L'oubli ne provoque aucune erreur : le serveur répond sur un document
qu'il n'a pas su placer dans le projet, et les diagnostics reviennent classés
sous un chemin que l'éditeur n'emploie pas — donc invisibles. Un test tient
l'aller-retour.

**Ce que le serveur raconte va dans le journal** (`target: "lsp"`), stderr
compris. `$/progress` fait exception : il remonte en `Evt::LspBusy` et s'affiche
dans l'infobulle du bouton, parce qu'il répond à la seule question qu'un serveur
rapide pose quand même — pourquoi la complétion est maigre les dix premières
secondes. PHPantom construit son index par couches et le dit là.

**Le test qui compte ne lance pas de processus.** `Session::run` prend ce dans
quoi il écrit et ce qu'il entend : la boucle entière se joue en mémoire, la
poignée de main comprise. `tests/lsp_phpantom.rs` fait l'autre moitié — le vrai
binaire, les vrais tubes, la poignée de main, un survol, des diagnostics poussés,
une action de code résolue et un flux de jetons sémantiques — et **se saute** quand `phpantom_lsp` n'est pas
installé, ce qui est le cas de la CI : un test qui échoue faute d'un programme
que personne n'a promis est un build rouge qui ne dit rien. Ce qu'il vérifie du
serveur est la **mécanique** et jamais le contenu : quelles fautes un serveur
signale est son affaire et celle de sa version.

### Aller à la définition, et revenir

Deux gestes, et le second est ce qui rend le premier utilisable : suivre une
définition emmène ailleurs — souvent dans un autre fichier, souvent dans
`vendor/` — et rien à l'écran ne dit d'où l'on venait.

**`gd` ne passe pas par le `GoToDefinition` de gpui-component.** Leur action lit
`hover_definition`, que seul un survol avec la touche système remplit : au
clavier, sans pointeur nulle part, elle n'a rien sur quoi agir et ne fait
**rien**, en silence — c'est exactement ainsi que `gd` avait l'air d'une touche
non liée. La requête est donc la nôtre, posée au caret. La **première** réponse
est prise : un serveur en rend parfois plusieurs — une interface et ses
implémentations — et choisir demanderait une liste, qui est un geste que cet
éditeur n'a pas.

**`Ctrl+O` et `Ctrl+I` sont des liaisons, pas des touches de vim.** Elles sont
déclarées dans la table sous `ClaudhubEditor`, un contexte que l'éditeur pose
lui-même, et non dans la machine modale : une liaison s'exécute **avant**
l'écouteur en phase de capture par lequel les touches de vim passent, si bien
qu'elles marchent le mode éteint aussi. Un contexte à lui plutôt qu'`Input` :
le champ de message de commit et les champs de recherche en sont, et n'ont pas
de piste. `F12` complète, pour qui ne connaît pas les deux autres — une lettre
avec la touche système n'était pas possible, tout caractère dans un éditeur
étant un caractère que quelqu'un tape.

**La piste est un module pur** (`ui::jumps`), comme `motion.rs` et `notes.rs` :
on lui donne un endroit, elle en rend un. Ce qui s'y teste est la seule partie
qui ne se devine pas — ce qu'un nouveau saut fait à ce qu'on avait rembobiné.

Cinq points :

- **Une piste par worktree**, comme il y a un éditeur par worktree, et elle s'en
  va avec lui.
- **Le départ est lu au moment du saut**, jamais repris dans la piste : on lit
  quelques lignes avant de suivre une définition, et revenir là où l'on avait
  atterri plutôt que là où l'on est parti est la différence entre une piste et
  une liste de signets. Un pas en arrière réécrit de même l'endroit qu'il
  quitte, pour que le pas en avant rende le caractère qu'on a laissé.
- **Un nouveau saut jette ce qui était devant**, comme un navigateur jette son
  avant quand on suit un lien depuis le milieu de son historique : deux futurs
  ne peuvent pas être tous les deux le suivant.
- **Toute ouverture passe par `open_at`**, qui est du même coup le seul endroit
  où la piste s'écrit — l'explorateur, une ligne de diff, une trame Sentry, une
  définition. La reprise de session n'y passe pas et demande son fichier
  directement : revenir où l'on était n'est pas un saut qu'on doit pouvoir
  défaire.
- **Un atterrissage centre, une motion non.** `Place` dit lequel des deux :
  `Nearest` ne défile que le minimum et ne bouge pas du tout quand la ligne est
  déjà là — c'est ce que veut une flèche qui descend d'une ligne, la page ne
  doit pas sauter sous l'œil. Un saut, lui, arrive de loin : laissé au
  minimum, il posait le symbole sur la **dernière** ligne du panneau, avec au-
  dessus le code qui y mène et rien de ce qui suit — or lire une définition,
  c'est lire ce qu'il y a autour. Il demande donc `Centre`, comme `zz`.
- **Un caret peut attendre d'être révélé.** Le fichier qui vient d'être ouvert
  installe un `EditorState` neuf, que rien n'a encore mis en page : il n'a ni
  plage de lignes visibles ni hauteur de ligne, si bien que défiler jusqu'au
  caret est une division par rien et ne dit rien. Le caret était au bon
  endroit et la vue restait en haut du fichier, ce qui se lit « le saut a
  raté ». La révélation est donc gardée (`Editing::reveal_at`) et retentée à
  chaque frame jusqu'à ce qu'une mesure existe — la même réponse que la
  première largeur du diff, et bornée pour la même raison : un panneau rétréci
  à zéro ne doit pas demander des images à l'infini.
- **Atterrir donne le focus à l'éditeur.** Un saut est un geste au clavier dont
  la touche suivante en est un autre — `Ctrl+O` pour revenir aussitôt, une
  motion pour lire autour —, et un caret dans un champ où personne ne tape n'en
  résout aucune : les touches de la piste vivent sous `ClaudhubEditor`, et les
  touches de vim sous un écouteur posé dans le même sous-arbre. Traverser vers
  un autre fichier construit un `EditorState` neuf, qui naît sans focus, et
  `enter_workspace` n'en donne que si l'écran **change** — un saut fait depuis
  l'écran « Édition » n'en donnait donc à personne. Ouvrir un fichier depuis
  l'arbre ne passe pas par là : parcourir au clavier doit laisser les flèches à
  l'arbre.
- **Un saut vers un autre fichier ne pose pas son caret lui-même** : le texte
  n'arrive qu'un aller-retour plus tard. Le geste est noté (`explorer::Pending`)
  et consommé à l'arrivée — la même forme que le `pending_jump` de la revue, et
  pour la même raison. Il porte son worktree : on se promène pendant qu'un
  fichier se lit, et poser un caret dans l'éditeur de quelqu'un d'autre est pire
  que de ne pas le poser.

Les deux flèches de la barre du fichier sont ces deux touches, pour qui n'a pas
le mode vim. Elles sont **toujours là**, éteintes quand il n'y a nulle part où
aller : une flèche qui apparaît et disparaît déplace les quatre boutons voisins
à chaque définition suivie.

### Lire et retoucher un fichier

`InputState::code_editor(langue).line_number(true)` fournit la coloration,
l'auto-indentation, les numéros et la recherche. **La police, la taille et la
hauteur de ligne se disent explicitement** (`Editor::font_family` / `text_size`
/ `line_height`, celles du diff) : sans la première, l'éditeur hérite de la
police proportionnelle de l'interface, où une indentation ne s'aligne plus ;
sans la troisième, il garde le `line_height(Rems(1.25))` d'`Input`, qui est
calé sur le rem et donc **sourd à la taille du texte** — zoomé, les glyphes
grandissent, les lignes non, et chacune se peint par-dessus la précédente.
Notre affinage passe après le leur, ce qui est ce qui permet de le corriger. Le langage vient de la même
table que la coloration des diffs, PHP compris. Au-delà de `files::MAX_LINES`,
l'ouverture est **refusée** avec un message qui renvoie à l'éditeur externe :
une fenêtre figée est un pire service qu'un refus.

**L'écriture est conditionnelle** (`files::write`, `expect`). Un agent écrit
dans les mêmes fichiers pendant qu'on les relit : l'empreinte de ce qu'on avait
lu est repassée à l'écriture, et un fichier qui a changé depuis la fait
refuser. C'est la seule façon de ne pas effacer une heure de travail avec une
correction de faute de frappe. Corollaire : après un enregistrement réussi,
l'empreinte retenue suit ce qu'on vient d'écrire — sinon le deuxième
enregistrement d'affilée échouerait, le fichier ayant changé par notre faute.

L'éditeur **a son écran** (« Édition ») et son panneau, `EditorPanel`. Il a
longtemps pris la place du diff, faute de place ailleurs : les deux
partageaient un panneau dont le titre changeait de « Diff » à « Éditeur » selon
le dernier geste, ce qui disait bien qu'il en portait deux. Les écrans ont
rendu la question sans objet — voir « Les sous-applications ». Ouvrir un
fichier **bascule sur cet écran-là** : le geste vient de l'explorateur, qui y
vit, mais aussi d'une ligne de diff, et y répondre en silence sur l'écran d'à
côté serait un fichier ouvert que personne ne voit.

**Un éditeur par worktree** (`ClaudhubApp::editings`, et `editing()` pour celui
qui s'affiche), comme il y a des terminaux par worktree et pour la même raison :
un worktree est un endroit où l'on travaille, et y revenir doit rendre ce qu'on
y avait ouvert. Un éditeur unique laissait le fichier du projet précédent à
l'écran après un changement de worktree — un fichier d'un arbre que le reste de
la fenêtre ne montre plus. Deux corollaires : l'abonnement qui allume
l'indicateur « non enregistré » **capture** son worktree plutôt que de lire la
sélection, sans quoi un événement arrivé après la bascule marquerait le
mauvais fichier ; et l'éditeur d'un worktree qui s'en va — dépôt retiré, worktree
supprimé — s'en va avec lui, comme ses terminaux.

**Les modes de vim s'y appliquent**, quand le réglage est allumé : c'est le
seul endroit de la fenêtre où il y a du texte à éditer, et ce sont des modes et
non des liaisons — voir « Les modes de vim dans l'éditeur ».

L'éditeur externe se déclare par une commande avec `{path}` et `{line}`
(`code -g {path}:{line}`, `phpstorm --line {line} {path}`,
`zed {path}:{line}`). Il est lancé **détaché**, ses sorties jetées : un éditeur
graphique ne rend la main qu'à sa fermeture, et un `vim` lancé dans le vide
n'en rendrait jamais. Le geste existe **depuis une ligne de diff** — on relit,
quelque chose cloche, on l'ouvre là où c'est.

Les chemins de l'explorateur sont **ramenés dans le worktree** avant toute
opération : un `../` saisi dans un dialogue de renommage en sortirait, et une
suppression y ferait des dégâts que git ne rattrape pas.

### Quel domaine de revue s'ouvre

`app::initial_range` choisit, au **premier** statut d'un worktree, entre les
modifications, l'index et la revue de branche : ouvrir sur un domaine vide alors que l'autre est
plein est la façon la plus sûre de faire croire que Claudhub ne voit rien — un
worktree d'agent est propre et n'a que des commits à relire, d'où le repli sur
la revue de branche, qui attend que la base soit connue (`initial_range` rend
alors `None` plutôt que de trancher). Ensuite
la portée appartient à l'utilisateur, d'où le drapeau `range_chosen` — un
rafraîchissement de statut, et il en arrive un à chaque écriture de fichier, ne
doit jamais reprendre la main sur son choix.

Pour la même raison, les onglets « Modifications » et « Index » portent leur
compte : il se lit sans cliquer. Les deux autres portent sur des commits et
leur compte coûterait une commande git de plus par onglet et par
rafraîchissement.

`review::rows_for` est la seule vraie décision de cette vue — quel fichier
apparaît, dans quel groupe, coché ou non. Elle est libre et testée : le statut
est la source pour les modifications en cours (lui seul distingue index et
répertoire de travail), `--numstat` pour les domaines qui parlent de commits et
n'ont pas de notion d'index.

### Le graphe d'historique

`git::history` lit les commits **et** calcule la disposition du graphe :
`git log --graph` produit un dessin en caractères qu'il faudrait re-parser pour
en refaire des coordonnées. L'algorithme est celui de tous les visualiseurs —
une liste de rails, chacun attendant un commit ; un commit prend le rail qui
l'attendait, y installe son premier parent et place les autres à côté. Les
rails libérés sont réutilisés avant d'en ouvrir de nouveaux, ce qui garde le
graphe étroit.

`layout` rend **exactement autant d'entrées que de commits** : la vue les
affiche côte à côte, et un décalage d'une ligne ferait pointer chaque trait sur
le mauvais commit.

Sélectionner un commit remplit la liste des fichiers sous le graphe : le graphe
seul ne dit pas ce qu'un commit a touché, et sans cette liste seul le premier
fichier s'ouvrait — les autres restaient invisibles.

Le graphe est peint par un `canvas` **par ligne**, ce qui est ce qui permet à la
liste de rester virtualisée : une ligne se dessine à partir de son seul
`GraphRow`, sans rien savoir de celles qu'on ne voit pas. La puce est peinte en
dernier pour recouvrir les courbes qui l'atteignent.

Le module s'appelle `history` et non `log` : un module `log` dans ce crate
masquerait la bibliothèque de journalisation du même nom.

### Les tags

Un tag est le seul objet git qu'une relecture crée **exprès** — une version, un
jalon, un point où l'on veut revenir — et c'était la seule chose que Claudhub ne
savait ni montrer ni faire. Le panneau est un onglet **à côté de « Historique »**,
et c'est sa place : un tag nomme un commit, l'historique montre les commits, et
le geste qu'on fait après avoir trouvé le commit à marquer est juste là. Cliquer
un tag ouvre le diff de son commit, exactement comme une ligne d'historique —
deux listes sur la même chose, l'une rangée par date de commit, l'autre par date
de tag.

**Le local et le distant sont deux savoirs différents**, et c'est ce qui
structure le module. Lister les tags est une lecture de `refs/tags`, quelques
millisecondes ; savoir si `origin` a un tag est un `ls-remote`, un aller-retour.
Tant que le bouton globe n'a pas été pressé, **rien ne dit quoi que ce soit du
distant** — le compte de la barre change de phrase, et aucune ligne ne porte
« local ». Prétendre le second en n'ayant fait que le premier, c'est un panneau
qui dit « poussé » d'un tag que personne n'a poussé.

Sept points qui ne se devinent pas :

- **Le message décide du genre.** Un message donné fait un tag annoté (objet à
  lui : auteur, date, texte), sans message c'est un tag léger — c'est la
  distinction de git, et un interrupteur de plus à côté du champ dirait la même
  chose deux fois.
- **Une création qui pousse est *une* commande** (`Cmd::CreateTag { push }`), et
  c'est le seul endroit qui déroge à « un geste, une commande ». La création est
  locale, le push est un aller-retour : deux commandes partiraient dans **deux
  files**, que rien n'ordonne, et le push pourrait partir avant que le tag
  existe — git refusant alors un tag dont il n'a jamais entendu parler. Le
  drapeau se lit dans `queue_of`, ce qui suffit à envoyer le tout dans la file
  du réseau. Un test verrouille les six commandes de tag et leur file.
- **Le commit visé est celui qu'on lit.** Le dialogue marque le commit
  sélectionné dans l'historique quand il y en a un, HEAD sinon : « taguer *ce*
  commit » est le geste qu'on a en le lisant.
- **`PROTOCOL_VERSION` passe à 5** : six commandes et deux événements de plus,
  et postcard est positionnel — les deux bouts du fil s'installent séparément.
- **Le nom est validé sous le champ** (`tags::is_valid_name`, pur et testé) : le
  refus de git — `fatal: 'v 1.0' is not a valid tag name` — arrive après la
  fermeture du dialogue, c'est-à-dire trop tard pour corriger quoi que ce soit.
- **Un tag annoté rapporte le commit qu'il marque**, jamais son propre objet :
  `%(*objectname:short)` déréférence, et l'empreinte de l'objet tag
  n'apparaîtrait nulle part ailleurs dans la fenêtre.
- **`ls-remote` liste un tag annoté deux fois** — l'objet, puis le commit sous
  `^{}` : c'est un tag, pas deux, et `parse_remote` le sait.
- **Un push de tag écrit `refs/tags/<nom>` en entier**, jamais le nom nu : une
  branche et un tag peuvent porter le même nom, et git pousse alors celui qu'il
  veut.

**Supprimer ici et supprimer sur origin sont deux entrées de menu**, pas un
drapeau sur une seule : ce sont deux regrets différents, et le second se dit —
un tag que quelqu'un d'autre a déjà récupéré ne revient pas. Les deux passent
par un dialogue de confirmation.

Deux détails de la liste : la lecture est gardée par `loaded` et non par « la
liste est vide » — un dépôt sans aucun tag redemanderait sinon à chaque frame —
et un rafraîchissement **oublie** ce qu'on savait du distant, puisque c'est
précisément le moment où l'on cesse d'en répondre.

### Les réglages

Ils vivent dans un **global gpui** (`settings::SettingsStore`) et non dans
`ClaudhubApp`. Ce n'est pas un choix de style : le formulaire de gpui-component
déclare chaque champ par un couple lire/écrire dont les fermetures ne reçoivent
qu'un `App`, sans accès à l'entité racine. `Settings::global(cx)` lit,
`Settings::update_global(cx, |s| …)` écrit.

`update_global` ré-applique le thème à chaque modification, y compris celles
qui ne le concernent pas — c'est lui qui porte les polices et la taille du
texte, et trier les champs qui l'affectent coûterait plus de code qu'un
`refresh_windows` de trop. L'écriture du fichier est **différée d'une demi-
seconde** : un champ de saisie émet une valeur par frappe et la molette un cran
par encoche.

Le formulaire n'a pas de bouton « Appliquer » : choisir une police ou une
taille sans en voir l'effet reviendrait à choisir à l'aveugle.

**Un écran et non un dialogue.** Le formulaire était une fenêtre modale — ce
qu'on prend quand on n'a nulle part où mettre un formulaire. Elle couvrait ce
qu'on réglait, elle ne pouvait pas rester ouverte à côté de l'effet qu'elle
produisait, et les deux choses pour lesquelles on vient ici — essayer un thème,
lire pourquoi quelque chose a échoué — sont justement les deux qui veulent
garder le reste de la fenêtre en vue. La barre était déjà là, le dock savait
déjà porter un panneau : voir « Les sous-applications ».

Trois conséquences du déménagement :

- **La fermeture de rendu tourne à chaque frame** et non plus une fois à
  l'ouverture. Ce que le formulaire demandait au système à ce moment-là est
  donc lu **une fois et retenu** (`settings_view::Environment`) : énumérer les
  polices installées et faire un `stat` sur chaque ligne d'`/etc/shells` au
  rythme des images est une entrée-sortie au milieu d'une frame. Le registre
  des thèmes, lui, se relit à chaque fois — il est surveillé, et un fichier
  déposé pendant que Claudhub tourne doit apparaître dans la liste. Le cache
  est vidé à l'arrivée du serveur distant, dont les shells sont ceux qui
  comptent.
- **La page demandée passe par l'identifiant du formulaire.**
  `default_selected_index` n'est lu qu'à la **création** de l'état, lequel vit
  aussi longtemps que l'`id`. `open_settings_at` incrémente donc un compteur
  qui entre dans l'`id` : une page nommée est une demande, honorée à chaque
  fois — même deux fois de suite, en s'étant promené entre les deux — là où
  `Page::First` veut dire « là où vous en étiez » et laisse le formulaire
  tranquille.
- **L'écran des réglages n'a pas de colonne de gauche** : voir « Les
  sous-applications ».

### Le journal

La page « Journal » des réglages montre ce que Claudhub a écrit depuis son
démarrage. Une application graphique n'a pas de console sous sa fenêtre :
sans elle, savoir pourquoi un `fetch` a échoué ou pourquoi le serveur distant
est mort demande de relancer depuis un terminal, c'est-à-dire de reproduire le
problème avant d'avoir le droit de le regarder.

`logging::init` installe donc **notre** `log::Log` par-dessus celui
d'`env_logger` : il lui passe l'enregistrement puis en garde une copie. Un
`format` d'`env_logger` n'aurait pas suffi — il reçoit un puits d'octets et ce
qui va être **imprimé**, quand la page veut le niveau et la cible tels quels,
l'un pour colorer la ligne, l'autre pour dire d'où elle vient.

**Trois étages, trois niveaux**, et c'est le partage qui rend le journal
lisible :

- `git::report` file **chaque** commande à `debug` — la commande, son
  répertoire, sa durée —, y compris les échecs, avec le code de sortie et
  stderr. `debug` et non `warn` pour l'échec : `git_opt` existe justement pour
  les lectures dont l'échec est la réponse normale — une branche sans amont, un
  fichier que git ne connaît pas — et avertir sur chacune enterrerait les
  vraies. L'exception est une commande qui **traîne** : passé la seconde, elle
  explique une interface qui semble bloquée, et elle passe à `info`. Sur un
  disque Windows monté par WSL, un `git status` y arrive tout seul — et c'est
  exactement le cas dont on veut être averti.
- `runtime::handle` nomme **chaque commande** et la chronomètre. Le nom vient
  de `Cmd::name`, un match exhaustif : ajouter une variante sans la nommer ne
  compile pas, là où un nom tiré de `Debug` aurait coûté le formatage de la
  charge utile — un `WriteFile` porte un fichier entier — à chaque commande.
  Une commande qui a produit un `Done` ou un `Failed` est une **écriture** :
  c'est ce que ces deux événements veulent dire, cela évite de classer soixante
  variantes une seconde fois, et elle passe à `info` avec sa durée.
- `done` et `fail` disent **ce qui a été fait**, où, et ce que git en a dit —
  la première ligne seulement, un `git push` écrivant un paragraphe. `fail` est
  le seul `warn` de la couche, et c'est ce qui lui donne sa valeur : ici
  l'opération est connue, donc on sait que quelqu'un l'attendait.

Côté `wt`, `capturing` **journalise toutes les lignes** et n'en rend qu'une.
Un `wt new` raconte toute sa séquence — la branche, les dossiers, les copies,
les ports, puis ce que `post_new` imprime — et cette narration est le seul
compte rendu qui existe de ce que les hooks d'un projet ont fait. Garder la
dernière ligne pour la barre d'état et jeter le reste laissait une phrase
derrière un `composer install` de trois minutes ; un échec à mi-parcours ne
laissait rien du tout, l'erreur étant portée par le `Result` et les étapes qui
y ont mené par personne.

Cinq points sur l'anneau :

- **Un anneau de deux mille lignes**, pas une liste qui croît : une session
  dure une journée, une cible bavarde écrit une ligne par frame, et un journal
  que personne n'a demandé ne doit pas pouvoir prendre la mémoire de la
  relecture qu'il est là pour expliquer.
- **Le filtre d'`env_logger` s'applique aussi à la copie.** `log::set_max_level`
  ne borne que par niveau ; c'est `enabled` qui applique les directives par
  cible de `CLAUDHUB_LOG`. L'oublier garderait en mémoire ce qu'on a dit à la
  console de ne pas imprimer.
- **Un compteur, pas la longueur de l'anneau.** `logging::written` est ce qui
  dit à la vue que sa copie est périmée ; la longueur cesse de bouger dès que
  l'anneau est plein. La vue ne recopie donc les deux mille entrées que quand
  il y a du neuf — la page se rend à chaque frame.
- **Rien ne réveille la vue quand une ligne est écrite** : un worker journalise,
  il ne touche pas à gpui. La page suit donc les frames que le reste de
  l'application provoque, et le balayage de fond en amène une toutes les deux
  secondes — c'est ce qui fait qu'une ligne écrite ailleurs finit par
  apparaître seule.
- **Deux cents lignes peintes au plus**, et c'est dit. La liste vit dans une
  page du formulaire, qui défile d'un bloc : elle n'est pas virtualisée, et
  peindre deux mille lignes stylées par frame est précisément ce qu'une liste
  virtualisée existe pour éviter. Le filtre s'applique **avant** la coupe — les
  deux cents derniers avertissements ne sont pas les avertissements des deux
  cents dernières lignes. Le bouton « Copier », lui, prend tout ce que le
  filtre a gardé : ce qui part dans un rapport est le journal, pas sa fin.

**Le journal est en anglais, et un test le garde.** La règle — le cœur en
anglais, la documentation en français — n'en avait aucun, contrairement aux
clés i18n. Elle passait inaperçue tant que ces lignes n'atteignaient qu'une
console que personne n'ouvrait ; la page les montre telles quelles, et une
ligne française au milieu de vingt anglaises se lit désormais comme une
incohérence. `logging::tests::nothing_speaks_french_in_the_journal` relit les
sources et n'inspecte que les littéraux d'un `log::` : élargir à toutes les
chaînes de l'arbre attraperait les fixtures de tests, pleines d'accents
exprès.

Le niveau affiché vit dans `ClaudhubApp` et non dans les réglages : c'est une
posture de lecture, qui change plusieurs fois pendant qu'on cherche, pas une
préférence qu'on s'attend à retrouver le lendemain. Il s'ouvre sur `Trace`,
c'est-à-dire sur tout ce que la console a gardé — l'anneau ne contient de toute
façon que ce que `CLAUDHUB_LOG` a laissé passer, et s'ouvrir sur une vue
rétrécie cacherait des lignes dont rien d'autre ne dit qu'elles existent.

Corollaire pour les vues : ce qui dépend d'un réglage se **relit à chaque
rendu**, jamais au moment de la construction. `TerminalView::sync_font` en est
l'exemple : changer la police invalide la géométrie mesurée, et il faut effacer
les bornes retenues pour que le pty soit redimensionné — sinon le texte change
de taille et le shell continue de croire à l'ancienne largeur en colonnes.

Les familles proposées viennent de `cx.text_system().all_font_names()`, filtrées
par convention de nommage pour les champs à chasse fixe : gpui n'expose pas
cette propriété de façon portable. La liste rate donc des familles, et le
fichier de réglages reste modifiable à la main pour ces cas-là.

### Les polices embarquées

Les défauts ne viennent **jamais** de l'hôte : `install_fonts` les enregistre
dans le système de texte de gpui au démarrage, et c'est ce qui fait qu'un
`DEFAULT_UI_FONT` est une promesse et non un pari. Ce sont **Iosevka Aile**
pour l'interface et **Iosevka Nerd Font Mono** pour l'éditeur, les diffs et le
terminal — la coupe `Mono` et non l'Iosevka nue, dont le `@` et le `W` valent
une cellule et demie et décaleraient la grille du terminal. Inter et JetBrains
Mono restent embarquées derrière : un `settings.json` qui les nomme doit
continuer de marcher sur une machine qui ne les a jamais installées.

Quatre points qui ne se devinent pas :

- **Les fontes sont sous-ensemblées**, par `tools/subset_fonts.py`, et le
  script est la seule façon de les refaire à la version suivante d'Iosevka. Les
  faces complètes pèsent neuf à onze mégaoctets pièce : les quatre entières
  ajouteraient cinquante mégaoctets à un binaire qui en fait soixante-quinze,
  pour une couverture que personne ne lit dans un diff. Ce qui reste est ce que
  Claudhub peint — latin et ses diacritiques, grec, cyrillique, ponctuation,
  monnaies, flèches, filets et blocs, plus les icônes Nerd pour la chasse fixe —
  et cela tient en huit mégaoctets et demi.
- **Un glyphe absent du sous-ensemble n'est pas un carré vide** sur une machine
  pourvue : gpui retombe sur les polices du système. Le sous-ensemble ne se paie
  que sur une machine nue, où un nom de fichier chinois dans un diff se
  perdrait. C'est l'arbitrage assumé, et il ne vaut que parce que le repli
  existe.
- **Le patcheur de Nerd Fonts ne met le nom complet que dans le champ 16** ; le
  champ 1, celui de la famille, dit « Iosevka NFM ». Sur lequel des deux un
  système de texte s'appuie n'est pas une chose à parier — un nom qui rate se
  résout en repli silencieux, la panne que ce dépôt paie déjà sur les icônes
  non embarquées —, d'où le renommage que le script applique aux deux.
- **Les fontes ne passent pas par `rust-embed`** mais par `include_bytes!` : le
  motif d'`Assets` ne couvre que `icons/**/*.svg`, et les lister là les mettrait
  dans le binaire une seconde fois. C'est aussi pourquoi `NotoColorEmoji.ttf`,
  présent dans `assets/fonts/` mais nommé nulle part dans le code, ne coûte rien
  au binaire.

### Ce qui tourne en ce moment

Un `fetch`, un `push`, un `wt up` prennent des secondes, parfois des minutes,
pendant lesquelles rien ne bougeait à l'écran. `ClaudhubApp::running` est
l'ensemble des écritures en vol, et `start` remplace `git.send` partout où le
geste en est une.

- **La clé est exactement la paire que le worker renverra.**
  `write_then_refresh` réémet le worktree et l'action qu'on lui a donnés, si
  bien que ce qu'on pose est exactement ce que `finish` retrouvera. C'est ce
  qui empêche l'unique panne d'un indicateur d'attente : un bouton qui tourne
  pour toujours en dit moins qu'un bouton qui n'a jamais tourné.
- **Deux moments vident tout** (`clear_running`) : la mort du serveur distant
  et l'arrivée d'un neuf. Ce qui était en vol est mort avec lui, et les
  commandes émises avant que le manche soit vivant ont été jetées — rien ne
  répondra jamais pour elles.
- **`wt up` et `wt down` ne nomment pas de worktree** dans leur réponse : `wt`
  travaille depuis le dépôt principal. `running` seul dirait « quelque chose
  démarre » sans dire où, et toutes les pastilles de la liste tourneraient ;
  d'où `wt_pending`, un seul worktree à la fois — démarrer deux projets d'un
  coup n'est pas un geste.
- **Ce qui part d'un menu n'a pas de bouton à faire tourner** : un menu se
  ferme au clic. L'intégration, le rebase, un `wt rm` s'annoncent donc dans la
  **barre d'état**, nommés et non comptés — « 3 opérations » ne dit rien, là où
  « Publication… » dit lequel des gestes qu'on vient de faire n'est pas fini.
  La liste est triée : un `HashSet` s'itère dans un ordre différent à chaque
  frame, et les mots danseraient.
- **La décision est hors de la vue** (`ui::inflight::InFlight`). Poser une
  clé qu'on ne reprend jamais est l'unique panne de cet indicateur, et c'est
  une chose qui se teste — pas une chose qu'on surveille. Le module ne connaît
  aucun type de gpui, comme `notes.rs` en face de `notes_view.rs`. Corollaire :
  la barre trie sur la **clé** i18n et non sur le libellé traduit, si bien que
  l'ordre des mots ne dépend plus de la langue.
- **`Action::running_key` a un joker assumé.** Seules les opérations assez
  longues pour valoir une ligne sont nommées ; un `Stage` qui finit en dix
  millisecondes afficherait un message que personne n'a le temps de lire. Un
  test vérifie que chaque action a ses deux messages dans les deux catalogues —
  une clé manquante s'affiche telle quelle dans la barre d'état.

### Ce qu'une opération a répondu

La barre d'état était le seul endroit où quoi que ce soit se disait, et c'est
le mauvais dès qu'il y a quelque chose à **lire** : un `git pull` répond
`Updating 9ae0f50b..55e38b36`, `Fast-forward`, puis une ligne par fichier
touché. Versé dans une barre haute d'une ligne, ce texte ne se tronque pas — il
se replie, et les quarante lignes se peignent par-dessus la fenêtre en
remontant depuis la barre.

Deux endroits, donc, et le partage est tout le sujet de `ui::notify` :

- **La barre garde une ligne**, toujours, quoi qu'il arrive. `headline` prend
  la **première** ligne non vide — git mène par ce qu'il a fait, la suite est le
  détail — et à défaut le libellé de succès de l'action.
- **Une bulle porte ce qui se lit** : la liste de fichiers d'un pull, les
  références qu'un push a bougées, la raison d'un refus de fusion.

Quatre points :

- **La décision est hors de la vue et testée**, comme `inflight.rs` et
  `notes.rs`. Trois règles : tout échec a sa bulle — la barre est écrasée par le
  message suivant, et un `push` refusé est précisément ce qu'on revient lire une
  minute plus tard ; une sortie de plus d'une ligne a la sienne, c'est la
  définition de « il y a à lire » ; rien d'autre n'en a, un `Stage` qui finit en
  dix millisecondes ferait une bulle que personne n'a le temps de lire et qui
  pousse les utiles hors de l'écran.
- **La bulle vient de gpui-component**, qui en a une et que `Root` réémet déjà —
  voir « Conventions gpui ». Rien d'autre à installer : elle s'empile, elle se
  ferme, et elle ne dispute pas sa place à la fenêtre. `NotificationDelivery`
  sait aussi poster au centre de notifications du système ; on n'y va pas, une
  fenêtre qu'on regarde n'a pas à parler par-dessus l'épaule du bureau.
- **Un échec ne s'efface pas tout seul** (`autohide` faux) : c'est la seule
  chose ici qu'on lit en retard.
- **Un seul libellé pour tous les échecs** (`notify-failed`), et non une
  troisième famille de trente clés en miroir de `success_key` : ce qui a échoué
  est dans le corps, git nommant l'opération lui-même — `error: failed to push
  some refs`.
- **Toute opération distante a sa bulle**, même quand elle tient sur une ligne
  (`notify::always_worth_reading` : fetch, pull, push, et les gestes de tag qui
  sont des pushes). Elle a coûté des secondes, on a fait autre chose pendant ce
  temps, et sa réponse se lit **après** — c'est-à-dire exactement ce que la
  barre ne sait pas servir, puisque le message suivant l'écrase. Un commit avait
  sa bulle par accident, sa sortie faisant deux lignes ; un push qui venait de
  publier trois commits n'en avait aucune.
- **Ce que git a écrit sur stderr en fait partie** (`git::git_reporting`, pour
  les commandes réseau et elles seules). `git push` et `git fetch` écrivent tout
  leur compte rendu là — `To github.com:…`, les références déplacées, `From
  origin` — et leur stdout est vide : lue par `git`, qui ne garde que stdout,
  une publication réussie ne rapportait littéralement rien à dire. stderr passe
  **en premier**, git menant par le compte rendu et finissant par ses conseils,
  et c'est la première ligne que la barre retient.

### Le magasin d'état

Les réglages disent comment Claudhub s'affiche ; le magasin
(`store::StateStore`, `<config>/state.json`) dit où l'on en est — la base à
laquelle on compare *ce* worktree, les dossiers qu'on y a repliés, le prochain
numéro de note, et où l'on en était en fermant (voir « Où l'on en était »).
Deux fichiers et non un seul : le premier se modifie à la
main, le second compterait quelques centaines de lignes par dépôt et n'y
survivrait pas.

Ce qui **n'y est plus** : les notes de relecture, parties dans un dossier de
fichiers Markdown (voir « Les notes sur le disque »). Le magasin garde
`next_note`, qui ne se déduit pas d'elles — une note supprimée libérerait son
numéro, et une note déjà envoyée à l'agent y serait désignée par un numéro qui
vaudrait pour une autre.

Pas de SQLite, et la question revient : le magasin fait moins d'un kilo-octet,
une base achèterait l'écriture partielle, la requête et la concurrence entre
processus, et rien ici n'en demande. Elle coûterait une dépendance C, un schéma
et ses migrations, et surtout elle ne s'ouvre pas dans l'éditeur de notes de
l'utilisateur — ce qui est justement là où les notes devaient aller.

Il est écrit **depuis le thread d'interface**, ce qui déroge à « `src/ui/` ne
fait jamais d'entrée-sortie ». C'est le précédent de `settings.rs` et la même
raison : quelques kilo-octets écrits une fois par demi-seconde ne valent pas un
aller-retour par le protocole. La règle vise les commandes git, dont la plus
rapide coûte déjà une frame, pas la préférence qu'on range.

Trois points qui ne se devinent pas :

- **La valeur relue l'emporte sur celle que git devine.** `ensure_review` crée
  l'état d'un worktree en y remettant ce que le magasin en avait ; `base` y est
  alors `Some`, et les deux endroits qui proposent un `default_base` —
  `Evt::Status` et `Evt::Branches` — ne l'écrasent pas, tous deux testant
  `is_none()`. C'est un choix de l'utilisateur, pas une devinette à refaire.
- **Une entrée retient son dépôt** (`WorktreeState::repo`). La purge se fait
  quand git vient d'énumérer les worktrees, seul moment où la liste est sûre ;
  sans ce champ, une entrée absente de cette liste ne se distinguerait pas
  d'une entrée appartenant à un dépôt qu'on n'a pas encore ouvert, et l'oublier
  effacerait les notes d'un worktree bien vivant.
- **Un seul point d'écriture** (`ClaudhubApp::persist_review`) plutôt qu'un par
  champ. Les replis sont triés avant d'être écrits : un `HashSet` sérialisé
  dans un ordre différent à chaque fois ferait un fichier qui change sans que
  rien n'ait changé.

### Chercher dans un panneau

`Ctrl+F` cherche dans le panneau où l'on vient de cliquer. Presque tout ce que
Claudhub affiche est une liste, et une liste qu'on ne peut pas interroger se
parcourt du regard.

**Le clic, et non le focus.** Le dock pose le focus sur l'onglet actif de
*chaque* zone — il y en a trois affichées en même temps — et rien là-dedans ne
dit laquelle l'utilisateur regarde. `panels::pane_root` note donc le panneau
touché, en phase de **capture** : une ligne de diff comme une case à cocher
consomment leur clic, et le panneau ne saurait jamais qu'on l'a touché.

**Un seul geste, deux comportements.** Là où la liste est libre de son ordre,
la recherche **filtre**. Là où l'ordre porte du sens, elle **saute** :

- Le **diff** est le fichier ; on y surligne les occurrences par-dessus la
  coloration (`highlight::overlay`) et `Ctrl+G` va de l'une à l'autre. C'est le
  seul panneau qui affiche un compte — c'est la seule liste dont on ne voie pas
  l'effet de la recherche, une occurrence pouvant être à quatre mille lignes.
- L'**historique** a un graphe dont les traits relient une ligne à ses
  voisines : en retirer une du milieu ferait pointer chacun d'eux sur le
  mauvais commit. Ce qui ne correspond pas est donc éteint, pas retiré.

Trois détails qui se paient :

- **La casse est déduite de la requête** : une requête tout en minuscules
  l'ignore, une majuscule la respecte. C'est la convention des éditeurs, et
  elle évite un bouton pour un réglage qu'on change à chaque recherche.
- **Les décalages sont en octets**, et `find_all` compare caractère à
  caractère plutôt que de chercher dans un `to_lowercase()` : la mise en
  minuscules change la longueur en octets de certains caractères, et les
  décalages rendus ne désigneraient plus rien.
- **Une recherche ignore les replis** — explorateur et liste de revue. Un
  fichier trouvé dans un dossier fermé ne se verrait pas, et la recherche
  paraîtrait n'avoir rien trouvé.

Les occurrences du diff sont calculées à chaque changement de requête et à
chaque arrivée de diff, **jamais au rendu** ; le rendu les consulte par
`(hunk, ligne)`. `DiffSearch::valid` est ce qui les invalide : les décalages
portaient sur un texte qui vient d'être remplacé.

`highlight::overlay` pose un fond sans toucher aux couleurs de texte, en
découpant aux frontières des deux découpages. Il rend des plages **triées et
disjointes**, l'invariant que gpui ne vérifie pas et dont la violation décale
tout ce qui suit.

Le terminal n'a pas de recherche : `Ctrl+F` y appartient au programme qui
tourne, et l'historique d'une grille alacritty n'est pas une liste que nous
tenions.

### Les barres de défilement

Tout panneau qui défile en porte une (`ui::scroll`). Une liste virtualisée ne
dit rien d'elle-même : `uniform_list` ne peint que ses entrées, et rien ne
distingue « il reste trois lignes » de « il en reste trois mille ». La barre
est le seul repère de position qu'ait ce genre de liste, et les nôtres font
couramment plusieurs milliers d'entrées — un diff de relecture d'agent,
l'explorateur d'un projet Laravel.

Elle se pose **par-dessus** le contenu, en absolu, d'où le `relative` du
conteneur : lui réserver une colonne rognerait seize pixels de largeur utile
dans chaque panneau, alors que la moitié d'entre eux n'ont pas de quoi défiler
la plupart du temps.

Quatre points, tous constatés à l'écran et aucun deviné — une barre qui ne se
peint pas ne provoque aucune erreur :

- **`min_h_0` et `min_w_0` sur le conteneur.** C'est un élément flex, dont la
  taille minimale vaut par défaut celle de son contenu : sans eux il prend la
  hauteur des huit mille lignes de l'arbre et la largeur du plus long nom de
  fichier. La liste, elle, garde la bonne taille — c'est la barre qui va se
  peindre trois cents pixels à droite du panneau, hors de vue.
- **`overflow_hidden`**, pour la même raison en aval.
- **`scrollbar()` de gpui-component plutôt qu'un enfant `Scrollbar` nu.**
  L'extension enveloppe la barre d'une couche absolue calée sur les quatre
  bords ; posée nue, elle ne reçoit pas de bornes utilisables et ne peint rien.
  C'est le seul de ces points qui ne se déduise pas de la mise en page.
- **L'identifiant va au conteneur**, distinct par appel : la couche que pose
  `scrollbar()` s'appelle toujours `scrollbar_layer`, et sans parent identifié
  les panneaux partageraient l'état d'une seule barre.

**`ScrollbarShow::Always`**, posé dans `theme::apply`. Ni `Scrolling`, le
défaut, qui efface la barre deux secondes après le dernier cran de molette —
on relit un diff en s'arrêtant à chaque hunk, et elle aurait disparu chaque
fois qu'on se demande où l'on en est ; ni `Hover`, qui ne la montre qu'une fois
le pointeur **sur la barre**, laquelle est invisible, donc introuvable.

Les panneaux non virtualisés — notes, conflits, Sentry —
n'avaient pas de poignée du tout : elle vient de `ClaudhubApp::scroll_of`, une
table plutôt qu'un champ par panneau. Créée au rendu, elle remettrait la liste
en haut à chaque frame.

Le terminal fait exception : son défilement n'est pas celui de gpui mais le
`display_offset` de la grille alacritty, qu'aucune `ScrollHandle` ne décrit.

### Le lissage de la molette

Un cran de molette est un **saut** : gpui traduit un `ScrollDelta::Lines` en
trois hauteurs de ligne et les ajoute d'un coup au décalage. Sur les listes que
Claudhub affiche — un diff de plusieurs milliers de lignes, l'arbre d'un projet
Laravel — l'œil perd sa place à chaque cran, et il en faut une vingtaine pour
traverser un hunk. `ui::motion` rejoue donc le saut en cent soixante
millisecondes, amorties en fin de course. Le procédé vient d'Aviary, dont le
module du même nom est la référence.

Le principe tient en une inversion : **on n'empêche pas gpui de sauter**. Il
n'y a pas de phase de capture pour la molette — le même piège que le zoom du
diff. On le laisse faire, on lit où il a atterri, on **remet** le décalage
d'avant, et on y va progressivement. D'où la place de l'écouteur : sur un
ancêtre **non défilant** de la liste, donc après son gestionnaire interne dans
la phase de remontée. C'est exactement le conteneur de la barre de défilement,
et `ClaudhubApp::scrolled` pose les deux d'un même geste — **une seule clé**
pour la barre et pour le mouvement, si bien qu'aucun panneau ne peut animer le
décalage d'un autre, ce qui le ferait sauter d'un bout à l'autre.

Cinq choses qu'on ne devine pas :

- **Le saut se lit, il ne se recalcule pas.** gpui ne rogne rien au moment de
  la molette — il ajoute son delta au décalage, et c'est la mise en page
  suivante qui le ramène dans les bornes. Refaire le calcul de notre côté
  demanderait la même hauteur de ligne que lui, or il capture la sienne **sous
  le style de texte de l'élément qui défile** quand notre écouteur, posé sur un
  ancêtre, lit la hauteur ambiante : sur le diff, qui n'a ni la police ni la
  taille de l'interface, trois lignes d'écart font deux ou trois pixels. Au
  milieu d'un saut de soixante, ils ne décalent que la provenance de la
  transition ; **au bord, ils sont tout le mouvement** — la destination y est
  rognée à la position qu'on occupe déjà, la provenance non, et la vue reculait
  de trois pixels pour y revenir en cent soixante millisecondes, à chaque cran.
  D'où `Axis::jump`, qui prend la différence avec le décalage écrit à la frame
  précédente, et ne s'y fie qu'à condition qu'elle ressemble au saut attendu.
- **Un pavé tactile n'est pas une molette.** Il envoie des
  `ScrollDelta::Pixels`, déjà continus et attachés au doigt : les lisser
  ajouterait un retard à un geste direct. Ils passent tels quels, et annulent
  la transition en cours.
- **Un saut demandé par le code gagne.** `scroll_to_item` — une flèche qui
  change de hunk, `reveal_file` — écrit le décalage sans rien dire à personne.
  `advance` compare donc ce qu'il trouve à ce qu'il avait écrit lui-même : un
  écart veut dire que quelqu'un d'autre est passé, et la transition est
  abandonnée plutôt que de ramener la vue en arrière.
- **La liste change de taille pendant le mouvement.** Un diff qui arrive
  pendant qu'un agent écrit, un dossier qu'on déplie : la destination est
  reprise à chaque frame sur les bornes du moment, depuis la position visible.
- **Les deux axes sont répartis comme gpui les répartit** — molette
  horizontale rabattue sur le vertical quand un seul axe déborde, composante
  dominante seule quand les deux débordent (`allow_concurrent_scroll` est faux
  par défaut). Un partage différent ferait aller le lissage ailleurs que le
  saut qu'il remplace.

`Axis` ne contient aucun type de gpui, et c'est ce qui rend la mécanique
testable : rendre le saut, additionner deux crans, céder à un saut
programmatique, repartir de la bonne position quand git a rogné le décalage sur
un bord.

Trois surfaces n'y passent pas, et chacune pour une raison différente :

- **Le diff** garde son écouteur à lui (`on_diff_scroll`). Le zoom et le
  lissage veulent tous deux rendre le saut que gpui vient d'appliquer, et deux
  écouteurs le rendraient deux fois ; un zoom en cours de transition l'annule,
  la destination ayant été calculée sur des lignes qui n'ont plus la même
  hauteur.
- **L'éditeur intégré** a le sien aussi, et rien n'y ressemble aux autres.
  Quatre choses, et chacune vient d'un essai raté :
  - **Sa poignée est `pub(crate)`, son offset non**
    (`InputState::scroll_offset` / `set_scroll_offset`). D'où
    `ScrollMotion::advance_at` et `wheel_at`, qui prennent l'offset et sa
    course en argument là où `advance` et `on_wheel` les lisent d'une poignée —
    les seconds ne sont plus que les premiers appliqués à une `ScrollHandle`.
  - **La molette lui est prise avant qu'il la voie.**
    `InputState::on_scroll_wheel` défile puis appelle `stop_propagation` dès que
    l'offset a bougé : un écouteur sur l'ancêtre — la disposition du diff —
    n'était jamais appelé, sauf tout en haut et tout en bas. Rien ne lissait
    quoi que ce soit, et `Ctrl`+molette zoomait pendant que l'éditeur continuait
    de défiler dessous. Un écouteur souris de fenêtre en phase de **capture**
    passe devant, et consommer l'événement là laisse tout le mouvement à nous —
    d'où le `canvas` qui l'enregistre, seul endroit où l'on tient des bornes au
    moment de peindre. C'est donc `ScrollMotion::push` et non `on_wheel` :
    rien n'a sauté, il n'y a pas de saut à rendre.
  - **Ce qu'on relit est en retard d'une frame.** `set_scroll_offset` est
    appliqué à la mise en page suivante, si bien que l'offset lu à la frame
    d'après est encore le précédent. Pris pour l'écriture de quelqu'un d'autre,
    il annulait la transition dès sa première frame — l'éditeur ne bougeait pas
    du tout. `ScrollMotion::owned` lève ce garde-fou, et lui seul : au repos,
    l'offset est relu comme partout, c'est le curseur ou une recherche qui
    déplace la vue.
  - **La course est calculée** : lignes visibles fois la hauteur de ligne pour
    la vue, lignes du fichier fois la même pour le contenu, la différence pour
    ce qui reste. C'est une approximation — une ligne repliée compte pour une —
    et elle n'a pas à être meilleure : `set_scroll_offset` rogne sur la vraie
    plage, et la frame suivante relit ce qui a été retenu.
- **Le terminal** non plus : son défilement est le `display_offset` de la
  grille alacritty, compté en lignes entières, et il n'y a pas de demi-ligne à
  dessiner.

### Les hauteurs de ligne

Aucune hauteur de ligne ne s'écrit en dur : elles viennent de
`theme::row_height` / `tall_row_height` / `bar_height` / `toolbar_height`, qui
les déduisent de la taille du texte. Une hauteur figée déborde dès qu'on
grossit la police — et c'est pire dans les listes virtualisées, qui ne mesurent
rien et réservent exactement ce qu'on leur annonce : la ligne suivante est
recouverte, pas repoussée. Les lignes à deux étages — un nom et son détail —
n'annoncent, elles, aucune hauteur du tout et se laissent dimensionner par leur
rembourrage.

### Le zoom

Deux zones se règlent séparément — les diffs et le terminal — parce que
grossir la sortie d'un agent pour la lire ne doit pas déplacer le code relu à
côté. La molette avec la touche système agit sur la zone survolée, les
raccourcis (`secondary-=`, `secondary--`, `secondary-0`) sur celle qui a le
focus.

L'éditeur intégré s'y ajoute, et prend la taille du **diff** : c'est du code des
deux côtés, jamais affiché en même temps, et deux tailles à tenir accordées en
feraient une de trop.

Piège à connaître dans la vue de diff comme dans l'éditeur : gpui n'a **pas de
phase de capture pour la molette**. Quand notre écouteur s'exécute, la liste a déjà défilé — les deux
sont en phase de remontée et l'enfant est traité avant son parent.
`on_diff_scroll` rend donc le décalage au lieu d'essayer de l'empêcher, sans
quoi chaque cran de zoom ferait aussi sauter la lecture de trois lignes.

### Copier depuis un diff

Ce qui sort du presse-papiers est du **code** : ni `+`/`-`, ni numéros de
ligne, ni en-tête `@@`. C'est ce qu'on colle dans un éditeur ou dans l'invite
d'un agent, et le nettoyer après coup est précisément la corvée que cette vue
doit épargner. `Ctrl+Maj+C` donne l'autre forme, un extrait de patch avec les
signes de git — le vrai signe moins de l'affichage n'y a pas sa place, il ne
s'applique pas.

La sélection se fait au clic, s'étend au glissement ou au Maj+clic, `Ctrl+A`
prend tout le fichier, et `Rendered::copy_text` est
libre et testée. Sans sélection, `Ctrl+C` prend le fichier entier : sur un
diff, le geste n'a pas d'autre sens et refuser d'agir serait un refus poli sans
raison.

Un clic sur une ligne **prend le focus**. Sans cela le focus reste au terminal,
et le `Ctrl+C` qui suit part au programme qui y tourne au lieu de copier. Pour
la même raison, la liaison de copie exclut `Input` et `ClaudhubTerminal` de son
prédicat : le champ de message de commit a sa propre copie.

### Annoter une relecture

Une note porte sur une plage de lignes, et on la renvoie à l'agent qui les a
écrites. Le modèle et tout ce qui se teste sans gpui sont dans `notes.rs` ;
`notes_view.rs` n'est que de la plomberie.

**Une note ne peut pas s'accrocher à la sélection.** `diff_selection` est un
couple d'indices dans la liste *affichée* : il est invalidé par la bascule
unifié/deux colonnes, par un changement de contexte, et par tout rechargement
du diff — c'est-à-dire par chaque écriture de fichier dans le worktree, ce qui
arrive plusieurs fois par minute pendant qu'un agent travaille. On retient donc
des **numéros de ligne**, un **côté** (`Old`/`New` : commenter du code supprimé
a un sens, et une ligne supprimée n'a pas de numéro dans la nouvelle version),
et **l'extrait de code** lui-même.

`notes::relocate` replace la note à chaque arrivée de diff, dans cet ordre :
aux numéros retenus si le texte qui s'y trouve est bien celui qu'on a cité ;
sinon par recherche de l'extrait, ligne à ligne, dans tout le diff ; sinon la
note est dite **décalée** et **reste dans la liste**. Une note perdue en
silence est pire que pas de note du tout.

Trois corollaires :

- **L'ancrage est arrêté au moment du geste**, pas à la validation du
  dialogue : le diff peut changer pendant qu'on écrit la remarque, et la note
  doit porter sur ce qu'on avait sous les yeux.
- **Les marqueurs de gouttière sont calculés en amont** (`notes::marks`, rangé
  dans un `Rc` sur `ReviewState`), à l'arrivée du diff et à chaque
  modification des notes. Jamais dans la fermeture de `uniform_list`, qui
  tourne pour chaque ligne visible à chaque frame. Deux vecteurs, un par
  disposition : les deux listes ne comptent pas les mêmes entrées.
- **Le dialogue n'est pas un popover.** La ligne annotée appartient à une
  liste virtualisée : le moindre défilement emporterait l'ancre, et le popover
  avec.

**L'envoi passe par le terminal, jamais par une API.** Claudhub compose un
prompt (`notes::prompt`, libre et testé) et le livre à l'agent qui, lui, a le
dépôt entre les mains. Deux détails qui se paient cher si on les rate :

- Le texte part en **collage encadré** (`Terminal::paste`), sans quoi un
  message multiligne arrive dans un shell comme autant de commandes validées.
- Le **retour chariot part dans un second envoi**, après un court silence : un
  TUI qui vient de recevoir un collage encadré peut avaler un `\r` arrivé dans
  la foulée, et le message resterait dans l'invite sans partir.
- S'il n'y a pas d'onglet d'agent, on en ouvre un et l'envoi est **différé**
  (`AGENT_WARMUP`) : rien dans un pty ne dit « je suis prêt », et ce qui arrive
  avant l'invite est lu par le shell qu'on n'a pas encore remplacé.

Les notes envoyées passent à `sent`, pas à `done` : c'est la relecture de la
réponse qui les clôt.

### Les notes sur le disque

Une note est du texte qu'on écrit à propos d'un bout de code. La ranger dans un
JSON d'état revenait à l'enfermer là où rien d'autre ne sait la lire : elle vit
donc dans un **dossier de fichiers Markdown**, un fichier par note, sous la
racine que dit `Settings::notes_dir` — vide, `<config>/notes`. Pointée sur un
coffre Obsidian, la relecture d'une branche s'y lit, s'y cherche et s'y relie
comme n'importe quelle note.

**Le dossier est la source de vérité.** Ce qu'on corrige dans Obsidian revient
dans Claudhub au chargement suivant du worktree. C'est ce qui décide de tout le
reste : le format doit être **relu** et pas seulement écrit, d'où le
frontmatter plat de `ui::vault` — un sous-ensemble de YAML assumé, nos clés
n'étant que des scalaires — et l'extrait cité dans un bloc de code dont la
clôture est calculée (un diff de Markdown contient des accents graves, et trois
suffiraient à refermer le bloc au milieu de l'extrait).

Six points, et chacun se paie :

- **`ui::vault` ne touche pas au disque** : il rend du texte et le relit, donc
  il se teste. Les fichiers passent par un worker (`Cmd::ReadNotes`,
  `Cmd::WriteNotes`, `files::read_notes` / `files::sync_notes`) — c'est la
  règle de `src/ui/`, et ici elle a une raison de plus que d'habitude : un
  coffre vit souvent sur un disque synchronisé, parfois sur un montage drvfs de
  WSL, et un `read_dir` dans le thread d'interface s'y paierait en fenêtre
  figée. Le magasin d'état, lui, garde sa dérogation : c'est notre fichier, et
  il fait un kilo-octet.
- **On n'efface que ce qui porte notre marque** (`claudhub:` en tête du
  frontmatter). Le dossier d'un coffre contient les notes de son propriétaire,
  et supprimer une remarque de relecture ne doit pas emporter son journal.
  `sync_notes` aligne le dossier sur la liste **entière** : c'est ainsi qu'une
  note supprimée s'en va, et qu'un fichier renommé dans le coffre ne laisse pas
  un doublon derrière lui.
- **On ne réécrit pas ce qui n'a pas changé.** Un coffre se synchronise, et
  toucher la date d'un fichier à chaque clic ferait travailler la
  synchronisation pour rien.
- **Le nom d'un fichier ne porte que l'identifiant et le fichier relu**, jamais
  les numéros de ligne : une note qui glisse de dix lignes garderait sinon un
  nom différent à chaque écriture, et les liens du coffre pointeraient dans le
  vide.
- **Rien ne s'écrit avant d'avoir lu** (`ReviewState::notes_loaded`). Écrire la
  liste vide qu'on a en mémoire au démarrage effacerait le dossier avant même
  de l'avoir ouvert. Et rien ne s'écrit pour un worktree qu'on n'a pas annoté
  (`notes_on_disk`), sans quoi ouvrir un dépôt sèmerait des dossiers vides dans
  le coffre.
- **La reprise de l'ancien magasin passe par le même chemin que
  l'installation neuve**, comme `migrate_agents` : les notes d'un `state.json`
  antérieur sont versées à l'arrivée du dossier, puis effacées du magasin. Une
  seule fois, et les identifiants déjà pris sont respectés.

### Ce que l'agent sait de Claudhub

L'agent tourne dans un pty, avec le dépôt entre les mains ; ce qui lui manquait
n'était pas un protocole mais une **adresse**. Trois variables la lui donnent,
posées par `TerminalGroup::open` sur tous les onglets — un shell les voit
aussi, et un agent lancé dans un terminal à côté n'a qu'à les recopier :
`CLAUDHUB_WORKTREE`, `CLAUDHUB_NOTES_DIR`, `CLAUDHUB_TODO`.

**Pas de serveur MCP, et c'est une décision.** Les notes sont déjà des fichiers
Markdown dans un dossier dont le disque fait foi : un serveur qui les
exposerait serait un habillage typé de `cat` et de `write`, pour un agent qui
sait ouvrir un fichier. MCP ne gagnerait son prix que sur ce qu'un fichier ne
dit pas — l'état vivant de la fenêtre, les actions sur elle — et rien de ce qui
est listé ici n'en demande. C'est le même raisonnement que « Ce qui tient lieu
de système d'extension » : le niveau le moins cher qui suffit.

Le prompt envoyé avec les notes (`notes-prompt-outro`) dit **où répondre et à
quoi ne pas toucher** : dans le corps du fichier, jamais dans le frontmatter.
Un agent qui passerait une note à `done` défairait la seule chose que cette
liste garantit — c'est la relecture de la réponse qui clôt une note, pas son
envoi. Les variables n'y sont pas développées : elles partent telles quelles,
ce qui garde `notes::prompt` pur et testable.

**Le coffre est surveillé comme le worktree** (`Cmd::WatchDir`, un dossier
sans récursion et sans passer par git). Sans cela, ce que l'agent écrit
n'apparaîtrait qu'au prochain changement de worktree — un aller sans retour.
Deux détails s'y paient :

- **Un dossier qui n'existe pas n'entre pas dans `watched`.** Le coffre d'un
  worktree qu'on n'a pas annoté n'existe pas encore ; s'il y entrait quand
  même, l'ordre renvoyé après sa création serait pris pour un doublon et ne
  poserait rien.
- **C'est l'écriture qui dit que le dossier est là.** `Evt::VaultWritten` ne
  porte pas de contenu — la vue tient ce qu'elle vient d'écrire — mais le fait
  que le dossier existe désormais ; il repose la surveillance et relit, ce qui
  rend aussi la vue à la vérité du disque quand une écriture a été refusée.

### Le panneau « Notes »

Il s'appelait « Relecture » et ne portait que les remarques. Quatre choses se
gèrent au même endroit désormais, parce qu'elles vivent déjà dans le même
dossier : les **tâches**, la **note libre**, les **remarques**, les **fichiers
relus**.

**Ce n'est pas un onglet mais le tiers du bas de la colonne de revue.** Il ne
dit pas ce qu'il y a à lire, il dit où l'on en est — ce qui reste à faire, ce
qu'on a eu à dire —, et cela se lit **pendant** qu'on choisit un fichier, pas à
la place. En onglet, devant « Modifications », il était derrière un clic : c'est
exactement ainsi qu'une liste de tâches cesse d'être tenue.

**Des sections repliables, ni trois panneaux ni des sous-onglets.** Trois
panneaux feraient trois onglets pour un seul sujet — et « Modifications » et
« Revue de branche » en sont deux parce qu'on les regarde *ensemble*, ce qui
n'est pas le cas ici. Des sous-onglets demanderaient un clic pour savoir où en
est l'agent. Un seul défilement, trois en-têtes qui portent chacune son compte,
et replier rend la hauteur à celle qu'on lit.

Trois détails qui se paient :

- **Le repli est porté par le titre, pas par la ligne entière.** Les boutons
  d'une section vivent sur son en-tête, et un clic sur « envoyer tout »
  remonterait replier ce qu'on vient d'agir.
- **Il ne se persiste pas.** C'est une posture de lecture, qui change plusieurs
  fois pendant une relecture, pas une préférence qu'on s'attend à retrouver le
  lendemain — d'où `ClaudhubApp::notes_collapsed`, en mémoire.
- **Une section vide se réduit à une ligne grise** (`section_empty`) et non à
  l'état vide pleine hauteur des panneaux : trois sections partagent ce
  défilement, et celle qui n'a rien ne doit pas pousser les deux autres hors de
  vue.

La barre du panneau porte le **chemin du coffre** et de quoi l'ouvrir
(`file://`, donc le bureau décide avec quoi). Il n'apparaissait nulle part, et
un coffre qu'on ne sait pas retrouver est un coffre qu'on n'ouvre pas dans
Obsidian.

**Le prompt se voit avant de partir.** Ce qui entre dans un terminal ne se
rattrape pas : l'agent a lu le collage avant qu'on ait vu ce qu'on envoyait. Le
dialogue est aussi l'endroit où l'on ajoute d'une phrase ce que les notes ne
disent pas. Les notes ne passent à `sent` qu'à la validation.

**« Tout rendre à relire »** vit dans la section des fichiers relus : on coche
fichier par fichier, et reprendre une revue depuis le début demandait sinon
autant de clics que la branche a de fichiers.

### La liste de tâches d'un worktree

`TODO.md`, dans le même dossier que les notes. C'est là que l'agent tient sa
progression, et c'est le seul fichier du coffre que Claudhub **n'écrit pas en
entier**.

- **Cocher retourne un caractère**, à une ligne connue (`vault::toggle_task`).
  Le reste du fichier est recopié tel quel : les sous-listes, les liens et la
  prose que l'agent met entre les tâches survivent, ce qu'un rendu à partir de
  nos seules structures ne garantirait jamais.
- **L'écriture est conditionnelle**, comme celle d'un fichier ouvert dans
  l'éditeur : l'empreinte de ce qu'on avait sous les yeux repart avec
  l'écriture, et un fichier que l'agent a touché entre-temps la fait refuser.
  C'est la seule façon de ne pas effacer d'un clic ce qu'il vient de cocher.
- **`sync_notes` ne l'efface pas.** La purge du dossier ne porte plus sur la
  marque `claudhub:` mais sur sa **valeur** (`files::is_ours` : `note` et
  `review`) : `claudhub: todo` porte notre marque sans nous appartenir, et une
  note supprimée emportait sinon la liste de tâches en cours.
- **Claudhub ne le crée pas tout seul** : il naît de la première tâche qu'on
  ajoute, comme le dossier de notes que `notes_on_disk` retient de semer.
  « Créer une liste vide » n'est pas un geste qu'on a envie de faire, et
  l'agent, lui, crée le sien par `$CLAUDHUB_TODO`. Le fichier posé explique son
  propre format — il finit dans un coffre, ouvert par quelqu'un qui n'a pas lu
  ceci.
- **Une tâche s'ajoute après la dernière**, pas à la fin du fichier
  (`vault::append_task`) : un agent écrit sous sa liste — ce qu'il a compris,
  ce qui reste à décider —, et une tâche posée après cette prose ne se lirait
  plus comme faisant partie de la liste.
- **Tout s'y édite en place, jamais dans un dialogue.** Une ligne de saisie en
  bas de la liste ajoute — elle est toujours là, et Entrée la laisse prête pour
  la suivante, une liste se remplissant d'une traite ; un clic sur un libellé
  le remplace par sa saisie, à sa place ; un libellé vidé **supprime** la
  tâche, ce qui est la convention de ces listes partout ailleurs et évite un
  bouton de plus par ligne (il y en a un quand même, pour qui ne connaît pas la
  convention). Le bouton `+` de l'en-tête ne fait que donner le focus à la
  ligne du bas : deux façons d'ajouter qui n'aboutiraient pas au même endroit
  seraient une de trop.
- **Deux zones de saisie et non une** (`task_input`, `task_edit_input`) : ce
  qu'on est en train de taper pour une nouvelle tâche ne doit pas disparaître
  parce qu'on corrige une faute deux lignes plus haut.
- **Perdre le focus valide.** `InputState` n'a pas d'événement d'échappement, et
  abandonner une correction parce qu'on a cliqué à côté serait le plus mauvais
  des deux défauts.
- **Les trois gestes passent par `rewrite_todo`** : une transformation pure qui
  rend `None` quand la ligne visée n'est plus ce qu'elle était — l'agent a
  écrit entre-temps —, et une écriture qui repart avec l'empreinte de ce qu'on
  avait sous les yeux.
- Il s'affiche **au-dessus** des notes et hors de leur défilement : c'est ce
  qu'on regarde pour savoir où en est l'agent, et le faire descendre avec une
  revue de trois cents notes reviendrait à ne jamais le voir.

### La note libre d'un worktree

`NOTES.md`, à côté des tâches et des remarques : ce qu'on écrit *sur* le
travail en cours et qui ne porte sur aucune ligne — une piste, une décision, ce
qu'on reprendra demain. Elle s'édite sur place dans le panneau, sans dialogue
ni bouton « enregistrer » : c'est un bloc-notes, et devoir le valider est le
meilleur moyen d'y perdre trois phrases.

Quatre choses, et trois d'entre elles la distinguent de tout le reste du
coffre :

- **Pas de frontmatter.** C'est du Markdown ordinaire, celui qu'on tiendrait de
  toute façon dans son coffre. Son absence de marque la met du même coup hors
  de portée de la purge de `sync_notes`.
- **Vide, elle n'existe pas.** Un fichier vide et un fichier absent ne se
  distinguent pas dans un coffre, et laisser une coquille par worktree ouvert
  est exactement ce que `notes_on_disk` évite ailleurs — d'où
  `files::write_vault_file`, où un texte vide **efface**, sous la même
  empreinte que l'écriture.
- **L'écriture est différée d'une seconde**, comme les réglages et pour la même
  raison : une zone de saisie émet une valeur par frappe, et un coffre se
  synchronise.
- **Une seule zone de saisie pour tous les worktrees**, dont le contenu suit
  celui qui est affiché. Elle n'est **jamais** rechargée pendant qu'on y écrit :
  le coffre est relu à chaque écriture — la nôtre comprise —, et remettre le
  texte du disque sous les doigts déplacerait le curseur au milieu d'une
  phrase. Ce qui arrive d'ailleurs pendant qu'on tape attend le prochain
  chargement, et c'est le bon arbitrage : deux mains sur le même paragraphe
  n'ont pas de fusion.

### Marquer un fichier relu

Une revue de branche fait couramment plusieurs centaines de fichiers, et rien
ne disait où l'on en était. Une coche par ligne le dit, et **un clic sur un
dossier vaut pour tout son sous-arbre**, replié compris — c'est le geste qui
vaut le détour, on relit un dossier entier bien plus souvent qu'un fichier
isolé.

**Une coche et non une case.** La case à cocher de cette liste veut déjà dire
« indexer » ; deux cases côte à côte pour deux gestes sans rapport se
confondraient au premier coup d'œil. La coche vit à droite, après le volume ;
une ligne relue prend un **fond vert** et son nom s'éteint — c'est ce qui fait
que la liste dit d'un coup d'œil ce qu'il reste à lire, là où une colonne de
coches se parcourt. La sélection passe devant le vert : perdre de vue l'endroit
où l'on est est pire qu'oublier une ligne déjà lue.

**Le volume retenu est ce qui périme la coche.** `vault::Reviewed` garde
`+n −m` au moment du clic, et `review::rows_for` n'allume la coche que si le
fichier les porte encore ; `Evt::DiffFiles` purge celles qui ne valent plus.
Sans cela, un agent qui réécrit un fichier laisserait une liste qui dit « relu »
d'un contenu que personne n'a lu — c'est le même garde que l'empreinte repassée
à l'écriture d'un fichier. L'approximation est assumée, comme celle des agents
« en cours » : une modification qui laisse le volume inchangé passe au travers.

Le suivi vit dans le même dossier que les notes, en cases à cocher Markdown
(`vault::INDEX`) : Obsidian les rend cliquables, et **décocher là rend le
fichier à relire ici**. Seuls les fichiers cochés y figurent — l'autre liste
n'a pas de bord. Le titre d'une section est la clé du domaine (`working`,
`branch master`) et non son libellé traduit : changer la langue de l'interface
ne doit pas rendre illisible ce qu'on a déjà écrit.

### Les profils d'agent

Un seul `agent_command` ne suffisait plus dès qu'annoter une relecture veut
dire l'envoyer : on veut choisir à qui. `Settings::terminal.agents` est donc une
liste de profils — nom, commande, arguments, environnement — et
`default_agent` désigne celui que le bouton lance.

**L'environnement est ce qui porte le modèle.** `ANTHROPIC_MODEL`, une clé par
profil : « configurer plusieurs modèles » n'appelle aucune dépendance HTTP,
seulement une variable de plus dans le pty. C'est le corollaire de la décision
de cadrage — l'IA passe par l'agent du terminal, jamais par une API depuis
Claudhub.

**`command` et `args` sont séparés**, et une ligne de commande se découpe par
`settings::split_command`, qui honore les guillemets. `split_whitespace` cassait
sur tout chemin contenant une espace, et c'est le genre de panne qu'on ne
comprend qu'après avoir lu le code. `join_command` refait le chemin inverse ;
l'aller-retour est testé, parce que le formulaire écrit des morceaux et les
relit en une ligne.

**La reprise de l'ancien réglage passe par le même code que l'installation
neuve** : `migrate_agents` ne fait quelque chose que si `agents` est vide, et
`#[serde(default)]` fait justement qu'un fichier antérieur est lu ainsi.
`agent_command` est vidé après coup, pour que la reprise n'ait lieu qu'une fois.

**`Cmd::ScanAgents` prend la liste entière des programmes.** Un agent lancé
depuis un terminal à côté compte autant que celui qu'on a démarré ici ; n'en
chercher qu'un n'en verrait qu'un sur deux. `agent::Process` retient lequel a
été reconnu, et le sélecteur de worktree le nomme — à deux profils près, « un agent
travaille ici » ne dit pas lequel.

Piège du formulaire : la clé d'état d'une ligne de la table porte **le nombre
de profils** (`claudhub-agent-{n}-{i}`). `use_keyed_state` garde un état par
clé ; sans le compte, supprimer le premier profil laisserait les champs de la
ligne 0 remplis avec l'ancien, et l'on écrirait dans les réglages ce qu'on
croyait avoir supprimé. Renommer, lui, ne change pas le compte — les champs
gardent donc leur curseur pendant la frappe.

### La répartition du chrome

La barre d'outils ne porte que des **actions**, et c'est ce qui décide de ce
qui monte et de ce qui descend. L'écran regardé, ce qui tourne en ce moment et
ce qu'une opération vient de répondre décrivent l'endroit où l'on est : ils
vivent dans la barre d'état, qui ne portait qu'un message épisodique et restait
vide la plupart du temps pendant que la barre du haut débordait. C'est aussi ce
qui lui a valu le choix de l'écran, plutôt qu'une seconde barre juste au-dessus
d'elle — voir « Les sous-applications ».

**Le worktree et la branche sont remontés**, eux, parce qu'ils ont cessé
d'être des mots qu'on lit : ce sont les deux sélecteurs qui pilotent tout le
reste de la fenêtre, et un sélecteur est une action. C'est la barre de titre de
Zed, et `ui::topbar` est le module qui la tient.

**Les panneaux « Dépôts » et « Branches » n'existent plus.** Ils ne sont pas
masqués par défaut, ils sont supprimés : deux colonnes qui redisaient ce que la
barre du haut porte désormais, et qui coûtaient à la revue toute leur largeur.
La revue et Sentry n'ont donc plus de colonne de gauche du tout ; l'édition et
les bases gardent la leur, l'arbre du projet et celui du schéma, seuls
désormais. `LAYOUT_VERSION` passe à 13 — une disposition d'avant nomme deux
panneaux que le registre ne sait plus construire.

Ce qu'ils portaient est donc monté avec eux, et c'est la moitié du travail :

- **Le nom du dépôt précède celui du worktree**, en gris. Deux worktrees
  appelés `main` dans deux dépôts est le cas courant, pas l'exception.
- **Les lignes sont celles de l'ancienne barre latérale, à l'identique.** Même
  densité, même hiérarchie — nom en `text_sm`, branche en `text_xs` grisé, le
  badge d'agent et le volume `+n −m` à droite — et la colonne d'icône est celle
  **du menu** (`PopupMenuItem::icon` + `checked`), pas une colonne à nous : la
  coche y remplace l'icône sur la ligne courante, comme dans tous les autres
  menus de la fenêtre. Un rendu à nous alignait ses noms un cran à côté des
  leurs.
- **La liste est un instantané pris à l'ouverture du menu.** `dropdown_menu`
  reconstruit ses entrées à chaque fermeture, donc elle est fraîche à chaque
  fois ; mais les fermetures de rendu d'une entrée tournent à chaque frame tant
  que le menu est ouvert, et y relire l'application ferait un emprunt par ligne
  et par image.
- **Un bouton dans une ligne de menu consomme son clic.** Le `+` d'un dépôt —
  nouveau worktree —, le `x` d'un dépôt introuvable, le bouton qui ouvre un
  worktree sur une branche : sans `stop_propagation`, chacun fermerait le menu
  qu'on est en train de parcourir. Les en-têtes qui les portent sont `disabled`
  pour que la ligne elle-même ne soit pas cliquable.
- **Les dépôts qui ne s'ouvrent plus sont en bas de la liste**, avec ce que git
  a répondu : c'est le seul endroit d'où on peut les retirer, et un dépôt qui
  n'apparaît nulle part ne se retire pas non plus — voir « Un dépôt qui n'est
  plus là ».
- **Le sélecteur de branche liste `branches::rows_for`**, la fonction même que
  le panneau utilisait : les locales sous leur titre, puis les distantes sans
  jumelle locale. Cliquer extrait ; le bouton de la ligne ouvre un worktree sur
  cette branche, qui est le geste d'ouverture d'une relecture quand le travail
  d'un agent a atterri sur une branche que personne n'a extraite. Une branche
  déjà déployée ailleurs est **grisée et dit chez qui** — git refuse deux
  extractions de la même branche, et une entrée qui ne sait répondre que par
  une erreur est pire qu'une entrée éteinte.
- **Ce qui manque, et c'est assumé : la recherche.** Un `PopupMenu` n'a pas de
  champ de filtre ; la liste défile (`scrollable`, quatre cent vingt pixels).
  Sur un dépôt à quatre-vingts branches, c'est un défilement là où il y avait
  un champ.
- **Le menu du worktree devient un bouton** (`…`, `render_worktree_actions`).
  C'était le clic droit d'une ligne de la barre latérale ; un clic droit a
  besoin d'une ligne où atterrir, et il n'y en a plus ici. Il porte ce qu'il
  portait — mise à jour, intégration, `wt up`/`down`, les tâches du projet —
  plus deux gestes qui n'avaient plus de maison : supprimer le checkout quand
  `wt` n'en tient pas la comptabilité, et **retirer le dépôt de la liste**, en
  dernière entrée derrière un séparateur.
- **L'état `wt` et le lien du projet suivent** : la pastille « démarré » et le
  bouton qui ouvre l'URL vivaient sur la ligne du worktree, ils sont à côté du
  bouton d'actions, pour le worktree affiché.
- **L'avance et le retard suivent la branche.** Ils se lisent avec elle, et les
  laisser en bas les aurait séparés de ce qu'ils qualifient. La barre d'état ne
  parle donc plus du dépôt du tout.

**La barre du haut *est* la barre de titre de la fenêtre**
(`gpui_component::TitleBar`, posée par `topbar::render_topbar`). Ce n'est pas un
raffinement : `TitleBar::title_bar_options()`, qu'ouvre `ui::run`, demande à la
plateforme de ne pas en dessiner une. Tant que rien ne la remplaçait, la
fenêtre Windows n'avait plus de quoi être déplacée, réduite ni fermée — sous un
gestionnaire de fenêtres pavant, où il n'y a de toute façon pas de décoration,
cela ne se voyait pas. En empiler une au-dessus de la nôtre coûterait trente
pixels pour redire ce qu'elle dit déjà ; c'est le raisonnement qui a fait
descendre le choix de l'écran dans la barre d'état. Elle garde donc notre
hauteur (`theme::toolbar_height`) et nos couleurs, et gagne le glissement, le
double-clic qui agrandit et les boutons de la fenêtre.

Deux choses à savoir : `ui::run` part de `TitleBar::window_options()` et non de
`Default`, qui pose aussi `app_owns_titlebar_drag` — sans lui, la plateforme et
notre barre se disputent le double-clic. Et **les boutons posés dans la zone de
déplacement restent cliquables** : la région est rendue en `HTCAPTION`, mais
gpui traite les messages souris de zone non cliente et les redistribue. C'est
ce que fait la barre de titre de Zed, aux mêmes conditions. Les boutons de la
fenêtre, eux, sont **hors** de cette zone — un `WindowControlArea` ancêtre
l'emporterait sur le leur.

**Une action va où se fait le geste dont elle est la fin.** `fetch`, `pull` et
`push` vivent donc dans la barre du panneau « Modifications » et non en haut de
la fenêtre : on y regarde ce qui a changé, on coche, on valide, et pousser est
le mot suivant de la même phrase — la tenir à l'autre bout de l'écran faisait
traverser la fenêtre pour la terminer. Ce qui reste en haut ne parle pas du
dépôt regardé : le menu de l'application et le nom du worktree. Les terminaux,
eux, se basculent depuis le **coin en bas à droite** de la barre d'état, à
l'angle de la fenêtre sur lequel ils s'ouvrent — en haut, ils étaient à l'autre
bout de l'écran du panneau qu'ils montrent.

**La barre du diff est en trois groupes**, et ce n'est pas de la mise en forme :
la taille minimale d'un élément flex vaut celle de son contenu, si bien qu'un
chemin long poussait les boutons de droite hors de la barre, sans que rien ne
les montre ni ne le dise. Le chemin porte donc `min_w_0` — il est le seul à
céder — et les deux groupes de boutons refusent de rétrécir. C'est le même
piège que celui des barres de défilement et celui des champs du formulaire des
bases.

**Un seul bouton de cette barre porte son mot** : « Éditer ». On relit un diff,
quelque chose cloche, on ouvre le fichier là où c'est — parmi six icônes qui
parlent toutes de lecture, celle qui écrit vaut d'être lue plutôt que devinée.

**`pull` et `push` portent leur compte et s'allument** quand il y a quelque
chose à faire — l'avance et le retard sur l'amont, tels que le statut les
rapporte. La barre d'état les affiche déjà, mais elle dit *où l'on est* ; sur
le bouton, le même nombre dit *ce qu'il y a à faire*, et un bouton éteint dit
qu'il n'y a rien à faire, ce qui est la moitié de ce qu'on cherche en arrivant
sur un worktree. Le compte ne vaut évidemment que si les références distantes
sont fraîches — voir le fetch automatique.

Les états vides portent une icône et, quand une action s'impose, un bouton :
une phrase grise ne dit pas quoi faire.

**Toute entrée de menu porte une icône** — clic droit comme menu déroulant.
Un menu contextuel se parcourt à la verticale et se choisit au geste, pas à la
lecture : l'icône est ce qui rend une entrée reconnaissable avant d'avoir lu
son libellé, et une seule entrée sans icône décale toutes les autres. Deux
entrées qui font la même chose sur deux objets — copier un chemin relatif, en
copier un absolu — portent la même : le libellé les distingue, l'icône dit de
quelle famille de gestes il s'agit. Les glyphes manquants sont pris chez
Lucide, à la version que porte déjà `assets/icons/` ; un nom d'icône qui ne
désigne aucun fichier ne provoque **aucune erreur**, il peint un vide.

### Le grain de l'interface

Ce qui datait n'était pas une couleur mais une **géométrie** : des rectangles
cousus bord à bord, des filets partout, et les rayons par défaut de
gpui-component — six et huit pixels, ceux d'un formulaire web. Quatre décisions,
et aucune ne demande de reprendre le dock.

**Les rayons montent à huit et douze**, dans `theme::apply`, et seulement si la
palette ne s'en occupe pas : `radius`, `radius.lg` et `shadow` sont des clés de
`ThemeConfig`, et un thème qui les déclare a choisi son grain. Ils portent tout
ce que la fenêtre affiche — boutons, champs, menus, dialogues.

**La carte, c'est le groupe d'onglets entier** — barre comprise. C'est le fork
qui la peint : `TabGroupSkin::frame` s'arrondit hors variant classique, son
clip emporte le coin sur la barre et le contenu à la fois, et les splits
s'espacent d'une gouttière de quatre pixels — c'est elle qui fait lire des
cartes plutôt que des rectangles échancrés. `panels::pane_root` ne dessine
donc **plus rien** : redessiner une carte à l'intérieur remettrait la couture
qu'on vient d'enlever. La barre est **sur la carte** (`tab_bar_segmented` =
fond), et la pastille active porte un ton surélevé (`tab_active` =
`secondary`) — sur la carte, elle serait invisible, et c'est précisément
pourquoi le fork fait lire ce jeton à la pastille Segmented au lieu de son
`background` codé en dur.

**La gouttière reste `tab_bar`**, dérivée du fond de quelques pour cent de
clarté en moins : c'est la couleur que le `split_frame` du fork peint entre
les cartes, et que la vue racine peint derrière tout — poignées de
redimensionnement, zones repliées appartiennent à ce plan-là.

**Le masque de contenu de gpui est rectangulaire.** L'arrondi d'un élément ne
rogne que son propre fond et sa bordure, jamais ses enfants : la carte
arrondie du groupe tenait en haut parce que le rail des onglets est en
retrait, et perdait ses coins bas sous le fond carré du contenu. D'où le
`rounded_b` du fond de `pane_root` — en bas, c'est lui qui a le dernier mot.
Corollaire pour la disposition par défaut : la moitié **fixe** d'une division
est celle du bas, jamais celle du haut — l'aire du dock est plus petite que la
fenêtre, et deux tailles fixes qui somment à sa hauteur font déborder la
colonne, coins coupés et gouttière avalée.

**Tout panneau passe par `panels::pane_frame`**, et pas seulement ceux que la
macro `panels!` fabrique : c'est lui qui peint le fond arrondi du bas, et le
panneau qui s'en dispensait — les terminaux, écrits à la main — avait deux
coins carrés au bas de sa carte sans que rien ne le signale. `pane_root` n'y
ajoute que la note du panneau touché, dont les terminaux n'ont que faire : la
recherche y appartient au programme qui tourne.

**La vue racine rembourre le dock des mêmes quatre pixels** que le fork met
entre les cartes : sans ce `p(4.)`, les zones touchent les bords de la
fenêtre, la barre du haut et la barre d'état, et les cartes ne respirent que
de l'intérieur.

**La poignée de redimensionnement ne peint rien au repos.** La couche de base
(`gpui-base`) tient **sa propre copie** du thème — poignées et barres de
défilement se peignent sans passer par gpui-component — et `theme::apply` doit
la projeter (`Theme::sync_base`) après ses retouches, faute de quoi rien
d'écrit sur le thème ne les atteint. La projection reconstruit la copie à
partir de zéro, donc l'extinction de la poignée (`resizable.handle`
transparent) vient **après** elle. La poignée reste saisissable — sa zone est
plus large que son trait — et se montre pendant le glissement, où elle est une
information ; au repos, une ligne grise recoudrait les cartes que la gouttière
vient de séparer. C'est pour écrire dans ce global que `gpui-base` est une
dépendance directe.

**Une ligne de liste est une pastille, pas une bande.** Le fond d'une ligne
survolée ou sélectionnée s'arrête avant les bords, et il est arrondi. Piège à
connaître : **`uniform_list` ignore les marges de ses entrées**, dont il calcule
lui-même la taille. Le retrait appartient donc à la **liste**
(`.px_1()` sur l'`uniform_list`), et l'entrée ne porte que son rayon ; là où la
liste n'est pas virtualisée, un `mx_1` sur la ligne suffit.

**Les onglets sont des pastilles** (`TabVariant::Segmented`), posé par
`dock_skin.set_tab_variant` à la construction de l'aire. Le variant par défaut
du dock, `Tab`, a un rayon **codé en dur à zéro** et rien dans le thème ne
l'atteint ; `Tab::with_variant` et `TabBar::with_variant` existent pourtant,
c'est seulement le panneau d'onglets du dock qui ne les transmettait pas. D'où
le **fork** (voir `Cargo.toml`) : neuf commits au-dessus de leur `main` — le
`TabVariant` que `DockSkin` fait passer jusqu'au `TabBar` ; les coins en boîte
bordée du bandeau réservés au variant classique, dont ils épousent les
rectangles ; le groupe lu comme une carte hors variant classique — cadre
arrondi, splits espacés, pastille Segmented sur le jeton `tab_active` au lieu
d'un `background` codé en dur — ; la même gouttière entre les **zones** du
dock (gauche, bas, centre) ; et `split_gap`, un crochet du rendu que l'aire
prend en **rembourrage** dans chaque case d'un split sauf la première — un
`gap` CSS sur le cadre du split n'espaçait rien, ce cadre n'ayant qu'un
enfant (le groupe redimensionnable), et une marge aurait faussé les tailles
que la mécanique de redimensionnement distribue.

Un sixième, qui ne parle pas du dock : **une sélection reste peinte quand le
focus part à un menu**. `layout_selections` rendait `None` dès que le champ
perdait le focus, si bien qu'un clic droit sur une sélection ouvrait son menu
par-dessus un texte qui n'avait plus l'air sélectionné — sur le seul geste pour
lequel la sélection est justement conservée, comme le dit leur propre `on_blur`.
Elle est désormais peinte dans le ton atténué déjà calculé pour les occurrences
de recherche, la sélection inactive que dessine tout éditeur.

Un septième, qui n'en parle pas non plus : **un contrôle peut cacher son caret
sans se désactiver** (`set_cursor_hidden`). C'est ce que demande un éditeur
modal, dont le curseur bloc est peint par-dessus le caractère sous le curseur —
voir « Les modes de vim dans l'éditeur ».

Un huitième, qui ne parle pas du dock non plus : **le fond d'un run est peint**.
`ShapedLine::paint` ne dessine que les glyphes, les soulignés et les barrés ; le
`background_color` d'un run appartient à `paint_background`, que rien dans
l'input n'appelait. Une `TextDecoration` qui en pose un — la façon documentée,
calquée sur Monaco, pour une application de marquer une plage — était donc
**invisible en silence** : ni erreur ni avertissement, la plage se peignait comme
le reste du fichier. C'est ce qui empêchait à la fois le curseur bloc de vim et
l'éclat d'un yank de s'afficher, et le symptôme se lisait « le curseur
n'apparaît qu'après un déplacement » — ce qu'on voyait alors était la
*sélection*, que seule une frappe pose. `LineLayout::paint` fait désormais une
passe de fond avant la passe de glyphes.

Un neuvième, qui ne parle pas du dock non plus : **une application peut replier
sans passer par la gouttière** (`fold_candidates`, `is_fold_candidate`,
`is_folded_at`, `set_folded`, `unfold_all` sur `InputState`). Les icônes de la
gouttière replient depuis l'intérieur de l'élément, `display_map` étant privé :
cela sert une souris et rien d'autre, et un éditeur modal n'avait aucun moyen
d'entrer — voir « Les modes de vim dans l'éditeur ».

Les commits ont vocation à partir en PR, et le fork à disparaître avec elle.

**`PanelStyle::TabBar`, et non le défaut `Auto`** : `Auto` rend un titre plat
dès qu'un groupe n'a qu'un panneau, et « Terminaux » ou « Éditeur » n'avaient
pas le même bandeau que leurs voisins — deux chromes pour une même fenêtre.

**Les couleurs posées sur le thème ne suffisent pas** : `Theme::tokens` est
dérivé de `Theme::colors` une seule fois, à l'application de la palette, et les
composants récents — la barre d'onglets du dock en tête — lisent `tokens`.
Toute couleur écrite dans `theme::apply` doit être suivie du recalcul
(`ThemeTokens::from(&theme.colors)`), sans quoi elle ne se voit nulle part et
rien ne le signale.

### Les thèmes

Une douzaine de palettes sont livrées — One Dark, Nord, Dracula, Tokyo Night,
Gruvbox, Catppuccin, Solarized, Synthwave '84. Les couleurs sont celles que
leurs auteurs publient ; seule la répartition des rôles est de nous.

Elles sont **générées** par `tools/gen_themes.py` à partir d'une palette d'une
quinzaine de couleurs. Un thème complet en compte une centaine, et les écrire à
la main serait une source d'erreurs muettes : une clé absente ne provoque pas
d'erreur, elle reprend la valeur par défaut, qui est *claire* — soit une tache
blanche au milieu d'un thème sombre que rien ne signale. Deux tests
verrouillent cela : chaque fichier livré doit se lire, et aucun ne doit avoir
moins de clés que les autres.

Le registre de gpui-component ne se charge **que depuis un répertoire**, qu'il
surveille. Les thèmes sont donc embarqués dans le binaire puis écrits dans
`<config>/themes/` au démarrage. L'effet de bord est heureux : le même
répertoire accueille les thèmes de l'utilisateur, et un fichier modifié est
rechargé sans relancer Claudhub. Corollaire à dire : les fichiers `claudhub-*.json`
sont réécrits à chaque démarrage — pour en modifier un, il faut le copier sous
un autre nom.

Deux réglages et non un seul : `theme` dit s'il fait clair ou sombre (le
système peut en décider), `light_theme` et `dark_theme` disent *quelle* palette
porte chacune des deux apparences. C'est la structure de `Theme` lui-même, et
la seule qui ne mente pas quand l'apparence suit le système.

Le chargement du registre est asynchrone : au premier `apply`, le thème choisi
n'y est pas encore, d'où la ré-application dans le rappel de `watch_dir`.

### Les sous-applications

Claudhub fait quatre métiers qui n'ont presque rien en commun : relire un
diff, retoucher un fichier, interroger une base, dépouiller une erreur. Tant
qu'ils partageaient une seule fenêtre, chacun payait la place des trois
autres — huit onglets au centre dont on n'en regarde jamais que deux, et un
panneau central qui changeait de nature selon le dernier geste.

Il y a donc cinq **écrans** (`ui::workspace::Workspace`) : Revue, Édition,
Bases, Sentry, Réglages. On passe de l'un à l'autre par la barre d'état, ou
par `Alt+1` à `Alt+5`. Les Réglages viennent en dernier, étant le seul qui ne
soit pas du travail — voir « Les réglages ».

**Un dock par écran, et non un dock dont le centre change.** Chacun a ses
panneaux, ses onglets et ses tailles, mémorisés séparément : régler la revue
ne déplace plus rien sur l'écran des bases. Les cinq sont **construits au
démarrage** et non à la première visite — un dock se bâtit avec `window`, et le
faire au rendu reviendrait à créer des entités au milieu d'une frame ; le coût
est une vingtaine de panneaux, qui ne portent aucun état.

**Une seule vue est partout : les terminaux**, ce à quoi on parle pendant qu'on
regarde n'importe lequel des écrans. C'est le seul panneau instancié une fois
**par dock** : un panneau n'appartient qu'à une aire à la fois, et un seul dock
est affiché. *Où* l'on travaille, en revanche, n'est plus un panneau du tout :
c'est le sélecteur de la barre du haut, qui vaut pour les cinq écrans — voir
« La répartition du chrome ».

**Trois écrans n'ont pas de colonne de gauche**, d'où l'`Option` de
`install_default_layout` : la revue et Sentry prennent toute la largeur, et le
formulaire des réglages a déjà sa propre barre latérale de pages — deux côte à
côte feraient deux listes à lire avant d'atteindre un champ.

**Le panneau central cesse d'être partagé** : le diff appartient à la revue,
l'éditeur à l'édition, la console SQL aux bases. C'est ce que la découpe achète
de plus visible, et le titre de l'onglet redevient une constante.

Six points qui ne se devinent pas :

- **Rien à faire en changeant d'écran que de changer de dock.** L'état — le
  worktree choisi, le fichier ouvert, la requête en cours — vit dans
  `ClaudhubApp` et non dans les panneaux : il est donc le même de tous les
  côtés, et c'est ce qui rend la bascule instantanée.
- **Rien, sauf le focus** (`focus_workspace`). On entre sur un écran pour
  travailler dans ce qu'il porte au centre, et le focus restait où l'écran
  précédent l'avait laissé — le plus souvent un terminal, à qui appartiennent
  alors les flèches, les touches de vim et la copie. L'éditeur et la console
  sont des champs de texte, donc nommés un par un ; partout ailleurs — le diff,
  Sentry, les réglages — c'est le manche de la racine, celui contre lequel les
  raccourcis de la fenêtre se résolvent. Un champ qu'on ne voit pas n'est jamais
  visé : le panneau peut être masqué, et il n'y a pas toujours un fichier ouvert
  ni une console — l'écran « Édition » rend alors le focus à l'arbre du projet,
  dont les flèches sont ce qu'on y cherche.
- **Un geste qui ouvre quelque chose emmène sur son écran.** Ouvrir un fichier
  bascule sur « Édition », ouvrir une console sur « Bases », « ajouter une
  connexion » sur « Réglages ». Le geste vient parfois d'ailleurs — une ligne
  de diff, le menu d'une table —, et y répondre en silence sur l'écran d'à côté
  serait un travail fait que personne ne voit.
- **Les tailles d'une division se donnent toutes, et ici ça se paie
  vraiment.** Un `None` vaut cent pixels dans l'état enregistré, et la pile
  répartit **au prorata** de ce qu'elle y lit : un centre décrit
  `[None, 220]` s'affiche à 31 / 69 au lieu de 76 / 24. La disposition d'un
  écran qu'on n'a jamais ouvert n'est jamais mesurée — elle garde ses valeurs
  de construction jusqu'à la première visite —, si bien que le défaut mal
  écrit se voyait sur trois écrans sur quatre. Les largeurs se donnent sur
  celle du **centre** et non de la fenêtre, pour la même raison de prorata.
- **Le choix de l'écran vit dans la barre d'état**, à son extrémité gauche, et
  non dans une barre à lui. Les deux se suivaient, hautes de trente pixels à
  elles deux pour porter quatre boutons et un nom de branche — deux bandeaux
  gris empilés sous la fenêtre, là où le dock se bat pour chaque ligne. Elles
  disent d'ailleurs la même chose : *où* l'on est. La branche, l'avance sur
  l'amont et l'écran regardé sont trois façons de répondre, et elles se lisent
  d'un coup d'œil sur une seule ligne. La barre passe donc à
  `theme::toolbar_height` : elle porte des boutons désormais, et vingt-deux
  pixels les feraient déborder. Elle est peinte par la **vue racine** et non
  par un panneau — un panneau se glisse ailleurs et se masque, et la navigation
  ne peut pas partir avec lui. C'est le même raisonnement qui a fait des
  sélecteurs de worktree et de branche des boutons de la barre du haut plutôt
  que des panneaux.
- **L'écran actif est plein, les autres en contour.** L'état « sélectionné »
  d'un `ButtonGroup` en contour n'est qu'un fond à peine plus clair, invisible
  sur la moitié des thèmes — c'est le constat qui avait déjà décidé du choix
  du moteur d'une connexion, et « où suis-je » est exactement la question que
  cette barre doit répondre sans qu'on la cherche.

L'écran qu'on regardait en fermant revient à l'ouverture. Il est retenu dans
`layout.json` et non dans les réglages : c'est l'état d'une fenêtre, au même
titre que la place des panneaux, pas une préférence qu'on écrit à la main.

### Le dock

La disposition appartient à `gpui_component::dock` : c'est lui qui gère le
glissement d'un panneau d'une zone à l'autre, les onglets et les zones
d'accueil. Chaque zone est donc une **entité à part** — `ui/panels.rs` — parce
que le dock ne sait déplacer que des entités.

Les panneaux ne portent aucun état : ils délèguent à `ClaudhubApp`, dont ils ne
gardent qu'une référence **faible** — forte, elle formerait un cycle,
l'application tenant le dock qui tient les panneaux. Ils l'observent, sans quoi
ils garderaient l'image de l'état au moment de leur construction.

Rendre depuis un `update` sur `ClaudhubApp` est licite : le rendu d'une vue enfant
a lieu *après* que la fermeture de rendu du parent a rendu la main, donc hors
de cet emprunt.

Six pièges, tous rencontrés :

- **Un panneau sans pile parente est verrouillé.** `is_locked` rend vrai quand
  le groupe n'a pas de pile au-dessus de lui, et rien ne se glisse ni ne
  s'accueille plus. Tout panneau doit donc être enveloppé, fût-ce dans une
  division d'un seul élément.
- **`toggle_dock` ne notifie pas l'aire**, seulement le dock intérieur :
  l'observation qui enregistre ne se déclenche pas toute seule, d'où l'appel
  explicite.
- **Un panneau ne s'active pas au clavier** — c'était vrai en 0.5.1, où
  `TabPanel::set_active_ix` était privé ; la refonte du dock l'a rendu
  possible : `TabGroup::select_tab` est public, et un panneau reçoit son groupe
  par `Panel::on_added_to`. C'est ce qui permet aux terminaux d'être des
  panneaux (voir « Les terminaux dans le dock »).
- **Le dernier panneau d'une zone ne se déplace pas.** `is_last_panel` remonte
  la pile : un panneau seul dans un `TabPanel` seul dans son conteneur est
  figé. C'est pourquoi les terminaux vivent dans le centre, sous la revue, et
  non dans une zone d'accueil — leur pile en compte deux, donc ils se
  glissent. Leur disparition passe alors par `Panel::visible`, pas par un
  repli de zone.
- **Les tailles d'une division se donnent toutes.** Un `None` vaut cent pixels
  dans l'état, et la pile répartit **au prorata** de ce qu'elle y lit : la
  proportion demandée passe à la trappe. Voir « Les sous-applications », où
  cela se voyait sur trois écrans sur quatre.
- **L'état se relit au moment d'écrire**, pas à l'appel : l'ouverture d'une
  zone est différée d'une frame, et le capturer tout de suite enregistrerait
  l'état d'avant le geste.
- **Le zoom d'un panneau est un bouton, pas une entrée de menu**
  (`panels::zoom_in_toolbar`, `PanelControl::Toolbar`) : deux clics pour une
  ligne unique n'en valent pas un. Le bouton `…` reste affiché malgré tout —
  `TabPanel::render_toolbar` le pose sans condition, et le retirer demanderait
  de vendorer gpui-component —, d'où l'entrée qu'il porte désormais.

La disposition est enregistrée dans `<config>/layout.json` — **quatre**, une
par écran, et le nom de celui qu'on regardait —, à part des réglages : c'est
l'état d'une fenêtre, volumineux et illisible, pas une préférence qu'on écrit à
la main. `LAYOUT_VERSION` la fait écarter quand les
panneaux changent de nom — reconstruire à partir de noms inconnus donnerait une
fenêtre pleine de cadres vides. Les panneaux se déclarent au registre du dock
(`panels::register`), sans quoi une disposition relue ne saurait pas les
fabriquer.

L'historique se charge au **rendu** de son onglet et non à la construction :
c'est ce qui évite un `git log` que personne ne regardera. D'où
`history_pending`, sans lequel chaque frame relancerait la commande pendant
tout le temps de la lecture.

### Les terminaux dans le dock

**Un panneau par terminal**, là où il y avait un panneau qui dessinait sa
propre rangée d'onglets. La barre du dock *est* cette rangée désormais, et
c'est ce qui permet de glisser un terminal dans un split, de l'envoyer dans une
autre zone ou de le zoomer comme n'importe quelle vue — rien de tout cela
n'était offert par une rangée peinte à la main.

Sept points, et aucun ne se devine :

- **Cinq panneaux par terminal, un par écran.** Un panneau n'appartient qu'à
  une aire à la fois et il y a cinq docks ; les cinq rendent la **même**
  `Entity<TerminalView>`, donc il n'y a toujours qu'un pty. Un seul dock étant
  affiché, deux d'entre eux ne peignent jamais la même grille dans la même
  frame. `OpenTerminal::panels` les range dans l'ordre de `Workspace::ALL`, d'où
  `Workspace::index`.
- **La place se garde par l'invisibilité, pas par un déménagement.** Les
  terminaux des worktrees qu'on ne regarde pas restent dans l'arbre du dock et
  deviennent invisibles (`terminal_shown`) : un `TabGroup` ne rend pas d'onglet
  pour un panneau invisible et se referme quand il n'en reste aucun. Un terminal
  glissé dans un split est donc toujours là après un aller-retour par un autre
  worktree — retirer puis reposer les panneaux aurait perdu leur place à chaque
  bascule, ce qui aurait vidé le geste de son intérêt.
- **Le premier terminal d'un écran ouvre son emplacement**, sous tout le centre
  (`InsertTarget::Split` sur la racine), là où la disposition par défaut mettait
  le panneau permanent ; les suivants rejoignent son groupe d'onglets
  (`InsertTarget::Tabs`, `activate: true` — l'onglet qu'on vient d'ouvrir est
  celui qu'on regarde).
- **`panel_handle` et `add_panel_view`, jamais `add_panel`.** Un `Entity<P>` se
  convertit tout seul en `PanelView` de la couche base, et le dock l'accepte
  sans rien dire — mais sans la présentation qui va avec : ni onglet, ni titre,
  ni contenu. C'est l'échec silencieux de la refonte du dock, déjà écrit dans
  `workspace.rs` pour la disposition par défaut, et c'est ce qui faisait ouvrir
  un terminal sur du vide.
- **Le nom de l'onglet vient de l'application, pas de la vue.** Il peut être
  donné à la main (clic droit sur l'onglet), et les cinq panneaux d'un même
  terminal doivent dire la même chose. Un nom vide rend au programme le sien,
  comme un libellé vidé supprime une tâche.
- **La croix est peinte par `Panel::title`**, et non par la peau du dock, qui
  ne dessine aucun bouton de fermeture par onglet — son menu ferme un *groupe*,
  ce qui n'est pas le même geste. `title` rend un élément, et un élément porte
  un bouton : c'est ce qui évite un sixième commit sur le fork pour une chose
  que seuls les terminaux veulent. Le clic est **consommé**
  (`stop_propagation`), sans quoi la croix commencerait par amener au premier
  plan le terminal qu'elle s'apprête à fermer.
- **Fermer un onglet tue le pty et ses quatre autres faces.** `on_removed` ne
  concerne que le panneau que l'utilisateur a fermé, celui d'un écran ; les
  quatre autres resteraient des onglets montrant un shell mort.
- **Les terminaux sont retirés de `layout.json` avant écriture**
  (`app::prune_terminals`). Un terminal est le seul panneau dont le contenu est
  un **processus**, et une disposition se relit longtemps après sa mort. Un
  groupe vidé par la purge s'en va avec, et la taille correspondante est retirée
  de la pile — les tailles d'un `Stack` sont positionnelles, en oublier une
  décale toutes les suivantes. Corollaire connu : un terminal glissé dans une
  zone latérale n'est pas purgé, `DockState::panel` étant privé.
- **Le « + » suit le dernier onglet**, et non le bord droit de la barre. C'est
  là que l'œil finit de lire les onglets, et un bouton à l'autre bout du panneau
  fait traverser la barre pour ouvrir le suivant — c'était déjà la règle de la
  rangée peinte à la main. La barre du dock n'offrant aucune place pour un
  bouton, il voyage dans le **titre du dernier terminal**, celui du worktree et
  non du groupe : glissés dans deux splits, les terminaux font toujours une
  seule liste, et un « + » est le bon nombre. C'est l'application qu'on
  interroge pour savoir qui est le dernier, jamais le groupe — il est au milieu
  de son propre rendu quand un onglet le demande, et c'est la panique que ce
  module a déjà payée deux fois.
- **La bascule de la barre d'état ouvre le premier.** Sans terminal il n'y a
  pas d'onglet, donc pas de « + » : c'est par elle qu'on en obtient un, et elle
  seule.
- **Un panneau de terminal ne lit pas l'application à sa construction.** Il est
  bâti au milieu d'un `update` sur `ClaudhubApp`, donc l'entité est sortie de la
  table : `app.read(cx)` y panique (« cannot read … while it is already being
  updated »). Sa visibilité initiale lui est **donnée** par l'appelant, qui la
  connaît, et l'observation prend le relais — même motif que le cache de
  visibilité des autres panneaux.
- **La fermeture est différée.** `on_removed` est appelé depuis l'édition du
  dock lui-même, et emporter les quatre autres faces repasse par
  `DockArea::remove_panel`, y compris sur l'aire en cours de modification. En
  direct, c'est la panique jumelle de la précédente.
- **Le premier worktree de la session ouvre un terminal.** C'est ce à quoi on
  parle pendant qu'on relit, et le premier geste de chaque session était
  `Ctrl+T` — un shell qu'il faut demander est un shell qu'on redemande à chaque
  fois. Le premier seulement (`ClaudhubApp::terminal_started`) : en ouvrir un
  par worktree visité laisserait une douzaine de shells derrière une
  après-midi de navigation, et ceux qu'on a fermés doivent rester fermés.
- **La bascule ouvre quand il n'y a rien à montrer.** Le drapeau est vrai par
  défaut et aucun terminal n'existe au démarrage : une bascule ordinaire
  passerait le premier clic à masquer du vide, c'est-à-dire à ne rien faire.
- **Montrer un terminal n'est pas lui donner le focus.** `reveal_terminal`
  passe par `TerminalPanel::activate`, qui demande à son `TabGroup` de
  sélectionner son onglet — un message livré dans un onglet caché est un message
  que personne ne voit arriver. `Ctrl+T`, lui, veut les deux : le geste dit « je
  veux un terminal maintenant », et un terminal qu'il faut cliquer avant d'y
  taper est un geste laissé à moitié fait — d'où `focus_terminal`, sur le
  **dernier** de la liste, celui qu'on regardait.
- **La fenêtre s'ouvre avec le focus posé.** `ClaudhubApp::new` finit par
  `window.focus`, et ce n'est pas une politesse faite au clavier : sans focus,
  aucun contexte n'est sur la pile, donc **aucun** raccourci ne se résolvait
  avant le premier clic. C'est le même défaut que le suivant, un cran plus tôt.
- **Ce qui retire du focus de l'arbre doit dire où il va.** Un `FocusHandle` que
  plus personne ne rend ne résout plus aucune liaison : le contexte `Claudhub`
  n'est plus dans la pile, et *tous* les raccourcis restent morts jusqu'à ce
  qu'un clic ramène le focus sur un nœud vivant. Cela se voyait sur `Ctrl+T`,
  qui masque le terminal dans lequel on tape et ne pouvait plus se répéter, et
  la fermeture d'un onglet avait exactement le même défaut. Masquer rend donc le
  focus à la racine ; fermer le rend au terminal voisin, et à la racine s'il n'en
  reste aucun.

### Masquer une vue

Le menu `…` d'un panneau ne contient qu'une chose : **masquer cette vue**.
Tout le reste de ce qu'un panneau sait faire vit dans sa propre barre — l'arbre
de la revue, les deux colonnes du diff, le repli de l'explorateur — et le
dupliquer là ferait deux chemins pour un même geste. Masquer, lui, ne parle pas
du contenu du panneau mais de sa place dans la fenêtre, et c'est le dock qui la
tient.

On revient par le **menu principal**, sous-menu « Vues »
(`Workspace::views`) :
une vue masquée n'a plus d'onglet, donc plus rien à cliquer, et ce sous-menu
est du même coup le seul endroit qui dise ce que la fenêtre ne montre pas.

Cinq points qui ne se devinent pas :

- **`Panel::visible`, jamais une zone d'accueil repliée.** C'est déjà le
  mécanisme des terminaux et des conflits, et pour la raison donnée plus haut :
  le dernier panneau d'une zone ne se déplace plus. `TabPanel::visible` rend
  faux quand aucun de ses onglets n'est visible, et `StackPanel` l'honore : une
  zone entièrement masquée se referme d'elle-même.
- **L'état vit dans `ClaudhubApp::hidden_panels`, sa copie dans les réglages.**
  Les panneaux observent l'application ; `Settings::update_global` ne notifie
  personne. `Settings::hidden_panels` est ce qui survit à la fermeture — et
  non `layout.json`, que `LAYOUT_VERSION` jette au premier renommage de
  panneau, alors que « je ne me sers pas de Sentry » n'a pas à disparaître avec
  la géométrie d'une fenêtre.
- **Chaque panneau met sa visibilité en cache**, comme les conflits :
  `Panel::visible` est appelé pendant la construction de la disposition, donc
  au milieu de `ClaudhubApp::new`, où lire l'entité racine est une panique. La
  valeur initiale se lit donc dans les réglages (`visible_at_startup`), et
  l'observation prend le relais. Un changement émet `PanelEvent::LayoutChanged`
  — c'est l'aire, pas le panneau, qui fait disparaître un onglet.
- **La liste est celle de l'écran courant**, et non les onze de la fenêtre :
  masquer « Console SQL » depuis la revue ne ferait rien voir changer, et une
  entrée sans effet se lit comme une entrée cassée.
- **Elle est bâtie sur les constantes `Panel::NAME`**, pas sur des
  littéraux : un nom recopié se serait désaccordé au premier renommage, et un
  nom qui ne désigne plus rien ne masque plus rien, en silence. Les conflits
  n'y sont pas — leur visibilité se décide toute seule, et les masquer
  cacherait le seul endroit d'où l'on termine une fusion.
- **Les lignes du sous-menu sont des `PopupMenuItem::element`.** On bascule
  plusieurs vues à la suite, or `PopupMenu::confirm` referme le menu après
  chaque entrée sans qu'on puisse s'y opposer : la ligne consomme donc son clic
  (`stop_propagation`) et l'entrée qui la porte ne le voit jamais. Un `checked`
  aurait de toute façon menti, étant figé à la construction du menu ; la coche
  est peinte par la ligne, qui relit l'état à chaque frame.

### Le balayage de fond

Le sélecteur de worktree dit, pour chacun, ce qu'il a en chantier (`+n −m`) et
si un agent y travaille. Ces deux informations portent sur des worktrees
**qu'on n'a pas ouverts** : le surveillant de fichiers ne couvre que celui qui
est affiché, et c'est justement ailleurs que se passe ce qu'on veut voir.

D'où un balayage périodique, dans sa **propre file** (`is_background`) : il
porte sur tous les worktrees ouverts, il revient toutes les quelques secondes,
et il ne doit jamais passer devant le diff qu'on vient de demander.

Deux périodes, parce que les deux relevés n'ont pas le même prix. Les agents se
lisent dans `/proc`, sans lancer de processus : toutes les deux secondes. Le
résumé coûte **deux commandes git par worktree** — `--numstat` compte les
lignes mais ignore ce qu'il ne suit pas, `status` voit les fichiers nouveaux
sans savoir ce qu'ils contiennent, et un worktree d'agent est plein de fichiers
nouveaux — donc un relevé sur cinq.

**Le fetch automatique bat sur la même horloge**, mais se compte en minutes :
`Settings::auto_fetch_minutes`, dix par défaut, zéro pour ne rien faire. Sans
lui, « en retard de trois commits » n'apparaît qu'après un fetch demandé à la
main, c'est-à-dire quand on se doutait déjà de quelque chose — et le compte que
portent les boutons ne vaudrait rien.

Quatre points :

- **Un horodatage, pas un compte de tics** (`ClaudhubApp::last_auto_fetch`). Le
  balayage bat toutes les deux secondes et le réglage se donne en minutes : un
  compteur de tics n'aurait aucun rapport lisible avec le champ, et changer le
  réglage ne vaudrait qu'au tour suivant.
- **Un dépôt et non un worktree.** Les références distantes sont partagées par
  tous les worktrees d'un dépôt ; en relever une par worktree reviendrait à
  faire dix fois le même travail et à se disputer le verrou des références.
- **`Cmd::AutoFetch` ne dit rien quand il aboutit.** Un message toutes les dix
  minutes pour annoncer qu'il ne s'est rien passé userait justement l'endroit
  où l'on regarde ce qui vient de se passer ; un échec — pas de distant, pas de
  réseau, pas d'authentification — n'est pas arrivé au moment où l'utilisateur
  regardait, et part dans la trace. `Evt::Fetched` ne porte donc pas un
  résultat mais une **occasion** : relire le statut du worktree affiché, d'où
  viennent l'avance et le retard.
- **File réseau**, comme le fetch demandé à la main, et non celle du fond : un
  fetch se compte en secondes, et le mettre avec les résumés ferait attendre la
  liste des worktrees le temps d'une connexion qui expire.

`agent::scan` est **Linux seulement**, par un `cfg` explicite et non par
accident : le parcours compile partout et échouerait en silence à l'ouverture
de `/proc`, ce qui se lit comme une détection cassée au lieu d'une absence
assumée. C'est aussi ce qui fixe la cible Windows à WSL2 plutôt qu'au natif.

La détection des agents passe par `/proc` et non par nos propres onglets : on
lance un agent depuis Claudhub, mais aussi depuis un terminal à côté, et c'est le
même travail qu'on veut voir. Le répertoire courant d'un processus dit dans
quel worktree il travaille ; le worktree le plus profond l'emporte, faute de
quoi un worktree imbriqué se verrait attribuer les agents de son parent.

Le relevé lui-même ne dit pas qu'un agent travaille : c'est la **différence**
entre deux relevés. `agent::Tracker` est ce qui retient le précédent, et il vit
dans le cœur et non dans la vue — c'est la seule décision de ce relevé qui se
teste, et le cœur est ce que la CI exécute sans gpui.

« En cours » veut dire **a consommé du processeur depuis le relevé
précédent**. C'est une approximation assumée : rien dans un processus ne dit
« je réfléchis » ou « j'attends une réponse ». Un agent au travail redessine
son affichage plusieurs fois par seconde et se voit ; un agent devant son
invite ne coûte rien. Le seuil (`AGENT_BUSY_TICKS`) écarte le clignotement
d'un curseur.

`parse_cpu_ticks` repart de la **dernière parenthèse fermante** de
`/proc/<pid>/stat` : le nom du programme est le deuxième champ, entre
parenthèses, et il peut contenir des espaces et des parenthèses — découper la
ligne sur les espaces décale tous les champs suivants. C'est le piège que tout
parseur naïf de `/proc` rate, et un test le verrouille.

### Intégrer un worktree

`git/repo.rs` sait fusionner, rebaser, abandonner et reprendre. Trois choses
n'y sont pas évidentes :

- **`GIT_EDITOR=true` est posé globalement**, comme `GIT_TERMINAL_PROMPT=0` et
  pour la même raison : `merge --continue` et `rebase --continue` ouvrent un
  éditeur, et un worker bloqué sur un éditeur que personne ne voit ne revient
  jamais.
- **`--ours` et `--theirs` s'inversent pendant un rebase**, git rejouant nos
  commits par-dessus les leurs. `repo::resolve` traduit donc le drapeau
  lui-même : la vue parle de « la nôtre » au sens de l'utilisateur, pas au sens
  de git.
- **L'opération en cours vit dans `Status`** : elle se lit au même moment et
  elle change la lecture de tout le reste. `pending_in` est libre et sans
  sous-processus — `status` la rappelle à chaque écriture de fichier ; seul
  `git rev-parse --git-dir` coûte un fork, et il en faut un parce que dans un
  worktree lié les marqueurs vivent dans `<principal>/.git/worktrees/<nom>`.

**Intégrer s'exécute depuis le dépôt principal**, et le worker vérifie d'abord
qu'il est propre et positionné sur la base ; sinon il refuse et le dit. La
vérification ne peut pas se faire dans la vue : elle ne connaît l'état d'un
checkout que s'il a été ouvert, et le principal ne l'est pas toujours. Une fois
la fusion faite, Claudhub propose de retirer le worktree et sa branche — `wt`
conserve délibérément la branche, c'est donc une question à poser.

Le panneau « Conflits » n'apparaît que quand il y a de quoi le remplir
(`Panel::visible`, comme les terminaux). **Une vue à trois volets n'est pas
promise** : garder la nôtre, garder la leur, marquer résolu, et l'éditeur pour
le reste — c'est ce qu'on fait dans la grande majorité des cas, et une fusion à
la main se fait dans un éditeur qui sait déjà la faire.

### Le `wt.toml` comme système d'extension

`wt` est une **dépendance**, pas un sous-processus : le dépôt est le nôtre, et
parser la sortie de sa CLI — alignée, colorée, traduite — reviendrait à lire ce
qui est fait pour un humain. Sa bibliothèque expose `config`, `git`, `state`,
`ops`, `tmpl`, `util` ; sa CLI et son interface plein écran restent derrière la
caractéristique `cli`, que Claudhub n'active pas — sans quoi il paierait
ratatui, clap et skim pour créer un dossier.

Elle vient du **dépôt distant**, `branch = "main"`, et non d'un chemin voisin :
Claudhub se compile ainsi sur une machine qui n'a pas `wt` à côté. Suivre une
branche ne rend pas la version flottante — `Cargo.lock` fige le commit, et il
faut un `cargo update -p wt` pour en changer.

Ce que cela donne : **le `wt.toml` d'un projet ajoute des actions à Claudhub
sans que Claudhub les connaisse**. Ses `[tasks.*]` apparaissent dans le menu
d'un worktree, ses `[[prompt]]` deviennent un dialogue, son `[status] up` une
pastille, son `[open]` un bouton. Rien de tout cela n'est compilé ici.

Trois règles à respecter :

- **Tout appel à `ops::App` part dans un worker**, et sur la file longue (celle
  du réseau) : un `post_new` installe des dépendances, un `up` démarre des
  conteneurs. Les mettre avec les lectures figerait la revue le temps d'un
  `composer install`.
- **Les questions se demandent en boucle** (`Cmd::WtQuestions` → `Evt` →
  `Cmd::WtQuestions`). Un `[[prompt]]` a un `when` qui peut dépendre d'une
  réponse précédente : les poser toutes d'un coup ferait sauter celles qu'une
  autre débloque. La boucle converge, et l'absence de nouvelle question est ce
  qui déclenche l'opération.
- **Les tâches partent dans un onglet de terminal, pas dans un panneau de
  sortie.** Elles sont interactives, colorées, parfois longues.
  `wt::task` rend les commandes — modèles résolus, environnement calculé — et
  c'est le terminal qui les lance, dans un `sh -lc`. Le partage avec la
  bibliothèque : ce qui tient une comptabilité (création, suppression, `up`,
  `down`) passe par elle, qui alloue les ports et écrit l'état ; le reste passe
  par le shell.

### Les trois moments où le projet pose ses questions

`wt.Phase` — `New`, `Up`, `Task` — décide quels `[[prompt]]` s'appliquent, et
c'est la même distinction que fait `wt` lui-même (`ask = "new"`, `"up"`,
`"both"`, `"task"`). Longtemps Claudhub n'en connaissait qu'une : la création.
Ce qui manquait se voyait sur Acetics, où les tenants dont le storage est monté
et les services du conteneur se choisissent à **chaque** démarrage tant qu'ils
n'ont pas été choisis une fois — démarrer sans eux ne monte rien, en silence.

`WtTarget` porte ce que les questions préparent, et il n'y a pas de quatrième
cas : un geste qui ne demande rien ne passe pas par le dialogue du tout. Le
même dialogue sert les trois, son titre le dit — « Nouveau worktree » au-dessus
des tenants d'un `wt up` serait un mensonge.

Quatre points qui ne se devinent pas :

- **C'est le worker qui amorce les réponses**, et seulement au premier tour.
  Un `wt up` part de ce que le worktree a retenu (`wt::saved_answers`,
  c'est-à-dire l'état que `wt` écrit) : c'est ce qui l'empêche de reposer ses
  questions au deuxième démarrage. Où `wt` range cet état ne regarde pas la
  vue, donc les réponses **reviennent** avec les questions et la vue adopte ce
  qui arrive.
- **Un compteur de tour, pas une comparaison des réponses.** Le tri des
  événements en retard se faisait en comparant les réponses envoyées à celles
  reçues ; l'amorçage les rend différentes par construction. `round` ne recule
  jamais, comme l'identifiant d'envoi de la console SQL.
- **Les questions d'une tâche sont celles qu'elle nomme**, et le filtre
  « déjà répondu » leur est appliqué comme aux autres — sans lui la boucle
  reposerait la même question indéfiniment. Leurs réponses deviennent les
  **arguments** de la tâche, découpés sur le séparateur du prompt, dans l'ordre
  de la tâche et non des réponses : un hook qui reçoit ses arguments dans le
  mauvais ordre agit sur autre chose sans jamais le dire. Rien de choisi ne
  donne aucun argument — et une tâche déclarée `interactive` montre alors son
  propre sélecteur dans l'onglet de terminal, ce qui est exactement là où il
  doit être.
- **Deux drapeaux et non un** (`has_new_prompts`, `has_up_prompts`) : un projet
  peut ne rien demander à la création et tout demander au démarrage, et ouvrir
  un dialogue vide pour s'en apercevoir serait un clic pour rien.

Le dialogue rend enfin ce que le `detail` d'une option dit. C'est la phrase sur
laquelle le choix se fait — « obligatoire si la branche contient une
migration » — et elle n'apparaissait nulle part. D'où :

- **Un choix simple s'affiche en lignes tant qu'il est court** (six options),
  libellé et détail sur la même ligne. Un menu déroulant cache le détail
  derrière un clic qui valide déjà la réponse. Au-delà, les lignes prendraient
  la hauteur du dialogue et le menu revient.
- **Un choix multiple gagne un champ de recherche** passé huit options, et une
  hauteur bornée qui défile. Les options viennent souvent d'une commande shell
  — sur Acetics les tenants sont une requête MariaDB, et il y en a
  quatre-vingts —, et quatre-vingts cases sans moyen de les réduire est une
  liste qu'on fait défiler au lieu de la lire. C'est ce que `wt` fait avec
  skim, et c'est la raison d'être de la question.
- **Le filtre masque des lignes, il ne touche jamais à la réponse.** Un tenant
  coché puis filtré reste coché — le compte le dit — parce qu'une recherche qui
  décoche en silence est la façon la plus sûre de cloner les mauvaises bases.

Le relevé de `[status] up` et de `[open]` est une commande shell par worktree :
**file de fond uniquement**, à la période des résumés, jamais devant un diff
demandé.

### L'exécuteur asynchrone

`runtime::executor` tient un runtime tokio multi-thread, démarré au premier
usage. Claudhub reste un programme à threads — les workers consomment des
`Cmd` et lancent des sous-processus git, parce qu'un `fork` bloque de toute
façon — et cet exécuteur **s'ajoute** à côté, pour les bibliothèques qui n'ont
pas d'interface bloquante.

La première est `sqlx`. Ce qu'il apporte et qu'un pilote bloquant ne pouvait
pas donner : un **vrai délai** (un futur qu'on laisse tomber s'annule), et une
seule pile pour ce qui viendra — un client HTTP asynchrone pour Sentry, des
sous-processus git lancés de front (`tokio::process`), tout ce qui voudra de
l'asynchrone le trouvera ici plutôt que d'amener un second exécuteur.

Trois règles :

- **Le pont est `block_on`, et il est à un seul endroit** : le worker qui
  traite la commande. C'est ce qui garde `runtime::handle` synchrone et pur —
  il rend un `Vec<Evt>`, il ne connaît pas le canal, et il se teste. Un worker
  qui attend un futur attend exactement comme il attendait `git`.
- **Jamais depuis le thread d'interface.** `block_on` y figerait la fenêtre,
  ce qui est précisément ce que le protocole `Cmd`/`Evt` existe pour éviter ;
  gpui a son propre exécuteur pour ce dont la vue a besoin.
- **Deux threads, et non le nombre de cœurs** que tokio prend par défaut : ce
  qui tourne dessus attend une socket, il n'y a pas de calcul à répartir, et
  une machine à seize cœurs n'a aucune raison de porter seize threads
  endormis. C'est aussi ce qui borne la concurrence vers un serveur qu'on ne
  veut pas inonder.

### Les bases de données

Deux surfaces : un **arbre** à gauche — connexion, base, table, colonne — et
une **console SQL** au centre du même écran. C'est l'explorateur de
PhpStorm, et le geste est le même : on déplie ce qu'on cherche, on interroge la
table qu'on a trouvée. Le port de ce que le fork Zed d'Acetics ajoute à
`database_panel`.

**Un seul pilote, `sqlx`**, pour SQLite comme pour MySQL et MariaDB — et le
même modèle pour le troisième moteur qu'on ajouterait : une variante
d'`Engine`, un module à côté, et rien à changer ni au protocole ni aux vues.
Il est asynchrone de bout en bout, d'où l'exécuteur partagé (voir plus bas).

**`NULL` n'est pas la chaîne « NULL ».** Une valeur de résultat est un
`db::Cell`, c'est-à-dire un `Option<String>`, et une colonne `TEXT` contient
couramment le mot. Les confondre se paie trois fois : la grille les affiche
pareil, l'export CSV écrit `NULL` là où un champ vide est attendu, et la copie
sort un mot qui ne veut plus rien dire une fois collée ailleurs.

**Une connexion par requête, jamais gardée.** Un panneau qui tient une
connexion ouverte sur un serveur qu'on n'interroge plus occupe un descripteur
et un processus côté serveur, et découvre la coupure du réseau au pire moment.
Un `connect` coûte quelques millisecondes en local.

**Une file à elles**, et deux workers. Ni celle des lectures — un `SELECT`
malheureux y emporterait un worker sur trois et le diff attendrait derrière —,
ni celle du réseau : une requête de trente secondes retarderait un `fetch`, et
un `fetch` lent retarderait la lecture d'un schéma. Deux workers parce que
déplier une base en demande plusieurs à la fois — ses tables, puis toutes ses
colonnes — et qu'ils attendent une socket, pas un cœur.

**Un délai qui annule vraiment.** `tokio::time::timeout` laisse tomber le
futur de la requête, et le pilote ferme la connexion en cours de route. C'est
ce qu'un pilote bloquant ne sait pas faire : il faut le convaincre de
s'arrêter par un moyen qui lui est propre — un rappel de progression pour
SQLite, un délai de socket pour MySQL — et ce qu'il n'a pas prévu ne
s'interrompt pas du tout. Le délai enveloppe le **geste entier** et non chaque
requête : une introspection en enchaîne plusieurs, et c'est la lecture du
schéma qu'on abandonne, pas sa troisième requête.

**SQLite est ouvert en lecture seule.** On interroge une base de
développement pendant qu'on relit le code qui l'écrit, et un `DELETE` parti
d'un doigt qui a glissé n'y est jamais un service ; c'est le moteur qui
refuse, ce qui vaut mieux qu'un filtre à nous sur le texte de la requête — on
ne devine pas ce qu'une requête fait en la lisant. Pour MySQL, la seule
barrière qui tienne est celle du compte de connexion : en poser une seconde
ici interdirait un `UPDATE` que l'utilisateur a le droit de faire.

**Les connexions se déclarent dans les réglages**, comme les profils d'agent :
c'est le deuxième niveau du système d'extension. Une connexion n'appartient pas
à un dépôt — on relit un projet dans cinq worktrees et la base de
développement est la même — d'où `Settings::databases` et non le magasin
d'état.

Trois choses à savoir sur ce formulaire, et chacune vient d'un essai raté :

- **La table est un `SettingItem::render` et non un `SettingItem::new`**, seule
  de toute la fenêtre. Un item ordinaire met son libellé dans une colonne et
  son champ dans ce qui reste — quatre cents pixels, taillés pour une case à
  cocher — et une connexion en demande cinq. Le premier essai posait une
  largeur en dur : elle débordait de la colonne, et c'est le sélecteur de
  moteur qui sortait de l'écran.
- **`min_w_0` sur chaque champ élastique.** La taille minimale d'un élément
  flex vaut celle de son contenu, et une saisie ne descend pas sous la
  sienne : sans lui, une ligne étroite pousse ses voisins dehors au lieu de
  les rétrécir. Le même piège que celui des barres de défilement.
- **Le moteur se choisit par deux boutons, plein contre contour.** Un menu
  déroulant cache le choix derrière un clic, et l'état « sélectionné » de deux
  boutons de même variante ne se lit pas — or « lequel des deux est actif »
  est exactement la question qu'on se pose en arrivant. Le bouton du panneau
  ouvre d'ailleurs les réglages **sur cette page** (`open_settings_at`) : la
  page se retient au moment où on l'ajoute à la liste, jamais par un indice
  écrit en dur qui désignerait la voisine dès la première page insérée avant
  elle.

**Le mot de passe voyage dans la `Cmd`**, ce qui déroge à la règle du jeton
Sentry, et la dérogation est bornée : `db::Connection` a un `Debug` **écrit à
la main** qui le masque, donc rien ne l'écrit dans une trace. Le faire relire
au worker coûterait un identifiant de connexion à faire voyager, une relecture
du fichier de réglages, et — l'écriture étant différée d'une demi-seconde — une
connexion qu'on vient de saisir interrogée avec ce qu'elle contenait avant.

Cinq points de l'arbre qui ne se devinent pas :

- **Chaque niveau se charge à son dépliage**, et un `Load` à quatre états
  (`Idle`, `Loading`, `Ready`, `Failed`) les distingue. Confondre « pas encore
  demandé » et « en route » relance la commande à chaque frame ; les deux se
  dessinent d'ailleurs différemment — un nœud vide, une roue qui tourne.
- **L'échec vit dans l'événement, pas dans la barre d'état.** Les `Evt::Db*`
  portent un `DbResult`, si bien qu'une erreur s'affiche **sous le nœud qui
  l'a demandée**. Un `Evt::Failed` l'aurait mise dans la barre d'état, où le
  message suivant l'efface, et rien n'aurait distingué la ligne en erreur de
  la ligne pas encore chargée.
- **Le filtre indexe ce qui est déjà ouvert, jamais ce qui ne l'est pas.**
  Taper trois lettres dans une recherche ne doit pas ouvrir une connexion vers
  un serveur de production. « Tout indexer » (l'éclair de la barre) est le
  geste qui se connecte partout, et il est explicite ; il avance par
  `db_continue_indexing`, appelé à chaque lecture qui arrive, et **ne retente
  jamais** ce qui a échoué — ce serait une boucle.
- **Les colonnes d'une base se lisent d'un coup** (`Cmd::DbAllColumns`) : une
  commande par table ferait trois cents connexions sur un schéma Laravel. La
  même réponse remplit l'arbre **et** l'index de complétions de la console.
- **Une entrée ne porte que des indices.** C'est la règle de `ui::tree` pour la
  même raison : la reconstruction est fréquente, et cloner le nom de chaque
  colonne d'un schéma entier à chaque fois est ce qu'une frame ne peut pas
  payer. L'arbre a son propre contexte clavier (`ClaudhubDb`), comme
  l'explorateur de projet et pour la même raison — deux jeux de flèches sur la
  même touche ne se départageraient pas.

### La console SQL

**Elle est le centre de l'écran des bases** (`ConsolePanel`). Elle a longtemps
pris la place du diff, faute d'un endroit à elle : le dock de gpui-component ne
sait pas activer un onglet depuis le code (`TabPanel::set_active_ix` est
privé), si bien qu'un panneau ouvert ailleurs se serait ouvert sans se montrer.
Les écrans lèvent la contrainte — chacun a son dock, et changer d'écran est un
geste à nous. Ouvrir une console **bascule sur l'écran des bases**.

**Une seule console à la fois.** Zed en ouvre une par onglet ; ici la place
centrale est unique, et deux consoles superposées demanderaient une barre
d'onglets à nous.

**Une fenêtre sur le résultat, et non « la page *n* ».** Elle commence à
`offset`, compte `shown` lignes, et **grandit** quand le défilement atteint le
bas (`TableDelegate::load_more`) : on parcourt un million de lignes sans jamais
en charger plus qu'on n'en a lu, et sans le saut de contexte qu'un « page
suivante » impose à l'œil au milieu d'une lecture. Les boutons de la barre
déplacent la fenêtre d'un bloc, le défilement la prolonge — et dans les deux
cas c'est le même envoi, à un `offset` différent. Prolonger n'appelle donc
**pas** `refresh` : il remettrait le défilement en haut, ce qui est le
contraire de ce qu'on vient de demander.

Neuf points :

- **La pagination se fait en lisant, pas en réécrivant la requête.** Ajouter un
  `LIMIT` à ce que l'utilisateur a écrit demanderait de comprendre sa requête —
  un `LIMIT` déjà présent, une union, une procédure — et de la réécrire, ce qui
  est le plus sûr moyen de lui faire exécuter autre chose que ce qu'il lit. Les
  lignes qui précèdent la page sont donc produites par le moteur puis jetées ;
  celles qui suivent ne sont jamais lues.
- **Le tri est fait par le moteur, jamais sur la page.** Trier en mémoire ce
  qu'on a sous les yeux mentirait dès la deuxième page : les mille lignes
  chargées seraient rangées entre elles, et la plus grande du résultat
  resterait à la page suivante. `db::order_by` **enveloppe** donc la requête
  (`SELECT * FROM (…) AS claudhub_result ORDER BY 3 DESC`) — une table dérivée
  ne change pas le sens de ce qu'elle contient, là où insérer un `ORDER BY`
  demanderait de comprendre la requête. On ordonne par le **rang** de la
  colonne : un rang ne se cite pas, alors qu'un nom demanderait les règles de
  guillemets de chaque moteur, et une colonne calculée s'appelle `count(*)`.
  La parenthèse fermante est sur sa propre ligne, hors de portée d'un
  commentaire `--` terminant la requête. Ce qu'on ne sait pas envelopper n'est
  **pas triable du tout** (`db::can_order`) plutôt que trié faux : plusieurs
  instructions, autre chose qu'une lecture, ou deux colonnes de même nom — ce
  qu'un `SELECT * FROM a JOIN b` produit, et que MySQL refuse dans une table
  dérivée. Les en-têtes perdent alors leur flèche.
- **L'enchaînement du tri est le nôtre, pas celui de la table.** Celui de
  gpui-component part du décroissant, ce qui surprend sur une grille, et il
  vit dans un état qu'un `refresh` reconstruit depuis `column()` à chaque
  résultat. Une seule des deux mémoires peut faire foi, et c'est celle de la
  console — c'est elle qui décide de la requête envoyée. La flèche suit le
  geste et non la réponse : une requête met parfois une seconde, et un en-tête
  qui ne bouge pas se lit comme un clic perdu.
- **Un identifiant d'envoi, et non la requête, écarte le résultat en retard.**
  Changer de page, trier et prolonger rejouent tous le **même texte** : les
  comparer ne distinguerait pas la réponse d'un geste de celle du geste qui
  l'a remplacé. `Cmd::DbQuery` porte donc un compteur qui ne recule jamais.
- **La sélection de cellules est la nôtre.** Celle de gpui-component n'en
  connaît qu'une à la fois (`cell_selectable`, `selected_cell`), or ce qu'on
  copie d'une grille de résultats est presque toujours une colonne ou un bloc.
  Deux mécanismes se disputeraient le clic et la couleur de fond : il n'y en a
  donc qu'un — clic, glissement, Maj+clic, `Ctrl+A` —, et `Results::selection`
  garde une **ancre et un curseur** plutôt que deux coins ordonnés, l'ancre
  étant ce qu'un Maj+clic conserve. Deux corollaires : la colonne est déclarée
  sans rembourrage (`Column::p_0`) pour que ce soit **notre** élément qui
  remplisse la cellule — sinon huit pixels de chaque bord ne répondent pas au
  clic — et un clic dans la grille **prend le focus**, sans quoi le `Ctrl+C`
  qui suit partirait à qui l'avait et `ClaudhubQuery` ne serait pas dans la
  pile de contextes.
- **Le presse-papiers prend des tabulations, le fichier des virgules.** Un
  presse-papiers se **colle** — dans une grille de tableur, dans un message —
  où la tabulation garde les colonnes et où la virgule ne fait qu'une seule
  cellule d'une ligne entière ; un fichier s'**ouvre**, et ce qui l'ouvre sait
  analyser du CSV. Une cellule seule, elle, sort telle quelle : c'est une
  valeur qu'on va coller dans une autre requête, pas un tableau. Un clic droit
  **hors** de la sélection la remplace, dedans il la garde — sans quoi le menu
  copierait la seule cellule qu'on vient de viser.
- **L'export rejoue la requête et écrit au fil de l'eau.** Exporter ce qui est
  affiché n'exporterait qu'une fenêtre — ce n'est jamais ce qu'on veut d'un
  export — et tout charger pour l'écrire ensuite ferait tenir un million de
  lignes dans le tas pour les recopier aussitôt. Le tri en vigueur part avec :
  on exporte ce qu'on lit, dans l'ordre où on le lit. Le délai est le sien
  (`db::EXPORT_TIMEOUT`), dix fois celui d'une requête, qui ne porte que sur
  une page.
- **La durée est mesurée dans le worker.** Depuis la vue, elle comprendrait
  l'attente dans la file et le prochain tour de la pompe d'événements — une
  requête d'une milliseconde passerait pour une requête de vingt.
- **L'éditeur et la grille se partagent la hauteur, et le partage se règle**
  (`v_resizable`) : on écrit une requête de vingt lignes, puis on lit trois
  cents lignes de résultat, et aucune proportion figée ne convient aux deux.
- **La durée est mesurée dans le worker.** Depuis la vue, elle comprendrait
  l'attente dans la file et le prochain tour de la pompe d'événements — une
  requête d'une milliseconde passerait pour une requête de vingt.
- **La table est une entité créée une fois** (`DataTable` de gpui-component,
  avec son délégué). La reconstruire à chaque résultat perdrait les largeurs
  qu'on vient de régler à la souris et remettrait le défilement en haut au
  milieu d'une pagination. Les largeurs de départ sont déduites des cinquante
  premières lignes seulement : une fenêtre en compte mille, et la colonne la
  plus large de la fenêtre n'est pas celle qu'on regarde — et elles ne sont
  **pas** recalculées quand la fenêtre grandit, ce qui ferait bouger les
  colonnes sous les yeux de qui défile. Sa molette est lissée comme partout
  ailleurs, par `ClaudhubApp::smoothed` et non `scrolled` : la table peint ses
  propres barres, et lui en poser une troisième en ferait deux à la même place.
- **Tout geste de la table repart vers l'application en différé.** Trier,
  prolonger, copier : la table appelle son délégué au milieu d'un `update` sur
  elle-même, et l'application répond en remplaçant ce délégué — donc en
  réempruntant l'entité qu'on est en train d'emprunter, ce que gpui refuse par
  une panique. D'où `Results::report`, et la référence **faible** à
  l'application qu'il porte, comme les panneaux du dock.
- **Le fournisseur de complétions filtre lui-même.** La liste de
  gpui-component affiche ce qu'on lui rend, en surlignant le préfixe, sans rien
  écarter : un schéma de trois cents tables proposerait sinon trois cents
  lignes à la première lettre tapée. Et le remplacement est donné en clair
  (`text_edit`), parce que la plage de repli de l'éditeur part du mot
  déclencheur — qui englobe le `users.` d'une colonne qualifiée, et l'on
  remplacerait la table par sa colonne.
- **`Ctrl+Entrée` est la touche de la requête**, c'est-à-dire celle du commit.
  Les deux ne peuvent pas coexister : la console déclare son contexte
  (`ClaudhubQuery`) et la liaison du commit l'exclut explicitement, plutôt que
  de se fier à une résolution par profondeur que personne ne relit. `Ctrl+C` et
  `Ctrl+A` sont dans le même cas face à la copie du diff, qui occupe la même
  place à l'écran : `COPY_PREDICATE` exclut la console, et
  `QUERY_COPY_PREDICATE` exclut les champs de saisie — l'éditeur de requête
  garde sa propre copie, comme le champ de message de commit.

Ce qu'il n'y a pas, et pourquoi : **aucun cache de schéma sur le disque**. Zed
en garde un dans son magasin clé-valeur pour que le filtre voie tout dès le
démarrage ; ici le magasin d'état fait un kilo-octet et se lit à la main, et un
schéma indexé en pèserait mille fois plus. L'index vit donc pour la session, et
« tout indexer » le refait en une commande par base.

### Les bases d'un worktree

Un projet comme Acetics **clone ses bases par worktree** : `wt new telavox`
laisse `wt_telavox_master` et ses `wt_telavox_tenant_*` à côté des quatre-vingts
bases du dépôt principal. L'arbre les montrait toutes, ce qui revient à n'en
montrer aucune — les trois qui appartiennent à la branche relue sont perdues
dans le reste.

**Un motif déclaré sur la connexion** (`db::Connection::scope`, `wt_{slug}_*`),
c'est-à-dire le deuxième niveau du système d'extension et non le `wt.toml` :
une connexion appartient à la machine, pas au projet, et le même dépôt se relit
depuis cinq checkouts contre le même serveur. `db::scope` est pur et testé.

Quatre règles, et chacune est ce qui empêche le filtre de mentir :

- **Un motif dont une variable ne se résout pas est écarté.** Le checkout
  principal n'a pas de slug : `wt_{slug}_*` n'y dit rien, et le résoudre en
  chaîne vide donnerait `wt__*`, qui ne correspond à rien du tout. Les
  variables sont `{worktree}` (le nom du dossier), `{slug}` (le même, mais
  **absent sur le principal** — c'est ce qui rend le motif inerte là-bas) et
  `{branch}`.
- **Aucun motif applicable montre tout.** Un scope qui ne sait pas décider ne
  doit jamais être la raison qu'une base disparaisse : une connexion sans motif,
  un worktree pas encore choisi, un projet dont les bases ne sont pas clonées se
  comportent exactement comme avant.
- **Rien n'est masqué en silence.** La barre du panneau porte le compte des
  bases écartées et la bascule qui les rend (`db_hidden_count`,
  `db_toggle_scope`), et **la bascule n'apparaît que si une connexion déclare un
  motif** : un bouton qui échange deux listes identiques se lit comme un bouton
  cassé. L'état vit en mémoire, comme le niveau du journal : c'est une posture
  de lecture, pas une préférence.
- **L'indexation suit le scope.** Se connecter aux quatre-vingts bases qu'on
  vient de masquer est exactement ce qu'on lui a demandé de ne pas faire ; la
  recherche non plus ne va pas au-delà — un résultat hors scope serait une ligne
  qu'on ne sait pas expliquer.

Deux corollaires : le filtre s'applique à l'**affichage**, les entrées de
l'arbre ne portant que des indices dans l'état — masquer, c'est sauter une ligne,
jamais retirer une base du vecteur ; et le motif n'entre **pas** dans
`Connection::key`, qui dit quelle connexion c'est et non ce qu'on en montre — une
console rouverte depuis un autre worktree doit rester la même console.

### L'historique des requêtes

La console retenait la requête affichée et rien d'autre : ce qu'on avait écrit
la veille, dans le worktree d'à côté, contre le tenant dont on ne se rappelle
plus le nom, disparaissait au premier effacement de l'éditeur. Une console SQL
sans historique est une console où l'on réécrit le même `SELECT` chaque
après-midi.

**Par worktree, parce que c'est là qu'est le travail** — une branche, un jeu de
bases clonées, une question à laquelle on répond. Rien n'est muré pour autant :
le sélecteur de portée du panneau lit tout le fichier (« ce worktree », « cette
connexion », « tout »), mais ce qu'on voit en arrivant est ce qu'on faisait ici.

**Un fichier à lui** (`<config>/sql_history.json`), et non le magasin d'état :
le magasin dit « où l'on en est », fait un kilo-octet et se réécrit en entier
toutes les demi-secondes pendant qu'on tape — le texte de la console y passe à
chaque frappe. Un journal cumulatif de plusieurs centaines de kilo-octets n'a
rien à faire à ce rythme-là. Il est sérialisé dans le thread d'interface — une
milliseconde — et **écrit en fond** : un journal s'écrit sur un geste, jamais
sur une frame.

**Un onglet à côté de l'arbre**, dans la colonne de gauche de l'écran des
bases, et non un popover : on écrit une requête **pendant** qu'on le regarde,
comme on prend une table dans l'arbre d'à côté. Derrière un clic, c'est ainsi
qu'un historique cesse d'être consulté. `LAYOUT_VERSION` passe à 15 — une
disposition d'avant ne connaît pas ce panneau.

Sept points qui ne se devinent pas :

- **La même requête rejouée ne fait pas une seconde ligne.** On corrige une
  faute de frappe, on relance le même `SELECT` quatre fois en regardant les
  données changer : quatre lignes identiques n'en disent pas plus qu'une et
  chassent la requête de ce matin qu'on cherchait. La ligne remonte, compte un
  passage de plus (`×4`) et **prend le résultat frais** — ce qu'une ligne dit du
  nombre de lignes et de la durée porte sur la dernière fois, pas sur la
  première. Les blancs sont normalisés : réindenter une requête n'en fait pas
  une autre.
- **Seul ce qu'un geste a demandé est classé** (`QueryState::record`). Changer
  de page, trier et prolonger rejouent tous le même texte : les classer
  compterait quatre passages pour une question posée, et c'est justement le
  compteur qui donne son sens au `×4`.
- **Une requête qui échoue est classée aussi.** Une faute de frappe qu'on
  corrige est exactement ce qu'on veut récupérer, et la ligne le dit — première
  ligne du message du moteur, le reste n'étant pas lu dans une liste.
- **Le plafond est par worktree** (`PER_WORKTREE`), pas global : un projet
  interrogé tout l'après-midi ne doit pas chasser les trois requêtes du worktree
  d'à côté.
- **Une entrée nomme sa connexion, elle ne la décrit pas.** On y range la clé
  (`db::Connection::key`) comme dans le magasin, donc sans mot de passe ; rejouer
  relit la connexion **dans les réglages**, et une connexion supprimée laisse la
  ligne en place en le disant plutôt que d'ouvrir une console qui échouerait à sa
  première requête.
- **Un clic charge, un double-clic exécute.** Ce qu'on fait d'une requête
  rappelée est de l'ajuster avant de la lancer : le focus part donc à l'éditeur.
- **Vider n'oublie que ce qui est affiché.** La portée et la recherche décident
  de ce qui part : ce qu'on demande d'oublier est ce qu'on a sous les yeux.

La liste est virtualisée par `v_virtual_list` et non `uniform_list` : ses lignes
n'ont pas la même hauteur — un en-tête de jour fait une ligne, une requête deux
(la requête, puis ce qu'elle a répondu) —, et les en-têtes sont **dans** la
liste et non des sections autour, une liste virtualisée ne parcourant qu'un
vecteur plat. Le groupement par jour, la portée, la déduplication et la
recherche sont dans `ui::sql_history`, qui ne connaît aucun type de gpui et se
teste, comme `notes.rs` devant `notes_view.rs` ; « aujourd'hui » et « hier » y
sont une décision et leur libellé appartient à la vue.

### Les domaines de revue

« Modifications » et « Revue de branche » sont **deux panneaux**, pas deux
onglets d'une même vue : on les regarde ensemble, l'un montrant ce qui change
maintenant, l'autre ce que la branche a écrit.

Une liste ne se vide jamais avant d'avoir de quoi la remplacer : à l'arrivée
d'un statut, les domaines connus sont **redemandés** et l'ancien contenu reste
à l'écran jusqu'à la réponse. L'inverse fait clignoter la liste à chaque
écriture de fichier.

Conséquence sur l'état : `ReviewState::files` est une **table par domaine**.
Une seule liste ferait clignoter l'un des deux panneaux chaque fois que l'autre
se recharge. Chaque panneau demande la sienne au rendu (`ensure_files`), avec
un garde `pending_files` — sans lui, chaque frame relancerait la commande
pendant tout le temps de la lecture.

`ReviewState::range` ne désigne plus « le domaine courant » mais celui du
fichier ouvert en dernier, quel que soit le panneau d'où le clic vient : c'est
lui qui décide de ce que la vue de diff compare, et de la possibilité
d'indexer.

Le diff est au **centre**, pas dans une zone d'accueil à droite : les zones
latérales occupent toute la hauteur, et le diff à droite couperait les
terminaux en deux au lieu de les laisser courir sous toute la revue.

### Lire et supprimer un fichier non versionné

`git diff --no-index` sort avec le code **1** dès qu'il trouve une différence,
et c'est le cas normal ici : le fichier entier *est* la différence. La lecture
passait par `git`, qui traite tout code non nul comme un échec et jette la
sortie avec — un fichier nouveau s'affichait donc vide. D'où `git_tolerant`,
qui accepte un code borné.

Le statut est lu avec `--untracked-files=all` et non `normal` : sans cela, un
dossier entièrement nouveau apparaît comme une seule entrée `dossier/` qu'on ne
peut ni lire ni indexer fichier par fichier — et un worktree d'agent en crée.
Le coût est un parcours complet des dossiers non versionnés *et non ignorés*,
que `.gitignore` borne déjà.

La suppression passe par `git clean` et non par `remove_file` : il refuse ce
qui est suivi, ce qui est la garantie qu'on veut — une erreur d'aiguillage dans
la vue ne peut pas détruire un fichier versionné.

### Un dépôt qui n'est plus là

Un dossier déplacé, un clone effacé, une partition non montée : le dépôt
mémorisé ne s'ouvre plus, et il n'apparaissait alors **nulle part** — deux
avertissements dans la trace à chaque démarrage, et aucun moyen de le retirer
sans éditer le fichier de réglages à la main.

Il reste donc affiché, en bas du sélecteur de worktree, en erreur — avec ce que
git a répondu, qui dit si le dossier a bougé ou si le disque manque — et avec de
quoi le retirer. Quatre points :

- **`Evt::RepoUnavailable` et non un `Failed`.** Ce n'est pas une opération qui
  a échoué mais un dépôt qui manque, et ce qu'il faut en faire dépend d'où il
  vient : **mémorisé**, il doit rester visible pour qu'on puisse le retirer ;
  **désigné à l'instant** dans un sélecteur de dossier, il n'a qu'à se dire
  dans la barre d'état — en garder une trace serait faire une relique d'une
  faute de frappe.
- **Une liste à part** (`ClaudhubApp::unavailable`) et non un drapeau sur
  `RepoState`. Tout ce qui parcourt `repos` — les résumés, le relevé des
  agents, celui de `wt`, le fetch automatique — suppose un dépôt qui existe ;
  un drapeau se paierait en gardes semés partout, dont un oubli ferait lancer
  des commandes git dans un dossier absent toutes les deux secondes.
- **Un bouton, pas une entrée de menu.** C'est la seule chose qu'on puisse
  faire d'une ligne pareille. Sur un dépôt ouvert, en revanche, le même geste
  est la dernière entrée du menu du worktree, derrière un séparateur : il ferme
  tout ce qu'on y avait ouvert, et ce n'est pas une chose qu'on fait deux fois
  par jour.
- **Le magasin d'état n'est pas purgé.** Retirer un dépôt de la liste ne touche
  à rien sur le disque ; ses notes et ses replis attendent le jour où on le
  rouvre, et les effacer ici ferait d'un rangement une perte.

### Quel worktree s'ouvre

`runtime::open_repo` retient le checkout d'où l'ouverture vient, et non le
premier de la liste — qui est toujours le dépôt principal. Lancer `claudhub` dans
un worktree doit ouvrir *ce* worktree. Le worktree retenu est le plus profond
dont le chemin est un préfixe de celui demandé, faute de quoi un worktree
imbriqué dans un autre serait attribué au mauvais.

Mais `opened_at` ne dit pas à lui seul « lancé ici » : un dépôt mémorisé demande
sa propre racine, et revient donc lui aussi avec un checkout. Trois candidats se
départagent par un **rang** (`session::pick_worktree`, libre et testé) : le
checkout qui contient le dossier de lancement l'emporte toujours, puis le
worktree de la session précédente, puis le premier de la liste. Le rang est ce
qui rend l'ordre des réponses sans importance — les dépôts sont énumérés par le
worker qui finit le premier, et sans lui le worktree mémorisé gagnait ou perdait
selon le jour. `select_worktree` pose le rang le plus fort, celui d'un choix à
la main, et `repo_opened` le rabaisse juste après pour les sélections qu'il fait
lui-même.

Corollaire : la sélection a lieu **même quand le dépôt était déjà ouvert**. Le
dépôt d'où l'on lance est presque toujours un dépôt mémorisé, donc déjà ouvert
quand `OpenIfRepo` répond — et cette réponse-là est la seule qui nomme le
checkout profond où l'on se tient plutôt que le dépôt principal. Le reste de
l'ouverture, lui, ne se refait pas.

### Où l'on en était

Les réglages disent comment Claudhub s'affiche, `layout.json` où sont les
panneaux et sur quel écran on était, le magasin ce à quoi un worktree se
compare. Aucun ne disait ce qu'on **faisait** : rouvrir demandait de retrouver
le dépôt, le worktree, le fichier, et de retaper la requête qui était prête à
partir. `store::Session`, écrit par `ui::session`, garde ces trois-là.

**On remet ce qu'on avait choisi, jamais ce qu'on avait obtenu.** Le worktree
sélectionné, le fichier ouvert dans l'éditeur, la connexion de la console et le
texte de sa requête — mais pas de diff, pas de grille de résultats, pas de
statut : tout cela revient de lui-même des lectures que la sélection déclenche,
et un `SELECT` rejoué au démarrage est une requête vers un serveur que personne
n'a demandé à joindre.

Cinq points qui ne se devinent pas :

- **La connexion est nommée par sa clé** (`db::Connection::key`) et non
  recopiée : elle est décrite dans les réglages, une copie ici vieillirait au
  premier changement, et la clé ne porte pas le mot de passe. Une connexion
  supprimée ne revient simplement pas.
- **Rouvrir n'est pas un geste.** Ouvrir un fichier bascule sur l'écran
  « Édition », ouvrir une console sur celui des bases — voir « Les
  sous-applications ». Ici il n'y a pas de geste, et l'écran qui revient est
  celui de `layout.json` : `take_restored_editing` est ce qui distingue les deux,
  et `reopen_db_console` est `start_db_console` sans ses deux effets de bord.
- **L'écriture se fait aux gestes, pas à la fermeture.** Une fenêtre se ferme
  aussi par un plantage ou une session qui s'achève, et un état écrit seulement
  en sortant est un état qu'on perd les jours où il aurait servi. Le texte de la
  requête part donc à chaque frappe, la demi-seconde différée du magasin faisant
  le reste.
- **Ce qui n'est pas encore remis tient lieu de ce qui n'est pas là.** La
  première frappe arrive avant qu'un dépôt ait répondu : sans ce repli,
  `persist_session` écrirait un worktree vide et effacerait celui qu'on est en
  train de rendre.
- **La demande de relecture du fichier ne part qu'une fois**
  (`restore_asked`). Chaque dépôt qui s'ouvre demande si le fichier mémorisé est
  l'un des siens, et une fenêtre à quatre dépôts lirait sinon quatre fois le même
  fichier. Ce qui ne trouve jamais son worktree reste là et s'en va avec la
  fenêtre : un fichier dont le worktree a été supprimé n'a rien à signaler.

### La base de la revue de branche

La base **par défaut** vient de git — `origin/HEAD`, puis `init.defaultBranch`,
puis les noms usuels *qui existent vraiment* (`branch::default_base`) — et
jamais d'un nom supposé. Ce n'est qu'un point de départ : un sélecteur avec
recherche (`base_select`, un `SelectState<SearchableVec<SharedString>>`) laisse
comparer à n'importe quelle branche, locale ou distante. Le choix est **propre
au worktree** : comparer un worktree d'agent à `dev` et un autre à la branche
d'où il est parti est le cas normal, pas l'exception — d'où le rafraîchissement
du sélecteur à chaque changement de worktree.

Choisir une base bascule sur la revue de branche : le faire en regardant ses
modifications en cours n'aurait aucun effet visible, ce qui ferait croire que le
sélecteur ne marche pas. Un `main` codé en dur produit un `unknown revision` au premier clic
sur tout dépôt qui s'appelle autrement, ce qui est le cas de la moitié d'entre
eux. Tant que la base est inconnue, ou que c'est la branche déployée dans ce
worktree (qui n'aurait rien à se comparer), l'onglet reste affiché mais
inactif : le faire disparaître décalerait les trois autres à chaque changement
de worktree.

Le statut arrive avant les branches : `Evt::Branches` repropage donc la base
aux revues déjà ouvertes, faute de quoi la première ouverte n'en aurait jamais.

### La coloration syntaxique

C'est le *code* qu'on relit, pas les marqueurs `+`/`-` : les lignes sont
colorées avec la grammaire du fichier, pas avec la grammaire `diff`.

Un hunk n'étant pas un fichier, `highlight.rs` reconstruit les deux versions —
ancienne (contexte + supprimées), nouvelle (contexte + ajoutées) — colore
chacune en un seul appel, puis redistribue les styles ligne par ligne. Une
ligne de contexte appartient aux deux : la seconde passe **remplace** la
première (`target.clear()`), elle ne s'y ajoute pas.

Deux invariants que gpui ne vérifie pas et dont la violation est silencieuse :
les plages doivent être **triées et disjointes** (`with_highlights` les
convertit en longueurs de runs consécutives, en les parcourant dans l'ordre
donné : une plage désordonnée décale tout ce qui suit), et les décalages sont
en **octets** — indexer en caractères casse dès le premier accent. Les deux
sont verrouillés par `diff_view::tests::highlight_runs_stay_sorted_and_disjoint`,
qui a déjà attrapé le doublon des lignes de contexte.

**Un fragment reçoit d'abord de quoi être reconnu** (`highlight::prologue`).
PHP l'impose : sans `<?php`, sa grammaire lit *tout* le fragment comme du
texte HTML et pas une couleur n'en sort. Or un hunk commence presque toujours
au milieu du fichier, donc après la balise d'ouverture — c'est le cas courant,
pas l'exception. Le prologue n'est ajouté que s'il manque : un fichier Blade
dont le hunk porte du HTML l'attend déjà, et lui en préfixer un second
casserait le parse. Les positions des lignes suivent le décalage, si bien que
les styles du prologue, n'appartenant à aucune d'elles, s'ignorent d'eux-mêmes.

**Une vue Blade est du HTML avant d'être du PHP** (`ui::blade`). Aucune
grammaire tree-sitter n'en est publiée, et la grammaire PHP ne voit dans
`@foreach` ou `{{ $x }}` que du texte. La grammaire colore donc ce qu'elle
sait lire — HTML compris, par injection —, puis `blade::overlay` repasse
dessus les directives, les échos, les commentaires et les **balises de
composant** : un scanner à la main, assumé comme tel. Trois conséquences à
retenir : un `.blade.php` ne reçoit **jamais** de prologue, sinon ses balises
seraient lues comme du code ; un rôle Blade se traduit en style par une
**liste de noms**, du plus juste au plus sûrement présent, parce que nos
thèmes ne définissent ni `punctuation` ni `operator` — sans repli, les
délimiteurs d'un écho restaient invisibles ;
`blade::tests::every_scope_resolves_to_a_colour` le vérifie, et `keys_of`
compare désormais aussi les styles de coloration d'un thème à l'autre.

**Un nom de composant pointé appartient à la surcouche.** La grammaire HTML ne
connaît pas de nom de balise avec un point : dans `<x-layout.app>` elle lit
`x-layout` comme une balise et `.app` comme un **attribut**, et le nom se coupe
en deux couleurs en son milieu. Les composants d'un projet Laravel vivant en
sous-dossiers, le point y est la règle : `blade::component` repeint donc le nom
entier d'un seul tenant — `<x-…>`, `</x-…>` et `<livewire:…>` —, dans la
couleur d'une balise et non dans une couleur à eux, un composant *étant* une
balise pour qui lit la vue.

**Le corps d'un bloc `@php` est rendu à la grammaire, pas peint par la
surcouche.** C'est du PHP — souvent une dizaine de lignes — et la grammaire n'y
voyait que du texte HTML, faute de connaître les deux marqueurs. Le colorer
depuis la surcouche voudrait dire porter une seconde analyse PHP dans un
scanner qui lit ligne à ligne. La ligne remise à la grammaire n'est donc pas
tout à fait celle de l'écran : `blade::mask_php` change `@php` en `<?` et
`@endphp` en `?>`, **complétés d'espaces jusqu'au même nombre d'octets** —
quatre et sept. C'est tout le procédé, et le compte d'octets est ce qui le rend
sûr : chaque décalage que la grammaire rend désigne encore le même caractère de
la vraie ligne, il n'y a rien à retraduire. `<?` seul est une balise que la
grammaire accepte (`php_tag: /<\?([pP][hH][pP]|=)?/`), et la surcouche repeint
ensuite les deux marqueurs en directives par-dessus ce qu'elle en a fait. Deux
gardes : `@php($total = 0)` est une instruction et non un bloc, donc n'est pas
masqué — le masquer poserait une balise PHP que la grammaire suivrait jusqu'au
bout du fichier —, et l'état de commentaire suit le masquage comme il suit le
scanner, un `@php` écrit dans `{{-- … --}}` n'ouvrant rien.

**Ce qui est écrit dans une autre langue est rendu à sa grammaire**
(`blade::Tint`). Trois endroits d'une vue n'ont rien de Blade : l'argument d'une
directive et le corps d'un écho sont du **PHP**, la valeur d'un attribut Alpine
est du **JavaScript**. Aucun n'est atteignable depuis l'analyse de la vue — ils
vivent dans ce que la grammaire PHP lit comme du texte —, et ils arrivaient donc
plats, une seule couleur pour `$invoice->total()` comme pour `'net'`. Chaque
fragment est donc analysé à part, avec ce qu'il faut devant pour être reconnu :
`<?php ` pour le PHP, exactement comme `highlight::prologue` un étage plus haut ;
rien pour du JavaScript, sauf une paire de parenthèses quand la valeur commence
par une accolade — `x-data="{ tab: 1 }"` est une *expression*, et un programme
JavaScript qui commence par une accolade est un **bloc**, où `tab:` serait une
étiquette.

Sept points, et les trois derniers sont ce qui rend la chose payable :

- **C'est tout ou rien, et les deux cas sont ce qui rend l'essai sans risque.**
  Si la grammaire a dit quelque chose, le fragment est du code et se lit comme
  du code : ce qu'elle n'a pas nommé — un identifiant, un opérateur, une
  espace — prend la couleur du texte ordinaire, exactement comme dans un fichier
  de cette langue-là. Laisser ces octets à la couleur de la valeur est ce qui
  faisait revenir `id = $wire.` et `prevEditId !==` en vert au milieu d'un
  JavaScript coloré : la couleur de chaîne s'étalait sur tout ce que la
  grammaire n'avait pas de mot pour nommer, et la valeur se lisait comme une
  chaîne avec des mots-clés dedans. Si elle n'a **rien** dit — une expression
  qu'elle ne sait pas lire, une valeur qui n'est qu'un identifiant nu —, le
  fragment garde la couleur unique qu'il avait ; ne pas essayer est exactement
  ce qui se passait avant, donc rien ne peut être pire qu'avant.
- **Un fragment PHP reçoit un point-virgule.** Sans lui, une expression nue —
  `true`, un cas d'énumération, à peu près tout ce qu'un `:prop` porte — n'est
  pas une instruction : l'analyse rend un nœud `ERROR` et la requête ne trouve
  rien dedans. Ce caractère-là est toute la différence entre une valeur colorée
  et une valeur grise.
- **La requête PHP livrée ne nomme pas un cas d'énumération**, et c'est réparé
  au-dessus d'elle (`highlight::PHP_CONSTANTS`, concaténé comme l'est déjà
  l'injection HTML). Elle ne nomme une constante que capitalisée — `Foo::BAR` —,
  ce qui était toute la convention à l'époque où elle a été écrite ;
  `ActionColor::Success` ne correspondait à **rien**, ni d'un côté ni de
  l'autre du `::`, dans un dépôt où les énumérations sont partout. Le nœud ne
  porte pas de noms de champs, d'où l'ancre : le premier nom est la classe, le
  second ce qu'on y lit. Cela vaut pour tout le PHP du dépôt, vues comprises,
  pas seulement pour les fragments.
- **`:name="…"` est lu comme du PHP, et c'est une convention, pas une
  déduction.** Le raccourci est ambigu par construction : sur une balise de
  composant c'est une propriété Blade, sur une balise ordinaire c'est le
  `x-bind` d'Alpine. Les départager demande de savoir quelle balise est ouverte,
  ce qu'un scanner ligne à ligne ne sait pas. Blade l'emporte parce qu'Alpine a
  une orthographe qui le dit — `x-bind:class` —, et qu'un projet qui l'emploie
  n'a plus rien d'ambigu ; un projet qui écrit `:class` pour Alpine obtient une
  lecture PHP d'une expression JavaScript, qui revient telle qu'elle était,
  `fragment` ne colorant rien de ce qu'il ne sait pas lire. `wire:` n'est touché
  ni dans un sens ni dans l'autre.
- **Une valeur peut tenir sur deux lignes**, et `x-effect="…"` sur deux lignes
  est la façon normale d'en écrire un. C'est la seconde chose que `blade::State`
  reporte d'une ligne à l'autre, à côté du commentaire ouvert.
- **Les grammaires sont construites une fois et gardées.**
  `SyntaxHighlighter::new` compile les requêtes d'une grammaire — des dizaines
  de millisecondes —, et celle du JavaScript n'est bâtie que si la vue en
  contient.
- **Dans l'éditeur, on ne peint que ce qui est demandé, et on le retient.**
  `styles` est appelé pour chaque groupe de lignes visible, à chaque frame :
  peindre les douze cents fragments d'une vue de trente-cinq kilo-octets coûte
  soixante-dix millisecondes, soit une frappe de gel à chaque frappe. Seuls les
  quarante fragments à l'écran sont analysés (trois millisecondes), et chacun est
  gardé, indexé par son début. Ce qui invalide le lot : une édition, que
  `refresh` signale — les décalages ont bougé —, et un changement de thème, que
  **personne** ne signale, le résolveur arrivant en `&dyn` sans identité à
  comparer. D'où un **témoin** : la couleur d'un nom, redemandée à chaque appel.
  Un thème qui peint les mots-clés autrement est un thème qui a changé, et une
  recherche de table par frame est tout le prix de s'en apercevoir. Les
  grammaires, elles, survivent au changement de thème — les reconstruire là
  repaierait leurs requêtes.

**Ce qui commence par une arobase n'est pas toujours une directive.** Deux
autres formes la partagent, et Alpine les met dans toutes les vues récentes :

- **La liaison d'événement** — `@click.prevent="open = !open"` — et c'est le
  `=` qui les départage, rien d'autre : `@click` et `@if` ont exactement la
  même forme, et seul un attribut est immédiatement suivi de sa valeur. La
  peindre en directive était faux deux fois : elle prenait la couleur d'un
  mot-clé au milieu d'une balise, et elle **effaçait** la couleur d'attribut que
  la grammaire HTML lui avait donnée, `apply` remplaçant ce qu'il couvre. La
  valeur, elle, est laissée à la grammaire, qui la lit comme la chaîne qu'elle
  est. `x-data`, `:class` et `wire:model.live` n'ont jamais rien demandé : la
  grammaire HTML les lit déjà comme des attributs.
- **L'échappement `@{{ … }}`**, par lequel une vue rend les accolades à Alpine
  ou à Vue au lieu de les lire. Seule l'arobase est consommée — assez pour que
  ce qui suit ne passe pas pour un écho, ce qui est tout l'intérêt de l'écrire.

**Et tout cela vaut aussi dans l'éditeur**, ce qui a demandé un second chemin.
La surcouche sert le diff, qui peint lui-même ; l'éditeur non — `EditorState`
demande des plages stylées à un coloriseur et les peint. Une vue ouverte là
n'avait donc *rien* de Blade : les balises autour d'un bloc `@php` étaient
colorées et tout ce qu'il contient restait gris. La couture est
`gpui_base::input::InputHighlighter`, qu'installe `set_highlighter_factory` —
une raison de plus pour que `gpui-base` soit une dépendance directe.
`blade::BladeHighlighter` fait ce que fait le diff, dans le même ordre et avec
les deux mêmes fonctions : `mask_document` puis `scan_document`. Quatre points :

- **L'édition incrémentale est jetée et le document reparsé entier.** Une
  édition décrit un changement par rapport au texte que tree-sitter a vu en
  dernier, et ce qu'il a vu est le texte **masqué**. Les deux s'accordent octet
  pour octet jusqu'à la frappe qui achève un `@php` : quatre octets loin du
  curseur changent alors de sens, l'édition ne décrit plus rien de réel, et rien
  ne viendrait jamais corriger l'arbre. Une vue fait quelques dizaines de
  kilo-octets, que tree-sitter analyse en une milliseconde ou deux ; au-delà de
  `blade::MAX_BYTES`, on laisse la grammaire seule.
- **Les plages rendues doivent couvrir la fenêtre demandée en entier**, triées
  et disjointes — le contrat de `InputHighlighter`, que rien ne vérifie.
  `blade::merge` parcourt donc la **fenêtre** et non les plages de la grammaire :
  la surcouche l'emporte là où elle porte, la grammaire répond ailleurs, et ce
  que ni l'une ni l'autre ne couvre prend le style par défaut. Parcourir les
  plages de la grammaire perdrait en silence une plage Blade tombée dans un
  trou. Les plages voisines de même style sont fusionnées, sans quoi `@php`
  sortait en deux morceaux, le masque ayant fait de `<?` un jeton à lui.
- **Les replis sont à refaire.** La règle est celle de gpui-component — tout
  nœud nommé couvrant au moins trois lignes — mais la fonction qui la porte est
  privée à son adaptateur, et poser un coloriseur à soi, c'est reprendre tout ce
  que celui par défaut fournissait. D'où `tree-sitter` en dépendance directe,
  à la version qu'il utilise : deux crates `tree-sitter` étrangères l'une à
  l'autre ne s'accorderaient sur aucun type.
- **`ensure_highlighter_factory` laisse le nôtre en place** : c'est le rendu de
  l'`Input` qui installe celui par défaut, et il ne le fait qu'à défaut.

PHP n'est pas dans les grammaires que gpui-component embarque, et c'est le
langage de la moitié des dépôts qu'on relit : `highlight::register_languages`
le déclare dans le registre partagé au démarrage. À appeler avant tout rendu —
le registre est un singleton verrouillé.

**Nix passe par le même chemin**, et pour une raison plus proche : c'est avec
quoi ce dépôt se construit, et un diff de `shell.nix` ou de `flake.nix` se
relit à côté du reste. Sa grammaire (`tree-sitter-nix`) apporte ses propres
injections, qui mettent du **bash** dans les phases et les hooks d'une
dérivation — `shellHook`, `buildPhase`, `writeShellScript` — c'est-à-dire dans
la partie d'un `shell.nix` qu'on regarde vraiment ; la grammaire bash, elle,
est bien embarquée. Ce qui n'est **pas** enregistré est la requête `locals` :
le crate livre `queries/locals.scm` mais n'en expose aucune constante, et les
clauses `#is-not? local` de la coloration continuent simplement de
s'appliquer — un identifiant lié dans un `let` reçoit donc la couleur d'un
builtin s'il en porte le nom, ce qui est le pire que coûte cette absence.

**L'injection HTML est recopiée chez nous** (`highlight::HTML_INJECTION`), et
ce n'est pas un détail : `tree_sitter_php::INJECTIONS_QUERY` est
`queries/injections.scm`, qui ne couvre que phpdoc et les heredocs. Le HTML
qui *entoure* le code vit dans un second fichier, `queries/injections-text.scm`,
que les liaisons Rust n'exposent sous aucune constante. Tant qu'on ne le
passait pas, **toute vue arrivait grise, balises comprises** — une injection
qui ne trouve pas sa grammaire ne produit aucune erreur, seulement du texte
nu, et c'est pourquoi `html_tags_are_coloured_in_a_view` existe.

`SyntaxHighlighter::new` compile les requêtes de la grammaire — près de
quarante millisecondes pour JavaScript. Jamais dans un `render`, et **une seule
instance pour les deux passes** : `update` ne fait que reparser un texte, alors
qu'en créer une seconde doublait le coût fixe de chaque fichier ouvert.

### Le terminal

`alacritty_terminal` fournit le parseur VTE, la grille, l'historique et le pty.
Claudhub écrit deux choses : `keys::key_bytes` (frappe → octets) et
`snapshot::capture` (grille → lignes stylées). Le rendu est du texte — un
`StyledText` par ligne avec ses runs — et non un canevas : une police à chasse
fixe suffit à aligner les colonnes, et gpui garde la charge du façonnage.

Le verrou de la grille est partagé avec la boucle d'E/S : **ne jamais dessiner
sous ce verrou**, d'où l'instantané.

**La molette n'a pas le même sens selon l'écran.** Dans l'écran secondaire —
un agent, `less`, `vim` — il n'y a pas d'historique : la grille est ce que le
programme dessine, et ce qui précède n'appartient qu'à lui. La molette s'y
traduit donc en flèches, trois lignes par cran, comme dans tous les terminaux ;
la faire défiler l'historique n'y produirait rien du tout. Ailleurs, elle
déplace bien l'affichage.

**Les lignes de l'historique sont numérotées négativement.** Le parcours de la
grille commence à `-display_offset` : dès qu'on remonte la molette, les lignes
visibles portent des indices négatifs. Les ramener par un `max(0)` les écrasait
toutes sur l'indice 0, où elles s'accumulaient en une seule — l'écran paraissait
« s'effacer » à chaque cran de molette. `snapshot::viewport_line` fait la
translation, pour les cellules comme pour le curseur, que remonter fait sortir
de la vue.

Les fractions de ligne sont **accumulées** (`take_lines`) : un pavé tactile en
envoie par dixièmes, et les arrondir chacune à zéro rend le défilement inerte.
La conversion se fait sur la hauteur d'une cellule, pas sur celle du texte
ambiant — elles diffèrent dès que le terminal n'a pas la taille de l'interface.

**Un programme peut demander la souris**, et il faut la lui donner
(`terminal::mouse`). Tant qu'on ne le faisait pas, la molette retombait sur les
flèches — la convention quand personne n'écoute — et un agent qui, lui, écoute,
recevait des déplacements de curseur au lieu d'un défilement. Il le disait :
« scroll wheel is sending arrow keys ».

Quatre décisions, et aucune n'est cosmétique :

- **Maj est la sortie de secours.** Un programme qui prend la souris prend
  aussi la sélection, et sans cette convention — celle de tous les terminaux —
  on ne pourrait plus rien copier de ce qu'il affiche.
- **Seul le bouton gauche part au programme.** Le milieu colle la sélection
  primaire et le droit ouvre le menu de Claudhub, d'où l'on copie justement.
  Les livrer aussi ne gagnerait que les rares interfaces qui les écoutent, et
  coûterait les deux seuls gestes qui n'ont pas d'équivalent ailleurs.
- **Un déplacement n'est rapporté qu'au changement de cellule.** Le programme
  redessine à chaque événement, et un geste de la main traverse dix cellules en
  cent événements.
- **Le format d'origine abandonne au-delà de la 223e colonne** plutôt que d'y
  rogner : un clic rapporté sur la mauvaise cellule est pire que pas de clic.
  C'est la raison d'être de SGR (`1006`), que demande tout ce qui a été écrit
  depuis quinze ans.

**Les marqueurs de session de l'agent qui nous a lancés sont effacés au
démarrage** (`agent::disinherit_session`, premier geste de `main`). Lancer
Claudhub depuis un agent est le cas courant — c'en est un qui l'écrit —, et ses
variables passaient à tout ce que nous démarrons : un `claude` ouvert dans un
onglet se croyait la sous-session de celui d'à côté et cessait d'enregistrer sa
transcription. La liste est explicite et non un balayage de `CLAUDE_CODE_*`,
qui emporterait la configuration de l'utilisateur avec les marqueurs. Effacer
dans notre propre environnement et non dans celui du pty : `wt` lance les hooks
du projet et `commit_msg` un agent en une passe, et Claudhub n'est la session de
personne pour eux non plus.

**Le redimensionnement est différé et planchérisé.** Un glissement à la souris
passe par toutes les largeurs intermédiaires ; transmettre chacune revient à
envoyer un `SIGWINCH` par image, et comme un shell redessine son invite *en
place*, ses redessins successifs s'empilent au lieu de se remplacer — l'écran
finit en fragments. La géométrie n'est donc transmise qu'après un temps
d'immobilité. Le plancher (`grid_size`) sert la même cause : sous vingt
colonnes, la moindre invite occupe des dizaines de lignes et fait déborder
l'historique. En dessous, le panneau rogne, ce que fait aussi une fenêtre de
terminal qu'on rétrécit trop.

**Une ligne de terminal a une hauteur fixe et ne revient pas à la ligne.**
Sans cela, une ligne plus large que le panneau est repliée par gpui : elle
occupe deux hauteurs, pousse tout ce qui suit vers le bas, et la grille ne
correspond plus à ce que le programme croit afficher. La géométrie étant
mesurée après la mise en page, la grille reste trop large pendant une frame
après chaque rétrécissement — précisément le moment où le repli s'installe.

Le curseur est un rectangle translucide posé par-dessus la grille, et non une
cellule inversée : l'inversion demanderait de redessiner le glyphe à l'envers,
alors qu'un fond translucide laisse lire ce qui est dessous. Il ne clignote
pas — un clignotement réveillerait l'interface deux fois par seconde et par
onglet, en permanence, pour une information que la position et le contraste
donnent déjà.

La sélection est un attribut de style de `Segment`, comme le gras ou la
couleur. Ce n'est pas un détail d'implémentation : la fusion des runs la prend
alors en compte toute seule, et une sélection découpe les runs exactement où il
faut sans une ligne de code dédiée.

### Sentry

Claudhub **lit** Sentry ; il ne lui envoie jamais rien. Un rapport d'erreur est
un point de départ souvent meilleur qu'une intention — il porte déjà la trace
et le fichier fautif — et le geste utile est de le confier à un agent avec le
code autour des frames de l'application.

- **Le jeton voyage en `Secret`**, un newtype dont le `Debug` écrit à la main
  masque la valeur — même motif que le mot de passe de `db::Connection` : une
  `Cmd` se journalise, un secret non. Il voyage parce que le worker tourne
  parfois dans un autre processus (le serveur WSL), dont le fichier de
  réglages n'est pas le nôtre ; `SENTRY_TOKEN` garde la priorité **côté
  worker**, c'est l'environnement du serveur qui fait foi. La commande de
  l'éditeur externe et celle du message de commit voyagent pour la même
  raison, dans leurs `Cmd` respectives.
- **L'organisation est un réglage, le projet appartient au dépôt**
  (`Store::repos`) : deux dépôts d'une même organisation n'ont pas les mêmes
  erreurs.
- **File réseau**, celle de fetch/pull/push : une API distante met parfois
  plusieurs secondes et ne doit pas occuper un worker de lecture.
- **Le code cité vient de l'événement**, pas du disque : c'est le code
  *déployé* au moment de l'erreur, et le relire aujourd'hui donnerait autre
  chose. `sentry::prompt` cite la pile entière — c'est le chemin qui a mené là
  — mais le code des seules frames `in_app` : une pile de framework fait cent
  lignes, et le bug n'y est pas.
- Deux formes de pile existent selon le SDK (`exception` et `stacktrace`
  dans `entries`), et le compte d'occurrences arrive tantôt en nombre, tantôt
  en chaîne. Les deux sont lues, et une fixture le verrouille.

« Ouvrir un worktree pour cette issue » est la boucle complète : `wt` crée le
worktree avec les copies et les hooks du projet, et le prompt est livré à
l'agent **quand la liste des worktrees revient** — c'est le seul signal qui
dise que les hooks ont fini.

### Les raccourcis clavier

**Une seule table décrit chaque liaison** (`shortcuts::table!`), et c'est
d'elle que sortent à la fois `bind_keys` et la fenêtre d'aide. Deux listes
auraient divergé au premier ajout, et une aide qui ment sur les touches est
pire qu'une absence d'aide. La table porte les touches, le groupe, le prédicat
et la **clé i18n** du libellé ; un test vérifie que chaque clé existe dans les
deux catalogues, un autre que chaque touche se lit — `KeyBinding::new`
*panique* sur ce qu'elle ne sait pas lire, et `init` tourne au démarrage.

**Deux prédicats, et pas un seul.** Sous Linux, `secondary` **est** Ctrl : une
liaison sur `secondary-r` prend au shell sa recherche arrière, `secondary-s`
son XOFF, et rien ne le dit. Ce qui s'écrit avec la touche système et une
**seule lettre** passe donc par `WINDOW_PREDICATE`, qui exclut le terminal ; ce
qui demande Maj ou une touche de fonction vaut partout (`PREDICATE`). C'est la
convention que les terminaux ont eux-mêmes fixée : Ctrl+Maj+C pour copier,
parce que Ctrl+C est pris.

**Une seule lettre est prise quand même**, et l'exception dit la règle :
`Ctrl+T` masque les terminaux, et ce qu'il masque est le terminal dans lequel
on tape. Une liaison qui s'arrête à son bord s'arrête précisément là où on en a
besoin — devoir cliquer ailleurs pour avoir le droit de le fermer n'est pas un
raccourci. Elle laissait le terminal tranquille au départ, avec `Ctrl+\`` comme
sortie de secours ; cet accent grave **n'existe pas sur un clavier AZERTY**, où
il est une touche morte derrière AltGr, si bien qu'il n'y avait aucun moyen sur
la moitié des claviers. Ce qu'on prend au programme est le `transpose-chars` de
readline.

**Un panneau ne s'active pas au clavier**, et ce n'est pas un oubli :
`TabPanel::set_active_ix` est privé dans gpui-component 0.5.1 et rien de public
n'en tient lieu. `Ctrl+1` à `Ctrl+9` désignent donc des **worktrees**, ce qui
est de toute façon le geste central — et leur ordre est celui de la barre
latérale, replis compris, sans quoi le même chiffre ne désignerait pas le même
worktree d'un pliage à l'autre. Les **écrans**, eux, s'atteignent par
`Alt+1` à `Alt+5`, et **pas** par `Ctrl+Maj+1` : gpui *retire* le Maj des
modificateurs quand la touche est un caractère sans casse — « on ne garde Maj
que pour les majuscules » —, si bien qu'un `secondary-shift-1` arrive comme
`ctrl-&` ou `ctrl-#` selon la disposition du clavier et ne se déclenche jamais,
en silence. Alt est conservé et la touche reste le chiffre. C'est valable
jusque dans le terminal : ce qu'on lui prend est le préfixe d'argument
numérique de readline, pas un caractère de contrôle.

**Tout se reconfigure**, page « Clavier » des réglages. Ce que `settings.json`
garde n'est pas la table mais ce qui s'en écarte (`Settings::shortcuts`, id →
touches) : une version qui ajoute un raccourci n'a besoin de rien dans ce
fichier, et un champ vidé **désactive** la liaison — la ligne reste, si bien
qu'on voit ce qu'on a éteint.

Six points, et aucun ne se devine :

- **L'id d'une liaison est sa famille et ses touches par défaut**
  (`review:j`, `window:f5`). Le défaut ne bouge jamais, donc l'id survit à la
  personnalisation qu'il porte. L'action ne conviendrait pas — la moitié en ont
  deux, `F5` et `Ctrl+R` pour un seul rafraîchissement — et les touches seules
  non plus : `Entrée` existe dans trois familles. Un test vérifie qu'aucun id
  n'est pris deux fois.
- **On ne remplace pas une liaison, on refait le trousseau.** Le keymap de gpui
  n'accepte que des ajouts et c'est le dernier qui gagne : l'ancienne touche
  continuerait de déclencher à côté de la nouvelle. `shortcuts::rebind` vide
  donc tout et réinstalle.
- **D'où l'instantané** (`BaseKeymap`) : vider le keymap emporte aussi les
  liaisons de gpui-component — l'éditeur intégré perdrait ses flèches, les
  dialogues leur Échap — et rien de public ne les repose. `init` en garde donc
  une copie, prise **avant** les nôtres. C'est ce qui fixe l'ordre de
  `ui::run` : `gpui_component::init`, puis le global des réglages, puis
  `shortcuts::init`.
- **Une touche que gpui lit n'est pas une touche qui existe.**
  `Keystroke::parse` accepte `ctrl-nonsense` sans broncher, et la liaison qui
  en sort est installée, ne se déclenche jamais et ne dit pas pourquoi — pire
  qu'un refus. `valid_keys` exige donc en plus que la touche soit un seul
  caractère ou l'un des noms de `NAMED_KEYS`, écrit à la main parce que la
  liste de gpui est *négative* et privée.
- **On appuie sur la touche plutôt que de l'écrire.** Le bouton « Appuyer »
  attend la frappe suivante et l'inscrit — c'est ce qu'on veut neuf fois sur
  dix ; le champ reste pour la dixième, une suite (`g g`) n'ayant pas de frappe
  unique à capturer, et le vider est la façon d'éteindre une liaison. La
  capture passe par un **intercepteur de frappes** (`intercept_keystrokes`) et
  non par une zone qui prend le focus : un intercepteur tourne *avant* le
  keymap, et `stop_propagation` y arrête la distribution net — appuyer sur
  `Ctrl+T` pour l'enregistrer ne doit pas aussi masquer les terminaux. La zone
  focalisable, elle, aurait demandé un contexte exclu de **chacun** des huit
  prédicats, dont un oubli est un raccourci qui se déclenche pendant qu'on
  l'enregistre. Échap abandonne, un modificateur seul est attendu, et
  `stroke_syntax` écrit la frappe comme la table l'écrirait — un test verrouille
  l'aller-retour, sans quoi la même touche se lirait d'une façon quand on la
  déclare et d'une autre quand on la presse.
- **Ce qui ne se lit pas n'est pas écrit** : le champ garde le texte, la ligne
  le signale, et le keymap garde ce qu'il avait. Le garde-fou est doublé côté
  installation, qui saute et journalise plutôt que de paniquer au démarrage.
- **Deux touches identiques sous le même prédicat sont signalées.** C'est
  l'ordre de déclaration qui les départage, ce qui n'est jamais ce qu'on
  voulait ; le compte est fait une fois par rendu et non par ligne.

L'aide se lit dans `shortcuts::sheet`, qui montre les touches **en vigueur** —
une aide qui ment sur les touches est pire que pas d'aide — et qui **réunit sur
une ligne** les plusieurs façons de faire un même geste : `F5` et `Ctrl+R`, `Ctrl+1` à `Ctrl+9`
rendus comme une plage, la flèche et son équivalent vim. C'est le geste qu'on
cherche dans cette liste, pas la touche.

### Le mode vim

Désactivé par défaut, et il faut que ça le reste : ses liaisons sont des
**lettres nues**. Un seul réglage (`Settings::vim_mode`) allume deux mécanismes
qui n'ont rien en commun, et les confondre est la façon la plus sûre de ne
comprendre ni l'un ni l'autre.

**Hors de l'éditeur, ce sont des liaisons**, et une liaison ne connaît pas de
mode : la main gauche sur la rangée de repos pour relire — `j`/`k` d'un **bloc
modifié** au suivant, `h`/`l` d'un fichier à l'autre, `gg`/`G`,
`Ctrl+D`/`Ctrl+U`, `y` pour copier, `/` puis `n`/`N` pour chercher.

`j`/`k` suivent les flèches nues et non la ligne : relire, c'est aller d'une
modification à la suivante, et les lignes de contexte entre deux blocs n'ont
rien à montrer. La touche système descend d'une ligne — `Ctrl+J`/`Ctrl+K`,
exactement comme `secondary-up`/`down` à côté : nu pour le geste qu'on fait
mille fois, modifié pour celui qu'on fait quand quelque chose cloche.
`]c`/`[c`, la convention de vim-gitgutter, restent à côté ; l'aide réunit les
deux façons sur une ligne.

**Dans l'éditeur, ce sont des modes** (`ui::vim`) : là, il y a du texte à
éditer, et `d` veut dire deux choses selon l'endroit où l'on en est. Voir « Les
modes de vim dans l'éditeur ».

**Le réglage n'ajoute pas de liaisons, il allume un contexte.** `bind_keys`
s'appelle une fois au démarrage ; les liaisons vim sont donc posées
inconditionnellement, sous un prédicat qui exige `ClaudhubVim`, et c'est la vue
racine qui déclare ce contexte quand le réglage est vrai. Le contexte étant
recalculé à chaque rendu, la bascule est immédiate.

Corollaire à connaître, et il ne se devine pas : **`ClaudhubVim` doit être
déclaré sur le même nœud que l'identifiant avec lequel il se combine.**
`KeyBindingContextPredicate::depth_of` évalue chaque identifiant contre un seul
niveau de la pile de contextes, si bien que `ClaudhubExplorer && ClaudhubVim`
ne se rencontre jamais quand l'un est sur l'arbre et l'autre sur la racine.
D'où `explorer_context(vim)` en plus de `context(vim)`.

### Les modes de vim dans l'éditeur

`ui::vim` est la machine modale de l'éditeur intégré : les modes, les motions,
les opérateurs, les registres. **Elle ne connaît aucun type de gpui** — elle
reçoit le texte et le curseur, elle rend l'édition à appliquer —, comme
`motion.rs` et `notes.rs` devant leurs vues, et c'est ce qui la rend testable :
vingt-six tests décrivent ce que fait chaque touche.

C'est un **sous-ensemble**, et c'en est un exprès : ce qu'une main tape sans y
penser. `hjkl`, `w`/`b`/`e` (et leurs majuscules), `0`/`^`/`$`, `gg`/`G`,
`f`/`F`/`t`/`T`, `Ctrl+D`/`Ctrl+U`/`Ctrl+F`/`Ctrl+B` ; `d`, `c`, `y` avec leur
motion, doublés, avec un compte ou avec un **objet de texte** ; `x`, `X`, `s`,
`S`, `D`, `C`, `Y`, `r`, `J`, `p`, `P`, `o`, `O`, `i`, `a`, `I`, `A` ; `v`,
`V`, `Esc` ; `u` ; `/`, `?`, `n`, `N` ; et `:w`, `:q`, `:wq`, `:42`. Ce qui n'y
est pas est ce qu'un éditeur fait déjà mieux (les registres nommés, les macros,
les marques) ou ce que Claudhub a ailleurs.

**Les objets de texte** — `iw`/`aw`, `i(`/`a(` et leurs frères pour `{`, `[`,
`<`, `i"`/`a"` pour les trois guillemets — valent après un opérateur et en mode
visuel, jamais seuls : `i` et `a` sont d'abord les deux façons d'entrer en
insertion, et c'est le contexte qui décide. Trois choses s'y jouent :

- **Un objet de mot ne traverse pas un saut de ligne.** `daw` sur le dernier
  mot d'une ligne prendrait sinon la suivante et son indentation. `aw` prend
  les blancs qui **suivent**, ou ceux qui précèdent quand il n'y en a pas
  après — c'est la règle de vim, et c'est ce qui fait qu'effacer un mot au
  milieu d'une phrase ne laisse pas deux espaces.
- **Les paires se comptent par imbrication**, dans les deux sens : `di(` depuis
  l'intérieur d'un appel qui en contient un autre prend le bon. Le curseur
  posé **sur** un des deux délimiteurs désigne cette paire-là.
- **Les guillemets d'une ligne s'apparient dans l'ordre** — le deuxième ferme
  le premier, le quatrième le troisième. C'est la seule lecture qui n'ait
  besoin de savoir ni ce qu'est une échappement ni ce qu'est un commentaire, et
  c'est celle de vim ; le prix est qu'un `\"` au milieu d'une chaîne décale
  l'appariement, ce qui se voit tout de suite et se corrige d'un `u`.

Six points qui ne se devinent pas :

- **L'écoute est en phase de capture, sur un ancêtre de l'éditeur**, et cette
  place *est* le mécanisme. Un écouteur de touche s'exécute **après** les
  liaisons — ce qui laisse `Ctrl+S` et `Alt+2` à la fenêtre — mais **avant** que
  la plateforme ne livre le caractère au champ qui a le focus. Consommer
  l'événement est donc ce qui empêche un `d` nu d'être tapé dans le fichier ; le
  laisser passer est ce qui fait du mode insertion un éditeur ordinaire. Rien
  n'est posé quand le réglage est éteint.
- **Le caractère lu est celui que la frappe a *produit*** (`key_char`), pas la
  touche sur laquelle on a appuyé : c'est ce qui met `$`, `^` et `0` là où il
  faut sur un clavier AZERTY, où ils sont décalés ou dans la rangée des
  chiffres.
- **Le curseur bloc est peint par nous** (`Editing::cursor`, une
  `TextDecorationCollection` de plus à côté de celle du yank). Il a d'abord été
  la **sélection** du caractère sous le curseur, et c'était faux deux fois : une
  sélection ne s'écrit qu'à une frappe, si bien qu'un fichier qui vient de
  s'ouvrir n'avait aucun curseur jusqu'à la première touche et n'en avait plus
  après un clic de souris ; et elle se peint dans le `selection` du thème, à
  quelques pour cent de clarté du fond — sur One Dark, il fallait le chercher.
  Une décoration, elle, est peinte **par-dessus** la sélection — les runs de
  texte viennent après les chemins de sélection dans l'ordre de peinture — et
  dans nos couleurs, c'est-à-dire le curseur bloc de tout terminal. La sélection reste posée par
  les `Change`, et c'est toujours elle qui dit où vim en est
  (`selected_range().start`), si bien qu'un clic de souris déplace le curseur
  sans rien à synchroniser. Le découpage se compte en **caractères** : en
  octets, un fichier accentué se couperait au milieu d'un caractère et
  paniquerait.
- **Le bloc se redemande à chaque frame, et se recalcule rarement.**
  `Vim::cursor` est interrogé au rendu et non après une frappe — c'est ce qui le
  met là dès la première image et le fait suivre la souris —, mais `value()`
  recopie le fichier entier : `Editing::cursor_at` retient le mode, le caret et
  la longueur du texte (lue sur le rope, qui est emprunté) pour laquelle il a
  été calculé, et une frame qui n'a rien vu bouger ne calcule rien.
- **Le curseur dit le mode par sa couleur** (`vim_mode_colour`) : celle du
  caret du thème en normal — là où l'œil l'attend —, `success` en insertion,
  `magenta` en visuel, `cyan` en visuel-ligne. Une seule table, partagée avec la
  pastille de la barre du fichier : le mot et le bloc disent la même chose, et
  c'est ce qui permet de lire le mode sans quitter le curseur des yeux, ce qui
  est tout l'intérêt d'un éditeur modal. Le glyphe, lui, prend celle des deux
  extrémités du thème qui **contraste** avec le bloc (`ink_on`) : le fond seul
  est juste en thème sombre, où le bloc est clair, et fait un trou en thème
  clair.
- **Le caret n'est rendu que là où il n'y a rien à couvrir.** Une ligne vide et
  la fin du fichier n'ont pas de caractère à peindre : `Vim::cursor` y rend une
  plage vide, et `set_cursor_hidden` rend alors le trait de l'éditeur — un
  curseur qui disparaît sur une ligne vide serait pire que deux curseurs.
- **Un yank s'allume une fraction de seconde**, comme le fait
  vim-highlightedyank. C'est le seul geste de vim qui ne change rien à
  l'écran : sans signe, on n'est jamais sûr qu'il a pris, et on recommence. Le
  module pur rend la plage (`Change::flash`) — et pour un **yank seulement**,
  un `d` ayant emporté le texte qu'il s'agirait d'éclairer — ; la vue la pose
  sur une `TextDecorationCollection`, créée une fois avec le fichier parce
  qu'une collection suit le texte à travers ses éditions, et l'éteint par un
  minuteur. Quatre choix : la teinte est celle d'une occurrence de recherche
  (`find::highlight_color`), déjà la couleur que cette interface pose sur du
  code pour dire « ici » et qui suit le thème ; la marque ne touche pas à la
  sélection, le curseur bloc étant juste là ; elle dure trois cents
  millisecondes et non la seconde du greffon, choisie pour un terminal où rien
  d'autre ne bouge ; et sa collection est créée **après** celle du curseur, si
  bien que le bloc reste visible dessous — les collections se composent dans
  l'ordre de création, la première gardant ses propriétés, et c'est le bon sens
  de lecture : l'éclat dit ce qui a été pris, le bloc dit où l'on est. Le
  minuteur est **remplacé** et non empilé — un `Task` gpui s'annule quand on le
  laisse tomber —, sans quoi le premier éteindrait le second yank.
- **Le caret s'éteint hors du mode insertion**, et c'est le septième commit du
  fork (`InputState::set_cursor_hidden`). Rien de public ne le faisait : le
  style de l'éditeur — qui porte la couleur du caret — est reconstruit depuis
  le thème à **chaque** rendu, si bien qu'un caret transparent posé par nous ne
  survivrait pas à la frame suivante, et `disabled`, qui le cache, grise le
  texte et le fond avec. Le clignotement est arrêté avec lui plutôt que laissé
  à tourner derrière un caret que personne ne peint : un clignotement est un
  rendu de la fenêtre deux fois par seconde tant que le champ a le focus —
  c'est le raisonnement du curseur du terminal. La consigne est **relue à
  chaque rendu**, comme `TerminalView::sync_font` : le mode change sous les
  touches et le réglage sous le formulaire, l'appel est idempotent, et c'est ce
  qui rend le caret dès qu'on éteint le mode.
- **`Ctrl+E` et `Ctrl+Y` bougent la page, pas le caret**, et c'est tout leur
  intérêt : lire la suite sans perdre sa place. Conséquence qui se paie
  ailleurs : `Ctrl+Y` n'est plus le rétablissement de l'éditeur, donc le redo
  n'a plus que `Ctrl+R` — que la fenêtre prenait pour un rafraîchissement.
  D'où `ClaudhubEditorVim`, un **second nom** de contexte et non
  `ClaudhubEditor && ClaudhubVim` : `depth_of` pèse chaque identifiant contre
  un seul niveau de la pile et les deux ne se rencontreraient jamais. La
  liaison de rafraîchissement l'exclut, et garde `F5`, qui est la touche qu'on
  cherche de toute façon.
- **Le préfixe `z` fait deux métiers, et aucun ne touche au texte** — d'où sa
  place avant le partage normal/visuel, les deux modes y répondant pareil.
  `zz`, `zt`, `zb` placent la ligne du caret dans la vue **sans déplacer le
  caret** : c'est ce qui les distingue de `z.` et `z-`, qui vont en plus au
  premier non-blanc et seraient donc deux réponses là où `Response` n'en porte
  qu'une — ils ne sont pas là. `zc`, `zo`, `za`, `zM`, `zR`, `zm`, `zr`
  replient : où commence et où finit un repli est la réponse de la grammaire,
  jamais la nôtre, et tout ce qui se décide ici est **lesquels** fermer. `zc`,
  `zo` et `za` agissent sur le repli **le plus intérieur** qui contient le
  caret, celui qu'on est en train de lire.
- **Un niveau de repli est une profondeur d'imbrication**, et elle se relit
  dans les plages : l'éditeur les donne à plat, une méthode dans une classe est
  simplement contenue par elle. `ui::folds` fait ce calcul et rien d'autre —
  pur, testé, la seule partie de tout cela qui se trompe en silence. Le niveau
  courant vit dans `Editing::fold_level`, `None` valant « tout ouvert »,
  c'est-à-dire un cran au-delà du plus profond : `zr` qui l'atteint est `zR`.
  Chaque changement de niveau **repart de tout ouvert** plutôt que d'ajouter à
  ce qui est là, la carte des replis ne sachant pas rouvrir par addition.
- **Rien de tout cela n'était atteignable** : les icônes de la gouttière
  replient depuis l'intérieur de l'élément, ce qui sert une souris et rien
  d'autre — `display_map` est privé. C'est le neuvième commit du fork, cinq
  méthodes sur `InputState` qui font les trois mêmes appels que l'icône.
- **`u` et `Ctrl+R` sont rendus à l'éditeur**, qui est le seul à savoir ce
  qu'était la dernière transaction — d'où `Command::Undo`, une action que la
  vue redispatche. `Ctrl+R` ne nous parvient d'ailleurs jamais : la fenêtre en
  fait un rafraîchissement, et le rétablissement se fait donc par le `Ctrl+Y`
  de l'éditeur.
- **Le défilement est repris à la main quand la tête remonte.**
  `set_selected_range` défile vers la **fin** de ce qu'on lui donne, ce qui est
  le curseur en mode normal mais le mauvais bout d'une sélection étendue vers
  le haut : `V` puis `k` sortirait du panneau sans que la vue suive.
- **Le registre est interne, sauf réglage.** `Settings::vim_clipboard` — éteint
  par défaut, comme vim — fait passer `y`, `d`, `x` et `p` par le presse-papiers
  du système, ce qui est le `unnamedplus` de vim. Le module pur n'a évidemment
  pas de presse-papiers : il **dit** ce qu'il vient d'arracher (`Change::yank`)
  et **accepte** qu'on lui pose un registre (`set_register`), et c'est la vue
  qui fait la navette. Deux détails s'y paient : le presse-papiers n'est lu
  qu'au moment où un `p` arrive et non à chaque frappe — une lecture est un
  aller-retour au serveur d'affichage —, et le drapeau « lignes entières », qui
  n'existe pas dans un presse-papiers, se relit sur le saut de ligne final.
- **`w` a l'exception de vim** : `dw` sur le dernier mot d'une ligne s'arrête
  au bout de la ligne au lieu d'avaler le saut de ligne et l'indentation de la
  suivante. C'est la seule irrégularité de la grammaire, et elle est dans
  l'opérateur, pas dans la motion.

Le mode courant et ce qui est en train d'être tapé — la ligne `/`, les touches
d'une commande incomplète — s'affichent dans la **barre du fichier**, là où
l'œil est déjà, et non dans la barre d'état à l'autre bout de la fenêtre.

### Ce qui tient lieu de système d'extension

La question revient à chaque intégration, alors elle est tranchée ici. Trois
niveaux, du moins cher au plus cher :

1. **Le `wt.toml` du projet** — tâches, questions, statuts, URLs, et depuis
   `wt` 0.7 les **serveurs de langage** (`[lsp.<nom>]`). Claudhub les affiche
   sans les connaître, et cela n'a coûté que la dépendance à `wt`. C'est le vrai
   système d'extension.
2. **Des commandes déclarées dans les réglages** — les profils d'agent en sont
   le premier exemple : un nom, une commande, un environnement. Le message de
   commit proposé en est le second, et il montre la forme la plus économe :
   une ligne de commande, le texte par l'entrée standard, la réponse par la
   sortie. Les connexions aux bases de données en sont le troisième, les
   serveurs de langage le quatrième. Pour ce qui n'est pas propre à un projet.
3. **Des extensions wasm, à la Zed — écarté.** Rien dans les besoins listés ne
   le demande, et le coût est sans commune mesure.

`wt` et Sentry sont donc des **modules compilés**, pas des greffons : les
traiter autrement ferait payer un mécanisme générique pour deux cas.

## Conventions gpui

Elles viennent d'Aviary, et les enfreindre produit des bugs silencieux.

- L'état qui survit à une frame vit dans un `Entity<T>` créé **une fois** dans
  le constructeur. Un `InputState` recréé dans `render` perd le curseur, la
  sélection et le texte dès la première frappe.
- `cx.listener(...)` pour les gestionnaires qui mutent la vue ; les
  souscriptions se font dans le constructeur, jamais dans `render`.
- `let theme = cx.theme().clone();` dès qu'une fermeture de rendu aura besoin
  de `&mut cx` — `cx.theme()` emprunte.
- `Theme::change` réinitialise les couleurs : toute palette s'applique
  **après**, puis `cx.refresh_windows()`.
- La vue racine doit ré-émettre les couches de `Root` (dialogues,
  notifications) à la fin de son `render`, sinon elles ne s'affichent nulle
  part.
- **La fermeture d'`open_dialog` ne doit rien toucher à `ClaudhubApp`.** C'est
  un `Fn` que `Root` retient et rappelle **à chaque frame**, depuis le rendu de
  la vue racine — donc au milieu de l'emprunt de l'application. Un
  `entity.update(cx, …)` là-dedans panique (« cannot update … while it is
  already being updated »), et un `read` aussi, pour la même raison : l'entité
  est sortie de la table le temps de son rendu. Un état que le dialogue affiche
  *et* modifie vit donc dans une **entité à lui**, posée en enfant (le motif de
  `server::WslPrompt`) ; ce qui ne s'exécute qu'au clic — `on_ok`, `on_cancel`,
  `on_click` — est libre, l'emprunt étant rendu à ce moment-là. Les autres
  fermetures de ce dépôt reçoivent `_cx` pour cette raison ; celle des
  raccourcis ne s'en sert que pour lire le thème, qui est un global.
- **Un `Dialog` ne peint pas ses boutons.** `on_ok` et `on_cancel` n'installent
  que les rappels d'Entrée et d'Échap : la rangée OK/Annuler n'est rendue que
  par un `AlertDialog`, qui ne prend ni champ de saisie, ni liste de questions,
  ni extrait de code — c'est-à-dire aucune des confirmations de cette fenêtre.
  Avec `close_button(false)` et `overlay_closable(false)`, un dialogue auquel on
  ne sait pas répondre est un dialogue dont on ne sort pas. `ui::dialogs::confirm`
  rend donc le pied de page, et ses boutons **dispatchent les mêmes actions que
  les touches** (`Confirm`, `Cancel`) : les deux chemins finissent dans le même
  `on_ok`, comme les boutons et les flèches de la barre du diff.
- **`key_context` prend un identifiant, pas un prédicat.** Passer
  `"Claudhub && !Dialog"` à `key_context` fait boucler le parseur et déborder la
  pile au premier rendu. L'expression va dans le troisième argument de
  `KeyBinding::new` ; le contexte va dans `shortcuts::context()`.
- Les raccourcis de l'application passent par `secondary-` : le reste du
  clavier appartient au programme qui tourne dans le terminal — sauf les
  flèches nues et, mode vim allumé, la rangée de repos. Voir « Les raccourcis
  clavier » pour ce que `secondary` prend au shell, et à quelles conditions.
- gpui rend via Vulkan sur Linux : `vulkan-loader` doit être dans
  `LD_LIBRARY_PATH`, ce dont `shell.nix` se charge.

## Interface bilingue

Toute chaîne visible passe par `tr!` (défini dans `main.rs`), qui rend un
`SharedString` — pas un `String` : les catalogues compilés donnent des
`Cow::Borrowed(&'static str)`, et une frame en rend des centaines.

`assets/i18n/{fr,en}.json`, objets plats, clés en kebab-case préfixées par
domaine (`review-`, `branch-`, `action-`). Les deux catalogues doivent avoir
les mêmes clés et les mêmes substitutions `%{…}` : `ui::i18n_tests` le vérifie.

**Le code est en anglais, la documentation en français.** Commentaires, doc
comments, noms et messages d'erreur des couches du cœur sont en anglais — c'est
la langue de gpui, de git et des dépendances qu'on lit à côté ; ce fichier, le
README et le `justfile` restent en français, ils s'adressent à qui tient le
dépôt. Un message d'erreur d'un worker remonte donc en anglais dans la barre
d'état, là où tout ce qui passe par `tr!` suit la langue choisie.

Corollaire d'un renommage : l'index du coffre s'appelle `Review.md`
(`vault::INDEX`) et non plus `Relecture.md`, que `vault::LEGACY_INDEX` **relit
sans jamais l'écrire** — un coffre contient des fichiers auxquels quelqu'un a
pu faire un lien, et perdre en silence les coches d'une relecture existante
coûterait plus cher que de porter une constante.

## Tests

Les couches `git`, `terminal` et `runtime` sont testables sans contexte gpui, et
c'est là que sont les tests. Ils portent sur les formats que nous parsons —
sortie porcelain, diff unifié, séquences de touches — parce que c'est là que se
trouvent les régressions silencieuses : un chemin renommé mal découpé produit
une liste plausible mais fausse.

`watch::tests::a_real_write_reaches_the_receiver` est le seul test qui touche le
système de fichiers ; il prouve la chaîne complète de la surveillance.
