//! A chat tab: an agent spoken to over ACP, beside the terminals.
//!
//! The protocol is `acp::chat::Chat`, pure; this is its face, drawn after the
//! AI panel of Tusk (`alpcanaydin/tusk`), whose client this one follows:
//!
//! - **a header** — the conversation's title, the context used, a menu for a
//!   new chat (this agent, or another one in this tab) and the history of the
//!   agent's earlier conversations (`session/list`);
//! - **the transcript**, a virtual list that follows its tail and offers the
//!   way back to it (`MessageScroller`): the user's turns in a frame, the
//!   agent's as selectable Markdown — one `TextViewState` per message, fed
//!   only what arrived since —, thoughts and tool calls as folding rows, the
//!   permissions the agent waits on as buttons on their call;
//! - **the plan**, folded to its progress and its current step;
//! - **the composer**: `/` offers the agent's commands, `@` the worktree's
//!   files (sent as links the agent opens itself), the arrows and Tab pick,
//!   Enter sends, Shift+Enter breaks the line, Escape stops a turn; under it
//!   the session's options, and the model — searchable, a list can run to the
//!   hundreds — beside send.
//!
//! **Nothing here moves on its own.** Tusk shimmers "Thinking" and fades each
//! streamed chunk in; both ask for frames for as long as the agent works, and
//! in this window a steady colour says the same thing for nothing.
//!
//! The view never sends a command itself: what the chat has to write goes out
//! as a [`ChatEvent`], which the application turns into `Cmd::AcpSend` — the
//! view is updated from inside the application's own event pump, where
//! reaching back into the application would be a double borrow.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    message_scroller::{MessageScroller, MessageScrollerState},
    popover::Popover,
    text::{TextView, TextViewState, TextViewStyle},
    v_flex, ActiveTheme, Disableable as _, Selectable as _, Sizable as _,
};
use gpui_kit::{
    actions, div, prelude::*, px, rems, uniform_list, AnyElement, App, ClipboardItem, Context,
    Entity, EventEmitter, FocusHandle, Focusable, HighlightStyle, Hsla, KeyBinding, MouseButton,
    ScrollStrategy, SharedString, StyleRefinement, Subscription, UniformListScrollHandle,
    WeakEntity, Window,
};
use serde_json::Value;

use crate::acp::chat::{
    command_matches, mention_at_end, mentioned_files, Answer, Chat, Choice, ConfigOption, Entry,
    Field, FieldKind, Notice, OptionKind, Phase, Status, ToolCall, ToolContent,
};
use crate::tr;
use crate::ui::icons::icon;

actions!(
    claudhub_chat,
    [
        /// The row above in the composer's list, or the caret up.
        ChatPickUp,
        /// The row below, or the caret down.
        ChatPickDown,
        /// The row picked into the text (Tab).
        ChatPickConfirm,
        /// Stops the turn under way (Escape).
        ChatEscape
    ]
);

/// The composer's key context: its keys win over the field's own only while
/// a list is open or a turn runs, and hand the key back otherwise.
const COMPOSER: &str = "ClaudhubChat";
/// The option picker's.
const PICKER: &str = "ClaudhubChatPicker";

/// The composer's keys, installed with the window's (`shortcuts::install`).
/// Not in the shortcut table: they are a list's navigation, as a field's
/// arrows are the field's, and they give the key back when there is no list.
pub fn key_bindings() -> Vec<KeyBinding> {
    let composer = Some("ClaudhubChat > Input");
    let picker = Some("ClaudhubChatPicker > Input");
    vec![
        KeyBinding::new("up", ChatPickUp, composer),
        KeyBinding::new("down", ChatPickDown, composer),
        KeyBinding::new("tab", ChatPickConfirm, composer),
        KeyBinding::new("escape", ChatEscape, composer),
        KeyBinding::new("up", ChatPickUp, picker),
        KeyBinding::new("down", ChatPickDown, picker),
    ]
}

