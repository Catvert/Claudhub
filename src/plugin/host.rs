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
//! pub fn view(state)                          -> node
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
use crate::plugin::view::{Handler, Item, Node, TextStyle};
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

    fn secret(&self, name: &str) -> Option<Secret> {
        self.secrets
            .lock()
            .expect("secrets lock")
            .get(name)
            .filter(|value| !value.trim().is_empty())
            .map(|value| Secret(value.clone()))
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

/// The node type the script builds. Opaque on its side, a [`Node`] on ours.
#[derive(Any, Debug, Clone)]
#[rune(item = ::claudhub)]
pub struct RuneNode(Node);

/// One row of a list.
#[derive(Any, Debug, Clone)]
#[rune(item = ::claudhub)]
pub struct RuneItem(Item);

/// Empty means absent. A script has no `Option` worth the ceremony here, and
/// "no icon" and "the icon called nothing" are the same thing.
fn maybe(text: &str) -> Option<String> {
    Some(text.trim())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Builds the module a script sees. Done once per plugin: its closures capture
/// that plugin's `Shared`, and a reload keeps them.
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
    module
        .function("primary", |node: RuneNode| match node.0 {
            Node::Button {
                label,
                icon,
                on_click,
                disabled,
                ..
            } => RuneNode(Node::Button {
                label,
                icon,
                on_click,
                disabled,
                primary: true,
            }),
            other => RuneNode(other),
        })
        .build()?;
    module
        .function("disabled", |node: RuneNode| match node.0 {
            Node::Button {
                label,
                icon,
                on_click,
                primary,
                ..
            } => RuneNode(Node::Button {
                label,
                icon,
                on_click,
                disabled: true,
                primary,
            }),
            other => RuneNode(other),
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
        .function("agent", move |text: Ref<str>| {
            it.effect(Effect::Agent(text.to_string()))
        })
        .build()?;
    let it = shared.clone();
    module
        .function("open", move |path: Ref<str>, line: i64| {
            it.effect(Effect::Open {
                path: PathBuf::from(&*path),
                line: usize::try_from(line).unwrap_or(0),
            })
        })
        .build()?;
    let it = shared.clone();
    module
        .function("notify", move |text: Ref<str>| {
            it.effect(Effect::Notify(text.to_string()))
        })
        .build()?;
    let id = shared.id.clone();
    module
        .function("log", move |text: Ref<str>| {
            log::info!(target: "plugin", "{id}: {}", &*text);
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

/// A loaded plugin's Rune side.
pub struct Host {
    shared: Arc<Shared>,
    runtime: Arc<RuntimeContext>,
    unit: Arc<Unit>,
}

impl Host {
    /// Compiles a script. The error is already the one to show: Rune's
    /// diagnostics, rendered, which name the line.
    pub fn load(manifest: &Manifest, shared: Arc<Shared>) -> Result<Self, String> {
        let source = std::fs::read_to_string(manifest.entry())
            .map_err(|e| format!("{}: {e}", manifest.entry().display()))?;
        Self::from_source(manifest.id.clone(), &source, shared)
    }

    /// The same, from a string. **This is what makes the host testable**: the
    /// whole loop plays out in memory, with no file and no process, on the
    /// model of the language client's `Session::run`.
    pub fn from_source(name: String, source: &str, shared: Arc<Shared>) -> Result<Self, String> {
        let mut context =
            Context::with_default_modules().map_err(|e| format!("Rune's standard library: {e}"))?;
        context
            .install(module(&shared).map_err(|e| format!("the claudhub module: {e}"))?)
            .map_err(|e| format!("the claudhub module: {e}"))?;
        let runtime = Arc::new(
            context
                .runtime()
                .map_err(|e| format!("Rune's runtime: {e}"))?,
        );

        let mut sources = Sources::new();
        sources
            .insert(Source::new(name, source).map_err(|e| format!("{e}"))?)
            .map_err(|e| format!("{e}"))?;
        let mut diagnostics = Diagnostics::new();
        let built = rune::prepare(&mut sources)
            .with_context(&context)
            .with_diagnostics(&mut diagnostics)
            .build();
        let unit = match built {
            Ok(unit) => unit,
            Err(_) => return Err(render(&diagnostics, &sources)),
        };
        Ok(Self {
            shared,
            runtime,
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
    pub fn view(&self, state: &Value) -> Result<Node, String> {
        let value = self
            .vm()
            .call(["view"], (state.clone(),))
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
            screen: "review".into(),
            icon: None,
            capabilities,
            settings: BTreeMap::from([("base".to_string(), "https://api.test".to_string())]),
            secrets: Vec::new(),
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
            declaration,
            panel: "ClaudhubPlugin:probe",
        };
        let shared = Shared::new(&manifest, tx);
        shared.set_worktree(Some(PathBuf::from("/p/site")));
        let host = Host::from_source(manifest.id.clone(), source, shared.clone())?;

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

        pub fn view(state) {
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

        let tree = probe.host.view(&state).expect("view renders");
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

        let tree = probe.host.view(&state).expect("view renders");
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
        let message = match probe("pub fn view(state) { column( }", vec![]) {
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

    /// The settings a manifest declares reach the script, and the ones the user
    /// wrote lie over them without recompiling anything.
    #[test]
    fn a_setting_is_read_at_the_moment_it_is_asked_for() {
        const SOURCE: &str = r#"
            use claudhub::*;
            pub async fn init(worktree) { Ok(setting("base")) }
            pub fn view(state) { text(state) }
        "#;
        let probe = probe(SOURCE, vec![]).expect("the script compiles");
        let state = crate::runtime::executor::block_on(probe.host.init(None)).expect("init");
        assert_eq!(
            probe.host.view(&state).expect("view"),
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
            probe.host.view(&state).expect("view"),
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
        let tree = probe.host.view(&state).expect("view renders");

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
        let tree = probe.host.view(&state).expect("view renders");
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
        assert_eq!(effects.len(), 2, "{effects:?}");
        let super::Effect::Agent(text) = &effects[0] else {
            panic!("expected a paste, got {effects:?}");
        };
        assert!(text.contains("Refaire le fil"), "{text}");
        assert!(text.contains("assertion failed"), "{text}");
        // The state came back whole: the run list is still there.
        probe.host.view(&state).expect("view still renders");
    }
}
