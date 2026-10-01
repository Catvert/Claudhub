//! The settings screen.
//!
//! The form comes from gpui-component: pages, groups, search and a reset button
//! are provided, and each field is declared by a read/write pair. Those closures
//! only receive an `App` — which is what forces the settings to live in a global
//! rather than in `ClaudhubApp`.
//!
//! There is no "Apply" button: every change takes effect as it is typed and the
//! file write follows, deferred. A form asking you to confirm before seeing the
//! result makes choosing a font or a size impossible except blind.
//!
//! **A screen and not a dialog.** It was a modal window — what one reaches for
//! when there is nowhere to put a form. It covered what was being adjusted, it
//! could not be left open beside the effect it produced, and the two things one
//! comes here for, trying a theme and reading why something failed, are exactly
//! the two that want the rest of the window still in sight. The bar was already
//! there and the dock already knew how to carry a panel; what the move costs is
//! that the render closure now runs **on every frame** instead of once at
//! opening — hence `Environment`, and the cached log records.

use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::setting::{
    NumberFieldOptions, SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme, Disableable, Selectable, Sizable, StyledExt, WindowExt,
};
use gpui_kit::{
    div, prelude::*, px, Anchor, App, Context, Entity, SharedString, Subscription, Window,
};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::{
    self, LanguageChoice, Settings, ThemeMode, DEFAULT_MONO_FONT, DEFAULT_UI_FONT,
};

/// Which page the settings screen shows.
///
/// An enum and not an index: an index into the form's list is one into the
/// list its **search box has filtered**, which is not the list declared here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Page {
    #[default]
    Appearance,
    Terminal,
    Review,
    Keyboard,
    Files,
    Lsp,
    Sentry,
    Databases,
    Scripts,
    Logs,
}

impl Page {
    /// The sidebar's order, and the only place that knows it: the form is built
    /// by walking this list, so a page cannot be added to one and not the other.
    const ORDER: [Page; 10] = [
        Page::Appearance,
        Page::Terminal,
        Page::Review,
        Page::Keyboard,
        Page::Files,
        Page::Lsp,
        Page::Sentry,
        Page::Databases,
        Page::Scripts,
        Page::Logs,
    ];

    /// The heading the sidebar shows.
    ///
    /// Written **here** and given to `SettingPage::new`, rather than each page
    /// naming itself: it is also the name the form answers by — `on_select`
    /// reports a title — and two spellings of it would mean a page one could
    /// reach and never return to.
    fn title(self) -> SharedString {
        match self {
            Page::Appearance => tr!("settings-page-appearance"),
            Page::Terminal => tr!("settings-page-terminal"),
            Page::Review => tr!("settings-page-review"),
            Page::Keyboard => tr!("settings-page-keyboard"),
            Page::Files => tr!("settings-page-files"),
            Page::Lsp => tr!("settings-page-lsp"),
            Page::Sentry => tr!("settings-page-sentry"),
            Page::Databases => tr!("settings-page-databases"),
            Page::Scripts => tr!("settings-page-scripts"),
            Page::Logs => tr!("settings-page-logs"),
        }
    }

    /// The page the form says one has just chosen.
    fn of_title(title: &SharedString) -> Option<Page> {
        Page::ORDER.into_iter().find(|page| page.title() == *title)
    }
}

/// What the screen asks the system for, and asks it **once**.
///
/// The dialog this screen replaces paid for it at opening; a screen has no
/// opening, and its declaration is rebuilt on every frame. Enumerating the
/// installed fonts and stat-ing every line of `/etc/shells` at that rate is
/// filesystem work in the middle of a frame.
pub(super) struct Environment {
    /// Shared, not owned: the nine pages are built again on every render of
    /// the form, and the fixed-pitch list is read by two of them — each took
    /// a copy of a few hundred names. The menus still want a `Vec` of their
    /// own, built where they are.
    ui_fonts: Shared,
    mono_fonts: Shared,
    shells: Shared,
}

impl Environment {
    fn read(cx: &App) -> Self {
        let installed = cx.text_system().all_font_names();
        Self {
            ui_fonts: choices(settings::font_choices(&installed, false, DEFAULT_UI_FONT)).into(),
            mono_fonts: choices(settings::font_choices(&installed, true, DEFAULT_MONO_FONT)).into(),
            shells: shell_choices().into(),
        }
    }
}

/// The settings form, as an entity of its own.
///
/// **An entity and not a closure**, and that is what makes the dialog possible
/// at all: `open_dialog` keeps a `Fn` that is called back **from the root
/// view's render**, and reading the root entity from there is the panic gpui
/// refuses. A child's `render` happens after the parent's closure has returned,
/// which is the rule every dock panel already lived by — this is the same
/// arrangement, minus the dock.
///
/// It is built once and outlives the dialog: closing the settings and opening
/// them again lands on the page one had left, which is what `Page::First`
/// means.
pub(super) struct SettingsForm {
    app: gpui_kit::WeakEntity<ClaudhubApp>,
    focus: gpui_kit::FocusHandle,
}

impl SettingsForm {
    pub(super) fn new(app: &Entity<ClaudhubApp>, cx: &mut Context<Self>) -> Self {
        // The form is a picture of the settings and of the log, both of which
        // change under it while it is open.
        cx.observe(app, |_, _, cx| cx.notify()).detach();
        Self {
            app: app.downgrade(),
            focus: cx.focus_handle(),
        }
    }
}

impl gpui_kit::Focusable for SettingsForm {
    fn focus_handle(&self, _: &App) -> gpui_kit::FocusHandle {
        self.focus.clone()
    }
}

impl Render for SettingsForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        app.update(cx, |app, cx| {
            app.render_settings_form(window, cx).into_any_element()
        })
    }
}

impl ClaudhubApp {
    /// The settings, on the page one had left them.
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_settings(None, window, cx);
    }

    /// The settings, on the page being asked for.
    ///
    /// "Add a connection" comes from the "Databases" panel; answering it with a
    /// form opened on appearance leaves you hunting through a nine-entry
    /// sidebar for what you had just asked for.
    pub(super) fn open_settings_at(
        &mut self,
        page: Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_settings(Some(page), window, cx);
    }

    /// The settings screen, on a named page or on the one it was left on.
    ///
    /// The page cannot simply be written into the form on every frame:
    /// `default_selected_index` is read when the form's state is **created**.
    /// That state is a `use_keyed_state`, which lives exactly as long as the
    /// element is drawn in consecutive frames — so it dies with the dialog and
    /// is born again on the next opening, which is what made the page one had
    /// asked for once come back for ever. The id carries a counter this bumps
    /// on **every** opening, so that the page shown is the one recorded here
    /// whatever the library does with that state.
    fn show_settings(&mut self, page: Option<Page>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(page) = page {
            self.settings_page = page;
        }
        self.settings_epoch += 1;
        let form = self.settings_form.clone();
        window.open_dialog(cx, move |dialog, window, _cx| {
            // **The size is given, and given here.** In the dock the form
            // filled its panel; a dialog is sized by its content, and this
            // content is a sidebar of pages beside a column of fields — both of
            // which ask for the room they are given rather than claiming any.
            // Left alone it came up as a search box and a heading.
            //
            // Read off the window rather than fixed: `Dialog` takes a width and
            // no height, and a form taller than the window is one whose last
            // field cannot be reached. Recomputed on every frame the dialog is
            // built, so it follows a resize.
            let viewport = window.viewport_size();
            let width = viewport.width.min(px(980.)) - px(64.);
            let height = viewport.height.min(px(720.)) - px(120.);
            dialog
                .title(tr!("workspace-settings"))
                .w(width)
                // A dialog one **reads**, so it closes rather than being
                // answered: one button, the overlay dismisses, and the cross is
                // there. The confirmations of this window do the opposite for
                // the opposite reason.
                .overlay_closable(true)
                .close_button(true)
                .footer(crate::ui::dialogs::close())
                // A **definite box**, width and height: the form's root is a
                // `size_full`, which resolves against nothing at all when its
                // parent is sized by what it contains.
                .child(div().w_full().h(height).child(form.clone()))
        });
        // **Deferred, and by the dialog's own handle.** A context menu gives
        // the focus back to whatever had it as it closes, *after* the handler
        // that opened this — so a focus set at once loses the race and the
        // dialog opens with no keyboard at all. `focus_dialog` and not
        // `focus_field`: this form is a dialog of **pages**, and the field of
        // the page one leaves dies with it, taking Escape and the buttons'
        // actions with it. That is the commit on the fork this exists for.
        window.defer(cx, |window, cx| window.focus_dialog(cx));
        cx.notify();
    }

    pub(super) fn render_settings_form(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let environment = self.settings_environment(cx);
        // The registry is populated asynchronously at startup and re-read here
        // rather than cached: it is watched, and a theme file dropped in the
        // folder while Claudhub runs has to show up in the list.
        let (light_themes, dark_themes) = theme_choices(cx);
        let logs = LogView {
            records: self.log_records(),
            level: self.logs_level,
            app: cx.entity(),
        };

        // The application, for the two pages that answer a click with something
        // more than a setting: the log and the keyboard's import.
        let app = cx.entity();
        // The list is walked from `Page::ORDER`, so the sidebar's order and the
        // enum that names its entries are one thing. Building it by hand meant
        // an index recorded beside each `push`, which named the neighbour as
        // soon as a page was inserted before it, with nothing to say so.
        let pages: Vec<SettingPage> = Page::ORDER
            .into_iter()
            .map(|page| match page {
                Page::Appearance => appearance_page(&environment, &light_themes, &dark_themes),
                Page::Terminal => terminal_page(&environment),
                Page::Review => review_page(),
                Page::Keyboard => keyboard_page(app.clone()),
                Page::Files => files_page(),
                Page::Lsp => lsp_page(),
                Page::Sentry => sentry_page(),
                Page::Databases => databases_page(),
                Page::Scripts => scripts_page(self.scripts_listing()),
                // The ring itself is behind an `Rc`: what is cloned here is
                // three words.
                Page::Logs => logs_page(logs.clone()),
            })
            .collect();
        let selected = Page::ORDER
            .iter()
            .position(|page| *page == self.settings_page)
            .unwrap_or_default();
        div().size_full().child(
            gpui_kit::component::setting::Settings::new(SharedString::from(format!(
                "claudhub-settings-{}",
                self.settings_epoch
            )))
            .sidebar_width(px(190.))
            .pages(pages)
            .default_selected_index(SelectIndex {
                page_ix: selected,
                group_ix: None,
            })
            // Where the sidebar goes next. The form has no other way of saying
            // it — its selection is element state, gone the moment the dialog
            // closes.
            .on_select(move |title, _, cx| {
                let Some(page) = Page::of_title(title) else {
                    return;
                };
                app.update(cx, |app, _| app.settings_page = page);
            }),
        )
    }

    /// What the system told us, read once and kept.
    ///
    /// Emptied when the remote server answers: its `/etc/shells` is the one the
    /// form must offer, and ours names nothing over there.
    fn settings_environment(&mut self, cx: &App) -> std::rc::Rc<Environment> {
        self.settings_env
            .get_or_insert_with(|| std::rc::Rc::new(Environment::read(cx)))
            .clone()
    }

    pub(super) fn forget_settings_environment(&mut self) {
        self.settings_env = None;
    }

    /// The log records, laid out for the page, copied only when there are new
    /// ones.
    ///
    /// `logging::records` copies the ring — two thousand entries — and this page
    /// renders on every frame. Each record is turned into the strings the row
    /// paints **here**, once: formatting the timestamp and the level in the
    /// render closure was two hundred `format!` per frame. The counter is what says the copy is out of date;
    /// the buffer's own length would stop moving as soon as the ring is full.
    ///
    /// Nothing notifies the view when a record is written — a worker thread
    /// logs, it does not touch gpui — so the page follows the frames the rest of
    /// the application causes. The background sweep alone brings one every two
    /// seconds, which is what makes a log written elsewhere appear on its own.
    fn log_records(&mut self) -> std::rc::Rc<Vec<LogRow>> {
        let written = crate::logging::written();
        if self.logs_seen != written {
            self.logs_seen = written;
            self.logs =
                std::rc::Rc::new(crate::logging::records().iter().map(LogRow::of).collect());
        }
        self.logs.clone()
    }
}

