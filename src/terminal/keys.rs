//! Turning a gpui keystroke into bytes for the pty.
//!
//! There is no single standard: the sequences below are the ones xterm emits
//! and every `xterm-256color` terminfo describes, which is the contract we
//! announce to the program through `TERM`.
//!
//! Two modes change what goes out on the wire. In "application cursor" mode
//! (DECCKM, requested by vim, less, most full-screen interfaces) the arrows
//! start with `ESC O` rather than `ESC [`. In "application keypad" mode the
//! numeric keypad changes too; we do not distinguish it, no common program
//! relying on it any more.

use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

/// Returns the bytes to write to the pty, or `None` if the keystroke has no
/// business there (a dead key, a lone modifier, a Claudhub shortcut).
pub fn key_bytes(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let m = &keystroke.modifiers;
    let key = keystroke.key.as_str();

    // The application's shortcuts (Ctrl/Cmd+T, Cmd+W…) are intercepted
    // upstream by gpui actions; whatever reaches here with the platform key
    // has no business in the terminal.
    if m.platform {
        return None;
    }

    // Meta's ESC prefix: it is what every terminal does for Alt+x, and what
    // readline and emacs count on.
    let alt = |mut bytes: Vec<u8>| -> Option<Vec<u8>> {
        if m.alt {
            bytes.insert(0, 0x1b);
        }
        Some(bytes)
    };

    // Movement keys, whose form depends on application mode.
    let cursor = |letter: char| -> Option<Vec<u8>> {
        // With a modifier, xterm switches to the long form `ESC [ 1 ; n X`,
        // where n encodes shift/alt/ctrl. That is what lets an editor tell
        // Ctrl+Right from a plain arrow.
        if let Some(n) = modifier_code(m) {
            return Some(format!("\x1b[1;{n}{letter}").into_bytes());
        }
        let intro = if mode.contains(TermMode::APP_CURSOR) {
            "\x1bO"
        } else {
            "\x1b["
        };
        Some(format!("{intro}{letter}").into_bytes())
    };

    // Editing keys, of the form `ESC [ n ~`.
    let tilde = |n: u8| -> Option<Vec<u8>> {
        match modifier_code(m) {
            Some(code) => Some(format!("\x1b[{n};{code}~").into_bytes()),
            None => Some(format!("\x1b[{n}~").into_bytes()),
        }
    };

    match key {
        "up" => cursor('A'),
        "down" => cursor('B'),
        "right" => cursor('C'),
        "left" => cursor('D'),
        "home" => cursor('H'),
        "end" => cursor('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => Some(b"\x1bOP".to_vec()),
        "f2" => Some(b"\x1bOQ".to_vec()),
        "f3" => Some(b"\x1bOR".to_vec()),
        "f4" => Some(b"\x1bOS".to_vec()),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),

        "enter" => alt(vec![b'\r']),
        "tab" if m.shift => Some(b"\x1b[Z".to_vec()),
        "tab" => alt(vec![b'\t']),
        // Backspace sends DEL (0x7f), not BS: that is what `xterm-256color`
        // describes, and the opposite breaks the shell's line editing.
        "backspace" if m.control => Some(vec![0x08]),
        "backspace" => alt(vec![0x7f]),
        "escape" => Some(vec![0x1b]),
        "space" if m.control => Some(vec![0x00]), // Ctrl+Space = NUL
        "space" => alt(vec![b' ']),

        _ => {
            if m.control {
                return control_byte(key).map(|b| {
                    let mut bytes = vec![b];
                    if m.alt {
                        bytes.insert(0, 0x1b);
                    }
                    bytes
                });
            }
            // The ordinary case: the character the keyboard layout actually
            // produced, accents and non-Latin layouts included. When there is
            // none, `key` is NOT a substitute: during dead-key composition
            // (the Belgian ^, ¨, ´…) gpui still delivers a KeyDown with no
            // key_char, and `key` is then the character *guessed from the US
            // layout* of the physical key — `[` for the Belgian circumflex.
            // Falling back on it typed a stray `[` before every ê; the
            // composed character arrives with the next keystroke, in its
            // key_char. The one keystroke legitimately without a key_char is
            // Alt+letter — the platform may withhold the character there —
            // hence the readline escape prefix stays available from `key`.
            if let Some(text) = keystroke.key_char.as_deref() {
                return alt(text.as_bytes().to_vec());
            }
            (m.alt && key.is_ascii() && key.chars().count() == 1)
                .then(|| alt(key.as_bytes().to_vec()))
                .flatten()
        }
    }
}

