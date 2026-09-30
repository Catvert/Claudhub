//! The chat tabs, from the application's side: opening one, feeding it what
//! its agent says, closing it — see `ui::chat_view` for the face and
//! `crate::acp` for the protocol and the lane.

use std::path::{Path, PathBuf};

use gpui_kit::{AppContext as _, Context, Entity, EntityId, Window};

use crate::runtime::protocol::Cmd;
use crate::ui::app::ClaudhubApp;
use crate::ui::chat_view::{ChatEvent, ChatView};

/// A chat tab, as the application keeps it.
pub struct OpenChat {
    pub worktree: PathBuf,
    pub view: Entity<ChatView>,
    pub panel: Entity<crate::ui::panels::ChatPanel>,
    /// Which terminal view holds it — the name its panel answers to.
    pub view_name: &'static str,
    /// The lane's number for it, copied so the pump never reads the view.
    pub chat: u64,
}

/// The next chat's number: counted for the process and never reused, so a
/// line from a chat just closed is never taken for the next one's.
fn next_chat() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl ClaudhubApp {
    /// Opens a chat with `agent` on a worktree, as a tab of the terminal view
    /// `placement` names.
    pub(super) fn open_chat(
        &mut self,
        worktree: &Path,
        agent: crate::acp::Agent,
        placement: crate::ui::settings::TerminalPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat = next_chat();
        let view_name = crate::ui::panels::TerminalPanel::name_of(placement);
        let view =
            cx.new(|cx| ChatView::new(chat, agent.clone(), worktree.to_path_buf(), window, cx));
        cx.subscribe(&view, |this, view, event: &ChatEvent, cx| {
            let chat = view.read(cx).id;
            match event {
                ChatEvent::Send(lines) => {
                    for line in lines {
                        this.git.send(Cmd::AcpSend {
                            chat,
                            line: line.clone(),
                        });
                    }
                }
                ChatEvent::Restart => {
                    let (worktree, agent) = {
                        let view = view.read(cx);
                        (view.worktree.clone(), view.agent.clone())
                    };
                    this.git.send(Cmd::AcpStart {
                        chat,
                        worktree,
                        agent,
                    });
                    view.update(cx, |view, cx| view.start(cx));
                }
                ChatEvent::Retitled => cx.notify(),
            }
        })
        .detach();
        if self.active.as_deref() == Some(worktree) {
            self.show_panel(view_name, cx);
        }
        let visible = self.terminal_shown(worktree, view_name);
        let app = cx.entity();
        let panel = {
            let (worktree, view) = (worktree.to_path_buf(), view.clone());
            cx.new(|cx| {
                crate::ui::panels::ChatPanel::new(&app, view_name, worktree, view, visible, cx)
            })
        };
        self.chats.push(OpenChat {
            worktree: worktree.to_path_buf(),
            view: view.clone(),
            panel: panel.clone(),
            view_name,
            chat,
        });
        // The process first, then its first line: both go down the same lane,
        // in the order they are sent.
        self.git.send(Cmd::AcpStart {
            chat,
            worktree: worktree.to_path_buf(),
            agent,
        });
        view.update(cx, |view, cx| view.start(cx));
        let id = panel.entity_id();
        self.dock_terminal(
            worktree,
            gpui_kit::component::dock::panel_handle(panel),
            id,
            placement,
            window,
            cx,
        );
        let focus = gpui_kit::Focusable::focus_handle(view.read(cx), cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Closes a chat: its agent, and its tab.
    pub(super) fn close_chat(
        &mut self,
        view: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self
            .chats
            .iter()
            .position(|chat| chat.view.entity_id() == view)
        else {
            return;
        };
        let chat = self.chats.remove(ix);
        self.git.send(Cmd::AcpStop { chat: chat.chat });
        let had_focus =
            gpui_kit::Focusable::focus_handle(chat.view.read(cx), cx).is_focused(window);
        let dock = self.dock.clone();
        dock.update(cx, |dock, cx| dock.remove_panel(chat.panel, window, cx));
        // The focus must not stay on what has just left the tree — see
        // `close_terminal`.
        if had_focus {
            let root = self.focus.clone();
            window.focus(&root, cx);
        }
        cx.notify();
    }

    /// Closes every chat of a worktree that is going away.
    pub(super) fn close_chats_of(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let doomed: Vec<EntityId> = self
            .chats
            .iter()
            .filter(|chat| chat.worktree == worktree)
            .map(|chat| chat.view.entity_id())
            .collect();
        for view in doomed {
            self.close_chat(view, window, cx);
        }
    }

    /// A line from a chat's agent. One whose tab is closed is dropped.
    pub(super) fn chat_line(&mut self, chat: u64, line: String, cx: &mut Context<Self>) {
        if let Some(open) = self.chats.iter().find(|open| open.chat == chat) {
            open.view.update(cx, |view, cx| view.receive(&line, cx));
        }
    }

    /// A chat's agent is gone.
    pub(super) fn chat_ended(&mut self, chat: u64, reason: Option<String>, cx: &mut Context<Self>) {
        if let Some(open) = self.chats.iter().find(|open| open.chat == chat) {
            open.view.update(cx, |view, cx| view.ended(reason, cx));
        }
    }

    /// The server died, and every agent it ran with it: no `AcpEnded` will
    /// come, so each chat is told here.
    pub(super) fn chats_lost(&mut self, reason: &str, cx: &mut Context<Self>) {
        for open in &self.chats {
            let reason = reason.to_string();
            open.view
                .update(cx, |view, cx| view.ended(Some(reason), cx));
        }
    }
}
