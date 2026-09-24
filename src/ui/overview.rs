//! The home screen's plane: where every node stands, and how the plane is
//! looked at.
//!
//! **A tree, read top to bottom, each parent centred over its children.** A
//! repository's git node is the root; under it the checkouts no branch leads
//! to — the main one — and the notes about the repository; under each
//! worktree, its notes, its terminals, then the worktrees cut from its branch.
//! Every node of a level stands on the same line, whatever its size: what is
//! aligned reads as a level, and a node added later lands in its place in the
//! tree, aligned with its siblings, with nothing for the hand to tidy.
//!
//! **A node moved by hand takes its subtree along**, as an offset from where
//! the tree would put it: a worktree dragged aside keeps its terminals and its
//! branches under it. `Reset` in the view forgets every offset and size.
//!
//! **Zoom is a scale, not a layout.** Zooming out does not repack the tree:
//! what one reads at a distance has to be where one will find it close up.
//! And a terminal keeps its grid at every zoom — its font grows and shrinks
//! with its card, the lines and columns stay — so zooming never reflows a
//! program.
//!
//! Pure: the view hands in what is open, this says where it goes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A worktree's card, in plane units — pixels at a zoom of one.
pub const CARD: (f32, f32) = (300., 244.);
/// A repository's git node.
pub const GIT: (f32, f32) = (300., 150.);
/// A note's size until the hand gives it another.
pub const NOTE: (f32, f32) = (280., 200.);
/// Between two siblings.
const SIBLING_GAP: f32 = 40.;
/// Between two levels.
const LEVEL_GAP: f32 = 64.;
/// Between two repositories' trees, when all of them are shown.
const GROUP_GAP: f32 = 160.;
/// The height of a node's head: what one drags it by.
pub const HEAD: f32 = 32.;

/// The smallest a node is let be, by kind: a card that still says its name
/// and branch, a terminal that still holds a prompt.
pub const MIN_GIT: (f32, f32) = (200., 90.);
pub const MIN_CARD: (f32, f32) = (220., 140.);
pub const MIN_TILE: (f32, f32) = (300., 160.);
pub const MIN_NOTE: (f32, f32) = (160., 90.);

pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 2.;
/// One press of a zoom button.
pub const ZOOM_STEP: f32 = 1.25;
/// Room kept around what `fit` and `reveal` bring into view.
const MARGIN: f32 = 24.;

/// A terminal card's size presets.
///
/// **Presets beside a free size**: the corner resizes to anything, and the
/// head's button jumps between the three sizes a terminal usually wants —
/// small to keep an eye on it, medium to read it, large to work in it. In
/// plane units, so a preset is a number of columns and lines whatever the
/// zoom.
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

    /// The preset a size is closest to, by width: what the head's button
    /// says, a card dragged to any size still reading as one of three.
    pub fn nearest(size: (f32, f32)) -> Tile {
        [Tile::Small, Tile::Medium, Tile::Large]
            .into_iter()
            .min_by(|a, b| {
                let gap = |t: &Tile| (t.size().0 - size.0).abs();
                gap(a).total_cmp(&gap(b))
            })
            .unwrap_or_default()
    }
}

/// A node maximised: which, and what to give back — the view from before,
/// and its size from before (`None`: the size its kind starts at).
#[derive(Debug, Clone, PartialEq)]
pub struct Maximized {
    pub node: Node,
    pub view: View,
    pub size: Option<(f32, f32)>,
}

/// The size that makes a node take `share` of the viewport at a zoom of one:
/// room it really has — a terminal gets the lines and columns — and not a
/// closer look at the room it had. Never smaller than it already is.
pub fn maximized_size(viewport: (f32, f32), share: f32, current: (f32, f32)) -> (f32, f32) {
    (
        (viewport.0 * share).max(current.0),
        (viewport.1 * share).max(current.1),
    )
}

/// A size dragged by `delta`, never under `min`.
pub fn resized(size: (f32, f32), delta: (f32, f32), min: (f32, f32)) -> (f32, f32) {
    ((size.0 + delta.0).max(min.0), (size.1 + delta.1).max(min.1))
}

