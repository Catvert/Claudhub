//! The terminals: one tab group per worktree.
//!
//! Each tab is a `Terminal` (pty + alacritty emulation) and a view that draws
//! it. The multiplexing is here and not in tmux: the tabs are attached to a
//! worktree, changing worktree changes group, and closing a worktree closes what
//! was running in it.
//!
//! The rendering is text, not a canvas: a grid line becomes `StyledText`, whose
//! style runs come from the snapshot, and gpui takes care of shaping, ligatures
//! and complex scripts — which a cell-by-cell renderer would have had to
//! reimplement.
//!
//! But a fixed-pitch font is **not** enough to line the columns up: a character
//! the font does not carry is drawn by whatever the system falls back to, whose
//! advance has no reason to be a cell wide. Shaped inside a run, it pushes
//! everything to its right — an agent's spinner cycles through four dingbats
//! Iosevka's subset leaves out, and the whole status line jittered left and
//! right once a frame. Each run is therefore **pinned to its column**, and a
//! character measured off the grid is given a box of its own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::{
    div, prelude::*, px, App, Bounds, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, Hsla, InputHandler, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Render, ScrollWheelEvent, SharedString, StyledText, TextRun,
    UTF16Selection, Window,
};
use gpui_component::{
    dock::{DockPlacement, InsertTarget, PanelId},
    menu::{ContextMenuExt, PopupMenuItem},
    v_flex, ActiveTheme, WindowExt as _,
};

use crate::terminal::{
    key_bytes, mouse, Paint, Segment, SelectionKind, Side, Snapshot, Spawn, TermSize, Terminal,
    TerminalEvent, ViewportPosition,
};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::{Settings, TerminalSettings};

/// Time allowed to a just-launched agent to show its prompt.
///
/// Nothing in a pty says "I am ready": what arrives before the prompt is read by
/// the shell we have not replaced yet, or lost. Two seconds cover an agent
/// starting on a loaded machine.
const AGENT_WARMUP: std::time::Duration = std::time::Duration::from_millis(2000);

/// The silence between the paste and the carriage return that confirms it.
const SUBMIT_DELAY: std::time::Duration = std::time::Duration::from_millis(120);

/// One terminal tab.
pub struct TerminalView {
    terminal: Terminal,
    snapshot: Snapshot,
    /// The snapshot cut into the runs the view paints, one entry per line.
    ///
    /// Held rather than worked out at render: cutting the runs and slicing
    /// their text is one walk of the visible grid, and it answers the same
    /// thing for as long as the snapshot, the font and the measured glyphs
    /// stay put. What invalidates it is `restyled`.
    painted: Vec<Vec<Painted>>,
    /// The runs have to be cut again: a new snapshot, a new font, or a glyph
    /// that has just been measured.
    restyled: bool,
    /// The title the running program has set, ready to be handed to a tab.
    ///
    /// Kept as a `SharedString` because the dock asks every tab for its title
    /// at every frame, and `Terminal` holds a `String` — it knows no gpui type.
    title: SharedString,
    focus: FocusHandle,
    font_size: Pixels,
    /// The effective font, re-read on every render from the settings: that is
    /// what makes a change in the form visible without reopening the tab.
    font_family: SharedString,
    /// The last known size of the render area, so the pty is only resized when
    /// the geometry really changes.
    bounds: Bounds<Pixels>,
    /// A cell's geometry, measured on the effective font. It serves to translate
    /// a mouse position back into a line and a column.
    cell: gpui::Size<Pixels>,
    /// Which characters the terminal font places on the grid.
    ///
    /// `advance` resolves a glyph in **that** font and fails when it has none,
    /// which is exactly the question: a character it cannot draw is a character
    /// some other font will, at its own width. The answer is cached because the
    /// visible grid is walked at every frame; `sync_font` empties it, coverage
    /// being a property of the family.
    on_grid: HashMap<char, bool>,
    /// True between button press and release: that is what tells a selection
    /// drag from a plain hover.
    selecting: bool,
    /// The cell the mouse was last reported at.
    ///
    /// A movement is measured in pixels and a report in cells: without this
    /// note, crossing a single cell would send ten identical events to the
    /// program, which would redraw ten times.
    mouse_cell: Option<(usize, usize)>,
    /// The fraction of a line left over by the last wheel event.
    scroll_remainder: f32,
    /// The geometry layout asked for, not yet passed on.
    pending_size: Option<TermSize>,
    /// True when a deferred transmission is already scheduled.
    resize_scheduled: bool,
    /// True when the geometry has changed since the running wait last looked.
    ///
    /// That is what makes the wait *trailing*: it restarts as long as the hand
    /// is still moving, so the pty is resized once, at the end.
    resize_moved: bool,
    label: SharedString,
    /// True when this tab runs a coding agent.
    ///
    /// It is what makes it possible to deliver review notes to it without
    /// picking the wrong tab. Recorded at opening and not derived from the
    /// title: an agent renames its tab as the conversation goes, and looking for
    /// its name in a changing title would be guesswork.
    agent: bool,
    /// True when the tab was launched **on a command** rather than on a shell.
    ///
    /// An agent, a `wt` task, a `just` recipe: three ways of asking for the same
    /// thing, and what they have in common is that the pty's child *is* the
    /// command. Recorded at opening, like `agent`, and for the same reason —
    /// nothing about the running process says it afterwards.
    runs_command: bool,
}

/// The pty's child has exited, and the tab was a shell — nothing to keep.
pub struct TerminalExited;

impl EventEmitter<TerminalExited> for TerminalView {}

impl TerminalView {
    /// Opens a pty. Separate from the view because it is the only step that can
    /// fail, and a failure in an entity constructor leaves no way out but a
    /// panic — during a render, so with a frozen window as its only message.
    pub fn open(
        working_directory: &Path,
        launch: &Launch,
        settings: &TerminalSettings,
        wsl: Option<&WslShell>,
    ) -> anyhow::Result<Terminal> {
        // An ordinary tab takes the settings' program; an explicit command — the
        // agent, a `wt` task — comes first, it is precisely what was asked to be
        // launched.
        let command = launch.command.clone().or_else(|| settings.program());
        let spawned = match wsl {
            Some(wsl) => wsl.wrap(working_directory, command, &launch.env),
            None => Spawned {
                cwd: working_directory.to_path_buf(),
                command,
                env: launch.env.clone(),
            },
        };
        Terminal::spawn(Spawn {
            working_directory: &spawned.cwd,
            command: spawned.command,
            env: spawned.env,
            // The real size arrives on the first render; this one only serves to
            // give the shell a plausible geometry before its first prompt.
            size: TermSize::new(80, 24, 8, 16),
            scrollback: settings.scrollback,
        })
    }

    pub fn attach(
        terminal: Terminal,
        label: SharedString,
        agent: bool,
        runs_command: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = Settings::global(cx);
        let font_size = px(settings.terminal.font_size);
        let font_family = SharedString::from(settings.terminal_font().to_string());
        let events = terminal.events();
        // One foreground task per terminal: it wakes the view when the I/O loop
        // has something new. Without it, the output would only appear at the
        // next render triggered by something else.
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                // What the I/O loop has already queued behind this one, taken
                // in the same breath: a wake-up is emitted per `pty_read`, and
                // a `cat` of a large file sends hundreds a second. Each one
                // used to copy the whole grid; what they all say is the same
                // thing — "redraw" — so they are drained here and answered
                // with a single snapshot. What a queued event *carries* — a
                // title, a child's death — is handled on the way through.
                let mut batch = vec![event];
                while let Ok(next) = events.try_recv() {
                    batch.push(next);
                }
                let alive = this
                    .update(cx, |view, cx| {
                        for event in batch {
                            match event {
                                TerminalEvent::Wakeup => {}
                                TerminalEvent::Title(title) => {
                                    view.title = SharedString::from(title.clone());
                                    view.terminal.set_title(title);
                                }
                                TerminalEvent::Bell => {}
                                // The child is gone and the tab has nothing left to
                                // show: `exit` in fish, `Ctrl+D`, a `just` recipe or a
                                // `wt` task that has run its course. The application
                                // closes the tab; the view only says so, being the one
                                // that hears the pty.
                                //
                                // **Only on a success.** A failure is the one thing
                                // one comes back to read — the failed test, the build
                                // that stopped on an error —, and a tab that closes
                                // itself takes the message with it. Everything else
                                // goes: a shell exits with 0, and so does a recipe
                                // that ran its course.
                                //
                                // A death by signal has no code, and neither has the
                                // loop's own end, which follows the child's: an exit
                                // one did not ask for keeps its tab.
                                TerminalEvent::Exited(code) => {
                                    view.terminal.mark_exited();
                                    if code == Some(0) {
                                        cx.emit(TerminalExited);
                                    }
                                }
                            }
                        }
                        view.take_snapshot();
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();

        Self {
            snapshot: terminal.snapshot(),
            painted: Vec::new(),
            restyled: true,
            title: SharedString::default(),
            terminal,
            focus: cx.focus_handle(),
            font_size,
            font_family,
            bounds: Bounds::default(),
            cell: gpui::size(px(8.), px(16.)),
            on_grid: HashMap::new(),
            selecting: false,
            mouse_cell: None,
            scroll_remainder: 0.,
            pending_size: None,
            resize_scheduled: false,
            resize_moved: false,
            label,
            agent,
            runs_command,
        }
    }

    pub fn is_agent(&self) -> bool {
        self.agent
    }

    /// Delivers a text to the running program, without confirming it.
    ///
    /// Goes through the **bracketed paste** `Terminal::paste` handles: without
    /// it, a multi-line text arrives in a shell as that many commands entered,
    /// which is the classic way of accidentally running what you only meant to
    /// have read.
    pub fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.terminal.paste(text);
        self.terminal.scroll_to_bottom();
        self.take_snapshot();
        cx.notify();
    }

    /// Confirms what has just been pasted.
    ///
    /// **Always in a send separate from the paste**, never at the end of the
    /// same one: a TUI that has just received a bracketed paste may swallow the
    /// carriage return following it in the same packet, and the message then
    /// stays in the prompt without going out.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        self.terminal.write_str("\r");
        self.terminal.scroll_to_bottom();
        self.take_snapshot();
        cx.notify();
    }

