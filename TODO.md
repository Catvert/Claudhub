# TODO — vers un poste de travail agentique

Claudhub sait aujourd'hui **regarder** : il liste les worktrees, montre ce qu'un
agent y a écrit, colore le diff, et donne un terminal pour lui reparler. Ce
qu'il ne sait pas faire, c'est **fermer la boucle** — annoter une relecture et
la renvoyer, intégrer le travail une fois validé, ouvrir un fichier que le diff
ne touche pas, ou partir d'un rapport d'erreur plutôt que d'une intention.
Chaque fois, il faut sortir de l'application.

Les jalons ci-dessous sont dans l'ordre où ces manques se débloquent les uns
les autres. Les références sont en `chemin:ligne`, à l'état du dépôt au
2026-08-19.

## Décisions de cadrage

Elles ne se rediscutent pas à chaque jalon.

- **`wt` devient une bibliothèque.** Le dépôt est le nôtre
  (`github.com/Catvert/wt`) : on y ajoute un `lib.rs` plutôt que de parser la
  sortie texte, alignée et localisée, de sa CLI.
- **L'IA passe par l'agent du terminal, jamais par une API depuis Claudhub.**
  Claudhub compose des prompts et les livre à un agent qui, lui, a le dépôt entre
  les mains et peut corriger. Aucune dépendance HTTP, aucune clé à garder.
  « Configurer plusieurs modèles » devient donc **configurer plusieurs profils
  d'agent**.
- **L'édition reste légère.** Retouche courte dans Claudhub, vrai travail dans
  l'éditeur externe de son choix. Claudhub ne devient pas un IDE.
- **Pas d'extensions wasm.** Le `wt.toml` d'un projet est déjà un système
  d'extension — voir la section finale.

---

## Jalon 0 — Un magasin d'état par worktree

Rien ne survit au redémarrage sinon les réglages : `ReviewState` est en
mémoire, et la base de comparaison d'un worktree — documentée comme lui étant
propre (`src/ui/app.rs:1039`) — est réapprise à chaque lancement. Les notes du
jalon 1 en ont besoin, la configuration Sentry par dépôt aussi. C'est court, et
rendre la base persistante en est le banc d'essai immédiat.

- [ ] `src/ui/store.rs`, calqué sur `src/ui/settings.rs` : un global gpui, un
      fichier `<config>/state.json` en 0600, écriture différée d'une demi-seconde
      (`load()` `settings.rs:240`, `save()` `:260`, `schedule_save` `:408`).
      `#[serde(default)]` partout, pour qu'un champ ajouté ne casse pas un
      fichier existant.

      ```rust
      struct WorktreeState { base: Option<String>, notes: Vec<Note>, collapsed: Vec<PathBuf> }
      struct RepoState     { sentry_project: Option<String> }   // rempli au jalon 5
      struct Store { worktrees: HashMap<PathBuf, WorktreeState>, repos: HashMap<PathBuf, RepoState> }
      ```

- [ ] Y persister `ReviewState::base` — l'état vivant reste en mémoire, seule
      l'écriture change. Vérifier que `set_base` (`src/ui/app.rs:1044`) la
      déclenche, et que la valeur relue l'emporte sur le `default_base` deviné
      par `Evt::Branches` (`src/runtime/protocol.rs:175`).
- [ ] Y persister `collapsed` (repli des dossiers de la liste de revue).
- [ ] Purger à l'ouverture d'un dépôt les entrées dont le worktree n'existe
      plus, en croisant avec `Repo::worktrees()`.
- [ ] `CLAUDE.md` : assumer que ce fichier est écrit **depuis le thread
      d'interface**, ce qui déroge à « `src/ui/` ne fait jamais d'E/S ». C'est
      le précédent de `settings.rs` et la même raison — quelques kilo-octets
      écrits une fois par demi-seconde ne valent pas un aller-retour par le
      protocole. La règle vise les commandes git, pas la préférence qu'on range.

**Vérification** : rouvrir Claudhub retrouve la base choisie sur chaque worktree.

---