/// What stands on the plane.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    /// A repository's git node, by its main path.
    Git(PathBuf),
    Worktree(PathBuf),
    /// A terminal, by its view's id: it lives no longer than the process.
    Terminal(u64),
    /// A note, by its file's path.
    Note(PathBuf),
}

/// Where the hand has put nodes, as offsets from where the tree would.
pub type Moved = HashMap<Node, (f32, f32)>;

/// The sizes the hand has given git nodes, worktree cards and notes; a
/// terminal carries its own.
pub type Sizes = HashMap<Node, (f32, f32)>;

/// Everything the hand has decided about the plane, over what the tree
/// would do by itself.
#[derive(Debug, Clone, Default)]
pub struct Hand {
    pub moved: Moved,
    pub sizes: Sizes,
    /// Folded to their head, like a minimised window: what hangs from them
    /// comes up under the head.
    pub collapsed: HashSet<Node>,
    /// Taken off the plane with everything that hangs from them.
    pub hidden: HashSet<Node>,
}

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
    /// Its terminals, in the order they were opened, with their size.
    pub terminals: Vec<(u64, (f32, f32))>,
    /// The notes hung from it, by file, in the order they were written.
    pub notes: Vec<PathBuf>,
}

/// A repository: its git node, its worktrees, and the notes hung from it.
#[derive(Debug, Clone)]
pub struct Group<'a> {
    pub main: &'a Path,
    pub checkouts: Vec<Checkout<'a>>,
    pub notes: Vec<PathBuf>,
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
    /// From a card to one of its terminals.
    Terminal,
    /// From a git node or a card to a note hung from it.
    Note,
}

/// A side of a node, where a link leaves or arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

impl Side {
    fn middle(self, rect: Rect) -> (f32, f32) {
        match self {
            Side::Top => (rect.x + rect.w / 2., rect.y),
            Side::Bottom => (rect.x + rect.w / 2., rect.bottom()),
            Side::Left => (rect.x, rect.y + rect.h / 2.),
            Side::Right => (rect.right(), rect.y + rect.h / 2.),
        }
    }
}

/// Where a link between two nodes leaves the one and reaches the other: the
/// two sides that face each other, on the axis where the nodes stand furthest
/// apart. A child under its parent hangs from the bottom — the tree as laid
/// out; dragged beside it, it is reached from the side it went to, and the
/// line no longer crosses the card to get there.
pub fn attach(parent: Rect, child: Rect) -> ((f32, f32), Side, (f32, f32), Side) {
    let apart_y = (child.y - parent.bottom()).max(parent.y - child.bottom());
    let apart_x = (child.x - parent.right()).max(parent.x - child.right());
    let (from, to) = if apart_y >= apart_x {
        if child.y + child.h / 2. >= parent.y + parent.h / 2. {
            (Side::Bottom, Side::Top)
        } else {
            (Side::Top, Side::Bottom)
        }
    } else if child.x + child.w / 2. >= parent.x + parent.w / 2. {
        (Side::Right, Side::Left)
    } else {
        (Side::Left, Side::Right)
    };
    (from.middle(parent), from, to.middle(child), to)
}

