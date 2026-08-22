//! Painting what a plugin produced, and keeping its script running.
//!
//! The decision is in `plugin::view` and the script; nothing here decides
//! anything. What is here is a walk over a tree and the plumbing that gets one:
//! a script started when its panel is first looked at, a gesture turned into an
//! `update`, an answer put back where it was awaited.
//!
//! **The tree is not computed here.** `Plugin::tree` is produced when the state
//! moves and kept behind an `Rc`; the render closure runs on every frame and
//! only reads it. That is `diff_view::Rendered`'s rule, and the reason a plugin
//! cannot make the window slow by being slow itself.
//!
//! **The panels are discovered once, before the window opens.** A panel has to
//! be in the dock's registry before `layout.json` is read back, so adding or
//! removing a plugin takes a restart. What reloads hot is the **script**, which
//! is the thing one edits.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::OnceLock;

use gpui::{div, prelude::*, uniform_list, Context, ElementId, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _, StyledExt as _,
};

use crate::plugin::host::{Effect, Request};
use crate::plugin::manifest::{self, Manifest};
use crate::plugin::view::{Item, Node, TextStyle};
use crate::plugin::Plugin;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;

/// How long a capability may take before the panel is told nobody answered.
///
/// Generous, because the two capabilities are a remote API and a CLI that
/// itself talks to one — and finite, because a request nobody answers is a
/// panel that spins for good. The workers have ceilings of their own
/// (`caps::HTTP_TIMEOUT`, `caps::SHELL_TIMEOUT`); this one covers what those
/// cannot, a server that died with the request in its queue.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// The plugins Claudhub ships, written into the user's directory at startup.
///
/// Exactly the themes' arrangement, and the same corollary to state: these
/// files are **rewritten on every start**, so modifying one means copying it
/// under another name.
#[derive(rust_embed::RustEmbed)]
#[folder = "plugins"]
struct BundledPlugins;

static MANIFESTS: OnceLock<Vec<Manifest>> = OnceLock::new();

pub fn plugins_dir() -> Option<PathBuf> {
    crate::ui::settings::config_dir().map(|dir| dir.join("plugins"))
}

/// Writes the bundled plugins and reads every plugin there is. Once, at
/// startup, before the window is built.
pub fn install() {
    let Some(dir) = plugins_dir() else {
        return;
    };
    if let Err(e) = write_bundled(&dir) {
        log::warn!(target: "plugin", "bundled plugins not installed: {e}");
    }
    let found = manifest::discover(&dir);
    for found in &found {
        log::info!(target: "plugin", "{} on the {} screen", found.id, found.declaration.screen);
    }
    let _ = MANIFESTS.set(found);
}

fn write_bundled(dir: &std::path::Path) -> std::io::Result<()> {
    for name in BundledPlugins::iter() {
        let Some(file) = BundledPlugins::get(&name) else {
            continue;
        };
        let target = dir.join(name.as_ref());
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, file.data)?;
    }
    Ok(())
}

pub fn manifests() -> &'static [Manifest] {
    MANIFESTS.get().map(Vec::as_slice).unwrap_or_default()
}

/// The plugins whose panel belongs to one screen.
pub fn on_screen(screen: &str) -> impl Iterator<Item = &'static Manifest> {
    let screen = screen.to_string();
    manifests()
        .iter()
        .filter(move |m| m.declaration.screen == screen)
}

pub fn by_panel(panel: &str) -> Option<&'static Manifest> {
    manifests().iter().find(|m| m.panel == panel)
}