    /// Whether something is running in there — what closing has to ask.
    ///
    /// Two shapes, and only one of them is job control. A **shell** is busy when
    /// a command holds the pty's foreground group (`Terminal::busy`). A tab
    /// **launched on a command** has no prompt to come back to — its child *is*
    /// the command — so it counts as busy for as long as it lives.
    ///
    /// That second half is not the agent's alone, and reading it off the profile
    /// was the bug: a `just` recipe and a `wt` task go through `sh -lc`, which
    /// **execs** the command it was given rather than keeping a shell in front
    /// of it, so the pty's child holds the foreground group itself — exactly
    /// what a shell at its prompt looks like. A recipe that had been building
    /// for two minutes was therefore closed without a word.
    ///
    /// What tells the two apart is the launch, and not "we named a program":
    /// the shell itself is named too, every tab of this window launching the one
    /// from the settings. The cost is a tab opened on an interactive command —
    /// a task whose `run` is `nix-shell` — asking to be closed even while it
    /// sits at a prompt, which is the side to be wrong on.
    pub fn busy(&self) -> bool {
        if self.terminal.has_exited() {
            return false;
        }
        self.runs_command || self.terminal.busy()
    }

    pub fn label(&self) -> SharedString {
        // The kept `SharedString` and not `Terminal::title`: the dock asks
        // every tab for its title at every frame, and that one would copy the
        // string each time.
        if self.title.is_empty() {
            self.label.clone()
        } else {
            self.title.clone()
        }
    }

    /// Brings the font into line with the current settings.
    ///
    /// A change of size or family invalidates the measured geometry: we clear
    /// the recorded bounds so the measuring canvas's next pass recomputes the
    /// grid and resizes the pty. Without that, the text would change size but
    /// the shell would keep believing in the old column width.
    fn sync_font(&mut self, cx: &App) {
        let settings = Settings::global(cx);
        let font_size = px(settings.terminal.font_size);
        let font_family = settings.terminal_font();
        if font_size == self.font_size && font_family == self.font_family.as_ref() {
            return;
        }
        self.font_size = font_size;
        self.font_family = SharedString::from(font_family.to_string());
        self.bounds = Bounds::default();
        self.on_grid.clear();
        // Everything measured is measured against the family: what was cut on
        // the old one says nothing about this one.
        self.restyled = true;
    }

    pub fn has_exited(&self) -> bool {
        self.terminal.has_exited()
    }

