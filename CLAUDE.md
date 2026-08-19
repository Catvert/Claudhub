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
  git/          couche git — sous-processus `git`, testable sans gpui
    mod.rs      exécution des commandes (stdin fermé, LC_ALL=C, pas de pager)
    repo.rs     découverte, worktrees, écritures (stage, commit, push…)
    status.rs   `status --porcelain=v2 -z` → index et worktree séparés
    branch.rs   `for-each-ref` → branches, amont, divergence
    diff.rs     `--numstat` et diff unifié → fichiers, hunks, lignes
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
    app.rs      `PerchApp` : l'état, la pompe d'événements, le chrome
    diff_view.rs   la vue de diff, virtualisée
    highlight.rs   coloration tree-sitter d'un diff
    sidebar.rs / review.rs / branches.rs / terminal_view.rs
    settings.rs / theme.rs / shortcuts.rs / icons.rs
```

### La boucle Cmd/Evt

Le thread d'interface envoie des `Cmd` par `runtime::Handle::send`. Trois
threads workers les consomment (`async_channel` est un canal MPMC, ils partagent
le même récepteur) et répondent par des `Evt`. `PerchApp::pump_events` les
draine par lots de 64 dans une tâche gpui de premier plan : un `update_in` par
événement forcerait un cycle d'effets à chaque fois.

Ajouter une opération, c'est : une variante de `Cmd`, un bras dans
`runtime::handle`, une ou plusieurs variantes d'`Evt`, un bras dans
`PerchApp::handle_event`. Jamais un appel à git depuis un `render` ou un
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

**Poser les surveillances ne se fait jamais dans le thread d'interface** :
c'était une demi-seconde de fenêtre figée à chaque changement de worktree.
`Watcher::watch` n'envoie qu'un ordre à un thread dédié et rend la main
immédiatement. Corollaire à connaître : la surveillance n'est pas effective au
retour de l'appel — sans importance, puisque la sélection d'un worktree
déclenche de toute façon une lecture du statut.

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
défiler horizontalement.

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

### Quel domaine de revue s'ouvre

`app::initial_range` choisit, au **premier** statut d'un worktree, entre les
modifications, l'index et la revue de branche : ouvrir sur un domaine vide alors que l'autre est
plein est la façon la plus sûre de faire croire que Perch ne voit rien — un
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
apparaît de quel côté. Elle est libre et testée : le statut est la source pour
les deux premiers domaines (lui seul distingue index et répertoire de travail,
et un fichier peut être des deux côtés), `--numstat` pour les deux autres, qui
parlent de commits et n'ont pas de notion d'index.

### Quel worktree s'ouvre

`runtime::open_repo` retient le checkout d'où l'ouverture vient, et non le
premier de la liste — qui est toujours le dépôt principal. Lancer `perch` dans
un worktree doit ouvrir *ce* worktree. Le worktree retenu est le plus profond
dont le chemin est un préfixe de celui demandé, faute de quoi un worktree
imbriqué dans un autre serait attribué au mauvais.

### La base de la revue de branche

Elle vient de git — `origin/HEAD`, puis `init.defaultBranch`, puis les noms
usuels *qui existent vraiment* (`branch::default_base`) — et jamais d'un nom
supposé. Un `main` codé en dur produit un `unknown revision` au premier clic
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

`SyntaxHighlighter::new` compile les requêtes de la grammaire — près de
quarante millisecondes pour JavaScript. Jamais dans un `render`, et **une seule
instance pour les deux passes** : `update` ne fait que reparser un texte, alors
qu'en créer une seconde doublait le coût fixe de chaque fichier ouvert.

### Le terminal

`alacritty_terminal` fournit le parseur VTE, la grille, l'historique et le pty.
Perch écrit deux choses : `keys::key_bytes` (frappe → octets) et
`snapshot::capture` (grille → lignes stylées). Le rendu est du texte — un
`StyledText` par ligne avec ses runs — et non un canevas : une police à chasse
fixe suffit à aligner les colonnes, et gpui garde la charge du façonnage.

Le verrou de la grille est partagé avec la boucle d'E/S : **ne jamais dessiner
sous ce verrou**, d'où l'instantané.

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
  `"Perch && !Dialog"` à `key_context` fait boucler le parseur et déborder la
  pile au premier rendu. L'expression va dans le troisième argument de
  `KeyBinding::new` ; le contexte va dans `shortcuts::context()`.
- Les raccourcis de l'application passent tous par `secondary-` : le reste du
  clavier appartient au programme qui tourne dans le terminal, et
  `key_bytes` refuse justement de transmettre la touche système au pty.
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
