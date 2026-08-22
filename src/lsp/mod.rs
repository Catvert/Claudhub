//! The language-server client: one process per worktree, and what it takes to
//! keep talking to it.
//!
//! **Why it lives in the core and not in `src/ui/`.** It launches a process,
//! reads pipes and holds documents — the very things the view is not allowed to
//! do — and, more to the point, it has to run *where the files are*: inside the
//! WSL distribution when the interface is a Windows `.exe`. It is the workers'
//! side of the wire, like git.
//!
//! **Why it is not a queue.** A language server is not a command that answers
//! and is done: it lives for hours, holds state — which documents are open and
//! at which version — and pushes messages nobody asked for. Two things follow.
//! It gets a lane of its own, next to the file watcher's, so a completion is
//! never behind a `composer install`; and, decisively, **order matters**: a
//! `didChange` must reach the server before the completion that depends on it,
//! which several workers sharing one channel cannot promise.
//!
//! **One owner per session.** The child, the pending requests and the open
//! documents belong to one thread, which receives the interface's orders and
//! the server's messages through the same channel. No mutex, and no way for a
//! request to be written between the halves of another.
//!
//! What crosses the wire towards the view is **raw JSON**, never typed values:
//! `lsp-types` belongs to the `ui` feature — a core module pulling it in breaks
//! the server build — and postcard, which is positional and not
//! self-describing, cannot carry a `serde_json::Value` back. The view types
//! what it reads; the two ends of the wire never have to agree on a version of
//! the LSP crate.

pub mod frame;
pub mod sync;
pub mod uri;

use std::collections::{BTreeMap, HashMap};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::runtime::protocol::{Evt, WorktreeId};

/// How long a request may stay unanswered before the view is told it failed.
///
/// A `Task` that never resolves is worse than an error: the completion popover
/// waits for ever, and the pending table grows for the length of the session.
/// Fifteen seconds is far past anything interactive and still short of the
/// patience of someone who typed a dot.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// A handshake that has not come back by then is a server that will not start.
/// Generous on purpose: PHPantom reads a Composer classmap before it answers,
/// and a cold NFS-mounted `vendor/` is slow the first time.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

/// How often the loop wakes with nothing to do, to expire what is overdue.
const TICK: Duration = Duration::from_millis(250);

/// A language server as it is declared — in the settings, or in the project's
/// `wt.toml`, which is the same shape on purpose.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Server {
    /// What names it in the interface, and in the journal.
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Extensions served, without the leading dot: `php`, `blade.php`.
    pub extensions: Vec<String>,
    /// The `languageId` announced for its files. Empty falls back to the
    /// extension that matched, which is right for `php` and wrong for nothing
    /// we ship.
    pub language_id: String,
}

impl Server {
    /// The length of the extension that matches this file, or `None`.
    ///
    /// The length is what lets the caller settle a tie: `page.blade.php`
    /// matches `php` and `blade.php` at once, and the longer one is the one
    /// that says something.
    pub fn matches(&self, path: &Path) -> Option<usize> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        self.extensions
            .iter()
            .filter(|ext| {
                let ext = ext.trim_start_matches('.').to_ascii_lowercase();
                !ext.is_empty() && name.ends_with(&format!(".{ext}"))
            })
            .map(|ext| ext.trim_start_matches('.').len())
            .max()
    }

    /// The `languageId` to announce for that file.
    pub fn language_for(&self, path: &Path) -> String {
        if !self.language_id.is_empty() {
            return self.language_id.clone();
        }
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("plaintext")
            .to_ascii_lowercase()
    }

    pub fn is_runnable(&self) -> bool {
        !self.command.trim().is_empty()
    }
}

/// The server that serves a file, the longest matching extension winning.
///
/// Ties go to the first declared, which is the order the settings show — a
/// choice the reader can see, rather than one a hash map made.
pub fn pick<'a>(servers: &'a [Server], path: &Path) -> Option<&'a Server> {
    servers
        .iter()
        .filter(|s| s.is_runnable())
        .filter_map(|s| s.matches(path).map(|len| (len, s)))
        .max_by_key(|(len, _)| *len)
        .map(|(_, s)| s)
}