/// What the view asks of the application.
pub enum ChatEvent {
    /// Lines for the agent, in order.
    Send(Vec<String>),
    /// Launch the agent again — it failed or died, or the tab now speaks to
    /// another one (`agent` has changed).
    Restart,
    /// The worktree's files, for the `@` list.
    WantFiles,
    /// A login to run in a terminal — the agent's own `auth login`.
    Login {
        program: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
}

/// An image pasted into the composer, waiting to go with the prompt.
struct Pasted {
    mime: &'static str,
    /// Base64, as the prompt carries it.
    data: String,
    bytes: usize,
}

/// The formats Claude reads, and the size past which the API refuses one.
const IMAGE_FORMATS: [gpui_kit::ImageFormat; 4] = [
    gpui_kit::ImageFormat::Png,
    gpui_kit::ImageFormat::Jpeg,
    gpui_kit::ImageFormat::Gif,
    gpui_kit::ImageFormat::Webp,
];
const IMAGE_MAX: usize = 5 * 1024 * 1024;

/// How many files the `@` list offers.
const MENTIONS: usize = 50;

/// How many diff rows a tool call shows before it stops.
const DIFF_ROWS: usize = 200;

/// Lines of context kept around a diff's changes.
const DIFF_CONTEXT: usize = 2;

/// What the composer's list offers for the text so far.
enum Suggest {
    Commands(Vec<(String, String)>),
    Files(Vec<String>),
}

impl Suggest {
    fn len(&self) -> usize {
        match self {
            Self::Commands(rows) => rows.len(),
            Self::Files(rows) => rows.len(),
        }
    }
}

pub struct ChatView {
    /// The number the lane knows this chat by — counted for the window.
    pub id: u64,
    pub agent: crate::acp::Agent,
    pub worktree: PathBuf,
    chat: Chat,
    /// The session the next `start` reopens — a kept tab's, or this one's
    /// own when its agent is restarted.
    resume: Option<String>,
    input: Entity<TextareaState>,
    scroller: Entity<MessageScrollerState>,
    /// One Markdown state per agent message or thought, beside the entry it
    /// renders, and how much of its text it has been fed: streaming appends,
    /// and a state re-parsed from the start at every chunk would cost the
    /// whole message each time.
    markdown: Vec<Option<(Entity<TextViewState>, usize)>>,
    /// The highlighted row of the `/` or `@` list.
    pick: usize,
    history_open: bool,
    plan_open: bool,
    /// The worktree's files, relative, for `@` — handed over by the
    /// application once asked (`ChatEvent::WantFiles`).
    files: Rc<Vec<PathBuf>>,
    files_asked: bool,
    /// The select whose picker is open, and the picker.
    picker: Option<(String, Entity<OptionPicker>)>,
    /// What the user has given the form the agent waits on so far.
    form: Option<FormState>,
    /// Images pasted into the composer, sent with the next prompt.
    pasted: Vec<Pasted>,
    /// The earlier chat whose deletion waits on a second press.
    deleting: Option<String>,
    /// A login was launched in a terminal: the auth screen says to come back.
    login_launched: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ChatEvent> for ChatView {}

impl ChatView {
    pub fn new(
        id: u64,
        agent: crate::acp::Agent,
        worktree: PathBuf,
        kept: Option<crate::ui::store::SavedChat>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 12)
                .submit_on_enter(true)
                .placeholder(tr!("chat-placeholder"))
        });
        // `submit_on_enter` turns a plain Enter into `PressEnter`; Shift+Enter
        // still breaks the line.
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::PressEnter { shift: false, .. } => this.submit(window, cx),
                InputEvent::Change => {
                    this.pick = 0;
                    this.ask_files_if_mentioning(cx);
                    cx.notify();
                }
                _ => {}
            });
        let mut chat = Chat::new(worktree.to_string_lossy().into_owned());
        let resume = kept.as_ref().and_then(|kept| kept.session.clone());
        chat.title = kept.and_then(|kept| kept.title);
        Self {
            id,
            agent,
            worktree,
            chat,
            resume,
            input,
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            markdown: Vec::new(),
            pick: 0,
            history_open: false,
            plan_open: false,
            files: Rc::new(Vec::new()),
            files_asked: false,
            picker: None,
            form: None,
            pasted: Vec::new(),
            deleting: None,
            login_launched: false,
            _subscriptions: vec![subscription],
        }
    }

    /// The first line, once the application has asked for the process.
    ///
    /// A restart comes back to the conversation it had: the session the chat
    /// had reached, or the one it was still to reopen.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        let cwd = self.worktree.to_string_lossy().into_owned();
        let title = self.chat.title.clone();
        let resume = self.chat.session.clone().or(self.resume.take());
        self.chat = match resume {
            Some(session) => Chat::resuming(cwd, session),
            None => Chat::new(cwd),
        };
        self.chat.title = title;
        self.history_open = false;
        let line = self.chat.start();
        self.sync(cx);
        cx.emit(ChatEvent::Send(vec![line]));
        cx.notify();
    }

    /// What a restart needs to open this chat again.
    pub fn saved(&self, right: bool) -> crate::ui::store::SavedChat {
        crate::ui::store::SavedChat {
            agent: self.agent.clone(),
            session: self.chat.session.clone().or(self.resume.clone()),
            title: self.chat.title.clone(),
            right,
        }
    }

    /// Whether a turn is under way.
    pub fn is_busy(&self) -> bool {
        self.chat.is_busy()
    }

    /// Whether the agent waits on a permission.
    pub fn is_asking(&self) -> bool {
        self.chat.is_asking()
    }

    /// The tab's title: the session's, or the agent's name.
    pub fn label(&self) -> SharedString {
        match &self.chat.title {
            Some(title) if !title.trim().is_empty() => SharedString::from(title.clone()),
            _ => SharedString::from(self.agent.label().to_string()),
        }
    }

    /// A line from the agent.
    pub fn receive(&mut self, line: &str, cx: &mut Context<Self>) {
        let out = self.chat.receive(line);
        self.sync(cx);
        self.send(out, cx);
    }

    /// The agent's process is gone.
    pub fn ended(&mut self, reason: Option<String>, cx: &mut Context<Self>) {
        self.chat.ended(reason);
        self.sync(cx);
    }

    /// The worktree's files, for the `@` list.
    pub fn set_files(&mut self, files: Rc<Vec<PathBuf>>, cx: &mut Context<Self>) {
        self.files = files;
        cx.notify();
    }

    fn send(&mut self, lines: Vec<String>, cx: &mut Context<Self>) {
        if !lines.is_empty() {
            cx.emit(ChatEvent::Send(lines));
        }
        cx.notify();
    }

    /// Brings the Markdown states and the list in line with the entries.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let entries = &self.chat.entries;
        self.markdown.truncate(entries.len());
        for (ix, entry) in entries.iter().enumerate() {
            let text = match entry {
                Entry::Agent { text, .. } | Entry::Thought { text, .. } => Some(text.as_str()),
                _ => None,
            };
            match (text, self.markdown.get_mut(ix)) {
                (Some(text), Some(Some((state, fed)))) => {
                    if *fed == text.len() {
                        continue;
                    }
                    // What streamed in is appended; anything else — a text
                    // that changed behind — is read again whole.
                    if text.len() > *fed && text.is_char_boundary(*fed) {
                        let more = text[*fed..].to_string();
                        state.update(cx, |state, cx| state.push_str(&more, cx));
                    } else {
                        let all = text.to_string();
                        state.update(cx, |state, cx| state.set_text(&all, cx));
                    }
                    *fed = text.len();
                }
                (Some(text), slot) => {
                    let state = cx.new(|cx| TextViewState::markdown(text, cx).selectable(true));
                    let fresh = Some((state, text.len()));
                    match slot {
                        Some(slot) => *slot = fresh,
                        None => self.markdown.push(fresh),
                    }
                }
                (None, Some(slot)) => *slot = None,
                (None, None) => self.markdown.push(None),
            }
        }
        let count = entries.len();
        self.scroller.update(cx, |scroller, cx| {
            let have = scroller.item_count();
            if count > have {
                scroller.append(count - have, cx);
            } else if count < have {
                scroller.reset(count, cx);
            }
            // Streaming grows the last rows, and a tool call is amended a few
            // rows back: those are measured again, not the whole transcript.
            scroller.remeasure_items(count.saturating_sub(8)..count, cx);
        });
        cx.notify();
    }

    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.chat.toggle(ix);
        self.scroller.update(cx, |scroller, cx| {
            scroller.remeasure_items(ix..ix + 1, cx);
        });
        cx.notify();
    }

    // — The composer ————————————————————————————————————————————————

    fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// The `@` list needs the files: asked once, the first time an `@` is
    /// being typed.
    fn ask_files_if_mentioning(&mut self, cx: &mut Context<Self>) {
        if self.files_asked || !self.files.is_empty() {
            return;
        }
        if mention_at_end(&self.text(cx)).is_some() {
            self.files_asked = true;
            cx.emit(ChatEvent::WantFiles);
        }
    }

    fn suggest(&self, cx: &App) -> Option<Suggest> {
        let text = self.text(cx);
        if text.starts_with('/') {
            let rows: Vec<(String, String)> = command_matches(&text, &self.chat.commands)
                .into_iter()
                .map(|command| (command.name.clone(), command.description.clone()))
                .collect();
            return (!rows.is_empty()).then_some(Suggest::Commands(rows));
        }
        let (_, query) = mention_at_end(&text)?;
        let rows: Vec<String> = if query.is_empty() {
            self.files
                .iter()
                .take(MENTIONS)
                .map(|path| path.to_string_lossy().into_owned())
                .collect()
        } else {
            let mut candidates = Vec::with_capacity(self.files.len());
            let mut origins = Vec::with_capacity(self.files.len());
            for (index, path) in self.files.iter().enumerate() {
                if let Some(text) = path.to_str() {
                    candidates.push(text);
                    origins.push(index);
                }
            }
            crate::ui::quick::rank(query, &candidates)
                .into_iter()
                .take(MENTIONS)
                .map(|hit| {
                    self.files[origins[hit.index]]
                        .to_string_lossy()
                        .into_owned()
                })
                .filter(|path| path != query)
                .collect()
        };
        (!rows.is_empty()).then_some(Suggest::Files(rows))
    }

    fn pick_step(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let Some(suggest) = self.suggest(cx) else {
            return false;
        };
        let count = suggest.len() as isize;
        self.pick = (self.pick as isize + delta).rem_euclid(count) as usize;
        cx.notify();
        true
    }

    /// The highlighted row, written into the text. `false` when no list is
    /// open.
    fn pick_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = self.text(cx);
        let written = match self.suggest(cx) {
            None => return false,
            Some(Suggest::Commands(rows)) => rows
                .get(self.pick.min(rows.len() - 1))
                .map(|(name, _)| format!("/{name} ")),
            Some(Suggest::Files(rows)) => rows.get(self.pick.min(rows.len() - 1)).map(|path| {
                let at = mention_at_end(&text).map_or(text.len(), |(at, _)| at);
                format!("{}@{path} ", &text[..at])
            }),
        };
        if let Some(written) = written {
            self.input.update(cx, |input, cx| {
                let end = written.len();
                input.set_value(written, window, cx);
                input.set_selected_range(end..end, cx);
            });
            self.pick = 0;
            cx.notify();
        }
        true
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // An open list: Enter picks from it.
        if self.pick_confirm(window, cx) {
            return;
        }
        let text = self.text(cx);
        let links: Vec<(String, String)> = mentioned_files(&text, &self.files)
            .into_iter()
            .map(|path| {
                let absolute = crate::wslpath::join(&self.worktree, path);
                (
                    path.to_string_lossy().into_owned(),
                    crate::lsp::uri::of(&absolute),
                )
            })
            .collect();
        let images: Vec<(String, String)> = self
            .pasted
            .iter()
            .map(|image| (image.mime.to_string(), image.data.clone()))
            .collect();
        let Some(line) = self.chat.prompt_with(&text, &links, &images) else {
            return;
        };
        self.pasted.clear();
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.history_open = false;
        self.sync(cx);
        self.scroller
            .update(cx, |scroller, cx| scroller.scroll_to_end(cx));
        self.send(vec![line], cx);
    }

    /// Text handed over from elsewhere in the window — a note, a failing
    /// test, a Sentry issue: sent when the chat can take it, left in the
    /// composer otherwise.
    pub fn deliver(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(line) = self.chat.prompt_with(text, &[], &[]) {
            self.sync(cx);
            self.scroller
                .update(cx, |scroller, cx| scroller.scroll_to_end(cx));
            self.send(vec![line], cx);
            return;
        }
        let text = text.to_string();
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
        cx.notify();
    }

    /// Takes the images out of a paste, when the agent reads them: `true`
    /// when the paste was images and nothing else is to be done with it.
    fn paste_images(&mut self, item: &gpui_kit::ClipboardItem, cx: &mut Context<Self>) -> bool {
        if !self.chat.takes_images() {
            return false;
        }
        let mut took = false;
        for entry in item.entries() {
            let gpui_kit::ClipboardEntry::Image(image) = entry else {
                continue;
            };
            if !IMAGE_FORMATS.contains(&image.format) || image.bytes.len() > IMAGE_MAX {
                continue;
            }
            use base64::Engine as _;
            self.pasted.push(Pasted {
                mime: image.format.mime_type(),
                data: base64::engine::general_purpose::STANDARD.encode(&image.bytes),
                bytes: image.bytes.len(),
            });
            took = true;
        }
        if took {
            cx.notify();
        }
        took
    }

    /// Runs one of the agent's logins in a terminal.
    fn login(&mut self, launch: &crate::acp::chat::AuthLaunch, cx: &mut Context<Self>) {
        let (program, args) = match &launch.command {
            Some(command) => (command.clone(), launch.args.clone()),
            // The agent's own command, the method's arguments after its own.
            None => {
                let mut args = self.agent.args.clone();
                args.extend(launch.args.iter().cloned());
                (self.agent.command.clone(), args)
            }
        };
        self.login_launched = true;
        cx.emit(ChatEvent::Login {
            program,
            args,
            env: launch.env.clone(),
        });
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let out = self.chat.cancel();
        self.send(out, cx);
    }

    fn new_chat(&mut self, cx: &mut Context<Self>) {
        let out = self.chat.new_chat();
        self.history_open = false;
        self.sync(cx);
        self.send(out, cx);
    }

    /// The tab speaks to another agent from now on: a new process, and a new
    /// conversation.
    fn switch_agent(&mut self, agent: crate::acp::Agent, cx: &mut Context<Self>) {
        self.agent = agent;
        self.resume = None;
        self.chat = Chat::new(self.worktree.to_string_lossy().into_owned());
        self.history_open = false;
        self.sync(cx);
        cx.emit(ChatEvent::Restart);
    }

    fn toggle_history(&mut self, cx: &mut Context<Self>) {
        self.history_open = !self.history_open;
        if self.history_open {
            let line = self.chat.list_sessions();
            self.send(line.into_iter().collect(), cx);
        }
        cx.notify();
    }

    fn reopen(&mut self, session: String, title: Option<String>, cx: &mut Context<Self>) {
        let out = self.chat.reopen(&session, title);
        self.history_open = false;
        self.sync(cx);
        self.send(out, cx);
    }

    fn set_option(&mut self, id: &str, value: Value, cx: &mut Context<Self>) {
        let line = self.chat.set_option(id, value);
        self.picker = None;
        self.send(line.into_iter().collect(), cx);
    }

    fn open_picker(
        &mut self,
        id: String,
        choices: Vec<Choice>,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.entity().downgrade();
        let picker =
            cx.new(|cx| OptionPicker::new(owner, id.clone(), choices, current, window, cx));
        let focus = picker.read(cx).query.read(cx).focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));
        self.picker = Some((id, picker));
        cx.notify();
    }

    // — Painting ————————————————————————————————————————————————————

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let agent_name = self
            .chat
            .agent_name
            .clone()
            .unwrap_or_else(|| self.agent.label().to_string());
        let title: SharedString = if self.history_open {
            tr!("chat-history")
        } else {
            match &self.chat.title {
                Some(title) if !title.trim().is_empty() => SharedString::from(title.clone()),
                _ => tr!("chat-new-title", { agent: agent_name }),
            }
        };
        let usage = self.chat.usage.as_ref().map(|usage| {
            let mut text = if usage.size > 0 {
                format!("{} / {}", tokens(usage.used), tokens(usage.size))
            } else {
                tokens(usage.used)
            };
            if let Some((amount, currency)) = &usage.cost {
                if *amount >= 0.005 {
                    text.push_str(&if currency == "USD" {
                        format!(" · ${amount:.2}")
                    } else {
                        format!(" · {amount:.2} {currency}")
                    });
                }
            }
            div()
                .flex_none()
                .mr_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(SharedString::from(text))
        });
        let this = cx.entity().downgrade();
        let current = self.agent.clone();
        let others: Vec<crate::acp::Agent> = crate::ui::settings::Settings::global(cx)
            .terminal
            .chat_agents()
            .into_iter()
            .filter(|agent| agent.label() != current.label())
            .collect();
        let connected = self.chat.is_connected();
        let new_menu = Button::new("chat-new-menu")
            .ghost()
            .xsmall()
            .icon(icon("plus"))
            .tooltip(tr!("chat-new-menu"))
            .dropdown_menu(move |menu: PopupMenu, _, _| {
                let mut menu = menu.min_w(px(240.));
                let same = this.clone();
                menu = menu.item(
                    PopupMenuItem::new(tr!("chat-new-with", { agent: current.label() }))
                        .icon(icon("message-square-plus"))
                        .disabled(!connected)
                        .on_click(move |_, _, cx| {
                            let _ = same.update(cx, |this, cx| this.new_chat(cx));
                        }),
                );
                if !others.is_empty() {
                    menu = menu.separator().label(tr!("chat-other-agents"));
                    for agent in &others {
                        let (this, agent) = (this.clone(), agent.clone());
                        menu = menu.item(
                            PopupMenuItem::new(SharedString::from(agent.label().to_string()))
                                .icon(icon("bot"))
                                .on_click(move |_, _, cx| {
                                    let agent = agent.clone();
                                    let _ =
                                        this.update(cx, |this, cx| this.switch_agent(agent, cx));
                                }),
                        );
                    }
                }
                menu
            });
        let history = Button::new("chat-history")
            .ghost()
            .xsmall()
            .icon(icon("history"))
            .selected(self.history_open)
            .disabled(!self.chat.can_list() && !self.history_open)
            .tooltip(tr!("chat-history"))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_history(cx)));
        h_flex()
            .flex_none()
            .w_full()
            .h(crate::ui::theme::bar_height(cx) + px(6.))
            .pl_3()
            .pr_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(crate::ui::icons::glyph("bot").text_color(theme.muted_foreground))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .ml_1()
                    .truncate()
                    .text_sm()
                    .child(title),
            )
            .children(usage)
            .child(new_menu)
            .child(history)
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if self.history_open {
            return self.render_history(cx);
        }
        let agent_name = self
            .chat
            .agent_name
            .clone()
            .unwrap_or_else(|| self.agent.label().to_string());
        let empty = self.chat.entries.is_empty();
        match &self.chat.status {
            Status::Starting(phase) if empty => centered(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(icon("loader-circle").text_color(theme.muted_foreground))
                    .child(phase_word(*phase)),
                theme.muted_foreground,
            ),
            Status::Failed(reason) => v_flex()
                .size_full()
                .gap_3()
                .p_4()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(tr!("chat-failed", { agent: agent_name })),
                )
                .child(
                    div()
                        .id("chat-failure")
                        .max_h(px(260.))
                        .overflow_y_scroll()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(reason.clone())),
                )
                .child(
                    h_flex().child(
                        Button::new("chat-restart")
                            .outline()
                            .small()
                            .icon(icon("refresh-cw"))
                            .label(tr!("chat-restart"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatEvent::Restart))),
                    ),
                )
                .into_any_element(),
            Status::AuthRequired => v_flex()
                .size_full()
                .gap_2()
                .p_4()
                .child(
                    div()
                        .text_sm()
                        .child(tr!("chat-sign-in", { agent: agent_name })),
                )
                .children(
                    self.chat
                        .auth_methods
                        .iter()
                        .enumerate()
                        .map(|(ix, method)| {
                            let id = method.id.clone();
                            let launch = method.launch.clone();
                            let in_terminal = launch.is_some();
                            v_flex()
                                .gap_1()
                                .child(
                                    h_flex().child(
                                        Button::new(("chat-auth", ix))
                                            .outline()
                                            .small()
                                            .icon(icon(if in_terminal {
                                                "square-terminal"
                                            } else {
                                                "log-in"
                                            }))
                                            .label(SharedString::from(method.name.clone()))
                                            .when(in_terminal, |button| {
                                                button.tooltip(tr!("chat-login-terminal"))
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                match &launch {
                                                    // Its own flow, in one of our terminals.
                                                    Some(launch) => this.login(launch, cx),
                                                    None => {
                                                        let line = this.chat.authenticate(&id);
                                                        this.send(line.into_iter().collect(), cx);
                                                    }
                                                }
                                            })),
                                    ),
                                )
                                .when(!method.description.is_empty(), |el| {
                                    el.child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(SharedString::from(method.description.clone())),
                                    )
                                })
                        }),
                )
                .when(self.login_launched, |el| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(theme.warning)
                            .child(tr!("chat-login-launched")),
                    )
                })
                .child(
                    h_flex().child(
                        Button::new("chat-auth-retry")
                            .ghost()
                            .small()
                            .label(tr!("chat-auth-retry"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let line = this.chat.retry_session();
                                this.send(line.into_iter().collect(), cx);
                            })),
                    ),
                )
                .into_any_element(),
            _ if empty => centered(
                v_flex()
                    .max_w(px(320.))
                    .gap_2()
                    .items_center()
                    .child(icon("bot").size(px(22.)).text_color(theme.muted_foreground))
                    .child(
                        div()
                            .text_color(theme.foreground)
                            .child(tr!("chat-with", { agent: agent_name })),
                    )
                    .child(div().text_xs().text_center().child(tr!("chat-empty"))),
                theme.muted_foreground,
            ),
            _ => {
                let view = cx.entity();
                MessageScroller::new(
                    ("chat-messages", self.id),
                    self.scroller.clone(),
                    move |ix, window, cx| render_row(&view, ix, window, cx),
                )
                .jump_button(true)
                .with_jump_button_label(tr!("chat-jump"))
                .with_content_style(StyleRefinement::default().py(px(10.)))
                .with_row_style(StyleRefinement::default().pb(px(0.)))
                .size_full()
                .into_any_element()
            }
        }
    }

    fn render_history(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(sessions) = self.chat.sessions.clone() else {
            return centered(tr!("chat-history-loading"), theme.muted_foreground);
        };
        if sessions.is_empty() {
            return centered(tr!("chat-history-none"), theme.muted_foreground);
        }
        let now = chrono::Utc::now().timestamp();
        let current = self.chat.session.clone();
        let (can_fork, can_delete) = (self.chat.can_fork(), self.chat.can_delete());
        let deleting = self.deleting.clone();
        div()
            .id("chat-history-list")
            .size_full()
            .overflow_y_scroll()
            .py_1()
            .children(sessions.into_iter().enumerate().map(|(ix, session)| {
                let when = session
                    .updated
                    .as_deref()
                    .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                    .map(|at| crate::ui::overview_view::ago(now, at.timestamp()))
                    .unwrap_or_default();
                let shown: SharedString = session
                    .title
                    .clone()
                    .map(SharedString::from)
                    .unwrap_or_else(|| tr!("chat-untitled"));
                let on_show = current.as_deref() == Some(session.id.as_str());
                let confirming = deleting.as_deref() == Some(session.id.as_str());
                let (fork_id, fork_title) = (session.id.clone(), session.title.clone());
                let delete_id = session.id.clone();
                // Copy it and go on from the copy; delete it, on a second
                // press — a conversation deleted is gone from the agent too.
                let actions = h_flex()
                    .flex_none()
                    .gap_0p5()
                    .when(can_fork, |el| {
                        el.child(
                            Button::new(("chat-session-fork", ix))
                                .ghost()
                                .xsmall()
                                .icon(icon("git-fork"))
                                .tooltip(tr!("chat-session-fork"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let line = this.chat.fork_session(&fork_id, fork_title.clone());
                                    this.history_open = false;
                                    this.send(line.into_iter().collect(), cx);
                                })),
                        )
                    })
                    .when(can_delete && !on_show, |el| {
                        el.child(
                            Button::new(("chat-session-delete", ix))
                                .ghost()
                                .xsmall()
                                .icon(icon("trash-2"))
                                .when(confirming, |button| {
                                    button.danger().label(tr!("chat-session-delete-confirm"))
                                })
                                .tooltip(tr!("chat-session-delete"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    if this.deleting.as_deref() == Some(delete_id.as_str()) {
                                        this.deleting = None;
                                        let line = this.chat.delete_session(&delete_id);
                                        this.send(line.into_iter().collect(), cx);
                                    } else {
                                        this.deleting = Some(delete_id.clone());
                                        cx.notify();
                                    }
                                })),
                        )
                    });
                h_flex()
                    .id(("chat-session", ix))
                    .mx_1()
                    .px_2()
                    .h(crate::ui::theme::row_height(cx))
                    .gap_2()
                    .items_center()
                    .rounded(theme.radius)
                    .text_sm()
                    .cursor_pointer()
                    .when(on_show, |el| el.bg(theme.list_active))
                    .hover(|el| el.bg(theme.list_hover))
                    .child(div().flex_1().min_w_0().truncate().child(shown))
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(when),
                    )
                    .child(actions)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.deleting = None;
                        this.reopen(session.id.clone(), session.title.clone(), cx);
                    }))
            }))
            .into_any_element()
    }

    fn render_plan(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let plan = &self.chat.plan;
        if plan.is_empty() || self.history_open {
            return None;
        }
        let theme = cx.theme().clone();
        let done = plan
            .iter()
            .filter(|entry| entry.status == "completed")
            .count();
        let open = self.plan_open;
        let current = plan
            .iter()
            .find(|entry| entry.status == "in_progress")
            .or(plan.iter().find(|entry| entry.status != "completed"))
            .map(|entry| entry.content.clone())
            .unwrap_or_default();
        Some(
            v_flex()
                .flex_none()
                .border_t_1()
                .border_color(theme.border)
                .px_3()
                .py_1()
                .child(
                    h_flex()
                        .id("chat-plan")
                        .gap_2()
                        .items_center()
                        .cursor_pointer()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .hover(|el| el.text_color(theme.foreground))
                        .child(chevron(open))
                        .child(icon("list-todo").xsmall())
                        .child(tr!("chat-plan-progress", { done: done, total: plan.len() }))
                        .when(!open, |el| {
                            el.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.foreground)
                                    .child(SharedString::from(current)),
                            )
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.plan_open = !this.plan_open;
                            cx.notify();
                        })),
                )
                .when(open, |el| {
                    el.children(plan.iter().map(|entry| {
                        let (glyph, color) = match entry.status.as_str() {
                            "completed" => ("circle-check", theme.success),
                            "in_progress" => ("loader-circle", theme.warning),
                            _ => ("circle-dashed", theme.muted_foreground),
                        };
                        let completed = entry.status == "completed";
                        h_flex()
                            .items_start()
                            .gap_2()
                            .py(px(2.))
                            .text_xs()
                            .child(icon(glyph).xsmall().text_color(color))
                            .child(
                                div()
                                    .flex_1()
                                    .text_color(if completed {
                                        theme.muted_foreground
                                    } else {
                                        theme.foreground
                                    })
                                    .when(completed, |el| el.line_through())
                                    .child(SharedString::from(entry.content.clone())),
                            )
                    }))
                })
                .into_any_element(),
        )
    }

    /// The line that says the agent is at work, and the way to stop it —
    /// steady, see the module.
    fn render_working(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let word = match &self.chat.status {
            Status::Busy => tr!("chat-working"),
            Status::Starting(phase) if !self.chat.entries.is_empty() => phase_word(*phase),
            _ => return None,
        };
        let busy = self.chat.is_busy();
        Some(
            h_flex()
                .flex_none()
                .px_3()
                .pb_1()
                .gap_2()
                .items_center()
                .text_xs()
                .text_color(theme.warning)
                .child(icon("loader-circle").xsmall())
                .child(word)
                .child(div().flex_1())
                .when(busy, |el| {
                    el.child(
                        div()
                            .id("chat-stop-link")
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|el| el.text_color(theme.foreground))
                            .child(tr!("chat-stop-short"))
                            .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_suggest(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let suggest = self.suggest(cx)?;
        let rows: Vec<(SharedString, SharedString)> = match suggest {
            Suggest::Commands(rows) => rows
                .into_iter()
                .map(|(name, description)| (format!("/{name}").into(), description.into()))
                .collect(),
            Suggest::Files(rows) => rows
                .into_iter()
                .map(|path| (format!("@{path}").into(), SharedString::default()))
                .collect(),
        };
        let pick = self.pick.min(rows.len().saturating_sub(1));
        Some(
            div()
                .id("chat-suggest")
                .absolute()
                .bottom_full()
                .left(px(8.))
                .right(px(8.))
                .mb_1()
                .max_h(px(240.))
                .overflow_y_scroll()
                .py_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_lg()
                .occlude()
                .children(
                    rows.into_iter()
                        .enumerate()
                        .map(|(ix, (name, description))| {
                            h_flex()
                                .id(("chat-suggest-row", ix))
                                .mx_1()
                                .px_2()
                                .h(crate::ui::theme::row_height(cx))
                                .gap_2()
                                .items_center()
                                .rounded(theme.radius)
                                .text_sm()
                                .when(ix == pick, |el| el.bg(theme.list_active))
                                .hover(|el| el.bg(theme.list_hover))
                                .child(
                                    div()
                                        .flex_none()
                                        .font_family(theme.mono_font_family.clone())
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(description),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.pick = ix;
                                        this.pick_confirm(window, cx);
                                    }),
                                )
                        }),
                )
                .into_any_element(),
        )
    }

    /// The session's options under the text, the model beside send.
    fn render_options(&self, cx: &mut Context<Self>) -> (Vec<AnyElement>, Option<AnyElement>) {
        let mut options: Vec<AnyElement> = Vec::new();
        let mut model: Option<AnyElement> = None;
        // Claude's adapter offers its modes twice — as legacy modes and as the
        // `mode` option. The option wins: it is the newer half of the protocol.
        let has_mode_option = self.chat.options.iter().any(ConfigOption::is_mode);
        if let (Some((current, modes)), false) = (self.chat.modes.clone(), has_mode_option) {
            let label = modes
                .iter()
                .find(|mode| mode.value == current)
                .map(|mode| mode.name.clone())
                .unwrap_or(current.clone());
            let this = cx.entity().downgrade();
            options.push(
                Button::new("chat-mode")
                    .ghost()
                    .xsmall()
                    .icon(icon("shield-check"))
                    .dropdown_caret(true)
                    .label(SharedString::from(label))
                    .dropdown_menu(move |menu: PopupMenu, _, _| {
                        modes.iter().fold(menu, |menu, mode| {
                            let (value, this) = (mode.value.clone(), this.clone());
                            menu.item(
                                PopupMenuItem::new(SharedString::from(mode.name.clone()))
                                    .checked(mode.value == current)
                                    .on_click(move |_, _, cx| {
                                        let _ = this.update(cx, |this, cx| {
                                            let line = this.chat.set_mode(&value);
                                            this.send(line.into_iter().collect(), cx);
                                        });
                                    }),
                            )
                        })
                    })
                    .into_any_element(),
            );
        }
        for (ix, option) in self.chat.options.iter().enumerate() {
            let id = option.id.clone();
            match option.kind.clone() {
                OptionKind::Select { current, choices } => {
                    let label = choices
                        .iter()
                        .find(|choice| choice.value == current)
                        .map(|choice| choice.name.clone())
                        .unwrap_or(current.clone());
                    let trigger = Button::new(("chat-option", ix))
                        .ghost()
                        .xsmall()
                        .icon(icon(option_glyph(option)))
                        .dropdown_caret(true)
                        .label(SharedString::from(label))
                        .tooltip(SharedString::from(option.name.clone()));
                    let open = self
                        .picker
                        .as_ref()
                        .filter(|(open, _)| *open == id)
                        .map(|(_, picker)| picker.clone());
                    let this = cx.entity().downgrade();
                    let button = Popover::new(("chat-option-popover", ix))
                        .anchor(gpui_kit::Anchor::BottomLeft)
                        .open(open.is_some())
                        .on_open_change(move |open, window, cx| {
                            let (id, choices, current) =
                                (id.clone(), choices.clone(), current.clone());
                            let open = *open;
                            let _ = this.update(cx, |this, cx| {
                                if open {
                                    this.open_picker(id, choices, current, window, cx);
                                } else {
                                    this.picker = None;
                                    cx.notify();
                                }
                            });
                        })
                        .trigger(trigger)
                        .content(move |_, _, _| match &open {
                            Some(picker) => picker.clone().into_any_element(),
                            None => div().into_any_element(),
                        })
                        .into_any_element();
                    if option.is_model() {
                        model = Some(button);
                    } else {
                        options.push(button);
                    }
                }
                OptionKind::Boolean(on) => options.push(
                    Button::new(("chat-option", ix))
                        .ghost()
                        .xsmall()
                        .icon(icon("zap"))
                        .label(SharedString::from(option.name.clone()))
                        .selected(on)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_option(&id, Value::Bool(!on), cx);
                        }))
                        .into_any_element(),
                ),
            }
        }
        (options, model)
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let busy = self.chat.is_busy();
        let (options, model) = self.render_options(cx);
        // During a turn, stop — and send too when the agent takes a message
        // into the turn (or queues it), as typing while Claude works does.
        let stop = busy.then(|| {
            Button::new("chat-stop")
                .ghost()
                .xsmall()
                .icon(icon("circle-stop"))
                .tooltip(tr!("chat-stop"))
                .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
        });
        let send = (!busy || self.chat.can_send()).then(|| {
            Button::new("chat-send")
                .primary()
                .xsmall()
                .icon(icon("arrow-up"))
                .tooltip(if busy {
                    tr!("chat-send-during")
                } else {
                    tr!("chat-send")
                })
                .disabled(!self.chat.can_send())
                .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
        });
        let view = cx.entity().downgrade();
        let pasted: Vec<AnyElement> = self
            .pasted
            .iter()
            .enumerate()
            .map(|(ix, image)| {
                h_flex()
                    .gap_1()
                    .items_center()
                    .pl_2()
                    .pr_1()
                    .py(px(1.))
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .text_xs()
                    .child(icon("image").xsmall().text_color(theme.muted_foreground))
                    .child(tr!("chat-image-chip", {
                        n: ix + 1,
                        size: (image.bytes / 1024).max(1)
                    }))
                    .child(
                        Button::new(("chat-image-remove", ix))
                            .ghost()
                            .xsmall()
                            .icon(icon("x"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if ix < this.pasted.len() {
                                    this.pasted.remove(ix);
                                }
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .relative()
            .flex_none()
            .m_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary.opacity(0.4))
            .key_context(COMPOSER)
            .on_action(cx.listener(|this, _: &ChatPickUp, _, cx| {
                if !this.pick_step(-1, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ChatPickDown, _, cx| {
                if !this.pick_step(1, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ChatPickConfirm, window, cx| {
                if !this.pick_confirm(window, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ChatEscape, _, cx| {
                if this.chat.is_busy() {
                    this.stop(cx);
                } else {
                    cx.propagate();
                }
            }))
            .children(self.render_suggest(cx))
            .when(!pasted.is_empty(), |el| {
                el.child(h_flex().flex_wrap().gap_1().px_2().pt_2().children(pasted))
            })
            .child(
                div().px_1().pt_1().child(
                    Textarea::new(&self.input)
                        .appearance(false)
                        .bordered(false)
                        // A pasted image joins the prompt; anything else is
                        // pasted as the field pastes it.
                        .on_paste(move |item, _, cx| {
                            view.update(cx, |this, cx| this.paste_images(item, cx))
                                .unwrap_or(false)
                        }),
                ),
            )
            .child(
                h_flex()
                    .items_end()
                    .gap_1()
                    .px_1()
                    .pb_1()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .flex_wrap()
                            .gap_1()
                            .children(options),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap_1()
                            .children(model)
                            .children(stop)
                            .children(send),
                    ),
            )
    }
}

/// What the user has given a form so far.
///
/// Its text fields are entities, built once when the form arrives — in the
/// render, the one place that has the window a field is created with, and
/// kept: a field built again at every frame loses what is typed into it.
struct FormState {
    id: String,
    picks: std::collections::HashMap<String, Vec<String>>,
    toggles: std::collections::HashMap<String, bool>,
    inputs: std::collections::HashMap<String, Entity<InputState>>,
}

impl ChatView {
    /// Builds the form's state when a form arrives, drops it when it goes.
    fn follow_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.chat.form else {
            self.form = None;
            return;
        };
        if self
            .form
            .as_ref()
            .is_some_and(|state| state.id == form.id())
        {
            return;
        }
        let inputs = form
            .fields
            .iter()
            .filter(|field| matches!(field.kind, FieldKind::Text | FieldKind::Number { .. }))
            .map(|field| {
                let placeholder: SharedString = if is_other(field) {
                    tr!("chat-form-other-help")
                } else {
                    field
                        .description
                        .clone()
                        .or(field.title.clone())
                        .unwrap_or_default()
                        .into()
                };
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
                (field.key.clone(), input)
            })
            .collect();
        self.form = Some(FormState {
            id: form.id(),
            picks: Default::default(),
            toggles: Default::default(),
            inputs,
        });
    }

    fn pick(&mut self, field: &Field, value: &str, cx: &mut Context<Self>) {
        let Some(state) = self.form.as_mut() else {
            return;
        };
        let picks = state.picks.entry(field.key.clone()).or_default();
        match field.kind {
            FieldKind::Many(_) => {
                if let Some(at) = picks.iter().position(|pick| pick == value) {
                    picks.remove(at);
                } else {
                    picks.push(value.to_string());
                }
            }
            _ => {
                // A second press on the same option takes it back: nothing is
                // required, and skipping one question must stay possible.
                if picks.first().map(String::as_str) == Some(value) {
                    picks.clear();
                } else {
                    *picks = vec![value.to_string()];
                }
            }
        }
        cx.notify();
    }

    fn answer_form(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.form else {
            return;
        };
        let mut answers = std::collections::HashMap::new();
        for (key, picks) in &state.picks {
            answers.insert(key.clone(), Answer::Picks(picks.clone()));
        }
        for (key, on) in &state.toggles {
            answers.insert(key.clone(), Answer::Boolean(*on));
        }
        for (key, input) in &state.inputs {
            answers.insert(
                key.clone(),
                Answer::Text(input.read(cx).value().to_string()),
            );
        }
        let line = self.chat.answer_form(&answers);
        self.form = None;
        self.send(line.into_iter().collect(), cx);
    }

    fn decline_form(&mut self, cx: &mut Context<Self>) {
        let line = self.chat.decline_form();
        self.form = None;
        self.send(line.into_iter().collect(), cx);
    }

    /// The form the agent waits on — Claude's questions — above the
    /// composer, framed in the colour of what waits on a hand.
    fn render_form(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let form = self.chat.form.as_ref()?;
        let theme = cx.theme().clone();
        // A link to open — an MCP server's sign-in: opened in the browser of
        // the desktop the window is on, which is where one signs in.
        if let Some(url) = form.url.clone() {
            return Some(
                v_flex()
                    .flex_none()
                    .mx_2()
                    .mt_2()
                    .p_3()
                    .gap_2()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.warning.opacity(0.7))
                    .bg(theme.background)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .text_sm()
                            .child(icon("globe").xsmall().text_color(theme.warning))
                            .child(
                                div()
                                    .flex_1()
                                    .child(SharedString::from(form.message.clone())),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(SharedString::from(url.clone())),
                    )
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_1()
                            .child(
                                Button::new("chat-url-decline")
                                    .ghost()
                                    .small()
                                    .label(tr!("chat-url-decline"))
                                    .on_click(cx.listener(|this, _, _, cx| this.decline_form(cx))),
                            )
                            .child(
                                Button::new("chat-url-open")
                                    .primary()
                                    .small()
                                    .icon(icon("globe"))
                                    .label(tr!("chat-url-open"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.open_url(&url);
                                        let line = this.chat.accept_url();
                                        this.send(line.into_iter().collect(), cx);
                                    })),
                            ),
                    )
                    .into_any_element(),
            );
        }
        let state = self.form.as_ref()?;
        let fields: Vec<AnyElement> = form
            .fields
            .iter()
            .enumerate()
            .map(|(ix, field)| self.render_field(ix, field, state, cx))
            .collect();
        Some(
            v_flex()
                .flex_none()
                .mx_2()
                .mt_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.warning.opacity(0.7))
                .bg(theme.background)
                .child(
                    v_flex()
                        .id("chat-form")
                        .max_h(px(420.))
                        .overflow_y_scroll()
                        .p_3()
                        .gap_3()
                        .when(!form.message.is_empty(), |el| {
                            el.child(
                                h_flex()
                                    .gap_2()
                                    .items_start()
                                    .text_sm()
                                    .child(icon("info").xsmall().text_color(theme.warning))
                                    .child(
                                        div()
                                            .flex_1()
                                            .child(SharedString::from(form.message.clone())),
                                    ),
                            )
                        })
                        .children(fields),
                )
                .child(
                    h_flex()
                        .justify_end()
                        .gap_1()
                        .px_3()
                        .pb_2()
                        .child(
                            Button::new("chat-form-skip")
                                .ghost()
                                .small()
                                .label(tr!("chat-form-skip"))
                                .tooltip(tr!("chat-form-skip-help"))
                                .on_click(cx.listener(|this, _, _, cx| this.decline_form(cx))),
                        )
                        .child(
                            Button::new("chat-form-answer")
                                .primary()
                                .small()
                                .label(tr!("chat-form-answer"))
                                .on_click(cx.listener(|this, _, _, cx| this.answer_form(cx))),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_field(
        &self,
        ix: usize,
        field: &Field,
        state: &FormState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let title: Option<SharedString> = if is_other(field) {
            Some(tr!("chat-form-other"))
        } else {
            field.title.clone().map(SharedString::from)
        };
        let head = v_flex()
            .gap_0p5()
            .children(title.map(|title| {
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(title)
            }))
            .children(
                field
                    .description
                    .clone()
                    // A text field says its description as its placeholder.
                    .filter(|_| !matches!(field.kind, FieldKind::Text | FieldKind::Number { .. }))
                    .map(|description| {
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(description))
                    }),
            );
        let body: AnyElement = match &field.kind {
            FieldKind::One(choices) | FieldKind::Many(choices) => {
                let many = matches!(field.kind, FieldKind::Many(_));
                let picked = state.picks.get(&field.key).cloned().unwrap_or_default();
                v_flex()
                    .gap_1()
                    .children(choices.iter().enumerate().map(|(n, choice)| {
                        let on = picked.contains(&choice.value);
                        let glyph = match (many, on) {
                            (true, true) => "square-check",
                            (true, false) => "square",
                            (false, true) => "circle-dot",
                            (false, false) => "circle",
                        };
                        let (field, value) = (field.clone(), choice.value.clone());
                        h_flex()
                            .id(("chat-form-choice", ix * 64 + n))
                            .items_start()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(if on { theme.primary } else { theme.border })
                            .when(on, |el| el.bg(theme.list_active))
                            .hover(|el| el.bg(theme.list_hover))
                            .cursor_pointer()
                            .child(icon(glyph).xsmall().mt_0p5().text_color(if on {
                                theme.primary
                            } else {
                                theme.muted_foreground
                            }))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_sm()
                                            .child(SharedString::from(choice.title.clone())),
                                    )
                                    .children(choice.description.clone().map(|description| {
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(SharedString::from(description))
                                    })),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.pick(&field, &value, cx);
                            }))
                    }))
                    .into_any_element()
            }
            FieldKind::Text | FieldKind::Number { .. } => match state.inputs.get(&field.key) {
                Some(input) => Input::new(input).small().into_any_element(),
                None => div().into_any_element(),
            },
            FieldKind::Boolean => {
                let on = state.toggles.get(&field.key).copied().unwrap_or(false);
                let key = field.key.clone();
                Checkbox::new(("chat-form-toggle", ix))
                    .checked(on)
                    .label(SharedString::from(
                        field.title.clone().unwrap_or_else(|| field.key.clone()),
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(state) = this.form.as_mut() {
                            let toggle = state.toggles.entry(key.clone()).or_default();
                            *toggle = !*toggle;
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }
        };
        v_flex()
            .gap_1()
            .when(!matches!(field.kind, FieldKind::Boolean), |el| {
                el.child(head)
            })
            .child(body)
            .into_any_element()
    }
}

/// The free-text companion Claude's adapter puts after each question of an
/// `AskUserQuestion` (`question_<n>_custom`, titled in English): said in the
/// window's words.
fn is_other(field: &Field) -> bool {
    field.key.ends_with("_custom") && field.title.as_deref() == Some("Other")
}

impl Focusable for ChatView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        self.follow_form(window, cx);
        v_flex()
            .size_full()
            .bg(theme.background)
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(self.render_body(cx)))
            .children(self.render_form(cx))
            .children(self.render_plan(cx))
            .children(self.render_working(cx))
            .child(self.render_composer(cx))
    }
}

/// One entry of the transcript — the list renders them as they come into view.
fn render_row(view: &Entity<ChatView>, ix: usize, _: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    let this = view.read(cx);
    let entries = &this.chat.entries;
    let last = ix + 1 == entries.len();
    // Tight between the steps of one turn, roomier around what is said.
    let step = |entry: Option<&Entry>| {
        matches!(
            entry,
            Some(
                Entry::Tool(_) | Entry::Thought { .. } | Entry::Subagent(_) | Entry::Compaction(_)
            )
        )
    };
    let gap = if last {
        0.
    } else if step(entries.get(ix)) && step(entries.get(ix + 1)) {
        4.
    } else {
        12.
    };
    let row = div().w_full().px_3().pb(px(gap));
    let Some(entry) = entries.get(ix) else {
        return row.into_any_element();
    };
    let weak = view.downgrade();
    match entry {
        Entry::User { text, images } => row
            .child(
                v_flex()
                    .gap_1()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted_foreground.opacity(0.06))
                    .px_3()
                    .py_2()
                    .text_sm()
                    .when(!text.is_empty(), |el| {
                        el.child(SharedString::from(text.clone()))
                    })
                    .when(*images > 0, |el| {
                        el.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(icon("image").xsmall())
                                .child(tr!("chat-images-sent", { count: images })),
                        )
                    }),
            )
            .into_any_element(),
        Entry::Compaction(compaction) => {
            let (word, color) = match compaction.status.as_str() {
                "completed" => (tr!("chat-compaction-done"), theme.muted_foreground),
                "failed" => (tr!("chat-compaction-failed"), theme.danger),
                "cancelled" => (tr!("chat-compaction-cancelled"), theme.muted_foreground),
                _ => (tr!("chat-compaction-running"), theme.warning),
            };
            let expanded = compaction.expanded;
            let foldable = !compaction.summary.is_empty() || compaction.error.is_some();
            row.child(
                h_flex()
                    .id(("chat-compaction", ix))
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .text_color(color)
                    .when(foldable, |el| {
                        el.cursor_pointer()
                            .hover(|el| el.text_color(theme.foreground))
                            .child(chevron(expanded))
                    })
                    .child(icon("archive").xsmall())
                    .child(word)
                    .on_click(move |_, _, cx| {
                        let _ = weak.update(cx, |this, cx| this.toggle(ix, cx));
                    }),
            )
            .when(expanded, |el| {
                el.child(
                    v_flex()
                        .mt_1()
                        .pl_3()
                        .gap_1()
                        .border_l_1()
                        .border_color(theme.border)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .children(compaction.error.clone().map(|error| {
                            div()
                                .text_color(theme.danger)
                                .child(SharedString::from(error))
                        }))
                        .when(!compaction.summary.is_empty(), |el| {
                            el.child(TextView::markdown(
                                SharedString::from(format!("chat-compaction-{ix}")),
                                SharedString::from(compaction.summary.clone()),
                            ))
                        }),
                )
            })
            .into_any_element()
        }
        Entry::Subagent(session) => match this.chat.subagent(session) {
            Some(agent) => row
                .child(render_subagent(&weak, ix, agent, &theme))
                .into_any_element(),
            None => row.into_any_element(),
        },
        Entry::Agent { .. } => {
            let Some(Some((state, _))) = this.markdown.get(ix) else {
                return row.into_any_element();
            };
            row.text_sm()
                .child(
                    TextView::new(state)
                        .selectable(true)
                        .style(text_style(&theme))
                        .code_block_actions(|code, _, _| {
                            let code = code.code().to_string();
                            Button::new("chat-code-copy")
                                .ghost()
                                .xsmall()
                                .icon(icon("copy"))
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                                })
                        }),
                )
                .into_any_element()
        }
        Entry::Thought { expanded, .. } => {
            let expanded = *expanded;
            let thinking = this.chat.is_busy() && last;
            let state = this.markdown.get(ix).cloned().flatten();
            row.child(
                h_flex()
                    .id(("chat-thought", ix))
                    .gap_1()
                    .items_center()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(if thinking {
                        theme.foreground
                    } else {
                        theme.muted_foreground
                    })
                    .hover(|el| el.text_color(theme.foreground))
                    .child(chevron(expanded))
                    .child(icon("brain").xsmall())
                    .child(if thinking {
                        tr!("chat-thinking")
                    } else {
                        tr!("chat-thought")
                    })
                    .on_click(move |_, _, cx| {
                        let _ = weak.update(cx, |this, cx| this.toggle(ix, cx));
                    }),
            )
            .when_some(state.filter(|_| expanded), |el, (state, _)| {
                el.child(
                    div()
                        .mt_1()
                        .pl_3()
                        .border_l_1()
                        .border_color(theme.border)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(
                            TextView::new(&state)
                                .selectable(true)
                                .style(text_style(&theme)),
                        ),
                )
            })
            .into_any_element()
        }
        Entry::Tool(tool) => row
            .child(render_tool(&weak, ix, tool, &theme))
            .into_any_element(),
        Entry::Notice(notice) => {
            let (text, error) = match notice {
                Notice::TokenLimit => (tr!("chat-token-limit"), false),
                Notice::TurnLimit => (tr!("chat-turn-limit"), false),
                Notice::Refused => (tr!("chat-refused"), false),
                Notice::Cancelled => (tr!("chat-cancelled"), false),
                Notice::ResumedWithoutHistory => (tr!("chat-resumed-without-history"), false),
                Notice::NotResumed(reason) => (tr!("chat-not-resumed", { reason: reason }), false),
                Notice::Error(message) => (SharedString::from(message.clone()), true),
                // The agent's own, with its weight: a warning in the warning's
                // colour, its title before what it explains.
                Notice::Agent {
                    severity,
                    title,
                    description,
                } => {
                    let (glyph, color) = match severity.as_str() {
                        "error" => ("circle-x", theme.danger),
                        "warning" => ("triangle-alert", theme.warning),
                        _ => ("info", theme.muted_foreground),
                    };
                    return row
                        .child(
                            h_flex()
                                .items_start()
                                .gap_2()
                                .px_2()
                                .py_1()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(color.opacity(0.5))
                                .text_xs()
                                .child(icon(glyph).xsmall().mt_0p5().text_color(color))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                .child(SharedString::from(title.clone())),
                                        )
                                        .children(description.clone().map(|description| {
                                            div()
                                                .text_color(theme.muted_foreground)
                                                .child(SharedString::from(description))
                                        })),
                                ),
                        )
                        .into_any_element();
                }
            };
            row.child(
                h_flex()
                    .items_start()
                    .gap_2()
                    .text_xs()
                    .text_color(if error {
                        theme.danger
                    } else {
                        theme.muted_foreground
                    })
                    .child(icon(if error { "circle-x" } else { "info" }).xsmall())
                    .child(div().flex_1().child(text)),
            )
            .into_any_element()
        }
    }
}

