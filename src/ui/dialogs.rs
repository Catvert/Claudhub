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

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{Cancel, Confirm, DialogFooter};
use gpui_kit::component::WindowExt as _;
use gpui_kit::{prelude::*, AnyElement, App, Context, Entity, Focusable, SharedString, Window};

use crate::tr;

/// Asks the hand a question: a dialog with its title and its body, left by
/// its buttons only — `footer`, one of this module's rows — and `on_ok` run
/// on `app` when it is answered.
///
/// `body` and `footer` are built again at every frame: `open_dialog` keeps a
/// `Fn` it calls back from the root's render. **The focus goes into the
/// dialog one turn later**, always: the buttons dispatch `Confirm` and
/// `Cancel` from the focus, and opened from a terminal, a node's cross or a
/// menu, the focus was still there — OK did nothing. Deferred, the dialog
/// being painted on the next frame. A dialog with a field focuses its field
/// instead (`focus_field`), and is not asked through here.
pub(super) fn ask<T: 'static>(
    app: Entity<T>,
    title: SharedString,
    body: impl Fn() -> AnyElement + 'static,
    footer: impl Fn() -> DialogFooter + 'static,
    on_ok: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let on_ok = std::rc::Rc::new(on_ok);
    window.open_dialog(cx, move |dialog, _, _| {
        let (app, on_ok) = (app.clone(), on_ok.clone());
        dialog
            .title(title.clone())
            .child(body())
            .overlay_closable(false)
            .close_button(false)
            .footer(footer())
            .on_ok(move |_, window, cx| {
                app.update(cx, |this, cx| on_ok(this, window, cx));
                true
            })
    });
    window.defer(cx, |window, cx| window.focus_dialog(cx));
}

/// The two buttons of a confirmation, cancel first.
pub(super) fn confirm() -> DialogFooter {
    footer(tr!("dialog-ok"), Some(tr!("dialog-cancel")))
}

/// A form whose primary action names what submitting it does.
pub(super) fn submit(label: SharedString) -> DialogFooter {
    footer(label, Some(tr!("dialog-cancel")))
}

/// The single button of a dialog one only reads.
pub(super) fn close() -> DialogFooter {
    footer(tr!("dialog-close"), None)
}

/// The three buttons of a question with two answers: cancelling, the other
/// answer, and the one Enter makes.
///
/// The middle one is the exception to the rule this module is built on. It
/// carries a gesture no key dispatches — there are two actions, `Confirm` and
/// `Cancel`, and this is a third answer — so it does its work in its own
/// handler and then dispatches `Cancel` to dismiss the dialog. Which means the
/// dialog's `on_cancel` must stay what cancelling is: doing nothing.
pub(super) fn choose(
    ok: SharedString,
    other: SharedString,
    on_other: impl Fn(&mut Window, &mut App) + 'static,
) -> DialogFooter {
    DialogFooter::new()
        .child(
            Button::new("dialog-cancel")
                .label(tr!("dialog-cancel"))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
        )
        .child(
            Button::new("dialog-other")
                .label(other)
                .on_click(move |_, window, cx| {
                    on_other(window, cx);
                    window.dispatch_action(Box::new(Cancel), cx);
                }),
        )
        .child(
            Button::new("dialog-ok")
                .label(ok)
                .primary()
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(Confirm { secondary: false }), cx)
                }),
        )
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

/// Puts the caret in a dialog's field, **one turn after** the dialog opens.
///
/// Two things at once, and they are the same thing. A dialog one opens to type
/// in should be typed in without a click first; and Enter confirms as soon as
/// the focus is *inside* the dialog — the field's own Enter propagates, and the
/// dialog's `Confirm` binding is the next one the keystroke finds. What was
/// missing was never the confirmation, it was the focus.
///
/// **Deferred, and that is the whole of it.** These dialogs are opened from a
/// popup menu — "New file here", "Rename" —, and a menu **puts the focus back
/// where it was** when it dismisses, which happens after the entry's handler has
/// run. Focusing straight away therefore lost the race, and the dialog opened
/// with the focus back in the explorer: nothing typed went in, and Enter, being
/// nowhere near the dialog, did nothing. Measured in gpui-component's own test
/// harness — a stolen focus confirms zero times, a deferred one confirms once.
///
/// Nothing is subscribed to the field's `PressEnter`: the same harness shows
/// that route **adds** a confirmation to the one the keystroke already carries,
/// so the dialog would be answered twice.
pub(super) fn focus_field<T: Focusable + 'static>(
    field: &Entity<T>,
    window: &mut Window,
    cx: &mut App,
) {
    let field = field.clone();
    window.defer(cx, move |window, cx| {
        field.focus_handle(cx).focus(window, cx);
    });
}
