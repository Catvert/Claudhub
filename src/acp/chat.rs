//! One chat with an agent, as the protocol sees it — pure.
//!
//! [`Chat`] is fed the agent's lines ([`Chat::receive`]) and the user's
//! gestures ([`Chat::prompt`], [`Chat::answer`]…), and returns the lines to
//! write back. Between the two it keeps what the view paints: the transcript
//! folded from `session/update`s, the plan, the modes and options, and where
//! the conversation stands. No process, no clock, no gpui: the whole protocol
//! is tested here with strings.
//!
//! **What the client offers the agent: nothing but a chat.** No `fs/*`, no
//! `terminal/*` — an agent told so reads and writes the worktree with its own
//! tools, which is how the same agent already works in a terminal, and it
//! keeps this side of the wire free of any file access. What is asked anyway
//! is refused, never left hanging.
//!
//! Every reply is read field by field (`crate::json`'s rule): an agent's
//! `null` where a string was expected must cost a word, not the message.

use std::collections::HashMap;

use serde_json::{json, Value};

/// The protocol version spoken.
pub const PROTOCOL_VERSION: u64 = 1;

/// JSON-RPC error codes the protocol names.
pub const AUTH_REQUIRED: i64 = -32000;
pub const METHOD_NOT_FOUND: i64 = -32601;

/// Where the conversation stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The handshake is under way.
    Starting(Phase),
    /// The agent wants a login before it opens a session.
    AuthRequired,
    Ready,
    /// A prompt is running.
    Busy,
    /// The agent refused to start, or died; what it said.
    Failed(String),
}

/// The steps of a handshake, for the view to name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The process is being launched: nothing has come back yet.
    Launching,
    /// `initialize` is out.
    Connecting,
    /// `session/new` is out.
    OpeningSession,
    /// `authenticate` is out.
    SigningIn,
    /// `session/load` or `session/resume` is out: an earlier conversation is
    /// coming back.
    Resuming,
}

/// What a notice says, for the view to translate — the core has no `tr!`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// `stopReason: max_tokens`.
    TokenLimit,
    /// `stopReason: max_turn_requests`.
    TurnLimit,
    /// `stopReason: refusal`.
    Refused,
    /// The user stopped the turn.
    Cancelled,
    /// An error the agent answered with, as it said it.
    Error(String),
    /// The conversation continues, but the agent cannot show what was said
    /// before (`session/resume`).
    ResumedWithoutHistory,
    /// The earlier conversation could not come back; this is a new one.
    NotResumed(String),
    /// What the agent flags beside its reply (`sessionUpdate: notice`): a
    /// setting it could not honour, a hook that blocked, a limit near.
    Agent {
        /// `info`, `warning` or `error`.
        severity: String,
        title: String,
        description: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    pub description: String,
    /// A login run in a terminal rather than through `authenticate` — see
    /// [`AuthLaunch`].
    pub launch: Option<AuthLaunch>,
}

/// A login the agent wants run in a terminal (`type: terminal`): Claude's
/// adapter offers `claude auth login` this way, and only to a client that
/// says it can (`auth.terminal`) — Claudhub has terminals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthLaunch {
    /// The program, when the agent names it (`_meta.terminal-auth`); `None`
    /// is the agent's own command, the arguments after it.
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// One value of a select — a mode, a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub name: String,
    /// The group it was listed under, when the agent grouped them — a
    /// model's provider, say.
    pub group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionKind {
    Select {
        current: String,
        choices: Vec<Choice>,
    },
    Boolean(bool),
}

/// A session option the agent offers (`configOptions`): model, effort…
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    /// `mode`, `model`, `thought_level`… — what the option is about, for the
    /// view to place it and give it a glyph.
    pub category: Option<String>,
    pub kind: OptionKind,
}

impl ConfigOption {
    /// Whether this is the session's model — shown apart, next to send.
    pub fn is_model(&self) -> bool {
        self.category.as_deref() == Some("model") || self.id == "model"
    }

    /// Whether this is the permission mode — which makes the legacy modes
    /// redundant.
    pub fn is_mode(&self) -> bool {
        self.category.as_deref() == Some("mode") || self.id == "mode"
    }
}

/// An earlier conversation the agent keeps (`session/list`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub title: Option<String>,
    /// RFC 3339, as the agent wrote it.
    pub updated: Option<String>,
}

/// A slash command the agent announced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub content: String,
    /// `pending`, `in_progress` or `completed`.
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    /// `allow_once`, `allow_always`, `reject_once`, `reject_always`.
    pub kind: String,
}

/// A question the agent is waiting on, attached to its tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct Permission {
    /// The agent's request id, echoed on the answer — it may be a string.
    request: Value,
    pub options: Vec<PermissionOption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolContent {
    Text(String),
    Diff {
        path: String,
        old: Option<String>,
        new: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub title: String,
    /// `read`, `edit`, `execute`, `search`, `fetch`, `think`… or `other`.
    pub kind: String,
    /// `pending`, `in_progress`, `completed` or `failed`.
    pub status: String,
    pub content: Vec<ToolContent>,
    /// `path` or `path:line`.
    pub locations: Vec<String>,
    /// What the tool was called with — shown when the call says nothing
    /// else, a command line or a pattern being the whole of what it did.
    pub raw_input: Value,
    /// The command's terminal, when the agent reports one — see
    /// [`Terminal`].
    pub terminal: Option<Terminal>,
    pub permission: Option<Permission>,
    /// Unfolded or folded by hand — or by a permission it asks —; `None`
    /// leaves it to [`ToolCall::is_expanded`]'s default.
    pub expanded: Option<bool>,
}

/// A command's output, as the agent reports it on its tool call.
///
/// **Zed's extension, not the protocol's `terminal/*`.** An agent that runs
/// its commands itself — Claude's adapter never asks the client to — reports
/// them in the call's `_meta`: `terminal_info` opens the terminal,
/// `terminal_output` (whole) or `terminal_output_delta` (a piece to append)
/// carry the output, `terminal_exit` the end. Offered at `initialize`
/// (`clientCapabilities._meta`); an agent that does not know it sends the
/// output as a fenced block of the call's content, which [`ToolCall::output`]
/// reads the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Terminal {
    /// Escape sequences taken out as it arrives: the view paints plain text,
    /// and stripping at every frame would be paid for every visible row.
    pub output: String,
    pub exit: Option<Exit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    pub code: Option<i64>,
    pub signal: Option<String>,
}

impl Exit {
    /// A code of zero, and no signal.
    pub fn succeeded(&self) -> bool {
        self.code == Some(0) && self.signal.is_none()
    }
}

/// How much of a command's output is kept: its tail. A build's log runs to
/// megabytes, and what one reads of it in a chat is where it ended.
const OUTPUT_KEPT: usize = 256 * 1024;

impl ToolCall {
    /// Whether the card shows its body: what the hand chose, else **open
    /// on an edit and folded on the rest**. A diff is what one reviews of a
    /// turn, and it is short; a command's output is long and read on
    /// demand — its header already says the line it ran and how it ended.
    pub fn is_expanded(&self) -> bool {
        self.expanded.unwrap_or_else(|| {
            self.content
                .iter()
                .any(|content| matches!(content, ToolContent::Diff { .. }))
        })
    }

    /// The command line the call ran, from what it was called with: a string,
    /// or an argv — `bash -lc <script>` read as its script.
    pub fn command(&self) -> Option<String> {
        match &self.raw_input["command"] {
            Value::String(line) if !line.trim().is_empty() => Some(line.clone()),
            Value::Array(argv) => {
                let argv: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
                match argv.as_slice() {
                    [shell, flag, script]
                        if shell.ends_with("sh")
                            && flag.starts_with('-')
                            && flag.ends_with('c') =>
                    {
                        Some(script.to_string())
                    }
                    [] => None,
                    argv => Some(argv.join(" ")),
                }
            }
            _ => None,
        }
    }

    /// Whether this call ran a command — the card then shows its output.
    pub fn is_command(&self) -> bool {
        self.terminal.is_some() || self.kind == "execute"
    }

    /// What the command printed: the terminal's, or — from an agent without
    /// the extension — the call's text content, its fence taken off.
    pub fn output(&self) -> Option<String> {
        if let Some(terminal) = &self.terminal {
            return Some(terminal.output.clone());
        }
        if self.kind != "execute" {
            return None;
        }
        let texts: Vec<String> = self
            .content
            .iter()
            .filter_map(|content| match content {
                ToolContent::Text(text) => Some(unfence(text)),
                ToolContent::Diff { .. } => None,
            })
            .collect();
        (!texts.is_empty()).then(|| crate::text::strip_ansi(&texts.join("\n")))
    }

    /// Reads what `_meta` says of the call's terminal.
    fn read_terminal(&mut self, meta: &Value) {
        if meta["terminal_info"].is_object() && self.terminal.is_none() {
            self.terminal = Some(Terminal::default());
        }
        if let Some(data) = meta["terminal_output"]["data"].as_str() {
            let terminal = self.terminal.get_or_insert_with(Terminal::default);
            terminal.output = crate::text::strip_ansi(data);
            keep_tail(&mut terminal.output);
        }
        if let Some(data) = meta["terminal_output_delta"]["data"].as_str() {
            let terminal = self.terminal.get_or_insert_with(Terminal::default);
            terminal.output.push_str(&crate::text::strip_ansi(data));
            keep_tail(&mut terminal.output);
        }
        let exit = &meta["terminal_exit"];
        if exit.is_object() {
            let terminal = self.terminal.get_or_insert_with(Terminal::default);
            let mut code = exit["exit_code"].as_i64();
            // **Claude's adapter says 1 for any failure**, and the real code
            // only in the first line of the output, where Claude Code's tool
            // result writes it (`Exit code 3`). That line is the code, not
            // something the command printed.
            if let Some(said) = said_exit_code(&terminal.output) {
                code = Some(said);
                let rest = terminal
                    .output
                    .split_once('\n')
                    .map_or("", |(_, rest)| rest);
                terminal.output = rest.to_string();
            }
            terminal.exit = Some(Exit {
                code,
                signal: exit["signal"].as_str().map(str::to_string),
            });
        }
    }
}

/// The code in an `Exit code N` first line, as Claude Code's tool result
/// opens a failed command's output.
fn said_exit_code(output: &str) -> Option<i64> {
    let first = output.lines().next()?;
    first.strip_prefix("Exit code ")?.trim().parse().ok()
}

/// Drops the head of an output past what is kept, on a line when it can.
fn keep_tail(output: &mut String) {
    if output.len() <= OUTPUT_KEPT {
        return;
    }
    let mut cut = output.len() - OUTPUT_KEPT;
    while !output.is_char_boundary(cut) {
        cut += 1;
    }
    let cut = output[cut..].find('\n').map_or(cut, |line| cut + line + 1);
    output.drain(..cut);
}

/// A fenced block's body; any other text as it is.
fn unfence(text: &str) -> String {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return text.to_string();
    };
    let body = rest.split_once('\n').map_or("", |(_, body)| body);
    body.strip_suffix("```")
        .unwrap_or(body)
        .trim_end_matches('\n')
        .to_string()
}

/// The last `lines` lines of a text, and how many came before them.
pub fn tail(text: &str, lines: usize) -> (&str, usize) {
    let text = text.trim_end_matches('\n');
    let total = if text.is_empty() {
        0
    } else {
        text.matches('\n').count() + 1
    };
    if total <= lines {
        return (text, 0);
    }
    let mut start = text.len();
    for _ in 0..lines {
        start = text[..start].rfind('\n').unwrap_or(0);
    }
    (&text[start + 1..], total - lines)
}

// One per line of the conversation, walked in order: boxing the tool call
// would add an allocation per row and save nothing that matters.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// What the user sent, and how many images went with it.
    User {
        text: String,
        images: usize,
        /// Read only for a message another session sent in
        /// ([`agent_message`]), which is shown folded.
        expanded: bool,
    },
    /// The agent's reply, Markdown, and the message it belongs to — two replies
    /// in a row are two messages only when the agent says so.
    Agent {
        text: String,
        id: Option<String>,
    },
    Thought {
        text: String,
        expanded: bool,
    },
    Tool(ToolCall),
    Notice(Notice),
    /// The context being compacted (`/compact`, or the agent's own).
    Compaction(Compaction),
    /// A subagent the agent started — its session id; its transcript is in
    /// [`Chat::subagents`].
    Subagent(String),
}

/// A compaction of the context, as the agent reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compaction {
    pub id: String,
    /// `in_progress`, `completed`, `failed` or `cancelled`.
    pub status: String,
    /// What the conversation was reduced to.
    pub summary: String,
    pub error: Option<String>,
    pub expanded: bool,
}

/// A subagent (Claude's `Agent` / `Task` tool), and what it did.
///
/// **Two ways in.** The protocol's: a session of its own, announced in its
/// parent's (`subagent_spawned`), whose updates arrive under its own session
/// id, closed by `subagent_state_update` — offered by the `subagents` client
/// capability, which the ACP SDK's schema **drops** today (checked against
/// claude-agent-acp 0.84: the field never reaches the adapter). And the one
/// that works now: the `Agent` call is the subagent — its input names its
/// type and task, its status its state, its content its report — and every
/// call of the subagent's carries the `Agent` call's id as
/// `_meta.claudeCode.parentToolUseId`. The key is then that id.
#[derive(Debug, Clone, PartialEq)]
pub struct Subagent {
    /// Its session id, or the id of the `Agent` call that started it.
    pub session: String,
    pub name: String,
    pub task: String,
    /// `None` while it runs; `completed`, `failed`, `cancelled` or
    /// `disconnected` once it ended.
    pub state: Option<String>,
    pub entries: Vec<Entry>,
    /// What it handed back to its parent, when it ended.
    pub report: String,
    /// The `Agent` call's own permission, when it asks one.
    pub permission: Option<Permission>,
    pub expanded: bool,
}

/// Context used and available, in tokens, and what the session has cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    pub used: u64,
    pub size: u64,
    pub cost: Option<(f64, String)>,
}

/// What an answer we wait on belongs to.
#[derive(Debug, Clone, PartialEq)]
enum Pending {
    Initialize,
    NewSession,
    /// `session/load` (`true`, the history replayed) or `session/resume`.
    Resume(bool),
    Authenticate,
    Prompt,
    /// A mode or an option: only an error is read.
    Setting,
    /// `session/list`.
    List,
    /// `_session/steering`, and the prompt it carried — sent again as a
    /// `session/prompt` when the turn it aimed at was over.
    Steer(Vec<Value>),
    /// `session/delete`, and which.
    Delete(String),
    /// `session/fork`, and the title the copy opens under.
    Fork(Option<String>),
}

