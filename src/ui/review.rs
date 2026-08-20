//! La revue : liste des fichiers touchés, et le diff du fichier choisi.
//!
//! Quatre domaines de comparaison, choisis par les onglets en tête de liste :
//! les modifications non indexées, l'index, tout le checkout contre HEAD, et
//! la branche entière depuis sa divergence d'avec sa base. Le dernier est
//! celui qui sert à relire le travail d'un agent avant de le pousser.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::{div, prelude::*, px, uniform_list, Context, Focusable, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::Textarea,
    select::Select,
    separator::Separator as Divider,
    v_flex, ActiveTheme, Disableable, Sizable, WindowExt,
};

use crate::git::{DiffFile, DiffRange, Status, StatusCode};
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::theme::{status_color, DiffColors};

/// Une entrée de la liste des modifications.
///
/// Les fichiers sont groupés comme dans les clients qui masquent l'index :
/// ce qui est suivi d'un côté, ce qui ne l'est pas encore de l'autre. Le
/// groupe porte sa propre case, qui indexe ou dés-indexe tout d'un coup.
#[derive(Clone)]
enum Row {
    Group(Group),
    /// Un dossier de l'arborescence, repliable.
    Dir(DirRow),
    File(FileRow),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    /// Fichiers que git suit déjà.
    Tracked,
    /// Fichiers jamais ajoutés. Les cocher, c'est les faire suivre.
    Untracked,
}

/// Un dossier dans la liste des modifications.
///
/// Les dossiers intermédiaires vides sont fusionnés avec leur unique enfant :
/// `app/Http/Livewire/Forms` tient sur une ligne au lieu de quatre, et c'est
/// ce qui rend l'arborescence lisible sur un projet Laravel ou Symfony, où
/// l'on descend de six niveaux avant de trouver un fichier.
#[derive(Clone)]
struct DirRow {
    /// Chemin complet, et clé du repli. C'est celui du dossier le plus profond
    /// de la chaîne fusionnée : replier `app/Http` et replier
    /// `app/Http/Livewire` sont deux gestes différents, mais une chaîne
    /// fusionnée n'en offre qu'un.
    path: PathBuf,
    /// Ce qui s'affiche : un segment, ou la chaîne fusionnée.
    label: String,
    depth: usize,
    collapsed: bool,
    /// Tous les fichiers du sous-arbre, y compris ceux qu'un repli cache :
    /// cocher un dossier fermé doit indexer ce qu'il contient, et non ce qu'on
    /// en voit.
    paths: Vec<PathBuf>,
    /// Vrai quand tout le sous-arbre est déjà indexé.
    staged: bool,
    /// Vrai quand tout le sous-arbre est déjà relu — c'est ce qui fait d'un
    /// clic sur un dossier une relecture de tout ce qu'il contient.
    reviewed: bool,
    added: usize,
    removed: usize,
}

#[derive(Clone)]
struct FileRow {
    path: PathBuf,
    /// Profondeur dans l'arborescence. Nulle en liste plate.
    depth: usize,
    name: String,
    directory: String,
    /// Les deux codes de git, celui de l'index puis celui du répertoire de
    /// travail : c'est l'information exacte, et elle tient en deux caractères
    /// là où une seule case à cocher devrait mentir sur les fichiers
    /// partiellement indexés.
    index: StatusCode,
    worktree: StatusCode,
    added: usize,
    removed: usize,
    /// Ce fichier ira dans le prochain commit, au moins en partie.
    staged: bool,
    untracked: bool,
    /// On l'a marqué relu, et il n'a pas changé depuis.
    reviewed: bool,
}

impl FileRow {
    /// Une partie seulement du fichier est indexée : ce que git écrit `MM`.
    fn partial(&self) -> bool {
        self.staged && !matches!(self.worktree, StatusCode::Unmodified)
    }

    fn codes(&self) -> String {
        let index = self.index.letter();
        let worktree = self.worktree.letter();
        if self.untracked {
            "?".into()
        } else if index.trim().is_empty() {
            worktree.to_string()
        } else if worktree.trim().is_empty() {
            index.to_string()
        } else {
            format!("{index}{worktree}")
        }
    }
}

impl ClaudhubApp {
    /// La liste des modifications en cours, et de quoi les valider.
    pub(super) fn render_changes(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.render_file_list(DiffRange::Working, window, cx)
    }

