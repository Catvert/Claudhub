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
    watch.rs    surveillance de fichiers (notify), debounce 250 ms
  terminal/     émulation
    mod.rs      pty + `Term` alacritty derrière un `FairMutex`
    snapshot.rs grille → lignes et runs de style, sans tenir le verrou
    keys.rs     frappe gpui → octets (séquences xterm)
  ui/           tout gpui
    mod.rs      `run()`, `AssetSource`, polices, i18n
    app.rs      `PerchApp` : l'état, la pompe d'événements, le chrome
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

### Le terminal

`alacritty_terminal` fournit le parseur VTE, la grille, l'historique et le pty.
Perch écrit deux choses : `keys::key_bytes` (frappe → octets) et
`snapshot::capture` (grille → lignes stylées). Le rendu est du texte — un
`StyledText` par ligne avec ses runs — et non un canevas : une police à chasse
fixe suffit à aligner les colonnes, et gpui garde la charge du façonnage.

Le verrou de la grille est partagé avec la boucle d'E/S : **ne jamais dessiner
sous ce verrou**, d'où l'instantané.

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
