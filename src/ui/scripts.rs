//! The focus view's scripts, run — see `crate::scripts` for what one is.
//!
//! One gpui-shell runtime for the window, created at the first script shown;
//! one `ScriptView` per script **and per board**, each born under a policy of
//! its own, whose `claudhub` module is bound to that board's worktree. A host
//! function cannot ask which view called it; a policy is what a view carries.
//!
//! **The script lays out, Rust paints and acts.** The module reads what the
//! application already knows, and its actions are the application's own
//! methods, which send `Cmd`s as a click would: a script never runs git. Its
//! components are the board's own cards and tabs, so a home is rearranged
//! without being rewritten.
//!
//! Three moments, each with its trap:
//!
//! - **Mounting runs the script's `init`, which may call the module, which
//!   reads the application** — so never from the application's render, where
//!   it is leased: a mount is deferred to the window (`Window::defer`).
//! - **A component paints with the application's methods**, from the script
//!   view's render, once the application's own has returned. It *reads* the
//!   application first: what a view reads while it renders is what repaints
//!   it when it changes.
//! - **What the script reads changes with the events**: after each batch, the
//!   views are refreshed — the script runs again (`refresh_scripts`). A bare
//!   repaint only materialises the description it already has.
//!
//! The scripts' folder is read off the thread every half second. A change to
//! a script's sources mounts it again; a broken save leaves the view that
//! worked on screen, and says why in a bubble. Nothing here reaches the
//! scripts' own grants: the policy permits no file, process nor network —
//! what a script may do is what the module offers.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::{v_flex, ActiveTheme};
use gpui_kit::{
    div, prelude::*, AnyElement, AnyWindowHandle, App, Context, Entity, SharedString, WeakEntity,
    Window,
};
use gpui_shell::policy::Policy;
use gpui_shell::{
    Capabilities, ComponentArgs, HostArguments, HostError, HostModule, HostObject, HostResult,
    HostValue, HttpRequestGrant, ScriptView, ShellRuntime,
};

use crate::scripts::{self, Kind, Script, Stamp};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::focus::View;
use crate::ui::overview::{AtWork, Doing};

/// How often the scripts' folder is read again.
const POLL: Duration = Duration::from_millis(500);

/// Where the scripts are: one folder each, under the configuration.
pub fn root() -> Option<PathBuf> {
    super::settings::config_dir().map(|dir| dir.join("scripts"))
}

/// The scripts written into the folder the first time: something to see,
/// and to change.
pub(super) const EXAMPLES: &[(&str, &str)] = &[
    (
        "home-columns/claudhub.json",
        include_str!("../../assets/scripts/home-columns/claudhub.json"),
    ),
    (
        "home-columns/main.js",
        include_str!("../../assets/scripts/home-columns/main.js"),
    ),
    (
        "dashboard/claudhub.json",
        include_str!("../../assets/scripts/dashboard/claudhub.json"),
    ),
    (
        "dashboard/main.js",
        include_str!("../../assets/scripts/dashboard/main.js"),
    ),
    (
        "sentry/claudhub.json",
        include_str!("../../assets/scripts/sentry/claudhub.json"),
    ),
    (
        "sentry/main.js",
        include_str!("../../assets/scripts/sentry/main.js"),
    ),
    (
        "sentry/sentry.js",
        include_str!("../../assets/scripts/sentry/sentry.js"),
    ),
    (
        "sentry/texts.js",
        include_str!("../../assets/scripts/sentry/texts.js"),
    ),
];

/// A board's view of one script: `(worktree, script id)`.
type Key = (PathBuf, String);

#[derive(Default)]
pub(crate) struct Scripts {
    /// Created at the first mount: a window that shows no script pays for
    /// no VM.
    runtime: Option<Rc<ShellRuntime>>,
    found: Vec<(Script, Stamp)>,
    broken: Vec<(String, String)>,
    mounted: HashMap<Key, Mounted>,
    /// Asked for and not answered yet: one deferred mount at a time each.
    mounting: HashSet<Key>,
    /// What the last mount of these sources said: not tried again before
    /// they change.
    failed: HashMap<Key, (Stamp, String)>,
    watching: bool,
    /// What the agents of the boards on show were doing at the last render:
    /// the terminals' components and `terminals()` read it.
    pub(super) at_work: AtWork,
    /// The Plugins screen — see `ui::plugins_view`.
    pub(super) screen: Screen,
    /// The last text written to the status file: written again only when it
    /// says something else.
    status: String,
    /// Each script's data — `storage_*` —, by its id: one map whatever the
    /// boards it is drawn on, read from disk at its first use.
    stores: HashMap<String, serde_json::Map<String, serde_json::Value>>,
    /// Where the stores' writes queue, one after the other — two written
    /// side by side could land in the wrong order.
    writer: Option<async_channel::Sender<(PathBuf, String)>>,
    /// The hosts each script asked for while running (`request_network`),
    /// beyond its manifest's.
    pub(super) requested: HashMap<String, Vec<String>>,
}

/// Where scripts' data lives: beside their folder, not in it — an update of
/// a script, or the agent editing it, never reaches its data.
fn data_root() -> Option<PathBuf> {
    super::settings::config_dir().map(|dir| dir.join("scripts-data"))
}

/// One scroll area a script view drew, and its smoothing — see
/// `install_smoothing`.
struct Track {
    motion: super::motion::ScrollMotion,
    handle: gpui_kit::ScrollHandle,
    /// Where it was when its view was last drawn: what says, when a wheel
    /// passes, that gpui has just moved it.
    offset: gpui_kit::Point<gpui_kit::Pixels>,
}

