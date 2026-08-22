//! Where one has been in the editor, and how to walk back through it.
//!
//! A jump — a definition followed, a file opened from a diff line — is the one
//! movement that loses the place one was reading: the caret leaves for another
//! part of the file, often another file, and nothing on screen says where it
//! came from. Vim answers with `Ctrl+O`, a browser with a back arrow, and both
//! are the same thing: a trail with a finger on it.
//!
//! **It knows nothing of gpui**, like `motion.rs` and `notes.rs` before it: it
//! is given a place and gives one back, which is what makes the awkward part —
//! what a new jump does to the trail one has walked back through — a thing to
//! test rather than a thing to watch.

use std::path::PathBuf;

/// How many places are kept. Vim keeps a hundred; the number matters less than
/// having one, an unbounded trail being a file path per jump for a session that
/// lasts a day.
const MAX: usize = 64;

/// A place in a worktree: a file, and where the caret was in it.
///
/// A byte offset and not a line: it is what the editor hands out and takes
/// back, and a line would have to be converted twice for the same answer. It
/// ages badly if the file is rewritten while one is away — the caret lands a
/// few characters off — which is the price of not holding the text itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spot {
    pub path: PathBuf,
    pub offset: usize,
}

impl Spot {
    pub fn new(path: impl Into<PathBuf>, offset: usize) -> Self {
        Self {
            path: path.into(),
            offset,
        }
    }
}

/// The trail of one worktree, and the finger on it.
///
/// `trail[at]` is where one stands. An empty trail is a worktree in which
/// nothing has jumped yet, which is not the same as a trail whose finger is at
/// the start: the first has nothing to go back to and never will until a jump
/// happens.
#[derive(Debug, Default)]
pub struct Jumps {
    trail: Vec<Spot>,
    at: usize,
}

impl Jumps {
    /// Records a jump from `from` to `to`.
    ///
    /// `from` is passed live rather than taken from the trail: the caret has
    /// almost always moved since one landed here — one reads a few lines before
    /// following a definition — and going back to where one arrived instead of
    /// where one left is the difference between a trail and a bookmark list.
    ///
    /// Everything ahead of the finger is dropped, as a browser drops its
    /// forward history when one follows a new link from the middle of it. Two
    /// futures cannot both be the next place.
    pub fn jump(&mut self, from: Spot, to: Spot) {
        if self.trail.is_empty() {
            self.trail.push(from);
            self.at = 0;
        } else {
            self.trail[self.at] = from;
            self.trail.truncate(self.at + 1);
        }
        self.trail.push(to);
        self.at = self.trail.len() - 1;
        if self.trail.len() > MAX {
            let extra = self.trail.len() - MAX;
            self.trail.drain(..extra);
            self.at -= extra;
        }
    }

    /// One step back, `here` being where the caret stands now.
    ///
    /// It is written into the trail before moving so that the way forward comes
    /// back to the character one left, and not to the one the jump had landed
    /// on.
    pub fn back(&mut self, here: Spot) -> Option<Spot> {
        if self.at == 0 || self.trail.is_empty() {
            return None;
        }
        self.trail[self.at] = here;
        self.at -= 1;
        Some(self.trail[self.at].clone())
    }

    /// One step forward, undoing a `back`.
    pub fn forward(&mut self, here: Spot) -> Option<Spot> {
        if self.at + 1 >= self.trail.len() {
            return None;
        }
        self.trail[self.at] = here;
        self.at += 1;
        Some(self.trail[self.at].clone())
    }

    /// Whether the buttons have anything to do — the only thing the view asks.
    pub fn can_back(&self) -> bool {
        self.at > 0
    }

    pub fn can_forward(&self) -> bool {
        self.at + 1 < self.trail.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spot(name: &str, offset: usize) -> Spot {
        Spot::new(name, offset)
    }

    #[test]
    fn a_jump_records_where_it_came_from() {
        let mut jumps = Jumps::default();
        assert!(!jumps.can_back(), "nothing has happened yet");
        jumps.jump(spot("a.php", 10), spot("b.php", 0));
        assert!(jumps.can_back());
        assert!(!jumps.can_forward());
        assert_eq!(jumps.back(spot("b.php", 5)), Some(spot("a.php", 10)));
        assert!(!jumps.can_back());
    }

    /// Going back and forward again lands on the character one left, not on the
    /// one the jump had arrived at.
    #[test]
    fn the_way_forward_remembers_where_one_had_got_to() {
        let mut jumps = Jumps::default();
        jumps.jump(spot("a.php", 10), spot("b.php", 0));
        jumps.back(spot("b.php", 200));
        assert_eq!(jumps.forward(spot("a.php", 12)), Some(spot("b.php", 200)));
        // And the way back has kept the reading position of the first file too.
        assert_eq!(jumps.back(spot("b.php", 200)), Some(spot("a.php", 12)));
    }

    #[test]
    fn a_new_jump_drops_what_lay_ahead() {
        let mut jumps = Jumps::default();
        jumps.jump(spot("a.php", 0), spot("b.php", 0));
        jumps.jump(spot("b.php", 0), spot("c.php", 0));
        jumps.back(spot("c.php", 0));
        assert!(jumps.can_forward(), "c is still ahead");
        jumps.jump(spot("b.php", 0), spot("d.php", 0));
        assert!(!jumps.can_forward(), "c is not the next place any more");
        assert_eq!(jumps.back(spot("d.php", 0)), Some(spot("b.php", 0)));
    }

    #[test]
    fn the_trail_stops_at_both_ends() {
        let mut jumps = Jumps::default();
        jumps.jump(spot("a.php", 0), spot("b.php", 0));
        assert_eq!(jumps.forward(spot("b.php", 0)), None);
        assert!(jumps.back(spot("b.php", 0)).is_some());
        assert_eq!(jumps.back(spot("a.php", 0)), None);
    }

    /// The oldest places go, and the finger goes with them: dropping from the
    /// front without moving it would make every step back land one place off.
    #[test]
    fn the_trail_is_bounded() {
        let mut jumps = Jumps::default();
        for i in 0..MAX * 2 {
            jumps.jump(spot("a.php", i), spot("a.php", i + 1));
        }
        assert_eq!(jumps.trail.len(), MAX);
        assert_eq!(jumps.at, MAX - 1);
        let mut last = spot("a.php", MAX * 2);
        for _ in 0..MAX - 1 {
            last = jumps.back(last).expect("the whole trail walks back");
        }
        assert!(!jumps.can_back());
    }
}