    /// What the grid **will** be, while the hand is still moving.
    ///
    /// The pty is only resized once the drag stops, so what is painted
    /// underneath during that time is the old grid, clipped — which reads as a
    /// frozen terminal. This badge is the whole answer: it says the geometry is
    /// on its way and what it will be, the way every tiling window manager
    /// does. It exists because of the multiplexer, where one drag moves a dozen
    /// terminals at once, and it is just as right on a dock splitter.
    fn render_pending_size(
        &self,
        font_size: Pixels,
        cx: &Context<Self>,
    ) -> Option<impl IntoElement> {
        self.pending_size.map(|size| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .text_size(font_size)
                        .text_color(cx.theme().popover_foreground)
                        .child(SharedString::from(format!(
                            "{} × {}",
                            size.columns, size.lines
                        ))),
                )
        })
    }

    /// Reads the grid, and says the runs have to be cut again.
    ///
    /// The one door in: what is painted is derived from the snapshot, and a
    /// snapshot replaced behind the derivation's back is a frame drawn from
    /// the grid as it was.
    fn take_snapshot(&mut self) {
        self.snapshot = self.terminal.snapshot();
        self.restyled = true;
    }

    /// Cuts the snapshot's lines into the runs the view paints.
    ///
    /// Once per snapshot and not once per frame: the cut asks `on_grid` about
    /// every character on screen and slices the line's text for every run —
    /// a walk of the grid, which is what a frame must not carry.
    fn restyle(&mut self) {
        self.painted = self
            .snapshot
            .lines
            .iter()
            .map(|line| painted_line(line, &self.on_grid))
            .collect();
        self.restyled = false;
    }

    /// Measures the characters on screen that have not been measured yet.
    ///
    /// One walk of the visible grid per frame, one measurement per character
    /// ever. `advance` resolves the glyph in **this** font and fails when it
    /// has none: that failure is the answer we are after, since what the font
    /// misses is drawn by another one, at another width.
    fn learn_glyphs(&mut self, window: &Window) {
        let unknown: Vec<char> = self
            .snapshot
            .lines
            .iter()
            .flat_map(|line| line.text.chars())
            .filter(|ch| !self.on_grid.contains_key(ch))
            .collect();
        if unknown.is_empty() {
            return;
        }
        let font_id = window.text_system().resolve_font(&gpui::Font {
            family: self.font_family.clone(),
            features: Default::default(),
            weight: Default::default(),
            style: Default::default(),
            fallbacks: None,
        });
        let width = f32::from(self.cell.width);
        for ch in unknown {
            let on_grid = window
                .text_system()
                .advance(font_id, self.font_size, ch)
                .is_ok_and(|advance| (f32::from(advance.width) - width).abs() <= 0.01);
            self.on_grid.insert(ch, on_grid);
        }
        // A character that has just been measured may cut a run in two.
        self.restyled = true;
    }

    /// Recomputes the grid for the available room.
    ///
    /// A character's width is measured on the font actually chosen, not guessed:
    /// a fixed pitch does not mean a known width, and a one-pixel discrepancy
    /// shifts the last column of an eighty-wide line.
    fn sync_size(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if bounds == self.bounds {
            return;
        }
        self.bounds = bounds;

        let font = gpui::Font {
            family: self.font_family.clone(),
            features: Default::default(),
            weight: Default::default(),
            style: Default::default(),
            fallbacks: None,
        };
        let font_id = window.text_system().resolve_font(&font);
        let cell_width = window
            .text_system()
            .advance(font_id, self.font_size, 'M')
            .map(|s| s.width)
            .unwrap_or(self.font_size * 0.6);
        let line_height = window.line_height().max(px(1.));

        self.cell = gpui::size(cell_width.max(px(1.)), line_height);
        let (columns, lines) = grid_size(bounds.size, self.cell);
        self.request_size(
            TermSize::new(
                columns,
                lines,
                f32::from(cell_width) as u16,
                f32::from(line_height) as u16,
            ),
            cx,
        );
    }

    /// Passes the new geometry on, once the drag has **stopped**.
    ///
    /// A mouse resize goes through every intermediate width. Passing them all on
    /// amounts to sending one `SIGWINCH` per frame: the program redraws every
    /// time, and since it redraws *in place*, its successive prompts pile up
    /// instead of replacing each other. So we wait for the size to settle;
    /// meanwhile, the panel clips the old grid, exactly as a window being
    /// resized does, and `pending_size` is what the tile paints over it.
    ///
    /// **The wait restarts on every change**, which is the difference between
    /// "one resize every 150 ms while the hand moves" and "one resize when the
    /// hand stops". It only started to matter when a dozen ptys came to depend
    /// on a single drag — the multiplexer's grid — and it is the honest reading
    /// of the sentence above: what is worth passing on is a geometry someone
    /// has settled on.
    ///
    /// Bounded all the same (`MAX_DEFERRALS`): a size that changed forever —
    /// an animation, a layout that never converges — would otherwise leave the
    /// program at a geometry that has not existed for a long time, and nothing
    /// would say why.
    fn request_size(&mut self, size: TermSize, cx: &mut Context<Self>) {
        if self.terminal.size() == size {
            self.pending_size = None;
            return;
        }
        self.pending_size = Some(size);
        // Noted whether or not a wait is already running: it is what tells the
        // one that is running that the hand has moved again.
        self.resize_moved = true;
        if self.resize_scheduled {
            return;
        }
        self.resize_scheduled = true;
        cx.spawn(async move |this, cx| {
            for _ in 0..MAX_DEFERRALS {
                cx.background_executor().timer(RESIZE_QUIET).await;
                let moved = this
                    .update(cx, |this, _| std::mem::take(&mut this.resize_moved))
                    .unwrap_or(false);
                if !moved {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.resize_scheduled = false;
                this.resize_moved = false;
                if let Some(size) = this.pending_size.take() {
                    this.terminal.resize(size);
                    this.snapshot = this.terminal.snapshot();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(bytes) = key_bytes(&event.keystroke, self.terminal.mode()) {
            // Consuming is half of `key_bytes`' contract: a propagated
            // keystroke is re-delivered as text to the input handler by the
            // platform (Linux directly, Windows through the WM_CHAR an
            // unconsumed keydown generates), and a space or an enter handled
            // here would then arrive twice.
            cx.stop_propagation();
            self.send_bytes(bytes, cx);
        }
    }

    /// A keystroke's bytes into the pty — from `on_key`, or from the two
    /// actions that reclaim Tab from the root's focus cycling.
    fn send_bytes(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        // Typing invalidates the selection: what it named will have moved as
        // soon as the program answers.
        self.terminal.clear_selection();
        // Every keystroke brings the view back to the bottom: that is what a
        // terminal does, and typing after scrolling back without the view
        // following would be disconcerting.
        self.terminal.scroll_to_bottom();
        self.terminal.write(bytes);
        self.take_snapshot();
        cx.notify();
    }

    /// Translates a window position into a viewport cell.
    ///
    /// The side (`Side`) comes from the half of the cell the pointer falls in:
    /// selecting from a character's right half must not include it, as in an
    /// editor.
    fn position_at(&self, point: Point<Pixels>) -> ViewportPosition {
        viewport_position(point - self.bounds.origin, self.cell)
    }

    /// Reports a mouse event to the program, if it is listening.
    ///
    /// Returns true when it has received it: the gesture then belongs entirely
    /// to it, and the view has neither a selection to extend nor scrollback to
    /// go up. **Shift is the escape hatch** — it is every terminal's convention,
    /// and without it nothing could be copied from a program that takes the
    /// mouse.
    fn report_mouse(
        &mut self,
        button: Option<mouse::Button>,
        action: mouse::Action,
        position: Point<Pixels>,
        modifiers: gpui::Modifiers,
    ) -> bool {
        if modifiers.shift || !self.terminal.reports_mouse() {
            return false;
        }
        let cell = self.position_at(position);
        let (column, line) = (cell.column, cell.line);
        // A movement is only worth reporting when the cell changes: the program
        // redraws on every event, and a movement of the hand crosses a dozen.
        // Nothing was sent, hence the `false` — there is no local gesture to
        // make out of a hover anyway.
        if action == mouse::Action::Move && self.mouse_cell == Some((column, line)) {
            return false;
        }
        self.mouse_cell = Some((column, line));
        self.terminal.report_mouse(mouse::Report {
            button,
            action,
            column,
            line,
            modifiers,
        })
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        if event.button != MouseButton::Left {
            return;
        }
        // Only the left button goes to the program. The middle one pastes the
        // primary selection and the right one opens Claudhub's menu — which is
        // precisely where copying happens, something a program taking the mouse
        // would otherwise make impossible.
        if self.report_mouse(
            Some(mouse::Button::Left),
            mouse::Action::Press,
            event.position,
            event.modifiers,
        ) {
            // A selection left behind would paint over what the program draws,
            // with no way left to remove it.
            self.terminal.clear_selection();
            self.take_snapshot();
            cx.notify();
            return;
        }
        let kind = match event.click_count {
            1 => SelectionKind::Simple,
            2 => SelectionKind::Word,
            // Past three, it is still the line: nobody counts clicks beyond
            // that, and resetting would be disconcerting.
            _ => SelectionKind::Line,
        };
        self.terminal
            .start_selection(self.position_at(event.position), kind);
        self.selecting = true;
        self.take_snapshot();
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A drag is not a hover: the program asks for them separately, and
        // `mouse::report` sorts them out.
        let held =
            matches!(event.pressed_button, Some(MouseButton::Left)).then_some(mouse::Button::Left);
        if !self.selecting
            && self.report_mouse(held, mouse::Action::Move, event.position, event.modifiers)
        {
            return;
        }
        if !self.selecting {
            return;
        }
        self.terminal
            .update_selection(self.position_at(event.position));
        self.take_snapshot();
        cx.notify();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // The release follows the press: if that one went to the program, there
        // is no selection under way, and that is what `selecting` says.
        if event.button != MouseButton::Left {
            return;
        }
        if !self.selecting
            && self.report_mouse(
                Some(mouse::Button::Left),
                mouse::Action::Release,
                event.position,
                event.modifiers,
            )
        {
            return;
        }
        self.selecting = false;
        // An empty selection is a plain click: keeping it would leave an
        // invisible remnant that would make the next copy fail.
        if !self.terminal.has_selection() {
            self.terminal.clear_selection();
            self.take_snapshot();
            cx.notify();
        }
    }

    /// Middle button: pastes X11/Wayland's primary selection, like every Unix
    /// terminal.
    ///
    /// It exists **only** there: Windows has a single clipboard, and gpui
    /// therefore exposes nothing to read — the middle button then has nothing to
    /// paste. Pasting the clipboard instead would be worse than doing nothing:
    /// the gesture does not mean that, and an unlucky click would dump into the
    /// terminal what had been copied for elsewhere.
    fn on_middle_click(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        if let Some(text) = cx
            .read_from_primary()
            .and_then(|item| item.text())
            .filter(|t| !t.is_empty())
        {
            self.terminal.paste(&text);
            self.terminal.scroll_to_bottom();
            self.take_snapshot();
            cx.notify();
        }
        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
        let _ = cx;
    }

    /// Clears the scrollback and the screen.
    ///
    /// The escape hatch when a program has left the terminal in an unreadable
    /// state: the shell does have a `clear`, but its prompt still has to answer.
    pub fn clear_scrollback(&mut self, cx: &mut Context<Self>) {
        self.terminal.clear();
        self.take_snapshot();
        cx.notify();
    }

    pub fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.terminal.selection_text().filter(|t| !t.is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub fn paste_from_clipboard(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .filter(|t| !t.is_empty())
        {
            self.terminal.paste(&text);
            self.terminal.scroll_to_bottom();
            self.take_snapshot();
            cx.notify();
        }
    }

    /// Selects all the visible content and the scrollback.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.terminal.select_all();
        self.take_snapshot();
        cx.notify();
    }

    /// Draws the cursor.
    ///
    /// A semi-transparent rectangle laid over the grid rather than an inverted
    /// cell: inversion would mean redrawing the glyph the other way round,
    /// whereas a translucent background lets the character underneath be read,
    /// which is all one asks of a block cursor.
    ///
    /// It does not blink. Blinking wakes the interface twice a second per tab,
    /// permanently, for information the position and the contrast already give;
    /// out of focus, the outline alone says well enough that typing would go
    /// elsewhere.
    fn render_cursor(&self, focused: bool, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let cursor = self.snapshot.cursor?;
        // Outside the visible area: the scrollback has been scrolled, and the
        // cursor stayed at the bottom with the program.
        let line = cursor.line?;
        if !cursor.visible || self.terminal.has_exited() {
            return None;
        }

        let color = cx.theme().caret;
        let element = div()
            .absolute()
            .left(self.cell.width * cursor.column as f32)
            .top(self.cell.height * line as f32)
            .w(self.cell.width)
            .h(self.cell.height)
            .when(focused, |el| el.bg(color.opacity(0.55)))
            .when(!focused, |el| {
                el.border_1().border_color(color.opacity(0.7))
            });
        Some(element)
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        // **The wheel stops here**, whatever comes of it. A terminal is the
        // only view that answers a notch with something other than a scroll —
        // arrows in the alternate screen, a report to the program, a zoom —
        // and the multiplexer's grid, which does scroll, sits above a dozen of
        // them: without this, one notch over a tile would move the tile *and*
        // the grid under it. A fraction of a line that has not added up to one
        // yet is consumed just the same, or the leftovers would leak upward.
        cx.stop_propagation();
        let line_height = window.line_height().max(px(1.));
        // The platform key changes the wheel's meaning: we enlarge the text
        // instead of scrolling back. The terminal handles its own scrolling, so
        // it is enough not to do it.
        if event.modifiers.secondary() {
            let steps = zoom_steps(event.delta.pixel_delta(line_height).y);
            if steps != 0. {
                Settings::update_global(cx, |s| {
                    s.zoom(crate::ui::settings::Zoom::Terminal, steps);
                });
            }
            return;
        }
        // A cell's height, and not the ambient text's: it is what gives the
        // number of lines of a pixel movement, and they differ as soon as the
        // terminal is not the interface's size.
        let cell = self.cell.height.max(px(1.));
        let lines = take_lines(
            &mut self.scroll_remainder,
            f32::from(event.delta.pixel_delta(cell).y),
            f32::from(cell),
        );
        if lines == 0 {
            return;
        }

        // The program asked for the mouse: the wheel belongs to it, and this is
        // the only case where it arrives as it is. One notch per line, as every
        // terminal does — the program decides what a notch is worth on its side.
        let button = if lines > 0 {
            mouse::Button::WheelUp
        } else {
            mouse::Button::WheelDown
        };
        if !event.modifiers.shift && self.terminal.reports_mouse() {
            let cell = self.position_at(event.position);
            for _ in 0..lines.unsigned_abs() {
                self.terminal.report_mouse(mouse::Report {
                    button: Some(button),
                    action: mouse::Action::Press,
                    column: cell.column,
                    line: cell.line,
                    modifiers: event.modifiers,
                });
            }
            return;
        }

        // In the alternate screen — an agent, `less`, `vim` — there is no
        // scrollback to go up: the grid is what the program draws, and it alone
        // knows what is above. The wheel is therefore translated into arrows
        // there, as in every terminal when nobody listens to the mouse; without
        // that it does nothing at all.
        if self.terminal.in_alternate_screen() {
            let key = if lines > 0 { "up" } else { "down" };
            let repeats = lines.unsigned_abs() as usize * ALT_SCREEN_LINES;
            if let Some(bytes) = arrow_bytes(key, self.terminal.mode()) {
                for _ in 0..repeats {
                    self.terminal.write(bytes.clone());
                }
            }
            return;
        }

        self.terminal.scroll(lines);
        self.take_snapshot();
        cx.notify();
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let default_fg = cx.theme().foreground;
        // What an inverse cell on default colours paints its glyph in: the
        // colour behind the grid, set on the root below.
        let default_bg = cx.theme().background;
        self.sync_font(cx);
        // Both only answer for the snapshot on screen: they run when it — or
        // the font under it — has moved, never on a frame that repeats one.
        if self.restyled {
            self.learn_glyphs(window);
            self.restyle();
        }
        let font_size = self.font_size;
        let font_family = self.font_family.clone();
        let entity = cx.entity();

        // The measuring happens in a background `canvas`, which receives the
        // final geometry after layout. Computing it during the list's render
        // would need a size nobody knows yet.
        let measure = gpui::canvas(
            move |bounds, window, cx| {
                entity.update(cx, |view, cx| view.sync_size(bounds, window, cx));
            },
            {
                // The paint phase is where an input handler registers, and
                // registering is what routes composed text here — see
                // `TerminalInputHandler`.
                let handler = TerminalInputHandler { view: cx.entity() };
                let focus = self.focus.clone();
                move |_, _, window, cx| window.handle_input(&focus, handler, cx)
            },
        )
        .absolute()
        .size_full();

        let selection_bg = cx.theme().selection;
        // Each line inside a box one cell high, which does not wrap and clips
        // what overflows.
        //
        // Without that, a line wider than the panel is *wrapped* by gpui: it
        // takes two heights, pushes everything after it down and the grid no
        // longer matches what the program thinks it is showing. That is what
        // showed after shrinking then reopening the panel — the geometry is
        // measured after layout, so the grid stays too wide for one frame, and
        // the wrapping that follows throws everything out of line.
        let cell = self.cell;
        let lines: Vec<_> = self
            .snapshot
            .lines
            .iter()
            .zip(self.painted.iter())
            .map(|(line, painted)| {
                div()
                    .h(cell.height)
                    // The runs are placed at their column, so the box they sit
                    // in is the origin they are measured from.
                    .relative()
                    // **A line is never squeezed**, and this is not a
                    // refinement: the box holds `size_full`, so as soon as the
                    // grid has more lines than fit — which is the whole of a
                    // shrinking resize, the pty being told only once the hand
                    // stops, and the whole of a spawn, whose 80×24 lands in
                    // whatever room there is — flexbox takes the height back
                    // out of every child. The glyphs are then drawn crushed,
                    // and it reads as the terminal scaling its own picture. A
                    // line that keeps its height simply overflows the bottom,
                    // which `overflow_hidden` clips: exactly what a window
                    // being resized does.
                    .flex_shrink_0()
                    .w_full()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .children(line_boxes(
                        line,
                        painted,
                        cell,
                        &font_family,
                        default_fg,
                        default_bg,
                        selection_bg,
                    ))
            })
            .collect();

        v_flex()
            .id("terminal")
            .key_context(crate::ui::shortcuts::terminal_context())
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &crate::ui::shortcuts::CopySelection, _, cx| {
                    this.copy_selection(cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::ui::shortcuts::PasteClipboard, _, cx| {
                    this.paste_from_clipboard(cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::ui::shortcuts::SelectAllText, _, cx| {
                    this.select_all(cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::ui::shortcuts::SendTab, _, cx| {
                    this.send_bytes(b"\t".to_vec(), cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::ui::shortcuts::SendBacktab, _, cx| {
                    this.send_bytes(b"\x1b[Z".to_vec(), cx)
                }),
            )
            .size_full()
            .relative()
            .bg(cx.theme().background)
            // The last background painted is what decides the card's bottom
            // corners: gpui's content mask is rectangular, and
            // `panels::pane_frame`'s rounding does not clip its children. This
            // background covers the panel's whole surface — without this
            // rounding, the terminal stays square at the bottom whatever is
            // painted underneath.
            .rounded_b(cx.theme().radius_lg)
            .font_family(font_family.clone())
            .text_size(font_size)
            .on_key_down(cx.listener(Self::on_key))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_middle_click))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            // The right click: the gestures one looks for first in a terminal,
            // and which `Ctrl+C` cannot carry — it belongs to the running
            // program.
            .context_menu({
                let entity = cx.entity();
                move |menu, _window, _cx| terminal_menu(menu, &entity)
            })
            .child(measure)
            .child(v_flex().size_full().overflow_hidden().children(lines))
            .children(self.render_cursor(focused, cx))
            // The grid on its way, while the hand is still moving.
            .children(self.render_pending_size(font_size, cx))
    }
}

/// The window's text route into the terminal.
///
/// Everything the keyboard *composes* arrives here, not as keystrokes: the ê
/// of a dead ^, the @ a Belgian AltGr+2 produces, what an IME commits. On
/// Windows a composed character exists only in the `WM_CHAR` that follows an
/// unconsumed keydown — the keydown itself carries at best the dead key — and
/// `WM_CHAR` goes to the focused input handler; without one, the character
/// vanished, and the Ctrl+Alt disguise of AltGr sent nothing at all. Linux
/// hands a propagated keystroke's text and every IME commit to the same
/// handler. `key_bytes` therefore emits nothing for plain text, and `on_key`
/// consumes what it does emit — three pieces of one contract, and breaking any
/// of them types letters twice or not at all.
struct TerminalInputHandler {
    view: Entity<TerminalView>,
}

impl InputHandler for TerminalInputHandler {
    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.view
            .update(cx, |view, cx| view.send_bytes(text.as_bytes().to_vec(), cx));
    }

    /// The IME preedit. Nothing is shown and nothing is sent: only committed
    /// text belongs to the pty, and the candidate window shows the draft.
    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        _text: &str,
        _selection: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut App) {}

    /// A zero-width caret, so the platform has a selection to anchor the IME
    /// candidate window to — a terminal has no text document to offer.
    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &mut self,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<std::ops::Range<usize>> {
        None
    }

    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<String> {
        None
    }

    /// Where the IME candidate window opens: the cursor's cell.
    fn bounds_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        let view = self.view.read(cx);
        let cell = view.cell;
        let mut origin = view.bounds.origin;
        if let Some(cursor) = view.snapshot.cursor {
            if let Some(line) = cursor.line {
                origin.x += cell.width * cursor.column as f32;
                origin.y += cell.height * line as f32;
            }
        }
        Some(Bounds { origin, size: cell })
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        None
    }
}

/// What a right click on a terminal offers.
///
/// The gestures one looks for first in a terminal, and which `Ctrl+C` cannot
/// carry — it belongs to the running program. A function of its own so that
/// `render` reads as the shape of the panel rather than as a menu with a
/// terminal around it.
fn terminal_menu(
    menu: gpui_component::menu::PopupMenu,
    entity: &gpui::Entity<TerminalView>,
) -> gpui_component::menu::PopupMenu {
    let (copy, paste) = (entity.clone(), entity.clone());
    let (all, clear) = (entity.clone(), entity.clone());
    menu.item(
        PopupMenuItem::new(tr!("terminal-copy"))
            .icon(icon("copy"))
            .on_click(move |_, _window, cx| {
                copy.update(cx, |this, cx| this.copy_selection(cx));
            }),
    )
    .item(
        PopupMenuItem::new(tr!("terminal-paste"))
            .icon(icon("clipboard-paste"))
            .on_click(move |_, _window, cx| {
                paste.update(cx, |this, cx| this.paste_from_clipboard(cx));
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(tr!("terminal-select-all"))
            .icon(icon("text-select"))
            .on_click(move |_, _window, cx| {
                all.update(cx, |this, cx| this.select_all(cx));
            }),
    )
    .item(
        PopupMenuItem::new(tr!("terminal-clear"))
            .icon(icon("eraser"))
            .on_click(move |_, _window, cx| {
                clear.update(cx, |this, cx| this.clear_scrollback(cx));
            }),
    )
}

/// Converts a pixel movement into whole lines.
///
/// The remainder is kept from one event to the next: a trackpad sends fractions
/// of a line, and rounding each to zero makes scrolling inert although they add
/// up to lines.
pub fn take_lines(remainder: &mut f32, pixels: f32, cell: f32) -> i32 {
    *remainder += pixels / cell.max(1.);
    let lines = remainder.trunc();
    *remainder -= lines;
    lines as i32
}

/// Lines sent per wheel notch in the alternate screen.
///
/// Three: the terminals' convention, and what both `less` and `vim` treat as a
/// natural movement.
const ALT_SCREEN_LINES: usize = 3;

/// An arrow's bytes, as the program expects them.
fn arrow_bytes(key: &str, mode: alacritty_terminal::term::TermMode) -> Option<Vec<u8>> {
    crate::terminal::key_bytes(&gpui::Keystroke::parse(key).ok()?, mode)
}

/// The grid's floor: below it, the panel clips rather than ask the program to
/// fold into a space where it can show nothing.
const MIN_COLUMNS: usize = 20;
const MIN_LINES: usize = 3;

/// How many cells fit in this room.
///
/// The floor is not cosmetic: a panel shrunk to nothing would ask for a
/// two-column terminal, where the slightest prompt takes fifty lines. The
/// program redraws, the scrollback overflows, and only fragments are left. Below
/// it, the panel clips — which is also what a terminal window shrunk too far
/// does.
pub fn grid_size(space: gpui::Size<Pixels>, cell: gpui::Size<Pixels>) -> (usize, usize) {
    let columns = (space.width / cell.width.max(px(1.))) as usize;
    let lines = (space.height / cell.height.max(px(1.))) as usize;
    (columns.max(MIN_COLUMNS), lines.max(MIN_LINES))
}

/// The quiet time before a new geometry is passed to the pty.
const RESIZE_QUIET: std::time::Duration = std::time::Duration::from_millis(150);

/// How many quiet times a size may keep moving through before it is passed on
/// anyway. Three seconds: longer than a drag, shorter than a wait one would
/// blame on the terminal.
const MAX_DEFERRALS: usize = 20;

/// One wheel notch is worth one point of size.
///
/// The number of lines the wheel announces does not come into it: three points
/// per notch would make the setting unusable, and a trackpad would send dozens
/// per gesture.
pub fn zoom_steps(delta_y: Pixels) -> f32 {
    if delta_y > px(0.) {
        1.
    } else if delta_y < px(0.) {
        -1.
    } else {
        0.
    }
}

/// A piece of a run that can be shaped in one go.
///
/// A run is cut wherever a character is not on the grid, so that one is drawn
/// on its own cell and cannot push its neighbours.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Chunk {
    col: usize,
    cells: usize,
    /// Byte range in the line's text.
    start: usize,
    end: usize,
}

/// Cuts a run into the pieces the view paints.
///
/// The rule needs no width table: a run's characters are its columns — a wide
/// character is a run of its own (`terminal::snapshot`). What escapes that is a
/// character carrying combining marks, and there the whole run is pinned as one
/// piece: its inside may drift, the runs after it may not.
fn chunks(
    line: &crate::terminal::Line,
    seg: &Segment,
    on_grid: &HashMap<char, bool>,
) -> Vec<Chunk> {
    let text = &line.text[seg.start..seg.end];
    let whole = Chunk {
        col: seg.col,
        cells: seg.cells,
        start: seg.start,
        end: seg.end,
    };
    if seg.cells != text.chars().count() {
        return vec![whole];
    }

    let mut out: Vec<Chunk> = Vec::new();
    let mut open: Option<Chunk> = None;
    for (col, (offset, ch)) in (seg.col..).zip(text.char_indices()) {
        let start = seg.start + offset;
        let end = start + ch.len_utf8();
        // An unmeasured character is taken as on the grid: it is the answer for
        // everything the font carries, and the wrong guess only costs what it
        // costs today.
        if on_grid.get(&ch).copied().unwrap_or(true) {
            match open.as_mut() {
                Some(chunk) => {
                    chunk.end = end;
                    chunk.cells += 1;
                }
                None => {
                    open = Some(Chunk {
                        col,
                        cells: 1,
                        start,
                        end,
                    })
                }
            }
        } else {
            out.extend(open.take());
            out.push(Chunk {
                col,
                cells: 1,
                start,
                end,
            });
        }
    }
    out.extend(open.take());
    out
}

/// A run of a line, cut and sliced once per snapshot.
///
/// The text is a `SharedString` because it is one anyway by the time it is
/// painted: what is kept is the **copy**, which would otherwise be made again
/// for every run of every visible line, at every frame.
struct Painted {
    /// Which of the line's segments gives it its colours.
    segment: usize,
    chunk: Chunk,
    text: SharedString,
}

/// Cuts a line into its runs and slices their text — once per snapshot.
fn painted_line(line: &crate::terminal::Line, on_grid: &HashMap<char, bool>) -> Vec<Painted> {
    let mut out = Vec::new();
    for (segment, seg) in line.segments.iter().enumerate() {
        for chunk in chunks(line, seg, on_grid) {
            let text = SharedString::from(line.text[chunk.start..chunk.end].to_string());
            out.push(Painted {
                segment,
                chunk,
                text,
            });
        }
    }
    out
}

/// Converts a snapshot line into the boxes that draw it.
///
/// One box per chunk, **placed at its column** rather than laid end to end.
/// The background is the box's and no longer the run's: a rectangle then
/// covers exactly the cells it is meant to, whatever the glyph inside measures.
fn line_boxes(
    line: &crate::terminal::Line,
    painted: &[Painted],
    cell: gpui::Size<Pixels>,
    family: &SharedString,
    default_fg: Hsla,
    default_bg: Hsla,
    selection_bg: Hsla,
) -> Vec<gpui::Div> {
    let mut out = Vec::new();
    for run in painted {
        let Some(seg) = line.segments.get(run.segment) else {
            continue;
        };
        let (chunk, text) = (&run.chunk, run.text.clone());
        let background = if seg.selected {
            // The selection wins over the cell's background colour, otherwise
            // it would vanish on a coloured line.
            Some(selection_bg)
        } else {
            match seg.bg {
                // The default background is the window's: painting nothing
                // avoids one rectangle per cell.
                Paint::Default => None,
                // Inverse video on default colours: the background takes the
                // theme's text colour — this is a program's caret or status
                // bar, and it must show.
                Paint::Inverted => Some(default_fg),
                Paint::DimDefault => Some(default_fg.opacity(0.66)),
                Paint::Rgb(r, g, b) => Some(rgb(r, g, b)),
            }
        };
        let styled = TextRun {
            len: text.len(),
            font: gpui::Font {
                family: family.clone(),
                features: Default::default(),
                weight: if seg.bold {
                    gpui::FontWeight::BOLD
                } else {
                    gpui::FontWeight::NORMAL
                },
                style: if seg.italic {
                    gpui::FontStyle::Italic
                } else {
                    gpui::FontStyle::Normal
                },
                fallbacks: None,
            },
            color: match seg.fg {
                Paint::Default => default_fg,
                Paint::Inverted => default_bg,
                // Faint through opacity rather than a mixed colour: the theme
                // background shows through, which is what "dim" reads as.
                Paint::DimDefault => default_fg.opacity(0.66),
                Paint::Rgb(r, g, b) => rgb(r, g, b),
            },
            background_color: None,
            underline: seg.underline.then(gpui::UnderlineStyle::default),
            strikethrough: seg.strikethrough.then(gpui::StrikethroughStyle::default),
        };
        out.push(
            div()
                .absolute()
                .top_0()
                .left(cell.width * chunk.col as f32)
                .w(cell.width * chunk.cells as f32)
                .h(cell.height)
                // A fallback glyph wider than its cell is clipped rather
                // than allowed to cover its neighbour.
                .overflow_hidden()
                .whitespace_nowrap()
                .when_some(background, |el, bg| el.bg(bg))
                .child(StyledText::new(text).with_runs(vec![styled])),
        );
    }
    out
}

/// Translates a pixel offset from the render area's corner into cell
/// coordinates.
///
/// A free function rather than a method: it is arithmetic whose half-cell error
/// is invisible to the eye but makes selection unpleasant, and which can be
/// tested without a window.
fn viewport_position(offset: Point<Pixels>, cell: gpui::Size<Pixels>) -> ViewportPosition {
    let width = f32::from(cell.width).max(1.0);
    let height = f32::from(cell.height).max(1.0);
    let column_f = f32::from(offset.x.max(px(0.))) / width;
    let line_f = f32::from(offset.y.max(px(0.))) / height;
    let column = column_f as usize;
    ViewportPosition {
        line: line_f as usize,
        column,
        // A cell's right half names the next boundary: that is what makes it
        // possible to select "abc" starting from the middle of the `a` without
        // including it, as in a text editor.
        side: if column_f - column as f32 > 0.5 {
            Side::Right
        } else {
            Side::Left
        },
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Hsla {
    gpui::Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

/// The distribution to open the terminals in, on Windows.
///
/// `None` everywhere else: the pty opens where it has always opened. On Windows,
/// on the other hand, the repositories live in WSL — a terminal opened locally
/// would look at a path that does not exist, and the agent launched in it would
/// not see the code. The pty stays local (ConPTY): only what runs inside it
/// crosses.
#[derive(Clone)]
pub struct WslShell {
    distro: String,
    login_shell: String,
}

impl WslShell {
    /// The distribution in service, if there is one.
    ///
    /// The login shell comes from the distribution itself, recorded at the
    /// moment the server was installed; failing that `/bin/sh`, which exists
    /// everywhere.
    pub fn current(cx: &gpui::App) -> Option<Self> {
        if !cfg!(windows) {
            return None;
        }
        let distro = Settings::global(cx).wsl_distro.trim().to_string();
        (!distro.is_empty()).then(|| Self {
            distro,
            login_shell: crate::ui::settings::server_shell()
                .unwrap_or_else(|| "/bin/sh".to_string()),
        })
    }

    /// What the local pty has to launch for the work to happen over there.
    ///
    /// The **Windows** pty's working directory becomes some valid but arbitrary
    /// folder: the real directory, the repository's, goes into `wsl.exe`'s
    /// `--cd`. Passing it the Linux path would make the opening fail before it
    /// even started.
    fn wrap(&self, worktree: &Path, command: Program, env: &HashMap<String, String>) -> Spawned {
        // Sorted: a hash map's order changes from one run to the next, and a
        // command line that moves for no reason is impossible to compare when it
        // has to be read in a trace.
        let mut vars: Vec<(String, String)> =
            env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        vars.sort();
        let (program, args) = crate::wsl::terminal_argv(
            &self.distro,
            &worktree.to_string_lossy(),
            &self.login_shell,
            command,
            &vars,
        );
        Spawned {
            cwd: std::env::temp_dir(),
            command: Some((program, args)),
            env: HashMap::new(),
        }
    }
}

/// A program and its arguments, as the pty will receive them. `None`: the login
/// shell.
type Program = Option<(String, Vec<String>)>;

/// What a tab really launches, once the platform is taken into account.
struct Spawned {
    cwd: PathBuf,
    command: Program,
    env: HashMap<String, String>,
}

/// What is needed to open a tab.
///
/// An aggregate rather than four parameters: an agent profile carries a command,
/// arguments, an environment and a name, and making them travel separately down
/// to the pty multiplied the chances of forgetting one.
pub struct Launch {
    /// `None` = the login shell, which is what somebody opening "a terminal"
    /// expects.
    pub command: Option<(String, Vec<String>)>,
    /// Variables added to the pty's environment. This is where an agent
    /// profile's model goes through.
    pub env: HashMap<String, String>,
    pub label: SharedString,
    /// True when this tab runs an agent: it is the one review notes will be
    /// delivered to.
    pub agent: bool,
}

impl Launch {
    pub fn shell() -> Self {
        Self {
            command: None,
            env: HashMap::new(),
            label: tr!("terminal-shell"),
            agent: false,
        }
    }

    pub fn agent(profile: &crate::ui::settings::AgentProfile) -> Self {
        Self {
            command: Some(profile.spawn()),
            env: profile
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            label: SharedString::from(profile.label().to_string()),
            agent: true,
        }
    }
}

/// One open terminal: its pty, and the panels that show it.
///
/// **One dock panel per terminal**, and no longer one panel holding a strip of
/// tabs of its own. The dock's tab bar *is* that strip now, which is what lets
/// a terminal be dragged into a split, on to another zone, or zoomed like any
/// other view — none of which a strip drawn by hand could offer.
///
/// The price is one panel **per screen**: a panel belongs to a single dock area
/// at a time and there are five. All five render this same `view`, so there is
/// still exactly one pty; and since only one dock is displayed at a time, no
/// two of them ever draw the same grid in the same frame.
pub struct OpenTerminal {
    pub worktree: PathBuf,
    pub view: Entity<TerminalView>,
    /// The name given by hand, if it was. Failing that the tab carries the
    /// running program, which is right until one opens three shells: they are
    /// then three tabs called `bash`, and only what one is doing in each tells
    /// them apart — which is precisely what the program name cannot say.
    pub name: Option<SharedString>,
    /// The panels, in the order of `Workspace::ALL`.
    pub panels: Vec<Entity<crate::ui::panels::TerminalPanel>>,
}

impl OpenTerminal {}

impl ClaudhubApp {
    /// The terminals of the worktree being looked at, in the order they opened.
    /// An iterator and not a list: most of its callers only ask whether there
    /// is one.
    pub(super) fn terminals_of<'a>(
        &'a self,
        worktree: &'a Path,
    ) -> impl DoubleEndedIterator<Item = &'a OpenTerminal> + 'a {
        self.terminals
            .iter()
            .filter(move |terminal| terminal.worktree == worktree)
    }

    /// Opens a terminal on a worktree and shows it.
    ///
    /// It joins the tab group of the worktree's other terminals when there is
    /// one — that is what makes the dock's bar read as *the* terminal bar — and
    /// otherwise opens the slot under the centre, which is where the default
    /// layout used to put the one permanent panel.
    pub(super) fn open_terminal(
        &mut self,
        worktree: &Path,
        mut launch: Launch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // What the agent needs to know about Claudhub, and no API will tell it:
        // where it works, where the review notes sent to it are, and which file
        // carries its task list. Environment variables rather than a protocol —
        // a shell sees them too, and an agent launched in a terminal alongside
        // only has to copy them.
        launch
            .env
            .insert("CLAUDHUB_WORKTREE".into(), worktree.display().to_string());
        // Read now and not at construction: the notes root is a setting, and a
        // terminal opened after changing it has to receive the right folder.
        if let Some(vault) = self.notes_dir(worktree, cx) {
            launch
                .env
                .insert("CLAUDHUB_NOTES_DIR".into(), vault.display().to_string());
            launch.env.insert(
                "CLAUDHUB_TODO".into(),
                crate::wslpath::join(&vault, crate::ui::vault::TODO)
                    .display()
                    .to_string(),
            );
        }
        // A pty we cannot open is a system problem: descriptor limit reached,
        // `/dev/pts` missing. We give the terminal up and say so, rather than
        // panic in the middle of a render — which is what this code used to do,
        // with a frozen window as its only symptom.
        //
        // The settings are re-read at opening rather than recorded once:
        // changing the shell or the scrollback has to hold for the next
        // terminal, without having to close the others.
        let settings = Settings::global(cx).terminal.clone();
        let wsl = WslShell::current(cx);
        let terminal = match TerminalView::open(worktree, &launch, &settings, wsl.as_ref()) {
            Ok(terminal) => terminal,
            Err(e) => {
                log::error!("opening the terminal: {e:#}");
                self.announce_error(SharedString::from(e.to_string()), cx);
                cx.notify();
                return;
            }
        };
        // Whether the tab runs a command is read from the launch and not from
        // the pty: `open` falls back on the settings' program when nothing was
        // asked for, and that fallback is the shell.
        let runs_command = launch.command.is_some();
        let view = cx.new(|cx| {
            TerminalView::attach(
                terminal,
                launch.label,
                launch.agent,
                runs_command,
                window,
                cx,
            )
        });
        // **A terminal opened on purpose is a terminal one wants to see.** The
        // panel may have been hidden — from the corner button, or from a
        // view's `…` menu —, and a pty installed behind a hidden panel is a
        // process nobody can answer: the `+` of the status bar looked broken,
        // the project task ran out of sight, the prompt handed to an agent
        // landed nowhere. Only for the worktree being looked at, since it is
        // the only one whose terminals this flag shows; and `set_panel_visible`
        // rather than `show_terminal_panel`, which opens one when there is none
        // — from here that is the recursion of opening the terminal we are
        // opening. It comes **before** `install_terminal`, which hands the
        // fresh panel the visibility it reads now.
        if self.active.as_deref() == Some(worktree) {
            self.set_panel_visible(crate::ui::panels::TerminalPanel::NAME, true, cx);
        }
        self.install_terminal(worktree.to_path_buf(), view, window, cx);
    }

    /// Puts a terminal's faces into the docks that carry one, and shows it.
    ///
    /// Split out from `open_terminal` because the layout registry takes the
    /// same path: a terminal read back from `layout.json` is a fresh pty that
    /// has to be adopted exactly like one just opened.
    pub(super) fn install_terminal(
        &mut self,
        worktree: PathBuf,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app = cx.entity();
        // Computed here and handed over: the panels are built inside this
        // `update`, so they cannot read the application to find out for
        // themselves — the entity is out of the table while this runs.
        let visible = self.terminal_shown(&worktree, cx);
        let mut panels = Vec::new();
        for workspace in crate::ui::workspace::Workspace::ALL {
            let (app, worktree, view) = (app.clone(), worktree.clone(), view.clone());
            // The multiplexer's face is the same panel, told two things the
            // others are not: show yourself whatever worktree is being looked
            // at, and say in your tab which project you belong to.
            panels.push(cx.new(|cx| {
                if workspace.shows_every_worktree() {
                    crate::ui::panels::TerminalPanel::in_grid(&app, worktree, view, cx)
                } else {
                    crate::ui::panels::TerminalPanel::new(&app, worktree, view, visible, cx)
                }
            }));
        }
        // A shell that exits closes its tab. `subscribe_in` and not
        // `subscribe`: removing the panels and handing the focus on both need a
        // window, which the event does not carry. Held here and not in the view
        // — closing a terminal is the application's gesture, and the same one
        // the cross goes through, panels of the five screens included.
        cx.subscribe_in(
            &view,
            window,
            |this, view, _: &TerminalExited, window, cx| {
                this.close_terminal(view.entity_id(), window, cx);
            },
        )
        .detach();
        self.terminals.push(OpenTerminal {
            worktree: worktree.clone(),
            view: view.clone(),
            name: None,
            panels: panels.clone(),
        });
        for (workspace, panel) in crate::ui::workspace::Workspace::ALL.into_iter().zip(panels) {
            self.dock_terminal(workspace, &worktree, panel, window, cx);
        }
        let handle = view.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Places one terminal panel in one screen's dock.
    fn dock_terminal(
        &mut self,
        workspace: crate::ui::workspace::Workspace,
        worktree: &Path,
        panel: Entity<crate::ui::panels::TerminalPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dock) = self.docks.get(&workspace).cloned() else {
            return;
        };
        let side = crate::ui::settings::Settings::global(cx).terminal.placement;
        // The node of a terminal already open on this worktree, in this dock:
        // that is the tab group the new one joins.
        let sibling = self
            .terminals
            .iter()
            .filter(|terminal| terminal.worktree == worktree)
            .filter_map(|terminal| terminal.panels.get(workspace.index()).cloned())
            .rfind(|other| other.entity_id() != panel.entity_id());
        dock.update(cx, |dock, cx| {
            // **`panel_handle` and `dock_panel_at`, never `add_panel`.** An
            // `Entity<P>` converts itself into base's `PanelView` and the dock
            // takes it without complaint — but without the presentation that
            // goes with it: no tab, no title, no content. It is the silent
            // failure of the dock rework, already written down in
            // `workspace.rs`, and it is what made a terminal open into nothing.
            crate::ui::panels::dock_panel_at(
                dock,
                gpui_component::dock::panel_handle(panel.clone()),
                |dock| {
                    sibling
                        .and_then(|sibling| {
                            let node = dock
                                .layout(DockPlacement::Center)?
                                .find_panel_node(PanelId::from(sibling.entity_id()))?;
                            Some(InsertTarget::Tabs {
                                node,
                                ix: None,
                                // The tab one has just opened is the tab one
                                // looks at.
                                activate: true,
                            })
                        })
                        .or_else(|| {
                            // The multiplexer holds nothing **but** terminals:
                            // the first one takes the whole centre, which is
                            // where the add has already put it. Splitting the
                            // empty root would pin it into a band at the
                            // bottom with nothing above.
                            if workspace.shows_every_worktree() {
                                return None;
                            }
                            // No terminal here yet: the slot under the
                            // screen's **content**, which on every screen but
                            // the review is the whole centre — and on the
                            // review is its right-hand half.
                            //
                            // The review's list column lives in the centre
                            // rather than in a dock of its own, so a terminal
                            // opened under the whole row raised the file list
                            // along with the diff, for nothing: one reads a
                            // list beside a terminal, never above one. Taking
                            // the last slot of a row says that in the one rule
                            // that also gives the right answer where the
                            // centre holds a single column.
                            let tree = dock.layout(DockPlacement::Center)?;
                            // Beside the content, the whole row is the right
                            // neighbour: a terminal opened to the right of the
                            // diff alone would leave the review's list column
                            // and the terminal sharing a height for no reason.
                            // Under it, the last slot of the row — see above.
                            let (node, placement, size) = match side {
                                crate::ui::settings::TerminalPlacement::Right => (
                                    tree.root().id(),
                                    gpui_base::Placement::Right,
                                    TERMINAL_WIDTH,
                                ),
                                crate::ui::settings::TerminalPlacement::Bottom => {
                                    let node = match tree.root().kind() {
                                        gpui_component::dock::PaneRef::Split {
                                            axis: gpui::Axis::Horizontal,
                                            children,
                                            ..
                                        } => children.last().map(|child| child.id()),
                                        _ => None,
                                    }
                                    .unwrap_or_else(|| tree.root().id());
                                    (node, gpui_base::Placement::Bottom, TERMINAL_HEIGHT)
                                }
                            };
                            Some(InsertTarget::Split {
                                node,
                                placement,
                                size: Some(size),
                            })
                        })
                },
                window,
                cx,
            );
        });
    }

    /// What a terminal's tab says: the name given by hand, or the program.
    pub(super) fn terminal_label(&self, view: gpui::EntityId, cx: &App) -> SharedString {
        let Some(terminal) = self
            .terminals
            .iter()
            .find(|terminal| terminal.view.entity_id() == view)
        else {
            return SharedString::default();
        };
        terminal
            .name
            .clone()
            .unwrap_or_else(|| terminal.view.read(cx).label())
    }

    /// Renames a terminal, or gives it its program's name back.
    ///
    /// An empty name **clears** it rather than showing an empty tab: it is the
    /// convention of the task list two panels over, and it saves a second
    /// gesture for "actually, put it back".
    pub(super) fn rename_terminal(
        &mut self,
        view: gpui::EntityId,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self
            .terminals
            .iter_mut()
            .find(|terminal| terminal.view.entity_id() == view)
        else {
            return;
        };
        let name = name.trim();
        terminal.name = (!name.is_empty()).then(|| SharedString::from(name.to_string()));
        cx.notify();
    }

    /// Asks for a terminal's new name.
    pub(super) fn ask_terminal_name(
        &mut self,
        view: gpui::EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.terminal_label(view, cx);
        self.open_text_dialog_with(
            tr!("terminal-rename"),
            tr!("terminal-rename-placeholder"),
            current,
            window,
            cx,
            move |this, name, _window, cx| this.rename_terminal(view, name, cx),
        );
    }

    /// The two gestures that close a terminal: the cross, and `Ctrl+W`.
    ///
    /// **A command running in there is asked about**, and it is the one place
    /// this window asks about something git cannot undo: closing sends SIGHUP,
    /// and what dies with it is a build half done, a migration half applied, an
    /// agent halfway through a task. Everywhere else the same tab closes without
    /// a word — a shell at its prompt has nothing to lose.
    ///
    /// The dock's own removal does **not** come through here: by the time
    /// `on_removed` fires the tab is already gone, and a cancelled dialogue
    /// would leave a pty alive with nothing to show it.
    pub(super) fn ask_close_terminal(
        &mut self,
        view: gpui::EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let busy = self
            .terminals
            .iter()
            .find(|terminal| terminal.view.entity_id() == view)
            .map(|terminal| terminal.view.clone())
            .filter(|terminal| terminal.read(cx).busy());
        let Some(terminal) = busy else {
            self.close_terminal(view, window, cx);
            return;
        };
        let label = terminal.read(cx).label();
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, label) = (entity.clone(), label.clone());
            dialog
                .title(tr!("terminal-close-busy"))
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(label))
                        .child(div().text_xs().child(tr!("terminal-close-busy-help"))),
                )
                .overlay_closable(false)
                .close_button(false)
                .footer(crate::ui::dialogs::confirm())
                .on_ok(move |_, window, cx| {
                    entity.update(cx, |this, cx| this.close_terminal(view, window, cx));
                    true
                })
        });
    }

    /// Closes the window — or asks first, when a terminal is in the middle of
    /// something. Returns whether the window may go **now**.
    ///
    /// Three gestures close the window, and gpui intercepts only the first:
    /// the window manager's — `Alt+F4`, its own cross, `WM_CLOSE` under
    /// Windows — comes through `on_window_should_close`; our title bar's cross
    /// calls `remove_window` outright unless it is given `on_close_window`;
    /// and the menu's « Quit » is a `cx.quit()` that asks nobody. All three
    /// come here, so that the one question is asked the same way whatever the
    /// hand did.
    ///
    /// The question is `ask_close_terminal`'s, and for the same reason: the
    /// window going away takes every pty with it, the same SIGHUP multiplied
    /// by the terminals of every worktree. Only what is busy is listed — a
    /// shell at its prompt has nothing to lose, and naming it would bury the
    /// build that has.
    pub(super) fn quit_or_ask(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let busy: Vec<SharedString> = self
            .terminals
            .iter()
            .map(|terminal| terminal.view.read(cx))
            .filter(|terminal| terminal.busy())
            .map(|terminal| terminal.label())
            .collect();
        if busy.is_empty() {
            return true;
        }
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let busy = busy.clone();
            dialog
                .title(tr!("window-close-busy"))
                .child(
                    v_flex()
                        .gap_1()
                        .children(busy.into_iter().map(|label| div().text_sm().child(label)))
                        .child(div().text_xs().child(tr!("window-close-busy-help"))),
                )
                .overlay_closable(false)
                .close_button(false)
                .footer(crate::ui::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    cx.quit();
                    true
                })
        });
        false
    }

    /// Closes a terminal: its pty, and its panel in each of the five docks.
    ///
    /// Called by whichever panel the user closed — one screen's — and it takes
    /// the other four with it: they are five faces of one pty, and leaving four
    /// of them pointing at a dead shell would be four tabs that do nothing.
    pub(super) fn close_terminal(
        &mut self,
        view: gpui::EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self
            .terminals
            .iter()
            .position(|terminal| terminal.view.entity_id() == view)
        else {
            return;
        };
        let terminal = self.terminals.remove(ix);
        let worktree = terminal.worktree.clone();
        let had_focus = terminal.view.read(cx).focus_handle(cx).is_focused(window);
        for (workspace, panel) in crate::ui::workspace::Workspace::ALL
            .into_iter()
            .zip(terminal.panels)
        {
            let Some(dock) = self.docks.get(&workspace).cloned() else {
                continue;
            };
            dock.update(cx, |dock, cx| dock.remove_panel(panel, window, cx));
        }
        // The focus was on what has just left the tree, and a focus handle
        // nobody renders any more resolves no binding: every shortcut would
        // stay dead until a click put the focus back on a live node. It goes to
        // the neighbouring terminal, which is what one is left looking at, and
        // to the root when there is none.
        if had_focus {
            if self.terminals_of(&worktree).next().is_none() {
                let root = self.focus.clone();
                window.focus(&root, cx);
            } else {
                self.focus_terminal(&worktree, window, cx);
            }
        }
        cx.notify();
    }

    /// Drops every terminal of a worktree — the worktree is gone.
    pub(super) fn close_terminals_of(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let doomed: Vec<gpui::EntityId> = self
            .terminals
            .iter()
            .filter(|terminal| terminal.worktree == worktree)
            .map(|terminal| terminal.view.entity_id())
            .collect();
        for view in doomed {
            self.close_terminal(view, window, cx);
        }
    }

    /// Closes the terminal that has focus, or the worktree's last one.
    ///
    /// `Ctrl+W` names no tab: what one means is the terminal being typed in,
    /// and failing that the one on screen — which, the tabs being in the order
    /// they opened, is the last.
    pub(super) fn close_focused_terminal(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let doomed = self
            .terminals
            .iter()
            .filter(|terminal| terminal.worktree == worktree)
            .find(|terminal| terminal.view.read(cx).focus_handle(cx).is_focused(window))
            .or_else(|| self.terminals_of(worktree).next_back())
            .map(|terminal| terminal.view.entity_id());
        if let Some(view) = doomed {
            self.ask_close_terminal(view, window, cx);
        }
    }

    /// Goes from one of the worktree's terminals to the next, and shows it.
    ///
    /// The dock has its own tab navigation, but it works on the group that has
    /// focus and there are several on screen; this one names the terminals, and
    /// only them, which is what the shortcut says.
    pub(super) fn step_terminal(
        &mut self,
        worktree: &Path,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let terminals: Vec<&OpenTerminal> = self.terminals_of(worktree).collect();
        if terminals.is_empty() {
            return;
        }
        let current = terminals
            .iter()
            .position(|terminal| terminal.view.read(cx).focus_handle(cx).is_focused(window))
            .unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(terminals.len() as isize) as usize;
        let (view, panel) = {
            let terminal = terminals[next];
            (
                terminal.view.clone(),
                terminal.panels.get(self.workspace.index()).cloned(),
            )
        };
        if let Some(panel) = panel {
            crate::ui::panels::TerminalPanel::activate(&panel, window, cx);
        }
        let handle = view.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Puts the focus on the worktree's last terminal, bringing its tab up.
    ///
    /// The last and not the first: it is the one that was opened most recently,
    /// which is the one being looked at. Showing a terminal is not focusing it
    /// — the dock only decides which tab is on screen — and a terminal one has
    /// to click before typing in it is a terminal the shortcut did not finish
    /// opening.
    pub(super) fn focus_terminal(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((view, panel)) = self.terminals_of(worktree).last().map(|terminal| {
            (
                terminal.view.clone(),
                terminal.panels.get(self.workspace.index()).cloned(),
            )
        }) else {
            return;
        };
        if let Some(panel) = panel {
            crate::ui::panels::TerminalPanel::activate(&panel, window, cx);
        }
        let handle = view.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// The worktree's terminals, opening one if it has none.
    ///
    /// Opening on demand and not when the worktree opens: a shell nobody asked
    /// for is a process nobody asked for.
    pub(super) fn ensure_terminal(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.terminals_of(worktree).next().is_none() {
            self.open_terminal(worktree, Launch::shell(), window, cx);
        }
    }

    /// Opens a terminal running the configured coding agent.
    pub(super) fn open_agent_terminal(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = Settings::global(cx).terminal.default_profile().cloned() else {
            return;
        };
        self.open_terminal(worktree, Launch::agent(&profile), window, cx);
    }

    /// True when one of this worktree's terminals has focus. That is what names
    /// the area the zoom shortcuts aim at.
    pub(super) fn terminal_focused(&self, window: &Window, cx: &App) -> bool {
        self.terminals
            .iter()
            .any(|terminal| terminal.view.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Hands a text to the worktree's agent, opening one if none is running.
    pub(super) fn send_to_agent(
        &mut self,
        worktree: &Path,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.agent_terminal(worktree, cx) {
            // Its tab may be behind another's: focusing a panel does not make
            // it the one on screen, and a message delivered into a hidden tab
            // is a message nobody sees arrive.
            self.reveal_terminal(&view, window, cx);
            let handle = view.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
            self.deliver_to_terminal(view, text, cx);
            return;
        }
        self.open_agent_terminal(worktree, window, cx);
        let Some(view) = self.agent_terminal(worktree, cx) else {
            return;
        };
        // Nothing in a pty says "I am ready", and what arrives before the prompt
        // is read by the shell we have not replaced yet.
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(AGENT_WARMUP).await;
            view.update(cx, |view, cx| view.paste_text(&text, cx));
            cx.background_executor().timer(SUBMIT_DELAY).await;
            view.update(cx, |view, cx| view.submit(cx));
        })
        .detach();
    }

    /// Brings a terminal's tab to the front, on the screen being looked at.
    fn reveal_terminal(
        &mut self,
        view: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = self
            .terminals
            .iter()
            .find(|terminal| terminal.view.entity_id() == view.entity_id())
            .and_then(|terminal| terminal.panels.get(self.workspace.index()).cloned());
        if let Some(panel) = panel {
            crate::ui::panels::TerminalPanel::activate(&panel, window, cx);
        }
    }

    /// Goes to work in a project picked from the multiplexer.
    ///
    /// Three things in one gesture, and none can be dropped: the worktree
    /// becomes the one being looked at — every dock only ever shows its own —,
    /// the terminals are made visible again in case they had been hidden, and
    /// the screen goes back to the last one **worked** in. Staying on the grid
    /// would answer "go here" with the same page one was already reading.
    pub(super) fn work_in_worktree(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let back = self.worked_in;
        self.select_worktree(worktree.to_path_buf(), window, cx);
        self.set_panel_visible(crate::ui::panels::TerminalPanel::NAME, true, cx);
        self.enter_workspace(back, window, cx);
        cx.notify();
    }

    /// The most recent agent terminal still running on this worktree.
    ///
    /// The most recent: it is the one being looked at, and relaunching an agent
    /// after quitting one is the normal gesture when the conversation has got
    /// bogged down.
    fn agent_terminal(&self, worktree: &Path, cx: &App) -> Option<Entity<TerminalView>> {
        self.terminals
            .iter()
            .filter(|terminal| terminal.worktree == worktree)
            .rev()
            .find(|terminal| {
                let view = terminal.view.read(cx);
                view.is_agent() && !view.has_exited()
            })
            .map(|terminal| terminal.view.clone())
    }

    /// Pastes, then confirms in a **second** send.
    ///
    /// The two are separated by a short silence: a TUI that has just received a
    /// bracketed paste may swallow a carriage return arriving right behind it,
    /// and the message would stay in the prompt without going out.
    fn deliver_to_terminal(
        &mut self,
        view: Entity<TerminalView>,
        text: String,
        cx: &mut Context<Self>,
    ) {
        view.update(cx, |view, cx| view.paste_text(&text, cx));
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(SUBMIT_DELAY).await;
            view.update(cx, |view, cx| view.submit(cx));
        })
        .detach();
        cx.notify();
    }
}

