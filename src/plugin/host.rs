//! The Rune host: what a plugin's script can call, and how it is run.
//!
//! **This is the only module that knows Rune**, and it is behind the `plugins`
//! feature, itself part of `ui`. The script runs on the interface's side; only
//! its input and output cross the wire, as [`crate::plugin::caps::Cap`]s the
//! core executes. That is what lets `claudhub-server` carry a plugin's requests
//! without carrying a scripting engine — and `just check-server` is what proves
//! it still does.
//!
//! # The script's contract
//!
//! Three entry points, and the split between them is the whole design:
//!
//! ```text
//! pub async fn init(worktree)                 -> Result<state, String>
//! pub fn view(state, panel)                   -> node
//! pub async fn update(state, action, payload) -> Result<state, String>
//! ```
//!
//! `view` is **synchronous and pure**: it turns a state into a tree and touches
//! nothing. That is what makes it safe to call whenever the state moves, and it
//! is the rule the whole window already lives by — `diff_view::Rendered` is
//! computed when the diff arrives, never in the render closure. `init` and
//! `update` are where the input and output happen.
//!
//! The state is an opaque Rune value kept here between calls. It is **not**
//! carried across a reload: recompiling gives new types for the same names, and
//! a stale object read by fresh code fails in ways nobody can explain. A reload
//! therefore replays `init`, which is what one wants after editing the code
//! that fetches.
//!
//! # Waiting without hanging
//!
//! A capability call registers a one-shot channel under an id that never goes
//! back — the language client's device, and the SQL console's — hands the
//! request to the application through `outbox`, and awaits the answer. Nothing
//! is left in that table: [`Shared::resolve`] answers it, the caller's timeout
//! answers it, and dropping the plugin fails everything it still held.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rune::runtime::{Ref, RuntimeContext};
use rune::{Any, Context, Diagnostics, Source, Sources, Unit, Value, Vm};

use crate::plugin::caps::Cap;
use crate::plugin::manifest::{Capability, Manifest};
use crate::plugin::view::{Handler, Item, Node, Pair, TextStyle, Tone};
use crate::runtime::Secret;

/// What a script asks the application to do, on this side of the wire.
///
/// Handing a text to the agent, opening a file, posting a notification: none of
/// these leaves the interface's process, so none of them is a capability. They
/// are collected as the script runs and drained when it returns — which is also
/// what keeps them in the order they were asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Paste a text into the worktree's agent terminal, opening one if needed.
    Agent(String),
    /// Open a file in the built-in editor, at a line.
    Open { path: PathBuf, line: usize },
    /// Say something in the status bar.
    Notify(String),
    /// Put a text in the clipboard.
    ///
    /// A reference one carries elsewhere — an issue's short id, a run's number,
    /// a sha — is read once and pasted into a branch name, a commit message, a
    /// chat. Selecting it by hand out of a panel that has no text selection is
    /// the one thing a plugin could not offer.
    Copy(String),
    /// Hand an address to the system's browser.
    ///
    /// A `Node::Link`'s gesture, and one a script cannot make itself: it has no
    /// capability that reaches a browser, and giving it one would be giving it
    /// a way to open anything at all without saying so in its manifest.
    OpenUrl(String),
    /// Create a worktree and hand a text to the agent that lands in it.
    ///
    /// Not Sentry's gesture but everyone's: opening a branch for a CI failure,
    /// for an issue, for a report, and starting the agent on it with what one
    /// already knows. Two things in one effect because they cannot be separated
    /// — the prompt has to wait for `wt` to finish its hooks, which take
    /// minutes and whose only signal is the worktree list coming back.
    Worktree { name: String, prompt: String },
    /// Remember something about the repository being looked at.
    ///
    /// **Per repository and not per worktree**, because that is what a
    /// project's configuration means: a Sentry project, a board's identifier,
    /// an API's base address are the same across five checkouts of the same
    /// code. A plugin that wants finer grain puts `worktree()` in its own key —
    /// the namespace is its own.
    Remember { key: String, value: String },
}

/// One capability call on its way out.
pub struct Request {
    /// Which plugin is asking. One outbox for all of them: the application has
    /// one place to drain, and the answer finds its way back by this name.
    pub plugin: String,
    pub call: u64,
    pub cap: Cap,
}

/// What the host functions share with the application.
///
/// Behind `Arc` because the closures registered in the Rune module outlive any
/// one call, and behind `Mutex` because the settings change under them — a
/// module is built once per plugin, and rebuilding it on every settings change
/// would recompile its script for a changed token.
pub struct Shared {
    pub id: String,
    worktree: Mutex<Option<PathBuf>>,
    settings: Mutex<BTreeMap<String, String>>,
    secrets: Mutex<BTreeMap<String, String>>,
    /// What this plugin has remembered about the repository being looked at.
    /// Refreshed by the application, exactly like the settings: the host holds
    /// no store of its own, and cannot — a store is a gpui global.
    state: Mutex<BTreeMap<String, String>>,
    /// What the system keyring has already answered, by the declaration that
    /// asked for it. Opening a keyring can ask the user to unlock it.
    keyring: Mutex<BTreeMap<String, String>>,
    allowed: Vec<Capability>,
    outbox: async_channel::Sender<Request>,
    pending: Mutex<HashMap<u64, async_channel::Sender<Result<String, String>>>>,
    next: AtomicU64,
    effects: Mutex<Vec<Effect>>,
}

impl Shared {
    pub fn new(manifest: &Manifest, outbox: async_channel::Sender<Request>) -> Arc<Self> {
        Arc::new(Self {
            id: manifest.id.clone(),
            worktree: Mutex::new(None),
            settings: Mutex::new(manifest.declaration.settings.clone()),
            secrets: Mutex::new(BTreeMap::new()),
            state: Mutex::new(BTreeMap::new()),
            keyring: Mutex::new(BTreeMap::new()),
            allowed: manifest.declaration.capabilities.clone(),
            outbox,
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            effects: Mutex::new(Vec::new()),
        })
    }

    /// The worktree the window is showing. A plugin's panel says something
    /// about *this* worktree, like every other panel.
    pub fn set_worktree(&self, worktree: Option<PathBuf>) {
        *self.worktree.lock().expect("worktree lock") = worktree;
    }

    /// Settings and secrets from the settings file, laid over the manifest's
    /// defaults. Called whenever they change, without recompiling anything.
    pub fn configure(&self, settings: BTreeMap<String, String>, secrets: BTreeMap<String, String>) {
        let mut current = self.settings.lock().expect("settings lock");
        for (key, value) in settings {
            current.insert(key, value);
        }
        *self.secrets.lock().expect("secrets lock") = secrets;
        // A token corrected in the form must not be answered from a cache.
        self.keyring.lock().expect("keyring lock").clear();
    }

    /// What the repository being looked at has remembered of this plugin.
    pub fn remember(&self, state: BTreeMap<String, String>) {
        *self.state.lock().expect("state lock") = state;
    }

    /// Hands one capability's answer back to whoever is awaiting it.
    ///
    /// An unknown id is not an error: it is a call the timeout already failed,
    /// or one a reload swept away, and the answer simply arrives too late.
    pub fn resolve(&self, call: u64, result: Result<String, String>) {
        let sender = self.pending.lock().expect("pending lock").remove(&call);
        if let Some(sender) = sender {
            let _ = sender.try_send(result);
        }
    }

    /// Fails everything still in flight. A reload, a plugin switched off, a
    /// server lost: a request nobody will ever answer must not leave a panel
    /// spinning for good.
    pub fn fail_all(&self, why: &str) {
        let pending = std::mem::take(&mut *self.pending.lock().expect("pending lock"));
        for (_, sender) in pending {
            let _ = sender.try_send(Err(why.to_string()));
        }
    }

    /// What the script asked of the application while it was running.
    pub fn take_effects(&self) -> Vec<Effect> {
        std::mem::take(&mut *self.effects.lock().expect("effects lock"))
    }

    fn effect(&self, effect: Effect) {
        self.effects.lock().expect("effects lock").push(effect);
    }

    /// The value behind a declared secret's name.
    ///
    /// Three forms, and they answer three different questions about where a
    /// token should live:
    ///
    /// - the value itself, in `settings.json` — written `0600`, and in clear;
    /// - `$NAME`, read from the environment **of the worker**, because that is
    ///   where the process that makes the request runs;
    /// - `keyring:…`, read from the system keyring **here**, because a keyring
    ///   belongs to a desktop session — which is the Windows side when the
    ///   workers live in WSL. Resolving it in the worker would look for a
    ///   session bus that a headless distribution does not have.
    fn secret(&self, name: &str) -> Option<Secret> {
        let value = self
            .secrets
            .lock()
            .expect("secrets lock")
            .get(name)
            .filter(|value| !value.trim().is_empty())
            .cloned()?;
        let Some(entry) = KeyringEntry::parse(&value) else {
            return Some(Secret(value));
        };
        // Cached after the first read: opening a keyring can ask the user to
        // unlock it, and a panel that fetches on every gesture would ask again
        // and again. Cleared whenever the settings change.
        if let Some(hit) = self
            .keyring
            .lock()
            .expect("keyring lock")
            .get(&value)
            .cloned()
        {
            return Some(Secret(hit));
        }
        match entry.read() {
            Ok(found) => {
                self.keyring
                    .lock()
                    .expect("keyring lock")
                    .insert(value, found.clone());
                Some(Secret(found))
            }
            Err(e) => {
                // Said and not silently empty: an unresolved placeholder makes
                // the request be refused with "no secret", which would send one
                // looking at the wrong thing.
                log::warn!(target: "plugin", "{}: {} — {e}", self.id, entry.describe());
                None
            }
        }
    }

    /// Sends a capability out and waits for its answer.
    async fn call(self: &Arc<Self>, capability: Capability, cap: Cap) -> Result<String, String> {
        if !self.allowed.contains(&capability) {
            // Declared and not inferred: a plugin that reaches for something its
            // manifest does not list is a plugin doing something its author did
            // not write down, and saying so beats doing it.
            return Err(format!(
                "{}: the capability `{}` is not declared in its plugin.toml",
                self.id,
                capability.name()
            ));
        }
        let call = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = async_channel::bounded(1);
        self.pending.lock().expect("pending lock").insert(call, tx);
        let request = Request {
            plugin: self.id.clone(),
            call,
            cap,
        };
        if self.outbox.try_send(request).is_err() {
            self.pending.lock().expect("pending lock").remove(&call);
            return Err("Claudhub is no longer listening".into());
        }
        rx.recv()
            .await
            .unwrap_or_else(|_| Err("the request was abandoned".into()))
    }
}

