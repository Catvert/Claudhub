//! What Claudhub lends the scripts beyond its cards: pieces a script could
//! not draw as well itself, and gestures it could not make at all.
//!
//! - **`CodeBlock`** — an excerpt of code, coloured by the grammar its path
//!   names, its numbers in a gutter and one line marked. The colouring is a
//!   grammar pass: it is **cached** by text, language and palette, since a
//!   component is built again at every repaint of the view around it.
//! - **`Icon`**, **`Badge`**, **`ShareBar`** — Claudhub's icons (a script's
//!   `svg()` reads its own folder, not ours), a word in a tinted pill, one
//!   value's share of a distribution.
//! - **`locate`**, **`open_file`** — a path as a server wrote it, brought back
//!   to a file of the worktree, and opened in the editor at a line. Only a
//!   file `locate` finds opens: a path comes from data a script fetched, and
//!   `../../.bashrc` is a path like any other.
//! - **`ask_agent`** — the text in the dialog where it is read and completed
//!   before it goes to the agent, as the notes and Sentry do.
//! - **`relative_time`** — an ISO instant as the interface says one.
//!
//! A tone is named, never given as a colour: `danger`, `warning`, `info`,
//! `success`, `primary`, `muted`, `foreground` — the palette decides.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Sizable as _};
use gpui_kit::{
    div, prelude::*, px, AnyElement, AnyWindowHandle, App, Hsla, SharedString, WeakEntity,
};
use gpui_shell::{HostError, HostModule, HostValue};

use crate::ui::app::ClaudhubApp;
use crate::ui::highlight::DocumentHighlights;
use crate::ui::icons::icon;
use crate::ui::scripts::{change, later};

/// The TypeScript face of what this module adds to `claudhub`.
pub(super) const DECLARATIONS: &str = r#"
/** A line of a `CodeBlock`: its number in the file, and its text. */
export interface CodeLine { line: number; text: string; }
/**
 * An excerpt of code, coloured by the language its `path` names, numbered,
 * `mark` the line drawn as the one that matters:
 * `CodeBlock.new("id", { path: "app/Http/Kernel.php", lines, mark: 42 })`.
 */
export const CodeBlock: HostComponent;
/**
 * One line of an excerpt, for a virtual list of one-line rows: the whole
 * excerpt is given — it is coloured as one, and cached — and `index` says
 * which of its lines this row is: `CodeLine.new("id", { path, lines, index, mark })`.
 */
export const CodeLine: HostComponent;
/** One of Claudhub's icons: `Icon.new("id", { name: "circle-x", size: "xsmall" | "small" | "medium" | "large", tone })`. */
export const Icon: HostComponent;
/** A word in a tinted pill: `Badge.new("id", { text: "error", tone: "danger" })`. */
export const Badge: HostComponent;
/** One value's share of a distribution: `ShareBar.new("id", { label: "production", share: 64 })`. */
export const ShareBar: HostComponent;

/** The file of this worktree a path names — a server's absolute path, a Windows one —, relative to the worktree; null when none. */
export function locate(path: string): string | null;
/** Opens a file of this worktree in the editor, at a line (from 1). Refused when `locate` finds no file. */
export function open_file(path: string, line?: number): void;
/** Shows a text in the dialog where the user reads and completes it before it goes to the worktree's agent. */
export function ask_agent(text: string): void;
/** An ISO 8601 instant as the interface says one: "3 min ago", or a local date. */
export function relative_time(iso: string): string;
/**
 * Claudhub's heights, in pixels, for a virtual list to be told exactly:
 * `row` is a list row's, `line` a line of code's.
 */
export function sizes(): { row: number; line: number };
/**
 * Claudhub's palette, as `#rrggbb` — what `cx.theme().colors` lacks: the
 * tones a state is told by. Append two digits for a tint: `palette().danger + "26"`.
 */
export function palette(): {
  danger: string; warning: string; info: string; success: string; primary: string;
  foreground: string; muted: string; muted_foreground: string; background: string;
  secondary: string; accent: string; border: string; list_hover: string; list_active: string; ring: string;
};
"#;