fn render_tool(
    view: &WeakEntity<ChatView>,
    ix: usize,
    tool: &ToolCall,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let (status_glyph, status_color) = match tool.status.as_str() {
        "completed" => ("check", theme.success),
        "failed" => ("circle-x", theme.danger),
        "cancelled" => ("circle-stop", theme.muted_foreground),
        _ => ("loader-circle", theme.muted_foreground),
    };
    let asking = tool.permission.is_some();
    let expanded = tool.expanded || asking;
    // A command shows what it printed, folded or not: that is the half of
    // it one opens the card for.
    let terminal = tool
        .is_command()
        .then(|| render_terminal(ix, tool, expanded, true, view, theme));
    // Its text content is its output, already shown above.
    let output_in_content = tool.is_command() && tool.terminal.is_none();
    let mut body: Vec<AnyElement> = Vec::new();
    if expanded {
        let mono = theme.mono_font_family.clone();
        body.extend(tool.locations.iter().map(|location| {
            div()
                .text_xs()
                .font_family(mono.clone())
                .text_color(theme.muted_foreground)
                .child(SharedString::from(location.clone()))
                .into_any_element()
        }));
        for (n, content) in tool.content.iter().enumerate() {
            if output_in_content && matches!(content, ToolContent::Text(_)) {
                continue;
            }
            body.push(match content {
                ToolContent::Text(text) => div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        TextView::markdown(
                            SharedString::from(format!("chat-tool-{ix}-{n}")),
                            SharedString::from(text.clone()),
                        )
                        .selectable(true),
                    )
                    .into_any_element(),
                ToolContent::Diff { path, old, new } => {
                    render_diff(path, old.as_deref(), new, theme)
                }
            });
        }
        // A call that says nothing else is read by what it was called with:
        // a command line, a pattern.
        if tool.content.is_empty() && !tool.raw_input.is_null() && !tool.is_command() {
            let raw = serde_json::to_string_pretty(&tool.raw_input).unwrap_or_default();
            body.push(code_box(&raw.chars().take(3000).collect::<String>(), theme));
        }
        if let Some(permission) = &tool.permission {
            let buttons = permission.options.iter().enumerate().map(|(n, option)| {
                let (view, tool_id, option_id) = (view.clone(), tool.id.clone(), option.id.clone());
                let button = Button::new(("chat-permission", ix * 16 + n))
                    .xsmall()
                    .label(SharedString::from(option.name.clone()))
                    .on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| {
                            let line = this.chat.answer(&tool_id, &option_id);
                            this.send(line.into_iter().collect(), cx);
                            this.sync(cx);
                        });
                    });
                if option.kind.starts_with("allow") {
                    button.outline()
                } else {
                    button.ghost()
                }
            });
            body.push(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .pt_1()
                    .children(buttons)
                    .into_any_element(),
            );
        }
    }
    let view = view.clone();
    v_flex()
        .rounded(theme.radius)
        .border_1()
        .border_color(if asking {
            theme.warning.opacity(0.7)
        } else {
            theme.border
        })
        .child(
            h_flex()
                .id(("chat-tool", ix))
                .gap_2()
                .px_2()
                .py_1()
                .items_center()
                .cursor_pointer()
                .text_xs()
                .text_color(theme.muted_foreground)
                .hover(|el| el.text_color(theme.foreground))
                .child(icon(kind_icon(&tool.kind)).xsmall())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(tool.title.clone())),
                )
                .child(icon(status_glyph).xsmall().text_color(status_color))
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.toggle(ix, cx));
                }),
        )
        .children(terminal)
        .when(!body.is_empty(), |el| {
            el.child(v_flex().gap_1().px_2().pb_2().children(body))
        })
        .into_any_element()
}

