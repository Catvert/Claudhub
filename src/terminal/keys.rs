//! Traduction d'une frappe gpui en octets pour le pty.
//!
//! Il n'y a pas de norme unique : les séquences ci-dessous sont celles que
//! xterm émet et que toutes les terminfo `xterm-256color` décrivent, ce qui
//! est le contrat que nous annonçons au programme via `TERM`.
//!
//! Deux modes changent ce qui part sur le fil. En mode « curseur applicatif »
//! (DECCKM, demandé par vim, less, la plupart des interfaces plein écran) les
//! flèches commencent par `ESC O` et non `ESC [`. En mode « clavier
//! applicatif » le pavé numérique change aussi ; nous ne le distinguons pas,
//! aucun programme courant ne s'y fiant plus.

use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

/// Rend les octets à écrire dans le pty, ou `None` si la frappe n'a rien à y
/// faire (une touche morte, un modificateur seul, un raccourci de Claudhub).
pub fn key_bytes(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let m = &keystroke.modifiers;
    let key = keystroke.key.as_str();

    // Les raccourcis de l'application (Ctrl/Cmd+T, Cmd+W…) sont interceptés
    // en amont par les actions gpui ; ce qui arrive ici avec la touche système
    // n'a rien à faire dans le terminal.
    if m.platform {
        return None;
    }

    // Le préfixe ESC de Meta : c'est ce que fait tout terminal pour Alt+x, et
    // ce sur quoi comptent readline et emacs.
    let alt = |mut bytes: Vec<u8>| -> Option<Vec<u8>> {
        if m.alt {
            bytes.insert(0, 0x1b);
        }
        Some(bytes)
    };

    // Les touches de déplacement, dont la forme dépend du mode applicatif.
    let cursor = |letter: char| -> Option<Vec<u8>> {
        // Avec un modificateur, xterm passe à la forme longue `ESC [ 1 ; n X`,
        // où n encode shift/alt/ctrl. C'est ce qui permet à un éditeur de
        // distinguer Ctrl+Droite d'une simple flèche.
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

    // Les touches d'édition, de la forme `ESC [ n ~`.
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
        // Retour arrière envoie DEL (0x7f), pas BS : c'est ce que décrit
        // `xterm-256color`, et l'inverse casse l'édition de ligne du shell.
        "backspace" if m.control => Some(vec![0x08]),
        "backspace" => alt(vec![0x7f]),
        "escape" => Some(vec![0x1b]),
        "space" if m.control => Some(vec![0x00]), // Ctrl+Espace = NUL
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
            // Le cas ordinaire : le caractère que la disposition clavier a
            // effectivement produit, accents et dispositions non latines
            // compris. `key` seul donnerait l'équivalent ASCII de la touche.
            let text = keystroke.key_char.as_deref().or({
                // Une touche d'une seule lettre sans key_char (certaines
                // dispositions) reste utilisable telle quelle.
                (key.chars().count() == 1).then_some(key)
            })?;
            alt(text.as_bytes().to_vec())
        }
    }
}

/// Le code de modificateur d'xterm : 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_code(m: &gpui::Modifiers) -> Option<u8> {
    let code = 1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.control);
    (code > 1).then_some(code)
}

/// L'octet de contrôle d'une touche combinée à Ctrl.
///
/// Ctrl+A..Z valent 1..26 ; les quelques symboles qui suivent viennent du
/// codage ASCII d'origine, où Ctrl efface simplement le bit 6.
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
        // Ctrl+/ vaut 0x1f dans tous les terminaux, d'où l'annulation d'emacs.
        '/' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(key: &str) -> Keystroke {
        Keystroke::parse(key).expect("raccourci valide")
    }

    fn bytes(key: &str, mode: TermMode) -> Vec<u8> {
        key_bytes(&stroke(key), mode).expect("la touche doit produire des octets")
    }

    #[test]
    fn arrows_follow_the_application_mode() {
        assert_eq!(bytes("up", TermMode::empty()), b"\x1b[A");
        // Sous vim ou less, la même touche change de forme.
        assert_eq!(bytes("up", TermMode::APP_CURSOR), b"\x1bOA");
    }

    #[test]
    fn modified_arrows_use_the_long_form() {
        // Ctrl+Droite : code 5 = 1 + ctrl(4).
        assert_eq!(bytes("ctrl-right", TermMode::empty()), b"\x1b[1;5C");
        assert_eq!(bytes("shift-left", TermMode::empty()), b"\x1b[1;2D");
        // Le mode applicatif ne s'applique pas à la forme longue.
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
        // Alt+f, le « mot suivant » de readline.
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
    fn platform_shortcuts_stay_in_the_application() {
        // Cmd/Super+T ouvre un onglet Claudhub : rien ne doit partir au shell.
        assert_eq!(key_bytes(&stroke("cmd-t"), TermMode::empty()), None);
    }
}