## Jalon 1 — Annoter une relecture et la renvoyer à l'agent

La fonctionnalité la plus différenciante, et celle qui s'appuie le mieux sur
l'existant : la sélection de lignes, le rendu virtualisé et l'extraction de
code propre sont déjà écrits.

### L'ancrage — la seule partie délicate

`ReviewState::diff_selection` est un couple `(ancre, tête)` d'indices dans la
liste **affichée** (`src/ui/app.rs:215`). Il est invalidé par la bascule
unifié/deux colonnes (`src/ui/diff_view.rs:446`) et par tout rechargement du
diff (`app.rs:745`, `:945`). **Une note ne peut pas s'y accrocher.**

On retient donc des **numéros de ligne** — `DiffLine::old_no`/`new_no` existent
déjà (`src/git/diff.rs:84`) — **plus l'extrait de code**.

- [ ] Le modèle, dans `src/ui/notes.rs` :

      ```rust
      struct Note {
          id: u64,
          range: DiffRange,           // le domaine où la note a été prise
          path: PathBuf,
          side: Side,                 // Old | New : commenter du code supprimé a un sens
          start: usize, end: usize,   // numéros de ligne, jamais des indices de liste
          excerpt: String,            // le code cité, tel que `copy_text` le rend
          body: String,               // la remarque
          sent: bool, done: bool,     // prévoir la résolution dès maintenant
      }
      ```

- [ ] `notes::relocate(&Rendered, &Note) -> Anchor` — **fonction libre et
      testée**, sur le modèle de `Rendered::copy_text` (`diff_view.rs:73`).
      Au rechargement : si le texte trouvé aux numéros retenus correspond, la
      note se replace ; sinon on la cherche par son extrait dans le fichier ;
      sinon elle est marquée **décalée** et reste dans la liste. Une note perdue
      en silence est pire que pas de note du tout.