/// The four points of a link drawn as an elbow: out of `from` square to its
/// side, across half way, and into `to` square to the side facing back — an
/// org chart's line, which reads as a tree where a long curve read as a
/// swoop. The view rounds the two corners.
pub fn elbow(from: (f32, f32), side: Side, to: (f32, f32)) -> [(f32, f32); 4] {
    match side {
        Side::Top | Side::Bottom => {
            let middle = (from.1 + to.1) / 2.;
            [from, (from.0, middle), (to.0, middle), to]
        }
        Side::Left | Side::Right => {
            let middle = (from.0 + to.0) / 2.;
            [from, (middle, from.1), (middle, to.1), to]
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub from: (f32, f32),
    pub to: (f32, f32),
    pub from_side: Side,
    pub to_side: Side,
    pub kind: LinkKind,
    /// The worktree the link belongs to — the child's, or the parent's for a
    /// terminal or a note: what says it belongs to the one on screen.
    pub worktree: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    /// The git nodes, by main path.
    pub gits: Vec<Card>,
    pub cards: Vec<Card>,
    pub tiles: Vec<Placed>,
    pub notes: Vec<Card>,
    pub links: Vec<Link>,
    /// Everything, for `fit` and the scrollbars.
    pub bounds: Rect,
}

impl Plan {
    pub fn tile(&self, id: u64) -> Option<Rect> {
        self.tiles.iter().find(|tile| tile.id == id).map(|t| t.rect)
    }

    /// Where a node stands, whatever its kind.
    pub fn rect(&self, node: &Node) -> Option<Rect> {
        match node {
            Node::Git(path) => self.gits.iter().find(|c| &c.path == path).map(|c| c.rect),
            Node::Worktree(path) => self.cards.iter().find(|c| &c.path == path).map(|c| c.rect),
            Node::Terminal(id) => self.tile(*id),
            Node::Note(path) => self.notes.iter().find(|n| &n.path == path).map(|n| n.rect),
        }
    }

    #[cfg(test)]
    pub fn note(&self, path: &str) -> Option<Rect> {
        self.rect(&Node::Note(PathBuf::from(path)))
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

/// One node of a repository's tree, before it is placed.
struct Branch {
    node: Node,
    size: (f32, f32),
    /// How it hangs from its parent.
    kind: LinkKind,
    worktree: PathBuf,
    children: Vec<usize>,
}

/// A repository's tree: index 0 is the git node.
fn tree(group: &Group, hand: &Hand) -> Vec<Branch> {
    let sizes = &hand.sizes;
    let size = |node: &Node, default: (f32, f32)| sizes.get(node).copied().unwrap_or(default);
    let main = group.main.to_path_buf();
    let git = Node::Git(main.clone());
    let mut nodes = vec![Branch {
        size: size(&git, GIT),
        node: git,
        kind: LinkKind::Root,
        worktree: main.clone(),
        children: Vec::new(),
    }];
    let add = |nodes: &mut Vec<Branch>, parent: usize, branch: Branch| {
        nodes.push(branch);
        let index = nodes.len() - 1;
        nodes[parent].children.push(index);
        index
    };
    for path in &group.notes {
        let node = Node::Note(path.clone());
        add(
            &mut nodes,
            0,
            Branch {
                size: size(&node, NOTE),
                node,
                kind: LinkKind::Note,
                worktree: main.clone(),
                children: Vec::new(),
            },
        );
    }
    // The worktrees, each under the one it branched from, depth first; what
    // no branch leads to — the main checkout, and whatever a cycle of
    // guesses left unreached — hangs from the git node.
    let parents = parents(&group.checkouts);
    let mut placed: Vec<Option<usize>> = vec![None; group.checkouts.len()];
    #[allow(clippy::too_many_arguments)]
    fn visit(
        checkout: usize,
        parent: usize,
        group: &Group,
        parents: &[Option<usize>],
        placed: &mut Vec<Option<usize>>,
        nodes: &mut Vec<Branch>,
        sizes: &Sizes,
        hidden: &HashSet<Node>,
    ) {
        if placed[checkout].is_some() {
            return;
        }
        let this = &group.checkouts[checkout];
        let path = this.path.to_path_buf();
        let node = Node::Worktree(path.clone());
        // Hidden, it goes with everything under it: its branches are marked
        // placed, so the pass that rescues the unreached does not bring them
        // back as roots.
        if hidden.contains(&node) {
            fn drop_under(
                checkout: usize,
                parents: &[Option<usize>],
                placed: &mut [Option<usize>],
            ) {
                placed[checkout] = Some(usize::MAX);
                for child in (0..parents.len()).filter(|&c| parents[c] == Some(checkout)) {
                    if placed[child].is_none() {
                        drop_under(child, parents, placed);
                    }
                }
            }
            drop_under(checkout, parents, placed);
            return;
        }
        let kind = if parent == 0 {
            LinkKind::Root
        } else {
            LinkKind::Branch
        };
        nodes.push(Branch {
            size: sizes.get(&node).copied().unwrap_or(CARD),
            node,
            kind,
            worktree: path.clone(),
            children: Vec::new(),
        });
        let index = nodes.len() - 1;
        nodes[parent].children.push(index);
        placed[checkout] = Some(index);
        for path in &this.notes {
            let node = Node::Note(path.clone());
            nodes.push(Branch {
                size: sizes.get(&node).copied().unwrap_or(NOTE),
                node,
                kind: LinkKind::Note,
                worktree: path.clone(),
                children: Vec::new(),
            });
            let child = nodes.len() - 1;
            nodes[index].children.push(child);
        }
        for &(id, size) in &this.terminals {
            nodes.push(Branch {
                size,
                node: Node::Terminal(id),
                kind: LinkKind::Terminal,
                worktree: path.clone(),
                children: Vec::new(),
            });
            let child = nodes.len() - 1;
            nodes[index].children.push(child);
        }
        for child in (0..parents.len()).filter(|&c| parents[c] == Some(checkout)) {
            visit(child, index, group, parents, placed, nodes, sizes, hidden);
        }
    }
    let hidden = &hand.hidden;
    for root in (0..parents.len()).filter(|&c| parents[c].is_none()) {
        visit(
            root,
            0,
            group,
            &parents,
            &mut placed,
            &mut nodes,
            sizes,
            hidden,
        );
    }
    for checkout in 0..parents.len() {
        visit(
            checkout,
            0,
            group,
            &parents,
            &mut placed,
            &mut nodes,
            sizes,
            hidden,
        );
    }
    // A folded node keeps its width and gives its height back: the level
    // under it comes up.
    for branch in &mut nodes {
        if hand.collapsed.contains(&branch.node) {
            branch.size.1 = HEAD;
        }
    }
    nodes
}

/// How wide each node's subtree is: its own width, or its children side by
/// side if that is wider.
fn widths(nodes: &[Branch]) -> Vec<f32> {
    fn width(index: usize, nodes: &[Branch], out: &mut [f32]) -> f32 {
        let children: f32 = nodes[index]
            .children
            .iter()
            .map(|&child| width(child, nodes, out))
            .sum::<f32>()
            + SIBLING_GAP * nodes[index].children.len().saturating_sub(1) as f32;
        out[index] = nodes[index].size.0.max(children);
        out[index]
    }
    let mut out = vec![0.; nodes.len()];
    if !nodes.is_empty() {
        width(0, nodes, &mut out);
    }
    out
}

/// Every node's depth, and each level's height: the tallest node on it.
fn levels(nodes: &[Branch]) -> (Vec<usize>, Vec<f32>) {
    let mut depth = vec![0; nodes.len()];
    let mut heights: Vec<f32> = Vec::new();
    let mut stack = vec![0];
    while let Some(index) = stack.pop() {
        let d = depth[index];
        if heights.len() <= d {
            heights.resize(d + 1, 0.);
        }
        heights[d] = heights[d].max(nodes[index].size.1);
        for &child in &nodes[index].children {
            depth[child] = d + 1;
            stack.push(child);
        }
    }
    (depth, heights)
}

pub fn plan(groups: &[Group], hand: &Hand) -> Plan {
    let moved = &hand.moved;
    let mut plan = Plan::default();
    let mut bounds: Option<Rect> = None;
    let mut left = 0.;
    for group in groups {
        let nodes = tree(group, hand);
        let widths = widths(&nodes);
        let (depth, heights) = levels(&nodes);
        let tops: Vec<f32> = heights
            .iter()
            .scan(0., |top, height| {
                let this = *top;
                *top += height + LEVEL_GAP;
                Some(this)
            })
            .collect();
        // Placed from the root down: each node centred in the room its
        // subtree was given, its children side by side under it, and the
        // hand's offset carried to everything below the node it was put on.
        let mut rects: Vec<Rect> = vec![Rect::default(); nodes.len()];
        let mut stack = vec![(0, left, (0., 0.))];
        while let Some((index, room_left, inherited)) = stack.pop() {
            let branch = &nodes[index];
            let own = moved.get(&branch.node).copied().unwrap_or((0., 0.));
            let offset = (inherited.0 + own.0, inherited.1 + own.1);
            let rect = Rect {
                x: room_left + (widths[index] - branch.size.0) / 2.,
                y: tops[depth[index]],
                w: branch.size.0,
                h: branch.size.1,
            }
            .moved(offset);
            rects[index] = rect;
            let children_width: f32 = branch.children.iter().map(|&c| widths[c]).sum::<f32>()
                + SIBLING_GAP * branch.children.len().saturating_sub(1) as f32;
            let mut x = room_left + (widths[index] - children_width) / 2.;
            for &child in &branch.children {
                stack.push((child, x, offset));
                x += widths[child] + SIBLING_GAP;
            }
        }
        for (index, branch) in nodes.iter().enumerate() {
            let rect = rects[index];
            bounds = Some(match bounds {
                Some(b) => b.union(&rect),
                None => rect,
            });
            for &child in &branch.children {
                let (from, from_side, to, to_side) = attach(rect, rects[child]);
                plan.links.push(Link {
                    from,
                    to,
                    from_side,
                    to_side,
                    kind: nodes[child].kind,
                    worktree: nodes[child].worktree.clone(),
                });
            }
            match &branch.node {
                Node::Git(path) => plan.gits.push(Card {
                    path: path.clone(),
                    rect,
                }),
                Node::Worktree(path) => plan.cards.push(Card {
                    path: path.clone(),
                    rect,
                }),
                Node::Terminal(id) => plan.tiles.push(Placed { id: *id, rect }),
                Node::Note(path) => plan.notes.push(Card {
                    path: path.clone(),
                    rect,
                }),
            }
        }
        left += widths.first().copied().unwrap_or(0.) + GROUP_GAP;
    }
    plan.bounds = bounds.unwrap_or_default();
    plan
}

/// The grid's step in plane units, at a zoom of one.
const GRID: f32 = 40.;
/// Closer than this on screen, the dots would be a tint and not a grid.
const GRID_MIN_SCREEN: f32 = 24.;

/// Where the background grid's lines fall along one axis, in screen pixels
/// from the canvas's edge: every `GRID` of the plane, doubled until they
/// stand at least `GRID_MIN_SCREEN` apart.
pub fn grid(pan: f32, zoom: f32, extent: f32) -> Vec<f32> {
    let mut step = GRID * zoom;
    if step <= 0. || extent <= 0. {
        return Vec::new();
    }
    while step < GRID_MIN_SCREEN {
        step *= 2.;
    }
    let first = pan.rem_euclid(step);
    let mut lines = Vec::new();
    let mut at = first;
    while at < extent {
        lines.push(at);
        at += step;
    }
    lines
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

    /// Everything in view, centred, and never larger than life: a plane
    /// smaller than the canvas stays at a zoom of one.
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
        // Centred on both axes; a plane still taller than the canvas at the
        // smallest zoom shows its top — the git nodes, where it starts.
        let centre = |start: f32, len: f32, room: f32| room / 2. - (start + len / 2.) * zoom;
        let y = if bounds.h * zoom <= viewport.1 - 2. * MARGIN {
            centre(bounds.y, bounds.h, viewport.1)
        } else {
            MARGIN - bounds.y * zoom
        };
        View {
            zoom,
            pan: (centre(bounds.x, bounds.w, viewport.0), y),
        }
    }

    /// The zoom and pan that make `rect` take `share` of the viewport, centred
    /// — a node maximised, like a window.
    pub fn focus(rect: Rect, viewport: (f32, f32), share: f32) -> View {
        if rect.w <= 0. || rect.h <= 0. || viewport.0 <= 0. {
            return View::default();
        }
        let zoom = (viewport.0 * share / rect.w)
            .min(viewport.1 * share / rect.h)
            .clamp(MIN_ZOOM, MAX_ZOOM);
        View {
            zoom,
            pan: (
                viewport.0 / 2. - (rect.x + rect.w / 2.) * zoom,
                viewport.1 / 2. - (rect.y + rect.h / 2.) * zoom,
            ),
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
            notes: Vec::new(),
        }
    }

    fn group<'a>(main: &'a str, checkouts: Vec<Checkout<'a>>) -> Group<'a> {
        Group {
            main: Path::new(main),
            checkouts,
            notes: Vec::new(),
        }
    }

    fn centre(rect: Rect) -> f32 {
        rect.x + rect.w / 2.
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

    /// Git on top, the main checkout under it, and under the main checkout
    /// its note, its terminals and its branches, side by side on one line —
    /// each parent centred over what hangs from it.
    #[test]
    fn the_tree_goes_down_and_centres_each_parent() {
        let mut main = checkout("/r", "main", true, None);
        main.notes = vec![PathBuf::from("/r/.claudhub/notes/5.md")];
        main.terminals = vec![(7, Tile::Small.size())];
        let mut feature = checkout("/r-a", "feature", false, Some("main"));
        feature.terminals = vec![(8, Tile::Large.size())];
        let plan = plan(&[group("/r", vec![main, feature])], &Hand::default());

        let git = plan.gits[0].rect;
        let main = plan.card(Path::new("/r")).unwrap();
        let note = plan.note("/r/.claudhub/notes/5.md").unwrap();
        let small = plan.tile(7).unwrap();
        let feature = plan.card(Path::new("/r-a")).unwrap();
        let large = plan.tile(8).unwrap();

        // Levels: git, main, then its children, then the branch's terminal.
        assert_eq!(git.y, 0.);
        assert_eq!(main.y, GIT.1 + LEVEL_GAP);
        let third = main.bottom() + LEVEL_GAP;
        assert_eq!((note.y, small.y, feature.y), (third, third, third));
        // The level under is as low as the tallest of the third: a terminal.
        assert_eq!(large.y, third + Tile::Small.size().1 + LEVEL_GAP);

        // In order, left to right, one gap apart — the branch's room being
        // its terminal's width.
        assert_eq!(small.x, note.right() + SIBLING_GAP);
        let branch_room = Tile::Large.size().0;
        assert_eq!(
            feature.x,
            small.right() + SIBLING_GAP + (branch_room - CARD.0) / 2.
        );
        // Each parent centred over its children, and git over main.
        assert_eq!(
            centre(main),
            (note.x + small.right() + SIBLING_GAP + branch_room) / 2.
        );
        assert_eq!(centre(git), centre(main));
        assert_eq!(centre(feature), centre(large));

        // From the middle of a parent's bottom to the middle of a child's top.
        let to_small = plan
            .links
            .iter()
            .find(|l| l.kind == LinkKind::Terminal && l.to.1 == small.y)
            .unwrap();
        assert_eq!(to_small.from, (centre(main), main.bottom()));
        assert_eq!(to_small.to, (centre(small), small.y));
        assert_eq!(to_small.worktree, PathBuf::from("/r"));
    }

    #[test]
    fn a_link_leaves_by_the_side_facing_its_child() {
        let parent = Rect {
            x: 0.,
            y: 0.,
            w: 100.,
            h: 100.,
        };
        let at = |x, y| Rect {
            x,
            y,
            w: 100.,
            h: 100.,
        };
        // Under it: the tree as laid out.
        let (from, from_side, to, to_side) = attach(parent, at(0., 300.));
        assert_eq!((from_side, to_side), (Side::Bottom, Side::Top));
        assert_eq!((from, to), ((50., 100.), (50., 300.)));
        // Dragged to its right, a little lower: from the side, into the side.
        let (from, from_side, to, to_side) = attach(parent, at(400., 50.));
        assert_eq!((from_side, to_side), (Side::Right, Side::Left));
        assert_eq!((from, to), ((100., 50.), (400., 100.)));
        // To the left, and above.
        assert_eq!(attach(parent, at(-400., 0.)).1, Side::Left);
        assert_eq!(attach(parent, at(0., -300.)).1, Side::Top);
        // Diagonal: the axis they are furthest apart on decides.
        assert_eq!(attach(parent, at(500., 200.)).1, Side::Right);
        assert_eq!(attach(parent, at(200., 500.)).1, Side::Bottom);
    }

    #[test]
    fn an_elbow_turns_half_way_along_the_side_it_leaves() {
        assert_eq!(
            elbow((50., 100.), Side::Bottom, (400., 300.)),
            [(50., 100.), (50., 200.), (400., 200.), (400., 300.)]
        );
        assert_eq!(
            elbow((100., 50.), Side::Right, (400., 150.)),
            [(100., 50.), (250., 50.), (250., 150.), (400., 150.)]
        );
    }

    #[test]
    fn a_moved_node_takes_its_subtree_along() {
        let mut main = checkout("/r", "main", true, None);
        main.terminals = vec![(7, Tile::Small.size())];
        let other = checkout("/r-a", "feature", false, Some("main"));
        let groups = [group("/r", vec![main, other])];
        let before = plan(&groups, &Hand::default());
        let moved = Moved::from([
            (Node::Worktree(PathBuf::from("/r")), (500., 40.)),
            (Node::Terminal(7), (0., 100.)),
        ]);
        let after = plan(
            &groups,
            &Hand {
                moved,
                ..Hand::default()
            },
        );
        let card = |p: &Plan, path: &str| p.card(Path::new(path)).unwrap();
        assert_eq!(card(&after, "/r").x, card(&before, "/r").x + 500.);
        // The terminal and the branch under it follow, plus their own offset.
        assert_eq!(after.tile(7).unwrap().x, before.tile(7).unwrap().x + 500.);
        assert_eq!(after.tile(7).unwrap().y, before.tile(7).unwrap().y + 140.);
        assert_eq!(card(&after, "/r-a").x, card(&before, "/r-a").x + 500.);
        // The git node above does not.
        assert_eq!(after.gits[0].rect, before.gits[0].rect);
    }

    #[test]
    fn a_resized_node_widens_its_room_and_lowers_the_next_level() {
        let mut main = checkout("/r", "main", true, None);
        main.terminals = vec![(7, Tile::Small.size())];
        let groups = [group("/r", vec![main])];
        let before = plan(&groups, &Hand::default());
        let sizes = Sizes::from([(Node::Worktree(PathBuf::from("/r")), (CARD.0, CARD.1 + 100.))]);
        let after = plan(
            &groups,
            &Hand {
                sizes,
                ..Hand::default()
            },
        );
        assert_eq!(after.tile(7).unwrap().y, before.tile(7).unwrap().y + 100.);
    }

    #[test]
    fn a_folded_node_keeps_its_head_and_brings_the_next_level_up() {
        let mut main = checkout("/r", "main", true, None);
        main.terminals = vec![(7, Tile::Small.size())];
        let groups = [group("/r", vec![main])];
        let before = plan(&groups, &Hand::default());
        let folded = Hand {
            collapsed: HashSet::from([Node::Worktree(PathBuf::from("/r"))]),
            ..Hand::default()
        };
        let after = plan(&groups, &folded);
        let card = after.card(Path::new("/r")).unwrap();
        assert_eq!((card.w, card.h), (CARD.0, HEAD));
        assert_eq!(
            after.tile(7).unwrap().y,
            before.tile(7).unwrap().y - (CARD.1 - HEAD)
        );
    }

    #[test]
    fn a_hidden_worktree_goes_with_everything_under_it() {
        let mut feature = checkout("/r-a", "feature", false, Some("main"));
        feature.terminals = vec![(7, Tile::Small.size())];
        let fix = checkout("/r-b", "fix", false, Some("feature"));
        let groups = [group(
            "/r",
            vec![checkout("/r", "main", true, None), feature, fix],
        )];
        let hand = Hand {
            hidden: HashSet::from([Node::Worktree(PathBuf::from("/r-a"))]),
            ..Hand::default()
        };
        let plan = plan(&groups, &hand);
        // Its terminal and the branch cut from it went with it, rather than
        // coming back as roots under the git node.
        assert_eq!(plan.cards.len(), 1);
        assert!(plan.tile(7).is_none());
    }

    #[test]
    fn a_maximised_node_is_given_the_room_and_never_less_than_it_had() {
        assert_eq!(
            maximized_size((2000., 1000.), 0.9, (760., 480.)),
            (1800., 900.)
        );
        assert_eq!(
            maximized_size((1000., 500.), 0.9, (2000., 300.)),
            (2000., 450.)
        );
        // At that size, the focus is at a zoom of one: the node is as big as
        // the screen allows, not magnified.
        let rect = Rect {
            x: 50.,
            y: 60.,
            w: 1800.,
            h: 900.,
        };
        assert_eq!(View::focus(rect, (2000., 1000.), 0.9).zoom, 1.);
    }

    #[test]
    fn maximising_fills_nine_tenths_of_the_screen_around_the_node() {
        let rect = Rect {
            x: 1000.,
            y: 500.,
            w: 400.,
            h: 200.,
        };
        let view = View::focus(rect, (1000., 1000.), 0.9);
        assert_eq!(view.zoom, 2.);
        let shown = view.screen(rect);
        assert_eq!(
            (shown.x + shown.w / 2., shown.y + shown.h / 2.),
            (500., 500.)
        );
        // A tall one: its height is what fills.
        let tall = Rect {
            w: 100.,
            h: 900.,
            ..rect
        };
        assert_eq!(View::focus(tall, (1000., 1000.), 0.9).zoom, 1.);
    }

    #[test]
    fn notes_hang_from_the_git_node_beside_the_main_checkout() {
        let mut groups = [group("/r", vec![checkout("/r", "main", true, None)])];
        groups[0].notes = vec![PathBuf::from("/r/.claudhub/notes/4.md")];
        let plan = plan(&groups, &Hand::default());
        let note = plan.note("/r/.claudhub/notes/4.md").unwrap();
        let main = plan.card(Path::new("/r")).unwrap();
        assert_eq!(note.y, main.y);
        assert_eq!(main.x, note.right() + SIBLING_GAP);
        assert!(plan.links.iter().any(|l| l.kind == LinkKind::Note));
    }

    #[test]
    fn a_cycle_of_guesses_loses_nothing() {
        // No main checkout, and two branches each guessing the other.
        let checkouts = vec![
            checkout("/a", "one", false, Some("two")),
            checkout("/b", "two", false, Some("one")),
        ];
        let plan = plan(&[group("/r", checkouts)], &Hand::default());
        assert_eq!(plan.cards.len(), 2);
    }

    #[test]
    fn repositories_stand_side_by_side() {
        let plan = plan(
            &[
                group("/a", vec![checkout("/a", "main", true, None)]),
                group("/b", vec![checkout("/b", "main", true, None)]),
            ],
            &Hand::default(),
        );
        assert_eq!(plan.gits[1].rect.x, GIT.0 + GROUP_GAP);
        assert_eq!(plan.gits[1].rect.y, 0.);
    }

    #[test]
    fn a_resize_stops_at_the_smallest_size() {
        assert_eq!(resized((400., 300.), (50., -20.), MIN_TILE), (450., 280.));
        assert_eq!(resized((400., 300.), (-500., -500.), MIN_TILE), MIN_TILE);
        assert_eq!(Tile::nearest((700., 1.)), Tile::Medium);
        assert_eq!(Tile::nearest((10., 1.)), Tile::Small);
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
    fn fitting_centres_everything_and_never_magnifies() {
        let bounds = Rect {
            x: 0.,
            y: 0.,
            w: 2000.,
            h: 1000.,
        };
        let view = View::fit(bounds, (1048., 1048.));
        assert_eq!(view.zoom, 0.5);
        let shown = view.screen(bounds);
        assert_eq!(shown.x + shown.w / 2., 524.);
        assert_eq!(shown.y + shown.h / 2., 524.);
        let small = Rect {
            w: 100.,
            h: 100.,
            ..bounds
        };
        let view = View::fit(small, (1048., 1048.));
        assert_eq!(view.zoom, 1.);
        assert_eq!(view.screen(small).x, 474.);
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
        assert_eq!(thumb((10., 900.), 1000.), None);
        assert_eq!(thumb((0., 2000.), 1000.), Some((0., 500.)));
        assert_eq!(thumb((-1000., 1000.), 1000.), Some((500., 500.)));
        // Panned past the end: the empty room counts — 2500 in all.
        assert_eq!(thumb((-1500., 500.), 1000.), Some((600., 400.)));
        assert_eq!(thumb_ratio((0., 2000.), 1000.), 2.);
    }

    #[test]
    fn the_grid_follows_the_plane_and_never_crowds() {
        // At a zoom of one, every forty pixels, shifted with the pan.
        assert_eq!(grid(10., 1., 100.), vec![10., 50., 90.]);
        assert_eq!(grid(-30., 1., 100.), vec![10., 50., 90.]);
        // Zoomed out a lot, the step doubles until it is readable: 4, 8,
        // 16, then 32 — the first not under the floor.
        let far = grid(0., 0.1, 200.);
        assert_eq!(far[1] - far[0], 32.);
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
