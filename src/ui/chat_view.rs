//! A chat tab: an agent spoken to over ACP, beside the terminals.
//!
//! The protocol is `acp::chat::Chat`, pure; this is its face. The view never
//! sends a command itself: what the chat has to write goes out as a
//! [`ChatEvent`], which the application turns into `Cmd::AcpSend` — the view
//! is updated from inside the application's own event pump, where reaching
//! back into the application would be a double borrow.
//!
//! **A tab type, not a terminal.** It sits in the terminals' tool windows and
//! answers to their names (`panels::ChatPanel`), so a bar of tabs holds shells
//! and chats side by side; but it holds no pty, and none of what reads a
//! terminal — the home screen, the agents' signal, the revival — sees it.

use std::path::PathBuf;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex, ActiveTheme, Disableable as _, Selectable as _, Sizable as _,
};
use gpui_kit::{
    div, prelude::*, px, AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    Hsla, ScrollHandle, SharedString, Window,
};
use serde_json::Value;

use crate::acp::chat::{
    Chat, Entry, Notice, OptionKind, Phase, PlanEntry, Status, ToolCall, ToolContent,
};
use crate::tr;
use crate::ui::icons::icon;

/// What the view asks of the application.
pub enum ChatEvent {
    /// Lines for the agent, in order.
    Send(Vec<String>),
    /// Launch the agent again: it failed, or it died.
    Restart,
    /// The tab's name changed — the agent titled the session.
    Retitled,
}

/// How near the bottom still counts as reading the end, so that new text
/// keeps the transcript scrolled down.
const FOLLOW_SLACK: f32 = 48.;

/// Lines of context kept around a diff's changes.
const DIFF_CONTEXT: usize = 2;

pub struct ChatView {
    /// The number the lane knows this chat by — counted for the window.
    pub id: u64,
    pub agent: crate::acp::Agent,
    pub worktree: PathBuf,
    chat: Chat,
    input: Entity<TextareaState>,
    scroll: ScrollHandle,
}

impl EventEmitter<ChatEvent> for ChatView {}

impl ChatView {
    pub fn new(
        id: u64,
        agent: crate::acp::Agent,
        worktree: PathBuf,
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
        cx.subscribe_in(&input, window, |this, _, event, window, cx| {
            if let gpui_kit::component::input::InputEvent::PressEnter { shift: false, .. } = event {
                this.submit(window, cx);
            }
        })
        .detach();
        let chat = Chat::new(worktree.to_string_lossy().into_owned());
        Self {
            id,
            agent,
            worktree,
            chat,
            input,
            scroll: ScrollHandle::new(),
        }
    }

