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

impl Side {
    /// The three, in the order the rails are laid around the centre.
    pub const ALL: [Side; 3] = [Side::Left, Side::Right, Side::Bottom];

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
    /// back. **Not** a reading of where it is: that is `side_of`.
    pub home: Side,
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
        home: Side::Left,
        conditional: false,
    },
    Tool {
        panel: "ClaudhubBranch",
        title: "range-branch",
        icon: "git-branch",
        home: Side::Left,
        conditional: false,
    },
    Tool {
        panel: "ClaudhubFiles",
        title: "panel-files",
        icon: "file-code",
        home: Side::Left,
        conditional: false,
    },
    Tool {
        panel: "ClaudhubSearch",
        title: "panel-search",
        icon: "search",
        home: Side::Left,
        conditional: false,
    },
    Tool {
        panel: "ClaudhubDb",
        title: "panel-databases",
        icon: "database",
        home: Side::Left,
        conditional: false,
    },
    Tool {
        panel: "ClaudhubTests",
        title: "panel-tests",
        icon: "circle-check",
        home: Side::Left,
        // Only where a runner exists: on everything else the honest panel is
        // no panel — there is nothing to run.
        conditional: true,
    },
    Tool {
        panel: "ClaudhubTerminal",
        title: "panel-terminal",
        icon: "square-terminal",
        home: Side::Bottom,
        conditional: false,
    },
    Tool {
        panel: "ClaudhubNotes",
        title: "panel-notes",
        icon: "sticky-note",
        home: Side::Right,
        conditional: false,
    },
    Tool {
        panel: "ClaudhubHistory",
        title: "panel-history",
        icon: "history",
        home: Side::Right,
        conditional: false,
    },
    // Past the ninth, no key names it: `Alt+1`… stop here, and the rest is
    // reached by its button.
    Tool {
        panel: "ClaudhubTags",
        title: "panel-tags",
        icon: "tag",
        home: Side::Right,
        conditional: false,
    },
    Tool {
        panel: "ClaudhubStashes",
        title: "panel-stashes",
        icon: "archive",
        home: Side::Right,
        conditional: false,
    },
    Tool {
        panel: "ClaudhubSqlHistory",
        title: "panel-sql-history",
        icon: "list",
        home: Side::Right,
        conditional: false,
    },
    Tool {
        panel: "ClaudhubTestRun",
        title: "panel-test-run",
        icon: "play",
        home: Side::Bottom,
        conditional: true,
    },
    // Nothing to resolve, no tab: it shifts the others aside to serve one time
    // in a hundred.
    Tool {
        panel: "ClaudhubConflicts",
        title: "panel-conflicts",
        icon: "triangle-alert",
        home: Side::Left,
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
    pub side: Option<Side>,
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
    /// Its zone is unfolded **and** it is the tab being displayed.
    pub active: bool,
    /// Hidden from the "Views" menu: the button stays, muted. It is where one
    /// calls the view back from, and a button that vanished would be a target
    /// that moves.
    pub dimmed: bool,
}

/// One edge's rail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rail {
    pub side: Side,
    pub buttons: Vec<Button>,
}

/// What pressing a button means. Decided here, carried out by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    /// The zone is folded, or another tab is in front: unfold, bring forward.
    Reveal { panel: &'static str, side: Side },
    /// It is already the displayed tab of an unfolded zone: fold the zone.
    Collapse { side: Side },
    /// The panel is not in the tree — hidden, or dragged out of everything:
    /// put it back where the default puts it.
    Restore { panel: &'static str, side: Side },
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
pub fn side_of(panel: &str, seats: &[Seat]) -> Option<Side> {
    seat(panel, seats).and_then(|seat| seat.side)
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
        buttons: Vec::new(),
    });
    for tool in TOOLS {
        let seat = seat(tool.panel, seats);
        // At the centre it is a document: reached by its tab, among the ones
        // it shares its group with.
        if seat.is_some_and(|seat| seat.side.is_none()) {
            continue;
        }
        let side = match seat {
            Some(seat) => seat.side.expect("just tested"),
            // Out of the tree. A button that puts it back, unless nothing
            // could make it come back.
            None if tool.conditional => continue,
            None => tool.home,
        };
        rails[side.index()].buttons.push(Button {
            panel: tool.panel,
            title: tool.title,
            icon: tool.icon,
            active: seat.is_some_and(|seat| seat.open && seat.shown),
            dimmed: hidden.contains(tool.panel),
        });
    }
    rails
}

