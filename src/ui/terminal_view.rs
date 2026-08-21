//! The terminals: one tab group per worktree.
//!
//! Each tab is a `Terminal` (pty + alacritty emulation) and a view that draws
//! it. The multiplexing is here and not in tmux: the tabs are attached to a
//! worktree, changing worktree changes group, and closing a worktree closes what
//! was running in it.
//!
//! The rendering is text, not a canvas: each grid line becomes a `StyledText`
//! whose style runs come from the snapshot. A fixed-pitch font is then enough to
//! line the columns up, and gpui takes care of shaping, ligatures and complex
//! scripts — which a cell-by-cell renderer would have had to reimplement.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::{
    div, prelude::*, px, App, Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable, Hsla,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render,
    ScrollWheelEvent, SharedString, StyledText, TextRun, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    menu::{ContextMenuExt, DropdownMenu, PopupMenuItem},
    v_flex, ActiveTheme, Sizable,
};

use crate::terminal::{
    key_bytes, mouse, Paint, SelectionKind, Side, Snapshot, Spawn, TermSize, Terminal,
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
    label: SharedString,
    /// True when this tab runs a coding agent.
    ///
    /// It is what makes it possible to deliver review notes to it without
    /// picking the wrong tab. Recorded at opening and not derived from the
    /// title: an agent renames its tab as the conversation goes, and looking for
    /// its name in a changing title would be guesswork.
    agent: bool,
}

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
                let alive = this
                    .update(cx, |view, cx| {
                        match event {
                            TerminalEvent::Wakeup => {}
                            TerminalEvent::Title(title) => {
                                view.terminal.set_title(title);
                            }
                            TerminalEvent::Bell => {}
                            TerminalEvent::Exited => view.terminal.mark_exited(),
                        }
                        view.snapshot = view.terminal.snapshot();
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
            terminal,
            focus: cx.focus_handle(),
            font_size,
            font_family,
            bounds: Bounds::default(),
            cell: gpui::size(px(8.), px(16.)),
            selecting: false,
            mouse_cell: None,
            scroll_remainder: 0.,
            pending_size: None,
            resize_scheduled: false,
            label,
            agent,
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
        self.snapshot = self.terminal.snapshot();
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
        self.snapshot = self.terminal.snapshot();
        cx.notify();
    }

    pub fn label(&self) -> SharedString {
        let title = self.terminal.title();
        if title.is_empty() {
            self.label.clone()
        } else {
            SharedString::from(title.to_string())
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
    }

    pub fn has_exited(&self) -> bool {
        self.terminal.has_exited()
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

    /// Passes the new geometry on, once the drag has settled.
    ///
    /// A mouse resize goes through every intermediate width. Passing them all on
    /// amounts to sending one `SIGWINCH` per frame: the program redraws every
    /// time, and since it redraws *in place*, its successive prompts pile up
    /// instead of replacing each other. So we wait for the size to settle;
    /// meanwhile, the panel clips the old grid, exactly as a window being
    /// resized does.
    fn request_size(&mut self, size: TermSize, cx: &mut Context<Self>) {
        if self.terminal.size() == size {
            self.pending_size = None;
            return;
        }
        self.pending_size = Some(size);
        if self.resize_scheduled {
            return;
        }
        self.resize_scheduled = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESIZE_QUIET).await;
            let _ = this.update(cx, |this, cx| {
                this.resize_scheduled = false;
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
            // Typing invalidates the selection: what it named will have moved as
            // soon as the program answers.
            self.terminal.clear_selection();
            // Every keystroke brings the view back to the bottom: that is what a
            // terminal does, and typing after scrolling back without the view
            // following would be disconcerting.
            self.terminal.scroll_to_bottom();
            self.terminal.write(bytes);
            self.snapshot = self.terminal.snapshot();
            cx.notify();
        }
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
            self.snapshot = self.terminal.snapshot();
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
        self.snapshot = self.terminal.snapshot();
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
        self.snapshot = self.terminal.snapshot();
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
            self.snapshot = self.terminal.snapshot();
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
            self.snapshot = self.terminal.snapshot();
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
        self.snapshot = self.terminal.snapshot();
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
            self.snapshot = self.terminal.snapshot();
            cx.notify();
        }
    }

    /// Selects all the visible content and the scrollback.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.terminal.select_all();
        self.snapshot = self.terminal.snapshot();
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
        self.snapshot = self.terminal.snapshot();
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
        self.sync_font(cx);
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
            |_, _, _, _| {},
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
        let cell_height = self.cell.height;
        let lines: Vec<_> = self
            .snapshot
            .lines
            .iter()
            .map(|line| {
                div()
                    .h(cell_height)
                    .w_full()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(styled_line(line, &font_family, default_fg, selection_bg))
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
                move |menu, _window, _cx| {
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
            })
            .child(measure)
            .child(v_flex().size_full().overflow_hidden().children(lines))
            .children(self.render_cursor(focused, cx))
    }
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

/// Converts a snapshot line into styled text.
fn styled_line(
    line: &crate::terminal::Line,
    family: &SharedString,
    default_fg: Hsla,
    selection_bg: Hsla,
) -> StyledText {
    let text = SharedString::from(line.text.clone());
    let mut runs: Vec<TextRun> = Vec::with_capacity(line.segments.len());
    for seg in &line.segments {
        let len = seg.end.saturating_sub(seg.start);
        if len == 0 {
            continue;
        }
        runs.push(TextRun {
            len,
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
                Paint::Rgb(r, g, b) => rgb(r, g, b),
            },
            background_color: if seg.selected {
                // The selection wins over the cell's background colour,
                // otherwise it would vanish on a coloured line.
                Some(selection_bg)
            } else {
                match seg.bg {
                    // The default background is the window's: painting nothing
                    // avoids one rectangle per cell.
                    Paint::Default => None,
                    Paint::Rgb(r, g, b) => Some(rgb(r, g, b)),
                }
            },
            underline: seg.underline.then(gpui::UnderlineStyle::default),
            strikethrough: seg.strikethrough.then(gpui::StrikethroughStyle::default),
        });
    }
    StyledText::new(text).with_runs(runs)
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

/// A worktree's tabs.
pub struct TerminalGroup {
    worktree: PathBuf,
    /// This worktree's notes folder, if it has one.
    ///
    /// It is here so it ends up in the pty's environment: an agent launched from
    /// Claudhub has to know where the remarks sent to it are and the task list
    /// it keeps. It is re-read by the application on every render, the notes
    /// root being a setting that can change under us.
    vault: Option<PathBuf>,
    tabs: Vec<Entity<TerminalView>>,
    active: usize,
    /// Why the last tab could not open, if that applies.
    error: Option<SharedString>,
}

impl TerminalGroup {
    pub fn new(worktree: PathBuf) -> Self {
        Self {
            worktree,
            vault: None,
            tabs: Vec::new(),
            active: 0,
            error: None,
        }
    }

    /// The notes folder to announce to the next tabs.
    ///
    /// Without `cx.notify`: what changes is not what is displayed but what the
    /// next pty will receive, and redrawing the group on every render would be a
    /// loop.
    pub fn set_vault(&mut self, vault: Option<PathBuf>) {
        self.vault = vault;
    }

    /// Opens a tab. An empty `command` launches the user's shell.
    ///
    /// `agent` says whether what is launched is a coding agent: it is to that
    /// tab that review notes will be delivered.
    pub fn open(&mut self, mut launch: Launch, window: &mut Window, cx: &mut Context<Self>) {
        // What the agent needs to know about Claudhub, and no API will tell it:
        // where it works, where the review notes sent to it are, and which file
        // carries its task list. Environment variables rather than a protocol —
        // a shell sees them too, and an agent launched in a terminal alongside
        // only has to copy them.
        launch.env.insert(
            "CLAUDHUB_WORKTREE".into(),
            self.worktree.display().to_string(),
        );
        if let Some(vault) = &self.vault {
            launch
                .env
                .insert("CLAUDHUB_NOTES_DIR".into(), vault.display().to_string());
            launch.env.insert(
                "CLAUDHUB_TODO".into(),
                vault.join(crate::ui::vault::TODO).display().to_string(),
            );
        }
        // A pty we cannot open is a system problem: descriptor limit reached,
        // `/dev/pts` missing. We give the tab up and say so, rather than panic
        // in the middle of a render — which is what this code used to do, with a
        // frozen window as its only symptom.
        // The settings are re-read at opening rather than recorded at
        // construction: changing the shell or the scrollback has to hold for the
        // next tab, without having to close the others.
        let settings = Settings::global(cx).terminal.clone();
        let wsl = WslShell::current(cx);
        let terminal = match TerminalView::open(&self.worktree, &launch, &settings, wsl.as_ref()) {
            Ok(terminal) => terminal,
            Err(e) => {
                log::error!("ouverture du terminal : {e:#}");
                self.error = Some(SharedString::from(e.to_string()));
                cx.notify();
                return;
            }
        };
        let view =
            cx.new(|cx| TerminalView::attach(terminal, launch.label, launch.agent, window, cx));
        self.error = None;
        self.tabs.push(view);
        self.active = self.tabs.len() - 1;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// True when the current tab has focus. That is what names the area the zoom
    /// shortcuts aim at.
    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.tabs
            .get(self.active)
            .is_some_and(|tab| tab.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Opens a tab running the configured coding agent.
    ///
    /// The gesture lives with the other terminal openings — in the "+" button's
    /// menu — and not in the window's toolbar: it is one more terminal in *this*
    /// worktree, not an action on the repository.
    pub fn open_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(profile) = Settings::global(cx).terminal.default_profile().cloned() else {
            return;
        };
        self.open_profile(&profile, window, cx);
    }

    /// Opens a named profile.
    pub fn open_profile(
        &mut self,
        profile: &crate::ui::settings::AgentProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if profile.command.trim().is_empty() {
            return;
        }
        self.open(Launch::agent(profile), window, cx);
    }

    /// Delivers a text to this worktree's agent, and confirms it.
    ///
    /// If there is no agent tab, one is opened — and the send is **deferred**:
    /// an agent takes a second or two to show its prompt, and what arrives
    /// before is read by the shell that has not been replaced yet, or simply
    /// lost.
    pub fn send_to_agent(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        match self.agent_tab(cx) {
            Some(index) => {
                self.active = index;
                self.focus_active(window, cx);
                self.deliver(index, text, cx);
            }
            None => {
                self.open_agent(window, cx);
                let Some(index) = self.agent_tab(cx) else {
                    return;
                };
                self.active = index;
                cx.spawn(async move |group, cx| {
                    cx.background_executor().timer(AGENT_WARMUP).await;
                    let _ = group.update(cx, |group, cx| group.deliver(index, text, cx));
                })
                .detach();
            }
        }
    }

    /// The most recent agent tab still running.
    ///
    /// The most recent: it is the one being looked at, and relaunching an agent
    /// after quitting one is the normal gesture when the conversation has got
    /// bogged down.
    fn agent_tab(&self, cx: &App) -> Option<usize> {
        self.tabs
            .iter()
            .enumerate()
            .rev()
            .find(|(_, tab)| {
                let tab = tab.read(cx);
                tab.is_agent() && !tab.has_exited()
            })
            .map(|(index, _)| index)
    }

    /// Pastes, then confirms in a **second** send.
    ///
    /// The two are separated by a short silence: a TUI that has just received a
    /// bracketed paste may swallow a carriage return arriving right behind it,
    /// and the message would stay in the prompt without going out.
    fn deliver(&mut self, index: usize, text: String, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index).cloned() else {
            return;
        };
        tab.update(cx, |view, cx| view.paste_text(&text, cx));
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(SUBMIT_DELAY).await;
            tab.update(cx, |view, cx| view.submit(cx));
        })
        .detach();
        cx.notify();
    }

    pub fn close(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        self.focus_active(window, cx);
        cx.notify();
    }

    pub fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        self.active = (self.active + 1) % self.tabs.len();
        self.focus_active(window, cx);
        cx.notify();
    }

    pub fn previous(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        self.focus_active(window, cx);
        cx.notify();
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.tabs.get(self.active) {
            let handle = view.read(cx).focus.clone();
            window.focus(&handle, cx);
        }
    }
}

