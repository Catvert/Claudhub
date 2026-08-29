//! The tool windows, and the three rails that name them.
//!
//! A tool window is a panel one calls up beside what one is reading — the file
//! list, the notes, a terminal — as against a **document**, which is what one
//! is reading. The difference is not the panel's nature but its place: the
//! documents share the centre, the tool windows sit against an edge, and each
//! edge carries a rail of buttons saying what is there and what is showing.
//!
//! **There is no second list.** A rail is computed, every frame, from what the
//! dock's tree actually holds — `Seat` is that reading, flattened. A panel
//! dragged from the left edge to the right appears on the other rail at the
//! next frame without anything being kept up to date, which is the only way
//! two views of one arrangement cannot drift apart. `Tool::home` says where a
//! panel *starts*, and is read at exactly two moments: building the default
//! layout, and putting back a panel that has left the tree altogether.
//!
//! Pure, and that is what makes the decision testable: the pressing of a
//! button, the dimming, the ordering and the zen fold are worked out here, and
//! `ui::dock_layout` does no more than paint the answer and carry it out.

use gpui::{px, Pixels};

/// An edge a tool window can hold.
///
/// The centre is not one: what sits there is a document, and a document has no
/// button — one reaches it by its tab, among the others it shares the group
/// with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
    Bottom,
}

/// Which half of an edge a tool window sits in.
///
/// A side rail is read in two runs — those pinned to the top, those pushed to
/// the bottom — and each run is a tab group of its own, so the two show at the
/// same time rather than taking turns. It is what lets the file list and the
/// tests be on screen together without either being a tab of the other.
///
/// The bottom edge has one slot: it is already a band across the width, and
/// splitting it would give two strips a terminal's worth of height each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Half {
    Top,
    Bottom,
}

impl Half {
    pub const BOTH: [Half; 2] = [Half::Top, Half::Bottom];
}

/// A place a tool window can sit: an edge, and which half of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Anchor {
    pub side: Side,
    pub half: Half,
}

impl Anchor {
    pub const fn new(side: Side, half: Half) -> Self {
        Self { side, half }
    }

    /// Every place a tool window may be sent to, in the order the menu offers
    /// them: the two halves of each side, then the bottom, which has one.
    pub fn all() -> Vec<Anchor> {
        let mut all = Vec::with_capacity(5);
        for side in [Side::Left, Side::Right] {
            for half in Half::BOTH {
                all.push(Anchor::new(side, half));
            }
        }
        all.push(Anchor::new(Side::Bottom, Half::Top));
        all
    }

    /// The i18n key naming this place in the menu.
    pub fn label(self) -> &'static str {
        match (self.side, self.half) {
            (Side::Left, Half::Top) => "anchor-left-top",
            (Side::Left, Half::Bottom) => "anchor-left-bottom",
            (Side::Right, Half::Top) => "anchor-right-top",
            (Side::Right, Half::Bottom) => "anchor-right-bottom",
            (Side::Bottom, _) => "anchor-bottom",
        }
    }
}

impl Side {
    /// The three, in the order the rails are laid around the centre.
    pub const ALL: [Side; 3] = [Side::Left, Side::Right, Side::Bottom];

    /// How many slots this edge is read in.
    pub fn halves(self) -> &'static [Half] {
        match self {
            Side::Bottom => &[Half::Top],
            _ => &[Half::Top, Half::Bottom],
        }
    }

    /// Its rank in `ALL` — the index of its slot wherever the three are held
    /// side by side.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    /// What a zone of this edge is worth when the area has to make one.
    ///
    /// The area builds a missing region at twice the minimum panel size, which
    /// is not a width anybody chose; this is the one we did. The left is the
    /// column of trees and lists, the right is read across rather than down —
    /// a note is a paragraph — and the bottom is a terminal, which wants lines
    /// more than it wants columns.
    pub fn default_size(self) -> Pixels {
        match self {
            Side::Left => px(350.),
            Side::Right => px(320.),
            Side::Bottom => px(240.),
        }
    }

    /// The height of the **bottom half**, which is the one given a size.
    ///
    /// A height and not the width above: inside a side zone the split runs
    /// down, so the two are different measurements and one of them was being
    /// read for the other. Nothing starts in the right's bottom half — that
    /// edge is one group — so its size is what a panel dragged there is given,
    /// and it is the taller because what lands on that edge is read in
    /// paragraphs, where the left's half is a list one glances at.
    pub fn half_size(self) -> Pixels {
        match self {
            Side::Left => px(240.),
            Side::Right => px(460.),
            // One slot: nothing to size against.
            Side::Bottom => px(0.),
        }
    }
}

/// What a button is called: a key of `assets/i18n`.
///
/// It carried a second arm — words a plugin wrote in its own language, which no
/// catalogue of ours held. Every view is ours again, so every name is a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Label(pub &'static str);

