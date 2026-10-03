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
//! - **What the script reads changes when the application says so**: each
//!   view observes it, and runs the script again at each of its notifications
//!   (`ScriptView::refresh`) — never at each batch of events, most of which
//!   change nothing: an ACP chat streams dozens a second. A bare repaint only
//!   materialises the description it already has.
//!
//! The scripts' folder is read off the thread every half second. A change to
//! a script's sources mounts it again; a broken save leaves the view that
//! worked on screen, and says why in a bubble. The policy permits HTTP and
//! HTTPS requests anywhere, and no file nor process: what a script may do
//! beyond that is what the module offers.

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
    HostValue, ScriptView, ShellRuntime,
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
];

/// The scripts that ship inside Claudhub — see `crate::scripts` —, written
/// to [`builtin_root`] and brought up to date with each build. Sentry was an
/// example once: an untouched copy of it in the user's folder is retired.
pub(super) const BUILTINS: &[(&str, &str)] = &[
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
    (
        "http-client/claudhub.json",
        include_str!("../../assets/scripts/http-client/claudhub.json"),
    ),
    (
        "http-client/main.js",
        include_str!("../../assets/scripts/http-client/main.js"),
    ),
    (
        "http-client/http.js",
        include_str!("../../assets/scripts/http-client/http.js"),
    ),
    (
        "http-client/texts.js",
        include_str!("../../assets/scripts/http-client/texts.js"),
    ),
    (
        "agent-library/claudhub.json",
        include_str!("../../assets/scripts/agent-library/claudhub.json"),
    ),
    (
        "agent-library/main.js",
        include_str!("../../assets/scripts/agent-library/main.js"),
    ),
    (
        "agent-library/library.js",
        include_str!("../../assets/scripts/agent-library/library.js"),
    ),
    (
        "agent-library/texts.js",
        include_str!("../../assets/scripts/agent-library/texts.js"),
    ),
];

/// Fingerprints of earlier versions of the builtins, as they may still lie
/// in the user's folder — see `scripts::retire`: Requêtes HTTP was written
/// there by the Plugins agent before it shipped, and Sentry seeded there as
/// an example.
const EARLIER: &[(&str, &str)] = &[
    ("http-client", "39e76b702af5bd1f"),
    ("sentry", "81ef7b6965f346f7"),
];

/// Where the builtins are written: beside the user's folder, Claudhub's
/// alone, emptied and written again by an update.
pub fn builtin_root() -> Option<PathBuf> {
    super::settings::config_dir().map(|dir| dir.join("scripts-builtin"))
}

/// Readies the folders, off the thread: the builtins written, the copies
/// that are no fork retired, the examples seeded.
pub(super) fn prepare(root: &Path, builtins: &Path) {
    if let Err(error) = scripts::install_builtins(builtins, BUILTINS) {
        log::warn!("writing the builtin scripts: {error}");
    }
    for id in scripts::ids_of(BUILTINS) {
        let earlier: Vec<&str> = EARLIER
            .iter()
            .filter(|(of, _)| *of == id)
            .map(|(_, print)| *print)
            .collect();
        match scripts::retire(root, id, BUILTINS, &earlier) {
            Ok(true) => {
                log::info!("script {id}: the copy in the scripts' folder gave way to the builtin")
            }
            Ok(false) => {}
            Err(error) => log::warn!("script {id}: retiring the copy: {error}"),
        }
    }
    if let Err(error) = scripts::seed(root, EXAMPLES) {
        log::warn!("writing the example scripts: {error}");
    }
}

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
    /// The marketplaces: their catalogs, what is installed from them, and
    /// their fetches — see `ui::market`.
    pub(super) markets: super::market::Markets,
    /// Each script's data — `storage_*` —, by its id: one map whatever the
    /// boards it is drawn on, read from disk at its first use.
    stores: HashMap<String, Store>,
    /// Where the stores' writes queue, one after the other — two written
    /// side by side could land in the wrong order.
    writer: Option<async_channel::Sender<(PathBuf, String)>>,
}

/// Where scripts' data lives: beside their folder, not in it — an update of
/// a script, or the agent editing it, never reaches its data.
fn data_root() -> Option<PathBuf> {
    super::settings::config_dir().map(|dir| dir.join("scripts-data"))
}