/// What a menu offers: a value and the label that stands for it.
type Choices = Vec<(SharedString, SharedString)>;

/// The same, read once and handed to every render of the form.
type Shared = std::rc::Rc<[(SharedString, SharedString)]>;

fn choices(names: Vec<String>) -> Choices {
    names
        .into_iter()
        .map(|name| (SharedString::from(name.clone()), SharedString::from(name)))
        .collect()
}

// — Fields bound to one setting ——————————————————————————————————————
//
// Most fields of the form read one field of `Settings` and write it back, and
// spelled out each was eight lines naming the field twice — one of which could
// drift from the other with nothing to say so. These name it once, as a path
// (`terminal.agent_hooks`). What reads or writes anything else — a font read
// through its fallback, a table of rows — is written out where it is.

/// A switch on one flag: `switch!(vim_mode, false)`.
macro_rules! switch {
    ($($field:ident).+, $default:expr) => {
        SettingField::switch(
            |cx: &App| Settings::global(cx).$($field).+,
            |value: bool, cx: &mut App| Settings::update_global(cx, |s| s.$($field).+ = value),
        )
        .default_value($default)
    };
}

/// A text field, or a menu of `options`, on one string.
macro_rules! text {
    ($($field:ident).+, $default:expr) => {
        SettingField::input(
            |cx: &App| Settings::global(cx).$($field).+.clone().into(),
            |value: SharedString, cx: &mut App| {
                Settings::update_global(cx, |s| s.$($field).+ = value.to_string())
            },
        )
        .default_value(SharedString::from($default))
    };
    ($options:expr, $($field:ident).+, $default:expr) => {
        SettingField::dropdown(
            $options,
            |cx: &App| Settings::global(cx).$($field).+.clone().into(),
            |value: SharedString, cx: &mut App| {
                Settings::update_global(cx, |s| s.$($field).+ = value.to_string())
            },
        )
        .default_value(SharedString::from($default))
    };
}

/// A menu on one choice written as its key: `keyed!(themes, theme: ThemeMode,
/// "dark")` reads `as_key` and writes `ThemeMode::from_key`.
macro_rules! keyed {
    ($options:expr, $($field:ident).+ : $kind:ty, $default:expr) => {
        SettingField::dropdown(
            $options,
            |cx: &App| Settings::global(cx).$($field).+.as_key().into(),
            |value: SharedString, cx: &mut App| {
                Settings::update_global(cx, |s| s.$($field).+ = <$kind>::from_key(&value))
            },
        )
        .default_value(SharedString::from($default))
    };
}

/// A number on one numeric field; `into` turns what was typed into the
/// field's type, bounds included.
macro_rules! number {
    ($options:expr, $($field:ident).+, $default:expr, $into:expr) => {
        SettingField::number_input(
            $options,
            |cx: &App| Settings::global(cx).$($field).+ as f64,
            |value: f64, cx: &mut App| {
                Settings::update_global(cx, |s| s.$($field).+ = ($into)(value))
            },
        )
        .default_value($default)
    };
}

/// The bounds of a number field.
fn bounds(min: f64, max: f64, step: f64) -> NumberFieldOptions {
    NumberFieldOptions { min, max, step }
}

/// Shells the system declares. The menu only offers them: the field stays free,
/// and empty always means "the login shell".
fn shell_choices() -> Vec<(SharedString, SharedString)> {
    choices(settings::available_shells())
}

/// The state of the "shell" field, kept from one render to the next.
///
/// The subscription lives inside it: a dropped `Subscription` is cut, and the
/// field would stop writing into the settings from the next frame on.
struct ShellField {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// The shell is typed freely, and the menu only offers.
///
/// A closed list would do if `/etc/shells` told the truth; it ignores everything
/// not installed by the system — a shell compiled by hand, a `nix run`, a `tmux
/// new-session` — and we do not want a setting you leave by editing a JSON file.
fn shell_item(shells: Shared) -> SettingItem {
    SettingItem::new(
        tr!("settings-shell"),
        SettingField::render(move |_, window, cx| {
            let shells = shells.clone();
            let state = window.use_keyed_state("claudhub-shell", cx, |window, cx| {
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(tr!("settings-shell-default"))
                        .default_value(Settings::global(cx).terminal.shell.clone())
                });
                let subscription = cx.subscribe(
                    &input,
                    |_: &mut ShellField, input, event: &InputEvent, cx| {
                        if !matches!(event, InputEvent::Change) {
                            return;
                        }
                        let value = input.read(cx).value().to_string();
                        Settings::update_global(cx, |s| s.terminal.shell = value);
                    },
                );
                ShellField {
                    input,
                    _subscription: subscription,
                }
            });
            let input = state.read(cx).input.clone();
            let for_menu = input.clone();
            h_flex()
                .w(px(300.))
                .gap_1()
                .child(div().flex_1().child(Input::new(&input).small()))
                .when(!shells.is_empty(), |el| {
                    el.child(
                        Button::new("detected-shells")
                            .outline()
                            .small()
                            .icon(icon("chevron-down"))
                            .tooltip(tr!("settings-shell-detected"))
                            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                                shells.iter().fold(menu, |menu, (value, label)| {
                                    let (input, value) = (for_menu.clone(), value.clone());
                                    menu.item(PopupMenuItem::new(label.clone()).on_click(
                                        move |_, window, cx| {
                                            input.update(cx, |state, cx| {
                                                state.set_value(value.clone(), window, cx)
                                            });
                                        },
                                    ))
                                })
                            }),
                    )
                })
        }),
    )
    .description(tr!("settings-shell-help"))
}

/// A row of a profiles table: a name, a command line and an environment.
///
/// Two lists have that shape — the terminal's agent profiles and the chat
/// agents — and one table serves both: the same three fields, the same keyed
/// state, the same stale-index guard.
trait Profile: Default + Clone + 'static {
    fn name(&self) -> String;
    fn set_name(&mut self, name: String);
    fn command_line(&self) -> String;
    /// The line is split honouring quotes: a path containing a space must not
    /// become two arguments.
    fn set_command_line(&mut self, line: &str);
    fn env_line(&self) -> String;
    fn set_env_line(&mut self, line: &str);
}

impl Profile for settings::AgentProfile {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn set_name(&mut self, name: String) {
        self.name = name;
    }
    fn command_line(&self) -> String {
        settings::AgentProfile::command_line(self)
    }
    fn set_command_line(&mut self, line: &str) {
        let mut parts = settings::split_command(line).into_iter();
        self.command = parts.next().unwrap_or_default();
        self.args = parts.collect();
    }
    fn env_line(&self) -> String {
        settings::AgentProfile::env_line(self)
    }
    fn set_env_line(&mut self, line: &str) {
        settings::AgentProfile::set_env_line(self, line)
    }
}