impl Label {
    /// The words to paint.
    ///
    /// A `SharedString` and not a `String`: a rail is rebuilt every frame, and
    /// a compiled catalogue hands back a borrow.
    pub fn text(self) -> gpui::SharedString {
        crate::tr!(self.0)
    }
}

/// A tool window, as the rails know it.
#[derive(Debug, Clone, Copy)]
pub struct Tool {
    /// The name the dock's registry builds it by, and the one a saved layout
    /// holds.
    pub panel: &'static str,
    /// What its button is called — the same thing its tab says.
    pub title: Label,
    /// A Lucide name from `assets/icons`.
    pub icon: &'static str,
    /// Where the default layout puts it, and where `Press::Restore` puts it
    /// back. **Not** a reading of where it is: that is `anchor_of`.
    pub home: Anchor,
    /// It shows itself only when it has something to show — the conflicts one
    /// time in a hundred, the tests where a runner exists, the run once one
    /// has been started.
    ///
    /// **No button while it has nothing**, which is what tells a situational
    /// view from a permanent one: a target that is there but never answers is
    /// one the eye learns to skip.
    pub conditional: bool,
}

/// The tool windows, in the order their buttons sit.
///
/// **A tool window only ever joins at the end**, and it is not an aesthetic
/// choice: the ranks of this table are what `Alt+1`… name, and one inserted in
/// the middle would move every key after it. It is the rule `Workspace::ALL`
/// carried, and it survives it.
///
/// The order does two things at once, and both are read. It is the order the
/// buttons sit in on a rail; and **within one half it is the order of the
/// tabs**, so whichever comes first is the view that half shows — which is why
/// the notes open the left's bottom half rather than the tests.
///
/// The first nine are the ones a key names, so they are the ones one reaches
/// for: what changed, on which branch, in which file, found where; then what
/// one had to say about it and whether it passes; then what happened, in which
/// table, and the shell one says it to.
pub const TOOLS: &[Tool] = &[
    // The left's top half, in the order the hand reaches for it: the tree of
    // files first, because a project is opened on a file.
    Tool {
        panel: "ClaudhubFiles",
        title: Label("panel-files"),
        icon: "pencil",
        home: Anchor::new(Side::Left, Half::Top),
        conditional: false,
    },
    Tool {
        panel: "ClaudhubSearch",
        title: Label("panel-search"),
        icon: "search",
        home: Anchor::new(Side::Left, Half::Top),
        conditional: false,
    },
    Tool {
        panel: "ClaudhubChanges",
        title: Label("range-working"),
        icon: "git-commit-horizontal",
        home: Anchor::new(Side::Left, Half::Top),
        conditional: false,
    },
    // **Not `git-branch`**: what this shows is not the branches but one branch
    // weighed against its base, which is what a pull request is. The glyph is
    // the history's, where it opens the column of branches.
    Tool {
        panel: "ClaudhubBranch",
        title: Label("range-branch"),
        icon: "git-pull-request",
        home: Anchor::new(Side::Left, Half::Top),
        conditional: false,
    },
    // **Beside the files and not under them**: a failing test is picked from
    // the same way a file is — one reads the list, one opens what it names.
    // The half below is for what one keeps an eye on while doing that.
    Tool {
        panel: "ClaudhubTests",
        title: Label("panel-tests"),
        icon: "circle-check",
        home: Anchor::new(Side::Left, Half::Top),
        // Only where a runner exists: on everything else the honest panel is
        // no panel — there is nothing to run.
        conditional: true,
    },
    // First of the bottom half, so it is the one that half shows: the notes are
    // read *while* choosing a file, which is the whole reason they are not a
    // tab of the five above.
    Tool {
        panel: "ClaudhubNotes",
        title: Label("panel-notes"),
        icon: "sticky-note",
        home: Anchor::new(Side::Left, Half::Bottom),
        conditional: false,
    },
    // The right edge holds one group and not two halves: what it carries is
    // read across rather than picked from, and a schema unfolded beside the
    // queries already run is two lists competing for the same column.
    Tool {
        panel: "ClaudhubDb",
        title: Label("panel-databases"),
        icon: "database",
        home: Anchor::new(Side::Right, Half::Top),
        conditional: false,
    },
    // The band across the width, and the two that want it: a run scrolls,
    // and a graph of commits is read left to right. Neither fits a column.
    Tool {
        panel: "ClaudhubTestRun",
        title: Label("panel-test-run"),
        icon: "play",
        home: Anchor::new(Side::Bottom, Half::Top),
        conditional: true,
    },
    Tool {
        panel: "ClaudhubHistory",
        title: Label("panel-history"),
        icon: "history",
        home: Anchor::new(Side::Bottom, Half::Top),
        conditional: false,
    },
    // Past the ninth, no key names it: `Alt+1`… stop here, and the rest is
    // reached by its button. The terminals are what can afford to be there:
    // `Ctrl+T` already calls them up by name, which no other tool window has.
    Tool {
        panel: "ClaudhubTerminal",
        title: Label("panel-terminal"),
        icon: "square-terminal",
        home: Anchor::new(Side::Bottom, Half::Top),
        conditional: false,
    },
    Tool {
        panel: "ClaudhubSqlHistory",
        title: Label("panel-sql-history"),
        icon: "list",
        home: Anchor::new(Side::Right, Half::Top),
        conditional: false,
    },
    // Nothing to resolve, no button: one time in a hundred.
    Tool {
        panel: "ClaudhubConflicts",
        title: Label("panel-conflicts"),
        icon: "git-merge",
        home: Anchor::new(Side::Left, Half::Bottom),
        conditional: true,
    },
    Tool {
        panel: "ClaudhubStashes",
        title: Label("panel-stashes"),
        icon: "archive",
        home: Anchor::new(Side::Left, Half::Bottom),
        conditional: false,
    },
    // **On the right, where one reads where things stand.** A list of errors
    // answers "what is happening in production", which is of a kind with the
    // history and the runs beside it — and what one does with one is open it in
    // the centre, so it has no business among what fills that centre.
    Tool {
        panel: "ClaudhubSentry",
        title: Label("panel-sentry"),
        icon: "triangle-alert",
        home: Anchor::new(Side::Right, Half::Top),
        conditional: false,
    },
    // And beside what happened: a run answers "what became of this branch",
    // which is the edge one reads where one stands.
    Tool {
        panel: "ClaudhubCi",
        title: Label("panel-ci"),
        icon: "github",
        home: Anchor::new(Side::Right, Half::Top),
        conditional: false,
    },
    Tool {
        panel: "ClaudhubTags",
        title: Label("panel-tags"),
        icon: "tags",
        home: Anchor::new(Side::Left, Half::Bottom),
        conditional: false,
    },
];

