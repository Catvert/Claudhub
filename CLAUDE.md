# CLAUDE.md

Guide de ce dépôt pour Claude Code et pour tout contributeur. C'est le **seul**
document d'architecture : quand un changement touche la structure, il se met à
jour dans le même commit.

## Commandes

Tout passe par `nix-shell` via le `justfile` ; n'appelez `cargo` directement
que si les bibliothèques de `shell.nix` sont déjà dans le périmètre.

- `just` / `just run` — build debug et lancement
- `just check` / `just clippy` (`-D warnings`) / `just fmt` / `just test`
- Un test isolé : `nix-shell --quiet --run "cargo test watch"`

Le projet doit passer `cargo fmt --check`, `clippy --all-targets -- -D warnings`
et `cargo test` en permanence.

## Architecture

Trois couches, et une règle qui les sépare : **seule `src/ui/` connaît gpui, et
elle ne fait jamais d'entrée-sortie**.

```
src/
  files.rs      lire, écrire (sous condition), ranger, éditeur externe
  sentry.rs     issues et traces Sentry, testées sur fixture
  wt.rs         le `wt.toml` d'un projet : questions, tâches, statut, URLs
  git/          couche git — sous-processus `git`, testable sans gpui
    mod.rs      exécution des commandes (stdin fermé, LC_ALL=C, pas de pager)
    repo.rs     découverte, worktrees, écritures (stage, commit, push…)
    status.rs   `status --porcelain=v2 -z` → index et worktree séparés
    branch.rs   `for-each-ref` → branches, amont, divergence
    diff.rs     `--numstat` et diff unifié → fichiers, hunks, lignes
    history.rs  `git log` → commits, et la disposition du graphe
  runtime/      les workers
    protocol.rs `Cmd` / `Evt` — des données, aucune logique
    mod.rs      trois threads consommant le même canal de commandes
    watch.rs    surveillance de fichiers (notify), debounce 250 ms,
                limitée aux dossiers que git connaît
  terminal/     émulation
    mod.rs      pty + `Term` alacritty derrière un `FairMutex`
    snapshot.rs grille → lignes et runs de style, sans tenir le verrou
    keys.rs     frappe gpui → octets (séquences xterm)
  ui/           tout gpui
    mod.rs      `run()`, `AssetSource`, polices, i18n
    app.rs      `ClaudhubApp` : l'état, la pompe d'événements, le chrome
    diff_view.rs   la vue de diff, virtualisée
    history_view.rs  l'historique et son graphe peint
    highlight.rs   coloration tree-sitter d'un diff
    sidebar.rs / review.rs / branches.rs / terminal_view.rs
    settings.rs     les réglages et leur global
    settings_view.rs  le formulaire, bâti sur `gpui_component::setting`
    tree.rs         chemins → arborescence repliable, en indices
    file_icons.rs   l'icône et la teinte d'un fichier, d'après son nom
                    (marques dans assets/icons/lang/, CC0)
    explorer.rs     l'explorateur de projet et l'éditeur intégré
    sentry_view.rs  les issues, leur trace, et de quoi les confier
    conflicts.rs    les conflits et le garde-fou d'une opération à mi-chemin
    worktree_ops.rs création guidée, tâches du projet, intégration
    store.rs        ce qu'on retient par worktree : base, replis, notes
    notes.rs        le modèle des notes, leur ancrage et leur prompt
    notes_view.rs   les gestes de la relecture annotée et son panneau
    vault.rs        les notes, le suivi de relecture et la liste de tâches
                    en Markdown, rendus et relus — aucune entrée-sortie ici
    find.rs         la recherche d'un panneau, et son routage
    motion.rs       le lissage de la molette, sans rien de gpui dedans
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
`Watcher::watch` n'envoie qu'un ordre à un thread dédié et rend la main
immédiatement. Corollaire à connaître : la surveillance n'est pas effective au
retour de l'appel — sans importance, puisque la sélection d'un worktree
déclenche de toute façon une lecture du statut.

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

**Les flèches sont les seules touches qui ne passent pas par la touche
système**, et c'est ce qui les rend délicates : elles appartiennent d'abord à
qui a le focus. `NAVIGATION_PREDICATE` exclut donc les champs de saisie, les
terminaux et les couches flottantes, exactement comme la copie.

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
il suffit de les retirer des replis, une chaîne fusionnée restant un ancêtre de
ce qu'elle contient.

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

### Lire et retoucher un fichier

`InputState::code_editor(langue).line_number(true)` fournit la coloration,
l'auto-indentation, les numéros et la recherche. Le langage vient de la même
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

L'éditeur **prend la place du diff** plutôt que d'occuper un panneau à lui : on
regarde l'un ou l'autre, et deux onglets à faire basculer pour un geste qui
vient de l'explorateur seraient un aller-retour de trop. **L'onglet dit alors
« Éditeur »** : le titre suit le contenu, sans quoi un onglet nommé « Diff »
ment sur ce qu'on a sous les yeux. Il est mis en cache dans `DiffPanel`, pour
la même raison que la visibilité des conflits — `Panel::title` est appelé par
le dock au fil du rendu de sa barre d'onglets, et y lire l'entité racine
pendant qu'elle se met à jour est ce que gpui refuse par une panique.

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

Corollaire pour les vues : ce qui dépend d'un réglage se **relit à chaque
rendu**, jamais au moment de la construction. `TerminalView::sync_font` en est
l'exemple : changer la police invalide la géométrie mesurée, et il faut effacer
les bornes retenues pour que le pty soit redimensionné — sinon le texte change
de taille et le shell continue de croire à l'ancienne largeur en colonnes.

Les familles proposées viennent de `cx.text_system().all_font_names()`, filtrées
par convention de nommage pour les champs à chasse fixe : gpui n'expose pas
cette propriété de façon portable. La liste rate donc des familles, et le
fichier de réglages reste modifiable à la main pour ces cas-là.

### Le magasin d'état

Les réglages disent comment Claudhub s'affiche ; le magasin
(`store::StateStore`, `<config>/state.json`) dit où l'on en est — la base à
laquelle on compare *ce* worktree, les dossiers qu'on y a repliés, le prochain
numéro de note. Deux fichiers et non un seul : le premier se modifie à la
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
- Les **branches** ont déjà leur filtre à demeure ; `Ctrl+F` s'y contente de
  lui donner le focus, plutôt que d'empiler deux champs qui font la même chose.

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

Les panneaux non virtualisés — notes, conflits, Sentry, barre latérale —
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

Quatre choses qu'on ne devine pas :

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
- **L'éditeur intégré** n'est pas lissé : la poignée de défilement d'un
  `InputState` est `pub(crate)` dans gpui-component, hors d'atteinte. Le lisser
  demanderait de vendorer la bibliothèque, ce qui n'est pas un prix à payer
  pour un confort.
- **Le terminal** non plus : son défilement est le `display_offset` de la
  grille alacritty, compté en lignes entières, et il n'y a pas de demi-ligne à
  dessiner.

### Les hauteurs de ligne

Aucune hauteur de ligne ne s'écrit en dur : elles viennent de
`theme::row_height` / `tall_row_height` / `bar_height` / `toolbar_height`, qui
les déduisent de la taille du texte. `tall_row_height` est pour les lignes à
deux étages — un nom et son détail — qu'une hauteur d'une seule ligne fait
déborder sur la suivante. Une hauteur figée déborde dès qu'on grossit la police — et
c'est pire dans les listes virtualisées, qui ne mesurent rien et réservent
exactement ce qu'on leur annonce : la ligne suivante est recouverte, pas
repoussée. La barre latérale, elle, n'annonce aucune hauteur du tout et se
laisse dimensionner par son rembourrage, ses lignes portant deux lignes de
texte.

### Le zoom

Deux zones se règlent séparément — les diffs et le terminal — parce que
grossir la sortie d'un agent pour la lire ne doit pas déplacer le code relu à
côté. La molette avec la touche système agit sur la zone survolée, les
raccourcis (`secondary-=`, `secondary--`, `secondary-0`) sur celle qui a le
focus.

Piège à connaître dans la vue de diff : gpui n'a **pas de phase de capture pour
la molette**. Quand notre écouteur s'exécute, la liste a déjà défilé — les deux
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

**Le coffre est surveillé comme le worktree** (`Watcher::watch_dir`, un dossier
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

C'est le **premier onglet** du centre, devant « Modifications » : il dit où
l'on en est — ce qui reste à faire, ce qu'on a eu à dire — là où les trois
suivants disent ce qu'il y a à lire, et c'est par là qu'on reprend un worktree
qu'on a quitté hier.

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
été reconnu, et la barre latérale le nomme — à deux profils près, « un agent
travaille ici » ne dit pas lequel.

Piège du formulaire : la clé d'état d'une ligne de la table porte **le nombre
de profils** (`claudhub-agent-{n}-{i}`). `use_keyed_state` garde un état par
clé ; sans le compte, supprimer le premier profil laisserait les champs de la
ligne 0 remplis avec l'ancien, et l'on écrirait dans les réglages ce qu'on
croyait avoir supprimé. Renommer, lui, ne change pas le compte — les champs
gardent donc leur curseur pendant la frappe.

### La répartition du chrome

La barre d'outils ne porte que des **actions** ; ce qui décrit l'endroit où
l'on est — branche, avance et retard sur l'amont — vit dans la barre d'état.
Ces informations ne changent presque jamais, et la barre d'état ne portait
qu'un message épisodique, donc restait vide la plupart du temps pendant que la
barre du haut débordait. Les boutons y sont séparés en trois groupes — le
réseau, l'agent, les panneaux — par des filets verticaux.

Les états vides portent une icône et, quand une action s'impose, un bouton :
au premier lancement la barre latérale vide est la première chose qu'on voit,
et une phrase grise ne dit pas quoi faire.

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

**Les panneaux sont des cartes, peintes par `panels::pane_root`.**
`TabPanel::render` de gpui-component 0.5.1 est un `size_full().bg(background)`
sans rayon ni marge, et aucun jeton de thème ne l'atteint. On peint donc la
gouttière **à l'intérieur** du panneau et le contenu par-dessus, en retrait de
quatre pixels, arrondi et bordé : le dock ne sait pas qu'il a des cartes.
`overflow_hidden` n'y est pas décoratif — sans lui, une liste virtualisée
déborde par-dessus les coins, qui ne se voient plus.

**La gouttière est la couleur de la barre d'onglets**, dérivée du fond de
quelques pour cent de clarté en moins. Les deux sont le même plan — celui sur
lequel les cartes sont posées — et les peindre de deux couleurs
proches-mais-pas-égales est exactement ce qui fait qu'une fenêtre a l'air mal
assemblée. L'onglet actif prend la couleur de la carte qu'il ouvre. La vue
racine peint la gouttière elle aussi : ce qui se voit entre deux panneaux —
poignée de redimensionnement, zone repliée — appartient à ce plan-là.

**Une ligne de liste est une pastille, pas une bande.** Le fond d'une ligne
survolée ou sélectionnée s'arrête avant les bords, et il est arrondi. Piège à
connaître : **`uniform_list` ignore les marges de ses entrées**, dont il calcule
lui-même la taille. Le retrait appartient donc à la **liste**
(`.px_1()` sur l'`uniform_list`), et l'entrée ne porte que son rayon ; là où la
liste n'est pas virtualisée — la barre latérale — un `mx_1` sur la ligne suffit.
Le cas des branches est le troisième : l'entrée y est un conteneur à la hauteur
imposée et c'est son enfant qui porte le fond, la ligne ayant besoin d'un
rembourrage que le rayon ne doit pas recouper.

**Les onglets sont des pastilles** (`TabVariant::Segmented`), posé par
`dock_skin.set_tab_variant` à la construction de l'aire. Le variant par défaut
du dock, `Tab`, a un rayon **codé en dur à zéro** et rien dans le thème ne
l'atteint ; `Tab::with_variant` et `TabBar::with_variant` existent pourtant,
c'est seulement le panneau d'onglets du dock qui ne les transmettait pas. D'où
le **fork** (voir `Cargo.toml`) : deux commits au-dessus de leur `main` — l'un
fait passer un `TabVariant` de `DockSkin` jusqu'au `TabBar`, qui le propage
lui-même à chaque onglet ; l'autre ne dessine les coins en boîte bordée du
bandeau (préfixe des boutons de repli, suffixe zoom/menu) que pour le variant
classique, dont ils épousent les rectangles — sur les autres, ils se lisaient
comme un reste de chrome autour de boutons nus. Les commits ont vocation à
partir en PR, et le fork à disparaître avec elle. Le rail de `Segmented` a son
propre jeton (`tab_bar_segmented`), aligné sur la gouttière comme `tab_bar`.

**`PanelStyle::TabBar`, et non le défaut `Auto`** : `Auto` rend un titre plat
dès qu'un groupe n'a qu'un panneau, et « Branches » ou « Terminaux » n'avaient
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

Quatre pièges, tous rencontrés :

- **`DockItem::split_with_sizes` de gpui-component 0.5.1 ajoute chaque panneau
  deux fois** — deux boucles identiques dans le même corps — et la disposition
  obtenue n'est pas celle qu'on décrit. D'où `ui/layout.rs`, qui refait la
  fonction correctement.
- **Un panneau sans `StackPanel` parent est verrouillé.**
  `TabPanel::is_locked` rend vrai quand `stack_panel` est `None`, et rien ne se
  glisse ni ne s'accueille plus. S'être passé de `split` pour contourner le
  point précédent avait donc supprimé le glissement en entier : tout panneau
  doit être enveloppé, fût-ce dans un conteneur d'un seul élément (`wrap`).
- **`toggle_dock` ne notifie pas l'aire**, seulement le dock intérieur :
  l'observation qui enregistre ne se déclenche pas toute seule, d'où l'appel
  explicite.
- **Le dernier panneau d'une zone ne se déplace pas.** `is_last_panel` remonte
  la pile : un panneau seul dans un `TabPanel` seul dans son conteneur est
  figé. C'est pourquoi les terminaux vivent dans le centre, sous la revue, et
  non dans une zone d'accueil — leur pile en compte deux, donc ils se
  glissent. Leur disparition passe alors par `Panel::visible`, pas par un
  repli de zone.
- **Les tailles d'une division se donnent toutes.** Un `None` laisse la pile
  partager la hauteur en parts égales, et la proportion demandée passe à la
  trappe.
- **L'état se relit au moment d'écrire**, pas à l'appel : l'ouverture d'une
  zone est différée d'une frame, et le capturer tout de suite enregistrerait
  l'état d'avant le geste.
- **Le zoom d'un panneau est un bouton, pas une entrée de menu**
  (`panels::zoom_in_toolbar`, `PanelControl::Toolbar`) : deux clics pour une
  ligne unique n'en valent pas un. Le bouton `…` reste affiché malgré tout —
  `TabPanel::render_toolbar` le pose sans condition, et le retirer demanderait
  de vendorer gpui-component —, d'où l'entrée qu'il porte désormais.

La disposition est enregistrée dans `<config>/layout.json`, à part des
réglages : c'est l'état d'une fenêtre, volumineux et illisible, pas une
préférence qu'on écrit à la main. `LAYOUT_VERSION` la fait écarter quand les
panneaux changent de nom — reconstruire à partir de noms inconnus donnerait une
fenêtre pleine de cadres vides. Les panneaux se déclarent au registre du dock
(`panels::register`), sans quoi une disposition relue ne saurait pas les
fabriquer.

L'historique se charge au **rendu** de son onglet et non à la construction :
c'est ce qui évite un `git log` que personne ne regardera. D'où
`history_pending`, sans lequel chaque frame relancerait la commande pendant
tout le temps de la lecture.

### Masquer une vue

Le menu `…` d'un panneau ne contient qu'une chose : **masquer cette vue**.
Tout le reste de ce qu'un panneau sait faire vit dans sa propre barre — l'arbre
de la revue, les deux colonnes du diff, le repli de l'explorateur — et le
dupliquer là ferait deux chemins pour un même geste. Masquer, lui, ne parle pas
du contenu du panneau mais de sa place dans la fenêtre, et c'est le dock qui la
tient.

On revient par le **menu principal**, sous-menu « Vues » (`panels::VIEWS`) :
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
- **`VIEWS` est bâtie sur les constantes `Panel::NAME`**, pas sur des
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

La barre latérale dit, pour chaque worktree, ce qu'il a en chantier (`+n −m`)
et si un agent y travaille. Ces deux informations portent sur des worktrees
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

`agent::scan` est **Linux seulement**, par un `cfg` explicite et non par
accident : le parcours compile partout et échouerait en silence à l'ouverture
de `/proc`, ce qui se lit comme une détection cassée au lieu d'une absence
assumée. C'est aussi ce qui fixe la cible Windows à WSL2 plutôt qu'au natif.

La détection des agents passe par `/proc` et non par nos propres onglets : on
lance un agent depuis Claudhub, mais aussi depuis un terminal à côté, et c'est le
même travail qu'on veut voir. Le répertoire courant d'un processus dit dans
quel worktree il travaille ; le worktree le plus profond l'emporte, faute de
quoi un worktree imbriqué se verrait attribuer les agents de son parent.

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
  qui déclenche la création.
- **Les tâches partent dans un onglet de terminal, pas dans un panneau de
  sortie.** Elles sont interactives, colorées, parfois longues.
  `wt::task` rend les commandes — modèles résolus, environnement calculé — et
  c'est le terminal qui les lance, dans un `sh -lc`. Le partage avec la
  bibliothèque : ce qui tient une comptabilité (création, suppression, `up`,
  `down`) passe par elle, qui alloue les ports et écrit l'état ; le reste passe
  par le shell.

Le relevé de `[status] up` et de `[open]` est une commande shell par worktree :
**file de fond uniquement**, à la période des résumés, jamais devant un diff
demandé.

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

### Quel worktree s'ouvre

`runtime::open_repo` retient le checkout d'où l'ouverture vient, et non le
premier de la liste — qui est toujours le dépôt principal. Lancer `claudhub` dans
un worktree doit ouvrir *ce* worktree. Le worktree retenu est le plus profond
dont le chemin est un préfixe de celui demandé, faute de quoi un worktree
imbriqué dans un autre serait attribué au mauvais.

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
`@foreach` ou `{{ $x }}` que du texte : la vue arrivait avec ses balises
colorées et tout le vocabulaire de Blade en gris. La grammaire colore donc ce
qu'elle sait lire, puis `blade::overlay` repasse dessus les directives, les
échos et les commentaires — un scanner à la main, assumé comme tel. Deux
conséquences à retenir : un `.blade.php` ne reçoit **jamais** de prologue, sinon
ses balises seraient lues comme du code ; et un rôle Blade se traduit en style
par une **liste de noms**, du plus juste au plus sûrement présent, parce que nos
thèmes ne définissent ni `punctuation` ni `operator` — sans repli, les
délimiteurs d'un écho restaient invisibles. `blade::tests::every_scope_resolves_to_a_colour`
le vérifie, et `keys_of` compare désormais aussi les styles de coloration d'un
thème à l'autre.

PHP n'est pas dans les grammaires que gpui-component embarque, et c'est le
langage de la moitié des dépôts qu'on relit : `highlight::register_languages`
le déclare dans le registre partagé au démarrage, avec ses injections HTML et
SQL. À appeler avant tout rendu — le registre est un singleton verrouillé.

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

Limite connue : le rapport de souris (`MOUSE_MODE`) n'est pas implémenté. Un
programme qui demande à recevoir les événements de molette reçoit des flèches
à la place.

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

- **Le jeton ne circule pas dans une `Cmd`.** Le worker le lit lui-même
  (`SENTRY_TOKEN`, puis les réglages) : un secret n'a rien à faire dans une
  énumération qu'on journalise. Le fichier de réglages est en 0600, ce qui ne
  fait pas de lui un coffre — d'où la priorité donnée à l'environnement.
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

**Un panneau ne s'active pas au clavier**, et ce n'est pas un oubli :
`TabPanel::set_active_ix` est privé dans gpui-component 0.5.1 et rien de public
n'en tient lieu. `Ctrl+1` à `Ctrl+9` désignent donc des **worktrees**, ce qui
est de toute façon le geste central — et leur ordre est celui de la barre
latérale, replis compris, sans quoi le même chiffre ne désignerait pas le même
worktree d'un pliage à l'autre.

L'aide se lit dans `shortcuts::sheet`, qui **réunit sur une ligne** les
plusieurs façons de faire un même geste : `F5` et `Ctrl+R`, `Ctrl+1` à `Ctrl+9`
rendus comme une plage, la flèche et son équivalent vim. C'est le geste qu'on
cherche dans cette liste, pas la touche.

### Le mode vim

Désactivé par défaut, et il faut que ça le reste : ses liaisons sont des
**lettres nues**. Ce n'est pas un mode d'édition — il n'y a rien à éditer dans
un diff, et l'éditeur intégré appartient à gpui-component — mais la main gauche
sur la rangée de repos pour relire : `j`/`k` d'une ligne à l'autre, `h`/`l`
d'un fichier à l'autre, `]c`/`[c` d'un bloc modifié au suivant comme le fait
vim-gitgutter, `gg`/`G`, `Ctrl+D`/`Ctrl+U`, `y` pour copier, `/` puis `n`/`N`
pour chercher.

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

### Ce qui tient lieu de système d'extension

La question revient à chaque intégration, alors elle est tranchée ici. Trois
niveaux, du moins cher au plus cher :

1. **Le `wt.toml` du projet** — tâches, questions, statuts, URLs. Claudhub les
   affiche sans les connaître, et cela n'a coûté que la dépendance à `wt`.
   C'est le vrai système d'extension.
2. **Des commandes déclarées dans les réglages** — les profils d'agent en sont
   le premier exemple : un nom, une commande, un environnement. Pour ce qui
   n'est pas propre à un projet.
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

## Tests

Les couches `git`, `terminal` et `runtime` sont testables sans contexte gpui, et
c'est là que sont les tests. Ils portent sur les formats que nous parsons —
sortie porcelain, diff unifié, séquences de touches — parce que c'est là que se
trouvent les régressions silencieuses : un chemin renommé mal découpé produit
une liste plausible mais fausse.

`watch::tests::a_real_write_reaches_the_receiver` est le seul test qui touche le
système de fichiers ; il prouve la chaîne complète de la surveillance.