pub struct Chat {
    cwd: String,
    /// An earlier session to open instead of a new one.
    resume: Option<String>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    caps: Value,
    pub status: Status,
    pub session: Option<String>,
    /// What the agent calls itself, once it has said.
    pub agent_name: Option<String>,
    pub auth_methods: Vec<AuthMethod>,
    pub entries: Vec<Entry>,
    /// The legacy modes, when the agent has no `mode` option: current and all.
    pub modes: Option<(String, Vec<Choice>)>,
    pub options: Vec<ConfigOption>,
    pub commands: Vec<Command>,
    pub plan: Vec<PlanEntry>,
    pub usage: Option<Usage>,
    pub title: Option<String>,
    /// The earlier conversations, once asked for (`list_sessions`).
    pub sessions: Option<Vec<SessionSummary>>,
    /// The form the agent waits on (`elicitation/create`) — see [`Form`].
    pub form: Option<Form>,
    /// The subagents of this conversation, in the order they started.
    pub subagents: Vec<Subagent>,
    /// Prompts sent and not answered yet: a turn is over when it is zero.
    open_prompts: usize,
    /// The agent takes a message into the turn under way
    /// (`_session/steering`, advertised in `initialize`'s `_meta`).
    steering: bool,
    /// The agent queues a prompt sent while a turn runs
    /// (`_meta.claudeCode.promptQueueing`).
    queueing: bool,
}