/// A secret kept in the system keyring rather than in a settings file.
///
/// Written `keyring:account` or `keyring:service/account`. The service defaults
/// to `claudhub`, which is what one wants nine times out of ten — and naming it
/// is what lets a plugin read an entry some other program created.
#[derive(Debug, PartialEq, Eq)]
pub struct KeyringEntry {
    service: String,
    account: String,
}

impl KeyringEntry {
    const PREFIX: &'static str = "keyring:";
    const DEFAULT_SERVICE: &'static str = "claudhub";

    /// `None` when the value is not a keyring reference at all.
    pub fn parse(value: &str) -> Option<Self> {
        let rest = value.trim().strip_prefix(Self::PREFIX)?.trim();
        if rest.is_empty() {
            return None;
        }
        Some(match rest.split_once('/') {
            Some((service, account)) if !service.is_empty() && !account.is_empty() => Self {
                service: service.to_string(),
                account: account.to_string(),
            },
            // A lone slash on either side is a typo, not a service: what is
            // there is taken as the account under the default service, which is
            // what the writer meant.
            _ => Self {
                service: Self::DEFAULT_SERVICE.to_string(),
                account: rest.trim_matches('/').to_string(),
            },
        })
    }

    /// Where Claudhub puts a plugin's secret when it stores one itself.
    ///
    /// One entry per plugin and per declared name, under our own service: two
    /// plugins asking for a `token` are two different tokens.
    pub fn for_plugin(plugin: &str, name: &str) -> Self {
        Self {
            service: Self::DEFAULT_SERVICE.to_string(),
            account: format!("{plugin}.{name}"),
        }
    }

    /// What the settings file records in place of the value.
    ///
    /// The service is left out when it is ours: `keyring:sentry.token` is what
    /// one wants to read in a file, and spelling `claudhub/` in front of every
    /// one of them would say nothing.
    pub fn reference(&self) -> String {
        if self.service == Self::DEFAULT_SERVICE {
            format!("{}{}", Self::PREFIX, self.account)
        } else {
            format!("{}{}/{}", Self::PREFIX, self.service, self.account)
        }
    }

    pub fn describe(&self) -> String {
        format!("keyring {}/{}", self.service, self.account)
    }

    fn entry(&self) -> Result<keyring::Entry, keyring::Error> {
        keyring::Entry::new(&self.service, &self.account)
    }

    fn read(&self) -> Result<String, keyring::Error> {
        self.entry()?.get_password()
    }

    /// **Never on the interface thread.** Opening a keyring can ask the user to
    /// unlock it, and the call blocks until they answer.
    pub fn store(&self, value: &str) -> Result<(), keyring::Error> {
        self.entry()?.set_password(value)
    }

    /// Removing what is no longer referenced. A missing entry is not a failure:
    /// it is the state one was asking for.
    pub fn forget(&self) -> Result<(), keyring::Error> {
        match self.entry()?.delete_credential() {
            Err(keyring::Error::NoEntry) => Ok(()),
            other => other,
        }
    }
}

/// Where a secret's value is kept, read off the value alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretPlace {
    /// In `settings.json`, in clear.
    Plain,
    /// In the environment of whichever process makes the request.
    Environment,
    /// In the system keyring.
    Keyring,
}

impl SecretPlace {
    pub fn of(value: &str) -> Self {
        let value = value.trim();
        if KeyringEntry::parse(value).is_some() {
            Self::Keyring
        } else if value.starts_with('$') && value.len() > 1 {
            Self::Environment
        } else {
            Self::Plain
        }
    }

    /// Does the value name where it lives rather than being it.
    ///
    /// That is what decides whether Claudhub stores anything: a reference typed
    /// by hand is recorded as it stands, a token is put away.
    pub fn is_reference(self) -> bool {
        !matches!(self, Self::Plain)
    }

    /// The i18n key of the one-word label the form shows under the field.
    pub fn label(self) -> &'static str {
        match self {
            Self::Plain => "settings-plugin-secret-plain",
            Self::Environment => "settings-plugin-secret-env",
            Self::Keyring => "settings-plugin-secret-keyring",
        }
    }
}

/// The node type the script builds. Opaque on its side, a [`Node`] on ours.
#[derive(Any, Debug, Clone)]
#[rune(item = ::claudhub)]
pub struct RuneNode(Node);

/// One row of a list.
#[derive(Any, Debug, Clone)]
#[rune(item = ::claudhub)]
pub struct RuneItem(Item);

/// One line of a two-column table.
#[derive(Any, Debug, Clone)]
#[rune(item = ::claudhub)]
pub struct RunePair(Pair);