thread_local! {
    /// The scroll areas of the script views, by the view and the area's
    /// identity: two boards draw one script with the same names.
    static TRACKS: std::cell::RefCell<HashMap<(gpui_kit::EntityId, String), Track>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Wheel smoothing for the scripts' lists and scroll areas, as Claudhub's own
/// panels have it (`ui::motion`).
///
/// gpui-shell says each area as a script view draws it
/// (`scroll_hook::observe_scroll_areas`): its transition advances there,
/// before layout reads the offset, and where it then stands is noted. The
/// wheel is heard **around** the view (`script_element`), after the area has
/// jumped: the one whose offset is no longer the one noted is the one gpui
/// moved, and its jump is replayed as a transition. Called once, at start-up.
pub(super) fn install_smoothing() {
    gpui_shell::scroll_hook::observe_scroll_areas(|identity, handle, axes, window, _cx| {
        let key = (window.current_view(), format!("{identity:?}"));
        let axes = if axes.horizontal {
            super::motion::Axes::Both
        } else {
            super::motion::Axes::Vertical
        };
        TRACKS.with(|tracks| {
            let mut tracks = tracks.borrow_mut();
            let track = tracks.entry(key).or_insert_with(|| Track {
                motion: super::motion::ScrollMotion::new(axes),
                handle: handle.clone(),
                offset: handle.offset(),
            });
            track.handle = handle.clone();
            track.motion.advance(handle, window);
            track.offset = handle.offset();
        });
    });
}

/// A wheel heard around script view `view`: the area gpui has just moved
/// takes the jump over. True when the view has to be drawn again.
fn smooth_wheel(
    view: gpui_kit::EntityId,
    event: &gpui_kit::ScrollWheelEvent,
    window: &Window,
) -> bool {
    TRACKS.with(|tracks| {
        let mut tracks = tracks.borrow_mut();
        let mut moved = false;
        for ((owner, _), track) in tracks.iter_mut() {
            if *owner != view || track.handle.offset() == track.offset {
                continue;
            }
            moved |= track.motion.on_wheel(&track.handle, event, window);
            track.offset = track.handle.offset();
        }
        moved
    })
}

/// Forgets the areas of a view that went away.
fn forget_tracks(view: gpui_kit::EntityId) {
    TRACKS.with(|tracks| tracks.borrow_mut().retain(|(owner, _), _| *owner != view));
}

/// The keyring service scripts' secrets are filed under, each as
/// `<script>/<name>`.
const SECRETS: &str = "claudhub-plugins";

/// The Plugins screen: whether it is the one shown, and what it has chosen.
#[derive(Default)]
pub(crate) struct Screen {
    pub open: bool,
    /// The script previewed and named to the agent.
    pub selected: Option<String>,
    /// The worktree the preview is drawn for; the one on show by default.
    pub preview_on: Option<PathBuf>,
    /// The chat of the agent that edits the scripts, by its view's id.
    pub agent: Option<u64>,
}

struct Mounted {
    view: Entity<ScriptView>,
    stamp: Stamp,
    /// The hosts it was mounted allowed to reach: a grant is frozen into
    /// the view's policy, so a changed one mounts it again.
    hosts: Vec<String>,
}

impl Scripts {
    /// The scripts found, by id.
    pub fn script(&self, id: &str) -> Option<&Script> {
        self.found
            .iter()
            .map(|(script, _)| script)
            .find(|script| script.id == id)
    }

    /// The scripts of a kind, in their folders' order.
    pub fn of_kind(&self, kind: Kind) -> impl Iterator<Item = &Script> {
        self.found
            .iter()
            .map(|(script, _)| script)
            .filter(move |script| script.kind == kind)
    }

    /// How many scripts were found.
    pub fn count(&self) -> usize {
        self.found.len()
    }

    /// The folders that meant to be scripts and are not, and why.
    pub fn broken(&self) -> &[(String, String)] {
        &self.broken
    }
}

impl ClaudhubApp {
    /// Starts reading the scripts' folder — once, at the first board drawn.
    pub(super) fn watch_scripts(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.scripts.watching, true) {
            return;
        }
        let Some(root) = root() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let folder = root.clone();
            cx.background_executor()
                .spawn(async move {
                    if let Err(error) = scripts::seed(&folder, EXAMPLES) {
                        log::warn!("writing the example scripts: {error}");
                    }
                })
                .await;
            loop {
                let folder = root.clone();
                let language = rust_i18n::locale().to_string();
                let found = cx
                    .background_executor()
                    .spawn(async move { scripts::discover(&folder, &language) })
                    .await;
                if this
                    .update(cx, |app, cx| app.scripts_found(found, cx))
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(POLL).await;
            }
        })
        .detach();
    }

    fn scripts_found(&mut self, found: scripts::Found, cx: &mut Context<Self>) {
        if self.scripts.found == found.scripts && self.scripts.broken == found.broken {
            return;
        }
        // A folder newly broken is said once, not at every reading.
        let newly: Vec<(String, String)> = found
            .broken
            .iter()
            .filter(|broken| !self.scripts.broken.contains(broken))
            .cloned()
            .collect();
        self.scripts.found = found.scripts;
        self.scripts.broken = found.broken;
        for (id, why) in newly {
            self.announce_error(tr!("scripts-broken", { id: id, why: why }), cx);
        }
        self.write_scripts_status(cx);
        cx.notify();
    }

    /// How each script fared: loaded from its current sources somewhere,
    /// failed, or not shown since it changed.
    fn fared(&self, script: &Script, stamp: Stamp) -> scripts::Fared {
        let failed = self
            .scripts
            .failed
            .iter()
            .find(|((_, id), (at, _))| *id == script.id && *at == stamp);
        if let Some((_, (_, why))) = failed {
            return scripts::Fared::Failed(why.clone());
        }
        let loaded = self
            .scripts
            .mounted
            .iter()
            .any(|((_, id), mounted)| *id == script.id && mounted.stamp == stamp);
        if loaded {
            scripts::Fared::Loaded
        } else {
            scripts::Fared::NotShown
        }
    }

    /// The status file the editing agent reads — see `scripts::STATUS` —,
    /// written off the thread, and only when it says something new.
    pub(super) fn write_scripts_status(&mut self, cx: &mut Context<Self>) {
        let Some(root) = root() else {
            return;
        };
        let fared: Vec<(&Script, scripts::Fared, Vec<String>)> = self
            .scripts
            .found
            .iter()
            .map(|(script, stamp)| {
                (
                    script,
                    self.fared(script, *stamp),
                    self.pending_hosts(&script.id, cx),
                )
            })
            .collect();
        let preview = self
            .scripts
            .screen
            .preview_on
            .as_ref()
            .map(|path| path.display().to_string());
        let text = scripts::status_text(
            &fared,
            &self.scripts.broken,
            self.scripts.screen.selected.as_deref(),
            preview.as_deref(),
        );
        if text == self.scripts.status || !root.is_dir() {
            return;
        }
        self.scripts.status = text.clone();
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = std::fs::write(root.join(scripts::STATUS), text) {
                    log::warn!("writing the scripts' status: {error}");
                }
            })
            .detach();
    }

    /// A new script of `kind`, written off the thread under a free name,
    /// then selected on the Plugins screen.
    pub(super) fn create_script(&mut self, kind: Kind, cx: &mut Context<Self>) {
        let Some(root) = root() else {
            return;
        };
        let base = match kind {
            Kind::Tab => "new-tab",
            Kind::Home => "new-home",
        };
        cx.spawn(async move |this, cx| {
            let written = cx
                .background_executor()
                .spawn(async move {
                    let id = scripts::free_id(&root, base);
                    let dir = root.join(&id);
                    std::fs::create_dir_all(&dir)?;
                    for (name, content) in scripts::skeleton(kind) {
                        std::fs::write(dir.join(name), content)?;
                    }
                    std::io::Result::Ok(id)
                })
                .await;
            let _ = this.update(cx, |app, cx| match written {
                Ok(id) => {
                    app.scripts.screen.selected = Some(id);
                    app.write_scripts_status(cx);
                    cx.notify();
                }
                Err(error) => app.announce_error(SharedString::from(error.to_string()), cx),
            });
        })
        .detach();
    }

    /// The script views run again: what they read may have changed. Called
    /// after each batch of events.
    pub(super) fn refresh_scripts(&mut self, cx: &mut Context<Self>) {
        for mounted in self.scripts.mounted.values() {
            mounted.view.update(cx, |view, cx| view.refresh(cx));
        }
    }

    /// The view of script `id` on the board of `path`: mounted already, or
    /// asked for — an empty place meanwhile —, and mounted again when its
    /// sources changed.
    pub(super) fn script_element(
        &mut self,
        path: &Path,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.watch_scripts(cx);
        let found = self
            .scripts
            .found
            .iter()
            .find(|(script, _)| script.id == id)
            .cloned();
        let Some((script, stamp)) = found else {
            return script_notice(tr!("scripts-missing", { id: id }), cx);
        };
        let key: Key = (path.to_path_buf(), id.to_string());
        let hosts = granted_hosts(id, cx);
        let current = self
            .scripts
            .mounted
            .get(&key)
            .map(|mounted| (mounted.view.clone(), (mounted.stamp, mounted.hosts.clone())));
        let failed = self
            .scripts
            .failed
            .get(&key)
            .filter(|(at, _)| *at == stamp)
            .map(|(_, why)| why.clone());
        let fresh = current
            .as_ref()
            .is_some_and(|(_, (at, with))| *at == stamp && *with == hosts);
        if !fresh && failed.is_none() && self.scripts.mounting.insert(key.clone()) {
            let app = cx.entity();
            window.defer(cx, move |window, cx| {
                let result = mount(&app, &key.0, &script, &hosts, window, cx);
                app.update(cx, |this, cx| {
                    this.mounted(key, &script, stamp, hosts, result, cx)
                });
            });
        }
        match (current, failed) {
            (Some((view, _)), _) => {
                // A non-scrolling ancestor of every area the script draws,
                // hearing the wheel after they have: see `install_smoothing`.
                let heard = view.clone();
                div()
                    .size_full()
                    .on_scroll_wheel(move |event, window, cx| {
                        if smooth_wheel(heard.entity_id(), event, window) {
                            heard.update(cx, |_, cx| cx.notify());
                        }
                    })
                    .child(view)
                    .into_any_element()
            }
            (None, Some(why)) => script_notice(why.into(), cx),
            (None, None) => div().size_full().into_any_element(),
        }
    }

    fn mounted(
        &mut self,
        key: Key,
        script: &Script,
        stamp: Stamp,
        hosts: Vec<String>,
        result: gpui_shell::anyhow::Result<Entity<ScriptView>>,
        cx: &mut Context<Self>,
    ) {
        self.scripts.mounting.remove(&key);
        match result {
            Ok(view) => {
                self.scripts.failed.remove(&key);
                let replaced = self
                    .scripts
                    .mounted
                    .insert(key, Mounted { view, stamp, hosts });
                if let Some(replaced) = replaced {
                    forget_tracks(replaced.view.entity_id());
                }
            }
            Err(error) => {
                let why = format!("{error:#}");
                log::warn!("script {}: {why}", script.id);
                // The view that worked stays; the error is said once per
                // save, not once per board.
                let said = self
                    .scripts
                    .failed
                    .iter()
                    .any(|((_, id), (at, _))| *id == key.1 && *at == stamp);
                if !said {
                    self.announce_error(
                        tr!("scripts-failed", { title: script.title.clone(), why: why.clone() }),
                        cx,
                    );
                }
                self.scripts.failed.insert(key, (stamp, why));
            }
        }
        self.write_scripts_status(cx);
        cx.notify();
    }

    /// Why the last mount of `id` on the board of `path` failed, if its
    /// sources have not changed since.
    pub(super) fn script_failure(&self, path: &Path, id: &str) -> Option<&str> {
        let (_, stamp) = self
            .scripts
            .found
            .iter()
            .find(|(script, _)| script.id == id)?;
        self.scripts
            .failed
            .get(&(path.to_path_buf(), id.to_string()))
            .filter(|(at, _)| at == stamp)
            .map(|(_, why)| why.as_str())
    }

    /// Script `id`'s data, read from disk the first time.
    pub(super) fn store_of(&mut self, id: &str) -> &mut serde_json::Map<String, serde_json::Value> {
        self.scripts
            .stores
            .entry(id.to_string())
            .or_insert_with(|| {
                data_root()
                    .and_then(|root| std::fs::read_to_string(root.join(format!("{id}.json"))).ok())
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                    .and_then(|value| match value {
                        serde_json::Value::Object(map) => Some(map),
                        _ => None,
                    })
                    .unwrap_or_default()
            })
    }

    /// Changes script `id`'s data, and queues it to be written.
    fn store_update(
        &mut self,
        id: &str,
        change: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
        cx: &mut Context<Self>,
    ) {
        let store = self.store_of(id);
        change(store);
        let text = serde_json::to_string_pretty(&*store).unwrap_or_default();
        let Some(root) = data_root() else {
            return;
        };
        let writer = self.scripts.writer.get_or_insert_with(|| {
            let (sender, receiver) = async_channel::unbounded::<(PathBuf, String)>();
            cx.background_executor()
                .spawn(async move {
                    while let Ok((path, text)) = receiver.recv().await {
                        let written = path
                            .parent()
                            .map_or(Ok(()), std::fs::create_dir_all)
                            .and_then(|()| std::fs::write(&path, text));
                        if let Err(error) = written {
                            log::warn!("writing {}: {error}", path.display());
                        }
                    }
                })
                .detach();
            sender
        });
        let _ = writer.try_send((root.join(format!("{id}.json")), text));
    }

    /// Forgets script `id`'s data, on disk too.
    pub(super) fn clear_store(&mut self, id: &str, cx: &mut Context<Self>) {
        self.store_update(id, |store| store.clear(), cx);
    }

    /// Where script `id`'s data is written.
    pub(super) fn store_path(id: &str) -> Option<PathBuf> {
        data_root().map(|root| root.join(format!("{id}.json")))
    }

    /// Every host script `id` wants: its manifest's, then those it asked for
    /// while running.
    pub(super) fn wanted_hosts(&self, id: &str) -> Vec<String> {
        let mut wanted: Vec<String> = self
            .scripts
            .script(id)
            .map(|script| script.permissions.network.clone())
            .unwrap_or_default();
        for host in self.scripts.requested.get(id).into_iter().flatten() {
            if !wanted.contains(host) {
                wanted.push(host.clone());
            }
        }
        wanted
    }

    /// The hosts script `id` wants and the user has not allowed.
    pub(super) fn pending_hosts(&self, id: &str, cx: &App) -> Vec<String> {
        scripts::pending_hosts(&self.wanted_hosts(id), &granted_hosts(id, cx))
    }

    /// A script asks for a host while running: kept, said once, and left to
    /// the user to allow in the Plugins screen.
    fn network_requested(&mut self, id: &str, host: String, cx: &mut Context<Self>) -> bool {
        if granted_hosts(id, cx).contains(&host) {
            return true;
        }
        if !self.wanted_hosts(id).contains(&host) {
            self.scripts
                .requested
                .entry(id.to_string())
                .or_default()
                .push(host.clone());
            let title = self
                .scripts
                .script(id)
                .map_or_else(|| id.to_string(), |script| script.title.clone());
            self.announce(
                tr!("plugins-network-asked", { title: title, host: host }),
                cx,
            );
            self.write_scripts_status(cx);
            cx.notify();
        }
        false
    }

    /// Allows or withdraws a host for script `id`; its views mount again
    /// with the new grant.
    pub(super) fn grant_host(
        &mut self,
        id: &str,
        host: &str,
        allowed: bool,
        cx: &mut Context<Self>,
    ) {
        let (id, host) = (id.to_string(), host.to_string());
        super::settings::Settings::update_global(cx, |settings| {
            let hosts = settings.plugin_grants.entry(id.clone()).or_default();
            hosts.retain(|known| *known != host);
            if allowed {
                hosts.push(host.clone());
                hosts.sort();
            }
            if hosts.is_empty() {
                settings.plugin_grants.remove(&id);
            }
        });
        self.write_scripts_status(cx);
        cx.notify();
    }

    /// Forgets the views of a worktree that went away.
    pub(super) fn drop_scripts_of(&mut self, path: &Path) {
        self.scripts.mounted.retain(|(worktree, _), mounted| {
            let kept = worktree != path;
            if !kept {
                forget_tracks(mounted.view.entity_id());
            }
            kept
        });
        self.scripts
            .failed
            .retain(|(worktree, _), _| worktree != path);
    }

    /// The script tab chosen on a board, if it is still there.
    pub(super) fn board_script(&self, path: &Path, cx: &App) -> Option<String> {
        super::store::Store::global(cx)
            .worktrees
            .get(path)
            .and_then(|state| state.focus_script.clone())
            .filter(|id| {
                self.scripts
                    .script(id)
                    .is_some_and(|script| script.kind == Kind::Tab)
            })
    }

    /// Puts a script's tab on a board.
    pub(super) fn show_board_script(&mut self, path: &Path, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        super::store::Store::update_global(cx, |store| {
            store
                .worktrees
                .entry(path.to_path_buf())
                .or_default()
                .focus_script = Some(id);
        });
        cx.notify();
    }

    /// The home script the settings name, if it is there.
    pub(super) fn home_script(&self, cx: &App) -> Option<String> {
        let id = super::settings::Settings::global(cx).home_script.trim();
        self.scripts
            .script(id)
            .filter(|script| script.kind == Kind::Home)
            .map(|script| script.id.clone())
    }

    // What the module reads. Each answers for one worktree.

    fn script_worktree(&self, path: &Path) -> HostValue {
        let (_, name) = self.project_label(path);
        let branch = self
            .repos
            .worktree(path)
            .and_then(|worktree| worktree.branch.clone());
        let (ahead, behind) = self
            .outlines
            .get(path)
            .and_then(|outline| outline.upstream)
            .unwrap_or((0, 0));
        HostObject::new()
            .field("path", path.to_string_lossy().into_owned())
            .field("name", name.to_string())
            .field("branch", branch.map_or(HostValue::Null, HostValue::from))
            .field("main", self.main_of(path).as_deref() == Some(path))
            .field(
                "repository",
                self.main_of(path).map_or(HostValue::Null, |main| {
                    main.to_string_lossy().into_owned().into()
                }),
            )
            .field("active", self.active.as_deref() == Some(path))
            .field("ahead", ahead as f64)
            .field("behind", behind as f64)
            .into()
    }

    fn script_changes(&self, path: &Path) -> HostValue {
        let summary = self.summaries.get(path).cloned().unwrap_or_default();
        HostObject::new()
            .field("files", summary.files as f64)
            .field("added", summary.added as f64)
            .field("removed", summary.removed as f64)
            .into()
    }

    fn script_tasks(&self, path: &Path) -> HostValue {
        let tasks = self
            .review
            .get(path)
            .and_then(|state| state.todo.as_ref())
            .map(|todo| todo.tasks.as_slice())
            .unwrap_or_default();
        HostValue::Array(
            tasks
                .iter()
                .map(|task| {
                    HostObject::new()
                        .field("label", task.label.clone())
                        .field("done", task.done)
                        .field("line", task.line as f64)
                        .field("depth", task.depth as f64)
                        .into()
                })
                .collect(),
        )
    }

    fn script_terminals(&self, path: &Path, cx: &App) -> HostValue {
        HostValue::Array(
            self.terminals_of(path)
                .map(|terminal| {
                    let id = terminal.view.entity_id().as_u64();
                    let doing = match self.scripts.at_work.terminals.get(&id) {
                        Some(Doing::Working) => HostValue::from("working"),
                        Some(Doing::Waiting) => HostValue::from("waiting"),
                        Some(Doing::Rest) | None => HostValue::Null,
                    };
                    HostObject::new()
                        .field("id", id as f64)
                        .field("label", terminal.label(cx).to_string())
                        .field("exited", terminal.exited)
                        .field("doing", doing)
                        .into()
                })
                .collect(),
        )
    }

    fn script_list(&self) -> HostValue {
        HostValue::Array(
            self.scripts
                .found
                .iter()
                .map(|(script, _)| {
                    HostObject::new()
                        .field("id", script.id.clone())
                        .field("title", script.title.clone())
                        .field(
                            "kind",
                            match script.kind {
                                Kind::Tab => "tab",
                                Kind::Home => "home",
                            },
                        )
                        .into()
                })
                .collect(),
        )
    }
}

