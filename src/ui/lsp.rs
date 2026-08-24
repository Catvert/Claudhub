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
use gpui::Entity;
use gpui::{App, SharedString, Task, WeakEntity, Window};
use gpui_component::input::{
    CodeActionProvider, CompletionProvider, DefinitionProvider,
    DocumentRangeSemanticTokensProvider, EditorState, HoverProvider, Rope, RopeExt,
};
use lsp_types::{
    CodeAction, CodeActionOrCommand, CodeActionResponse, CompletionContext, CompletionResponse,
    Diagnostic, GotoDefinitionResponse, Hover, LocationLink, SemanticTokenType, SemanticTokens,
    SemanticTokensLegend, ShowDocumentParams, TextEdit, WorkspaceEdit,
};
use serde_json::{json, Value};

use crate::lsp::Server;
use crate::runtime::protocol::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::settings::Settings;

/// The absolute path the protocol names a document by.
///
/// Everything else in this window calls a file by its path **inside** the
/// worktree — that is what git prints, and what every command joins the
/// worktree onto at the last moment. The protocol has no worktree to join: a
/// URI is absolute or it is nothing, and `file://app/User.php` reads as a host
/// called `app` and a file called `/User.php`. The join therefore happens here,
/// at the one boundary, and never in the core, which is handed a path already
/// made.
fn full(worktree: &Path, path: &Path) -> PathBuf {
    crate::wslpath::join(worktree, path)
}

/// And back: what the rest of the window calls that file.
///
/// A path outside the worktree is left as it is — a server may point into a
/// runtime's sources, which belong to nobody's tree — and one inside comes back
/// relative, which is what makes it the same file as the explorer's.
fn local(worktree: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(worktree).unwrap_or(path).to_path_buf()
}

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
    pub code_actions: bool,
    pub semantic: Option<Semantic>,
}

/// What a server offers in the way of semantic tokens.
///
/// The legend is **its** vocabulary — `parameter`, `enumMember`, `decorator` —
/// and the index in it is all a token carries. We keep it translated into the
/// names our themes know (see `theme_name`), in the server's own order, because
/// that order is what the indices refer to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Semantic {
    pub names: Vec<&'static str>,
    /// The server answers for the whole document. PHPantom says `full: true,
    /// range: false`, so asking it for a range is asking for a refusal — and
    /// the editor asks us for the whole document anyway.
    pub full: bool,
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
            code_actions: has("codeActionProvider"),
            semantic: value
                .get("semanticTokensProvider")
                .map(|provider| Semantic {
                    names: provider
                        .get("legend")
                        .and_then(|legend| legend.get("tokenTypes"))
                        .and_then(Value::as_array)
                        .map(|types| {
                            types
                                .iter()
                                .filter_map(Value::as_str)
                                .map(theme_name)
                                .collect()
                        })
                        .unwrap_or_default(),
                    full: !matches!(
                        provider.get("full"),
                        None | Some(Value::Bool(false)) | Some(Value::Null)
                    ),
                }),
        }
    }
}