/// Every tool window.
///
/// A function and no longer the table read directly: it took the plugins'
/// panels after ours, which is what made a rail's contents a run-time question.
/// There are none, so it is the table — kept as a function because two dozen
/// call sites read it and a rail is not where one saves a `Vec`.
pub fn tools() -> Vec<Tool> {
    TOOLS.to_vec()
}

/// What one edge may carry before the rail stops being readable.
///
/// A rail does **not** scroll: one that has to be scrolled is one that can no
/// longer be aimed at, which is the whole of what a rail is for. Nine is the
/// number of the keys, and a test holds the table to it — a tool window too
/// many is then a decision taken at compile time rather than an icon quietly
/// cropped.
pub const MAX_PER_RAIL: usize = 9;

/// Where a panel sits, as the dock's tree says.
///
/// The only input the rails have, and the reason there is no second list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    pub panel: String,
    /// `None` is the centre — a document, so no button.
    pub anchor: Option<Anchor>,
    /// It is the displayed tab of its group.
    pub shown: bool,
    /// Its zone is unfolded.
    pub open: bool,
    /// The panel draws itself at all — `BasePanel::visible`.
    ///
    /// Read off the panel and not worked out here: what makes a view invisible
    /// is its own business, and the reasons differ — put away by hand, no
    /// runner to run, nothing to resolve.
    pub visible: bool,
    /// Its rank among its group's tabs, which is the order its rail shows.
    pub order: usize,
}

/// One button of a rail, as the view has to paint it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub panel: &'static str,
    pub title: Label,
    pub icon: &'static str,
    /// On screen: its zone unfolded, its tab in front, and not put away.
    ///
    /// The only state a button carries. A view put away and a view behind
    /// another are both simply not on screen, and a rail that told them apart
    /// would be answering a question nobody asks of it.
    pub active: bool,
}

/// One edge's rail, read in two runs.
///
/// `top` is pinned to the start of the edge and `bottom` pushed to its end,
/// which is what makes the two halves legible as two: a single run of nine
/// buttons says nothing about which of them can be on screen together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rail {
    pub side: Side,
    pub top: Vec<Button>,
    pub bottom: Vec<Button>,
}

impl Rail {
    pub fn is_empty(&self) -> bool {
        self.top.is_empty() && self.bottom.is_empty()
    }

    pub fn buttons(&self) -> impl Iterator<Item = &Button> {
        self.top.iter().chain(self.bottom.iter())
    }
}

