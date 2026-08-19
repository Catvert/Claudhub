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
    div, prelude::*, px, App, Bounds, Context, Entity, FocusHandle, Focusable, Hsla, KeyDownEvent,
    MouseButton, Pixels, Render, ScrollWheelEvent, SharedString, StyledText, TextRun, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Sizable,
};

use crate::terminal::{key_bytes, Paint, Snapshot, Spawn, TermSize, Terminal, TerminalEvent};
use crate::tr;
use crate::ui::app::PerchApp;
use crate::ui::icons::icon;
use crate::ui::settings::TerminalSettings;

/// Un onglet de terminal.
pub struct TerminalView {
    terminal: Terminal,
    snapshot: Snapshot,
    focus: FocusHandle,
    font_size: Pixels,
    /// Dernière taille connue de la zone de rendu, pour ne redimensionner le
    /// pty que quand la géométrie change vraiment.
    bounds: Bounds<Pixels>,
    label: SharedString,
}

impl TerminalView {
    pub fn new(
        working_directory: &Path,
        command: Option<(String, Vec<String>)>,
        settings: &TerminalSettings,
        label: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Self> {
        let font_size = px(settings.font_size);
        let terminal = Terminal::spawn(Spawn {
            working_directory,
            command,
            env: HashMap::new(),
            // La vraie taille arrive au premier rendu ; celle-ci ne sert qu'à
            // ce que le shell ait une géométrie plausible avant sa première
            // invite.
            size: TermSize::new(80, 24, 8, 16),
            scrollback: settings.scrollback,
        })?;

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

        Ok(Self {
            snapshot: terminal.snapshot(),
            terminal,
            focus: cx.focus_handle(),
            font_size,
            bounds: Bounds::default(),
            label,
        })
    }

    pub fn label(&self) -> SharedString {
        let title = self.terminal.title();
        if title.is_empty() {
            self.label.clone()
        } else {
            SharedString::from(title.to_string())
        }
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
            family: "JetBrains Mono".into(),
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
            // Toute frappe ramène en bas : c'est ce que fait un terminal, et
            // taper en ayant remonté l'historique sans que la vue suive serait
            // déroutant.
            self.terminal.scroll_to_bottom();
            self.terminal.write(bytes);
            self.snapshot = self.terminal.snapshot();
            cx.notify();
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let line_height = window.line_height().max(px(1.));
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let default_fg = cx.theme().foreground;
        let font_size = self.font_size;
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

        let lines: Vec<_> = self
            .snapshot
            .lines
            .iter()
            .map(|line| styled_line(line, default_fg, font_size))
            .collect();

        v_flex()
            .id("terminal")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .font_family("JetBrains Mono")
            .text_size(font_size)
            .on_key_down(cx.listener(Self::on_key))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| window.focus(&this.focus)),
            )
            .child(measure)
            .child(v_flex().size_full().overflow_hidden().children(lines))
    }
}

/// Convertit une ligne de l'instantané en texte stylé.
fn styled_line(line: &crate::terminal::Line, default_fg: Hsla, font_size: Pixels) -> StyledText {
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
                family: "JetBrains Mono".into(),
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
            background_color: match seg.bg {
                // Le fond par défaut est celui de la fenêtre : ne rien peindre
                // évite un rectangle par cellule.
                Paint::Default => None,
                Paint::Rgb(r, g, b) => Some(rgb(r, g, b)),
            },
            underline: seg.underline.then(gpui::UnderlineStyle::default),
            strikethrough: seg.strikethrough.then(gpui::StrikethroughStyle::default),
        });
    }
    let _ = font_size;
    StyledText::new(text).with_runs(runs)
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
    settings: TerminalSettings,
}

impl TerminalGroup {
    pub fn new(worktree: PathBuf, settings: TerminalSettings) -> Self {
        Self {
            worktree,
            tabs: Vec::new(),
            active: 0,
            settings,
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
        let worktree = self.worktree.clone();
        let settings = self.settings.clone();
        let view = cx.new(|cx| {
            TerminalView::new(&worktree, command, &settings, label, window, cx).unwrap_or_else(
                |e| {
                    // Un pty qu'on n'arrive pas à ouvrir est un problème système
                    // (limite de descripteurs, /dev/pts absent) : le dire et ne
                    // pas ouvrir d'onglet vaut mieux qu'un onglet mort.
                    log::error!("ouverture du terminal : {e:#}");
                    panic!("terminal indisponible : {e:#}");
                },
            )
        });
        self.tabs.push(view);
        self.active = self.tabs.len() - 1;
        self.focus_active(window, cx);
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
                    .child(div().flex_1())
                    .child(
                        Button::new("new-tab")
                            .ghost()
                            .xsmall()
                            .icon(icon("plus"))
                            .tooltip(tr!("terminal-new"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open(None, tr!("terminal-shell"), window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
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
        let settings = self.settings.terminal.clone();
        let path = worktree.to_path_buf();
        let group = cx.new(|_| TerminalGroup::new(path, settings));
        group.update(cx, |group, cx| {
            group.open(None, tr!("terminal-shell"), window, cx);
        });
        self.terminals.insert(worktree.to_path_buf(), group.clone());
        group
    }

    /// Ouvre un onglet exécutant l'agent de codage configuré.
    pub(super) fn open_agent_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let command = self.settings.terminal.agent_command.clone();
        if command.trim().is_empty() {
            return;
        }
        let mut parts = command.split_whitespace().map(str::to_string);
        let Some(program) = parts.next() else { return };
        let args: Vec<String> = parts.collect();
        let group = self.terminal_group(&worktree, window, cx);
        group.update(cx, |group, cx| {
            group.open(
                Some((program.clone(), args)),
                SharedString::from(program),
                window,
                cx,
            );
        });
        self.show_terminal = true;
        cx.notify();
    }
}