impl Profile for crate::acp::Agent {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn set_name(&mut self, name: String) {
        self.name = name;
    }
    fn command_line(&self) -> String {
        crate::cmdline::join_command(
            std::iter::once(self.command.as_str()).chain(self.args.iter().map(String::as_str)),
        )
    }
    fn set_command_line(&mut self, line: &str) {
        let mut parts = crate::cmdline::split_command(line).into_iter();
        self.command = parts.next().unwrap_or_default();
        self.args = parts.collect();
    }
    fn env_line(&self) -> String {
        crate::cmdline::join_command(self.env.iter().map(|(key, value)| format!("{key}={value}")))
    }
    fn set_env_line(&mut self, line: &str) {
        self.env = crate::cmdline::split_command(line)
            .into_iter()
            .filter_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                (!key.is_empty()).then(|| (key.to_string(), value.to_string()))
            })
            .collect();
    }
}

/// Which list a profiles table edits, and how its rows are keyed.
struct Profiles<P: Profile> {
    /// Prefix of the rows' state keys — two tables must not share one.
    key: &'static str,
    list: fn(&Settings) -> &Vec<P>,
    list_mut: fn(&mut Settings) -> &mut Vec<P>,
}

// By hand: a derive would ask `P: Copy`, and a profile is not.
impl<P: Profile> Clone for Profiles<P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: Profile> Copy for Profiles<P> {}

impl<P: Profile> Profiles<P> {
    /// Changes a profile in place, if the index still exists.
    ///
    /// The index may be one frame stale: a subscription set up for row 2
    /// outlives row 2's disappearance, and writing out of bounds would panic
    /// in the middle of a render.
    fn edit(self, index: usize, cx: &mut App, edit: impl FnOnce(&mut P)) {
        Settings::update_global(cx, |s| {
            if let Some(profile) = (self.list_mut)(s).get_mut(index) {
                edit(profile);
            }
        });
    }
}

/// The terminal's agent profiles.
const AGENT_PROFILES: Profiles<settings::AgentProfile> = Profiles {
    key: "claudhub-agent",
    list: |s| &s.terminal.agents,
    list_mut: |s| &mut s.terminal.agents,
};

/// The agents a chat tab speaks to over ACP.
const CHAT_AGENTS: Profiles<crate::acp::Agent> = Profiles {
    key: "claudhub-chat-agent",
    list: |s| &s.terminal.chat_agents,
    list_mut: |s| &mut s.terminal.chat_agents,
};

/// The state of one row of a profiles table, kept from one render to the next.
///
/// The three subscriptions live inside it: dropped, they would be cut and the
/// fields would stop writing into the settings on the next frame.
struct AgentField {
    name: Entity<InputState>,
    command: Entity<InputState>,
    env: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

/// A profiles table.
///
/// A bespoke field because there is nothing like it in gpui-component's form:
/// these are rows added and removed, with three inputs each.
///
/// **The state key carries the number of profiles** (`claudhub-agent-{n}-{i}`).
/// `use_keyed_state` keeps one state per key: without the count, deleting the
/// first profile would leave row 0's fields filled with the old one, and we
/// would write into the settings what we thought we had deleted. Renaming a
/// profile, on the other hand, does not change the count — so the fields keep
/// their cursor while typing.
fn profiles_item<P: Profile>(
    profiles: Profiles<P>,
    title: SharedString,
    help: SharedString,
    add: SharedString,
) -> SettingItem {
    SettingItem::new(
        title,
        SettingField::render(move |_, window, cx| {
            let list = (profiles.list)(Settings::global(cx)).clone();
            let count = list.len();
            let rows: Vec<_> = list
                .iter()
                .enumerate()
                .map(|(index, profile)| profile_row(profiles, index, count, profile, window, cx))
                .collect();
            v_flex().w(px(460.)).gap_1().children(rows).child(
                h_flex().child(
                    Button::new(SharedString::from(format!("{}-add", profiles.key)))
                        .outline()
                        .small()
                        .icon(icon("plus"))
                        .label(add.clone())
                        .on_click(move |_, _window, cx| {
                            Settings::update_global(cx, |s| {
                                (profiles.list_mut)(s).push(P::default())
                            });
                        }),
                ),
            )
        }),
    )
    .description(help)
}

fn agents_item() -> SettingItem {
    profiles_item(
        AGENT_PROFILES,
        tr!("settings-agents"),
        tr!("settings-agents-help"),
        tr!("settings-agent-add"),
    )
}

fn chat_agents_item() -> SettingItem {
    profiles_item(
        CHAT_AGENTS,
        tr!("settings-chat-agents"),
        tr!("settings-chat-agents-help"),
        tr!("settings-chat-agent-add"),
    )
}

/// The agents shipped, back in the table — a row whose command was left
/// empty offers no chat, and retyping an npm package name is not a gesture
/// anyone should have to know.
fn chat_agents_reset_item() -> SettingItem {
    SettingItem::new(
        tr!("settings-chat-agents-reset"),
        SettingField::render(|_, _window, _cx| {
            Button::new("chat-agents-reset")
                .outline()
                .small()
                .icon(icon("refresh-cw"))
                .label(tr!("settings-chat-agents-reset-button"))
                .on_click(|_, _window, cx| {
                    Settings::update_global(cx, |s| {
                        s.terminal.chat_agents = crate::acp::Agent::defaults();
                    });
                })
        }),
    )
    .description(tr!("settings-chat-agents-reset-help"))
}

fn profile_row<P: Profile>(
    profiles: Profiles<P>,
    index: usize,
    count: usize,
    profile: &P,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let key = format!("{}-{count}-{index}", profiles.key);
    let (name, command, env) = (profile.name(), profile.command_line(), profile.env_line());
    let state = window.use_keyed_state(SharedString::from(key), cx, move |window, cx| {
        let name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("settings-agent-name"))
                .default_value(name)
        });
        let command_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("settings-agent-command"))
                .default_value(command)
        });
        let env_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("settings-agent-env"))
                .default_value(env)
        });
        let subscriptions = vec![
            cx.subscribe(
                &name_input,
                move |_: &mut AgentField, input, event: &InputEvent, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let value = input.read(cx).value().to_string();
                    profiles.edit(index, cx, |profile| profile.set_name(value));
                },
            ),
            cx.subscribe(
                &command_input,
                move |_: &mut AgentField, input, event: &InputEvent, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let value = input.read(cx).value().to_string();
                    profiles.edit(index, cx, |profile| profile.set_command_line(&value));
                },
            ),
            cx.subscribe(
                &env_input,
                move |_: &mut AgentField, input, event: &InputEvent, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let value = input.read(cx).value().to_string();
                    profiles.edit(index, cx, |profile| profile.set_env_line(&value));
                },
            ),
        ];
        AgentField {
            name: name_input,
            command: command_input,
            env: env_input,
            _subscriptions: subscriptions,
        }
    });
    let field = state.read(cx);
    let (name, command, env) = (field.name.clone(), field.command.clone(), field.env.clone());
    h_flex()
        .gap_1()
        .items_center()
        .child(div().w(px(90.)).child(Input::new(&name).small()))
        .child(div().flex_1().child(Input::new(&command).small()))
        .child(div().w(px(130.)).child(Input::new(&env).small()))
        .child(
            Button::new((
                SharedString::from(format!("{}-remove", profiles.key)),
                index,
            ))
            .ghost()
            .small()
            .icon(icon("trash-2"))
            .tooltip(tr!("settings-agent-remove"))
            .on_click(move |_, _window, cx| {
                Settings::update_global(cx, |s| {
                    let list = (profiles.list_mut)(s);
                    if index < list.len() {
                        list.remove(index);
                    }
                });
            }),
        )
}

/// The profile launched when nobody says which.
///
/// The list of choices is read again on every render of the form: it changes
/// while the table just above is being edited.
fn default_agent_item() -> SettingItem {
    SettingItem::new(
        tr!("settings-default-agent"),
        SettingField::render(|_, _window, cx| {
            let profiles = Settings::global(cx).terminal.agents.clone();
            let current = Settings::global(cx)
                .terminal
                .default_profile()
                .map(|profile| profile.label().to_string())
                .unwrap_or_default();
            Button::new("default-agent")
                .outline()
                .small()
                .label(SharedString::from(current))
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                    profiles.iter().fold(menu, |menu, profile| {
                        let label = SharedString::from(profile.label().to_string());
                        let chosen = label.clone();
                        menu.item(PopupMenuItem::new(label).on_click(move |_, _window, cx| {
                            let chosen = chosen.to_string();
                            Settings::update_global(cx, |s| s.terminal.default_agent = chosen);
                        }))
                    })
                })
        }),
    )
    .description(tr!("settings-default-agent-help"))
}

/// The registry's palettes, light ones and dark ones apart.
///
/// One reading of the registry and not one per appearance: `sorted_themes`
/// copies and sorts the whole list, and this runs on every frame the settings
/// screen paints.
fn theme_choices(cx: &App) -> (Choices, Choices) {
    let mut light: Choices = Vec::new();
    let mut dark: Choices = Vec::new();
    for theme in gpui_kit::component::ThemeRegistry::global(cx).sorted_themes() {
        let choice = (theme.name.clone(), theme.name.clone());
        match theme.mode {
            gpui_kit::component::ThemeMode::Light => light.push(choice),
            gpui_kit::component::ThemeMode::Dark => dark.push(choice),
        }
    }
    (light, dark)
}