/// Empty means absent. A script has no `Option` worth the ceremony here, and
/// "no icon" and "the icon called nothing" are the same thing.
fn maybe(text: &str) -> Option<String> {
    Some(text.trim())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Builds the module a script sees. Done once per plugin — see [`Compiler`],
/// which holds it: its closures capture that plugin's `Shared`, and a reload
/// keeps them.
///
/// **Every text argument is a `Ref<str>` and never a `String`**, and this is
/// the one thing here that cannot be guessed. Rune passes an argument by
/// *taking* it: a host function declared `|t: String|` moves the field out of
/// the object it came from, so `item(run.title, …)` empties `run.title` — the
/// first `view` works, the second fails with "value is moved", and nothing
/// points at the line that did it. `Ref<str>` reads without taking. The async
/// functions convert to owned **before** the future is built, so no borrow
/// guard is held across an `await`, which would leave the state borrowed for as
/// long as the request lasts.
fn module(shared: &Arc<Shared>) -> Result<rune::Module, rune::ContextError> {
    let mut module = rune::Module::with_crate("claudhub")?;
    module.ty::<RuneNode>()?;
    module.ty::<RuneItem>()?;
    module.ty::<RunePair>()?;

    // — The view vocabulary ——————————————————————————————————————————
    fn styled(text: &str, style: TextStyle) -> RuneNode {
        RuneNode(Node::Text {
            text: text.to_string(),
            style,
        })
    }
    module
        .function("text", |t: Ref<str>| styled(&t, TextStyle::Body))
        .build()?;
    module
        .function("title", |t: Ref<str>| styled(&t, TextStyle::Title))
        .build()?;
    module
        .function("dim", |t: Ref<str>| styled(&t, TextStyle::Dim))
        .build()?;
    module
        .function("mono", |t: Ref<str>| styled(&t, TextStyle::Mono))
        .build()?;
    module
        .function("code", |text: Ref<str>, language: Ref<str>| {
            RuneNode(Node::Code {
                text: text.to_string(),
                language: maybe(&language),
                start_line: None,
                mark: None,
            })
        })
        .build()?;
    // The same excerpt, numbered from where it was cut out and with the line
    // the trace named picked out. Kept apart from `code` rather than folded
    // into it with two zeroes: an excerpt with no place in a file is the
    // common case, and four arguments for it would be four arguments to read.
    module
        .function(
            "snippet",
            |text: Ref<str>, language: Ref<str>, start: i64, mark: i64| {
                RuneNode(Node::Code {
                    text: text.to_string(),
                    language: maybe(&language),
                    // Zero and below mean "not numbered": a script has no null
                    // to hand us, and a line number is one-based everywhere a
                    // file is quoted.
                    start_line: usize::try_from(start).ok().filter(|n| *n > 0),
                    mark: usize::try_from(mark).ok().filter(|n| *n > 0),
                })
            },
        )
        .build()?;
    module
        .function("badge", |text: Ref<str>, tone: Ref<str>| {
            RuneNode(Node::Badge {
                text: text.to_string(),
                tone: Tone::named(&tone),
            })
        })
        .build()?;
    module
        .function("callout", |text: Ref<str>, tone: Ref<str>| {
            RuneNode(Node::Callout {
                text: text.to_string(),
                tone: Tone::named(&tone),
            })
        })
        .build()?;
    module
        .function("pair", |key: Ref<str>, value: Ref<str>| {
            RunePair(Pair {
                key: key.to_string(),
                value: value.to_string(),
                tone: Tone::Neutral,
            })
        })
        .build()?;
    module
        .function("toned", |pair: RunePair, tone: Ref<str>| {
            RunePair(Pair {
                tone: Tone::named(&tone),
                ..pair.0
            })
        })
        .build()?;
    module
        .function("fields", |rows: Vec<RunePair>| {
            RuneNode(Node::Fields {
                rows: rows.into_iter().map(|p| p.0).collect(),
            })
        })
        .build()?;
    module
        .function("meter", |label: Ref<str>, value: Ref<str>, percent: i64| {
            RuneNode(Node::Meter {
                label: label.to_string(),
                value: value.to_string(),
                percent: percent.clamp(0, 100) as u8,
            })
        })
        .build()?;
    module
        .function("divider", || RuneNode(Node::Divider))
        .build()?;
    module
        .function("link", |label: Ref<str>, url: Ref<str>, icon: Ref<str>| {
            RuneNode(Node::Link {
                label: label.to_string(),
                url: url.to_string(),
                icon: maybe(&icon),
            })
        })
        .build()?;
    module
        .function("column", |nodes: Vec<RuneNode>| {
            RuneNode(Node::Column(nodes.into_iter().map(|n| n.0).collect()))
        })
        .build()?;
    module
        .function("row", |nodes: Vec<RuneNode>| {
            RuneNode(Node::Row(nodes.into_iter().map(|n| n.0).collect()))
        })
        .build()?;
    module
        .function("section", |title: Ref<str>, nodes: Vec<RuneNode>| {
            RuneNode(Node::Section {
                title: title.to_string(),
                body: nodes.into_iter().map(|n| n.0).collect(),
                folded: false,
            })
        })
        .build()?;
    module
        .function(
            "item",
            |title: Ref<str>, detail: Ref<str>, badge: Ref<str>, icon: Ref<str>| {
                RuneItem(Item {
                    title: title.to_string(),
                    detail: maybe(&detail),
                    badge: maybe(&badge),
                    icon: maybe(&icon),
                })
            },
        )
        .build()?;
    module
        .function(
            "list",
            |id: Ref<str>, items: Vec<RuneItem>, selected: i64, action: Ref<str>| {
                RuneNode(Node::List {
                    id: id.to_string(),
                    items: items.into_iter().map(|i| i.0).collect(),
                    // Negative means none: a script has no null to hand us, and
                    // "no row is selected" has to be sayable.
                    selected: usize::try_from(selected).ok(),
                    // The payload of a row's selection is its index, put there
                    // by the panel: the script wrote neither.
                    on_select: maybe(&action).map(|action| Handler::new(action, String::new())),
                })
            },
        )
        .build()?;
    module
        .function(
            "field",
            |id: Ref<str>, value: Ref<str>, placeholder: Ref<str>, action: Ref<str>| {
                RuneNode(Node::Field {
                    id: id.to_string(),
                    value: value.to_string(),
                    placeholder: placeholder.to_string(),
                    // The text typed is the payload: the script wrote neither,
                    // and asking it to thread an index would be ceremony.
                    on_change: maybe(&action).map(|action| Handler::new(action, String::new())),
                })
            },
        )
        .build()?;
    module
        .function(
            "button",
            |label: Ref<str>, icon: Ref<str>, action: Ref<str>, payload: Ref<str>| {
                RuneNode(Node::Button {
                    label: label.to_string(),
                    icon: maybe(&icon),
                    on_click: maybe(&action)
                        .map(|action| Handler::new(action, payload.to_string())),
                    disabled: false,
                    primary: false,
                })
            },
        )
        .build()?;
    // Modifiers rather than eight-argument constructors: they compose, and a
    // script reads `primary(button(…))` the way the button reads on screen.
    // Each one raises a flag in place and hands the node back: a node the
    // modifier does not apply to comes through untouched, which is what keeps
    // `primary(text(…))` from being an error a script has to guard against.
    module
        .function("primary", |mut node: RuneNode| {
            if let Node::Button { primary, .. } = &mut node.0 {
                *primary = true;
            }
            node
        })
        .build()?;
    module
        .function("disabled", |mut node: RuneNode| {
            if let Node::Button { disabled, .. } = &mut node.0 {
                *disabled = true;
            }
            node
        })
        .build()?;
    module
        .function("folded", |mut node: RuneNode| {
            if let Node::Section { folded, .. } = &mut node.0 {
                *folded = true;
            }
            node
        })
        .build()?;
    module
        .function("fill", |node: RuneNode| {
            RuneNode(Node::Fill(Box::new(node.0)))
        })
        .build()?;
    module
        .function("empty", |message: Ref<str>| {
            RuneNode(Node::Empty {
                message: message.to_string(),
            })
        })
        .build()?;
    module
        .function("spinner", || RuneNode(Node::Spinner))
        .build()?;
    module
        .function("nothing", || RuneNode(Node::nothing()))
        .build()?;

    // — What the script may read ——————————————————————————————————————
    let it = shared.clone();
    module
        .function("worktree", move || {
            it.worktree
                .lock()
                .expect("worktree lock")
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        })
        .build()?;
    let it = shared.clone();
    module
        .function("setting", move |name: Ref<str>| {
            it.settings
                .lock()
                .expect("settings lock")
                .get(&*name)
                .cloned()
                .unwrap_or_default()
        })
        .build()?;

    // — What the script may do here ————————————————————————————————————
    let it = shared.clone();
    module
        .function("worktree_for", move |name: Ref<str>, prompt: Ref<str>| {
            it.effect(Effect::Worktree {
                name: name.to_string(),
                prompt: prompt.to_string(),
            })
        })
        .build()?;
    let it = shared.clone();
    module
        .function("state", move |key: Ref<str>| {
            it.state
                .lock()
                .expect("state lock")
                .get(&*key)
                .cloned()
                .unwrap_or_default()
        })
        .build()?;
    let it = shared.clone();
    module
        .function("set_state", move |key: Ref<str>, value: Ref<str>| {
            // **Written here as well as sent out**, and this is not belt and
            // braces: the effect is drained when the script *returns*, so
            // without the write in front, the very next line reading `state`
            // would get what was there before. A store one cannot read one's
            // own writes from is a trap every plugin would fall into — Sentry
            // did, and its "save" button looked like a dead button.
            //
            // The lasting copy is still the application's: this one is
            // overwritten by the next sweep with whatever the store holds, so a
            // write that could not land — no worktree open — fades rather than
            // lying about it.
            it.state
                .lock()
                .expect("state lock")
                .insert(key.to_string(), value.to_string());
            it.effect(Effect::Remember {
                key: key.to_string(),
                value: value.to_string(),
            })
        })
        .build()?;

    // **Joining strings, because Rune has no `join`.** Its `Vec` has `push`,
    // `sort` and `remove`, and nothing that turns a list of lines into a text —
    // which is what these scripts do all day, a prompt and a code excerpt both
    // being a list of lines. Written here rather than folded by hand in every
    // plugin, and taking its list **by reference** for the reason every other
    // argument does: Rune passes by taking, and a `Vec<String>` would empty the
    // state it came from.
    let id = shared.id.clone();
    module
        .function("join", move |list: Ref<rune::runtime::Vec>, separator: Ref<str>| {
            let mut out = String::new();
            let mut first = true;
            for item in list.iter() {
                let Ok(text) = item.borrow_string_ref() else {
                    // **Journalled and skipped, not refused.** Returning a
                    // `Result` here would thread a `?` through every helper
                    // that assembles a line — five deep in a plugin that builds
                    // a prompt — for a mistake that is the script's and that
                    // the journal names precisely. A list holding something
                    // that is not a line is a bug; it is said where a plugin's
                    // misbehaviour is already said.
                    log::warn!(target: "plugin", "{id}: join skipped a value that is not a string");
                    continue;
                };
                if !first {
                    out.push_str(&separator);
                }
                first = false;
                out.push_str(&text);
            }
            out
        })
        .build()?;

    // **A JSON reader, because Rune has none.** It is what the CI plugin dodged
    // by asking `gh --template` to format for it, and what an HTTP API leaves
    // no way around. Generic and not Sentry-shaped: every plugin that fetches
    // needs exactly this.
    module
        .function("json", |text: Ref<str>| {
            serde_json::from_str::<serde_json::Value>(&text)
                .map_err(|e| format!("unreadable JSON: {e}"))
                .and_then(|value| to_rune(&value).map_err(|e| format!("unreadable JSON: {e}")))
        })
        .build()?;

    let it = shared.clone();
    module
        .function("agent", move |text: Ref<str>| {
            it.effect(Effect::Agent(text.to_string()))
        })
        .build()?;
    let it = shared.clone();
    module
        .function("open", move |path: Ref<str>, line: i64| {
            // Read as a path **inside the worktree**, which is what every path
            // in this window is — see `plugin::in_worktree` for what that costs
            // and why a `..` is refused rather than resolved.
            match crate::plugin::in_worktree(&path) {
                Some(path) => it.effect(Effect::Open {
                    path,
                    line: usize::try_from(line).unwrap_or(0),
                }),
                None => log::warn!(
                    target: "plugin",
                    "{}: `{}` is not a path in the worktree", it.id, &*path
                ),
            }
        })
        .build()?;
    let it = shared.clone();
    module
        .function("notify", move |text: Ref<str>| {
            it.effect(Effect::Notify(text.to_string()))
        })
        .build()?;
    let it = shared.clone();
    module
        .function("open_url", move |url: Ref<str>| {
            it.effect(Effect::OpenUrl(url.to_string()))
        })
        .build()?;
    let it = shared.clone();
    module
        .function("copy", move |text: Ref<str>| {
            it.effect(Effect::Copy(text.to_string()))
        })
        .build()?;
    let id = shared.id.clone();
    module
        .function("log", move |text: Ref<str>| {
            log::info!(target: "plugin", "{id}: {}", &*text);
        })
        .build()?;

    // — Telling the time ——————————————————————————————————————————————
    //
    // **Two functions and no formatting.** Every API that reports anything
    // dates it, and "five hours ago" is what one reads it as — but *how* one
    // writes that is the script's business and the catalogue's, not the host's.
    // What the host owes is the only two things a script cannot do at all: read
    // the clock, and read an ISO-8601 stamp.
    module
        .function("now", || chrono::Utc::now().timestamp())
        .build()?;
    module
        .function("epoch", |stamp: Ref<str>| {
            // RFC 3339 is what every JSON API writes, Sentry included
            // (`2026-08-23T10:12:33.123456Z`). Zero for what does not parse:
            // a script has no null, and a date it cannot read is a date it
            // must be able to leave out rather than fail on.
            chrono::DateTime::parse_from_rfc3339(stamp.trim())
                .map(|when| when.timestamp())
                .unwrap_or(0)
        })
        .build()?;

    // — What the script may do out there ——————————————————————————————
    let it = shared.clone();
    module
        .function(
            "http",
            move |method: Ref<str>,
                  url: Ref<str>,
                  headers: Vec<(String, String)>,
                  body: Ref<str>,
                  secret: Ref<str>| {
                // Owned before the future exists: a `Ref` held across an
                // `await` would keep the state borrowed for the whole request.
                let cap = Cap::Http {
                    method: method.to_string(),
                    url: url.to_string(),
                    headers,
                    body: maybe(&body),
                    secret: maybe(&secret).and_then(|name| it.secret(&name)),
                };
                let it = it.clone();
                async move { it.call(Capability::Http, cap).await }
            },
        )
        .build()?;
    let it = shared.clone();
    module
        .function("shell", move |command: Ref<str>| {
            let command = command.to_string();
            let it = it.clone();
            async move {
                let worktree = it.worktree.lock().expect("worktree lock").clone();
                let Some(worktree) = worktree else {
                    return Err("no worktree is open".to_string());
                };
                it.call(Capability::Shell, Cap::Shell { worktree, command })
                    .await
            }
        })
        .build()?;

    Ok(module)
}

/// Turns what an API answered into something a script can walk.
///
/// Numbers all become the shape they were written in — Sentry writes an
/// occurrence count as a **string** in one endpoint and as a number in
/// another, and a reader that refused either would fail on half the answers.
/// Nothing here decides: it is a faithful copy, and it is the script that says
/// what a field means.
fn to_rune(value: &serde_json::Value) -> Result<Value, rune::alloc::Error> {
    /// `()` is what a script reads as "nothing here", and it is also the only
    /// value that cannot fail to build.
    fn nothing() -> Value {
        rune::to_value(()).expect("the unit value always builds")
    }
    fn or_nothing<T: rune::runtime::ToValue>(value: T) -> Value {
        rune::to_value(value).unwrap_or_else(|_| nothing())
    }
    Ok(match value {
        serde_json::Value::Null => nothing(),
        serde_json::Value::Bool(b) => or_nothing(*b),
        serde_json::Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => or_nothing(i),
            (None, Some(f)) => or_nothing(f),
            _ => nothing(),
        },
        serde_json::Value::String(text) => or_nothing(text.as_str()),
        serde_json::Value::Array(items) => {
            let mut out = rune::runtime::Vec::new();
            for item in items {
                out.push(to_rune(item)?)?;
            }
            or_nothing(out)
        }
        serde_json::Value::Object(fields) => {
            let mut out = rune::runtime::Object::new();
            for (key, field) in fields {
                out.insert(
                    rune::alloc::String::try_from(key.as_str())?,
                    to_rune(field)?,
                )?;
            }
            or_nothing(out)
        }
    })
}