    /// La revue de branche : ce que la branche a écrit depuis sa base.
    ///
    /// Tant que la base est inconnue — dépôt sans branche d'intégration, ou
    /// branche déployée ici qui n'aurait rien à se comparer — le panneau le
    /// dit plutôt que d'afficher une liste vide qu'on croirait fausse.
    pub(super) fn render_branch_review(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match self.active_review().and_then(|state| state.base.clone()) {
            Some(base) => self
                .render_file_list(DiffRange::Branch { base }, window, cx)
                .into_any_element(),
            None => v_flex()
                .size_full()
                .child(self.render_base_bar(cx))
                .child(
                    div()
                        .p_3()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("range-branch-none")),
                )
                .into_any_element(),
        }
    }

    pub(super) fn render_file_list(
        &mut self,
        range: DiffRange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(worktree) = self.active.clone() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("no-worktree")),
                )
                .into_any_element();
        };

        // Deux panneaux affichent cette liste en même temps : chacun a sa
        // recherche, sans quoi filtrer les modifications filtrerait aussi la
        // revue de branche.
        let pane = if matches!(range, DiffRange::Working) {
            crate::ui::find::Pane::Changes
        } else {
            crate::ui::find::Pane::Branch
        };
        let find = self.render_find(pane, cx);
        let query = self.query(pane, cx);
        // C'est le panneau qui demande sa liste : lui seul sait ce qu'il
        // affiche, et charger les deux domaines d'avance coûterait une
        // commande pour un onglet que personne n'ouvrira.
        self.ensure_files(range.clone(), cx);
        // Prise avant tout emprunt de l'état : c'est la vue qui la détient, et
        // la liste ne fait que s'y accrocher.
        let scroll = self.file_scroll(&range);
        let Some(state) = self.review.get(&worktree) else {
            return div().into_any_element();
        };
        let selected = state.selected.clone();
        let collapsed = state.collapsed.clone();
        // La liste plate reste la référence : c'est elle qui compte ce qui est
        // indexé et qui donne à la case d'un groupe les fichiers sur lesquels
        // agir, y compris ceux qu'un dossier replié cache.
        // Le filtre s'applique à la liste plate, avant l'arborescence : c'est
        // elle la référence, et un dossier dont plus rien ne reste doit
        // disparaître avec ses fichiers.
        let flat: Vec<Row> = self
            .rows(&range, cx)
            .into_iter()
            .filter(|row| match row {
                Row::File(file) => crate::ui::find::matches(&query, &file.path.to_string_lossy()),
                _ => true,
            })
            .collect();
        let staged_count = flat
            .iter()
            .filter(|row| matches!(row, Row::File(file) if file.staged))
            .count();
        // Pendant une recherche, les replis sont ignorés : un fichier trouvé
        // dans un dossier fermé ne se verrait pas, et la recherche paraîtrait
        // n'avoir rien trouvé.
        let rows = if crate::ui::settings::Settings::global(cx).review_tree {
            if query.trim().is_empty() {
                tree_rows(&flat, &collapsed)
            } else {
                tree_rows(&flat, &HashSet::new())
            }
        } else {
            flat.clone()
        };
        let can_commit = staged_count > 0;
        let commits = matches!(range, DiffRange::Working);
        // Deux listes vivent côte à côte : elles ne peuvent pas porter le même
        // identifiant, sans quoi elles partageraient leur défilement.
        let list_id = match &range {
            DiffRange::Working => "working".to_string(),
            DiffRange::Branch { base } => format!("branch-{base}"),
            DiffRange::Commit { id, .. } => format!("commit-{id}"),
        };

        // Sans filet droit : c'était la couture avec le diff voisin, du temps
        // où les panneaux se touchaient — la gouttière les sépare désormais.
        v_flex()
            .size_full()
            .when(!matches!(range, DiffRange::Working), |el| {
                el.child(self.render_base_bar(cx))
            })
            .when(matches!(range, DiffRange::Working), |el| {
                el.child(self.render_changes_bar(cx))
            })
            .children(find)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(rows.is_empty(), |el| {
                        el.child(
                            div()
                                .p_3()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr!("review-clean")),
                        )
                    })
                    // Liste virtualisée : une revue de branche touche couramment
                    // plusieurs centaines de fichiers, et reconstruire autant de
                    // lignes — chacune avec ses deux boutons — à chaque frame suffit
                    // à faire tomber l'interface à quelques images par seconde.
                    .when(!rows.is_empty(), |el| {
                        let rows = std::rc::Rc::new(rows);
                        let flat = std::rc::Rc::new(flat);
                        let entity = cx.entity();
                        let colors = DiffColors::of(cx);
                        let count = rows.len();
                        // Seules les modifications en cours se cochent : sur un
                        // commit déjà écrit, il n'y a rien à indexer.
                        let checkable = matches!(range, DiffRange::Working);
                        let row_range = range.clone();
                        el.child(
                            self.scrolled(
                                gpui::SharedString::from(format!("file-bar-{}", list_id)),
                                &scroll,
                                crate::ui::motion::Axes::Vertical,
                                window,
                                uniform_list(
                                    gpui::SharedString::from(format!("file-list-{}", list_id)),
                                    count,
                                    move |visible, _window, cx| {
                                        visible
                                            .map(|ix| {
                                                render_row(
                                                    &rows,
                                                    &flat,
                                                    ix,
                                                    &worktree,
                                                    &row_range,
                                                    selected.as_deref(),
                                                    &colors,
                                                    checkable,
                                                    &entity,
                                                    cx,
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                )
                                .size_full()
                                // Le retrait des lignes est ici et non sur
                                // elles : `uniform_list` pose ses entrées à la
                                // taille qu'il calcule, et une marge sur une
                                // entrée est ignorée. C'est ce retrait qui
                                // laisse les fonds arrondis respirer au lieu
                                // de traverser le panneau d'un bord à l'autre.
                                .px_1()
                                .track_scroll(&scroll.clone()),
                                cx,
                            ),
                        )
                    }),
            )
            .when(commits, |el| {
                el.child(self.render_commit_box(can_commit, staged_count, cx))
            })
            .into_any_element()
    }

    /// Les entrées de la liste d'un domaine.
    ///
    /// Le statut est la source pour les modifications en cours — lui seul
    /// distingue index et répertoire de travail — et `--numstat` pour les
    /// domaines qui portent sur des commits et n'ont pas de notion d'index.
    fn rows(&self, range: &DiffRange, _cx: &Context<Self>) -> Vec<Row> {
        let Some(state) = self.active_review() else {
            return Vec::new();
        };
        let files = state.files.get(range).map(Vec::as_slice).unwrap_or(&[]);
        rows_for(range, &state.status, files, &state.reviewed)
    }

    /// Les fichiers de la liste, dans l'ordre où ils s'affichent.
    ///
    /// L'ordre affiché et non l'ordre brut : un dossier replié cache ses
    /// fichiers, et les flèches ne doivent pas ouvrir un fichier que la liste
    /// ne montre pas — le suivant serait alors introuvable à l'œil.
    fn visible_files(&self, range: &DiffRange, cx: &Context<Self>) -> Vec<PathBuf> {
        self.visible_rows(range, cx)
            .into_iter()
            .filter_map(|row| match row {
                Row::File(file) => Some(file.path),
                _ => None,
            })
            .collect()
    }

    /// Les entrées telles que la liste les affiche, dossiers compris.
    fn visible_rows(&self, range: &DiffRange, cx: &Context<Self>) -> Vec<Row> {
        let Some(state) = self.active_review() else {
            return Vec::new();
        };
        let flat = self.rows(range, cx);
        if crate::ui::settings::Settings::global(cx).review_tree {
            tree_rows(&flat, &state.collapsed)
        } else {
            flat
        }
    }

    /// Amène la liste sur un fichier.
    ///
    /// L'indice est celui de la liste **affichée** — dossiers compris, et sans
    /// ce que les replis cachent : c'est cette liste-là que la vue virtualise,
    /// et un indice pris ailleurs désignerait une autre ligne.
    pub(super) fn reveal_file(&mut self, range: &DiffRange, path: &Path, cx: &mut Context<Self>) {
        let Some(index) = self
            .visible_rows(range, cx)
            .iter()
            .position(|row| matches!(row, Row::File(file) if file.path == path))
        else {
            return;
        };
        self.file_scroll(range)
            .scroll_to_item(index, gpui::ScrollStrategy::Center);
    }

    /// Ouvre le fichier précédent ou suivant de la liste.
    ///
    /// Aux extrémités, rien ne se passe : boucler ferait recommencer une revue
    /// qu'on vient de finir sans que rien ne le signale.
    pub(super) fn step_file(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(range) = self.review.get(&worktree).map(|state| state.range.clone()) else {
            return;
        };
        let files = self.visible_files(&range, cx);
        let Some(state) = self.review.get(&worktree) else {
            return;
        };
        let current = state
            .selected
            .as_ref()
            .and_then(|path| files.iter().position(|file| file == path));
        let Some(index) = step_index(current, delta, files.len()) else {
            return;
        };
        let Some(path) = files.get(index).cloned() else {
            return;
        };
        self.open_file(worktree, path, range, cx);
    }

    /// La barre du panneau des modifications : ce qu'on fait au dépôt, puis la
    /// bascule d'affichage.
    ///
    /// `fetch`, `pull` et `push` vivent ici et non dans la barre d'outils de la
    /// fenêtre : ce sont les gestes de ce panneau-là — on regarde ce qui a
    /// changé, on coche, on valide, on pousse —, et les tenir à l'autre bout
    /// de l'écran faisait traverser la fenêtre pour terminer une phrase
    /// commencée en bas.
    fn render_changes_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_active = self.active.is_some();
        // L'avance et le retard sur l'amont, tels que le statut les rapporte.
        // Ils sont **sur les boutons** et pas seulement dans la barre d'état :
        // ce sont eux qui disent lequel des deux gestes il y a à faire, et un
        // bouton éteint dit qu'il n'y a rien à faire — ce qui est la moitié de
        // l'information qu'on cherche en arrivant sur un worktree.
        let (ahead, behind) = self
            .active_review()
            .map(|state| (state.status.ahead, state.status.behind))
            .unwrap_or((0, 0));
        self.bar(cx)
            .child(
                Button::new("fetch")
                    .ghost()
                    .xsmall()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-fetch"))
                    .disabled(!has_active)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(worktree) = this.active.clone() {
                            this.git.send(Cmd::Fetch { worktree });
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("pull")
                    .ghost()
                    .xsmall()
                    .icon(icon("arrow-down-to-line"))
                    .tooltip(if behind > 0 {
                        tr!("action-pull-behind", { count: behind })
                    } else {
                        tr!("action-pull")
                    })
                    .when(behind > 0, |el| el.primary().label(behind.to_string()))
                    .disabled(!has_active)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(worktree) = this.active.clone() {
                            this.git.send(Cmd::Pull { worktree });
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("push")
                    .ghost()
                    .xsmall()
                    .icon(icon("arrow-up-from-line"))
                    .tooltip(if ahead > 0 {
                        tr!("action-push-ahead", { count: ahead })
                    } else {
                        tr!("action-push")
                    })
                    .when(ahead > 0, |el| el.primary().label(ahead.to_string()))
                    .disabled(!has_active)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(worktree) = this.active.clone() {
                            this.git.send(Cmd::Push {
                                worktree,
                                force_with_lease: false,
                            });
                        }
                        cx.notify();
                    })),
            )
            .child(Divider::vertical().h(px(12.)))
            .child(self.tree_toggle(cx))
    }

    /// La barre de la revue de branche : la bascule, et le choix de la base.
    ///
    /// La branche d'intégration devinée par git est un point de départ, pas une
    /// fatalité — on compare aussi bien à `dev`, à une autre branche de travail
    /// ou à une distante.
    fn render_base_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        self.bar(cx).child(self.tree_toggle(cx)).child(
            Select::new(&self.base_select)
                .xsmall()
                .title_prefix(tr!("range-base-prefix"))
                .placeholder(tr!("range-base-placeholder"))
                .menu_width(crate::ui::base_select::MENU_WIDTH),
        )
    }

    fn bar(&self, cx: &mut Context<Self>) -> gpui::Div {
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_1()
            .gap_1()
            .items_center()
            .justify_end()
            .border_b_1()
            .border_color(cx.theme().border)
    }

    fn tree_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tree = crate::ui::settings::Settings::global(cx).review_tree;
        Button::new("review-tree")
            .ghost()
            .xsmall()
            .icon(icon(if tree { "list-tree" } else { "list" }))
            .tooltip(if tree {
                tr!("review-as-list")
            } else {
                tr!("review-as-tree")
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_review_tree(cx)))
    }

    fn render_commit_box(
        &self,
        can_commit: bool,
        staged: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .w_full()
            .p_2()
            .gap_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(Textarea::new(&self.commit_input).h(px(64.)))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("commit-staged-count", { count: staged })),
                    )
                    .children(self.suggest_button(can_commit, cx))
                    .child(
                        Button::new("commit")
                            .primary()
                            .xsmall()
                            .icon(icon("git-commit-horizontal"))
                            .label(tr!("action-commit"))
                            .disabled(!can_commit)
                            .on_click(cx.listener(|this, _, _, cx| this.commit(false, cx))),
                    ),
            )
    }

    /// Le bouton qui demande un message à l'agent.
    ///
    /// Il n'existe pas quand le réglage est vide : proposer un geste qui
    /// échouera faute de commande vaut moins que de ne rien proposer. Il
    /// tourne pendant l'attente — l'agent met dix à trente secondes, et un
    /// bouton qui ne dit rien pendant ce temps-là se clique trois fois.
    fn suggest_button(&self, can_commit: bool, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let command = crate::ui::settings::Settings::global(cx)
            .commit_message_command
            .clone();
        if command.trim().is_empty() {
            return None;
        }
        let waiting = self.suggesting_message.is_some();
        Some(
            Button::new("commit-suggest")
                .ghost()
                .xsmall()
                .icon(icon(if waiting { "loader-circle" } else { "sparkles" }))
                .tooltip(tr!("commit-suggest"))
                .disabled(!can_commit || waiting)
                .on_click(cx.listener(|this, _, _, cx| this.suggest_commit_message(cx))),
        )
    }

    /// Demande à l'agent un message pour ce qui est indexé.
    ///
    /// La commande part dans un worker comme tout le reste : `claude -p` met
    /// dix à trente secondes, et les attendre depuis un gestionnaire de clic
    /// figerait la fenêtre pour toute la durée.
    pub(super) fn suggest_commit_message(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if self.suggesting_message.is_some() {
            return;
        }
        self.suggesting_message = Some(worktree.clone());
        self.git.send(Cmd::SuggestMessage { worktree });
        self.announce(tr!("commit-suggest-running"), cx);
        cx.notify();
    }

    /// Bascule entre l'arborescence et la liste plate.
    ///
    /// Le choix est global et persistant : c'est une habitude de lecture, pas
    /// une décision qu'on reprend par worktree.
    pub(super) fn toggle_review_tree(&mut self, cx: &mut Context<Self>) {
        crate::ui::settings::Settings::update_global(cx, |s| s.review_tree = !s.review_tree);
        cx.notify();
    }

    /// Replie ou déplie un dossier de la liste.
    pub(super) fn toggle_directory(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(state) = self.active_review_mut() else {
            return;
        };
        if !state.collapsed.remove(&path) {
            state.collapsed.insert(path);
        }
        if let Some(worktree) = self.active_path() {
            self.persist_review(&worktree, cx);
        }
        cx.notify();
    }

    /// Coche ou décoche des fichiers, c'est-à-dire les indexe ou les retire de
    /// l'index. C'est le seul geste d'indexation que l'interface propose : la
    /// case remplace les deux listes que git distingue.
    pub(super) fn set_staged(
        &mut self,
        worktree: PathBuf,
        paths: Vec<PathBuf>,
        staged: bool,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        self.git.send(if staged {
            Cmd::Stage { worktree, paths }
        } else {
            Cmd::Unstage { worktree, paths }
        });
        cx.notify();
    }

    /// Valide ce qui est dans l'index. `amend` reprend le commit précédent.
    pub(super) fn commit(&mut self, amend: bool, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let message = self.commit_input.read(cx).value().to_string();
        if message.trim().is_empty() && !amend {
            return;
        }
        self.git.send(Cmd::Commit {
            worktree,
            message,
            amend,
            all: false,
        });
        cx.notify();
    }
}