fn appearance_page(
    environment: &Environment,
    light_themes: &Choices,
    dark_themes: &Choices,
) -> SettingPage {
    let themes = vec![
        (SharedString::from("dark"), tr!("settings-theme-dark")),
        (SharedString::from("light"), tr!("settings-theme-light")),
        (SharedString::from("system"), tr!("settings-theme-system")),
    ];
    let languages = vec![
        (
            SharedString::from("system"),
            tr!("settings-language-system"),
        ),
        (SharedString::from("fr"), SharedString::from("Français")),
        (SharedString::from("en"), SharedString::from("English")),
    ];

    SettingPage::new(Page::Appearance.title())
        .default_open(true)
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-theme"))
                .item(
                    SettingItem::new(
                        tr!("settings-theme"),
                        keyed!(themes, theme: ThemeMode, "dark"),
                    )
                    .description(tr!("settings-theme-help")),
                )
                .item(
                    SettingItem::new(
                        tr!("settings-dark-theme"),
                        text!(
                            dark_themes.clone(),
                            dark_theme,
                            settings::DEFAULT_DARK_THEME
                        ),
                    )
                    .description(tr!("settings-palette-help")),
                )
                .item(SettingItem::new(
                    tr!("settings-light-theme"),
                    text!(
                        light_themes.clone(),
                        light_theme,
                        settings::DEFAULT_LIGHT_THEME
                    ),
                ))
                .item(
                    SettingItem::new(
                        tr!("settings-language"),
                        keyed!(languages, language: LanguageChoice, "system"),
                    )
                    .description(tr!("settings-language-help")),
                ),
        )
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-fonts"))
                // The two families are read through their fallback, and so
                // are written out rather than bound to a field.
                .item(
                    SettingItem::new(
                        tr!("settings-ui-font"),
                        SettingField::dropdown(
                            environment.ui_fonts.to_vec(),
                            |cx: &App| Settings::global(cx).ui_font().to_string().into(),
                            |value: SharedString, cx: &mut App| {
                                Settings::update_global(cx, |s| {
                                    s.ui_font_family = value.to_string()
                                })
                            },
                        )
                        .default_value(SharedString::from(DEFAULT_UI_FONT)),
                    )
                    .description(tr!("settings-ui-font-help")),
                )
                .item(SettingItem::new(
                    tr!("settings-ui-font-size"),
                    number!(size_range(), font_size, 14.0, clamp_size),
                ))
                .item(
                    SettingItem::new(
                        tr!("settings-mono-font"),
                        SettingField::dropdown(
                            environment.mono_fonts.to_vec(),
                            |cx: &App| Settings::global(cx).mono_font().to_string().into(),
                            |value: SharedString, cx: &mut App| {
                                Settings::update_global(cx, |s| {
                                    s.mono_font_family = value.to_string()
                                })
                            },
                        )
                        .default_value(SharedString::from(DEFAULT_MONO_FONT)),
                    )
                    .description(tr!("settings-mono-font-help")),
                )
                .item(SettingItem::new(
                    tr!("settings-diff-font-size"),
                    number!(size_range(), diff_font_size, 13.0, clamp_size),
                )),
        )
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-window"))
                .item(
                    SettingItem::new(tr!("settings-maximized"), switch!(start_maximized, false))
                        .description(tr!("settings-maximized-help")),
                ),
        )
}

fn terminal_page(environment: &Environment) -> SettingPage {
    // The terminal's font reuses the fixed-pitch list, preceded by the "same as
    // the diffs" entry: the terminal is allowed not to choose, so that setting
    // the fixed pitch once is enough for the common case.
    let fonts: Choices = std::iter::once((SharedString::default(), tr!("settings-font-inherit")))
        .chain(environment.mono_fonts.iter().cloned())
        .collect();
    let placements = vec![
        (
            SharedString::from("bottom"),
            tr!("settings-terminal-placement-bottom"),
        ),
        (
            SharedString::from("right"),
            tr!("settings-terminal-placement-right"),
        ),
    ];

    SettingPage::new(Page::Terminal.title())
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-shell"))
                .item(shell_item(environment.shells.clone()))
                .item(agents_item())
                .item(default_agent_item())
                .item(chat_agents_item())
                .item(chat_agents_reset_item())
                .item(
                    SettingItem::new(
                        tr!("settings-agent-hooks"),
                        switch!(terminal.agent_hooks, true),
                    )
                    .description(tr!("settings-agent-hooks-help")),
                ),
        )
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-terminal-display"))
                .item(
                    SettingItem::new(
                        tr!("settings-terminal-placement"),
                        keyed!(
                            placements,
                            terminal.placement: settings::TerminalPlacement,
                            "bottom"
                        ),
                    )
                    .description(tr!("settings-terminal-placement-help")),
                )
                .item(SettingItem::new(
                    tr!("settings-terminal-font"),
                    text!(fonts, terminal.font_family, SharedString::default()),
                ))
                .item(SettingItem::new(
                    tr!("settings-terminal-font-size"),
                    number!(size_range(), terminal.font_size, 13.0, clamp_size),
                ))
                .item(
                    SettingItem::new(
                        tr!("settings-scrollback"),
                        number!(
                            bounds(0., 200_000., 1_000.),
                            terminal.scrollback,
                            10_000.0,
                            |value: f64| value.clamp(0., 200_000.) as usize
                        ),
                    )
                    .description(tr!("settings-scrollback-help")),
                )
                .item(
                    SettingItem::new(
                        tr!("settings-column-min"),
                        number!(
                            bounds(
                                crate::ui::settings::COLUMN_MIN_RANGE.0 as f64,
                                crate::ui::settings::COLUMN_MIN_RANGE.1 as f64,
                                20.,
                            ),
                            terminal.column_min,
                            crate::ui::settings::COLUMN_MIN_DEFAULT as f64,
                            |value: f64| {
                                let (least, most) = crate::ui::settings::COLUMN_MIN_RANGE;
                                (value as f32).clamp(least, most)
                            }
                        ),
                    )
                    .description(tr!("settings-column-min-help")),
                ),
        )
}

/// The keyboard.
///
/// A short page, and that is accepted: vim mode changes the meaning of half the
/// keys, and it is the first place one goes looking for it. The reminder about
/// `F1` is there because help you cannot find is not help.
fn keyboard_page(app: Entity<ClaudhubApp>) -> SettingPage {
    SettingPage::new(Page::Keyboard.title())
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-vim"))
                .item(
                    SettingItem::new(tr!("settings-vim-mode"), switch!(vim_mode, false))
                        .description(tr!("settings-vim-mode-help")),
                )
                .item(
                    SettingItem::new(tr!("settings-vim-clipboard"), switch!(vim_clipboard, false))
                        .description(tr!("settings-vim-clipboard-help")),
                ),
        )
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-shortcuts"))
                .item(shortcuts_item(app)),
        )
}

/// Bumped when the whole table moves under the rows.
///
/// A row's field is an `InputState` built **once**, from the keys the binding
/// had then; nothing tells it they have all just changed, and an import that
/// left the fields showing the old keys would read as an import that did
/// nothing. The key the state is filed under moves instead, so the next frame
/// builds them again. Not a counter on the settings: this is a fact about the
/// rows on screen, and it must reach a `&mut App` that knows nothing else.
static SHORTCUTS_GENERATION: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// VS Code's keyboard, written over ours.
///
/// **The file is read from here**, which is the thread that draws — the rule it
/// bends is about git commands, not about a form reading four kilobytes on a
/// click. It has to be here for the same reason the keyring does: a VS Code
/// configuration belongs to the desktop session, which is the Windows side when
/// the workers are running in WSL, and a worker looking for it over there would
/// find nothing and say the user has no VS Code.
fn import_vscode(app: &Entity<ClaudhubApp>, cx: &mut App) {
    let file = directories::BaseDirs::new()
        .map(|dirs| dirs.config_dir().to_path_buf())
        .into_iter()
        .flat_map(|config| crate::ui::vscode::candidates(&config))
        .find(|path| path.exists());
    if let Some(path) = &file {
        log::info!("importing the VS Code keyboard from {}", path.display());
    }
    // No file is not a failure: VS Code's own defaults are what somebody who
    // never opened its keyboard settings means by importing it.
    let text = file
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok());
    let keymap = crate::ui::vscode::keymap(text.as_deref());
    let mut moved = 0;
    Settings::update_global(cx, |settings| {
        moved = keymap.apply(&mut settings.shortcuts);
    });
    crate::ui::shortcuts::rebind(cx);
    SHORTCUTS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    app.update(cx, |app, cx| {
        app.announce(tr!("settings-shortcuts-imported", { count: moved }), cx)
    });
}

/// Every binding, editable.
///
/// **A `SettingItem::render`**, like the databases and for the same reason: an
/// ordinary item cuts its field to four hundred pixels, and a row here is a
/// label, a field and two buttons across the page.
///
/// The list is `shortcuts::all()` — the table both the keymap and the help come
/// out of. A second list would have diverged on the first addition, which is
/// the whole point of that module.
fn shortcuts_item(app: Entity<ClaudhubApp>) -> SettingItem {
    SettingItem::render(move |_, window, cx| {
        let app = app.clone();
        let overrides = Settings::global(cx).shortcuts.clone();
        let vim = Settings::global(cx).vim_mode;
        // Which keys are claimed twice under the same predicate. Counted once
        // here and not per row: a duplicate is settled by declaration order,
        // which is never what was meant, and nothing else would say so.
        let mut claimed: std::collections::HashMap<(&str, String), usize> =
            std::collections::HashMap::new();
        for entry in crate::ui::shortcuts::all() {
            let keys = entry.effective(&overrides).trim().to_string();
            if !keys.is_empty() {
                *claimed.entry((entry.predicate, keys)).or_default() += 1;
            }
        }

        let mut rows: Vec<gpui_kit::AnyElement> = Vec::new();
        for group in crate::ui::shortcuts::Group::ORDER {
            let family: Vec<_> = crate::ui::shortcuts::all()
                .filter(|entry| entry.group == group)
                .collect();
            if family.is_empty() {
                continue;
            }
            rows.push(
                div()
                    .pt_2()
                    .text_sm()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!(group.key()))
                    .into_any_element(),
            );
            for entry in family {
                let row = crate::ui::shortcuts::Setting::of(entry, &overrides, vim);
                let conflict = !row.keys.is_empty()
                    && claimed
                        .get(&(entry.predicate, row.keys.clone()))
                        .is_some_and(|count| *count > 1);
                rows.push(shortcut_row(entry, &row, conflict, window, cx).into_any_element());
            }
        }
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_start()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("settings-shortcuts-help")),
                    )
                    // The keyboard one already has, for whoever comes from the
                    // editor next door. No confirmation asked: it writes only
                    // the bindings it speaks for, each row keeps its own way
                    // back, and a dialog inside a dialog is a focus one loses.
                    .child(
                        Button::new("import-vscode")
                            .small()
                            .outline()
                            .label(tr!("settings-shortcuts-import-vscode"))
                            .tooltip(tr!("settings-shortcuts-import-vscode-help"))
                            .on_click(move |_, _window, cx| import_vscode(&app, cx)),
                    ),
            )
            .children(rows)
    })
}