/// The height the first terminal of a screen opens at.
const TERMINAL_HEIGHT: Pixels = px(260.);

/// And the width, when it opens beside the content rather than under it.
const TERMINAL_WIDTH: Pixels = px(520.);

#[cfg(test)]
mod tests {

    /// A trackpad sends fractions of a line: losing them one by one makes
    /// scrolling inert.
    #[test]
    fn fractions_of_a_line_add_up_instead_of_vanishing() {
        let mut remainder = 0.;
        assert_eq!(take_lines(&mut remainder, 6., 16.), 0);
        assert_eq!(take_lines(&mut remainder, 6., 16.), 0);
        assert_eq!(take_lines(&mut remainder, 6., 16.), 1);
        // The overflow is kept for later rather than thrown away.
        assert!(remainder > 0.1 && remainder < 0.2);

        // Downwards, the same thing in negative.
        let mut remainder = 0.;
        assert_eq!(take_lines(&mut remainder, -32., 16.), -2);
        assert_eq!(remainder, 0.);

        // An absurd cell height does not divide by zero.
        let mut remainder = 0.;
        assert_eq!(take_lines(&mut remainder, 5., 0.), 5);
    }

    /// The floor is not cosmetic: below twenty columns, a shell prompt takes
    /// dozens of lines, the program redraws, and the resize drag leaves nothing
    /// but stacked fragments.
    #[test]
    fn a_squeezed_panel_still_gets_a_usable_grid() {
        let cell = gpui::size(px(8.), px(16.));
        assert_eq!(grid_size(gpui::size(px(800.), px(320.)), cell), (100, 20));
        // Shrunk to nothing: we clip rather than ask for two columns.
        assert_eq!(grid_size(gpui::size(px(10.), px(4.)), cell), (20, 3));
        // A zero-width cell does not divide by zero.
        assert_eq!(
            grid_size(gpui::size(px(800.), px(320.)), gpui::size(px(0.), px(0.))),
            (800, 320)
        );
    }
    use super::*;