/// What stands in a script's place when it cannot: its error, or that it
/// is gone.
fn script_notice(text: SharedString, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .size_full()
        .p_4()
        .gap_2()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(
            div()
                .text_color(theme.danger)
                .child(tr!("scripts-unavailable")),
        )
        .child(
            div()
                .font_family(theme.mono_font_family.clone())
                .child(text),
        )
        .into_any_element()
}

/// Loads and mounts a script for the board of `path`, under a policy whose
/// `claudhub` module answers for that worktree.
///
/// The policy is set as the default for the time of the load and the mount,
/// which is when the runtime takes it: a load links the imports against the
/// default's modules, and a mount gives the view the default to keep.
fn mount(
    app: &Entity<ClaudhubApp>,
    path: &Path,
    script: &Script,
    hosts: &[String],
    window: &mut Window,
    cx: &mut App,
) -> gpui_shell::anyhow::Result<Entity<ScriptView>> {
    let runtime = match app.read(cx).scripts.runtime.clone() {
        Some(runtime) => runtime,
        None => {
            let runtime = ShellRuntime::new(cx)?;
            app.update(cx, |this, _| this.scripts.runtime = Some(runtime.clone()));
            runtime
        }
    };
    let module = module(
        app.downgrade(),
        path,
        &script.id,
        Some(window.window_handle()),
    );
    let policy = Policy::new()
        .with_application(&script.id)
        .with_capabilities(capabilities(hosts))
        .with_host_module(module)
        .map_err(|error| gpui_shell::anyhow::anyhow!("{}", error.message()))?;
    gpui_shell::policy::set_default(policy);
    let view = runtime
        .load_application(&script.dir, &script.entry)
        .and_then(|loaded| runtime.mount_application(&loaded, window, cx));
    gpui_shell::policy::set_default(Policy::new());
    view
}