impl Chat {
    /// A chat about to be launched in `cwd` — a path of the workers' world.
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            resume: None,
            next_id: 1,
            pending: HashMap::new(),
            caps: Value::Null,
            status: Status::Starting(Phase::Launching),
            session: None,
            agent_name: None,
            auth_methods: Vec::new(),
            entries: Vec::new(),
            modes: None,
            options: Vec::new(),
            commands: Vec::new(),
            plan: Vec::new(),
            usage: None,
            title: None,
            sessions: None,
            form: None,
            subagents: Vec::new(),
            open_prompts: 0,
            steering: false,
            queueing: false,
        }
    }

    /// A chat that reopens an earlier session of the same agent — the one a
    /// tab had when the window closed, or before its agent was restarted.
    pub fn resuming(cwd: impl Into<String>, session: impl Into<String>) -> Self {
        let mut chat = Self::new(cwd);
        chat.resume = Some(session.into());
        chat
    }

    /// The first line: `initialize`, sent once the process is asked for.
    pub fn start(&mut self) -> String {
        self.status = Status::Starting(Phase::Connecting);
        self.request(Pending::Initialize, "initialize", initialize_params())
    }

    /// Whether a prompt can go now.
    pub fn is_ready(&self) -> bool {
        self.status == Status::Ready
    }

    /// Whether the agent is working on a turn.
    pub fn is_busy(&self) -> bool {
        self.status == Status::Busy
    }

    /// Whether a permission question is waiting on the user.
    pub fn is_asking(&self) -> bool {
        let asks = |entries: &[Entry]| {
            entries
                .iter()
                .any(|entry| matches!(entry, Entry::Tool(tool) if tool.permission.is_some()))
        };
        self.form.is_some()
            || asks(&self.entries)
            || self
                .subagents
                .iter()
                .any(|agent| agent.permission.is_some() || asks(&agent.entries))
    }

    /// Whether what is typed can go now: at rest, or during a turn to an
    /// agent that takes it in (steering) or queues it.
    pub fn can_send(&self) -> bool {
        self.is_ready() || (self.is_busy() && (self.steering || self.queueing))
    }

    /// Whether the agent reads images in a prompt.
    pub fn takes_images(&self) -> bool {
        self.can(&["promptCapabilities", "image"])
    }

    /// Whether the agent deletes its conversations.
    pub fn can_delete(&self) -> bool {
        self.can(&["sessionCapabilities", "delete"])
    }

    /// Whether the agent copies a conversation to go on from it apart.
    pub fn can_fork(&self) -> bool {
        self.can(&["sessionCapabilities", "fork"])
    }

    /// Deletes an earlier conversation.
    pub fn delete_session(&mut self, session: &str) -> Option<String> {
        if !self.can_delete() || !self.is_connected() || self.session.as_deref() == Some(session) {
            return None;
        }
        Some(self.request(
            Pending::Delete(session.to_string()),
            "session/delete",
            json!({ "sessionId": session }),
        ))
    }

    /// Copies a conversation, and opens the copy here once it exists.
    pub fn fork_session(&mut self, session: &str, title: Option<String>) -> Option<String> {
        if !self.can_fork() || !self.is_connected() {
            return None;
        }
        Some(self.request(
            Pending::Fork(title),
            "session/fork",
            json!({ "sessionId": session, "cwd": self.cwd, "mcpServers": [] }),
        ))
    }

    /// The user opened the link a URL form points at: `accept`.
    pub fn accept_url(&mut self) -> Option<String> {
        let form = self.form.take_if(|form| form.url.is_some())?;
        Some(response(form.request, Ok(json!({ "action": "accept" }))))
    }

    /// The user filled the form in: `accept`, with what the fields hold.
    pub fn answer_form(&mut self, answers: &HashMap<String, Answer>) -> Option<String> {
        let form = self.form.take()?;
        let content = form.content(answers);
        Some(response(
            form.request,
            Ok(json!({ "action": "accept", "content": content })),
        ))
    }

    /// The user skipped the form: `decline` — the agent is told so and goes
    /// on, where `cancel` would stop the tool call.
    pub fn decline_form(&mut self) -> Option<String> {
        let form = self.form.take()?;
        Some(response(form.request, Ok(json!({ "action": "decline" }))))
    }

    /// One line from the agent; returns the lines to write back.
    pub fn receive(&mut self, line: &str) -> Vec<String> {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").filter(|id| !id.is_null()).cloned();
        match (method, id) {
            (None, Some(id)) => self.answered(&id, &message),
            (Some(method), Some(id)) => self.asked(id, method, &message["params"]),
            (Some("session/update"), None) => {
                let params = &message["params"];
                let session = params["sessionId"].as_str();
                if session.is_some() && session == self.session.as_deref() {
                    self.update(&params["update"]);
                } else if let Some(agent) = self
                    .subagents
                    .iter_mut()
                    .find(|agent| Some(agent.session.as_str()) == session)
                {
                    fold(&mut agent.entries, &params["update"]);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The process is gone. `None` when the view ended it.
    pub fn ended(&mut self, reason: Option<String>) {
        self.pending.clear();
        self.open_prompts = 0;
        self.drop_permissions();
        if let Some(reason) = reason {
            self.status = Status::Failed(reason);
        }
    }

    /// Sends what the user typed.
    pub fn prompt(&mut self, text: &str) -> Option<String> {
        self.prompt_with(text, &[], &[])
    }

    /// Sends what the user typed, and the files it mentions — each a
    /// `resource_link` (its name and its `file://` URI), the baseline every
    /// agent reads: the agent opens the file itself, and nothing here does.
    ///
    /// And the images pasted with it — `(mime type, base64)` —, when the agent
    /// reads them.
    ///
    /// **During a turn**, the message goes into it (`_session/steering`) when
    /// the agent offers that — as typing while Claude works does in its CLI —
    /// and is otherwise queued behind it as one more `session/prompt`.
    pub fn prompt_with(
        &mut self,
        text: &str,
        files: &[(String, String)],
        images: &[(String, String)],
    ) -> Option<String> {
        let text = text.trim();
        let images: &[(String, String)] = if self.takes_images() { images } else { &[] };
        if (text.is_empty() && images.is_empty()) || !self.can_send() {
            return None;
        }
        let session = self.session.clone()?;
        self.entries.push(Entry::User {
            text: text.to_string(),
            images: images.len(),
            expanded: false,
        });
        let mut prompt = Vec::new();
        if !text.is_empty() {
            prompt.push(json!({ "type": "text", "text": text }));
        }
        prompt.extend(
            images
                .iter()
                .map(|(mime, data)| json!({ "type": "image", "mimeType": mime, "data": data })),
        );
        prompt.extend(
            files
                .iter()
                .map(|(name, uri)| json!({ "type": "resource_link", "name": name, "uri": uri })),
        );
        if self.is_busy() && self.steering {
            return Some(self.request(
                Pending::Steer(prompt.clone()),
                "_session/steering",
                json!({
                    "sessionId": session,
                    "prompt": prompt,
                    // A turn that ended meanwhile hands the message back
                    // rather than starting one this chat does not follow.
                    "_meta": { "steering": { "idleBehavior": "promptRequired" } }
                }),
            ));
        }
        Some(self.send_prompt(session, prompt))
    }

    fn send_prompt(&mut self, session: String, prompt: Vec<Value>) -> String {
        self.status = Status::Busy;
        self.open_prompts += 1;
        self.request(
            Pending::Prompt,
            "session/prompt",
            json!({ "sessionId": session, "prompt": prompt }),
        )
    }

    /// Whether the agent keeps its conversations and lists them.
    pub fn can_list(&self) -> bool {
        self.can(&["sessionCapabilities", "list"])
    }

    /// Asks for the earlier conversations of this worktree.
    pub fn list_sessions(&mut self) -> Option<String> {
        if !self.can_list() || matches!(self.status, Status::Starting(_) | Status::Failed(_)) {
            return None;
        }
        Some(self.request(Pending::List, "session/list", json!({ "cwd": self.cwd })))
    }

    /// Whether the agent's process is up and past its handshake — what a new
    /// chat or an earlier one needs in this tab.
    pub fn is_connected(&self) -> bool {
        matches!(self.status, Status::Ready | Status::Busy)
    }

    /// A fresh conversation in the same tab, with the same agent. A turn
    /// under way is stopped first.
    pub fn new_chat(&mut self) -> Vec<String> {
        if !self.is_connected() {
            return Vec::new();
        }
        let mut lines = self.cancel();
        self.forget();
        lines.push(self.new_session());
        lines
    }

    /// An earlier conversation, in place of this one.
    pub fn reopen(&mut self, session: &str, title: Option<String>) -> Vec<String> {
        if !self.is_connected() || self.session.as_deref() == Some(session) {
            return Vec::new();
        }
        let mut lines = self.cancel();
        self.forget();
        self.title = title;
        self.resume = Some(session.to_string());
        lines.push(self.open_session());
        lines
    }

    /// Everything that belonged to the conversation on show.
    fn forget(&mut self) {
        self.pending
            .retain(|_, pending| matches!(pending, Pending::Setting));
        self.open_prompts = 0;
        self.subagents.clear();
        self.entries.clear();
        self.plan.clear();
        self.usage = None;
        self.title = None;
        self.session = None;
        self.modes = None;
        self.options.clear();
        self.commands.clear();
    }

    /// Stops the running turn: the notification, and a `cancelled` for every
    /// question still open — the protocol asks for both.
    pub fn cancel(&mut self) -> Vec<String> {
        let Some(session) = self.session.clone() else {
            return Vec::new();
        };
        if !self.is_busy() {
            return Vec::new();
        }
        let mut lines = self.cancel_permissions();
        lines.push(notification(
            "session/cancel",
            json!({ "sessionId": session }),
        ));
        lines
    }

    /// The user picked one of a tool call's permission options.
    pub fn answer(&mut self, tool: &str, option: &str) -> Option<String> {
        let permission = match self.subagent_mut(tool) {
            Some(agent) => agent.permission.take()?,
            None => self.tool_mut(tool)?.permission.take()?,
        };
        Some(response(
            permission.request,
            Ok(json!({ "outcome": { "outcome": "selected", "optionId": option } })),
        ))
    }

    /// Signs in with one of the agent's methods.
    pub fn authenticate(&mut self, method: &str) -> Option<String> {
        if self.status != Status::AuthRequired {
            return None;
        }
        self.status = Status::Starting(Phase::SigningIn);
        Some(self.request(
            Pending::Authenticate,
            "authenticate",
            json!({ "methodId": method }),
        ))
    }

    /// Opens the session again — after a login made elsewhere.
    pub fn retry_session(&mut self) -> Option<String> {
        if self.status != Status::AuthRequired {
            return None;
        }
        Some(self.open_session())
    }

    /// Picks a legacy mode.
    pub fn set_mode(&mut self, mode: &str) -> Option<String> {
        let session = self.session.clone()?;
        let (current, modes) = self.modes.as_mut()?;
        if !modes.iter().any(|m| m.value == mode) {
            return None;
        }
        *current = mode.to_string();
        Some(self.request(
            Pending::Setting,
            "session/set_mode",
            json!({ "sessionId": session, "modeId": mode }),
        ))
    }

    /// Sets a session option; shown at once, corrected by the answer.
    pub fn set_option(&mut self, id: &str, value: Value) -> Option<String> {
        let session = self.session.clone()?;
        let option = self.options.iter_mut().find(|option| option.id == id)?;
        match (&mut option.kind, &value) {
            (OptionKind::Select { current, .. }, Value::String(v)) => *current = v.clone(),
            (OptionKind::Boolean(b), Value::Bool(v)) => *b = *v,
            _ => return None,
        }
        let mut params = json!({ "sessionId": session, "configId": id, "value": value });
        if value.is_boolean() {
            params["type"] = json!("boolean");
        }
        Some(self.request(Pending::Setting, "session/set_config_option", params))
    }

    /// Folds or unfolds a thought, a tool call, a compaction or a subagent.
    pub fn toggle(&mut self, ix: usize) {
        match self.entries.get_mut(ix) {
            Some(Entry::Thought { expanded, .. } | Entry::User { expanded, .. }) => {
                *expanded = !*expanded
            }
            Some(Entry::Tool(tool)) => tool.expanded = Some(!tool.is_expanded()),
            Some(Entry::Compaction(compaction)) => compaction.expanded = !compaction.expanded,
            Some(Entry::Subagent(session)) => {
                let session = session.clone();
                if let Some(agent) = self.subagent_mut(&session) {
                    agent.expanded = !agent.expanded;
                }
            }
            _ => {}
        }
    }

    /// What the user sent, in order, as the timeline lists it — not what
    /// another session sent in ([`agent_message`]).
    pub fn prompts(&self) -> Vec<Prompt<'_>> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(entry, item)| match item {
                Entry::User { text, images, .. } if agent_message(text).is_none() => Some(Prompt {
                    entry,
                    summary: text.lines().map(str::trim).find(|line| !line.is_empty()),
                    images: *images,
                }),
                _ => None,
            })
            .collect()
    }

    /// The subagents pinned above the composer, each with the index of its
    /// card in [`Chat::entries`]: those still running, and while a turn is
    /// under way, those it launched — since the last prompt — even ended,
    /// so that a fan-out of five reads as five until the turn is over.
    pub fn pinned_subagents(&self) -> Vec<(usize, &Subagent)> {
        let last_prompt = self.prompts().last().map_or(0, |prompt| prompt.entry);
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(entry, item)| match item {
                Entry::Subagent(session) => Some((entry, self.subagent(session)?)),
                _ => None,
            })
            .filter(|(entry, agent)| {
                agent.state.is_none() || self.is_busy() && *entry > last_prompt
            })
            .collect()
    }

    /// A subagent, by its session id.
    pub fn subagent(&self, session: &str) -> Option<&Subagent> {
        self.subagents.iter().find(|agent| agent.session == session)
    }

    fn subagent_mut(&mut self, session: &str) -> Option<&mut Subagent> {
        self.subagents
            .iter_mut()
            .find(|agent| agent.session == session)
    }

    // — Lines out ————————————————————————————————————————————————————

    fn request(&mut self, pending: Pending, method: &str, params: Value) -> String {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, pending);
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()
    }

    /// Whether the agent says it can do this (`agentCapabilities`): a member
    /// present and not `false`.
    fn can(&self, path: &[&str]) -> bool {
        let found = path.iter().fold(&self.caps, |value, key| &value[*key]);
        !found.is_null() && found != &Value::Bool(false)
    }

    /// The session the chat asks for: the earlier one when there is one to
    /// come back to and the agent can bring it back, a new one otherwise.
    fn open_session(&mut self) -> String {
        let Some(session) = self.resume.clone() else {
            return self.new_session();
        };
        let replayed = self.can(&["loadSession"]);
        if !replayed && !self.can(&["sessionCapabilities", "resume"]) {
            self.resume = None;
            return self.new_session();
        }
        // Named before the answer: `session/load` replays the history as
        // `session/update`s, which arrive before it and are read by session.
        self.session = Some(session.clone());
        self.status = Status::Starting(Phase::Resuming);
        let method = if replayed {
            "session/load"
        } else {
            "session/resume"
        };
        self.request(
            Pending::Resume(replayed),
            method,
            json!({ "sessionId": session, "cwd": self.cwd, "mcpServers": [] }),
        )
    }

    fn new_session(&mut self) -> String {
        self.status = Status::Starting(Phase::OpeningSession);
        self.request(
            Pending::NewSession,
            "session/new",
            json!({ "cwd": self.cwd, "mcpServers": [] }),
        )
    }

    // — Answers to what we asked ——————————————————————————————————————

    fn answered(&mut self, id: &Value, message: &Value) -> Vec<String> {
        let Some(pending) = id.as_u64().and_then(|id| self.pending.remove(&id)) else {
            return Vec::new();
        };
        let result = match message.get("error") {
            Some(error) => Err((error["code"].as_i64().unwrap_or(0), error_text(error))),
            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
        };
        match (pending, result) {
            (Pending::Initialize, Ok(result)) => {
                self.caps = result["agentCapabilities"].clone();
                self.steering = result["_meta"]["steering"]["supported"] == json!(true);
                self.queueing = self.caps["_meta"]["claudeCode"]["promptQueueing"] == json!(true);
                self.agent_name = result["agentInfo"]["title"]
                    .as_str()
                    .or(result["agentInfo"]["name"].as_str())
                    .filter(|name| !name.is_empty())
                    .map(str::to_string);
                self.auth_methods = list(&result["authMethods"])
                    .map(|m| AuthMethod {
                        id: text(&m["id"]),
                        name: text(&m["name"]),
                        description: text(&m["description"]),
                        launch: auth_launch(m),
                    })
                    .collect();
                vec![self.open_session()]
            }
            (Pending::NewSession, Ok(result)) => {
                self.session = result["sessionId"].as_str().map(str::to_string);
                self.session_opened(&result);
                Vec::new()
            }
            (Pending::Authenticate, Ok(_)) => vec![self.open_session()],
            (Pending::Resume(replayed), Ok(result)) => {
                self.resume = None;
                if replayed {
                    // What the history left running belonged to a process
                    // that is gone.
                    self.close_abandoned_calls();
                } else {
                    self.entries
                        .push(Entry::Notice(Notice::ResumedWithoutHistory));
                }
                self.session_opened(&result);
                Vec::new()
            }
            (Pending::Prompt, Ok(result)) => {
                self.open_prompts = self.open_prompts.saturating_sub(1);
                if self.open_prompts == 0 {
                    self.status = Status::Ready;
                    self.drop_permissions();
                    self.close_abandoned_calls();
                }
                let notice = match result["stopReason"].as_str() {
                    Some("max_tokens") => Some(Notice::TokenLimit),
                    Some("max_turn_requests") => Some(Notice::TurnLimit),
                    Some("refusal") => Some(Notice::Refused),
                    Some("cancelled") => Some(Notice::Cancelled),
                    _ => None,
                };
                self.entries.extend(notice.map(Entry::Notice));
                Vec::new()
            }
            (Pending::Setting, Ok(result)) => {
                if result["configOptions"].is_array() {
                    self.options = parse_options(&result["configOptions"]);
                }
                Vec::new()
            }
            (
                Pending::NewSession | Pending::Resume(_) | Pending::Prompt | Pending::Authenticate,
                Err((code, _)),
            ) if code == AUTH_REQUIRED => {
                self.status = Status::AuthRequired;
                Vec::new()
            }
            // The session is gone — expired, deleted, another machine's: a
            // new one rather than a dead tab, and the reason said.
            (Pending::Resume(_), Err((_, message))) => {
                self.resume = None;
                self.session = None;
                self.entries.clear();
                self.entries
                    .push(Entry::Notice(Notice::NotResumed(message)));
                vec![self.new_session()]
            }
            (Pending::Initialize | Pending::NewSession, Err((_, message))) => {
                self.status = Status::Failed(message);
                Vec::new()
            }
            (Pending::Authenticate, Err((_, message))) => {
                self.status = Status::AuthRequired;
                self.entries.push(Entry::Notice(Notice::Error(message)));
                Vec::new()
            }
            (Pending::Prompt, Err((_, message))) => {
                self.open_prompts = self.open_prompts.saturating_sub(1);
                if self.open_prompts == 0 {
                    self.status = Status::Ready;
                    self.drop_permissions();
                }
                self.entries.push(Entry::Notice(Notice::Error(message)));
                Vec::new()
            }
            (Pending::Steer(prompt), Ok(result)) => {
                // The turn ended before the message reached it: it is a turn
                // of its own.
                if result["outcome"] == json!("promptRequired") {
                    if let Some(session) = self.session.clone() {
                        return vec![self.send_prompt(session, prompt)];
                    }
                }
                Vec::new()
            }
            (Pending::Steer(_), Err((_, message))) => {
                self.entries.push(Entry::Notice(Notice::Error(message)));
                Vec::new()
            }
            (Pending::Delete(session), Ok(_)) => {
                if let Some(sessions) = &mut self.sessions {
                    sessions.retain(|summary| summary.id != session);
                }
                Vec::new()
            }
            (Pending::Fork(title), Ok(result)) => match result["sessionId"].as_str() {
                Some(copy) => {
                    let copy = copy.to_string();
                    self.reopen(&copy, title)
                }
                None => Vec::new(),
            },
            (Pending::Delete(_) | Pending::Fork(_), Err((_, message))) => {
                self.entries.push(Entry::Notice(Notice::Error(message)));
                Vec::new()
            }
            (Pending::Setting, Err((_, message))) => {
                self.entries.push(Entry::Notice(Notice::Error(message)));
                Vec::new()
            }
            (Pending::List, Ok(result)) => {
                self.sessions = Some(
                    list(&result["sessions"])
                        .map(|session| SessionSummary {
                            id: text(&session["sessionId"]),
                            title: session["title"]
                                .as_str()
                                .filter(|title| !title.trim().is_empty())
                                .map(str::to_string),
                            updated: session["updatedAt"].as_str().map(str::to_string),
                        })
                        .filter(|session| !session.id.is_empty())
                        .collect(),
                );
                Vec::new()
            }
            // A list that will not come is an empty one, not a spinner.
            (Pending::List, Err(_)) => {
                self.sessions = Some(Vec::new());
                Vec::new()
            }
        }
    }

    fn session_opened(&mut self, result: &Value) {
        self.modes = parse_modes(&result["modes"]);
        if result["configOptions"].is_array() {
            self.options = parse_options(&result["configOptions"]);
        }
        self.status = Status::Ready;
    }

    // — What the agent asks of us ————————————————————————————————————

    fn asked(&mut self, id: Value, method: &str, params: &Value) -> Vec<String> {
        if method == "elicitation/create" {
            return self.form_asked(id, params);
        }
        if method != "session/request_permission" {
            // Nothing else was offered at `initialize`; an answer is owed all
            // the same, or the agent waits for ever.
            return vec![response(
                id,
                Err((METHOD_NOT_FOUND, format!("{method} is not supported"))),
            )];
        }
        let tool = &params["toolCall"];
        let tool_id = text(&tool["toolCallId"]);
        let options: Vec<PermissionOption> = list(&params["options"])
            .map(|o| PermissionOption {
                id: text(&o["optionId"]),
                name: text(&o["name"]),
                kind: text(&o["kind"]),
            })
            .collect();
        // The `Agent` call itself asks: the question is the subagent's.
        if let Some(agent) = self.subagent_mut(&tool_id) {
            agent.permission = Some(Permission {
                request: id,
                options,
            });
            agent.expanded = true;
            return Vec::new();
        }
        // A subagent's call is its own, in its transcript — by the session
        // it came from, the call it names as its parent, or where it was
        // already filed.
        let session = params["sessionId"].as_str();
        let parent = tool["_meta"]["claudeCode"]["parentToolUseId"].as_str();
        let owner = self.subagents.iter().position(|agent| {
            Some(agent.session.as_str()) == session
                || Some(agent.session.as_str()) == parent
                || agent
                    .entries
                    .iter()
                    .any(|entry| matches!(entry, Entry::Tool(call) if call.id == tool_id))
        });
        match owner {
            Some(owner) => upsert_tool(&mut self.subagents[owner].entries, tool),
            None => upsert_tool(&mut self.entries, tool),
        }
        match self.tool_mut(&tool_id) {
            Some(tool) => {
                tool.permission = Some(Permission {
                    request: id,
                    options,
                });
                tool.expanded = Some(true);
                Vec::new()
            }
            None => vec![response(
                id,
                Ok(json!({ "outcome": { "outcome": "cancelled" } })),
            )],
        }
    }

    /// A form the agent asks to be filled. One at a time: the agent waits on
    /// it, and a second one while the first is open is refused rather than
    /// hidden behind it.
    fn form_asked(&mut self, id: Value, params: &Value) -> Vec<String> {
        if self.form.is_some() {
            return vec![response(id, Ok(json!({ "action": "decline" })))];
        }
        match Form::parse(id.clone(), params) {
            Some(form) => {
                self.form = Some(form);
                Vec::new()
            }
            // Nothing this view can show: declined, which the agent reads as
            // "skipped" and goes on.
            None => vec![response(id, Ok(json!({ "action": "decline" })))],
        }
    }

    /// The open questions, answered `cancelled`.
    fn cancel_permissions(&mut self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .form
            .take()
            .map(|form| response(form.request, Ok(json!({ "action": "cancel" }))))
            .into_iter()
            .collect();
        for agent in &mut self.subagents {
            if let Some(permission) = agent.permission.take() {
                lines.push(response(
                    permission.request,
                    Ok(json!({ "outcome": { "outcome": "cancelled" } })),
                ));
            }
        }
        let entries = self.entries.iter_mut().chain(
            self.subagents
                .iter_mut()
                .flat_map(|agent| agent.entries.iter_mut()),
        );
        for entry in entries {
            if let Entry::Tool(tool) = entry {
                if let Some(permission) = tool.permission.take() {
                    lines.push(response(
                        permission.request,
                        Ok(json!({ "outcome": { "outcome": "cancelled" } })),
                    ));
                }
            }
        }
        lines
    }

    /// The calls still pending once the turn is over, marked `cancelled`.
    ///
    /// **A steered message aborts the generation under way** (that is what
    /// taking it "now" means), and a call the model had started there is
    /// started again, under a new id, with nothing ever closing the first —
    /// seen against claude-agent-acp 0.84. Left as it is, it spins for good.
    fn close_abandoned_calls(&mut self) {
        // A subagent too: pinned above the composer while it runs, it would
        // stay there for good.
        for agent in &mut self.subagents {
            if agent.state.is_none() {
                agent.state = Some("cancelled".to_string());
            }
        }
        let entries = self.entries.iter_mut().chain(
            self.subagents
                .iter_mut()
                .flat_map(|agent| agent.entries.iter_mut()),
        );
        for entry in entries {
            if let Entry::Tool(tool) = entry {
                if matches!(tool.status.as_str(), "pending" | "in_progress") {
                    tool.status = "cancelled".to_string();
                }
            }
        }
    }

    /// The open questions, forgotten: the turn they belonged to is over.
    fn drop_permissions(&mut self) {
        self.form = None;
        for agent in &mut self.subagents {
            agent.permission = None;
        }
        let entries = self.entries.iter_mut().chain(
            self.subagents
                .iter_mut()
                .flat_map(|agent| agent.entries.iter_mut()),
        );
        for entry in entries {
            if let Entry::Tool(tool) = entry {
                tool.permission = None;
            }
        }
    }

    // — The session's news ——————————————————————————————————————————

    fn update(&mut self, update: &Value) {
        if self.route_subagent_call(update) {
            return;
        }
        if fold(&mut self.entries, update) {
            return;
        }
        match update["sessionUpdate"].as_str().unwrap_or_default() {
            "plan" => self.plan = parse_plan(&update["entries"]),
            "notice" => self.entries.push(Entry::Notice(Notice::Agent {
                severity: update["severity"].as_str().unwrap_or("info").to_string(),
                title: text(&update["title"]),
                description: update["description"].as_str().map(str::to_string),
            })),
            "compaction_update" => {
                let id = text(&update["compactionId"]);
                let summary: String = list(&update["summary"]).map(block_text).collect();
                let error = update["error"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| update["error"]["message"].as_str().map(str::to_string));
                let status = update["status"]
                    .as_str()
                    .unwrap_or("in_progress")
                    .to_string();
                match self.compaction_mut(&id) {
                    Some(compaction) => {
                        compaction.status = status;
                        if !summary.is_empty() {
                            compaction.summary = summary;
                        }
                        if error.is_some() {
                            compaction.error = error;
                        }
                    }
                    None => self.entries.push(Entry::Compaction(Compaction {
                        id,
                        status,
                        summary,
                        error,
                        expanded: false,
                    })),
                }
            }
            "compaction_summary_chunk" => {
                let id = text(&update["compactionId"]);
                let chunk = block_text(&update["content"]);
                if let Some(compaction) = self.compaction_mut(&id) {
                    compaction.summary.push_str(&chunk);
                }
            }
            "subagent_spawned" => {
                let session = text(&update["subagentSessionId"]);
                if session.is_empty() || self.subagent(&session).is_some() {
                    return;
                }
                self.subagents.push(Subagent {
                    session: session.clone(),
                    name: text(&update["name"]),
                    task: text(&update["task"]),
                    state: None,
                    entries: Vec::new(),
                    report: String::new(),
                    permission: None,
                    expanded: false,
                });
                self.entries.push(Entry::Subagent(session));
            }
            "subagent_state_update" => {
                let session = text(&update["subagentSessionId"]);
                if let Some(agent) = self.subagent_mut(&session) {
                    agent.state = update["state"].as_str().map(str::to_string);
                }
            }
            "available_commands_update" => {
                self.commands = list(&update["availableCommands"])
                    .map(|c| Command {
                        name: text(&c["name"]),
                        description: text(&c["description"]),
                    })
                    .collect();
            }
            "current_mode_update" => {
                if let Some((current, _)) = &mut self.modes {
                    *current = text(&update["currentModeId"]);
                }
            }
            "config_option_update" => self.options = parse_options(&update["configOptions"]),
            "session_info_update" => {
                if let Some(title) = update["title"].as_str() {
                    self.title = Some(title.to_string());
                }
            }
            "usage_update" => {
                self.usage = Some(Usage {
                    used: update["used"].as_u64().unwrap_or(0),
                    size: update["size"].as_u64().unwrap_or(0),
                    cost: update["cost"]["amount"].as_f64().map(|amount| {
                        let currency = update["cost"]["currency"].as_str().unwrap_or("USD");
                        (amount, currency.to_string())
                    }),
                });
            }
            _ => {}
        }
    }

    /// A subagent's call, filed under the subagent; or the `Agent` call
    /// itself, which is the subagent. `false` for any other update.
    fn route_subagent_call(&mut self, update: &Value) -> bool {
        if !matches!(
            update["sessionUpdate"].as_str(),
            Some("tool_call" | "tool_call_update")
        ) {
            return false;
        }
        let meta = &update["_meta"]["claudeCode"];
        let id = text(&update["toolCallId"]);
        if let Some(parent) = meta["parentToolUseId"].as_str() {
            if let Some(agent) = self.subagent_mut(parent) {
                upsert_tool(&mut agent.entries, update);
                return true;
            }
        }
        let is_agent = matches!(meta["toolName"].as_str(), Some("Agent" | "Task"))
            || self.subagent(&id).is_some();
        if !is_agent || id.is_empty() {
            return false;
        }
        if self.subagent(&id).is_none() {
            self.subagents.push(Subagent {
                session: id.clone(),
                name: String::new(),
                task: String::new(),
                state: None,
                entries: Vec::new(),
                report: String::new(),
                permission: None,
                expanded: false,
            });
            self.entries.push(Entry::Subagent(id.clone()));
        }
        let Some(agent) = self.subagent_mut(&id) else {
            return true;
        };
        let input = &update["rawInput"];
        if let Some(kind) = input["subagent_type"].as_str().filter(|k| !k.is_empty()) {
            agent.name = kind.to_string();
        }
        match input["description"].as_str().filter(|d| !d.is_empty()) {
            Some(task) => agent.task = task.to_string(),
            None => {
                // The first title is the tool's own name; the description
                // comes after it.
                if let Some(title) = update["title"].as_str() {
                    if !title.is_empty() && !matches!(title, "Agent" | "Task") {
                        agent.task = title.to_string();
                    }
                }
            }
        }
        if let Some(state @ ("completed" | "failed")) = update["status"].as_str() {
            agent.state = Some(state.to_string());
        }
        if let Some(content) = update.get("content").filter(|c| c.is_array()) {
            let report: Vec<String> = tool_content(content)
                .into_iter()
                .filter_map(|content| match content {
                    ToolContent::Text(text) => Some(text),
                    ToolContent::Diff { .. } => None,
                })
                .collect();
            if !report.is_empty() {
                agent.report = report.join("\n\n");
            }
        }
        true
    }

    /// A tool call, in the transcript or in a subagent's.
    fn tool_mut(&mut self, id: &str) -> Option<&mut ToolCall> {
        let Chat {
            entries, subagents, ..
        } = self;
        tool_in(entries, id).or_else(|| {
            subagents
                .iter_mut()
                .find_map(|agent| tool_in(&mut agent.entries, id))
        })
    }

    fn compaction_mut(&mut self, id: &str) -> Option<&mut Compaction> {
        self.entries.iter_mut().rev().find_map(|entry| match entry {
            Entry::Compaction(compaction) if compaction.id == id => Some(compaction),
            _ => None,
        })
    }
}

