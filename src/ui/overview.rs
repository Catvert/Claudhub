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
    /// The node it leads to: what says an agent is at work at its end.
    pub child: Node,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    /// The git nodes, by main path.
    pub gits: Vec<Card>,
    pub cards: Vec<Card>,
    pub tiles: Vec<Placed>,
    pub notes: Vec<Card>,
    pub links: Vec<Link>,
    /// Every node with its place and its parent, parents before their
    /// children: the order `hold` walks.
    pub order: Vec<(Node, Rect, Option<Node>)>,
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
        if hand.hidden.contains(&node) {
            continue;
        }
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
            if hidden.contains(&node) {
                continue;
            }
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
        let mut parent_of: Vec<Option<usize>> = vec![None; nodes.len()];
        for (index, branch) in nodes.iter().enumerate() {
            for &child in &branch.children {
                parent_of[child] = Some(index);
            }
        }
        for (index, branch) in nodes.iter().enumerate() {
            let rect = rects[index];
            plan.order.push((
                branch.node.clone(),
                rect,
                parent_of[index].map(|p| nodes[p].node.clone()),
            ));
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
                    child: nodes[child].node.clone(),
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

/// What an agent says it is doing, as a link shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doing {
    Rest,
    /// A turn under way: the link flows.
    Working,
    /// It asks the user something: the link pulses, and louder than work —
    /// the one state that cannot go on without a hand.
    Waiting,
}

/// A terminal on the plane, as far as its agent goes.
#[derive(Debug, Clone)]
pub struct AgentTile<'a> {
    pub id: u64,
    pub worktree: &'a Path,
    /// It runs an agent: launched as one, or Claude typed at its prompt.
    pub agent: bool,
    /// What its own agent says — Claude's status for its pid, or the hooks'
    /// word for its session — `None` when nothing speaks for this terminal
    /// alone.
    pub word: Option<Doing>,
}

/// Where an agent is at work, or waits, on the plane: the links that move.
/// Nothing at rest is in either map.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AtWork {
    /// The terminals, by id.
    pub terminals: HashMap<u64, Doing>,
    /// The worktrees with no agent terminal on the plane to say so — an
    /// agent started in a terminal of another program: their card's link
    /// moves instead, or the work would not show at all.
    pub cards: HashMap<PathBuf, Doing>,
}

impl AtWork {
    pub fn is_empty(&self) -> bool {
        self.terminals.is_empty() && self.cards.is_empty()
    }

    /// Something flows, and not only pulses: the frames it needs are
    /// closer together.
    pub fn flows(&self) -> bool {
        self.terminals
            .values()
            .chain(self.cards.values())
            .any(|doing| *doing == Doing::Working)
    }
}

/// Which links move, and how, from what the agents say.
///
/// A terminal's own agent decides when it speaks; the worktree's state —
/// `worktrees`, what its agents say together, the loudest of them — only
/// speaks for the agent terminals nothing names, and not when a named one
/// already accounts for it. A shell with no agent in it never moves: the
/// worktree says nothing of a prompt beside it. And a card moves only where
/// no agent terminal is on the plane: one that says it is at rest has said
/// so, and the worktree's state — a guess from the processor, which typing
/// a prompt burns — does not overrule it.
pub fn at_work(tiles: &[AgentTile], worktrees: &HashMap<&Path, Doing>) -> AtWork {
    let accounted = |worktree: &Path| {
        tiles.iter().any(|tile| {
            tile.worktree == worktree && tile.word.is_some_and(|word| word != Doing::Rest)
        })
    };
    let terminals: HashMap<u64, Doing> = tiles
        .iter()
        .filter_map(|tile| {
            let doing = match tile.word {
                Some(word) => word,
                None if tile.agent && !accounted(tile.worktree) => {
                    worktrees.get(tile.worktree).copied().unwrap_or(Doing::Rest)
                }
                None => Doing::Rest,
            };
            (doing != Doing::Rest).then_some((tile.id, doing))
        })
        .collect();
    let cards = worktrees
        .iter()
        .filter(|(worktree, doing)| {
            **doing != Doing::Rest
                && !tiles
                    .iter()
                    .any(|tile| tile.worktree == **worktree && tile.agent)
        })
        .map(|(worktree, doing)| (worktree.to_path_buf(), *doing))
        .collect();
    AtWork { terminals, cards }
}