/// The hosts the user allowed script `id` to reach.
pub(super) fn granted_hosts(id: &str, cx: &App) -> Vec<String> {
    super::settings::Settings::global(cx)
        .plugin_grants
        .get(id)
        .cloned()
        .unwrap_or_default()
}

/// What a script may do beyond drawing: HTTPS requests to the hosts the user
/// allowed — any method, any path, port 443 —, and writing to the
/// clipboard. No file, no process, no raw socket, no `localStorage`: the
/// module keeps its data (`storage_*`) and its secrets (`secret`).
fn capabilities(hosts: &[String]) -> Capabilities {
    const METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];
    Capabilities::new()
        .http_requests(
            hosts.iter().map(|host| {
                HttpRequestGrant::new(host.clone(), METHODS, Vec::<String>::new(), ["/"])
            }),
        )
        .clipboard_write(true)
}

/// The TypeScript face of the `claudhub` module, checked against what is
/// registered at every mount (`HostModule::validate`).
const DECLARATIONS: &str = r#"
import { NativeElement, HostValue } from "gpui-kit";

/** The worktree this view is drawn for. */
export interface Worktree {
  path: string;
  name: string;
  branch: string | null;
  /** The repository's main checkout. */
  main: boolean;
  /** The path of the repository's main checkout: one key for every worktree of it. */
  repository: string | null;
  /** The one the editor shows. */
  active: boolean;
  ahead: number;
  behind: number;
}
/** What waits for a commit. */
export interface Changes { files: number; added: number; removed: number; }
/** A checkbox of the worktree's `TODO.md`. */
export interface Task { label: string; done: boolean; line: number; depth: number; }
export interface Terminal {
  id: number;
  label: string;
  exited: boolean;
  /** What its agent does: at work, waiting on the user, or nothing. */
  doing: "working" | "waiting" | null;
}
export interface ScriptInfo { id: string; title: string; kind: "tab" | "home"; }
export type Tab = "home" | "git" | "review" | "pr" | "tests" | "notes" | "todo" | "terminals";
/** A piece of the board painted by Claudhub: `Card.new("id")`. */
export interface HostComponent { "new"(id: string, props?: HostValue): NativeElement; }