/// The Rune context a plugin compiles against.
///
/// **One per plugin, kept across reloads.**
/// `Context::with_default_modules` assembles Rune's whole standard library and
/// `runtime()` derives the table a machine runs on; the claudhub module's
/// closures capture *that* plugin's `Shared`, which a reload keeps — so the
/// context is the plugin's, and it does not have to be built again when the
/// script is saved. It used to be, on every keystroke of a hot reload.
pub struct Compiler {
    context: Context,
    runtime: Arc<RuntimeContext>,
}

impl Compiler {
    pub fn new(shared: &Arc<Shared>) -> Result<Self, String> {
        let mut context =
            Context::with_default_modules().map_err(|e| format!("Rune's standard library: {e}"))?;
        context
            .install(module(shared).map_err(|e| format!("the claudhub module: {e}"))?)
            .map_err(|e| format!("the claudhub module: {e}"))?;
        let runtime = Arc::new(
            context
                .runtime()
                .map_err(|e| format!("Rune's runtime: {e}"))?,
        );
        Ok(Self { context, runtime })
    }
}

/// A loaded plugin's Rune side.
pub struct Host {
    shared: Arc<Shared>,
    runtime: Arc<RuntimeContext>,
    unit: Arc<Unit>,
}

impl Host {
    /// Compiles a script. The error is already the one to show: Rune's
    /// diagnostics, rendered, which name the line.
    pub fn load(
        manifest: &Manifest,
        shared: Arc<Shared>,
        compiler: &Compiler,
    ) -> Result<Self, String> {
        let source = std::fs::read_to_string(manifest.entry())
            .map_err(|e| format!("{}: {e}", manifest.entry().display()))?;
        Self::from_source(manifest.id.clone(), &source, shared, compiler)
    }

    /// The same, from a string. **This is what makes the host testable**: the
    /// whole loop plays out in memory, with no file and no process, on the
    /// model of the language client's `Session::run`.
    pub fn from_source(
        name: String,
        source: &str,
        shared: Arc<Shared>,
        compiler: &Compiler,
    ) -> Result<Self, String> {
        let mut sources = Sources::new();
        sources
            .insert(Source::new(name, source).map_err(|e| format!("{e}"))?)
            .map_err(|e| format!("{e}"))?;
        let mut diagnostics = Diagnostics::new();
        let built = rune::prepare(&mut sources)
            .with_context(&compiler.context)
            .with_diagnostics(&mut diagnostics)
            .build();
        let unit = match built {
            Ok(unit) => unit,
            Err(_) => return Err(render(&diagnostics, &sources)),
        };
        Ok(Self {
            shared,
            runtime: compiler.runtime.clone(),
            unit: Arc::new(unit),
        })
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// A fresh machine per call.
    ///
    /// `Vm::new` is a stack and two `Arc`s, so this costs nothing measurable —
    /// and it is what keeps the host free of a `&mut` that would have to be
    /// held across an `await`. Everything a plugin remembers is in the state it
    /// returns, which is the contract anyway.
    fn vm(&self) -> Vm {
        Vm::new(self.runtime.clone(), self.unit.clone())
    }

    /// `init(worktree)` — the state a plugin starts from.
    pub async fn init(&self, worktree: Option<&std::path::Path>) -> Result<Value, String> {
        let arg = worktree
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let value = self
            .vm()
            .async_call(["init"], (arg,))
            .await
            .map_err(|e| format!("init: {e}"))?;
        unwrap(value, "init")
    }

    /// `view(state)` — synchronous, and called whenever the state moves.
    /// `view(state, panel)`, for one of the plugin's panels.
    ///
    /// The panel's id and not its dock name: a script names its own panels, and
    /// `ClaudhubPlugin:sentry/issues` is Claudhub's bookkeeping, not the
    /// script's. Two panels of one plugin read the **same** state — which is
    /// what makes a master/detail arrangement work at all, the detail being a
    /// second reading of what the list chose.
    pub fn view(&self, state: &Value, panel: &str) -> Result<Node, String> {
        let value = self
            .vm()
            .call(["view"], (state.clone(), panel.to_string()))
            .map_err(|e| format!("view: {e}"))?;
        let node: RuneNode =
            rune::from_value(value).map_err(|e| format!("view must return a node: {e}"))?;
        Ok(node.0)
    }

    /// `update(state, action, payload)` — where a gesture's work happens.
    pub async fn update(
        &self,
        state: &Value,
        action: &str,
        payload: &str,
    ) -> Result<Value, String> {
        let value = self
            .vm()
            .async_call(
                ["update"],
                (state.clone(), action.to_string(), payload.to_string()),
            )
            .await
            .map_err(|e| format!("update({action}): {e}"))?;
        unwrap(value, "update")
    }
}

/// `init` and `update` answer a `Result`: a plugin that cannot reach its API
/// has to be able to say so rather than return a state that is a lie.
fn unwrap(value: Value, what: &str) -> Result<Value, String> {
    // `Result<Value, Value>` and not `Result<Value, String>`: a script's `?`
    // propagates whatever error the standard library gave it — a
    // `ParseIntError`, not a sentence — and reading only the `String` case
    // would take a failure for a state, which is the one mistake here that
    // would go unnoticed.
    match rune::from_value::<Result<Value, Value>>(value.clone()) {
        Ok(Ok(state)) => Ok(state),
        Ok(Err(reason)) => {
            Err(rune::from_value::<String>(reason.clone())
                .unwrap_or_else(|_| format!("{reason:?}")))
        }
        // Not a `Result` at all: a script that returns its state bare is doing
        // the common thing, and refusing it would be pedantry.
        Err(_) => {
            log::debug!(target: "plugin", "{what} answered outside a Result");
            Ok(value)
        }
    }
}

/// Rune's diagnostics as one string, which is what a panel can show.
fn render(diagnostics: &Diagnostics, sources: &Sources) -> String {
    let mut buffer = rune::termcolor::Buffer::no_color();
    if diagnostics.emit(&mut buffer, sources).is_err() {
        return "the script does not compile".into();
    }
    let text = String::from_utf8_lossy(buffer.as_slice())
        .trim()
        .to_string();
    if text.is_empty() {
        "the script does not compile".into()
    } else {
        text
    }
}

#[cfg(test)]
mod tests_support {
    //! The harness both test modules share: a host compiled from a string, and
    //! a thread standing in for the workers.
    //!
    //! The thread lives as long as the probe and answers through a policy that
    //! each call swaps in. One thread per call would mean closing the outbox to
    //! end it — and an outbox closed once is closed for the rest of the probe,
    //! so the second gesture would be told Claudhub had gone away.

    use super::*;
    use crate::plugin::manifest::Declaration;

    type Answer = Box<dyn Fn(&Cap) -> Result<String, String> + Send>;

    pub struct Probe {
        pub host: Host,
        pub shared: Arc<Shared>,
        pub outbox: async_channel::Receiver<Request>,
        answer: Arc<Mutex<Answer>>,
        responder: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            self.outbox.close();
            if let Some(responder) = self.responder.take() {
                let _ = responder.join();
            }
        }
    }

    fn declaration(capabilities: Vec<Capability>) -> Declaration {
        Declaration {
            title: "Probe".into(),
            panels: Vec::new(),
            screen: "review".into(),
            icon: None,
            capabilities,
            settings: BTreeMap::from([("base".to_string(), "https://api.test".to_string())]),
            secrets: Vec::new(),
            required: Vec::new(),
        }
    }

    pub fn probe(source: &str, capabilities: Vec<Capability>) -> Result<Probe, String> {
        probe_with(source, declaration(capabilities))
    }

    pub fn probe_with(source: &str, declaration: Declaration) -> Result<Probe, String> {
        let (tx, outbox) = async_channel::unbounded();
        let manifest = Manifest {
            id: "probe".into(),
            dir: PathBuf::from("/nowhere"),
            panels: crate::plugin::manifest::sole_panel("probe", &declaration),
            declaration,
        };
        let shared = Shared::new(&manifest, tx);
        shared.set_worktree(Some(PathBuf::from("/p/site")));
        let compiler = Compiler::new(&shared)?;
        let host = Host::from_source(manifest.id.clone(), source, shared.clone(), &compiler)?;

        let answer: Arc<Mutex<Answer>> =
            Arc::new(Mutex::new(Box::new(|_| Err("nothing was expected".into()))));
        let responder = {
            let (outbox, shared, answer) = (outbox.clone(), shared.clone(), answer.clone());
            std::thread::spawn(move || {
                while let Ok(request) = outbox.recv_blocking() {
                    let result = answer.lock().expect("answer lock")(&request.cap);
                    shared.resolve(request.call, result);
                }
            })
        };
        Ok(Probe {
            host,
            shared,
            outbox,
            answer,
            responder: Some(responder),
        })
    }

    /// Runs `f`, answering every capability with `answer`.
    ///
    /// The answers come from another thread because that is what the
    /// application is from the script's point of view: something else that
    /// eventually says something back.
    pub fn answer_with<T>(
        probe: &Probe,
        answer: impl Fn(&Cap) -> Result<String, String> + Send + 'static,
        f: impl std::future::Future<Output = T>,
    ) -> T {
        *probe.answer.lock().expect("answer lock") = Box::new(answer);
        crate::runtime::executor::block_on(f)
    }
}

#[cfg(test)]
mod tests {
    //! **The test that counts launches no process and opens no window.**
    //!
    //! The whole loop plays out in memory: a script compiled from a string, a
    //! capability answered by hand, and the tree that comes out compared to the
    //! one expected. It is the language client's `Session::run` device, and for
    //! the same reason — what is worth locking is the mechanics, never what a
    //! remote service happens to answer today.