/// What pressing a button means. Decided here, carried out by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    /// The zone is folded, or another tab is in front: unfold, bring forward.
    Reveal { panel: &'static str, side: Side },
    /// It is already on screen: put its half away.
    ///
    /// **The half, and neither the view alone nor the whole zone.** Putting
    /// away one view of a half left the next one showing in its place — one
    /// asked to close the tests and got the errors, under a rail where nothing
    /// was lit. Folding the zone would have taken the other half's work off the
    /// screen with it.
    ///
    /// A half left with nothing visible stops being drawn on its own, and the
    /// other takes the room: that is what makes the two fold independently
    /// without a second notion of folding beside the dock's.
    Hide { anchor: Anchor },
    /// The panel is not in the tree — hidden, or dragged out of everything:
    /// put it back where the default puts it.
    Restore { panel: &'static str, anchor: Anchor },
}

/// The tool this name declares, if it is one.
pub fn tool(panel: &str) -> Option<Tool> {
    tools().into_iter().find(|tool| tool.panel == panel)
}

/// The seat a panel of this name holds, if the tree holds one.
fn seat<'a>(panel: &str, seats: &'a [Seat]) -> Option<&'a Seat> {
    seats.iter().find(|seat| seat.panel == panel)
}

/// The edge a panel sits against, `None` for the centre or for absent.
///
/// What tells a tool window from a document, which is what decides whether
/// showing it is a step of the trail — see `ui::jumps`.
pub fn anchor_of(panel: &str, seats: &[Seat]) -> Option<Anchor> {
    seat(panel, seats).and_then(|seat| seat.anchor)
}

/// The edge a panel sits against — `None` for the centre or for absent.
pub fn side_of(panel: &str, seats: &[Seat]) -> Option<Side> {
    anchor_of(panel, seats).map(|anchor| anchor.side)
}

/// Where a tool window may be sent, its own place left out.
///
/// A menu offering the place one is already in is a menu with a line that does
/// nothing. A panel out of the tree has no place to leave out, so it is offered
/// all of them.
pub fn moves(panel: &str, seats: &[Seat]) -> Vec<Anchor> {
    let here = anchor_of(panel, seats);
    Anchor::all()
        .into_iter()
        .filter(|anchor| Some(*anchor) != here)
        .collect()
}

/// The three rails, from what the dock holds, what is folded and what is off.
///
/// **Folded and off are two different things**, and the rail is where the
/// difference shows: a folded view keeps its button — that press is how one
/// gets it back — and one taken off has none at all, which is the whole of what
/// taking it off means. The title bar's "Views" menu is where it comes back
/// from, and it is the only place, so that menu lists exactly what a rail can
/// carry.
///
/// A tool window with no seat still gets a button — that is how one calls back
/// a view the dock no longer holds — **unless** its tab comes and goes with its
/// content: nothing would make a conditional panel appear, so its button would
/// do nothing.
///
/// **The order is the tree's**, tab by tab, and the table's only for what the
/// tree does not hold. It was the table's alone, on the reasoning that a rail
/// which reordered itself under the hand could not be aimed at twice — but a
/// tab order does not move on its own, it moves when one moves it, and reading
/// the tree is what lets one put the files before the changes without editing
/// the source. What has no seat goes last: a view called back has to land
/// somewhere, and the end is the one place that displaces nothing.
pub fn rails(
    seats: &[Seat],
    folded: &std::collections::BTreeSet<String>,
    off: &std::collections::BTreeSet<String>,
) -> [Rail; 3] {
    // Ranked while collecting, sorted once at the end: a button's place is its
    // panel's place among its group's tabs.
    struct Ranked {
        side: Side,
        top: Vec<(Option<usize>, Button)>,
        bottom: Vec<(Option<usize>, Button)>,
    }
    let mut rails = Side::ALL.map(|side| Ranked {
        side,
        top: Vec::new(),
        bottom: Vec::new(),
    });
    for tool in tools() {
        // Taken off its rail: no button, and that is the point.
        if off.contains(tool.panel) {
            continue;
        }
        let seat = seat(tool.panel, seats);
        // At the centre it is a document: reached by its tab, among the ones
        // it shares its group with.
        if seat.is_some_and(|seat| seat.anchor.is_none()) {
            continue;
        }
        // **A situational view has no button while it has nothing to show.**
        // The conflicts are the case that names the rule: they are one time in
        // a hundred, and a permanent button for them is a target one learns to
        // ignore. The tests and the run being followed are the same — no
        // runner, no run, no button.
        //
        // Folded **by hand** is the exception, and it has to be: that is the
        // one invisibility one calls the view back from, so its button stays.
        let away = folded.contains(tool.panel);
        if tool.conditional && !away && !seat.is_some_and(|seat| seat.visible) {
            continue;
        }
        let anchor = match seat {
            Some(seat) => seat.anchor.expect("just tested"),
            None => tool.home,
        };
        let button = Button {
            panel: tool.panel,
            title: tool.title,
            icon: tool.icon,
            active: seat.is_some_and(|seat| seat.open && seat.shown && seat.visible),
        };
        // Seated, so the tree says where it goes; loose, so it goes last.
        let rank = seat.map(|seat| seat.order);
        let rail = &mut rails[anchor.side.index()];
        match anchor.half {
            Half::Top => rail.top.push((rank, button)),
            Half::Bottom => rail.bottom.push((rank, button)),
        }
    }
    for rail in &mut rails {
        // Stable, so two panels the tree cannot rank keep the table's order.
        rail.top.sort_by_key(|(rank, _)| rank.unwrap_or(usize::MAX));
        rail.bottom
            .sort_by_key(|(rank, _)| rank.unwrap_or(usize::MAX));
    }
    let mut rails = rails.map(|rail| Rail {
        side: rail.side,
        top: rail.top.into_iter().map(|(_, button)| button).collect(),
        bottom: rail.bottom.into_iter().map(|(_, button)| button).collect(),
    });
    let _ = &mut rails;
    // The rail does not scroll, so what it cannot show it hides. A **warning**
    // and not an assertion: our own table is held to the cap by a test, but a
    // plugin's panels join a rail at run time, and a window one has installed
    // too many plugins into is not a window that should stop.
    for rail in &rails {
        if rail.buttons().count() > MAX_PER_RAIL {
            log::warn!(
                "the {:?} rail carries {} buttons, more than it can show",
                rail.side,
                rail.buttons().count()
            );
        }
    }
    rails
}