export function worktree(): Worktree;
export function changes(): Changes;
export function tasks(): Task[];
export function terminals(): Terminal[];
export function scripts(): ScriptInfo[];
/** The interface's language: `"fr"` or `"en"`. */
export function language(): string;

/** Shows one of the board's own tabs. */
export function open_tab(tab: Tab): void;
/** Shows a script's tab on the board. */
export function open_script(id: string): void;
/** Ticks or unticks a task of `TODO.md`, by its line. */
export function toggle_task(line: number, done: boolean): void;
/** Opens a terminal on the worktree. */
export function open_terminal(): void;
/** Hands a text to the worktree's agent, opening one if none runs. */
export function send_to_agent(text: string): void;
/** Says something in a bubble. */
export function notify(text: string): void;

/** This script's data, kept across restarts and shared by its views on every board. Any JSON. */
export function storage_get(key: string): HostValue;
export function storage_set(key: string, value: HostValue): void;
export function storage_remove(key: string): void;
export function storage_keys(): string[];

/** A secret of this script, from the system keyring; null when none was set. */
export function secret(name: string): Promise<string | null>;
export function set_secret(name: string, value: string): Promise<void>;
export function delete_secret(name: string): Promise<void>;

/** The hosts the user allowed this script to `fetch` from (HTTPS). */
export function granted_hosts(): string[];
/**
 * Asks for a host not in the manifest — a self-hosted instance. True when it
 * is allowed already; otherwise the user is asked in the Plugins screen, and
 * the script is mounted again once it is.
 */
export function request_network(host: string): boolean;
/** Opens an http(s) address in the browser. */
export function open_url(url: string): void;
/** Puts a text on the clipboard. */
export function copy_text(text: string): void;

export const BranchCard: HostComponent;
export const PullRequestCard: HostComponent;
export const ChangesCard: HostComponent;
export const ReviewCard: HostComponent;
export const TasksCard: HostComponent;
export const NoteCard: HostComponent;
export const RunCard: HostComponent;
/** The worktree's terminals, a sub-tab each. */
export const Terminals: HostComponent;
/** Claudhub's own home, whole. */
export const DefaultHome: HostComponent;
export const GitTab: HostComponent;
export const TestsTab: HostComponent;
export const NotesTab: HostComponent;
export const TerminalsTab: HostComponent;
"#;

