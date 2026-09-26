//! The focus view's board: which card stands in which column, and where a
//! card dropped lands.
//!
//! A worktree's cards are laid out in columns the hand arranges — a card is
//! dragged by its head to another column, or to the right of the last one,
//! which makes a new column. Nothing is placed by a formula once the hand has
//! spoken: the board is what it was left as, and only what is new — a
//! terminal just opened, a note just written — is placed by rule.
//!
//! Pure: the view hands in what is on show and what it measured, this says
//! where things go.

use std::path::{Path, PathBuf};

use crate::ui::overview::Node;

/// A place on the board, as it is kept across a restart.
///
/// A terminal has no identity that outlives the process — its id is its
/// view's — so what is kept of it is that a terminal stood here: the
/// terminals that come back take those places in reading order.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Key {
    /// The worktree's own card.
    Card,
    Note(PathBuf),
    Terminal,
}

/// A place on the board in memory: a card, or the place a terminal of a
/// past session held until one comes back to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    Card(Node),
    /// Held for a terminal to come back; painted as nothing.
    Vacant,
}

impl Slot {
    fn node(&self) -> Option<&Node> {
        match self {
            Slot::Card(node) => Some(node),
            Slot::Vacant => None,
        }
    }
}

/// Where a card goes: a column, and its rank in it. A column one past the
/// last is a new column at the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub column: usize,
    pub index: usize,
}

/// Where what was asked for from a column's own button lands: the column
/// whose « Terminal » or « Note » was pressed. The one wish said out loud.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    pub terminal: Option<usize>,
    pub note: Option<usize>,
}

impl Pending {
    #[cfg(test)]
    fn terminal(column: usize) -> Pending {
        Pending {
            terminal: Some(column),
            note: None,
        }
    }
}

/// A worktree's board.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Board {
    pub columns: Vec<Vec<Slot>>,
    /// Each column's width, when the hand gave it one; `None` fills — the
    /// columns without one share what the others leave. Kept in step with
    /// `columns`: a column that goes takes its width with it.
    widths: Vec<Option<f32>>,
}

impl Board {
    /// A board read back from the store, for the worktree at `path`.
    pub fn from_keys(path: &std::path::Path, keys: &[Vec<Key>], widths: &[Option<f32>]) -> Board {
        let mut board = Board {
            columns: Vec::new(),
            widths: Vec::new(),
        };
        let columns: Vec<Vec<Slot>> = keys
            .iter()
            .map(|column| {
                column
                    .iter()
                    .map(|key| match key {
                        Key::Card => Slot::Card(Node::Worktree(path.to_path_buf())),
                        Key::Note(note) => Slot::Card(Node::Note(note.clone())),
                        Key::Terminal => Slot::Vacant,
                    })
                    .collect()
            })
            .collect();
        for (index, column) in columns.into_iter().enumerate() {
            if !column.is_empty() {
                board.columns.push(column);
                board.widths.push(widths.get(index).copied().flatten());
            }
        }
        board
    }

    /// The width the hand gave a column, if it gave one.
    pub fn width(&self, column: usize) -> Option<f32> {
        self.widths.get(column).copied().flatten()
    }

    /// Gives a column a width, or — `None` — lets it fill again.
    pub fn set_width(&mut self, column: usize, width: Option<f32>) {
        if let Some(slot) = self.widths.get_mut(column) {
            *slot = width;
        }
    }

    /// What the store keeps of the widths, column for column with `keys`.
    pub fn widths(&self) -> Vec<Option<f32>> {
        self.columns
            .iter()
            .zip(&self.widths)
            .filter(|(column, _)| {
                column
                    .iter()
                    .any(|slot| !matches!(slot, Slot::Card(Node::Git(_) | Node::Changes(_))))
            })
            .map(|(_, width)| *width)
            .collect()
    }

    /// A new column at the right, which fills until the hand says otherwise.
    fn push_column(&mut self, column: Vec<Slot>) {
        self.columns.push(column);
        self.widths.push(None);
    }

    /// Drops the empty columns, and their widths with them.
    fn prune(&mut self) {
        let widths = std::mem::take(&mut self.widths);
        let columns = std::mem::take(&mut self.columns);
        for (column, width) in columns.into_iter().zip(widths) {
            if !column.is_empty() {
                self.columns.push(column);
                self.widths.push(width);
            }
        }
    }