/// What a multiple choice shows, where nothing picked means everything:
/// the `choices` picked, in their order — or all of them, when none of the
/// picks is still among them. A pick gone from the choices, a worktree
/// removed, does not leave a plane showing nothing.
pub fn shown_of(picked: &[PathBuf], choices: &[PathBuf]) -> Vec<PathBuf> {
    let shown: Vec<PathBuf> = choices
        .iter()
        .filter(|choice| picked.contains(choice))
        .cloned()
        .collect();
    if shown.is_empty() {
        choices.to_vec()
    } else {
        shown
    }
}

/// Pressing one entry of a multiple choice, against what is `shown`: in if
/// it was out, out if it was in — and `None` for the last one in, a plane
/// showing nothing being no choice at all.
pub fn toggle(shown: &[PathBuf], item: &Path) -> Option<Vec<PathBuf>> {
    if shown.iter().any(|path| path == item) {
        let rest: Vec<PathBuf> = shown.iter().filter(|path| *path != item).cloned().collect();
        (!rest.is_empty()).then_some(rest)
    } else {
        let mut more = shown.to_vec();
        more.push(item.to_path_buf());
        Some(more)
    }
}

/// The length of a rounded rectangle's outline, its corners quarter
/// circles.
pub fn outline_length(w: f32, h: f32, radius: f32) -> f32 {
    let r = radius.min(w / 2.).min(h / 2.).max(0.);
    2. * (w + h) - 8. * r + std::f32::consts::TAU * r
}

/// Dashes marching round a closed outline: `dash` and `gap` stretched so a
/// whole number of periods fits, or the seam where the outline closes would
/// show a dash cut short.
pub fn marching_round(length: f32, travelled: f32, dash: f32, gap: f32) -> Vec<(f32, f32)> {
    let target = dash + gap;
    if length <= 0. || target <= 0. {
        return Vec::new();
    }
    let periods = (length / target).round().max(1.);
    let stretch = length / periods / target;
    marching(length, travelled * stretch, dash * stretch, gap * stretch)
}

/// A comet going round a closed outline, `tail` long: its stretch in one
/// piece, or in two across the seam.
pub fn comet_round(length: f32, travelled: f32, tail: f32) -> Vec<(f32, f32)> {
    if length <= 0. || tail <= 0. {
        return Vec::new();
    }
    let tail = tail.min(length);
    let head = travelled.rem_euclid(length);
    if head >= tail {
        vec![(head - tail, head)]
    } else {
        let mut pieces = Vec::new();
        if head > 0. {
            pieces.push((0., head));
        }
        pieces.push((length - (tail - head), length));
        pieces
    }
}

/// A slow breath between 0 and 1, `period` seconds long: what a waiting
/// link pulses by.
pub fn breath(seconds: f32, period: f32) -> f32 {
    if period <= 0. {
        return 0.;
    }
    0.5 - 0.5 * (seconds / period * std::f32::consts::TAU).cos()
}

/// The length of a polyline.
pub fn length(points: &[(f32, f32)]) -> f32 {
    points
        .windows(2)
        .map(|pair| ((pair[1].0 - pair[0].0).powi(2) + (pair[1].1 - pair[0].1).powi(2)).sqrt())
        .sum()
}

/// The stretches of a link of `length` that dashes marching from parent to
/// child light, once they have `travelled` that far: each `dash` long,
/// `gap` apart, clipped to the link.
pub fn marching(length: f32, travelled: f32, dash: f32, gap: f32) -> Vec<(f32, f32)> {
    let period = dash + gap;
    if length <= 0. || period <= 0. {
        return Vec::new();
    }
    let mut start = travelled.rem_euclid(period) - period;
    let mut stretches = Vec::new();
    while start < length {
        let (from, to) = (start.max(0.), (start + dash).min(length));
        if to > from {
            stretches.push((from, to));
        }
        start += period;
    }
    stretches
}

/// The stretch a comet lights, its head having `travelled` that far: it
/// runs from parent to child, `tail` long, and comes back in from the parent
/// once its tail has left — a pause the length of its tail between two runs.
pub fn comet(length: f32, travelled: f32, tail: f32) -> Option<(f32, f32)> {
    if length <= 0. || tail <= 0. {
        return None;
    }
    let head = travelled.rem_euclid(length + tail);
    let (from, to) = ((head - tail).max(0.), head.min(length));
    (to > from).then_some((from, to))
}

