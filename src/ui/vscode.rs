//! VS Code's keyboard, read into ours.
//!
//! Whoever comes here comes from an editor, and the muscle is in the hand
//! before it is in the settings: an import is the one gesture that makes a
//! window feel like the one next to it, and it is cheap because both
//! applications already agree on most of the keyboard.
//!
//! **A table of gestures, not of commands.** VS Code has three thousand
//! commands and we have a hundred and fifty bindings; what can be carried over
//! is the handful of things both applications do, named on both sides. The
//! table below is that list, and `keymap` is all the deciding: pure, and tested
//! against the real binding table, since a mapping naming a binding that has
//! moved is a line that silently does nothing.
//!
//! Two sources, in that order. VS Code's **own defaults**, written here, which
//! is what an import means for someone who never opened its keyboard settings;
//! then their `keybindings.json` if they have one, which is the only place a
//! command with no default of its own — `git.pull` has none — ever gets a key.
//!
//! **An import never switches a binding off.** A command VS Code has no
//! readable key for is left as ours: the alternative is a window whose gesture
//! has quietly gone, with nothing on screen to say which one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ui::shortcuts::{self, Overrides};

/// One gesture both applications have.
pub struct Mapping {
    /// What VS Code calls it.
    pub command: &'static str,
    /// What we call it: `Entry::id()`, which is a family and the **default**
    /// keys. Checked against the real table by a test — the id is what
    /// `settings.json` carries, so it cannot be derived from anything that
    /// moves.
    pub binding: &'static str,
    /// What VS Code binds it to out of the box, on Windows and Linux. Empty
    /// where VS Code ships no key at all, which is most of the git commands.
    ///
    /// Written as VS Code writes it, unreadable keys included — `ctrl+numpad0`
    /// is what it really binds "reset zoom" to, and translating it is how one
    /// finds out we cannot: the mapping is then dropped, which is the answer.
    pub keys: &'static str,
}

const fn map(command: &'static str, binding: &'static str, keys: &'static str) -> Mapping {
    Mapping {
        command,
        binding,
        keys,
    }
}

