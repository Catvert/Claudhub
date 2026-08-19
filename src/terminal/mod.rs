//! Terminaux intégrés.
//!
//! L'émulation est celle d'alacritty (`alacritty_terminal`) : parseur VTE,
//! grille de cellules, historique, ouverture du pty et boucle d'E/S. Claudhub
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

pub use alacritty_terminal::index::Side;
pub use keys::key_bytes;
pub use snapshot::{Cursor, Line, Paint, Segment, Snapshot};

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use alacritty_terminal::event::{Event as AlacEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Point};
use alacritty_terminal::selection::{Selection, SelectionType};
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

/// Une position dans la zone visible, en cellules.
///
/// `side` dit de quel côté de la cellule le pointeur se trouve ; c'est ce qui
/// permet de sélectionner un caractère en partant de sa moitié droite sans
/// l'inclure, comme le fait un éditeur de texte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewportPosition {
    pub line: usize,
    pub column: usize,
    pub side: Side,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    Simple,
    /// Double-clic : étend jusqu'aux frontières du mot.
    Word,
    /// Triple-clic : la ligne entière.
    Line,
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
struct Proxy {
    events: async_channel::Sender<TerminalEvent>,
    /// Voie d'écriture vers le pty.
    ///
    /// Elle n'existe qu'une fois la boucle d'E/S créée, alors que le proxy lui
    /// est passé à la construction : d'où le `OnceLock`, rempli juste après.
    pty: Arc<std::sync::OnceLock<EventLoopSender>>,
}