- [ ] Tests : aller-retour ancre ↔ index, ligne ajoutée (pas d'`old_no`), ligne
      supprimée (pas de `new_no`), `NoNewline`, fichier modifié sous la note.

### Les gestes

- [ ] **Annoter la sélection** : raccourci dans `src/ui/shortcuts.rs`
      (`actions!` `:13`/`:81`, `bind_keys` `:98`, sous `COPY_PREDICATE`) et
      entrée du menu contextuel du diff. Une note porte sur une **plage**, pas
      sur une ligne.
- [ ] Saisie par `InputState::multi_line(true).auto_grow(2, 8)`, dans un
      popover ancré à la sélection. **L'entité est créée une fois**, jamais dans
      un `render` — voir `commit_input` (`src/ui/app.rs:381`).
- [ ] **Marqueur en gouttière** sur les lignes annotées. `render_row`
      (`diff_view.rs:863`) et `render_split_row` (`:1045`) reçoivent déjà
      `selected: bool` ; on leur passe de même un booléen. **Il est calculé en
      amont**, dans un `Rc<Vec<bool>>` indexé par ligne, reconstruit quand les
      notes changent — jamais dans la fermeture de `uniform_list` (`:806`), qui
      tourne à chaque frame.
- [ ] Bouton dans l'en-tête de la vue diff, à côté de `copy-file` (`:717`).
- [ ] **Panneau `ClaudhubNotes`** (« Relecture ») : les notes du worktree, groupées
      par fichier, avec leur extrait. Cliquer ouvre le fichier et défile jusqu'à
      la ligne (`move_diff_selection` sait déjà défiler, `:593`). Cocher marque
      traité ; supprimer ; filtrer sur « non traitées ».

### L'envoi

- [ ] `notes::prompt(&[Note]) -> String` — **libre et testée**, c'est la pièce à
      verrouiller, le reste n'est que plomberie. `Rendered::copy_text` donne
      déjà le code propre (sans `+`/`-`, sans numéros, sans `@@`).

      ```
      Voici mes remarques de relecture sur la branche <b>.

      ## src/ui/app.rs:120-134
      ```rust
      <extrait>
      ```
      > <la remarque>
      ```

- [ ] `TerminalGroup::send_to_agent(text)` : écrit dans le pty de l'onglet agent
      (`Terminal::write`, `src/terminal/mod.rs:288`), **encadré par les séquences
      de collage entre crochets** que Claudhub gère déjà (`mod.rs:426`) — sans quoi
      un texte multiligne s'exécute ligne à ligne. S'il n'y a pas d'onglet agent,
      en ouvrir un (`open_agent`, `src/ui/terminal_view.rs:801`) et attendre que
      le programme soit prêt.
- [ ] **Le `\r` final part séparément**, après le collage, et non dans le même
      paquet : un TUI qui vient de recevoir un collage encadré peut avaler un
      retour chariot collé au bout.
- [ ] Vérifier le comportement sur plusieurs milliers de caractères.
- [ ] Trois boutons : « envoyer les notes non traitées », « envoyer cette
      note », et — sans passer par une note — **« demander à l'agent »** sur la
      sélection courante avec une question libre. C'est le geste le plus
      fréquent en pratique.
- [ ] Les notes envoyées passent à `sent`, pas à `done` : c'est la relecture de
      la réponse qui les clôt.

---

## Jalon 2 — Profils d'agent

Court, et placé ici parce qu'il prolonge directement le jalon 1 : dès qu'on
envoie du texte à un agent, on veut choisir lequel.

- [ ] `Settings::terminal.agent_command: String` (`src/ui/settings.rs:127`)
      devient `agents: Vec<AgentProfile>` + `default_agent: String`.

      ```rust
      struct AgentProfile { name: String, command: String, args: Vec<String>, env: HashMap<String, String> }
      ```

      `env` est ce qui porte le modèle (`ANTHROPIC_MODEL`, une clé par profil…) :
      `terminal::Spawn` accepte déjà un `env` (`src/terminal/mod.rs:172`).
- [ ] **Migration silencieuse** : si `agent_command` est présent et `agents`
      vide, en faire un profil. `#[serde(default)]` (`settings.rs:116`, `:168`)
      rend l'opération sans risque.
- [ ] **Correctif au passage** : `open_agent` découpe la commande par
      `split_whitespace()` (`terminal_view.rs:806`), donc sans guillemets ni
      échappement — un chemin contenant une espace casse. Le profil porte
      `command` et `args` séparément, le problème disparaît. Même défaut dans
      `TerminalSettings::program` (`settings.rs:151`).
- [ ] `Cmd::ScanAgents { program }` (`src/runtime/protocol.rs:55`) prend
      désormais **la liste** des noms de programme de tous les profils — sinon
      la détection dans `/proc` (`src/agent.rs:47`) ne voit plus qu'un agent sur
      deux. `AgentState` (`app.rs:270`) dit *quel* agent tourne, pas seulement
      combien.
- [ ] Le menu « + » du groupe de terminaux (`terminal_view.rs:908`) liste les
      profils. Le formulaire de réglages reçoit une table de profils, via
      `SettingField::render` qui autorise un champ sur mesure
      (`settings_view.rs:104`).

---

## Jalon 3 — Worktrees : `wt` en bibliothèque, et l'intégration du travail

### `wt` comme dépendance

- [ ] Dans `/home/finch/Projets/wt` : ajouter `src/lib.rs` exportant `config`,
      `git`, `state`, `ops`, `tmpl`, `util`, et laisser `[[bin]]` n'être qu'un
      appelant mince. Le cœur est déjà découplé de l'interface — `ops.rs` le
      revendique — donc l'opération est mécanique.
- [ ] Claudhub en dépend (`path` pendant le développement, `git =` ensuite).
      `src/wt.rs` côté Claudhub, couche métier sans gpui, lit le `wt.toml` du dépôt.
- [ ] **Tout appel à `ops::App` part dans un worker** : il lance des hooks shell
      et peut durer des minutes.

### Ce que ça donne — et c'est là le système de plugins

Le `wt.toml` d'un projet **ajoute des actions à Claudhub sans que Claudhub les
connaisse**.

- [ ] **Création guidée** : le dialogue de nouveau worktree
      (`src/ui/sidebar.rs:433`) propose le slug, la branche selon le template du
      projet, la base, et pose les `[[prompt]]` déclarés (`choice` / `multi` /
      `confirm` / `text`, avec `source` dynamique et `when` conditionnel) —
      la partie de `wt` la plus directement transposable en dialogue gpui.
      Puis `wt` fait les `dirs`, les `[[copy]]` (hardlink pour `vendor/`, copie
      pour le `.env`), les ports, et `post_new`.
- [ ] **Menu contextuel d'un worktree** : `up`, `down`, `rm`, et **les
      `[tasks.*]` du projet, listées dynamiquement**.
- [ ] **Exécution dans un onglet terminal**, pas dans un panneau de sortie : les
      hooks sont interactifs, colorés, parfois longs — `TerminalGroup::open`
      (`terminal_view.rs:756`) accepte déjà une commande arbitraire et donne
      tout cela gratuitement. Garder `ops::App::set_sink` pour les opérations
      non interactives dont on ne veut qu'un toast.
- [ ] **Statut et URLs** : `[status] up` et `[open]` alimentent la barre latérale
      et un bouton « ouvrir ». Ce sont des commandes shell : **file de fond
      uniquement** (`is_background`), période longue, jamais devant un diff
      demandé.

### Intégrer un worktree — le vrai manque

Aucune commande de merge n'existe dans `src/git/`.

- [ ] `git/repo.rs` : `merge`, `rebase`, `merge_abort`, `merge_continue`, et les
      `Cmd`/`Evt`/`Action` correspondants.
- [ ] **« Mettre à jour depuis la base »** dans le worktree — la base a avancé
      pendant que l'agent travaillait. Merge ou rebase selon un réglage.
- [ ] **« Intégrer dans `<base>` »** — exécuté **depuis le dépôt principal**,
      après avoir vérifié qu'il est propre et positionné sur la base ; sinon on
      le dit et on refuse. Puis proposer d'enchaîner `wt rm` et la suppression
      de la branche — `wt` conserve délibérément la branche, c'est donc à Claudhub
      de poser la question.
- [ ] **Conflits** : `Status` connaît déjà l'état `Unmerged`
      (`src/git/status.rs:29`), il n'est qu'affiché. Ajouter un domaine de revue
      « Conflits », la liste des fichiers concernés, et par fichier les trois
      seules actions qui tiennent en peu de code : garder le nôtre, garder le
      leur, ouvrir dans l'éditeur (jalon 4), puis marquer résolu.
      **Une vue à trois volets n'est pas promise ici** — c'est un projet en soi,
      et le dire évite de le découvrir à mi-parcours.
- [ ] **Garde-fou** : un merge interrompu laisse le dépôt à mi-chemin. Tant
      qu'il dure, la barre d'état l'affiche et propose Continuer / Abandonner —
      sans quoi l'utilisateur se retrouve dans un état que Claudhub ne nomme pas.

---

## Jalon 4 — Explorateur de projet, lecture et retouche

### L'arbre

- [ ] `git ls-files --cached --others --exclude-standard` donne en **un seul
      appel** tous les fichiers suivis et non ignorés — c'est déjà ce que fait
      `src/runtime/watch.rs` pour décider quoi surveiller. Bâtir l'arbre en
      mémoire à partir de cette liste : pas d'E/S par dossier, et un projet
      Laravel de quarante mille répertoires ne coûte rien.
- [ ] Bascule « montrer aussi les fichiers ignorés », qui demande elle une
      lecture de disque paresseuse (`Cmd::ListDir { path }`, un niveau à la
      fois, depuis le worker) — en second temps.
- [ ] **Généraliser `review::tree_rows`** (`src/ui/review.rs:1021`) plutôt
      qu'adopter `gpui_component::tree` : la fusion des dossiers à enfant unique
      (`:1097`), le repli, l'indentation et la virtualisation par `uniform_list`
      (`:243`) sont déjà écrits, testés (`:1220`–`:1290`) et cohérents avec la
      liste de revue. Le travail est de paramétrer `Node`/`emit` sur le type de
      feuille, aujourd'hui soudé à `FileRow`.
- [ ] Panneau `ClaudhubFiles`. Bonus quasi gratuit une fois l'arbre généralisé :
      afficher le statut git, puisque `ReviewState::status` est déjà là.
- [ ] Actions : ouvrir, renommer, supprimer, nouveau fichier/dossier, copier le
      chemin. Confirmation pour les destructions — modèle : `confirm_removal`
      (`review.rs:487`).

### Lire et retoucher

- [ ] `Cmd::ReadFile { worktree, path }` → `Evt::FileContent`, et
      `Cmd::WriteFile { worktree, path, content, expect: Option<Hash> }`.
- [ ] La vue utilise **`InputState::code_editor(language).line_number(true)`** de
      gpui-component 0.5.1 : coloration, auto-indentation, numéros de ligne,
      recherche, jusqu'à 50 K lignes. Le langage se déduit de l'extension —
      `src/ui/highlight.rs:42` porte déjà cette table, PHP y est enregistré à la
      main.
- [ ] Au-delà de la limite documentée, refuser d'ouvrir et proposer l'éditeur
      externe, plutôt que de figer la fenêtre. Détecter et refuser les binaires
      (cf. `FileDiff::binary`, `src/git/diff.rs:106`).
- [ ] **Écriture concurrente** : un agent écrit dans les mêmes fichiers pendant
      qu'on les lit. `expect` porte le hachage lu ; si le fichier a changé
      depuis, l'écriture est refusée et Claudhub le dit. C'est la seule façon de ne
      pas effacer le travail d'un agent avec une correction de faute de frappe.
- [ ] Indicateur de modification non enregistrée, confirmation à la fermeture.
      Le worktree étant surveillé, la sauvegarde rafraîchit le diff toute seule.

### Éditeur externe

- [ ] Réglage `external_editor` : une commande avec `{path}` et `{line}`,
      lancée par un worker (`Cmd::OpenExternal`) — jamais depuis le thread
      d'interface. Découpage honorant les guillemets (cf. jalon 2).
- [ ] Préréglages : VS Code (`code -g {path}:{line}`), PhpStorm
      (`phpstorm --line {line} {path}`), Zed (`zed {path}:{line}`), et « dans un
      onglet terminal » pour `nvim`/`helix`, qui ne coûte rien puisque
      `TerminalGroup::open` accepte une commande arbitraire et travaille déjà
      dans le bon répertoire.
- [ ] Le geste doit exister **depuis une ligne de diff**, avec son numéro de
      ligne (`src/ui/diff_view.rs:665`) : c'est le cas d'usage réel — on relit,
      quelque chose cloche, on l'ouvre là où c'est.

---

## Jalon 5 — Sentry

Lire les issues d'un projet, les rapprocher du code, et en faire un point de
départ pour un agent. Claudhub n'envoie aucun événement à Sentry. En dernier parce
que le clic « aller à la frame fautive » exige le jalon 4.

- [ ] **HTTP** : déclarer `ureq` (bloquant, `rustls`, pas de tokio) — il épouse
      le modèle à threads du runtime. `zed-reqwest` traîne déjà dans l'arbre via
      gpui-component, mais en dépendre transitivement serait fragile.
- [ ] **Configuration** : `sentry.org` dans les réglages ; le **projet dépend du
      dépôt**, donc il va dans `Store::repos` (jalon 0). Le jeton se lit
      **d'abord dans `SENTRY_TOKEN`**, et n'est écrit dans le fichier de réglages
      qu'à défaut — le fichier est en 0600, ce qui ne fait pas de lui un coffre.
      À dire dans le README.
- [ ] `Cmd::LoadIssues { org, project, query }` → `Evt::Issues`, sur la **file
      réseau** (celle de fetch/pull/push) : une API distante met parfois
      plusieurs secondes et ne doit pas occuper un worker de lecture.
- [ ] **Panneau `ClaudhubSentry`** : les issues (titre, culprit, occurrences,
      dernière vue, niveau) ; la sélection montre la trace, chaque frame
      `fichier:ligne` cliquable vers l'explorateur.
- [ ] **« Confier à un agent »** — compose un prompt avec le titre, le message,
      la trace et le code autour des frames `in_app`, puis le livre par le canal
      du jalon 1.
- [ ] **« Ouvrir un worktree pour cette issue »** — `wt new sentry-<id>`
      (jalon 3), puis l'agent y démarre avec ce prompt. C'est la boucle
      complète : du rapport d'erreur au worktree relu.
- [ ] Parsing testé sur fixture JSON, comme tous les formats que nous parsons.

---

## Sur la notion de « plugin »

À trancher dans `CLAUDE.md`, sinon la question revient à chaque intégration.
Trois niveaux, du moins cher au plus cher :

1. **Le `wt.toml` du projet** — tâches, prompts, statuts, URLs. Claudhub les
   affiche sans les connaître. C'est le vrai système d'extension, et il ne
   coûte que le jalon 3.
2. **Des commandes déclarées dans les réglages de Claudhub** (nom, commande, où
   l'exécuter, sortie en terminal ou en toast), pour ce qui n'est pas propre à
   un projet.
3. **Des extensions wasm, à la Zed — écarté.** Rien dans les besoins listés ne
   le demande, et le coût est sans commune mesure.

`wt` et Sentry sont donc des modules compilés, pas des greffons : les traiter
autrement ferait payer un mécanisme générique pour deux cas.

---

## Rappels qui valent pour chaque jalon

- Seule `src/ui/` connaît gpui. Aucune commande, aucun accès disque, aucun
  appel réseau depuis un `render` ou un gestionnaire de clic — tout passe par
  `Cmd`/`Evt`.
- Ajouter une capacité = une variante de `Cmd`, un bras dans `runtime::handle`,
  une ou plusieurs variantes d'`Evt`, un bras dans `ClaudhubApp::handle_event`, le
  plus souvent une variante d'`Action` pour le message de succès.
- Ajouter un panneau = `render_xxx` sur `ClaudhubApp`, une ligne dans la macro
  `panels!`, la même dans `declare!` de `panels::register`, les clés i18n,
  l'insertion dans `install_default_layout`, et **incrémenter `LAYOUT_VERSION`**
  (`src/ui/app.rs:46`) — sans quoi les dispositions enregistrées reviennent avec
  des cadres vides.
- Toute chaîne visible passe par `tr!`, avec ses deux clés dans
  `assets/i18n/{fr,en}.json` — `ui::i18n_tests` verrouille la parité.
- Ce qui se déduit d'un diff se calcule une fois, à l'arrivée, dans
  `diff_view::Rendered` derrière un `Rc`. Les fermetures de rendu ne calculent
  rien.
- `CLAUDE.md` se met à jour dans le commit qui change la structure.
- `just fmt`, `just clippy` (`-D warnings`), `just test` doivent passer en
  permanence.

## Vérification manuelle, sur un dépôt jetable

- Annoter trois lignes, envoyer, vérifier que le collage n'exécute rien tout
  seul.
- Créer un worktree via `wt` avec ses hooks et son `.env`.
- Provoquer un conflit et l'intégrer.
- Ouvrir un fichier non modifié, le retoucher, vérifier que le diff se
  rafraîchit.
- Laisser un agent écrire dans un fichier ouvert et vérifier que l'écriture est
  refusée.

## Dette relevée en chemin

- [x] `split_whitespace()` sur des commandes utilisateur casse sur tout chemin
      contenant une espace. Corrigé pour l'agent **et** pour le shell :
      `settings::split_command` honore les guillemets, et les deux appelants y
      passent.
- [x] `Cargo.toml` renvoie à `src/ui/terminal_element.rs`, qui n'existe pas : le
      rendu est dans `src/ui/terminal_view.rs`.
- [x] Le README annonce encore « écran de préférences » parmi les manques,
      alors que `src/ui/settings_view.rs` existe.