/// The gestures an import carries over.
///
/// Short on purpose. What is *not* here is as deliberate: VS Code's `F1` is a
/// command palette we do not have, its `F5` starts a debugger, and its trees
/// answer the arrows exactly as ours already do.
pub static MAPPINGS: &[Mapping] = &[
    // ── The window ──────────────────────────────────────────────────────────
    map(
        "workbench.action.openSettings",
        "window:secondary-,",
        "ctrl+,",
    ),
    map(
        "workbench.action.toggleSidebarVisibility",
        "window:secondary-b",
        "ctrl+b",
    ),
    map("workbench.action.zoomIn", "window:secondary-=", "ctrl+="),
    map("workbench.action.zoomOut", "window:secondary--", "ctrl+-"),
    map(
        "workbench.action.zoomReset",
        "window:secondary-0",
        "ctrl+numpad0",
    ),
    // The trail. VS Code puts it on the platform key rather than on the browsers'
    // Alt and an arrow, and that difference is the whole reason someone asks for
    // this import.
    map(
        "workbench.action.navigateBack",
        "window:alt-left",
        "ctrl+alt+-",
    ),
    map(
        "workbench.action.navigateForward",
        "window:alt-right",
        "ctrl+shift+-",
    ),
    // ── The repository ──────────────────────────────────────────────────────
    // None of these has a key in VS Code; they are here for the file, which is
    // where somebody who drives git from the keyboard has given them one.
    map("git.fetch", "repo:secondary-shift-r", ""),
    map("git.pull", "repo:secondary-shift-u", ""),
    map("git.push", "repo:secondary-shift-p", ""),
    map("git.commit", "repo:secondary-enter", ""),
    // ── The editor ──────────────────────────────────────────────────────────
    map(
        "workbench.action.files.save",
        "review:secondary-s",
        "ctrl+s",
    ),
    map(
        "workbench.action.closeActiveEditor",
        "review:secondary-w",
        "ctrl+w",
    ),
    map("editor.action.revealDefinition", "review:f12", "f12"),
    map(
        "editor.action.clipboardCopyAction",
        "review:secondary-c",
        "ctrl+c",
    ),
    map("editor.action.selectAll", "review:secondary-a", "ctrl+a"),
    // ── Searching ───────────────────────────────────────────────────────────
    map("actions.find", "search:secondary-f", "ctrl+f"),
    map("workbench.action.quickOpen", "search:secondary-p", "ctrl+p"),
    // VS Code steps through matches with `F3`, where we took the Sublime and
    // macOS `Ctrl+G`. One of the few places the two really differ.
    map(
        "editor.action.nextMatchFindAction",
        "search:secondary-g",
        "f3",
    ),
    map(
        "editor.action.previousMatchFindAction",
        "search:secondary-shift-g",
        "shift+f3",
    ),
    map("closeFindWidget", "search:escape", "escape"),
    map(
        "workbench.action.findInFiles",
        "project:secondary-shift-f",
        "ctrl+shift+f",
    ),
    // ── The terminals ───────────────────────────────────────────────────────
    // `Ctrl+`` is VS Code's, and it is a dead key behind AltGr on an AZERTY
    // keyboard — which is why it is not our default. It is imported all the
    // same: an import says what VS Code does, and choosing on the user's behalf
    // which of their own keys they can reach is not ours to do. The row's own
    // undo is one click away.
    map(
        "workbench.action.terminal.toggleTerminal",
        "terminal:secondary-t",
        "ctrl+`",
    ),
    map(
        "workbench.action.terminal.new",
        "terminal:secondary-shift-t",
        "ctrl+shift+`",
    ),
    map(
        "workbench.action.terminal.kill",
        "terminal:secondary-shift-w",
        "",
    ),
    map(
        "workbench.action.terminal.focusNext",
        "terminal:secondary-tab",
        "ctrl+pagedown",
    ),
    map(
        "workbench.action.terminal.focusPrevious",
        "terminal:secondary-shift-tab",
        "ctrl+pageup",
    ),
    map(
        "workbench.action.terminal.copySelection",
        "terminal:secondary-shift-c",
        "ctrl+shift+c",
    ),
    map(
        "workbench.action.terminal.paste",
        "terminal:secondary-shift-v",
        "ctrl+shift+v",
    ),
    map(
        "workbench.action.terminal.selectAll",
        "terminal:secondary-shift-a",
        "",
    ),
];

/// What an import has to say about the bindings it speaks for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Keymap {
    /// Binding id → keystrokes, where VS Code binds the gesture differently
    /// from us.
    pub set: Overrides,
    /// The bindings VS Code agrees with us on. They are **cleared** rather than
    /// left alone: an import speaks for these gestures, so a customisation of
    /// one of them is exactly what it is undoing.
    pub clear: Vec<String>,
}

impl Keymap {
    /// Writes the import into the overrides, and says how many bindings
    /// actually moved.
    ///
    /// The count is read off the overrides and not off the table's size: what a
    /// balloon has to say is how much of the keyboard has just changed, and on
    /// a window already set up like VS Code the honest answer is none.
    pub fn apply(&self, overrides: &mut Overrides) -> usize {
        let mut moved = 0;
        for id in &self.clear {
            if overrides.remove(id).is_some() {
                moved += 1;
            }
        }
        for (id, keys) in &self.set {
            if overrides.insert(id.clone(), keys.clone()).as_ref() != Some(keys) {
                moved += 1;
            }
        }
        moved
    }
}