/// How many lines of a command's output a folded card shows.
const OUTPUT_FOLDED: usize = 8;

/// How many an unfolded one keeps, in a box that scrolls.
const OUTPUT_UNFOLDED: usize = 2000;

/// A command's terminal on its card: the line it ran, the tail of what it
/// printed — all of it, scrolling, once the card is open —, and how it ended.
fn render_terminal(
    ix: usize,
    tool: &ToolCall,
    expanded: bool,
    // `false` for a subagent's call, whose card has nothing to unfold into.
    unfold: bool,
    view: &WeakEntity<ChatView>,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let mono = theme.mono_font_family.clone();
    let output = tool.output().unwrap_or_default();
    let (shown, hidden) = crate::acp::chat::tail(
        &output,
        if expanded {
            OUTPUT_UNFOLDED
        } else {
            OUTPUT_FOLDED
        },
    );
    let running = matches!(tool.status.as_str(), "pending" | "in_progress");
    let exit = tool
        .terminal
        .as_ref()
        .and_then(|terminal| terminal.exit.clone());
    let ended: Option<(SharedString, Hsla)> = match (&exit, tool.status.as_str()) {
        (Some(exit), _) => Some(match (exit.code, &exit.signal) {
            (Some(code), _) => (
                tr!("chat-exit-code", { code: code }),
                if exit.succeeded() {
                    theme.success
                } else {
                    theme.danger
                },
            ),
            (None, Some(signal)) => (SharedString::from(signal.clone()), theme.danger),
            (None, None) => (tr!("chat-exit-ended"), theme.muted_foreground),
        }),
        (None, "failed") => Some((tr!("chat-exit-failed"), theme.danger)),
        (None, "cancelled") => Some((tr!("chat-exit-interrupted"), theme.muted_foreground)),
        _ => None,
    };
    let unfolding = view.clone();
    v_flex()
        .mx_2()
        .mb_2()
        .rounded(theme.radius)
        .bg(theme.muted_foreground.opacity(0.08))
        .px_2()
        .py_1()
        .gap_0p5()
        .text_xs()
        .font_family(mono)
        .children(tool.command().map(|command| {
            div()
                .text_color(theme.foreground)
                .child(SharedString::from(format!("$ {command}")))
        }))
        .when(hidden > 0 && !expanded && unfold, |el| {
            el.child(
                div()
                    .id(("chat-output-more", ix))
                    .cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .hover(|el| el.text_color(theme.foreground))
                    .child(tr!("chat-output-more", { count: hidden }))
                    .on_click(move |_, _, cx| {
                        let _ = unfolding.update(cx, |this, cx| this.toggle(ix, cx));
                    }),
            )
        })
        .when(!shown.is_empty(), |el| {
            let text = div()
                .text_color(theme.muted_foreground)
                .child(SharedString::from(shown.to_string()));
            if expanded {
                el.child(
                    div()
                        .id(("chat-output", ix))
                        .max_h(px(360.))
                        .overflow_y_scroll()
                        .child(text),
                )
            } else {
                el.child(text)
            }
        })
        .when(shown.is_empty() && running, |el| {
            el.child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(tr!("chat-output-waiting")),
            )
        })
        .children(ended.map(|(word, color)| div().text_color(color).child(word)))
        .into_any_element()
}