/// Folds what a transcript is made of — the messages, the thoughts, the tool
/// calls — into `entries`: the conversation's, or a subagent's. `false` for
/// an update that is about something else.
fn fold(entries: &mut Vec<Entry>, update: &Value) -> bool {
    match update["sessionUpdate"].as_str().unwrap_or_default() {
        "agent_message_chunk" => {
            let chunk = block_text(&update["content"]);
            let id = update["messageId"].as_str().map(str::to_string);
            match entries.last_mut() {
                Some(Entry::Agent { text, id: last }) if id.is_none() || *last == id => {
                    text.push_str(&chunk)
                }
                _ => entries.push(Entry::Agent { text: chunk, id }),
            }
        }
        "agent_thought_chunk" => {
            let chunk = block_text(&update["content"]);
            match entries.last_mut() {
                Some(Entry::Thought { text, .. }) => text.push_str(&chunk),
                _ => entries.push(Entry::Thought {
                    text: chunk,
                    expanded: false,
                }),
            }
        }
        "user_message_chunk" => {
            let block = &update["content"];
            let image = block["type"] == json!("image");
            let chunk = if image {
                String::new()
            } else {
                block_text(block)
            };
            match entries.last_mut() {
                Some(Entry::User { text, images, .. }) => {
                    text.push_str(&chunk);
                    *images += usize::from(image);
                }
                _ => entries.push(Entry::User {
                    text: chunk,
                    images: usize::from(image),
                    expanded: false,
                }),
            }
        }
        "tool_call" | "tool_call_update" => upsert_tool(entries, update),
        _ => return false,
    }
    true
}

fn tool_in<'a>(entries: &'a mut [Entry], id: &str) -> Option<&'a mut ToolCall> {
    entries.iter_mut().rev().find_map(|entry| match entry {
        Entry::Tool(tool) if tool.id == id => Some(tool),
        _ => None,
    })
}

/// A `tool_call` creates, a `tool_call_update` amends what it names — and
/// creates too, when the call it names was never announced.
fn upsert_tool(entries: &mut Vec<Entry>, update: &Value) {
    let id = text(&update["toolCallId"]);
    let content = update
        .get("content")
        .filter(|content| content.is_array())
        .map(tool_content);
    let locations = update.get("locations").filter(|l| l.is_array()).map(|l| {
        list(l)
            .map(|location| match location["line"].as_u64() {
                Some(line) => format!("{}:{line}", text(&location["path"])),
                None => text(&location["path"]),
            })
            .collect::<Vec<_>>()
    });
    if let Some(tool) = tool_in(entries, &id) {
        if let Some(title) = update["title"].as_str() {
            tool.title = title.to_string();
        }
        if let Some(kind) = update["kind"].as_str() {
            tool.kind = kind.to_string();
        }
        if let Some(status) = update["status"].as_str() {
            tool.status = status.to_string();
        }
        if let Some(content) = content {
            tool.content = content;
        }
        if let Some(locations) = locations {
            tool.locations = locations;
        }
        if !update["rawInput"].is_null() {
            tool.raw_input = update["rawInput"].clone();
        }
        tool.read_terminal(&update["_meta"]);
        return;
    }
    entries.push(Entry::Tool(ToolCall {
        title: update["title"].as_str().unwrap_or("Tool").to_string(),
        kind: update["kind"].as_str().unwrap_or("other").to_string(),
        status: update["status"].as_str().unwrap_or("pending").to_string(),
        content: content.unwrap_or_default(),
        locations: locations.unwrap_or_default(),
        raw_input: update["rawInput"].clone(),
        terminal: None,
        permission: None,
        expanded: None,
        id,
    }));
    if let Some(Entry::Tool(tool)) = entries.last_mut() {
        tool.read_terminal(&update["_meta"]);
    }
}

/// How a login method wants to be run in a terminal, when it does.
fn auth_launch(method: &Value) -> Option<AuthLaunch> {
    let env = |value: &Value| -> Vec<(String, String)> {
        value
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
            .collect()
    };
    let args = |value: &Value| -> Vec<String> {
        list(value)
            .filter_map(|arg| arg.as_str().map(str::to_string))
            .collect()
    };
    let named = &method["_meta"]["terminal-auth"];
    if let Some(command) = named["command"].as_str() {
        return Some(AuthLaunch {
            command: Some(command.to_string()),
            args: args(&named["args"]),
            env: env(&named["env"]),
        });
    }
    (method["type"] == json!("terminal")).then(|| AuthLaunch {
        command: None,
        args: args(&method["args"]),
        env: env(&method["env"]),
    })
}

/// `initialize`'s parameters: a chat, and nothing else — see the module.
fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false,
            // A command's output reported on its call, in pieces — see
            // `Terminal` —, and a login run in one of our terminals — see
            // `AuthLaunch`.
            "_meta": {
                "terminal_output": true,
                "terminal_output_delta": true,
                "terminal-auth": true
            },
            "auth": { "terminal": true },
            // Forms: what makes Claude's `AskUserQuestion` available at all —
            // without it the adapter takes the tool away, and the questions
            // come back as text to answer "by letter". See `Form`. And the
            // links an MCP server's sign-in opens in a browser.
            "elicitation": { "form": {}, "url": {} },
            // A subagent's work in a transcript of its own — see `Subagent`.
            "subagents": {},
            "session": {
                // Notices beside the reply, rather than bold lines inside it.
                "notices": {},
                // `/compact`'s progress and summary.
                "compaction": {},
                // Fast mode as a switch rather than an on/off select.
                "configOptions": { "boolean": {} }
            }
        },
        "clientInfo": {
            "name": "claudhub",
            "title": "Claudhub",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn notification(method: &str, params: Value) -> String {
    json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string()
}

fn response(id: Value, result: Result<Value, (i64, String)>) -> String {
    match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        }),
    }
    .to_string()
}

