//! Built-in terminals.
//!
//! The emulation is alacritty's (`alacritty_terminal`): VTE parser, cell grid,
//! scrollback, pty opening and I/O loop. Claudhub writes only two things on top
//! — the translation of gpui keystrokes into bytes (`keys`) and a snapshot of
//! the grid the view can draw (`Snapshot`).
//!
//! Sharing goes through a `FairMutex`: the I/O loop writes into the `Term` from
//! its own thread, the interface thread reads it on every frame. That lock is
//! fair, so a terminal spewing `yes` cannot starve the interface trying to
//! paint it.

mod keys;
pub mod mouse;
mod snapshot;

pub use alacritty_terminal::index::Side;
pub use keys::key_bytes;
pub use mouse::{Action as MouseAction, Button as MouseButton, Report as MouseReport};
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

/// What the view needs to know about a terminal, between two frames.
#[derive(Debug, Clone)]
pub enum TerminalEvent {
    /// New content: a redraw is needed.
    Wakeup,
    /// The program changed the title (shells put the running command there,
    /// which gives the tab its name).
    Title(String),
    Bell,
    /// The process has exited; the session is dead but its content stays
    /// readable, which is exactly what one wants after a failed test.
    ///
    /// Carries the child's exit code when there is one — `None` for a death by
    /// signal, and for the loop's own end, which alacritty signals a second
    /// time right after the child's. It is what tells a command that has run
    /// its course from one that failed, and the tab is only closed on the
    /// former.
    Exited(Option<i32>),
}

/// A position in the visible area, in cells.
///
/// `side` says which side of the cell the pointer is on; that is what makes it
/// possible to select a character starting from its right half without
/// including it, as a text editor does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewportPosition {
    pub line: usize,
    pub column: usize,
    pub side: Side,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    Simple,
    /// Double click: extends to word boundaries.
    Word,
    /// Triple click: the whole line.
    Line,
}

