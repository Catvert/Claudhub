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
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::{Settings, TerminalSettings};

/// Temps laissé à un agent qu'on vient de lancer pour afficher son invite.
///
/// Rien dans un pty ne dit « je suis prêt » : ce qui arrive avant l'invite est
/// lu par le shell qu'on n'a pas encore remplacé, ou perdu. Deux secondes
/// couvrent le démarrage d'un agent sur une machine chargée.
const AGENT_WARMUP: std::time::Duration = std::time::Duration::from_millis(2000);

/// Silence entre le collage et le retour chariot qui le valide.
const SUBMIT_DELAY: std::time::Duration = std::time::Duration::from_millis(120);

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
    /// Fraction de ligne non consommée par le dernier événement de molette.
    scroll_remainder: f32,
    /// Géométrie demandée par la mise en page, pas encore transmise.
    pending_size: Option<TermSize>,
    /// Vrai quand une transmission différée est déjà programmée.
    resize_scheduled: bool,
    label: SharedString,
    /// Vrai quand cet onglet exécute un agent de codage.
    ///
    /// C'est ce qui permet de lui livrer des notes de relecture sans se
    /// tromper d'onglet. Retenu à l'ouverture et non déduit du titre : un
    /// agent renomme son onglet au fil de la conversation, et chercher son nom
    /// dans un titre changeant reviendrait à jouer aux devinettes.
    agent: bool,
}

impl TerminalView {
    /// Ouvre un pty. Séparé de la vue parce que c'est la seule étape qui peut
    /// échouer, et qu'un échec dans un constructeur d'entité ne laisse d'autre
    /// issue que la panique — pendant un rendu, donc avec la fenêtre figée
    /// pour seul message.
    pub fn open(
        working_directory: &Path,
        launch: &Launch,
        settings: &TerminalSettings,
    ) -> anyhow::Result<Terminal> {
        Terminal::spawn(Spawn {
            working_directory,
            // Un onglet ordinaire prend le programme des réglages ; une
            // commande explicite — l'agent — passe avant, elle est justement
            // ce qu'on a demandé à lancer.
            command: launch.command.clone().or_else(|| settings.program()),
            env: launch.env.clone(),
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
        agent: bool,
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
            scroll_remainder: 0.,
            pending_size: None,
            resize_scheduled: false,
            label,
            agent,
        }
    }

    pub fn is_agent(&self) -> bool {
        self.agent
    }

    /// Livre un texte au programme qui tourne, sans le valider.
    ///
    /// Passe par le **collage encadré** que gère `Terminal::paste` : sans lui,
    /// un texte multiligne arrive dans un shell comme autant de commandes
    /// validées, ce qui est la façon classique d'exécuter par accident ce
    /// qu'on voulait seulement faire lire.
    pub fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.terminal.paste(text);
        self.terminal.scroll_to_bottom();
        self.snapshot = self.terminal.snapshot();
        cx.notify();
    }

    /// Valide ce qui vient d'être collé.
    ///
    /// **Toujours dans un envoi séparé du collage**, jamais au bout du même :
    /// un TUI qui vient de recevoir un collage encadré peut avaler le retour
    /// chariot qui le suit dans le même paquet, et le message reste alors dans
    /// l'invite sans partir.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        self.terminal.write_str("\r");
        self.terminal.scroll_to_bottom();
        self.snapshot = self.terminal.snapshot();
        cx.notify();
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
    fn sync_size(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
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

        self.cell = gpui::size(cell_width.max(px(1.)), line_height);
        let (columns, lines) = grid_size(bounds.size, self.cell);
        self.request_size(
            TermSize::new(
                columns,
                lines,
                f32::from(cell_width) as u16,
                f32::from(line_height) as u16,
            ),
            cx,
        );
    }