    /// The first line, once the application has asked for the process.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.chat = Chat::new(self.worktree.to_string_lossy().into_owned());
        let line = self.chat.start();
        self.follow();
        cx.emit(ChatEvent::Send(vec![line]));
        cx.notify();
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
        let title = self.chat.title.clone();
        let near = self.near_bottom();
        let out = self.chat.receive(line);
        if near {
            self.follow();
        }
        if !out.is_empty() {
            cx.emit(ChatEvent::Send(out));
        }
        if self.chat.title != title {
            cx.emit(ChatEvent::Retitled);
        }
        cx.notify();
    }

    /// The agent's process is gone.
    pub fn ended(&mut self, reason: Option<String>, cx: &mut Context<Self>) {
        self.chat.ended(reason);
        cx.notify();
    }

    fn near_bottom(&self) -> bool {
        let offset = -self.scroll.offset().y;
        let max = self.scroll.max_offset().y;
        max - offset <= px(FOLLOW_SLACK)
    }

    fn follow(&self) {
        self.scroll.scroll_to_bottom();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        let Some(line) = self.chat.prompt(&text) else {
            return;
        };
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.follow();
        cx.emit(ChatEvent::Send(vec![line]));
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let out = self.chat.cancel();
        if !out.is_empty() {
            cx.emit(ChatEvent::Send(out));
        }
        cx.notify();
    }

    fn send_one(&mut self, line: Option<String>, cx: &mut Context<Self>) {
        if let Some(line) = line {
            cx.emit(ChatEvent::Send(vec![line]));
        }
        cx.notify();
    }

    // — Painting ————————————————————————————————————————————————————

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let name = self
            .chat
            .agent_name
            .clone()
            .unwrap_or_else(|| self.agent.label().to_string());
        let (word, color) = status_word(&self.chat.status, &theme);
        // Claude's adapter offers its modes twice — as legacy modes and as the
        // `mode` option. The option wins: it is the newer half of the protocol.
        let legacy_modes = self
            .chat
            .modes
            .clone()
            .filter(|_| !self.chat.options.iter().any(|option| option.id == "mode"));
        let usage = self.chat.usage.as_ref().map(|usage| {
            let mut text = format!("{} / {}", tokens(usage.used), tokens(usage.size));
            if let Some((amount, currency)) = &usage.cost {
                text.push_str(&if currency == "USD" {
                    format!(" · ${amount:.2}")
                } else {
                    format!(" · {amount:.2} {currency}")
                });
            }
            text
        });
        h_flex()
            .flex_none()
            .w_full()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(crate::ui::icons::glyph("bot"))
            .child(div().text_sm().child(SharedString::from(name)))
            .child(div().text_xs().text_color(color).child(word))
            .child(div().flex_1())
            .children(legacy_modes.map(|(current, modes)| {
                let label = modes
                    .iter()
                    .find(|mode| mode.value == current)
                    .map(|mode| mode.name.clone())
                    .unwrap_or(current.clone());
                let this = cx.entity().downgrade();
                Button::new("chat-mode")
                    .ghost()
                    .xsmall()
                    .label(SharedString::from(label))
                    .dropdown_menu(move |menu, _, _| {
                        modes.iter().fold(menu, |menu, mode| {
                            let (value, this) = (mode.value.clone(), this.clone());
                            menu.item(
                                PopupMenuItem::new(SharedString::from(mode.name.clone()))
                                    .checked(mode.value == current)
                                    .on_click(move |_, _, cx| {
                                        let _ = this.update(cx, |this, cx| {
                                            let line = this.chat.set_mode(&value);
                                            this.send_one(line, cx);
                                        });
                                    }),
                            )
                        })
                    })
            }))
            .children(self.chat.options.iter().enumerate().map(|(ix, option)| {
                let this = cx.entity().downgrade();
                let id = option.id.clone();
                match option.kind.clone() {
                    OptionKind::Select { current, choices } => {
                        let label = choices
                            .iter()
                            .find(|choice| choice.value == current)
                            .map(|choice| choice.name.clone())
                            .unwrap_or(current.clone());
                        Button::new(("chat-option", ix))
                            .ghost()
                            .xsmall()
                            .label(SharedString::from(label))
                            .tooltip(SharedString::from(option.name.clone()))
                            .dropdown_menu(move |menu, _, _| {
                                choices.iter().fold(menu, |menu, choice| {
                                    let (id, value, this) =
                                        (id.clone(), choice.value.clone(), this.clone());
                                    menu.item(
                                        PopupMenuItem::new(SharedString::from(choice.name.clone()))
                                            .checked(choice.value == current)
                                            .on_click(move |_, _, cx| {
                                                let _ = this.update(cx, |this, cx| {
                                                    let line = this.chat.set_option(
                                                        &id,
                                                        Value::from(value.clone()),
                                                    );
                                                    this.send_one(line, cx);
                                                });
                                            }),
                                    )
                                })
                            })
                            .into_any_element()
                    }
                    OptionKind::Boolean(on) => Button::new(("chat-option", ix))
                        .ghost()
                        .xsmall()
                        .selected(on)
                        .label(SharedString::from(option.name.clone()))
                        .on_click(move |_, _, cx| {
                            let _ = this.update(cx, |this, cx| {
                                let line = this.chat.set_option(&id, Value::Bool(!on));
                                this.send_one(line, cx);
                            });
                        })
                        .into_any_element(),
                }
            }))
            .children(usage.map(|usage| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(usage))
            }))
    }

    /// What stands in front of the transcript while there is no session: the
    /// handshake, a login, a failure.
    fn render_state(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let this = cx.entity().downgrade();
        match &self.chat.status {
            Status::Ready | Status::Busy => None,
            Status::Starting(_) => None,
            Status::AuthRequired => Some(
                v_flex()
                    .gap_2()
                    .p_3()
                    .child(div().text_sm().child(tr!("chat-auth-required")))
                    .children(
                        self.chat
                            .auth_methods
                            .iter()
                            .enumerate()
                            .map(|(ix, method)| {
                                let (id, this) = (method.id.clone(), this.clone());
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        Button::new(("chat-auth", ix))
                                            .small()
                                            .label(SharedString::from(method.name.clone()))
                                            .on_click(move |_, _, cx| {
                                                let _ = this.update(cx, |this, cx| {
                                                    let line = this.chat.authenticate(&id);
                                                    this.send_one(line, cx);
                                                });
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(SharedString::from(method.description.clone())),
                                    )
                            }),
                    )
                    .child(
                        Button::new("chat-auth-retry")
                            .small()
                            .ghost()
                            .label(tr!("chat-auth-retry"))
                            .on_click(move |_, _, cx| {
                                let _ = this.update(cx, |this, cx| {
                                    let line = this.chat.retry_session();
                                    this.send_one(line, cx);
                                });
                            }),
                    )
                    .into_any_element(),
            ),
            Status::Failed(reason) => Some(
                v_flex()
                    .gap_2()
                    .p_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.danger)
                            .child(tr!("chat-failed", { agent: self.agent.label() })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .whitespace_normal()
                            .child(SharedString::from(reason.clone())),
                    )
                    .child(
                        Button::new("chat-restart")
                            .small()
                            .icon(icon("refresh-cw"))
                            .label(tr!("chat-restart"))
                            .on_click(move |_, _, cx| {
                                let _ = this.update(cx, |_, cx| cx.emit(ChatEvent::Restart));
                            }),
                    )
                    .into_any_element(),
            ),
        }
    }

    fn render_entry(&self, ix: usize, entry: &Entry, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let chat = self.id;
        let this = cx.entity().downgrade();
        match entry {
            Entry::User(text) => h_flex()
                .w_full()
                .justify_end()
                .child(
                    div()
                        .max_w(gpui_kit::relative(0.85))
                        .px_3()
                        .py_2()
                        .rounded(theme.radius_lg)
                        .bg(theme.secondary)
                        .text_sm()
                        .whitespace_normal()
                        .child(SharedString::from(text.clone())),
                )
                .into_any_element(),
            Entry::Agent { text, .. } => div()
                .w_full()
                .text_sm()
                .child(gpui_kit::component::text::TextView::markdown(
                    SharedString::from(format!("chat-{chat}-{ix}")),
                    SharedString::from(text.clone()),
                ))
                .into_any_element(),
            Entry::Thought { text, expanded } => v_flex()
                .w_full()
                .gap_1()
                .child(
                    fold_row(
                        ("chat-thought", ix),
                        *expanded,
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(tr!("chat-thinking")),
                    )
                    .on_click(move |_, _, cx| {
                        let _ = this.update(cx, |this, cx| {
                            this.chat.toggle(ix);
                            cx.notify();
                        });
                    }),
                )
                .when(*expanded, |el| {
                    el.child(
                        div()
                            .pl_4()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(gpui_kit::component::text::TextView::markdown(
                                SharedString::from(format!("chat-{chat}-{ix}")),
                                SharedString::from(text.clone()),
                            )),
                    )
                })
                .into_any_element(),
            Entry::Tool(tool) => self.render_tool(ix, tool, cx),
            Entry::Notice(notice) => {
                let (text, color) = match notice {
                    Notice::TokenLimit => (tr!("chat-token-limit"), theme.warning),
                    Notice::TurnLimit => (tr!("chat-turn-limit"), theme.warning),
                    Notice::Refused => (tr!("chat-refused"), theme.warning),
                    Notice::Cancelled => (tr!("chat-cancelled"), theme.muted_foreground),
                    Notice::Error(message) => (SharedString::from(message.clone()), theme.danger),
                };
                div()
                    .text_xs()
                    .text_color(color)
                    .whitespace_normal()
                    .child(text)
                    .into_any_element()
            }
        }
    }

    fn render_tool(&self, ix: usize, tool: &ToolCall, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let chat = self.id;
        let this = cx.entity().downgrade();
        let color = match tool.status.as_str() {
            "completed" => theme.success,
            "failed" => theme.danger,
            "in_progress" => theme.warning,
            _ => theme.muted_foreground,
        };
        let mono = theme.mono_font_family.clone();
        let body = tool.expanded.then(|| {
            v_flex()
                .pl_4()
                .gap_1()
                .children(tool.locations.iter().map(|location| {
                    div()
                        .text_xs()
                        .font_family(mono.clone())
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(location.clone()))
                }))
                .children(tool.content.iter().enumerate().map(|(n, content)| {
                    match content {
                        ToolContent::Text(text) => div()
                            .text_xs()
                            .child(gpui_kit::component::text::TextView::markdown(
                                SharedString::from(format!("chat-{chat}-{ix}-{n}")),
                                SharedString::from(text.clone()),
                            ))
                            .into_any_element(),
                        ToolContent::Diff { path, old, new } => {
                            render_diff(path, old.as_deref(), new, &mono, &theme)
                        }
                    }
                }))
        });
        let permission =
            tool.permission.as_ref().map(|permission| {
                let tool_id = tool.id.clone();
                h_flex().pl_4().gap_1().flex_wrap().children(
                    permission.options.iter().enumerate().map(|(n, option)| {
                        let (tool_id, option_id, this) =
                            (tool_id.clone(), option.id.clone(), this.clone());
                        let button = Button::new(("chat-permission", ix * 16 + n))
                            .small()
                            .label(SharedString::from(option.name.clone()))
                            .on_click(move |_, _, cx| {
                                let _ = this.update(cx, |this, cx| {
                                    let line = this.chat.answer(&tool_id, &option_id);
                                    this.send_one(line, cx);
                                });
                            });
                        if option.kind.starts_with("allow") {
                            button.primary()
                        } else {
                            button.ghost()
                        }
                    }),
                )
            });
        v_flex()
            .w_full()
            .gap_1()
            .child(
                fold_row(
                    ("chat-tool", ix),
                    tool.expanded,
                    h_flex()
                        .gap_2()
                        .items_center()
                        .min_w_0()
                        .child(crate::ui::icons::glyph(kind_icon(&tool.kind)))
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .child(SharedString::from(tool.title.clone())),
                        )
                        .child(div().size(px(6.)).flex_none().rounded_full().bg(color)),
                )
                .on_click(move |_, _, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.chat.toggle(ix);
                        cx.notify();
                    });
                }),
            )
            .children(body)
            .children(permission)
            .into_any_element()
    }

    fn render_plan(&self, plan: &[PlanEntry], cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .flex_none()
            .w_full()
            .px_3()
            .py_1()
            .gap_0p5()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(tr!("chat-plan")),
            )
            .children(plan.iter().map(|entry| {
                let (glyph, color) = match entry.status.as_str() {
                    "completed" => ("circle-check", theme.success),
                    "in_progress" => ("loader-circle", theme.warning),
                    _ => ("circle-dashed", theme.muted_foreground),
                };
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(crate::ui::icons::glyph(glyph).text_color(color))
                    .child(
                        div()
                            .text_xs()
                            .whitespace_normal()
                            .when(entry.status == "completed", |el| {
                                el.line_through().text_color(theme.muted_foreground)
                            })
                            .child(SharedString::from(entry.content.clone())),
                    )
            }))
    }

    fn render_input(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let this = cx.entity().downgrade();
        let busy = self.chat.is_busy();
        let ready = self.chat.is_ready();
        let starting = match &self.chat.status {
            Status::Starting(phase) => Some(phase_word(*phase)),
            _ => None,
        };
        let action = if busy {
            Button::new("chat-stop")
                .small()
                .icon(icon("circle-stop"))
                .tooltip(tr!("chat-stop"))
                .on_click(move |_, _, cx| {
                    let _ = this.update(cx, |this, cx| this.stop(cx));
                })
        } else {
            Button::new("chat-send")
                .small()
                .primary()
                .icon(icon("send"))
                .tooltip(tr!("chat-send"))
                .disabled(!ready)
                .on_click(move |_, window, cx| {
                    let _ = this.update(cx, |this, cx| this.submit(window, cx));
                })
        };
        v_flex()
            .flex_none()
            .w_full()
            .p_2()
            .gap_1()
            .border_t_1()
            .border_color(theme.border)
            .children(starting.map(|word| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(word)
            }))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_end()
                    .child(div().flex_1().min_w_0().child(Textarea::new(&self.input)))
                    .child(action),
            )
    }
}

