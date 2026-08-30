//! Modal editing for the built-in editor.
//!
//! The keys of vim, on the file one is editing: the modes, the motions, the
//! operators and the registers. Nothing here knows gpui — it takes the text and
//! the caret, and it renders the edit to apply — which is what makes it
//! testable, as `motion.rs` and `notes.rs` are in front of their views.
//!
//! It is a **subset**, and the subset is the point: what a hand types without
//! thinking during a review. What is not here is what an editor's own machinery
//! already does better (multiple registers, macros, marks, text objects) or what
//! Claudhub has elsewhere (`Ctrl+F`'s search bar).

use std::ops::Range;

/// The mode the editor is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    /// Characterwise visual.
    Visual,
    /// Linewise visual — `V`.
    VisualLine,
    /// Blockwise visual — `Ctrl+V`: a rectangle, and not a run of text.
    VisualBlock,
}

impl Mode {
    /// The i18n key of the name the toolbar shows.
    pub fn key(self) -> &'static str {
        match self {
            Mode::Normal => "vim-mode-normal",
            Mode::Insert => "vim-mode-insert",
            Mode::Visual => "vim-mode-visual",
            Mode::VisualLine => "vim-mode-visual-line",
            Mode::VisualBlock => "vim-mode-visual-block",
        }
    }
}

/// One keystroke, reduced to what vim cares about.
///
/// `ch` is the character the keystroke **produced** and not the key it was
/// pressed on: that is what makes `$`, `^` and `0` land where they should on an
/// AZERTY keyboard, where they are shifted or in the numeric row.
#[derive(Clone)]
pub struct Key {
    pub ch: Option<char>,
    /// The name of a key that produces no character: `escape`, `enter`,
    /// `backspace`.
    pub name: String,
    pub ctrl: bool,
}

impl Key {
    fn is(&self, name: &str) -> bool {
        self.name == name
    }
}

/// A replacement of one range of the text by another.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
}

/// What the view applies after one keystroke.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Change {
    pub edit: Option<Edit>,
    /// What to select afterwards, in the text **the edit leaves behind**.
    ///
    /// In normal mode it is the character under the caret — the block cursor —
    /// and in insert mode it is empty.
    pub selection: Range<usize>,
    /// Where vim considers the caret to be, which is the start of the selection
    /// except when the visual head is its end.
    pub head: usize,
    /// The ranges a **copy** left in place, for the view to flash.
    ///
    /// A yank is the one gesture of vim that changes nothing on screen: without
    /// a sign, one is never sure it took. It is what vim-highlightedyank exists
    /// for, and it is filled for a yank only — a delete has taken the text away,
    /// and there is nothing left to light up. Several ranges rather than one
    /// because a blockwise yank takes a rectangle, and lighting the whole span
    /// would say it took the lines and not the columns.
    pub flash: Vec<Range<usize>>,
    /// What this keystroke tore out or copied, when it did.
    ///
    /// The view is what puts it on the system clipboard, the setting being on:
    /// `ui::vim` has no clipboard, and having one would be a gpui type in a
    /// module whose whole point is not to have any.
    pub yank: Option<Register>,
}

/// What vim asks the application for, having no way to do it itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Undo,
    Redo,
    Save,
    Close,
    SaveAndClose,
    /// `gd`. It is a command and not a motion: where it lands is not something
    /// the text can be read for — a language server has to be asked, and it may
    /// answer with another file.
    GoToDefinition,
    /// `zz`, `zt`, `zb`: where the current line goes in the viewport. The text
    /// does not move, so there is nothing to apply — only a view to scroll.
    Reveal(Reveal),
    /// `Ctrl+E` and `Ctrl+Y`: that many lines of view, down when positive.
    Scroll(isize),
    /// The `z` commands that fold. What folds is the grammar's business, not
    /// ours: these say open, close or toggle, and the editor knows where.
    Fold(Fold),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reveal {
    Centre,
    Top,
    Bottom,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fold {
    /// `zc`, `zo`, `za`: the fold the cursor is in.
    Close,
    Open,
    Toggle,
    /// `zM`, `zR`: every fold in the file.
    CloseAll,
    OpenAll,
    /// `zm`, `zr`: one level of nesting more, or less.
    More,
    Less,
}

/// The answer to one keystroke.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Response {
    /// Not ours: insert mode, or a key vim does not claim. The editor gets it.
    Ignored,
    /// Taken, and nothing to apply — a count being typed, an operator waiting
    /// for its motion, a mode indicator to repaint.
    Consumed,
    Apply(Change),
    Command(Command),
}

/// What a yank or a delete left behind.
///
/// Public because it is what crosses over to the system clipboard when the
/// setting asks for it: the clipboard holds text and nothing else, so the
/// linewise flag has to travel beside it.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Register {
    pub text: String,
    /// Whole lines, which is what decides where `p` puts them back.
    pub linewise: bool,
}

/// A `/` or `:` line being typed.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Prompt {
    kind: char,
    text: String,
}

/// The modal state of one open file.
#[derive(Default)]
pub struct Vim {
    mode: Mode,
    /// The keys of a command not yet complete: a count, an operator, the `g` of
    /// `gg`, the `f` waiting for its character.
    pending: String,
    /// The other end of a visual selection.
    anchor: usize,
    /// Where vim believes the caret is. It is authoritative in visual mode only:
    /// elsewhere a click of the mouse moves the caret without telling us.
    head: usize,
    /// The column `j` and `k` try to keep. `usize::MAX` is `$`'s column, which
    /// sticks to the end of every line it crosses.
    column: Option<usize>,
    register: Register,
    /// What the keystroke under way has taken, waiting for its `Change`.
    yanked: Option<Register>,
    /// The ranges it copied without touching them, waiting for the same.
    flashed: Vec<Range<usize>>,
    prompt: Option<Prompt>,
    last_search: String,
    search_backward: bool,
    /// Whether the occurrences are lit. vim's `hlsearch`, put out by `Esc` in
    /// normal mode — the gesture every configuration binds `:nohlsearch` to.
    hlsearch: bool,
    /// A blockwise insertion under way, waiting for the `Esc` that repeats it
    /// down the other rows.
    block_insert: Option<BlockInsert>,
    /// The folds the editor holds shut, given afresh at every keystroke: the
    /// gutter icons close them too, so this is read and never remembered.
    folds: Vec<crate::ui::folds::Range>,
    /// The last `f`, `F`, `t` or `T`, for `;` and `,` to do again.
    last_find: Option<Find>,
    /// The keys of the change under way, kept until it is known whether it
    /// changes anything: a count, an operator and its motion are three
    /// keystrokes and one command.
    recording: Vec<Key>,
    /// Where an insertion began and how long the text was then.
    ///
    /// The pair `BlockInsert` keeps, for the same reason and read the same way:
    /// together they tell a plain forward insertion from one that has been
    /// clicked away from or backspaced through — the case this gives up on
    /// rather than replay something nobody typed.
    insert_at: Option<(usize, usize)>,
    /// What `.` plays again.
    last_change: Option<Repeat>,
    /// Whether `.` is playing. Nothing is recorded while it does — a repeat is
    /// not a new change, and recording it would make `.` its own last change.
    replaying: bool,
    /// Whether the visual selection came from the mouse rather than from keys.
    ///
    /// What `.` plays is keystrokes, and there are none that describe a drag:
    /// filing `d` alone would give a repeat that waits for a motion and does
    /// nothing. The change before it stays what `.` plays — the same ruling as
    /// an insertion that was clicked away from: never invent the half that was
    /// not typed.
    adopted: bool,
}

/// The last change, as `.` needs it to happen again.
///
/// The keys are the command **up to** insert mode, and the text is what was
/// typed once there: those are two different things, and recording the second
/// as keystrokes would replay the letters without what the editor did with them
/// — an auto-indent, a completion, a bracket closed for you.
#[derive(Clone)]
struct Repeat {
    keys: Vec<Key>,
    /// What was typed in insert mode, when the command went there.
    insert: Option<String>,
}

/// What `I`, `A` and `c` set up in blockwise visual mode: one types on the top
/// row, and `Esc` writes the same thing on all the others.
///
/// That repeat is the reason one reaches for `Ctrl+V` in the first place — a
/// prefix on twenty lines — and it is the only gesture of this module whose two
/// halves are several keystrokes apart.
struct BlockInsert {
    /// The line **indices** the typing is repeated on, the caret's own excluded.
    ///
    /// Indices and not offsets: what is typed on the top row shifts every offset
    /// below it, and a line number does not move as long as no newline is typed
    /// — which is exactly the case this gives up on.
    lines: Vec<usize>,
    /// The column it goes in, `None` being the end of each line — what `$A`
    /// asks for.
    column: Option<usize>,
    /// Whether a line too short to reach the column is padded with blanks, as
    /// `A` does, or skipped, as `I` does.
    pad: bool,
    /// Where the insertion began, and how long the text was then: together they
    /// tell a plain forward insertion from one that has been clicked away from
    /// or backspaced through, which is what this gives up on rather than repeat
    /// something nobody typed.
    start: usize,
    len: usize,
}

impl Vim {
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Starts in insert mode, without a keystroke having asked for it.
    ///
    /// For a **dialog one opens in order to write**: a note, a prompt read back
    /// before it goes. Landing there in normal mode means the first letters of
    /// the remark run as commands, and the field is empty, so there is nothing
    /// for them to run on either. A file is the other way round — one arrives in
    /// it to read — which is why normal stays the mode everything else opens in.
    ///
    /// The half-typed command and the pending prompt go with it; nothing is left
    /// for `.` to replay, the insertion having begun at no keystroke.
    pub fn start_insert(&mut self) {
        self.mode = Mode::Insert;
        self.pending.clear();
        self.prompt = None;
        self.column = None;
        self.block_insert = None;
        self.insert_at = None;
        self.recording.clear();
    }

    /// The end of the selection that is being moved — where the block cursor
    /// goes in a visual mode, which the editor's own caret says nothing about.
    pub fn head(&self) -> usize {
        self.head
    }

    /// Takes on a selection made outside vim: the mouse, `Ctrl+A`, a shifted
    /// arrow the input binds for itself.
    ///
    /// Without it a dragged selection was highlighted and vim knew nothing of
    /// it: the mode stayed normal, the block cursor sat at the **start** of what
    /// had been selected — normal mode reads the caret off the editor, and the
    /// editor's caret is the range's start — and `c`, `d` or `y` then operated
    /// on the one character under it. A selection on screen that the next
    /// operator ignores is the one thing a modal editor must not show.
    ///
    /// It is always **characterwise**: a mouse selects a run of text, and coming
    /// back into `V` or `Ctrl+V` would cover more than what is lit.
    ///
    /// The head is the end the mouse was dragging, which is what tells a
    /// selection grown rightwards from one grown leftwards — `o`, `h` and `l`
    /// carry on from the right end afterwards. Vim's head is **inclusive**,
    /// where the editor's range is not: the last character is the head, not the
    /// offset past it.
    pub fn adopt(&mut self, text: &str, range: Range<usize>, reversed: bool) {
        if self.mode == Mode::Insert {
            return;
        }
        self.pending.clear();
        // A desired column left over from an earlier `$` would make the next
        // `j` reach the end of the line rather than the column being pointed at.
        self.column = None;
        let (start, end) = (range.start.min(text.len()), range.end.min(text.len()));
        if start >= end {
            // A plain click: the caret is the editor's again, which is what
            // normal mode already assumes.
            self.mode = Mode::Normal;
            self.head = start;
            self.anchor = start;
            self.adopted = false;
            return;
        }
        let last = prev_boundary(text, end);
        let (anchor, head) = if reversed {
            (last, start)
        } else {
            (start, last)
        };
        self.anchor = anchor;
        self.head = head;
        self.mode = Mode::Visual;
        // The keys typed before the drag are not part of what follows it.
        self.recording.clear();
        self.adopted = true;
    }

    /// Hands vim the register to paste from.
    ///
    /// It is how the system clipboard gets in when the setting asks for it. The
    /// linewise flag is not in a clipboard, so it is read off the text: whole
    /// lines end with a newline, which is the same convention every editor that
    /// shares a clipboard with vim uses.
    pub fn set_register(&mut self, text: String) {
        self.register = Register {
            linewise: text.ends_with('\n'),
            text,
        };
    }

    /// The pattern whose occurrences are to be lit, `None` when there are none
    /// to light or `Esc` has put them out.
    ///
    /// vim's `hlsearch`, and the half of `/` that was missing: a search that
    /// only moves the caret makes one press `n` to find out whether there was
    /// anything else. The view paints them on a layer of its own rather than
    /// opening the editor's own search panel, which **takes the focus** — and
    /// the focus is what `n` and `N` need to stay where they are.
    pub fn highlights(&self) -> Option<&str> {
        (self.hlsearch && !self.last_search.is_empty()).then_some(self.last_search.as_str())
    }

    /// The pattern `n` and `N` walk, as `Ctrl+F` left it.
    ///
    /// Pushed in before a keystroke the way the register and the folds are: the
    /// editor's own search bar writes to the same place, and one pattern told
    /// twice is two patterns that drift apart. Taking it from there is what
    /// makes `n` carry on a search typed into the bar — and, the other way
    /// round, what makes `Ctrl+F` open on what `/` was looking for.
    pub fn set_search(&mut self, query: &str) {
        if query.is_empty() || query == self.last_search {
            return;
        }
        self.last_search = query.to_string();
        self.search_backward = false;
        self.hlsearch = true;
    }

    /// Hands vim what the editor currently holds folded shut.
    ///
    /// Only `j` and `k` read it, and that is deliberate: they are the motions a
    /// hand runs down a file with, and a fold is precisely what they must step
    /// over. A jump that names a line — `G`, a search — is asking for that line.
    pub fn set_folds(&mut self, folds: Vec<crate::ui::folds::Range>) {
        self.folds = folds;
    }

    /// The `/` or `:` line, as the toolbar shows it while it is being typed.
    pub fn prompt(&self) -> Option<String> {
        self.prompt
            .as_ref()
            .map(|p| format!("{}{}", p.kind, p.text))
    }

    /// The keys typed towards a command that is not complete yet. Vim shows them
    /// in the corner of its status line, and it is the only thing that says why
    /// the next key will not do what it usually does.
    pub fn pending(&self) -> &str {
        &self.pending
    }

    /// Answers one keystroke.
    ///
    /// `rows` is the height of the viewport in lines, which is what `Ctrl+D` and
    /// `Ctrl+U` move by half of.
    ///
    /// It is a wrapper around `answer`, which does the work: what is added here
    /// is the note-taking `.` needs, and `.` itself — the one key that is not a
    /// command of its own but a command played again.
    pub fn press(&mut self, key: &Key, text: &str, cursor: usize, rows: usize) -> Response {
        if self.replaying {
            return self.answer(key, text, cursor, rows);
        }
        // `.` is normal mode's alone, and only when nothing is half-typed: after
        // `d` or `f` it is the character being waited for, and in a `/` line it
        // is a full stop.
        if self.mode == Mode::Normal
            && self.pending.is_empty()
            && self.prompt.is_none()
            && key.ch == Some('.')
            && !key.ctrl
        {
            return self.repeat(text, cursor, rows);
        }
        let was = self.mode;
        // While insert mode is being recorded the keys are text, and text is not
        // read key by key: see `note_change`.
        if self.insert_at.is_none() {
            self.recording.push(key.clone());
        }
        let response = self.answer(key, text, cursor, rows);
        self.note_change(was, text, cursor, &response);
        response
    }