/// Grid size. The pixel dimensions matter: full-screen programs query the pty to
/// place their images and their frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub columns: usize,
    pub lines: usize,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl TermSize {
    /// A grid of less than one column or one line makes alacritty's grid panic;
    /// that is what happens while a panel is collapsing, so the floor is here and
    /// not in the view.
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

/// The bridge between alacritty's I/O loop and the interface thread.
///
/// `try_send` and not `send`: if the view has not drained yet, losing a wake-up
/// is better — the next one will redraw the current state anyway — than
/// blocking the thread that reads the pty.
#[derive(Clone)]
struct Proxy {
    events: async_channel::Sender<TerminalEvent>,
    /// The write channel towards the pty.
    ///
    /// It only exists once the I/O loop has been created, whereas the proxy is
    /// handed to it at construction: hence the `OnceLock`, filled just after.
    pty: Arc<std::sync::OnceLock<EventLoopSender>>,
}

impl EventListener for Proxy {
    fn send_event(&self, event: AlacEvent) {
        let mapped = match event {
            AlacEvent::Wakeup => TerminalEvent::Wakeup,
            AlacEvent::Title(t) => TerminalEvent::Title(t),
            AlacEvent::ResetTitle => TerminalEvent::Title(String::new()),
            AlacEvent::Bell => TerminalEvent::Bell,
            AlacEvent::ChildExit(status) => TerminalEvent::Exited(status.code()),
            AlacEvent::Exit => TerminalEvent::Exited(None),
            // An answer the emulator owes the program: terminal identity, cursor
            // position, state of a mode. It is not optional — fish queries the
            // terminal at startup and waits **ten seconds** before giving up,
            // then does without the features that depended on it.
            AlacEvent::PtyWrite(text) => {
                if let Some(pty) = self.pty.get() {
                    let _ = pty.send(Msg::Input(text.into_bytes().into()));
                }
                return;
            }
            // Clipboard, colours, cursor shape: nothing the view can handle
            // today, and ignoring them has no consequence.
            _ => return,
        };
        let _ = self.events.try_send(mapped);
    }
}

/// A session: a pty, its emulator, and the thread joining them.
pub struct Terminal {
    term: Arc<FairMutex<Term<Proxy>>>,
    sender: EventLoopSender,
    events: async_channel::Receiver<TerminalEvent>,
    size: TermSize,
    /// Launch directory — that of the worktree the tab belongs to.
    working_directory: PathBuf,
    title: String,
    exited: bool,
    /// The pid of what was started in the pty — a shell, or the program a
    /// profile named. Kept for the one question a closing tab has to ask: is
    /// anything running in there.
    child: u32,
}

/// What is needed to start a session.
pub struct Spawn<'a> {
    pub working_directory: &'a Path,
    /// Program and arguments. `None` = the user's login shell, which is what
    /// somebody opening "a terminal" expects.
    pub command: Option<(String, Vec<String>)>,
    pub env: HashMap<String, String>,
    pub size: TermSize,
    /// Scrollback lines kept.
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
        // Without TERM, full-screen programs fall back to a dumb terminal;
        // `xterm-256color` is what alacritty's emulation describes and what
        // every terminfo knows.
        env.entry("TERM".into())
            .or_insert_with(|| "xterm-256color".into());
        env.entry("COLORTERM".into())
            .or_insert_with(|| "truecolor".into());
        // A marker for scripts and prompts: we are inside Claudhub.
        env.insert("CLAUDHUB".into(), "1".into());

        // On Linux the `..Default::default()` fallback covers no field, hence
        // the allow; on Windows it provides `escape_args`, without which the
        // literal does not compile.
        #[allow(clippy::needless_update)]
        let pty_options = tty::Options {
            shell: options
                .command
                .map(|(program, args)| tty::Shell::new(program, args)),
            working_directory: Some(options.working_directory.to_path_buf()),
            // Without draining, the output written just before the process ends
            // is lost — that is, the error we were trying to read.
            drain_on_exit: true,
            env,
            ..Default::default()
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

        // Read before the pty is moved into the loop, which is the only chance:
        // `EventLoop::new` takes it, and nothing hands the child back.
        let child = pty.child().id();

        let event_loop = EventLoop::new(
            term.clone(),
            proxy.clone(),
            pty,
            pty_options.drain_on_exit,
            false,
        )
        .context("starting the terminal's input/output loop")?;
        let sender = event_loop.channel();
        // To be installed before the loop starts: the terminal's first query
        // arrives as soon as the first prompt does.
        let _ = proxy.pty.set(sender.clone());
        // The JoinHandle is deliberately dropped: shutdown goes through
        // `Msg::Shutdown` in `Drop`, and waiting for the thread while closing a
        // tab would make the interface wait.
        let _ = event_loop.spawn();

        Ok(Self {
            term,
            sender,
            events: evt_rx,
            size: options.size,
            working_directory: options.working_directory.to_path_buf(),
            title: String::new(),
            exited: false,
            child,
        })
    }

    /// The event channel, to be drained from a gpui task.
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

    /// Whether a command is running in front of the shell right now.
    ///
    /// What a closing tab asks before killing the pty. A shell sitting at its
    /// prompt **is** the pty's foreground process group; a shell running a job
    /// has handed that group over, which is the whole of job control and the
    /// only reading that costs nothing to take. It says nothing about a tab
    /// whose child is not a shell — an agent's — and that half of the question
    /// belongs to the view, which is what knows a profile was launched.
    ///
    /// Off Linux it always answers no: reading a foreground group is `/proc`,
    /// and the Windows build's terminals are the one thing that stays on the
    /// Windows side.
    pub fn busy(&self) -> bool {
        if self.exited {
            return false;
        }
        #[cfg(target_os = "linux")]
        {
            std::fs::read_to_string(format!("/proc/{}/stat", self.child))
                .ok()
                .as_deref()
                .is_some_and(running_command)
        }
        #[cfg(not(target_os = "linux"))]
        false
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

    /// Sends bytes to the program. All input goes through here.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        if self.exited {
            return;
        }
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    pub fn write_str(&self, text: &str) {
        self.write(text.as_bytes().to_vec());
    }

    /// Resizes the grid *and* the pty. Both, otherwise the program keeps drawing
    /// at the old size: it is the pty that carries SIGWINCH.
    pub fn resize(&mut self, size: TermSize) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.term.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(size.into()));
    }

    /// Scrolls the scrollback by `lines` lines (positive = towards the past).
    pub fn scroll(&self, lines: i32) {
        use alacritty_terminal::grid::Scroll;
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    /// True when a full-screen program occupies the grid.
    ///
    /// There is then no scrollback: what is shown is what the program draws, and
    /// what came before belongs to it alone.
    pub fn in_alternate_screen(&self) -> bool {
        self.mode()
            .contains(alacritty_terminal::term::TermMode::ALT_SCREEN)
    }

    /// Clears the screen and the whole scrollback.
    pub fn clear(&self) {
        use alacritty_terminal::vte::ansi::{ClearMode, Handler};
        let mut term = self.term.lock();
        term.clear_screen(ClearMode::All);
        term.clear_screen(ClearMode::Saved);
    }

    /// Brings the view back to the bottom — what every keystroke does in a terminal.
    pub fn scroll_to_bottom(&self) {
        use alacritty_terminal::grid::Scroll;
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    // — Selection ————————————————————————————————————————————————

    /// Opens a selection at a viewport position.
    ///
    /// `kind` tells a simple drag from a double click (word) and a triple click
    /// (line): alacritty takes care of extending to semantic boundaries itself,
    /// with the same rules as in an ordinary terminal.
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

    /// Extends the running selection. Without a prior call to
    /// `start_selection`, does nothing — a drag that did not start in the
    /// terminal must not select anything there.
    pub fn update_selection(&self, position: ViewportPosition) {
        let mut term = self.term.lock();
        let point = self.grid_point(&term, position);
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, position.side);
        }
    }

    /// Selects everything, scrollback included.
    pub fn select_all(&self) {
        let mut term = self.term.lock();
        let total = term.grid().total_lines();
        let columns = self.size.columns.saturating_sub(1);
        // The first scrollback line carries the most negative index; the last
        // visible line is at `lines - 1`.
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

    /// The selected text, as it will be pasted elsewhere.
    ///
    /// It is alacritty that rebuilds it: it knows which lines are the
    /// continuation of a line that was too long and must therefore not be cut
    /// by a newline, which a naive assembly of the visible lines would not.
    pub fn selection_text(&self) -> Option<String> {
        self.term.lock().selection_to_string()
    }

    /// Converts a viewport position into a grid point, scrollback included:
    /// without that translation, a selection made after scrolling back would
    /// name the lines at the bottom.
    fn grid_point(&self, term: &Term<Proxy>, position: ViewportPosition) -> Point {
        let offset = term.grid().display_offset();
        let line = position.line.min(self.size.lines.saturating_sub(1));
        let column = position.column.min(self.size.columns.saturating_sub(1));
        alacritty_terminal::term::viewport_to_point(offset, Point::new(line, Column(column)))
    }

    /// Pastes text.
    ///
    /// In "bracketed paste" mode, the content is wrapped in the sequences the
    /// program expects: without them, a shell reads a pasted multi-line text as
    /// that many commands entered, which is the classic way of accidentally
    /// running what you only meant to read.
    pub fn paste(&self, text: &str) {
        use alacritty_terminal::term::TermMode;
        if self.mode().contains(TermMode::BRACKETED_PASTE) {
            self.write_str("\x1b[200~");
            self.write_str(&text.replace('\x1b', ""));
            self.write_str("\x1b[201~");
        } else {
            // Outside that mode, a carriage return means confirm: it is every
            // terminal's behaviour, and changing it would break a deliberate
            // paste of commands.
            self.write_str(&text.replace("\r\n", "\r").replace('\n', "\r"));
        }
    }

    /// Snapshot of the grid for one frame.
    ///
    /// The lock is only held for the length of the copy: drawing under the lock
    /// would block the I/O loop for the whole render.
    pub fn snapshot(&self) -> Snapshot {
        snapshot::capture(&self.term.lock())
    }

    /// Reports a mouse event to the program, if it asked for one.
    ///
    /// Returns true when the event was delivered to it: the view then has
    /// nothing left to do with it — no scrolling, no selecting. The program has
    /// the mouse, as in any terminal.
    pub fn report_mouse(&self, event: mouse::Report) -> bool {
        let Some(bytes) = mouse::report(self.mode(), event) else {
            return false;
        };
        self.write(bytes);
        true
    }

    /// True if the program listens to the mouse. The view uses it for what is
    /// decided **before** the event — leaving the selection to Shift, for
    /// instance.
    pub fn reports_mouse(&self) -> bool {
        self.mode()
            .intersects(alacritty_terminal::term::TermMode::MOUSE_MODE)
    }

    /// True if the program is in "application" mode for the mouse or the keys —
    /// the view needs it to know whether the wheel should scroll the scrollback
    /// or be passed to the program.
    pub fn mode(&self) -> alacritty_terminal::term::TermMode {
        *self.term.lock().mode()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Closes the I/O loop, which closes the pty, which sends SIGHUP to the
        // process group: without that, closing a tab would leave what it was
        // running alive.
        let _ = self.sender.send(Msg::Shutdown);
    }
}

