//! What the editor does with a language server.
//!
//! The interface is already written: `gpui_component`'s editor carries an
//! `Lsp` — a completion provider, a hover provider, a definition provider — and
//! a `DiagnosticSet`, with the popovers, the underlines and the gutter that go
//! with them. Nothing here draws anything. What it does is the one thing that
//! was missing: **be the client**, and reach it through the `Cmd`/`Evt` loop
//! rather than from the frame it is asked in.
//!
//! Three things hold it together.
//!
//! **The providers return a `Task`, and the answer comes from an event.** A
//! provider is called in the interface thread and must hand back a future; the
//! worker answers, minutes of plumbing later, on the event channel like
//! everything else. `lsp_request` bridges the two with a one-shot channel — a
//! `bounded(1)`, `async_channel` being already in the tree — filed under an id
//! that never goes back, the same device as the SQL console's send id. An
//! answer that never comes would leave a popover spinning for ever, so nothing
//! is left waiting: a request is failed by the core's timeout, by the session
//! ending, or by the request that supersedes it.
//!
//! **A provider holds a weak reference to the application**, like the dock's
//! panels and the results table: it is created per opened file and outlives
//! nothing.
//!
//! **The server is per worktree and off by default**, and it is the file that
//! decides which one: `lsp::pick` takes the longest matching extension, so a
//! Blade view goes to the entry that says `blade.php` and not to the one that
//! says `php`. Opening a file of another language restarts the session with the
//! server that serves it — a worktree is one project, and two servers running
//! for one would both answer the same question.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{anyhow, Result};
use gpui::{App, SharedString, Task, WeakEntity, Window};
use gpui_component::input::{CompletionProvider, DefinitionProvider, HoverProvider, Rope, RopeExt};
use lsp_types::{
    CompletionContext, CompletionResponse, Diagnostic, GotoDefinitionResponse, Hover, LocationLink,
    ShowDocumentParams,
};
use serde_json::{json, Value};

use crate::lsp::Server;
use crate::runtime::protocol::Cmd;
use crate::ui::app::ClaudhubApp;
use crate::ui::settings::Settings;

/// How long a change waits before it is sent.
///
/// A keystroke emits a change event, and a document is sent whole or as an
/// edit either way: without this, typing a line is fifty round trips through
/// the queue for a state nobody asked about in between. Short enough that the
/// completion asked right after a dot sees the dot.
pub const CHANGE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(120);

/// Where a worktree's language server stands.
///
/// Four states and not two, because a button that only knows on and off says
/// nothing at the one moment it is asked: a server that will not start — the
/// binary is not installed, the project has no `composer.json` it likes — must
/// say so, not look like a server that is thinking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Starting,
    Ready,
    Failed(SharedString),
}

/// A worktree's session, as the view knows it.
pub struct Session {
    pub server: Server,
    pub status: Status,
    /// What the server says it is busy with (`$/progress`).
    pub busy: Option<SharedString>,
    pub capabilities: Capabilities,
    /// Diagnostics by file. Kept for every file the server pushes, not only the
    /// open one: PHPantom sweeps the whole project after startup, and the count
    /// on the button is the answer to "is this branch clean".
    pub diagnostics: HashMap<PathBuf, Vec<Diagnostic>>,
}

impl Session {
    /// How many errors and warnings, all files together.
    pub fn problems(&self) -> usize {
        self.diagnostics.values().map(Vec::len).sum()
    }
}

/// What the server said it can do, read once at the handshake.
///
/// Posting a provider for something the server does not have is a round trip
/// per gesture to be told nothing — and, for the hover, one per pointer rest.
#[derive(Debug, Default, Clone)]
pub struct Capabilities {
    pub completion: bool,
    /// The characters that open a completion by themselves: `->`, `::`, `$` in
    /// PHP. They come from the server because only it knows its language.
    pub triggers: Vec<String>,
    pub hover: bool,
    pub definition: bool,
}