    use super::tests_support::*;
    use super::*;

    const CI: &str = r#"
        use claudhub::*;

        pub async fn init(worktree) {
            let out = shell("gh run list").await?;
            let runs = out.split('\n').filter(|l| l != "").collect::<Vec>();
            Ok(#{ runs: runs, chosen: -1 })
        }

        pub fn view(state, panel) {
            let rows = [];
            for run in state.runs {
                rows.push(item(run, "", "failure", "circle-x"));
            }
            column([
                title("Runs"),
                list("runs", rows, state.chosen, "choose"),
            ])
        }

        pub async fn update(state, action, payload) {
            if action == "choose" {
                state.chosen = payload.parse::<i64>()?;
                agent(`look at ${state.runs[state.chosen]}`);
            }
            Ok(state)
        }
    "#;

    #[test]
    fn a_script_asks_for_a_capability_and_paints_what_comes_back() {
        let probe = probe(CI, vec![Capability::Shell]).expect("the script compiles");
        let state = answer_with(
            &probe,
            |cap| {
                assert!(matches!(cap, Cap::Shell { .. }), "{cap:?}");
                Ok("release\nnightly\n".into())
            },
            probe.host.init(Some(std::path::Path::new("/p/site"))),
        )
        .expect("init succeeds");

        let tree = probe.host.view(&state, "main").expect("view renders");
        let Node::Column(children) = &tree else {
            panic!("expected a column, got {tree:?}");
        };
        let Some(Node::List {
            items,
            selected,
            on_select,
            ..
        }) = children.get(1)
        else {
            panic!("expected a list, got {children:?}");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "release");
        assert_eq!(items[1].badge.as_deref(), Some("failure"));
        // A negative index means no row is chosen: a script has no null to
        // hand us, and "nothing is selected" has to be sayable.
        assert_eq!(*selected, None);
        assert_eq!(
            on_select.as_ref().map(|h| h.action.as_str()),
            Some("choose")
        );
    }

    /// A gesture goes through `update`, and what the script asks of the window
    /// comes back as effects, in the order it asked for them.
    #[test]
    fn a_gesture_moves_the_state_and_leaves_its_effects_behind() {
        let probe = probe(CI, vec![Capability::Shell]).expect("the script compiles");
        let state = answer_with(
            &probe,
            |_| Ok("release\nnightly\n".into()),
            probe.host.init(None),
        )
        .expect("init succeeds");

        let state = answer_with(
            &probe,
            |_| Err("nothing else should be asked".into()),
            probe.host.update(&state, "choose", "1"),
        )
        .expect("update succeeds");

        let tree = probe.host.view(&state, "main").expect("view renders");
        let Node::Column(children) = &tree else {
            panic!("expected a column");
        };
        let Some(Node::List { selected, .. }) = children.get(1) else {
            panic!("expected a list");
        };
        assert_eq!(*selected, Some(1));
        assert_eq!(
            probe.shared.take_effects(),
            vec![Effect::Agent("look at nightly".into())]
        );
        // Drained, not copied: the window acts on each of them exactly once.
        assert!(probe.shared.take_effects().is_empty());
    }

    /// Declared and not inferred. A plugin reaching for something its manifest
    /// does not list is doing what its author did not write down.
    #[test]
    fn an_undeclared_capability_is_refused_before_it_leaves() {
        let probe = probe(CI, vec![Capability::Http]).expect("the script compiles");
        let message = answer_with(
            &probe,
            |_| panic!("nothing must reach the workers"),
            probe.host.init(None),
        )
        .expect_err("shell is not declared");
        assert!(message.contains("shell"), "{message}");
    }

    /// A script that does not compile gives back what a panel can show, and it
    /// names the line — the whole point of keeping Rune's own diagnostics.
    #[test]
    fn a_script_that_does_not_compile_says_where() {
        let message = match probe("pub fn view(state, panel) { column( }", vec![]) {
            Err(message) => message,
            Ok(_) => panic!("that is not Rune"),
        };
        assert!(message.contains("probe"), "{message}");
        assert!(message.contains('1'), "{message}");
    }

    /// **A request never stays pending.** A reload, a plugin switched off, a
    /// server lost: the future has to come back, or the panel spins for good.
    #[test]
    fn a_swept_away_request_comes_back_as_an_error() {
        let probe = probe(CI, vec![Capability::Shell]).expect("the script compiles");
        let shared = probe.shared.clone();
        let outcome = answer_with(
            &probe,
            move |_| {
                // Swept away while the worker was about to answer, which is
                // what a reload does. The answer that follows finds nothing to
                // resolve and is dropped — arriving too late is not an error.
                shared.fail_all("the plugin was reloaded");
                Ok("too late".into())
            },
            probe.host.init(None),
        );
        let message = outcome.expect_err("the request was swept away");
        assert!(message.contains("reloaded"), "{message}");
    }

    /// A secret's three forms, read off the value alone.
    ///
    /// The parsing is what a test can hold: reading a keyring needs a desktop
    /// session, which no test has and which CI certainly does not.
    #[test]
    fn a_secret_says_where_it_lives() {
        use super::KeyringEntry;
        // A plain value is a plain value: nothing is parsed out of it.
        assert_eq!(KeyringEntry::parse("sntrys_hunter2"), None);
        assert_eq!(KeyringEntry::parse("$SENTRY_TOKEN"), None);
        // `keyring:` with nothing behind it names nothing, and must not become
        // an entry with an empty account.
        assert_eq!(KeyringEntry::parse("keyring:"), None);
        assert_eq!(KeyringEntry::parse("keyring:   "), None);

        let default = KeyringEntry::parse("keyring:sentry").expect("an account");
        assert_eq!(default.describe(), "keyring claudhub/sentry");
        let named = KeyringEntry::parse("keyring:com.acme.tools/sentry").expect("both");
        assert_eq!(named.describe(), "keyring com.acme.tools/sentry");
        // A lone slash is a typo, not a service: what is there is the account.
        assert_eq!(
            KeyringEntry::parse("keyring:/sentry")
                .expect("an account")
                .describe(),
            "keyring claudhub/sentry"
        );
    }

    /// What decides whether Claudhub puts a secret away or records it as it
    /// stands — the crux of "the keyring is the default place".
    #[test]
    fn a_value_says_whether_it_is_a_secret_or_an_address() {
        use super::SecretPlace;
        assert_eq!(SecretPlace::of("sntrys_hunter2"), SecretPlace::Plain);
        assert_eq!(SecretPlace::of("  "), SecretPlace::Plain);
        // A lone dollar is a token that begins with one, not an address.
        assert_eq!(SecretPlace::of("$"), SecretPlace::Plain);
        assert_eq!(SecretPlace::of("$SENTRY_TOKEN"), SecretPlace::Environment);
        assert_eq!(
            SecretPlace::of("keyring:sentry.token"),
            SecretPlace::Keyring
        );
        // `keyring:` naming nothing is not an address either: it would become an
        // entry with an empty account.
        assert_eq!(SecretPlace::of("keyring:"), SecretPlace::Plain);

        assert!(!SecretPlace::of("sntrys_hunter2").is_reference());
        assert!(SecretPlace::of("$SENTRY_TOKEN").is_reference());
        assert!(SecretPlace::of("keyring:sentry.token").is_reference());
    }

    /// What the settings file records once Claudhub has put a token away.
    #[test]
    fn an_entry_of_ours_is_written_without_naming_our_service() {
        use super::KeyringEntry;
        let ours = KeyringEntry::for_plugin("sentry", "token");
        assert_eq!(ours.reference(), "keyring:sentry.token");
        // And what it writes reads back as what it is.
        assert_eq!(KeyringEntry::parse(&ours.reference()), Some(ours));
        // Someone else's service is spelled out, since leaving it out would
        // point at ours.
        let theirs = KeyringEntry::parse("keyring:com.acme/api").expect("both");
        assert_eq!(theirs.reference(), "keyring:com.acme/api");
    }

    /// The round trip against the **real** keyring, skipped where there is none.
    ///
    /// The precedent is `tests/lsp_phpantom.rs`: a test that fails for want of a
    /// service nobody promised is a red build that says nothing. A desktop
    /// session has a Secret Service; CI does not.
    #[test]
    fn a_secret_goes_into_the_keyring_and_comes_back() {
        use super::KeyringEntry;
        let entry = KeyringEntry::for_plugin("claudhub-test", "probe");
        if entry.store("s3cret").is_err() {
            eprintln!("no keyring on this machine — skipped");
            return;
        }
        assert_eq!(entry.read().expect("what was just stored"), "s3cret");
        entry.forget().expect("removing it");
        assert!(entry.read().is_err(), "it must be gone");
        // Removing what is not there is the state one asked for, not a failure.
        entry.forget().expect("removing it twice");
    }

    /// The settings a manifest declares reach the script, and the ones the user
    /// wrote lie over them without recompiling anything.
    #[test]
    fn a_setting_is_read_at_the_moment_it_is_asked_for() {
        const SOURCE: &str = r#"
            use claudhub::*;
            pub async fn init(worktree) { Ok(setting("base")) }
            pub fn view(state, panel) { text(state) }
        "#;
        let probe = probe(SOURCE, vec![]).expect("the script compiles");
        let state = crate::runtime::executor::block_on(probe.host.init(None)).expect("init");
        assert_eq!(
            probe.host.view(&state, "main").expect("view"),
            Node::Text {
                text: "https://api.test".into(),
                style: TextStyle::Body
            }
        );

        probe.shared.configure(
            BTreeMap::from([("base".to_string(), "https://other.test".to_string())]),
            BTreeMap::new(),
        );
        let state = crate::runtime::executor::block_on(probe.host.init(None)).expect("init");
        assert_eq!(
            probe.host.view(&state, "main").expect("view"),
            Node::Text {
                text: "https://other.test".into(),
                style: TextStyle::Body
            }
        );
    }
}

#[cfg(test)]
mod shipped {
    //! The plugin Claudhub ships has to compile and to render.
    //!
    //! It is the themes' test, one floor over: a bundled file that does not
    //! read produces **no error at all** — the panel simply stays empty, and
    //! nothing says why. Here the whole loop is played with a canned `gh`
    //! answer, so what is checked is the script and not GitHub.

    use super::tests_support::*;
    use crate::plugin::manifest::Capability;
    use crate::plugin::view::Node;