impl Focusable for ChatView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for ChatView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entries: Vec<AnyElement> = self
            .chat
            .entries
            .iter()
            .enumerate()
            .map(|(ix, entry)| self.render_entry(ix, entry, cx))
            .collect();
        let plan = self.chat.plan.clone();
        let empty = entries.is_empty();
        let theme = cx.theme().clone();
        v_flex()
            .size_full()
            .child(self.render_header(cx))
            .children(self.render_state(cx))
            .child(
                div()
                    .id(("chat-transcript", self.id))
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        v_flex()
                            .w_full()
                            .p_3()
                            .pr(crate::ui::theme::scroll_gutter())
                            .gap_3()
                            .when(empty && self.chat.is_ready(), |el| {
                                el.child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child(tr!("chat-empty", { agent: self.agent.label() })),
                                )
                            })
                            .children(entries),
                    ),
            )
            .when(!plan.is_empty(), |el| el.child(self.render_plan(&plan, cx)))
            .child(self.render_input(cx))
    }
}

/// A row that folds what is under it: a chevron and a label.
fn fold_row(
    id: impl Into<gpui_kit::ElementId>,
    expanded: bool,
    label: impl IntoElement,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    h_flex()
        .id(id)
        .gap_1()
        .items_center()
        .cursor_pointer()
        .child(crate::ui::icons::glyph(if expanded {
            "chevron-down"
        } else {
            "chevron-right"
        }))
        .child(label)
}