impl Capabilities {
    pub fn read(payload: &str) -> Self {
        let value: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
        let has = |key: &str| {
            // A capability is `true`, or an object describing itself; only
            // `false` and absence mean no.
            !matches!(
                value.get(key),
                None | Some(Value::Bool(false)) | Some(Value::Null)
            )
        };
        Self {
            completion: has("completionProvider"),
            triggers: value
                .get("completionProvider")
                .and_then(|c| c.get("triggerCharacters"))
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            hover: has("hoverProvider"),
            definition: has("definitionProvider"),
        }
    }
}

impl ClaudhubApp {
    /// The servers that may serve this worktree, the project's first.
    ///
    /// The project's `wt.toml` wins over the settings for the same language,
    /// and that order is the whole argument for reading it at all: the machine
    /// says what it has installed, the project says what its code wants, and
    /// the project is the one that knows.
    pub(super) fn lsp_servers(&self, worktree: &Path, cx: &App) -> Vec<Server> {
        let mut servers: Vec<Server> = self
            .main_of(worktree)
            .and_then(|main| self.wt_project(&main))
            .map(|project| project.lsp.clone())
            .unwrap_or_default();
        servers.extend(Settings::global(cx).lsp.iter().cloned());
        servers
    }

    pub(super) fn lsp_session(&self, worktree: &Path) -> Option<&Session> {
        self.lsp.get(worktree)
    }

    /// Is the language server switched on for this worktree.
    pub(super) fn lsp_enabled(&self, worktree: &Path) -> bool {
        self.review.get(worktree).is_some_and(|state| state.lsp)
    }

    /// The button. Switching on starts a server for the file in the editor;
    /// switching off ends the session and takes its diagnostics with it —
    /// leaving underlines nothing maintains any more would be worse than none.
    pub(super) fn toggle_lsp(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let on = !self.lsp_enabled(&worktree);
        if let Some(state) = self.review.get_mut(&worktree) {
            state.lsp = on;
        }
        self.persist_review(&worktree, cx);
        if on {
            self.lsp_sync_editor(window, cx);
        } else {
            self.lsp_stop(&worktree, cx);
        }
        cx.notify();
    }

    fn lsp_stop(&mut self, worktree: &Path, cx: &mut gpui::Context<Self>) {
        if self.lsp.remove(worktree).is_none() {
            return;
        }
        self.git.send(Cmd::LspStop {
            worktree: worktree.to_path_buf(),
        });
        self.lsp_clear_diagnostics(cx);
    }

    /// Brings the session into line with the file being edited: starts the
    /// server that serves it, opens the document, and posts the providers.
    ///
    /// Called at every opening and whenever the button is switched on, which is
    /// what makes it the single place where a session is born.
    pub(super) fn lsp_sync_editor(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(editing) = self.editing.as_ref() else {
            return;
        };
        let (worktree, path) = (editing.worktree.clone(), editing.path.clone());
        if !self.lsp_enabled(&worktree) {
            return;
        }
        let servers = self.lsp_servers(&worktree, cx);
        let Some(server) = crate::lsp::pick(&servers, &path).cloned() else {
            return; // nobody serves this language: no session, and no button
        };
        // A session for another language is not this file's: the worktree gets
        // the server its file asks for, and two of them would answer the same
        // question twice.
        let running = self
            .lsp
            .get(&worktree)
            .is_some_and(|session| session.server == server);
        if !running {
            self.lsp_stop(&worktree, cx);
            self.git.send(Cmd::LspStart {
                worktree: worktree.clone(),
                server: server.clone(),
            });
            self.lsp.insert(
                worktree.clone(),
                Session {
                    server: server.clone(),
                    status: Status::Starting,
                    busy: None,
                    capabilities: Capabilities::default(),
                    diagnostics: HashMap::new(),
                },
            );
        }
        let text = self
            .editing
            .as_ref()
            .map(|editing| editing.input.read(cx).value().to_string())
            .unwrap_or_default();
        self.git.send(Cmd::LspOpen {
            worktree: worktree.clone(),
            path: path.clone(),
            language_id: server.language_for(&path),
            text,
        });
        self.lsp_install_providers(window, cx);
    }