impl ClaudhubApp {
    /// Loads every plugin and opens the lane their requests leave by.
    ///
    /// One outbox for all of them: the application has a single place to drain,
    /// and the answer finds its way home by the plugin's name.
    pub(super) fn start_plugins(&mut self, cx: &mut Context<Self>) {
        let (sender, outbox) = async_channel::unbounded::<Request>();
        self.plugins = manifests()
            .iter()
            .map(|manifest| Plugin::load(manifest.clone(), sender.clone()))
            .collect();
        self.configure_plugins(cx);
        // The lane out: a request becomes a `Cmd`, and its deadline is noted.
        cx.spawn(async move |this, cx| {
            while let Ok(request) = outbox.recv().await {
                let alive = this
                    .update(cx, |this, _| {
                        this.plugin_deadlines.push((
                            request.plugin.clone(),
                            request.call,
                            std::time::Instant::now() + CALL_TIMEOUT,
                        ));
                        this.git.send(Cmd::PluginCall {
                            plugin: request.plugin,
                            call: request.call,
                            cap: request.cap,
                        });
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();

        // The plugin directory is watched exactly as the notes vault is: what
        // one edits in an editor must appear here without a restart.
        if let Some(dir) = plugins_dir() {
            if dir.is_dir() {
                self.git.send(Cmd::WatchDir { dir });
            }
        }
    }

    /// Hands each plugin what the settings say about it.
    ///
    /// Called at startup and whenever the settings change: a token corrected in
    /// the form must not need a recompilation of the script that uses it.
    pub(super) fn configure_plugins(&mut self, cx: &mut Context<Self>) {
        let settings = crate::ui::settings::Settings::global(cx).plugins.clone();
        for plugin in &self.plugins {
            let configured = settings
                .get(&plugin.manifest.id)
                .cloned()
                .unwrap_or_default();
            plugin
                .shared()
                .configure(configured.settings, configured.secrets);
        }
    }

    /// A file moved under the plugin directory.
    ///
    /// The script is recompiled and the plugin starts over. A compilation that
    /// fails keeps the machine that worked — an editor saves halfway through a
    /// word, and losing a working panel on every keystroke would make the
    /// reload worse than a restart.
    pub(super) fn reload_plugins(&mut self, cx: &mut Context<Self>) {
        for plugin in &mut self.plugins {
            plugin.reload();
        }
        self.plugin_deadlines.clear();
        self.configure_plugins(cx);
        cx.notify();
    }

    /// Tells every plugin which worktree the window is showing.
    pub(super) fn plugins_follow_worktree(&mut self, cx: &mut Context<Self>) {
        let worktree = self.active.clone();
        for plugin in &mut self.plugins {
            plugin.set_worktree(worktree.as_deref());
        }
        self.plugin_deadlines.clear();
        cx.notify();
    }

    /// A capability's answer, back from a worker.
    pub(super) fn plugin_result(
        &mut self,
        plugin: String,
        call: u64,
        result: Result<String, String>,
    ) {
        self.plugin_deadlines
            .retain(|(id, pending, _)| !(*id == plugin && *pending == call));
        if let Some(found) = self.plugins.iter().find(|p| p.manifest.id == plugin) {
            found.shared().resolve(call, result);
        }
    }

    /// Fails what has waited too long.
    ///
    /// Called from the background sweep rather than from a timer per request:
    /// the sweep already beats every two seconds, and a ninety-second ceiling
    /// has no use for finer grain. **A request never stays pending** is the
    /// rule; where the clock comes from is not part of it.
    pub(super) fn expire_plugin_calls(&mut self) {
        let now = std::time::Instant::now();
        let (expired, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.plugin_deadlines)
            .into_iter()
            .partition(|(_, _, deadline)| *deadline <= now);
        self.plugin_deadlines = kept;
        for (id, call, _) in expired {
            if let Some(found) = self.plugins.iter().find(|p| p.manifest.id == id) {
                found
                    .shared()
                    .resolve(call, Err("no answer within ninety seconds".into()));
            }
        }
    }

    fn plugin_index(&self, panel: &str) -> Option<usize> {
        self.plugins.iter().position(|p| p.manifest.panel == panel)
    }

    /// Starts a plugin the first time its panel is looked at.
    ///
    /// At render time and once only, like the Sentry panel's first load: a
    /// script that fetches has no business running for a tab nobody opened, nor
    /// once per frame.
    fn plugin_boot(&mut self, panel: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.plugin_index(panel) else {
            return;
        };
        if self.plugins[index].busy
            || self.plugins[index].started()
            || self.plugins[index].error.is_some()
        {
            return;
        }
        let Some(host) = self.plugins[index].host() else {
            return;
        };
        self.plugins[index].busy = true;
        let worktree = self.active.clone();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = host.init(worktree.as_deref()).await;
            this.update_in(cx, |this, window, cx| {
                this.plugin_settled(panel, outcome, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// A gesture: `update(state, action, payload)`, then repaint.
    fn plugin_gesture(
        &mut self,
        panel: &'static str,
        action: String,
        payload: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.plugin_index(panel) else {
            return;
        };
        let (Some(host), Some(state)) = (self.plugins[index].host(), self.plugins[index].state())
        else {
            return;
        };
        self.plugins[index].busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = host.update(&state, &action, &payload).await;
            this.update_in(cx, |this, window, cx| {
                this.plugin_settled(panel, outcome, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn plugin_settled(
        &mut self,
        panel: &'static str,
        outcome: Result<rune::Value, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.plugin_index(panel) else {
            return;
        };
        self.plugins[index].busy = false;
        match outcome {
            Ok(state) => self.plugins[index].settle(state),
            Err(message) => self.plugins[index].fail(message),
        }
        let effects = self.plugins[index].take_effects();
        self.apply_plugin_effects(effects, window, cx);
        cx.notify();
    }

    /// What the script asked of the window, in the order it asked.
    fn apply_plugin_effects(
        &mut self,
        effects: Vec<Effect>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for effect in effects {
            match effect {
                Effect::Agent(text) => {
                    let Some(worktree) = self.active.clone() else {
                        continue;
                    };
                    // Shown before it is sent: a message delivered into a
                    // hidden tab is a message nobody sees arrive. It is what
                    // the notes' `deliver` does, and for the same reason.
                    self.show_terminal_panel(window, cx);
                    self.send_to_agent(&worktree, text, window, cx);
                }
                // The external editor when there is one, ours otherwise:
                // exactly what a Sentry frame does — a plugin's "open this
                // line" is the same gesture.
                Effect::Open { path, line } => {
                    if crate::ui::settings::Settings::global(cx)
                        .external_editor
                        .trim()
                        .is_empty()
                    {
                        self.open_in_editor(path, cx);
                    } else {
                        self.open_externally(path, line, cx);
                    }
                }
                Effect::Notify(text) => self.announce(SharedString::from(text), cx),
            }
        }
    }

    // — Painting ————————————————————————————————————————————————————————

    pub(super) fn render_plugin(
        &mut self,
        panel: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.plugin_boot(panel, window, cx);
        let Some(index) = self.plugin_index(panel) else {
            return empty_panel(tr!("plugin-missing"), cx).into_any_element();
        };
        let title = SharedString::from(self.plugins[index].manifest.title().to_string());
        let icon_name = self.plugins[index].manifest.icon().to_string();
        let busy = self.plugins[index].busy;
        let error = self.plugins[index].error.clone();
        let tree = self.plugins[index].tree.clone();
        let scroll = self.scroll_of(panel);

        let bar = h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon(&icon_name).xsmall())
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(title),
            )
            .when(busy, |el| {
                el.child(
                    icon("loader-circle")
                        .xsmall()
                        .text_color(cx.theme().muted_foreground),
                )
            })
            .child(
                Button::new("plugin-reload")
                    .ghost()
                    .xsmall()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-refresh"))
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        if let Some(index) = this.plugin_index(panel) {
                            this.plugins[index].reload();
                            cx.notify();
                        }
                    })),
            );

        // The error goes **in the panel** and not in the status bar, which the
        // next message overwrites: a script that does not compile is exactly
        // what one comes back to read twice. Same choice as the database tree's.
        let body = match error {
            Some(message) => v_flex()
                .flex_1()
                .min_h_0()
                .p_2()
                .gap_1()
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .text_color(cx.theme().danger)
                        .child(icon("triangle-alert").xsmall())
                        .child(div().text_xs().child(tr!("plugin-failed"))),
                )
                .child(
                    div()
                        .id("plugin-error")
                        .size_full()
                        .overflow_scroll()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(message)),
                )
                .into_any_element(),
            None => {
                // Painted before the scroll wrapper: `scrolled` takes `&mut
                // self` too, and the two borrows would meet.
                let painted = self.render_plugin_node(panel, &tree, cx);
                div()
                    .flex_1()
                    .min_h_0()
                    .child(
                        self.scrolled(
                            "plugin-scroll",
                            &scroll,
                            crate::ui::motion::Axes::Vertical,
                            window,
                            v_flex()
                                .id("plugin-body")
                                .size_full()
                                .overflow_y_scroll()
                                .track_scroll(&scroll)
                                .p_1()
                                .child(painted),
                            cx,
                        ),
                    )
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .child(bar)
            .child(body)
            .into_any_element()
    }

    /// One node. Recursive, and the recursion is what the bounded vocabulary
    /// buys: there are ten shapes and none of them can invent an eleventh.
    fn render_plugin_node(
        &mut self,
        panel: &'static str,
        node: &Node,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match node {
            Node::Column(children) => v_flex()
                .w_full()
                .gap_1()
                .children(
                    children
                        .iter()
                        .map(|child| self.render_plugin_node(panel, child, cx)),
                )
                .into_any_element(),
            Node::Row(children) => h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .children(
                    children
                        .iter()
                        .map(|child| self.render_plugin_node(panel, child, cx)),
                )
                .into_any_element(),
            Node::Section { title, body } => {
                let key = format!("{panel}/{title}");
                let folded = self.plugin_folded.contains(&key);
                let header = h_flex()
                    .id(SharedString::from(key.clone()))
                    .w_full()
                    .gap_1()
                    .items_center()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        icon(if folded {
                            "chevron-right"
                        } else {
                            "chevron-down"
                        })
                        .xsmall(),
                    )
                    .child(SharedString::from(title.clone()))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        if !this.plugin_folded.remove(&key) {
                            this.plugin_folded.insert(key.clone());
                        }
                        cx.notify();
                    }));
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(header)
                    .when(!folded, |el| {
                        el.children(
                            body.iter()
                                .map(|child| self.render_plugin_node(panel, child, cx)),
                        )
                    })
                    .into_any_element()
            }
            Node::Text { text, style } => {
                let text = SharedString::from(text.clone());
                match style {
                    TextStyle::Title => div().px_1().text_sm().font_semibold().child(text),
                    TextStyle::Body => div().px_1().text_sm().child(text),
                    TextStyle::Dim => div()
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(text),
                    TextStyle::Mono => div()
                        .px_1()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(text),
                }
                .into_any_element()
            }
            // The language is carried and not yet used: colouring an excerpt
            // means going through `ui::highlight`, which wants a whole
            // buffer's worth of context to say anything useful. The field is
            // there so a plugin written today keeps meaning what it says.
            Node::Code { text, .. } => div()
                .id("plugin-code")
                .w_full()
                .overflow_x_scroll()
                .p_2()
                .rounded(cx.theme().radius)
                .bg(cx.theme().secondary)
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .whitespace_nowrap()
                .child(SharedString::from(text.clone()))
                .into_any_element(),
            Node::List {
                id,
                items,
                selected,
                on_select,
            } => self.render_plugin_list(panel, id, items, *selected, on_select.as_ref(), cx),
            Node::Button {
                label,
                icon: name,
                on_click,
                disabled,
                primary,
            } => {
                let handler = on_click.clone();
                Button::new(SharedString::from(format!("{panel}/{label}")))
                    .small()
                    .label(SharedString::from(label.clone()))
                    .when_some(name.clone(), |button, name| button.icon(icon(&name)))
                    .map(|button| {
                        if *primary {
                            button.primary()
                        } else {
                            button.outline()
                        }
                    })
                    .disabled(*disabled || handler.is_none())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(handler) = handler.clone() {
                            this.plugin_gesture(panel, handler.action, handler.payload, window, cx);
                        }
                    }))
                    .into_any_element()
            }
            Node::Empty { message } => v_flex()
                .w_full()
                .py_4()
                .items_center()
                .gap_1()
                .text_color(cx.theme().muted_foreground)
                .child(icon("inbox"))
                .child(div().text_sm().child(SharedString::from(message.clone())))
                .into_any_element(),
            Node::Spinner => h_flex()
                .w_full()
                .py_4()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(icon("loader-circle"))
                .into_any_element(),
        }
    }

    /// A list, virtualised.
    ///
    /// `uniform_list` and an explicit row height, like every other list of this
    /// window: it finds the visible range by a division instead of walking a
    /// vector of sizes, and a virtualised list reserves exactly the height it
    /// is told — a row taller than announced covers the next one instead of
    /// pushing it down. That is why an item is two storeys at most.
    fn render_plugin_list(
        &mut self,
        panel: &'static str,
        id: &str,
        items: &[Item],
        selected: Option<usize>,
        on_select: Option<&crate::plugin::view::Handler>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if items.is_empty() {
            return div().into_any_element();
        }
        let rows: std::rc::Rc<Vec<Item>> = std::rc::Rc::new(items.to_vec());
        let count = rows.len();
        let tall = rows.iter().any(|item| item.detail.is_some());
        let height = if tall {
            crate::ui::theme::row_height(cx) * 1.8
        } else {
            crate::ui::theme::row_height(cx)
        };
        // Sixteen rows at most before the panel's own scroll takes over: a list
        // inside a scrolling column has no height of its own to grow into, and
        // one that took the whole panel would push everything under it out of
        // reach.
        let shown = count.min(16);
        let action = on_select.map(|handler| handler.action.clone());
        let entity = cx.entity();
        let theme = cx.theme().clone();
        let element_id = ElementId::Name(SharedString::from(format!("{panel}/{id}")));
        div()
            .w_full()
            .h(height * shown as f32)
            .child(
                uniform_list(element_id, count, move |range, _window, cx| {
                    let mut painted = Vec::new();
                    for index in range {
                        let Some(item) = rows.get(index) else {
                            continue;
                        };
                        painted.push(plugin_row(
                            index,
                            item,
                            selected == Some(index),
                            height,
                            &theme,
                            action.clone().map(|action| {
                                let entity = entity.clone();
                                move |window: &mut Window, cx: &mut gpui::App| {
                                    entity.update(cx, |this, cx| {
                                        this.plugin_gesture(
                                            panel,
                                            action.clone(),
                                            index.to_string(),
                                            window,
                                            cx,
                                        );
                                    });
                                }
                            }),
                            cx,
                        ));
                    }
                    painted
                })
                .size_full()
                // The inset belongs to the **list**: `uniform_list` computes its
                // items' size itself and ignores their margins, so a row can
                // only carry its radius.
                .px_1(),
            )
            .into_any_element()
    }
}

/// One row of a plugin's list: a pill, never a band.
#[allow(clippy::too_many_arguments)]
fn plugin_row(
    index: usize,
    item: &Item,
    selected: bool,
    height: gpui::Pixels,
    theme: &gpui_component::theme::Theme,
    on_click: Option<impl Fn(&mut Window, &mut gpui::App) + 'static>,
    _cx: &mut gpui::App,
) -> gpui::AnyElement {
    let clickable = on_click.is_some();
    v_flex()
        .id(("plugin-row", index))
        .h(height)
        .w_full()
        .px_2()
        .justify_center()
        .rounded(theme.radius)
        .when(clickable, |el| el.cursor_pointer())
        .when(selected, |el| el.bg(theme.accent))
        .when(clickable && !selected, |el| {
            el.hover(|s| s.bg(theme.accent.opacity(0.4)))
        })
        .child(
            h_flex()
                .w_full()
                .gap_1()
                .items_center()
                .when_some(item.icon.clone(), |el, name| {
                    el.child(icon(&name).xsmall().text_color(theme.muted_foreground))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(SharedString::from(item.title.clone())),
                )
                .when_some(item.badge.clone(), |el, badge| {
                    el.child(
                        div()
                            .px_1()
                            .rounded(theme.radius)
                            .bg(theme.secondary)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(badge)),
                    )
                }),
        )
        .when_some(item.detail.clone(), |el, detail| {
            el.child(
                div()
                    .w_full()
                    .truncate()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(detail)),
            )
        })
        .when_some(on_click, |el, on_click| {
            el.on_click(move |_, window, cx| on_click(window, cx))
        })
        .into_any_element()
}

fn empty_panel(message: SharedString, cx: &Context<ClaudhubApp>) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(icon("puzzle"))
        .child(div().text_sm().px_4().child(message))
}

/// Section folds, kept in memory and not persisted: a reading posture that
/// changes several times in a session, like the notes panel's.
pub(super) type Folds = HashSet<String>;