/// Whether a view belongs in the menu that takes views off their rails.
///
/// **What one takes off is a view whose button is a fixture.** A situational
/// one — the conflicts, a run being followed — has a button only while it has
/// something to show, so offering to take it off is offering to remove what is
/// not there. The one exception is a view already off: that is the state one
/// has to be able to come back from, and this menu is the only way back.
pub fn in_view_menu(tool: &Tool, off: &std::collections::BTreeSet<String>) -> bool {
    !tool.conditional || off.contains(tool.panel)
}

/// What pressing a button does.
pub fn press(panel: &str, seats: &[Seat]) -> Press {
    let Some(tool) = tool(panel) else {
        // Not one of ours: nothing sensible to do, and the caller has no
        // button to have pressed.
        return Press::Restore {
            panel: "",
            anchor: Anchor::new(Side::Left, Half::Top),
        };
    };
    match seat(panel, seats) {
        Some(seat) => match seat.anchor {
            // On screen: the press puts its half away — see `Hide`.
            Some(anchor) if seat.open && seat.shown && seat.visible => Press::Hide { anchor },
            Some(anchor) => Press::Reveal {
                panel: tool.panel,
                side: anchor.side,
            },
            // A document has no button; pressing one anyway brings it forward.
            None => Press::Reveal {
                panel: tool.panel,
                side: tool.home.side,
            },
        },
        None => Press::Restore {
            panel: tool.panel,
            anchor: tool.home,
        },
    }
}

/// The tool windows one half is showing — what putting that half away takes
/// off the screen.
///
/// Named rather than counted: the caller hides them one by one, each by the
/// name the dock knows it by.
pub fn showing_in(anchor: Anchor, seats: &[Seat]) -> Vec<&'static str> {
    tools()
        .into_iter()
        .filter(|tool| {
            seats
                .iter()
                .any(|seat| seat.panel == tool.panel && seat.anchor == Some(anchor) && seat.visible)
        })
        .map(|tool| tool.panel)
        .collect()
}