/// The `claudhub` module, for the board of `path`.
fn module(
    app: WeakEntity<ClaudhubApp>,
    path: &Path,
    id: &str,
    window: Option<AnyWindowHandle>,
) -> HostModule {
    let id = id.to_string();
    let module = HostModule::new("claudhub")
        .declarations(declarations())
        .function(
            "worktree",
            read(&app, path, |this, path, _| this.script_worktree(path)),
        )
        .function(
            "changes",
            read(&app, path, |this, path, _| this.script_changes(path)),
        )
        .function(
            "tasks",
            read(&app, path, |this, path, _| this.script_tasks(path)),
        )
        .function(
            "terminals",
            read(&app, path, |this, path, cx| this.script_terminals(path, cx)),
        )
        .function("scripts", read(&app, path, |this, _, _| this.script_list()))
        .function("language", |_| Ok(HostValue::from(&*rust_i18n::locale())))
        .function("storage_get", {
            let (app, id) = (app.clone(), id.clone());
            move |arguments| {
                let key = arguments.string(0)?.to_string();
                change(&app, |this, _| {
                    Ok(this
                        .store_of(&id)
                        .get(&key)
                        .map_or(HostValue::Null, from_json))
                })
            }
        })
        .function("storage_keys", {
            let (app, id) = (app.clone(), id.clone());
            move |_| {
                change(&app, |this, _| {
                    Ok(HostValue::Array(
                        this.store_of(&id)
                            .keys()
                            .map(|key| HostValue::from(key.as_str()))
                            .collect(),
                    ))
                })
            }
        })
        .function("storage_set", {
            let (app, id) = (app.clone(), id.clone());
            move |arguments| {
                let key = arguments.string(0)?.to_string();
                let value = to_json(arguments.value(1)?);
                change(&app, |this, cx| {
                    this.store_update(
                        &id,
                        |store| {
                            store.insert(key, value);
                        },
                        cx,
                    );
                    Ok(HostValue::Null)
                })
            }
        })
        .function("storage_remove", {
            let (app, id) = (app.clone(), id.clone());
            move |arguments| {
                let key = arguments.string(0)?.to_string();
                change(&app, |this, cx| {
                    this.store_update(
                        &id,
                        |store| {
                            store.remove(&key);
                        },
                        cx,
                    );
                    Ok(HostValue::Null)
                })
            }
        })
        .async_function("secret", {
            let id = id.clone();
            move |arguments| {
                let entry = secret_entry(&id, arguments.string(0)?)?;
                Ok(async move {
                    match entry.get_password() {
                        Ok(secret) => Ok(HostValue::from(secret)),
                        Err(keyring::Error::NoEntry) => Ok(HostValue::Null),
                        Err(error) => Err(HostError::new(error.to_string())),
                    }
                })
            }
        })
        .async_function("set_secret", {
            let id = id.clone();
            move |arguments| {
                let entry = secret_entry(&id, arguments.string(0)?)?;
                let secret = arguments.string(1)?.to_string();
                Ok(async move {
                    entry
                        .set_password(&secret)
                        .map(|()| HostValue::Null)
                        .map_err(|error| HostError::new(error.to_string()))
                })
            }
        })
        .async_function("delete_secret", {
            let id = id.clone();
            move |arguments| {
                let entry = secret_entry(&id, arguments.string(0)?)?;
                Ok(async move {
                    match entry.delete_credential() {
                        Ok(()) | Err(keyring::Error::NoEntry) => Ok(HostValue::Null),
                        Err(error) => Err(HostError::new(error.to_string())),
                    }
                })
            }
        })
        .function("granted_hosts", {
            let id = id.clone();
            move |_| {
                gpui_shell::with_current_app(|cx| {
                    HostValue::Array(
                        granted_hosts(&id, cx)
                            .into_iter()
                            .map(HostValue::from)
                            .collect(),
                    )
                })
                .ok_or_else(unreachable)
            }
        })
        .function("request_network", {
            let (app, id) = (app.clone(), id.clone());
            move |arguments| {
                let text = arguments.string(0)?;
                let host = scripts::host_of(text).ok_or_else(|| {
                    HostError::new(format!("`{text}` is not a host — write `api.example.com`"))
                })?;
                change(&app, |this, cx| {
                    Ok(HostValue::from(this.network_requested(&id, host, cx)))
                })
            }
        })
        .function("open_url", |arguments| {
            let url = arguments.string(0)?.to_string();
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err(HostError::new("only http and https addresses open"));
            }
            gpui_shell::with_current_app(|cx| cx.open_url(&url)).ok_or_else(unreachable)?;
            Ok(HostValue::Null)
        })
        .function("copy_text", |arguments| {
            let text = arguments.string(0)?.to_string();
            gpui_shell::with_current_app(|cx| {
                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text))
            })
            .ok_or_else(unreachable)?;
            Ok(HostValue::Null)
        })
        .function("open_tab", {
            let (app, path) = (app.clone(), path.to_path_buf());
            move |arguments| {
                let tab = tab_named(arguments.string(0)?)?;
                let path = path.clone();
                later(&app, window, move |this, _, cx| {
                    this.show_board_view(&path, tab, cx)
                })
            }
        })
        .function("open_script", {
            let (app, path) = (app.clone(), path.to_path_buf());
            move |arguments| {
                let id = arguments.string(0)?.to_string();
                let path = path.clone();
                later(&app, window, move |this, _, cx| {
                    this.show_board_script(&path, &id, cx)
                })
            }
        })
        .function("toggle_task", {
            let (app, path) = (app.clone(), path.to_path_buf());
            move |arguments| {
                let line = usize::try_from(arguments.integer(0)?)
                    .map_err(|_| HostError::new("a line is a number from zero"))?;
                let done = arguments.boolean(1)?;
                let path = path.clone();
                later(&app, window, move |this, _, cx| {
                    this.toggle_task_in(path, line, done, cx)
                })
            }
        })
        .function("open_terminal", {
            let (app, path) = (app.clone(), path.to_path_buf());
            move |_| {
                let path = path.clone();
                later(&app, window, move |this, window, cx| {
                    this.open_focus_terminal(&path, window, cx)
                })
            }
        })
        .function("send_to_agent", {
            let (app, path) = (app.clone(), path.to_path_buf());
            move |arguments| {
                let text = arguments.string(0)?.to_string();
                let path = path.clone();
                later(&app, window, move |this, window, cx| {
                    this.send_to_agent(&path, text, window, cx)
                })
            }
        })
        .function("notify", {
            let app = app.clone();
            move |arguments| {
                let text = SharedString::from(arguments.string(0)?.to_string());
                later(&app, window, move |this, _, cx| this.announce(text, cx))
            }
        })
        .component(
            "BranchCard",
            paint(&app, path, |this, path, _, cx| this.home_branch(path, cx)),
        )
        .component(
            "PullRequestCard",
            paint(&app, path, |this, path, _, cx| this.home_pr(path, cx)),
        )
        .component(
            "ChangesCard",
            paint(&app, path, |this, path, _, cx| {
                this.home_to_commit(path, cx)
            }),
        )
        .component(
            "ReviewCard",
            paint(&app, path, |this, path, _, cx| this.home_review(path, cx)),
        )
        .component(
            "TasksCard",
            paint(&app, path, |this, path, _, cx| this.home_tasks(path, cx)),
        )
        .component(
            "NoteCard",
            paint(&app, path, |this, path, _, cx| this.home_note(path, cx)),
        )
        .component(
            "RunCard",
            paint(&app, path, |this, path, _, cx| {
                this.home_run(path, cx)
                    .unwrap_or_else(|| div().into_any_element())
            }),
        )
        .component(
            "Terminals",
            paint(&app, path, |this, path, window, cx| {
                let at_work = this.scripts.at_work.clone();
                this.home_terminals(path, &at_work, window, cx)
            }),
        )
        .component(
            "DefaultHome",
            paint(&app, path, |this, path, window, cx| {
                let at_work = this.scripts.at_work.clone();
                this.render_home_view(path, &at_work, window, cx)
            }),
        )
        .component(
            "GitTab",
            paint(&app, path, |this, path, window, cx| {
                this.render_git_view(path, window, cx)
            }),
        )
        .component(
            "TestsTab",
            paint(&app, path, |this, path, window, cx| {
                this.render_tests_view(path, window, cx)
            }),
        )
        .component(
            "NotesTab",
            paint(&app, path, |this, path, _, cx| {
                this.render_notes_tab(path, cx)
            }),
        )
        .component(
            "TerminalsTab",
            paint(&app, path, |this, path, window, cx| {
                let at_work = this.scripts.at_work.clone();
                this.render_terminals_view(path, &at_work, window, cx)
            }),
        );
    super::script_kit::lend(module, &app, path, window)
}