/// Waits for the next keystroke and records it for that binding.
///
/// **A keystroke interceptor and not a focused element.** An interceptor runs
/// *before* the keymap, and `stop_propagation` there stops the dispatch dead:
/// pressing `Ctrl+T` to record it must not also hide the terminals. Doing it
/// with a focusable capture zone would have meant a context excluded from every
/// predicate — eight strings to keep in step, one forgotten being a shortcut
/// firing while one records it.
///
/// Escape gives up, a modifier held on its own is waited out, and everything
/// else is written to the field, to the settings and to the keymap at once.
fn start_capture(
    entry: &'static crate::ui::shortcuts::Entry,
    state: &Entity<ShortcutField>,
    cx: &mut App,
) {
    if state.read(cx).capturing {
        state.update(cx, |field, _| field.capturing = false);
        return;
    }
    let already = state.read(cx)._capture.is_some();
    state.update(cx, |field, _| field.capturing = true);
    if already {
        return;
    }
    let handle = state.clone();
    let subscription = cx.intercept_keystrokes(move |event, window, cx| {
        if !handle.read(cx).capturing {
            return;
        }
        // The key belongs to the capture, whatever it is bound to elsewhere.
        cx.stop_propagation();
        let Some(keys) = crate::ui::shortcuts::stroke_syntax(&event.keystroke) else {
            return; // a modifier on its own: wait for the key it qualifies
        };
        handle.update(cx, |field, _| field.capturing = false);
        if keys == "escape" {
            return;
        }
        let input = handle.read(cx).keys.clone();
        input.update(cx, |input, cx| input.set_value(keys.clone(), window, cx));
        Settings::update_global(cx, |settings| {
            if keys == entry.keys {
                settings.shortcuts.remove(&entry.id());
            } else {
                settings.shortcuts.insert(entry.id(), keys.clone());
            }
        });
        crate::ui::shortcuts::rebind(cx);
    });
    state.update(cx, |field, _| field._capture = Some(subscription));
}

/// A binding's field, kept from one render to the next.
struct ShortcutField {
    keys: Entity<InputState>,
    /// Waiting for the key to record.
    capturing: bool,
    /// The keystroke interceptor, installed on the first capture and kept
    /// afterwards: `capturing` is what turns it on and off. Dropping a
    /// subscription from inside its own callback is not a thing to try.
    _capture: Option<Subscription>,
    _subscription: Subscription,
}

/// One binding: what it does, the keys it answers to, and the way back.
///
/// **The state's key is the binding's id**, which never moves: the list is
/// neither added to nor reordered while the window is open. What it does carry
/// is `SHORTCUTS_GENERATION`, and only for the one gesture that changes every
/// row at once. Resetting writes into the field itself — the state would
/// otherwise keep the text one has just abandoned.
fn shortcut_row(
    entry: &'static crate::ui::shortcuts::Entry,
    row: &crate::ui::shortcuts::Setting,
    conflict: bool,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    // Everything the row says about the binding is decided once, for the whole
    // list, by `shortcuts::Row`: reading the overrides here meant a copy of the
    // map per row and per frame.
    let crate::ui::shortcuts::Setting {
        id,
        keys: current,
        customised,
        invalid,
        idle,
    } = row.clone();

    let generation = SHORTCUTS_GENERATION.load(std::sync::atomic::Ordering::Relaxed);
    let state = window.use_keyed_state(
        SharedString::from(format!("claudhub-shortcut-{generation}-{id}")),
        cx,
        move |window, cx| {
            let keys = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(SharedString::from(entry.keys))
                    .default_value(current.clone())
            });
            let subscription =
                cx.subscribe(&keys, move |_: &mut ShortcutField, input, event, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let value = input.read(cx).value().trim().to_string();
                    // What does not read is left in the field and not written:
                    // the row says so, and the keymap keeps what it had.
                    if !crate::ui::shortcuts::valid_keys(&value) {
                        return;
                    }
                    Settings::update_global(cx, |settings| {
                        if value == entry.keys {
                            settings.shortcuts.remove(&entry.id());
                        } else {
                            settings.shortcuts.insert(entry.id(), value.clone());
                        }
                    });
                    // The keymap is rebuilt at once: a shortcut one has to
                    // restart to try is a shortcut one sets blind.
                    crate::ui::shortcuts::rebind(cx);
                });
            ShortcutField {
                keys,
                capturing: false,
                _capture: None,
                _subscription: subscription,
            }
        },
    );
    let input = state.read(cx).keys.clone();
    let for_reset = input.clone();
    let capturing = state.read(cx).capturing;
    let for_capture = state.clone();

    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .when(idle, |el| el.text_color(cx.theme().muted_foreground))
                .child(tr!(entry.label)),
        )
        .when(idle, |el| {
            el.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("settings-shortcut-vim-only")),
            )
        })
        // The warnings are read, not guessed: a key gpui cannot make sense of,
        // and a key two bindings claim under the same predicate — which
        // declaration order settles, silently.
        .when(invalid, |el| {
            el.child(
                icon("triangle-alert")
                    .xsmall()
                    .text_color(cx.theme().danger),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(tr!("settings-shortcut-invalid")),
            )
        })
        .when(conflict && !invalid, |el| {
            el.child(
                icon("triangle-alert")
                    .xsmall()
                    .text_color(cx.theme().warning),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(tr!("settings-shortcut-conflict")),
            )
        })
        .child(div().w(px(180.)).child(Input::new(&input).small()))
        // Pressing the keys rather than spelling them, which is what one wants
        // nine times out of ten; the field stays for the tenth — a sequence
        // (`g g`) has no single keystroke to capture, and emptying it is how a
        // binding is switched off.
        .child(
            Button::new(SharedString::from(format!("capture-{id}")))
                .small()
                .when(capturing, |el| el.primary())
                .when(!capturing, |el| el.outline())
                .label(if capturing {
                    tr!("settings-shortcut-capturing")
                } else {
                    tr!("settings-shortcut-capture")
                })
                .on_click(move |_, _window, cx| {
                    start_capture(entry, &for_capture, cx);
                }),
        )
        .child(
            Button::new(SharedString::from(format!("reset-{id}")))
                .ghost()
                .small()
                .icon(icon("undo-2"))
                .tooltip(tr!("settings-shortcut-reset"))
                .disabled(!customised)
                .on_click(move |_, window, cx| {
                    Settings::update_global(cx, |settings| {
                        settings.shortcuts.remove(&entry.id());
                    });
                    for_reset.update(cx, |input, cx| {
                        input.set_value(entry.keys, window, cx);
                    });
                    crate::ui::shortcuts::rebind(cx);
                }),
        )
}

fn review_page() -> SettingPage {
    SettingPage::new(Page::Review.title())
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-integration"))
                .item(
                    SettingItem::new(
                        tr!("settings-update-rebase"),
                        switch!(update_with_rebase, false),
                    )
                    .description(tr!("settings-update-rebase-help")),
                )
                .item(
                    SettingItem::new(
                        tr!("settings-integrate-no-ff"),
                        switch!(integrate_no_ff, true),
                    )
                    .description(tr!("settings-integrate-no-ff-help")),
                )
                .item(
                    SettingItem::new(
                        tr!("settings-commit-message"),
                        text!(
                            commit_message_command,
                            crate::ui::settings::DEFAULT_COMMIT_MESSAGE_COMMAND
                        ),
                    )
                    .description(tr!("settings-commit-message-help")),
                )
                .item(
                    SettingItem::new(
                        tr!("settings-auto-fetch"),
                        number!(
                            bounds(0., 240., 5.),
                            auto_fetch_minutes,
                            10.0,
                            |value: f64| value.clamp(0., 240.) as u32
                        ),
                    )
                    .description(tr!("settings-auto-fetch-help")),
                ),
        )
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-diff"))
                .item(
                    SettingItem::new(
                        tr!("settings-diff-context"),
                        number!(bounds(0., 50., 1.), diff_context, 3.0, |value: f64| {
                            value.clamp(0., 50.) as usize
                        }),
                    )
                    .description(tr!("settings-diff-context-help")),
                )
                .item(SettingItem::render(|_, _, cx| {
                    div()
                        .text_xs()
                        .text_color(gpui_kit::component::ActiveTheme::theme(cx).muted_foreground)
                        .child(tr!("settings-diff-context-note"))
                }))
                .item(
                    SettingItem::new(
                        tr!("settings-diff-whole-file"),
                        switch!(diff_whole_file, false),
                    )
                    .description(tr!("settings-diff-whole-file-help")),
                )
                .item(
                    SettingItem::new(tr!("settings-diff-split"), switch!(diff_split, false))
                        .description(tr!("settings-diff-split-help")),
                ),
        )
}

