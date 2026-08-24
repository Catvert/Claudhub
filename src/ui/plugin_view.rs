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

use gpui::{div, prelude::*, uniform_list, Context, ElementId, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
    v_flex, ActiveTheme, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _,
};

use crate::plugin::host::{Effect, Host, Request};
use crate::plugin::manifest::{self, Manifest, PanelSpec};
use crate::plugin::view::{Item, Node, Pair, TextStyle, Tone};
use crate::plugin::Plugin;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use rune::Value;

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

/// The plugins found on disk.
///
/// Behind a lock and not a `OnceLock`: installing one has to show up in the
/// settings page at once, without a restart. What is stored is a leaked slice
/// rather than a `Vec`, so `manifests()` can hand out `&'static` — a manifest
/// is read on every frame by every plugin's tab title, and cloning one there
/// would be a frame's work for nothing. The leak is bounded by the number of
/// installations in a session, which is a handful, and it is the same trade the
/// panel names already make.
static MANIFESTS: std::sync::RwLock<&'static [Manifest]> = std::sync::RwLock::new(&[]);

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
    discover();
}

/// Reads the plugin directory again. At startup, and after an installation or a
/// removal — never on a file changing, which is the script's business and not
/// the list's.
pub fn discover() {
    let Some(dir) = plugins_dir() else {
        return;
    };
    let found = manifest::discover(&dir);
    for found in &found {
        log::info!(target: "plugin", "{} on the {} screen", found.id, found.declaration.screen);
    }
    if let Ok(mut slot) = MANIFESTS.write() {
        *slot = Vec::leak(found);
    }
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
    MANIFESTS.read().map(|slot| *slot).unwrap_or_default()
}

/// The plugins whose panel belongs to one screen.
pub fn on_screen(screen: &str) -> impl Iterator<Item = &'static Manifest> + '_ {
    manifests()
        .iter()
        .filter(move |m| m.declaration.screen == screen)
}

/// The plugin a dock name belongs to, and which of its panels it is.
///
/// Two panels of one plugin share a script, a state and a set of settings; what
/// tells them apart is only the second half of the name, which the script gets
/// as `view`'s second argument.
pub fn by_panel(panel: &str) -> Option<(&'static Manifest, &'static PanelSpec)> {
    manifests()
        .iter()
        .find_map(|m| m.panel_named(panel).map(|spec| (m, spec)))
}

/// Is this plugin switched on. A plugin nobody has configured is **on**.
pub fn enabled(id: &str, cx: &gpui::App) -> bool {
    crate::ui::settings::Settings::global(cx)
        .plugins
        .get(id)
        .map(|p| p.enabled)
        .unwrap_or(true)
}