/// What VS Code's keyboard says about ours, in our own spelling.
///
/// `user` is the text of their `keybindings.json`, when there is one.
pub fn keymap(user: Option<&str>) -> Keymap {
    let ours: HashMap<String, &'static str> = shortcuts::all()
        .map(|entry| (entry.id(), entry.keys))
        .collect();
    let declared = user.map(entries).unwrap_or_default();

    let mut keymap = Keymap::default();
    for mapping in MAPPINGS {
        let Some(default) = ours.get(mapping.binding) else {
            // A mapping the table has left behind. The test is what catches
            // this; the log is for the build where it was not run.
            log::warn!("no such binding {}, skipping", mapping.binding);
            continue;
        };
        let mut keys = mapping.keys.to_string();
        for declaration in &declared {
            match &declaration.removes {
                // A removal names a key as well as a command, and takes only
                // that one out: `-workbench.action.terminal.new` with another
                // key than the one in hand says nothing about the one in hand.
                Some(command) if command == mapping.command => {
                    if declaration.key.is_empty()
                        || keystrokes(&declaration.key) == keystrokes(&keys)
                    {
                        keys.clear();
                    }
                }
                Some(_) => {}
                // Later wins, which is VS Code's own rule for its file.
                None if declaration.command == mapping.command => {
                    keys = declaration.key.clone();
                }
                None => {}
            }
        }
        let Some(keys) = keystrokes(&keys) else {
            continue;
        };
        if keys == *default {
            keymap.clear.push(mapping.binding.to_string());
        } else {
            keymap.set.insert(mapping.binding.to_string(), keys);
        }
    }
    keymap
}

/// One line of `keybindings.json`, as far as we read it.
#[derive(Debug, PartialEq, Eq)]
struct Declaration {
    /// The command it binds. Empty when it only removes one.
    command: String,
    /// The command it takes a key away from — VS Code's `-` prefix.
    removes: Option<String>,
    key: String,
}

/// The declarations of a `keybindings.json`.
///
/// **Read field by field**, never deserialised into a structure: the file is
/// hand-written, half of it is commented out, and one entry missing a `key` is
/// not a reason to import nothing.
fn entries(text: &str) -> Vec<Declaration> {
    let json = uncomment(text);
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) else {
        log::warn!("the VS Code keyboard file could not be read");
        return Vec::new();
    };
    let Some(array) = value.as_array() else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|item| {
            let command = item.get("command")?.as_str()?.trim();
            let key = item
                .get("key")
                .and_then(|key| key.as_str())
                .unwrap_or_default()
                .trim();
            Some(match command.strip_prefix('-') {
                Some(removed) => Declaration {
                    command: String::new(),
                    removes: Some(removed.to_string()),
                    key: key.to_string(),
                },
                None => Declaration {
                    command: command.to_string(),
                    removes: None,
                    key: key.to_string(),
                },
            })
        })
        .collect()
}

/// JSON with the comments and the trailing commas taken out.
///
/// `keybindings.json` is JSONC: VS Code opens it on a header of comments and a
/// commented-out example, and `serde_json` refuses the lot. Line breaks are
/// kept where a comment was, so that a parse error still names a line the user
/// can find.
fn uncomment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut star = false;
                for c in chars.by_ref() {
                    if star && c == '/' {
                        break;
                    }
                    if c == '\n' {
                        out.push('\n');
                    }
                    star = c == '*';
                }
            }
            _ => out.push(c),
        }
    }
    uncomma(&out)
}

/// The same text with the comma before a `]` or a `}` dropped.
fn uncomma(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    let bytes: Vec<char> = text.chars().collect();
    for (ix, c) in bytes.iter().enumerate() {
        if in_string {
            out.push(*c);
            if escaped {
                escaped = false;
            } else if *c == '\\' {
                escaped = true;
            } else if *c == '"' {
                in_string = false;
            }
            continue;
        }
        if *c == '"' {
            in_string = true;
            out.push(*c);
            continue;
        }
        if *c == ','
            && bytes[ix + 1..]
                .iter()
                .find(|c| !c.is_whitespace())
                .is_some_and(|c| *c == ']' || *c == '}')
        {
            continue;
        }
        out.push(*c);
    }
    out
}

/// VS Code's spelling of a chord, in ours.
///
/// `None` for a chord we would install dead: the numeric keypad, an `oem_`
/// scan code, a modifier on its own. `valid_keys` is what settles it — the same
/// reading the settings form does, so an import can put nothing in the file
/// that typing it by hand could not.
pub fn keystrokes(chord: &str) -> Option<String> {
    let strokes: Vec<String> = chord.split_whitespace().map(keystroke).collect();
    if strokes.is_empty() {
        return None;
    }
    let keys = strokes.join(" ");
    shortcuts::valid_keys(&keys).then_some(keys)
}