/// An error's message, with the detail agents often put in `data`.
fn error_text(error: &Value) -> String {
    let message = error["message"].as_str().unwrap_or("error");
    let detail = match &error["data"] {
        Value::String(detail) => Some(detail.as_str()),
        data => data["details"].as_str().or(data["message"].as_str()),
    };
    match detail {
        Some(detail) if !detail.is_empty() && !message.contains(detail) => {
            format!("{message}: {detail}")
        }
        _ => message.to_string(),
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

fn list(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

/// A content block as Markdown: text as it is, a resource by its name.
fn block_text(block: &Value) -> String {
    match block["type"].as_str() {
        Some("text") => text(&block["text"]),
        Some("resource_link") => format!("[{}]({})", text(&block["name"]), text(&block["uri"])),
        Some("resource") => match block["resource"]["text"].as_str() {
            Some(body) => format!("```\n{body}\n```"),
            None => text(&block["resource"]["uri"]),
        },
        Some("image") => "*(image)*".into(),
        Some("audio") => "*(audio)*".into(),
        _ => String::new(),
    }
}

fn tool_content(content: &Value) -> Vec<ToolContent> {
    list(content)
        .filter_map(|c| match c["type"].as_str()? {
            "content" => Some(ToolContent::Text(block_text(&c["content"]))),
            "diff" => Some(ToolContent::Diff {
                path: text(&c["path"]),
                old: c["oldText"].as_str().map(str::to_string),
                new: text(&c["newText"]),
            }),
            _ => None,
        })
        .collect()
}

fn parse_choices(options: &Value) -> Vec<Choice> {
    let mut choices = Vec::new();
    for option in list(options) {
        // A group holds its own options: flattened, the group being a label
        // the menu can do without.
        if option.get("options").is_some_and(Value::is_array) {
            let group = option["name"]
                .as_str()
                .or(option["group"].as_str())
                .map(str::to_string);
            choices.extend(list(&option["options"]).map(|o| Choice {
                value: text(&o["value"]),
                name: text(&o["name"]),
                group: group.clone(),
            }));
        } else {
            choices.push(Choice {
                value: text(&option["value"]),
                name: text(&option["name"]),
                group: None,
            });
        }
    }
    choices
}

fn parse_options(options: &Value) -> Vec<ConfigOption> {
    list(options)
        .filter_map(|o| {
            let kind = match o["type"].as_str()? {
                "select" => OptionKind::Select {
                    current: text(&o["currentValue"]),
                    choices: parse_choices(&o["options"]),
                },
                "boolean" => OptionKind::Boolean(o["currentValue"].as_bool().unwrap_or(false)),
                _ => return None,
            };
            Some(ConfigOption {
                id: text(&o["id"]),
                name: text(&o["name"]),
                category: o["category"].as_str().map(str::to_string),
                kind,
            })
        })
        .collect()
}

fn parse_modes(modes: &Value) -> Option<(String, Vec<Choice>)> {
    let current = modes.get("currentModeId")?.as_str()?.to_string();
    let all = list(&modes["availableModes"])
        .map(|m| Choice {
            value: text(&m["id"]),
            name: text(&m["name"]),
            group: None,
        })
        .collect();
    Some((current, all))
}

fn parse_plan(entries: &Value) -> Vec<PlanEntry> {
    list(entries)
        .map(|e| PlanEntry {
            content: text(&e["content"]),
            status: text(&e["status"]),
        })
        .collect()
}

/// A form the agent waits on (`elicitation/create`, form mode): a message
/// and fields described by a flat JSON Schema.
///
/// **Claude's `AskUserQuestion` arrives this way** — each question a select
/// (`oneOf` of titled options, or an array of `anyOf` for several picks),
/// followed by an optional free-text field of its own, the CLI's "Other" —
/// and so does anything else the agent asks through its MCP servers. The
/// shapes read here are the ones the protocol names; a field of another shape
/// is left out, and a form left with none is declined.
#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    /// The agent's request id, echoed on the answer.
    request: Value,
    pub message: String,
    pub fields: Vec<Field>,
    /// A form of the `url` mode: no field, a link to open — an MCP server's
    /// sign-in, most often.
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// The property's key — what the answer is filed under.
    pub key: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub kind: FieldKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    /// One of these.
    One(Vec<FieldChoice>),
    /// Any of these.
    Many(Vec<FieldChoice>),
    Text,
    Boolean,
    /// A number — `integer` when the schema says so.
    Number {
        integer: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldChoice {
    /// What is sent back.
    pub value: String,
    /// What is shown.
    pub title: String,
    pub description: Option<String>,
}

/// What the user gave a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Picks(Vec<String>),
    Text(String),
    Boolean(bool),
}

impl Form {
    /// What tells this form from the next one: the agent's request id.
    pub fn id(&self) -> String {
        self.request.to_string()
    }

    fn parse(request: Value, params: &Value) -> Option<Self> {
        if params["mode"] == json!("url") {
            let url = params["url"].as_str().filter(|url| !url.is_empty())?;
            return Some(Self {
                request,
                message: text(&params["message"]),
                fields: Vec::new(),
                url: Some(url.to_string()),
            });
        }
        let schema = &params["requestedSchema"];
        let fields: Vec<Field> = schema["properties"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(key, property)| {
                Some(Field {
                    key: key.clone(),
                    title: property["title"].as_str().map(str::to_string),
                    description: property["description"].as_str().map(str::to_string),
                    kind: field_kind(property)?,
                })
            })
            .collect();
        if fields.is_empty() {
            return None;
        }
        Some(Self {
            request,
            message: text(&params["message"]),
            fields,
            url: None,
        })
    }

    /// The answer's `content`: each field that was given something, in the
    /// type its schema asks for. An empty text, a number that does not read,
    /// no pick: left out, which is what "optional" means.
    pub fn content(&self, answers: &HashMap<String, Answer>) -> Value {
        let mut content = serde_json::Map::new();
        for field in &self.fields {
            let Some(answer) = answers.get(&field.key) else {
                continue;
            };
            let value = match (&field.kind, answer) {
                (FieldKind::One(_), Answer::Picks(picks)) => picks.first().map(|pick| json!(pick)),
                (FieldKind::Many(_), Answer::Picks(picks)) if !picks.is_empty() => {
                    Some(json!(picks))
                }
                (FieldKind::Text, Answer::Text(value)) if !value.trim().is_empty() => {
                    Some(json!(value.trim()))
                }
                (FieldKind::Boolean, Answer::Boolean(value)) => Some(json!(value)),
                (FieldKind::Number { integer: true }, Answer::Text(value)) => {
                    value.trim().parse::<i64>().ok().map(|n| json!(n))
                }
                (FieldKind::Number { integer: false }, Answer::Text(value)) => {
                    value.trim().parse::<f64>().ok().map(|n| json!(n))
                }
                _ => None,
            };
            if let Some(value) = value {
                content.insert(field.key.clone(), value);
            }
        }
        Value::Object(content)
    }
}

/// A property's kind, from the few shapes the protocol names.
fn field_kind(property: &Value) -> Option<FieldKind> {
    match property["type"].as_str()? {
        "string" => Some(match choices(property) {
            Some(choices) => FieldKind::One(choices),
            None => FieldKind::Text,
        }),
        "array" => choices(&property["items"]).map(FieldKind::Many),
        "boolean" => Some(FieldKind::Boolean),
        "number" => Some(FieldKind::Number { integer: false }),
        "integer" => Some(FieldKind::Number { integer: true }),
        _ => None,
    }
}

/// A select's options: titled (`oneOf` / `anyOf` of `const` and `title`), or
/// a bare `enum` with its optional `enumNames`.
fn choices(schema: &Value) -> Option<Vec<FieldChoice>> {
    let titled = schema["oneOf"].as_array().or(schema["anyOf"].as_array());
    if let Some(options) = titled {
        let choices: Vec<FieldChoice> = options
            .iter()
            .filter_map(|option| {
                let value = option["const"].as_str()?.to_string();
                Some(FieldChoice {
                    title: option["title"].as_str().unwrap_or(&value).to_string(),
                    description: option["description"].as_str().map(str::to_string),
                    value,
                })
            })
            .collect();
        return (!choices.is_empty()).then_some(choices);
    }
    let values = schema["enum"].as_array()?;
    let names = schema["enumNames"].as_array();
    let choices: Vec<FieldChoice> = values
        .iter()
        .enumerate()
        .filter_map(|(ix, value)| {
            let value = value.as_str()?.to_string();
            let title = names
                .and_then(|names| names.get(ix))
                .and_then(Value::as_str)
                .unwrap_or(&value)
                .to_string();
            Some(FieldChoice {
                value,
                title,
                description: None,
            })
        })
        .collect();
    (!choices.is_empty()).then_some(choices)
}

/// A prompt of the conversation: where it is, and what it reads as.
#[derive(Debug, PartialEq)]
pub struct Prompt<'a> {
    /// Its index in [`Chat::entries`].
    pub entry: usize,
    /// Its first line with something on it; `None` for images alone.
    pub summary: Option<&'a str>,
    pub images: usize,
}

/// The prompt being read, by its position among the prompts.
///
/// `tops` says where each one starts against the view's top: above it
/// (negative), in it, or `None` below it. The one read is the last to have
/// crossed `reading` — a line a third down, not the edge: the last prompt
/// sat in plain view under an older one still lit — and at the end of the
/// transcript, the last one whatever its place.
pub fn prompt_at(tops: &[Option<f32>], reading: f32, at_end: bool) -> Option<usize> {
    if at_end {
        return tops.len().checked_sub(1);
    }
    tops.iter()
        .rposition(|top| top.is_some_and(|top| top <= reading))
}

/// A message another Claude session sent into this one, as Claude Code
/// writes it into the conversation: `<agent-message from="…">`, after a line
/// that says so. A subagent's final report comes that way — a flush-left
/// preamble, then the report indented two spaces.
#[derive(Debug, PartialEq)]
pub struct AgentMessage<'a> {
    /// The sending session's agent id.
    pub from: &'a str,
    /// A subagent handing its work back.
    pub handback: bool,
    /// What it says: a report without its preamble and its indent.
    pub text: String,
}

/// Reads a user message as [`AgentMessage`]; `None` for one typed by hand.
/// Tolerates a message still streaming in, its closing tag not there yet.
pub fn agent_message(text: &str) -> Option<AgentMessage<'_>> {
    const OPEN: &str = "<agent-message from=\"";
    let start = text.find(OPEN)?;
    // The tag opens the message: one line at most before it.
    if text[..start].trim().contains('\n') {
        return None;
    }
    let rest = &text[start + OPEN.len()..];
    let from = &rest[..rest.find('"')?];
    let rest = &rest[from.len()..];
    let body = &rest[rest.find('>')? + 1..];
    let body = body.split("</agent-message>").next().unwrap_or(body);
    let handback = body.trim_start().starts_with("[Subagent hand-back]");
    let report = handback.then(|| {
        body.lines()
            .skip_while(|line| !line.starts_with("  "))
            .map(|line| line.strip_prefix("  ").unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n")
    });
    let text = match report {
        Some(report) if !report.trim().is_empty() => report.trim().to_string(),
        _ => body.trim().to_string(),
    };
    Some(AgentMessage {
        from,
        handback,
        text,
    })
}

/// The slash commands the composer offers for what is typed: a `/` and a
/// word with no space yet — the command's arguments are the agent's
/// business. Matched on any part of the name, case ignored, in the agent's
/// order.
pub fn command_matches<'a>(typed: &str, commands: &'a [Command]) -> Vec<&'a Command> {
    let Some(query) = typed.strip_prefix('/') else {
        return Vec::new();
    };
    if query.contains(char::is_whitespace) {
        return Vec::new();
    }
    let query = query.to_lowercase();
    commands
        .iter()
        .filter(|command| command.name.to_lowercase().contains(&query))
        .collect()
}

/// The mention being typed: the `@word` that ends the text — where its `@`
/// is, and what follows it. A word elsewhere is already written, and an
/// `@` inside a word (an address) is none.
pub fn mention_at_end(typed: &str) -> Option<(usize, &str)> {
    let start = typed.rfind(char::is_whitespace).map_or(0, |space| {
        space + typed[space..].chars().next().map_or(1, char::len_utf8)
    });
    let word = &typed[start..];
    let query = word.strip_prefix('@')?;
    Some((start, query))
}