/// A dash array — what gpui's stroke takes — that lights exactly these
/// stretches, sorted and apart. gpui has no dash offset and every array
/// starts with a dash: a stretch that does not start at the origin is led
/// by one too short to see. Nothing to light is an empty array, which the
/// caller must not stroke: gpui doubles an odd array, and a lone gap would
/// come back a dash the length of the link.
pub fn dash_array(stretches: &[(f32, f32)]) -> Vec<f32> {
    const INVISIBLE: f32 = 0.01;
    if stretches.is_empty() {
        return Vec::new();
    }
    let mut array = Vec::with_capacity(stretches.len() * 2 + 2);
    let mut at = 0.;
    for &(from, to) in stretches {
        if array.is_empty() && from > INVISIBLE {
            array.push(INVISIBLE);
            at = INVISIBLE;
        }
        if !array.is_empty() {
            array.push((from - at).max(0.));
        }
        array.push(to - from);
        at = to;
    }
    // What follows the last one is dark, however long the link is.
    array.push(f32::MAX / 4.);
    array
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

/// Keeps every node that stood on `before` where it stood, by giving it the
/// offset that takes it back — and returns the nodes it moved.
///
/// **What is on screen stays put when a node comes or goes.** The tree
/// centres a parent over its children, so closing a child moved its parent,
/// and every neighbour of the parent's with it — the hand reached for a node
/// that had just slid away. Offsets are what the hand's own moves are made
/// of, so a node held is a node as if dragged back; and a node added still
/// lands where the tree puts it, beside siblings that did not move.
///
/// **One node at a time, parents first**: an offset carries down the tree,
/// so fixing a parent moves its children, and fixing a child in the same
/// pass would count that move twice.
pub fn hold(groups: &[Group], hand: &mut Hand, before: &Plan) -> Vec<Node> {
    let natural = plan(groups, hand);
    let mut held = Vec::new();
    let mut settle = |hand: &mut Hand, node: &Node, (dx, dy): (f32, f32)| {
        let offset = hand.moved.entry(node.clone()).or_default();
        offset.0 += dx;
        offset.1 += dy;
        if !held.contains(node) {
            held.push(node.clone());
        }
    };
    // What was there: back where it stood.
    for _ in 0..=before.order.len() {
        let now = plan(groups, hand);
        let drifted = now.order.iter().find_map(|(node, rect, _)| {
            let old = before.rect(node)?;
            let (dx, dy) = (old.x - rect.x, old.y - rect.y);
            (dx.abs() > 0.01 || dy.abs() > 0.01).then(|| (node.clone(), (dx, dy)))
        });
        let Some((node, delta)) = drifted else {
            break;
        };
        settle(hand, &node, delta);
    }
    // What is new: where the tree puts it **beside its neighbour** — the
    // sibling before it, or its parent when it is the first — moved by what
    // that neighbour was moved by. Held alone, a new child would inherit its
    // parent's correction and land on its sibling.
    for (index, (node, natural_rect, parent)) in natural.order.iter().enumerate() {
        if before.rect(node).is_some() {
            continue;
        }
        let neighbour = natural.order[..index]
            .iter()
            .rev()
            .find(|(_, _, p)| p == parent)
            .map(|(n, _, _)| n.clone())
            .or_else(|| parent.clone());
        let now = plan(groups, hand);
        let Some(current) = now.rect(node) else {
            continue;
        };
        let correction = neighbour
            .and_then(|n| Some((now.rect(&n)?, natural.rect(&n)?)))
            .map_or((0., 0.), |(held_at, was_at)| {
                (held_at.x - was_at.x, held_at.y - was_at.y)
            });
        let target = (natural_rect.x + correction.0, natural_rect.y + correction.1);
        let (dx, dy) = (target.0 - current.x, target.1 - current.y);
        if dx.abs() > 0.01 || dy.abs() > 0.01 {
            settle(hand, node, (dx, dy));
        }
    }
    held
}

/// True when the plane gained or lost a node — what `hold` answers — and
/// not when it shows other projects, which is a new plane to fit rather
/// than one to keep.
pub fn nodes_changed(before: &Plan, now: &Plan) -> bool {
    let projects = |plan: &Plan| plan.gits.iter().map(|g| g.path.clone()).collect::<Vec<_>>();
    if projects(before) != projects(now) {
        return false;
    }
    let nodes = |plan: &Plan| {
        plan.order
            .iter()
            .map(|(n, _, _)| n.clone())
            .collect::<Vec<_>>()
    };
    nodes(before) != nodes(now)
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

    /// The complaint that made `hold`: closing a child moved its parent.
    #[test]
    fn closing_a_child_leaves_its_parent_and_its_siblings_where_they_were() {
        let mut main = checkout("/r", "main", true, None);
        main.terminals = vec![(7, Tile::Small.size()), (8, Tile::Small.size())];
        let feature = checkout("/r-a", "feature", false, Some("main"));
        let before = plan(
            &[group("/r", vec![main.clone(), feature.clone()])],
            &Hand::default(),
        );

        // Terminal 7 closes.
        main.terminals = vec![(8, Tile::Small.size())];
        let groups = [group("/r", vec![main, feature])];
        let mut hand = Hand::default();
        assert!(nodes_changed(&before, &plan(&groups, &hand)));
        let held = hold(&groups, &mut hand, &before);
        assert!(!held.is_empty());
        let after = plan(&groups, &hand);
        for node in [
            Node::Git(PathBuf::from("/r")),
            Node::Worktree(PathBuf::from("/r")),
            Node::Worktree(PathBuf::from("/r-a")),
            Node::Terminal(8),
        ] {
            assert_eq!(after.rect(&node), before.rect(&node), "{node:?}");
        }
        // Nothing left to hold: a second pass moves nothing.
        assert!(hold(&groups, &mut hand, &before).is_empty());
    }

    #[test]
    fn a_new_child_lands_beside_siblings_that_stay() {
        let mut main = checkout("/r", "main", true, None);
        main.terminals = vec![(7, Tile::Small.size())];
        let before = plan(&[group("/r", vec![main.clone()])], &Hand::default());
        main.terminals.push((8, Tile::Small.size()));
        let groups = [group("/r", vec![main])];
        let mut hand = Hand::default();
        hold(&groups, &mut hand, &before);
        let after = plan(&groups, &hand);
        assert_eq!(after.tile(7), before.tile(7));
        assert_eq!(
            after.rect(&Node::Worktree(PathBuf::from("/r"))),
            before.rect(&Node::Worktree(PathBuf::from("/r")))
        );
        // Beside the first, one gap along, on its line.
        let (first, second) = (after.tile(7).unwrap(), after.tile(8).unwrap());
        assert_eq!(second.y, first.y);
        assert_eq!(second.x, first.right() + SIBLING_GAP);
    }

    #[test]
    fn another_project_is_not_a_change_to_hold() {
        let a = plan(
            &[group("/a", vec![checkout("/a", "main", true, None)])],
            &Hand::default(),
        );
        let b = plan(
            &[group("/b", vec![checkout("/b", "main", true, None)])],
            &Hand::default(),
        );
        assert!(!nodes_changed(&a, &b));
        assert!(!nodes_changed(&a, &a));
    }

    #[test]
    fn a_hidden_note_leaves_the_plane_and_its_neighbours_close_up() {
        let mut main = checkout("/r", "main", true, None);
        main.notes = vec![
            PathBuf::from("/r/.claudhub/notes/a.md"),
            PathBuf::from("/r/.claudhub/notes/b.md"),
        ];
        let mut groups = [group("/r", vec![main])];
        groups[0].notes = vec![PathBuf::from("/r/.claudhub/notes/repo.md")];
        let hand = Hand {
            hidden: HashSet::from([
                Node::Note(PathBuf::from("/r/.claudhub/notes/a.md")),
                Node::Note(PathBuf::from("/r/.claudhub/notes/repo.md")),
            ]),
            ..Hand::default()
        };
        let plan = plan(&groups, &hand);
        assert_eq!(plan.notes.len(), 1);
        assert!(plan.note("/r/.claudhub/notes/b.md").is_some());
        // The one left stands centred under its card, alone.
        let card = plan.card(Path::new("/r")).unwrap();
        let note = plan.note("/r/.claudhub/notes/b.md").unwrap();
        assert_eq!(note.x + note.w / 2., card.x + card.w / 2.);
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

    fn tile(id: u64, worktree: &str, agent: bool, word: Option<Doing>) -> AgentTile<'_> {
        AgentTile {
            id,
            worktree: Path::new(worktree),
            agent,
            word,
        }
    }

    fn doing(pairs: &[(&'static str, Doing)]) -> HashMap<&'static Path, Doing> {
        pairs
            .iter()
            .map(|&(path, doing)| (Path::new(path), doing))
            .collect()
    }

    #[test]
    fn a_terminal_moves_by_its_own_word_first() {
        // Three agents in one worktree, each named: the one at work flows,
        // the one asking pulses, the one done stays still — whatever the
        // worktree's state, the loudest of the three, says.
        let worktrees = doing(&[("/a", Doing::Waiting)]);
        let tiles = [
            tile(1, "/a", true, Some(Doing::Working)),
            tile(2, "/a", true, Some(Doing::Rest)),
            tile(3, "/a", true, Some(Doing::Waiting)),
        ];
        let found = at_work(&tiles, &worktrees);
        assert_eq!(
            found.terminals,
            HashMap::from([(1, Doing::Working), (3, Doing::Waiting)])
        );
        assert!(found.cards.is_empty());
        assert!(found.flows());
    }

    #[test]
    fn an_unnamed_agent_terminal_takes_its_worktree_s_state() {
        let worktrees = doing(&[("/a", Doing::Waiting)]);
        let tiles = [
            tile(1, "/a", true, None),
            // A shell beside it has no agent in it, and does not move.
            tile(2, "/a", false, None),
            // Nor does an agent of a worktree at rest.
            tile(3, "/b", true, None),
        ];
        let found = at_work(&tiles, &worktrees);
        assert_eq!(found.terminals, HashMap::from([(1, Doing::Waiting)]));
        // Only a pulse: frames can be further apart.
        assert!(!found.flows());
        // A named terminal already accounts for the worktree: the unnamed one
        // beside it is not lit by the same state twice.
        let tiles = [
            tile(1, "/a", true, None),
            tile(4, "/a", true, Some(Doing::Waiting)),
        ];
        let found = at_work(&tiles, &worktrees);
        assert_eq!(found.terminals, HashMap::from([(4, Doing::Waiting)]));
    }

    #[test]
    fn work_with_no_terminal_on_the_plane_moves_the_card() {
        let worktrees = doing(&[("/a", Doing::Working), ("/b", Doing::Waiting)]);
        let tiles = [tile(1, "/a", true, None), tile(2, "/b", false, None)];
        let found = at_work(&tiles, &worktrees);
        assert_eq!(found.terminals, HashMap::from([(1, Doing::Working)]));
        assert_eq!(
            found.cards,
            HashMap::from([(PathBuf::from("/b"), Doing::Waiting)])
        );
    }

    #[test]
    fn an_agent_that_says_it_rests_is_not_overruled_by_the_guess() {
        // Typing a prompt burns processor, so the worktree is guessed at
        // work; the agent itself says it is idle, and nothing moves — not
        // its link, not the card's in its place.
        let worktrees = doing(&[("/a", Doing::Working)]);
        let found = at_work(&[tile(1, "/a", true, Some(Doing::Rest))], &worktrees);
        assert!(found.is_empty());
    }

    #[test]
    fn a_breath_goes_from_nothing_to_full_and_back() {
        assert!(breath(0., 2.).abs() < 1e-6);
        assert!((breath(1., 2.) - 1.).abs() < 1e-6);
        assert!(breath(2., 2.).abs() < 1e-5);
        assert_eq!(breath(1., 0.), 0.);
    }

    #[test]
    fn dashes_march_toward_the_child() {
        // Dash 4, gap 6, on a link of 20: at rest, dashes at 0 and 10.
        assert_eq!(marching(20., 0., 4., 6.), vec![(0., 4.), (10., 14.)]);
        // Three further on, everything has moved three toward the child.
        assert_eq!(marching(20., 3., 4., 6.), vec![(3., 7.), (13., 17.)]);
        // Eight on, a dash comes in from the parent and one leaves at the
        // child.
        assert_eq!(
            marching(20., 8., 4., 6.),
            vec![(0., 2.), (8., 12.), (18., 20.)]
        );
        // A whole period on, the same picture.
        assert_eq!(marching(20., 10., 4., 6.), marching(20., 0., 4., 6.));
        // Clipped at the child.
        assert_eq!(marching(12., 0., 4., 6.), vec![(0., 4.), (10., 12.)]);
        assert!(marching(0., 0., 4., 6.).is_empty());
    }

    #[test]
    fn a_comet_runs_the_link_and_comes_back() {
        assert_eq!(comet(100., 0., 10.), None);
        assert_eq!(comet(100., 5., 10.), Some((0., 5.)));
        assert_eq!(comet(100., 50., 10.), Some((40., 50.)));
        // Its head gone past the child, its tail still in.
        assert_eq!(comet(100., 105., 10.), Some((95., 100.)));
        // The whole run is the link and the tail, and it starts over.
        assert_eq!(comet(100., 115., 10.), Some((0., 5.)));
    }

    #[test]
    fn a_dash_array_lights_exactly_the_stretches() {
        // Replays the array the way gpui walks it: dash, gap, dash…
        fn lit(array: &[f32], length: f32) -> Vec<(f32, f32)> {
            let mut at = 0.;
            let mut out = Vec::new();
            for (index, step) in array.iter().enumerate() {
                let next = (at + step).min(length);
                if index % 2 == 0 && next > at {
                    out.push((at, next));
                }
                at = next;
                if at >= length {
                    break;
                }
            }
            out
        }
        let stretches = [(0., 4.), (10., 14.)];
        let array = dash_array(&stretches);
        assert_eq!(array.len() % 2, 0);
        assert_eq!(lit(&array, 20.), stretches.to_vec());
        // Not from the origin: led by a dash nobody sees.
        let array = dash_array(&[(5., 8.)]);
        assert_eq!(array.len() % 2, 0);
        let lit = lit(&array, 20.);
        assert_eq!(lit.len(), 2);
        assert!(lit[0].1 <= 0.01);
        assert_eq!(lit[1], (5., 8.));
        assert!(dash_array(&[]).is_empty());
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn nothing_picked_shows_every_choice() {
        let choices = paths(&["/a", "/b", "/c"]);
        assert_eq!(shown_of(&[], &choices), choices);
        // In the choices' order, not the picks'.
        assert_eq!(
            shown_of(&paths(&["/c", "/a"]), &choices),
            paths(&["/a", "/c"])
        );
        // A pick gone from the choices is not a plane showing nothing.
        assert_eq!(shown_of(&paths(&["/gone"]), &choices), choices);
    }

    #[test]
    fn pressing_an_entry_turns_it_in_or_out_but_never_the_last() {
        let shown = paths(&["/a", "/b"]);
        assert_eq!(toggle(&shown, Path::new("/b")), Some(paths(&["/a"])));
        assert_eq!(
            toggle(&shown, Path::new("/c")),
            Some(paths(&["/a", "/b", "/c"]))
        );
        assert_eq!(toggle(&paths(&["/a"]), Path::new("/a")), None);
    }

    #[test]
    fn dashes_round_an_outline_close_on_a_whole_period() {
        // A square of 40 without corners: 160 round. A period of 15 asked
        // is eleven of them, stretched to 160 / 11.
        let length = outline_length(40., 40., 0.);
        assert_eq!(length, 160.);
        let dashes = marching_round(length, 0., 6., 9.);
        assert_eq!(dashes.len(), 11);
        let period = 160. / 11.;
        assert!((dashes[1].0 - period).abs() < 1e-3);
        // The last one ends a gap before the seam, where the first starts.
        let last = dashes[dashes.len() - 1];
        assert!((160. - last.1 - (period - (last.1 - last.0))).abs() < 1e-3);
        // Rounded corners take their quarter circles off the straight sides.
        let round = outline_length(40., 40., 5.);
        assert!((round - (160. - 40. + std::f32::consts::TAU * 5.)).abs() < 1e-3);
    }

    #[test]
    fn a_comet_round_an_outline_crosses_the_seam_in_two_pieces() {
        assert_eq!(comet_round(100., 50., 10.), vec![(40., 50.)]);
        assert_eq!(comet_round(100., 104., 10.), vec![(0., 4.), (94., 100.)]);
        assert_eq!(comet_round(100., 100., 10.), vec![(90., 100.)]);
        // Longer than the outline, it is the outline.
        assert_eq!(comet_round(20., 5., 50.), vec![(0., 5.), (5., 20.)]);
    }
}