fn files_page() -> SettingPage {
    SettingPage::new(Page::Files.title()).group(
        SettingGroup::new()
            .item(
                SettingItem::new(
                    tr!("settings-external-editor"),
                    text!(external_editor, SharedString::default()),
                )
                .description(tr!("settings-external-editor-help")),
            )
            .item(
                SettingItem::new(
                    tr!("settings-notes-dir"),
                    text!(notes_dir, SharedString::default()),
                )
                .description(tr!("settings-notes-dir-help")),
            )
            // The distribution is only chosen here after the fact: the question
            // is asked on first startup, where no setting is known yet. The
            // field exists to change it — a machine often has several — and the
            // change only takes effect on the next launch, the running server
            // already having its repositories open.
            .item(
                SettingItem::new(
                    tr!("settings-wsl-distro"),
                    text!(wsl_distro, SharedString::default()),
                )
                .description(tr!("settings-wsl-distro-help")),
            )
            .item(
                SettingItem::new(
                    tr!("settings-show-ignored"),
                    switch!(show_ignored_files, false),
                )
                .description(tr!("settings-show-ignored-help")),
            )
            .item(
                SettingItem::new(tr!("settings-save-all"), switch!(save_all_tabs, true))
                    .description(tr!("settings-save-all-help")),
            )
            .item(
                SettingItem::new(
                    tr!("settings-preview-tab"),
                    switch!(editor_preview_tab, true),
                )
                .description(tr!("settings-preview-tab-help")),
            )
            .item(
                SettingItem::new(
                    tr!("settings-tab-limit"),
                    number!(
                        bounds(0., 100., 1.),
                        editor_tab_limit,
                        10.0,
                        |value: f64| { value.clamp(0., 100.) as usize }
                    ),
                )
                .description(tr!("settings-tab-limit-help")),
            ),
    )
}

/// Sentry: the organisation and the token. The **project** belongs to the
/// repository and lives in the state store, not here — two repositories of the
/// same organisation do not have the same errors.
/// The databases page.
///
/// Connections are declared here and nowhere else: it is the second level of the
/// extension system — a declaration, not code — the same as the agent profiles',
/// and the "Databases" panel is only the view of that list.
/// The Sentry views' settings: the account, and what is asked of it.
///
/// The **project** is not here, and that is the whole distinction: the
/// organisation and the token belong to the machine, the project belongs to the
/// repository — five checkouts of one code have the same errors. Its field is
/// on the panel, which is also where one is when one decides to change it.
fn sentry_page() -> SettingPage {
    SettingPage::new(Page::Sentry.title()).group(
        SettingGroup::new()
            .item(SettingItem::new(
                tr!("settings-sentry-org"),
                text!(sentry_org, SharedString::default()),
            ))
            .item(
                SettingItem::new(
                    tr!("settings-sentry-token"),
                    text!(sentry_token, SharedString::default()),
                )
                .description(tr!("settings-sentry-token-help")),
            )
            .item(
                SettingItem::new(
                    tr!("settings-sentry-host"),
                    text!(sentry_host, SharedString::default()),
                )
                .description(tr!("settings-sentry-host-help")),
            )
            .item(
                SettingItem::new(
                    tr!("settings-sentry-query"),
                    text!(sentry_query, SharedString::default()),
                )
                .description(tr!("settings-sentry-query-help")),
            )
            .item(
                SettingItem::new(
                    tr!("settings-sentry-intro"),
                    text!(sentry_intro, SharedString::default()),
                )
                .description(tr!("settings-sentry-intro-help")),
            ),
    )
}

/// What the scripts page shows of the scripts' folder, read from the
/// application when the form is built.
struct ScriptsListing {
    /// The home's menu: Claudhub's own, then each script of the `home` kind.
    homes: Choices,
    /// `(title, id, kind)` of each script found.
    found: Vec<(SharedString, SharedString, SharedString)>,
    /// `(id, why)` of each folder that meant to be one.
    broken: Vec<(SharedString, SharedString)>,
}

impl ClaudhubApp {
    fn scripts_listing(&self) -> ScriptsListing {
        use crate::scripts::Kind;
        let homes = std::iter::once((SharedString::default(), tr!("settings-home-builtin")))
            .chain(self.scripts.of_kind(Kind::Home).map(|script| {
                (
                    SharedString::from(script.id.clone()),
                    SharedString::from(script.title.clone()),
                )
            }))
            .collect();
        let found = [Kind::Home, Kind::Tab]
            .into_iter()
            .flat_map(|kind| self.scripts.of_kind(kind))
            .map(|script| {
                let kind = match script.kind {
                    Kind::Home => tr!("settings-script-home"),
                    Kind::Tab => tr!("settings-script-tab"),
                };
                (
                    SharedString::from(script.title.clone()),
                    SharedString::from(script.id.clone()),
                    kind,
                )
            })
            .collect();
        let broken = self
            .scripts
            .broken()
            .iter()
            .map(|(id, why)| {
                (
                    SharedString::from(id.clone()),
                    SharedString::from(why.clone()),
                )
            })
            .collect();
        ScriptsListing {
            homes,
            found,
            broken,
        }
    }
}

/// The focus view's scripts: which one is the home, and where they are.
fn scripts_page(listing: ScriptsListing) -> SettingPage {
    let ScriptsListing {
        homes,
        found,
        broken,
    } = listing;
    SettingPage::new(Page::Scripts.title()).group(
        SettingGroup::new()
            .item(
                SettingItem::new(
                    tr!("settings-home-script"),
                    text!(homes, home_script, SharedString::default()),
                )
                .description(tr!("settings-home-script-help")),
            )
            .item(SettingItem::render(move |_, _window, cx| {
                let theme = cx.theme().clone();
                let folder = crate::ui::scripts::root();
                let shown = folder
                    .as_ref()
                    .map(|folder| SharedString::from(folder.display().to_string()))
                    .unwrap_or_default();
                let rows = found.iter().map(|(title, id, kind)| {
                    h_flex()
                        .w_full()
                        .gap_2()
                        .text_sm()
                        .child(div().child(title.clone()))
                        .child(
                            div()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(id.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(kind.clone()),
                        )
                });
                let broken = broken.iter().map(|(id, why)| {
                    div()
                        .w_full()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(SharedString::from(format!("{id} — {why}")))
                });
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(tr!("settings-scripts-help")),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(shown),
                            )
                            .child(
                                Button::new("scripts-folder")
                                    .outline()
                                    .small()
                                    .icon(icon("folder-open"))
                                    .label(tr!("settings-scripts-open"))
                                    .disabled(folder.is_none())
                                    .on_click(move |_, _, cx| {
                                        let Some(folder) = &folder else {
                                            return;
                                        };
                                        // Made on demand: the folder is where
                                        // one drops the first script.
                                        if let Err(error) = std::fs::create_dir_all(folder) {
                                            log::warn!("scripts folder: {error}");
                                        }
                                        cx.open_with_system(folder);
                                    }),
                            ),
                    )
                    .children(rows)
                    .children(broken)
                    .into_any_element()
            })),
    )
}

fn databases_page() -> SettingPage {
    SettingPage::new(Page::Databases.title()).group(
        SettingGroup::new().item(databases_item()).item(
            SettingItem::new(
                tr!("settings-db-page-size"),
                number!(
                    bounds(1., 100_000., 100.),
                    db_page_size,
                    500.,
                    |value: f64| value.clamp(1., 100_000.) as usize
                ),
            )
            .description(tr!("settings-db-page-size-help")),
        ),
    )
}

/// The connections table.
///
/// **A `SettingItem::render` and not a `SettingItem::new`**, the only one in
/// this whole window. An ordinary item puts its label in one column and its
/// field in what is left — four hundred pixels, cut for a checkbox or a menu —
/// and a connection needs five: name, host, port, user, password. The first
/// attempt set a hard-coded width, which overflowed the column and pushed the
/// engine picker off screen. A free element takes the whole page, and the title
/// is written by hand.
///
/// **The state key carries the number of connections**
/// (`claudhub-database-{n}-{i}`), like the agent profiles' and for the same
/// reason: `use_keyed_state` keeps one state per key, and without the count,
/// deleting the first connection would leave row 0's fields filled with the old
/// one — we would write into the settings what we thought we had deleted.
/// Changing engine, on the other hand, does not change the count: the fields
/// keep what had been typed into them.
fn databases_item() -> SettingItem {
    SettingItem::render(move |_, window, cx| {
        let connections = Settings::global(cx).databases.clone();
        let count = connections.len();
        let rows: Vec<_> = connections
            .iter()
            .enumerate()
            .map(|(index, connection)| database_row(index, count, connection, window, cx))
            .collect();
        v_flex()
            .w_full()
            .gap_2()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(tr!("settings-databases")))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("settings-databases-help")),
                    ),
            )
            .children(rows)
            .child(
                h_flex().child(
                    Button::new("add-database")
                        .outline()
                        .small()
                        .icon(icon("plus"))
                        .label(tr!("settings-database-add"))
                        .on_click(|_, _window, cx| {
                            Settings::update_global(cx, |s| {
                                s.databases.push(crate::db::Connection::default())
                            });
                        }),
                ),
            )
    })
}