/// What a module function answers outside a call, which cannot happen
/// from a script.
pub(super) fn unreachable() -> HostError {
    HostError::new("Claudhub is not reachable outside a call")
}

/// A function of the module that changes the application — or reads what
/// it loads on demand.
pub(super) fn change(
    app: &WeakEntity<ClaudhubApp>,
    act: impl FnOnce(&mut ClaudhubApp, &mut Context<ClaudhubApp>) -> HostResult,
) -> HostResult {
    gpui_shell::with_current_app(|cx| app.update(cx, act).ok())
        .flatten()
        .ok_or_else(unreachable)?
}

/// The keyring entry of a script's secret `name`.
fn secret_entry(id: &str, name: &str) -> Result<keyring::Entry, HostError> {
    if !scripts::valid_id(name) {
        return Err(HostError::new(format!(
            "`{name}` is not a secret's name: letters, digits, `-`, `_` and `.`"
        )));
    }
    keyring::Entry::new(SECRETS, &format!("{id}/{name}"))
        .map_err(|error| HostError::new(error.to_string()))
}

/// Forgets script `id`'s secret `name` — the Plugins screen's gesture.
pub(super) fn forget_secret(id: &str, name: &str) {
    if let Ok(entry) = secret_entry(id, name) {
        if let Err(error) = entry.delete_credential() {
            if !matches!(error, keyring::Error::NoEntry) {
                log::warn!("forgetting a script's secret: {error}");
            }
        }
    }
}

/// A value a script hands over, as JSON.
fn to_json(value: &HostValue) -> serde_json::Value {
    match value {
        HostValue::Null => serde_json::Value::Null,
        HostValue::Bool(flag) => serde_json::Value::Bool(*flag),
        HostValue::Number(number) => serde_json::Number::from_f64(*number)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        HostValue::Str(text) => serde_json::Value::String(text.clone()),
        HostValue::Array(items) => serde_json::Value::Array(items.iter().map(to_json).collect()),
        HostValue::Object(fields) => serde_json::Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), to_json(value)))
                .collect(),
        ),
    }
}

/// JSON, as a value for a script.
fn from_json(value: &serde_json::Value) -> HostValue {
    match value {
        serde_json::Value::Null => HostValue::Null,
        serde_json::Value::Bool(flag) => HostValue::Bool(*flag),
        serde_json::Value::Number(number) => HostValue::Number(number.as_f64().unwrap_or(0.)),
        serde_json::Value::String(text) => HostValue::Str(text.clone()),
        serde_json::Value::Array(items) => HostValue::Array(items.iter().map(from_json).collect()),
        serde_json::Value::Object(fields) => HostValue::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), from_json(value)))
                .collect(),
        ),
    }
}

/// The module's TypeScript face: its own, then what `script_kit` lends.
fn declarations() -> String {
    format!("{DECLARATIONS}{}", super::script_kit::DECLARATIONS)
}

/// A tab by the name a script gives it.
fn tab_named(name: &str) -> Result<View, HostError> {
    Ok(match name {
        "home" => View::Home,
        "git" => View::Git,
        "review" => View::Review,
        "pr" => View::Pr,
        "tests" => View::Tests,
        "notes" => View::Notes,
        "todo" => View::Todo,
        "terminals" => View::Terminals,
        other => {
            return Err(HostError::new(format!(
                "no tab `{other}`: home, git, review, pr, tests, notes, todo or terminals"
            )))
        }
    })
}

/// A function of the module that reads the application for `path`.
fn read(
    app: &WeakEntity<ClaudhubApp>,
    path: &Path,
    answer: fn(&ClaudhubApp, &Path, &App) -> HostValue,
) -> impl Fn(&HostArguments) -> HostResult + 'static {
    let (app, path) = (app.clone(), path.to_path_buf());
    move |_| {
        gpui_shell::with_current_app(|cx| {
            let cx: &App = cx;
            app.upgrade().map(|app| answer(app.read(cx), &path, cx))
        })
        .flatten()
        .ok_or_else(|| HostError::new("Claudhub is not reachable outside a call"))
    }
}