impl Render for TerminalGroup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active;
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, view)| (ix, view.read(cx).label(), view.read(cx).has_exited()))
            .collect();

        v_flex()
            .size_full()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .h(px(28.))
                    .w_full()
                    .px_1()
                    .gap_1()
                    .items_center()
                    .bg(cx.theme().title_bar)
                    .children(tabs.into_iter().map(|(ix, label, exited)| {
                        h_flex()
                            .id(("tab", ix))
                            .h(px(22.))
                            .px_2()
                            .gap_1()
                            .items_center()
                            .rounded(cx.theme().radius)
                            .cursor_pointer()
                            .when(ix == active, |el| el.bg(cx.theme().accent))
                            .hover(|s| s.bg(cx.theme().accent.opacity(0.5)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.active = ix;
                                this.focus_active(window, cx);
                                cx.notify();
                            }))
                            .child(icon("terminal").xsmall())
                            .child(
                                div()
                                    .max_w(px(160.))
                                    .truncate()
                                    .text_xs()
                                    .when(exited, |el| el.text_color(cx.theme().muted_foreground))
                                    .child(label),
                            )
                            .child(
                                Button::new(("close-tab", ix))
                                    .ghost()
                                    .xsmall()
                                    .icon(icon("x"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.close(ix, window, cx);
                                    })),
                            )
                    }))
                    // The button follows the last tab rather than sticking to
                    // the right edge: that is where the eye finishes reading the
                    // tabs, and a button at the other end of the panel means
                    // crossing the bar to open the next one.
                    .child(
                        Button::new("new-tab")
                            .ghost()
                            .xsmall()
                            .icon(icon("plus"))
                            .tooltip(tr!("terminal-new"))
                            // One agent profile per entry: the menu is the only
                            // place the choice arises, and a list coming from
                            // the settings saves reopening them to launch
                            // something else.
                            .dropdown_menu({
                                let entity = cx.entity();
                                move |menu, _window, cx| {
                                    let shell = entity.clone();
                                    let profiles = Settings::global(cx).terminal.agents.clone();
                                    let menu = menu.item(
                                        PopupMenuItem::new(tr!("terminal-new"))
                                            .icon(icon("plus"))
                                            .on_click(move |_, window, cx| {
                                                shell.update(cx, |this, cx| {
                                                    this.open(Launch::shell(), window, cx)
                                                });
                                            }),
                                    );
                                    if profiles.is_empty() {
                                        return menu;
                                    }
                                    profiles
                                        .into_iter()
                                        .fold(menu.separator(), |menu, profile| {
                                            let entity = entity.clone();
                                            let label =
                                                SharedString::from(profile.label().to_string());
                                            menu.item(
                                                PopupMenuItem::new(label)
                                                    .icon(icon("bot"))
                                                    .on_click(move |_, window, cx| {
                                                        entity.update(cx, |this, cx| {
                                                            this.open_profile(&profile, window, cx)
                                                        });
                                                    }),
                                            )
                                        })
                                }
                            }),
                    )
                    .child(div().flex_1()),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when_some(self.error.clone(), |el, message| {
                        el.child(
                            div()
                                .p_3()
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child(message),
                        )
                    })
                    .children(self.tabs.get(active).cloned()),
            )
    }
}

impl ClaudhubApp {
    pub(super) fn render_terminals(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(worktree) = self.active.clone() else {
            return div().into_any_element();
        };
        // The group is created on first request: opening a worktree must not
        // launch a shell nobody needs.
        let group = self.terminal_group(&worktree, window, cx);
        group.into_any_element()
    }

    /// A worktree's group, created if needed with a first tab.
    pub(super) fn terminal_group(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalGroup> {
        // Re-read on every call, so on every render of the panel: the notes root
        // is a setting, and a tab opened after changing it has to receive the
        // right folder.
        let vault = self.notes_dir(worktree, cx);
        if let Some(group) = self.terminals.get(worktree).cloned() {
            group.update(cx, |group, _| group.set_vault(vault));
            return group;
        }
        let path = worktree.to_path_buf();
        let group = cx.new(|_| TerminalGroup::new(path));
        group.update(cx, |group, cx| {
            group.set_vault(vault);
            group.open(Launch::shell(), window, cx);
        });
        self.terminals.insert(worktree.to_path_buf(), group.clone());
        group
    }
}

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