/// The zen fold, and the way back.
///
/// Folding the three is easy; what makes it zen rather than destructive is
/// coming back to exactly what was unfolded. `folded` is what the previous zen
/// put away — empty when one is not in zen — and the answer is the new opening
/// of the three edges and what the next return has to give back.
///
/// Nothing unfolded to start with means the gesture has nothing to hide, and
/// it gives back the last fold instead: the key is one key, and a zen one
/// cannot leave is a trap.
pub fn zen(open: [bool; 3], folded: &[Side]) -> ([bool; 3], Vec<Side>) {
    if folded.is_empty() {
        let hiding: Vec<Side> = Side::ALL
            .into_iter()
            .filter(|side| open[side.index()])
            .collect();
        return ([false; 3], hiding);
    }
    let mut back = open;
    for side in folded {
        back[side.index()] = true;
    }
    (back, Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn seat(panel: &str, anchor: Option<Anchor>, shown: bool, open: bool) -> Seat {
        Seat {
            panel: panel.to_string(),
            anchor,
            shown,
            open,
            visible: true,
            order: 0,
        }
    }

    /// The same, put away by hand.
    fn away(panel: &str, anchor: Option<Anchor>) -> Seat {
        Seat {
            visible: false,
            ..seat(panel, anchor, true, true)
        }
    }

    fn at(side: Side, half: Half) -> Option<Anchor> {
        Some(Anchor::new(side, half))
    }

    fn none() -> BTreeSet<String> {
        BTreeSet::new()
    }

    fn rail(rails: &[Rail; 3], side: Side) -> &Rail {
        &rails[side.index()]
    }

    fn names(buttons: &[Button]) -> Vec<&str> {
        buttons.iter().map(|button| button.panel).collect()
    }

    fn all_names(rail: &Rail) -> Vec<&str> {
        rail.buttons().map(|button| button.panel).collect()
    }

    /// A panel of the centre is a document: it is reached by its tab, and no
    /// rail claims it.
    #[test]
    fn a_panel_of_the_centre_has_no_button() {
        let seats = vec![seat("ClaudhubNotes", None, true, true)];
        let rails = rails(&seats, &none(), &none());
        assert!(Side::ALL
            .iter()
            .all(|side| !all_names(rail(&rails, *side)).contains(&"ClaudhubNotes")));
    }

    /// Dragged from one edge to the other, a panel appears on the other rail
    /// and nowhere else — with nothing kept up to date for it to do so. This
    /// is the whole reason the seats are read from the tree.
    #[test]
    fn a_panel_dragged_across_changes_rail() {
        let seats = vec![seat(
            "ClaudhubNotes",
            at(Side::Left, Half::Top),
            false,
            true,
        )];
        let rails = rails(&seats, &none(), &none());
        assert!(names(&rail(&rails, Side::Left).top).contains(&"ClaudhubNotes"));
        assert!(!all_names(rail(&rails, Side::Right)).contains(&"ClaudhubNotes"));
    }

    /// And from one half to the other: the two runs of a rail are read off the
    /// tree exactly as the two edges are.
    #[test]
    fn a_panel_dragged_down_changes_half() {
        let up = vec![seat(
            "ClaudhubNotes",
            at(Side::Right, Half::Top),
            false,
            true,
        )];
        let down = vec![seat(
            "ClaudhubNotes",
            at(Side::Right, Half::Bottom),
            false,
            true,
        )];
        assert!(
            names(&rail(&rails(&up, &none(), &none()), Side::Right).top).contains(&"ClaudhubNotes")
        );
        assert!(
            names(&rail(&rails(&down, &none(), &none()), Side::Right).bottom)
                .contains(&"ClaudhubNotes")
        );
    }

    /// Active is "unfolded **and** in front". The displayed tab of a folded
    /// zone is not on screen, and a button lit for it would be lying.
    #[test]
    fn only_a_shown_tab_of_an_unfolded_zone_is_active() {
        let lit = |shown, open| {
            let seats = vec![seat(
                "ClaudhubNotes",
                at(Side::Right, Half::Top),
                shown,
                open,
            )];
            rails(&seats, &none(), &none())[Side::Right.index()].top[0].active
        };
        assert!(lit(true, true));
        assert!(!lit(true, false));
        assert!(!lit(false, true));
    }

    /// Pressing what is on screen puts **its half** away — not the view alone,
    /// which would leave the next tab of the half showing in its place, and not
    /// the zone, whose other half is somebody else's work.
    #[test]
    fn pressing_what_is_on_screen_puts_its_half_away() {
        let open = vec![seat(
            "ClaudhubNotes",
            at(Side::Right, Half::Top),
            true,
            true,
        )];
        assert_eq!(
            press("ClaudhubNotes", &open),
            Press::Hide {
                anchor: Anchor::new(Side::Right, Half::Top)
            }
        );
        // And pressing it again brings it back — from a view put away as from
        // a folded zone.
        let put_away = vec![away("ClaudhubNotes", at(Side::Right, Half::Top))];
        let folded = vec![seat(
            "ClaudhubNotes",
            at(Side::Right, Half::Top),
            true,
            false,
        )];
        for seats in [&put_away, &folded] {
            assert_eq!(
                press("ClaudhubNotes", seats),
                Press::Reveal {
                    panel: "ClaudhubNotes",
                    side: Side::Right,
                }
            );
        }
    }

    /// Pressing a tab that is **not** in front selects it. Folding there would
    /// hide the zone one has just asked to look into.
    #[test]
    fn pressing_a_tab_behind_selects_it() {
        let seats = vec![
            seat("ClaudhubNotes", at(Side::Right, Half::Top), true, true),
            seat("ClaudhubHistory", at(Side::Right, Half::Top), false, true),
        ];
        assert_eq!(
            press("ClaudhubHistory", &seats),
            Press::Reveal {
                panel: "ClaudhubHistory",
                side: Side::Right,
            }
        );
    }

    /// A view put away keeps its button — unlit — and that is where one calls
    /// it back from. A button that vanished would be a target that moves.
    #[test]
    fn a_view_put_away_keeps_an_unlit_button() {
        let hidden = BTreeSet::from(["ClaudhubNotes".to_string()]);
        // Put away, so the panel draws nothing — which is what the seat says.
        let seats = vec![away("ClaudhubNotes", at(Side::Right, Half::Top))];
        let button = rails(&seats, &hidden, &none())[Side::Right.index()]
            .buttons()
            .find(|button| button.panel == "ClaudhubNotes")
            .cloned()
            .expect("the button stays");
        assert!(!button.active, "put away is not on screen");
        // Out of the tree entirely, the press puts it back where it belongs.
        assert_eq!(
            press("ClaudhubNotes", &[]),
            Press::Restore {
                panel: "ClaudhubNotes",
                anchor: Anchor::new(Side::Left, Half::Bottom),
            }
        );
    }

    /// And what goes with it is everything that half is showing: closing the
    /// tests must not hand the half over to the errors beside them.
    #[test]
    fn putting_a_half_away_takes_what_it_shows() {
        let here = at(Side::Right, Half::Bottom);
        let seats = vec![
            seat("ClaudhubTests", here, true, true),
            seat("ClaudhubStashes", here, false, true),
            // The other half stays: it is somebody else's work.
            seat("ClaudhubNotes", at(Side::Right, Half::Top), true, true),
        ];
        let going = showing_in(Anchor::new(Side::Right, Half::Bottom), &seats);
        assert!(going.contains(&"ClaudhubTests"));
        assert!(going.contains(&"ClaudhubStashes"));
        assert!(!going.contains(&"ClaudhubNotes"));
    }

    /// A situational view has no button while it has nothing to show — the
    /// conflicts, which are one time in a hundred, and a target that is there
    /// but never answers is one the eye learns to skip.
    ///
    /// Being in the tree is not enough: it is there from the first frame, and
    /// invisible for all but a few of them.
    #[test]
    fn a_situational_view_with_nothing_to_show_has_no_button() {
        let idle = vec![Seat {
            visible: false,
            ..seat(
                "ClaudhubConflicts",
                at(Side::Left, Half::Bottom),
                false,
                true,
            )
        }];
        assert!(
            !all_names(rail(&rails(&idle, &none(), &none()), Side::Left))
                .contains(&"ClaudhubConflicts")
        );
        let conflicting = vec![seat(
            "ClaudhubConflicts",
            at(Side::Left, Half::Bottom),
            true,
            true,
        )];
        assert!(
            all_names(rail(&rails(&conflicting, &none(), &none()), Side::Left))
                .contains(&"ClaudhubConflicts")
        );
    }

    /// Folded **by hand** is the exception: that is the one invisibility one
    /// calls the view back from, so its button stays.
    #[test]
    fn a_situational_view_folded_by_hand_keeps_its_button() {
        let folded = BTreeSet::from(["ClaudhubTests".to_string()]);
        let seats = vec![away("ClaudhubTests", at(Side::Left, Half::Bottom))];
        assert!(
            all_names(rail(&rails(&seats, &folded, &none()), Side::Left))
                .contains(&"ClaudhubTests")
        );
    }

    /// **Off is off.** A view taken off its rail has no button at all — that is
    /// the whole of what taking it off means, and the difference with folding,
    /// which the two used to share one set for: the press that put a view away
    /// marked it exactly as "I do not use this" did, so the button one had just
    /// pressed was the button that then had to go.
    #[test]
    fn a_view_taken_off_its_rail_has_no_button() {
        let seats = vec![seat(
            "ClaudhubNotes",
            at(Side::Left, Half::Bottom),
            true,
            true,
        )];
        let off = BTreeSet::from(["ClaudhubNotes".to_string()]);
        assert!(
            !all_names(rail(&rails(&seats, &none(), &off), Side::Left)).contains(&"ClaudhubNotes")
        );
        // Folded, it keeps it: that press is how one gets it back.
        let folded = BTreeSet::from(["ClaudhubNotes".to_string()]);
        assert!(
            all_names(rail(&rails(&seats, &folded, &none()), Side::Left))
                .contains(&"ClaudhubNotes")
        );
    }

    /// The menu that takes views off lists what a rail can carry — and a view
    /// already off, which is the one state it is the only way back from.
    #[test]
    fn the_views_menu_leaves_out_what_has_no_button_to_lose() {
        let none = BTreeSet::new();
        let tool = |panel: &str| TOOLS.iter().find(|tool| tool.panel == panel).unwrap();
        assert!(in_view_menu(tool("ClaudhubNotes"), &none));
        // Situational: its button comes and goes with its content.
        assert!(!in_view_menu(tool("ClaudhubTestRun"), &none));
        let off = BTreeSet::from(["ClaudhubTestRun".to_string()]);
        assert!(in_view_menu(tool("ClaudhubTestRun"), &off));
    }

    /// A panel whose tab comes and goes with its content gets no button while
    /// it has no seat: nothing would make it appear, so the button would do
    /// nothing at all.
    #[test]
    fn a_conditional_panel_with_no_seat_has_no_button() {
        let idle = rails(&[], &none(), &none());
        assert!(!all_names(rail(&idle, Side::Bottom)).contains(&"ClaudhubTestRun"));
        let running = vec![seat(
            "ClaudhubTestRun",
            at(Side::Bottom, Half::Top),
            true,
            true,
        )];
        let running = rails(&running, &none(), &none());
        assert!(all_names(rail(&running, Side::Bottom)).contains(&"ClaudhubTestRun"));
    }

    /// **The buttons follow the tabs**, which is what lets one put the files
    /// before the changes without editing the table. A tab order does not move
    /// on its own — it moves when one moves it.
    #[test]
    fn the_buttons_follow_the_tabs() {
        let here = at(Side::Left, Half::Top);
        let ranked = |panel, order| Seat {
            order,
            ..seat(panel, here, false, true)
        };
        let seats = vec![ranked("ClaudhubChanges", 1), ranked("ClaudhubFiles", 0)];
        let rails = rails(&seats, &none(), &none());
        let shown = names(&rail(&rails, Side::Left).top);
        assert_eq!(&shown[..2], ["ClaudhubFiles", "ClaudhubChanges"]);
    }

    /// What the tree does not hold goes **last**, whatever the table says: a
    /// view called back has to land somewhere, and the end displaces nothing.
    #[test]
    fn what_has_no_seat_goes_last() {
        let here = at(Side::Left, Half::Top);
        // Only the search is seated, and last in its group.
        let seats = vec![Seat {
            order: 7,
            ..seat("ClaudhubSearch", here, true, true)
        }];
        let rails = rails(&seats, &none(), &none());
        let shown = names(&rail(&rails, Side::Left).top);
        assert_eq!(shown.first(), Some(&"ClaudhubSearch"));
    }

    /// A move offers every place but the one it is already in: a menu entry
    /// that does nothing is a menu entry one learns to distrust.
    #[test]
    fn a_move_leaves_out_the_place_one_is_in() {
        let seats = vec![seat(
            "ClaudhubNotes",
            at(Side::Right, Half::Top),
            true,
            true,
        )];
        let targets = moves("ClaudhubNotes", &seats);
        assert_eq!(targets.len(), Anchor::all().len() - 1);
        assert!(!targets.contains(&Anchor::new(Side::Right, Half::Top)));
        assert!(targets.contains(&Anchor::new(Side::Right, Half::Bottom)));
        // Out of the tree, there is no place to leave out.
        assert_eq!(moves("ClaudhubNotes", &[]).len(), Anchor::all().len());
    }

    /// No edge carries more than a rail can be aimed at. A tool window too
    /// many is a decision to take, not an icon to crop.
    /// Ours, at least: a plugin's panels join a rail at run time, and what
    /// this holds is the table one can decide about at compile time.
    #[test]
    fn no_rail_carries_more_than_it_can_show() {
        let rails = rails(&[], &none(), &none());
        for side in Side::ALL {
            let held = rail(&rails, side).buttons().count();
            assert!(held <= MAX_PER_RAIL, "{side:?} carries {held} buttons");
        }
    }

    /// **No two buttons under one glyph.** A rail is aimed at by its icons,
    /// and two views sharing one is a target that answers half the time. It is
    /// what the branch list cost: the range comparing a branch to its base was
    /// holding `git-branch`, and gave it up for `git-pull-request`.
    #[test]
    fn no_two_tools_share_an_icon() {
        let mut seen = std::collections::BTreeMap::new();
        for tool in TOOLS {
            if let Some(other) = seen.insert(tool.icon, tool.panel) {
                panic!("{} and {} both draw {}", other, tool.panel, tool.icon);
            }
        }
    }

    /// Zen folds the three and gives back exactly what was unfolded — not the
    /// three, which would open zones one had put away oneself.
    #[test]
    fn zen_gives_back_what_it_took() {
        let open = [true, false, true];
        let (folded, taken) = zen(open, &[]);
        assert_eq!(folded, [false, false, false]);
        assert_eq!(taken, vec![Side::Left, Side::Bottom]);
        let (back, taken) = zen(folded, &taken);
        assert_eq!(back, open);
        assert!(taken.is_empty());
    }

    /// Nothing unfolded, and the gesture gives back the last fold: one key,
    /// and a zen one cannot leave would be a trap.
    #[test]
    fn zen_on_an_already_bare_window_gives_the_fold_back() {
        let (folded, taken) = zen([false, false, false], &[]);
        assert_eq!(folded, [false, false, false]);
        assert!(taken.is_empty());
    }

    /// Every tool window's title, and every place a move offers, is a key of
    /// both catalogues — the check `Workspace::views` carried over nine copied
    /// lists.
    #[test]
    fn every_tool_has_a_title_in_both_catalogues() {
        let (en, fr) = (
            include_str!("../../assets/i18n/en.json"),
            include_str!("../../assets/i18n/fr.json"),
        );
        let named = TOOLS
            .iter()
            .map(|tool| tool.title.0)
            .chain(Anchor::all().into_iter().map(Anchor::label));
        for title in named {
            let key = format!("\"{title}\":");
            assert!(en.contains(&key), "missing from en.json: {title}");
            assert!(fr.contains(&key), "missing from fr.json: {title}");
        }
    }
}