/// What the interface asks of a session.
#[derive(Debug, Clone)]
pub enum Ask {
    Open {
        path: PathBuf,
        language_id: String,
        text: String,
    },
    Change {
        path: PathBuf,
        text: String,
    },
    Close {
        path: PathBuf,
    },
    Save {
        path: PathBuf,
    },
    /// One LSP request, its `params` already serialised by the view.
    Request {
        id: u64,
        method: String,
        params: String,
    },
    Cancel {
        id: u64,
    },
}

/// The sessions, one per worktree.
///
/// The lane the runtime hands the `Cmd::Lsp*` to directly, as it does the
/// watcher's: putting them in a queue would let a diff delay a keystroke's
/// `didChange`, and nothing here talks to git.
pub struct Host {
    sessions: Mutex<HashMap<WorktreeId, Sender<Message>>>,
    events: async_channel::Sender<Evt>,
}

impl Host {
    pub fn new(events: async_channel::Sender<Evt>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            events,
        }
    }

    /// Starts a session, replacing any that was running for that worktree —
    /// the settings may have changed the command, and two servers on one
    /// worktree would answer the same question twice.
    pub fn start(&self, worktree: WorktreeId, server: Server) {
        self.stop(worktree.clone());
        if !server.is_runnable() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let events = self.events.clone();
        let name = format!("claudhub-lsp-{}", server.name);
        let spawned = std::thread::Builder::new().name(name).spawn({
            let worktree = worktree.clone();
            let tx = tx.clone();
            move || session(worktree, server, rx, tx, events)
        });
        match spawned {
            Ok(_) => {
                self.sessions.lock().unwrap().insert(worktree, tx);
            }
            Err(e) => {
                log::warn!("no thread for the language server: {e:#}");
                let _ = self.events.try_send(Evt::LspStopped {
                    worktree,
                    reason: Some(e.to_string()),
                });
            }
        }
    }

    /// Ends a session. Dropping the sender would do it too, but saying so lets
    /// the thread kill the child before the channel's error reaches it.
    pub fn stop(&self, worktree: WorktreeId) {
        if let Some(session) = self.sessions.lock().unwrap().remove(&worktree) {
            let _ = session.send(Message::Stop);
        }
    }

    /// Hands an order to a session. A worktree with no session drops it — the
    /// same behaviour as a command issued before the remote server is up, and
    /// for the same reason: the view re-asks by itself.
    pub fn ask(&self, worktree: &Path, ask: Ask) {
        let sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.get(worktree) else {
            log::debug!("no language server for {}, dropped", worktree.display());
            return;
        };
        let _ = session.send(Message::Ask(ask));
    }
}

/// What the session thread receives: the interface's orders and the server's
/// messages, on one channel, so one owner sees them in the order they happened.
enum Message {
    Ask(Ask),
    Incoming(String),
    /// The server's stdout closed: it exited, or it was killed.
    Ended,
    Stop,
}

/// What an answer belongs to.
enum Origin {
    Handshake,
    /// A request the view made, and which is waiting on it.
    View(u64),
}

struct Pending {
    origin: Origin,
    deadline: Instant,
}

/// How a server wants its changes, read from `textDocumentSync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncKind {
    None,
    Full,
    Incremental,
}

fn session(
    worktree: WorktreeId,
    server: Server,
    inbox: Receiver<Message>,
    self_tx: Sender<Message>,
    events: async_channel::Sender<Evt>,
) {
    let mut child = match launch(&worktree, &server) {
        Ok(child) => child,
        Err(e) => {
            log::warn!("language server {}: {e:#}", server.name);
            let _ = events.try_send(Evt::LspStopped {
                worktree,
                reason: Some(format!("{e:#}")),
            });
            return;
        }
    };
    let mut stdin = child.stdin.take().expect("stdin was piped");
    read_pipes(&mut child, &server, self_tx);

    let mut state = Session {
        worktree: worktree.clone(),
        server,
        events: events.clone(),
        next_id: 1,
        pending: HashMap::new(),
        documents: HashMap::new(),
        sync: SyncKind::Full,
        ready: false,
        queued: Vec::new(),
    };

    let reason = state.run(&mut stdin, inbox);

    // The child dies with the session, always: a language server on a
    // twenty-thousand-file project holds hundreds of megabytes, and one left
    // behind by a worktree nobody looks at is a leak nothing else would catch.
    let _ = child.kill();
    let _ = child.wait();
    let _ = events.try_send(Evt::LspStopped { worktree, reason });
}

