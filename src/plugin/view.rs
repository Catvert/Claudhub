//! What a plugin draws: a tree of data, and nothing that knows gpui.
//!
//! This is the same split as `notes.rs` in front of `notes_view.rs`, pushed one
//! notch further: the script produces a `Node`, `ui::plugin_view` paints it.
//! The tree being plain data is what makes it testable, comparable and
//! serialisable — a plugin's rendering can be checked without a window.
//!
//! **The vocabulary is bounded on purpose.** It is expressive enough for a
//! master/detail panel with a code excerpt, and deliberately not enough to be a
//! second UI framework. What a plugin will never get, and it has to stay that
//! way: virtualised lists of several thousand rows with resizable columns, a
//! wheel smoothing of its own, a focus context of its own, a gesture on a diff
//! line. `List` is drawn with `uniform_list` at `theme::row_height`, so an item
//! is two storeys at most and every one of them the same height — that is the
//! bound, and it is the rest of the window's bound too.
//!
//! **No text field**, and that is the one omission worth stating: an input
//! needs an `InputState` created once in a constructor and living across
//! frames, which a tree rebuilt at will cannot own. What a plugin has to be
//! told is declared in the settings.

use serde::{Deserialize, Serialize};

/// What a gesture sends back to the script.
///
/// A function name and an opaque payload, rather than a closure: a closure
/// could not cross the boundary, and a name is what a hot reload can rebind to
/// freshly compiled code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handler {
    /// The `action` argument handed to the script's `update`.
    pub action: String,
    /// Carried through untouched. The script decides what it means.
    pub payload: String,
}

impl Handler {
    pub fn new(action: impl Into<String>, payload: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            payload: payload.into(),
        }
    }
}

/// How something reads on the severity scale.
///
/// Five tones and not a colour: a plugin says what a value *means* — an error,
/// a warning, something that went well — and the theme says what that looks
/// like. It is the rule [`TextStyle`] already follows one notch lower.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Tone {
    #[default]
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}

impl Tone {
    /// What a script names it. Anything else is `Neutral`, which is what an
    /// unknown severity should read as rather than an error.
    pub fn named(name: &str) -> Self {
        match name.trim() {
            "info" => Self::Info,
            "success" | "ok" => Self::Success,
            "warning" | "warn" => Self::Warning,
            "danger" | "error" | "fatal" => Self::Danger,
            _ => Self::Neutral,
        }
    }
}

/// One line of a [`Node::Fields`] table: a name, its value, and how it reads.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Pair {
    pub key: String,
    pub value: String,
    pub tone: Tone,
}

/// How a piece of text reads. Four roles and not a style sheet: a plugin says
/// what a string *is*, the theme says what it looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TextStyle {
    /// A heading inside the panel.
    Title,
    #[default]
    Body,
    /// Secondary, in the muted colour: a date, a count, an origin.
    Dim,
    /// Fixed pitch, in the diff's font: an identifier, a sha, a path.
    Mono,
}

/// One row of a `List`. Two storeys at most — see the module's note on heights.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Item {
    pub title: String,
    /// The second storey, greyed. `None` keeps the row on one line.
    pub detail: Option<String>,
    /// Right-aligned, in a pill: a count, a status, a duration.
    pub badge: Option<String>,
    /// A Lucide name, resolved the way every other icon of the window is. A
    /// name matching no file paints nothing and reports no error — that is
    /// gpui-component's behaviour, not ours.
    pub icon: Option<String>,
}