impl ClaudhubApp {
    /// Demande confirmation avant de jeter des modifications.
    ///
    /// Seule action de Claudhub qui détruit du travail sans que git en garde une
    /// copie : ni `reflog` ni `stash` ne rattrapent un `restore --worktree`.
    /// D'où le dialogue, même si tout le reste de l'interface agit au clic.
    fn confirm_removal(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        untracked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = path.display().to_string();
        let entity = cx.entity();
        let (title, warning) = if untracked {
            (tr!("delete-title"), tr!("delete-warning"))
        } else {
            (tr!("discard-title"), tr!("discard-warning"))
        };
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (worktree, path, entity) = (worktree.clone(), path.clone(), entity.clone());
            dialog
                .title(title.clone())
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(label.clone()))
                        .child(div().text_xs().child(warning.clone())),
                )
                .overlay_closable(false)
                .close_button(false)
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        let paths = vec![path.clone()];
                        let worktree = worktree.clone();
                        this.git.send(if untracked {
                            Cmd::Delete { worktree, paths }
                        } else {
                            Cmd::Discard { worktree, paths }
                        });
                        cx.notify();
                    });
                    true
                })
        });
    }

    pub(super) fn apply_hunk(&mut self, patch: String, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.git.send(Cmd::ApplyHunk {
            worktree,
            patch,
            reverse: false,
        });
        cx.notify();
    }
}

