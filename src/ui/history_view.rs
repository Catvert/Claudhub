//! Le panneau d'historique et son graphe.
//!
//! Le graphe est dessiné, pas écrit en caractères : une courbe rend le
//! rattachement d'une branche lisible d'un coup d'œil là où un `|/` demande
//! d'être déchiffré. Chaque ligne peint sa propre portion — les traits qui la
//! traversent, ceux qui s'y referment, ceux qui en partent — ce qui permet à
//! la liste de rester virtualisée : une ligne se dessine sans rien savoir de
//! celles qu'on ne voit pas.

use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, uniform_list, Bounds, Context, Entity, Hsla, PathBuilder, Pixels,
    Point, SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    resizable::{resizable_panel, v_resizable},
    v_flex, ActiveTheme, Selectable, Sizable,
};

use crate::git::{DiffRange, GraphRow, LogRange};
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::{ClaudhubApp, History};

/// Largeur d'une colonne du graphe.
const LANE: Pixels = px(14.);
// La hauteur d'une ligne se déduit de la taille du texte
// (`theme::row_height`) : la liste réserve la hauteur d'un seul élément et ne
// mesure rien, si bien qu'une ligne trop haute recouvre la suivante dès qu'on
// grossit la police.
const DOT: Pixels = px(7.);
const STROKE: Pixels = px(1.5);
/// Nombre de commits demandés. Au-delà, l'historique se lit par recherche, pas
/// par défilement — et un `git log` sans limite sur un dépôt ancien coûte des
/// secondes pour des lignes que personne n'atteindra.
const LIMIT: usize = 2_000;

/// Couleur d'une colonne. Les teintes tournent pour que deux branches
/// voisines ne se confondent pas ; elles n'ont pas d'autre sens, git n'ayant
/// aucune notion d'identité de branche au niveau d'un commit.
fn lane_color(column: usize, cx: &gpui::App) -> Hsla {
    const HUES: [f32; 6] = [0.58, 0.35, 0.08, 0.78, 0.14, 0.95];
    let theme = cx.theme();
    Hsla {
        h: HUES[column % HUES.len()],
        s: 0.55,
        l: if theme.mode.is_dark() { 0.62 } else { 0.42 },
        a: 1.0,
    }
}

impl ClaudhubApp {
    /// Charge l'historique du worktree courant s'il ne l'est pas déjà.
    pub(super) fn ensure_history(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        if state.history.is_some() || state.history_pending {
            return;
        }
        state.history_pending = true;
        let range = state.history_range.clone();
        self.git.send(Cmd::LoadHistory {
            worktree,
            range,
            limit: LIMIT,
        });
        cx.notify();
    }

    pub(super) fn set_history_range(&mut self, range: LogRange, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        if state.history_range == range {
            return;
        }
        state.history_range = range.clone();
        // L'historique précédent est jeté tout de suite : le garder le temps du
        // chargement donnerait l'impression que le bouton n'a rien fait, puis
        // que la liste change toute seule.
        state.history = None;
        state.history_pending = true;
        self.git.send(Cmd::LoadHistory {
            worktree,
            range,
            limit: LIMIT,
        });
        cx.notify();
    }

    /// Affiche le diff d'un commit.
    pub(super) fn open_commit(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        let Some(history) = state.history.clone() else {
            return;
        };
        let Some(commit) = history.commits.get(index) else {
            return;
        };

        state.commit = Some(commit.id.clone());
        // Le premier parent : c'est la comparaison qu'attend un relecteur
        // devant un merge, celle qui montre ce que la fusion a apporté.
        let range = DiffRange::Commit {
            id: commit.id.clone(),
            parent: commit.parents.first().cloned(),
        };
        state.range = range.clone();
        state.selected = None;
        state.diff = None;
        state.diff_selection = None;
        // Les diffs d'un autre commit n'ont plus d'usage : les garder ferait
        // enfler l'état d'un domaine par commit consulté.
        state
            .files
            .retain(|kept, _| !matches!(kept, DiffRange::Commit { .. }));
        state
            .pending_files
            .retain(|kept| !matches!(kept, DiffRange::Commit { .. }));
        self.ensure_files(range, cx);
        cx.notify();
    }

    /// Un commit correspond-il à la requête ? Son sujet, son auteur, son
    /// abrégé : les trois choses par lesquelles on retrouve un commit.
    pub(super) fn commit_matches(commit: &crate::git::Commit, query: &str) -> bool {
        crate::ui::find::matches(query, &commit.summary)
            || crate::ui::find::matches(query, &commit.author)
            || crate::ui::find::matches(query, &commit.short)
    }