/// A node of what a plugin draws.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Node {
    Column(Vec<Node>),
    Row(Vec<Node>),
    /// A collapsible block with its title.
    ///
    /// **Which way it starts is the script's, whether it is folded now is the
    /// panel's.** A fold is a reading posture — it changes several times in a
    /// session and is not persisted — but where it starts is a statement about
    /// the content: a distribution one glances at once belongs behind its
    /// title, a stack trace does not. The panel therefore keeps the sections
    /// that differ from what the script asked for, which is `tree::Folds`'
    /// polarity one floor up: remembering the exception keeps the set small.
    Section {
        title: String,
        body: Vec<Node>,
        /// Folded until someone opens it.
        folded: bool,
    },
    Text {
        text: String,
        style: TextStyle,
    },
    /// An excerpt, coloured by the same table that colours the diffs when the
    /// language is known.
    ///
    /// `start_line` numbers it — a stack frame's excerpt without its numbers is
    /// a block one has to count through to find the line the trace named — and
    /// `mark` is that line, painted the way a diff paints the row one is on.
    /// Both are **absolute** line numbers, one-based, as every tool that quotes
    /// a file writes them.
    Code {
        text: String,
        language: Option<String>,
        start_line: Option<usize>,
        mark: Option<usize>,
    },
    List {
        /// Stable across renders: it keys the scroll handle, and a new id on
        /// every frame would put the list back at the top on every frame.
        id: String,
        items: Vec<Item>,
        selected: Option<usize>,
        on_select: Option<Handler>,
    },
    /// A line of text one can type into.
    ///
    /// **The tree does not own it.** An `InputState` has to live across frames
    /// — it holds the caret and the selection — and this tree is rebuilt at
    /// will. What owns it is the window, under `id`, exactly as the settings
    /// form owns its rows' fields. The `id` must therefore be stable: a new one
    /// each frame is a field that loses the caret on every keystroke.
    ///
    /// `value` seeds it and **does not follow it afterwards**: the field is the
    /// source of truth while one types in it, which is the free note's rule —
    /// putting the state's text back under the fingers would move the caret in
    /// the middle of a word.
    Field {
        id: String,
        value: String,
        placeholder: String,
        /// Handed the text on every keystroke, in the payload.
        on_change: Option<Handler>,
    },
    Button {
        label: String,
        icon: Option<String>,
        on_click: Option<Handler>,
        disabled: bool,
        /// Solid rather than outlined: the one gesture the panel is for.
        primary: bool,
    },
    /// A pill: a level, a count, a status. Inline, in its tone's colour.
    Badge {
        text: String,
        tone: Tone,
    },
    /// A sentence behind a coloured rule — the shape a message takes when it is
    /// the thing the panel is about, not a line among others.
    Callout {
        text: String,
        tone: Tone,
    },
    /// A table of names and values, laid out in two columns.
    ///
    /// It is the one shape a `Row` of `Text` could not give: the values line up
    /// with each other, which is what makes eight of them readable at a glance
    /// rather than eight sentences.
    Fields {
        rows: Vec<Pair>,
    },
    /// A proportion, drawn: a label, a bar, and the value it stands for.
    ///
    /// **A whole percent and not a float**, so the tree stays comparable — the
    /// whole vocabulary is `Eq`, which is what lets a plugin's rendering be
    /// asserted in a test.
    Meter {
        label: String,
        value: String,
        percent: u8,
    },
    /// A rule between two blocks.
    Divider,
    /// The child takes whatever height is left.
    ///
    /// **It is what makes a list a list panel rather than a block in a column.**
    /// Without it, a list has to be given a height in advance, and sixteen rows
    /// was the number picked: past that it scrolled inside a column that
    /// scrolled too — two scrollbars for one gesture, neither of them the
    /// panel's — and under it the panel ended in a band of nothing. Said by the
    /// script and not guessed by the panel: a plugin knows which of its blocks
    /// is the one being read, and there is no reading of a tree that says it.
    Fill(Box<Node>),
    /// Something outside, opened by the system's browser.
    ///
    /// A `Button` could not do it: its gesture goes back to the script, and a
    /// script has no way to reach a browser — which is why the first version of
    /// the Sentry plugin had a permalink button that did nothing at all.
    Link {
        label: String,
        url: String,
        icon: Option<String>,
    },
    /// The empty state, with the same shape as everywhere else in the window.
    Empty {
        message: String,
    },
    /// Something has gone out and not come back.
    Spinner,
}

impl Node {
    /// An empty column, which is what a plugin that has nothing to say renders.
    pub fn nothing() -> Self {
        Node::Column(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tree crosses serialisation unchanged.
    ///
    /// It is not decoration: this is what lets a plugin's rendering be recorded
    /// in a test, and what would let the tree travel one day if the host ever
    /// moved out of the interface's process.
    #[test]
    fn the_tree_survives_a_round_trip() {
        let tree = Node::Column(vec![
            Node::Text {
                text: "Runs".into(),
                style: TextStyle::Title,
            },
            Node::List {
                id: "runs".into(),
                items: vec![Item {
                    title: "release".into(),
                    detail: Some("il y a 3 min".into()),
                    badge: Some("failure".into()),
                    icon: Some("circle-x".into()),
                }],
                selected: Some(0),
                on_select: Some(Handler::new("select", "42")),
            },
            Node::Code {
                text: "fn main() {}".into(),
                language: Some("rust".into()),
                start_line: Some(12),
                mark: Some(13),
            },
            Node::Fields {
                rows: vec![Pair {
                    key: "level".into(),
                    value: "error".into(),
                    tone: Tone::Danger,
                }],
            },
            Node::Meter {
                label: "production".into(),
                value: "100%".into(),
                percent: 100,
            },
            Node::Badge {
                text: "error".into(),
                tone: Tone::Danger,
            },
            Node::Callout {
                text: "File does not exist".into(),
                tone: Tone::Danger,
            },
            Node::Divider,
            Node::Section {
                title: "Répartition".into(),
                body: vec![Node::Divider],
                folded: true,
            },
            Node::Fill(Box::new(Node::Divider)),
            Node::Link {
                label: "Sentry".into(),
                url: "https://sentry.io/…".into(),
                icon: Some("external-link".into()),
            },
            Node::Field {
                id: "project".into(),
                value: "site".into(),
                placeholder: "acme/site".into(),
                on_change: Some(Handler::new("set-project", String::new())),
            },
        ]);
        let json = serde_json::to_string(&tree).expect("a tree serialises");
        let back: Node = serde_json::from_str(&json).expect("and reads back");
        assert_eq!(tree, back);
    }
}