    fn cell() -> gpui::Size<Pixels> {
        gpui::size(px(8.), px(16.))
    }

    fn run(text: &str, col: usize, cells: usize) -> (crate::terminal::Line, Segment) {
        let line = crate::terminal::Line {
            text: text.to_string(),
            segments: Vec::new(),
        };
        let seg = Segment {
            start: 0,
            end: line.text.len(),
            col,
            cells,
            fg: Paint::Default,
            bg: Paint::Default,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            hidden: false,
            inverse: false,
            selected: false,
        };
        (line, seg)
    }

    /// The reported flicker: an agent's spinner cycles through dingbats the
    /// font does not carry, and each frame shifted the whole status line.
    #[test]
    fn a_character_off_the_grid_is_cut_out_of_its_run() {
        let (line, seg) = run("\u{2733} Fiddling", 4, 10);
        let on_grid = HashMap::from([('\u{2733}', false)]);
        let cut: Vec<(usize, usize, &str)> = chunks(&line, &seg, &on_grid)
            .iter()
            .map(|c| (c.col, c.cells, &line.text[c.start..c.end]))
            .collect();
        assert_eq!(
            cut,
            [(4, 1, "\u{2733}"), (5, 9, " Fiddling")],
            "the dingbat gets a cell of its own, and what follows starts where \
             the grid says"
        );
    }