fn launch(worktree: &Path, server: &Server) -> anyhow::Result<Child> {
    let mut command = Command::new(&server.command);
    command
        .args(&server.args)
        .envs(&server.env)
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(command.spawn()?)
}

/// The two reader threads: frames on stdout, and the journal on stderr.
///
/// stdout belongs to the protocol — a server writing anything else there
/// corrupts the stream — and stderr is where servers say what they are doing.
/// Both are pumped, because a full pipe blocks the writer, and a server blocked
/// on its own stderr answers nothing.
fn read_pipes(child: &mut Child, server: &Server, self_tx: Sender<Message>) {
    if let Some(stdout) = child.stdout.take() {
        let name = format!("claudhub-lsp-{}-in", server.name);
        let _ = std::thread::Builder::new().name(name).spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match frame::read(&mut reader) {
                    Ok(Some(payload)) => {
                        if self_tx.send(Message::Incoming(payload)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        log::warn!("language server frame: {e:#}");
                        break;
                    }
                }
            }
            let _ = self_tx.send(Message::Ended);
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let label = server.name.clone();
        let name = format!("claudhub-lsp-{}-err", server.name);
        let _ = std::thread::Builder::new().name(name).spawn(move || {
            use std::io::BufRead;
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log::debug!(target: "lsp", "{label}: {line}");
            }
        });
    }
}

struct Session {
    worktree: WorktreeId,
    server: Server,
    events: async_channel::Sender<Evt>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    documents: HashMap<PathBuf, sync::Document>,
    sync: SyncKind,
    ready: bool,
    /// What the view asked before the handshake came back. Replayed in order:
    /// a `didOpen` issued the moment the button was pressed must not be lost,
    /// and it must not arrive after the completion that needs it either.
    queued: Vec<Ask>,
}

impl Session {
    /// The loop. Returns why it ended, for the event that says so.
    fn run(&mut self, stdin: &mut impl std::io::Write, inbox: Receiver<Message>) -> Option<String> {
        if let Err(e) = self.handshake(stdin) {
            return Some(format!("{e:#}"));
        }
        loop {
            match inbox.recv_timeout(TICK) {
                Ok(Message::Ask(ask)) => {
                    if self.ready {
                        self.perform(stdin, ask);
                    } else {
                        self.queued.push(ask);
                    }
                }
                Ok(Message::Incoming(payload)) => self.incoming(stdin, &payload),
                Ok(Message::Ended) => return Some("the server exited".into()),
                Ok(Message::Stop) => return None,
                Err(RecvTimeoutError::Timeout) => {}
                // The host dropped the sender: the window is gone.
                Err(RecvTimeoutError::Disconnected) => return None,
            }
            if let Some(reason) = self.expire() {
                return Some(reason);
            }
        }
    }

    fn handshake(&mut self, stdin: &mut impl std::io::Write) -> anyhow::Result<()> {
        let id = self.request(
            stdin,
            "initialize",
            initialize_params(&self.worktree),
            Origin::Handshake,
            HANDSHAKE_TIMEOUT,
        )?;
        log::info!(
            "language server {} starting in {} (request {id})",
            self.server.name,
            self.worktree.display()
        );
        Ok(())
    }