    /// What the store keeps of it.
    pub fn keys(&self) -> Vec<Vec<Key>> {
        self.columns
            .iter()
            .map(|column| {
                column
                    .iter()
                    .filter_map(|slot| match slot {
                        Slot::Card(Node::Worktree(_)) => Some(Key::Card),
                        Slot::Card(Node::Note(path)) => Some(Key::Note(path.clone())),
                        Slot::Card(Node::Terminal(_)) | Slot::Vacant => Some(Key::Terminal),
                        Slot::Card(Node::Git(_) | Node::Changes(_)) => None,
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|column| !column.is_empty())
            .collect()
    }

    fn position(&self, node: &Node) -> Option<(usize, usize)> {
        self.columns.iter().enumerate().find_map(|(c, column)| {
            column
                .iter()
                .position(|slot| slot.node() == Some(node))
                .map(|i| (c, i))
        })
    }

    /// Brings the board in line with what is on show: a card gone leaves,
    /// a card new comes in by rule. Returns whether anything moved.
    ///
    /// The rule, for what the hand has not placed: the worktree's card at
    /// the top of the first column; a note where `pending` says — the column
    /// whose « Note » was pressed — else under the card, in the first column;
    /// a terminal where `pending` says, else in the place a terminal of the
    /// last session held, else in a column of its own at the right. A
    /// terminal is what one reads beside the rest, not under it.
    pub fn settle(&mut self, present: &[Node], pending: Pending) -> bool {
        let before = self.clone();
        for column in &mut self.columns {
            column.retain(|slot| match slot {
                Slot::Card(node) => present.contains(node),
                Slot::Vacant => true,
            });
        }
        for node in present {
            if self.position(node).is_some() {
                continue;
            }
            match node {
                Node::Worktree(_) => {
                    if self.columns.is_empty() {
                        self.push_column(Vec::new());
                    }
                    self.columns[0].insert(0, Slot::Card(node.clone()));
                }
                Node::Terminal(_) => {
                    let vacant = self.columns.iter().enumerate().find_map(|(c, column)| {
                        column
                            .iter()
                            .position(|slot| *slot == Slot::Vacant)
                            .map(|i| (c, i))
                    });
                    match (pending.terminal, vacant) {
                        (Some(c), _) if c < self.columns.len() => {
                            self.columns[c].push(Slot::Card(node.clone()))
                        }
                        (_, Some((c, i))) => self.columns[c][i] = Slot::Card(node.clone()),
                        _ => self.push_column(vec![Slot::Card(node.clone())]),
                    }
                }
                Node::Note(_) | Node::Git(_) | Node::Changes(_) => {
                    if self.columns.is_empty() {
                        self.push_column(Vec::new());
                    }
                    let column = pending
                        .note
                        .filter(|column| {
                            *column < self.columns.len() && matches!(node, Node::Note(_))
                        })
                        .unwrap_or(0);
                    self.columns[column].push(Slot::Card(node.clone()));
                }
            }
        }
        self.prune();
        *self != before
    }

    /// Lets go of the places held for terminals that did not come back —
    /// once they all have had the chance. Returns whether any was held.
    pub fn drop_vacants(&mut self) -> bool {
        let before = self.columns.iter().flatten().count();
        for column in &mut self.columns {
            column.retain(|slot| *slot != Slot::Vacant);
        }
        self.prune();
        self.columns.iter().flatten().count() != before
    }

    /// The columns as the screen shows them: the empty ones and the places
    /// held for a terminal left out, each card with its rank on the board.
    pub fn shown(&self) -> Vec<(usize, Vec<(usize, Node)>)> {
        self.columns
            .iter()
            .enumerate()
            .map(|(c, column)| {
                let cards: Vec<(usize, Node)> = column
                    .iter()
                    .enumerate()
                    .filter_map(|(i, slot)| slot.node().map(|node| (i, node.clone())))
                    .collect();
                (c, cards)
            })
            .filter(|(_, cards)| !cards.is_empty())
            .collect()
    }

    /// A target read off the screen — ranks among what is shown — as the
    /// board counts it, the held places included.
    pub fn resolve(&self, on_screen: Target) -> Target {
        let shown = self.shown();
        let Some((column, cards)) = shown.get(on_screen.column) else {
            return Target {
                column: self.columns.len(),
                index: 0,
            };
        };
        let index = cards
            .get(on_screen.index)
            .map_or(self.columns[*column].len(), |(index, _)| *index);
        Target {
            column: *column,
            index,
        }
    }

    /// Moves a card to `target`, counted on the board as it stands with the
    /// card still in it — which is how a drop is read off the screen. A
    /// column left empty goes.
    pub fn move_card(&mut self, node: &Node, target: Target) {
        let Some((from_column, from_index)) = self.position(node) else {
            return;
        };
        let mut target = target;
        // Within its own column, a card taken out shifts what is below it.
        if target.column == from_column && target.index > from_index {
            target.index -= 1;
        }
        let slot = self.columns[from_column].remove(from_index);
        if target.column >= self.columns.len() {
            self.push_column(vec![slot]);
        } else {
            let column = &mut self.columns[target.column];
            let index = target.index.min(column.len());
            column.insert(index, slot);
        }
        self.prune();
    }
}

/// What the view measured of a column at its last frame, in window pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct ColumnSpan {
    pub left: f32,
    pub right: f32,
    /// Each card's top and bottom, in order.
    pub cards: Vec<(f32, f32)>,
}

/// What the view measured at its last frame: its columns, and the room it
/// had opened for a drop — `(column, rank, height)`, the height counting the
/// gap after it — which pushed the cards below it down.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Geometry {
    pub spans: Vec<ColumnSpan>,
    pub opened: Option<(usize, usize, f32)>,
}

impl Geometry {
    /// Whether a point this far across falls on one of the board's columns:
    /// which of the boards side by side a press was on.
    pub fn covers(&self, x: f32) -> bool {
        self.spans
            .iter()
            .any(|span| span.left <= x && x <= span.right)
    }

