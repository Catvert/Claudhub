//! The home screen's plane: where every node stands, and how the plane is
//! looked at.
//!
//! **One column per worktree**, read top to bottom: the repository's git node
//! above them all, then each worktree's card with its terminals stacked under
//! it in the order they were opened — so that a terminal is read beneath what
//! it belongs to rather than in a row of every project's shells. A column
//! stands **lower by one step per branching** — the main checkout first, the
//! worktrees cut from it a step down — and a line runs from the card of the
//! branch it started from, the way a file tree says which folder holds which.
//!
//! **A node moved by hand keeps its place.** What the hand says is an offset
//! from where the layout would put the node, so a worktree's terminals follow
//! its card wherever it is dragged — they stand under it until they are
//! dragged themselves — and a worktree added later still finds a free column.
//!
//! **Zoom is a scale, not a layout.** Zooming out does not repack the columns:
//! what one reads at a distance has to be where one will find it close up.
//! And a terminal keeps its grid at every zoom — its font grows and shrinks
//! with its card, the columns stay — so zooming never reflows a program.
//!
//! Pure: the view hands in what is open, this says where it goes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A worktree's card, in plane units — pixels at a zoom of one.
pub const CARD: (f32, f32) = (300., 244.);
/// A repository's git node.
pub const GIT: (f32, f32) = (300., 150.);
/// How far a column moves down per branching step.
const STEP: f32 = 56.;
/// Between two columns.
const COLUMN_GAP: f32 = 48.;
/// Between two nodes of a column.
const ROW_GAP: f32 = 28.;
/// Between two repositories, when all of them are shown.
const GROUP_GAP: f32 = 96.;
/// How far in from a node's left edge the vertical links run: a column's
/// nodes are left-aligned, so its links are one straight line.
const LINK_X: f32 = 28.;
/// The height of a node's head: what one drags it by, and where a branch's
/// link leaves its card.
pub const HEAD: f32 = 32.;

pub const MIN_ZOOM: f32 = 0.15;
pub const MAX_ZOOM: f32 = 2.;
/// One press of a zoom button.
pub const ZOOM_STEP: f32 = 1.25;
/// Room kept around what `fit` and `reveal` bring into view.
const MARGIN: f32 = 24.;

/// A terminal card's size.
///
/// **Presets and not a free size**, the multiplexer's reason: a card one
/// drags to a size is one dragged every time, and a terminal wants one of
/// three things — small to keep an eye on it, medium to read it, large to
/// work in it. In plane units, so a preset is a number of columns and lines
/// whatever the zoom.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tile {
    Small,
    #[default]
    Medium,
    Large,
}

impl Tile {
    pub fn size(self) -> (f32, f32) {
        match self {
            Tile::Small => (520., 340.),
            Tile::Medium => (760., 480.),
            Tile::Large => (1040., 660.),
        }
    }

    /// Round and round: the head carries one button.
    pub fn next(self) -> Self {
        match self {
            Tile::Small => Tile::Medium,
            Tile::Medium => Tile::Large,
            Tile::Large => Tile::Small,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tile::Small => "S",
            Tile::Medium => "M",
            Tile::Large => "L",
        }
    }
}

/// What can be dragged on the plane.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    /// A repository's git node, by its main path.
    Git(PathBuf),
    Worktree(PathBuf),
    /// A terminal, by its view's id: it lives no longer than the process.
    Terminal(u64),
}

/// Where the hand has put nodes, as offsets from where the layout would.
pub type Moved = HashMap<Node, (f32, f32)>;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    fn union(&self, other: &Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect {
            x,
            y,
            w: self.right().max(other.right()) - x,
            h: self.bottom().max(other.bottom()) - y,
        }
    }

    fn moved(self, (dx, dy): (f32, f32)) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            ..self
        }
    }
}