/// One script's data — `storage_*`.
type Store = serde_json::Map<String, serde_json::Value>;

/// The data of the scripts `found` whose data is not `known` yet, read off
/// the thread with the folder: a script is mounted only once found, so its
/// data is there before its first `storage_get`. Missing or unreadable, it
/// is empty.
fn read_stores(found: &scripts::Found, known: &HashSet<String>) -> Vec<(String, Store)> {
    let root = data_root();
    found
        .scripts
        .iter()
        .map(|(script, _)| &script.id)
        .filter(|id| !known.contains(*id))
        .map(|id| {
            let store = root
                .as_ref()
                .and_then(|root| std::fs::read_to_string(root.join(format!("{id}.json"))).ok())
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|value| match value {
                    serde_json::Value::Object(map) => Some(map),
                    _ => None,
                })
                .unwrap_or_default();
            (id.clone(), store)
        })
        .collect()
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
    /// The marketplace entry shown in the preview's place — `(slug,
    /// folder)` —, when one of the catalog is chosen rather than a script.
    pub catalog: Option<(String, String)>,
}

struct Mounted {
    view: Entity<ScriptView>,
    stamp: Stamp,
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
        self.reload_markets(cx);
        let (Some(root), Some(builtins)) = (root(), builtin_root()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let (folder, shipped) = (root.clone(), builtins.clone());
            cx.background_executor()
                .spawn(async move { prepare(&folder, &shipped) })
                .await;
            loop {
                let (folder, shipped) = (root.clone(), builtins.clone());
                let installed = super::market::installed_root();
                let language = rust_i18n::locale().to_string();
                let Ok(known) = this.update(cx, |app, _| {
                    app.scripts.stores.keys().cloned().collect::<HashSet<_>>()
                }) else {
                    break;
                };
                let (found, stores) = cx
                    .background_executor()
                    .spawn(async move {
                        let found = scripts::merge(
                            installed
                                .as_deref()
                                .map(|installed| scripts::discover(installed, &language))
                                .unwrap_or_default(),
                            scripts::discover(&shipped, &language),
                            scripts::discover(&folder, &language),
                        );
                        let stores = read_stores(&found, &known);
                        (found, stores)
                    })
                    .await;
                if this
                    .update(cx, |app, cx| app.scripts_found(found, stores, cx))
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(POLL).await;
            }
        })
        .detach();
    }

    fn scripts_found(
        &mut self,
        found: scripts::Found,
        stores: Vec<(String, Store)>,
        cx: &mut Context<Self>,
    ) {
        // What was read meanwhile does not overwrite what a script wrote.
        for (id, store) in stores {
            self.scripts.stores.entry(id).or_insert(store);
        }
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
        let fared: Vec<scripts::Report> = self
            .scripts
            .found
            .iter()
            .map(|(script, stamp)| scripts::Report {
                script,
                fared: self.fared(script, *stamp),
                enabled: script_enabled(&script.id, cx),
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

    /// Turns script `id` on or off for the boards. Off, its views are let
    /// go — each one observes the application, shown or not —; the Plugins
    /// screen's preview mounts it again if it is the one selected.
    pub(super) fn set_script_enabled(&mut self, id: &str, enabled: bool, cx: &mut Context<Self>) {
        let id = id.to_string();
        super::settings::Settings::update_global(cx, |settings| {
            settings.disabled_plugins.retain(|known| *known != id);
            if !enabled {
                settings.disabled_plugins.push(id.clone());
                settings.disabled_plugins.sort();
            }
        });
        if !enabled {
            self.let_go(&id);
        }
        self.write_scripts_status(cx);
        cx.notify();
    }

    /// Lets go of every view of script `id`: each one observes the
    /// application, shown or not.
    fn let_go(&mut self, id: &str) {
        self.scripts.mounted.retain(|(_, of), mounted| {
            let kept = of != id;
            if !kept {
                forget_tracks(mounted.view.entity_id());
            }
            kept
        });
        self.scripts.failed.retain(|(_, of), _| of != id);
    }

    /// Forks builtin `id` into the user's folder, off the thread: the copy
    /// takes its place at the next reading, and stays selected.
    pub(super) fn fork_script(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(root) = root() else {
            return;
        };
        let id = id.to_string();
        // An installed plugin is copied from its folder, a builtin from the
        // binary.
        let installed = self
            .scripts
            .script(&id)
            .filter(|script| script.origin == scripts::Origin::Market)
            .map(|script| script.dir.clone());
        cx.spawn(async move |this, cx| {
            let (folder, of) = (root.clone(), id.clone());
            let forked = cx
                .background_executor()
                .spawn(async move {
                    match installed {
                        Some(from) => {
                            let into = folder.join(&of);
                            if into.exists() {
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::AlreadyExists,
                                    format!("{} exists already", into.display()),
                                ));
                            }
                            crate::market::copy_dir(&from, &into, 0).map(|()| into)
                        }
                        None => scripts::fork(&folder, &of, BUILTINS),
                    }
                })
                .await;
            let _ = this.update(cx, |app, cx| match forked {
                Ok(dir) => {
                    app.scripts.screen.selected = Some(id.clone());
                    app.announce(
                        tr!("plugins-forked", { id: id, path: dir.display().to_string() }),
                        cx,
                    );
                    app.write_scripts_status(cx);
                    cx.notify();
                }
                Err(error) => app.announce_error(SharedString::from(error.to_string()), cx),
            });
        })
        .detach();
    }

    /// Back to builtin `id`: the fork set aside in a hidden folder, never
    /// deleted.
    pub(super) fn unfork_script(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(root) = root() else {
            return;
        };
        let id = id.to_string();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        cx.spawn(async move |this, cx| {
            let (folder, of) = (root.clone(), id.clone());
            let aside = cx
                .background_executor()
                .spawn(async move { scripts::set_aside(&folder, &of, stamp) })
                .await;
            let _ = this.update(cx, |app, cx| match aside {
                Ok(path) => {
                    app.announce(
                        tr!("plugins-unforked", { id: id, path: path.display().to_string() }),
                        cx,
                    );
                    app.write_scripts_status(cx);
                    cx.notify();
                }
                Err(error) => app.announce_error(SharedString::from(error.to_string()), cx),
            });
        })
        .detach();
    }

    /// Forgets script `id` once its folder is gone for good: its views, its
    /// data in memory, and what the settings and the store say of it.
    pub(super) fn forget_script(&mut self, id: &str, cx: &mut Context<Self>) {
        self.let_go(id);
        self.scripts.stores.remove(id);
        super::settings::Settings::update_global(cx, |settings| {
            settings.disabled_plugins.retain(|off| off != id);
            if settings.home_script.trim() == id {
                settings.home_script.clear();
            }
        });
        super::store::Store::update_global(cx, |store| {
            for state in store.worktrees.values_mut() {
                if state.focus_script.as_deref() == Some(id) {
                    state.focus_script = None;
                }
            }
        });
        if self.scripts.screen.selected.as_deref() == Some(id) {
            self.scripts.screen.selected = None;
        }
        self.write_scripts_status(cx);
        cx.notify();
    }

    /// Removes the user's script `id`, off the thread: its folder put away
    /// under `scripts::REMOVED`, never deleted. Its data and secrets stay,
    /// for the folder to come back as it was.
    pub(super) fn remove_script(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(root) = root() else {
            return;
        };
        let id = id.to_string();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        cx.spawn(async move |this, cx| {
            let of = id.clone();
            let removed = cx
                .background_executor()
                .spawn(async move { scripts::remove(&root, &of, stamp) })
                .await;
            let _ = this.update(cx, |app, cx| match removed {
                Ok(path) => {
                    if app.scripts.screen.selected.as_deref() == Some(id.as_str()) {
                        app.scripts.screen.selected = None;
                    }
                    app.let_go(&id);
                    app.announce(
                        tr!("plugins-removed", { id: id, path: path.display().to_string() }),
                        cx,
                    );
                    app.write_scripts_status(cx);
                    cx.notify();
                }
                Err(error) => app.announce_error(SharedString::from(error.to_string()), cx),
            });
        })
        .detach();
    }

    /// Renames the user's script `from` to `to`, off the thread. Its data
    /// and its declared secrets go with it — both are filed by id —, and so
    /// does what the settings and the store say of it.
    pub(super) fn rename_script(&mut self, from: &str, to: &str, cx: &mut Context<Self>) {
        let (Some(root), Some(data)) = (root(), data_root()) else {
            return;
        };
        let (from, to) = (from.to_string(), to.trim().to_string());
        if from == to {
            return;
        }
        let secrets: Vec<String> = self
            .scripts
            .script(&from)
            .map(|script| {
                script
                    .permissions
                    .secrets
                    .iter()
                    .map(|secret| secret.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let (old, new) = (from.clone(), to.clone());
            let renamed = cx
                .background_executor()
                .spawn(async move { rename_with_data(&root, &data, &old, &new, &secrets) })
                .await;
            let _ = this.update(cx, |app, cx| match renamed {
                Ok(()) => {
                    if let Some(store) = app.scripts.stores.remove(&from) {
                        app.scripts.stores.insert(to.clone(), store);
                    }
                    app.let_go(&from);
                    super::settings::Settings::update_global(cx, |settings| {
                        for off in &mut settings.disabled_plugins {
                            if *off == from {
                                off.clone_from(&to);
                            }
                        }
                        settings.disabled_plugins.sort();
                        if settings.home_script.trim() == from {
                            settings.home_script.clone_from(&to);
                        }
                    });
                    super::store::Store::update_global(cx, |store| {
                        for state in store.worktrees.values_mut() {
                            if state.focus_script.as_deref() == Some(from.as_str()) {
                                state.focus_script = Some(to.clone());
                            }
                        }
                    });
                    if app.scripts.screen.selected.as_deref() == Some(from.as_str()) {
                        app.scripts.screen.selected = Some(to.clone());
                    }
                    app.announce(tr!("plugins-renamed", { from: from, to: to }), cx);
                    app.write_scripts_status(cx);
                    cx.notify();
                }
                Err(error) => app.announce_error(SharedString::from(error), cx),
            });
        })
        .detach();
    }

    /// The script views run again, for a change the application does not
    /// notify — their stored data cleared. The rest reaches them by their
    /// observation of the application (`mounted`).
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
        let current = self
            .scripts
            .mounted
            .get(&key)
            .map(|mounted| (mounted.view.clone(), mounted.stamp));
        let failed = self
            .scripts
            .failed
            .get(&key)
            .filter(|(at, _)| *at == stamp)
            .map(|(_, why)| why.clone());
        let fresh = current.as_ref().is_some_and(|(_, at)| *at == stamp);
        if !fresh && failed.is_none() && self.scripts.mounting.insert(key.clone()) {
            let app = cx.entity();
            window.defer(cx, move |window, cx| {
                let result = mount(&app, &key.0, &script, window, cx);
                app.update(cx, |this, cx| this.mounted(key, &script, stamp, result, cx));
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
        result: gpui_shell::anyhow::Result<Entity<ScriptView>>,
        cx: &mut Context<Self>,
    ) {
        self.scripts.mounting.remove(&key);
        match result {
            Ok(view) => {
                self.scripts.failed.remove(&key);
                let app = cx.entity();
                view.update(cx, |_, cx| {
                    cx.observe(&app, |view, _, cx| view.refresh(cx)).detach();
                });
                let replaced = self.scripts.mounted.insert(key, Mounted { view, stamp });
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

    /// Script `id`'s data — read with the folder (`read_stores`), never here.
    pub(super) fn store_of(&mut self, id: &str) -> &mut Store {
        self.scripts.stores.entry(id.to_string()).or_default()
    }

    /// Whether script `id` keeps anything — for a render, which changes nothing.
    pub(super) fn has_store(&self, id: &str) -> bool {
        self.scripts
            .stores
            .get(id)
            .is_some_and(|store| !store.is_empty())
    }

    /// Changes script `id`'s data, and queues it to be written.
    fn store_update(&mut self, id: &str, change: impl FnOnce(&mut Store), cx: &mut Context<Self>) {
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
                        if let Err(error) = crate::files::write_atomic(&path, &text) {
                            log::warn!("writing {}: {error}", path.display());
                        }
                    }
                })
                .detach();
            sender
        });
        let _ = writer.try_send((root.join(format!("{id}.json")), text));
    }

    /// The views of script `id` on the boards other than `path`'s run again:
    /// its data changed under them. Deferred — the script that wrote is still
    /// running, and the runtime is the same.
    fn refresh_elsewhere(&self, id: &str, path: &Path, cx: &mut Context<Self>) {
        let views: Vec<Entity<ScriptView>> = self
            .scripts
            .mounted
            .iter()
            .filter(|((worktree, of), _)| of == id && worktree != path)
            .map(|(_, mounted)| mounted.view.clone())
            .collect();
        if views.is_empty() {
            return;
        }
        cx.defer(move |cx| {
            for view in views {
                view.update(cx, |view, cx| view.refresh(cx));
            }
        });
    }

    /// Forgets script `id`'s data, on disk too.
    pub(super) fn clear_store(&mut self, id: &str, cx: &mut Context<Self>) {
        self.store_update(id, |store| store.clear(), cx);
    }

    /// Where script `id`'s data is written.
    pub(super) fn store_path(id: &str) -> Option<PathBuf> {
        data_root().map(|root| root.join(format!("{id}.json")))
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
                    && script_enabled(id, cx)
            })
    }

    /// The tab scripts the boards show: those found and not turned off.
    pub(super) fn enabled_scripts(&self, kind: Kind, cx: &App) -> Vec<Script> {
        self.scripts
            .of_kind(kind)
            .filter(|script| script_enabled(&script.id, cx))
            .cloned()
            .collect()
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
            .filter(|script| script.kind == Kind::Home && script_enabled(id, cx))
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

    fn script_list(&self, cx: &App) -> HostValue {
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
                        .field("enabled", script_enabled(&script.id, cx))
                        .field(
                            "origin",
                            match script.origin {
                                scripts::Origin::User => "user",
                                scripts::Origin::Builtin => "builtin",
                                scripts::Origin::Fork => "fork",
                                scripts::Origin::Market => "market",
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
        script.origin != scripts::Origin::Market,
        Some(window.window_handle()),
    );
    let policy = Policy::new()
        .with_application(&script.id)
        .with_capabilities(capabilities())
        .with_host_module(module)
        .map_err(|error| gpui_shell::anyhow::anyhow!("{}", error.message()))?;
    gpui_shell::policy::set_default(policy);
    let view = runtime
        .load_application(&script.dir, &script.entry)
        .and_then(|loaded| runtime.mount_application(&loaded, window, cx));
    gpui_shell::policy::set_default(Policy::new());
    view
}

/// Whether the user has script `id` on — see `Settings::disabled_plugins`.
pub(super) fn script_enabled(id: &str, cx: &App) -> bool {
    !super::settings::Settings::global(cx)
        .disabled_plugins
        .iter()
        .any(|off| off == id)
}

/// What a script may do beyond drawing: HTTP and HTTPS requests, anywhere —
/// the user asked for plugins trusted with the network as an application
/// is, a grant per host being a question at every new address —, and
/// writing to the clipboard. No file, no process, no raw socket, no
/// `localStorage`: the module keeps its data (`storage_*`) and its secrets
/// (`secret`).
fn capabilities() -> Capabilities {
    Capabilities::new()
        .any_http_request(true)
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
export interface ScriptInfo {
  id: string;
  title: string;
  kind: "tab" | "home";
  /** Off, no board shows it. */
  enabled: boolean;
  /** Shipped with Claudhub, the user's own, the user's copy of a shipped one, or installed from a marketplace. */
  origin: "builtin" | "user" | "fork" | "market";
}
/** A skill an agent started in this worktree can read — see the agent library. */
export interface Skill {
  name: string;
  description: string;
  /** What it takes after its name, as its front matter says; empty when it says nothing. */
  argument_hint: string;
  /** Its folder, and its `SKILL.md`: paths of the machine the agents run on. */
  dir: string;
  file: string;
  /** The project's — versioned, shared by a commit —, the user's, or an installed plugin's. */
  scope: "project" | "personal" | "plugin";
  /** The agent that finds it of itself where it lies; any agent can read the file. */
  reader: "claude" | "codex";
  plugin: string | null;
  /** The project's companion prompt for it — `.claudhub/prompts/<name>.md`, versioned —; empty when none. */
  prompt: string;
}
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
/** The skills an agent started in this worktree can read, the project's first; null while they are listed. */
export function skills(): Skill[] | null;
/** A skill's `SKILL.md` past its front matter, by its folder; null when it is not listed. */
export function skill_body(dir: string): string | null;
/** Lists the skills again — after one was written, pulled or installed. */
export function reload_skills(): void;
/** Copies a personal or plugin skill into the project's folder for its agent, where a commit shares it. */
export function share_skill(dir: string): void;
/** Writes a skill's companion prompt into the project, by the skill's name; a blank one is removed. */
export function save_skill_prompt(name: string, text: string): void;
/** The chat agents the settings offer, by name. */
export function chat_agents(): string[];
/**
 * Opens a chat with the agent of that name on the worktree, and sends it `prompt`
 * once its session is up; the chat becomes the terminal `Terminals` shows.
 */
export function start_agent(agent: string, prompt: string): void;

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
///
/// A plugin installed from a marketplace is not `trusted`: what it hands the
/// agent is shown to the user first, as `ask_agent` does — an agent acts, and
/// someone else's code is not to prompt it unseen.
fn module(
    app: WeakEntity<ClaudhubApp>,
    path: &Path,
    id: &str,
    trusted: bool,
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
        .function(
            "scripts",
            read(&app, path, |this, _, cx| this.script_list(cx)),
        )
        .function("language", |_| Ok(HostValue::from(&*rust_i18n::locale())))
        .function("skills", {
            let (app, path) = (app.clone(), path.to_path_buf());
            move |_| change(&app, |this, _| Ok(this.script_skills(&path)))
        })
        .function("skill_body", {
            let (app, path) = (app.clone(), path.to_path_buf());
            move |arguments| {
                let dir = arguments.string(0)?.to_string();
                change(&app, |this, _| Ok(this.script_skill_body(&path, &dir)))
            }
        })
        .function("reload_skills", {
            let (app, path) = (app.clone(), path.to_path_buf());
            move |_| {
                change(&app, |this, _| {
                    this.reload_library(&path);
                    Ok(HostValue::Null)
                })
            }
        })
        .function("share_skill", {
            let (app, path) = (app.clone(), path.to_path_buf());
            move |arguments| {
                let dir = arguments.string(0)?.to_string();
                change(&app, |this, _| {
                    this.share_skill(&path, &dir)
                        .map(|()| HostValue::Null)
                        .map_err(HostError::new)
                })
            }
        })
        .function("save_skill_prompt", {
            let (app, path) = (app.clone(), path.to_path_buf());
            move |arguments| {
                let name = arguments.string(0)?.to_string();
                let text = arguments.string(1)?.to_string();
                change(&app, |this, _| {
                    this.save_skill_prompt(&path, &name, &text);
                    Ok(HostValue::Null)
                })
            }
        })
        .function("chat_agents", |_| {
            gpui_shell::with_current_app(|cx| ClaudhubApp::script_chat_agents(cx))
                .ok_or_else(unreachable)
        })
        .function("start_agent", {
            let (app, path) = (app.clone(), path.to_path_buf());
            move |arguments| {
                let agent = arguments.string(0)?.to_string();
                let prompt = arguments.string(1)?.to_string();
                let path = path.clone();
                later(&app, window, move |this, window, cx| {
                    this.start_library_agent(&path, &agent, prompt, window, cx)
                })
            }
        })
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
        // A write that changes nothing is not one: a script that stores in
        // its render would otherwise run the others, which run it again.
        .function("storage_set", {
            let (app, id, path) = (app.clone(), id.clone(), path.to_path_buf());
            move |arguments| {
                let key = arguments.string(0)?.to_string();
                let value = to_json(arguments.value(1)?);
                change(&app, |this, cx| {
                    if this.store_of(&id).get(&key) != Some(&value) {
                        this.store_update(
                            &id,
                            |store| {
                                store.insert(key, value);
                            },
                            cx,
                        );
                        this.refresh_elsewhere(&id, &path, cx);
                    }
                    Ok(HostValue::Null)
                })
            }
        })
        .function("storage_remove", {
            let (app, id, path) = (app.clone(), id.clone(), path.to_path_buf());
            move |arguments| {
                let key = arguments.string(0)?.to_string();
                change(&app, |this, cx| {
                    if this.store_of(&id).contains_key(&key) {
                        this.store_update(
                            &id,
                            |store| {
                                store.remove(&key);
                            },
                            cx,
                        );
                        this.refresh_elsewhere(&id, &path, cx);
                    }
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
                    if trusted {
                        this.send_to_agent(&path, text, window, cx)
                    } else {
                        this.confirm_agent_prompt(path, text, window, cx)
                    }
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

/// Renames script `from`'s folder, data file and declared secrets to `to`.
/// The data of a script once named `to` is not written over: refused.
fn rename_with_data(
    root: &Path,
    data: &Path,
    from: &str,
    to: &str,
    secrets: &[String],
) -> Result<(), String> {
    let (old, new) = (
        data.join(format!("{from}.json")),
        data.join(format!("{to}.json")),
    );
    if old.exists() && new.exists() {
        return Err(format!(
            "{} holds the data of an earlier `{to}`: move it away first",
            new.display()
        ));
    }
    let builtins = scripts::ids_of(BUILTINS);
    scripts::rename(root, from, to, &builtins).map_err(|error| error.to_string())?;
    if old.exists() {
        std::fs::rename(&old, &new).map_err(|error| error.to_string())?;
    }
    for name in secrets {
        if let Err(error) = move_secret(from, to, name) {
            log::warn!("script {from}: moving its secret {name}: {error}");
        }
    }
    Ok(())
}

/// Moves a script's secret `name` from id `from` to id `to`.
fn move_secret(from: &str, to: &str, name: &str) -> Result<(), String> {
    let said = |error: HostError| error.message().to_string();
    let (old, new) = (
        secret_entry(from, name).map_err(said)?,
        secret_entry(to, name).map_err(said)?,
    );
    match old.get_password() {
        Ok(secret) => {
            new.set_password(&secret)
                .map_err(|error| error.to_string())?;
            old.delete_credential().map_err(|error| error.to_string())
        }
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
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
        let module = module(
            WeakEntity::new_invalid(),
            Path::new("/w"),
            "test",
            true,
            None,
        );
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

    /// The scripts shipped as examples and as builtins, and the skeletons of
    /// a new one, are found, link against the module's declared exports and evaluate — a renamed export or a typo in an
    /// import fails here rather than on someone's first board.
    #[test]
    fn the_shipped_scripts_load_against_the_module() {
        let root = std::env::temp_dir().join(format!("claudhub-scripts-{}", std::process::id()));
        let keep = std::env::temp_dir().join("claudhub-scripts-kept");
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            scripts::seed(&root, EXAMPLES).unwrap(),
            ["home-columns", "dashboard"]
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
                "dashboard/main.js" => (*path, "// an older version\n"),
                _ => (*path, *content),
            })
            .collect();
        let elsewhere = root.with_extension("older");
        let _ = std::fs::remove_dir_all(&elsewhere);
        scripts::seed(&elsewhere, &older).unwrap();
        assert_eq!(scripts::seed(&elsewhere, EXAMPLES).unwrap(), ["dashboard"]);
        assert_eq!(
            std::fs::read_to_string(elsewhere.join("dashboard/main.js")).unwrap(),
            include_str!("../../assets/scripts/dashboard/main.js")
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

        // The builtins: written once, not again while they are this build's.
        let builtins = root.with_extension("builtin");
        let _ = std::fs::remove_dir_all(&builtins);
        assert!(scripts::install_builtins(&builtins, BUILTINS).unwrap());
        assert!(!scripts::install_builtins(&builtins, BUILTINS).unwrap());
        // Sentry as the example it was: seeded, untouched, it gives way…
        let sentry: Vec<(&str, &str)> = BUILTINS
            .iter()
            .copied()
            .filter(|(path, _)| path.starts_with("sentry/"))
            .collect();
        let legacy = root.with_extension("legacy");
        let _ = std::fs::remove_dir_all(&legacy);
        scripts::seed(&legacy, &sentry).unwrap();
        std::fs::write(legacy.join("sentry/gpui-kit.d.ts"), "// generated").unwrap();
        assert!(scripts::retire(&legacy, "sentry", BUILTINS, &[]).unwrap());
        assert!(!legacy.join("sentry").exists());
        assert!(!std::fs::read_to_string(legacy.join(".examples"))
            .unwrap()
            .contains("sentry"));
        // …but changed, it is a fork, and stays.
        scripts::seed(&legacy, &sentry).unwrap();
        std::fs::write(legacy.join("sentry/main.js"), "// mine\n").unwrap();
        assert!(!scripts::retire(&legacy, "sentry", BUILTINS, &[]).unwrap());
        // A fork is written once, and set aside rather than deleted.
        assert!(scripts::fork(&legacy, "sentry", BUILTINS).is_err());
        let aside = scripts::set_aside(&legacy, "sentry", 7).unwrap();
        assert_eq!(
            std::fs::read_to_string(aside.join("main.js")).unwrap(),
            "// mine\n"
        );
        let forked = scripts::fork(&legacy, "http-client", BUILTINS).unwrap();
        assert!(forked.join(scripts::MANIFEST).is_file());
        assert!(!scripts::retire(&legacy, "http-client", &[], &[]).unwrap());
        let merged = scripts::merge(
            scripts::Found::default(),
            scripts::discover(&builtins, "fr"),
            scripts::discover(&legacy, "fr"),
        );
        let origins: Vec<(&str, scripts::Origin)> = merged
            .scripts
            .iter()
            .map(|(script, _)| (script.id.as_str(), script.origin))
            .collect();
        assert_eq!(
            origins,
            [
                ("agent-library", scripts::Origin::Builtin),
                ("http-client", scripts::Origin::Fork),
                ("sentry", scripts::Origin::Builtin),
            ]
        );
        // A copy identical to the builtin's is no fork either.
        assert!(scripts::retire(&legacy, "http-client", BUILTINS, &[]).unwrap());
        let _ = std::fs::remove_dir_all(&legacy);

        let found = scripts::merge(
            scripts::Found::default(),
            scripts::discover(&builtins, "fr"),
            scripts::discover(&root, "fr"),
        );
        assert_eq!(found.broken, Vec::<(String, String)>::new());
        let kinds: Vec<(&str, Kind)> = found
            .scripts
            .iter()
            .map(|(script, _)| (script.id.as_str(), script.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("agent-library", Kind::Tab),
                ("dashboard", Kind::Tab),
                ("home-columns", Kind::Home),
                ("http-client", Kind::Tab),
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

        // A script of the user's is renamed, but never to a builtin's name —
        // it would become its fork — nor over another; and removed, it is
        // put away, not deleted.
        let builtin_ids = scripts::ids_of(BUILTINS);
        assert!(scripts::rename(&root, "new-tab", "sentry", &builtin_ids).is_err());
        assert!(scripts::rename(&root, "new-tab", "new-home", &builtin_ids).is_err());
        assert!(scripts::rename(&root, "new-tab", "../out", &builtin_ids).is_err());
        scripts::rename(&root, "new-tab", "mine", &builtin_ids).unwrap();
        let removed = scripts::remove(&root, "mine", 7).unwrap();
        assert!(removed.starts_with(root.join(scripts::REMOVED)));
        assert!(removed.join(scripts::MANIFEST).is_file());
        let ids: Vec<String> = scripts::discover(&root, "fr")
            .scripts
            .into_iter()
            .map(|(script, _)| script.id)
            .collect();
        assert_eq!(ids, ["dashboard", "home-columns", "new-home"]);
        let mut stored = Store::new();
        stored.insert("kept".into(), serde_json::Value::Bool(true));
        let data = root.join("data.json");
        crate::files::write_atomic(&data, &serde_json::to_string(&stored).unwrap()).unwrap();
        assert_eq!(
            std::fs::read_dir(&root)
                .unwrap()
                .flatten()
                .filter(|entry| { entry.file_name().to_string_lossy().ends_with(".tmp") })
                .count(),
            0
        );
        assert!(std::fs::read_to_string(&data).unwrap().contains("kept"));

        // Kept on demand, to read the `gpui-kit.d.ts` the runtime wrote.
        if std::env::var_os("CLAUDHUB_KEEP_SCRIPTS").is_some() {
            let _ = std::fs::remove_dir_all(&keep);
            let _ = std::fs::rename(&root, &keep);
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&builtins);
    }
}