/// A function of the module that acts: what it does waits until the call
/// has unwound — the script is still running, and the application may be
/// the one that called it —, then runs with the window.
pub(super) fn later(
    app: &WeakEntity<ClaudhubApp>,
    window: Option<AnyWindowHandle>,
    act: impl FnOnce(&mut ClaudhubApp, &mut Window, &mut Context<ClaudhubApp>) + 'static,
) -> HostResult {
    let window = window.ok_or_else(|| HostError::new("this view has no window to act in"))?;
    let app = app.clone();
    gpui_shell::with_current_app(move |cx| {
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                let _ = app.update(cx, |this, cx| act(this, window, cx));
            });
        })
    })
    .ok_or_else(|| HostError::new("Claudhub is not reachable outside a call"))?;
    Ok(HostValue::Null)
}

/// A component of the module: a piece of the board, painted by the
/// application's own method for `path`.
fn paint(
    app: &WeakEntity<ClaudhubApp>,
    path: &Path,
    draw: fn(&mut ClaudhubApp, &Path, &mut Window, &mut Context<ClaudhubApp>) -> AnyElement,
) -> impl for<'a> Fn(ComponentArgs<'a>, &mut Window, &mut App) -> AnyElement + 'static {
    let (app, path) = (app.clone(), path.to_path_buf());
    move |_, window, cx| {
        let Some(app) = app.upgrade() else {
            return div().into_any_element();
        };
        // Read first: what a view reads while it renders is what repaints it.
        let _ = app.read(cx);
        app.update(cx, |this, cx| draw(this, &path, window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `claudhub` module of the declared shape, whose functions answer
    /// nothing and whose components paint nothing: what linking needs.
    fn stub() -> HostModule {
        let declared = declarations();
        let mut module = HostModule::new("claudhub").declarations(declared.clone());
        let name = |rest: &str| -> String {
            rest.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect()
        };
        for line in declared.lines().map(str::trim_start) {
            if let Some(rest) = line.strip_prefix("export function ") {
                module = module.function(name(rest), |_| Ok(HostValue::Null));
            } else if let Some(rest) = line.strip_prefix("export const ") {
                module = module.component(name(rest), |_, _, _| div().into_any_element());
            }
        }
        module
    }

    /// What the module registers is what it declares: the runtime refuses a
    /// module whose two halves differ, and it would refuse it at every mount.
    #[test]
    fn the_module_registers_what_it_declares() {
        let module = module(WeakEntity::new_invalid(), Path::new("/w"), "test", None);
        if let Err(error) = module.validate() {
            panic!("{}", error.message());
        }
        let stub = stub();
        let mut registered = module.function_names();
        let mut declared = stub.function_names();
        registered.sort_unstable();
        declared.sort_unstable();
        assert_eq!(registered, declared);
    }

    /// The scripts shipped as examples, and the skeletons of a new one, are
    /// found, link against the module's declared exports and evaluate — a renamed export or a typo in an
    /// import fails here rather than on someone's first board.
    #[test]
    fn the_shipped_scripts_load_against_the_module() {
        let root = std::env::temp_dir().join(format!("claudhub-scripts-{}", std::process::id()));
        let keep = std::env::temp_dir().join("claudhub-scripts-kept");
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            scripts::seed(&root, EXAMPLES).unwrap(),
            ["home-columns", "dashboard", "sentry"]
        );
        // Removed, an example does not come back.
        std::fs::remove_dir_all(root.join("dashboard")).unwrap();
        assert!(scripts::seed(&root, EXAMPLES).unwrap().is_empty());
        assert!(!root.join("dashboard").exists());
        // Put back by hand, to be loaded below with the others.
        for (path, content) in EXAMPLES
            .iter()
            .filter(|(path, _)| path.starts_with("dashboard/"))
        {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), content).unwrap();
        }
        // A newer one replaces what is still as it was written…
        let older: Vec<(&str, &str)> = EXAMPLES
            .iter()
            .map(|(path, content)| match *path {
                "sentry/main.js" => (*path, "// an older version\n"),
                _ => (*path, *content),
            })
            .collect();
        let elsewhere = root.with_extension("older");
        let _ = std::fs::remove_dir_all(&elsewhere);
        scripts::seed(&elsewhere, &older).unwrap();
        assert_eq!(scripts::seed(&elsewhere, EXAMPLES).unwrap(), ["sentry"]);
        assert_eq!(
            std::fs::read_to_string(elsewhere.join("sentry/main.js")).unwrap(),
            include_str!("../../assets/scripts/sentry/main.js")
        );
        // …and never what the user changed.
        scripts::seed(&elsewhere, &older).unwrap();
        std::fs::write(elsewhere.join("home-columns/main.js"), "// mine\n").unwrap();
        let newer: Vec<(&str, &str)> = EXAMPLES
            .iter()
            .map(|(path, content)| match *path {
                "home-columns/main.js" => (*path, "// newer\n"),
                _ => (*path, *content),
            })
            .collect();
        assert!(!scripts::seed(&elsewhere, &newer)
            .unwrap()
            .contains(&"home-columns".to_string()));
        assert_eq!(
            std::fs::read_to_string(elsewhere.join("home-columns/main.js")).unwrap(),
            "// mine\n"
        );
        let _ = std::fs::remove_dir_all(&elsewhere);
        // And what « New script » writes, of either kind.
        for (kind, base) in [(Kind::Tab, "new-tab"), (Kind::Home, "new-home")] {
            let id = scripts::free_id(&root, base);
            assert_eq!(id, base);
            std::fs::create_dir_all(root.join(&id)).unwrap();
            for (name, content) in scripts::skeleton(kind) {
                std::fs::write(root.join(&id).join(name), content).unwrap();
            }
            assert_eq!(scripts::free_id(&root, base), format!("{base}-2"));
        }

        let found = scripts::discover(&root, "fr");
        assert_eq!(found.broken, Vec::<(String, String)>::new());
        let kinds: Vec<(&str, Kind)> = found
            .scripts
            .iter()
            .map(|(script, _)| (script.id.as_str(), script.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("dashboard", Kind::Tab),
                ("home-columns", Kind::Home),
                ("new-home", Kind::Home),
                ("new-tab", Kind::Tab),
                ("sentry", Kind::Tab),
            ]
        );

        gpui_shell::policy::set_default(Policy::new().with_host_module(stub()).unwrap());
        let runtime = ShellRuntime::new_isolated().unwrap();
        for (script, _) in &found.scripts {
            if let Err(error) = runtime.load_application(&script.dir, &script.entry) {
                panic!("{}: {error:#}", script.id);
            }
        }
        gpui_shell::policy::set_default(Policy::new());
        // Kept on demand, to read the `gpui-kit.d.ts` the runtime wrote.
        if std::env::var_os("CLAUDHUB_KEEP_SCRIPTS").is_some() {
            let _ = std::fs::remove_dir_all(&keep);
            let _ = std::fs::rename(&root, &keep);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