/// A worktree as the plan needs it.
#[derive(Debug, Clone)]
pub struct Checkout<'a> {
    pub path: &'a Path,
    pub branch: Option<&'a str>,
    pub is_main: bool,
    /// The branch it most likely started from, as `git::outline` guessed it;
    /// `None` until the first reading.
    pub base: Option<&'a str>,
    /// Its terminals, in the order they were opened.
    pub terminals: Vec<(u64, Tile)>,
}

/// A repository: its git node, and its worktrees, the main one included.
#[derive(Debug, Clone)]
pub struct Group<'a> {
    pub main: &'a Path,
    pub checkouts: Vec<Checkout<'a>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub path: PathBuf,
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub id: u64,
    pub rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// From a repository's git node to the checkouts no branch leads to.
    Root,
    /// From the card of the branch a worktree started from to its card.
    Branch,
    /// From a card to its first terminal, and from each terminal to the next.
    Terminal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub from: (f32, f32),
    pub to: (f32, f32),
    pub kind: LinkKind,
    /// The worktree the link leads into: what says it belongs to the one on
    /// screen.
    pub worktree: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    /// The git nodes, by main path.
    pub gits: Vec<Card>,
    pub cards: Vec<Card>,
    pub tiles: Vec<Placed>,
    pub links: Vec<Link>,
    /// Everything, for `fit` and the scrollbars.
    pub bounds: Rect,
}

impl Plan {
    pub fn tile(&self, id: u64) -> Option<Rect> {
        self.tiles.iter().find(|tile| tile.id == id).map(|t| t.rect)
    }

    #[cfg(test)]
    pub fn card(&self, path: &Path) -> Option<Rect> {
        self.cards
            .iter()
            .find(|card| card.path == path)
            .map(|c| c.rect)
    }
}

/// Which checkout each one branched from, by index.
///
/// The one whose branch is the other's base — `origin/main` naming the
/// checkout of `main` too — and failing that the main checkout: a worktree
/// whose base is checked out nowhere started from the repository all the
/// same. The main checkout is a root whatever it guessed of itself: the
/// branch it has diverged least from may well be a feature just cut from it.
pub fn parents(checkouts: &[Checkout]) -> Vec<Option<usize>> {
    let main = checkouts.iter().position(|c| c.is_main);
    checkouts
        .iter()
        .enumerate()
        .map(|(index, checkout)| {
            if checkout.is_main {
                return None;
            }
            let by_base = checkout.base.and_then(|base| {
                let holds = |name: &str| {
                    checkouts
                        .iter()
                        .enumerate()
                        .position(|(other, c)| other != index && c.branch == Some(name))
                };
                holds(base).or_else(|| {
                    crate::git::branch::split_remote(base).and_then(|(_, short)| holds(short))
                })
            });
            by_base.or(main.filter(|&main| main != index))
        })
        .collect()
}

/// The checkouts in column order, with their depth: each after the one it
/// branched from, depth first. A cycle — two branches each guessing the
/// other — cannot come from `parents` through the main checkout, but a
/// repository without one can have it: what no root reaches is laid out as a
/// root of its own, so nothing is lost.
fn lanes(parents: &[Option<usize>]) -> Vec<(usize, usize)> {
    let mut order = Vec::with_capacity(parents.len());
    let mut placed = vec![false; parents.len()];
    fn visit(
        node: usize,
        depth: usize,
        parents: &[Option<usize>],
        placed: &mut [bool],
        order: &mut Vec<(usize, usize)>,
    ) {
        if placed[node] {
            return;
        }
        placed[node] = true;
        order.push((node, depth));
        for child in (0..parents.len()).filter(|&c| parents[c] == Some(node)) {
            visit(child, depth + 1, parents, placed, order);
        }
    }
    for root in (0..parents.len()).filter(|&n| parents[n].is_none()) {
        visit(root, 0, parents, &mut placed, &mut order);
    }
    for node in 0..parents.len() {
        visit(node, 0, parents, &mut placed, &mut order);
    }
    order
}