    /// Posts on the editor what the server can answer, and nothing else.
    fn lsp_install_providers(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(editing) = self.editing.as_ref() else {
            return;
        };
        let (worktree, path) = (editing.worktree.clone(), editing.path.clone());
        let Some(session) = self.lsp.get(&worktree) else {
            return;
        };
        // Before the handshake there are no capabilities to read: the providers
        // are posted again when it lands, which is `lsp_ready`'s other job.
        let capabilities = session.capabilities.clone();
        let provider = Rc::new(Provider {
            app: cx.entity().downgrade(),
            worktree,
            path,
            triggers: capabilities.triggers.clone(),
        });
        let app = cx.entity().downgrade();
        editing.input.update(cx, |state, cx| {
            let lsp = state.lsp_mut();
            lsp.completion_provider = capabilities
                .completion
                .then(|| provider.clone() as Rc<dyn CompletionProvider>);
            lsp.hover_provider = capabilities
                .hover
                .then(|| provider.clone() as Rc<dyn HoverProvider>);
            lsp.definition_provider = capabilities
                .definition
                .then(|| provider.clone() as Rc<dyn DefinitionProvider>);
            // Following a definition into another file is ours to do: the
            // built-in behaviour jumps inside the document being edited, which
            // is right for a local symbol and useless for the class it came
            // from. Returning `true` says we have shown it.
            lsp.show_document = Some(Rc::new(move |params: &ShowDocumentParams, _window, cx| {
                let Some(path) = crate::lsp::uri::path(params.uri.as_str()) else {
                    return false;
                };
                let line = params
                    .selection
                    .map(|range| range.start.line as usize + 1)
                    .unwrap_or(1);
                let _ = line; // the editor has no "go to line" yet
                app.update(cx, |this, cx| this.open_in_editor(path, cx))
                    .is_ok()
            }));
            cx.notify();
        });
    }