/// A connection's fields, kept from one render to the next.
struct DatabaseField {
    name: Entity<InputState>,
    path: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    databases: Entity<InputState>,
    scope: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

/// A connection: its name, its engine, and the address that engine asks for.
///
/// **The engine is chosen with two buttons and not with a dropdown.** It is the
/// first gesture — an empty connection opens on SQLite, and one has to be able
/// to leave it without guessing that a button hides a list. Both labels read at
/// a glance, which is exactly what one is after when coming to declare a MariaDB
/// database.
///
/// **`min_w_0` on every elastic field**: a flex item's minimum size defaults to
/// its content's, and an input does not go below its own — without it, a narrow
/// row pushes its neighbours out instead of shrinking them. It is the same trap
/// as the scrollbars'.
fn database_row(
    index: usize,
    count: usize,
    connection: &crate::db::Connection,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let key = format!("claudhub-database-{count}-{index}");
    let values = connection.clone();
    let state = window.use_keyed_state(SharedString::from(key), cx, move |window, cx| {
        let mut field = |placeholder: SharedString,
                         value: String,
                         masked: bool,
                         cx: &mut Context<DatabaseField>| {
            cx.new(|cx| {
                let state = InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value);
                if masked {
                    state.masked(true)
                } else {
                    state
                }
            })
        };
        let name = field(
            tr!("settings-database-name"),
            values.name.clone(),
            false,
            cx,
        );
        let path = field(
            tr!("settings-database-path"),
            values.path.clone(),
            false,
            cx,
        );
        let host = field(
            tr!("settings-database-host"),
            values.host.clone(),
            false,
            cx,
        );
        let port = field(
            tr!("settings-database-port"),
            // Zero means "the engine's port": showing it would suggest that port
            // 0 had been chosen.
            if values.port == 0 {
                String::new()
            } else {
                values.port.to_string()
            },
            false,
            cx,
        );
        let user = field(
            tr!("settings-database-user"),
            values.user.clone(),
            false,
            cx,
        );
        let password = field(
            tr!("settings-database-password"),
            values.password.clone(),
            true,
            cx,
        );
        let databases = field(
            tr!("settings-database-databases"),
            values.databases.join(", "),
            false,
            cx,
        );
        let scope = field(
            SharedString::from(format!(
                "{} — {}",
                tr!("settings-database-scope"),
                crate::db::scope::EXAMPLE
            )),
            values.scope.clone(),
            false,
            cx,
        );
        let watch = |input: &Entity<InputState>,
                     edit: fn(&mut crate::db::Connection, String),
                     cx: &mut Context<DatabaseField>| {
            cx.subscribe(
                input,
                move |_: &mut DatabaseField, input, event: &InputEvent, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let value = input.read(cx).value().to_string();
                    edit_database(index, cx, |connection| edit(connection, value));
                },
            )
        };
        let subscriptions = vec![
            watch(&name, |c, v| c.name = v, cx),
            watch(&path, |c, v| c.path = v, cx),
            watch(&host, |c, v| c.host = v, cx),
            // An unreadable port counts as zero, that is, "the engine's": a
            // half-typed field must not prevent writing the rest.
            watch(&port, |c, v| c.port = v.trim().parse().unwrap_or(0), cx),
            watch(&user, |c, v| c.user = v, cx),
            watch(&password, |c, v| c.password = v, cx),
            watch(
                &databases,
                |c, v| {
                    c.databases = v
                        .split(',')
                        .map(|name| name.trim().to_string())
                        .filter(|name| !name.is_empty())
                        .collect()
                },
                cx,
            ),
            watch(&scope, |c, v| c.scope = v, cx),
        ];
        DatabaseField {
            name,
            path,
            host,
            port,
            user,
            password,
            databases,
            scope,
            _subscriptions: subscriptions,
        }
    });
    let field = state.read(cx);
    let (name, path, host, port, user, password, databases, scope) = (
        field.name.clone(),
        field.path.clone(),
        field.host.clone(),
        field.port.clone(),
        field.user.clone(),
        field.password.clone(),
        field.databases.clone(),
        field.scope.clone(),
    );
    let engine = connection.engine;
    let sqlite = engine == crate::db::Engine::Sqlite;

    v_flex()
        .w_full()
        .min_w_0()
        .gap_1p5()
        .p_2()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .items_center()
                .child(div().flex_1().min_w_0().child(Input::new(&name).small()))
                .children(crate::db::Engine::ALL.map(|choice| {
                    let chosen = engine == choice;
                    Button::new(("database-engine", index * 10 + choice as usize))
                        .small()
                        // Solid against outline, and not a button's "selected"
                        // state alone: on two neighbouring buttons of the same
                        // variant, the nuance it brings does not read, and that
                        // is precisely the question one asks on arriving — which
                        // of the two is active.
                        .map(|button| {
                            if chosen {
                                button.primary()
                            } else {
                                button.outline()
                            }
                        })
                        .selected(chosen)
                        .label(SharedString::new_static(choice.label()))
                        .on_click(move |_, _window, cx| {
                            edit_database(index, cx, |connection| connection.engine = choice);
                        })
                }))
                .child(
                    Button::new(("remove-database", index))
                        .ghost()
                        .small()
                        .icon(icon("trash-2"))
                        .tooltip(tr!("settings-database-remove"))
                        .on_click(move |_, _window, cx| {
                            Settings::update_global(cx, |s| {
                                if index < s.databases.len() {
                                    s.databases.remove(index);
                                }
                            });
                        }),
                ),
        )
        .when(sqlite, |this| {
            this.child(div().w_full().min_w_0().child(Input::new(&path).small()))
        })
        .when(!sqlite, |this| {
            this.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(div().flex_1().min_w_0().child(Input::new(&host).small()))
                    .child(
                        div()
                            .w(px(72.))
                            .flex_none()
                            .child(Input::new(&port).small()),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(div().flex_1().min_w_0().child(Input::new(&user).small()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&password).small()),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .child(Input::new(&databases).small()),
            )
        })
        // The worktree scope, for both engines: it says which of the databases
        // belong to the checkout being reviewed, and a SQLite file attached
        // beside others is the same question.
        .child(div().w_full().min_w_0().child(Input::new(&scope).small()))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("settings-database-scope-help")),
        )
}

/// The language servers page.
///
/// The third list declared in the settings, after the agent profiles and the
/// database connections, and the same reasoning: a declaration, not code. What
/// it does **not** carry is the server's own configuration — PHPantom reads a
/// `.phpantom.toml` it watches itself, and a second place to say the same thing
/// would be a second truth. Nor the environment, which stays writable by hand
/// in `settings.json`: a language server takes its settings from its own file,
/// where an agent takes its model from a variable.
fn lsp_page() -> SettingPage {
    SettingPage::new(Page::Lsp.title()).group(SettingGroup::new().item(lsp_item()))
}

/// The servers table, a `SettingItem::render` for the databases' reason: four
/// fields do not fit in the column an ordinary item leaves.
fn lsp_item() -> SettingItem {
    SettingItem::render(move |_, window, cx| {
        let servers = Settings::global(cx).lsp.clone();
        let count = servers.len();
        let rows: Vec<_> = servers
            .iter()
            .enumerate()
            .map(|(index, server)| lsp_row(index, count, server, window, cx))
            .collect();
        v_flex()
            .w_full()
            .gap_2()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(tr!("settings-lsp")))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("settings-lsp-help")),
                    ),
            )
            .children(rows)
            .child(
                h_flex().child(
                    Button::new("add-lsp")
                        .outline()
                        .small()
                        .icon(icon("plus"))
                        .label(tr!("settings-lsp-add"))
                        .on_click(|_, _window, cx| {
                            Settings::update_global(cx, |s| s.lsp.push(Default::default()));
                        }),
                ),
            )
    })
}

/// A server's fields, kept from one render to the next.
struct LspField {
    name: Entity<InputState>,
    command: Entity<InputState>,
    extensions: Entity<InputState>,
    language: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

/// One declared server.
///
/// **The command and its arguments are one field**, split by
/// `cmdline::split_command`, which honours quotes: `split_whitespace` breaks on
/// every path containing a space, and that is a failure one only understands
/// after reading the code. The state key carries the count, like the databases'
/// and the agent profiles': without it, deleting the first row would leave row
/// zero's fields filled with the old one.
fn lsp_row(
    index: usize,
    count: usize,
    server: &crate::lsp::Server,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let key = format!("claudhub-lsp-{count}-{index}");
    let values = server.clone();
    let state = window.use_keyed_state(SharedString::from(key), cx, move |window, cx| {
        let mut field = |placeholder: SharedString, value: String, cx: &mut Context<LspField>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let name = field(tr!("settings-lsp-name"), values.name.clone(), cx);
        let command = field(
            tr!("settings-lsp-command"),
            crate::cmdline::join_command(
                std::iter::once(values.command.clone()).chain(values.args.clone()),
            ),
            cx,
        );
        let extensions = field(
            tr!("settings-lsp-extensions"),
            values.extensions.join(", "),
            cx,
        );
        let language = field(tr!("settings-lsp-language"), values.language_id.clone(), cx);
        let watch = |input: &Entity<InputState>,
                     edit: fn(&mut crate::lsp::Server, String),
                     cx: &mut Context<LspField>| {
            cx.subscribe(
                input,
                move |_: &mut LspField, input, event: &InputEvent, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let value = input.read(cx).value().to_string();
                    edit_lsp(index, cx, |server| edit(server, value));
                },
            )
        };
        let subscriptions = vec![
            watch(&name, |s, v| s.name = v, cx),
            watch(
                &command,
                |s, v| {
                    let mut parts = crate::cmdline::split_command(&v).into_iter();
                    s.command = parts.next().unwrap_or_default();
                    s.args = parts.collect();
                },
                cx,
            ),
            watch(
                &extensions,
                |s, v| {
                    s.extensions = v
                        .split(',')
                        .map(|ext| ext.trim().trim_start_matches('.').to_string())
                        .filter(|ext| !ext.is_empty())
                        .collect()
                },
                cx,
            ),
            watch(&language, |s, v| s.language_id = v.trim().to_string(), cx),
        ];
        LspField {
            name,
            command,
            extensions,
            language,
            _subscriptions: subscriptions,
        }
    });
    let field = state.read(cx);
    let (name, command, extensions, language) = (
        field.name.clone(),
        field.command.clone(),
        field.extensions.clone(),
        field.language.clone(),
    );