    /// Transmet la nouvelle géométrie, une fois le glissement calmé.
    ///
    /// Un redimensionnement à la souris passe par toutes les largeurs
    /// intermédiaires. Les transmettre toutes revient à envoyer un `SIGWINCH`
    /// par image : le programme redessine à chaque fois, et comme il redessine
    /// *en place*, ses invites successives s'empilent au lieu de se remplacer.
    /// On attend donc que la taille se stabilise ; pendant ce temps, le
    /// panneau rogne l'ancienne grille, exactement comme le fait une fenêtre
    /// qu'on redimensionne.
    fn request_size(&mut self, size: TermSize, cx: &mut Context<Self>) {
        if self.terminal.size() == size {
            self.pending_size = None;
            return;
        }
        self.pending_size = Some(size);
        if self.resize_scheduled {
            return;
        }
        self.resize_scheduled = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESIZE_QUIET).await;
            let _ = this.update(cx, |this, cx| {
                this.resize_scheduled = false;
                if let Some(size) = this.pending_size.take() {
                    this.terminal.resize(size);
                    this.snapshot = this.terminal.snapshot();
                    cx.notify();
                }
            });
        })
        .detach();
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

    /// Vide l'historique et l'écran.
    ///
    /// La sortie de secours quand un programme a laissé le terminal dans un
    /// état illisible : le shell a bien un `clear`, mais il faut encore que
    /// son invite réponde.
    pub fn clear_scrollback(&mut self, cx: &mut Context<Self>) {
        self.terminal.clear();
        self.snapshot = self.terminal.snapshot();
        cx.notify();
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
        // La hauteur d'une cellule, et non celle du texte ambiant : c'est
        // elle qui donne le nombre de lignes d'un déplacement en pixels, et
        // elles diffèrent dès que le terminal n'a pas la taille de
        // l'interface.
        let cell = self.cell.height.max(px(1.));
        let lines = take_lines(
            &mut self.scroll_remainder,
            f32::from(event.delta.pixel_delta(cell).y),
            f32::from(cell),
        );
        if lines == 0 {
            return;
        }

        // Dans l'écran secondaire — un agent, `less`, `vim` — il n'y a pas
        // d'historique à remonter : la grille est ce que le programme dessine,
        // et lui seul sait ce qu'il y a au-dessus. La molette s'y traduit donc
        // en flèches, comme dans tous les terminaux ; sans quoi elle ne fait
        // rien du tout, ce qui est exactement ce qu'on nous a signalé.
        if self.terminal.in_alternate_screen() {
            let key = if lines > 0 { "up" } else { "down" };
            let repeats = lines.unsigned_abs() as usize * ALT_SCREEN_LINES;
            if let Some(bytes) = arrow_bytes(key, self.terminal.mode()) {
                for _ in 0..repeats {
                    self.terminal.write(bytes.clone());
                }
            }
            return;
        }

        self.terminal.scroll(lines);
        self.snapshot = self.terminal.snapshot();
        cx.notify();
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
                    let (copy, paste) = (entity.clone(), entity.clone());
                    let (all, clear) = (entity.clone(), entity.clone());
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
                    .item(PopupMenuItem::new(tr!("terminal-select-all")).on_click(
                        move |_, _window, cx| {
                            all.update(cx, |this, cx| this.select_all(cx));
                        },
                    ))
                    .item(PopupMenuItem::new(tr!("terminal-clear")).on_click(
                        move |_, _window, cx| {
                            clear.update(cx, |this, cx| this.clear_scrollback(cx));
                        },
                    ))
                }
            })
            .child(measure)
            .child(v_flex().size_full().overflow_hidden().children(lines))
            .children(self.render_cursor(focused, cx))
    }
}

/// Convertit un déplacement en pixels en lignes entières.
///
/// Le reliquat est conservé d'un événement à l'autre : un pavé tactile envoie
/// des fractions de ligne, et les arrondir chacune à zéro rend le défilement
/// inerte alors qu'elles finissent par faire des lignes.
pub fn take_lines(remainder: &mut f32, pixels: f32, cell: f32) -> i32 {
    *remainder += pixels / cell.max(1.);
    let lines = remainder.trunc();
    *remainder -= lines;
    lines as i32
}

/// Lignes envoyées par cran de molette dans l'écran secondaire.
///
/// Trois : la convention des terminaux, et ce que `less` comme `vim` traitent
/// comme un déplacement naturel.
const ALT_SCREEN_LINES: usize = 3;

/// Les octets d'une flèche, tels que le programme les attend.
fn arrow_bytes(key: &str, mode: alacritty_terminal::term::TermMode) -> Option<Vec<u8>> {
    crate::terminal::key_bytes(&gpui::Keystroke::parse(key).ok()?, mode)
}

/// Plancher de la grille : en deçà, le panneau rogne au lieu de demander au
/// programme de se replier dans un espace où il ne peut rien afficher.
const MIN_COLUMNS: usize = 20;
const MIN_LINES: usize = 3;