    /// Files what the keystroke just answered amounts to.
    ///
    /// A change is rarely one key: `3dw` is four, `cwfoo` and its `Esc` are six,
    /// and what `.` has to play again is all of them. They are kept as they come
    /// and thrown away as soon as the command turns out to change nothing — a
    /// motion, a mode, a scroll.
    fn note_change(&mut self, was: Mode, text: &str, cursor: usize, response: &Response) {
        // Going into insert mode: the keys so far **are** the command, and
        // everything after them is text.
        if was != Mode::Insert && self.mode == Mode::Insert {
            let Response::Apply(change) = response else {
                self.recording.clear();
                return;
            };
            let len = match &change.edit {
                Some(edit) => text.len() - (edit.range.end - edit.range.start) + edit.text.len(),
                None => text.len(),
            };
            self.insert_at = Some((change.head, len));
            return;
        }
        // Coming out of it: what was typed is what lies between where the
        // insertion began and where the caret is now. Deduced and not recorded,
        // which is the only way to get what the editor added of its own accord.
        if was == Mode::Insert && self.mode != Mode::Insert {
            let taken = self.insert_at.take();
            let keys = std::mem::take(&mut self.recording);
            let Some((start, len)) = taken else { return };
            if cursor < start || text.len() != len + (cursor - start) {
                // Clicked away from, or backspaced past its start: nothing is
                // filed rather than a command whose text half is a guess. The
                // change before it stays what `.` plays.
                return;
            }
            if self.adopted {
                self.adopted = false;
                return;
            }
            if let Some(typed) = text.get(start..cursor) {
                self.last_change = Some(Repeat {
                    keys,
                    insert: Some(typed.to_string()),
                });
            }
            return;
        }
        if self.mode == Mode::Insert {
            return; // still typing
        }
        if matches!(response, Response::Apply(change) if change.edit.is_some()) {
            let keys = std::mem::take(&mut self.recording);
            if !std::mem::take(&mut self.adopted) {
                self.last_change = Some(Repeat { keys, insert: None });
            }
        } else if self.pending.is_empty() && self.mode == Mode::Normal {
            // Nothing changed, nothing is half-typed and no mode is being held
            // open: whatever was being recorded was a motion, and a motion is
            // not what `.` plays. A visual mode **is** held open — `Ctrl+V j I`
            // is three keys and one command — so its keys stay.
            self.recording.clear();
            self.adopted = false;
        }
    }

    /// `.`: the last change, again, where the caret is now.
    ///
    /// The keys are played back through `answer` on a **copy** of the text, each
    /// one’s edit applied to it, and what the view is handed at the end is a
    /// single `Edit`: the shortest one that turns the text into the copy. That
    /// is what keeps `.` one transaction — one `u` undoes it — where replaying
    /// six keystrokes into the editor would be six.
    fn repeat(&mut self, text: &str, cursor: usize, rows: usize) -> Response {
        let Some(last) = self.last_change.clone() else {
            return Response::Consumed;
        };
        self.replaying = true;
        let mut buffer = text.to_string();
        let mut caret = cursor.min(buffer.len());
        let mut yank = None;
        let mut flash = Vec::new();
        let mut play = |vim: &mut Self, key: &Key, buffer: &mut String, caret: &mut usize| {
            if let Response::Apply(change) = vim.answer(key, buffer, *caret, rows) {
                if let Some(edit) = &change.edit {
                    *buffer = apply_edit(buffer, edit);
                }
                *caret = change.head.min(buffer.len());
                yank = change.yank.or(yank.take());
                if !change.flash.is_empty() {
                    flash = change.flash;
                }
            }
        };
        for key in &last.keys {
            play(self, key, &mut buffer, &mut caret);
        }
        if let Some(typed) = last.insert.filter(|_| self.mode == Mode::Insert) {
            buffer.insert_str(caret, &typed);
            caret += typed.len();
            // The `Esc` of the original, and not a mode set by hand: it is what
            // steps the caret back onto the last character typed, and what
            // repeats a blockwise insertion down its other rows.
            let escape = Key {
                ch: None,
                name: "escape".into(),
                ctrl: false,
            };
            play(self, &escape, &mut buffer, &mut caret);
        }
        self.replaying = false;
        if self.mode == Mode::Insert {
            // The command went somewhere its text half could not follow: normal
            // mode is where `.` leaves one, never a mode nobody asked for.
            self.mode = Mode::Normal;
        }
        self.pending.clear();
        self.head = caret.min(buffer.len());
        Response::Apply(Change {
            edit: minimal_edit(text, &buffer),
            selection: self.selection(&buffer),
            head: self.head,
            flash,
            yank,
        })
    }

    /// One keystroke, answered — everything `press` does not do itself.
    fn answer(&mut self, key: &Key, text: &str, cursor: usize, rows: usize) -> Response {
        if self.mode == Mode::Normal {
            // The caret is the editor's in normal mode: a click moves it, and
            // vim has to start from where the eye is.
            self.head = cursor.min(text.len());
        }
        if self.prompt.is_some() {
            return self.typing_prompt(key, text);
        }
        if self.mode == Mode::Insert {
            if key.is("escape") {
                self.mode = Mode::Normal;
                self.pending.clear();
                self.column = None;
                let caret = cursor.min(text.len());
                if let Some(change) = self.repeat_block_insert(text, caret) {
                    return Response::Apply(change);
                }
                // vim steps back onto the last character typed.
                let head = clamp_to_line(text, prev_boundary(text, caret));
                return Response::Apply(self.landing(text, head));
            }
            return Response::Ignored;
        }
        if key.ctrl {
            return self.control(key, text, rows);
        }
        let Some(ch) = key.ch else {
            if key.is("escape") {
                return self.escape(text);
            }
            // The two keys of normal mode that produce no character. They are
            // claimed here for a reason beyond completeness: the editor binds
            // both, and a `backspace` left to it **deletes** — the one keystroke
            // of normal mode that would destroy text nobody asked it to.
            if key.is("enter") {
                let below = move_lines(text, self.head, 1, Some(0));
                return self.motion_to(text, first_non_blank(text, below));
            }
            if key.is("backspace") {
                // vim's `Backspace` steps back over a line break, where `h`
                // stops at the start of the line.
                return self.motion_to(text, prev_boundary(text, self.head));
            }
            return Response::Ignored;
        };
        self.pending.push(ch);
        self.run(text)
    }

    /// `Esc` in normal mode drops what was being typed; in visual mode it drops
    /// the selection.
    fn escape(&mut self, text: &str) -> Response {
        self.pending.clear();
        // `:nohlsearch`, which is what every vim configuration binds `Esc` to:
        // the occurrences of the last search have been read, and leaving them
        // lit turns a search into a state.
        self.hlsearch = false;
        if self.mode == Mode::Normal {
            return Response::Consumed;
        }
        self.mode = Mode::Normal;
        let head = clamp_to_line(text, self.head);
        Response::Apply(self.landing(text, head))
    }

    /// The keystrokes vim writes with the control key.
    fn control(&mut self, key: &Key, text: &str, rows: usize) -> Response {
        let half = (rows / 2).max(1);
        match key.name.as_str() {
            "d" => self.motion_to(text, move_lines(text, self.head, half as isize, None)),
            "u" => self.motion_to(text, move_lines(text, self.head, -(half as isize), None)),
            "f" => self.motion_to(text, move_lines(text, self.head, rows as isize, None)),
            "b" => self.motion_to(text, move_lines(text, self.head, -(rows as isize), None)),
            // The window's `Ctrl+R` refresh steps aside for the editor in vim
            // mode: here the key is redo, and there is nowhere else for redo to
            // go — `Ctrl+Y`, which the editor binds to it, is vim's scroll.
            "r" => Response::Command(Command::Redo),
            // One line of the view, the caret staying where it is unless the
            // page walks out from under it. The editor knows the height; all
            // that is decided here is the direction.
            "e" => Response::Command(Command::Scroll(1)),
            "y" => Response::Command(Command::Scroll(-1)),
            // A rectangle. `Ctrl+Q` is vim's own second key for it, kept here
            // for the same reason vim has it: `Ctrl+V` is the paste of every
            // other program, and it does not always reach us.
            "v" | "q" => self.toggle_visual(text, Mode::VisualBlock),
            _ => Response::Ignored,
        }
    }

    /// A `/`, `?` or `:` line being typed.
    fn typing_prompt(&mut self, key: &Key, text: &str) -> Response {
        let Some(prompt) = self.prompt.as_mut() else {
            return Response::Ignored;
        };
        if key.is("escape") {
            self.prompt = None;
            return Response::Consumed;
        }
        if key.is("backspace") {
            if prompt.text.pop().is_none() {
                self.prompt = None;
            }
            return Response::Consumed;
        }
        if key.is("enter") {
            let prompt = self.prompt.take().expect("checked above");
            return self.execute_prompt(prompt, text);
        }
        if let Some(ch) = key.ch {
            prompt.text.push(ch);
            return Response::Consumed;
        }
        Response::Ignored
    }

    fn execute_prompt(&mut self, prompt: Prompt, text: &str) -> Response {
        if prompt.kind == ':' {
            let line = prompt.text.trim().trim_end_matches('!');
            return match line {
                "w" => Response::Command(Command::Save),
                "q" => Response::Command(Command::Close),
                "wq" | "x" => Response::Command(Command::SaveAndClose),
                _ => match line.parse::<usize>() {
                    // `:42` goes to the line, as `42G` does.
                    Ok(n) => self.motion_to(text, start_of_line_no(text, n.saturating_sub(1))),
                    Err(_) => Response::Consumed,
                },
            };
        }
        if !prompt.text.is_empty() {
            self.last_search = prompt.text;
            self.search_backward = prompt.kind == '?';
        }
        self.hlsearch = true;
        self.search(text, false)
    }

    /// Goes to the next occurrence of the last pattern, `reverse` swapping the
    /// direction — which is what `N` is.
    fn search(&mut self, text: &str, reverse: bool) -> Response {
        if self.last_search.is_empty() {
            return Response::Consumed;
        }
        let backward = self.search_backward != reverse;
        let found = if backward {
            find_backward(text, &self.last_search, self.head)
        } else {
            find_forward(text, &self.last_search, self.head)
        };
        match found {
            Some(offset) => self.motion_to(text, offset),
            None => Response::Consumed,
        }
    }

    /// Interprets what has been typed so far, and runs it once it is complete.
    fn run(&mut self, text: &str) -> Response {
        let keys: Vec<char> = self.pending.chars().collect();
        let mut at = 0;
        let count = read_count(&keys, &mut at);
        if at == keys.len() {
            return Response::Consumed;
        }
        // An operator only exists in normal mode: in visual mode the selection
        // *is* the range, and `d` acts at once.
        if self.mode == Mode::Normal && matches!(keys[at], 'd' | 'c' | 'y') {
            let operator = keys[at];
            at += 1;
            // `2d3w` deletes six words, as vim multiplies the two counts.
            let second = read_count(&keys, &mut at);
            if at == keys.len() {
                return Response::Consumed;
            }
            let count = count.unwrap_or(1) * second.unwrap_or(1);
            return self.operate(text, operator, count, &keys[at..]);
        }
        // A text object only exists after an operator or in visual mode: `i` and
        // `a` on their own are the two ways of entering insert mode.
        if self.mode != Mode::Normal && matches!(keys[at], 'i' | 'a') {
            if at + 1 >= keys.len() {
                return Response::Consumed;
            }
            let (around, object) = (keys[at] == 'a', keys[at + 1]);
            self.pending.clear();
            let Some(range) = text_object(text, self.head, around, object) else {
                return Response::Consumed;
            };
            if self.mode != Mode::Visual {
                self.mode = Mode::Visual;
            }
            self.anchor = range.start;
            let head = prev_boundary_in(text, range.end, range.start);
            return Response::Apply(self.landing(text, head));
        }
        match parse_motion(&keys[at..]) {
            Parsed::Incomplete => Response::Consumed,
            Parsed::Motion(motion) => {
                let target = self.aim(text, motion, count);
                self.pending.clear();
                match target {
                    Some(Target { offset, .. }) => self.motion_to(text, offset),
                    None => Response::Consumed,
                }
            }
            Parsed::None => self.simple(text, count, &keys[at..]),
        }
    }

    /// `d`, `c` and `y` followed by what they act on.
    fn operate(&mut self, text: &str, operator: char, count: usize, rest: &[char]) -> Response {
        // `dd`, `cc`, `yy`: the operator doubled takes whole lines.
        if rest[0] == operator {
            self.pending.clear();
            let range = line_span(text, self.head, count);
            return self.apply_operator(text, operator, range, true);
        }
        // `diw`, `ci(`, `da"`: the object names its own range, and the caret's
        // place in it does not enter into it.
        if matches!(rest[0], 'i' | 'a') {
            if rest.len() < 2 {
                return Response::Consumed;
            }
            self.pending.clear();
            let Some(range) = text_object(text, self.head, rest[0] == 'a', rest[1]) else {
                return Response::Consumed;
            };
            return self.apply_operator(text, operator, range, false);
        }
        match parse_motion(rest) {
            Parsed::Incomplete => Response::Consumed,
            Parsed::None => {
                self.pending.clear();
                Response::Consumed
            }
            Parsed::Motion(motion) => {
                let target = self.aim(text, motion, Some(count));
                self.pending.clear();
                let Some(Target { offset, kind }) = target else {
                    return Response::Consumed;
                };
                // vim's one special case: `dw` on the last word of a line stops
                // at the end of that line rather than eating the newline and the
                // indentation of the next.
                let offset = match motion {
                    Motion::WordForward(_)
                        if line_index(text, offset) > line_index(text, self.head) =>
                    {
                        end_of_line(text, self.head)
                    }
                    _ => offset,
                };
                let (from, to) = (self.head.min(offset), self.head.max(offset));
                let range = match kind {
                    Kind::Exclusive => from..to,
                    Kind::Inclusive => from..next_boundary(text, to),
                    Kind::Linewise => {
                        let first = start_of_line(text, from);
                        let last = end_of_line(text, to);
                        first..last
                    }
                };
                self.apply_operator(text, operator, range, kind == Kind::Linewise)
            }
        }
    }