/// xterm's modifier code: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_code(m: &gpui::Modifiers) -> Option<u8> {
    let code = 1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.control);
    (code > 1).then_some(code)
}

/// The control byte for a key combined with Ctrl.
///
/// Ctrl+A..Z are 1..26; the few symbols that follow come from the original
/// ASCII encoding, where Ctrl simply clears bit 6.
fn control_byte(key: &str) -> Option<u8> {
    let c = key.chars().next()?;
    if key.chars().count() != 1 {
        return None;
    }
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '@' => Some(0),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        // Ctrl+/ is 0x1f in every terminal, hence emacs's undo.
        '/' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(key: &str) -> Keystroke {
        Keystroke::parse(key).expect("valid keystroke")
    }

    fn bytes(key: &str, mode: TermMode) -> Vec<u8> {
        key_bytes(&stroke(key), mode).expect("the key must produce bytes")
    }

    #[test]
    fn arrows_follow_the_application_mode() {
        assert_eq!(bytes("up", TermMode::empty()), b"\x1b[A");
        // Under vim or less, the same key changes form.
        assert_eq!(bytes("up", TermMode::APP_CURSOR), b"\x1bOA");
    }

    #[test]
    fn modified_arrows_use_the_long_form() {
        // Ctrl+Right: code 5 = 1 + ctrl(4).
        assert_eq!(bytes("ctrl-right", TermMode::empty()), b"\x1b[1;5C");
        assert_eq!(bytes("shift-left", TermMode::empty()), b"\x1b[1;2D");
        // Application mode does not apply to the long form.
        assert_eq!(bytes("ctrl-right", TermMode::APP_CURSOR), b"\x1b[1;5C");
    }

    #[test]
    fn control_letters_map_to_control_bytes() {
        assert_eq!(bytes("ctrl-c", TermMode::empty()), vec![0x03]);
        assert_eq!(bytes("ctrl-d", TermMode::empty()), vec![0x04]);
        assert_eq!(bytes("ctrl-a", TermMode::empty()), vec![0x01]);
    }

    #[test]
    fn alt_prefixes_with_escape() {
        // Alt+f, readline's "forward word".
        assert_eq!(bytes("alt-f", TermMode::empty()), b"\x1bf");
        assert_eq!(bytes("alt-ctrl-a", TermMode::empty()), vec![0x1b, 0x01]);
    }

    #[test]
    fn backspace_sends_del_not_backspace() {
        assert_eq!(bytes("backspace", TermMode::empty()), vec![0x7f]);
    }

    #[test]
    fn shift_tab_is_a_back_tab() {
        assert_eq!(bytes("shift-tab", TermMode::empty()), b"\x1b[Z");
        assert_eq!(bytes("tab", TermMode::empty()), vec![b'\t']);
    }

    #[test]
    fn editing_keys_carry_their_modifiers() {
        assert_eq!(bytes("delete", TermMode::empty()), b"\x1b[3~");
        assert_eq!(bytes("ctrl-delete", TermMode::empty()), b"\x1b[3;5~");
        assert_eq!(bytes("pageup", TermMode::empty()), b"\x1b[5~");
    }

    #[test]
    fn a_dead_key_sends_nothing() {
        // A Belgian ^ while composing: gpui guesses the US-layout character
        // of the physical key (`[`) and delivers no key_char. Nothing must
        // reach the pty — the composed ê comes with the next keystroke.
        assert_eq!(key_bytes(&stroke("["), TermMode::empty()), None);
    }

    #[test]
    fn the_composed_character_is_what_goes_out() {
        // ^ then e: the second keystroke carries ê in its key_char.
        assert_eq!(bytes("e->ê", TermMode::empty()), "ê".as_bytes());
    }

    #[test]
    fn platform_shortcuts_stay_in_the_application() {
        // Cmd/Super+T opens a Claudhub tab: nothing must go to the shell.
        assert_eq!(key_bytes(&stroke("cmd-t"), TermMode::empty()), None);
    }
}