    v_flex()
        .w_full()
        .min_w_0()
        .gap_1p5()
        .p_2()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .items_center()
                .child(div().flex_1().min_w_0().child(Input::new(&name).small()))
                .child(
                    Button::new(("remove-lsp", index))
                        .ghost()
                        .small()
                        .icon(icon("trash-2"))
                        .tooltip(tr!("settings-lsp-remove"))
                        .on_click(move |_, _window, cx| {
                            Settings::update_global(cx, |s| {
                                if index < s.lsp.len() {
                                    s.lsp.remove(index);
                                }
                            });
                        }),
                ),
        )
        .child(div().w_full().min_w_0().child(Input::new(&command).small()))
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&extensions).small()),
                )
                .child(
                    div()
                        .w(px(120.))
                        .flex_none()
                        .child(Input::new(&language).small()),
                ),
        )
}

/// Changes a server in place, if the index still exists — a subscription set up
/// for row 2 outlives row 2 by a frame.
fn edit_lsp(index: usize, cx: &mut App, edit: impl FnOnce(&mut crate::lsp::Server)) {
    Settings::update_global(cx, |s| {
        if let Some(server) = s.lsp.get_mut(index) {
            edit(server);
        }
    });
}

/// Changes a connection in place, if the index still exists.
///
/// A subscription set up for row 2 outlives row 2's disappearance by a frame,
/// and writing out of bounds would panic in the middle of a render.
fn edit_database(index: usize, cx: &mut App, edit: impl FnOnce(&mut crate::db::Connection)) {
    Settings::update_global(cx, |s| {
        if let Some(connection) = s.databases.get_mut(index) {
            edit(connection);
        }
    });
}

/// What the logs page needs: the records, what is being shown of them, and a
/// way back to the application — the level is a posture of reading, not a
/// preference, and it lives in `ClaudhubApp` rather than in the settings file.
#[derive(Clone)]
struct LogView {
    records: std::rc::Rc<Vec<LogRow>>,
    level: log::LevelFilter,
    app: Entity<ClaudhubApp>,
}

/// One record, as the page paints it.
///
/// Built when the ring moves and not when a frame is drawn: the level is what
/// the filter and the colour read, and everything else is already a string.
/// `line` is the clipboard's form — the same shape `env_logger` prints — kept
/// beside the rest so copying joins what is there instead of formatting two
/// thousand records again.
pub(super) struct LogRow {
    level: log::Level,
    at: SharedString,
    level_label: SharedString,
    target: SharedString,
    message: SharedString,
    line: SharedString,
}

impl LogRow {
    fn of(entry: &crate::logging::Entry) -> Self {
        Self {
            level: entry.level,
            at: SharedString::from(entry.at.format("%H:%M:%S%.3f").to_string()),
            level_label: SharedString::from(entry.level.to_string()),
            target: SharedString::from(entry.target.clone()),
            message: SharedString::from(entry.message.clone()),
            line: SharedString::from(log_line(entry)),
        }
    }
}

/// How many rows are painted.
///
/// The ring holds two thousand, and this list is **not** virtualised: it lives
/// inside the form's page, which scrolls as one block. Painting two thousand
/// styled lines per frame is what a virtualised list exists to avoid, and the
/// tail is what a log is read from — hence a cap, and a line that says so
/// rather than a list that silently stops.
const LOG_ROWS: usize = 200;

/// The levels the filter offers, from the widest to the narrowest.
const LOG_LEVELS: [log::LevelFilter; 5] = [
    log::LevelFilter::Trace,
    log::LevelFilter::Debug,
    log::LevelFilter::Info,
    log::LevelFilter::Warn,
    log::LevelFilter::Error,
];

/// The colour of a level. Warnings and errors are the two one is looking for;
/// the rest is context, and painting it would make the page unreadable.
fn level_color(level: log::Level, cx: &App) -> gpui_kit::Hsla {
    match level {
        log::Level::Error => cx.theme().danger,
        log::Level::Warn => cx.theme().warning,
        _ => cx.theme().muted_foreground,
    }
}

/// One record, as a line of text — what the copy button puts in the clipboard.
///
/// The same shape as what `env_logger` prints on stderr: a log pasted into a
/// report has to look like the one whoever reads it would have got from a
/// terminal.
fn log_line(entry: &crate::logging::Entry) -> String {
    format!(
        "[{} {:<5} {}] {}",
        entry.at.format("%Y-%m-%dT%H:%M:%S%.3f"),
        entry.level,
        entry.target,
        entry.message
    )
}

/// What Claudhub has written since it started.
///
/// **A page and not a file.** A graphical application has no console under its
/// window: without this, finding out why a fetch failed or why the remote server
/// died means relaunching from a terminal, which is asking the user to reproduce
/// the problem before being allowed to look at it.
fn logs_page(view: LogView) -> SettingPage {
    SettingPage::new(Page::Logs.title())
        // Nothing here is a setting, so nothing here resets.
        .resettable(false)
        .group(
            SettingGroup::new().item(SettingItem::render(move |_, _window, cx| {
                let LogView {
                    records,
                    level,
                    app,
                } = &view;
                // Filtered before being cut: the last two hundred **warnings** are
                // not the warnings among the last two hundred records, and the
                // second reading is the one that makes a page look empty.
                let shown: Vec<&LogRow> = records
                    .iter()
                    .filter(|entry| entry.level <= *level)
                    .collect();
                let total = shown.len();
                let mono = cx.theme().mono_font_family.clone();
                let muted = cx.theme().muted_foreground;
                let rows = shown
                    .iter()
                    .rev()
                    .take(LOG_ROWS)
                    .rev()
                    .map(|entry| {
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_start()
                            .text_xs()
                            .font_family(mono.clone())
                            .child(div().flex_none().text_color(muted).child(entry.at.clone()))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(44.))
                                    .text_color(level_color(entry.level, cx))
                                    .child(entry.level_label.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(px(160.))
                                    .truncate()
                                    .text_color(muted)
                                    .child(entry.target.clone()),
                            )
                            // No `truncate` on the message: a log line one cannot
                            // read to the end is a log line for nothing. It wraps.
                            .child(div().flex_1().min_w_0().child(entry.message.clone()))
                    })
                    .collect::<Vec<_>>();

                v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(tr!("settings-logs-help")),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .justify_between()
                            .child(
                                ButtonGroup::new("log-level")
                                    .outline()
                                    .compact()
                                    .small()
                                    .children(LOG_LEVELS.map(|choice| {
                                        Button::new(("log-level", choice as usize))
                                            .label(choice.to_string())
                                            .selected(choice == *level)
                                    }))
                                    .on_click({
                                        let app = app.clone();
                                        move |selected: &Vec<usize>, _window, cx| {
                                            let Some(choice) =
                                                selected.first().and_then(|ix| LOG_LEVELS.get(*ix))
                                            else {
                                                return;
                                            };
                                            app.update(cx, |this, cx| {
                                                this.logs_level = *choice;
                                                cx.notify();
                                            });
                                        }
                                    }),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Button::new("log-copy")
                                            .outline()
                                            .small()
                                            .icon(icon("copy"))
                                            .label(tr!("settings-logs-copy"))
                                            .disabled(total == 0)
                                            .on_click({
                                                // Everything the filter kept, not
                                                // the two hundred painted: what goes
                                                // into a report is the log, not the
                                                // end of it. Joined **in the click**
                                                // and not before it: a page that
                                                // repaints on every frame was
                                                // assembling two thousand lines for
                                                // a button nobody had pressed.
                                                let records = records.clone();
                                                let level = *level;
                                                move |_, _window, cx| {
                                                    let text = records
                                                        .iter()
                                                        .filter(|entry| entry.level <= level)
                                                        .map(|entry| entry.line.as_ref())
                                                        .collect::<Vec<_>>()
                                                        .join("\n");
                                                    cx.write_to_clipboard(
                                                        gpui_kit::ClipboardItem::new_string(text),
                                                    );
                                                }
                                            }),
                                    )
                                    .child(
                                        Button::new("log-clear")
                                            .outline()
                                            .small()
                                            .icon(icon("trash-2"))
                                            .label(tr!("settings-logs-clear"))
                                            .disabled(records.is_empty())
                                            .on_click({
                                                let app = app.clone();
                                                move |_, _window, cx| {
                                                    crate::logging::clear();
                                                    app.update(cx, |_, cx| cx.notify());
                                                }
                                            }),
                                    ),
                            ),
                    )
                    .when(total == 0, |el| {
                        el.child(
                            div()
                                .py_2()
                                .text_sm()
                                .text_color(muted)
                                .child(tr!("settings-logs-empty")),
                        )
                    })
                    // The cap is said rather than hidden: a list that stops without
                    // a word reads as a list that has nothing more in it.
                    .when(total > LOG_ROWS, |el| {
                        el.child(div().text_xs().text_color(muted).child(tr!(
                            "settings-logs-truncated",
                            { shown: LOG_ROWS.to_string(), total: total.to_string() }
                        )))
                    })
                    .children(rows)
            })),
        )
}

/// Bounds common to the text sizes, the same as the wheel's.
fn size_range() -> NumberFieldOptions {
    bounds(
        settings::MIN_FONT_SIZE as f64,
        settings::MAX_FONT_SIZE as f64,
        1.0,
    )
}

fn clamp_size(value: f64) -> f32 {
    settings::clamp_font_size(value as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_detected_shells_are_absolute_paths() {
        // The menu fills a field that will be executed: a relative entry there
        // would depend on the worktree's current directory.
        for (value, _) in shell_choices() {
            assert!(value.starts_with('/'), "{value}");
        }
    }
}
