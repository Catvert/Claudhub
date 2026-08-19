//! Terminaux intégrés.
//!
//! L'émulation est celle d'alacritty (`alacritty_terminal`) : parseur VTE,
//! grille de cellules, historique, ouverture du pty et boucle d'E/S. Perch
//! n'écrit que deux choses par-dessus — la traduction des touches gpui en
//! octets (`keys`) et un instantané de la grille que la vue sait dessiner
//! (`Snapshot`).
//!
//! Le partage se fait par `FairMutex` : la boucle d'E/S écrit dans le `Term`
//! depuis son propre thread, le thread d'interface le lit à chaque frame. Ce
//! verrou-là est équitable, donc un terminal qui déverse `yes` ne peut pas
//! affamer l'interface qui essaie de le peindre.

mod keys;
mod snapshot;

pub use keys::key_bytes;
pub use snapshot::{Cursor, Line, Paint, Segment, Snapshot};

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use alacritty_terminal::event::{Event as AlacEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use anyhow::{Context, Result};

/// Ce que la vue a besoin de savoir d'un terminal, entre deux frames.
#[derive(Debug, Clone)]
pub enum TerminalEvent {
    /// Nouveau contenu : il faut redessiner.
    Wakeup,
    /// Le programme a changé le titre (les shells y mettent la commande en
    /// cours, ce qui donne son nom à l'onglet).
    Title(String),
    Bell,
    /// Le processus est sorti ; la session est morte mais son contenu reste
    /// lisible, ce qui est exactement ce qu'on veut après un test échoué.
    Exited,
}

/// Taille de la grille. Les dimensions en pixels comptent : les programmes
/// plein écran interrogent le pty pour placer leurs images et leurs cadres.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub columns: usize,
    pub lines: usize,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl TermSize {
    /// Une grille de moins d'une colonne ou d'une ligne fait paniquer la
    /// grille d'alacritty ; c'est ce qui arrive pendant qu'un panneau se
    /// replie, donc le plancher est ici et pas dans la vue.
    pub fn new(columns: usize, lines: usize, cell_width: u16, cell_height: u16) -> Self {
        Self {
            columns: columns.max(2),
            lines: lines.max(1),
            cell_width: cell_width.max(1),
            cell_height: cell_height.max(1),
        }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

impl From<TermSize> for WindowSize {
    fn from(s: TermSize) -> Self {
        WindowSize {
            num_lines: s.lines as u16,
            num_cols: s.columns as u16,
            cell_width: s.cell_width,
            cell_height: s.cell_height,
        }
    }
}

/// Le pont entre la boucle d'E/S d'alacritty et le thread d'interface.
///
/// `try_send` et non `send` : si la vue n'a pas encore drainé, mieux vaut
/// perdre un réveil — le prochain redessinera de toute façon l'état courant —
/// que bloquer le thread qui lit le pty.
#[derive(Clone)]
struct Proxy(async_channel::Sender<TerminalEvent>);

impl EventListener for Proxy {
    fn send_event(&self, event: AlacEvent) {
        let mapped = match event {
            AlacEvent::Wakeup => TerminalEvent::Wakeup,
            AlacEvent::Title(t) => TerminalEvent::Title(t),
            AlacEvent::ResetTitle => TerminalEvent::Title(String::new()),
            AlacEvent::Bell => TerminalEvent::Bell,
            AlacEvent::Exit | AlacEvent::ChildExit(_) => TerminalEvent::Exited,
            // Presse-papiers, couleurs, forme du curseur : rien que la vue
            // sache traiter aujourd'hui, et les ignorer est sans conséquence.
            _ => return,
        };
        let _ = self.0.try_send(mapped);
    }
}

/// Une session : un pty, son émulateur, et le thread qui les relie.
pub struct Terminal {
    term: Arc<FairMutex<Term<Proxy>>>,
    sender: EventLoopSender,
    events: async_channel::Receiver<TerminalEvent>,
    size: TermSize,
    /// Répertoire de lancement — celui du worktree auquel l'onglet appartient.
    working_directory: PathBuf,
    title: String,
    exited: bool,
}

/// De quoi démarrer une session.
pub struct Spawn<'a> {
    pub working_directory: &'a Path,
    /// Programme et arguments. `None` = le shell de connexion de
    /// l'utilisateur, ce qu'attend quelqu'un qui ouvre « un terminal ».
    pub command: Option<(String, Vec<String>)>,
    pub env: HashMap<String, String>,
    pub size: TermSize,
    /// Lignes d'historique conservées.
    pub scrollback: usize,
}

impl Terminal {
    pub fn spawn(options: Spawn<'_>) -> Result<Self> {
        let (evt_tx, evt_rx) = async_channel::unbounded();
        let proxy = Proxy(evt_tx);

        let mut env = options.env;
        // Sans TERM, les programmes plein écran retombent sur un terminal
        // muet ; `xterm-256color` est ce que décrit l'émulation d'alacritty et
        // ce que toutes les terminfo connaissent.
        env.entry("TERM".into())
            .or_insert_with(|| "xterm-256color".into());
        env.entry("COLORTERM".into())
            .or_insert_with(|| "truecolor".into());
        // Repère pour les scripts et les invites : on est dans Perch.
        env.insert("PERCH".into(), "1".into());

        let pty_options = tty::Options {
            shell: options
                .command
                .map(|(program, args)| tty::Shell::new(program, args)),
            working_directory: Some(options.working_directory.to_path_buf()),
            // Sans drain, la sortie écrite juste avant la fin du processus est
            // perdue — c'est-à-dire l'erreur qu'on cherchait à lire.
            drain_on_exit: true,
            env,
        };

        let config = Config {
            scrolling_history: options.scrollback,
            ..Default::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(
            config,
            &options.size,
            proxy.clone(),
        )));

        let pty = tty::new(&pty_options, options.size.into(), 0).with_context(|| {
            format!(
                "ouverture d'un terminal dans {}",
                options.working_directory.display()
            )
        })?;

        let event_loop = EventLoop::new(term.clone(), proxy, pty, pty_options.drain_on_exit, false)
            .context("démarrage de la boucle d'entrées-sorties du terminal")?;
        let sender = event_loop.channel();
        // Le JoinHandle est délibérément lâché : l'arrêt passe par
        // `Msg::Shutdown` dans `Drop`, et attendre le thread au moment de
        // fermer un onglet ferait attendre l'interface.
        let _ = event_loop.spawn();

        Ok(Self {
            term,
            sender,
            events: evt_rx,
            size: options.size,
            working_directory: options.working_directory.to_path_buf(),
            title: String::new(),
            exited: false,
        })
    }

    /// Canal des événements, à drainer depuis une tâche gpui.
    pub fn events(&self) -> async_channel::Receiver<TerminalEvent> {
        self.events.clone()
    }

    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn set_title(&mut self, title: String) {
        self.title = title;
    }

    pub fn has_exited(&self) -> bool {
        self.exited
    }

    pub fn mark_exited(&mut self) {
        self.exited = true;
    }

    pub fn size(&self) -> TermSize {
        self.size
    }

    /// Envoie des octets au programme. Toute saisie passe par là.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        if self.exited {
            return;
        }
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    pub fn write_str(&self, text: &str) {
        self.write(text.as_bytes().to_vec());
    }

    /// Redimensionne la grille *et* le pty. Les deux, sinon le programme
    /// continue de dessiner à l'ancienne taille : c'est le pty qui porte
    /// SIGWINCH.
    pub fn resize(&mut self, size: TermSize) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.term.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(size.into()));
    }

    /// Fait défiler l'historique de `lines` lignes (positif = vers le passé).
    pub fn scroll(&self, lines: i32) {
        use alacritty_terminal::grid::Scroll;
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    /// Ramène la vue en bas — ce que fait toute frappe dans un terminal.
    pub fn scroll_to_bottom(&self) {
        use alacritty_terminal::grid::Scroll;
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    /// Instantané de la grille pour une frame.
    ///
    /// Le verrou n'est tenu que le temps de la copie : dessiner sous le verrou
    /// bloquerait la boucle d'E/S pendant tout le rendu.
    pub fn snapshot(&self) -> Snapshot {
        snapshot::capture(&self.term.lock())
    }

    /// Vrai si le programme est en mode « application » pour la souris ou les
    /// touches — la vue en a besoin pour savoir si la molette doit défiler
    /// dans l'historique ou être transmise au programme.
    pub fn mode(&self) -> alacritty_terminal::term::TermMode {
        *self.term.lock().mode()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Ferme la boucle d'E/S, qui ferme le pty, ce qui envoie SIGHUP au
        // groupe de processus : sans cela, fermer un onglet laisserait tourner
        // ce qu'il exécutait.
        let _ = self.sender.send(Msg::Shutdown);
    }
}