    /// The editor's text has changed: tell the server, once the typing pauses.
    pub(super) fn lsp_editor_changed(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        if editing.lsp_pending {
            return; // a send is already scheduled: it will read the latest text
        }
        editing.lsp_pending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CHANGE_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                let Some(editing) = this.editing.as_mut() else {
                    return;
                };
                editing.lsp_pending = false;
                let (worktree, path) = (editing.worktree.clone(), editing.path.clone());
                let text = editing.input.read(cx).value().to_string();
                if !this.lsp_enabled(&worktree) || !this.lsp.contains_key(&worktree) {
                    return;
                }
                this.git.send(Cmd::LspChange {
                    worktree,
                    path,
                    text,
                });
            });
        })
        .detach();
    }

    pub(super) fn lsp_editor_saved(&mut self) {
        let Some(editing) = self.editing.as_ref() else {
            return;
        };
        if !self.lsp.contains_key(&editing.worktree) {
            return;
        }
        self.git.send(Cmd::LspSave {
            worktree: editing.worktree.clone(),
            path: editing.path.clone(),
        });
    }

    /// The editor is closing on that file: the server must forget it, or it
    /// keeps answering about a text nobody is holding any more.
    pub(super) fn lsp_editor_closed(&mut self, worktree: PathBuf, path: PathBuf) {
        if !self.lsp.contains_key(&worktree) {
            return;
        }
        self.git.send(Cmd::LspClose { worktree, path });
    }

    // — What comes back ————————————————————————————————————————

    pub(super) fn lsp_ready(
        &mut self,
        worktree: PathBuf,
        name: String,
        capabilities: String,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let capabilities = Capabilities::read(&capabilities);
        log::info!("language server {name} ready in {}", worktree.display());
        if let Some(session) = self.lsp.get_mut(&worktree) {
            session.status = Status::Ready;
            session.capabilities = capabilities;
        }
        // The providers were posted with no capabilities to read: now there are.
        if self
            .editing
            .as_ref()
            .is_some_and(|e| e.worktree == worktree)
        {
            self.lsp_install_providers(window, cx);
        }
        cx.notify();
    }

    pub(super) fn lsp_stopped(
        &mut self,
        worktree: PathBuf,
        reason: Option<String>,
        cx: &mut gpui::Context<Self>,
    ) {
        // Everything that was waiting on it dies with it: a `Task` nobody
        // resolves is a popover that spins for ever.
        self.lsp_fail_pending(&format!(
            "the language server stopped{}",
            reason
                .as_deref()
                .map(|r| format!(": {r}"))
                .unwrap_or_default()
        ));
        match reason {
            // We ended it: the session is already gone from the table.
            None => {}
            Some(reason) => {
                if let Some(session) = self.lsp.get_mut(&worktree) {
                    session.status = Status::Failed(reason.clone().into());
                    session.diagnostics.clear();
                }
                log::warn!("language server in {}: {reason}", worktree.display());
                self.lsp_clear_diagnostics(cx);
            }
        }
        cx.notify();
    }

    pub(super) fn lsp_busy(
        &mut self,
        worktree: PathBuf,
        message: Option<String>,
        cx: &mut gpui::Context<Self>,
    ) {
        if let Some(session) = self.lsp.get_mut(&worktree) {
            session.busy = message.map(SharedString::from);
        }
        cx.notify();
    }

    /// One answer, handed to whoever is waiting for it.
    pub(super) fn lsp_answer(&mut self, id: u64, result: Result<String, String>) {
        if let Some(waiting) = self.lsp_pending.remove(&id) {
            let _ = waiting.try_send(result);
        }
    }

    pub(super) fn lsp_diagnostics(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        payload: String,
        cx: &mut gpui::Context<Self>,
    ) {
        let diagnostics: Vec<Diagnostic> = serde_json::from_str(&payload).unwrap_or_default();
        if let Some(session) = self.lsp.get_mut(&worktree) {
            if diagnostics.is_empty() {
                session.diagnostics.remove(&path);
            } else {
                session.diagnostics.insert(path.clone(), diagnostics);
            }
        }
        // The editor only holds the file it is showing; the rest is kept for
        // the count on the button.
        if self
            .editing
            .as_ref()
            .is_some_and(|e| e.worktree == worktree && e.path == path)
        {
            self.lsp_paint_diagnostics(cx);
        }
        cx.notify();
    }

    /// Puts the open file's diagnostics into the editor's own set.
    fn lsp_paint_diagnostics(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(editing) = self.editing.as_ref() else {
            return;
        };
        let diagnostics = self
            .lsp
            .get(&editing.worktree)
            .and_then(|session| session.diagnostics.get(&editing.path))
            .cloned()
            .unwrap_or_default();
        editing.input.update(cx, |state, cx| {
            if let Some(set) = state.diagnostics_mut() {
                set.clear();
                set.extend(diagnostics);
                cx.notify();
            }
        });
    }

    fn lsp_clear_diagnostics(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(editing) = self.editing.as_ref() else {
            return;
        };
        editing.input.update(cx, |state, cx| {
            if let Some(set) = state.diagnostics_mut() {
                set.clear();
                cx.notify();
            }
        });
    }

    // — The bridge ——————————————————————————————————————————————

    /// Sends a request and hands back what will carry its answer.
    ///
    /// The id never goes back, and the previous request of the same method for
    /// the same worktree is **cancelled**: a completion is asked on one
    /// keystroke and stale on the next, and a server told nothing goes on
    /// computing an answer nobody will read.
    fn lsp_request(
        &mut self,
        worktree: &Path,
        method: &str,
        params: Value,
    ) -> async_channel::Receiver<Result<String, String>> {
        let (sender, receiver) = async_channel::bounded(1);
        if method == "textDocument/completion" {
            if let Some(previous) = self.lsp_asking.insert(
                (worktree.to_path_buf(), method.to_string()),
                self.lsp_next_id,
            ) {
                if self.lsp_pending.remove(&previous).is_some() {
                    self.git.send(Cmd::LspCancel {
                        worktree: worktree.to_path_buf(),
                        id: previous,
                    });
                }
            }
        }
        let id = self.lsp_next_id;
        self.lsp_next_id += 1;
        self.lsp_pending.insert(id, sender);
        self.git.send(Cmd::LspRequest {
            worktree: worktree.to_path_buf(),
            id,
            method: method.to_string(),
            params: params.to_string(),
        });
        receiver
    }

    fn lsp_fail_pending(&mut self, reason: &str) {
        for (_, waiting) in std::mem::take(&mut self.lsp_pending) {
            let _ = waiting.try_send(Err(reason.to_string()));
        }
        self.lsp_asking.clear();
    }
}