/// Adds the pieces and the gestures to the module of the board of `path`.
pub(super) fn lend(
    module: HostModule,
    app: &WeakEntity<ClaudhubApp>,
    path: &Path,
    window: Option<AnyWindowHandle>,
) -> HostModule {
    module
        .component("CodeBlock", code_block)
        .component("CodeLine", code_line)
        .component("Icon", |args, _, cx| {
            let props = args.props();
            let Some(name) = text(props, "name").filter(|name| shipped(name)) else {
                return div().into_any_element();
            };
            let glyph = icon(name);
            let glyph = match text(props, "size") {
                Some("xsmall") => glyph.xsmall(),
                Some("small") => glyph.small(),
                Some("large") => glyph.large(),
                _ => glyph,
            };
            match text(props, "tone").and_then(|name| tone(name, cx)) {
                Some(tone) => glyph.text_color(tone).into_any_element(),
                None => glyph.into_any_element(),
            }
        })
        .component("Badge", |args, _, cx| {
            let props = args.props();
            let tone = text(props, "tone")
                .and_then(|name| tone(name, cx))
                .unwrap_or(cx.theme().muted_foreground);
            badge(text(props, "text").unwrap_or_default(), tone).into_any_element()
        })
        .component("ShareBar", |args, _, cx| {
            let props = args.props();
            let share = number(props, "share").unwrap_or(0.).clamp(0., 100.);
            let theme = cx.theme();
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .text_xs()
                .child(
                    div()
                        .w(px(160.))
                        .flex_none()
                        .truncate()
                        .child(SharedString::from(
                            text(props, "label").unwrap_or_default().to_string(),
                        )),
                )
                .child(
                    div().flex_1().h(px(6.)).rounded_sm().bg(theme.muted).child(
                        div()
                            .h_full()
                            .w(gpui_kit::relative(share as f32 / 100.))
                            .rounded_sm()
                            .bg(theme.primary),
                    ),
                )
                .child(
                    div()
                        .w(px(40.))
                        .flex_none()
                        .text_right()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(format!("{share:.0} %"))),
                )
                .into_any_element()
        })
        .function("locate", {
            let (app, path) = (app.clone(), path.to_path_buf());
            move |arguments| {
                let named = arguments.string(0)?.to_string();
                change(&app, |this, _| {
                    Ok(this
                        .locate_in(&path, &named)
                        .map_or(HostValue::Null, HostValue::from))
                })
            }
        })
        .function("open_file", {
            let (app, path) = (app.clone(), path.to_path_buf());
            move |arguments| {
                let named = arguments.string(0)?.to_string();
                let line = match arguments.get(1) {
                    Some(value) if !value.is_null() => Some(arguments.integer(1)?),
                    _ => None,
                };
                let file = change(&app, |this, _| {
                    Ok(this
                        .locate_in(&path, &named)
                        .map_or(HostValue::Null, HostValue::from))
                })?;
                let Some(file) = file.as_str().map(PathBuf::from) else {
                    return Err(HostError::new(format!(
                        "`{named}` is not a file of this worktree"
                    )));
                };
                let worktree = path.clone();
                later(&app, window, move |this, window, cx| {
                    if this.active.as_deref() != Some(worktree.as_path()) {
                        this.select_worktree(worktree, window, cx);
                    }
                    this.leave_overview(cx);
                    let landing = line.map(|line| crate::ui::explorer::Landing::Position {
                        line: u32::try_from(line.saturating_sub(1)).unwrap_or(0),
                        character: 0,
                    });
                    this.open_at(file, landing, cx);
                })
            }
        })
        .function("ask_agent", {
            let (app, path) = (app.clone(), path.to_path_buf());
            move |arguments| {
                let text = arguments.string(0)?.to_string();
                let worktree = path.clone();
                later(&app, window, move |this, window, cx| {
                    this.confirm_agent_prompt(worktree, text, window, cx)
                })
            }
        })
        .function("sizes", |_| {
            gpui_shell::with_current_app(|cx| {
                let line = code_line_height(cx);
                gpui_shell::HostObject::new()
                    .field(
                        "row",
                        f64::from(f32::from(crate::ui::theme::row_height(cx))),
                    )
                    .field("line", f64::from(f32::from(line)))
                    .into()
            })
            .ok_or_else(crate::ui::scripts::unreachable)
        })
        .function("palette", |_| {
            gpui_shell::with_current_app(|cx| {
                let theme = cx.theme();
                [
                    ("danger", theme.danger),
                    ("warning", theme.warning),
                    ("info", theme.info),
                    ("success", theme.success),
                    ("primary", theme.primary),
                    ("foreground", theme.foreground),
                    ("muted", theme.muted),
                    ("muted_foreground", theme.muted_foreground),
                    ("background", theme.background),
                    ("secondary", theme.secondary),
                    ("accent", theme.accent),
                    ("border", theme.border),
                    ("list_hover", theme.list_hover),
                    ("list_active", theme.list_active),
                    ("ring", theme.ring),
                ]
                .into_iter()
                .fold(gpui_shell::HostObject::new(), |object, (name, colour)| {
                    object.field(name, hex(colour))
                })
                .into()
            })
            .ok_or_else(crate::ui::scripts::unreachable)
        })
        .function("relative_time", |arguments| {
            Ok(HostValue::from(crate::ui::sentry::when(
                arguments.string(0)?,
            )))
        })
}