/// Combien de cellules tiennent dans cette place.
///
/// Le plancher n'est pas cosmétique : un panneau réduit à rien demanderait un
/// terminal de deux colonnes, où la moindre invite occupe cinquante lignes. Le
/// programme redessine, l'historique déborde, et il ne reste que des
/// fragments. En dessous, le panneau rogne — ce que fait aussi une fenêtre de
/// terminal qu'on rétrécit trop.
pub fn grid_size(space: gpui::Size<Pixels>, cell: gpui::Size<Pixels>) -> (usize, usize) {
    let columns = (space.width / cell.width.max(px(1.))) as usize;
    let lines = (space.height / cell.height.max(px(1.))) as usize;
    (columns.max(MIN_COLUMNS), lines.max(MIN_LINES))
}

/// Délai d'immobilité avant de transmettre une nouvelle géométrie au pty.
const RESIZE_QUIET: std::time::Duration = std::time::Duration::from_millis(150);

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

/// Ce qu'il faut pour ouvrir un onglet.
///
/// Un agrégat plutôt que quatre paramètres : un profil d'agent porte une
/// commande, des arguments, un environnement et un nom, et les faire voyager
/// séparément jusqu'au pty multipliait les occasions d'en oublier un.
pub struct Launch {
    /// `None` = le shell de connexion, ce qu'attend quelqu'un qui ouvre « un
    /// terminal ».
    pub command: Option<(String, Vec<String>)>,
    /// Variables ajoutées à l'environnement du pty. C'est par là que passe le
    /// modèle d'un profil d'agent.
    pub env: HashMap<String, String>,
    pub label: SharedString,
    /// Vrai quand cet onglet exécute un agent : c'est à lui que les notes de
    /// relecture seront livrées.
    pub agent: bool,
}

impl Launch {
    pub fn shell() -> Self {
        Self {
            command: None,
            env: HashMap::new(),
            label: tr!("terminal-shell"),
            agent: false,
        }
    }

    pub fn agent(profile: &crate::ui::settings::AgentProfile) -> Self {
        Self {
            command: Some(profile.spawn()),
            env: profile
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            label: SharedString::from(profile.label().to_string()),
            agent: true,
        }
    }
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
    ///
    /// `agent` dit si ce qu'on lance est un agent de codage : c'est à cet
    /// onglet-là que les notes de relecture seront livrées.
    pub fn open(&mut self, launch: Launch, window: &mut Window, cx: &mut Context<Self>) {
        // Un pty qu'on n'arrive pas à ouvrir est un problème système : limite
        // de descripteurs atteinte, `/dev/pts` absent. On renonce à l'onglet et
        // on le dit, plutôt que de paniquer au milieu d'un rendu — ce que
        // faisait ce code, avec pour seul symptôme une fenêtre figée.
        // Les réglages sont relus à l'ouverture plutôt que retenus à la
        // construction : changer le shell ou le défilement arrière doit valoir
        // pour le prochain onglet, sans avoir à fermer les autres.
        let settings = Settings::global(cx).terminal.clone();
        let terminal = match TerminalView::open(&self.worktree, &launch, &settings) {
            Ok(terminal) => terminal,
            Err(e) => {
                log::error!("ouverture du terminal : {e:#}");
                self.error = Some(SharedString::from(e.to_string()));
                cx.notify();
                return;
            }
        };
        let view =
            cx.new(|cx| TerminalView::attach(terminal, launch.label, launch.agent, window, cx));
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
        let Some(profile) = Settings::global(cx).terminal.default_profile().cloned() else {
            return;
        };
        self.open_profile(&profile, window, cx);
    }

    /// Ouvre un profil nommé.
    pub fn open_profile(
        &mut self,
        profile: &crate::ui::settings::AgentProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if profile.command.trim().is_empty() {
            return;
        }
        self.open(Launch::agent(profile), window, cx);
    }

    /// Livre un texte à l'agent de ce worktree, et le valide.
    ///
    /// S'il n'y a pas d'onglet d'agent, on en ouvre un — et l'envoi est
    /// **différé** : un agent met une seconde ou deux à afficher son invite, et
    /// ce qui arrive avant est lu par le shell qui n'a pas encore été remplacé,
    /// ou tout simplement perdu.
    pub fn send_to_agent(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        match self.agent_tab(cx) {
            Some(index) => {
                self.active = index;
                self.focus_active(window, cx);
                self.deliver(index, text, cx);
            }
            None => {
                self.open_agent(window, cx);
                let Some(index) = self.agent_tab(cx) else {
                    return;
                };
                self.active = index;
                cx.spawn(async move |group, cx| {
                    cx.background_executor().timer(AGENT_WARMUP).await;
                    let _ = group.update(cx, |group, cx| group.deliver(index, text, cx));
                })
                .detach();
            }
        }
    }

