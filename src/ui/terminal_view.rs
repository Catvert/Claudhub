//! Les terminaux : un groupe d'onglets par worktree.
//!
//! Chaque onglet est un `Terminal` (pty + émulation alacritty) et une vue qui
//! le dessine. Le multiplexage est ici et non dans tmux : les onglets sont
//! attachés à un worktree, changer de worktree change de groupe, et fermer un
//! worktree ferme ce qui tournait dedans.
//!
//! Le rendu est du texte, pas un canevas : chaque ligne de la grille devient
//! un `StyledText` dont les runs de style viennent de l'instantané. Une police
//! à chasse fixe suffit alors à aligner les colonnes, et gpui s'occupe du
//! façonnage, des ligatures et des scripts complexes — ce qu'un rendu cellule
//! par cellule aurait fallu réécrire.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::{
    div, prelude::*, px, App, Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable, Hsla,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render,
    ScrollWheelEvent, SharedString, StyledText, TextRun, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    menu::{ContextMenuExt, DropdownMenu, PopupMenuItem},
    v_flex, ActiveTheme, Sizable,
};

use crate::terminal::{
    key_bytes, Paint, SelectionKind, Side, Snapshot, Spawn, TermSize, Terminal, TerminalEvent,
    ViewportPosition,
};
use crate::tr;
use crate::ui::app::PerchApp;
use crate::ui::icons::icon;
use crate::ui::settings::{Settings, TerminalSettings};

/// Un onglet de terminal.
pub struct TerminalView {
    terminal: Terminal,
    snapshot: Snapshot,
    focus: FocusHandle,
    font_size: Pixels,
    /// Police effective, relue à chaque rendu depuis les réglages : c'est ce
    /// qui fait qu'un changement dans le formulaire se voit sans rouvrir
    /// l'onglet.
    font_family: SharedString,
    /// Dernière taille connue de la zone de rendu, pour ne redimensionner le
    /// pty que quand la géométrie change vraiment.
    bounds: Bounds<Pixels>,
    /// Géométrie d'une cellule, mesurée sur la police effective. Elle sert à
    /// retraduire une position de souris en ligne et colonne.
    cell: gpui::Size<Pixels>,
    /// Vrai entre l'enfoncement et le relâchement du bouton : c'est ce qui
    /// distingue un glissement de sélection d'un simple survol.
    selecting: bool,
    label: SharedString,
}

impl TerminalView {
    /// Ouvre un pty. Séparé de la vue parce que c'est la seule étape qui peut
    /// échouer, et qu'un échec dans un constructeur d'entité ne laisse d'autre
    /// issue que la panique — pendant un rendu, donc avec la fenêtre figée
    /// pour seul message.
    pub fn open(
        working_directory: &Path,
        command: Option<(String, Vec<String>)>,
        settings: &TerminalSettings,
    ) -> anyhow::Result<Terminal> {
        Terminal::spawn(Spawn {
            working_directory,
            // Un onglet ordinaire prend le programme des réglages ; une
            // commande explicite — l'agent — passe avant, elle est justement
            // ce qu'on a demandé à lancer.
            command: command.or_else(|| settings.program()),
            env: HashMap::new(),
            // La vraie taille arrive au premier rendu ; celle-ci ne sert qu'à
            // ce que le shell ait une géométrie plausible avant sa première
            // invite.
            size: TermSize::new(80, 24, 8, 16),
            scrollback: settings.scrollback,
        })
    }