/// Whether a shell's `/proc/<pid>/stat` says a job is running in front of it.
///
/// The fields are read from the **last closing parenthesis** and not by
/// splitting the line: the program name is the second field, in parentheses,
/// and it may hold spaces and parentheses of its own — the same trap
/// `agent::parse_cpu_ticks` pays for. After the name come state, ppid, pgrp,
/// session, tty and `tpgid`, the foreground group of the terminal it is
/// attached to. A shell at its prompt is that group itself; a shell running a
/// command has handed it over, and `-1` means no terminal at all.
fn running_command(stat: &str) -> bool {
    let Some(rest) = stat.rfind(')').map(|at| &stat[at + 1..]) else {
        return false;
    };
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let group = fields.get(2).and_then(|f| f.parse::<i32>().ok());
    let foreground = fields.get(5).and_then(|f| f.parse::<i32>().ok());
    match (group, foreground) {
        (Some(group), Some(foreground)) => foreground > 0 && foreground != group,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shell's own line, then the same shell with `sleep` in front of it.
    /// The name in parentheses holds a space and a parenthesis, which is what
    /// splitting the line on whitespace would trip on.
    #[test]
    fn a_shell_is_busy_when_the_terminal_is_not_its_own() {
        let idle = "4242 (fi(sh) one) S 4200 4242 4242 34816 4242 4194304 1 2 3 4";
        assert!(!running_command(idle), "sitting at its prompt");
        let running = "4242 (fi(sh) one) S 4200 4242 4242 34816 4711 4194304 1 2 3 4";
        assert!(running_command(running), "a job in front of it");
        let detached = "4242 (bash) S 4200 4242 4242 0 -1 4194304 1 2 3 4";
        assert!(!running_command(detached), "no terminal at all");
        assert!(!running_command("nonsense"));
    }

    /// Runs a real program in a real pty and reads the result off the grid. It
    /// is the only test proving the whole chain — pty, input/output loop,
    /// parser, snapshot; everything else checks isolated pieces.
    #[test]
    fn a_real_command_reaches_the_grid() {
        let terminal = Terminal::spawn(Spawn {
            working_directory: &std::env::temp_dir(),
            // `printf` rather than an interactive shell: no prompt, no user
            // configuration file, a predictable output.
            command: Some((
                "/bin/sh".into(),
                vec!["-c".into(), "printf 'claudhub \\033[31mred\\033[0m'".into()],
            )),
            env: HashMap::new(),
            size: TermSize::new(40, 5, 8, 16),
            scrollback: 100,
        })
        .expect("the system must be able to open a pty");

        // Reading is asynchronous: we wait for the first line to fill, with a
        // deadline that fails rather than hangs.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut snapshot = terminal.snapshot();
        while std::time::Instant::now() < deadline
            && !snapshot.lines[0].text.starts_with("claudhub")
        {
            std::thread::sleep(std::time::Duration::from_millis(25));
            snapshot = terminal.snapshot();
        }

        assert_eq!(
            snapshot.lines[0].text, "claudhub red",
            "the program's output did not reach the grid"
        );

        // And the colour the program emitted is indeed carried by the run.
        let red = snapshot.lines[0]
            .segments
            .iter()
            .find(|s| &snapshot.lines[0].text[s.start..s.end] == "red")
            .expect("the coloured word must form its own run");
        assert!(
            matches!(red.fg, Paint::Rgb(..)),
            "the program's red was not resolved: {:?}",
            red.fg
        );
    }
}