/// The files a prompt mentions: every `@path` word naming one of `known`,
/// once each, in the order they appear. A trailing punctuation mark is the
/// sentence's, not the path's.
pub fn mentioned_files<'a>(
    typed: &str,
    known: &'a [std::path::PathBuf],
) -> Vec<&'a std::path::Path> {
    let mut found: Vec<&std::path::Path> = Vec::new();
    for word in typed.split_whitespace() {
        let Some(path) = word.strip_prefix('@') else {
            continue;
        };
        let path = path.trim_end_matches([',', '.', ';', ':', '!', '?', ')']);
        if let Some(file) = known.iter().find(|file| file.as_os_str() == path) {
            if !found.contains(&file.as_path()) {
                found.push(file);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(line: &str) -> Value {
        serde_json::from_str(line).unwrap()
    }

    /// A chat past its handshake, on session `s1`.
    fn ready() -> Chat {
        let mut chat = Chat::new("/work/tree");
        let init = sent(&chat.start());
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["params"]["clientCapabilities"]["terminal"], false);
        let answer = json!({
            "jsonrpc": "2.0", "id": init["id"],
            "result": { "protocolVersion": 1, "agentInfo": { "name": "claude-acp", "title": "Claude" } }
        });
        let out = chat.receive(&answer.to_string());
        assert_eq!(out.len(), 1);
        let new = sent(&out[0]);
        assert_eq!(new["method"], "session/new");
        assert_eq!(new["params"]["cwd"], "/work/tree");
        assert_eq!(new["params"]["mcpServers"], json!([]));
        assert_eq!(chat.status, Status::Starting(Phase::OpeningSession));
        let opened = json!({
            "jsonrpc": "2.0", "id": new["id"],
            "result": {
                "sessionId": "s1",
                "modes": { "currentModeId": "default", "availableModes": [
                    { "id": "default", "name": "Default" }, { "id": "plan", "name": "Plan" }
                ]},
                "configOptions": [
                    { "id": "model", "name": "Model", "type": "select", "currentValue": "b",
                      "options": [{ "value": "a", "name": "A" }, { "group": "g", "name": "G",
                                    "options": [{ "value": "b", "name": "B" }] }] },
                    { "id": "odd", "name": "Odd", "type": "slider" }
                ]
            }
        });
        assert!(chat.receive(&opened.to_string()).is_empty());
        assert_eq!(chat.status, Status::Ready);
        assert_eq!(chat.agent_name.as_deref(), Some("Claude"));
        chat
    }

    fn update(chat: &mut Chat, update: Value) {
        let line = json!({
            "jsonrpc": "2.0", "method": "session/update",
            "params": { "sessionId": "s1", "update": update }
        });
        assert!(chat.receive(&line.to_string()).is_empty());
    }

    #[test]
    fn the_handshake_opens_a_session_and_reads_its_settings() {
        let chat = ready();
        assert_eq!(chat.session.as_deref(), Some("s1"));
        assert_eq!(chat.modes.as_ref().unwrap().1.len(), 2);
        // The slider is unknown and left out; the group is flattened.
        assert_eq!(chat.options.len(), 1);
        match &chat.options[0].kind {
            OptionKind::Select { current, choices } => {
                assert_eq!(current, "b");
                assert_eq!(choices.len(), 2);
                assert_eq!(choices[1].name, "B");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_turn_streams_into_one_reply_and_ends_on_its_answer() {
        let mut chat = ready();
        assert!(chat.prompt("   ").is_none());
        let prompt = sent(&chat.prompt(" why? ").unwrap());
        assert_eq!(prompt["method"], "session/prompt");
        assert_eq!(prompt["params"]["prompt"][0]["text"], "why?");
        assert!(chat.is_busy());
        // A second prompt waits for the first.
        assert!(chat.prompt("again").is_none());
        update(
            &mut chat,
            json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "hm" } }),
        );
        for chunk in ["Because", " it is."] {
            update(
                &mut chat,
                json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": chunk } }),
            );
        }
        // Another session's news is none of ours.
        let stray = json!({ "jsonrpc": "2.0", "method": "session/update",
            "params": { "sessionId": "other", "update": { "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "noise" } } } });
        chat.receive(&stray.to_string());
        assert_eq!(chat.entries.len(), 3);
        assert_eq!(
            chat.entries[2],
            Entry::Agent {
                text: "Because it is.".into(),
                id: None
            }
        );
        let done = json!({ "jsonrpc": "2.0", "id": prompt["id"], "result": { "stopReason": "max_tokens" } });
        chat.receive(&done.to_string());
        assert!(chat.is_ready());
        assert_eq!(
            chat.entries.last(),
            Some(&Entry::Notice(Notice::TokenLimit))
        );
    }

    #[test]
    fn two_messages_are_two_only_when_the_agent_says_so() {
        let mut chat = ready();
        for (id, chunk) in [("m1", "one"), ("m1", " more"), ("m2", "two")] {
            update(
                &mut chat,
                json!({ "sessionUpdate": "agent_message_chunk", "messageId": id,
                        "content": { "type": "text", "text": chunk } }),
            );
        }
        assert_eq!(chat.entries.len(), 2);
    }

    #[test]
    fn a_tool_call_is_amended_by_its_updates() {
        let mut chat = ready();
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read a.rs",
                    "kind": "read", "status": "pending",
                    "locations": [{ "path": "/w/a.rs", "line": 3 }, { "path": "/w/b.rs" }] }),
        );
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
                    "content": [
                        { "type": "content", "content": { "type": "text", "text": "fn main() {}" } },
                        { "type": "diff", "path": "/w/a.rs", "oldText": null, "newText": "x" },
                        { "type": "terminal", "terminalId": "t" }
                    ] }),
        );
        let Entry::Tool(tool) = &chat.entries[0] else {
            panic!("{:?}", chat.entries);
        };
        assert_eq!(tool.title, "Read a.rs");
        assert_eq!(tool.status, "completed");
        assert_eq!(tool.locations, vec!["/w/a.rs:3", "/w/b.rs"]);
        assert_eq!(tool.content.len(), 2);
        assert_eq!(
            tool.content[1],
            ToolContent::Diff {
                path: "/w/a.rs".into(),
                old: None,
                new: "x".into()
            }
        );
    }

    #[test]
    fn the_timeline_lists_what_the_user_sent_and_knows_where_one_reads() {
        let user = |text: &str, images| Entry::User {
            text: text.into(),
            images,
            expanded: false,
        };
        let mut chat = Chat::new("/w".to_string());
        chat.entries = vec![
            user("\n  fix the tests\nplease", 0),
            Entry::Agent {
                text: "done".into(),
                id: None,
            },
            user("<agent-message from=\"a\">\nreport", 0),
            user("", 2),
        ];
        let prompts = chat.prompts();
        assert_eq!(
            prompts,
            vec![
                Prompt {
                    entry: 0,
                    summary: Some("fix the tests"),
                    images: 0
                },
                Prompt {
                    entry: 3,
                    summary: None,
                    images: 2
                },
            ]
        );
        // The first above the view, the second a quarter down, reading a
        // third down: the second is read.
        assert_eq!(prompt_at(&[Some(-300.), Some(150.)], 200., false), Some(1));
        // Still below the reading line, or below the view: the first.
        assert_eq!(prompt_at(&[Some(-300.), Some(450.)], 200., false), Some(0));
        assert_eq!(prompt_at(&[Some(-300.), None], 200., false), Some(0));
        // Nothing crossed yet; and at the end, the last one.
        assert_eq!(prompt_at(&[Some(500.)], 200., false), None);
        assert_eq!(prompt_at(&[Some(-300.), Some(450.)], 200., true), Some(1));
    }

    #[test]
    fn a_subagent_report_is_read_without_its_wrapper() {
        let sent = "Another Claude session sent a message:\n<agent-message from=\"adb79\">\n\
                    [Subagent hand-back] The text below is the final report.\n\
                    Notes above are not part of it.\n  J'ai fait les points.\n\n  **Commits**\n  - `397ebb7`\n\
                    </agent-message>";
        let message = agent_message(sent).unwrap();
        assert_eq!(message.from, "adb79");
        assert!(message.handback);
        assert_eq!(
            message.text,
            "J'ai fait les points.\n\n**Commits**\n- `397ebb7`"
        );
        // Streaming in, and from a session that is not a subagent.
        let other = agent_message("<agent-message from=\"x\">\nbonjour").unwrap();
        assert_eq!((other.handback, other.text.as_str()), (false, "bonjour"));
        // Typed by hand, or merely quoting one.
        assert_eq!(agent_message("bonjour"), None);
        assert_eq!(agent_message("a\nb\n<agent-message from=\"x\">"), None);
    }

    #[test]
    fn an_edit_opens_and_a_command_stays_folded_until_the_hand_says() {
        let mut chat = ready();
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "e", "title": "Edit a.rs",
                    "kind": "edit", "status": "completed",
                    "content": [{ "type": "diff", "path": "/w/a.rs", "oldText": "a", "newText": "b" }] }),
        );
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "c", "title": "ls",
                    "kind": "execute", "status": "completed", "rawInput": { "command": "ls" } }),
        );
        let expanded = |chat: &Chat, ix: usize| match &chat.entries[ix] {
            Entry::Tool(tool) => tool.is_expanded(),
            entry => panic!("{entry:?}"),
        };
        assert!(expanded(&chat, 0));
        assert!(!expanded(&chat, 1));
        // A press flips what is shown, whichever the default was.
        chat.toggle(0);
        chat.toggle(1);
        assert!(!expanded(&chat, 0));
        assert!(expanded(&chat, 1));
    }

    #[test]
    fn a_permission_is_asked_on_the_tool_and_answered_by_its_id() {
        let mut chat = ready();
        chat.prompt("edit it").unwrap();
        let ask = json!({
            "jsonrpc": "2.0", "id": "req-9", "method": "session/request_permission",
            "params": { "sessionId": "s1",
                "toolCall": { "toolCallId": "t2", "title": "Edit a.rs", "kind": "edit" },
                "options": [
                    { "optionId": "yes", "name": "Allow", "kind": "allow_once" },
                    { "optionId": "no", "name": "Reject", "kind": "reject_once" }
                ] }
        });
        assert!(chat.receive(&ask.to_string()).is_empty());
        assert!(chat.is_asking());
        let answer = sent(&chat.answer("t2", "yes").unwrap());
        assert_eq!(answer["id"], "req-9");
        assert_eq!(answer["result"]["outcome"]["optionId"], "yes");
        assert!(!chat.is_asking());
        // Answered once.
        assert!(chat.answer("t2", "yes").is_none());
    }

    #[test]
    fn a_stop_cancels_the_open_questions_and_the_turn() {
        let mut chat = ready();
        chat.prompt("go").unwrap();
        let ask = json!({
            "jsonrpc": "2.0", "id": 4, "method": "session/request_permission",
            "params": { "sessionId": "s1", "toolCall": { "toolCallId": "t3" },
                        "options": [{ "optionId": "yes", "name": "Allow", "kind": "allow_once" }] }
        });
        chat.receive(&ask.to_string());
        let lines: Vec<Value> = chat.cancel().iter().map(|l| sent(l)).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["id"], 4);
        assert_eq!(lines[0]["result"]["outcome"]["outcome"], "cancelled");
        assert_eq!(lines[1]["method"], "session/cancel");
        assert!(lines[1].get("id").is_none());
    }

    #[test]
    fn what_was_not_offered_is_refused_and_not_left_hanging() {
        let mut chat = ready();
        let ask = json!({ "jsonrpc": "2.0", "id": 12, "method": "fs/read_text_file",
                          "params": { "path": "/etc/passwd" } });
        let out = chat.receive(&ask.to_string());
        let answer = sent(&out[0]);
        assert_eq!(answer["id"], 12);
        assert_eq!(answer["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn an_agent_that_wants_a_login_asks_for_one() {
        let mut chat = Chat::new("/w");
        let init = sent(&chat.start());
        let answer = json!({ "jsonrpc": "2.0", "id": init["id"], "result": {
            "authMethods": [{ "id": "claude-login", "name": "Log in", "description": null }] } });
        let new = sent(&chat.receive(&answer.to_string())[0]);
        assert_eq!(chat.auth_methods[0].description, "");
        let refused = json!({ "jsonrpc": "2.0", "id": new["id"],
            "error": { "code": AUTH_REQUIRED, "message": "Authentication required" } });
        chat.receive(&refused.to_string());
        assert_eq!(chat.status, Status::AuthRequired);
        let auth = sent(&chat.authenticate("claude-login").unwrap());
        assert_eq!(auth["params"]["methodId"], "claude-login");
        let ok = json!({ "jsonrpc": "2.0", "id": auth["id"], "result": {} });
        let again = sent(&chat.receive(&ok.to_string())[0]);
        assert_eq!(again["method"], "session/new");
    }

    #[test]
    fn an_error_says_its_detail_and_a_death_its_reason() {
        let mut chat = Chat::new("/w");
        let init = sent(&chat.start());
        let failed = json!({ "jsonrpc": "2.0", "id": init["id"],
            "error": { "code": -32603, "message": "Internal error", "data": { "details": "no node" } } });
        chat.receive(&failed.to_string());
        assert_eq!(
            chat.status,
            Status::Failed("Internal error: no node".into())
        );

        let mut chat = ready();
        chat.prompt("x").unwrap();
        chat.ended(Some("exit code 1".into()));
        assert_eq!(chat.status, Status::Failed("exit code 1".into()));
        // An end the view asked for says nothing.
        let mut chat = ready();
        chat.ended(None);
        assert!(chat.is_ready());
    }

    #[test]
    fn settings_show_at_once_and_follow_the_agent() {
        let mut chat = ready();
        let line = sent(&chat.set_option("model", json!("a")).unwrap());
        assert_eq!(line["method"], "session/set_config_option");
        assert!(
            matches!(&chat.options[0].kind, OptionKind::Select { current, .. } if current == "a")
        );
        assert!(chat.set_option("model", json!(true)).is_none());
        assert!(chat.set_mode("nope").is_none());
        let mode = sent(&chat.set_mode("plan").unwrap());
        assert_eq!(mode["params"]["modeId"], "plan");
        update(
            &mut chat,
            json!({ "sessionUpdate": "current_mode_update", "currentModeId": "default" }),
        );
        assert_eq!(chat.modes.as_ref().unwrap().0, "default");
        update(
            &mut chat,
            json!({ "sessionUpdate": "plan", "entries": [
                { "content": "Read", "status": "completed", "priority": "high" },
                { "content": "Write", "status": "pending" } ] }),
        );
        assert_eq!(chat.plan.len(), 2);
        update(
            &mut chat,
            json!({ "sessionUpdate": "usage_update", "used": 1200, "size": 200000,
                    "cost": { "amount": 0.25, "currency": "USD" } }),
        );
        assert_eq!(
            chat.usage.as_ref().unwrap().cost,
            Some((0.25, "USD".into()))
        );
    }

    fn initialized(chat: &mut Chat, caps: Value) -> Value {
        let init = sent(&chat.start());
        let answer = json!({ "jsonrpc": "2.0", "id": init["id"],
                             "result": { "agentCapabilities": caps } });
        sent(&chat.receive(&answer.to_string())[0])
    }

    #[test]
    fn an_earlier_session_comes_back_with_its_history() {
        let mut chat = Chat::resuming("/w", "old");
        let load = initialized(&mut chat, json!({ "loadSession": true }));
        assert_eq!(load["method"], "session/load");
        assert_eq!(load["params"]["sessionId"], "old");
        assert_eq!(chat.status, Status::Starting(Phase::Resuming));
        // The replay arrives before the answer, and is ours.
        let replay = |kind: &str, text: &str| {
            json!({ "jsonrpc": "2.0", "method": "session/update", "params": {
                "sessionId": "old",
                "update": { "sessionUpdate": kind, "content": { "type": "text", "text": text } } } })
            .to_string()
        };
        chat.receive(&replay("user_message_chunk", "hello"));
        chat.receive(&replay("agent_message_chunk", "hi"));
        let done = json!({ "jsonrpc": "2.0", "id": load["id"], "result": {} });
        chat.receive(&done.to_string());
        assert!(chat.is_ready());
        assert_eq!(chat.session.as_deref(), Some("old"));
        assert_eq!(
            chat.entries[0],
            Entry::User {
                text: "hello".into(),
                images: 0,
                expanded: false
            }
        );
        assert_eq!(chat.entries.len(), 2);
    }

    #[test]
    fn without_load_a_session_resumes_and_says_what_is_missing() {
        let mut chat = Chat::resuming("/w", "old");
        let resume = initialized(
            &mut chat,
            json!({ "sessionCapabilities": { "resume": {} } }),
        );
        assert_eq!(resume["method"], "session/resume");
        let done = json!({ "jsonrpc": "2.0", "id": resume["id"], "result": {} });
        chat.receive(&done.to_string());
        assert_eq!(
            chat.entries,
            vec![Entry::Notice(Notice::ResumedWithoutHistory)]
        );
        // And an agent that can do neither opens a new one.
        let mut chat = Chat::resuming("/w", "old");
        let new = initialized(&mut chat, json!({ "loadSession": false }));
        assert_eq!(new["method"], "session/new");
        assert_eq!(chat.session, None);
    }

    #[test]
    fn a_session_that_will_not_come_back_gives_way_to_a_new_one() {
        let mut chat = Chat::resuming("/w", "old");
        let load = initialized(&mut chat, json!({ "loadSession": true }));
        let refused = json!({ "jsonrpc": "2.0", "id": load["id"],
            "error": { "code": -32603, "message": "Session not found" } });
        let out = chat.receive(&refused.to_string());
        assert_eq!(sent(&out[0])["method"], "session/new");
        assert_eq!(chat.session, None);
        assert_eq!(
            chat.entries,
            vec![Entry::Notice(Notice::NotResumed(
                "Session not found".into()
            ))]
        );
    }

    #[test]
    fn slash_offers_the_agents_commands_until_the_first_space() {
        let commands = vec![
            Command {
                name: "review".into(),
                description: "Review".into(),
            },
            Command {
                name: "compact".into(),
                description: "Compact".into(),
            },
            Command {
                name: "init".into(),
                description: String::new(),
            },
        ];
        let names = |typed: &str| -> Vec<String> {
            command_matches(typed, &commands)
                .iter()
                .map(|c| c.name.clone())
                .collect()
        };
        assert_eq!(names("/"), vec!["review", "compact", "init"]);
        assert_eq!(names("/RE"), vec!["review"]);
        assert_eq!(names("/co"), vec!["compact"]);
        assert!(names("/review now").is_empty());
        assert!(names("review").is_empty());
    }

    #[test]
    fn a_mention_is_the_word_being_typed() {
        assert_eq!(mention_at_end("look at @src/ma"), Some((8, "src/ma")));
        assert_eq!(mention_at_end("@"), Some((0, "")));
        assert_eq!(mention_at_end("é @x"), Some((3, "x")));
        assert_eq!(mention_at_end("mail me@host"), None);
        assert_eq!(mention_at_end("@done "), None);
        let known = vec![
            std::path::PathBuf::from("src/main.rs"),
            std::path::PathBuf::from("README.md"),
        ];
        let found = mentioned_files(
            "see @src/main.rs, and @README.md. @src/main.rs @nope",
            &known,
        );
        assert_eq!(
            found,
            vec![
                std::path::Path::new("src/main.rs"),
                std::path::Path::new("README.md")
            ]
        );
    }

    #[test]
    fn a_prompt_carries_the_files_it_mentions_as_links() {
        let mut chat = ready();
        let line = sent(
            &chat
                .prompt_with(
                    "read @a.rs",
                    &[("a.rs".into(), "file:///w/a.rs".into())],
                    &[],
                )
                .unwrap(),
        );
        let prompt = &line["params"]["prompt"];
        assert_eq!(prompt[1]["type"], "resource_link");
        assert_eq!(prompt[1]["uri"], "file:///w/a.rs");
    }

    #[test]
    fn the_history_lists_and_reopens_in_the_same_tab() {
        let mut chat = Chat::new("/w");
        let init = sent(&chat.start());
        let answer = json!({ "jsonrpc": "2.0", "id": init["id"], "result": {
            "agentCapabilities": { "loadSession": true, "sessionCapabilities": { "list": {} } } } });
        let new = sent(&chat.receive(&answer.to_string())[0]);
        let opened = json!({ "jsonrpc": "2.0", "id": new["id"], "result": { "sessionId": "s1" } });
        chat.receive(&opened.to_string());
        let list = sent(&chat.list_sessions().unwrap());
        assert_eq!(list["params"]["cwd"], "/w");
        let listed = json!({ "jsonrpc": "2.0", "id": list["id"], "result": { "sessions": [
            { "sessionId": "old", "title": "Fix the build", "updatedAt": "2026-09-30T10:00:00Z" },
            { "sessionId": "", "title": "broken" },
            { "sessionId": "blank", "title": "  " } ] } });
        chat.receive(&listed.to_string());
        let sessions = chat.sessions.clone().unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[1].title, None);
        // The one on show is not reopened.
        assert!(chat.reopen("s1", None).is_empty());
        let lines = chat.reopen("old", Some("Fix the build".into()));
        let load = sent(lines.last().unwrap());
        assert_eq!(load["method"], "session/load");
        assert_eq!(chat.title.as_deref(), Some("Fix the build"));
        assert!(chat.entries.is_empty());
        // And a new chat, once back.
        let done = json!({ "jsonrpc": "2.0", "id": load["id"], "result": {} });
        chat.receive(&done.to_string());
        let lines = chat.new_chat();
        assert_eq!(sent(lines.last().unwrap())["method"], "session/new");
        assert_eq!(chat.session, None);
    }

    #[test]
    fn a_command_reports_its_terminal_on_its_call() {
        let mut chat = ready();
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "t9", "title": "cargo test",
                    "kind": "execute", "status": "in_progress",
                    "rawInput": { "command": "cargo test", "description": "Run the tests" },
                    "_meta": { "terminal_info": { "terminal_id": "t9" } } }),
        );
        for piece in ["running 3 tests\n", "\u{1b}[32mok\u{1b}[0m\n"] {
            update(
                &mut chat,
                json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t9",
                        "_meta": { "terminal_output_delta": { "terminal_id": "t9", "data": piece } } }),
            );
        }
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t9", "status": "completed",
                    "content": [{ "type": "terminal", "terminalId": "t9" }],
                    "_meta": { "terminal_exit": { "terminal_id": "t9", "exit_code": 101, "signal": null } } }),
        );
        let Entry::Tool(tool) = &chat.entries[0] else {
            panic!("{:?}", chat.entries);
        };
        assert_eq!(tool.command().as_deref(), Some("cargo test"));
        assert!(tool.is_command());
        assert_eq!(tool.output().as_deref(), Some("running 3 tests\nok\n"));
        let exit = tool.terminal.as_ref().unwrap().exit.clone().unwrap();
        assert_eq!(exit.code, Some(101));
        assert!(!exit.succeeded());
        // The whole output replaces what was there.
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t9",
                    "_meta": { "terminal_output": { "terminal_id": "t9", "data": "all" } } }),
        );
        let Entry::Tool(tool) = &chat.entries[0] else {
            panic!();
        };
        assert_eq!(tool.output().as_deref(), Some("all"));
    }

    /// As Claude's adapter really sends a failed command (recorded from
    /// `claude-agent-acp` 0.84): the code in the output's first line, and
    /// 1 in `terminal_exit`.
    #[test]
    fn a_failed_command_ends_with_the_code_it_really_had() {
        let mut chat = ready();
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "b", "kind": "execute",
                    "title": "Terminal", "rawInput": {}, "status": "pending",
                    "_meta": { "terminal_info": { "terminal_id": "b" } } }),
        );
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "b",
                    "rawInput": { "command": "echo hi; exit 3" }, "title": "echo hi; exit 3" }),
        );
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "b",
                    "_meta": { "terminal_output_delta": { "terminal_id": "b",
                                                          "data": "Exit code 3\nhi" } } }),
        );
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "b", "status": "failed",
                    "_meta": { "terminal_exit": { "terminal_id": "b", "exit_code": 1, "signal": null } } }),
        );
        let Entry::Tool(tool) = &chat.entries[0] else {
            panic!();
        };
        assert_eq!(tool.command().as_deref(), Some("echo hi; exit 3"));
        assert_eq!(tool.output().as_deref(), Some("hi"));
        assert_eq!(
            tool.terminal.as_ref().unwrap().exit.as_ref().unwrap().code,
            Some(3)
        );
        assert_eq!(said_exit_code("Exit code x\n"), None);
    }

    #[test]
    fn without_the_extension_the_output_is_the_fenced_content() {
        let mut chat = ready();
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "kind": "execute",
                    "rawInput": { "command": ["bash", "-lc", "ls -la"] },
                    "content": [{ "type": "content",
                                  "content": { "type": "text", "text": "```console\na\nb\n```" } }] }),
        );
        let Entry::Tool(tool) = &chat.entries[0] else {
            panic!();
        };
        assert_eq!(tool.command().as_deref(), Some("ls -la"));
        assert_eq!(tool.output().as_deref(), Some("a\nb"));
        // A read is no command.
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "t2", "kind": "read",
                    "content": [{ "type": "content", "content": { "type": "text", "text": "x" } }] }),
        );
        let Entry::Tool(tool) = &chat.entries[1] else {
            panic!();
        };
        assert_eq!(tool.output(), None);
        let init = super::initialize_params();
        assert_eq!(
            init["clientCapabilities"]["_meta"]["terminal_output_delta"],
            true
        );
    }

    #[test]
    fn a_tail_says_how_much_it_left_out() {
        assert_eq!(tail("a\nb\nc\nd\n", 2), ("c\nd", 2));
        assert_eq!(tail("a\nb", 5), ("a\nb", 0));
        assert_eq!(tail("", 3), ("", 0));
        let mut long = "x".repeat(OUTPUT_KEPT) + "\nlast line";
        long.insert_str(0, "first\n");
        keep_tail(&mut long);
        assert!(long.len() <= OUTPUT_KEPT);
        assert!(long.ends_with("last line"));
    }

    /// As Claude's adapter really builds an `AskUserQuestion` form
    /// (`askUserQuestionsToCreateRequest`, claude-agent-acp 0.84).
    fn ask_user_question() -> Value {
        json!({
            "jsonrpc": "2.0", "id": 21, "method": "elicitation/create",
            "params": {
                "mode": "form", "sessionId": "s1", "toolCallId": "toolu_1",
                "message": "Please answer the following questions.",
                "requestedSchema": { "type": "object", "properties": {
                    "question_0": { "type": "string", "title": "Next", "description": "What now?",
                        "oneOf": [
                            { "const": "Merge", "title": "Merge", "description": "Run just ci, then merge" },
                            { "const": "Review", "title": "Review" } ] },
                    "question_0_custom": { "type": "string", "title": "Other",
                        "description": "Type your own answer (optional)." },
                    "question_1": { "type": "array", "title": "Checks",
                        "items": { "anyOf": [ { "const": "fmt", "title": "fmt" },
                                              { "const": "clippy", "title": "clippy" } ] } },
                    "ignored": { "type": "object" }
                } }
            }
        })
    }

    #[test]
    fn claude_asks_its_questions_as_a_form() {
        let init = super::initialize_params();
        assert!(init["clientCapabilities"]["elicitation"]["form"].is_object());
        let mut chat = ready();
        chat.prompt("what next?").unwrap();
        assert!(chat.receive(&ask_user_question().to_string()).is_empty());
        assert!(chat.is_asking());
        let form = chat.form.clone().unwrap();
        assert_eq!(form.fields.len(), 3);
        assert_eq!(form.fields[0].key, "question_0");
        match &form.fields[0].kind {
            FieldKind::One(choices) => {
                assert_eq!(
                    choices[0].description.as_deref(),
                    Some("Run just ci, then merge")
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(form.fields[1].kind, FieldKind::Text);
        assert!(matches!(form.fields[2].kind, FieldKind::Many(_)));
        // A second form while the first is open is declined, not hidden.
        let second = sent(&chat.receive(&ask_user_question().to_string())[0]);
        assert_eq!(second["result"]["action"], "decline");
        let answers = HashMap::from([
            (
                "question_0".to_string(),
                Answer::Picks(vec!["Merge".into()]),
            ),
            ("question_0_custom".to_string(), Answer::Text("  ".into())),
            (
                "question_1".to_string(),
                Answer::Picks(vec!["fmt".into(), "clippy".into()]),
            ),
        ]);
        let line = sent(&chat.answer_form(&answers).unwrap());
        assert_eq!(line["id"], 21);
        assert_eq!(line["result"]["action"], "accept");
        assert_eq!(
            line["result"]["content"],
            json!({ "question_0": "Merge", "question_1": ["fmt", "clippy"] })
        );
        assert!(!chat.is_asking());
    }

    #[test]
    fn a_form_is_skipped_declined_or_stopped_with_its_turn() {
        let mut chat = ready();
        chat.prompt("x").unwrap();
        chat.receive(&ask_user_question().to_string());
        let line = sent(&chat.decline_form().unwrap());
        assert_eq!(line["result"]["action"], "decline");
        chat.receive(&ask_user_question().to_string());
        let lines: Vec<Value> = chat.cancel().iter().map(|l| sent(l)).collect();
        assert_eq!(lines[0]["result"]["action"], "cancel");
        // A form with nothing this view can show is declined at once.
        let odd = json!({ "jsonrpc": "2.0", "id": 5, "method": "elicitation/create",
            "params": { "mode": "form", "message": "?", "requestedSchema": { "type": "object",
                "properties": { "x": { "type": "object" } } } } });
        let line = sent(&chat.receive(&odd.to_string())[0]);
        assert_eq!(line["result"]["action"], "decline");
        // And numbers read as numbers, enums as choices.
        let form = Form::parse(
            json!(1),
            &json!({ "message": "m", "requestedSchema": {
            "properties": {
                "n": { "type": "integer" },
                "e": { "type": "string", "enum": ["a", "b"], "enumNames": ["A"] } } } }),
        )
        .unwrap();
        let content = form.content(&HashMap::from([
            ("n".to_string(), Answer::Text(" 42 ".into())),
            ("e".to_string(), Answer::Picks(vec!["b".into()])),
        ]));
        assert_eq!(content, json!({ "n": 42, "e": "b" }));
        match &form.fields.iter().find(|f| f.key == "e").unwrap().kind {
            FieldKind::One(choices) => {
                assert_eq!(choices[0].title, "A");
                assert_eq!(choices[1].title, "b");
            }
            other => panic!("{other:?}"),
        }
    }

    /// A chat past its handshake with an agent that answers as Claude's
    /// adapter does: images, steering, queueing, subagents, delete and fork.
    fn ready_like_claude() -> Chat {
        let mut chat = Chat::new("/w");
        let init = sent(&chat.start());
        let answer = json!({ "jsonrpc": "2.0", "id": init["id"], "result": {
            "agentCapabilities": {
                "_meta": { "claudeCode": { "promptQueueing": true } },
                "promptCapabilities": { "image": true },
                "loadSession": true,
                "sessionCapabilities": { "delete": {}, "fork": {}, "list": {} } },
            "authMethods": [
                { "id": "claude-ai-login", "name": "Claude Subscription", "type": "terminal",
                  "args": ["--cli", "auth", "login", "--claudeai"],
                  "_meta": { "terminal-auth": { "command": "/usr/bin/node",
                      "args": ["/x/index.js", "--cli", "auth", "login", "--claudeai"],
                      "label": "Claude Login" } } },
                { "id": "console-login", "name": "Anthropic Console", "type": "terminal",
                  "args": ["--cli", "auth", "login", "--console"] },
                { "id": "gateway", "name": "Gateway" } ],
            "_meta": { "steering": { "supported": true } } } });
        let new = sent(&chat.receive(&answer.to_string())[0]);
        let opened = json!({ "jsonrpc": "2.0", "id": new["id"], "result": { "sessionId": "s1" } });
        chat.receive(&opened.to_string());
        chat
    }

    #[test]
    fn capabilities_offer_what_the_view_can_show() {
        let caps = &super::initialize_params()["clientCapabilities"];
        assert_eq!(caps["auth"]["terminal"], true);
        assert_eq!(caps["_meta"]["terminal-auth"], true);
        assert!(caps["elicitation"]["url"].is_object());
        assert!(caps["subagents"].is_object());
        assert!(caps["session"]["notices"].is_object());
        assert!(caps["session"]["compaction"].is_object());
        assert!(caps["session"]["configOptions"]["boolean"].is_object());
        // The login methods: named, the agent's own, or none.
        let chat = ready_like_claude();
        let launch = chat.auth_methods[0].launch.clone().unwrap();
        assert_eq!(launch.command.as_deref(), Some("/usr/bin/node"));
        assert_eq!(launch.args[0], "/x/index.js");
        let own = chat.auth_methods[1].launch.clone().unwrap();
        assert_eq!(own.command, None);
        assert_eq!(own.args, vec!["--cli", "auth", "login", "--console"]);
        assert_eq!(chat.auth_methods[2].launch, None);
    }

    #[test]
    fn a_message_during_a_turn_is_steered_into_it() {
        let mut chat = ready_like_claude();
        let first = sent(&chat.prompt("build it").unwrap());
        assert!(chat.is_busy() && chat.can_send());
        let steer = sent(&chat.prompt("and add a test").unwrap());
        assert_eq!(steer["method"], "_session/steering");
        assert_eq!(steer["params"]["prompt"][0]["text"], "and add a test");
        assert_eq!(
            steer["params"]["_meta"]["steering"]["idleBehavior"],
            "promptRequired"
        );
        let injected =
            json!({ "jsonrpc": "2.0", "id": steer["id"], "result": { "outcome": "injected" } });
        assert!(chat.receive(&injected.to_string()).is_empty());
        // The turn's answer ends it.
        let done =
            json!({ "jsonrpc": "2.0", "id": first["id"], "result": { "stopReason": "end_turn" } });
        chat.receive(&done.to_string());
        assert!(chat.is_ready());
        // A steer that finds the turn over is sent again as a prompt.
        chat.prompt("x").unwrap();
        let late = sent(&chat.prompt("late").unwrap());
        let over = json!({ "jsonrpc": "2.0", "id": late["id"], "result": { "outcome": "promptRequired" } });
        let again = sent(&chat.receive(&over.to_string())[0]);
        assert_eq!(again["method"], "session/prompt");
        assert_eq!(again["params"]["prompt"][0]["text"], "late");
    }

    #[test]
    fn without_steering_a_message_is_queued_and_the_turn_waits_for_both() {
        let mut chat = ready_like_claude();
        chat.steering = false;
        let first = sent(&chat.prompt("one").unwrap());
        let second = sent(&chat.prompt("two").unwrap());
        assert_eq!(second["method"], "session/prompt");
        let done = |id: &Value| {
            json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "end_turn" } })
                .to_string()
        };
        chat.receive(&done(&first["id"]));
        assert!(chat.is_busy());
        chat.receive(&done(&second["id"]));
        assert!(chat.is_ready());
        // An agent that does neither waits for the turn.
        chat.queueing = false;
        chat.prompt("three").unwrap();
        assert!(chat.prompt("four").is_none());
    }

    #[test]
    fn images_go_with_the_prompt_to_an_agent_that_reads_them() {
        let mut chat = ready_like_claude();
        let images = vec![("image/png".to_string(), "iVBOR".to_string())];
        let line = sent(&chat.prompt_with("", &[], &images).unwrap());
        assert_eq!(line["params"]["prompt"][0]["type"], "image");
        assert_eq!(line["params"]["prompt"][0]["mimeType"], "image/png");
        assert_eq!(
            chat.entries[0],
            Entry::User {
                text: String::new(),
                images: 1,
                expanded: false
            }
        );
        // One that does not is sent the text alone, and nothing is sent without it.
        let mut plain = ready();
        assert!(plain.prompt_with("", &[], &images).is_none());
        let line = sent(&plain.prompt_with("look", &[], &images).unwrap());
        assert_eq!(line["params"]["prompt"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_subagent_has_its_own_transcript_and_its_own_questions() {
        let mut chat = ready_like_claude();
        chat.prompt("explore").unwrap();
        update(
            &mut chat,
            json!({ "sessionUpdate": "subagent_spawned", "subagentSessionId": "sub1",
                    "name": "Explore", "task": "Find the parser", "capabilities": {} }),
        );
        let child = |update: Value| {
            json!({ "jsonrpc": "2.0", "method": "session/update",
                    "params": { "sessionId": "sub1", "update": update } })
            .to_string()
        };
        chat.receive(&child(
            json!({ "sessionUpdate": "tool_call", "toolCallId": "c1",
                                     "title": "Grep parse", "kind": "search" }),
        ));
        chat.receive(&child(json!({ "sessionUpdate": "agent_message_chunk",
                                     "content": { "type": "text", "text": "Found it." } })));
        assert_eq!(chat.entries.len(), 2, "{:?}", chat.entries);
        assert_eq!(chat.entries[1], Entry::Subagent("sub1".into()));
        let agent = chat.subagent("sub1").unwrap();
        assert_eq!(agent.name, "Explore");
        assert_eq!(agent.entries.len(), 2);
        // Its permission is asked on its own call.
        let ask = json!({ "jsonrpc": "2.0", "id": 30, "method": "session/request_permission",
            "params": { "sessionId": "sub1", "toolCall": { "toolCallId": "c2", "title": "Edit" },
                        "options": [{ "optionId": "ok", "name": "Allow", "kind": "allow_once" }] } });
        chat.receive(&ask.to_string());
        assert!(chat.is_asking());
        assert_eq!(chat.subagent("sub1").unwrap().entries.len(), 3);
        let answer = sent(&chat.answer("c2", "ok").unwrap());
        assert_eq!(answer["id"], 30);
        update(
            &mut chat,
            json!({ "sessionUpdate": "subagent_state_update", "subagentSessionId": "sub1",
                    "state": "completed" }),
        );
        assert_eq!(
            chat.subagent("sub1").unwrap().state.as_deref(),
            Some("completed")
        );
        chat.toggle(1);
        assert!(chat.subagent("sub1").unwrap().expanded);
    }

    #[test]
    fn notices_and_compaction_are_their_own_rows() {
        let mut chat = ready();
        update(
            &mut chat,
            json!({ "sessionUpdate": "notice", "severity": "warning",
                    "title": "Auto mode unavailable", "description": "Using Accept edits." }),
        );
        assert_eq!(
            chat.entries[0],
            Entry::Notice(Notice::Agent {
                severity: "warning".into(),
                title: "Auto mode unavailable".into(),
                description: Some("Using Accept edits.".into())
            })
        );
        update(
            &mut chat,
            json!({ "sessionUpdate": "compaction_update", "compactionId": "k", "status": "in_progress" }),
        );
        update(
            &mut chat,
            json!({ "sessionUpdate": "compaction_summary_chunk", "compactionId": "k",
                    "content": { "type": "text", "text": "We fixed " } }),
        );
        update(
            &mut chat,
            json!({ "sessionUpdate": "compaction_summary_chunk", "compactionId": "k",
                    "content": { "type": "text", "text": "the build." } }),
        );
        update(
            &mut chat,
            json!({ "sessionUpdate": "compaction_update", "compactionId": "k", "status": "completed" }),
        );
        let Entry::Compaction(compaction) = &chat.entries[1] else {
            panic!("{:?}", chat.entries);
        };
        assert_eq!(compaction.status, "completed");
        assert_eq!(compaction.summary, "We fixed the build.");
    }

    #[test]
    fn earlier_chats_are_deleted_and_copied() {
        let mut chat = ready_like_claude();
        chat.sessions = Some(vec![SessionSummary {
            id: "old".into(),
            title: Some("Old".into()),
            updated: None,
        }]);
        assert!(chat.delete_session("s1").is_none(), "not the one on show");
        let delete = sent(&chat.delete_session("old").unwrap());
        assert_eq!(delete["method"], "session/delete");
        chat.receive(&json!({ "jsonrpc": "2.0", "id": delete["id"], "result": {} }).to_string());
        assert!(chat.sessions.as_ref().unwrap().is_empty());
        let fork = sent(&chat.fork_session("s1", Some("Mine".into())).unwrap());
        assert_eq!(fork["method"], "session/fork");
        let copied = json!({ "jsonrpc": "2.0", "id": fork["id"], "result": { "sessionId": "s2" } });
        let load = sent(chat.receive(&copied.to_string()).last().unwrap());
        assert_eq!(load["method"], "session/load");
        assert_eq!(load["params"]["sessionId"], "s2");
        assert_eq!(chat.title.as_deref(), Some("Mine"));
    }

    #[test]
    fn a_link_to_open_is_a_form_without_fields() {
        let mut chat = ready();
        let ask = json!({ "jsonrpc": "2.0", "id": 9, "method": "elicitation/create",
            "params": { "mode": "url", "sessionId": "s1", "message": "Sign in to Linear",
                        "url": "https://linear.app/oauth", "elicitationId": "e1" } });
        assert!(chat.receive(&ask.to_string()).is_empty());
        let form = chat.form.clone().unwrap();
        assert_eq!(form.url.as_deref(), Some("https://linear.app/oauth"));
        assert!(form.fields.is_empty());
        let line = sent(&chat.accept_url().unwrap());
        assert_eq!(line["result"]["action"], "accept");
        // A url form without its url is declined.
        let bare = json!({ "jsonrpc": "2.0", "id": 10, "method": "elicitation/create",
            "params": { "mode": "url", "message": "?" } });
        assert_eq!(
            sent(&chat.receive(&bare.to_string())[0])["result"]["action"],
            "decline"
        );
    }

    /// As claude-agent-acp 0.84 really reports a subagent (recorded): the
    /// `Agent` call, and the subagent's calls naming it as their parent.
    #[test]
    fn the_agent_call_is_the_subagent_and_its_calls_are_filed_under_it() {
        let mut chat = ready_like_claude();
        chat.prompt("count").unwrap();
        let meta = |tool: &str, parent: Option<&str>| match parent {
            Some(parent) => {
                json!({ "claudeCode": { "toolName": tool, "parentToolUseId": parent } })
            }
            None => json!({ "claudeCode": { "toolName": tool } }),
        };
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "toolu_A", "title": "Task",
                    "kind": "think", "rawInput": {}, "_meta": meta("Agent", None) }),
        );
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "toolu_A",
                    "title": "Exécuter ls et compter les fichiers",
                    "rawInput": { "description": "Exécuter ls et compter les fichiers",
                                  "subagent_type": "general-purpose", "prompt": "…" },
                    "_meta": meta("Agent", None) }),
        );
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "toolu_B", "title": "ls -1 | wc -l",
                    "kind": "execute", "rawInput": { "command": "ls -1 | wc -l" },
                    "_meta": meta("Bash", Some("toolu_A")) }),
        );
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "toolu_A", "status": "completed",
                    "content": [{ "type": "content", "content": { "type": "text", "text": "3 files." } }],
                    "_meta": meta("Agent", None) }),
        );
        assert_eq!(
            chat.entries,
            vec![
                Entry::User {
                    text: "count".into(),
                    images: 0,
                    expanded: false
                },
                Entry::Subagent("toolu_A".into()),
            ]
        );
        let agent = chat.subagent("toolu_A").unwrap();
        assert_eq!(agent.name, "general-purpose");
        assert_eq!(agent.task, "Exécuter ls et compter les fichiers");
        assert_eq!(agent.state.as_deref(), Some("completed"));
        assert_eq!(agent.report, "3 files.");
        assert!(matches!(&agent.entries[0], Entry::Tool(tool) if tool.id == "toolu_B"));
        // A permission for one of its calls lands on that call, not beside it.
        let ask = json!({ "jsonrpc": "2.0", "id": 41, "method": "session/request_permission",
            "params": { "sessionId": "s1", "toolCall": { "toolCallId": "toolu_B" },
                        "options": [{ "optionId": "y", "name": "Allow", "kind": "allow_once" }] } });
        chat.receive(&ask.to_string());
        assert_eq!(chat.subagent("toolu_A").unwrap().entries.len(), 1);
        assert!(chat.is_asking());
        assert_eq!(sent(&chat.answer("toolu_B", "y").unwrap())["id"], 41);
        // And the `Agent` call's own question is the subagent's.
        let own = json!({ "jsonrpc": "2.0", "id": 42, "method": "session/request_permission",
            "params": { "sessionId": "s1", "toolCall": { "toolCallId": "toolu_A" },
                        "options": [{ "optionId": "y", "name": "Allow", "kind": "allow_once" }] } });
        chat.receive(&own.to_string());
        assert!(chat.subagent("toolu_A").unwrap().permission.is_some());
        assert_eq!(chat.entries.len(), 2);
        assert_eq!(sent(&chat.answer("toolu_A", "y").unwrap())["id"], 42);
    }

    #[test]
    fn the_turns_subagents_are_pinned_until_it_ends() {
        let mut chat = ready_like_claude();
        let agent_call = |id: &str, status: Option<&str>| {
            let mut call = json!({ "sessionUpdate": "tool_call", "toolCallId": id,
                "title": "Task", "rawInput": { "description": id },
                "_meta": { "claudeCode": { "toolName": "Agent" } } });
            if let Some(status) = status {
                call["sessionUpdate"] = json!("tool_call_update");
                call["status"] = json!(status);
            }
            call
        };
        let first = sent(&chat.prompt("one").unwrap());
        update(&mut chat, agent_call("old", None));
        update(&mut chat, agent_call("old", Some("completed")));
        let done = |id: &Value| {
            json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "end_turn" } })
                .to_string()
        };
        chat.receive(&done(&first["id"]));
        assert!(chat.pinned_subagents().is_empty(), "the turn is over");
        let second = sent(&chat.prompt("two").unwrap());
        update(&mut chat, agent_call("a", None));
        update(&mut chat, agent_call("b", None));
        update(&mut chat, agent_call("a", Some("completed")));
        // Ended or not, the turn's own stay, in the order they started.
        let pinned: Vec<(usize, &str)> = chat
            .pinned_subagents()
            .into_iter()
            .map(|(entry, agent)| (entry, agent.session.as_str()))
            .collect();
        assert_eq!(pinned, [(3, "a"), (4, "b")]);
        // One the turn left running is closed with it.
        chat.receive(&done(&second["id"]));
        assert!(chat.pinned_subagents().is_empty());
        assert_eq!(
            chat.subagent("b").unwrap().state.as_deref(),
            Some("cancelled")
        );
    }

    #[test]
    fn a_call_left_pending_when_the_turn_ends_is_closed() {
        let mut chat = ready();
        let prompt = sent(&chat.prompt("go").unwrap());
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "ghost", "title": "Terminal",
                    "kind": "execute", "status": "pending" }),
        );
        update(
            &mut chat,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "real", "kind": "execute",
                    "status": "completed" }),
        );
        let done =
            json!({ "jsonrpc": "2.0", "id": prompt["id"], "result": { "stopReason": "end_turn" } });
        chat.receive(&done.to_string());
        let statuses: Vec<&str> = chat
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Tool(tool) => Some(tool.status.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(statuses, vec!["cancelled", "completed"]);
    }

    #[test]
    fn a_line_that_is_not_json_is_ignored() {
        let mut chat = ready();
        assert!(chat.receive("npm WARN something").is_empty());
        assert!(chat
            .receive("{\"jsonrpc\":\"2.0\",\"id\":999,\"result\":{}}")
            .is_empty());
    }
}