/// What the user still has to say before this plugin can work.
///
/// The manifest's defaults laid over by the settings, secrets included: from
/// here a setting and a secret are the same thing, something one has to say.
pub fn missing(manifest: &'static Manifest, cx: &gpui::App) -> Vec<&'static str> {
    // Borrowed, not copied: this is read per frame, once per plugin of the
    // screen, and only the value of a required field is ever needed.
    let configured = crate::ui::settings::Settings::global(cx)
        .plugins
        .get(&manifest.id);
    manifest.missing(&|name| {
        configured
            .and_then(|c| c.settings.get(name).or_else(|| c.secrets.get(name)))
            .or_else(|| manifest.declaration.settings.get(name))
            .cloned()
    })
}

/// Switched on **and** told what it needs.
///
/// A plugin missing a required field has no panel and does not run: landing on
/// an empty view and having to work out that the fault is a blank in another
/// page is the worst way to learn it.
pub fn usable(manifest: &'static Manifest, cx: &gpui::App) -> bool {
    enabled(&manifest.id, cx) && missing(manifest, cx).is_empty()
}

/// The same, from the panel's name — which is what the dock knows it by.
pub fn panel_enabled(panel: &str, cx: &gpui::App) -> bool {
    by_panel(panel).is_none_or(|(manifest, _)| usable(manifest, cx))
}

/// Does this screen have anything at all to show.
///
/// A screen carrying panels of its own always does. One that carries **only**
/// plugins — Sentry's, since Sentry became one — has something to show only
/// while one of them is usable; otherwise the bar points at an empty room.
pub fn screen_has_content(workspace: crate::ui::workspace::Workspace, cx: &gpui::App) -> bool {
    workspace.carries_own_panels() || on_screen(workspace.key()).any(|m| usable(m, cx))
}

/// The script of a plugin, as a root and a path.
///
/// The **directory** stands where a worktree usually does: `files::read` joins
/// the two, and the editor keys what it holds by that root. A plugin's folder is
/// simply another root — which is what makes editing one cost no new plumbing.
pub fn script_of(manifest: &Manifest) -> (PathBuf, PathBuf) {
    (manifest.dir.clone(), PathBuf::from("main.rn"))
}

impl ClaudhubApp {
    /// Loads every plugin and opens the lane their requests leave by.
    ///
    /// One outbox for all of them: the application has a single place to drain,
    /// and the answer finds its way home by the plugin's name.
    pub(super) fn start_plugins(&mut self, cx: &mut Context<Self>) {
        let (sender, outbox) = async_channel::unbounded::<Request>();
        self.plugin_outbox = Some(sender);
        self.rebuild_plugins(cx);
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
    }

    /// Loads every plugin the registry knows, and watches what it lives in.
    ///
    /// **One watch per plugin directory, and not one on their parent.**
    /// `Cmd::WatchDir` sets a watch on a folder **without recursion** — the
    /// notes vault's arrangement — so watching `<config>/plugins` alone sees a
    /// plugin appear and never sees its `main.rn` change. That is the whole
    /// hot reload, and it fired for nothing until each plugin's own folder was
    /// watched too. The parent is kept as well: it is what notices an install.
    pub(super) fn rebuild_plugins(&mut self, cx: &mut Context<Self>) {
        let Some(sender) = self.plugin_outbox.clone() else {
            return;
        };
        // The settings page shows each plugin's revision, and reads it through
        // a cache: a plugin appearing, going or moving is what makes it stale.
        crate::ui::settings_view::forget_plugin_revisions();
        self.plugins = manifests()
            .iter()
            .map(|manifest| Plugin::load(manifest.clone(), sender.clone()))
            .collect();
        self.configure_plugins(cx);
        self.plugin_deadlines.clear();
        if let Some(dir) = plugins_dir() {
            if dir.is_dir() {
                self.git.send(Cmd::WatchDir { dir });
            }
        }
        for manifest in manifests() {
            self.git.send(Cmd::WatchDir {
                dir: manifest.dir.clone(),
            });
        }
        let worktree = self.active.clone();
        for plugin in &mut self.plugins {
            plugin.set_worktree(worktree.as_deref());
        }
        cx.notify();
    }

    /// Installs, updates or removes a plugin.
    ///
    /// The directory is computed here and not in the worker: a name typed by
    /// hand ends in a `git clone` and, for a removal, in a `remove_dir_all`, so
    /// what may not leave the plugin directory is settled before anything is
    /// sent — `install::dir_of` is what refuses a `..`.
    pub(super) fn manage_plugin(
        &mut self,
        id: &str,
        op: crate::plugin::install::Manage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = plugins_dir() else {
            return;
        };
        match crate::plugin::install::dir_of(&root, id) {
            Ok(dir) => {
                // Through the in-flight machinery: a
                // clone takes seconds, and the status bar is where one looks to
                // know whether anything is happening. **The key is exactly the
                // pair the answer comes back with** — the directory and
                // `Action::Plugin` — which is what keeps a spinner from turning
                // for ever.
                self.start(
                    Some(dir.clone()),
                    crate::runtime::Action::Plugin,
                    Cmd::PluginManage { dir, op },
                    cx,
                );
            }
            Err(e) => self.plugin_failed(format!("{e:#}"), window, cx),
        }
        cx.notify();
    }

    /// What became of it.
    ///
    /// The registry is read again on success, so a plugin just installed is in
    /// the settings page at once — configurable, editable, and saying whether
    /// it compiles. Its **panel**, on the other hand, waits for a restart: a
    /// panel has to be in the dock's registry before `layout.json` is read, and
    /// there is no place in the docks for one that was not there when they were
    /// built. The page says so rather than letting one look for a tab.
    pub(super) fn plugin_managed(
        &mut self,
        dir: PathBuf,
        op: String,
        result: Result<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.flight
            .finish(&Some(dir.clone()), crate::runtime::Action::Plugin);
        match result {
            Ok(what) => {
                discover();
                self.rebuild_plugins(cx);
                self.leave_empty_workspace(window, cx);
                self.announce(SharedString::from(format!("{op}: {what}")), cx);
            }
            Err(message) => self.plugin_failed(message, window, cx),
        }
        cx.notify();
    }

    /// A failure gets its balloon, like every other one in this window: the
    /// status bar is overwritten by the next message, and "this repository
    /// carries no plugin.toml" is exactly what one comes back to read.
    fn plugin_failed(&mut self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        self.notify(
            crate::ui::notify::notice(
                crate::runtime::Action::Plugin,
                &message,
                crate::ui::notify::Level::Error,
            ),
            window,
            cx,
        );
        cx.notify();
    }

    /// Delivers the held prompt, if the worktree it targets has just arrived.
    ///
    /// Called on every `Evt::Worktrees`: it is the only signal that says `wt`
    /// has finished — creating a worktree runs hooks that take minutes, and
    /// nothing else marks their end.
    pub(super) fn deliver_awaited_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, _)) = self.awaiting_agent.as_ref() else {
            return;
        };
        let path = path.clone();
        if !self
            .repos
            .iter()
            .flat_map(|repo| repo.worktrees.iter())
            .any(|w| w.path == path)
        {
            return;
        }
        let Some((_, text)) = self.awaiting_agent.take() else {
            return;
        };
        self.select_worktree(path.clone(), window, cx);
        self.show_terminal_panel(window, cx);
        self.send_to_agent(&path, text, window, cx);
    }

    /// Puts a secret where it belongs, and records how to find it again.
    ///
    /// **The keyring is the default place**, and that is the point: typing a
    /// token in the form must not write it in clear into a file. What the
    /// settings then hold is a reference — `keyring:sentry.token` — and the
    /// value never touches the disk in readable form.
    ///
    /// A value that **names** where it lives (`$NOM`, `keyring:…`) is recorded
    /// as it stands: the user has already decided.
    ///
    /// **In the background**, never on the interface thread: opening a keyring
    /// can ask the user to unlock it, and the call blocks until they answer.
    /// And on **blur**, never on a keystroke — a D-Bus round trip per character
    /// would be absurd, and a locked keyring would ask once per letter.
    pub(super) fn keep_secret(
        &mut self,
        plugin: String,
        name: String,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::plugin::host::{KeyringEntry, SecretPlace};
        let entry = KeyringEntry::for_plugin(&plugin, &name);
        let value = value.trim().to_string();

        // Emptied, or already a reference: nothing to put away. What we had put
        // in the keyring for this name goes, so an emptied field leaves nothing
        // behind.
        if value.is_empty() || SecretPlace::of(&value).is_reference() {
            let forgotten = entry;
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = forgotten.forget() {
                        log::debug!(target: "plugin", "{} — {e}", forgotten.describe());
                    }
                })
                .detach();
            record_secret(&plugin, &name, &value, cx);
            return;
        }

        cx.spawn_in(window, async move |this, cx| {
            let stored = cx
                .background_executor()
                .spawn({
                    let (entry, value) = (entry, value.clone());
                    async move { entry.store(&value).map(|()| entry.reference()) }
                })
                .await;
            this.update_in(cx, |this, window, cx| match stored {
                Ok(reference) => record_secret(&plugin, &name, &reference, cx),
                Err(e) => {
                    // **Said, not silent.** Falling back to the settings file is
                    // what a machine with no keyring needs — a headless box, a
                    // session with no Secret Service — but a token that lands in
                    // clear when one asked for a keyring has to be announced,
                    // not discovered.
                    record_secret(&plugin, &name, &value, cx);
                    this.plugin_failed(
                        format!(
                            "{}: {e}\n{}",
                            tr!("settings-plugin-secret-failed"),
                            entry_hint()
                        ),
                        window,
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    /// Leaves a screen that has nothing left to show.
    ///
    /// Three moments make one: opening a window whose `layout.json` remembers a
    /// screen whose plugin is no longer configured, switching that plugin off,
    /// and uninstalling it. Staying would leave the bar pointing at a room with
    /// nothing in it, and no button lit.
    pub(super) fn leave_empty_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::ui::workspace::Workspace;
        if self.workspace == Workspace::Settings
            || crate::ui::plugin_view::screen_has_content(self.workspace, cx)
        {
            return;
        }
        let elsewhere = Workspace::working(cx).next().unwrap_or_default();
        self.enter_workspace(elsewhere, window, cx);
    }

    /// Has this plugin's panel nothing to show — see `Node::is_empty_state`.
    ///
    /// A plugin nobody loaded answers yes, which is the right answer: its tab
    /// would carry the empty column `tree_of` hands back.
    pub(super) fn plugin_panel_empty(&self, panel: &str) -> bool {
        let Some(index) = self.plugin_index(panel) else {
            return true;
        };
        let Some(spec) = self.plugins[index].manifest.panel_named(panel) else {
            return true;
        };
        self.plugins[index].tree_of(&spec.id).is_empty_state()
    }

    /// Asks before removing a plugin.
    ///
    /// `remove_dir_all` on a directory one may have edited: this is the one
    /// gesture here that git does not undo, the plugin's own commits included.
    pub(super) fn confirm_plugin_removal(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity();
        let label = SharedString::from(id.clone());
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (id, entity, label) = (id.clone(), entity.clone(), label.clone());
            dialog
                .title(tr!("settings-plugin-remove"))
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(label))
                        .child(div().text_xs().child(tr!("settings-plugin-remove-help"))),
                )
                .overlay_closable(false)
                .close_button(false)
                .footer(crate::ui::dialogs::confirm())
                .on_ok(move |_, window, cx| {
                    entity.update(cx, |this, cx| {
                        this.manage_plugin(&id, crate::plugin::install::Manage::Remove, window, cx);
                    });
                    true
                })
        });
    }

    /// Opens a plugin's script in the editing screen.
    ///
    /// The plugin's **directory** stands where a worktree usually does: the
    /// editor keys what it holds by a root, `files::read` joins the two, and a
    /// folder outside any repository is simply another root. Saving writes the
    /// file, the watch on that folder fires, and the script recompiles — the
    /// hot reload closes on itself without a single line about plugins in the
    /// editor.
    pub(super) fn edit_plugin(
        &mut self,
        manifest: &Manifest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (root, path) = script_of(manifest);
        self.editing_root = Some(root.clone());
        self.git.send(Cmd::ReadFile {
            worktree: root,
            path,
        });
        self.travel_reveal(crate::ui::workspace::Workspace::Files, window, cx);
        cx.notify();
    }

    /// Hands each plugin what the settings say about it.
    ///
    /// Called at startup and whenever the settings change: a token corrected in
    /// the form must not need a recompilation of the script that uses it.
    pub(super) fn configure_plugins(&mut self, cx: &mut Context<Self>) {
        let settings = crate::ui::settings::Settings::global(cx).plugins.clone();
        // What each plugin has remembered about the repository being looked at.
        // **It has to be handed back**, and that is the half that was missing:
        // `set_state` writes into the store *and* into the host's copy, so a
        // project chosen this afternoon worked for the rest of the session and
        // came back empty the next morning — the store had it, nobody read it.
        // Here rather than at the write: this is the pass that hands a plugin
        // what the window knows, and the repository changes under it too.
        let remembered = self
            .active
            .as_deref()
            .and_then(|worktree| self.main_of(worktree))
            .and_then(|main| {
                crate::ui::store::Store::global(cx)
                    .repos
                    .get(&main)
                    .map(|repo| repo.plugin_state.clone())
            })
            .unwrap_or_default();
        for plugin in &self.plugins {
            let configured = settings
                .get(&plugin.manifest.id)
                .cloned()
                .unwrap_or_default();
            plugin
                .shared()
                .configure(configured.settings, configured.secrets);
            plugin.shared().remember(
                remembered
                    .get(&plugin.manifest.id)
                    .cloned()
                    .unwrap_or_default(),
            );
        }
    }

    /// A file moved under the plugin directory.
    ///
    /// The script is recompiled and the plugin starts over. A compilation that
    /// fails keeps the machine that worked — an editor saves halfway through a
    /// word, and losing a working panel on every keystroke would make the
    /// reload worse than a restart.
    pub(super) fn reload_plugins(&mut self, cx: &mut Context<Self>) {
        // Something moved under a plugin's folder — very possibly a `git pull`
        // run outside Claudhub. See `settings_view::plugin_revision`.
        crate::ui::settings_view::forget_plugin_revisions();
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
        // What a plugin remembers is kept **per repository**, so changing
        // worktree can change it: the pass that hands a plugin what the window
        // knows has to run again. It is also what puts a project chosen in a
        // previous session back under the script before `init` replays.
        self.configure_plugins(cx);
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

    /// Which plugin a panel belongs to. Several panels answer with the same
    /// index: one script, one state, several windows onto it.
    fn plugin_index(&self, panel: &str) -> Option<usize> {
        self.plugins.iter().position(|p| p.manifest.owns(panel))
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
            || !usable_id(&self.plugins[index].manifest.id, cx)
        {
            return;
        }
        let Some(host) = self.plugins[index].host() else {
            return;
        };
        // Nobody asked for this from anywhere: it is the first load, and every
        // panel of the plugin is blank until it lands.
        self.plugins[index].start(None);
        self.wake_when_slow(cx);
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

    /// What a call needs: the machine and the state it runs against.
    ///
    /// `None` as soon as one of the two is missing — a plugin that has gone, a
    /// script that does not compile, a state not yet settled — and a gesture on
    /// any of those is a gesture that does nothing, which is right.
    fn plugin_call(&self, panel: &str) -> Option<(std::rc::Rc<Host>, Value)> {
        let index = self.plugin_index(panel)?;
        Some((self.plugins[index].host()?, self.plugins[index].state()?))
    }

    /// Records what a call came back with. The two gestures share it.
    fn plugin_outcome(&mut self, panel: &'static str, outcome: Result<Value, String>) {
        let Some(index) = self.plugin_index(panel) else {
            return;
        };
        match outcome {
            Ok(state) => self.plugins[index].settle(state),
            Err(message) => self.plugins[index].fail(message),
        }
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
        let Some((host, state)) = self.plugin_call(panel) else {
            return;
        };
        let Some(index) = self.plugin_index(panel) else {
            return;
        };
        // The panel the gesture came from keeps what it shows; the others wait.
        // See `Plugin::waiting_in`.
        self.plugins[index].start(Some(panel));
        self.wake_when_slow(cx);
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

    /// Asks for a frame when the grace period is over.
    ///
    /// Without it a call that really is slow would show nothing until the next
    /// frame something else asked for: the spinner is a function of the clock,
    /// and nothing else in the window ticks. One timer per gesture, and a
    /// gesture is a click.
    fn wake_when_slow(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::plugin::SHOW_AFTER)
                .await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }

    /// A keystroke's gesture.
    ///
    /// Separate from `plugin_gesture` because a subscription has no window, and
    /// because typing must not be able to open a terminal or an editor: the
    /// effects a keystroke produces are applied at the next settling, with the
    /// window the pump has.
    fn plugin_gesture_from_field(
        &mut self,
        panel: &'static str,
        action: String,
        typed: String,
        cx: &mut Context<Self>,
    ) {
        let Some((host, state)) = self.plugin_call(panel) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let outcome = host.update(&state, &action, &typed).await;
            this.update(cx, |this, cx| {
                this.plugin_outcome(panel, outcome);
                // The effects wait: a keystroke has no window to open anything
                // with, and the next settling drains them.
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn plugin_settled(
        &mut self,
        panel: &'static str,
        outcome: Result<Value, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.plugin_outcome(panel, outcome);
        let Some(index) = self.plugin_index(panel) else {
            return;
        };
        let effects = self.plugins[index].take_effects();
        let id = self.plugins[index].manifest.id.clone();
        self.apply_plugin_effects(&id, effects, window, cx);
        cx.notify();
    }

    /// Shows what a plugin is about to hand an agent, and lets it be edited.
    ///
    /// **The notes' own dialog, on the same field** (`prompt_input`): what goes
    /// into a terminal cannot be taken back — an agent has read the paste
    /// before one has seen what one just sent — and the two gestures are the
    /// same gesture.
    ///
    /// Here it earns a second reason. What a script writes is a **report**, not
    /// a request: a Sentry issue arrives with its trace, its context and its
    /// code, and what one wants to add is the one sentence that narrows it —
    /// start with the controller, leave the migrations alone, this only happens
    /// in production. That sentence has nowhere else to be written.
    ///
    /// An empty field sends nothing: emptying it is how one changes one's mind
    /// once the dialog is open.
    fn confirm_agent_prompt(
        &mut self,
        worktree: PathBuf,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.prompt_input.clone();
        let entity = cx.entity();
        input.update(cx, |input, cx| input.set_value(text, window, cx));
        window.open_dialog(cx, move |dialog, _window, _cx| {
            // Cloned into the closure and never read from it: `open_dialog`
            // keeps a `Fn` called back from the root's own render, where
            // reading the application is a panic. See "Conventions gpui".
            let input = input.clone();
            let entity = entity.clone();
            let worktree = worktree.clone();
            dialog
                .title(tr!("agent-prompt-title"))
                .child(
                    v_flex()
                        .gap_2()
                        .w(gpui::px(640.))
                        .child(div().text_xs().child(tr!("agent-prompt-hint")))
                        .child(gpui_component::input::Textarea::new(&input)),
                )
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .on_ok(move |_, window, cx| {
                    let text = input.read(cx).value().to_string();
                    if text.trim().is_empty() {
                        return true;
                    }
                    entity.update(cx, |this, cx| {
                        // Shown before it is sent: a message delivered into a
                        // hidden tab is a message nobody sees arrive. It is
                        // what the notes' `deliver` does, and for the same
                        // reason.
                        this.show_terminal_panel(window, cx);
                        this.send_to_agent(&worktree, text, window, cx);
                    });
                    true
                })
        });
        // The text is already there and it is meant to be added to: the caret
        // goes in the field.
        super::dialogs::focus_field(&self.prompt_input, window, cx);
    }

    /// What the script asked of the window, in the order it asked.
    fn apply_plugin_effects(
        &mut self,
        plugin: &str,
        effects: Vec<Effect>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A balloon says who is speaking, and the plugin's own title is the
        // only name the window and the script agree on — the panel's would name
        // one of two readings of the same state.
        let title = SharedString::from(
            manifests()
                .iter()
                .find(|manifest| manifest.id == plugin)
                .map(|manifest| manifest.declaration.title.clone())
                .unwrap_or_else(|| plugin.to_string()),
        );
        for effect in effects {
            match effect {
                Effect::Agent(text) => {
                    let Some(worktree) = self.active.clone() else {
                        continue;
                    };
                    self.confirm_agent_prompt(worktree, text, window, cx);
                }
                // The external editor when there is one, ours otherwise:
                // exactly what a Sentry frame does — a plugin's "open this
                // line" is the same gesture.
                Effect::Open { path, line } => {
                    if !crate::ui::settings::Settings::global(cx)
                        .external_editor
                        .trim()
                        .is_empty()
                    {
                        self.open_externally(path, line, cx);
                        continue;
                    }
                    // **At the line, not at the top of the file.** A frame
                    // names a line, and opening the file at its first one
                    // leaves the reader to find by hand what the trace had
                    // just told them — a Laravel controller is eight hundred
                    // lines. It goes through `jump_to`, so the trail knows
                    // where one came from: this is exactly the search
                    // results' gesture, and it is the one that needed
                    // `Ctrl+O` most, the panel one leaves being on another
                    // screen.
                    match line.checked_sub(1) {
                        Some(line) => self.jump_to(
                            path,
                            crate::ui::explorer::Landing::Position {
                                line: u32::try_from(line).unwrap_or(u32::MAX),
                                character: 0,
                            },
                            window,
                            cx,
                        ),
                        // Nothing said is the file itself, which is what a
                        // plugin opening a file rather than a place means.
                        None => self.open_in_editor(path, cx),
                    }
                }
                Effect::OpenUrl(url) => cx.open_url(&url),
                Effect::Copy(text) => cx.write_to_clipboard(gpui::ClipboardItem::new_string(text)),
                // **Titled by the plugin**, where the window's own messages
                // carry no title: a balloon that says "Sentry" above "ACME-7X
                // copié" says who is speaking, and a plugin is the one voice
                // here that is not the application's. It fades on its own — a
                // script has no level to tell a failure by, and everything
                // coming through here is something one asked for a moment ago.
                Effect::Notify(text) => {
                    window.push_notification(
                        gpui_component::notification::Notification::new()
                            .with_type(gpui_component::notification::NotificationType::Success)
                            .title(title.clone())
                            .message(SharedString::from(text))
                            .autohide(true),
                        cx,
                    );
                }
                // The worktree does not exist yet: the prompt is **held** and
                // delivered when the worktree list comes back, which is the
                // only signal saying `wt` has finished its hooks — they install
                // dependencies and take minutes.
                Effect::Worktree { name, prompt } => {
                    let Some(main) = self
                        .active
                        .as_deref()
                        .and_then(|worktree| self.main_of(worktree))
                    else {
                        continue;
                    };
                    if let Some(root) = self.wt_project(&main).map(|p| p.root.clone()) {
                        self.awaiting_agent = Some((root.join(&name), prompt));
                    }
                    self.start_worktree(main, name, None, window, cx);
                }
                // Written against the **repository**, which is what a project's
                // configuration belongs to. Nothing to do afterwards: the next
                // sweep hands it back to the script, and the script asked for
                // the write because it already holds the value.
                Effect::Remember { key, value } => {
                    let Some(main) = self
                        .active
                        .as_deref()
                        .and_then(|worktree| self.main_of(worktree))
                    else {
                        continue;
                    };
                    let id = plugin.to_string();
                    crate::ui::store::Store::update_global(cx, move |store| {
                        store
                            .repos
                            .entry(main)
                            .or_default()
                            .plugin_state
                            .entry(id)
                            .or_default()
                            .insert(key, value);
                    });
                    self.configure_plugins(cx);
                }
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
        // The panel's own title and icon, not the plugin's: a master/detail
        // plugin's two tabs must not read the same, and the tab beside them
        // says which is which.
        let spec = self.plugins[index].manifest.panel_named(panel);
        let title = SharedString::from(
            spec.map(|spec| spec.title.clone())
                .unwrap_or_else(|| panel.to_string()),
        );
        let icon_name = spec
            .map(|spec| spec.icon.clone())
            .unwrap_or_else(|| "puzzle".into());
        let tree = spec
            .map(|spec| self.plugins[index].tree_of(&spec.id))
            .unwrap_or_else(|| std::rc::Rc::new(Node::nothing()));
        // `working` and not `busy`: see `Plugin::working` — a gesture that
        // comes back in a frame turns nothing.
        let busy = self.plugins[index].working();
        let waiting_here = self.plugins[index].waiting_in(panel);
        let error = self.plugins[index].error.clone();
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
            // It **turns**, and that is the whole point: a static glyph beside
            // a title reads as decoration, and the two gestures this panel is
            // for — listing the errors, opening one — are each a round trip to
            // a remote API that takes a second or two.
            .when(busy, |el| {
                el.child(
                    Spinner::new()
                        .xsmall()
                        .icon(icon("loader-circle"))
                        .color(cx.theme().muted_foreground),
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
            // **Waiting is shown where the answer will land**, which is every
            // panel but the one the gesture came from — see
            // `Plugin::waiting_in`. Opening an issue leaves the list one
            // clicked in exactly as it was and makes the centre wait; the bar's
            // spinner turns on both, since it is the plugin that is busy.
            None if waiting_here => waiting(cx).into_any_element(),
            None => {
                // Painted before the scroll wrapper: `scrolled` takes `&mut
                // self` too, and the two borrows would meet.
                self.lay_out_plugin_code(panel, &tree, cx);
                let painted = self.render_plugin_node(panel, &mut 0, &tree, window, cx);
                // **A tree that fills does not get the panel's scroll.** The
                // filled child takes what is left and scrolls itself, so
                // wrapping the lot in a second scrolling box would put two
                // bars on one gesture and leave a band of nothing under the
                // list. What stays above it is a header, and a header does not
                // scroll.
                if tree.fills() {
                    return v_flex()
                        .size_full()
                        .child(bar)
                        .child(v_flex().flex_1().min_h_0().p_1().child(painted))
                        .into_any_element();
                }
                div()
                    .flex_1()
                    .min_h_0()
                    .child(
                        self.scrolled(
                            // **One key per panel, not one for all of them.**
                            // The key names the bar *and* keys the smoothing,
                            // and a master/detail plugin puts two panels on
                            // screen at once: sharing it made each frame
                            // advance one motion from two renders, against two
                            // different offsets, so the detail either jumped
                            // back where the list was or did not move at all.
                            // It is the rule `ui::surface` already pays for the
                            // two code surfaces.
                            SharedString::from(format!("plugin-scroll/{panel}")),
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
        seq: &mut usize,
        node: &Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // **A number per node, and it is what makes the ids unique.** gpui
        // keys a stateful element by its id among its siblings, and every code
        // block used to be called `plugin-code`: the second one silently took
        // the first one's scroll offset, so a stack trace's ten excerpts all
        // scrolled together. The walk is depth-first and the tree is rebuilt
        // identically between gestures, so the number is stable across frames —
        // which is what an id has to be.
        *seq += 1;
        let rank = *seq;
        match node {
            // **A column carrying a `Fill` claims its parent's free height.**
            // Left at `w_full` its height is the content's, so the filled
            // child had nothing to grow into and the list under it resolved
            // its `size_full` against an indefinite height — painted zero
            // pixels tall, under a count line that said fifty errors.
            Node::Column(children) => v_flex()
                .w_full()
                .gap_1()
                .when(node.fills(), |el| el.flex_1().min_h_0())
                .children(
                    children
                        .iter()
                        .map(|child| self.render_plugin_node(panel, seq, child, window, cx)),
                )
                .into_any_element(),
            Node::Row(children) => h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .children(
                    children
                        .iter()
                        .map(|child| self.render_plugin_node(panel, seq, child, window, cx)),
                )
                .into_any_element(),
            Node::Section {
                title,
                body,
                folded: starts_folded,
            } => {
                let key = format!("{panel}/{title}");
                // **What is kept is the exception, not the state.** The script
                // says which way a section starts — a distribution one glances
                // at once belongs behind its title, a trace does not — and the
                // panel remembers only the sections one has since turned the
                // other way. It is `tree::Folds`' polarity, and it keeps the
                // set small for the same reason.
                let folded = *starts_folded != self.plugin_folded.contains(&key);
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
                            body.iter().map(|child| {
                                self.render_plugin_node(panel, seq, child, window, cx)
                            }),
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
            Node::Code { .. } => self.render_plugin_code(rank, node, cx),
            Node::Badge { text, tone } => badge(text, *tone, cx).into_any_element(),
            Node::Callout { text, tone } => {
                let colour = tone_colour(*tone, cx);
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_start()
                    // A rule and not a filled block: what this says is *how* to
                    // read the sentence, and a full background would make it
                    // shout louder than the title above it.
                    .child(div().w(gpui::px(2.)).self_stretch().min_h_4().bg(colour))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(colour)
                            .child(SharedString::from(text.clone())),
                    )
                    .into_any_element()
            }
            Node::Fields { rows } => render_fields(rows, cx).into_any_element(),
            Node::Meter {
                label,
                value,
                percent,
            } => render_meter(label, value, *percent, cx).into_any_element(),
            Node::Fill(inner) => {
                // The list is asked for in fill mode rather than wrapped: a
                // height given to the wrapper would not reach past the fixed
                // one the list gives itself.
                let painted = match inner.as_ref() {
                    Node::List {
                        id,
                        items,
                        selected,
                        on_select,
                    } => self.render_plugin_list(
                        panel,
                        id,
                        items,
                        *selected,
                        on_select.as_ref(),
                        true,
                        window,
                        cx,
                    ),
                    other => self.render_plugin_node(panel, seq, other, window, cx),
                };
                // **A flex column and not a `div`**: gpui's default display is
                // `Block`, where the flex properties of a child are ignored —
                // the filled list was laid out at its content height, so the
                // `size_full` under it resolved against nothing and the panel
                // ended on the band of emptiness this whole node exists to
                // remove. It is what the root's own body already is.
                v_flex()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .child(painted)
                    .into_any_element()
            }
            Node::Divider => div()
                .w_full()
                .h(gpui::px(1.))
                .my_1()
                .bg(cx.theme().border)
                .into_any_element(),
            Node::Link {
                label,
                url,
                icon: name,
            } => {
                let target = url.clone();
                Button::new(("plugin-link", rank))
                    .ghost()
                    .small()
                    .label(SharedString::from(label.clone()))
                    .when_some(name.clone(), |button, name| button.icon(icon(&name)))
                    .on_click(move |_, _window, cx| cx.open_url(&target))
                    .into_any_element()
            }
            Node::List {
                id,
                items,
                selected,
                on_select,
            } => self.render_plugin_list(
                panel,
                id,
                items,
                *selected,
                on_select.as_ref(),
                false,
                window,
                cx,
            ),
            field @ Node::Field { .. } => self.render_plugin_field(panel, field, window, cx),
            Node::Button {
                label,
                icon: name,
                on_click,
                disabled,
                primary,
            } => {
                let handler = on_click.clone();
                Button::new(SharedString::from(format!("{panel}/{rank}/{label}")))
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
                .child(
                    Spinner::new()
                        .icon(icon("loader-circle"))
                        .color(cx.theme().muted_foreground),
                )
                .into_any_element(),
        }
    }

    /// Lays out every excerpt of a tree, once, outside the walk that paints it.
    ///
    /// **Never in the render closure**, which is the whole module's rule:
    /// parsing costs milliseconds, making the strings of a two hundred line
    /// trace costs hundreds of allocations, and a panel repaints on every
    /// frame. It is keyed by what the excerpt says, so a tree rebuilt
    /// identically after a gesture finds its lines again and pays nothing.
    ///
    /// **A tree that has not moved is not walked at all.** `Plugin::repaint`
    /// hands out a fresh `Rc` when the script settles and the same one
    /// otherwise, so pointer equality is the exact question — and it is also
    /// what tells the lists their rows are still good.
    ///
    /// The theme is the **witness**: a palette change is signalled to nobody —
    /// the lesson `ui::blade`'s editor cache already learned — and colours
    /// captured under the old one would stay until the next fetch.
    fn lay_out_plugin_code(
        &mut self,
        panel: &'static str,
        tree: &std::rc::Rc<Node>,
        cx: &mut Context<Self>,
    ) {
        let theme = cx.theme().highlight_theme.clone();
        match &self.plugin_code.theme {
            Some(seen) if std::sync::Arc::ptr_eq(seen, &theme) => {}
            _ => {
                self.plugin_code.by_text.clear();
                self.plugin_code.lists.clear();
                self.plugin_code.seen.clear();
                self.plugin_code.theme = Some(theme.clone());
            }
        }
        if self
            .plugin_code
            .seen
            .get(panel)
            .is_some_and(|seen| std::rc::Rc::ptr_eq(seen, tree))
        {
            return;
        }
        self.plugin_code.seen.insert(panel, tree.clone());
        self.plugin_code.epoch += 1;
        let epoch = self.plugin_code.epoch;
        self.plugin_code.epoch_of.insert(panel, epoch);

        let mut wanted = Vec::new();
        tree.excerpts(&mut wanted);
        for node in wanted {
            let key = code_key(node);
            if self.plugin_code.by_text.contains_key(&key) {
                continue;
            }
            self.plugin_code
                .by_text
                .insert(key, std::rc::Rc::new(PaintedCode::of(node, &theme)));
        }
        // A panel one has scrolled through a long trace may hold a hundred
        // excerpts; a plugin fetching in a loop would grow this without end.
        // The cap is generous and the eviction total: what a tree needs is put
        // back on the next frame, and a re-parse of what is on screen is what
        // an arrival already costs. The lists follow the same rule.
        if self.plugin_code.by_text.len() > MAX_CODE_CACHE {
            self.plugin_code.by_text.clear();
        }
        if self.plugin_code.lists.len() > MAX_CODE_CACHE {
            self.plugin_code.lists.clear();
        }
    }

    /// An excerpt: numbered, coloured, and with the line the trace named picked
    /// out.
    ///
    /// The numbers are a column of their own rather than part of the text: a
    /// text carrying them cannot be copied without them, and copying an excerpt
    /// to paste it into a file is what one does with it.
    ///
    /// Nothing here is computed: the lines were made by `lay_out_plugin_code`
    /// when the tree arrived, and this walks them. What is read from the theme
    /// stays here — a palette is three colours, and reading them per frame is
    /// what every other row of this window does.
    fn render_plugin_code(
        &mut self,
        rank: usize,
        node: &Node,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let painted = self.plugin_code.by_text.get(&code_key(node)).cloned();
        let theme = cx.theme().clone();
        let marked = theme.accent.opacity(0.6);
        let gutter = theme.muted_foreground.opacity(0.7);
        let lines = painted
            .iter()
            .flat_map(|code| code.lines.iter())
            .map(|line| {
                let here = line.marked;
                h_flex()
                    .w_full()
                    .when(here, |el| el.bg(marked))
                    .when_some(line.number.clone(), |el, number| {
                        el.child(
                            div()
                                .flex_none()
                                .px_2()
                                .text_color(if here { theme.foreground } else { gutter })
                                .child(number),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .pr_2()
                            .when(line.number.is_none(), |el| el.pl_2())
                            .child(if line.styles.is_empty() {
                                line.text.clone().into_any_element()
                            } else {
                                // The offsets are in **bytes** and the ranges
                                // sorted and disjoint — `with_highlights`
                                // checks neither, and gets both wrong in
                                // silence.
                                gpui::StyledText::new(line.text.clone())
                                    .with_highlights(line.styles.clone())
                                    .into_any_element()
                            }),
                    )
            })
            .collect::<Vec<_>>();
        div()
            .id(("plugin-code", rank))
            .w_full()
            .overflow_x_scroll()
            .py_1()
            .rounded(cx.theme().radius)
            .bg(cx.theme().secondary)
            .font_family(cx.theme().mono_font_family.clone())
            .text_xs()
            .whitespace_nowrap()
            .child(v_flex().min_w_full().children(lines))
            .into_any_element()
    }

    /// A line of text a plugin can be typed into.
    ///
    /// **The window owns the `InputState`, not the tree.** `use_keyed_state` is
    /// what the settings form already uses for its rows: the state is created
    /// on the first frame that asks for it under that key and handed back
    /// afterwards, so the caret and the selection survive a tree rebuilt on
    /// every gesture. The key carries the panel, so two plugins asking for a
    /// `project` are two fields.
    fn render_plugin_field(
        &mut self,
        panel: &'static str,
        field: &Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // The node whole rather than its four parts: a field's shape is one
        // thing, and spreading it over an argument list is what made clippy
        // count to eight.
        let Node::Field {
            id,
            value,
            placeholder,
            on_change,
        } = field
        else {
            return div().into_any_element();
        };
        let key = SharedString::from(format!("{panel}/field/{id}"));
        let entity = cx.entity();
        let action = on_change.as_ref().map(|handler| handler.action.clone());
        let (seed, hint) = (value.to_string(), placeholder.to_string());
        let state = window.use_keyed_state(key, cx, move |window, cx| {
            let input = cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx)
                    .placeholder(SharedString::from(hint))
                    .default_value(seed)
            });
            let subscription = action.map(|action| {
                cx.subscribe(
                    &input,
                    move |_: &mut PluginField,
                          input,
                          event: &gpui_component::input::InputEvent,
                          cx| {
                        if !matches!(event, gpui_component::input::InputEvent::Change) {
                            return;
                        }
                        // The typed text is the payload. Every keystroke, which
                        // is what makes a field a filter as much as a setting —
                        // `update` is synchronous unless the script awaits.
                        let typed = input.read(cx).value().to_string();
                        let action = action.clone();
                        entity.update(cx, |app, cx| {
                            app.plugin_gesture_from_field(panel, action, typed, cx)
                        });
                    },
                )
            });
            PluginField {
                input,
                _subscription: subscription,
            }
        });
        gpui_component::input::Input::new(&state.read(cx).input.clone())
            .small()
            .into_any_element()
    }

    /// A list, virtualised, with its bar and its wheel smoothing.
    ///
    /// `uniform_list` and an explicit row height, like every other list of this
    /// window: it finds the visible range by a division instead of walking a
    /// vector of sizes, and a virtualised list reserves exactly the height it
    /// is told — a row taller than announced covers the next one instead of
    /// pushing it down. That is why an item is two storeys at most.
    ///
    /// **It carries a bar and a smoothed wheel like every other list here**, and
    /// it had neither: a virtualised list says nothing about where one is, and
    /// a notch of three rows applied whole is the one place in this window
    /// where the wheel did not glide. The handle is the window's, kept under
    /// the panel's and the list's names together — two plugins asking for
    /// `issues` are two lists.
    ///
    /// `fill` says the list is what the panel is for: it takes the height that
    /// is left instead of the sixteen rows a list in a column has to guess at.
    #[allow(clippy::too_many_arguments)]
    fn render_plugin_list(
        &mut self,
        panel: &'static str,
        id: &str,
        items: &[Item],
        selected: Option<usize>,
        on_select: Option<&crate::plugin::view::Handler>,
        fill: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if items.is_empty() {
            return div().into_any_element();
        }
        let key = format!("{panel}/{id}");
        // **The rows are copied when the tree moves, not when a frame is
        // drawn.** `uniform_list`'s closure outlives the tree it was built
        // from, so the items have to be owned; copying them per frame meant
        // four `String` per row, for a list nobody had touched. The epoch is
        // the tree's — see `lay_out_plugin_code`.
        let epoch = self
            .plugin_code
            .epoch_of
            .get(panel)
            .copied()
            .unwrap_or_default();
        let rows = match self.plugin_code.lists.get(&key) {
            Some((seen, rows)) if *seen == epoch => rows.clone(),
            _ => {
                let rows = std::rc::Rc::new(PaintedList::of(items));
                self.plugin_code
                    .lists
                    .insert(key.clone(), (epoch, rows.clone()));
                rows
            }
        };
        let count = rows.items.len();
        let height = if rows.tall {
            crate::ui::theme::row_height(cx) * 1.8
        } else {
            crate::ui::theme::row_height(cx)
        };
        // Sixteen rows at most before the panel's own scroll takes over: a list
        // inside a scrolling column has no height of its own to grow into, and
        // one that took the whole panel would push everything under it out of
        // reach. A filled list has no such problem — it *is* what is left.
        let shown = count.min(16);
        let handle = self.plugin_lists.entry(key.clone()).or_default().clone();
        let action = on_select.map(|handler| handler.action.clone());
        let entity = cx.entity();
        let theme = cx.theme().clone();
        let element_id = ElementId::Name(SharedString::from(key.clone()));
        let list = uniform_list(element_id, count, move |range, _window, cx| {
            let mut painted = Vec::new();
            for index in range {
                let Some(item) = rows.items.get(index) else {
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
        .track_scroll(&handle)
        .size_full();
        let scrolled = self.scrolled(
            SharedString::from(format!("plugin-list/{key}")),
            &handle,
            crate::ui::motion::Axes::Vertical,
            window,
            list,
            cx,
        );
        // A flex column here too, for the same reason as the `Fill` node's
        // wrapper: what it holds asks for the whole of it.
        v_flex()
            .w_full()
            .map(|el| {
                if fill {
                    el.flex_1().min_h_0()
                } else {
                    el.h(height * shown as f32)
                }
            })
            .child(scrolled)
            .into_any_element()
    }
}

/// How many excerpts and lists are kept laid out before the lot is dropped.
const MAX_CODE_CACHE: usize = 256;

/// What a plugin's panels have already been laid out into, and the palette it
/// was done under.
///
/// Everything here is derived from a tree, and a tree only moves when the
/// script settles a new state — `Plugin::repaint` builds a fresh `Rc` then, and
/// never between two frames. `seen` is what says so, and `epoch` is what a
/// cache keyed by something other than the tree compares itself against.
#[derive(Default)]
pub struct CodeStyles {
    by_text: std::collections::HashMap<u64, std::rc::Rc<PaintedCode>>,
    /// The rows of each list, keyed by panel and list id, with the epoch they
    /// were copied at.
    lists: std::collections::HashMap<String, (u64, std::rc::Rc<PaintedList>)>,
    /// The tree each panel was last laid out from.
    seen: std::collections::HashMap<&'static str, std::rc::Rc<Node>>,
    /// Bumped every time a panel's tree moves.
    epoch: u64,
    epoch_of: std::collections::HashMap<&'static str, u64>,
    theme: Option<std::sync::Arc<gpui_component::highlighter::HighlightTheme>>,
}

/// An excerpt, ready to paint: one entry per line, numbers included.
///
/// **The strings are made here and not in the render closure.** A trace of two
/// hundred lines was costing two hundred `format!` and four hundred allocations
/// per frame, for a block whose text had not moved since the last gesture.
struct PaintedCode {
    lines: Vec<PaintedLine>,
}

impl PaintedCode {
    /// Lays an excerpt out: its lines, their numbers, and their colours.
    ///
    /// A language that names no grammar leaves the lines plain, and so does a
    /// node without one: guessing a grammar from the text is how a shell
    /// transcript ends up painted as Rust.
    fn of(node: &Node, theme: &gpui_component::highlighter::HighlightTheme) -> Self {
        let Node::Code {
            text,
            language,
            start_line,
            mark,
        } = node
        else {
            return Self { lines: Vec::new() };
        };
        let styles = language.as_ref().map(|language| {
            crate::ui::highlight::DocumentHighlights::for_language(language, text, theme)
        });
        // The width of the number column, from the last line: a gutter that
        // grows halfway down the excerpt would shift the code under it.
        let width = start_line
            .map(|first| format!("{}", first + text.lines().count()).len())
            .unwrap_or(0);
        let lines = text
            .lines()
            .enumerate()
            .map(|(index, line)| {
                let number = start_line.map(|first| first + index);
                PaintedLine {
                    marked: matches!((number, mark), (Some(n), Some(m)) if n == *m),
                    number: number.map(|number| SharedString::from(format!("{number:>width$}"))),
                    text: SharedString::from(line.to_string()),
                    styles: styles
                        .as_ref()
                        .map(|styles| styles.line(index).to_vec())
                        .unwrap_or_default(),
                }
            })
            .collect();
        Self { lines }
    }
}

/// A list's rows, ready to paint, and whether they are two storeys.
///
/// Built when the tree moves: `uniform_list`'s closure has to own its rows, and
/// copying the items per frame meant four `String` per row for a list nobody
/// had touched. `tall` goes with them — it is a reading of the same rows, and
/// the row height is what a virtualised list is told before anything is
/// painted.
struct PaintedList {
    tall: bool,
    items: Vec<PaintedItem>,
}

struct PaintedItem {
    title: SharedString,
    detail: Option<SharedString>,
    badge: Option<SharedString>,
    icon: Option<SharedString>,
}

impl PaintedList {
    fn of(items: &[Item]) -> Self {
        Self {
            tall: items.iter().any(|item| item.detail.is_some()),
            items: items
                .iter()
                .map(|item| PaintedItem {
                    title: SharedString::from(item.title.clone()),
                    detail: item.detail.clone().map(SharedString::from),
                    badge: item.badge.clone().map(SharedString::from),
                    icon: item.icon.clone().map(SharedString::from),
                })
                .collect(),
        }
    }
}

struct PaintedLine {
    /// The absolute number, right-aligned, when the excerpt is numbered.
    number: Option<SharedString>,
    text: SharedString,
    /// The line the trace named, painted the way a diff paints the row one is
    /// on.
    marked: bool,
    styles: Vec<(std::ops::Range<usize>, gpui::HighlightStyle)>,
}

/// A key for one excerpt. What it says, hashed — the text, its language and the
/// numbering: two frames of the same file quote different lines, and a path
/// would not tell them apart.
fn code_key(node: &Node) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    if let Node::Code {
        text,
        language,
        start_line,
        mark,
    } = node
    {
        language.hash(&mut hasher);
        text.hash(&mut hasher);
        start_line.hash(&mut hasher);
        mark.hash(&mut hasher);
    }
    hasher.finish()
}

/// The colour a tone reads in. Named by the theme and never by us: a palette
/// says what an error looks like, and half of them are not red.
fn tone_colour(tone: Tone, cx: &gpui::App) -> gpui::Hsla {
    match tone {
        Tone::Neutral => cx.theme().muted_foreground,
        Tone::Info => cx.theme().info,
        Tone::Success => cx.theme().success,
        Tone::Warning => cx.theme().warning,
        Tone::Danger => cx.theme().danger,
    }
}

/// A pill. Tinted on its own background rather than filled: a row of five
/// filled badges is a row of five things shouting, and what a badge does is
/// qualify the line it sits on.
fn badge(text: &str, tone: Tone, cx: &gpui::App) -> impl IntoElement {
    let colour = tone_colour(tone, cx);
    div()
        .flex_none()
        .px_1p5()
        .rounded(cx.theme().radius)
        .bg(colour.opacity(0.15))
        .text_xs()
        .text_color(colour)
        .whitespace_nowrap()
        .child(SharedString::from(text.to_string()))
}

/// A two-column table of names and values.
///
/// The names take a fixed share and the values the rest, which is the whole
/// point: eight values that line up are read at a glance, and eight that do not
/// are eight sentences.
fn render_fields(rows: &[Pair], cx: &gpui::App) -> impl IntoElement {
    let theme = cx.theme().clone();
    v_flex()
        .w_full()
        .rounded(theme.radius)
        .bg(theme.secondary.opacity(0.5))
        .children(rows.iter().enumerate().map(|(index, row)| {
            h_flex()
                .w_full()
                .px_2()
                .py_0p5()
                .gap_2()
                .items_start()
                // A rule between the lines and not around them: the block is
                // already a block, and a grid of boxes would be a spreadsheet.
                .when(index > 0, |el| el.border_t_1().border_color(theme.border))
                .child(
                    div()
                        .w(gpui::relative(0.34))
                        .flex_none()
                        .truncate()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(row.key.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(tone_colour(row.tone, cx))
                        .child(SharedString::from(row.value.clone())),
                )
        }))
}

/// A proportion: a label, a bar, and the value.
///
/// The bar is the reason this is a node and not a row of two texts — "22%" and
/// "100%" side by side are two numbers, and two bars are a comparison.
fn render_meter(label: &str, value: &str, percent: u8, cx: &gpui::App) -> impl IntoElement {
    let theme = cx.theme().clone();
    h_flex()
        .w_full()
        .px_1()
        .gap_2()
        .items_center()
        .text_xs()
        .child(
            div()
                .w(gpui::relative(0.3))
                .flex_none()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .flex_none()
                .w(gpui::relative(0.25))
                .h(gpui::px(6.))
                .rounded(theme.radius)
                .bg(theme.secondary)
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(f32::from(percent) / 100.))
                        .rounded(theme.radius)
                        .bg(theme.primary),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(SharedString::from(value.to_string())),
        )
}

/// One row of a plugin's list: a pill, never a band.
#[allow(clippy::too_many_arguments)]
fn plugin_row(
    index: usize,
    item: &PaintedItem,
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
        .pl_2()
        .pr(crate::ui::theme::scroll_gutter())
        .justify_center()
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
                        .child(item.title.clone()),
                )
                .when_some(item.badge.clone(), |el, badge| {
                    el.child(
                        div()
                            .px_1()
                            .rounded(theme.radius)
                            .bg(theme.secondary)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(badge),
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
                    .child(detail),
            )
        })
        .when_some(on_click, |el, on_click| {
            el.on_click(move |_, window, cx| on_click(window, cx))
        })
        .into_any_element()
}

/// What a panel shows while its script is waiting on something.
///
/// A spinner and nothing else: what a plugin is fetching has no name we know —
/// the script asked, not us — and inventing one ("Chargement des erreurs…")
/// would be Claudhub speaking for a plugin it does not read.
fn waiting(cx: &Context<ClaudhubApp>) -> impl IntoElement {
    h_flex()
        .flex_1()
        .min_h_0()
        .size_full()
        .justify_center()
        .items_center()
        .child(
            Spinner::new()
                .large()
                .icon(icon("loader-circle"))
                .color(cx.theme().muted_foreground),
        )
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

/// [`usable`], from an id — what a loaded plugin knows itself by.
fn usable_id(id: &str, cx: &gpui::App) -> bool {
    manifests()
        .iter()
        .find(|m| m.id == id)
        .is_some_and(|m| usable(m, cx))
}

/// A plugin field's input, owned by the window under its key.
struct PluginField {
    input: gpui::Entity<gpui_component::input::InputState>,
    _subscription: Option<gpui::Subscription>,
}

/// Writes what the settings file holds for one secret. An empty value takes the
/// entry out rather than leaving a blank one behind.
fn record_secret(plugin: &str, name: &str, value: &str, cx: &mut gpui::App) {
    let (plugin, name, value) = (plugin.to_string(), name.to_string(), value.to_string());
    crate::ui::settings::Settings::update_global(cx, move |settings| {
        let entry = settings.plugins.entry(plugin).or_default();
        if value.is_empty() {
            entry.secrets.remove(&name);
        } else {
            entry.secrets.insert(name, value);
        }
    });
}

/// What to try when the keyring refuses.
fn entry_hint() -> String {
    tr!("settings-plugin-secret-hint").to_string()
}

/// Section folds, kept in memory and not persisted: a reading posture that
/// changes several times in a session, like the notes panel's.
pub(super) type Folds = HashSet<String>;