    /// Sends a request and remembers what is waiting for it.
    fn request(
        &mut self,
        stdin: &mut impl std::io::Write,
        method: &str,
        params: Value,
        origin: Origin,
        timeout: Duration,
    ) -> anyhow::Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(
            id,
            Pending {
                origin,
                deadline: Instant::now() + timeout,
            },
        );
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        frame::write(stdin, &message.to_string())?;
        Ok(id)
    }

    fn notify(&self, stdin: &mut impl std::io::Write, method: &str, params: Value) {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        if let Err(e) = frame::write(stdin, &message.to_string()) {
            log::warn!("writing {method} to the language server: {e:#}");
        }
    }

    fn emit(&self, event: Evt) {
        if let Err(e) = self.events.try_send(event) {
            log::debug!("language server event dropped: {e}");
        }
    }

    /// Carries out one of the view's orders.
    fn perform(&mut self, stdin: &mut impl std::io::Write, ask: Ask) {
        match ask {
            Ask::Open {
                path,
                language_id,
                text,
            } => {
                let document = sync::Document::new(text);
                self.notify(
                    stdin,
                    "textDocument/didOpen",
                    json!({"textDocument": {
                        "uri": uri::of(&path),
                        "languageId": language_id,
                        "version": document.version,
                        "text": document.text,
                    }}),
                );
                self.documents.insert(path, document);
            }
            Ask::Change { path, text } => {
                if self.sync == SyncKind::None {
                    return;
                }
                let Some(document) = self.documents.get_mut(&path) else {
                    // A change for a document the server does not have is a
                    // change it would refuse: the view re-opens on its own.
                    log::debug!("change for a closed document: {}", path.display());
                    return;
                };
                let Some(edit) = document.edit(text) else {
                    return;
                };
                let version = document.version;
                let changes = match self.sync {
                    SyncKind::Incremental => json!([{
                        "range": {
                            "start": {"line": edit.start.line, "character": edit.start.character},
                            "end": {"line": edit.end.line, "character": edit.end.character},
                        },
                        "text": edit.text,
                    }]),
                    // `Full` and, defensively, `None`, which returned above.
                    _ => json!([{"text": self.documents[&path].text}]),
                };
                self.notify(
                    stdin,
                    "textDocument/didChange",
                    json!({
                        "textDocument": {"uri": uri::of(&path), "version": version},
                        "contentChanges": changes,
                    }),
                );
            }
            Ask::Close { path } => {
                if self.documents.remove(&path).is_some() {
                    self.notify(
                        stdin,
                        "textDocument/didClose",
                        json!({"textDocument": {"uri": uri::of(&path)}}),
                    );
                }
            }
            Ask::Save { path } => {
                if let Some(document) = self.documents.get(&path) {
                    self.notify(
                        stdin,
                        "textDocument/didSave",
                        json!({
                            "textDocument": {"uri": uri::of(&path)},
                            "text": document.text,
                        }),
                    );
                }
            }
            Ask::Request { id, method, params } => {
                let params: Value = serde_json::from_str(&params).unwrap_or(Value::Null);
                if let Err(e) = self.request(
                    stdin,
                    &method.clone(),
                    params,
                    Origin::View(id),
                    REQUEST_TIMEOUT,
                ) {
                    self.emit(Evt::LspAnswer {
                        worktree: self.worktree.clone(),
                        id,
                        result: Err(format!("{e:#}")),
                    });
                }
            }
            Ask::Cancel { id } => {
                // The server's id, not ours: find the one that carries it.
                let server_id = self.pending.iter().find_map(|(server_id, pending)| {
                    matches!(pending.origin, Origin::View(waiting) if waiting == id)
                        .then_some(*server_id)
                });
                if let Some(server_id) = server_id {
                    self.pending.remove(&server_id);
                    self.notify(stdin, "$/cancelRequest", json!({"id": server_id}));
                }
            }
        }
    }

    /// One message from the server: an answer, a notification, or a request of
    /// its own.
    fn incoming(&mut self, stdin: &mut impl std::io::Write, payload: &str) {
        let Ok(message) = serde_json::from_str::<Value>(payload) else {
            log::warn!("unreadable message from the language server");
            return;
        };
        let id = message.get("id").and_then(Value::as_u64);
        let method = message.get("method").and_then(Value::as_str);
        match (id, method) {
            // An answer to one of ours.
            (Some(id), None) => self.answer(stdin, id, &message),
            // A request from the server: it waits for an answer, and a server
            // left waiting stops serving.
            (Some(id), Some(method)) => self.serve(stdin, id, method, &message),
            (None, Some(method)) => self.notification(method, &message),
            (None, None) => log::debug!("a message that is neither"),
        }
    }

    fn answer(&mut self, stdin: &mut impl std::io::Write, id: u64, message: &Value) {
        let Some(pending) = self.pending.remove(&id) else {
            // A cancelled request whose answer arrived anyway.
            return;
        };
        let error = message.get("error").map(|e| {
            e.get("message")
                .and_then(Value::as_str)
                .unwrap_or("the language server refused")
                .to_string()
        });
        match pending.origin {
            Origin::Handshake => {
                if let Some(error) = error {
                    log::warn!("language server {}: {error}", self.server.name);
                    return;
                }
                let capabilities = message
                    .get("result")
                    .and_then(|r| r.get("capabilities"))
                    .cloned()
                    .unwrap_or(Value::Null);
                self.sync = sync_kind(&capabilities);
                self.notify(stdin, "initialized", json!({}));
                self.ready = true;
                log::info!(
                    "language server {} ready ({:?} sync)",
                    self.server.name,
                    self.sync
                );
                self.emit(Evt::LspReady {
                    worktree: self.worktree.clone(),
                    name: self.server.name.clone(),
                    capabilities: capabilities.to_string(),
                });
                for ask in std::mem::take(&mut self.queued) {
                    self.perform(stdin, ask);
                }
            }
            Origin::View(view_id) => {
                let result = match error {
                    Some(error) => Err(error),
                    None => Ok(message.get("result").unwrap_or(&Value::Null).to_string()),
                };
                self.emit(Evt::LspAnswer {
                    worktree: self.worktree.clone(),
                    id: view_id,
                    result,
                });
            }
        }
    }

    /// Answers what the server asks of us.
    ///
    /// Only three of them occur, and each has an answer that costs nothing;
    /// everything else gets the error the specification defines. Saying nothing
    /// is the one thing that must not happen: a server that registers a
    /// capability and waits for the acknowledgement stops there.
    fn serve(&self, stdin: &mut impl std::io::Write, id: u64, method: &str, message: &Value) {
        let result = match method {
            "client/registerCapability" | "client/unregisterCapability" => Value::Null,
            "window/workDoneProgress/create" => Value::Null,
            // We hold no per-server settings: one `null` per item asked, which
            // is how a client says "take your defaults".
            "workspace/configuration" => {
                let items = message
                    .get("params")
                    .and_then(|p| p.get("items"))
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0);
                Value::Array(vec![Value::Null; items])
            }
            other => {
                log::debug!("language server asked for {other}, which we do not do");
                let error = json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": "not supported"},
                });
                if let Err(e) = frame::write(stdin, &error.to_string()) {
                    log::warn!("answering the language server: {e:#}");
                }
                return;
            }
        };
        let answer = json!({"jsonrpc": "2.0", "id": id, "result": result});
        if let Err(e) = frame::write(stdin, &answer.to_string()) {
            log::warn!("answering the language server: {e:#}");
        }
    }

    fn notification(&self, method: &str, message: &Value) {
        let params = message.get("params");
        match method {
            "textDocument/publishDiagnostics" => {
                let Some(params) = params else { return };
                let Some(path) = params
                    .get("uri")
                    .and_then(Value::as_str)
                    .and_then(uri::path)
                else {
                    return;
                };
                let diagnostics = params.get("diagnostics").cloned().unwrap_or(json!([]));
                self.emit(Evt::LspDiagnostics {
                    worktree: self.worktree.clone(),
                    path,
                    diagnostics: diagnostics.to_string(),
                });
            }
            // What the server is busy with. It is the answer to "why is the
            // completion thin for the first ten seconds": PHPantom builds its
            // index in layers, and says so here.
            "$/progress" => {
                let value = params.and_then(|p| p.get("value"));
                let kind = value.and_then(|v| v.get("kind")).and_then(Value::as_str);
                let message = match kind {
                    Some("end") => None,
                    _ => value.and_then(progress_text),
                };
                self.emit(Evt::LspBusy {
                    worktree: self.worktree.clone(),
                    message,
                });
            }
            "window/logMessage" | "window/showMessage" => {
                if let Some(text) = params
                    .and_then(|p| p.get("message"))
                    .and_then(Value::as_str)
                {
                    log::debug!(target: "lsp", "{}: {text}", self.server.name);
                }
            }
            other => log::debug!(target: "lsp", "notification {other}"),
        }
    }

    /// Fails what has waited too long, and says so once for the handshake — a
    /// server that never answers `initialize` is a server that will not serve.
    fn expire(&mut self) -> Option<String> {
        let now = Instant::now();
        let overdue: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in overdue {
            match self.pending.remove(&id).map(|p| p.origin) {
                Some(Origin::Handshake) => return Some("no answer to initialize".into()),
                Some(Origin::View(view_id)) => self.emit(Evt::LspAnswer {
                    worktree: self.worktree.clone(),
                    id: view_id,
                    result: Err("the language server did not answer".into()),
                }),
                None => {}
            }
        }
        None
    }
}