pub fn plan(groups: &[Group], moved: &Moved) -> Plan {
    let mut plan = Plan::default();
    let mut bounds: Option<Rect> = None;
    fn grow(rect: Rect, bounds: &mut Option<Rect>) {
        *bounds = Some(match bounds {
            Some(b) => b.union(&rect),
            None => rect,
        });
    }
    let offset = |node: Node| moved.get(&node).copied().unwrap_or((0., 0.));
    let add = |a: (f32, f32), b: (f32, f32)| (a.0 + b.0, a.1 + b.1);
    let mut top = 0.;
    for group in groups {
        let git = Rect {
            x: 0.,
            y: top,
            w: GIT.0,
            h: GIT.1,
        }
        .moved(offset(Node::Git(group.main.to_path_buf())));
        grow(git, &mut bounds);
        plan.gits.push(Card {
            path: group.main.to_path_buf(),
            rect: git,
        });
        let first_card = top + GIT.1 + 2. * ROW_GAP;
        let mut bottom = top + GIT.1;
        let parents = parents(&group.checkouts);
        let mut rects: Vec<Option<Rect>> = vec![None; group.checkouts.len()];
        let mut x = 0.;
        for (index, depth) in lanes(&parents) {
            let checkout = &group.checkouts[index];
            let own = offset(Node::Worktree(checkout.path.to_path_buf()));
            let card = Rect {
                x,
                y: first_card + depth as f32 * STEP,
                w: CARD.0,
                h: CARD.1,
            }
            .moved(own);
            rects[index] = Some(card);
            grow(card, &mut bounds);
            bottom = f32::max(bottom, card.bottom());
            plan.cards.push(Card {
                path: checkout.path.to_path_buf(),
                rect: card,
            });
            match parents[index].and_then(|p| rects[p]) {
                Some(parent) => plan.links.push(Link {
                    from: (parent.right(), parent.y + HEAD / 2.),
                    to: (card.x + LINK_X, card.y),
                    kind: LinkKind::Branch,
                    worktree: checkout.path.to_path_buf(),
                }),
                None => plan.links.push(Link {
                    from: (git.x + LINK_X, git.bottom()),
                    to: (card.x + LINK_X, card.y),
                    kind: LinkKind::Root,
                    worktree: checkout.path.to_path_buf(),
                }),
            }
            // The column's own width, before any hand moved it: the next
            // column goes where it would have, whatever this one did.
            let mut width = CARD.0;
            let mut y = first_card + depth as f32 * STEP + CARD.1 + ROW_GAP;
            let mut previous = card;
            for &(id, tile) in &checkout.terminals {
                let (w, h) = tile.size();
                let rect = Rect { x, y, w, h }.moved(add(own, offset(Node::Terminal(id))));
                plan.links.push(Link {
                    from: (previous.x + LINK_X, previous.bottom()),
                    to: (rect.x + LINK_X, rect.y),
                    kind: LinkKind::Terminal,
                    worktree: checkout.path.to_path_buf(),
                });
                plan.tiles.push(Placed { id, rect });
                grow(rect, &mut bounds);
                bottom = f32::max(bottom, rect.bottom());
                previous = rect;
                y += h + ROW_GAP;
                width = width.max(w);
            }
            x += width + COLUMN_GAP;
        }
        top = bottom + GROUP_GAP;
    }
    plan.bounds = bounds.unwrap_or_default();
    plan
}

/// A scrollbar's thumb along one axis: where it starts and how long it is,
/// in pixels of a track as long as the viewport. `content` is where the
/// plane's bounds are on screen along that axis. `None` when everything is
/// in view — a bar with nothing to scroll is noise.
///
/// What the bar measures is the plane **and** the viewport together: one can
/// pan past the edge of what there is, and the thumb has to say so rather
/// than pin itself to a border.
pub fn thumb(content: (f32, f32), viewport: f32) -> Option<(f32, f32)> {
    let (low, high) = (content.0.min(0.), content.1.max(viewport));
    let total = high - low;
    if viewport <= 0. || total <= viewport + 0.5 {
        return None;
    }
    Some((-low / total * viewport, viewport / total * viewport))
}