    const MANIFEST: &str = include_str!("../../plugins/ci/plugin.toml");
    const SOURCE: &str = include_str!("../../plugins/ci/main.rn");

    /// What `gh run list --template …` writes: five tab-separated fields.
    const RUNS: &str = "42\tRefaire le fil\tRelease\tcompleted\tfailure\n\
                        41\tPeindre le fond\tRelease\tin_progress\t\n";

    #[test]
    fn the_shipped_plugin_compiles_and_renders() {
        let declaration: crate::plugin::manifest::Declaration =
            toml::from_str(MANIFEST).expect("the shipped manifest reads");
        assert_eq!(declaration.capabilities, vec![Capability::Shell]);
        assert!(declaration.settings.contains_key("list"));

        let probe = probe_with(SOURCE, declaration).expect("the shipped script compiles");
        let state = answer_with(
            &probe,
            |_| Ok(RUNS.into()),
            probe.host.init(Some(std::path::Path::new("/p/site"))),
        )
        .expect("init succeeds");
        let tree = probe.host.view(&state, "main").expect("view renders");

        let Node::Column(children) = &tree else {
            panic!("expected a column, got {tree:?}");
        };
        let Some(Node::List { items, .. }) = children.get(1) else {
            panic!("expected a list, got {children:?}");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Refaire le fil");
        assert_eq!(items[0].badge.as_deref(), Some("failure"));
        assert_eq!(items[0].icon.as_deref(), Some("circle-x"));
        // An unfinished run has no conclusion: the badge falls back to its
        // status, which is the only thing there is to say about it.
        assert_eq!(items[1].badge.as_deref(), Some("in_progress"));
        assert_eq!(items[1].icon.as_deref(), Some("loader-circle"));
        // Nothing is chosen yet, so the detail section is not there: an empty
        // one would push the list out of view to say nothing.
        assert_eq!(children.len(), 2);
    }

    /// Choosing a row opens its detail, and handing it over leaves the paste
    /// behind as an effect.
    #[test]
    fn choosing_a_run_and_handing_it_to_the_agent() {
        let declaration = toml::from_str(MANIFEST).expect("the shipped manifest reads");
        let probe = probe_with(SOURCE, declaration).expect("the shipped script compiles");
        let state = answer_with(
            &probe,
            |_| Ok(RUNS.into()),
            probe.host.init(Some(std::path::Path::new("/p/site"))),
        )
        .expect("init succeeds");

        let state = answer_with(
            &probe,
            |_| panic!("choosing a row asks nothing of anyone"),
            probe.host.update(&state, "choose", "0"),
        )
        .expect("choose succeeds");
        let tree = probe.host.view(&state, "main").expect("view renders");
        let Node::Column(children) = &tree else {
            panic!("expected a column");
        };
        assert_eq!(children.len(), 3, "the detail section appears");

        let state = answer_with(
            &probe,
            |_| Ok("une ligne\nune autre\nassertion failed".into()),
            probe.host.update(&state, "hand", "42"),
        )
        .expect("hand succeeds");
        let effects = probe.shared.take_effects();
        // The paste and nothing else: the announcement it used to carry was a
        // claim the script cannot make — the prompt goes through a review
        // dialog, and only the window knows whether it was confirmed.
        assert_eq!(effects.len(), 1, "{effects:?}");
        let super::Effect::Agent(text) = &effects[0] else {
            panic!("expected a paste, got {effects:?}");
        };
        assert!(text.contains("Refaire le fil"), "{text}");
        assert!(text.contains("assertion failed"), "{text}");
        // The state came back whole: the run list is still there.
        probe.host.view(&state, "main").expect("view still renders");
    }
}

#[cfg(test)]
mod sentry_plugin {
    //! Sentry, as a plugin — the acceptance gate the plugin system set itself.
    //!
    //! The fixtures are the ones the Rust version was locked by, and they are
    //! what makes this a test of the **API** and not of Sentry: a count written
    //! as a string in one endpoint and as a number in another, two shapes of
    //! stack trace depending on the SDK, an issue with half its fields missing.
    //! If a script cannot read those without a capability cut for it, the
    //! vocabulary is wrong.

    use super::tests_support::*;
    use crate::plugin::manifest::Capability;
    use crate::plugin::view::Node;

    const MANIFEST: &str = include_str!("../../plugins/sentry/plugin.toml");
    const SOURCE: &str = include_str!("../../plugins/sentry/main.rn");

    const ISSUES: &str = r#"[
      {
        "id": "4508",
        "shortId": "ACME-7X",
        "title": "TypeError: Cannot read properties of undefined",
        "culprit": "app/Http/Controllers/QuoteController.php in store",
        "level": "error",
        "count": "137",
        "lastSeen": "2026-08-19T10:12:00Z",
        "permalink": "https://sentry.io/organizations/acme/issues/4508/"
      },
      {
        "id": "4509",
        "title": "ValueError",
        "count": 3,
        "lastSeen": "2026-08-18T22:00:00Z"
      }
    ]"#;

    const EVENT: &str = r#"{
      "message": "Cannot read properties of undefined (reading 'total')",
      "entries": [
        {
          "type": "exception",
          "data": {
            "values": [
              {
                "stacktrace": {
                  "frames": [
                    {
                      "filename": "/vendor/laravel/framework/src/Foundation/Http/Kernel.php",
                      "function": "handle",
                      "lineNo": 141,
                      "inApp": false
                    },
                    {
                      "filename": "app/Http/Controllers/QuoteController.php",
                      "function": "store",
                      "lineNo": 88,
                      "inApp": true,
                      "context": [
                        [86, "    public function store(Request $request)"],
                        [87, "    {"],
                        [88, "        return $request->quote->total;"],
                        [89, "    }"]
                      ]
                    }
                  ]
                }
              }
            ]
          }
        }
      ]
    }"#;

    fn probe_sentry() -> Probe {
        let mut declaration: crate::plugin::manifest::Declaration =
            toml::from_str(MANIFEST).expect("the shipped manifest reads");
        assert_eq!(declaration.capabilities, vec![Capability::Http]);
        assert_eq!(declaration.secrets, vec!["token".to_string()]);
        declaration.settings.insert("org".into(), "acme".into());
        let probe = probe_with(SOURCE, declaration).expect("the shipped script compiles");
        // The project belongs to the repository, which is what `set_state`
        // writes and what the application hands back.
        probe.shared.remember(std::collections::BTreeMap::from([(
            "project".to_string(),
            "site".to_string(),
        )]));
        probe
    }

    /// The first list of a tree, wherever it sits.
    ///
    /// The panel now leads with a picker, a filter and a count, so a hard index
    /// would have to be moved every time the head of the column gains a line —
    /// and what these tests are about is what the list holds.
    fn first_list(nodes: &[Node]) -> &[crate::plugin::view::Item] {
        fn list_in(node: &Node) -> Option<&[crate::plugin::view::Item]> {
            match node {
                Node::List { items, .. } => Some(items.as_slice()),
                // The panel's own list is wrapped in a `Fill`: it is what the
                // panel is for, so it takes the height that is left.
                Node::Fill(inner) => list_in(inner),
                _ => None,
            }
        }
        nodes
            .iter()
            .find_map(list_in)
            .unwrap_or_else(|| panic!("no list in {nodes:?}"))
    }

    /// Every node of a tree, sections walked into.
    ///
    /// The detail panel puts its trace, its context and its breadcrumbs in
    /// folding sections, which is a reading posture and not a shape a test
    /// should have to know.
    fn flatten(nodes: &[Node], out: &mut Vec<Node>) {
        for node in nodes {
            match node {
                Node::Section { body, .. } => flatten(body, out),
                Node::Fill(inner) => flatten(std::slice::from_ref(inner), out),
                Node::Column(children) | Node::Row(children) => {
                    out.push(node.clone());
                    flatten(children, out);
                }
                other => out.push(other.clone()),
            }
        }
    }

    fn everything(tree: &Node) -> Vec<Node> {
        let Node::Column(children) = tree else {
            panic!("expected a column, got {tree:?}");
        };
        let mut out = Vec::new();
        flatten(children, &mut out);
        out
    }