impl ClaudhubApp {
    /// The file of `worktree` a path names, relative to it — see
    /// `sentry::Frame::locate`, which refuses a path that climbs out.
    fn locate_in(&self, worktree: &Path, named: &str) -> Option<String> {
        let known = self.known_files(worktree);
        crate::sentry::Frame {
            filename: named.to_string(),
            ..Default::default()
        }
        .locate(|candidate| known.contains(candidate))
    }
}

/// A colour as `#rrggbb`, its opacity left to whoever tints it.
fn hex(colour: Hsla) -> String {
    let rgba = gpui_kit::Rgba::from(colour);
    let byte = |channel: f32| (channel.clamp(0., 1.) * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        byte(rgba.r),
        byte(rgba.g),
        byte(rgba.b)
    )
}

/// A text property, when it is one.
fn text<'a>(props: &'a HostValue, key: &str) -> Option<&'a str> {
    props.get(key).and_then(HostValue::as_str)
}

/// A number property, when it is one.
fn number(props: &HostValue, key: &str) -> Option<f64> {
    props.get(key).and_then(HostValue::as_number)
}

/// Whether Claudhub ships an icon of that name: one it does not would draw
/// nothing, and say nothing of why.
fn shipped(name: &str) -> bool {
    super::Assets::get(&format!("icons/{name}.svg")).is_some()
}

/// A tone, by the name a script gives it.
fn tone(name: &str, cx: &App) -> Option<Hsla> {
    let theme = cx.theme();
    Some(match name {
        "danger" => theme.danger,
        "warning" => theme.warning,
        "info" => theme.info,
        "success" => theme.success,
        "primary" => theme.primary,
        "muted" => theme.muted_foreground,
        "foreground" => theme.foreground,
        _ => return None,
    })
}

/// A word in a tinted pill — the look of Sentry's level and status badges.
pub(super) fn badge(text: &str, tone: Hsla) -> impl IntoElement {
    div()
        .px_1p5()
        .rounded_sm()
        .text_xs()
        .bg(tone.opacity(0.15))
        .text_color(tone)
        .child(SharedString::from(text.to_string()))
}

/// The lines a `CodeBlock` is given: `{line, text}` objects, or `[line,
/// text]` pairs.
fn lines_of(props: &HostValue) -> Vec<(usize, SharedString)> {
    props
        .get("lines")
        .and_then(HostValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(|line| {
            let (number, text) = match line.as_array() {
                Some([number, text, ..]) => (number.as_number()?, text.as_str()?),
                _ => (
                    line.get("line")?.as_number()?,
                    line.get("text").or_else(|| line.get("source"))?.as_str()?,
                ),
            };
            Some((
                number.max(0.) as usize,
                SharedString::from(text.to_string()),
            ))
        })
        .collect()
}

thread_local! {
    /// The colouring of the excerpts drawn lately, by language, text and
    /// palette.
    static COLOURED: RefCell<HashMap<u64, Rc<DocumentHighlights>>> = RefCell::new(HashMap::new());
}

/// How many colourings are kept: a trace of two hundred frames, and some.
const COLOURED_KEPT: usize = 512;

/// An excerpt's colouring, worked out once.
fn coloured(language: &'static str, source: &str, cx: &App) -> Rc<DocumentHighlights> {
    let theme = cx.theme().highlight_theme.clone();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (language, source, std::sync::Arc::as_ptr(&theme) as usize).hash(&mut hasher);
    let key = hasher.finish();
    COLOURED.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(found) = cache.get(&key) {
            return found.clone();
        }
        // Forgotten whole rather than one by one: what was drawn a moment ago
        // is drawn again at the next repaint, and comes straight back.
        if cache.len() >= COLOURED_KEPT {
            cache.clear();
        }
        let made = Rc::new(DocumentHighlights::for_language(language, source, &theme));
        cache.insert(key, made.clone());
        made
    })
}