/// One keystroke of a chord.
///
/// The modifiers are taken from the front while they are modifiers, and
/// **never the last token**: what is left is the key, rejoined with the `+` it
/// was split on so that `ctrl++` is the plus sign and not two empty names.
fn keystroke(stroke: &str) -> String {
    let tokens: Vec<&str> = stroke.split('+').collect();
    let (mut secondary, mut alt, mut shift) = (false, false, false);
    let mut ix = 0;
    while ix + 1 < tokens.len() {
        match tokens[ix].to_ascii_lowercase().as_str() {
            // Ctrl and Cmd are one modifier here: VS Code's `key` is what it
            // binds on Windows and Linux, and `secondary` is that key on both,
            // as it is the Cmd of whoever reads this file on a Mac.
            "ctrl" | "cmd" | "meta" | "win" | "super" => secondary = true,
            "alt" | "option" => alt = true,
            "shift" => shift = true,
            _ => break,
        }
        ix += 1;
    }
    let key = tokens[ix..].join("+");
    let mut out = String::new();
    for (held, name) in [(secondary, "secondary"), (alt, "alt"), (shift, "shift")] {
        if held {
            out.push_str(name);
            out.push('-');
        }
    }
    out.push_str(&key.to_ascii_lowercase());
    out
}

/// Where VS Code keeps its keyboard, under a configuration directory.
///
/// The flavours are the same file under another name — a distribution ships
/// VSCodium or the OSS build where Microsoft ships `Code` — and they are listed
/// in the order of how likely one is to be the one running.
pub fn candidates(config: &Path) -> Vec<PathBuf> {
    ["Code", "VSCodium", "Code - OSS", "Code - Insiders"]
        .iter()
        .map(|flavour| config.join(flavour).join("User").join("keybindings.json"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one thing a table like this gets wrong: a binding id that has moved.
    /// It is written into `settings.json`, so it cannot be derived, and a
    /// mapping naming one that is gone imports nothing without a word.
    #[test]
    fn every_mapping_names_a_binding_we_have() {
        let ours: Vec<String> = shortcuts::all().map(|entry| entry.id()).collect();
        for mapping in MAPPINGS {
            assert!(
                ours.iter().any(|id| id == mapping.binding),
                "{} names {}, which no longer exists",
                mapping.command,
                mapping.binding
            );
        }
    }

    #[test]
    fn modifiers_become_ours() {
        assert_eq!(
            keystrokes("ctrl+shift+f").as_deref(),
            Some("secondary-shift-f")
        );
        assert_eq!(keystrokes("ctrl+,").as_deref(), Some("secondary-,"));
        assert_eq!(keystrokes("ctrl+-").as_deref(), Some("secondary--"));
        assert_eq!(keystrokes("ctrl++").as_deref(), Some("secondary-+"));
        assert_eq!(keystrokes("f3").as_deref(), Some("f3"));
        assert_eq!(keystrokes("shift+f3").as_deref(), Some("shift-f3"));
        assert_eq!(keystrokes("alt+left").as_deref(), Some("alt-left"));
        assert_eq!(keystrokes("cmd+s").as_deref(), Some("secondary-s"));
    }

    /// A chord is two keystrokes with a space between, which is our spelling
    /// too — `g g` is already in the table.
    #[test]
    fn a_chord_stays_a_chord() {
        assert_eq!(
            keystrokes("ctrl+k ctrl+s").as_deref(),
            Some("secondary-k secondary-s")
        );
    }

    /// What we would install dead. `numpad0` parses perfectly and matches
    /// nothing for the rest of the session, which is the failure this refuses.
    #[test]
    fn what_gpui_cannot_read_is_refused() {
        assert_eq!(keystrokes("ctrl+numpad0"), None);
        assert_eq!(keystrokes("ctrl+oem_3"), None);
        assert_eq!(keystrokes(""), None);
    }

    #[test]
    fn comments_and_trailing_commas_go() {
        let text = r#"
// Place your key bindings in this file
[
    /* a block
       comment */
    { "key": "ctrl+i", "command": "actions.find" }, // and a line one
]
"#;
        let declared = entries(text);
        assert_eq!(declared.len(), 1);
        assert_eq!(declared[0].command, "actions.find");
        assert_eq!(declared[0].key, "ctrl+i");
    }

    /// A `//` inside a string is not a comment, and a file whose first binding
    /// is a URL must not lose the rest of itself.
    #[test]
    fn a_slash_in_a_string_is_not_a_comment() {
        let text = r#"[{ "key": "ctrl+i", "command": "a//b" }, { "key": "f3", "command": "c" }]"#;
        let declared = entries(text);
        assert_eq!(declared.len(), 2);
        assert_eq!(declared[0].command, "a//b");
    }

    /// No file at all is still an import: VS Code's own defaults are what
    /// somebody who never opened its settings means by "VS Code's keyboard".
    #[test]
    fn the_defaults_alone_are_an_import() {
        let keymap = keymap(None);
        // F3, which we do not bind to that.
        assert_eq!(
            keymap.set.get("search:secondary-g").map(String::as_str),
            Some("f3")
        );
        // Ctrl+S, which we do: nothing to write, and a customisation to undo.
        assert!(keymap.clear.iter().any(|id| id == "review:secondary-s"));
        assert!(!keymap.set.contains_key("review:secondary-s"));
        // Reset zoom is on the numeric keypad over there: unreadable, so ours
        // is left alone rather than switched off.
        assert!(!keymap.set.contains_key("window:secondary-0"));
        assert!(!keymap.clear.iter().any(|id| id == "window:secondary-0"));
    }

    #[test]
    fn the_users_own_keys_win() {
        let text = r#"[
            { "key": "ctrl+alt+p", "command": "git.pull" },
            { "key": "ctrl+shift+f3", "command": "editor.action.nextMatchFindAction" }
        ]"#;
        let keymap = keymap(Some(text));
        assert_eq!(
            keymap.set.get("repo:secondary-shift-u").map(String::as_str),
            Some("secondary-alt-p")
        );
        assert_eq!(
            keymap.set.get("search:secondary-g").map(String::as_str),
            Some("secondary-shift-f3")
        );
    }

    /// A binding they took away in VS Code is one we leave alone: an import
    /// adds a keyboard, it does not take gestures out of this window.
    #[test]
    fn a_removal_leaves_ours_standing() {
        let text = r#"[{ "key": "f3", "command": "-editor.action.nextMatchFindAction" }]"#;
        let keymap = keymap(Some(text));
        assert!(!keymap.set.contains_key("search:secondary-g"));
        assert!(!keymap.clear.iter().any(|id| id == "search:secondary-g"));
    }

    /// What the balloon says. Applying the same import twice moves nothing the
    /// second time.
    #[test]
    fn only_what_moves_is_counted() {
        let keymap = keymap(None);
        let mut overrides = Overrides::new();
        let first = keymap.apply(&mut overrides);
        assert!(first > 0);
        assert_eq!(keymap.apply(&mut overrides), 0);
    }

    /// A customisation of a gesture the import speaks for is undone, even when
    /// VS Code's key for it is the one we already had.
    #[test]
    fn an_agreement_undoes_a_customisation() {
        let mut overrides = Overrides::new();
        overrides.insert("review:secondary-s".into(), "f8".into());
        assert!(keymap(None).apply(&mut overrides) > 0);
        assert!(!overrides.contains_key("review:secondary-s"));
    }

    #[test]
    fn the_flavours_are_the_same_file() {
        let found = candidates(Path::new("/home/x/.config"));
        assert!(found.contains(&PathBuf::from("/home/x/.config/Code/User/keybindings.json")));
        assert!(found.contains(&PathBuf::from(
            "/home/x/.config/VSCodium/User/keybindings.json"
        )));
    }
}