    /// The columns as they would stand without the room opened for a drop.
    ///
    /// The drop is read against these and not against what was painted: the
    /// room opens where the drop would land, pushes the cards below it, and
    /// read against the pushed cards the next frame would aim elsewhere — the
    /// room would chase the pointer up and down a column.
    pub fn closed(&self) -> Vec<ColumnSpan> {
        let mut spans = self.spans.clone();
        if let Some((column, rank, height)) = self.opened {
            if let Some(span) = spans.get_mut(column) {
                for card in span.cards.iter_mut().skip(rank) {
                    card.0 -= height;
                    card.1 -= height;
                }
            }
        }
        spans
    }
}

/// How far into a card the pointer has to go to be under it: its head, and
/// a little more — never past its middle, for a card smaller than that.
///
/// The middle alone read well for cards of a size, and badly for the tall
/// ones: a terminal that fills its column has its middle half a screen down,
/// and going under it meant dragging down to there. What one aims at when
/// placing a card below another is the other's head, not its middle.
const PAST_HEAD: f32 = 56.;

/// Where a card let go at `pointer` lands: the column under the pointer —
/// the nearest one when it is between two — and in it, after every card
/// whose head the pointer is past (`PAST_HEAD`). Right of the last column is
/// a new column; `None` with nothing measured.
pub fn drop_target(columns: &[ColumnSpan], pointer: (f32, f32)) -> Option<Target> {
    let last = columns.last()?;
    let (x, y) = pointer;
    if x > last.right {
        return Some(Target {
            column: columns.len(),
            index: 0,
        });
    }
    let column = columns
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| distance(a, x).total_cmp(&distance(b, x)))
        .map(|(index, _)| index)?;
    let index = columns[column]
        .cards
        .iter()
        .take_while(|(top, bottom)| top + ((bottom - top) / 2.).min(PAST_HEAD) < y)
        .count();
    Some(Target { column, index })
}

/// How far `x` is from a column, nothing when it is over it.
fn distance(span: &ColumnSpan, x: f32) -> f32 {
    if x < span.left {
        span.left - x
    } else if x > span.right {
        x - span.right
    } else {
        0.
    }
}

/// The worktrees on show, side by side: the ones chosen in the sidebar, as
/// long as the window's own — `primary` — is among them; else that one
/// alone. Choosing a worktree anywhere else in the window is choosing it
/// here, and the others go.
///
/// In the sidebar's order, the boards reading as the list they were picked
/// from; what the pickers no longer show drops out, the primary never.
pub fn shown_worktrees(
    chosen: &[PathBuf],
    primary: Option<&Path>,
    on_show: &[PathBuf],
) -> Vec<PathBuf> {
    let Some(primary) = primary else {
        return Vec::new();
    };
    if !chosen.iter().any(|path| path == primary) {
        return vec![primary.to_path_buf()];
    }
    let mut shown: Vec<PathBuf> = chosen
        .iter()
        .filter(|path| path.as_path() == primary || on_show.contains(path))
        .cloned()
        .collect();
    shown.dedup();
    // Not in the list — another project's — reads first.
    shown.sort_by_key(|path| on_show.iter().position(|shown| shown == path));
    shown
}