    /// Sélectionne le commit trouvé suivant, et l'amène dans la vue.
    ///
    /// Le graphe interdit de filtrer : ses traits relient une ligne à ses
    /// voisines, et retirer une ligne du milieu ferait pointer chacun d'eux
    /// sur le mauvais commit. La recherche éteint donc ce qui ne correspond
    /// pas, et cette touche va d'une trouvaille à la suivante.
    pub(super) fn step_history_match(&mut self, delta: isize, cx: &mut Context<Self>) {
        let query = self.query(crate::ui::find::Pane::History, cx);
        if query.trim().is_empty() {
            return;
        }
        let Some(state) = self.active_review() else {
            return;
        };
        let Some(history) = state.history.clone() else {
            return;
        };
        let selected = state.commit.clone();
        let count = history.commits.len();
        if count == 0 {
            return;
        }
        let from = selected
            .and_then(|id| history.commits.iter().position(|c| c.id == id))
            .map(|index| index as isize)
            .unwrap_or(if delta > 0 { -1 } else { count as isize });
        // La recherche boucle : on cherche à faire le tour de ce qu'on a
        // trouvé, pas à buter au bout de la liste.
        for step in 1..=count as isize {
            let index = (from + delta * step).rem_euclid(count as isize) as usize;
            if Self::commit_matches(&history.commits[index], &query) {
                self.open_commit(index, cx);
                self.history_scroll
                    .scroll_to_item(index, gpui::ScrollStrategy::Center);
                return;
            }
        }
    }