/// The three providers, one object: they answer about the same document, and
/// splitting them would be three weak references and three clones per file.
struct Provider {
    app: WeakEntity<ClaudhubApp>,
    worktree: PathBuf,
    path: PathBuf,
    triggers: Vec<String>,
}

impl Provider {
    /// The round trip, typed at both ends.
    fn ask<T: serde::de::DeserializeOwned + 'static>(
        &self,
        method: &'static str,
        params: Value,
        cx: &mut App,
    ) -> Task<Result<Option<T>>> {
        let app = self.app.clone();
        let worktree = self.worktree.clone();
        let Ok(receiver) = app.update(cx, |this, _| this.lsp_request(&worktree, method, params))
        else {
            return Task::ready(Err(anyhow!("the window is gone")));
        };
        cx.spawn(async move |_cx| {
            let payload = receiver
                .recv()
                .await
                .map_err(|_| anyhow!("no answer from the language server"))?
                .map_err(|e| anyhow!(e))?;
            // A server answering `null` is answering: it has nothing to say
            // here, which is not a failure.
            if payload.trim() == "null" {
                return Ok(None);
            }
            Ok(Some(serde_json::from_str::<T>(&payload)?))
        })
    }

    fn document(&self) -> Value {
        json!({"uri": crate::lsp::uri::of(&self.path)})
    }

    fn position(&self, text: &Rope, offset: usize) -> Value {
        let position = text.offset_to_position(offset);
        json!({"line": position.line, "character": position.character})
    }
}

impl CompletionProvider for Provider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let params = json!({
            "textDocument": self.document(),
            "position": self.position(text, offset),
            "context": trigger,
        });
        let task = self.ask::<CompletionResponse>("textDocument/completion", params, cx);
        cx.spawn(async move |_cx| Ok(task.await?.unwrap_or(CompletionResponse::Array(Vec::new()))))
    }

    /// Cheap and synchronous: it runs in the interface thread on every
    /// keystroke. The trigger characters come from the server — `->` and `::`
    /// in PHP — and a word character opens the list too, which is what every
    /// editor does and what nobody thinks to ask for.
    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        if new_text.is_empty() {
            return false;
        }
        if self.triggers.iter().any(|t| new_text.ends_with(t.as_str())) {
            return true;
        }
        new_text
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
    }
}

impl HoverProvider for Provider {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<Hover>>> {
        let params = json!({
            "textDocument": self.document(),
            "position": self.position(text, offset),
        });
        self.ask::<Hover>("textDocument/hover", params, cx)
    }
}

impl DefinitionProvider for Provider {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<LocationLink>>> {
        let params = json!({
            "textDocument": self.document(),
            "position": self.position(text, offset),
        });
        let task = self.ask::<GotoDefinitionResponse>("textDocument/definition", params, cx);
        cx.spawn(async move |_cx| Ok(task.await?.map(links).unwrap_or_default()))
    }
}

/// The three shapes a definition comes back in, brought to the one the editor
/// takes.
///
/// A server answers `Location`, an array of them, or `LocationLink`s, and which
/// one is its choice — `linkSupport` only says we can read the third. Nothing
/// in the interface distinguishes them: what it shows is a target.
fn links(response: GotoDefinitionResponse) -> Vec<LocationLink> {
    let one = |location: lsp_types::Location| LocationLink {
        origin_selection_range: None,
        target_uri: location.uri,
        target_range: location.range,
        target_selection_range: location.range,
    };
    match response {
        GotoDefinitionResponse::Scalar(location) => vec![one(location)],
        GotoDefinitionResponse::Array(locations) => locations.into_iter().map(one).collect(),
        GotoDefinitionResponse::Link(links) => links,
    }
}

/// The line the button shows: what the server is doing, or what went wrong.
pub fn tooltip(session: Option<&Session>) -> Option<SharedString> {
    let session = session?;
    match &session.status {
        Status::Failed(reason) => Some(reason.clone()),
        Status::Starting => Some(SharedString::from(session.server.name.clone())),
        Status::Ready => Some(session.busy.clone().unwrap_or_else(|| {
            SharedString::from(match session.problems() {
                0 => session.server.name.clone(),
                n => format!("{} — {n}", session.server.name),
            })
        })),
    }
}