/// How far the plane moves, in screen pixels, for a thumb dragged by one.
pub fn thumb_ratio(content: (f32, f32), viewport: f32) -> f32 {
    let (low, high) = (content.0.min(0.), content.1.max(viewport));
    if viewport <= 0. {
        return 1.;
    }
    (high - low) / viewport
}

/// How the plane is looked at: a point `p` of the plane is at
/// `p × zoom + pan` on the screen, the screen being the canvas's own box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub zoom: f32,
    pub pan: (f32, f32),
}

impl Default for View {
    fn default() -> Self {
        Self {
            zoom: 1.,
            pan: (MARGIN, MARGIN),
        }
    }
}

impl View {
    pub fn screen(self, rect: Rect) -> Rect {
        Rect {
            x: rect.x * self.zoom + self.pan.0,
            y: rect.y * self.zoom + self.pan.1,
            w: rect.w * self.zoom,
            h: rect.h * self.zoom,
        }
    }

    pub fn point(self, (x, y): (f32, f32)) -> (f32, f32) {
        (x * self.zoom + self.pan.0, y * self.zoom + self.pan.1)
    }

    /// Zooms by `factor`, keeping the point of the plane under `anchor` —
    /// the pointer, or the canvas's middle for a button — where it is.
    pub fn zoom_at(self, factor: f32, anchor: (f32, f32)) -> View {
        let zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let ratio = zoom / self.zoom;
        View {
            zoom,
            pan: (
                anchor.0 - (anchor.0 - self.pan.0) * ratio,
                anchor.1 - (anchor.1 - self.pan.1) * ratio,
            ),
        }
    }

    /// Everything in view, and never larger than life: a plane smaller than
    /// the canvas stays at a zoom of one, in its top-left corner.
    pub fn fit(bounds: Rect, viewport: (f32, f32)) -> View {
        if bounds.w <= 0. || bounds.h <= 0. {
            return View::default();
        }
        let room = (
            (viewport.0 - 2. * MARGIN).max(1.),
            (viewport.1 - 2. * MARGIN).max(1.),
        );
        let zoom = (room.0 / bounds.w)
            .min(room.1 / bounds.h)
            .clamp(MIN_ZOOM, 1.);
        View {
            zoom,
            pan: (MARGIN - bounds.x * zoom, MARGIN - bounds.y * zoom),
        }
    }

    /// The least move that brings `rect` into view, at the same zoom. What
    /// does not fit shows its top-left corner: a terminal's head and its
    /// first lines are what one came to.
    pub fn reveal(self, rect: Rect, viewport: (f32, f32)) -> View {
        let on_screen = self.screen(rect);
        let axis = |start: f32, len: f32, room: f32| -> f32 {
            if start < MARGIN || len > room - 2. * MARGIN {
                MARGIN - start
            } else if start + len > room - MARGIN {
                room - MARGIN - (start + len)
            } else {
                0.
            }
        };
        View {
            zoom: self.zoom,
            pan: (
                self.pan.0 + axis(on_screen.x, on_screen.w, viewport.0),
                self.pan.1 + axis(on_screen.y, on_screen.h, viewport.1),
            ),
        }
    }
}

