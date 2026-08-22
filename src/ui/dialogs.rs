//! The button row at the bottom of a dialog.
//!
//! gpui-component paints it for an `AlertDialog` only: a plain `Dialog` keeps
//! `on_ok` and `on_cancel` for Enter and Escape and draws nothing at all. Every
//! confirmation in this window is a plain dialog — they carry a text field, a
//! list of questions, a code excerpt, which is exactly what an `AlertDialog`
//! does not take — so all of them were keyboard-only, and said so nowhere: with
//! `close_button(false)` and `overlay_closable(false)`, a dialog one does not
//! know how to answer is a dialog one cannot leave.
//!
//! The buttons dispatch the very actions the keys dispatch — `Confirm` and
//! `Cancel` — so both routes end in the same `on_ok` / `on_cancel`. Two ways of
//! making one gesture that did not end up in the same place would be one too
//! many, and it is also what keeps the dialogs' code unchanged apart from the
//! line that adds this footer.

use gpui::{prelude::*, SharedString};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{Cancel, Confirm, DialogFooter};

use crate::tr;

/// The two buttons of a confirmation, cancel first.
pub(super) fn confirm() -> DialogFooter {
    footer(tr!("dialog-ok"), Some(tr!("dialog-cancel")))
}

/// The single button of a dialog one only reads.
pub(super) fn close() -> DialogFooter {
    footer(tr!("dialog-close"), None)
}

/// The row itself, with the labels it is given: a destructive gesture deserves
/// to name what it does rather than to answer "OK".
fn footer(ok: SharedString, cancel: Option<SharedString>) -> DialogFooter {
    DialogFooter::new()
        .children(cancel.map(|label| {
            Button::new("dialog-cancel")
                .label(label)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx))
        }))
        .child(
            Button::new("dialog-ok")
                .label(ok)
                .primary()
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(Confirm { secondary: false }), cx)
                }),
        )
}