    #[test]
    fn the_issue_list_reads_whatever_shape_the_count_came_in() {
        let probe = probe_sentry();
        let state = answer_with(&probe, |_| Ok(ISSUES.into()), probe.host.init(None))
            .expect("init succeeds");
        let tree = probe.host.view(&state, "issues").expect("view renders");
        let Node::Column(children) = &tree else {
            panic!("expected a column, got {tree:?}");
        };
        let items = first_list(children);
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].title,
            "TypeError: Cannot read properties of undefined"
        );
        // A string in the list, a number elsewhere: both read.
        assert_eq!(items[0].badge.as_deref(), Some("137"));
        assert_eq!(items[1].badge.as_deref(), Some("3"));
        // The second line carries the culprit and how long ago it was seen,
        // joined only where there are two things to join.
        let detail = items[0].detail.as_deref().expect("a culprit and a date");
        assert!(detail.starts_with("app/Http/Controllers"), "{detail}");
        assert!(detail.contains(" · il y a "), "{detail}");
        // An issue with no culprit still reads: the date stands alone, with no
        // separator hanging off the front of it.
        let bare = items[1].detail.as_deref().expect("a date at least");
        assert!(bare.starts_with("il y a "), "{bare}");
    }

    #[test]
    fn a_trace_keeps_its_order_and_quotes_only_the_application() {
        let probe = probe_sentry();
        let state = answer_with(&probe, |_| Ok(ISSUES.into()), probe.host.init(None))
            .expect("init succeeds");
        let state = answer_with(
            &probe,
            // The endpoint answers a **list** of events, newest first: the
            // `events/latest/` that returned one has gone from the API.
            |_| Ok(format!("[{EVENT}]")),
            probe.host.update(&state, "choose", "0"),
        )
        .expect("choosing an issue fetches its event");

        // The **centre** panel: the trace is what wants the width, and the list
        // one chose from stays where it was on the left.
        let tree = probe.host.view(&state, "issue").expect("view renders");
        let Node::Column(body) = &tree else {
            panic!("expected a column");
        };
        let _ = body;
        let all = everything(&tree);

        // The whole stack is listed — it is the path that led there — and the
        // most recent frame leads: Sentry's order is the call's, and what one
        // comes for is the line that threw.
        let paths: Vec<&str> = all
            .iter()
            .filter_map(|node| match node {
                Node::Button {
                    label, on_click, ..
                } if on_click.as_ref().is_some_and(|h| h.action == "open") => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            paths,
            vec![
                "app/Http/Controllers/QuoteController.php:88",
                "/vendor/laravel/framework/src/Foundation/Http/Kernel.php:141",
            ]
        );

        // Only the application's frames carry their code, and it is quoted
        // **bare**: the numbers are the panel's column, so the excerpt stays
        // copyable into a file, which is what one does with an excerpt.
        let excerpts: Vec<_> = all
            .iter()
            .filter_map(|node| match node {
                Node::Code {
                    text,
                    language,
                    start_line,
                    mark,
                } => Some((text.clone(), language.clone(), *start_line, *mark)),
                _ => None,
            })
            .collect();
        assert_eq!(excerpts.len(), 1, "only the in-app frame is quoted");
        let (text, language, start, mark) = &excerpts[0];
        assert!(text.starts_with("    public function store"), "{text}");
        assert!(!text.contains("86"), "no numbers in the text: {text}");
        assert_eq!(language.as_deref(), Some("php"));
        assert_eq!(*start, Some(86));
        assert_eq!(*mark, Some(88), "the line the trace named is picked out");

        // The header says what it is and how bad, and the permalink is a link
        // and not a button with no action — which is what it was, and what
        // made it a dead button.
        assert!(
            all.iter()
                .any(|node| matches!(node, Node::Badge { text, .. } if text == "error")),
            "{all:?}"
        );
        assert!(
            all.iter()
                .any(|node| matches!(node, Node::Link { url, .. } if url.contains("sentry.io"))),
            "{all:?}"
        );
    }

    /// One issue, with the short id a team reads it by.
    const SHORT_ID: &str = r#"[
      {
        "id": "4508",
        "shortId": "ACETICS-4Q",
        "title": "TypeError",
        "level": "error",
        "count": "137"
      }
    ]"#;

    /// What `issues/{id}/tags/` answers: the top values of each tag, with the
    /// total they are a share of.
    const TAGS: &str = r#"[
      {
        "key": "environment",
        "name": "Environment",
        "totalValues": 200,
        "topValues": [{ "name": "production", "value": "production", "count": 200 }]
      },
      {
        "key": "release",
        "name": "Release",
        "totalValues": 200,
        "topValues": [
          { "name": "5.9.4-2", "value": "5.9.4-2", "count": 44 },
          { "name": "6.1.0-2", "value": "6.1.0-2", "count": 156 }
        ]
      }
    ]"#;

    /// An event with everything a Sentry page shows around a trace.
    const RICH_EVENT: &str = r#"{
      "message": "Cannot read properties of undefined",
      "tags": [
        { "key": "handled", "value": "yes" },
        { "key": "level", "value": "error" },
        { "key": "runtime", "value": "php 8.3.31" }
      ],
      "entries": [
        {
          "type": "breadcrumbs",
          "data": {
            "values": [
              { "category": "query", "message": "select * from quotes", "level": "info" },
              { "category": "http", "message": "GET /quotes/12", "level": "warning" }
            ]
          }
        },
        {
          "type": "exception",
          "data": {
            "values": [
              {
                "stacktrace": {
                  "frames": [
                    {
                      "filename": "app/Http/Controllers/QuoteController.php",
                      "function": "store",
                      "lineNo": 88,
                      "inApp": true,
                      "context": [[87, "    {"], [88, "        return $q->total;"]]
                    }
                  ]
                }
              }
            ]
          }
        }
      ]
    }"#;

    /// **What a trace alone does not say.**
    ///
    /// An error is not diagnosable from its stack: what makes it so is what is
    /// around it — on which release, in which environment, and what happened in
    /// the second before. Sentry's own page leads with those, and a panel that
    /// dropped them would be read once here and once in the browser.
    #[test]
    fn an_issue_carries_what_surrounds_it() {
        let probe = probe_sentry();
        let state =
            answer_with(&probe, |_| Ok(ISSUES.into()), probe.host.init(None)).expect("init");
        let state = answer_with(
            &probe,
            // Two round trips for one gesture, told apart by the address:
            // the event carries this occurrence, the tags carry all of them.
            |cap| match cap {
                super::Cap::Http { url, .. } if url.ends_with("/tags/") => Ok(TAGS.into()),
                _ => Ok(format!("[{RICH_EVENT}]")),
            },
            probe.host.update(&state, "choose", "0"),
        )
        .expect("choose");

        let all = everything(&probe.host.view(&state, "issue").expect("view"));

        // The event's tags, in a table: eight values that line up are read at a
        // glance, and eight sentences are not.
        let pairs: Vec<_> = all
            .iter()
            .filter_map(|node| match node {
                Node::Fields { rows } => Some(rows.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert!(
            pairs
                .iter()
                .any(|p| p.key == "runtime" && p.value == "php 8.3.31"),
            "{pairs:?}"
        );
        // `handled: yes` is not a neutral fact, and the tone says so.
        let handled = pairs.iter().find(|p| p.key == "handled").expect("handled");
        assert_eq!(handled.tone, crate::plugin::view::Tone::Success);
        // The counters of the header are in a table of their own.
        assert!(pairs.iter().any(|p| p.key == "événements"), "{pairs:?}");

        // The distribution, in bars: a share is a comparison, and two numbers
        // side by side are two numbers.
        let meters: Vec<_> = all
            .iter()
            .filter_map(|node| match node {
                Node::Meter { label, percent, .. } => Some((label.clone(), *percent)),
                _ => None,
            })
            .collect();
        assert!(
            meters.contains(&("production".to_string(), 100)),
            "{meters:?}"
        );
        assert!(meters.contains(&("6.1.0-2".to_string(), 78)), "{meters:?}");

        // And the breadcrumbs, in the order they happened.
        let crumbs = all
            .iter()
            .find_map(|node| match node {
                Node::List { id, items, .. } if id == "crumbs" => Some(items.clone()),
                _ => None,
            })
            .expect("a breadcrumb list");
        assert_eq!(crumbs.len(), 2);
        assert_eq!(crumbs[0].title, "select * from quotes");
        assert_eq!(crumbs[1].detail.as_deref(), Some("http"));
    }

    /// **The three things a panel does with its own shape.**
    ///
    /// The list is what the issues panel is for, so it says so and takes the
    /// height that is left; the distribution is glanced at once, so it starts
    /// folded and the trace keeps the screen; and the short id is a reference
    /// one carries elsewhere, so it is a button and not a line of text.
    #[test]
    fn the_panel_says_what_fills_what_folds_and_what_is_carried_away() {
        let probe = probe_sentry();
        let state =
            answer_with(&probe, |_| Ok(SHORT_ID.into()), probe.host.init(None)).expect("init");

        // The list fills: without it, it was given sixteen rows in advance and
        // the panel ended in a band of nothing under them.
        let Node::Column(children) = &probe.host.view(&state, "issues").expect("view") else {
            panic!("expected a column");
        };
        assert!(
            children.iter().any(
                |node| matches!(node, Node::Fill(inner) if matches!(**inner, Node::List { .. }))
            ),
            "{children:?}"
        );

        let state = answer_with(
            &probe,
            |cap| match cap {
                super::Cap::Http { url, .. } if url.ends_with("/tags/") => Ok(TAGS.into()),
                _ => Ok(format!("[{RICH_EVENT}]")),
            },
            probe.host.update(&state, "choose", "0"),
        )
        .expect("choose");
        let tree = probe.host.view(&state, "issue").expect("view");
        let Node::Column(body) = &tree else {
            panic!("expected a column");
        };
        let sections: Vec<_> = body
            .iter()
            .filter_map(|node| match node {
                Node::Section { title, folded, .. } => Some((title.as_str(), *folded)),
                _ => None,
            })
            .collect();
        assert!(sections.contains(&("Répartition", true)), "{sections:?}");
        assert!(
            sections
                .iter()
                .any(|(title, folded)| title.starts_with("Trace") && !folded),
            "the trace is what one came to read: {sections:?}"
        );

        // The short id is a gesture, and the gesture puts it in the clipboard.
        let all = everything(&tree);
        let handler = all
            .iter()
            .find_map(|node| match node {
                Node::Button {
                    label, on_click, ..
                } if label == "ACETICS-4Q" => on_click.clone(),
                _ => None,
            })
            .expect("the short id is a button");
        assert_eq!(handler.payload, "ACETICS-4Q");
        let _ = probe.shared.take_effects();
        answer_with(
            &probe,
            |_| panic!("copying asks nothing of anyone"),
            probe.host.update(&state, &handler.action, &handler.payload),
        )
        .expect("copy");
        let effects = probe.shared.take_effects();
        assert!(
            effects.contains(&super::Effect::Copy("ACETICS-4Q".into())),
            "{effects:?}"
        );
    }

    /// The list filters on what it holds, and a choice survives the filter.
    ///
    /// The row one clicks is an index into what is **shown**; what the state
    /// keeps is the rank in the whole list. Confusing the two means clearing
    /// the filter opens an error one was not reading.
    #[test]
    fn filtering_the_list_does_not_move_the_choice() {
        let probe = probe_sentry();
        let state =
            answer_with(&probe, |_| Ok(ISSUES.into()), probe.host.init(None)).expect("init");
        let state = answer_with(
            &probe,
            |_| panic!("filtering asks nothing of anyone"),
            probe.host.update(&state, "filter", "valueerror"),
        )
        .expect("filter");

        let tree = probe.host.view(&state, "issues").expect("view");
        let Node::Column(children) = &tree else {
            panic!("expected a column");
        };
        let items = first_list(children);
        assert_eq!(items.len(), 1, "the filter reaches the title");
        assert_eq!(items[0].title, "ValueError");

        // Row 0 of the filtered list is issue 1 of the whole one.
        let state = answer_with(
            &probe,
            |_| Ok("[]".into()),
            probe.host.update(&state, "choose", "0"),
        )
        .expect("choose");
        let state = answer_with(
            &probe,
            |_| panic!("clearing the filter asks nothing of anyone"),
            probe.host.update(&state, "filter", ""),
        )
        .expect("filter");
        let all = everything(&probe.host.view(&state, "issue").expect("view"));
        assert!(
            all.iter()
                .any(|node| matches!(node, Node::Text { text, .. } if text == "ValueError")),
            "the filter moved the choice: {all:?}"
        );
    }

    #[test]
    fn handing_it_over_pastes_the_trace_and_the_code() {
        let probe = probe_sentry();
        let state =
            answer_with(&probe, |_| Ok(ISSUES.into()), probe.host.init(None)).expect("init");
        let state = answer_with(
            &probe,
            // The endpoint answers a **list** of events, newest first: the
            // `events/latest/` that returned one has gone from the API.
            |_| Ok(format!("[{EVENT}]")),
            probe.host.update(&state, "choose", "0"),
        )
        .expect("choose");
        let _ = probe.shared.take_effects();

        answer_with(
            &probe,
            |_| panic!("handing over asks nothing of anyone"),
            probe.host.update(&state, "hand", ""),
        )
        .expect("hand");
        let effects = probe.shared.take_effects();
        let Some(super::Effect::Agent(text)) = effects.first() else {
            panic!("expected a paste, got {effects:?}");
        };
        // **The ticket goes first, and it is not a courtesy**: what is pasted
        // is one event of one moment, and everything asked next — is it still
        // happening, since which release, on how many users — is a question for
        // Sentry. An agent holding its MCP server asks it, and the only thing
        // it was missing was the reference.
        assert!(text.contains("ACME-7X"), "the short id: {text}");
        assert!(
            text.contains("acme"),
            "the organisation it belongs to: {text}"
        );
        assert!(
            text.contains("https://sentry.io/organizations/acme/issues/4508/"),
            "and the link, for an agent with no server: {text}"
        );
        assert!(text.contains("QuoteController.php:88"), "{text}");
        assert!(text.contains("Kernel.php:141"), "the whole stack: {text}");
        assert!(text.contains("$request->quote->total"), "{text}");
        // The framework frame is listed but not quoted: a hundred lines where
        // the bug is not.
        assert!(!text.contains("public function handle"), "{text}");
    }

    /// **A frame opens the file the worktree holds, not one at the root of the
    /// machine.**
    ///
    /// Sentry names a frame after the deployed application's root, and hands
    /// over what it could not relativise with its leading separator still on —
    /// `/vendor/laravel/…`. Joined as it stands, an absolute path *replaces*
    /// the worktree, and the panel answered `No such file or directory` about a
    /// path nobody had written.
    #[test]
    fn a_frame_opens_a_file_inside_the_worktree() {
        let probe = probe_sentry();
        let state =
            answer_with(&probe, |_| Ok(ISSUES.into()), probe.host.init(None)).expect("init");
        let state = answer_with(
            &probe,
            |_| Ok(format!("[{EVENT}]")),
            probe.host.update(&state, "choose", "0"),
        )
        .expect("choose");
        probe.shared.take_effects();

        let _ = answer_with(
            &probe,
            |_| panic!("opening a file asks nothing of anyone"),
            probe.host.update(
                &state,
                "open",
                "/vendor/laravel/framework/src/Foundation/Http/Kernel.php:141",
            ),
        )
        .expect("open");
        assert_eq!(
            probe.shared.take_effects(),
            vec![super::Effect::Open {
                path: std::path::PathBuf::from(
                    "vendor/laravel/framework/src/Foundation/Http/Kernel.php"
                ),
                line: 141,
            }]
        );
    }

    /// No project, no request: a plugin must not query a remote API to find out
    /// it has nothing to ask it.
    /// Le projet se règle **dans le panneau**, pas dans les réglages.
    ///
    /// C'est le trou que le portage avait laissé : la page des réglages ne
    /// connaît que les clés globales d'un manifeste, et un projet Sentry
    /// appartient au dépôt. Sans champ dans la vue, il n'y avait aucun moyen de
    /// le poser, et le plugin renvoyait vers une page où il n'était pas.
    #[test]
    fn the_project_is_chosen_in_the_panel() {
        let declaration = toml::from_str(MANIFEST).expect("manifest");
        let probe = probe_with(SOURCE, declaration).expect("compiles");
        let state = answer_with(
            &probe,
            |_| panic!("nothing must leave before there is a project"),
            probe.host.init(None),
        )
        .expect("init");

        // Le champ est là, avec de quoi enregistrer.
        let tree = probe.host.view(&state, "issues").expect("view");
        let Node::Column(children) = &tree else {
            panic!("expected a column, got {tree:?}");
        };
        // Le sélecteur mène, et il est là quoi qu'il arrive : c'est le seul
        // réglage de ce plugin qui appartient au dépôt, et on le corrige aussi
        // souvent qu'on le pose.
        let Some(Node::Row(picker)) = children.first() else {
            panic!("expected the picker, got {children:?}");
        };
        assert!(
            matches!(picker.get(1), Some(Node::Field { id, .. }) if id == "project"),
            "{picker:?}"
        );

        // Taper ne va rien chercher : une requête par lettre interrogerait
        // Sentry à chaque caractère du nom.
        let state = answer_with(
            &probe,
            |_| panic!("typing asks nothing of anyone"),
            probe.host.update(&state, "typing", "site"),
        )
        .expect("typing");
        assert!(probe.shared.take_effects().is_empty());

        // Enregistrer écrit contre le dépôt **et repart chercher tout de
        // suite**. C'est ce que l'ancien test ne vérifiait pas : il regardait
        // l'effet et se contentait d'un `view` qui rendait quelque chose, si
        // bien qu'il est resté vert pendant que le bouton ne faisait rien.
        let state = answer_with(
            &probe,
            |_| Ok(ISSUES.into()),
            probe.host.update(&state, "set-project", ""),
        )
        .expect("set-project");
        assert_eq!(
            probe.shared.take_effects(),
            vec![super::Effect::Remember {
                key: "project".into(),
                value: "site".into()
            }]
        );
        // Et le panneau montre les erreurs, pas de nouveau le sélecteur : un
        // script doit relire ce qu'il vient d'écrire.
        let tree = probe.host.view(&state, "issues").expect("view");
        let Node::Column(children) = &tree else {
            panic!("expected a column, got {tree:?}");
        };
        assert_eq!(first_list(children).len(), 2, "the picker is still there");
    }

    #[test]
    fn with_no_project_nothing_leaves() {
        let declaration = toml::from_str(MANIFEST).expect("manifest");
        let probe = probe_with(SOURCE, declaration).expect("compiles");
        let state = answer_with(
            &probe,
            |_| panic!("nothing must reach the workers"),
            probe.host.init(None),
        )
        .expect("init succeeds");
        let tree = probe.host.view(&state, "issues").expect("view renders");
        let Node::Column(children) = &tree else {
            panic!("expected a column, got {tree:?}");
        };
        // The picker leads, and the sentence explaining why follows it.
        assert!(
            matches!(children.first(), Some(Node::Row(_))),
            "{children:?}"
        );
        assert!(
            matches!(children.get(1), Some(Node::Empty { .. })),
            "{children:?}"
        );
    }

    /// **One issue whose event has gone must not take the list with it.**
    ///
    /// `events/latest/` has left the API and answers 404; an event outside the
    /// retention window does too. Either way it is one row's business, and
    /// letting `update` fail would replace the whole panel with a raw message
    /// — the list, the picker and the way back included.
    #[test]
    fn an_issue_with_no_event_left_keeps_the_others() {
        let probe = probe_sentry();
        let state =
            answer_with(&probe, |_| Ok(ISSUES.into()), probe.host.init(None)).expect("init");

        let state = answer_with(
            &probe,
            |_| Err("https://sentry.io/…/events/: http status: 404".into()),
            probe.host.update(&state, "choose", "0"),
        )
        .expect("a 404 on one issue is not the panel's death");

        // The list is still there — that is the whole point, and it is now a
        // panel of its own, which makes it plainer still.
        let tree = probe.host.view(&state, "issues").expect("view renders");
        let Node::Column(children) = &tree else {
            panic!("expected a column, got {tree:?}");
        };
        assert_eq!(first_list(children).len(), 2, "the list is gone");
        // And the reason sits in the issue's own panel.
        let tree = probe.host.view(&state, "issue").expect("view renders");
        let Node::Column(body) = &tree else {
            panic!("expected a column, got {tree:?}");
        };
        assert!(
            body.iter()
                .any(|node| matches!(node, Node::Callout { text, .. } if text.contains("404"))),
            "{body:?}"
        );
    }

    /// An issue Sentry keeps no event for says so, rather than spinning.
    #[test]
    fn an_issue_whose_events_expired_says_so() {
        let probe = probe_sentry();
        let state =
            answer_with(&probe, |_| Ok(ISSUES.into()), probe.host.init(None)).expect("init");
        let state = answer_with(
            &probe,
            |_| Ok("[]".into()),
            probe.host.update(&state, "choose", "0"),
        )
        .expect("an empty list is an answer");
        let tree = probe.host.view(&state, "issue").expect("view");
        let Node::Column(body) = &tree else {
            panic!("expected a column");
        };
        assert!(
            body.iter().any(
                |node| matches!(node, Node::Callout { text, .. } if text.contains("ne garde plus"))
            ),
            "{body:?}"
        );
        // And not a spinner, which would turn for ever.
        assert!(
            !body.iter().any(|node| matches!(node, Node::Spinner)),
            "{body:?}"
        );
    }

    /// **A project typed wrong must be correctable from the screen that says
    /// so.** Letting `init` fail would make the state absent, so `view` would
    /// never be called, so the picker would go with it — and a 404 would be a
    /// panel one cannot leave.
    #[test]
    fn a_project_that_does_not_exist_can_still_be_corrected() {
        let probe = probe_sentry();
        let state = answer_with(
            &probe,
            |_| Err("https://sentry.io/… answered 404 Not Found".into()),
            probe.host.init(None),
        )
        .expect("a 404 is an answer, not a reason to have no view");

        let tree = probe.host.view(&state, "issues").expect("view renders");
        let Node::Column(children) = &tree else {
            panic!("expected a column, got {tree:?}");
        };
        // The picker is there, seeded with what was tried, so the typo is in
        // front of the cursor rather than to be typed again.
        let Some(Node::Row(picker)) = children.first() else {
            panic!("expected the picker, got {children:?}");
        };
        assert!(
            matches!(picker.get(1), Some(Node::Field { value, .. }) if value == "site"),
            "{picker:?}"
        );
        // And what Sentry said is shown, not swallowed.
        assert!(
            children
                .iter()
                .any(|node| matches!(node, Node::Code { text, .. } if text.contains("404"))),
            "{children:?}"
        );
    }
}