/// The zoom factor of a wheel movement, in pixels: a notch — three lines of
/// about twenty pixels — is a tenth or so, and a trackpad's small steps add
/// up to the same thing.
pub fn wheel_factor(pixels_up: f32) -> f32 {
    1.0018_f32.powf(pixels_up)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout<'a>(
        path: &'a str,
        branch: &'a str,
        is_main: bool,
        base: Option<&'a str>,
    ) -> Checkout<'a> {
        Checkout {
            path: Path::new(path),
            branch: Some(branch),
            is_main,
            base,
            terminals: Vec::new(),
        }
    }

    #[test]
    fn a_worktree_hangs_under_the_checkout_of_its_base() {
        let checkouts = [
            checkout("/r", "main", true, Some("feature")),
            checkout("/r-a", "feature", false, Some("origin/main")),
            checkout("/r-b", "fix", false, Some("feature")),
            checkout("/r-c", "loose", false, Some("gone")),
            checkout("/r-d", "fresh", false, None),
        ];
        assert_eq!(
            parents(&checkouts),
            // The main one is a root even though it guessed `feature`; a
            // remote name finds the local checkout; an unknown base and no
            // base at all both hang under the main checkout.
            vec![None, Some(0), Some(1), Some(0), Some(0)]
        );
    }

    #[test]
    fn lanes_go_depth_first_and_a_cycle_loses_nothing() {
        assert_eq!(
            lanes(&[None, Some(0), Some(1), Some(0)]),
            vec![(0, 0), (1, 1), (2, 2), (3, 1)]
        );
        // No main checkout, and two branches each guessing the other.
        assert_eq!(lanes(&[Some(1), Some(0)]), vec![(0, 0), (1, 1)]);
    }

    fn group<'a>(main: &'a str, checkouts: Vec<Checkout<'a>>) -> Group<'a> {
        Group {
            main: Path::new(main),
            checkouts,
        }
    }

    #[test]
    fn a_column_is_its_card_then_its_terminals_and_is_as_wide_as_its_widest() {
        let mut main = checkout("/r", "main", true, None);
        main.terminals = vec![(7, Tile::Small), (8, Tile::Large)];
        let feature = checkout("/r-a", "feature", false, Some("main"));
        let plan = plan(&[group("/r", vec![main, feature])], &Moved::new());

        let git = plan.gits[0].rect;
        let card = plan.card(Path::new("/r")).unwrap();
        assert_eq!((card.x, card.y), (0., GIT.1 + 2. * ROW_GAP));
        let small = plan.tile(7).unwrap();
        let large = plan.tile(8).unwrap();
        assert_eq!((small.x, small.y), (0., card.bottom() + ROW_GAP));
        assert_eq!((large.x, large.y), (0., small.bottom() + ROW_GAP));

        // The branch in the next column, past the widest terminal of this
        // one, and a step lower.
        let branch = plan.card(Path::new("/r-a")).unwrap();
        assert_eq!(branch.x, Tile::Large.size().0 + COLUMN_GAP);
        assert_eq!(branch.y, card.y + STEP);

        // The git node leads to the main card, the main card's head to the
        // branch's top, and each terminal hangs under the one before: one
        // straight line down the column.
        let kinds: Vec<_> = plan.links.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                LinkKind::Root,
                LinkKind::Terminal,
                LinkKind::Terminal,
                LinkKind::Branch
            ]
        );
        assert_eq!(plan.links[0].from, (LINK_X, git.bottom()));
        assert!(plan.links[1..3].iter().all(|l| l.from.0 == l.to.0));
        assert_eq!(plan.links[3].from, (card.right(), card.y + HEAD / 2.));
        assert_eq!(plan.links[3].to, (branch.x + LINK_X, branch.y));
        assert_eq!(plan.bounds.bottom(), large.bottom());
    }

    #[test]
    fn a_moved_card_takes_its_terminals_and_leaves_the_next_column_in_place() {
        let mut main = checkout("/r", "main", true, None);
        main.terminals = vec![(7, Tile::Small)];
        let other = checkout("/r-a", "feature", false, None);
        let groups = [group("/r", vec![main, other])];
        let before = plan(&groups, &Moved::new());
        let moved = Moved::from([
            (Node::Worktree(PathBuf::from("/r")), (500., 40.)),
            (Node::Terminal(7), (0., 100.)),
        ]);
        let after = plan(&groups, &moved);
        let card = |p: &Plan| p.card(Path::new("/r")).unwrap();
        assert_eq!(card(&after).x, card(&before).x + 500.);
        // The terminal follows its card, plus its own offset.
        assert_eq!(after.tile(7).unwrap().x, before.tile(7).unwrap().x + 500.);
        assert_eq!(after.tile(7).unwrap().y, before.tile(7).unwrap().y + 140.);
        // The next column does not move for it.
        assert_eq!(
            after.card(Path::new("/r-a")),
            before.card(Path::new("/r-a"))
        );
        // And the links follow what moved.
        assert_eq!(after.links[0].to.0, card(&after).x + LINK_X);
    }

    #[test]
    fn repositories_stack() {
        let plan = plan(
            &[
                group("/a", vec![checkout("/a", "main", true, None)]),
                group("/b", vec![checkout("/b", "main", true, None)]),
            ],
            &Moved::new(),
        );
        let first = plan.card(Path::new("/a")).unwrap();
        assert_eq!(plan.gits[1].rect.y, first.bottom() + GROUP_GAP);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let view = View {
            zoom: 1.,
            pan: (10., 20.),
        };
        let anchor = (300., 200.);
        let zoomed = view.zoom_at(2., anchor);
        assert_eq!(zoomed.zoom, 2.);
        let before = (
            (anchor.0 - view.pan.0) / view.zoom,
            (anchor.1 - view.pan.1) / view.zoom,
        );
        let after = (
            (anchor.0 - zoomed.pan.0) / zoomed.zoom,
            (anchor.1 - zoomed.pan.1) / zoomed.zoom,
        );
        assert_eq!(before, after);
        assert_eq!(view.zoom_at(100., anchor).zoom, MAX_ZOOM);
        assert_eq!(view.zoom_at(0.001, anchor).zoom, MIN_ZOOM);
    }

    #[test]
    fn fitting_shows_everything_and_never_magnifies() {
        let bounds = Rect {
            x: 0.,
            y: 0.,
            w: 2000.,
            h: 1000.,
        };
        let view = View::fit(bounds, (1048., 1048.));
        assert_eq!(view.zoom, 0.5);
        assert!(view.screen(bounds).right() <= 1048. - MARGIN + 0.01);
        let small = Rect {
            w: 100.,
            h: 100.,
            ..bounds
        };
        assert_eq!(View::fit(small, (1048., 1048.)).zoom, 1.);
    }

    #[test]
    fn revealing_moves_the_least_and_only_when_needed() {
        let view = View::default();
        let viewport = (1000., 800.);
        let inside = Rect {
            x: 100.,
            y: 100.,
            w: 200.,
            h: 200.,
        };
        assert_eq!(view.reveal(inside, viewport), view);
        let right = Rect { x: 1200., ..inside };
        let moved = view.reveal(right, viewport);
        assert_eq!(moved.screen(right).right(), 1000. - MARGIN);
        assert_eq!(moved.pan.1, view.pan.1);
        let big = Rect {
            x: 500.,
            y: 500.,
            w: 5000.,
            h: 5000.,
        };
        let shown = view.reveal(big, viewport).screen(big);
        assert_eq!((shown.x, shown.y), (MARGIN, MARGIN));
    }

    #[test]
    fn the_thumb_measures_the_plane_and_the_viewport_together() {
        // Everything in view: no bar.
        assert_eq!(thumb((10., 900.), 1000.), None);
        // Twice the viewport, scrolled to the start.
        assert_eq!(thumb((0., 2000.), 1000.), Some((0., 500.)));
        // Scrolled halfway.
        assert_eq!(thumb((-1000., 1000.), 1000.), Some((500., 500.)));
        // Panned past the end: the empty room counts — 2500 in all, of
        // which the viewport is the last thousand but one hundred.
        assert_eq!(thumb((-1500., 500.), 1000.), Some((600., 400.)));
        // A thumb dragged one pixel moves the plane by the scale of the bar.
        assert_eq!(thumb_ratio((0., 2000.), 1000.), 2.);
    }

    #[test]
    fn a_wheel_up_zooms_in_and_down_zooms_out() {
        assert!(wheel_factor(60.) > 1.);
        assert!(wheel_factor(-60.) < 1.);
        assert_eq!(wheel_factor(0.), 1.);
    }

    #[test]
    fn the_terminal_presets_go_round() {
        assert_eq!(Tile::default(), Tile::Medium);
        assert_eq!(Tile::Large.next(), Tile::Small);
        assert!(Tile::Small.size().0 < Tile::Medium.size().0);
    }
}
