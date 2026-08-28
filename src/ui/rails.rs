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
            Side::Left => px(280.),
            Side::Right => px(320.),
            Side::Bottom => px(260.),
        }
    }
}

/// A tool window, as the rails know it.
pub struct Tool {
    /// The name the dock's registry builds it by, and the one a saved layout
    /// holds.
    pub panel: &'static str,
    /// The i18n key of its title — the same one its tab carries.
    pub title: &'static str,
    /// A Lucide name from `assets/icons`.
    pub icon: &'static str,
    /// Where the default layout puts it, and where `Press::Restore` puts it
    /// back. **Not** a reading of where it is: that is `anchor_of`.
    pub home: Anchor,
    /// Its tab comes and goes with its content — the `needed:` clause of the
    /// `panels!` macro. With no seat, no button: one offered for a panel that
    /// nothing can make appear is a button that does nothing.
    pub conditional: bool,
}

/// The tool windows, in the order their buttons sit.
///
/// **A tool window only ever joins at the end**, and it is not an aesthetic
/// choice: the ranks of this table are what `Alt+1`… name, and one inserted in
/// the middle would move every key after it. It is the rule `Workspace::ALL`
/// carried, and it survives it.
///
/// The order says what the window is for: the left chooses, the right
/// remembers, the bottom runs. Read down, it is the working day — what changed,
/// on which branch, in which file, found where, in which table, does it pass;
/// then what one had to say about it, what happened, what is put aside; then
/// what is running.
pub const TOOLS: &[Tool] = &[
    Tool {
        panel: "ClaudhubChanges",
        title: "range-working",
        icon: "file-diff",
        home: Anchor::new(Side::Left, Half::Top),
        conditional: false,
    },
    Tool {
        panel: "ClaudhubBranch",
        title: "range-branch",
        icon: "git-branch",
        home: Anchor::new(Side::Left, Half::Top),
        conditional: false,
    },
    Tool {
        panel: "ClaudhubFiles",
        title: "panel-files",
        icon: "file-code",
        home: Anchor::new(Side::Left, Half::Top),
        conditional: false,
    },
    Tool {
        panel: "ClaudhubSearch",
        title: "panel-search",
        icon: "search",
        home: Anchor::new(Side::Left, Half::Top),
        conditional: false,
    },
    Tool {
        panel: "ClaudhubDb",
        title: "panel-databases",
        icon: "database",
        home: Anchor::new(Side::Left, Half::Top),
        conditional: false,
    },
    Tool {
        panel: "ClaudhubTests",
        title: "panel-tests",
        icon: "circle-check",
        home: Anchor::new(Side::Left, Half::Bottom),
        // Only where a runner exists: on everything else the honest panel is
        // no panel — there is nothing to run.
        conditional: true,
    },
    Tool {
        panel: "ClaudhubTerminal",
        title: "panel-terminal",
        icon: "square-terminal",
        home: Anchor::new(Side::Bottom, Half::Top),
        conditional: false,
    },
    Tool {
        panel: "ClaudhubNotes",
        title: "panel-notes",
        icon: "pencil",
        home: Anchor::new(Side::Right, Half::Top),
        conditional: false,
    },
    Tool {
        panel: "ClaudhubHistory",
        title: "panel-history",
        icon: "git-commit-horizontal",
        home: Anchor::new(Side::Right, Half::Top),
        conditional: false,
    },
    // Past the ninth, no key names it: `Alt+1`… stop here, and the rest is
    // reached by its button.
    Tool {
        panel: "ClaudhubTags",
        title: "panel-tags",
        icon: "tags",
        home: Anchor::new(Side::Right, Half::Bottom),
        conditional: false,
    },
    Tool {
        panel: "ClaudhubStashes",
        title: "panel-stashes",
        icon: "archive",
        home: Anchor::new(Side::Right, Half::Bottom),
        conditional: false,
    },
    Tool {
        panel: "ClaudhubSqlHistory",
        title: "panel-sql-history",
        icon: "list",
        home: Anchor::new(Side::Right, Half::Bottom),
        conditional: false,
    },
    Tool {
        panel: "ClaudhubTestRun",
        title: "panel-test-run",
        icon: "play",
        home: Anchor::new(Side::Bottom, Half::Top),
        conditional: true,
    },
    // Nothing to resolve, no tab: it shifts the others aside to serve one time
    // in a hundred.
    Tool {
        panel: "ClaudhubConflicts",
        title: "panel-conflicts",
        icon: "git-merge",
        home: Anchor::new(Side::Left, Half::Bottom),
        conditional: true,
    },
];

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
}