/// A progress notification's most useful line: its title, then its message.
fn progress_text(value: &Value) -> Option<String> {
    let title = value.get("title").and_then(Value::as_str);
    let message = value.get("message").and_then(Value::as_str);
    match (title, message) {
        (Some(title), Some(message)) => Some(format!("{title} — {message}")),
        (Some(text), None) | (None, Some(text)) => Some(text.to_string()),
        (None, None) => None,
    }
}

/// How the server wants its changes. The field is either a number or an object,
/// both shapes being current, and its absence means no synchronisation at all.
fn sync_kind(capabilities: &Value) -> SyncKind {
    let field = capabilities.get("textDocumentSync");
    let number = match field {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::Object(_)) => field
            .and_then(|f| f.get("change"))
            .and_then(Value::as_u64)
            .or(Some(0)),
        _ => None,
    };
    match number {
        Some(1) => SyncKind::Full,
        Some(2) => SyncKind::Incremental,
        _ => SyncKind::None,
    }
}

/// What we tell the server we can do.
///
/// It decides what it sends back: a server whose client claims no
/// `linkSupport` answers definitions as plain locations, and one told nothing
/// of `contextSupport` never says which character triggered a completion. The
/// list mirrors what the editor in `gpui-component` actually renders — claiming
/// more would only invite answers nothing displays.
fn initialize_params(worktree: &Path) -> Value {
    let root = uri::of(worktree);
    json!({
        "processId": std::process::id(),
        "clientInfo": {"name": "Claudhub", "version": env!("CARGO_PKG_VERSION")},
        "rootUri": root,
        "workspaceFolders": [{
            "uri": root,
            "name": worktree.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        }],
        "capabilities": {
            "workspace": {"workspaceFolders": true, "configuration": true},
            "textDocument": {
                "synchronization": {"dynamicRegistration": false, "didSave": true},
                "completion": {
                    "contextSupport": true,
                    "completionItem": {
                        "snippetSupport": false,
                        "documentationFormat": ["markdown", "plaintext"],
                    },
                },
                "hover": {"contentFormat": ["markdown", "plaintext"]},
                "definition": {"linkSupport": true},
                "codeAction": {
                    "codeActionLiteralSupport": {
                        "codeActionKind": {
                            "valueSet": ["quickfix", "refactor", "source"],
                        },
                    },
                },
                "semanticTokens": {
                    "requests": {"range": true, "full": false},
                    "tokenTypes": [],
                    "tokenModifiers": [],
                    "formats": ["relative"],
                },
                "publishDiagnostics": {"relatedInformation": true, "versionSupport": true},
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str, extensions: &[&str]) -> Server {
        Server {
            name: name.into(),
            command: "true".into(),
            extensions: extensions.iter().map(|e| e.to_string()).collect(),
            ..Default::default()
        }
    }

    /// The tie a Laravel project produces on every view, and the only rule that
    /// resolves it: the longer extension says more.
    #[test]
    fn the_longest_extension_wins() {
        let servers = vec![server("php", &["php"]), server("blade", &["blade.php"])];
        let picked = pick(&servers, Path::new("resources/views/page.blade.php")).unwrap();
        assert_eq!(picked.name, "blade");
        let picked = pick(&servers, Path::new("app/Models/User.php")).unwrap();
        assert_eq!(picked.name, "php");
    }

    #[test]
    fn a_file_nobody_serves_picks_nothing() {
        let servers = vec![server("php", &["php"])];
        assert!(pick(&servers, Path::new("README.md")).is_none());
        // An extension is a suffix after a dot, never a substring: `x.notphp`
        // must not match `php`.
        assert!(pick(&servers, Path::new("x.notphp")).is_none());
    }

    /// A declared server with no command is a line in the settings someone is
    /// still filling in: it must not be started, and must not shadow another.
    #[test]
    fn a_server_without_a_command_is_not_picked() {
        let mut empty = server("php", &["php"]);
        empty.command = "  ".into();
        let servers = vec![empty, server("other", &["php"])];
        assert_eq!(pick(&servers, Path::new("a.php")).unwrap().name, "other");
    }

    #[test]
    fn the_language_id_falls_back_to_the_extension() {
        let php = server("php", &["php"]);
        assert_eq!(php.language_for(Path::new("a.PHP")), "php");
        let mut blade = server("blade", &["blade.php"]);
        blade.language_id = "blade".into();
        assert_eq!(blade.language_for(Path::new("a.blade.php")), "blade");
    }

    /// Both shapes of `textDocumentSync` are current, and reading the object
    /// one as "absent" would send whole documents to a server that asked for
    /// ranges.
    #[test]
    fn both_shapes_of_the_sync_capability_are_read() {
        assert_eq!(
            sync_kind(&json!({"textDocumentSync": 2})),
            SyncKind::Incremental
        );
        assert_eq!(sync_kind(&json!({"textDocumentSync": 1})), SyncKind::Full);
        assert_eq!(
            sync_kind(&json!({"textDocumentSync": {"openClose": true, "change": 2}})),
            SyncKind::Incremental
        );
        assert_eq!(sync_kind(&json!({})), SyncKind::None);
    }

    /// The session loop, driven end to end without a process.
    ///
    /// `run` takes what it writes to and what it hears from, which is what
    /// makes this possible: the messages are queued in the order they would
    /// happen, and the handshake's id is 1 because that is where the counter
    /// starts. What it proves is the whole chain — the handshake, the queueing
    /// of what the view asked too early, the document versions, the range
    /// computed from the two texts, and the answer finding the view's own id
    /// again.
    fn drive(messages: Vec<Message>) -> (String, Vec<Evt>) {
        let (events_tx, events_rx) = async_channel::unbounded();
        let (inbox_tx, inbox_rx) = mpsc::channel();
        for message in messages {
            inbox_tx.send(message).unwrap();
        }
        inbox_tx.send(Message::Stop).unwrap();
        let mut session = Session {
            worktree: PathBuf::from("/p/site"),
            server: server("php", &["php"]),
            events: events_tx,
            next_id: 1,
            pending: HashMap::new(),
            documents: HashMap::new(),
            sync: SyncKind::Full,
            ready: false,
            queued: Vec::new(),
        };
        let mut written: Vec<u8> = Vec::new();
        session.run(&mut written, inbox_rx);
        let mut events = Vec::new();
        while let Ok(event) = events_rx.try_recv() {
            events.push(event);
        }
        (String::from_utf8(written).unwrap(), events)
    }

    /// What the session wrote, read back through our own framing — asserting
    /// on the raw text would be asserting on serde's key order, which is
    /// alphabetical and none of our business.
    fn frames(written: &str) -> Vec<Value> {
        let mut reader = std::io::Cursor::new(written.as_bytes().to_vec());
        let mut messages = Vec::new();
        while let Ok(Some(payload)) = frame::read(&mut reader) {
            messages.push(serde_json::from_str(&payload).unwrap());
        }
        messages
    }

    /// The one message with that method, of which there is always exactly one
    /// in these tests.
    fn sent<'a>(messages: &'a [Value], method: &str) -> &'a Value {
        messages
            .iter()
            .find(|m| m.get("method").and_then(Value::as_str) == Some(method))
            .unwrap_or_else(|| panic!("{method} was never sent"))
    }

    fn answer(id: u64, result: Value) -> Message {
        Message::Incoming(json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string())
    }

    #[test]
    fn a_whole_session_holds_together() {
        let capabilities = json!({"textDocumentSync": 2, "hoverProvider": true});
        let (written, events) = drive(vec![
            // Asked before the handshake came back: it must be replayed, not
            // lost and not reordered.
            Message::Ask(Ask::Open {
                path: PathBuf::from("/p/site/app/User.php"),
                language_id: "php".into(),
                text: "<?php\nclass User {}\n".into(),
            }),
            answer(1, json!({"capabilities": capabilities})),
            Message::Ask(Ask::Change {
                path: PathBuf::from("/p/site/app/User.php"),
                text: "<?php\nclass Users {}\n".into(),
            }),
            Message::Ask(Ask::Request {
                id: 77,
                method: "textDocument/hover".into(),
                params: json!({"position": {"line": 1, "character": 7}}).to_string(),
            }),
            answer(2, json!({"contents": "class User"})),
            Message::Incoming(
                json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/publishDiagnostics",
                    "params": {
                        "uri": "file:///p/site/app/User.php",
                        "diagnostics": [{"message": "undefined variable"}],
                    },
                })
                .to_string(),
            ),
        ]);

        let messages = frames(&written);
        // The handshake first, then what was queued behind it.
        let methods: Vec<&str> = messages
            .iter()
            .filter_map(|m| m.get("method").and_then(Value::as_str))
            .collect();
        assert_eq!(
            methods,
            [
                "initialize",
                "initialized",
                "textDocument/didOpen",
                "textDocument/didChange",
                "textDocument/hover",
            ]
        );
        let open = sent(&messages, "textDocument/didOpen");
        assert_eq!(open["params"]["textDocument"]["version"], 1);
        assert_eq!(
            open["params"]["textDocument"]["uri"],
            "file:///p/site/app/User.php"
        );

        // The server asked for ranges, so the change is one, and it carries the
        // single character that changed — not the whole file.
        let change = sent(&messages, "textDocument/didChange");
        assert_eq!(change["params"]["textDocument"]["version"], 2);
        let edit = &change["params"]["contentChanges"][0];
        assert_eq!(edit["text"], "s");
        assert_eq!(edit["range"]["start"], json!({"line": 1, "character": 10}));
        assert_eq!(edit["range"]["end"], json!({"line": 1, "character": 10}));

        match &events[..] {
            [Evt::LspReady { name, .. }, Evt::LspAnswer { id, result, .. }, Evt::LspDiagnostics {
                path, diagnostics, ..
            }] => {
                assert_eq!(name, "php");
                // The view's id, not the server's.
                assert_eq!(*id, 77);
                assert!(result.as_ref().unwrap().contains("class User"));
                assert_eq!(path, Path::new("/p/site/app/User.php"));
                assert!(diagnostics.contains("undefined variable"));
            }
            other => panic!("{other:?}"),
        }
    }

    /// A server that asks something of us must be answered, whatever it asks:
    /// one left waiting on a capability registration stops serving.
    #[test]
    fn every_server_request_gets_an_answer() {
        let (written, _) = drive(vec![
            answer(1, json!({"capabilities": {}})),
            Message::Incoming(
                json!({"jsonrpc": "2.0", "id": 40, "method": "client/registerCapability",
                       "params": {"registrations": []}})
                .to_string(),
            ),
            Message::Incoming(
                json!({"jsonrpc": "2.0", "id": 41, "method": "workspace/configuration",
                       "params": {"items": [{"section": "php"}, {"section": "phpstan"}]}})
                .to_string(),
            ),
            Message::Incoming(
                json!({"jsonrpc": "2.0", "id": 42, "method": "workspace/inlayHint/refresh"})
                    .to_string(),
            ),
        ]);
        let messages = frames(&written);
        let answered = |id: u64| {
            messages
                .iter()
                .find(|m| m.get("id").and_then(Value::as_u64) == Some(id))
                .unwrap_or_else(|| panic!("{id} was left waiting"))
        };
        assert_eq!(answered(40)["result"], Value::Null);
        // One `null` per item asked: that is how a client says "your defaults".
        assert_eq!(answered(41)["result"], json!([null, null]));
        // And what we do not do is refused, rather than left silent.
        assert_eq!(answered(42)["error"]["code"], -32601);
    }

    /// A dead session must fail what was waiting on it: a `Task` that never
    /// resolves leaves the completion popover spinning for ever.
    #[test]
    fn a_cancelled_request_stops_waiting() {
        let (written, events) = drive(vec![
            answer(1, json!({"capabilities": {}})),
            Message::Ask(Ask::Request {
                id: 5,
                method: "textDocument/completion".into(),
                params: "{}".into(),
            }),
            Message::Ask(Ask::Cancel { id: 5 }),
            // The answer arrives anyway, as it does when the cancel and the
            // answer cross: it must not reach the view a second time.
            answer(2, json!({"items": []})),
        ]);
        let messages = frames(&written);
        assert_eq!(sent(&messages, "$/cancelRequest")["params"]["id"], 2);
        assert!(!events.iter().any(|e| matches!(e, Evt::LspAnswer { .. })));
    }

    #[test]
    fn a_progress_notification_reads_title_then_message() {
        let value = json!({"kind": "begin", "title": "Indexing", "message": "1200 files"});
        assert_eq!(progress_text(&value).unwrap(), "Indexing — 1200 files");
        assert_eq!(
            progress_text(&json!({"kind": "report", "message": "vendor"})).unwrap(),
            "vendor"
        );
        assert!(progress_text(&json!({"kind": "end"})).is_none());
    }
}