/// A server's token type, in the vocabulary our themes speak.
///
/// **The translation is here and not in the theme**, and that is the whole
/// difficulty of semantic tokens. What comes back is an index into the server's
/// legend — `parameter`, `enumMember`, `typeParameter` — and the editor resolves
/// that *name* against the theme at paint time. Our palettes are written in
/// tree-sitter's vocabulary, which has no `parameter` and no `enumMember`: a
/// name the theme does not know resolves to nothing, and a token with no style
/// is a token that changes nothing. It fails silently, which is the only way
/// this feature can fail.
///
/// So the legend we hand the editor is the server's, **in its order** — the
/// indices refer to it — with each name replaced by one our themes define. A
/// dotted name costs nothing: `function.method` is exact where a theme
/// distinguishes methods and falls back to `function` where it does not.
///
/// An empty name is deliberate: it means "say nothing here". `variable` is the
/// case — our themes give variables no colour of their own, and painting them
/// would only overwrite what tree-sitter already decided, with the same hue.
fn theme_name(token_type: &str) -> &'static str {
    match token_type {
        // Everything that names a kind of thing takes the type colour. Our
        // palettes have no hue for a namespace, and inventing one here would be
        // inventing it for twelve themes at once.
        "namespace" | "class" | "interface" | "enum" | "struct" | "type" | "typeParameter" => {
            "type"
        }
        // The one that pays for the whole feature: what tells a parameter from
        // any other variable, which no grammar can know. `variable.special` is
        // the only variable-ish hue our themes define, and its meaning — this
        // identifier is not an ordinary one — is the right one to borrow.
        "parameter" => "variable.special",
        "property" | "event" => "property",
        "function" => "function",
        "method" => "function.method",
        "macro" => "function.macro",
        // A PHP attribute, `#[Route(...)]`, which is what a decorator is here.
        "decorator" => "attribute",
        "enumMember" => "constant",
        "keyword" | "modifier" => "keyword",
        "comment" => "comment",
        "string" => "string",
        "number" => "number",
        "regexp" => "string.regex",
        "operator" => "operator",
        // `variable`, and anything a server invents: left to the grammar.
        _ => "",
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

    /// Whether any of them would serve this file — the button's only question.
    ///
    /// Asked at every frame of the editor's bar, where `lsp_servers` would
    /// clone the whole list to answer it: what is wanted is not *which* server
    /// but whether there is one, and that is a walk of two borrowed lists.
    pub(super) fn lsp_serves(&self, worktree: &Path, path: &Path, cx: &App) -> bool {
        let project = self
            .main_of(worktree)
            .and_then(|main| self.wt_project(&main))
            .map(|project| project.lsp.as_slice())
            .unwrap_or_default();
        project
            .iter()
            .chain(Settings::global(cx).lsp.iter())
            .any(|server| server.is_runnable() && server.matches(path).is_some())
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
        let Some(editing) = self.editing() else {
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
            .editing()
            .map(|editing| editing.input.read(cx).value().to_string())
            .unwrap_or_default();
        self.git.send(Cmd::LspOpen {
            path: full(&worktree, &path),
            worktree: worktree.clone(),
            language_id: server.language_for(&path),
            text,
        });
        self.lsp_install_providers(window, cx);
    }

    /// Posts on the editor what the server can answer, and nothing else.
    fn lsp_install_providers(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(editing) = self.editing() else {
            return;
        };
        let (worktree, path) = (editing.worktree.clone(), editing.path.clone());
        let Some(session) = self.lsp.get(&worktree) else {
            return;
        };
        // Before the handshake there are no capabilities to read: the providers
        // are posted again when it lands, which is `lsp_ready`'s other job.
        let capabilities = session.capabilities.clone();
        let home = worktree.clone();
        let provider = Rc::new(Provider {
            app: cx.entity().downgrade(),
            path: full(&worktree, &path),
            worktree,
            triggers: capabilities.triggers.clone(),
            semantic: capabilities.semantic.clone(),
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
            // A `Vec` in their API: several providers may answer for one
            // document — a linter's fixes next to a server's. We have one, and
            // it is posted or not.
            lsp.semantic_tokens_provider = capabilities
                .semantic
                .as_ref()
                .map(|_| provider.clone() as Rc<dyn DocumentRangeSemanticTokensProvider>);
            lsp.code_action_providers = if capabilities.code_actions {
                vec![provider.clone() as Rc<dyn CodeActionProvider>]
            } else {
                Vec::new()
            };
            // Following a definition into another file is ours to do: the
            // built-in behaviour jumps inside the document being edited, which
            // is right for a local symbol and useless for the class it came
            // from. Returning `true` says we have shown it.
            lsp.show_document = Some(Rc::new(move |params: &ShowDocumentParams, window, cx| {
                let Some(path) = crate::lsp::uri::path(params.uri.as_str()) else {
                    return false;
                };
                let path = local(&home, &path);
                let landing =
                    params
                        .selection
                        .map(|range| crate::ui::explorer::Landing::Position {
                            line: range.start.line,
                            character: range.start.character,
                        });
                // **Deferred, and it has to be.** This hook is called from
                // inside the editor entity's own update — the context menu's
                // "go to definition", and a system-key click, both reach it
                // that way. Opening a file reads that very entity, to note
                // where the trail is leaving from, and reading an entity that
                // is checked out for writing is a panic. The answer stays
                // immediate: we have taken the jump, we just make it a moment
                // later.
                let app = app.clone();
                // **Said before the jump, not after.** Our own `Ctrl`+click
                // listener sits on an ancestor of the editor and runs in this
                // same dispatch, right after the editor's handler returns: it
                // reads this to know the click has already been answered. The
                // application is not the entity being written to here — the
                // editor is — so the flag can be set at once, where the jump
                // itself cannot.
                let _ = app.update(cx, |this, _| this.followed_definition = true);
                window.defer(cx, move |window, cx| {
                    let _ = app.update(cx, |this, cx| match landing {
                        Some(landing) => this.jump_to(path, landing, window, cx),
                        None => this.open_in_editor(path, cx),
                    });
                });
                true
            }));
            cx.notify();
        });
    }

    /// The editor's text has changed: tell the server, once the typing pauses.
    ///
    /// The worktree is given and not read off the selection, as it is for the
    /// unsaved indicator: one browses during the debounce, and the flag would
    /// be cleared on somebody else's editor — leaving this one unable to send
    /// another change for as long as it stays open.
    pub(super) fn lsp_editor_changed(
        &mut self,
        owner: &std::path::Path,
        cx: &mut gpui::Context<Self>,
    ) {
        let owner = owner.to_path_buf();
        let Some(editing) = self
            .editings
            .get_mut(&owner)
            .and_then(|tabs| tabs.active_mut())
        else {
            return;
        };
        if editing.lsp_pending {
            return; // a send is already scheduled: it will read the latest text
        }
        editing.lsp_pending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CHANGE_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                let Some(editing) = this
                    .editings
                    .get_mut(&owner)
                    .and_then(|tabs| tabs.active_mut())
                else {
                    return;
                };
                editing.lsp_pending = false;
                let (worktree, path) = (editing.worktree.clone(), editing.path.clone());
                let text = editing.input.read(cx).value().to_string();
                if !this.lsp_enabled(&worktree) || !this.lsp.contains_key(&worktree) {
                    return;
                }
                this.git.send(Cmd::LspChange {
                    path: full(&worktree, &path),
                    worktree,
                    text,
                });
            });
        })
        .detach();
    }

    pub(super) fn lsp_editor_saved(&mut self) {
        let Some(editing) = self.editing() else {
            return;
        };
        if !self.lsp.contains_key(&editing.worktree) {
            return;
        }
        self.git.send(Cmd::LspSave {
            path: full(&editing.worktree, &editing.path),
            worktree: editing.worktree.clone(),
        });
    }

    /// The editor is closing on that file: the server must forget it, or it
    /// keeps answering about a text nobody is holding any more.
    pub(super) fn lsp_editor_closed(&mut self, worktree: PathBuf, path: PathBuf) {
        if !self.lsp.contains_key(&worktree) {
            return;
        }
        let path = full(&worktree, &path);
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
        if self.editing().is_some_and(|e| e.worktree == worktree) {
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

    /// The server asks for an edit: apply what concerns the open file, and
    /// answer — a request left hanging keeps the server waiting for ever.
    pub(super) fn lsp_apply_edit(
        &mut self,
        worktree: PathBuf,
        id: u64,
        payload: String,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let edit: Option<WorkspaceEdit> = serde_json::from_str(&payload).ok();
        let applied = match (edit, self.editing()) {
            (Some(edit), Some(editing)) if editing.worktree == worktree => {
                let state = editing.input.clone();
                let path = full(&editing.worktree, &editing.path);
                apply_to(&state, &path, &edit, window, cx)
            }
            // Nothing open to apply it to, or an unreadable edit: refused, and
            // said out loud. A fix that silently does nothing is the worst of
            // the three answers.
            _ => false,
        };
        if !applied {
            self.announce(tr!("editor-lsp-edit-refused"), cx);
        }
        self.git.send(Cmd::LspApplied {
            worktree,
            id,
            applied,
        });
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
        // The server answers by URI, so the path arrives absolute; everything
        // it is compared against here is a path inside the worktree.
        let path = local(&worktree, &path);
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
            .editing()
            .is_some_and(|e| e.worktree == worktree && e.path == path)
        {
            self.lsp_paint_diagnostics(cx);
        }
        cx.notify();
    }

    /// Puts the open file's diagnostics into the editor's own set.
    fn lsp_paint_diagnostics(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(editing) = self.editing() else {
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
        let Some(editing) = self.editing() else {
            return;
        };
        editing.input.update(cx, |state, cx| {
            if let Some(set) = state.diagnostics_mut() {
                set.clear();
                cx.notify();
            }
        });
    }

    /// Follows the definition of what is under the caret.
    ///
    /// **It does not go through gpui-component's `GoToDefinition`.** That action
    /// reads `hover_definition`, which is only filled by Cmd-hovering the
    /// symbol: from the keyboard, with no pointer anywhere near, it has nothing
    /// to act on and does nothing at all — silently, which is how `gd` looked
    /// like a key that was not bound. The request is ours, asked at the caret.
    ///
    /// The **first** location is taken. A server may answer several — an
    /// interface and its implementations — and choosing between them wants a
    /// list to pick from, which is a gesture this editor does not have; landing
    /// on the first is what every editor does before it grows one.
    ///
    /// **And when nobody answers, the project is searched instead.** No server
    /// running, none that serves this language, one that does not know the
    /// symbol: all three are the same thing to the hand, a key pressed on a
    /// name one wants to go to. `git grep` is a poorer answer than a server's
    /// — it cannot tell a declaration from a use — but it is an answer, and it
    /// is what this window had before the server existed. See
    /// `search_for_definition`.
    pub(super) fn goto_definition(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(editing) = self.editing() else {
            return;
        };
        let (worktree, path) = (editing.worktree.clone(), editing.path.clone());
        let state = editing.input.read(cx);
        let offset = state.selected_range().start;
        let position = state.text().offset_to_position(offset);
        // Read here, while the editor is at hand: the fallback runs from inside
        // the answer's closure, by which time the caret may have moved.
        let symbol = crate::ui::search::symbol_at(&state.text().to_string(), offset);
        if !self.lsp_enabled(&worktree) || !self.lsp.contains_key(&worktree) {
            self.fallback_to_search(symbol, window, cx);
            return;
        }
        let params = json!({
            "textDocument": {"uri": crate::lsp::uri::of(&full(&worktree, &path))},
            "position": {"line": position.line, "character": position.character},
        });
        let receiver = self.lsp_request(&worktree, "textDocument/definition", params);
        cx.spawn_in(window, async move |this, cx| {
            let answer = receiver.recv().await;
            let _ = this.update_in(cx, |this, window, cx| {
                let target = match answer {
                    Ok(Ok(payload)) => serde_json::from_str::<GotoDefinitionResponse>(&payload)
                        .ok()
                        .map(links)
                        .unwrap_or_default()
                        .into_iter()
                        .next(),
                    _ => None,
                };
                let Some(link) = target else {
                    // A server that says nothing is not the end of the gesture:
                    // it says nothing about a `@method` docblock, about a name
                    // in a Blade view, about anything it has not indexed yet.
                    this.fallback_to_search(symbol, window, cx);
                    return;
                };
                let Some(path) = crate::lsp::uri::path(link.target_uri.as_str()) else {
                    this.fallback_to_search(symbol, window, cx);
                    return;
                };
                // A definition in `vendor/` is a file of the worktree like any
                // other, and it has to be called by the same name as the
                // explorer calls it — otherwise the same file open twice under
                // two names, and a save that writes to neither.
                let path = local(&worktree, &path);
                let start = link.target_selection_range.start;
                this.jump_to(
                    path,
                    crate::ui::explorer::Landing::Position {
                        line: start.line,
                        character: start.character,
                    },
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    /// The fallback of a jump that found no definition: look the symbol up in
    /// the project.
    ///
    /// **A symbol and not a selection.** What is under the caret is a word, and
    /// `search::symbol_at` is what says which — the caret standing after a name
    /// counts, a number does not.
    fn fallback_to_search(
        &mut self,
        symbol: Option<String>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(symbol) = symbol else {
            // Nothing to look for: said out loud, a key that answers nothing
            // being a key one presses again.
            self.announce(tr!("editor-no-definition"), cx);
            return;
        };
        self.search_for_definition(&symbol, window, cx);
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
#[derive(Clone)]
struct Provider {
    app: WeakEntity<ClaudhubApp>,
    worktree: PathBuf,
    path: PathBuf,
    triggers: Vec<String>,
    semantic: Option<Semantic>,
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

    /// A copy for the asynchronous leg: `ask` needs the weak handle and the
    /// paths, and the provider itself lives behind an `Rc` the future cannot
    /// hold.
    fn clone_for_command(&self) -> Self {
        self.clone()
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

impl CodeActionProvider for Provider {
    /// One provider, one id. Theirs is a `Vec` because a document may have
    /// several — a linter's fixes beside a server's — and the id is how an
    /// action found its way back to the one that offered it.
    fn id(&self) -> SharedString {
        SharedString::from("claudhub-lsp")
    }

    fn code_actions(
        &self,
        state: Entity<EditorState>,
        range: std::ops::Range<usize>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<CodeAction>>> {
        let text = state.read(cx).text().clone();
        let start = text.offset_to_position(range.start);
        let end = text.offset_to_position(range.end);
        // **The diagnostics of the range travel with the request**, and they
        // are not a courtesy: a quick fix is offered *for* a diagnostic, and a
        // server given none has nothing to fix — the list comes back empty and
        // reads as "this server has no code actions".
        let app = self.app.clone();
        let worktree = self.worktree.clone();
        // The set is keyed the way the rest of the window names files; ours is
        // the absolute path the protocol wants.
        let path = local(&worktree, &self.path);
        let diagnostics = app
            .update(cx, |this, _| {
                this.lsp
                    .get(&worktree)
                    .and_then(|session| session.diagnostics.get(&path))
                    .map(|found| {
                        found
                            .iter()
                            .filter(|d| {
                                d.range.end.line >= start.line && d.range.start.line <= end.line
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let params = json!({
            "textDocument": self.document(),
            "range": {"start": start, "end": end},
            "context": {"diagnostics": diagnostics},
        });
        let task = self.ask::<CodeActionResponse>("textDocument/codeAction", params, cx);
        cx.spawn(async move |_cx| {
            Ok(task
                .await?
                .unwrap_or_default()
                .into_iter()
                .map(|item| match item {
                    CodeActionOrCommand::CodeAction(action) => action,
                    // A bare `Command` is the older shape of the same thing:
                    // dropping it would hide half of what some servers offer.
                    CodeActionOrCommand::Command(command) => CodeAction {
                        title: command.title.clone(),
                        command: Some(command),
                        ..Default::default()
                    },
                })
                .collect())
        })
    }

    /// Carries out an action, in the three shapes one arrives in.
    ///
    /// **Resolved first when it has to be.** A server is allowed to answer a
    /// list of titles and compute the edit only for the one that is chosen —
    /// that is what `data` without `edit` means, and doing nothing there is how
    /// a quick fix looks broken.
    ///
    /// Then the edit, if it carries one, and then its command, whose own effect
    /// usually comes back as a `workspace/applyEdit` — the round trip through
    /// `Evt::LspApplyEdit`.
    fn perform_code_action(
        &self,
        state: Entity<EditorState>,
        action: CodeAction,
        _push_to_history: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let path = self.path.clone();
        let needs_resolve = action.edit.is_none() && action.data.is_some();
        let resolve = needs_resolve.then(|| {
            self.ask::<CodeAction>(
                "codeAction/resolve",
                serde_json::to_value(&action).unwrap_or(Value::Null),
                cx,
            )
        });
        let command = self.clone_for_command();
        window.spawn(cx, async move |cx| {
            let action = match resolve {
                Some(task) => task.await?.unwrap_or(action),
                None => action,
            };
            if let Some(edit) = action.edit.clone() {
                let state = state.clone();
                let path = path.clone();
                cx.update(|window, cx| apply_to(&state, &path, &edit, window, cx))?;
            }
            if let Some(invocation) = action.command.clone() {
                let params = json!({
                    "command": invocation.command,
                    "arguments": invocation.arguments.unwrap_or_default(),
                });
                let task = cx.update(|_window, cx| {
                    command.ask::<Value>("workspace/executeCommand", params, cx)
                })?;
                task.await?;
            }
            Ok(())
        })
    }
}

/// Applies to the open document what a workspace edit says about it, and says
/// whether it applied all of it.
///
/// **Only the open document.** An edit naming another file — what a rename
/// produces — is refused rather than written: every write in Claudhub carries
/// the digest of what was read, so that an agent's work is never overwritten,
/// and applying blind edits to files nobody has open would be the one place
/// that rule is broken. The server is told, and it reports it.
///
/// **Back to front.** The edits' positions all refer to the text as it is now;
/// applied in reading order, the first one moves everything the second one
/// points at. Sorting them descending is what makes each in turn land on a
/// position that is still true.
pub(super) fn apply_to(
    state: &Entity<EditorState>,
    path: &Path,
    edit: &WorkspaceEdit,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let ours = crate::lsp::uri::of(path);
    let mut mine: Vec<TextEdit> = Vec::new();
    let mut elsewhere = false;
    if let Some(changes) = &edit.changes {
        for (uri, edits) in changes {
            if uri.as_str() == ours {
                mine.extend(edits.iter().cloned());
            } else {
                elsewhere = true;
            }
        }
    }
    if let Some(lsp_types::DocumentChanges::Edits(documents)) = &edit.document_changes {
        for document in documents {
            if document.text_document.uri.as_str() == ours {
                mine.extend(document.edits.iter().map(|edit| match edit {
                    lsp_types::OneOf::Left(edit) => edit.clone(),
                    // An annotated edit carries the same text and a label for a
                    // confirmation dialog we do not show.
                    lsp_types::OneOf::Right(annotated) => annotated.text_edit.clone(),
                }));
            } else {
                elsewhere = true;
            }
        }
    }
    // Creating, renaming and deleting files (`DocumentChanges::Operations`) is
    // the same refusal for the same reason.
    if matches!(
        edit.document_changes,
        Some(lsp_types::DocumentChanges::Operations(_))
    ) {
        elsewhere = true;
    }
    if !mine.is_empty() {
        mine.sort_by_key(|edit| std::cmp::Reverse(edit.range.start));
        state.update(cx, |state, cx| {
            state.apply_lsp_edits(&mine, window, cx);
            cx.notify();
        });
    }
    !elsewhere
}

impl DocumentRangeSemanticTokensProvider for Provider {
    /// The server's legend, in its order, spoken in our themes' vocabulary.
    ///
    /// The order is not a detail: a token carries an **index** into this list,
    /// and one entry out of place recolours a whole file wrongly and in
    /// silence.
    fn legend(&self) -> SemanticTokensLegend {
        SemanticTokensLegend {
            token_types: self
                .semantic
                .as_ref()
                .map(|semantic| {
                    semantic
                        .names
                        .iter()
                        .map(|name| SemanticTokenType::new(name))
                        .collect()
                })
                .unwrap_or_default(),
            // Accepted by the editor and not mapped to anything: a modifier
            // would want a style of its own — `deprecated` struck through — and
            // there is nothing to hang one on yet.
            token_modifiers: Vec::new(),
        }
    }

    fn semantic_tokens(
        &self,
        text: &Rope,
        range: std::ops::Range<usize>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<SemanticTokens>> {
        // The editor asks for the whole document — it caches and windows the
        // result at paint time, so scrolling costs nothing. We ask the server
        // the way it says it can answer: PHPantom offers `full` and refuses
        // `range`, and asking for the one it refuses gets an error rather than
        // colours.
        let full = self.semantic.as_ref().is_some_and(|s| s.full);
        let (method, params) = if full {
            (
                "textDocument/semanticTokens/full",
                json!({"textDocument": self.document()}),
            )
        } else {
            (
                "textDocument/semanticTokens/range",
                json!({
                    "textDocument": self.document(),
                    "range": {
                        "start": self.position(text, range.start),
                        "end": self.position(text, range.end),
                    },
                }),
            )
        };
        let task = self.ask::<SemanticTokens>(method, params, cx);
        cx.spawn(async move |_cx| Ok(task.await?.unwrap_or_default()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::highlighter::HighlightTheme;

    /// The protocol wants an absolute path and the window keeps a relative one,
    /// and the two must be the same file in both directions — a URI built from
    /// a relative path names a *host*, and a diagnostic filed under a path the
    /// editor does not use is a diagnostic nobody sees.
    #[test]
    fn a_document_is_the_same_file_on_both_sides() {
        let worktree = Path::new("/home/me/site");
        let path = Path::new("app/Models/User.php");
        let absolute = full(worktree, path);
        assert_eq!(absolute, Path::new("/home/me/site/app/Models/User.php"));
        assert_eq!(
            crate::lsp::uri::of(&absolute),
            "file:///home/me/site/app/Models/User.php"
        );
        assert_eq!(local(worktree, &absolute), path);
        // A definition in `vendor/` is a file of the worktree like any other.
        let vendor = Path::new("/home/me/site/vendor/laravel/framework/src/Foo.php");
        assert_eq!(
            local(worktree, vendor),
            Path::new("vendor/laravel/framework/src/Foo.php")
        );
        // What is outside is left alone: a runtime's sources belong to no tree,
        // and half a path would name nothing.
        let outside = Path::new("/usr/lib/php/Bar.php");
        assert_eq!(local(worktree, outside), outside);
    }

    /// What PHPantom 0.10 announces, in its order.
    const PHPANTOM: [&str; 15] = [
        "namespace",
        "class",
        "interface",
        "enum",
        "type",
        "typeParameter",
        "parameter",
        "variable",
        "property",
        "function",
        "method",
        "decorator",
        "enumMember",
        "keyword",
        "comment",
    ];

    /// A style name a theme does not know resolves to nothing, and a token with
    /// no style changes nothing on screen. That is the only way this feature
    /// fails: in silence, and looking exactly like a server that sent nothing.
    ///
    /// The rule checked here is the resolver's own: an exact name, or the
    /// segment before the first dot — which is what makes `function.method`
    /// safe on a theme that only knows `function`.
    #[test]
    fn every_token_type_lands_on_a_colour_our_themes_define() {
        for file in crate::ui::theme::BundledThemes::iter() {
            let content = crate::ui::theme::BundledThemes::get(&file).expect("embedded");
            let value: Value = serde_json::from_slice(&content.data).expect("JSON");
            for theme in value["themes"].as_array().expect("themes") {
                let syntax = theme["highlight"]["syntax"]
                    .as_object()
                    .expect("a style table");
                for token_type in PHPANTOM {
                    let name = theme_name(token_type);
                    if name.is_empty() {
                        continue; // deliberately left to the grammar
                    }
                    let known = syntax.contains_key(name)
                        || name
                            .split_once('.')
                            .is_some_and(|(prefix, _)| syntax.contains_key(prefix));
                    assert!(
                        known,
                        "{file}: `{token_type}` maps to `{name}`, which the theme does not define"
                    );
                }
            }
        }
    }

    /// And the same against the two themes gpui-component ships, which a user
    /// falls back to when the bundled ones are not on disk yet.
    #[test]
    fn every_token_type_lands_on_a_colour_in_the_default_themes() {
        use gpui_component::input::HighlightStyleResolver as _;
        for theme in [
            HighlightTheme::default_dark(),
            HighlightTheme::default_light(),
        ] {
            for token_type in PHPANTOM {
                let name = theme_name(token_type);
                if name.is_empty() {
                    continue;
                }
                assert!(
                    theme.style(name).is_some(),
                    "`{token_type}` maps to `{name}`, unknown in \"{}\"",
                    theme.name
                );
            }
        }
    }

    /// The order is the server's, and it is what the indices refer to: one
    /// entry out of place recolours a file wrongly and in silence.
    #[test]
    fn the_legend_keeps_the_servers_order() {
        let capabilities = Capabilities::read(
            &json!({
                "semanticTokensProvider": {
                    "legend": {"tokenTypes": ["parameter", "class", "comment"]},
                    "full": true,
                    "range": false,
                },
            })
            .to_string(),
        );
        let semantic = capabilities.semantic.expect("a legend");
        assert_eq!(semantic.names, ["variable.special", "type", "comment"]);
        assert!(semantic.full);
    }

    /// A server with no semantic tokens must not get a provider: the editor
    /// would ask on every change and be answered with an error.
    #[test]
    fn a_server_without_the_capability_offers_no_legend() {
        assert!(Capabilities::read("{}").semantic.is_none());
        // Declared but only by range: the provider is posted, and it is the
        // request that changes.
        let only_range = Capabilities::read(
            &json!({"semanticTokensProvider": {"legend": {"tokenTypes": []}, "range": true}})
                .to_string(),
        );
        assert!(!only_range.semantic.expect("declared").full);
    }
}