/// A subagent: its name, its task, how it stands — and, unfolded, what it
/// did: its messages, and its calls with their output and their questions.
fn render_subagent(
    view: &WeakEntity<ChatView>,
    ix: usize,
    agent: &crate::acp::chat::Subagent,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let asking = agent.permission.is_some()
        || agent
            .entries
            .iter()
            .any(|entry| matches!(entry, Entry::Tool(tool) if tool.permission.is_some()));
    let expanded = agent.expanded || asking;
    let (glyph, color) = match agent.state.as_deref() {
        Some("completed") => ("check", theme.success),
        Some("failed" | "disconnected") => ("circle-x", theme.danger),
        Some(_) => ("circle-stop", theme.muted_foreground),
        None => ("loader-circle", theme.warning),
    };
    let calls = agent
        .entries
        .iter()
        .filter(|entry| matches!(entry, Entry::Tool(_)))
        .count();
    let toggle = view.clone();
    let mut body: Vec<AnyElement> = if expanded {
        agent
            .entries
            .iter()
            .enumerate()
            .filter_map(|(n, entry)| match entry {
                Entry::Agent { text, .. } => Some(
                    div()
                        .text_xs()
                        .child(
                            TextView::markdown(
                                SharedString::from(format!("chat-sub-{ix}-{n}")),
                                SharedString::from(text.clone()),
                            )
                            .selectable(true),
                        )
                        .into_any_element(),
                ),
                Entry::Tool(tool) => Some(render_nested_tool(view, ix * 1024 + n, tool, theme)),
                _ => None,
            })
            .collect()
    } else {
        Vec::new()
    };
    if expanded && !agent.report.is_empty() {
        // What it handed back, after what it did.
        body.push(
            div()
                .pt_1()
                .text_xs()
                .child(
                    TextView::markdown(
                        SharedString::from(format!("chat-sub-report-{ix}")),
                        SharedString::from(agent.report.clone()),
                    )
                    .selectable(true),
                )
                .into_any_element(),
        );
    }
    if let Some(permission) = &agent.permission {
        let buttons = permission.options.iter().enumerate().map(|(n, option)| {
            let (view, agent_id, option_id) =
                (view.clone(), agent.session.clone(), option.id.clone());
            let button = Button::new(("chat-subagent-permission", ix * 16 + n))
                .xsmall()
                .label(SharedString::from(option.name.clone()))
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        let line = this.chat.answer(&agent_id, &option_id);
                        this.send(line.into_iter().collect(), cx);
                        this.sync(cx);
                    });
                });
            if option.kind.starts_with("allow") {
                button.outline()
            } else {
                button.ghost()
            }
        });
        body.push(
            h_flex()
                .flex_wrap()
                .gap_1()
                .children(buttons)
                .into_any_element(),
        );
    }
    v_flex()
        .rounded(theme.radius)
        .border_1()
        .border_color(if asking {
            theme.warning.opacity(0.7)
        } else {
            theme.border
        })
        .child(
            h_flex()
                .id(("chat-subagent", ix))
                .gap_2()
                .px_2()
                .py_1()
                .items_center()
                .cursor_pointer()
                .text_xs()
                .text_color(theme.muted_foreground)
                .hover(|el| el.text_color(theme.foreground))
                .child(chevron(expanded))
                .child(icon("bot").xsmall())
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.foreground)
                        .child(SharedString::from(if agent.name.is_empty() {
                            tr!("chat-subagent").to_string()
                        } else {
                            agent.name.clone()
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(agent.task.clone())),
                )
                .when(calls > 0, |el| {
                    el.child(tr!("chat-subagent-calls", { count: calls }))
                })
                .child(icon(glyph).xsmall().text_color(color))
                .on_click(move |_, _, cx| {
                    let _ = toggle.update(cx, |this, cx| this.toggle(ix, cx));
                }),
        )
        .when(!body.is_empty(), |el| {
            el.child(
                v_flex()
                    .gap_1()
                    .px_2()
                    .pb_2()
                    .ml_2()
                    .pl_2()
                    .border_l_1()
                    .border_color(theme.border)
                    .children(body),
            )
        })
        .into_any_element()
}