/// What pressing a button does.
pub fn press(panel: &str, seats: &[Seat]) -> Press {
    let Some(tool) = tool(panel) else {
        // Not one of ours: nothing sensible to do, and the caller has no
        // button to have pressed.
        return Press::Restore {
            panel: "",
            side: Side::Left,
        };
    };
    match seat(panel, seats) {
        Some(seat) => match seat.side {
            // Folding is asked of a zone and not of a panel: what the button
            // says is "put this away", and what is in the way is the zone.
            Some(side) if seat.open && seat.shown => Press::Collapse { side },
            Some(side) => Press::Reveal {
                panel: tool.panel,
                side,
            },
            // A document has no button; pressing one anyway brings it forward.
            None => Press::Reveal {
                panel: tool.panel,
                side: tool.home,
            },
        },
        None => Press::Restore {
            panel: tool.panel,
            side: tool.home,
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

    fn seat(panel: &str, side: Option<Side>, shown: bool, open: bool) -> Seat {
        Seat {
            panel: panel.to_string(),
            side,
            shown,
            open,
        }
    }

    fn none() -> BTreeSet<String> {
        BTreeSet::new()
    }

    fn rail(rails: &[Rail; 3], side: Side) -> &Rail {
        &rails[side.index()]
    }

    fn names(rail: &Rail) -> Vec<&str> {
        rail.buttons.iter().map(|button| button.panel).collect()
    }

    /// A panel of the centre is a document: it is reached by its tab, and no
    /// rail claims it.
    #[test]
    fn a_panel_of_the_centre_has_no_button() {
        let seats = vec![seat("ClaudhubNotes", None, true, true)];
        let rails = rails(&seats, &none());
        assert!(Side::ALL
            .iter()
            .all(|side| !names(rail(&rails, *side)).contains(&"ClaudhubNotes")));
    }

    /// Dragged from one edge to the other, a panel appears on the other rail
    /// and nowhere else — with nothing kept up to date for it to do so. This
    /// is the whole reason the seats are read from the tree.
    #[test]
    fn a_panel_dragged_across_changes_rail() {
        let seats = vec![seat("ClaudhubNotes", Some(Side::Left), false, true)];
        let rails = rails(&seats, &none());
        assert!(names(rail(&rails, Side::Left)).contains(&"ClaudhubNotes"));
        assert!(!names(rail(&rails, Side::Right)).contains(&"ClaudhubNotes"));
    }

    /// Active is "unfolded **and** in front". The displayed tab of a folded
    /// zone is not on screen, and a button lit for it would be lying.
    #[test]
    fn only_a_shown_tab_of_an_unfolded_zone_is_active() {
        let shown = vec![seat("ClaudhubNotes", Some(Side::Right), true, true)];
        let folded = vec![seat("ClaudhubNotes", Some(Side::Right), true, false)];
        let behind = vec![seat("ClaudhubNotes", Some(Side::Right), false, true)];
        let lit = |seats: &[Seat]| rails(seats, &none())[Side::Right.index()].buttons[0].active;
        assert!(lit(&shown));
        assert!(!lit(&folded));
        assert!(!lit(&behind));
    }

    /// Pressing what is already in front puts the zone away; pressing again
    /// brings it back.
    #[test]
    fn pressing_what_is_in_front_folds_the_zone() {
        let open = vec![seat("ClaudhubNotes", Some(Side::Right), true, true)];
        assert_eq!(
            press("ClaudhubNotes", &open),
            Press::Collapse { side: Side::Right }
        );
        let folded = vec![seat("ClaudhubNotes", Some(Side::Right), true, false)];
        assert_eq!(
            press("ClaudhubNotes", &folded),
            Press::Reveal {
                panel: "ClaudhubNotes",
                side: Side::Right,
            }
        );
    }

    /// Pressing a tab that is **not** in front selects it. Folding there would
    /// hide the zone one has just asked to look into.
    #[test]
    fn pressing_a_tab_behind_selects_it() {
        let seats = vec![
            seat("ClaudhubNotes", Some(Side::Right), true, true),
            seat("ClaudhubHistory", Some(Side::Right), false, true),
        ];
        assert_eq!(
            press("ClaudhubHistory", &seats),
            Press::Reveal {
                panel: "ClaudhubHistory",
                side: Side::Right,
            }
        );
    }

    /// Hidden from the menu, a panel keeps its button — muted — and pressing
    /// it puts the panel back where the default layout has it. The button is
    /// where one calls a hidden view back from.
    #[test]
    fn a_hidden_panel_keeps_a_muted_button_that_restores_it() {
        let hidden = BTreeSet::from(["ClaudhubNotes".to_string()]);
        let rails = rails(&[], &hidden);
        let button = &rail(&rails, Side::Right)
            .buttons
            .iter()
            .find(|button| button.panel == "ClaudhubNotes")
            .expect("the button stays");
        assert!(button.dimmed);
        assert!(!button.active);
        assert_eq!(
            press("ClaudhubNotes", &[]),
            Press::Restore {
                panel: "ClaudhubNotes",
                side: Side::Right,
            }
        );
    }

    /// A panel whose tab comes and goes with its content gets no button while
    /// it has no seat: nothing would make it appear, so the button would do
    /// nothing at all.
    #[test]
    fn a_conditional_panel_with_no_seat_has_no_button() {
        let idle = rails(&[], &none());
        assert!(!names(rail(&idle, Side::Bottom)).contains(&"ClaudhubTestRun"));
        let running = vec![seat("ClaudhubTestRun", Some(Side::Bottom), true, true)];
        let running = rails(&running, &none());
        assert!(names(rail(&running, Side::Bottom)).contains(&"ClaudhubTestRun"));
    }

    /// The buttons follow the table and never the tabs: a rail that reordered
    /// itself under the hand could not be aimed at twice.
    #[test]
    fn the_buttons_follow_the_table_and_not_the_tabs() {
        let seats = vec![
            seat("ClaudhubHistory", Some(Side::Right), true, true),
            seat("ClaudhubNotes", Some(Side::Right), false, true),
        ];
        assert_eq!(
            names(rail(&rails(&seats, &none()), Side::Right)),
            vec![
                "ClaudhubNotes",
                "ClaudhubHistory",
                "ClaudhubTags",
                "ClaudhubStashes",
                "ClaudhubSqlHistory",
            ]
        );
    }

    /// No edge carries more than a rail can be aimed at. A tool window too
    /// many is a decision to take, not an icon to crop.
    #[test]
    fn no_rail_carries_more_than_it_can_show() {
        let rails = rails(&[], &none());
        for side in Side::ALL {
            assert!(
                rail(&rails, side).buttons.len() <= MAX_PER_RAIL,
                "{side:?} carries {} buttons",
                rail(&rails, side).buttons.len()
            );
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

    /// Every tool window's title is a key of both catalogues — the check
    /// `Workspace::views` carried over nine copied lists.
    #[test]
    fn every_tool_has_a_title_in_both_catalogues() {
        let (en, fr) = (
            include_str!("../../assets/i18n/en.json"),
            include_str!("../../assets/i18n/fr.json"),
        );
        for tool in TOOLS {
            let key = format!("\"{}\":", tool.title);
            assert!(en.contains(&key), "missing from en.json: {}", tool.title);
            assert!(fr.contains(&key), "missing from fr.json: {}", tool.title);
        }
    }
}
