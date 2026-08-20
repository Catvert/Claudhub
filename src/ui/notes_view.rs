//! Les gestes de la relecture annotée : prendre une note, la replacer dans le
//! diff, la lister, la renvoyer à l'agent.
//!
//! Le modèle et tout ce qui se teste sans gpui vivent dans `notes.rs` ; ici il
//! n'y a que de la plomberie d'interface.
//!
//! Deux choses n'y sont pas évidentes :
//!
//! - **L'ancrage est arrêté au moment du geste**, pas à la validation du
//!   dialogue. Un agent écrit dans le worktree pendant qu'on le relit, chaque
//!   écriture recharge le diff, et la sélection ne survit pas au rechargement :
//!   décider de l'ancrage à la validation ferait porter la note sur ce qui est
//!   arrivé pendant qu'on l'écrivait.
//! - **Les marqueurs de gouttière sont calculés en amont**, à l'arrivée du
//!   diff et à chaque modification des notes, jamais dans la fermeture de la
//!   liste virtualisée : celle-ci tourne pour chaque ligne visible à chaque
//!   frame, animation de molette comprise.

use std::path::{Path, PathBuf};

use gpui::{div, prelude::*, px, Context, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::Input,
    v_flex, ActiveTheme, Disableable, Selectable, Sizable, WindowExt,
};

use crate::git::DiffRange;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::notes::{self, Note, Side};

/// Une note en cours de rédaction, avec son ancrage déjà arrêté.
pub struct NoteDraft {
    /// Renseigné quand on retouche une note existante plutôt que d'en créer
    /// une : le corps change, l'ancrage ne bouge pas.
    pub editing: Option<u64>,
    pub range: DiffRange,
    pub path: PathBuf,
    pub side: Side,
    pub start: usize,
    pub end: usize,
    pub excerpt: String,
}

impl ClaudhubApp {
    // — Prendre une note ————————————————————————————————————————