/// A Ctrl+click in the sidebar: the worktree joins what is shown, or
/// leaves it — never the last one, a screen with nothing on it being no
/// answer to anything.
pub fn toggled(shown: &[PathBuf], path: &Path) -> Vec<PathBuf> {
    if !shown.iter().any(|shown| shown == path) {
        let mut shown = shown.to_vec();
        shown.push(path.to_path_buf());
        return shown;
    }
    if shown.len() == 1 {
        return shown.to_vec();
    }
    shown
        .iter()
        .filter(|shown| shown.as_path() != path)
        .cloned()
        .collect()
}

/// What stands for a worktree on the sidebar's rail: two letters of its
/// name — the first of two words, or the start of one.
pub fn initials(label: &str) -> String {
    let name = label.rsplit('/').next().unwrap_or(label);
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let letters: String = match words.as_slice() {
        [] => String::new(),
        [one] => one.chars().take(2).collect(),
        [first, second, ..] => first
            .chars()
            .take(1)
            .chain(second.chars().take(1))
            .collect(),
    };
    letters.to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> Node {
        Node::Worktree(PathBuf::from("/r"))
    }

    fn note(name: &str) -> Node {
        Node::Note(PathBuf::from(format!("/r/.claudhub/notes/{name}.md")))
    }

    fn nodes(board: &Board) -> Vec<Vec<Option<Node>>> {
        board
            .columns
            .iter()
            .map(|column| column.iter().map(|slot| slot.node().cloned()).collect())
            .collect()
    }

    /// Nothing placed by hand: the card and the notes in the first column,
    /// each terminal in a column of its own.
    #[test]
    fn a_fresh_board_puts_terminals_beside_the_card() {
        let mut board = Board::default();
        let present = [card(), note("a"), Node::Terminal(7), Node::Terminal(8)];
        assert!(board.settle(&present, Pending::default()));
        assert_eq!(
            nodes(&board),
            vec![
                vec![Some(card()), Some(note("a"))],
                vec![Some(Node::Terminal(7))],
                vec![Some(Node::Terminal(8))],
            ]
        );
        assert!(!board.settle(&present, Pending::default()));
    }

    /// A terminal opened from a column's own button stacks in it; one that
    /// closes leaves, and its column with it when it was alone.
    #[test]
    fn a_terminal_goes_where_it_was_asked_and_leaves_its_column() {
        let mut board = Board::default();
        board.settle(&[card(), Node::Terminal(7)], Pending::default());
        board.settle(
            &[card(), Node::Terminal(7), Node::Terminal(9)],
            Pending::terminal(1),
        );
        assert_eq!(
            nodes(&board),
            vec![
                vec![Some(card())],
                vec![Some(Node::Terminal(7)), Some(Node::Terminal(9))],
            ]
        );
        board.settle(&[card()], Pending::default());
        assert_eq!(nodes(&board), vec![vec![Some(card())]]);
    }

    /// Kept across a restart: the card and notes by name, the terminals as
    /// places the revived ones take in reading order.
    #[test]
    fn terminals_come_back_to_their_places() {
        let mut board = Board::default();
        board.settle(
            &[card(), Node::Terminal(1), Node::Terminal(2)],
            Pending::default(),
        );
        board.move_card(
            &note("a"),
            Target {
                column: 0,
                index: 0,
            },
        );
        board.move_card(
            &Node::Terminal(2),
            Target {
                column: 0,
                index: 1,
            },
        );
        let keys = board.keys();
        assert_eq!(
            keys,
            vec![vec![Key::Card, Key::Terminal], vec![Key::Terminal]]
        );
        let mut back = Board::from_keys(std::path::Path::new("/r"), &keys, &[]);
        back.settle(
            &[card(), Node::Terminal(30), Node::Terminal(31)],
            Pending::default(),
        );
        assert_eq!(
            nodes(&back),
            vec![
                vec![Some(card()), Some(Node::Terminal(30))],
                vec![Some(Node::Terminal(31))],
            ]
        );
    }

    /// A terminal asked for in a column goes there, even with a place held
    /// elsewhere; the places nobody came back to are let go.
    #[test]
    fn a_wish_beats_a_held_place_and_held_places_go() {
        let mut board = Board::from_keys(
            std::path::Path::new("/r"),
            &[vec![Key::Card], vec![Key::Terminal]],
            &[],
        );
        board.settle(&[card(), Node::Terminal(5)], Pending::terminal(0));
        assert_eq!(
            nodes(&board),
            vec![vec![Some(card()), Some(Node::Terminal(5))], vec![None]]
        );
        assert!(board.drop_vacants());
        assert_eq!(
            nodes(&board),
            vec![vec![Some(card()), Some(Node::Terminal(5))]]
        );
        assert!(!board.drop_vacants());
    }

    /// A note asked for from a column's button lands at its foot; one that
    /// comes by itself goes under the card.
    #[test]
    fn a_note_goes_where_it_was_asked() {
        let mut board = Board::default();
        board.settle(&[card(), Node::Terminal(7)], Pending::default());
        let asked = Pending {
            terminal: None,
            note: Some(1),
        };
        board.settle(&[card(), Node::Terminal(7), note("a")], asked);
        board.settle(
            &[card(), Node::Terminal(7), note("a"), note("b")],
            Pending::default(),
        );
        assert_eq!(
            nodes(&board),
            vec![
                vec![Some(card()), Some(note("b"))],
                vec![Some(Node::Terminal(7)), Some(note("a"))],
            ]
        );
    }

    /// A card dropped in another column, lower in its own, or right of the
    /// last one; a column it empties goes.
    #[test]
    fn a_card_moves_between_columns() {
        let mut board = Board::default();
        board.settle(
            &[card(), note("a"), note("b"), Node::Terminal(7)],
            Pending::default(),
        );
        board.move_card(
            &note("a"),
            Target {
                column: 0,
                index: 3,
            },
        );
        assert_eq!(
            nodes(&board)[0],
            vec![Some(card()), Some(note("b")), Some(note("a"))]
        );
        board.move_card(
            &note("b"),
            Target {
                column: 1,
                index: 0,
            },
        );
        assert_eq!(
            nodes(&board)[1],
            vec![Some(note("b")), Some(Node::Terminal(7))]
        );
        board.move_card(
            &Node::Terminal(7),
            Target {
                column: 2,
                index: 0,
            },
        );
        assert_eq!(nodes(&board).len(), 3);
        board.move_card(
            &note("b"),
            Target {
                column: 0,
                index: 1,
            },
        );
        assert_eq!(
            nodes(&board),
            vec![
                vec![Some(card()), Some(note("b")), Some(note("a"))],
                vec![Some(Node::Terminal(7))],
            ]
        );
    }

    /// A place held for a terminal is not on screen, and a drop read off
    /// the screen still lands where it was aimed.
    #[test]
    fn a_drop_is_counted_past_the_held_places() {
        let mut board = Board::from_keys(
            std::path::Path::new("/r"),
            &[
                vec![Key::Card, Key::Terminal, Key::Note("/n.md".into())],
                vec![Key::Terminal],
            ],
            &[],
        );
        board.settle(&[card(), Node::Note("/n.md".into())], Pending::default());
        let shown = board.shown();
        assert_eq!(shown.len(), 1);
        assert_eq!(
            shown[0].1.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert_eq!(
            board.resolve(Target {
                column: 0,
                index: 1
            }),
            Target {
                column: 0,
                index: 2
            }
        );
        assert_eq!(
            board.resolve(Target {
                column: 0,
                index: 2
            }),
            Target {
                column: 0,
                index: 3
            }
        );
        assert_eq!(
            board.resolve(Target {
                column: 1,
                index: 0
            }),
            Target {
                column: 2,
                index: 0
            }
        );
    }

    /// The room opened for a drop is taken back out before the next drop
    /// is read: the cards below it are where they would be without it.
    #[test]
    fn the_room_opened_for_a_drop_does_not_move_the_aim() {
        let geometry = Geometry {
            spans: vec![ColumnSpan {
                left: 0.,
                right: 100.,
                cards: vec![(0., 100.), (190., 290.)],
            }],
            opened: Some((0, 1, 80.)),
        };
        let closed = geometry.closed();
        assert_eq!(closed[0].cards, vec![(0., 100.), (110., 210.)]);
        // Aimed just above the second card's middle: still before it, where
        // the painted card would have said after the room's top.
        assert_eq!(
            drop_target(&closed, (50., 150.)),
            Some(Target {
                column: 0,
                index: 1
            })
        );
        assert_eq!(
            drop_target(&closed, (50., 170.)),
            Some(Target {
                column: 0,
                index: 2
            })
        );
    }

    /// A column keeps the width the hand gave it through the moves of its
    /// cards, loses it with its last card, and a new column fills.
    #[test]
    fn a_width_goes_with_its_column() {
        let mut board = Board::default();
        board.settle(
            &[card(), Node::Terminal(7), Node::Terminal(8)],
            Pending::default(),
        );
        board.set_width(1, Some(700.));
        assert_eq!(
            (board.width(0), board.width(1), board.width(2)),
            (None, Some(700.), None)
        );
        // The first column's card leaves it: the column goes, and the width
        // stays with the terminal's column, now the first.
        board.move_card(
            &card(),
            Target {
                column: 2,
                index: 0,
            },
        );
        assert_eq!(board.width(0), Some(700.));
        assert_eq!(board.width(1), None);
        assert_eq!(board.widths(), vec![Some(700.), None]);
        let back = Board::from_keys(std::path::Path::new("/r"), &board.keys(), &board.widths());
        assert_eq!(back.width(0), Some(700.));
        // A card dropped right of the last makes a column that fills.
        let mut board = back;
        board.settle(
            &[card(), Node::Terminal(7), Node::Terminal(8)],
            Pending::default(),
        );
        board.move_card(
            &card(),
            Target {
                column: 2,
                index: 0,
            },
        );
        assert_eq!(board.width(2), None);
    }

    /// Under a tall card as soon as the pointer is past its head, and not
    /// only past its middle.
    #[test]
    fn a_tall_card_is_passed_by_its_head() {
        let columns = [ColumnSpan {
            left: 0.,
            right: 100.,
            cards: vec![(0., 200.), (210., 1000.)],
        }];
        let at = |y| drop_target(&columns, (50., y)).map(|target| target.index);
        assert_eq!(at(40.), Some(0));
        assert_eq!(at(60.), Some(1));
        assert_eq!(at(250.), Some(1));
        assert_eq!(at(270.), Some(2));
    }

    #[test]
    fn a_drop_lands_by_the_middles_of_the_cards() {
        let columns = [
            ColumnSpan {
                left: 0.,
                right: 100.,
                cards: vec![(0., 100.), (110., 200.)],
            },
            ColumnSpan {
                left: 120.,
                right: 220.,
                cards: vec![(0., 300.)],
            },
        ];
        let at = |x, y| drop_target(&columns, (x, y));
        assert_eq!(
            at(50., 20.),
            Some(Target {
                column: 0,
                index: 0
            })
        );
        assert_eq!(
            at(50., 60.),
            Some(Target {
                column: 0,
                index: 1
            })
        );
        assert_eq!(
            at(50., 500.),
            Some(Target {
                column: 0,
                index: 2
            })
        );
        // Between two columns, the nearer one.
        assert_eq!(
            at(115., 400.),
            Some(Target {
                column: 1,
                index: 1
            })
        );
        // Right of the last: a new column.
        assert_eq!(
            at(400., 10.),
            Some(Target {
                column: 2,
                index: 0
            })
        );
        assert_eq!(drop_target(&[], (0., 0.)), None);
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn the_primary_alone_unless_chosen() {
        let on_show = paths(&["/a", "/b", "/c"]);
        // Chosen elsewhere: that one alone.
        assert_eq!(
            shown_worktrees(&paths(&["/a", "/b"]), Some(Path::new("/c")), &on_show),
            paths(&["/c"])
        );
        // Among the chosen: all of them, in the sidebar's order.
        assert_eq!(
            shown_worktrees(&paths(&["/c", "/a"]), Some(Path::new("/a")), &on_show),
            paths(&["/a", "/c"])
        );
        // What the pickers no longer show drops out, the primary never.
        assert_eq!(
            shown_worktrees(&paths(&["/z", "/b", "/y"]), Some(Path::new("/z")), &on_show),
            paths(&["/z", "/b"])
        );
        assert!(shown_worktrees(&[], None, &on_show).is_empty());
    }

    #[test]
    fn a_ctrl_click_adds_or_removes_never_the_last() {
        let shown = paths(&["/a"]);
        assert_eq!(toggled(&shown, Path::new("/b")), paths(&["/a", "/b"]));
        assert_eq!(toggled(&shown, Path::new("/a")), paths(&["/a"]));
        assert_eq!(
            toggled(&paths(&["/a", "/b"]), Path::new("/a")),
            paths(&["/b"])
        );
    }

    #[test]
    fn two_letters_stand_for_a_worktree() {
        assert_eq!(initials("feature-login"), "FL");
        assert_eq!(initials("wt/fix_merge"), "FM");
        assert_eq!(initials("claudhub"), "CL");
        assert_eq!(initials("é"), "É");
        assert_eq!(initials("--"), "");
    }
}