/// One button of a rail, as the view has to paint it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub panel: &'static str,
    pub title: &'static str,
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
    /// It is already on screen: put it away.
    ///
    /// **The view and not the zone.** A zone holds two halves and each holds
    /// several views; folding the lot to put one away would take the other
    /// half's work off the screen with it. A half left with nothing visible
    /// stops being drawn on its own, and the other takes the room — which is
    /// what makes the two halves fold independently without a second notion of
    /// folding beside the one the dock already has.
    Hide { panel: &'static str },
    /// The panel is not in the tree — hidden, or dragged out of everything:
    /// put it back where the default puts it.
    Restore { panel: &'static str, anchor: Anchor },
}

/// The tool this name declares, if it is one.
pub fn tool(panel: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|tool| tool.panel == panel)
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

/// The three rails, from what the dock holds and what has been hidden.
///
/// A tool window with no seat still gets a button — that is how one calls back
/// a view hidden from the menu — **unless** its tab comes and goes with its
/// content: nothing would make a conditional panel appear, so its button would
/// do nothing. The order is the table's and never the tabs': a rail that
/// reordered itself under the hand could not be aimed at twice.
pub fn rails(seats: &[Seat], hidden: &std::collections::BTreeSet<String>) -> [Rail; 3] {
    let mut rails = Side::ALL.map(|side| Rail {
        side,
        top: Vec::new(),
        bottom: Vec::new(),
    });
    for tool in TOOLS {
        let seat = seat(tool.panel, seats);
        // At the centre it is a document: reached by its tab, among the ones
        // it shares its group with.
        if seat.is_some_and(|seat| seat.anchor.is_none()) {
            continue;
        }
        let anchor = match seat {
            Some(seat) => seat.anchor.expect("just tested"),
            // Out of the tree. A button that puts it back, unless nothing
            // could make it come back.
            None if tool.conditional => continue,
            None => tool.home,
        };
        let button = Button {
            panel: tool.panel,
            title: tool.title,
            icon: tool.icon,
            active: seat.is_some_and(|seat| seat.open && seat.shown)
                && !hidden.contains(tool.panel),
        };
        let rail = &mut rails[anchor.side.index()];
        match anchor.half {
            Half::Top => rail.top.push(button),
            Half::Bottom => rail.bottom.push(button),
        }
    }
    // The rail does not scroll, so what it cannot show it hides. Said here as
    // well as in the test: a plugin's panels join a rail at run time, where a
    // table read at compile time cannot see them.
    debug_assert!(
        rails
            .iter()
            .all(|rail| rail.buttons().count() <= MAX_PER_RAIL),
        "a rail carries more buttons than it can show"
    );
    rails
}

/// What pressing a button does.
pub fn press(panel: &str, seats: &[Seat], hidden: &std::collections::BTreeSet<String>) -> Press {
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
            // On screen: the press puts it away. Not the zone — see `Hide`.
            Some(_) if seat.open && seat.shown && !hidden.contains(panel) => {
                Press::Hide { panel: tool.panel }
            }
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
        let rails = rails(&seats, &none());
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
        let rails = rails(&seats, &none());
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
        assert!(names(&rail(&rails(&up, &none()), Side::Right).top).contains(&"ClaudhubNotes"));
        assert!(names(&rail(&rails(&down, &none()), Side::Right).bottom).contains(&"ClaudhubNotes"));
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
            rails(&seats, &none())[Side::Right.index()].top[0].active
        };
        assert!(lit(true, true));
        assert!(!lit(true, false));
        assert!(!lit(false, true));
    }

    /// Pressing what is on screen puts **the view** away, not its zone: the
    /// other half of the edge is somebody else's work, and it stays.
    #[test]
    fn pressing_what_is_on_screen_puts_the_view_away() {
        let open = vec![seat(
            "ClaudhubNotes",
            at(Side::Right, Half::Top),
            true,
            true,
        )];
        assert_eq!(
            press("ClaudhubNotes", &open, &none()),
            Press::Hide {
                panel: "ClaudhubNotes"
            }
        );
        // And pressing it again brings it back — from a view put away as from
        // a folded zone.
        let away = BTreeSet::from(["ClaudhubNotes".to_string()]);
        let folded = vec![seat(
            "ClaudhubNotes",
            at(Side::Right, Half::Top),
            true,
            false,
        )];
        for (seats, hidden) in [(&open, &away), (&folded, &none())] {
            assert_eq!(
                press("ClaudhubNotes", seats, hidden),
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
            press("ClaudhubHistory", &seats, &none()),
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
        let seats = vec![seat(
            "ClaudhubNotes",
            at(Side::Right, Half::Top),
            true,
            true,
        )];
        let button = rails(&seats, &hidden)[Side::Right.index()]
            .buttons()
            .find(|button| button.panel == "ClaudhubNotes")
            .cloned()
            .expect("the button stays");
        assert!(!button.active, "put away is not on screen");
        // Out of the tree entirely, the press puts it back where it belongs.
        assert_eq!(
            press("ClaudhubNotes", &[], &none()),
            Press::Restore {
                panel: "ClaudhubNotes",
                anchor: Anchor::new(Side::Right, Half::Top),
            }
        );
    }

    /// A panel whose tab comes and goes with its content gets no button while
    /// it has no seat: nothing would make it appear, so the button would do
    /// nothing at all.
    #[test]
    fn a_conditional_panel_with_no_seat_has_no_button() {
        let idle = rails(&[], &none());
        assert!(!all_names(rail(&idle, Side::Bottom)).contains(&"ClaudhubTestRun"));
        let running = vec![seat(
            "ClaudhubTestRun",
            at(Side::Bottom, Half::Top),
            true,
            true,
        )];
        let running = rails(&running, &none());
        assert!(all_names(rail(&running, Side::Bottom)).contains(&"ClaudhubTestRun"));
    }

    /// The buttons follow the table and never the tabs: a rail that reordered
    /// itself under the hand could not be aimed at twice.
    #[test]
    fn the_buttons_follow_the_table_and_not_the_tabs() {
        let seats = vec![
            seat("ClaudhubHistory", at(Side::Right, Half::Top), true, true),
            seat("ClaudhubNotes", at(Side::Right, Half::Top), false, true),
        ];
        assert_eq!(
            names(&rail(&rails(&seats, &none()), Side::Right).top),
            vec!["ClaudhubNotes", "ClaudhubHistory"]
        );
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
    #[test]
    fn no_rail_carries_more_than_it_can_show() {
        let rails = rails(&[], &none());
        for side in Side::ALL {
            let held = rail(&rails, side).buttons().count();
            assert!(held <= MAX_PER_RAIL, "{side:?} carries {held} buttons");
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
            .map(|tool| tool.title)
            .chain(Anchor::all().into_iter().map(Anchor::label));
        for title in named {
            let key = format!("\"{title}\":");
            assert!(en.contains(&key), "missing from en.json: {title}");
            assert!(fr.contains(&key), "missing from fr.json: {title}");
        }
    }
}