    pub(super) fn render_history(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let find = self.render_find(crate::ui::find::Pane::History, cx);
        let query = self.query(crate::ui::find::Pane::History, cx);
        let Some(state) = self.active_review() else {
            return div().into_any_element();
        };
        let range = state.history_range.clone();
        let selected = state.commit.clone();
        let history = state.history.clone();

        let row_height = crate::ui::theme::row_height(cx);
        let header = h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(
                [
                    (LogRange::All, tr!("history-all")),
                    (LogRange::Head, tr!("history-head")),
                ]
                .into_iter()
                .enumerate()
                .map(|(ix, (target, label))| {
                    let selected = range == target;
                    Button::new(("history-range", ix))
                        .ghost()
                        .xsmall()
                        .label(label)
                        .selected(selected)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_history_range(target.clone(), cx);
                        }))
                }),
            );

        let Some(history) = history else {
            return v_flex()
                .size_full()
                .child(header)
                .children(find)
                .child(
                    div()
                        .p_3()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("history-loading")),
                )
                .into_any_element();
        };
        if history.commits.is_empty() {
            return v_flex()
                .size_full()
                .child(header)
                .children(find)
                .child(
                    div()
                        .p_3()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("history-empty")),
                )
                .into_any_element();
        }

        let entity = cx.entity();
        let count = history.commits.len();
        let gutter = LANE * history.width as f32 + px(6.);

        let commit_range = self
            .active_review()
            .and_then(|state| state.commit.as_ref().map(|_| state.range.clone()))
            .filter(|range| matches!(range, DiffRange::Commit { .. }));

        let graph = v_flex().size_full().child(header).children(find).child(
            div().flex_1().min_h_0().child(crate::ui::scroll::vertical(
                "history-bar",
                &self.history_scroll,
                uniform_list("history", count, move |visible, _window, cx| {
                    visible
                        .map(|ix| {
                            // Filtrer est impossible ici : les traits du graphe
                            // relient une ligne à ses voisines, et en retirer
                            // une du milieu ferait pointer chacun d'eux sur le
                            // mauvais commit. Ce qui ne correspond pas
                            // s'éteint donc, et reste à sa place.
                            let dimmed = !query.is_empty()
                                && !history
                                    .commits
                                    .get(ix)
                                    .is_some_and(|c| ClaudhubApp::commit_matches(c, &query));
                            render_commit(
                                &history,
                                ix,
                                gutter,
                                selected.as_deref(),
                                row_height,
                                dimmed,
                                &entity,
                                cx,
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .size_full()
                .track_scroll(self.history_scroll.clone()),
            )),
        );

        // Le graphe seul ne dit pas ce qu'un commit a touché : la liste de ses
        // fichiers va dessous, sinon sélectionner un commit n'ouvre que son
        // premier fichier et les autres restent invisibles.
        let Some(range) = commit_range else {
            return graph.into_any_element();
        };
        v_resizable("claudhub-history-split")
            .with_state(&self.history_split)
            .child(resizable_panel().size(px(420.)).child(graph))
            .child(
                resizable_panel()
                    .child(self.render_file_list(range, window, cx).into_any_element()),
            )
            .into_any_element()
    }
}

#[allow(clippy::too_many_arguments)]
fn render_commit(
    history: &Rc<History>,
    index: usize,
    gutter: Pixels,
    selected: Option<&str>,
    row_height: Pixels,
    // Le commit ne correspond pas à la recherche en cours.
    dimmed: bool,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let (Some(commit), Some(row)) = (history.commits.get(index), history.graph.get(index)) else {
        return div().into_any_element();
    };
    let is_selected = selected == Some(commit.id.as_str());
    let dot_color = lane_color(row.column, cx);
    let muted = cx.theme().muted_foreground;
    let accent = cx.theme().accent;

    let row = row.clone();
    let entity = entity.clone();
    let graph = canvas(
        move |_, _, _| {},
        move |bounds, _, window, cx| paint_graph(&row, bounds, window, cx),
    )
    .w(gutter)
    .h(row_height);

    h_flex()
        .id(("commit", index))
        .h(row_height)
        .w_full()
        .items_center()
        .gap_2()
        .pr_2()
        // Une ligne d'historique tient sur une ligne : sans cela, un sha ou un
        // nom d'auteur un peu long revient à la ligne, dépasse la hauteur
        // réservée par la liste virtualisée et recouvre le résumé du commit.
        .overflow_hidden()
        .whitespace_nowrap()
        // Éteint plutôt que masqué : la ligne garde sa place, donc le graphe
        // garde ses traits.
        .when(dimmed, |el| el.opacity(0.35))
        .cursor_pointer()
        .when(is_selected, |el| el.bg(accent))
        .hover(|s| s.bg(accent.opacity(0.5)))
        .on_click(move |_, _window, cx| {
            entity.update(cx, |this, cx| this.open_commit(index, cx));
        })
        .child(graph)
        .child(
            div()
                .flex_none()
                // Dix caractères de chasse fixe : la longueur que git donne à
                // `%h` sur un dépôt de cette taille, plus une marge.
                .w(px(84.))
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .text_color(dot_color)
                .child(commit.short.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .child(commit.summary.clone()),
        )
        .children(commit.refs.iter().take(2).map(|reference| {
            div()
                .flex_none()
                .px_1()
                .rounded(cx.theme().radius)
                .bg(dot_color.opacity(0.18))
                .text_xs()
                .text_color(dot_color)
                .child(SharedString::from(reference.clone()))
        }))
        .child(
            div()
                .flex_none()
                .max_w(px(110.))
                .truncate()
                .text_xs()
                .text_color(muted)
                .child(commit.author.clone()),
        )
        .child(
            div()
                .flex_none()
                .w(px(88.))
                .text_right()
                .text_xs()
                .text_color(muted)
                .child(commit.date.clone()),
        )
        .into_any_element()
}

/// Peint la portion de graphe d'une ligne.
///
/// Les traits sont dessinés avant la puce pour qu'elle les recouvre : une
/// courbe qui arrive sur un commit doit disparaître sous lui, pas le traverser.
fn paint_graph(row: &GraphRow, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut gpui::App) {
    let x = |column: usize| bounds.origin.x + LANE * column as f32 + LANE / 2.;
    let top = bounds.origin.y;
    let middle = top + bounds.size.height / 2.;
    let bottom = top + bounds.size.height;

    let mut line = |from: Point<Pixels>, to: Point<Pixels>, ctrl: Option<Point<Pixels>>, color| {
        let mut path = PathBuilder::stroke(STROKE);
        path.move_to(from);
        match ctrl {
            // Une courbe dont le point de contrôle est à l'aplomb du départ :
            // le trait quitte sa colonne verticalement puis s'infléchit, ce qui
            // donne le raccord doux des visualiseurs d'historique plutôt qu'un
            // angle vif.
            Some(ctrl) => path.curve_to(to, ctrl),
            None => path.line_to(to),
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    };

    // Les branches qui ne font que passer.
    for &column in &row.through {
        let color = lane_color(column, cx);
        line(
            gpui::point(x(column), top),
            gpui::point(x(column), bottom),
            None,
            color,
        );
    }

    // Le trait vertical du commit lui-même : depuis le haut s'il a un enfant
    // au-dessus, vers le bas s'il a un parent en dessous. On les dessine dans
    // tous les cas : la première et la dernière ligne se contentent d'un demi
    // segment, ce qui est exactement ce qu'on veut voir aux extrémités.
    let own = lane_color(row.column, cx);
    line(
        gpui::point(x(row.column), top),
        gpui::point(x(row.column), bottom),
        None,
        own,
    );

    // Les rails qui se referment sur ce commit : ils descendent de leur colonne
    // et rejoignent la puce.
    for &column in &row.incoming {
        let color = lane_color(column, cx);
        line(
            gpui::point(x(column), top),
            gpui::point(x(row.column), middle),
            Some(gpui::point(x(column), middle)),
            color,
        );
    }

    // Les parents placés ailleurs : le trait part de la puce vers leur colonne.
    for &column in &row.outgoing {
        let color = lane_color(column, cx);
        line(
            gpui::point(x(row.column), middle),
            gpui::point(x(column), bottom),
            Some(gpui::point(x(column), middle)),
            color,
        );
    }

    // La puce, en dernier pour couvrir les traits qui l'atteignent.
    let radius = DOT / 2.;
    window.paint_quad(gpui::fill(
        Bounds::new(
            gpui::point(x(row.column) - radius, middle - radius),
            gpui::size(DOT, DOT),
        ),
        own,
    ));
}
