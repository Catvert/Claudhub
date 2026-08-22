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
}

impl Mode {
    /// The i18n key of the name the toolbar shows.
    pub fn key(self) -> &'static str {
        match self {
            Mode::Normal => "vim-mode-normal",
            Mode::Insert => "vim-mode-insert",
            Mode::Visual => "vim-mode-visual",
            Mode::VisualLine => "vim-mode-visual-line",
        }
    }
}

/// One keystroke, reduced to what vim cares about.
///
/// `ch` is the character the keystroke **produced** and not the key it was
/// pressed on: that is what makes `$`, `^` and `0` land where they should on an
/// AZERTY keyboard, where they are shifted or in the numeric row.
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
    /// The range a **copy** left in place, for the view to flash.
    ///
    /// A yank is the one gesture of vim that changes nothing on screen: without
    /// a sign, one is never sure it took. It is what vim-highlightedyank exists
    /// for, and it is `Some` for a yank only — a delete has taken the text away,
    /// and there is nothing left to light up.
    pub flash: Option<Range<usize>>,
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
    /// The range it copied without touching it, waiting for the same.
    flashed: Option<Range<usize>>,
    prompt: Option<Prompt>,
    last_search: String,
    search_backward: bool,
}

impl Vim {
    pub fn mode(&self) -> Mode {
        self.mode
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
    pub fn press(&mut self, key: &Key, text: &str, cursor: usize, rows: usize) -> Response {
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
                // vim steps back onto the last character typed.
                let head = clamp_to_line(text, prev_boundary(text, cursor.min(text.len())));
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
            return Response::Ignored;
        };
        self.pending.push(ch);
        self.run(text)
    }

    /// `Esc` in normal mode drops what was being typed; in visual mode it drops
    /// the selection.
    fn escape(&mut self, text: &str) -> Response {
        self.pending.clear();
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
            // `Ctrl+R` never reaches us — the window binds it to a refresh — so
            // redo is also on the editor's own `Ctrl+Y`. Kept all the same for
            // the day that binding moves.
            "r" => Response::Command(Command::Redo),
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
            if self.mode == Mode::VisualLine {
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
                self.flashed = Some(range.clone());
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
            let end = advance(text, self.head, n).min(end_of_line(text, self.head));
            if end <= self.head {
                return Response::Consumed;
            }
            let replacement: String = std::iter::repeat_n(rest[1], n).collect();
            return Response::Apply(self.edit(text, self.head..end, replacement, self.head));
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
            'v' => self.enter_visual(text, Mode::Visual),
            'V' => self.enter_visual(text, Mode::VisualLine),
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
            'v' => {
                self.mode = if self.mode == Mode::Visual {
                    Mode::Normal
                } else {
                    Mode::Visual
                };
                Response::Apply(self.landing(text, self.head))
            }
            'V' => {
                self.mode = if self.mode == Mode::VisualLine {
                    Mode::Normal
                } else {
                    Mode::VisualLine
                };
                Response::Apply(self.landing(text, self.head))
            }
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

    fn enter_visual(&mut self, text: &str, mode: Mode) -> Response {
        self.mode = mode;
        self.anchor = self.head;
        Response::Apply(self.landing(text, self.head))
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
            flash: self.flashed.take(),
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
            flash: self.flashed.take(),
            yank: self.yanked.take(),
        }
    }

    /// The block cursor, or the visual selection.
    fn selection(&self, text: &str) -> Range<usize> {
        match self.mode {
            Mode::Insert => self.head..self.head,
            Mode::Normal => block(text, self.head),
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
                return Some(Target {
                    offset: move_lines(text, head, delta, Some(column)),
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
            Motion::Find { ch, till, backward } => {
                self.column = None;
                let found = find_in_line(text, head, ch, till, backward, count)?;
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
    Find {
        ch: char,
        till: bool,
        backward: bool,
    },
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
            Parsed::Motion(Motion::Find {
                ch: keys[1],
                till: c == 't' || c == 'T',
                backward: c == 'F' || c == 'T',
            })
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
    for couple in quotes.chunks_exact(2) {
        let (open, close) = (couple[0], couple[1]);
        if at <= close {
            pair = Some((open, close));
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
fn find_forward(text: &str, needle: &str, at: usize) -> Option<usize> {
    let from = next_boundary(text, at);
    if let Some(hit) = text.get(from..).and_then(|rest| rest.find(needle)) {
        return Some(from + hit);
    }
    text.find(needle)
}

fn find_backward(text: &str, needle: &str, at: usize) -> Option<usize> {
    if let Some(hit) = text[..at.min(text.len())].rfind(needle) {
        return Some(hit);
    }
    text.rfind(needle)
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
                    if let Some(ch) = key.ch {
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
        assert_eq!(change.flash, Some(0..4));
        let mut vim = Vim::default();
        vim.press(&key('y'), text, 0, 10);
        let Response::Apply(change) = vim.press(&key('y'), text, 0, 10) else {
            panic!("a doubled operator applies");
        };
        assert_eq!(change.flash, Some(0..10));
        let mut vim = Vim::default();
        vim.press(&key('d'), text, 0, 10);
        let Response::Apply(change) = vim.press(&key('w'), text, 0, 10) else {
            panic!("an operator with its motion applies");
        };
        assert_eq!(change.flash, None);
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
        assert_eq!(change.flash, Some(0..4));

        // A whole line, without its newline.
        let mut vim = Vim::default();
        vim.press(&key('y'), text, 0, 10);
        let Response::Apply(change) = vim.press(&key('y'), text, 0, 10) else {
            panic!("yy copies");
        };
        assert_eq!(change.flash, Some(0..7));

        let mut vim = Vim::default();
        vim.press(&key('d'), text, 0, 10);
        let Response::Apply(change) = vim.press(&key('w'), text, 0, 10) else {
            panic!("dw deletes");
        };
        assert_eq!(change.flash, None);

        // And a motion flashes nothing at all.
        let mut vim = Vim::default();
        let Response::Apply(change) = vim.press(&key('l'), text, 0, 10) else {
            panic!("a motion applies");
        };
        assert_eq!(change.flash, None);
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