/// The size code is drawn at, and the height of its lines: the diffs'.
fn code_size(cx: &App) -> gpui_kit::Pixels {
    px(super::settings::Settings::global(cx).diff_font_size)
}

fn code_line_height(cx: &App) -> gpui_kit::Pixels {
    crate::ui::diff_view::line_height(code_size(cx))
}

/// One painted line of an excerpt: its number in the gutter, its text
/// coloured, its ground marked when it is the line that matters.
fn painted_line(
    number: usize,
    text: SharedString,
    styles: Option<&[(std::ops::Range<usize>, gpui_kit::HighlightStyle)]>,
    marked: bool,
    cx: &App,
) -> gpui_kit::Div {
    let theme = cx.theme();
    let painted = match styles {
        Some(styles) if !styles.is_empty() => gpui_kit::StyledText::new(text)
            .with_highlights(styles.to_vec())
            .into_any_element(),
        _ => div().child(text).into_any_element(),
    };
    h_flex()
        .h(code_line_height(cx))
        .w_full()
        .gap_2()
        .items_center()
        .font_family(theme.mono_font_family.clone())
        .text_size(code_size(cx))
        .bg(if marked {
            theme.warning.opacity(0.15)
        } else {
            theme.secondary
        })
        .child(
            div()
                .w(px(48.))
                .flex_none()
                .text_right()
                .text_color(theme.muted_foreground)
                .child(SharedString::from(number.to_string())),
        )
        .child(div().whitespace_nowrap().child(painted))
}

/// An excerpt's colouring, by the language its path names.
fn colouring(
    props: &HostValue,
    lines: &[(usize, SharedString)],
    cx: &App,
) -> Option<Rc<DocumentHighlights>> {
    let path = text(props, "path").unwrap_or_default();
    let source: String = lines
        .iter()
        .map(|(_, text)| text.as_ref())
        .collect::<Vec<_>>()
        .join("\n");
    crate::ui::highlight::language_for_path(Path::new(path))
        .map(|language| coloured(language, &source, cx))
}

/// The `CodeLine` component.
fn code_line(
    args: gpui_shell::ComponentArgs<'_>,
    _window: &mut gpui_kit::Window,
    cx: &mut App,
) -> AnyElement {
    let props = args.props();
    let lines = lines_of(props);
    let index = number(props, "index").unwrap_or(0.).max(0.) as usize;
    let Some((number_at, text_at)) = lines.get(index).cloned() else {
        return div().h(code_line_height(cx)).into_any_element();
    };
    let mark = number(props, "mark").map(|line| line.max(0.) as usize);
    let styles = colouring(props, &lines, cx);
    painted_line(
        number_at,
        text_at,
        styles.as_ref().map(|styles| styles.line(index)),
        mark == Some(number_at),
        cx,
    )
    .into_any_element()
}

/// The `CodeBlock` component.
fn code_block(
    args: gpui_shell::ComponentArgs<'_>,
    _window: &mut gpui_kit::Window,
    cx: &mut App,
) -> AnyElement {
    let props = args.props();
    let lines = lines_of(props);
    let mark = number(props, "mark").map(|line| line.max(0.) as usize);
    let styles = colouring(props, &lines, cx);
    let rows: Vec<gpui_kit::Div> = lines
        .into_iter()
        .enumerate()
        .map(|(index, (number, text))| {
            painted_line(
                number,
                text,
                styles.as_ref().map(|styles| styles.line(index)),
                mark == Some(number),
                cx,
            )
        })
        .collect();
    v_flex()
        .w_full()
        .rounded(cx.theme().radius)
        .overflow_hidden()
        .children(rows)
        .into_any_element()
}