    pub fn attach(
        terminal: Terminal,
        label: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = Settings::global(cx);
        let font_size = px(settings.terminal.font_size);
        let font_family = SharedString::from(settings.terminal_font().to_string());
        let events = terminal.events();
        // Une tâche de premier plan par terminal : elle réveille la vue quand
        // la boucle d'E/S a du nouveau. Sans elle, la sortie n'apparaîtrait
        // qu'au prochain rendu déclenché par autre chose.
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let alive = this
                    .update(cx, |view, cx| {
                        match event {
                            TerminalEvent::Wakeup => {}
                            TerminalEvent::Title(title) => {
                                view.terminal.set_title(title);
                            }
                            TerminalEvent::Bell => {}
                            TerminalEvent::Exited => view.terminal.mark_exited(),
                        }
                        view.snapshot = view.terminal.snapshot();
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();

        Self {
            snapshot: terminal.snapshot(),
            terminal,
            focus: cx.focus_handle(),
            font_size,
            font_family,
            bounds: Bounds::default(),
            cell: gpui::size(px(8.), px(16.)),
            selecting: false,
            label,
        }
    }

    pub fn label(&self) -> SharedString {
        let title = self.terminal.title();
        if title.is_empty() {
            self.label.clone()
        } else {
            SharedString::from(title.to_string())
        }
    }

    /// Aligne la police sur les réglages courants.
    ///
    /// Un changement de taille ou de famille invalide la géométrie mesurée :
    /// on efface les bornes retenues pour que le prochain passage du canvas de
    /// mesure recalcule la grille et redimensionne le pty. Sans cela, le texte
    /// changerait de taille mais le shell continuerait de croire à
    /// l'ancienne largeur en colonnes.
    fn sync_font(&mut self, cx: &App) {
        let settings = Settings::global(cx);
        let font_size = px(settings.terminal.font_size);
        let font_family = settings.terminal_font();
        if font_size == self.font_size && font_family == self.font_family.as_ref() {
            return;
        }
        self.font_size = font_size;
        self.font_family = SharedString::from(font_family.to_string());
        self.bounds = Bounds::default();
    }

    pub fn has_exited(&self) -> bool {
        self.terminal.has_exited()
    }

    /// Recalcule la grille pour la place disponible.
    ///
    /// La largeur d'un caractère est mesurée sur la police effectivement
    /// choisie, pas devinée : une chasse fixe ne veut pas dire une largeur
    /// connue, et un écart d'un pixel décale la dernière colonne d'une ligne
    /// de quatre-vingts.
    fn sync_size(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        if bounds == self.bounds {
            return;
        }
        self.bounds = bounds;

        let font = gpui::Font {
            family: self.font_family.clone(),
            features: Default::default(),
            weight: Default::default(),
            style: Default::default(),
            fallbacks: None,
        };
        let font_id = window.text_system().resolve_font(&font);
        let cell_width = window
            .text_system()
            .advance(font_id, self.font_size, 'M')
            .map(|s| s.width)
            .unwrap_or(self.font_size * 0.6);
        let line_height = window.line_height().max(px(1.));
        let _ = cx;

        self.cell = gpui::size(cell_width.max(px(1.)), line_height);
        let columns = (bounds.size.width / cell_width.max(px(1.))) as usize;
        let lines = (bounds.size.height / line_height) as usize;
        self.terminal.resize(TermSize::new(
            columns,
            lines,
            f32::from(cell_width) as u16,
            f32::from(line_height) as u16,
        ));
        self.snapshot = self.terminal.snapshot();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(bytes) = key_bytes(&event.keystroke, self.terminal.mode()) {
            // Taper invalide la sélection : ce qu'elle désignait aura bougé
            // dès que le programme aura répondu.
            self.terminal.clear_selection();
            // Toute frappe ramène en bas : c'est ce que fait un terminal, et
            // taper en ayant remonté l'historique sans que la vue suive serait
            // déroutant.
            self.terminal.scroll_to_bottom();
            self.terminal.write(bytes);
            self.snapshot = self.terminal.snapshot();
            cx.notify();
        }
    }

    /// Traduit une position de la fenêtre en cellule du viewport.
    ///
    /// Le côté (`Side`) vient de la moitié de cellule où tombe le pointeur :
    /// sélectionner en partant de la moitié droite d'un caractère ne doit pas
    /// l'inclure, comme dans un éditeur.
    fn position_at(&self, point: Point<Pixels>) -> ViewportPosition {
        viewport_position(point - self.bounds.origin, self.cell)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        if event.button != MouseButton::Left {
            return;
        }
        let kind = match event.click_count {
            1 => SelectionKind::Simple,
            2 => SelectionKind::Word,
            // Au-delà de trois, c'est encore la ligne : personne ne compte les
            // clics au-delà, et réinitialiser serait déroutant.
            _ => SelectionKind::Line,
        };
        self.terminal
            .start_selection(self.position_at(event.position), kind);
        self.selecting = true;
        self.snapshot = self.terminal.snapshot();
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.selecting {
            return;
        }
        self.terminal
            .update_selection(self.position_at(event.position));
        self.snapshot = self.terminal.snapshot();
        cx.notify();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if event.button != MouseButton::Left {
            return;
        }
        self.selecting = false;
        // Une sélection vide est un simple clic : la garder laisserait un
        // reliquat invisible qui ferait échouer la copie suivante.
        if !self.terminal.has_selection() {
            self.terminal.clear_selection();
            self.snapshot = self.terminal.snapshot();
            cx.notify();
        }
    }

    /// Bouton du milieu : colle la sélection primaire d'X11/Wayland, comme
    /// tout terminal Unix.
    fn on_middle_click(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(text) = cx
            .read_from_primary()
            .and_then(|item| item.text())
            .filter(|t| !t.is_empty())
        {
            self.terminal.paste(&text);
            self.terminal.scroll_to_bottom();
            self.snapshot = self.terminal.snapshot();
            cx.notify();
        }
    }

    pub fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.terminal.selection_text().filter(|t| !t.is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub fn paste_from_clipboard(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .filter(|t| !t.is_empty())
        {
            self.terminal.paste(&text);
            self.terminal.scroll_to_bottom();
            self.snapshot = self.terminal.snapshot();
            cx.notify();
        }
    }

    /// Sélectionne tout le contenu visible et l'historique.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.terminal.select_all();
        self.snapshot = self.terminal.snapshot();
        cx.notify();
    }

    /// Dessine le curseur.
    ///
    /// Un rectangle semi-transparent posé par-dessus la grille plutôt qu'une
    /// cellule inversée : l'inversion demanderait de redessiner le glyphe
    /// dans l'autre sens, alors qu'un fond translucide laisse lire le
    /// caractère qui est dessous, ce qui est tout ce qu'on demande à un
    /// curseur de bloc.
    ///
    /// Il ne clignote pas. Un clignotement réveille l'interface deux fois par
    /// seconde et par onglet, en permanence, pour une information que la
    /// position et le contraste donnent déjà ; hors du focus, le contour seul
    /// dit assez que la frappe irait ailleurs.
    fn render_cursor(&self, focused: bool, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let cursor = self.snapshot.cursor?;
        // Hors de la zone visible : on a remonté l'historique, et le curseur
        // est resté en bas avec le programme.
        let line = cursor.line?;
        if !cursor.visible || self.terminal.has_exited() {
            return None;
        }

        let color = cx.theme().caret;
        let element = div()
            .absolute()
            .left(self.cell.width * cursor.column as f32)
            .top(self.cell.height * line as f32)
            .w(self.cell.width)
            .h(self.cell.height)
            .when(focused, |el| el.bg(color.opacity(0.55)))
            .when(!focused, |el| {
                el.border_1().border_color(color.opacity(0.7))
            });
        Some(element)
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let line_height = window.line_height().max(px(1.));
        // La touche système change le sens de la molette : on grossit le texte
        // au lieu de remonter l'historique. Le terminal traite lui-même son
        // défilement, il suffit donc de ne pas le faire.
        if event.modifiers.secondary() {
            let steps = zoom_steps(event.delta.pixel_delta(line_height).y);
            if steps != 0. {
                Settings::update_global(cx, |s| {
                    s.zoom(crate::ui::settings::Zoom::Terminal, steps);
                });
            }
            return;
        }
        let delta = event.delta.pixel_delta(line_height).y / line_height;
        let lines = delta.round() as i32;
        if lines != 0 {
            self.terminal.scroll(lines);
            self.snapshot = self.terminal.snapshot();
            cx.notify();
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let default_fg = cx.theme().foreground;
        self.sync_font(cx);
        let font_size = self.font_size;
        let font_family = self.font_family.clone();
        let entity = cx.entity();

        // La mesure se fait dans un `canvas` de fond, qui reçoit la géométrie
        // définitive après la mise en page. La calculer pendant le rendu de la
        // liste demanderait une taille que personne ne connaît encore.
        let measure = gpui::canvas(
            move |bounds, window, cx| {
                entity.update(cx, |view, cx| view.sync_size(bounds, window, cx));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let selection_bg = cx.theme().selection;
        // Chaque ligne dans une boîte de la hauteur d'une cellule, qui ne
        // revient pas à la ligne et qui rogne ce qui dépasse.
        //
        // Sans cela, une ligne plus large que le panneau est *repliée* par
        // gpui : elle occupe deux hauteurs, pousse tout ce qui suit vers le bas
        // et la grille ne correspond plus à ce que le programme croit
        // afficher. C'est ce qui se voyait après avoir rétréci puis rouvert le
        // panneau — la géométrie est mesurée après la mise en page, donc la
        // grille reste trop large pendant une frame, et le repli qui s'ensuit
        // désaligne tout.
        let cell_height = self.cell.height;
        let lines: Vec<_> = self
            .snapshot
            .lines
            .iter()
            .map(|line| {
                div()
                    .h(cell_height)
                    .w_full()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(styled_line(line, &font_family, default_fg, selection_bg))
            })
            .collect();

        v_flex()
            .id("terminal")
            .key_context(crate::ui::shortcuts::terminal_context())
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &crate::ui::shortcuts::CopySelection, _, cx| {
                    this.copy_selection(cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::ui::shortcuts::PasteClipboard, _, cx| {
                    this.paste_from_clipboard(cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::ui::shortcuts::SelectAllText, _, cx| {
                    this.select_all(cx)
                }),
            )
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .font_family(font_family.clone())
            .text_size(font_size)
            .on_key_down(cx.listener(Self::on_key))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_middle_click))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            // Le clic droit : les gestes qu'on cherche d'abord dans un
            // terminal, et que `Ctrl+C` ne peut pas porter — il appartient au
            // programme qui tourne.
            .context_menu({
                let entity = cx.entity();
                move |menu, _window, _cx| {
                    let (copy, paste, all) = (entity.clone(), entity.clone(), entity.clone());
                    menu.item(PopupMenuItem::new(tr!("terminal-copy")).on_click(
                        move |_, _window, cx| {
                            copy.update(cx, |this, cx| this.copy_selection(cx));
                        },
                    ))
                    .item(PopupMenuItem::new(tr!("terminal-paste")).on_click(
                        move |_, _window, cx| {
                            paste.update(cx, |this, cx| this.paste_from_clipboard(cx));
                        },
                    ))
                    .separator()
                    .item(
                        PopupMenuItem::new(tr!("terminal-select-all")).on_click(
                            move |_, _window, cx| {
                                all.update(cx, |this, cx| this.select_all(cx));
                            },
                        ),
                    )
                }
            })
            .child(measure)
            .child(v_flex().size_full().overflow_hidden().children(lines))
            .children(self.render_cursor(focused, cx))
    }
}

/// Un cran de molette vaut un point de taille.
///
/// Le nombre de lignes que la molette annonce n'entre pas en compte : trois
/// points par cran rendrait le réglage inutilisable, et un pavé tactile en
/// enverrait des dizaines par geste.
pub fn zoom_steps(delta_y: Pixels) -> f32 {
    if delta_y > px(0.) {
        1.
    } else if delta_y < px(0.) {
        -1.
    } else {
        0.
    }
}

/// Convertit une ligne de l'instantané en texte stylé.
fn styled_line(
    line: &crate::terminal::Line,
    family: &SharedString,
    default_fg: Hsla,
    selection_bg: Hsla,
) -> StyledText {
    let text = SharedString::from(line.text.clone());
    let mut runs: Vec<TextRun> = Vec::with_capacity(line.segments.len());
    for seg in &line.segments {
        let len = seg.end.saturating_sub(seg.start);
        if len == 0 {
            continue;
        }
        runs.push(TextRun {
            len,
            font: gpui::Font {
                family: family.clone(),
                features: Default::default(),
                weight: if seg.bold {
                    gpui::FontWeight::BOLD
                } else {
                    gpui::FontWeight::NORMAL
                },
                style: if seg.italic {
                    gpui::FontStyle::Italic
                } else {
                    gpui::FontStyle::Normal
                },
                fallbacks: None,
            },
            color: match seg.fg {
                Paint::Default => default_fg,
                Paint::Rgb(r, g, b) => rgb(r, g, b),
            },
            background_color: if seg.selected {
                // La sélection l'emporte sur la couleur de fond de la cellule,
                // sinon elle disparaîtrait sur une ligne colorée.
                Some(selection_bg)
            } else {
                match seg.bg {
                    // Le fond par défaut est celui de la fenêtre : ne rien
                    // peindre évite un rectangle par cellule.
                    Paint::Default => None,
                    Paint::Rgb(r, g, b) => Some(rgb(r, g, b)),
                }
            },
            underline: seg.underline.then(gpui::UnderlineStyle::default),
            strikethrough: seg.strikethrough.then(gpui::StrikethroughStyle::default),
        });
    }
    StyledText::new(text).with_runs(runs)
}

/// Traduit un décalage en pixels depuis le coin de la zone de rendu en
/// coordonnées de cellule.
///
/// Fonction libre plutôt que méthode : c'est de l'arithmétique dont l'erreur
/// d'une demi-cellule ne se voit pas à l'œil mais rend la sélection
/// désagréable, et qui se teste sans fenêtre.
fn viewport_position(offset: Point<Pixels>, cell: gpui::Size<Pixels>) -> ViewportPosition {
    let width = f32::from(cell.width).max(1.0);
    let height = f32::from(cell.height).max(1.0);
    let column_f = f32::from(offset.x.max(px(0.))) / width;
    let line_f = f32::from(offset.y.max(px(0.))) / height;
    let column = column_f as usize;
    ViewportPosition {
        line: line_f as usize,
        column,
        // La moitié droite d'une cellule désigne la frontière suivante : c'est
        // ce qui permet de sélectionner « abc » en partant du milieu du `a`
        // sans l'inclure, comme dans un éditeur de texte.
        side: if column_f - column as f32 > 0.5 {
            Side::Right
        } else {
            Side::Left
        },
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Hsla {
    gpui::Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

/// Les onglets d'un worktree.
pub struct TerminalGroup {
    worktree: PathBuf,
    tabs: Vec<Entity<TerminalView>>,
    active: usize,
    /// Pourquoi le dernier onglet n'a pas pu s'ouvrir, s'il y a lieu.
    error: Option<SharedString>,
}

impl TerminalGroup {
    pub fn new(worktree: PathBuf) -> Self {
        Self {
            worktree,
            tabs: Vec::new(),
            active: 0,
            error: None,
        }
    }

    /// Ouvre un onglet. `command` vide lance le shell de l'utilisateur.
    pub fn open(
        &mut self,
        command: Option<(String, Vec<String>)>,
        label: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Un pty qu'on n'arrive pas à ouvrir est un problème système : limite
        // de descripteurs atteinte, `/dev/pts` absent. On renonce à l'onglet et
        // on le dit, plutôt que de paniquer au milieu d'un rendu — ce que
        // faisait ce code, avec pour seul symptôme une fenêtre figée.
        // Les réglages sont relus à l'ouverture plutôt que retenus à la
        // construction : changer le shell ou le défilement arrière doit valoir
        // pour le prochain onglet, sans avoir à fermer les autres.
        let settings = Settings::global(cx).terminal.clone();
        let terminal = match TerminalView::open(&self.worktree, command, &settings) {
            Ok(terminal) => terminal,
            Err(e) => {
                log::error!("ouverture du terminal : {e:#}");
                self.error = Some(SharedString::from(e.to_string()));
                cx.notify();
                return;
            }
        };
        let view = cx.new(|cx| TerminalView::attach(terminal, label, window, cx));
        self.error = None;
        self.tabs.push(view);
        self.active = self.tabs.len() - 1;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Vrai quand l'onglet courant a le focus. C'est ce qui désigne la zone
    /// que les raccourcis de zoom visent.
    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.tabs
            .get(self.active)
            .is_some_and(|tab| tab.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Ouvre un onglet exécutant l'agent de codage configuré.
    ///
    /// Le geste vit avec les autres ouvertures de terminal — dans le menu du
    /// bouton « + » — et non dans la barre d'outils de la fenêtre : c'est un
    /// terminal de plus dans *ce* worktree, pas une action sur le dépôt.
    pub fn open_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command = Settings::global(cx).terminal.agent_command.clone();
        if command.trim().is_empty() {
            return;
        }
        let mut parts = command.split_whitespace().map(str::to_string);
        let Some(program) = parts.next() else { return };
        let args: Vec<String> = parts.collect();
        self.open(
            Some((program.clone(), args)),
            SharedString::from(program),
            window,
            cx,
        );
    }

    pub fn close(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        self.focus_active(window, cx);
        cx.notify();
    }

    pub fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        self.active = (self.active + 1) % self.tabs.len();
        self.focus_active(window, cx);
        cx.notify();
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.tabs.get(self.active) {
            let handle = view.read(cx).focus.clone();
            window.focus(&handle);
        }
    }
}

impl Render for TerminalGroup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active;
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, view)| (ix, view.read(cx).label(), view.read(cx).has_exited()))
            .collect();

        v_flex()
            .size_full()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .h(px(28.))
                    .w_full()
                    .px_1()
                    .gap_1()
                    .items_center()
                    .bg(cx.theme().title_bar)
                    .children(tabs.into_iter().map(|(ix, label, exited)| {
                        h_flex()
                            .id(("tab", ix))
                            .h(px(22.))
                            .px_2()
                            .gap_1()
                            .items_center()
                            .rounded(cx.theme().radius)
                            .cursor_pointer()
                            .when(ix == active, |el| el.bg(cx.theme().accent))
                            .hover(|s| s.bg(cx.theme().accent.opacity(0.5)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.active = ix;
                                this.focus_active(window, cx);
                                cx.notify();
                            }))
                            .child(icon("terminal").xsmall())
                            .child(
                                div()
                                    .max_w(px(160.))
                                    .truncate()
                                    .text_xs()
                                    .when(exited, |el| el.text_color(cx.theme().muted_foreground))
                                    .child(label),
                            )
                            .child(
                                Button::new(("close-tab", ix))
                                    .ghost()
                                    .xsmall()
                                    .icon(icon("x"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.close(ix, window, cx);
                                    })),
                            )
                    }))
                    // Le bouton suit le dernier onglet plutôt que de se coller
                    // au bord droit : c'est là que le regard finit sa lecture
                    // des onglets, et un bouton à l'autre bout du panneau
                    // demande de traverser la barre pour ouvrir la suite.
                    .child(
                        Button::new("new-tab")
                            .ghost()
                            .xsmall()
                            .icon(icon("plus"))
                            .tooltip(tr!("terminal-new"))
                            .dropdown_menu({
                                let entity = cx.entity();
                                move |menu, _window, _cx| {
                                    let (shell, agent) = (entity.clone(), entity.clone());
                                    menu.item(PopupMenuItem::new(tr!("terminal-new")).on_click(
                                        move |_, window, cx| {
                                            shell.update(cx, |this, cx| {
                                                this.open(None, tr!("terminal-shell"), window, cx)
                                            });
                                        },
                                    ))
                                    .item(
                                        PopupMenuItem::new(tr!("terminal-agent")).on_click(
                                            move |_, window, cx| {
                                                agent.update(cx, |this, cx| {
                                                    this.open_agent(window, cx)
                                                });
                                            },
                                        ),
                                    )
                                }
                            }),
                    )
                    .child(div().flex_1()),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when_some(self.error.clone(), |el, message| {
                        el.child(
                            div()
                                .p_3()
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child(message),
                        )
                    })
                    .children(self.tabs.get(active).cloned()),
            )
    }
}

impl PerchApp {
    pub(super) fn render_terminals(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(worktree) = self.active.clone() else {
            return div().into_any_element();
        };
        // Le groupe est créé à la première demande : ouvrir un worktree ne doit
        // pas lancer un shell dont personne n'a besoin.
        let group = self.terminal_group(&worktree, window, cx);
        group.into_any_element()
    }