/// Rend une ligne de la liste : un en-tête de groupe ou un fichier.
///
/// Fonction libre parce que la fermeture d'une liste virtualisée ne reçoit pas
/// la vue : elle capture l'entité et repasse par `update` pour agir, comme le
/// font les gestionnaires de dialogue.
#[allow(clippy::too_many_arguments)]
fn render_row(
    rows: &std::rc::Rc<Vec<Row>>,
    flat: &std::rc::Rc<Vec<Row>>,
    index: usize,
    worktree: &Path,
    range: &DiffRange,
    selected: Option<&Path>,
    colors: &DiffColors,
    checkable: bool,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    match rows.get(index) {
        Some(Row::Group(group)) => render_group(flat, index, *group, worktree, entity, cx),
        Some(Row::Dir(dir)) => {
            render_dir(dir, index, worktree, range, colors, checkable, entity, cx)
        }
        Some(Row::File(file)) => render_file(
            file, index, worktree, range, selected, colors, checkable, entity, cx,
        ),
        None => div().into_any_element(),
    }
}

/// Décalage d'un niveau d'arborescence.
///
/// Proportionnel au texte, comme les hauteurs : une indentation figée disparaît
/// quand la police grossit, et l'arbre redevient une liste plate.
fn indent(depth: usize, cx: &gpui::App) -> gpui::Pixels {
    px(8.) + crate::ui::theme::row_height(cx) * 0.5 * depth as f32
}

/// Un dossier : le chevron, la case qui indexe tout ce qu'il contient, et le
/// total de ses lignes.
#[allow(clippy::too_many_arguments)]
fn render_dir(
    row: &DirRow,
    index: usize,
    worktree: &Path,
    range: &DiffRange,
    colors: &DiffColors,
    checkable: bool,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let staged = row.staged;
    let count = row.paths.len();

    h_flex()
        .id(("dir", index))
        .h(crate::ui::theme::row_height(cx))
        .rounded(cx.theme().radius)
        .pl(indent(row.depth, cx))
        .pr_2()
        .gap_1()
        .items_center()
        .cursor_pointer()
        .whitespace_nowrap()
        .overflow_hidden()
        // Un dossier vert est un dossier entièrement relu : c'est ce que sa
        // coche promet en un clic.
        .when(row.reviewed, |el| el.bg(cx.theme().success.opacity(0.12)))
        .hover(|s| s.bg(cx.theme().accent.opacity(0.4)))
        .on_click({
            let (entity, path) = (entity.clone(), row.path.clone());
            move |_, _window, cx| {
                entity.update(cx, |this, cx| this.toggle_directory(path.clone(), cx));
            }
        })
        .child(
            icon(if row.collapsed {
                "chevron-right"
            } else {
                "chevron-down"
            })
            .xsmall(),
        )
        // La case d'un dossier agit sur tout son sous-arbre, replié compris :
        // cocher un dossier fermé doit indexer ce qu'il contient, et non ce
        // qu'on en voit.
        .when(checkable, |el| {
            let (entity, worktree, paths) =
                (entity.clone(), worktree.to_path_buf(), row.paths.clone());
            el.child(
                Checkbox::new(("stage-dir", index))
                    .checked(staged)
                    .on_click(move |_, _window, cx| {
                        entity.update(cx, |this, cx| {
                            this.set_staged(worktree.clone(), paths.clone(), !staged, cx)
                        });
                    }),
            )
        })
        .child(
            icon(if row.collapsed {
                "folder-closed"
            } else {
                "folder-open"
            })
            .xsmall()
            .text_color(cx.theme().muted_foreground),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .child(row.label.clone()),
        )
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(count.to_string()),
        )
        .when(row.added > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(colors.added_fg)
                    .child(format!("+{}", row.added)),
            )
        })
        .when(row.removed > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(colors.removed_fg)
                    .child(format!("−{}", row.removed)),
            )
        })
        .child(render_reviewed(
            ("reviewed-dir", index),
            row.reviewed,
            worktree,
            range,
            row.paths.clone(),
            entity,
            cx,
        ))
        .into_any_element()
}