    /// The three operators, once their range is known.
    fn apply_operator(
        &mut self,
        text: &str,
        operator: char,
        range: Range<usize>,
        linewise: bool,
    ) -> Response {
        let taken = text[range.clone()].to_string();
        self.record(Register {
            text: if linewise && !taken.ends_with('\n') {
                format!("{taken}\n")
            } else {
                taken
            },
            linewise,
        });
        self.mode = Mode::Normal;
        match operator {
            'y' => {
                // Yanking leaves the caret at the start of what was taken, and
                // the range lit for a moment: nothing else on screen says it
                // took.
                self.flashed = vec![range.clone()];
                let head = clamp_to_line(text, range.start);
                Response::Apply(self.landing(text, head))
            }
            'd' if linewise => {
                // A whole line goes with its newline, or with the one before it
                // when it is the last of the file.
                let range = swallow_newline(text, range);
                let head = clamp_to_line(&delete_result(text, &range), range.start);
                Response::Apply(self.edit(text, range, String::new(), head))
            }
            'd' => {
                let head = range.start;
                Response::Apply(self.edit(text, range, String::new(), head))
            }
            'c' if linewise => {
                // `cc` keeps the line and its indentation: that is where the
                // cursor is expected to land, not against the left margin.
                let indent = indent_of(text, range.start);
                let head = range.start + indent.len();
                self.mode = Mode::Insert;
                Response::Apply(self.edit(text, range, indent, head))
            }
            _ => {
                let head = range.start;
                self.mode = Mode::Insert;
                Response::Apply(self.edit(text, range, String::new(), head))
            }
        }
    }

    /// The commands that are neither a motion nor an operator.
    fn simple(&mut self, text: &str, count: Option<usize>, rest: &[char]) -> Response {
        let n = count.unwrap_or(1);
        let ch = rest[0];
        // The commands that wait for one more key.
        if ch == 'r' {
            if rest.len() < 2 {
                return Response::Consumed;
            }
            self.pending.clear();
            if self.mode == Mode::VisualBlock {
                return self.block_replace(text, rest[1]);
            }
            let end = advance(text, self.head, n).min(end_of_line(text, self.head));
            if end <= self.head {
                return Response::Consumed;
            }
            let replacement: String = std::iter::repeat_n(rest[1], n).collect();
            return Response::Apply(self.edit(text, self.head..end, replacement, self.head));
        }
        // `z` is a prefix in both modes: none of what it opens touches the
        // text, so there is no reason for visual mode to answer differently.
        if ch == 'z' {
            if rest.len() < 2 {
                return Response::Consumed;
            }
            self.pending.clear();
            return match rest[1] {
                'z' => Response::Command(Command::Reveal(Reveal::Centre)),
                't' => Response::Command(Command::Reveal(Reveal::Top)),
                'b' => Response::Command(Command::Reveal(Reveal::Bottom)),
                'c' => Response::Command(Command::Fold(Fold::Close)),
                'o' => Response::Command(Command::Fold(Fold::Open)),
                'a' => Response::Command(Command::Fold(Fold::Toggle)),
                'M' => Response::Command(Command::Fold(Fold::CloseAll)),
                'R' => Response::Command(Command::Fold(Fold::OpenAll)),
                'm' => Response::Command(Command::Fold(Fold::More)),
                'r' => Response::Command(Command::Fold(Fold::Less)),
                _ => Response::Consumed,
            };
        }
        // `g` is a prefix, and the motion parser has already taken the only
        // motion it opens (`gg`): what reaches here is a command.
        if ch == 'g' && self.mode == Mode::Normal {
            if rest.len() < 2 {
                return Response::Consumed;
            }
            self.pending.clear();
            return match rest[1] {
                'd' => Response::Command(Command::GoToDefinition),
                _ => Response::Consumed,
            };
        }
        self.pending.clear();
        if self.mode != Mode::Normal {
            return self.visual_simple(text, ch);
        }
        match ch {
            'i' => self.insert_at(text, self.head),
            'a' => self.insert_at(
                text,
                next_boundary(text, self.head).min(end_of_line(text, self.head)),
            ),
            'I' => self.insert_at(text, first_non_blank(text, self.head)),
            'A' => self.insert_at(text, end_of_line(text, self.head)),
            'o' => self.open_line(text, false),
            'O' => self.open_line(text, true),
            'v' => self.toggle_visual(text, Mode::Visual),
            'V' => self.toggle_visual(text, Mode::VisualLine),
            'x' => {
                let end = advance(text, self.head, n).min(end_of_line(text, self.head));
                self.yank(text, self.head..end, false);
                let head = clamp_to_line(&delete_result(text, &(self.head..end)), self.head);
                Response::Apply(self.edit(text, self.head..end, String::new(), head))
            }
            'X' => {
                let start = retreat(text, self.head, n).max(start_of_line(text, self.head));
                self.yank(text, start..self.head, false);
                Response::Apply(self.edit(text, start..self.head, String::new(), start))
            }
            's' => {
                let end = advance(text, self.head, n).min(end_of_line(text, self.head));
                self.yank(text, self.head..end, false);
                self.mode = Mode::Insert;
                Response::Apply(self.edit(text, self.head..end, String::new(), self.head))
            }
            'S' => self.apply_operator(text, 'c', line_span(text, self.head, n), true),
            'D' => {
                let range = self.head..end_of_line(text, self.head);
                self.yank(text, range.clone(), false);
                let head = clamp_to_line(&delete_result(text, &range), self.head);
                Response::Apply(self.edit(text, range, String::new(), head))
            }
            'C' => {
                let range = self.head..end_of_line(text, self.head);
                self.yank(text, range.clone(), false);
                self.mode = Mode::Insert;
                Response::Apply(self.edit(text, range, String::new(), self.head))
            }
            'Y' => self.apply_operator(text, 'y', line_span(text, self.head, n), true),
            'p' => self.paste(text, true, n),
            'P' => self.paste(text, false, n),
            'J' => self.join(text, n),
            'u' => Response::Command(Command::Undo),
            'n' => self.search(text, false),
            'N' => self.search(text, true),
            '/' | '?' | ':' => {
                self.prompt = Some(Prompt {
                    kind: ch,
                    text: String::new(),
                });
                Response::Consumed
            }
            _ => Response::Consumed,
        }
    }

    /// The commands of visual mode, which act on the selection.
    fn visual_simple(&mut self, text: &str, ch: char) -> Response {
        // A rectangle is not a run of text: what cuts, copies or fills it is one
        // edit per line, and it answers before the arms below ever ask for a
        // range. What it has no answer of its own to — `o`, `v`, `u`, a search —
        // falls through to them.
        if self.mode == Mode::VisualBlock {
            if let Some(response) = self.block_command(text, ch) {
                return response;
            }
        }
        let linewise = self.mode == Mode::VisualLine;
        let range = self.visual_range(text);
        match ch {
            'd' | 'x' => self.apply_operator(text, 'd', range, linewise),
            'y' => self.apply_operator(text, 'y', range, linewise),
            'c' | 's' => self.apply_operator(text, 'c', range, linewise),
            'p' => {
                let register = self.register.clone();
                self.mode = Mode::Normal;
                let head = range.start;
                Response::Apply(self.edit(text, range, register.text, head))
            }
            'o' => {
                std::mem::swap(&mut self.anchor, &mut self.head);
                Response::Apply(self.landing(text, self.head))
            }
            'v' => self.toggle_visual(text, Mode::Visual),
            'V' => self.toggle_visual(text, Mode::VisualLine),
            'u' => Response::Command(Command::Undo),
            ':' | '/' | '?' => {
                self.prompt = Some(Prompt {
                    kind: ch,
                    text: String::new(),
                });
                Response::Consumed
            }
            _ => Response::Consumed,
        }
    }

    /// `v`, `V` and `Ctrl+V`, from wherever one is.
    ///
    /// The same key again leaves visual mode; another one swaps to it and
    /// **keeps the anchor**, which is what makes `v` after `V` narrow what is
    /// selected rather than start again from the caret.
    fn toggle_visual(&mut self, text: &str, mode: Mode) -> Response {
        if self.mode == mode {
            self.mode = Mode::Normal;
            let head = clamp_to_line(text, self.head);
            return Response::Apply(self.landing(text, head));
        }
        if self.mode == Mode::Normal {
            self.anchor = self.head;
            // A desired column left over from an earlier `$` would make the
            // rectangle reach the end of every line before one has asked for it.
            self.column = None;
        }
        self.mode = mode;
        Response::Apply(self.landing(text, self.head))
    }

    /// The two columns a blockwise selection lies between.
    ///
    /// The right one is `usize::MAX` when `$` has been pressed: the rectangle
    /// then reaches the end of every line it covers, however long they are —
    /// which is the whole reason vim keeps a desired column.
    fn block_columns(&self, text: &str) -> (usize, usize) {
        let anchor = column_of(text, self.anchor);
        let head = self.column.unwrap_or_else(|| column_of(text, self.head));
        (anchor.min(head), anchor.max(head))
    }

    /// The rectangle, one range per line — empty on a line too short to reach
    /// the left column, which is a line the block covers and has nothing of.
    fn block_rows(&self, text: &str) -> Vec<Range<usize>> {
        let (left, right) = self.block_columns(text);
        let (a, b) = (line_index(text, self.anchor), line_index(text, self.head));
        (a.min(b)..=a.max(b))
            .map(|row| {
                let start = start_of_line_no(text, row);
                let from = column_offset(text, start, left);
                let to = if right == usize::MAX {
                    end_of_line(text, start)
                } else {
                    column_offset(text, start, right + 1)
                };
                from..to.max(from)
            })
            .collect()
    }

    /// The rectangle the view paints.
    ///
    /// Blockwise is the one selection the editor cannot hold: its own is a
    /// single run of text, and a block is one range per line. It is asked for at
    /// every frame, like `cursor`, and for the same reason.
    pub fn block_selection(&self, text: &str) -> Vec<Range<usize>> {
        if self.mode != Mode::VisualBlock {
            return Vec::new();
        }
        self.block_rows(text)
            .into_iter()
            .filter(|row| !row.is_empty())
            .collect()
    }

    /// The commands of blockwise visual mode, `None` being "not one of mine".
    fn block_command(&mut self, text: &str, ch: char) -> Option<Response> {
        if !matches!(ch, 'd' | 'x' | 'y' | 'c' | 's' | 'p' | 'I' | 'A') {
            return None;
        }
        let rows = self.block_rows(text);
        let first = rows.first()?.start;
        let (left, right) = self.block_columns(text);
        let top = line_index(text, first);
        let bottom = top + rows.len() - 1;
        if matches!(ch, 'I' | 'A') {
            // `A` goes past the right edge, and `$A` past the end of every line,
            // however long each one is.
            let column = match ch {
                'I' => Some(left),
                _ if right == usize::MAX => None,
                _ => Some(right + 1),
            };
            let start = start_of_line_no(text, top);
            let at = match column {
                Some(column) => column_offset(text, start, column),
                None => end_of_line(text, start),
            };
            self.mode = Mode::Insert;
            self.arm_block_insert(text, top, bottom, column, ch == 'A', at);
            return Some(Response::Apply(self.landing(text, at)));
        }
        // What is taken goes to the register in every case, `p` excepted, which
        // has to read what is in there before it clobbers it.
        let pasted = (ch == 'p').then(|| self.register.text.clone());
        self.record(Register {
            text: block_text(text, &rows),
            linewise: false,
        });
        if ch == 'y' {
            self.flashed = rows.into_iter().filter(|row| !row.is_empty()).collect();
            self.mode = Mode::Normal;
            let head = clamp_to_line(text, first);
            return Some(Response::Apply(self.landing(text, head)));
        }
        let cuts: Vec<_> = rows
            .iter()
            .filter(|row| !row.is_empty())
            .map(|row| (row.clone(), String::new()))
            .collect();
        let mut edit = if cuts.is_empty() {
            Edit {
                range: first..first,
                text: String::new(),
            }
        } else {
            splice(text, &cuts)
        };
        if let Some(pasted) = pasted {
            // A register has no width of its own: it goes back where the
            // rectangle began, in one piece, rather than be cut into rows one
            // would have invented.
            edit.text.insert_str(0, &pasted);
        }
        let after = apply_edit(text, &edit);
        if matches!(ch, 'c' | 's') {
            self.mode = Mode::Insert;
            self.arm_block_insert(&after, top, bottom, Some(left), false, first);
            return Some(Response::Apply(
                self.edit(text, edit.range, edit.text, first),
            ));
        }
        self.mode = Mode::Normal;
        let head = clamp_to_line(&after, first);
        Some(Response::Apply(
            self.edit(text, edit.range, edit.text, head),
        ))
    }

    /// `r` over a rectangle: every character of it becomes the same one.
    fn block_replace(&mut self, text: &str, ch: char) -> Response {
        let rows = self.block_rows(text);
        let head = rows.first().map(|row| row.start).unwrap_or(self.head);
        let cuts: Vec<_> = rows
            .iter()
            .filter(|row| !row.is_empty())
            .map(|row| {
                let filled = text[row.clone()].chars().map(|_| ch).collect::<String>();
                (row.clone(), filled)
            })
            .collect();
        self.mode = Mode::Normal;
        if cuts.is_empty() {
            let head = clamp_to_line(text, head);
            return Response::Apply(self.landing(text, head));
        }
        let edit = splice(text, &cuts);
        let head = clamp_to_line(&apply_edit(text, &edit), head);
        Response::Apply(self.edit(text, edit.range, edit.text, head))
    }

    /// Arms the repeat `Esc` will make of what is about to be typed.
    fn arm_block_insert(
        &mut self,
        text: &str,
        top: usize,
        bottom: usize,
        column: Option<usize>,
        pad: bool,
        start: usize,
    ) {
        self.block_insert = Some(BlockInsert {
            lines: (top + 1..=bottom).collect(),
            column,
            pad,
            start,
            len: text.len(),
        });
    }

    /// Writes what has just been typed on the other rows of the block.
    ///
    /// It gives up — and gives up entirely, rather than repeat something nobody
    /// typed — on anything but a plain forward insertion at the caret: a
    /// newline, a backspace, a click elsewhere. Vim gives up on the newline too.
    fn repeat_block_insert(&mut self, text: &str, caret: usize) -> Option<Change> {
        let pending = self.block_insert.take()?;
        if caret < pending.start || text.len() != pending.len + (caret - pending.start) {
            return None;
        }
        let typed = text.get(pending.start..caret)?;
        if typed.is_empty() || typed.contains('\n') {
            return None;
        }
        let typed = typed.to_string();
        let mut cuts = Vec::new();
        for line in pending.lines {
            let start = start_of_line_no(text, line);
            let end = end_of_line(text, start);
            let (at, payload) = match pending.column {
                None => (end, typed.clone()),
                Some(column) => {
                    let at = column_offset(text, start, column);
                    if at >= end {
                        // The line stops before the block: `A` pads out to the
                        // column, `I` skips the line, as vim does with each.
                        if !pending.pad {
                            continue;
                        }
                        let blanks = column.saturating_sub(column_of(text, end));
                        (end, format!("{}{typed}", " ".repeat(blanks)))
                    } else {
                        (at, typed.clone())
                    }
                }
            };
            cuts.push((at..at, payload));
        }
        if cuts.is_empty() {
            return None;
        }
        let edit = splice(text, &cuts);
        let after = apply_edit(text, &edit);
        let head = clamp_to_line(&after, prev_boundary(text, caret));
        Some(self.edit(text, edit.range, edit.text, head))
    }