    /// Le groupe d'un worktree, créé au besoin avec un premier onglet.
    pub(super) fn terminal_group(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalGroup> {
        if let Some(group) = self.terminals.get(worktree) {
            return group.clone();
        }
        let path = worktree.to_path_buf();
        let group = cx.new(|_| TerminalGroup::new(path));
        group.update(cx, |group, cx| {
            group.open(None, tr!("terminal-shell"), window, cx);
        });
        self.terminals.insert(worktree.to_path_buf(), group.clone());
        group
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell() -> gpui::Size<Pixels> {
        gpui::size(px(8.), px(16.))
    }

    #[test]
    fn maps_pixels_to_the_cell_under_them() {
        let p = viewport_position(gpui::point(px(0.), px(0.)), cell());
        assert_eq!((p.line, p.column), (0, 0));

        // Colonne 3, ligne 2 : 3×8 et 2×16, plus un poil.
        let p = viewport_position(gpui::point(px(25.), px(33.)), cell());
        assert_eq!((p.line, p.column), (2, 3));
    }

    #[test]
    fn the_half_of_the_cell_decides_the_side() {
        // Premier tiers de la cellule 2 : on vise sa frontière gauche.
        let p = viewport_position(gpui::point(px(18.), px(0.)), cell());
        assert_eq!(p.column, 2);
        assert_eq!(p.side, Side::Left);

        // Dernier tiers de la même cellule : frontière droite.
        let p = viewport_position(gpui::point(px(22.), px(0.)), cell());
        assert_eq!(p.column, 2);
        assert_eq!(p.side, Side::Right);
    }

    #[test]
    fn a_pointer_above_or_left_of_the_view_clamps_to_the_origin() {
        // Un glissement qui sort de la zone ne doit pas produire d'indice
        // négatif : la conversion en `usize` déborderait.
        let p = viewport_position(gpui::point(px(-40.), px(-90.)), cell());
        assert_eq!((p.line, p.column), (0, 0));
    }
}