/// A subagent's call: a line, the tail of its output when it ran a command,
/// and its permission when it asks one.
fn render_nested_tool(
    view: &WeakEntity<ChatView>,
    key: usize,
    tool: &ToolCall,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let (status_glyph, status_color) = match tool.status.as_str() {
        "completed" => ("check", theme.success),
        "failed" => ("circle-x", theme.danger),
        "cancelled" => ("circle-stop", theme.muted_foreground),
        _ => ("loader-circle", theme.muted_foreground),
    };
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(icon(kind_icon(&tool.kind)).xsmall())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(tool.title.clone())),
                )
                .child(icon(status_glyph).xsmall().text_color(status_color)),
        )
        .when(tool.is_command() && tool.output().is_some(), |el| {
            el.child(render_terminal(key, tool, false, false, view, theme))
        })
        .children(tool.permission.as_ref().map(|permission| {
            h_flex()
                .flex_wrap()
                .gap_1()
                .children(permission.options.iter().enumerate().map(|(n, option)| {
                    let (view, tool_id, option_id) =
                        (view.clone(), tool.id.clone(), option.id.clone());
                    let button = Button::new(("chat-sub-permission", key * 16 + n))
                        .xsmall()
                        .label(SharedString::from(option.name.clone()))
                        .on_click(move |_, _, cx| {
                            let _ = view.update(cx, |this, cx| {
                                let line = this.chat.answer(&tool_id, &option_id);
                                this.send(line.into_iter().collect(), cx);
                                this.sync(cx);
                            });
                        });
                    if option.kind.starts_with("allow") {
                        button.outline()
                    } else {
                        button.ghost()
                    }
                }))
        }))
        .into_any_element()
}