    fn insert_at(&mut self, text: &str, offset: usize) -> Response {
        self.mode = Mode::Insert;
        Response::Apply(self.landing(text, offset.min(text.len())))
    }

    /// `o` and `O`: a new line, indented like the one it is opened from.
    fn open_line(&mut self, text: &str, above: bool) -> Response {
        let indent = indent_of(text, self.head);
        self.mode = Mode::Insert;
        if above {
            let start = start_of_line(text, self.head);
            let head = start + indent.len();
            return Response::Apply(self.edit(text, start..start, format!("{indent}\n"), head));
        }
        let end = end_of_line(text, self.head);
        let head = end + 1 + indent.len();
        Response::Apply(self.edit(text, end..end, format!("\n{indent}"), head))
    }

    /// `p` and `P`, charwise or linewise as the register says.
    fn paste(&mut self, text: &str, after: bool, count: usize) -> Response {
        let register = self.register.clone();
        if register.text.is_empty() {
            return Response::Consumed;
        }
        let payload = register.text.repeat(count);
        if register.linewise {
            let at = if after {
                let end = end_of_line(text, self.head);
                if end < text.len() {
                    end + 1
                } else {
                    text.len()
                }
            } else {
                start_of_line(text, self.head)
            };
            // A file whose last line has no newline needs one before a line is
            // laid after it.
            let payload = if at == text.len() && !text.ends_with('\n') && at > 0 {
                format!("\n{}", payload.trim_end_matches('\n'))
            } else {
                payload
            };
            let head = at + leading_blanks(&payload);
            return Response::Apply(self.edit(text, at..at, payload, head));
        }
        let at = if after {
            next_boundary(text, self.head).min(end_of_line(text, self.head))
        } else {
            self.head
        };
        // The caret lands on the last character put back, as vim leaves it.
        let head = at + prev_boundary(&payload, payload.len());
        Response::Apply(self.edit(text, at..at, payload, head))
    }

    /// `J`: the following lines joined onto this one, separated by a single
    /// space — the whole run being one edit, since that is what an undo has to
    /// take back in one go.
    fn join(&mut self, text: &str, count: usize) -> Response {
        let start = start_of_line(text, self.head);
        let mut end = end_of_line(text, start);
        let mut joined = text[start..end].to_string();
        // The caret lands on the first inserted space, as vim leaves it.
        let mut head = start + joined.trim_end().len();
        let mut first = true;
        for _ in 0..count.max(2) - 1 {
            if end >= text.len() {
                break;
            }
            let next_end = end_of_line(text, end + 1);
            let next = text[end + 1..next_end].trim_start();
            if first {
                head = start + joined.trim_end().len();
                first = false;
            }
            if !joined.is_empty() && !joined.ends_with(' ') && !next.is_empty() {
                joined.push(' ');
            }
            joined.push_str(next);
            end = next_end;
        }
        if first {
            // Nothing below: there was nothing to join.
            return Response::Consumed;
        }
        Response::Apply(self.edit(text, start..end, joined, head))
    }

    fn yank(&mut self, text: &str, range: Range<usize>, linewise: bool) {
        self.record(Register {
            text: text[range].to_string(),
            linewise,
        });
    }

    /// Files what has just been taken, for the register and for the `Change`
    /// alike — the view carrying it to the clipboard from there.
    fn record(&mut self, register: Register) {
        self.register = register.clone();
        self.yanked = Some(register);
    }

    /// The selection a visual mode covers, the head being inclusive.
    fn visual_range(&self, text: &str) -> Range<usize> {
        let (from, to) = (self.anchor.min(self.head), self.anchor.max(self.head));
        if self.mode == Mode::VisualLine {
            return start_of_line(text, from)..end_of_line(text, to);
        }
        from..next_boundary(text, to)
    }

    /// A motion's landing, in whichever mode.
    fn motion_to(&mut self, text: &str, offset: usize) -> Response {
        self.pending.clear();
        let head = if self.mode == Mode::Normal {
            clamp_to_line(text, offset)
        } else {
            offset.min(text.len())
        };
        Response::Apply(self.landing(text, head))
    }

    /// What the view has to show once the caret is at `head`.
    fn landing(&mut self, text: &str, head: usize) -> Change {
        self.head = head.min(text.len());
        Change {
            edit: None,
            selection: self.selection(text),
            head: self.head,
            flash: std::mem::take(&mut self.flashed),
            yank: self.yanked.take(),
        }
    }

    /// The same, after an edit — the selection being computed on the text the
    /// edit **leaves behind**, and not on the one it was computed from. Without
    /// that, the block cursor would vanish after every `x` and only come back on
    /// the next motion.
    fn edit(
        &mut self,
        text: &str,
        range: Range<usize>,
        replacement: String,
        head: usize,
    ) -> Change {
        self.head = head;
        let mut after = String::with_capacity(text.len() + replacement.len());
        after.push_str(&text[..range.start]);
        after.push_str(&replacement);
        after.push_str(&text[range.end..]);
        Change {
            selection: self.selection(&after),
            edit: Some(Edit {
                range,
                text: replacement,
            }),
            head,
            flash: std::mem::take(&mut self.flashed),
            yank: self.yanked.take(),
        }
    }

    /// The block cursor, or the visual selection.
    fn selection(&self, text: &str) -> Range<usize> {
        match self.mode {
            Mode::Insert => self.head..self.head,
            // A rectangle is painted by the view, on a layer of its own: the
            // editor's selection is one run of text, and handing it the whole
            // span would light the columns the block leaves out.
            Mode::Normal | Mode::VisualBlock => block(text, self.head),
            _ => self.visual_range(text),
        }
    }

    /// Where the block cursor is to be painted, `caret` being where the editor
    /// says the caret is.
    ///
    /// The view asks at every frame rather than only after a keystroke, and that
    /// is the point: nothing has been pressed yet when a file opens, and a click
    /// of the mouse moves the caret without telling vim. Asking is what makes the
    /// cursor be there from the first frame, and follow the mouse afterwards.
    ///
    /// `None` in insert mode, where the editor's own caret is the cursor, and an
    /// **empty** range on an empty line and at the end of the file, where there
    /// is no character to paint over — the view gives the caret back for those.
    pub fn cursor(&self, text: &str, caret: usize) -> Option<Range<usize>> {
        match self.mode {
            Mode::Insert => None,
            // The caret is the editor's in normal mode, as `press` assumes.
            Mode::Normal => Some(block(text, caret)),
            // In a visual mode the head is vim's own: the editor's selection
            // covers the whole range, and its caret says nothing about which end
            // is being moved.
            _ => Some(block(text, self.head)),
        }
    }

    /// Where a motion lands, and how an operator is to read that range.
    fn aim(&mut self, text: &str, motion: Motion, given: Option<usize>) -> Option<Target> {
        let head = self.head;
        let count = given.unwrap_or(1);
        let target = match motion {
            Motion::Left => {
                self.column = None;
                Target::exclusive(retreat(text, head, count).max(start_of_line(text, head)))
            }
            Motion::Right => {
                self.column = None;
                Target::exclusive(advance(text, head, count).min(end_of_line(text, head)))
            }
            Motion::Down | Motion::Up => {
                let delta = if motion == Motion::Down {
                    count as isize
                } else {
                    -(count as isize)
                };
                let column = self.column.unwrap_or_else(|| column_of(text, head));
                self.column = Some(column);
                let row = crate::ui::folds::step(
                    &self.folds,
                    line_index(text, head),
                    delta,
                    last_line(text),
                );
                return Some(Target {
                    offset: line_column(text, row, column),
                    kind: Kind::Linewise,
                });
            }
            Motion::WordForward(big) => {
                self.column = None;
                let mut at = head;
                for _ in 0..count {
                    at = word_forward(text, at, big);
                }
                Target::exclusive(at)
            }
            Motion::WordBackward(big) => {
                self.column = None;
                let mut at = head;
                for _ in 0..count {
                    at = word_backward(text, at, big);
                }
                Target::exclusive(at)
            }
            Motion::WordEnd(big) => {
                self.column = None;
                let mut at = head;
                for _ in 0..count {
                    at = word_end(text, at, big);
                }
                Target::inclusive(at)
            }
            Motion::LineStart => {
                self.column = Some(0);
                Target::exclusive(start_of_line(text, head))
            }
            Motion::FirstNonBlank => {
                self.column = None;
                Target::exclusive(first_non_blank(text, head))
            }
            Motion::LineEnd => {
                // `$` sticks: `j` after it stays at the end of every line.
                self.column = Some(usize::MAX);
                let line = if count > 1 {
                    move_lines(text, head, count as isize - 1, Some(usize::MAX))
                } else {
                    head
                };
                Target::inclusive(prev_boundary_in(
                    text,
                    end_of_line(text, line),
                    start_of_line(text, line),
                ))
            }
            Motion::GoToLine(explicit) => {
                self.column = None;
                let line = match explicit.or(given) {
                    Some(n) => n.saturating_sub(1),
                    None => last_line(text),
                };
                return Some(Target {
                    offset: first_non_blank(text, start_of_line_no(text, line)),
                    kind: Kind::Linewise,
                });
            }
            Motion::FirstLine => {
                self.column = None;
                return Some(Target {
                    offset: first_non_blank(text, start_of_line_no(text, count.saturating_sub(1))),
                    kind: Kind::Linewise,
                });
            }
            Motion::Find(find) => {
                self.column = None;
                self.last_find = Some(find);
                let found = find_in_line(text, head, find.ch, find.till, find.backward, count)?;
                if find.backward {
                    Target::exclusive(found)
                } else {
                    Target::inclusive(found)
                }
            }
            Motion::FindAgain(reverse) => {
                self.column = None;
                let find = self.last_find?;
                let backward = find.backward != reverse;
                // A `t` played again would not move: the caret is already one
                // character short of the one it was told to stop before, and
                // "up to the next one" is where it already stands. vim steps
                // over that character first, and that is what makes `;` walk
                // down a line rather than sit on it.
                let from = match (find.till, backward) {
                    (true, false) => next_boundary(text, head),
                    (true, true) => prev_boundary_in(text, head, start_of_line(text, head)),
                    _ => head,
                };
                let found = find_in_line(text, from, find.ch, find.till, backward, count)?;
                if backward {
                    Target::exclusive(found)
                } else {
                    Target::inclusive(found)
                }
            }
        };
        Some(target)
    }
}

/// The three ways an operator reads the range a motion describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Exclusive,
    Inclusive,
    Linewise,
}

struct Target {
    offset: usize,
    kind: Kind,
}

impl Target {
    fn exclusive(offset: usize) -> Self {
        Self {
            offset,
            kind: Kind::Exclusive,
        }
    }
    fn inclusive(offset: usize) -> Self {
        Self {
            offset,
            kind: Kind::Inclusive,
        }
    }
}

/// A character search within the line: what `f`, `F`, `t` and `T` describe, and
/// what `;` and `,` play again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Find {
    ch: char,
    /// `t` and `T`: stop one character short of it.
    till: bool,
    backward: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    /// `w` / `W`, the flag being the big word.
    WordForward(bool),
    WordBackward(bool),
    WordEnd(bool),
    LineStart,
    FirstNonBlank,
    LineEnd,
    /// `G`, with the count it was given.
    GoToLine(Option<usize>),
    /// `gg`.
    FirstLine,
    Find(Find),
    /// `;` and `,`: the last find again, the flag turning it round.
    FindAgain(bool),
}

enum Parsed {
    /// A motion that needs one more key: `g`, `f`.
    Incomplete,
    Motion(Motion),
    /// Not a motion at all.
    None,
}

/// Reads the count a command starts with. A leading `0` is `^`'s neighbour and
/// not a count, which is why the digit is only taken when one is under way.
fn read_count(keys: &[char], at: &mut usize) -> Option<usize> {
    let mut count = 0usize;
    while *at < keys.len() {
        let c = keys[*at];
        if !c.is_ascii_digit() || (c == '0' && count == 0) {
            break;
        }
        count = count.saturating_mul(10) + (c as usize - '0' as usize);
        *at += 1;
    }
    (count > 0).then_some(count)
}

fn parse_motion(keys: &[char]) -> Parsed {
    match keys[0] {
        'h' => Parsed::Motion(Motion::Left),
        'l' | ' ' => Parsed::Motion(Motion::Right),
        'j' => Parsed::Motion(Motion::Down),
        'k' => Parsed::Motion(Motion::Up),
        'w' => Parsed::Motion(Motion::WordForward(false)),
        'W' => Parsed::Motion(Motion::WordForward(true)),
        'b' => Parsed::Motion(Motion::WordBackward(false)),
        'B' => Parsed::Motion(Motion::WordBackward(true)),
        'e' => Parsed::Motion(Motion::WordEnd(false)),
        'E' => Parsed::Motion(Motion::WordEnd(true)),
        '0' => Parsed::Motion(Motion::LineStart),
        '^' => Parsed::Motion(Motion::FirstNonBlank),
        '$' => Parsed::Motion(Motion::LineEnd),
        'G' => Parsed::Motion(Motion::GoToLine(None)),
        ';' => Parsed::Motion(Motion::FindAgain(false)),
        ',' => Parsed::Motion(Motion::FindAgain(true)),
        'g' => {
            if keys.len() < 2 {
                return Parsed::Incomplete;
            }
            match keys[1] {
                'g' => Parsed::Motion(Motion::FirstLine),
                _ => Parsed::None,
            }
        }
        c @ ('f' | 'F' | 't' | 'T') => {
            if keys.len() < 2 {
                return Parsed::Incomplete;
            }
            Parsed::Motion(Motion::Find(Find {
                ch: keys[1],
                till: c == 't' || c == 'T',
                backward: c == 'F' || c == 'T',
            }))
        }
        _ => Parsed::None,
    }
}

// — Reading the text ————————————————————————————————————————————

fn char_at(text: &str, at: usize) -> Option<char> {
    text.get(at..).and_then(|rest| rest.chars().next())
}

/// The one character the block cursor covers, at `caret`.
///
/// Empty on an empty line and at the end of the file: there is no character
/// there, and a block cursor is a character painted over.
pub fn block(text: &str, caret: usize) -> Range<usize> {
    let caret = clamp_to_line(text, caret);
    caret..next_boundary(text, caret).min(end_of_line(text, caret))
}

fn next_boundary(text: &str, at: usize) -> usize {
    match char_at(text, at) {
        Some(c) => at + c.len_utf8(),
        None => text.len(),
    }
}

fn prev_boundary(text: &str, at: usize) -> usize {
    prev_boundary_in(text, at, 0)
}

