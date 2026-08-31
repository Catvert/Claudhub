//! Starting the server, on Windows.
//!
//! On that platform the workers do not live in this process but in a WSL2
//! distribution, so we must: know which one, install the binary shipped beside
//! the executable into it, launch it there, and tell the user where things
//! stand. Elsewhere this module does nothing — the workers are already here.
//!
//! **Everything that talks to `wsl.exe` goes into a thread.** Waking a
//! sleeping distribution takes seconds, and copying twelve megabytes is not
//! free: this is exactly what the "`src/ui/` never does I/O" rule exists to
//! avoid. The view only keeps a progress state and paints it.

use gpui::{div, prelude::*, px, Context, Render, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Selectable, Sizable, WindowExt,
};

use crate::runtime::remote;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;

/// Where the server stands, from the window's point of view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ServerState {
    /// The workers are in this process: there is nothing to say about it, and
    /// the status bar stays silent.
    #[default]
    Local,
    /// Starting up — distribution queried, binary installed, connection
    /// opened. The message says which of those steps.
    Starting(SharedString),
    /// Up and answering.
    Up,
    /// Down, or never started. The message is the transport's, kept whole: it
    /// is the only thing that says *why*.
    Down(String),
}

/// The question asked on first startup, while no distribution is chosen.
///
/// **It is an entity of its own, not a field of `ClaudhubApp`.** The closure
/// `open_dialog` keeps is an `Fn` called back on **every frame**, from the root
/// view's render — that is, in the middle of a borrow of `ClaudhubApp`.
/// Touching the application there, even to read it, panics ("cannot update …
/// while it is already being updated"): the choice therefore lives in a state
/// of its own, which the dialog displays like any child view.
pub struct WslPrompt {
    pub distros: Vec<String>,
    pub chosen: usize,
}

impl Render for WslPrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chosen = self.chosen;
        v_flex().gap_1().children(
            self.distros
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, name)| {
                    Button::new(("wsl-distro", index))
                        .ghost()
                        .w_full()
                        .justify_start()
                        .selected(index == chosen)
                        .icon(icon(if index == chosen {
                            "circle-check"
                        } else {
                            "circle"
                        }))
                        .label(name)
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.chosen = index;
                            cx.notify();
                        }))
                }),
        )
    }
}