fn render_group(
    rows: &std::rc::Rc<Vec<Row>>,
    index: usize,
    group: Group,
    worktree: &Path,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let checked = group_checked(rows, group);
    let paths = group_paths(rows, group);
    let count = paths.len();
    let label = match group {
        Group::Tracked => tr!("group-tracked"),
        Group::Untracked => tr!("group-untracked"),
    };
    let (entity, worktree) = (entity.clone(), worktree.to_path_buf());

    h_flex()
        .h(crate::ui::theme::row_height(cx))
        .w_full()
        .px_2()
        .gap_2()
        .items_center()
        .bg(cx.theme().secondary)
        .child(
            Checkbox::new(("group", index))
                .checked(checked)
                .on_click(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.set_staged(worktree.clone(), paths.clone(), !checked, cx)
                    });
                }),
        )
        .child(
            div()
                .flex_1()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(format!("{label} ({count})")),
        )
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_file(
    row: &FileRow,
    index: usize,
    worktree: &Path,
    range: &DiffRange,
    selected: Option<&Path>,
    colors: &DiffColors,
    checkable: bool,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let is_selected = selected == Some(row.path.as_path());
    let staged = row.staged;

    h_flex()
        .id(("file", index))
        .h(crate::ui::theme::row_height(cx))
        .rounded(cx.theme().radius)
        .pl(indent(row.depth, cx))
        .pr_2()
        .gap_2()
        .items_center()
        .cursor_pointer()
        .whitespace_nowrap()
        .overflow_hidden()
        // Relu : un fond vert, qui se voit d'un coup d'œil là où la seule
        // coche à droite demande de parcourir une colonne. La sélection passe
        // devant — c'est l'endroit où l'on est, et le perdre de vue est pire
        // que d'oublier une ligne déjà lue.
        .when(row.reviewed && !is_selected, |el| {
            el.bg(cx.theme().success.opacity(0.12))
        })
        .when(is_selected, |el| el.bg(cx.theme().accent))
        .hover(|s| s.bg(cx.theme().accent.opacity(0.5)))
        .on_click({
            let (entity, worktree, path, range) = (
                entity.clone(),
                worktree.to_path_buf(),
                row.path.clone(),
                range.clone(),
            );
            move |_, window, cx| {
                // Le focus revient à la vue : sans cela, les flèches
                // continueraient de parcourir l'arbre de l'explorateur si
                // c'est de là qu'on venait, et la relecture au clavier du
                // fichier qu'on vient d'ouvrir serait inerte.
                let handle = entity.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
                entity.update(cx, |this, cx| {
                    this.open_file(worktree.clone(), path.clone(), range.clone(), cx)
                });
            }
        })
        // Cocher, c'est indexer. Les domaines qui portent sur des commits déjà
        // écrits n'ont rien à cocher : la case y serait un bouton qui ment.
        .when(checkable, |el| {
            let (entity, worktree, path) =
                (entity.clone(), worktree.to_path_buf(), row.path.clone());
            el.child(Checkbox::new(("stage", index)).checked(staged).on_click(
                move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.set_staged(worktree.clone(), vec![path.clone()], !staged, cx)
                    });
                },
            ))
        })
        .child(
            div()
                .w(px(20.))
                .flex_none()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(status_color(
                    if row.untracked {
                        StatusCode::Untracked
                    } else if staged {
                        row.index
                    } else {
                        row.worktree
                    },
                    cx,
                ))
                .child(row.codes()),
        )
        // L'icône dit la famille par sa forme et le langage par sa teinte :
        // c'est ce qui rend une liste de deux cents fichiers parcourable du
        // regard, là où les codes de git ne disent que ce qui a changé.
        .child(crate::ui::file_icons::file_icon(&row.path, cx))
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .items_baseline()
                // Un fichier relu s'éteint : c'est ce qui fait que la liste
                // dit d'un coup d'œil ce qu'il reste à lire, là où la seule
                // coche à droite demande de parcourir une colonne.
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .when(row.reviewed, |el| {
                            el.text_color(cx.theme().muted_foreground)
                        })
                        .child(row.name.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(row.directory.clone()),
                ),
        )
        // Un fichier dont une partie seulement est indexée : la case cochée ne
        // suffit pas à le dire, et c'est précisément le cas où l'on croit
        // valider tout un fichier alors qu'on n'en valide que la moitié.
        .when(row.partial(), |el| {
            el.child(
                div()
                    .flex_none()
                    .px_1()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().warning.opacity(0.18))
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(tr!("file-partially-staged")),
            )
        })
        .when(row.added > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(colors.added_fg)
                    .child(format!("+{}", row.added)),
            )
        })
        .when(row.removed > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(colors.removed_fg)
                    .child(format!("−{}", row.removed)),
            )
        })
        // Un fichier suivi se rend à son état d'origine ; un fichier nouveau
        // n'en a pas — il se supprime, ce qui n'est pas le même geste et ne
        // porte donc ni la même icône ni le même avertissement.
        .when(checkable, |el| {
            let (entity, worktree, path) =
                (entity.clone(), worktree.to_path_buf(), row.path.clone());
            let untracked = row.untracked;
            el.child(
                Button::new(("discard", index))
                    .ghost()
                    .xsmall()
                    .icon(icon(if untracked { "trash-2" } else { "undo-2" }))
                    .tooltip(if untracked {
                        tr!("action-delete")
                    } else {
                        tr!("action-discard")
                    })
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| {
                            this.confirm_removal(
                                worktree.clone(),
                                path.clone(),
                                untracked,
                                window,
                                cx,
                            )
                        });
                    }),
            )
        })
        // Un fichier se marque relu tout seul, comme un dossier se marque
        // entier : c'est le geste de base, le dossier n'en étant que le
        // raccourci.
        .child(render_reviewed(
            ("reviewed", index),
            row.reviewed,
            worktree,
            range,
            vec![row.path.clone()],
            entity,
            cx,
        ))
        .into_any_element()
}

