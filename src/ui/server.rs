//! La mise en route du serveur, sous Windows.
//!
//! Sur cette plateforme, les workers ne vivent pas dans ce processus mais dans
//! une distribution WSL2, et il faut donc : savoir laquelle, y installer le
//! binaire livré à côté de l'exécutable, l'y lancer, et dire à l'utilisateur
//! où l'on en est. Ailleurs, ce module ne fait rien — les workers sont déjà là.
//!
//! **Tout ce qui parle à `wsl.exe` part dans un thread.** Réveiller une
//! distribution endormie prend des secondes, et copier douze mégaoctets n'est
//! pas gratuit : c'est exactement ce que la règle « `src/ui/` ne fait jamais
//! d'entrée-sortie » existe pour éviter. La vue ne garde qu'un état
//! d'avancement et le peint.

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

/// Où en est le serveur, du point de vue de la fenêtre.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ServerState {
    /// Les workers sont dans ce processus : il n'y a rien à en dire, et la
    /// barre d'état reste muette.
    #[default]
    Local,
    /// En route — distribution interrogée, binaire installé, connexion
    /// ouverte. Le message dit laquelle de ces étapes.
    Starting(SharedString),
    /// Debout et répondant.
    Up,
    /// Tombé, ou jamais parti. Le message est celui du transport, et il est
    /// gardé en entier : c'est la seule chose qui dise *pourquoi*.
    Down(String),
}

/// La question posée au premier démarrage, tant qu'aucune distribution n'est
/// choisie.
///
/// **C'est une entité à elle, et non un champ de `ClaudhubApp`.** La fermeture
/// que `open_dialog` retient est un `Fn` rappelé à **chaque frame**, depuis le
/// rendu de la vue racine — c'est-à-dire au milieu d'un emprunt de
/// `ClaudhubApp`. Y toucher à l'application, fût-ce pour la lire, panique
/// (« cannot update … while it is already being updated ») : le choix vit donc
/// dans son propre état, que le dialogue affiche comme n'importe quelle vue
/// enfant.
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

    /// Reprend la mise en route après un échec, à la demande.
    ///
    /// Manuelle et non automatique : un serveur qui meurt en boucle se
    /// relancerait en boucle, et l'utilisateur est le seul à savoir s'il vient
    /// de fermer sa distribution ou de mettre à jour son installation.
    pub(super) fn restart_backend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_backend(window, cx);
    }

    /// Demande dans quelle distribution les workers doivent tourner.
    fn ask_wsl_distro(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.server_state = ServerState::Starting(tr!("server-listing"));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let found = in_a_thread(crate::wsl::distributions).await;
            this.update_in(cx, |app, window, cx| match found {
                Ok(distros) => {
                    // L'état reste « en route » tant que la question est
                    // posée : annoncer un serveur indisponible pendant qu'on
                    // demande où le mettre serait se plaindre de soi-même.
                    app.wsl_prompt = Some(cx.new(|_| WslPrompt { distros, chosen: 0 }));
                    app.open_wsl_dialog(window, cx);
                }
                Err(e) => app.server_failed(e, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Le dialogue de choix, et ce qu'il déclenche.
    ///
    /// Rien dans la fermeture ne touche à `ClaudhubApp` : elle est rappelée au
    /// rendu, donc pendant qu'il est emprunté. La liste des distributions
    /// s'affiche par son entité (voir `WslPrompt`), et les deux boutons ne
    /// s'exécutent qu'au clic, où l'emprunt est rendu.
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
                .on_ok(move |_, window, cx| {
                    entity.update(cx, |this, cx| this.accept_wsl_distro(window, cx));
                    true
                })
                .on_cancel(move |_, _window, cx| {
                    // Refuser de choisir laisse la fenêtre ouverte et sans
                    // workers : c'est dit, plutôt que d'attendre en silence.
                    cancel.update(cx, |this, cx| {
                        this.wsl_prompt = None;
                        this.server_state = ServerState::Down(tr!("server-wsl-none").to_string());
                        cx.notify();
                    });
                    true
                })
        });
    }

    /// Retient le choix et lance la mise en route.
    fn accept_wsl_distro(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.wsl_prompt.take() else {
            return;
        };
        let prompt = prompt.read(cx);
        let Some(distro) = prompt.distros.get(prompt.chosen).cloned() else {
            return;
        };
        // Retenu avant de connecter : la question ne se repose pas au
        // démarrage suivant, même si celui-ci échoue — on n'a pas envie de
        // rechoisir sa distribution parce que WSL dormait.
        Settings::update_global(cx, |settings| settings.wsl_distro = distro.clone());
        self.connect_wsl(distro, window, cx);
    }

    /// Installe le serveur au besoin, puis s'y connecte.
    fn connect_wsl(&mut self, distro: String, window: &mut Window, cx: &mut Context<Self>) {
        self.server_state =
            ServerState::Starting(tr!("server-starting", { distro: distro.clone() }));
        cx.notify();
        // Le répertoire de démarrage du serveur décide du dépôt qui s'ouvre :
        // celui qu'on regardait, à défaut aucun.
        let cwd = self
            .active
            .as_ref()
            .and_then(|path| path.to_str().map(str::to_string));
        cx.spawn_in(window, async move |this, cx| {
            let opened = in_a_thread(move || remote::connect_wsl(&distro, cwd.as_deref())).await;
            this.update_in(cx, |app, window, cx| match opened {
                Ok((git, events, probe)) => {
                    // Le shell de connexion de là-bas : c'est lui qu'un
                    // onglet de terminal lancera, et le seul moment où on
                    // pouvait le demander est celui-ci.
                    crate::ui::settings::set_server_shell(probe.shell);
                    app.backend_ready(git, events, window, cx);
                }
                Err(e) => app.server_failed(e, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Connecte une ligne de commande explicite.
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

    /// Le serveur répond : on lui donne le manche et on l'écoute.
    ///
    /// L'ancienne pompe s'éteint d'elle-même, son canal étant clos avec le
    /// transport qu'elle drainait.
    fn backend_ready(
        &mut self,
        git: crate::runtime::Handle,
        events: async_channel::Receiver<crate::runtime::Evt>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.git = git;
        self.pump_events(events, window, cx);
        // Le serveur neuf ne sait rien de ce qu'on avait ouvert : on lui
        // redonne les dépôts, et la surveillance du worktree affiché. Le
        // reste — statut, diff — se redemande par les chemins habituels.
        //
        // **Deux sources, et il en faut deux.** Ceux qu'on avait ouverts, pour
        // une relance après la mort du serveur ; et ceux que les réglages
        // retiennent, pour le démarrage — `ClaudhubApp::new` les a demandés
        // alors que le manche était encore vide, et ces commandes-là ont été
        // jetées. Sans la seconde, une fenêtre Windows rouvrait toujours vide
        // alors que la liste, elle, n'avait rien perdu.
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
                            .xsmall()
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

/// Exécute un travail bloquant dans un thread et rend sa réponse.
///
/// Un thread plutôt que l'exécuteur de fond de gpui : ce qui se passe ici
/// attend `wsl.exe` pendant des secondes, et occuper un fil de l'exécuteur
/// tout ce temps priverait le reste de la fenêtre du sien.
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