impl ClaudhubApp {
    /// Starts the server up, once the window is built.
    ///
    /// Called from a task and not from the constructor: opening a dialog needs
    /// a window already mounted, and `Root`'s layers are only installed on the
    /// first render.
    pub(super) fn start_backend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // An explicit command line wins over everything: it is the test lever,
        // and the escape hatch when startup gets it wrong.
        if let Some(argv) = remote::command_from_env() {
            self.connect_argv(argv, window, cx);
            return;
        }
        if !cfg!(windows) {
            return; // this process's workers are already running
        }
        let distro = Settings::global(cx).wsl_distro.clone();
        if distro.trim().is_empty() {
            self.ask_wsl_distro(window, cx);
        } else {
            self.connect_wsl(distro, window, cx);
        }
    }

    /// Resumes startup after a failure, on request.
    ///
    /// Manual and not automatic: a server dying in a loop would relaunch in a
    /// loop, and the user is the only one who knows whether they have just
    /// closed their distribution or updated their installation.
    pub(super) fn restart_backend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_backend(window, cx);
    }

    /// Asks which distribution the workers should run in.
    fn ask_wsl_distro(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.server_state = ServerState::Starting(tr!("server-listing"));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let found = in_a_thread(crate::wsl::distributions).await;
            this.update_in(cx, |app, window, cx| match found {
                Ok(distros) => {
                    // The state stays "starting" while the question is being
                    // asked: announcing an unavailable server while asking
                    // where to put it would be complaining about ourselves.
                    app.wsl_prompt = Some(cx.new(|_| WslPrompt { distros, chosen: 0 }));
                    app.open_wsl_dialog(window, cx);
                }
                Err(e) => app.server_failed(e, cx),
            })
            .ok();
        })
        .detach();
    }

    /// The choice dialog, and what it triggers.
    ///
    /// Nothing in the closure touches `ClaudhubApp`: it is called back at
    /// render time, so while it is borrowed. The distribution list displays
    /// through its entity (see `WslPrompt`), and the two buttons only run on
    /// click, where the borrow has been given back.
    fn open_wsl_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity();
        let Some(prompt) = self.wsl_prompt.clone() else {
            return;
        };
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, cancel, prompt) = (entity.clone(), entity.clone(), prompt.clone());
            dialog
                .title(tr!("server-wsl-title"))
                .child(
                    v_flex()
                        .w(px(420.))
                        .gap_2()
                        .child(div().text_sm().child(tr!("server-wsl-help")))
                        .child(prompt),
                )
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .on_ok(move |_, window, cx| {
                    entity.update(cx, |this, cx| this.accept_wsl_distro(window, cx));
                    true
                })
                .on_cancel(move |_, _window, cx| {
                    // Refusing to choose leaves the window open and without
                    // workers: that is said, rather than waiting in silence.
                    cancel.update(cx, |this, cx| {
                        this.wsl_prompt = None;
                        this.server_state = ServerState::Down(tr!("server-wsl-none").to_string());
                        cx.notify();
                    });
                    true
                })
        });
    }

    /// Records the choice and starts the server up.
    fn accept_wsl_distro(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.wsl_prompt.take() else {
            return;
        };
        let prompt = prompt.read(cx);
        let Some(distro) = prompt.distros.get(prompt.chosen).cloned() else {
            return;
        };
        // Recorded before connecting: the question is not asked again on the
        // next start, even if this one fails — nobody wants to re-pick their
        // distribution because WSL was asleep.
        Settings::update_global(cx, |settings| settings.wsl_distro = distro.clone());
        self.connect_wsl(distro, window, cx);
    }

    /// Installs the server if needed, then connects to it.
    fn connect_wsl(&mut self, distro: String, window: &mut Window, cx: &mut Context<Self>) {
        self.server_state =
            ServerState::Starting(tr!("server-starting", { distro: distro.clone() }));
        cx.notify();
        // The server's start directory decides which repository opens: the one
        // named on the command line, the one being looked at, or none. It is
        // the only way an argument reaches the remote case — the handle was
        // empty when the window opened, and everything sent then was dropped.
        //
        // The argument is a *Windows* path, since that is what the Explorer's
        // verb passes; the wire carries Linux ones. A path that translates to
        // nothing is simply not the start directory — the failure is reported
        // when it is a folder somebody picked, not when it is a command line
        // whose repository will be looked for again as the session is restored.
        let named = self
            .launch_arg
            .clone()
            .and_then(|path| self.repo_path_for_server(path, cx).ok());
        let cwd = named
            .or_else(|| self.active.clone())
            .and_then(|path| path.to_str().map(str::to_string));
        cx.spawn_in(window, async move |this, cx| {
            let opened = in_a_thread(move || remote::connect_wsl(&distro, cwd.as_deref())).await;
            this.update_in(cx, |app, window, cx| match opened {
                Ok((git, events, probe)) => {
                    // The login shell from over there: it is what a terminal
                    // tab will launch, and this is the only moment we could ask
                    // for it.
                    crate::ui::settings::set_server_shell(probe.shell);
                    app.backend_ready(git, events, window, cx);
                }
                Err(e) => app.server_failed(e, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Connects an explicit command line.
    ///
    /// No start directory here, unlike `connect_wsl`: the server inherits ours,
    /// so a folder named on the command line is not what it opens on. This is
    /// the `CLAUDHUB_SERVER_CMD` lever — a test path, and the only place where
    /// the argument goes unheard.
    fn connect_argv(&mut self, argv: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.server_state = ServerState::Starting(tr!("server-listing"));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let opened = in_a_thread(move || remote::connect(&argv)).await;
            this.update_in(cx, |app, window, cx| match opened {
                Ok((git, events)) => app.backend_ready(git, events, window, cx),
                Err(e) => app.server_failed(e, cx),
            })
            .ok();
        })
        .detach();
    }

    /// The server answers: we give it the handle and listen to it.
    ///
    /// The old pump shuts down by itself, its channel closing with the
    /// transport it drained.
    fn backend_ready(
        &mut self,
        git: crate::runtime::Handle,
        events: async_channel::Receiver<crate::runtime::Evt>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.git = git;
        self.pump_events(events, window, cx);
        // The fresh server knows nothing of what was open: we hand it the
        // repositories back, and the watch on the displayed worktree. The rest
        // — status, diff — is asked again through the usual paths.
        //
        // **Two sources, and both are needed.** The ones we had open, for a
        // relaunch after the server's death; and the ones the settings
        // remember, for startup — `ClaudhubApp::new` asked for them while the
        // handle was still empty, and those commands were dropped. Without the
        // second, a Windows window always reopened empty even though the list
        // itself had lost nothing.
        let mut mains: Vec<std::path::PathBuf> =
            self.repos.iter().map(|repo| repo.main.clone()).collect();
        for path in Settings::global(cx).repositories.clone() {
            if !mains.contains(&path) {
                mains.push(path);
            }
        }
        for main in mains {
            self.git.send(crate::runtime::Cmd::OpenRepo(main));
        }
        // The startup check was dropped with everything sent before the
        // handshake: asked again here, like the repositories above.
        self.git.send(crate::runtime::Cmd::ReleaseCheck);
        if let Some(active) = self.active.clone() {
            self.git.send(crate::runtime::Cmd::Watch {
                worktree: active.clone(),
            });
            if let Some(vault) = self.notes_dir(&active, cx) {
                self.git.send(crate::runtime::Cmd::WatchDir { dir: vault });
            }
            self.request_status(active);
        }
        cx.notify();
    }

    pub(super) fn server_failed(&mut self, error: anyhow::Error, cx: &mut Context<Self>) {
        let message = format!("{error:#}");
        log::warn!("server unavailable: {message}");
        self.server_state = ServerState::Down(message);
        cx.notify();
    }

    /// What the status bar says about the server, when it has anything to say.
    ///
    /// Nothing in local mode nor while it answers: a permanent line announcing
    /// that all is well would use up the place where what is wrong shows.
    pub(super) fn render_server_status(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (color, label, detail, relaunch) = match &self.server_state {
            ServerState::Local | ServerState::Up => return None,
            ServerState::Starting(message) => (
                cx.theme().muted_foreground,
                message.clone(),
                String::new(),
                false,
            ),
            ServerState::Down(message) => (
                cx.theme().danger,
                tr!("server-unavailable"),
                message.clone(),
                true,
            ),
        };
        Some(
            h_flex()
                .gap_1()
                .text_color(color)
                .child(icon(if relaunch {
                    "triangle-alert"
                } else {
                    "loader-circle"
                }))
                .child(div().max_w(px(360.)).truncate().child(label))
                .when(relaunch, |el| {
                    el.child(
                        Button::new("server-relaunch")
                            .ghost()
                            .small()
                            .label(tr!("server-relaunch"))
                            .when(!detail.is_empty(), |b| b.tooltip(detail.clone()))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.restart_backend(window, cx);
                            })),
                    )
                }),
        )
    }
}

/// Runs a blocking job in a thread and returns its answer.
///
/// A thread rather than gpui's background executor: what happens here waits on
/// `wsl.exe` for seconds, and occupying one of the executor's threads all that
/// time would deprive the rest of the window of it.
async fn in_a_thread<T, F>(work: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    let (tx, rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("claudhub-wsl".into())
        .spawn(move || {
            let _ = tx.send_blocking(work());
        })?;
    rx.recv().await?
}