fn render_diff(
    path: &str,
    old: Option<&str>,
    new: &str,
    mono: &SharedString,
    theme: &gpui_kit::component::theme::Theme,
) -> AnyElement {
    let rows = diff_rows(old.unwrap_or_default(), new, DIFF_CONTEXT);
    v_flex()
        .w_full()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .child(
            div()
                .px_2()
                .py_0p5()
                .text_xs()
                .bg(theme.secondary)
                .font_family(mono.clone())
                .child(SharedString::from(path.to_string())),
        )
        .children(rows.into_iter().map(|row| {
            let (bg, sign): (Option<Hsla>, &str) = match row.0 {
                '+' => (Some(theme.success.opacity(0.15)), "+"),
                '-' => (Some(theme.danger.opacity(0.15)), "-"),
                '…' => (None, "…"),
                _ => (None, " "),
            };
            h_flex()
                .w_full()
                .px_2()
                .text_xs()
                .font_family(mono.clone())
                .when_some(bg, |el, bg| el.bg(bg))
                .child(div().w(px(12.)).flex_none().child(SharedString::from(sign)))
                .child(
                    div()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(SharedString::from(row.1)),
                )
        }))
        .into_any_element()
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
        "edit" | "delete" | "move" => "pencil",
        "search" => "search",
        "execute" => "square-terminal",
        "fetch" => "globe",
        "think" => "sparkles",
        _ => "settings-2",
    }
}

fn status_word(status: &Status, theme: &gpui_kit::component::theme::Theme) -> (SharedString, Hsla) {
    match status {
        Status::Starting(phase) => (phase_word(*phase), theme.muted_foreground),
        Status::AuthRequired => (tr!("chat-status-auth"), theme.warning),
        Status::Ready => (tr!("chat-status-ready"), theme.muted_foreground),
        Status::Busy => (tr!("chat-status-busy"), theme.warning),
        Status::Failed(_) => (tr!("chat-status-failed"), theme.danger),
    }
}

fn phase_word(phase: Phase) -> SharedString {
    match phase {
        Phase::Launching | Phase::Connecting => tr!("chat-connecting"),
        Phase::OpeningSession => tr!("chat-opening"),
        Phase::SigningIn => tr!("chat-signing-in"),
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