fn render_diff(
    path: &str,
    old: Option<&str>,
    new: &str,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let rows = diff_rows(old.unwrap_or_default(), new, DIFF_CONTEXT);
    let mono = theme.mono_font_family.clone();
    v_flex()
        .w_full()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .child(
            div()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .border_b_1()
                .border_color(theme.border)
                .child(SharedString::from(path.to_string())),
        )
        .children(rows.into_iter().take(DIFF_ROWS).map(|(sign, line)| {
            let (bg, fg): (Option<Hsla>, Hsla) = match sign {
                '+' => (Some(theme.success.opacity(0.12)), theme.foreground),
                '-' => (Some(theme.danger.opacity(0.12)), theme.foreground),
                _ => (None, theme.muted_foreground),
            };
            div()
                .px_2()
                .text_xs()
                .font_family(mono.clone())
                .whitespace_nowrap()
                .overflow_hidden()
                .text_color(fg)
                .when_some(bg, |el, bg| el.bg(bg))
                .child(SharedString::from(format!("{sign} {line}")))
        }))
        .into_any_element()
}

fn code_box(text: &str, theme: &gpui_kit::component::Theme) -> AnyElement {
    div()
        .rounded(theme.radius)
        .bg(theme.muted_foreground.opacity(0.08))
        .px_2()
        .py_1()
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .child(SharedString::from(text.to_string()))
        .into_any_element()
}

