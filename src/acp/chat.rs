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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    pub description: String,
}

/// One value of a select — a mode, a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub name: String,
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
    pub kind: OptionKind,
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
    pub permission: Option<Permission>,
    pub expanded: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    User(String),
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
}

/// Context used and available, in tokens, and what the session has cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    pub used: u64,
    pub size: u64,
    pub cost: Option<(f64, String)>,
}

/// What an answer we wait on belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Initialize,
    NewSession,
    /// `session/load` (`true`, the history replayed) or `session/resume`.
    Resume(bool),
    Authenticate,
    Prompt,
    /// A mode or an option: only an error is read.
    Setting,
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
        self.entries
            .iter()
            .any(|entry| matches!(entry, Entry::Tool(tool) if tool.permission.is_some()))
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
                if params["sessionId"].as_str() == self.session.as_deref() {
                    self.update(&params["update"]);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The process is gone. `None` when the view ended it.
    pub fn ended(&mut self, reason: Option<String>) {
        self.pending.clear();
        self.drop_permissions();
        if let Some(reason) = reason {
            self.status = Status::Failed(reason);
        }
    }

    /// Sends what the user typed.
    pub fn prompt(&mut self, text: &str) -> Option<String> {
        let text = text.trim();
        if text.is_empty() || !self.is_ready() {
            return None;
        }
        let session = self.session.clone()?;
        self.entries.push(Entry::User(text.to_string()));
        self.status = Status::Busy;
        Some(self.request(
            Pending::Prompt,
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{ "type": "text", "text": text }] }),
        ))
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
        let permission = self.tool_mut(tool)?.permission.take()?;
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

    /// Folds or unfolds a thought or a tool call.
    pub fn toggle(&mut self, ix: usize) {
        match self.entries.get_mut(ix) {
            Some(Entry::Thought { expanded, .. }) => *expanded = !*expanded,
            Some(Entry::Tool(tool)) => tool.expanded = !tool.expanded,
            _ => {}
        }
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
                if !replayed {
                    self.entries
                        .push(Entry::Notice(Notice::ResumedWithoutHistory));
                }
                self.session_opened(&result);
                Vec::new()
            }
            (Pending::Prompt, Ok(result)) => {
                self.status = Status::Ready;
                self.drop_permissions();
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
                self.status = Status::Ready;
                self.drop_permissions();
                self.entries.push(Entry::Notice(Notice::Error(message)));
                Vec::new()
            }
            (Pending::Setting, Err((_, message))) => {
                self.entries.push(Entry::Notice(Notice::Error(message)));
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
        if method != "session/request_permission" {
            // Nothing else was offered at `initialize`; an answer is owed all
            // the same, or the agent waits for ever.
            return vec![response(
                id,
                Err((METHOD_NOT_FOUND, format!("{method} is not supported"))),
            )];
        }
        let tool = &params["toolCall"];
        self.upsert_tool(tool);
        let options = list(&params["options"])
            .map(|o| PermissionOption {
                id: text(&o["optionId"]),
                name: text(&o["name"]),
                kind: text(&o["kind"]),
            })
            .collect();
        let tool_id = text(&tool["toolCallId"]);
        match self.tool_mut(&tool_id) {
            Some(tool) => {
                tool.permission = Some(Permission {
                    request: id,
                    options,
                });
                tool.expanded = true;
                Vec::new()
            }
            None => vec![response(
                id,
                Ok(json!({ "outcome": { "outcome": "cancelled" } })),
            )],
        }
    }

    /// The open questions, answered `cancelled`.
    fn cancel_permissions(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        for entry in &mut self.entries {
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

    /// The open questions, forgotten: the turn they belonged to is over.
    fn drop_permissions(&mut self) {
        for entry in &mut self.entries {
            if let Entry::Tool(tool) = entry {
                tool.permission = None;
            }
        }
    }

    // — The session's news ——————————————————————————————————————————

    fn update(&mut self, update: &Value) {
        match update["sessionUpdate"].as_str().unwrap_or_default() {
            "agent_message_chunk" => {
                let chunk = block_text(&update["content"]);
                let id = update["messageId"].as_str().map(str::to_string);
                match self.entries.last_mut() {
                    Some(Entry::Agent { text, id: last }) if id.is_none() || *last == id => {
                        text.push_str(&chunk)
                    }
                    _ => self.entries.push(Entry::Agent { text: chunk, id }),
                }
            }
            "agent_thought_chunk" => {
                let chunk = block_text(&update["content"]);
                match self.entries.last_mut() {
                    Some(Entry::Thought { text, .. }) => text.push_str(&chunk),
                    _ => self.entries.push(Entry::Thought {
                        text: chunk,
                        expanded: false,
                    }),
                }
            }
            "user_message_chunk" => {
                let chunk = block_text(&update["content"]);
                match self.entries.last_mut() {
                    Some(Entry::User(text)) => text.push_str(&chunk),
                    _ => self.entries.push(Entry::User(chunk)),
                }
            }
            "tool_call" | "tool_call_update" => self.upsert_tool(update),
            "plan" => self.plan = parse_plan(&update["entries"]),
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

    fn tool_mut(&mut self, id: &str) -> Option<&mut ToolCall> {
        self.entries.iter_mut().rev().find_map(|entry| match entry {
            Entry::Tool(tool) if tool.id == id => Some(tool),
            _ => None,
        })
    }

    /// A `tool_call` creates, a `tool_call_update` amends what it names — and
    /// creates too, when the call it names was never announced.
    fn upsert_tool(&mut self, update: &Value) {
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
        if let Some(tool) = self.tool_mut(&id) {
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
            return;
        }
        self.entries.push(Entry::Tool(ToolCall {
            title: update["title"].as_str().unwrap_or("Tool").to_string(),
            kind: update["kind"].as_str().unwrap_or("other").to_string(),
            status: update["status"].as_str().unwrap_or("pending").to_string(),
            content: content.unwrap_or_default(),
            locations: locations.unwrap_or_default(),
            permission: None,
            expanded: false,
            id,
        }));
    }
}

/// `initialize`'s parameters: a chat, and nothing else — see the module.
fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false
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
            choices.extend(list(&option["options"]).map(|o| Choice {
                value: text(&o["value"]),
                name: text(&o["name"]),
            }));
        } else {
            choices.push(Choice {
                value: text(&option["value"]),
                name: text(&option["name"]),
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
        assert_eq!(chat.entries[0], Entry::User("hello".into()));
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
    fn a_line_that_is_not_json_is_ignored() {
        let mut chat = ready();
        assert!(chat.receive("npm WARN something").is_empty());
        assert!(chat
            .receive("{\"jsonrpc\":\"2.0\",\"id\":999,\"result\":{}}")
            .is_empty());
    }
}
