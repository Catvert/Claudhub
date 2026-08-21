//! Mouse reporting.
//!
//! A full-screen program can ask to **receive** the mouse rather than let the
//! terminal do what it likes with it: that is how a list is picked by click
//! and a pane scrolls by wheel. It announces this through a private mode,
//! alacritty remembers it in `TermMode`, and it is up to us to send the
//! matching sequences.
//!
//! Without that, the wheel fell back to arrows — three per notch, the terminal
//! convention when nobody listens to the mouse — and an agent that does listen
//! received cursor moves instead of scrolling. It says so, in fact: "scroll
//! wheel is sending arrow keys".
//!
//! Three encodings coexist, and the program chooses:
//!
//! - **SGR** (`1006`), the only one with no edge: the numbers are written in
//!   decimal, so a column past the 223rd can be expressed. It is what
//!   everything written in the last fifteen years asks for.
//! - **UTF-8** (`1005`), a patch over the previous one.
//! - **The original format**, one byte per number, where nothing goes past the
//!   223rd column — a terminal was a hundred and thirty wide when it was
//!   defined. The event is **given up** there rather than clamped: reporting a
//!   click on the wrong cell is worse than reporting none.
//!
//! Nothing here talks to the pty or to the grid: these are bytes derived from
//! an event and a mode, so it is testable — like `keys`.

use alacritty_terminal::term::TermMode;
use gpui::Modifiers;

/// What moved. Wheels count: the protocol treats them as buttons, numbered
/// separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

impl Button {
    fn code(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
            Self::WheelUp => 64,
            Self::WheelDown => 65,
        }
    }

    fn is_wheel(self) -> bool {
        matches!(self, Self::WheelUp | Self::WheelDown)
    }
}

/// What happens to the button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Press,
    Release,
    /// A move, button held or not. This is the only case where the program may
    /// have asked for nothing while still listening for clicks.
    Move,
}

/// A mouse event, in grid cells (zero-based indices, as everywhere else here;
/// the protocol itself counts from one).
#[derive(Debug, Clone, Copy)]
pub struct Report {
    /// `None` on a move with no button held.
    pub button: Option<Button>,
    pub action: Action,
    pub column: usize,
    pub line: usize,
    pub modifiers: Modifiers,
}