/// The chat's Markdown: paragraphs a little apart, inline code on a quiet
/// chip rather than in the accent.
fn text_style(theme: &gpui_kit::component::Theme) -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.6))
        .inline_code(HighlightStyle {
            background_color: Some(theme.muted_foreground.opacity(0.14)),
            ..Default::default()
        })
}

fn centered(child: impl IntoElement, color: Hsla) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .p_4()
        .text_sm()
        .text_color(color)
        .child(child)
        .into_any_element()
}

fn chevron(expanded: bool) -> gpui_kit::component::Icon {
    icon(if expanded {
        "chevron-down"
    } else {
        "chevron-right"
    })
    .xsmall()
}

/// A diff's rows: `' '` context, `'-'` removed, `'+'` added, `'…'` a gap.
///
/// Pure, over `hunks::regions` — the comparison the gutter already trusts.
/// A new file (`old` empty) is all additions.
pub(crate) fn diff_rows(old: &str, new: &str, context: usize) -> Vec<(char, String)> {
    let a: Vec<&str> = if old.is_empty() {
        Vec::new()
    } else {
        old.split('\n').collect()
    };
    let b: Vec<&str> = new.split('\n').collect();
    let regions = crate::ui::hunks::regions(&a, &b);
    let shared = |rows: &mut Vec<(char, String)>, lines: &[&str]| {
        rows.extend(lines.iter().map(|l| (' ', l.to_string())));
    };
    let mut rows = Vec::new();
    // Where the last change ended, on the new side.
    let mut at_b = 0usize;
    for (n, (x, y)) in regions.into_iter().enumerate() {
        // The lines both sides share since the last change: context after
        // it, a gap, context before this one — or all of them, when the gap
        // would hide less than it costs.
        let lead = if n == 0 { 0 } else { context };
        if y.start - at_b > lead + context {
            shared(&mut rows, &b[at_b..at_b + lead]);
            rows.push(('…', String::new()));
            shared(&mut rows, &b[y.start - context..y.start]);
        } else {
            shared(&mut rows, &b[at_b..y.start]);
        }
        rows.extend(a[x].iter().map(|l| ('-', l.to_string())));
        rows.extend(b[y.clone()].iter().map(|l| ('+', l.to_string())));
        at_b = y.end;
    }
    let rest = b.len() - at_b;
    if !rows.is_empty() {
        rows.extend(
            b[at_b..at_b + rest.min(context)]
                .iter()
                .map(|l| (' ', l.to_string())),
        );
        if rest > context {
            rows.push(('…', String::new()));
        }
    }
    rows
}

fn kind_icon(kind: &str) -> &'static str {
    match kind {
        "read" => "file-text",
        "edit" => "file-pen",
        "delete" => "trash",
        "move" => "folder-input",
        "search" => "search",
        "execute" => "square-terminal",
        "think" => "brain",
        "fetch" => "globe",
        "switch_mode" => "list-todo",
        _ => "wrench",
    }
}

/// An option's glyph, from what it is about.
fn option_glyph(option: &ConfigOption) -> &'static str {
    let id = option.id.as_str();
    match option.category.as_deref() {
        Some("mode") => "shield-check",
        Some("model") => "cpu",
        Some("thought_level") => "brain",
        _ if id == "mode" => "shield-check",
        _ if id == "model" => "cpu",
        _ if id.contains("effort") || id.contains("thought") || id.contains("reason") => "brain",
        _ => "sliders-horizontal",
    }
}

fn phase_word(phase: Phase) -> SharedString {
    match phase {
        Phase::Launching | Phase::Connecting => tr!("chat-connecting"),
        Phase::OpeningSession => tr!("chat-opening"),
        Phase::SigningIn => tr!("chat-signing-in"),
        Phase::Resuming => tr!("chat-resuming"),
    }
}

/// A count of tokens as one reads it: `950`, `12k`, `1.2M`.
fn tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{}k", n / 1_000),
        _ => format!("{:.1}M", n as f64 / 1_000_000.),
    }
}

/// A select's values — a model list runs into the hundreds: a search field
/// over a virtual list, only the visible rows built.
pub struct OptionPicker {
    owner: WeakEntity<ChatView>,
    id: String,
    choices: Vec<Choice>,
    current: String,
    query: Entity<InputState>,
    matches: Vec<usize>,
    pick: usize,
    scroll: UniformListScrollHandle,
    _subscription: Subscription,
}

impl OptionPicker {
    fn new(
        owner: WeakEntity<ChatView>,
        id: String,
        choices: Vec<Choice>,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("chat-option-search")));
        let subscription =
            cx.subscribe_in(
                &query,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.refilter(cx),
                    InputEvent::PressEnter { .. } => this.confirm(None, cx),
                    _ => {}
                },
            );
        let pick = choices
            .iter()
            .position(|choice| choice.value == current)
            .unwrap_or(0);
        let scroll = UniformListScrollHandle::new();
        scroll.scroll_to_item(pick, ScrollStrategy::Center);
        Self {
            matches: (0..choices.len()).collect(),
            owner,
            id,
            choices,
            current,
            query,
            pick,
            scroll,
            _subscription: subscription,
        }
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        self.matches = (0..self.choices.len())
            .filter(|&ix| {
                let choice = &self.choices[ix];
                let hay = format!(
                    "{} {} {}",
                    choice.name,
                    choice.value,
                    choice.group.as_deref().unwrap_or_default()
                )
                .to_lowercase();
                words.iter().all(|word| hay.contains(word))
            })
            .collect();
        self.pick = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.matches.len();
        if count == 0 {
            return;
        }
        self.pick = (self.pick as isize + delta).rem_euclid(count as isize) as usize;
        self.scroll
            .scroll_to_item(self.pick, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn confirm(&mut self, row: Option<usize>, cx: &mut Context<Self>) {
        let Some(&ix) = self.matches.get(row.unwrap_or(self.pick)) else {
            return;
        };
        let (id, value) = (self.id.clone(), self.choices[ix].value.clone());
        let _ = self.owner.update(cx, |owner, cx| {
            owner.set_option(&id, Value::String(value), cx);
        });
    }
}

impl Render for OptionPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let row_height = crate::ui::theme::row_height(cx);
        let count = self.matches.len();
        let this = cx.entity();
        let list = uniform_list("chat-option-list", count, move |range, _, cx| {
            let picker = this.read(cx);
            let theme = cx.theme().clone();
            range
                .map(|row| {
                    let choice = &picker.choices[picker.matches[row]];
                    let selected = choice.value == picker.current;
                    let this = this.clone();
                    h_flex()
                        .id(("chat-option-row", row))
                        .w_full()
                        .h(row_height)
                        .px_2()
                        .gap_2()
                        .items_center()
                        .rounded(theme.radius)
                        .text_sm()
                        .when(row == picker.pick, |el| el.bg(theme.list_active))
                        .hover(|el| el.bg(theme.list_hover))
                        .child(
                            div()
                                .w(px(14.))
                                .flex_none()
                                .when(selected, |el| el.child(icon("check").xsmall())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(choice.name.clone())),
                        )
                        .children(choice.group.clone().map(|group| {
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(group))
                        }))
                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                            this.update(cx, |picker, cx| picker.confirm(Some(row), cx));
                        })
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.scroll)
        .h(row_height * count.clamp(1, 12) as f32);
        v_flex()
            .w(px(320.))
            .gap_1()
            .key_context(PICKER)
            .on_action(cx.listener(|this, _: &ChatPickUp, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &ChatPickDown, _, cx| this.step(1, cx)))
            .child(Input::new(&self.query).small())
            .child(if count == 0 {
                div()
                    .p_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(tr!("chat-option-none"))
                    .into_any_element()
            } else {
                list.into_any_element()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{diff_rows, tokens};

    #[test]
    fn a_new_file_is_all_additions() {
        let rows = diff_rows("", "a\nb", 2);
        assert_eq!(rows, vec![('+', "a".into()), ('+', "b".into())]);
    }

    #[test]
    fn a_change_keeps_its_context_and_folds_the_rest() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9";
        let new = "1\n2\n3\n4\nfive\n6\n7\n8\n9";
        let rows = diff_rows(old, new, 2);
        assert_eq!(
            rows,
            vec![
                ('…', String::new()),
                (' ', "3".into()),
                (' ', "4".into()),
                ('-', "5".into()),
                ('+', "five".into()),
                (' ', "6".into()),
                (' ', "7".into()),
                ('…', String::new()),
            ]
        );
    }

    #[test]
    fn nothing_changed_is_nothing_shown() {
        assert!(diff_rows("a\nb", "a\nb", 2).is_empty());
    }

    #[test]
    fn token_counts_read_short() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(12_345), "12k");
        assert_eq!(tokens(1_200_000), "1.2M");
    }
}
