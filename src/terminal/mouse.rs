//! Le rapport de souris.
//!
//! Un programme plein écran peut demander à **recevoir** la souris plutôt que
//! de laisser le terminal en faire ce qu'il veut : c'est ainsi qu'une liste se
//! choisit au clic et qu'un panneau défile à la molette. Il l'annonce par un
//! mode privé, alacritty le retient dans `TermMode`, et c'est à nous de lui
//! envoyer les séquences correspondantes.
//!
//! Sans cela, la molette retombait sur les flèches — trois par cran, la
//! convention des terminaux quand personne n'écoute la souris — et un agent
//! qui, lui, écoute, recevait des déplacements de curseur au lieu d'un
//! défilement. Il le dit, d'ailleurs : « scroll wheel is sending arrow keys ».
//!
//! Trois encodages coexistent, et le programme choisit :
//!
//! - **SGR** (`1006`), le seul qui n'ait pas de bord : les nombres sont écrits
//!   en décimal, donc une colonne au-delà de la 223e s'y exprime. C'est ce que
//!   demande tout ce qui a été écrit ces quinze dernières années.
//! - **UTF-8** (`1005`), une rustine sur le précédent.
//! - **Le format d'origine**, un octet par nombre, où rien ne dépasse la
//!   223e colonne — un terminal en faisait cent trente quand il a été défini.
//!   L'événement y est **abandonné** plutôt que rogné : rapporter un clic sur
//!   la mauvaise cellule est pire que de n'en rapporter aucun.
//!
//! Rien ici ne parle au pty ni à la grille : ce sont des octets déduits d'un
//! événement et d'un mode, donc c'est testable — comme `keys`.

use alacritty_terminal::term::TermMode;
use gpui::Modifiers;

/// Ce qui a bougé. Les molettes en font partie : le protocole les traite comme
/// des boutons, numérotés à part.
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

/// Ce qui arrive au bouton.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Press,
    Release,
    /// Un déplacement, bouton tenu ou non. C'est le seul cas où le programme
    /// peut ne rien avoir demandé alors qu'il écoute les clics.
    Move,
}

/// Un événement de souris, en cellules de la grille (indices à partir de zéro,
/// comme partout ailleurs chez nous ; le protocole, lui, compte à partir de un).
#[derive(Debug, Clone, Copy)]
pub struct Report {
    /// `None` sur un déplacement sans bouton tenu.
    pub button: Option<Button>,
    pub action: Action,
    pub column: usize,
    pub line: usize,
    pub modifiers: Modifiers,
}

/// Les octets à écrire dans le pty, ou `None` quand le programme n'a rien
/// demandé de tel — auquel cas l'appelant reste libre du geste : défiler
/// l'historique, sélectionner du texte.
pub fn report(mode: TermMode, event: Report) -> Option<Vec<u8>> {
    if !mode.intersects(TermMode::MOUSE_MODE) {
        return None;
    }
    // Un déplacement n'est rapporté que si le programme l'a demandé : `1002`
    // pendant qu'un bouton est tenu, `1003` en permanence. Les envoyer à un
    // programme qui n'écoute que les clics remplirait le pty à chaque geste de
    // la main.
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
    // Le format d'origine n'a pas de relâchement par bouton : il rend le
    // même code pour les trois, et le programme se souvient de celui qui était
    // enfoncé. SGR, lui, distingue par la lettre finale.
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

/// Le décalage de trente-deux du format d'origine, qui met chaque nombre dans
/// un octet imprimable. Au-delà de la 223e cellule il n'y a plus de place, et
/// il n'y a rien à rapporter d'honnête.
fn offset(value: usize) -> Option<u32> {
    (value <= 223).then(|| value as u32 + 32)
}

/// Les modificateurs, tels que xterm les ajoute au code du bouton.
///
/// **Pas sur la molette** : ses codes 64 et 65 portent déjà le bit 6, et lui
/// ajouter Ctrl (16) donnerait un nombre qu'aucun programme ne lit comme un
/// cran de molette. C'est aussi que Ctrl+molette appartient au terminal, qui
/// en fait son zoom.
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

    /// Le cas qui nous a été signalé : un agent demande la souris, la molette
    /// doit lui parvenir comme telle et non en flèches.
    #[test]
    fn a_wheel_notch_is_a_button_of_its_own() {
        let up = report(sgr(), at(Some(Button::WheelUp), Action::Press, 0, 0));
        assert_eq!(up.as_deref(), Some(&b"\x1b[<64;1;1M"[..]));
        let down = report(sgr(), at(Some(Button::WheelDown), Action::Press, 9, 4));
        assert_eq!(down.as_deref(), Some(&b"\x1b[<65;10;5M"[..]));
    }

    /// Sans demande du programme, rien ne part : la molette reste au terminal,
    /// qui en fait son historique.
    #[test]
    fn nothing_is_reported_to_a_program_that_asked_for_nothing() {
        assert!(report(
            TermMode::ALT_SCREEN,
            at(Some(Button::WheelUp), Action::Press, 0, 0)
        )
        .is_none());
    }

    /// SGR distingue le relâchement par sa lettre finale ; le format d'origine
    /// n'a qu'un code pour les trois boutons.
    #[test]
    fn a_release_is_told_differently_by_the_two_encodings() {
        let modern = report(sgr(), at(Some(Button::Right), Action::Release, 2, 3));
        assert_eq!(modern.as_deref(), Some(&b"\x1b[<2;3;4m"[..]));

        let legacy = report(
            TermMode::MOUSE_REPORT_CLICK,
            at(Some(Button::Right), Action::Release, 2, 3),
        )
        .expect("le clic est rapporté");
        assert_eq!(legacy[3], 32 + 3, "le bouton relâché n'est pas nommé");
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
            "Ctrl+molette appartient au terminal, pas au programme"
        );
    }

    /// Un déplacement ne part qu'à qui l'a demandé, et le glissement et le
    /// survol ne se demandent pas de la même façon.
    #[test]
    fn a_move_is_reported_only_to_who_asked_for_it() {
        let dragging = at(Some(Button::Left), Action::Move, 1, 1);
        let hovering = at(None, Action::Move, 1, 1);

        assert!(report(TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE, dragging).is_none());
        assert!(report(TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE, hovering).is_none());

        let drag = report(TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE, dragging);
        assert_eq!(drag.as_deref(), Some(&b"\x1b[<32;2;2M"[..]), "le bit 32");
        assert!(report(TermMode::MOUSE_MOTION | TermMode::SGR_MOUSE, hovering).is_some());
    }

    /// Le format d'origine ne sait pas dire « colonne 300 ». On abandonne
    /// l'événement : un clic rapporté sur la mauvaise cellule est pire que
    /// pas de clic du tout.
    #[test]
    fn the_oldest_encoding_gives_up_past_its_last_column() {
        let far = at(Some(Button::Left), Action::Press, 300, 0);
        assert!(report(TermMode::MOUSE_REPORT_CLICK, far).is_none());
        assert!(
            report(sgr(), far).is_some(),
            "SGR n'a pas cette limite, et c'est pour cela qu'il existe"
        );
    }
}