/// The bytes to write to the pty, or `None` when the program asked for nothing
/// of the sort — in which case the caller stays free to do as it likes: scroll
/// the scrollback, select text.
pub fn report(mode: TermMode, event: Report) -> Option<Vec<u8>> {
    if !mode.intersects(TermMode::MOUSE_MODE) {
        return None;
    }
    // A move is only reported if the program asked for it: `1002` while a
    // button is held, `1003` all the time. Sending them to a program that only
    // listens for clicks would fill the pty on every movement of the hand.
    if event.action == Action::Move {
        let wanted = if event.button.is_some() {
            TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION
        } else {
            TermMode::MOUSE_MOTION
        };
        if !mode.intersects(wanted) {
            return None;
        }
    }

    let button = event.button.unwrap_or(Button::Left);
    // The original format has no per-button release: it returns the same code
    // for all three, and the program remembers which was down. SGR, for its
    // part, distinguishes them by the final letter.
    let released = event.action == Action::Release;
    let sgr = mode.contains(TermMode::SGR_MOUSE);
    let mut code = if released && !sgr { 3 } else { button.code() };
    if event.action == Action::Move {
        code += 32;
    }
    code += modifier_bits(&event.modifiers, button);

    let column = event.column + 1;
    let line = event.line + 1;
    if sgr {
        let end = if released { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{column};{line}{end}").into_bytes());
    }
    if mode.contains(TermMode::UTF8_MOUSE) {
        let mut out = b"\x1b[M".to_vec();
        for value in [u32::from(code) + 32, offset(column)?, offset(line)?] {
            let mut buffer = [0u8; 4];
            out.extend_from_slice(char::from_u32(value)?.encode_utf8(&mut buffer).as_bytes());
        }
        return Some(out);
    }
    Some(vec![
        0x1b,
        b'[',
        b'M',
        code.checked_add(32)?,
        u8::try_from(offset(column)?).ok()?,
        u8::try_from(offset(line)?).ok()?,
    ])
}

/// The original format's offset of thirty-two, which puts every number in a
/// printable byte. Past the 223rd cell there is no room left, and nothing
/// honest to report.
fn offset(value: usize) -> Option<u32> {
    (value <= 223).then(|| value as u32 + 32)
}

/// The modifiers, as xterm adds them to the button code.
///
/// **Not on the wheel**: its codes 64 and 65 already carry bit 6, and adding
/// Ctrl (16) would give a number no program reads as a wheel notch. Also,
/// Ctrl+wheel belongs to the terminal, which turns it into zoom.
fn modifier_bits(modifiers: &Modifiers, button: Button) -> u8 {
    if button.is_wheel() {
        return 0;
    }
    let mut bits = 0;
    if modifiers.shift {
        bits += 4;
    }
    if modifiers.alt {
        bits += 8;
    }
    if modifiers.control {
        bits += 16;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(button: Option<Button>, action: Action, column: usize, line: usize) -> Report {
        Report {
            button,
            action,
            column,
            line,
            modifiers: Modifiers::default(),
        }
    }

    fn sgr() -> TermMode {
        TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE
    }

    /// The case that was reported to us: an agent asks for the mouse, and the
    /// wheel must reach it as such and not as arrows.
    #[test]
    fn a_wheel_notch_is_a_button_of_its_own() {
        let up = report(sgr(), at(Some(Button::WheelUp), Action::Press, 0, 0));
        assert_eq!(up.as_deref(), Some(&b"\x1b[<64;1;1M"[..]));
        let down = report(sgr(), at(Some(Button::WheelDown), Action::Press, 9, 4));
        assert_eq!(down.as_deref(), Some(&b"\x1b[<65;10;5M"[..]));
    }

    /// Without the program asking, nothing goes out: the wheel stays with the
    /// terminal, which turns it into scrollback.
    #[test]
    fn nothing_is_reported_to_a_program_that_asked_for_nothing() {
        assert!(report(
            TermMode::ALT_SCREEN,
            at(Some(Button::WheelUp), Action::Press, 0, 0)
        )
        .is_none());
    }

    /// SGR tells a release by its final letter; the original format has only
    /// one code for all three buttons.
    #[test]
    fn a_release_is_told_differently_by_the_two_encodings() {
        let modern = report(sgr(), at(Some(Button::Right), Action::Release, 2, 3));
        assert_eq!(modern.as_deref(), Some(&b"\x1b[<2;3;4m"[..]));

        let legacy = report(
            TermMode::MOUSE_REPORT_CLICK,
            at(Some(Button::Right), Action::Release, 2, 3),
        )
        .expect("the click is reported");
        assert_eq!(legacy[3], 32 + 3, "the released button is not named");
    }

    #[test]
    fn the_modifiers_ride_on_the_button_but_not_on_the_wheel() {
        let mut event = at(Some(Button::Left), Action::Press, 0, 0);
        event.modifiers.control = true;
        assert_eq!(report(sgr(), event).as_deref(), Some(&b"\x1b[<16;1;1M"[..]));

        let mut wheel = at(Some(Button::WheelUp), Action::Press, 0, 0);
        wheel.modifiers.control = true;
        assert_eq!(
            report(sgr(), wheel).as_deref(),
            Some(&b"\x1b[<64;1;1M"[..]),
            "Ctrl+wheel belongs to the terminal, not to the program"
        );
    }

    /// A move only goes to whoever asked for it, and dragging and hovering are
    /// not asked for the same way.
    #[test]
    fn a_move_is_reported_only_to_who_asked_for_it() {
        let dragging = at(Some(Button::Left), Action::Move, 1, 1);
        let hovering = at(None, Action::Move, 1, 1);

        assert!(report(TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE, dragging).is_none());
        assert!(report(TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE, hovering).is_none());

        let drag = report(TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE, dragging);
        assert_eq!(drag.as_deref(), Some(&b"\x1b[<32;2;2M"[..]), "bit 32");
        assert!(report(TermMode::MOUSE_MOTION | TermMode::SGR_MOUSE, hovering).is_some());
    }

    /// The original format cannot say "column 300". We give the event up: a
    /// click reported on the wrong cell is worse than no click at all.
    #[test]
    fn the_oldest_encoding_gives_up_past_its_last_column() {
        let far = at(Some(Button::Left), Action::Press, 300, 0);
        assert!(report(TermMode::MOUSE_REPORT_CLICK, far).is_none());
        assert!(
            report(sgr(), far).is_some(),
            "SGR has no such limit, and that is why it exists"
        );
    }
}
