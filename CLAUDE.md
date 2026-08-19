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
    theme.rs / shortcuts.rs / icons.rs
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

**Deux colonnes, une seule référence.** La vue côte à côte (`SplitRow`,
`split_rows`) n'est qu'un autre agencement de la liste unifiée : ses entrées ne
portent que des **indices dans `rows`**, jamais du texte ni des styles à elles.
La copie ramène donc toujours la sélection à la liste unifiée
(`unified_span`), qui seule porte l'ordre du fichier — appariées, une
suppression et l'ajout qui lui répond tiennent sur une même entrée, et il faut
bien décider laquelle vient d'abord. Conséquence à ne pas oublier : les indices
de `diff_selection` désignent la liste **affichée**, donc basculer de mode
abandonne la sélection.

Chaque colonne est taillée pour la plus longue ligne du fichier, et non pour la
moitié de la vue : les tailler à la vue couperait le code ou le renverrait à la
ligne, alors qu'un défilement horizontal emmène les deux colonnes ensemble et
garde les versions en regard.

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