    /// L'onglet d'agent le plus récent qui tourne encore.
    ///
    /// Le plus récent : c'est celui qu'on regarde, et relancer un agent après
    /// en avoir quitté un est le geste normal quand la conversation s'est
    /// enlisée.
    fn agent_tab(&self, cx: &App) -> Option<usize> {
        self.tabs
            .iter()
            .enumerate()
            .rev()
            .find(|(_, tab)| {
                let tab = tab.read(cx);
                tab.is_agent() && !tab.has_exited()
            })
            .map(|(index, _)| index)
    }

    /// Colle, puis valide dans un **second** envoi.
    ///
    /// Les deux sont séparés par un court silence : un TUI qui vient de
    /// recevoir un collage encadré peut avaler un retour chariot arrivé dans
    /// la foulée, et le message resterait dans l'invite sans partir.
    fn deliver(&mut self, index: usize, text: String, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index).cloned() else {
            return;
        };
        tab.update(cx, |view, cx| view.paste_text(&text, cx));
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(SUBMIT_DELAY).await;
            let _ = tab.update(cx, |view, cx| view.submit(cx));
        })
        .detach();
        cx.notify();
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
                            // Un profil d'agent par entrée : le menu est le
                            // seul endroit où le choix se pose, et une liste
                            // qui vient des réglages évite d'avoir à les
                            // rouvrir pour lancer autre chose.
                            .dropdown_menu({
                                let entity = cx.entity();
                                move |menu, _window, cx| {
                                    let shell = entity.clone();
                                    let profiles = Settings::global(cx).terminal.agents.clone();
                                    let menu = menu.item(
                                        PopupMenuItem::new(tr!("terminal-new")).on_click(
                                            move |_, window, cx| {
                                                shell.update(cx, |this, cx| {
                                                    this.open(Launch::shell(), window, cx)
                                                });
                                            },
                                        ),
                                    );
                                    if profiles.is_empty() {
                                        return menu;
                                    }
                                    profiles
                                        .into_iter()
                                        .fold(menu.separator(), |menu, profile| {
                                            let entity = entity.clone();
                                            let label =
                                                SharedString::from(profile.label().to_string());
                                            menu.item(PopupMenuItem::new(label).on_click(
                                                move |_, window, cx| {
                                                    entity.update(cx, |this, cx| {
                                                        this.open_profile(&profile, window, cx)
                                                    });
                                                },
                                            ))
                                        })
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

impl ClaudhubApp {
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
            group.open(Launch::shell(), window, cx);
        });
        self.terminals.insert(worktree.to_path_buf(), group.clone());
        group
    }
}

#[cfg(test)]
mod tests {

    /// Un pavé tactile envoie des fractions de ligne : les perdre une à une
    /// rend le défilement inerte.
    #[test]
    fn fractions_of_a_line_add_up_instead_of_vanishing() {
        let mut remainder = 0.;
        assert_eq!(take_lines(&mut remainder, 6., 16.), 0);
        assert_eq!(take_lines(&mut remainder, 6., 16.), 0);
        assert_eq!(take_lines(&mut remainder, 6., 16.), 1);
        // Le trop-plein reste pour la suite plutôt que d'être jeté.
        assert!(remainder > 0.1 && remainder < 0.2);

        // Vers le bas, la même chose en négatif.
        let mut remainder = 0.;
        assert_eq!(take_lines(&mut remainder, -32., 16.), -2);
        assert_eq!(remainder, 0.);

        // Une hauteur de cellule absurde ne divise pas par zéro.
        let mut remainder = 0.;
        assert_eq!(take_lines(&mut remainder, 5., 0.), 5);
    }

    /// Le plancher n'est pas cosmétique : sous vingt colonnes, une invite de
    /// shell occupe des dizaines de lignes, le programme redessine, et le
    /// glissement de redimensionnement ne laisse que des fragments empilés.
    #[test]
    fn a_squeezed_panel_still_gets_a_usable_grid() {
        let cell = gpui::size(px(8.), px(16.));
        assert_eq!(grid_size(gpui::size(px(800.), px(320.)), cell), (100, 20));
        // Réduit à rien : on rogne plutôt que de demander deux colonnes.
        assert_eq!(grid_size(gpui::size(px(10.), px(4.)), cell), (20, 3));
        // Une cellule de largeur nulle ne divise pas par zéro.
        assert_eq!(
            grid_size(gpui::size(px(800.), px(320.)), gpui::size(px(0.), px(0.))),
            (800, 320)
        );
    }
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