impl EventListener for Proxy {
    fn send_event(&self, event: AlacEvent) {
        let mapped = match event {
            AlacEvent::Wakeup => TerminalEvent::Wakeup,
            AlacEvent::Title(t) => TerminalEvent::Title(t),
            AlacEvent::ResetTitle => TerminalEvent::Title(String::new()),
            AlacEvent::Bell => TerminalEvent::Bell,
            AlacEvent::Exit | AlacEvent::ChildExit(_) => TerminalEvent::Exited,
            // Une réponse que l'émulateur doit au programme : identité du
            // terminal, position du curseur, état d'un mode. Ce n'est pas
            // facultatif — fish interroge le terminal au démarrage et attend
            // **dix secondes** avant de renoncer, puis se prive des
            // fonctionnalités qui en dépendaient.
            AlacEvent::PtyWrite(text) => {
                if let Some(pty) = self.pty.get() {
                    let _ = pty.send(Msg::Input(text.into_bytes().into()));
                }
                return;
            }
            // Presse-papiers, couleurs, forme du curseur : rien que la vue
            // sache traiter aujourd'hui, et les ignorer est sans conséquence.
            _ => return,
        };
        let _ = self.events.try_send(mapped);
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
        let proxy = Proxy {
            events: evt_tx,
            pty: Arc::new(std::sync::OnceLock::new()),
        };

        let mut env = options.env;
        // Sans TERM, les programmes plein écran retombent sur un terminal
        // muet ; `xterm-256color` est ce que décrit l'émulation d'alacritty et
        // ce que toutes les terminfo connaissent.
        env.entry("TERM".into())
            .or_insert_with(|| "xterm-256color".into());
        env.entry("COLORTERM".into())
            .or_insert_with(|| "truecolor".into());
        // Repère pour les scripts et les invites : on est dans Claudhub.
        env.insert("CLAUDHUB".into(), "1".into());

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

        let event_loop = EventLoop::new(
            term.clone(),
            proxy.clone(),
            pty,
            pty_options.drain_on_exit,
            false,
        )
        .context("démarrage de la boucle d'entrées-sorties du terminal")?;
        let sender = event_loop.channel();
        // À installer avant que la boucle démarre : la première interrogation
        // du terminal arrive dès la première invite.
        let _ = proxy.pty.set(sender.clone());
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

    /// Vrai quand un programme plein écran occupe la grille.
    ///
    /// Il n'y a alors pas d'historique : ce qui est affiché est ce que le
    /// programme dessine, et ce qui précède n'appartient qu'à lui.
    pub fn in_alternate_screen(&self) -> bool {
        self.mode()
            .contains(alacritty_terminal::term::TermMode::ALT_SCREEN)
    }

    /// Vide l'écran et tout l'historique.
    pub fn clear(&self) {
        use alacritty_terminal::vte::ansi::{ClearMode, Handler};
        let mut term = self.term.lock();
        term.clear_screen(ClearMode::All);
        term.clear_screen(ClearMode::Saved);
    }

    /// Ramène la vue en bas — ce que fait toute frappe dans un terminal.
    pub fn scroll_to_bottom(&self) {
        use alacritty_terminal::grid::Scroll;
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    // — Sélection ————————————————————————————————————————————————

    /// Ouvre une sélection à une position du viewport.
    ///
    /// `kind` distingue le glissement simple, le double-clic (mot) et le
    /// triple-clic (ligne) : alacritty se charge lui-même d'étendre aux
    /// frontières sémantiques, avec les mêmes règles que dans un terminal
    /// ordinaire.
    pub fn start_selection(&self, position: ViewportPosition, kind: SelectionKind) {
        let mut term = self.term.lock();
        let point = self.grid_point(&term, position);
        let ty = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Word => SelectionType::Semantic,
            SelectionKind::Line => SelectionType::Lines,
        };
        term.selection = Some(Selection::new(ty, point, position.side));
    }

    /// Étend la sélection en cours. Sans appel préalable à `start_selection`,
    /// ne fait rien — un glissement qui n'a pas commencé dans le terminal ne
    /// doit pas y sélectionner quoi que ce soit.
    pub fn update_selection(&self, position: ViewportPosition) {
        let mut term = self.term.lock();
        let point = self.grid_point(&term, position);
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, position.side);
        }
    }

    /// Sélectionne tout, historique compris.
    pub fn select_all(&self) {
        let mut term = self.term.lock();
        let total = term.grid().total_lines();
        let columns = self.size.columns.saturating_sub(1);
        // La première ligne de l'historique porte l'indice le plus négatif ;
        // la dernière ligne visible est à `lines - 1`.
        let top = Point::new(
            alacritty_terminal::index::Line(-((total - self.size.lines) as i32)),
            Column(0),
        );
        let bottom = Point::new(
            alacritty_terminal::index::Line(self.size.lines as i32 - 1),
            Column(columns),
        );
        let mut selection = Selection::new(SelectionType::Simple, top, Side::Left);
        selection.update(bottom, Side::Right);
        term.selection = Some(selection);
    }

    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    pub fn has_selection(&self) -> bool {
        self.term
            .lock()
            .selection
            .as_ref()
            .is_some_and(|s| !s.is_empty())
    }

    /// Le texte sélectionné, tel qu'il sera collé ailleurs.
    ///
    /// C'est alacritty qui le reconstitue : il sait quelles lignes sont la
    /// continuation d'une ligne trop longue et ne doivent donc pas être
    /// coupées par un saut de ligne, ce qu'un assemblage naïf des lignes
    /// visibles ne saurait pas.
    pub fn selection_text(&self) -> Option<String> {
        self.term.lock().selection_to_string()
    }

    /// Convertit une position du viewport en point de la grille, historique
    /// compris : sans cette translation, une sélection faite après avoir
    /// remonté l'historique désignerait les lignes du bas.
    fn grid_point(&self, term: &Term<Proxy>, position: ViewportPosition) -> Point {
        let offset = term.grid().display_offset();
        let line = position.line.min(self.size.lines.saturating_sub(1));
        let column = position.column.min(self.size.columns.saturating_sub(1));
        alacritty_terminal::term::viewport_to_point(offset, Point::new(line, Column(column)))
    }

    /// Colle du texte.
    ///
    /// En mode « collage entre crochets », le contenu est encadré par les
    /// séquences que le programme attend : sans elles, un shell interprète un
    /// texte multiligne collé comme autant de commandes validées, ce qui est
    /// la façon classique d'exécuter par accident ce qu'on voulait seulement
    /// relire.
    pub fn paste(&self, text: &str) {
        use alacritty_terminal::term::TermMode;
        if self.mode().contains(TermMode::BRACKETED_PASTE) {
            self.write_str("\x1b[200~");
            self.write_str(&text.replace('\x1b', ""));
            self.write_str("\x1b[201~");
        } else {
            // Hors de ce mode, un retour chariot vaut validation : c'est le
            // comportement de tous les terminaux, et le changer casserait un
            // collage volontaire de commandes.
            self.write_str(&text.replace("\r\n", "\r").replace('\n', "\r"));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Fait tourner un vrai programme dans un vrai pty et lit le résultat sur
    /// la grille. C'est le seul test qui prouve la chaîne complète — pty,
    /// boucle d'entrées-sorties, parseur, instantané ; tout le reste vérifie
    /// des morceaux isolés.
    #[test]
    fn a_real_command_reaches_the_grid() {
        let terminal = Terminal::spawn(Spawn {
            working_directory: &std::env::temp_dir(),
            // `printf` plutôt qu'un shell interactif : pas d'invite, pas de
            // fichier de configuration de l'utilisateur, une sortie prévisible.
            command: Some((
                "/bin/sh".into(),
                vec![
                    "-c".into(),
                    "printf 'claudhub \\033[31mrouge\\033[0m'".into(),
                ],
            )),
            env: HashMap::new(),
            size: TermSize::new(40, 5, 8, 16),
            scrollback: 100,
        })
        .expect("le système doit pouvoir ouvrir un pty");

        // La lecture est asynchrone : on attend que la première ligne se
        // remplisse, avec une échéance qui fait échouer plutôt que pendre.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut snapshot = terminal.snapshot();
        while std::time::Instant::now() < deadline
            && !snapshot.lines[0].text.starts_with("claudhub")
        {
            std::thread::sleep(std::time::Duration::from_millis(25));
            snapshot = terminal.snapshot();
        }

        assert_eq!(
            snapshot.lines[0].text, "claudhub rouge",
            "la sortie du programme n'est pas arrivée sur la grille"
        );

        // Et la couleur émise par le programme est bien portée par le run.
        let red = snapshot.lines[0]
            .segments
            .iter()
            .find(|s| &snapshot.lines[0].text[s.start..s.end] == "rouge")
            .expect("le mot coloré doit former son propre run");
        assert!(
            matches!(red.fg, Paint::Rgb(..)),
            "le rouge du programme n'a pas été résolu : {:?}",
            red.fg
        );
    }
}