/// Le bouton qui marque un fichier — ou tout un dossier — relu.
///
/// Une coche et non une case : la case à cocher de cette liste veut déjà dire
/// « indexer », et deux cases côte à côte pour deux gestes sans rapport se
/// confondraient au premier coup d'œil. Elle vit à droite, après le volume, où
/// rien ne la dispute à la lecture du nom.
///
/// Sur un dossier, elle porte tout le sous-arbre, replié compris — comme la
/// case d'indexation, et pour la même raison : c'est le geste qui vaut le
/// détour, une revue de branche ayant plus de dossiers relus d'un bloc que de
/// fichiers relus un par un.
fn render_reviewed(
    id: (&'static str, usize),
    reviewed: bool,
    worktree: &Path,
    range: &DiffRange,
    paths: Vec<PathBuf>,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &gpui::App,
) -> Button {
    let (entity, worktree, range) = (entity.clone(), worktree.to_path_buf(), range.clone());
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(
            icon(if reviewed { "check-check" } else { "check" }).text_color(if reviewed {
                cx.theme().success
            } else {
                cx.theme().muted_foreground.opacity(0.5)
            }),
        )
        .tooltip(if reviewed {
            tr!("action-unreview")
        } else {
            tr!("action-review")
        })
        .on_click(move |_, _window, cx| {
            entity.update(cx, |this, cx| {
                this.set_reviewed(
                    worktree.clone(),
                    range.clone(),
                    paths.clone(),
                    !reviewed,
                    cx,
                )
            });
        })
}

/// Les entrées de la liste pour un domaine de revue donné.
///
/// Le fichier voisin, ou rien aux extrémités.
///
/// Sans fichier ouvert, la première flèche prend l'extrémité vers laquelle
/// elle pointe.
fn step_index(current: Option<usize>, delta: isize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let next = match current {
        Some(index) => index as isize + delta,
        None if delta > 0 => 0,
        None => len as isize - 1,
    };
    (next >= 0 && next < len as isize).then_some(next as usize)
}

/// Fonction libre parce que c'est la seule vraie décision de cette vue — quel
/// fichier apparaît, dans quel groupe, coché ou non — et qu'elle se teste sans
/// fenêtre.
///
/// Le statut est la source pour les modifications en cours : lui seul
/// distingue ce qui est indexé de ce qui ne l'est pas, distinction que la case
/// à cocher restitue. Les autres domaines portent sur des commits, qui n'ont
/// pas de notion d'index, et viennent de `--numstat`.
fn rows_for(
    range: &DiffRange,
    status: &Status,
    files: &[DiffFile],
    reviewed: &[crate::ui::vault::Reviewed],
) -> Vec<Row> {
    let volumes: std::collections::HashMap<&PathBuf, (usize, usize)> = files
        .iter()
        .map(|f| (&f.path, (f.added, f.removed)))
        .collect();
    let volume = |path: &PathBuf| volumes.get(path).copied().unwrap_or((0, 0));
    // Relu **et** inchangé depuis : le volume retenu est ce qui périme la
    // coche, faute de quoi elle dirait « relu » d'un fichier qu'un agent vient
    // de réécrire.
    let is_reviewed = |path: &PathBuf, added: usize, removed: usize| {
        reviewed.iter().any(|item| {
            item.range == *range
                && item.path == *path
                && item.added == added
                && item.removed == removed
        })
    };

    match range {
        DiffRange::Working => {
            let mut tracked = Vec::new();
            let mut untracked = Vec::new();
            for file in &status.files {
                if matches!(file.index, StatusCode::Ignored) {
                    continue;
                }
                let (added, removed) = volume(&file.path);
                let row = FileRow {
                    path: file.path.clone(),
                    depth: 0,
                    name: file.file_name(),
                    directory: file.directory(),
                    index: file.index,
                    worktree: file.worktree,
                    added,
                    removed,
                    staged: file.is_staged(),
                    untracked: file.is_untracked(),
                    reviewed: is_reviewed(&file.path, added, removed),
                };
                if row.untracked {
                    untracked.push(row);
                } else {
                    tracked.push(row);
                }
            }

            let mut rows = Vec::new();
            if !tracked.is_empty() {
                rows.push(Row::Group(Group::Tracked));
                rows.extend(tracked.into_iter().map(Row::File));
            }
            if !untracked.is_empty() {
                rows.push(Row::Group(Group::Untracked));
                rows.extend(untracked.into_iter().map(Row::File));
            }
            rows
        }
        DiffRange::Branch { .. } | DiffRange::Commit { .. } => files
            .iter()
            .map(|f| {
                Row::File(FileRow {
                    path: f.path.clone(),
                    depth: 0,
                    name: f
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    directory: f
                        .path
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    index: if f.removed == 0 {
                        StatusCode::Added
                    } else if f.added == 0 {
                        StatusCode::Deleted
                    } else {
                        StatusCode::Modified
                    },
                    worktree: StatusCode::Unmodified,
                    added: f.added,
                    removed: f.removed,
                    // Un commit est déjà écrit : rien à cocher.
                    staged: true,
                    untracked: false,
                    reviewed: is_reviewed(&f.path, f.added, f.removed),
                })
            })
            .collect(),
    }
}

/// Les fichiers d'un groupe, pour les cases qui agissent sur tout le lot.
fn group_paths(rows: &[Row], group: Group) -> Vec<PathBuf> {
    let mut inside = false;
    let mut paths = Vec::new();
    for row in rows {
        match row {
            Row::Group(g) => inside = *g == group,
            Row::File(file) if inside => paths.push(file.path.clone()),
            Row::File(_) | Row::Dir(_) => {}
        }
    }
    paths
}

/// Vrai si tout le groupe est déjà indexé.
fn group_checked(rows: &[Row], group: Group) -> bool {
    let mut inside = false;
    let mut any = false;
    for row in rows {
        match row {
            Row::Group(g) => inside = *g == group,
            Row::File(file) if inside => {
                any = true;
                if !file.staged {
                    return false;
                }
            }
            Row::File(_) | Row::Dir(_) => {}
        }
    }
    any
}

/// Met la liste plate en arborescence.
///
/// Les groupes sont conservés tels quels ; chaque bloc de fichiers entre deux
/// groupes devient un arbre. La construction est déléguée à `ui::tree`, qui ne
/// connaît que des chemins — ce qu'une ligne affiche est décidé ici, et c'est
/// ce qui permet à l'explorateur de projet d'utiliser le même arbre avec
/// d'autres feuilles.
fn tree_rows(flat: &[Row], collapsed: &HashSet<PathBuf>) -> Vec<Row> {
    let mut out = Vec::new();
    let mut block: Vec<FileRow> = Vec::new();
    for row in flat {
        match row {
            Row::Group(group) => {
                flush(&mut block, collapsed, &mut out);
                out.push(Row::Group(*group));
            }
            Row::File(file) => block.push(file.clone()),
            // Une liste déjà en arbre ne se remet pas en arbre.
            Row::Dir(_) => {}
        }
    }
    flush(&mut block, collapsed, &mut out);
    out
}

fn flush(block: &mut Vec<FileRow>, collapsed: &HashSet<PathBuf>, out: &mut Vec<Row>) {
    if block.is_empty() {
        return;
    }
    let files: Vec<FileRow> = std::mem::take(block);
    let paths: Vec<PathBuf> = files.iter().map(|file| file.path.clone()).collect();
    for entry in crate::ui::tree::build(&paths, collapsed) {
        match entry {
            crate::ui::tree::Entry::Dir {
                path,
                label,
                depth,
                collapsed,
                leaves,
            } => {
                // Les agrégats d'un dossier portent sur **tout** son
                // sous-arbre, replié compris : c'est ce que la case à cocher
                // indexe, et ce que le volume annonce.
                let inside: Vec<&FileRow> = leaves.iter().map(|index| &files[*index]).collect();
                out.push(Row::Dir(DirRow {
                    path,
                    label,
                    depth,
                    collapsed,
                    paths: inside.iter().map(|file| file.path.clone()).collect(),
                    staged: inside.iter().all(|file| file.staged),
                    reviewed: inside.iter().all(|file| file.reviewed),
                    added: inside.iter().map(|file| file.added).sum(),
                    removed: inside.iter().map(|file| file.removed).sum(),
                }));
            }
            crate::ui::tree::Entry::Leaf { index, depth } => {
                let mut file = files[index].clone();
                file.depth = depth;
                // Le dossier est porté par la ligne au-dessus : le répéter sur
                // chaque fichier est exactement le bruit que l'arborescence
                // supprime.
                file.directory.clear();
                out.push(Row::File(file));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FileStatus;

    fn file(path: &str, index: StatusCode, worktree: StatusCode) -> FileStatus {
        FileStatus {
            path: PathBuf::from(path),
            original: None,
            index,
            worktree,
        }
    }

    fn status(files: Vec<FileStatus>) -> Status {
        Status {
            files,
            ..Status::default()
        }
    }

    fn files_of(rows: &[Row]) -> Vec<&FileRow> {
        rows.iter()
            .filter_map(|row| match row {
                Row::File(file) => Some(file),
                Row::Group(_) | Row::Dir(_) => None,
            })
            .collect()
    }

    fn groups_of(rows: &[Row]) -> Vec<Group> {
        rows.iter()
            .filter_map(|row| match row {
                Row::Group(group) => Some(*group),
                Row::File(_) | Row::Dir(_) => None,
            })
            .collect()
    }

    fn tree(paths: &[&str], collapsed: &[&str]) -> Vec<Row> {
        let flat: Vec<Row> = paths
            .iter()
            .map(|p| {
                Row::File(FileRow {
                    path: PathBuf::from(p),
                    depth: 0,
                    name: Path::new(p)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    directory: String::new(),
                    index: StatusCode::Modified,
                    worktree: StatusCode::Unmodified,
                    added: 1,
                    removed: 0,
                    staged: true,
                    untracked: false,
                    reviewed: false,
                })
            })
            .collect();
        let collapsed: HashSet<PathBuf> = collapsed.iter().map(PathBuf::from).collect();
        tree_rows(&flat, &collapsed)
    }

    fn shape(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Group(_) => "groupe".to_string(),
                Row::Dir(dir) => format!(
                    "{}[{}] {}",
                    " ".repeat(dir.depth),
                    dir.paths.len(),
                    dir.label
                ),
                Row::File(file) => format!("{}{}", " ".repeat(file.depth), file.name),
            })
            .collect()
    }

    #[test]
    fn lonely_directories_are_merged_into_one_line() {
        // Le point de l'arborescence sur un projet Laravel : sans fusion,
        // `app/Http/Livewire/Forms` coûte quatre lignes et quatre niveaux
        // d'indentation pour un seul fichier.
        let rows = tree(&["app/Http/Livewire/Forms/BillForm.php"], &[]);
        assert_eq!(
            shape(&rows),
            vec!["[1] app/Http/Livewire/Forms", " BillForm.php"]
        );
    }

    #[test]
    fn a_directory_splits_where_its_contents_split() {
        let rows = tree(
            &["src/ui/app.rs", "src/ui/review.rs", "src/git/diff.rs"],
            &[],
        );
        assert_eq!(
            shape(&rows),
            vec![
                "[3] src",
                " [1] git",
                "  diff.rs",
                " [2] ui",
                "  app.rs",
                "  review.rs",
            ]
        );
    }

    #[test]
    fn a_collapsed_directory_hides_its_files_but_still_counts_them() {
        // Ce compte n'est pas cosmétique : c'est la liste sur laquelle agit la
        // case du dossier, et cocher un dossier fermé doit indexer ce qu'il
        // contient, pas ce qu'on en voit.
        let rows = tree(&["src/ui/app.rs", "src/ui/review.rs"], &["src/ui"]);
        assert_eq!(shape(&rows), vec!["[2] src/ui"]);
        let Row::Dir(dir) = &rows[0] else {
            panic!("un dossier");
        };
        assert!(dir.collapsed);
        assert_eq!(dir.paths.len(), 2);
        assert_eq!(dir.added, 2);
    }

    #[test]
    fn files_at_the_root_come_after_the_directories() {
        let rows = tree(&["Cargo.toml", "src/main.rs"], &[]);
        assert_eq!(shape(&rows), vec!["[1] src", " main.rs", "Cargo.toml"]);
    }

    #[test]
    fn groups_survive_the_tree() {
        let flat = rows_for(
            &DiffRange::Working,
            &status(vec![
                file("src/a.rs", StatusCode::Modified, StatusCode::Unmodified),
                file("src/b.rs", StatusCode::Untracked, StatusCode::Untracked),
            ]),
            &[],
            &[],
        );
        let rows = tree_rows(&flat, &HashSet::new());
        // Un arbre par groupe, et non un arbre unique qui mélangerait le suivi
        // et le non-suivi sous le même dossier.
        assert_eq!(
            shape(&rows),
            vec!["groupe", "[1] src", " a.rs", "groupe", "[1] src", " b.rs"]
        );
    }

    #[test]
    fn staged_and_unstaged_files_share_one_list() {
        // Le point de la fusion : plus deux domaines à recoudre mentalement,
        // une seule liste où la case dit ce qui partira au prochain commit.
        let status = status(vec![
            file("indexe.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("modifie.rs", StatusCode::Unmodified, StatusCode::Modified),
            file("nouveau.rs", StatusCode::Untracked, StatusCode::Untracked),
        ]);
        let rows = rows_for(&DiffRange::Working, &status, &[], &[]);
        let files = files_of(&rows);

        assert_eq!(files.len(), 3);
        assert!(files.iter().find(|f| f.name == "indexe.rs").unwrap().staged);
        assert!(
            !files
                .iter()
                .find(|f| f.name == "modifie.rs")
                .unwrap()
                .staged
        );
        assert!(
            !files
                .iter()
                .find(|f| f.name == "nouveau.rs")
                .unwrap()
                .staged
        );

        // Les fichiers jamais ajoutés forment leur propre groupe : les cocher
        // ne veut pas dire la même chose que pour un fichier déjà suivi.
        assert_eq!(groups_of(&rows), vec![Group::Tracked, Group::Untracked]);
    }

    #[test]
    fn a_partially_staged_file_says_so() {
        // `MM` : une case cochée laisserait croire que tout le fichier part.
        let status = status(vec![file(
            "moitie.rs",
            StatusCode::Modified,
            StatusCode::Modified,
        )]);
        let rows = rows_for(&DiffRange::Working, &status, &[], &[]);
        let file = files_of(&rows)[0];
        assert!(file.staged);
        assert!(file.partial(), "l'indexation partielle doit être signalée");
        assert_eq!(file.codes(), "MM");
    }

    #[test]
    fn the_codes_show_what_git_says() {
        let status = status(vec![
            file("ajoute.rs", StatusCode::Added, StatusCode::Unmodified),
            file("efface.rs", StatusCode::Unmodified, StatusCode::Deleted),
            file("neuf.rs", StatusCode::Untracked, StatusCode::Untracked),
        ]);
        let rows = rows_for(&DiffRange::Working, &status, &[], &[]);
        let files = files_of(&rows);
        assert_eq!(files[0].codes(), "A");
        assert_eq!(files[1].codes(), "D");
        assert_eq!(files[2].codes(), "?");
    }

    fn diff_file(path: &str, added: usize, removed: usize) -> DiffFile {
        DiffFile {
            path: PathBuf::from(path),
            original: None,
            added,
            removed,
            binary: false,
        }
    }

    fn reviewed(path: &str, added: usize, removed: usize) -> crate::ui::vault::Reviewed {
        crate::ui::vault::Reviewed {
            range: DiffRange::Working,
            path: PathBuf::from(path),
            added,
            removed,
        }
    }

    /// Le volume retenu est ce qui périme la coche : un agent qui réécrit un
    /// fichier annule sa relecture, sans quoi la liste dirait « relu » d'un
    /// contenu que personne n'a lu.
    #[test]
    fn a_file_that_changed_since_is_no_longer_reviewed() {
        let status = status(vec![file(
            "a.rs",
            StatusCode::Modified,
            StatusCode::Unmodified,
        )]);
        let rows = rows_for(
            &DiffRange::Working,
            &status,
            &[diff_file("a.rs", 12, 3)],
            &[reviewed("a.rs", 12, 3)],
        );
        assert!(files_of(&rows)[0].reviewed);

        let rows = rows_for(
            &DiffRange::Working,
            &status,
            &[diff_file("a.rs", 13, 3)],
            &[reviewed("a.rs", 12, 3)],
        );
        assert!(!files_of(&rows)[0].reviewed, "le fichier a rechangé depuis");
    }

    /// Une relecture prise dans un domaine ne vaut pas dans l'autre : ce n'est
    /// pas le même diff qu'on a lu.
    #[test]
    fn a_review_belongs_to_its_range() {
        let status = status(vec![file(
            "a.rs",
            StatusCode::Modified,
            StatusCode::Unmodified,
        )]);
        let rows = rows_for(
            &DiffRange::Branch {
                base: "master".into(),
            },
            &status,
            &[diff_file("a.rs", 1, 0)],
            &[reviewed("a.rs", 1, 0)],
        );
        assert!(!files_of(&rows)[0].reviewed);
    }

    /// La coche d'un dossier ne s'allume que quand tout son sous-arbre est lu,
    /// replié compris — c'est ce qu'elle promet en un clic.
    #[test]
    fn a_directory_is_reviewed_only_when_all_of_it_is() {
        let status = status(vec![
            file("src/un.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("src/deux.rs", StatusCode::Modified, StatusCode::Unmodified),
        ]);
        let files = [diff_file("src/un.rs", 1, 0), diff_file("src/deux.rs", 2, 0)];
        let flat = rows_for(
            &DiffRange::Working,
            &status,
            &files,
            &[reviewed("src/un.rs", 1, 0)],
        );
        let dirs = tree_rows(&flat, &HashSet::new());
        let dir = dirs
            .iter()
            .find_map(|row| match row {
                Row::Dir(dir) => Some(dir),
                _ => None,
            })
            .expect("un dossier");
        assert!(!dir.reviewed);
        assert_eq!(dir.paths.len(), 2, "la coche porte tout le sous-arbre");

        let flat = rows_for(
            &DiffRange::Working,
            &status,
            &files,
            &[reviewed("src/un.rs", 1, 0), reviewed("src/deux.rs", 2, 0)],
        );
        let dirs = tree_rows(&flat, &HashSet::new());
        assert!(dirs
            .iter()
            .any(|row| matches!(row, Row::Dir(dir) if dir.reviewed)));
    }

    #[test]
    fn an_empty_group_is_not_shown() {
        let status = status(vec![file(
            "suivi.rs",
            StatusCode::Modified,
            StatusCode::Unmodified,
        )]);
        let rows = rows_for(&DiffRange::Working, &status, &[], &[]);
        assert_eq!(groups_of(&rows), vec![Group::Tracked]);
    }

    #[test]
    fn a_group_is_checked_only_when_all_of_it_is() {
        let mixed = status(vec![
            file("un.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("deux.rs", StatusCode::Unmodified, StatusCode::Modified),
        ]);
        let rows = rows_for(&DiffRange::Working, &mixed, &[], &[]);
        assert!(!group_checked(&rows, Group::Tracked));
        assert_eq!(group_paths(&rows, Group::Tracked).len(), 2);

        let everything = status(vec![
            file("un.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("deux.rs", StatusCode::Added, StatusCode::Unmodified),
        ]);
        let rows = rows_for(&DiffRange::Working, &everything, &[], &[]);
        assert!(group_checked(&rows, Group::Tracked));
    }

    #[test]
    fn a_group_checkbox_only_covers_its_own_files() {
        let status = status(vec![
            file("suivi.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("neuf.rs", StatusCode::Untracked, StatusCode::Untracked),
        ]);
        let rows = rows_for(&DiffRange::Working, &status, &[], &[]);
        assert_eq!(
            group_paths(&rows, Group::Untracked),
            vec![PathBuf::from("neuf.rs")]
        );
        assert_eq!(
            group_paths(&rows, Group::Tracked),
            vec![PathBuf::from("suivi.rs")]
        );
    }

    #[test]
    fn arrows_walk_the_file_list_without_wrapping() {
        assert_eq!(step_index(Some(1), 1, 4), Some(2));
        assert_eq!(step_index(Some(0), -1, 4), None, "avant le premier, rien");
        assert_eq!(step_index(Some(3), 1, 4), None, "après le dernier non plus");
        assert_eq!(step_index(None, 1, 4), Some(0));
        assert_eq!(step_index(None, -1, 4), Some(3));
        assert_eq!(step_index(None, 1, 0), None);
    }

    #[test]
    fn volumes_come_from_numstat_and_default_to_zero() {
        let status = status(vec![file(
            "a.rs",
            StatusCode::Modified,
            StatusCode::Unmodified,
        )]);
        let files = vec![DiffFile {
            path: PathBuf::from("a.rs"),
            original: None,
            added: 12,
            removed: 3,
            binary: false,
        }];
        let rows = rows_for(&DiffRange::Working, &status, &files, &[]);
        let row = files_of(&rows)[0];
        assert_eq!((row.added, row.removed), (12, 3));

        // Sans `--numstat` encore arrivé, la ligne s'affiche quand même.
        let rows = rows_for(&DiffRange::Working, &status, &[], &[]);
        assert_eq!(
            (files_of(&rows)[0].added, files_of(&rows)[0].removed),
            (0, 0)
        );
    }

    #[test]
    fn commit_ranges_come_from_the_file_list_alone() {
        // Aucun statut : une revue de commits ne parle que de ce que git a
        // déjà écrit, et rien n'y est à cocher.
        let files = vec![DiffFile {
            path: PathBuf::from("dossier/ajoute.rs"),
            original: None,
            added: 5,
            removed: 0,
            binary: false,
        }];
        let rows = rows_for(
            &DiffRange::Commit {
                id: "abc".into(),
                parent: None,
            },
            &Status::default(),
            &files,
            &[],
        );
        assert!(groups_of(&rows).is_empty(), "pas de groupes sur un commit");
        let row = files_of(&rows)[0];
        assert_eq!(row.name, "ajoute.rs");
        assert_eq!(row.directory, "dossier");
        assert_eq!(row.index, StatusCode::Added);
        assert!(!row.partial());
    }
}