    /// Ouvre le dialogue d'annotation sur la sélection courante.
    ///
    /// Sans sélection, rien : une note porte sur une **plage**, et prendre le
    /// fichier entier — ce que fait la copie faute de mieux — donnerait une
    /// remarque qui ne désigne rien.
    pub(super) fn annotate_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let Some(state) = self.active_review() else {
            return;
        };
        let (Some(diff), Some(path)) = (state.diff.clone(), state.selected.clone()) else {
            return;
        };
        let range = state.range.clone();
        // La copie et la note partent du même endroit : la liste unifiée, qui
        // seule porte l'ordre du fichier.
        let Some((from, to)) = (match (split, state.diff_selection) {
            (true, Some((a, b))) => diff.unified_span(a, b),
            (false, Some((a, b))) => Some((a.min(b), a.max(b))),
            (_, None) => None,
        }) else {
            self.announce(tr!("note-needs-a-selection"), cx);
            return;
        };
        let Some((side, start, end)) = notes::anchor_selection(&diff, from, to) else {
            self.announce(tr!("note-needs-a-selection"), cx);
            return;
        };
        self.note_draft = Some(NoteDraft {
            editing: None,
            range,
            path,
            side,
            start,
            end,
            excerpt: diff.copy_text(from, to, false),
        });
        self.open_note_dialog(String::new(), window, cx);
    }

    /// Rouvre une note pour en corriger le texte.
    pub(super) fn edit_note(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(note) = self.note(id).cloned() else {
            return;
        };
        self.note_draft = Some(NoteDraft {
            editing: Some(note.id),
            range: note.range,
            path: note.path,
            side: note.side,
            start: note.start,
            end: note.end,
            excerpt: note.excerpt,
        });
        self.open_note_dialog(note.body, window, cx);
    }

    /// Le dialogue de saisie.
    ///
    /// Un dialogue et non un popover ancré à la ligne : la ligne appartient à
    /// une liste virtualisée, et le moindre défilement — celui que provoque
    /// déjà l'ouverture du clavier — emporterait l'ancre et le popover avec.
    fn open_note_dialog(&mut self, body: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.note_draft.as_ref() else {
            return;
        };
        let title = SharedString::from(format!(
            "{}:{}",
            draft.path.display(),
            span_label(draft.start, draft.end)
        ));
        let excerpt = draft.excerpt.clone();
        let input = self.note_input.clone();
        let entity = cx.entity();
        input.update(cx, |input, cx| input.set_value(body, window, cx));
        let mono = cx.theme().mono_font_family.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (input, excerpt, mono) = (input.clone(), excerpt.clone(), mono.clone());
            let (on_ok, on_cancel) = (entity.clone(), entity.clone());
            dialog
                .title(tr!("note-title"))
                .child(
                    v_flex()
                        .gap_2()
                        .w(px(560.))
                        .child(
                            div()
                                .text_xs()
                                .font_family(mono.clone())
                                .child(title.clone()),
                        )
                        // L'extrait est rappelé sous les yeux : on écrit une
                        // remarque *sur* du code, et le dialogue recouvre
                        // justement celui qu'on regardait.
                        .child(
                            v_flex()
                                .id("note-excerpt")
                                .max_h(px(160.))
                                .overflow_y_scroll()
                                .p_2()
                                .rounded(px(4.))
                                .text_xs()
                                .font_family(mono)
                                .children(
                                    excerpt_lines(&excerpt, usize::MAX)
                                        .into_iter()
                                        .map(|line| div().whitespace_nowrap().child(line)),
                                ),
                        )
                        .child(Input::new(&input)),
                )
                .confirm()
                .on_ok(move |_, _window, cx| {
                    let body = input.read(cx).value().to_string();
                    on_ok.update(cx, |this, cx| this.save_note(body, cx));
                    true
                })
                .on_cancel(move |_, _window, cx| {
                    on_cancel.update(cx, |this, _| this.note_draft = None);
                    true
                })
        });
    }

    fn save_note(&mut self, body: String, cx: &mut Context<Self>) {
        let Some(draft) = self.note_draft.take() else {
            return;
        };
        if body.trim().is_empty() {
            return;
        }
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        match draft.editing {
            // Retoucher une note rouvre la question : elle repart comme non
            // envoyée, sans quoi la version corrigée ne partirait jamais.
            Some(id) => {
                if let Some(note) = state.notes.iter_mut().find(|note| note.id == id) {
                    note.body = body;
                    note.sent = false;
                }
            }
            None => {
                let id = state.next_note;
                state.next_note += 1;
                state.notes.push(Note {
                    id,
                    range: draft.range,
                    path: draft.path,
                    side: draft.side,
                    start: draft.start,
                    end: draft.end,
                    excerpt: draft.excerpt,
                    body,
                    sent: false,
                    done: false,
                });
            }
        }
        self.refresh_note_marks(&worktree);
        self.persist_review(&worktree, cx);
        cx.notify();
    }

    pub(super) fn delete_note(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if let Some(state) = self.review.get_mut(&worktree) {
            state.notes.retain(|note| note.id != id);
        }
        self.refresh_note_marks(&worktree);
        self.persist_review(&worktree, cx);
        cx.notify();
    }

    pub(super) fn set_note_done(&mut self, id: u64, done: bool, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if let Some(note) = self
            .review
            .get_mut(&worktree)
            .and_then(|state| state.notes.iter_mut().find(|note| note.id == id))
        {
            note.done = done;
        }
        self.refresh_note_marks(&worktree);
        self.persist_review(&worktree, cx);
        cx.notify();
    }

    pub(super) fn toggle_notes_filter(&mut self, cx: &mut Context<Self>) {
        self.notes_only_open = !self.notes_only_open;
        cx.notify();
    }

    fn note(&self, id: u64) -> Option<&Note> {
        self.active_review()?
            .notes
            .iter()
            .find(|note| note.id == id)
    }

    // — Replacer les notes dans le diff ————————————————————————

    /// Recalcule les lignes annotées du diff affiché.
    ///
    /// Appelée à l'arrivée d'un diff et à chaque modification des notes, jamais
    /// pendant un rendu : `relocate` parcourt le diff entier par note.
    pub(super) fn refresh_note_marks(&mut self, worktree: &Path) {
        let Some(state) = self.review.get_mut(worktree) else {
            return;
        };
        let (Some(diff), Some(path)) = (state.diff.clone(), state.selected.clone()) else {
            state.note_marks = std::rc::Rc::new(notes::Marks::default());
            state.drifted.clear();
            return;
        };
        let range = state.range.clone();
        let mut spans = Vec::new();
        let mut drifted = std::collections::HashSet::new();
        for note in state
            .notes
            .iter()
            .filter(|note| note.path == path && note.range == range && !note.done)
        {
            match notes::relocate(&diff, note).rows() {
                Some(span) => spans.push(span),
                None => {
                    drifted.insert(note.id);
                }
            }
        }
        state.note_marks = std::rc::Rc::new(notes::marks(&diff, &spans));
        state.drifted = drifted;
    }

    /// Ouvre le fichier d'une note et l'amène sous les yeux.
    pub(super) fn reveal_note(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some((path, range)) = self
            .note(id)
            .map(|note| (note.path.clone(), note.range.clone()))
        else {
            return;
        };
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let already = self
            .active_review()
            .is_some_and(|state| state.selected.as_deref() == Some(path.as_path()));
        if !already {
            // Le diff n'est pas encore là : la sélection sera posée à son
            // arrivée, par `Evt::FileDiff`, comme pour un débordement de
            // flèche. Le drapeau est posé **après** l'ouverture, qui efface
            // justement les sauts armés par un geste précédent.
            self.open_file(worktree.clone(), path, range, cx);
            if let Some(state) = self.review.get_mut(&worktree) {
                state.pending_note = Some(id);
            }
            return;
        }
        self.select_note_rows(id, cx);
    }

    /// Sélectionne les lignes d'une note dans le diff déjà affiché.
    pub(super) fn select_note_rows(&mut self, id: u64, cx: &mut Context<Self>) {
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let Some(state) = self.active_review() else {
            return;
        };
        let (Some(diff), Some(note)) = (
            state.diff.clone(),
            state.notes.iter().find(|note| note.id == id).cloned(),
        ) else {
            return;
        };
        let Some((from, to)) = notes::relocate(&diff, &note).rows() else {
            self.announce(tr!("note-drifted"), cx);
            return;
        };
        // En deux colonnes, les indices unifiés ne désignent pas les mêmes
        // entrées : on retrouve celles qui les recouvrent.
        let shown = if split {
            split_span(&diff, from, to)
        } else {
            Some((from, to))
        };
        let Some((from, to)) = shown else { return };
        if let Some(state) = self.active_review_mut() {
            state.diff_selection = Some((from, to));
        }
        self.diff_scroll
            .scroll_to_item(from, gpui::ScrollStrategy::Center);
        cx.notify();
    }

    // — Envoyer ————————————————————————————————————————————————

    /// Livre des notes à l'agent du worktree.
    ///
    /// `only` désigne une note ; sans lui, toutes celles qui ne sont pas
    /// traitées. Elles passent à `sent` et non à `done` : c'est la relecture de
    /// la réponse qui les clôt.
    pub(super) fn send_notes(
        &mut self,
        only: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let branch = self
            .active_worktree()
            .and_then(|w| w.branch.clone())
            .unwrap_or_else(|| tr!("branch-detached").to_string());
        let Some(state) = self.review.get(&worktree) else {
            return;
        };
        let chosen: Vec<Note> = state
            .notes
            .iter()
            .filter(|note| match only {
                Some(id) => note.id == id,
                None => !note.done,
            })
            .cloned()
            .collect();
        if chosen.is_empty() {
            self.announce(tr!("note-nothing-to-send"), cx);
            return;
        }
        let count = chosen.len();
        let ids: Vec<u64> = chosen.iter().map(|note| note.id).collect();
        let text = notes::prompt(&branch, &chosen);
        self.deliver(worktree.clone(), text, window, cx);

        if let Some(state) = self.review.get_mut(&worktree) {
            for note in state.notes.iter_mut().filter(|note| ids.contains(&note.id)) {
                note.sent = true;
            }
        }
        self.persist_review(&worktree, cx);
        self.announce(tr!("note-sent", { count: count }), cx);
    }

    /// Pose une question libre sur la sélection courante.
    ///
    /// Sans passer par une note : c'est le geste le plus fréquent en pratique
    /// — on relit, quelque chose intrigue, on demande, et il n'y a rien à
    /// consigner.
    pub(super) fn ask_about_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let Some(state) = self.active_review() else {
            return;
        };
        let (Some(diff), Some(path)) = (state.diff.clone(), state.selected.clone()) else {
            return;
        };
        // Sans sélection, la question porte sur tout le fichier : contrairement
        // à une note, elle n'a pas à désigner une plage précise.
        let (from, to) = match (split, state.diff_selection) {
            (true, Some((a, b))) => match diff.unified_span(a, b) {
                Some(span) => span,
                None => return,
            },
            (false, Some((a, b))) => (a.min(b), a.max(b)),
            (_, None) => (0, diff.rows.len().saturating_sub(1)),
        };
        let excerpt = diff.copy_text(from, to, false);
        let location = match notes::anchor_selection(&diff, from, to) {
            Some((_, start, end)) => format!("{}:{}", path.display(), span_label(start, end)),
            None => path.display().to_string(),
        };
        self.open_text_dialog(
            tr!("note-ask-title"),
            tr!("note-ask-placeholder"),
            window,
            cx,
            move |this, question, window, cx| {
                if question.trim().is_empty() {
                    return;
                }
                let Some(worktree) = this.active.clone() else {
                    return;
                };
                let text = notes::ask(&location, &path, &excerpt, &question);
                this.deliver(worktree, text, window, cx);
            },
        );
    }

    /// Livre un texte à l'agent, en ouvrant le panneau des terminaux.
    fn deliver(
        &mut self,
        worktree: PathBuf,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = self.terminal_group(&worktree, window, cx);
        group.update(cx, |group, cx| group.send_to_agent(text, window, cx));
        self.show_terminal_panel(window, cx);
    }

    // — Le panneau ——————————————————————————————————————————————

    pub(super) fn render_notes(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let notes_scroll = self.scroll_of("notes");
        let Some(state) = self.active_review() else {
            return empty_notes(tr!("no-worktree"), cx).into_any_element();
        };
        let only_open = self.notes_only_open;
        let drifted = state.drifted.clone();
        let notes: Vec<Note> = state
            .notes
            .iter()
            .filter(|note| !only_open || !note.done)
            .cloned()
            .collect();
        let total = state.notes.len();
        let pending = state.notes.iter().filter(|note| !note.done).count();
        let mono = cx.theme().mono_font_family.clone();

        let bar = h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("reply").xsmall())
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("note-count", { count: pending })),
            )
            .child(
                Button::new("notes-filter")
                    .ghost()
                    .xsmall()
                    .icon(icon(if only_open { "eye-off" } else { "eye" }))
                    .selected(only_open)
                    .tooltip(tr!("note-only-open"))
                    .on_click(cx.listener(|this, _, _window, cx| this.toggle_notes_filter(cx))),
            )
            .child(
                Button::new("notes-send-all")
                    .ghost()
                    .xsmall()
                    .icon(icon("send"))
                    .tooltip(tr!("note-send-all"))
                    .disabled(pending == 0)
                    .on_click(cx.listener(|this, _, window, cx| this.send_notes(None, window, cx))),
            );

        if notes.is_empty() {
            return v_flex()
                .size_full()
                .child(bar)
                .child(empty_notes(
                    if total == 0 {
                        tr!("note-empty")
                    } else {
                        tr!("note-all-done")
                    },
                    cx,
                ))
                .into_any_element();
        }

        // Groupées par fichier, dans l'ordre où les notes ont été prises : une
        // relecture se relit dans l'ordre où elle s'est faite.
        let mut groups: Vec<(PathBuf, Vec<Note>)> = Vec::new();
        for note in notes {
            match groups.last_mut() {
                Some((path, bucket)) if *path == note.path => bucket.push(note),
                _ => groups.push((note.path.clone(), vec![note])),
            }
        }

        // Les lignes sont construites d'avance et non dans une fermeture
        // paresseuse : `render_note` emprunte la vue *et* le contexte, ce
        // qu'un itérateur consommé plus loin dans la même expression
        // n'autorise pas.
        let (secondary, muted) = (cx.theme().secondary, cx.theme().muted_foreground);
        let mut sections = Vec::new();
        for (path, bucket) in groups {
            let mut rows = Vec::new();
            for note in bucket {
                rows.push(
                    self.render_note(note, &drifted, mono.clone(), cx)
                        .into_any_element(),
                );
            }
            sections.push(
                v_flex()
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .font_family(mono.clone())
                            .bg(secondary)
                            .text_color(muted)
                            .truncate()
                            .child(path.display().to_string()),
                    )
                    .children(rows),
            );
        }

        v_flex()
            .size_full()
            .child(bar)
            .child(
                div().flex_1().min_h_0().child(crate::ui::scroll::vertical(
                    "notes-bar",
                    &notes_scroll,
                    v_flex()
                        .id("notes-list")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&notes_scroll)
                        .children(sections),
                )),
            )
            .into_any_element()
    }

    fn render_note(
        &mut self,
        note: Note,
        drifted: &std::collections::HashSet<u64>,
        mono: SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = note.id;
        let is_drifted = drifted.contains(&id);
        let muted = cx.theme().muted_foreground;
        v_flex()
            .id(("note", id as usize))
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Checkbox::new(("note-done", id as usize))
                            .checked(note.done)
                            .on_click({
                                let entity = cx.entity();
                                let done = note.done;
                                move |_, _window, cx| {
                                    entity.update(cx, |this, cx| this.set_note_done(id, !done, cx));
                                }
                            }),
                    )
                    .child(
                        div()
                            .id(("note-loc", id as usize))
                            .flex_1()
                            .text_xs()
                            .font_family(mono.clone())
                            .text_color(muted)
                            .cursor_pointer()
                            .truncate()
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.reveal_note(id, cx);
                            }))
                            .child(span_label(note.start, note.end)),
                    )
                    // Une note dont on ne retrouve plus le code reste dans la
                    // liste : la perdre en silence serait pire que ne pas
                    // l'avoir prise.
                    .when(is_drifted, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child(tr!("note-drifted")),
                        )
                    })
                    .when(note.sent, |el| {
                        el.child(div().text_color(muted).child(icon("check").xsmall()))
                    })
                    .child(
                        Button::new(("note-send", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(icon("send"))
                            .tooltip(tr!("note-send"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.send_notes(Some(id), window, cx);
                            })),
                    )
                    .child(
                        Button::new(("note-edit", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(icon("pencil"))
                            .tooltip(tr!("note-edit"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_note(id, window, cx);
                            })),
                    )
                    .child(
                        Button::new(("note-delete", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(icon("trash-2"))
                            .tooltip(tr!("note-delete"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.delete_note(id, cx);
                            })),
                    ),
            )
            // L'extrait, tronqué à quelques lignes : le panneau sert à
            // retrouver une note, pas à relire le fichier.
            .child(
                v_flex()
                    .text_xs()
                    .font_family(mono)
                    .text_color(muted)
                    .children(
                        excerpt_lines(&note.excerpt, EXCERPT_LINES)
                            .into_iter()
                            .map(|line| div().truncate().child(line)),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .when(note.done, |el| el.text_color(muted).line_through())
                    .child(SharedString::from(note.body.clone())),
            )
    }
}

/// Lignes d'extrait montrées dans le panneau : de quoi reconnaître la note,
/// pas de quoi relire le fichier — c'est le diff qui est là pour ça.
const EXCERPT_LINES: usize = 4;

/// Découpe un extrait en lignes, tronqué à `limit`.
///
/// Une ligne par élément et non un seul texte : gpui ne coupe pas un texte sur
/// ses `\n`, et un extrait de six lignes s'afficherait sur une seule.
fn excerpt_lines(excerpt: &str, limit: usize) -> Vec<SharedString> {
    let mut lines: Vec<SharedString> = excerpt
        .lines()
        .take(limit)
        .map(|line| SharedString::from(line.to_string()))
        .collect();
    if excerpt.lines().count() > limit {
        lines.push(SharedString::new_static("…"));
    }
    lines
}

/// `120` ou `120-134` : la forme que tout le monde sait lire.
fn span_label(start: usize, end: usize) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}

/// Les entrées de la vue en colonnes qui recouvrent une plage unifiée.
fn split_span(
    diff: &crate::ui::diff_view::Rendered,
    from: usize,
    to: usize,
) -> Option<(usize, usize)> {
    let mut bounds: Option<(usize, usize)> = None;
    for (index, row) in diff.split.iter().enumerate() {
        if row
            .unified()
            .any(|unified| unified >= from && unified <= to)
        {
            bounds = Some(match bounds {
                Some((a, b)) => (a.min(index), b.max(index)),
                None => (index, index),
            });
        }
    }
    bounds
}

/// L'état vide du panneau : une icône et une phrase, comme partout ailleurs.
fn empty_notes(message: SharedString, cx: &Context<ClaudhubApp>) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(icon("reply"))
        .child(div().text_sm().child(message))
}