    #[test]
    fn a_run_the_font_carries_is_shaped_in_one_go() {
        let (line, seg) = run("hello", 7, 5);
        let cut = chunks(&line, &seg, &HashMap::new());
        assert_eq!(cut.len(), 1);
        assert_eq!((cut[0].col, cut[0].cells), (7, 5));
    }

    /// Combining marks break the character-for-column count: the run is then
    /// pinned as one piece rather than cut where it cannot be.
    #[test]
    fn a_run_carrying_combining_marks_stays_whole() {
        let (line, seg) = run("e\u{301}x", 0, 2);
        let on_grid = HashMap::from([('\u{301}', false)]);
        let cut = chunks(&line, &seg, &on_grid);
        assert_eq!(cut.len(), 1);
        assert_eq!((cut[0].col, cut[0].cells), (0, 2));
    }

    #[test]
    fn maps_pixels_to_the_cell_under_them() {
        let p = viewport_position(gpui::point(px(0.), px(0.)), cell());
        assert_eq!((p.line, p.column), (0, 0));

        // Column 3, line 2: 3×8 and 2×16, plus a shade.
        let p = viewport_position(gpui::point(px(25.), px(33.)), cell());
        assert_eq!((p.line, p.column), (2, 3));
    }

    #[test]
    fn the_half_of_the_cell_decides_the_side() {
        // First third of cell 2: we aim at its left boundary.
        let p = viewport_position(gpui::point(px(18.), px(0.)), cell());
        assert_eq!(p.column, 2);
        assert_eq!(p.side, Side::Left);

        // Last third of the same cell: right boundary.
        let p = viewport_position(gpui::point(px(22.), px(0.)), cell());
        assert_eq!(p.column, 2);
        assert_eq!(p.side, Side::Right);
    }

    #[test]
    fn a_pointer_above_or_left_of_the_view_clamps_to_the_origin() {
        // A drag leaving the area must not produce a negative index: the
        // conversion to `usize` would overflow.
        let p = viewport_position(gpui::point(px(-40.), px(-90.)), cell());
        assert_eq!((p.line, p.column), (0, 0));
    }
}