/// The character boundary before `at`, never going below `floor`.
fn prev_boundary_in(text: &str, at: usize, floor: usize) -> usize {
    let mut i = at.min(text.len());
    if i <= floor {
        return floor;
    }
    i -= 1;
    while i > floor && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn advance(text: &str, mut at: usize, count: usize) -> usize {
    for _ in 0..count {
        at = next_boundary(text, at);
    }
    at
}

fn retreat(text: &str, mut at: usize, count: usize) -> usize {
    for _ in 0..count {
        at = prev_boundary(text, at);
    }
    at
}

fn start_of_line(text: &str, at: usize) -> usize {
    text[..at.min(text.len())]
        .rfind('\n')
        .map(|p| p + 1)
        .unwrap_or(0)
}

fn end_of_line(text: &str, at: usize) -> usize {
    let at = at.min(text.len());
    text[at..].find('\n').map(|p| at + p).unwrap_or(text.len())
}

fn first_non_blank(text: &str, at: usize) -> usize {
    let start = start_of_line(text, at);
    let end = end_of_line(text, at);
    let mut i = start;
    while i < end {
        match char_at(text, i) {
            Some(c) if c == ' ' || c == '\t' => i = next_boundary(text, i),
            _ => break,
        }
    }
    i.min(if end > start {
        prev_boundary(text, end)
    } else {
        end
    })
}

/// The indentation of the line `at` belongs to.
fn indent_of(text: &str, at: usize) -> String {
    let start = start_of_line(text, at);
    let end = end_of_line(text, at);
    text[start..end]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

fn leading_blanks(text: &str) -> usize {
    text.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(char::len_utf8)
        .sum()
}

fn line_index(text: &str, at: usize) -> usize {
    text[..at.min(text.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
}

fn last_line(text: &str) -> usize {
    text.bytes().filter(|b| *b == b'\n').count()
        - usize::from(text.ends_with('\n') && !text.is_empty())
}

fn start_of_line_no(text: &str, line: usize) -> usize {
    let mut at = 0;
    for _ in 0..line {
        let end = end_of_line(text, at);
        if end >= text.len() {
            return start_of_line(text, text.len());
        }
        at = end + 1;
    }
    at
}

fn column_of(text: &str, at: usize) -> usize {
    text[start_of_line(text, at)..at].chars().count()
}

/// The offset `delta` lines away, at `column` — or at the same column, when
/// none is given.
fn move_lines(text: &str, at: usize, delta: isize, column: Option<usize>) -> usize {
    let column = column.unwrap_or_else(|| column_of(text, at));
    let row = line_index(text, at) as isize + delta;
    let row = row.clamp(0, last_line(text) as isize) as usize;
    line_column(text, row, column)
}

/// The offset of the `column`-th character of line `row`, clamped to its end.
fn line_column(text: &str, row: usize, column: usize) -> usize {
    let start = start_of_line_no(text, row);
    let end = end_of_line(text, start);
    let mut offset = start;
    for (seen, (i, c)) in text[start..end].char_indices().enumerate() {
        if seen == column {
            return start + i;
        }
        offset = start + i + c.len_utf8();
    }
    offset
}

/// Normal mode's caret sits **on** a character, never past the last one.
fn clamp_to_line(text: &str, at: usize) -> usize {
    let at = at.min(text.len());
    let start = start_of_line(text, at);
    let end = end_of_line(text, at);
    if at >= end && end > start {
        return prev_boundary(text, end);
    }
    at.max(start)
}

/// The offset of the `column`-th character of the line starting at `start`,
/// clamped to that line's end.
fn column_offset(text: &str, start: usize, column: usize) -> usize {
    let end = end_of_line(text, start);
    let mut at = start;
    for _ in 0..column {
        if at >= end {
            return end;
        }
        at = next_boundary(text, at);
    }
    at.min(end)
}

/// A rectangle, as one text: the rows one under the other.
fn block_text(text: &str, rows: &[Range<usize>]) -> String {
    rows.iter()
        .map(|row| &text[row.clone()])
        .collect::<Vec<_>>()
        .join("\n")
}

/// Several cuts of one rectangle, made into the single edit the view applies.
///
/// The rows of a block are contiguous lines, so what lies between two cuts is
/// text to keep: one range from the first cut to the last, with the kept pieces
/// spliced back in, leaves the same thing behind as many edits would — and it is
/// **one** transaction, which is what an undo has to take back in one go.
///
/// The cuts are in the order of the text and do not overlap, which is what the
/// rows of a rectangle are.
fn splice(text: &str, cuts: &[(Range<usize>, String)]) -> Edit {
    let start = cuts[0].0.start;
    let end = cuts[cuts.len() - 1].0.end;
    let mut out = String::new();
    let mut at = start;
    for (range, replacement) in cuts {
        out.push_str(&text[at..range.start]);
        out.push_str(replacement);
        at = range.end;
    }
    out.push_str(&text[at..end]);
    Edit {
        range: start..end,
        text: out,
    }
}

/// The text an edit leaves behind.
/// The shortest replacement that turns `before` into `after`.
///
/// What `.` hands the view: a repeat is several edits chained on a copy of the
/// text, and the view applies **one**. Common prefix and common suffix, which is
/// the same reckoning `lsp::sync` makes for a document change — and for the same
/// reason: a range that covers the whole file is an undo step that swallows the
/// file, a scroll position lost and, in the editor, a transaction the size of
/// the buffer.
///
/// The boundaries are walked back onto **characters**: two texts can share a
/// prefix that ends in the middle of an accented one, and a range that cuts a
/// character in half panics on the slice.
fn minimal_edit(before: &str, after: &str) -> Option<Edit> {
    if before == after {
        return None;
    }
    let mut start = 0;
    let ceiling = before.len().min(after.len());
    while start < ceiling && before.as_bytes()[start] == after.as_bytes()[start] {
        start += 1;
    }
    while start > 0 && !(before.is_char_boundary(start) && after.is_char_boundary(start)) {
        start -= 1;
    }
    let (mut end, mut tail) = (before.len(), after.len());
    while end > start && tail > start && before.as_bytes()[end - 1] == after.as_bytes()[tail - 1] {
        end -= 1;
        tail -= 1;
    }
    while end < before.len() && !(before.is_char_boundary(end) && after.is_char_boundary(tail)) {
        end += 1;
        tail += 1;
    }
    Some(Edit {
        range: start..end,
        text: after[start..tail].to_string(),
    })
}

fn apply_edit(text: &str, edit: &Edit) -> String {
    let mut out = String::with_capacity(text.len() + edit.text.len());
    out.push_str(&text[..edit.range.start]);
    out.push_str(&edit.text);
    out.push_str(&text[edit.range.end..]);
    out
}

fn delete_result(text: &str, range: &Range<usize>) -> String {
    let mut out = String::with_capacity(text.len() - (range.end - range.start));
    out.push_str(&text[..range.start]);
    out.push_str(&text[range.end..]);
    out
}

/// `count` whole lines from the one `at` belongs to, newline included.
fn line_span(text: &str, at: usize, count: usize) -> Range<usize> {
    let start = start_of_line(text, at);
    let mut end = end_of_line(text, at);
    for _ in 1..count {
        if end >= text.len() {
            break;
        }
        end = end_of_line(text, end + 1);
    }
    start..end
}

/// Widens a linewise range to take its newline, or the one before it when the
/// range ends the file.
fn swallow_newline(text: &str, range: Range<usize>) -> Range<usize> {
    if range.end < text.len() {
        return range.start..range.end + 1;
    }
    if range.start > 0 {
        return range.start - 1..range.end;
    }
    range
}

// — The words ————————————————————————————————————————————————————

#[derive(PartialEq, Clone, Copy)]
enum Class {
    Blank,
    Word,
    Punct,
}

fn class_of(c: char, big: bool) -> Class {
    if c.is_whitespace() {
        Class::Blank
    } else if big || c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

fn class_at(text: &str, at: usize, big: bool) -> Class {
    char_at(text, at)
        .map(|c| class_of(c, big))
        .unwrap_or(Class::Blank)
}

fn word_forward(text: &str, mut at: usize, big: bool) -> usize {
    let len = text.len();
    if at >= len {
        return len;
    }
    let start = class_at(text, at, big);
    if start != Class::Blank {
        while at < len && class_at(text, at, big) == start {
            at = next_boundary(text, at);
        }
    }
    while at < len && class_at(text, at, big) == Class::Blank {
        at = next_boundary(text, at);
    }
    at
}

fn word_backward(text: &str, mut at: usize, big: bool) -> usize {
    if at == 0 {
        return 0;
    }
    at = prev_boundary(text, at);
    while at > 0 && class_at(text, at, big) == Class::Blank {
        at = prev_boundary(text, at);
    }
    let class = class_at(text, at, big);
    if class == Class::Blank {
        return at;
    }
    while at > 0 {
        let previous = prev_boundary(text, at);
        if class_at(text, previous, big) != class {
            break;
        }
        at = previous;
    }
    at
}

fn word_end(text: &str, mut at: usize, big: bool) -> usize {
    let len = text.len();
    if at >= len {
        return len;
    }
    at = next_boundary(text, at);
    while at < len && class_at(text, at, big) == Class::Blank {
        at = next_boundary(text, at);
    }
    if at >= len {
        return prev_boundary(text, len);
    }
    let class = class_at(text, at, big);
    loop {
        let next = next_boundary(text, at);
        if next >= len || class_at(text, next, big) != class {
            return at;
        }
        at = next;
    }
}

/// The text objects: `iw`, `a(`, `i"` and their kin.
///
/// They are what a hand reaches for far more often than a count — a word, the
/// inside of a call, the inside of a string — and they are the reason `i` and
/// `a` mean something else after an operator than they do on their own.
fn text_object(text: &str, at: usize, around: bool, object: char) -> Option<Range<usize>> {
    match object {
        'w' => Some(word_object(text, at, around, false)),
        'W' => Some(word_object(text, at, around, true)),
        '"' | '\'' | '`' => quote_object(text, at, around, object),
        '(' | ')' | 'b' => pair_object(text, at, around, '(', ')'),
        '{' | '}' | 'B' => pair_object(text, at, around, '{', '}'),
        '[' | ']' => pair_object(text, at, around, '[', ']'),
        '<' | '>' => pair_object(text, at, around, '<', '>'),
        _ => None,
    }
}

/// The run of characters of one class around the caret — and, for `aw`, the
/// blanks that follow it, or the ones before it when there are none after.
///
/// It never crosses a newline: a word object is a thing of one line, and `daw`
/// on the last word of a line must not pull the next one up.
fn word_object(text: &str, at: usize, around: bool, big: bool) -> Range<usize> {
    let at = at.min(prev_boundary(text, text.len()));
    let class = class_at(text, at, big);
    let mut start = at;
    while start > 0 {
        let previous = prev_boundary(text, start);
        if char_at(text, previous) == Some('\n') || class_at(text, previous, big) != class {
            break;
        }
        start = previous;
    }
    let mut end = next_boundary(text, at);
    while end < text.len() && char_at(text, end) != Some('\n') && class_at(text, end, big) == class
    {
        end = next_boundary(text, end);
    }
    if !around {
        return start..end;
    }
    let mut after = end;
    while after < text.len()
        && char_at(text, after) != Some('\n')
        && class_at(text, after, big) == Class::Blank
    {
        after = next_boundary(text, after);
    }
    if after > end {
        return start..after;
    }
    let mut before = start;
    while before > 0 {
        let previous = prev_boundary(text, before);
        if char_at(text, previous) == Some('\n') || class_at(text, previous, big) != Class::Blank {
            break;
        }
        before = previous;
    }
    before..end
}

/// The pair of brackets the caret is inside of — or the one it sits on.
fn pair_object(
    text: &str,
    at: usize,
    around: bool,
    open: char,
    close: char,
) -> Option<Range<usize>> {
    let here = char_at(text, at);
    let start = if here == Some(open) {
        at
    } else {
        let from = if here == Some(close) {
            prev_boundary(text, at)
        } else {
            at
        };
        scan_back(text, from, open, close)?
    };
    let end = scan_forward(text, next_boundary(text, start), open, close)?;
    if around {
        return Some(start..next_boundary(text, end));
    }
    Some(next_boundary(text, start)..end)
}

/// The unmatched opening bracket at or before `from`.
fn scan_back(text: &str, from: usize, open: char, close: char) -> Option<usize> {
    let mut at = from.min(text.len());
    let mut depth = 0usize;
    loop {
        match char_at(text, at) {
            Some(c) if c == close && at != from => depth += 1,
            Some(c) if c == open => {
                if depth == 0 {
                    return Some(at);
                }
                depth -= 1;
            }
            _ => {}
        }
        if at == 0 {
            return None;
        }
        at = prev_boundary(text, at);
    }
}

/// The closing bracket that matches, starting from `from`.
fn scan_forward(text: &str, from: usize, open: char, close: char) -> Option<usize> {
    let mut at = from;
    let mut depth = 0usize;
    while at < text.len() {
        match char_at(text, at) {
            Some(c) if c == open => depth += 1,
            Some(c) if c == close => {
                if depth == 0 {
                    return Some(at);
                }
                depth -= 1;
            }
            _ => {}
        }
        at = next_boundary(text, at);
    }
    None
}

/// The quoted run the caret is in, or the next one on the line.
///
/// The quotes of a line are paired **in order**, which is the only reading that
/// does not need to know what an escape or a comment is: the second quote closes
/// the first, the fourth the third. Vim does the same, and it is what makes
/// `ci"` work from anywhere before the string as well as from inside it.
fn quote_object(text: &str, at: usize, around: bool, quote: char) -> Option<Range<usize>> {
    let (start, end) = (start_of_line(text, at), end_of_line(text, at));
    let quotes: Vec<usize> = text[start..end]
        .char_indices()
        .filter(|(_, c)| *c == quote)
        .map(|(i, _)| start + i)
        .collect();
    let mut pair = None;
    for [open, close] in quotes.as_chunks::<2>().0 {
        if at <= *close {
            pair = Some((*open, *close));
            break;
        }
    }
    let (open, close) = pair?;
    if around {
        return Some(open..next_boundary(text, close));
    }
    Some(next_boundary(text, open)..close)
}

/// `f`, `F`, `t` and `T`, which never leave the line.
fn find_in_line(
    text: &str,
    at: usize,
    ch: char,
    till: bool,
    backward: bool,
    count: usize,
) -> Option<usize> {
    let (start, end) = (start_of_line(text, at), end_of_line(text, at));
    let mut found = at;
    for _ in 0..count {
        if backward {
            let slice = &text[start..found];
            let hit = slice.rfind(ch)?;
            found = start + hit;
        } else {
            let from = next_boundary(text, found);
            if from > end {
                return None;
            }
            let hit = text[from..end].find(ch)?;
            found = from + hit;
        }
    }
    if !till {
        return Some(found);
    }
    Some(if backward {
        next_boundary(text, found)
    } else {
        prev_boundary_in(text, found, start)
    })
}

/// The next occurrence of `needle`, wrapping around the end of the file — which
/// is what vim does, and what makes `n` reach what is above the caret.
///
/// The comparison is `Ctrl+F`'s (smart case: an all-lowercase pattern ignores
/// case, a capital respects it), and deliberately so — the two searches share
/// one pattern (`Vim::set_search`), and `hlsearch` already lights occurrences
/// with that reckoning: a case-sensitive `n` skipped matches it had lit.
fn find_forward(text: &str, needle: &str, at: usize) -> Option<usize> {
    let from = next_boundary(text, at);
    if let Some(hit) = crate::ui::find::find_from(needle, text, from) {
        return Some(hit.start);
    }
    crate::ui::find::find_from(needle, text, 0).map(|hit| hit.start)
}

fn find_backward(text: &str, needle: &str, at: usize) -> Option<usize> {
    let hits = crate::ui::find::find_all(needle, text);
    hits.iter()
        .rev()
        .find(|hit| hit.end <= at)
        .or_else(|| hits.last())
        .map(|hit| hit.start)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The view, reduced to what a test needs: apply the edit, keep the caret.
    struct Editor {
        vim: Vim,
        text: String,
        cursor: usize,
    }

    impl Editor {
        fn new(text: &str) -> Self {
            Self {
                vim: Vim::default(),
                text: text.to_string(),
                cursor: 0,
            }
        }

        /// Types a string of keys. `\x1b` is Escape and `\n` is Enter, which is
        /// what the two look like coming out of the keyboard anyway.
        fn press(&mut self, keys: &str) -> &mut Self {
            for ch in keys.chars() {
                let key = match ch {
                    '\x1b' => Key {
                        ch: None,
                        name: "escape".into(),
                        ctrl: false,
                    },
                    '\n' => Key {
                        ch: None,
                        name: "enter".into(),
                        ctrl: false,
                    },
                    c => Key {
                        ch: Some(c),
                        name: c.to_string(),
                        ctrl: false,
                    },
                };
                self.apply(&key);
            }
            self
        }

        /// A selection made with the mouse: the editor holds it, and vim is
        /// told about it the way the surface tells it — the range, and which
        /// end was being dragged. The caret it leaves behind is the range's
        /// start, which is what `selected_range().start` answers.
        fn drag(&mut self, range: Range<usize>, reversed: bool) -> &mut Self {
            self.vim.adopt(&self.text, range.clone(), reversed);
            self.cursor = range.start;
            self
        }

        fn control(&mut self, name: &str) -> &mut Self {
            self.apply(&Key {
                ch: None,
                name: name.into(),
                ctrl: true,
            })
        }

        fn apply(&mut self, key: &Key) -> &mut Self {
            let response = self.vim.press(key, &self.text, self.cursor, 10);
            match response {
                Response::Apply(change) => {
                    if let Some(edit) = change.edit {
                        self.text.replace_range(edit.range, &edit.text);
                    }
                    self.cursor = change.head;
                }
                // Insert mode: the editor would have typed the character.
                Response::Ignored => {
                    let typed = key.ch.or_else(|| key.is("enter").then_some('\n'));
                    if let Some(ch) = typed {
                        self.text.insert(self.cursor, ch);
                        self.cursor += ch.len_utf8();
                    }
                }
                _ => {}
            }
            self
        }

        fn mode(&self) -> Mode {
            self.vim.mode()
        }
    }

    /// The caret is the first thing a mode changes, and `hjkl` is what a hand
    /// types without looking.
    #[test]
    fn the_home_row_walks_the_file() {
        let mut editor = Editor::new("one two\nthree\n");
        editor.press("ll");
        assert_eq!(editor.cursor, 2);
        editor.press("j");
        assert_eq!(&editor.text[editor.cursor..editor.cursor + 1], "r");
        editor.press("h");
        assert_eq!(&editor.text[editor.cursor..editor.cursor + 1], "h");
    }

    /// A closed fold is stepped over, not walked into: the lines it hides have
    /// no cursor to show, so `j` landing on one is a caret nobody can see.
    #[test]
    fn the_home_row_steps_over_a_closed_fold() {
        let mut editor = Editor::new(
            "# one
body
more
# two
tail
",
        );
        editor.vim.set_folds(vec![(0, 3)]);
        editor.press("j");
        assert_eq!(&editor.text[editor.cursor..editor.cursor + 1], "#", "# two");
        assert_eq!(line_index(&editor.text, editor.cursor), 3);
        editor.press("k");
        assert_eq!(line_index(&editor.text, editor.cursor), 0);
    }

    /// `$` sticks: the column it sets follows `j` down every line, however long
    /// they are — which is the whole reason vim keeps a desired column.
    #[test]
    fn the_end_of_line_column_sticks() {
        let mut editor = Editor::new("long line here\nab\nanother one\n");
        editor.press("$");
        assert_eq!(editor.cursor, 13);
        editor.press("j");
        assert_eq!(&editor.text[editor.cursor..editor.cursor + 1], "b");
        editor.press("j");
        assert_eq!(&editor.text[editor.cursor..editor.cursor + 1], "e");
    }

    /// Normal mode's caret sits **on** a character and never past the last one:
    /// that is what makes the block cursor and `x` agree.
    #[test]
    fn the_caret_stays_on_a_character() {
        let mut editor = Editor::new("ab\ncd\n");
        editor.press("$");
        assert_eq!(editor.cursor, 1);
        editor.press("x");
        assert_eq!(editor.text, "a\ncd\n");
        assert_eq!(editor.cursor, 0);
    }

    #[test]
    fn words_go_by_class_and_not_by_blanks() {
        let mut editor = Editor::new("foo.bar baz\n");
        editor.press("w");
        assert_eq!(editor.cursor, 3);
        editor.press("w");
        assert_eq!(editor.cursor, 4);
        editor.press("W");
        assert_eq!(editor.cursor, 8);
        editor.press("b");
        assert_eq!(editor.cursor, 4);
        editor.press("e");
        assert_eq!(editor.cursor, 6);
    }

    #[test]
    fn an_operator_takes_a_motion_and_a_count() {
        let mut editor = Editor::new("one two three four\n");
        editor.press("d2w");
        assert_eq!(editor.text, "three four\n");
        editor.press("dw");
        assert_eq!(editor.text, "four\n");
    }

    /// `dd` takes the newline with it, and the last line takes the one before
    /// it — otherwise deleting the last line leaves an empty one behind.
    #[test]
    fn deleting_a_line_takes_its_newline() {
        let mut editor = Editor::new("one\ntwo\nthree\n");
        editor.press("jdd");
        assert_eq!(editor.text, "one\nthree\n");
        assert_eq!(&editor.text[editor.cursor..editor.cursor + 1], "t");

        let mut editor = Editor::new("one\ntwo");
        editor.press("jdd");
        assert_eq!(editor.text, "one");
    }

    #[test]
    fn a_count_repeats_a_line_operator() {
        let mut editor = Editor::new("a\nb\nc\nd\n");
        editor.press("2dd");
        assert_eq!(editor.text, "c\nd\n");
    }

    /// `cc` empties the line but keeps its indentation: that is where the hand
    /// expects to carry on typing.
    #[test]
    fn changing_a_line_keeps_its_indentation() {
        let mut editor = Editor::new("    let x = 1;\nnext\n");
        editor.press("cc");
        assert_eq!(editor.mode(), Mode::Insert);
        assert_eq!(editor.text, "    \nnext\n");
        assert_eq!(editor.cursor, 4);
        editor.press("y\x1b");
        assert_eq!(editor.text, "    y\nnext\n");
        assert_eq!(editor.mode(), Mode::Normal);
    }

    #[test]
    fn yank_and_paste_know_whether_they_hold_lines() {
        let mut editor = Editor::new("one\ntwo\n");
        editor.press("yyp");
        assert_eq!(editor.text, "one\none\ntwo\n");
        assert_eq!(editor.cursor, 4);

        let mut editor = Editor::new("abc\n");
        editor.press("ylp");
        assert_eq!(editor.text, "aabc\n");
        assert_eq!(editor.cursor, 1);
    }

    #[test]
    fn opening_a_line_carries_the_indentation() {
        let mut editor = Editor::new("    one\n");
        editor.press("ox\x1b");
        assert_eq!(editor.text, "    one\n    x\n");

        let mut editor = Editor::new("    one\n");
        editor.press("Ox\x1b");
        assert_eq!(editor.text, "    x\n    one\n");
    }

    #[test]
    fn the_insertion_commands_land_where_vim_lands() {
        let mut editor = Editor::new("ab\n");
        editor.press("aX\x1b");
        assert_eq!(editor.text, "aXb\n");

        let mut editor = Editor::new("  ab\n");
        editor.press("IX\x1b");
        assert_eq!(editor.text, "  Xab\n");

        let mut editor = Editor::new("ab\n");
        editor.press("AX\x1b");
        assert_eq!(editor.text, "abX\n");
    }

    #[test]
    fn visual_mode_acts_on_its_selection() {
        let mut editor = Editor::new("hello world\n");
        editor.press("vlld");
        assert_eq!(editor.text, "lo world\n");
        assert_eq!(editor.mode(), Mode::Normal);

        let mut editor = Editor::new("one\ntwo\nthree\n");
        editor.press("Vjd");
        assert_eq!(editor.text, "three\n");
    }

    #[test]
    fn visual_yank_leaves_the_caret_at_the_start() {
        let mut editor = Editor::new("hello\n");
        editor.press("llvly");
        assert_eq!(editor.cursor, 2);
        editor.press("P");
        assert_eq!(editor.text, "hellllo\n");
    }

    /// `G` goes to the last line, `gg` to the first, and a count names one.
    #[test]
    fn the_goto_motions_count_lines() {
        let mut editor = Editor::new("a\nb\nc\nd\n");
        editor.press("G");
        assert_eq!(editor.cursor, 6);
        editor.press("gg");
        assert_eq!(editor.cursor, 0);
        editor.press("3G");
        assert_eq!(editor.cursor, 4);
    }

    #[test]
    fn find_stays_on_its_line() {
        let mut editor = Editor::new("a,b,c\nd,e\n");
        editor.press("f,");
        assert_eq!(editor.cursor, 1);
        editor.press("0");
        editor.press("2f,");
        assert_eq!(editor.cursor, 3);
        // Nothing further on the line: the caret does not move.
        editor.press("f;");
        assert_eq!(editor.cursor, 3);
        editor.press("t,");
        assert_eq!(editor.cursor, 3);
    }

    #[test]
    fn replacing_a_character_stays_on_the_line() {
        let mut editor = Editor::new("abc\n");
        editor.press("rx");
        assert_eq!(editor.text, "xbc\n");
        assert_eq!(editor.cursor, 0);
        editor.press("2rz");
        assert_eq!(editor.text, "zzc\n");
    }

    #[test]
    fn joining_puts_one_space_between_the_lines() {
        let mut editor = Editor::new("one\n  two\nthree\n");
        editor.press("J");
        assert_eq!(editor.text, "one two\nthree\n");
        assert_eq!(editor.cursor, 3);

        let mut editor = Editor::new("a\nb\nc\nd\n");
        editor.press("3J");
        assert_eq!(editor.text, "a b c\nd\n");
    }

    /// A search takes its pattern on a prompt line, and wraps around the end of
    /// the file — which is what makes `n` reach what is above the caret.
    #[test]
    fn a_search_wraps_around() {
        let mut editor = Editor::new("alpha\nbeta\nalpha\n");
        editor.press("/alpha\n");
        assert_eq!(editor.cursor, 11);
        editor.press("n");
        assert_eq!(editor.cursor, 0);
        editor.press("N");
        assert_eq!(editor.cursor, 11);
    }

    /// The search reads case as `Ctrl+F` does — smart case: an all-lowercase
    /// pattern ignores case, a capital respects it. The two share one pattern,
    /// and `hlsearch` already lights occurrences with that reckoning.
    #[test]
    fn a_lowercase_search_ignores_case_a_capital_respects_it() {
        let mut editor = Editor::new("x\nTodo here\ntodo there\n");
        editor.press("/todo\n");
        assert_eq!(editor.cursor, 2, "lowercase must reach the capitalised hit");
        editor.press("n");
        assert_eq!(editor.cursor, 12);
        editor.press("N");
        assert_eq!(editor.cursor, 2);

        let mut editor = Editor::new("x\nTodo here\ntodo there\n");
        editor.press("/Todo\n");
        assert_eq!(editor.cursor, 2);
        editor.press("n");
        assert_eq!(editor.cursor, 2, "a capital in the pattern is exact");
    }

    /// The prompt is shown while it is being typed: it is the only thing that
    /// says what a key that changes nothing is doing.
    #[test]
    fn the_prompt_reads_back_what_is_typed() {
        let mut editor = Editor::new("x\n");
        editor.press("/ab");
        assert_eq!(editor.vim.prompt().as_deref(), Some("/ab"));
        editor.press("\x1b");
        assert_eq!(editor.vim.prompt(), None);
    }

    #[test]
    fn the_command_line_carries_the_editors_own_gestures() {
        let mut vim = Vim::default();
        let text = "x\n";
        for ch in ":wq".chars() {
            vim.press(&key(ch), text, 0, 10);
        }
        assert_eq!(
            vim.press(&enter(), text, 0, 10),
            Response::Command(Command::SaveAndClose)
        );

        let mut vim = Vim::default();
        for ch in ":42".chars() {
            vim.press(&key(ch), text, 0, 10);
        }
        assert!(matches!(
            vim.press(&enter(), text, 0, 10),
            Response::Apply(_)
        ));
    }

    /// `gg` is a motion and `gd` a command: the same first key, and the parser
    /// must not settle on either before the second one arrives.
    #[test]
    fn the_g_prefix_tells_a_motion_from_a_jump() {
        let text = "one\ntwo\n";
        let mut vim = Vim::default();
        assert_eq!(vim.press(&key('g'), text, 4, 10), Response::Consumed);
        assert_eq!(
            vim.press(&key('d'), text, 4, 10),
            Response::Command(Command::GoToDefinition)
        );
        // And the motion still works, `gd` having cleared what was pending.
        let mut vim = Vim::default();
        vim.press(&key('g'), text, 4, 10);
        let Response::Apply(change) = vim.press(&key('g'), text, 4, 10) else {
            panic!("gg goes to the first line");
        };
        assert_eq!(change.head, 0);
    }

    /// `z` waits for its second key, and answers in both modes: none of what it
    /// opens touches the text.
    #[test]
    fn the_z_prefix_scrolls_and_folds() {
        let text = "one\ntwo\n";
        let mut vim = Vim::default();
        assert_eq!(vim.press(&key('z'), text, 0, 10), Response::Consumed);
        assert_eq!(
            vim.press(&key('z'), text, 0, 10),
            Response::Command(Command::Reveal(Reveal::Centre))
        );
        for (ch, expected) in [
            ('t', Command::Reveal(Reveal::Top)),
            ('b', Command::Reveal(Reveal::Bottom)),
            ('c', Command::Fold(Fold::Close)),
            ('o', Command::Fold(Fold::Open)),
            ('a', Command::Fold(Fold::Toggle)),
            ('M', Command::Fold(Fold::CloseAll)),
            ('R', Command::Fold(Fold::OpenAll)),
            ('m', Command::Fold(Fold::More)),
            ('r', Command::Fold(Fold::Less)),
        ] {
            let mut vim = Vim::default();
            vim.press(&key('z'), text, 0, 10);
            assert_eq!(
                vim.press(&key(ch), text, 0, 10),
                Response::Command(expected),
                "z{ch}"
            );
        }
        // Visual mode answers the same, and `z` on its own leaves no pending
        // keys behind for the next command to trip over.
        let mut vim = Vim::default();
        vim.press(&key('v'), text, 0, 10);
        vim.press(&key('z'), text, 0, 10);
        assert_eq!(
            vim.press(&key('z'), text, 0, 10),
            Response::Command(Command::Reveal(Reveal::Centre))
        );
        assert_eq!(vim.pending(), "");
    }

    /// `Ctrl+E` and `Ctrl+Y` move the page, not the caret — and `Ctrl+Y` is
    /// therefore not the editor's redo any more, which is why `Ctrl+R` has to
    /// reach us.
    #[test]
    fn the_control_keys_scroll_a_line_at_a_time() {
        let text = "one\ntwo\nthree\n";
        let ctrl = |name: &str| Key {
            ch: None,
            name: name.into(),
            ctrl: true,
        };
        let mut vim = Vim::default();
        assert_eq!(
            vim.press(&ctrl("e"), text, 0, 10),
            Response::Command(Command::Scroll(1))
        );
        assert_eq!(
            vim.press(&ctrl("y"), text, 0, 10),
            Response::Command(Command::Scroll(-1))
        );
        assert_eq!(
            vim.press(&ctrl("r"), text, 0, 10),
            Response::Command(Command::Redo)
        );
    }

    #[test]
    fn undo_is_handed_back_to_the_editor() {
        let mut vim = Vim::default();
        assert_eq!(
            vim.press(&key('u'), "x\n", 0, 10),
            Response::Command(Command::Undo)
        );
    }

    /// Insert mode belongs to the editor: everything but `Esc` goes through.
    #[test]
    fn insert_mode_hands_the_keys_over() {
        let mut vim = Vim::default();
        vim.press(&key('i'), "ab\n", 0, 10);
        assert_eq!(vim.mode(), Mode::Insert);
        assert_eq!(vim.press(&key('z'), "ab\n", 0, 10), Response::Ignored);
        let Response::Apply(change) = vim.press(&escape(), "zab\n", 1, 10) else {
            panic!("Escape leaves insert mode");
        };
        // The caret steps back onto the last character typed.
        assert_eq!(change.head, 0);
        assert_eq!(vim.mode(), Mode::Normal);
    }

    /// The block cursor is a selection of one character — and a character is not
    /// a byte, or an accented file would have it cut in half.
    #[test]
    fn the_block_cursor_covers_one_character() {
        let mut vim = Vim::default();
        let text = "éa\n";
        let Response::Apply(change) = vim.press(&key('l'), text, 0, 10) else {
            panic!("a motion applies");
        };
        assert_eq!(change.selection, 2..3);
        let Response::Apply(change) = vim.press(&key('h'), text, 2, 10) else {
            panic!("a motion applies");
        };
        assert_eq!(change.selection, 0..2);
    }

    /// A copy is the one gesture that changes nothing: it says so by lighting
    /// what it took, and a `d` has nothing left to light.
    #[test]
    fn a_yank_asks_for_its_flash_and_a_delete_does_not() {
        let mut vim = Vim::default();
        let text = "let x = 1;\n";
        vim.press(&key('y'), text, 0, 10);
        let Response::Apply(change) = vim.press(&key('w'), text, 0, 10) else {
            panic!("an operator with its motion applies");
        };
        assert_eq!(change.flash, vec![0..4]);
        let mut vim = Vim::default();
        vim.press(&key('y'), text, 0, 10);
        let Response::Apply(change) = vim.press(&key('y'), text, 0, 10) else {
            panic!("a doubled operator applies");
        };
        assert_eq!(change.flash, vec![0..10]);
        let mut vim = Vim::default();
        vim.press(&key('d'), text, 0, 10);
        let Response::Apply(change) = vim.press(&key('w'), text, 0, 10) else {
            panic!("an operator with its motion applies");
        };
        assert!(change.flash.is_empty());
    }

    /// The cursor is there before the first keystroke, which is what a file
    /// that has just opened shows.
    #[test]
    fn the_cursor_is_painted_without_a_keystroke() {
        let vim = Vim::default();
        assert_eq!(vim.cursor("let x = 1;\n", 0), Some(0..1));
        // And it follows the caret, which is where a click of the mouse left it.
        assert_eq!(vim.cursor("let x = 1;\n", 4), Some(4..5));
    }

    /// Nothing to paint over on an empty line: the view gives the caret back
    /// rather than showing no cursor at all.
    #[test]
    fn an_empty_line_has_no_block_to_paint() {
        let vim = Vim::default();
        assert_eq!(vim.cursor("\nabc", 0), Some(0..0));
        assert_eq!(vim.cursor("abc", 3), Some(2..3));
    }

    /// Insert mode leaves the cursor to the editor, whose caret is a line.
    #[test]
    fn insert_mode_has_no_block_cursor() {
        let mut vim = Vim::default();
        vim.press(&key('i'), "abc\n", 0, 10);
        assert_eq!(vim.mode(), Mode::Insert);
        assert_eq!(vim.cursor("abc\n", 0), None);
    }

    #[test]
    fn deleting_an_accented_character_cuts_on_a_boundary() {
        let mut editor = Editor::new("éàü\n");
        editor.press("x");
        assert_eq!(editor.text, "àü\n");
        editor.press("dw");
        assert_eq!(editor.text, "\n");
    }

    /// A half page is half of what the viewport shows, and the file's last line
    /// is as far as it goes.
    #[test]
    fn the_half_page_moves_by_the_viewport() {
        let mut editor = Editor::new("1\n2\n3\n4\n5\n6\n7\n8\n");
        editor.control("d");
        assert_eq!(editor.cursor, 10);
        editor.control("u");
        assert_eq!(editor.cursor, 0);
    }

    /// A count under way swallows the digits, and the pending keys are what
    /// says why nothing has happened yet.
    #[test]
    fn a_pending_command_says_so() {
        let mut vim = Vim::default();
        assert_eq!(vim.press(&key('2'), "abc\n", 0, 10), Response::Consumed);
        assert_eq!(vim.pending(), "2");
        assert_eq!(vim.press(&key('d'), "abc\n", 0, 10), Response::Consumed);
        assert_eq!(vim.pending(), "2d");
        assert!(matches!(
            vim.press(&key('l'), "abc\n", 0, 10),
            Response::Apply(_)
        ));
        assert_eq!(vim.pending(), "");
    }

    /// The block cursor survives an edit: it is computed on the text the edit
    /// leaves behind, not on the one it was computed from.
    #[test]
    fn the_block_cursor_comes_back_after_an_edit() {
        let mut vim = Vim::default();
        let Response::Apply(change) = vim.press(&key('x'), "abc\n", 0, 10) else {
            panic!("x deletes");
        };
        assert_eq!(change.selection, 0..1);
        // Insert mode has a caret and not a block.
        let Response::Apply(change) = vim.press(&key('s'), "bc\n", 0, 10) else {
            panic!("s deletes and inserts");
        };
        assert_eq!(change.selection, 0..0);
    }

    /// The text objects are what a hand reaches for far more often than a
    /// count: a word, the inside of a call, the inside of a string.
    #[test]
    fn a_word_object_stops_at_the_line() {
        let mut editor = Editor::new("one two three\nfour\n");
        editor.press("wdiw");
        assert_eq!(editor.text, "one  three\nfour\n");

        // `aw` takes the blanks that follow the word.
        let mut editor = Editor::new("one two three\n");
        editor.press("wdaw");
        assert_eq!(editor.text, "one three\n");

        // The last word of a line has no blanks after it: the ones before go.
        let mut editor = Editor::new("one two\nnext\n");
        editor.press("wdaw");
        assert_eq!(editor.text, "one\nnext\n");
    }

    #[test]
    fn a_bracket_object_counts_its_nesting() {
        let mut editor = Editor::new("call(one, two(x), three)\n");
        // From inside the outer call, and from anywhere in it.
        editor.press("wdi(");
        assert_eq!(editor.text, "call()\n");

        let mut editor = Editor::new("call(one, two(x), three)\n");
        editor.press("fxda(");
        assert_eq!(editor.text, "call(one, two, three)\n");

        let mut editor = Editor::new("{ a: [1, 2] }\n");
        editor.press("di{");
        assert_eq!(editor.text, "{}\n");
    }

    /// A line's quotes are paired in order — the second closes the first — which
    /// is the only reading that needs to know nothing of escapes or comments.
    #[test]
    fn a_quote_object_pairs_in_order() {
        let mut editor = Editor::new("let x = \"hello\";\n");
        editor.press("ci\"");
        assert_eq!(editor.text, "let x = \"\";\n");
        assert_eq!(editor.mode(), Mode::Insert);

        // From inside the second string, the second pair is the one taken.
        let mut editor = Editor::new("let x = 'a' + 'b';\n");
        editor.press("fbda'");
        assert_eq!(editor.text, "let x = 'a' + ;\n");
    }

    /// Visual mode takes an object too, and a `V` that meets one comes back down
    /// to a characterwise selection — the object is not a line.
    #[test]
    fn visual_mode_selects_an_object() {
        let mut editor = Editor::new("one two three\n");
        editor.press("wviwd");
        assert_eq!(editor.text, "one  three\n");

        let mut editor = Editor::new("call(one)\n");
        editor.press("f(Vi(y");
        assert_eq!(editor.vim.mode(), Mode::Normal);
        editor.press("$p");
        assert_eq!(editor.text, "call(one)one\n");
    }

    /// What a keystroke tore out is reported, so that the view can put it on the
    /// system clipboard when the setting asks for it.
    #[test]
    fn what_was_taken_is_reported() {
        let mut vim = Vim::default();
        let text = "one two\n";
        vim.press(&key('d'), text, 0, 10);
        let Response::Apply(change) = vim.press(&key('w'), text, 0, 10) else {
            panic!("dw deletes");
        };
        let yank = change.yank.expect("a delete fills the register");
        assert_eq!(yank.text, "one ");
        assert!(!yank.linewise);

        // A motion takes nothing, and says so.
        let Response::Apply(change) = vim.press(&key('l'), "two\n", 0, 10) else {
            panic!("a motion applies");
        };
        assert_eq!(change.yank, None);
    }

    /// A register handed in from the outside carries no linewise flag: the
    /// clipboard holds text and nothing else, so the newline decides.
    #[test]
    fn a_clipboard_register_reads_its_shape_off_the_text() {
        let mut editor = Editor::new("one\ntwo\n");
        editor.vim.set_register("added\n".into());
        editor.press("p");
        assert_eq!(editor.text, "one\nadded\ntwo\n");

        let mut editor = Editor::new("one\n");
        editor.vim.set_register("X".into());
        editor.press("p");
        assert_eq!(editor.text, "oXne\n");
    }

    /// A yank is the one gesture that changes nothing on screen: the range it
    /// took is reported so that the view can light it up. A delete has taken the
    /// text away and has nothing left to light.
    #[test]
    fn a_yank_reports_what_to_flash() {
        let mut vim = Vim::default();
        let text = "one two\nthree\n";
        vim.press(&key('y'), text, 0, 10);
        let Response::Apply(change) = vim.press(&key('w'), text, 0, 10) else {
            panic!("yw copies");
        };
        assert_eq!(change.flash, vec![0..4]);

        // A whole line, without its newline.
        let mut vim = Vim::default();
        vim.press(&key('y'), text, 0, 10);
        let Response::Apply(change) = vim.press(&key('y'), text, 0, 10) else {
            panic!("yy copies");
        };
        assert_eq!(change.flash, vec![0..7]);

        let mut vim = Vim::default();
        vim.press(&key('d'), text, 0, 10);
        let Response::Apply(change) = vim.press(&key('w'), text, 0, 10) else {
            panic!("dw deletes");
        };
        assert!(change.flash.is_empty());

        // And a motion flashes nothing at all.
        let mut vim = Vim::default();
        let Response::Apply(change) = vim.press(&key('l'), text, 0, 10) else {
            panic!("a motion applies");
        };
        assert!(change.flash.is_empty());
    }

    /// A rectangle is what `Ctrl+V` selects, and what it cuts is the same
    /// columns of every line it covers — never the lines themselves.
    #[test]
    fn a_block_is_cut_out_of_every_line_it_covers() {
        let mut editor = Editor::new("one\ntwo\nthree\n");
        editor.control("v");
        assert_eq!(editor.mode(), Mode::VisualBlock);
        editor.press("jl").press("d");
        assert_eq!(editor.text, "e\no\nthree\n");
        assert_eq!(editor.mode(), Mode::Normal);
        // And the same key again is the way out of the mode.
        editor.control("v");
        assert_eq!(editor.mode(), Mode::VisualBlock);
        editor.control("v");
        assert_eq!(editor.mode(), Mode::Normal);
    }

    /// `Ctrl+V` then `I` is what one comes to this mode for: a prefix on every
    /// line at once, written by the `Esc` that ends the insertion.
    #[test]
    fn a_block_insertion_repeats_down_the_rows() {
        let mut editor = Editor::new("a\nb\nc\n");
        editor.control("v");
        editor.press("jjI// \x1b");
        assert_eq!(editor.text, "// a\n// b\n// c\n");
        assert_eq!(editor.mode(), Mode::Normal);

        // A newline gives the repeat up rather than write something nobody
        // typed — which is what vim does with it too.
        let mut editor = Editor::new("a\nb\n");
        editor.control("v");
        editor.press("jIx\ny\x1b");
        assert_eq!(editor.text, "x\nya\nb\n");
    }

    /// `A` goes past the right edge of the block, and pads out a line too short
    /// to reach it; `I` skips such a line, as vim does.
    #[test]
    fn appending_to_a_block_pads_a_short_line() {
        let mut editor = Editor::new("ab\nc\nde\n");
        editor.control("v");
        editor.press("ljjA!\x1b");
        assert_eq!(editor.text, "ab!\nc !\nde!\n");

        let mut editor = Editor::new("ab\nc\nde\n");
        editor.press("l").control("v");
        editor.press("jjI!\x1b");
        assert_eq!(editor.text, "a!b\nc\nd!e\n");
    }

    /// `$` sticks here as it does everywhere: the block reaches the end of every
    /// line it crosses, however long each one is.
    #[test]
    fn the_dollar_block_reaches_the_end_of_every_line() {
        let mut editor = Editor::new("long\nab\n");
        editor.control("v");
        editor.press("$j").press("d");
        assert_eq!(editor.text, "\n\n");
    }

    /// A blockwise copy takes the rows one under the other, and lights each of
    /// them: the span between them is not what was taken.
    #[test]
    fn a_blockwise_yank_takes_the_rows_and_lights_them() {
        let mut vim = Vim::default();
        let text = "one\ntwo\nthree\n";
        vim.press(&control("v"), text, 0, 10);
        vim.press(&key('j'), text, 0, 10);
        vim.press(&key('l'), text, 0, 10);
        assert_eq!(vim.block_selection(text), vec![0..2, 4..6]);
        let Response::Apply(change) = vim.press(&key('y'), text, 0, 10) else {
            panic!("a blockwise yank copies");
        };
        assert_eq!(change.flash, vec![0..2, 4..6]);
        assert_eq!(
            change.yank,
            Some(Register {
                text: "on\ntw".into(),
                linewise: false,
            })
        );
        // Nothing paints a rectangle outside the mode that has one.
        assert!(vim.block_selection(text).is_empty());
    }

    /// `r` fills the rectangle, and only the rectangle.
    #[test]
    fn replacing_a_block_fills_its_columns() {
        let mut editor = Editor::new("one\ntwo\nthree\n");
        editor.control("v");
        editor.press("jl").press("rX");
        assert_eq!(editor.text, "XXe\nXXo\nthree\n");
    }

    fn control(name: &str) -> Key {
        Key {
            ch: None,
            name: name.into(),
            ctrl: true,
        }
    }

    /// `Enter` in normal mode goes down a line, to its first non-blank — vim's
    /// `+`. It is claimed rather than left to the editor, which would open a
    /// line in a mode that does not type.
    #[test]
    fn enter_goes_down_a_line() {
        let mut editor = Editor::new("one\n    two\nthree\n");
        editor.press("\n");
        assert_eq!(editor.text, "one\n    two\nthree\n", "nothing was typed");
        assert_eq!(editor.cursor, 8, "the `t` of two");
    }

    /// `Backspace` steps back over a line break, where `h` stops at the start of
    /// the line — and above all it does **not** delete: the editor binds that
    /// key, and left to it, normal mode destroyed text.
    #[test]
    fn backspace_steps_back_without_deleting() {
        let mut editor = Editor::new("ab\ncd\n");
        editor.press("j");
        assert_eq!(editor.cursor, 3);
        let key = Key {
            ch: None,
            name: "backspace".into(),
            ctrl: false,
        };
        editor.apply(&key);
        assert_eq!(editor.text, "ab\ncd\n");
        assert_eq!(editor.cursor, 1, "the last character of the line above");
    }

    /// `;` walks the line on the last `f`, and `,` walks it back: without them
    /// a search within the line is worth typing once, which is not what it is
    /// for.
    #[test]
    fn a_find_is_walked_with_semicolons() {
        let mut editor = Editor::new("a.b.c.d\n");
        editor.press("f.");
        assert_eq!(editor.cursor, 1);
        editor.press(";");
        assert_eq!(editor.cursor, 3);
        editor.press(";");
        assert_eq!(editor.cursor, 5);
        editor.press(",");
        assert_eq!(editor.cursor, 3);
    }

    /// A `t` played again steps over the character it stopped short of: the
    /// caret is already where "up to the next one" would leave it, so a repeat
    /// that did not step first would sit still.
    #[test]
    fn a_till_played_again_gets_past_its_character() {
        let mut editor = Editor::new("a.b.c.d\n");
        editor.press("t.");
        assert_eq!(editor.cursor, 0);
        editor.press(";");
        assert_eq!(editor.cursor, 2);
        editor.press(";");
        assert_eq!(editor.cursor, 4);
        editor.press(",");
        assert_eq!(
            editor.cursor, 2,
            "it steps over the one just behind it, too"
        );
    }

    /// An operator takes `;` as it takes any motion: `d;` deletes up to the next
    /// occurrence, count and all.
    #[test]
    fn an_operator_can_aim_with_a_semicolon() {
        let mut editor = Editor::new("a.b.c.d\n");
        editor.press("f.");
        editor.press("d;");
        assert_eq!(editor.text, "ac.d\n", "`;` is inclusive, as `f` is");
    }

    /// `/` and `n`: the search line is typed, and `Enter` runs it.
    #[test]
    fn a_search_line_is_typed_and_run() {
        let mut editor = Editor::new("one two\nthree two\n");
        editor.press("/two\n");
        assert_eq!(editor.cursor, 4);
        editor.press("n");
        assert_eq!(editor.cursor, 14);
        editor.press("N");
        assert_eq!(editor.cursor, 4);
    }

    /// The occurrences stay lit after a search, and `Esc` puts them out: a
    /// search that only moved the caret left one pressing `n` to find out
    /// whether there was anything else.
    #[test]
    fn a_search_lights_what_it_found() {
        let mut editor = Editor::new("one two\nthree two\n");
        assert_eq!(editor.vim.highlights(), None);
        editor.press("/two\n");
        assert_eq!(editor.vim.highlights(), Some("two"));
        editor.press("\x1b");
        assert_eq!(editor.vim.highlights(), None, "`Esc` is `:nohlsearch`");
        // The pattern is not forgotten with the light: `n` still walks it.
        editor.press("n");
        assert_eq!(editor.cursor, 14);
    }

    /// One pattern for both searches: what was typed into `Ctrl+F` is what `n`
    /// carries on, without anything being typed twice.
    #[test]
    fn the_bar_and_the_slash_share_one_pattern() {
        let mut editor = Editor::new("one two\nthree two\n");
        editor.vim.set_search("two");
        assert_eq!(editor.vim.highlights(), Some("two"));
        editor.press("n");
        assert_eq!(editor.cursor, 4);
        editor.press("n");
        assert_eq!(editor.cursor, 14);
    }

    /// What is being typed on a `/` or `:` line is what the bar shows, and it is
    /// the only thing that says why the next key does nothing else.
    #[test]
    fn the_line_being_typed_is_readable() {
        let mut editor = Editor::new("one\n");
        editor.press("/on");
        assert_eq!(editor.vim.prompt().as_deref(), Some("/on"));
        editor.press("\x1b");
        assert_eq!(editor.vim.prompt(), None);
    }

    /// `:w`, `:wq` and `:q` are asked of the application: vim has no file to
    /// write and no tab to close.
    #[test]
    fn the_colon_line_asks_the_application() {
        let mut editor = Editor::new("one\n");
        for (line, wanted) in [
            (":w\n", Command::Save),
            (":q!\n", Command::Close),
            (":wq\n", Command::SaveAndClose),
            (":x\n", Command::SaveAndClose),
        ] {
            let mut response = Response::Consumed;
            for ch in line.chars() {
                let key = match ch {
                    '\n' => enter(),
                    c => key(c),
                };
                response = editor.vim.press(&key, &editor.text, editor.cursor, 10);
            }
            assert_eq!(response, Response::Command(wanted), "{line:?}");
        }
    }

    /// `:42` is `42G`: a line number is the one thing a colon line answers
    /// itself.
    #[test]
    fn a_colon_line_number_goes_to_it() {
        let mut editor = Editor::new("one\ntwo\nthree\n");
        editor.press(":3\n");
        assert_eq!(line_index(&editor.text, editor.cursor), 2);
    }

    /// `.` plays the last change again, where the caret is now — the key that
    /// makes an operator worth typing once.
    #[test]
    fn the_last_change_happens_again() {
        let mut editor = Editor::new("one two three four\n");
        editor.press("dw");
        assert_eq!(editor.text, "two three four\n");
        editor.press(".");
        assert_eq!(editor.text, "three four\n");
    }

    /// A repeat is not itself a change: `.` twice is the command twice, and not
    /// the second `.` playing the first.
    #[test]
    fn a_repeat_is_not_what_gets_repeated() {
        let mut editor = Editor::new("abcdef\n");
        editor.press("x..");
        assert_eq!(editor.text, "def\n");
    }

    /// What was typed in insert mode goes with the command: `cw` is the half one
    /// sees, and the word one typed is the other.
    #[test]
    fn a_change_carries_what_was_typed() {
        let mut editor = Editor::new("aa\nbb\n");
        editor.press("cwyes\x1b");
        assert_eq!(editor.text, "yes\nbb\n");
        editor.press("j0.");
        assert_eq!(editor.text, "yes\nyes\n");
    }

    /// An insertion that opens a line is played whole — the line it opens as
    /// much as what goes on it.
    #[test]
    fn an_opened_line_is_played_whole() {
        let mut editor = Editor::new("one\ntwo\n");
        editor.press("oand\x1b");
        assert_eq!(editor.text, "one\nand\ntwo\n");
        editor.press(".");
        assert_eq!(editor.text, "one\nand\nand\ntwo\n");
    }

    /// Moving about does not disturb what `.` holds: only a command that
    /// changed the text becomes the last change.
    #[test]
    fn a_motion_is_not_a_change() {
        let mut editor = Editor::new("aaa bbb ccc\n");
        editor.press("x");
        editor.press("ww");
        editor.press(".");
        assert_eq!(editor.text, "aa bbb cc\n");
    }

    /// One `Edit` and not the six keystrokes it took: that is what makes a
    /// repeat a single transaction, and what keeps `u` from having to be pressed
    /// as many times as one typed.
    #[test]
    fn a_repeat_is_one_edit() {
        let mut editor = Editor::new("aa\nbb\n");
        editor.press("cwyes\x1b");
        editor.press("j0");
        let key = key('.');
        let response = editor.vim.press(&key, &editor.text, editor.cursor, 10);
        let Response::Apply(change) = response else {
            panic!("a repeat applies");
        };
        let edit = change.edit.expect("something changed");
        assert_eq!(&editor.text[edit.range.clone()], "bb");
        assert_eq!(edit.text, "yes");
    }

    /// In insert mode a full stop is a full stop: `.` is a command of normal
    /// mode alone, and the interception has to know it.
    #[test]
    fn a_full_stop_is_typed_where_it_belongs() {
        let mut editor = Editor::new("");
        editor.press("ione. two.\x1b");
        assert_eq!(editor.text, "one. two.");
    }

    /// Nothing has changed yet: `.` is taken and does nothing, rather than
    /// answering with an edit of nothing at all.
    #[test]
    fn a_repeat_with_nothing_to_repeat_is_quiet() {
        let mut editor = Editor::new("one\n");
        editor.press(".");
        assert_eq!(editor.text, "one\n");
        assert_eq!(editor.cursor, 0);
    }

    /// A blockwise insertion replays whole, its `Esc` included: that `Esc` is
    /// what writes the typing down the other rows, so a repeat that set the mode
    /// by hand would play the top line and none of the rest.
    #[test]
    fn a_rectangle_replays_with_its_escape() {
        let mut editor = Editor::new("aa\nbb\ncc\ndd\n");
        editor.control("v").press("jI-\x1b");
        assert_eq!(editor.text, "-aa\n-bb\ncc\ndd\n");
        editor.press("jj0.");
        assert_eq!(editor.text, "-aa\n-bb\n-cc\n-dd\n");
    }

    /// A keyed visual selection replays as the keys it was made of: `v`, its
    /// motion and its operator are one command.
    #[test]
    fn a_visual_command_replays_as_its_keys() {
        let mut editor = Editor::new("abcd\nefgh\n");
        editor.press("vld");
        assert_eq!(editor.text, "cd\nefgh\n");
        editor.press("j0.");
        assert_eq!(editor.text, "cd\ngh\n");
    }

    /// A change made on a **dragged** selection is not what `.` plays: there are
    /// no keystrokes that describe a drag, and `d` on its own would be a repeat
    /// that waits for a motion. What was filed before it stays.
    #[test]
    fn a_dragged_change_is_not_filed() {
        let mut editor = Editor::new("aaa bbb ccc\n");
        editor.press("x");
        editor.drag(2..5, false);
        editor.press("d");
        assert_eq!(editor.text, "aab ccc\n");
        editor.press(".");
        assert_eq!(editor.text, "aa ccc\n", "the `x` before it");
    }

    /// A selection made with the mouse is a visual selection: without that,
    /// what was lit on screen and what the next operator took were two
    /// different things — the mode stayed normal, the caret sat at the start of
    /// the range, and `d` deleted the one character under it.
    #[test]
    fn a_dragged_selection_is_a_visual_selection() {
        let mut editor = Editor::new("one two three\n");
        editor.drag(4..7, false);
        assert_eq!(editor.mode(), Mode::Visual);
        editor.press("d");
        assert_eq!(editor.text, "one  three\n");
        assert_eq!(editor.mode(), Mode::Normal);
    }

    /// `c` on a dragged selection replaces it and leaves insert mode behind,
    /// which is the gesture one reaches for after selecting with the mouse.
    #[test]
    fn a_dragged_selection_can_be_changed() {
        let mut editor = Editor::new("one two three\n");
        editor.drag(4..7, false);
        editor.press("c");
        assert_eq!(editor.mode(), Mode::Insert);
        assert_eq!(editor.text, "one  three\n");
        editor.press("six");
        assert_eq!(editor.text, "one six three\n");
    }

    /// The head is the end that was being dragged: the block cursor goes there,
    /// and it is what `o`, `h` and `l` carry on from. A selection grown
    /// leftwards has it at the **start**.
    #[test]
    fn the_dragged_end_is_the_head() {
        let mut editor = Editor::new("one two three\n");
        editor.drag(4..7, false);
        assert_eq!(editor.vim.head(), 6, "the last character of the range");
        editor.drag(4..7, true);
        assert_eq!(editor.vim.head(), 4);
        // `o` swaps the ends, and the range it covers does not move.
        editor.press("o");
        assert_eq!(editor.vim.head(), 6);
        editor.press("d");
        assert_eq!(editor.text, "one  three\n");
    }

    /// A plain click selects nothing, and nothing is what visual mode covers:
    /// it puts the surface back in normal mode rather than leaving a selection
    /// on screen that has already gone.
    #[test]
    fn a_click_leaves_visual_mode() {
        let mut editor = Editor::new("one two three\n");
        editor.drag(4..7, false);
        editor.drag(9..9, false);
        assert_eq!(editor.mode(), Mode::Normal);
        editor.press("x");
        assert_eq!(editor.text, "one two tree\n");
    }

    /// Insert mode keeps the mouse to itself: selecting a word to type over it
    /// is what every editor does, and vim has no visual mode to go to from
    /// there.
    #[test]
    fn insert_mode_keeps_its_own_selection() {
        let mut editor = Editor::new("one two three\n");
        editor.press("i");
        editor.drag(4..7, false);
        assert_eq!(editor.mode(), Mode::Insert);
    }

    fn key(ch: char) -> Key {
        Key {
            ch: Some(ch),
            name: ch.to_string(),
            ctrl: false,
        }
    }

    fn escape() -> Key {
        Key {
            ch: None,
            name: "escape".into(),
            ctrl: false,
        }
    }

    fn enter() -> Key {
        Key {
            ch: None,
            name: "enter".into(),
            ctrl: false,
        }
    }
}
