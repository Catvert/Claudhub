//! Where one has been, and how to walk back through it.
//!
//! A jump — a definition followed, a file opened from a diff line, a Sentry
//! frame opened from another screen — is the one movement that loses the place
//! one was reading: the caret leaves for another part of the file, often
//! another file, sometimes another screen, and nothing tells where it came
//! from. Vim answers with `Ctrl+O`, a browser with a back arrow and the fourth
//! mouse button, and all three are the same thing: a trail with a finger on it.
//!
//! **One trail, and not one per subject.** A place is a file *or* a screen
//! (`Place`), because the gesture that crosses from one to the other — reading
//! an error, opening the line it names — is precisely the one that had nowhere
//! to be written down. Two trails would have meant two "back"s that do not do
//! the same thing, and no way to undo a movement that started on one and ended
//! on the other.
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

/// A place one can come back to.
///
/// A document carries **nothing but its name**: what one comes back to in it —
/// the issue a plugin has open, the file list of a review, the hit one was on —
/// is that view's own state, which has not moved while one was away. Copying it
/// in here would be a second, ageing copy of what is already right.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A file, and where the caret was in it.
    Editor(Spot),
    /// A document of the centre, and only that — the diff, a plugin's detail,
    /// the list of hits one jumped to.
    ///
    /// A **document** and not any panel: unfolding a tool window is not a
    /// movement. Showing the history beside what one is reading does not change
    /// where one is, and a back arrow that folded a zone would be one nobody
    /// could predict. See `ClaudhubApp::travel_to_panel`, which asks `rails`
    /// which of the two a name is.
    Panel(&'static str),
    /// A query in the SQL console, on the connection that answered it.
    ///
    /// The exception the rule above earns: the console's state is exactly what
    /// a gesture **replaces** — following a foreign key, opening a table — and
    /// coming back to "the databases screen" would come back to the query that
    /// took the place of the one being read.
    ///
    /// The query is the one that was **sent**, not the text being typed: it is
    /// what produced what was on screen, and it is the only one that can be run
    /// again to put it back. The connection is named by its key, as a session
    /// names it: a trail holds what one chose, and a password is not a thing to
    /// carry a second copy of.
    Query {
        connection: String,
        database: Option<String>,
        sql: String,
    },
}

impl Place {
    /// The file this place names, when it names one. What tells a reopening of
    /// the file already open — which is not a jump — from a real one.
    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            Place::Editor(spot) => Some(&spot.path),
            Place::Panel(_) | Place::Query { .. } => None,
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
    trail: Vec<Place>,
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
    pub fn jump(&mut self, from: Place, to: Place) {
        if self.trail.is_empty() {
            self.trail.push(from);
            self.at = 0;
        } else {
            self.trail[self.at] = from;
            self.trail.truncate(self.at + 1);
            // Two identical places in a row are one place. Going back to a
            // screen by hand — the way one does, `Alt+5` and another frame in
            // the same list — and leaving it again would otherwise write it
            // twice, and one step back out of the second would move nothing on
            // screen. That is the one step nobody can tell from a broken
            // button.
            if self.at > 0 && self.trail[self.at] == self.trail[self.at - 1] {
                self.trail.pop();
                self.at -= 1;
            }
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
    pub fn back(&mut self, here: Place) -> Option<Place> {
        if self.at == 0 || self.trail.is_empty() {
            return None;
        }
        self.trail[self.at] = here;
        self.at -= 1;
        Some(self.trail[self.at].clone())
    }

    /// One step forward, undoing a `back`.
    pub fn forward(&mut self, here: Place) -> Option<Place> {
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

    fn spot(name: &str, offset: usize) -> Place {
        Place::Editor(Spot::new(name, offset))
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

    /// The case the trail exists for since it crosses screens: one reads an
    /// error on Sentry, opens the line it names, and one step back is the
    /// screen — not another place in the file one has just landed in.
    #[test]
    fn a_screen_and_a_file_share_one_trail() {
        let mut jumps = Jumps::default();
        jumps.jump(Place::Panel("ClaudhubDiff"), spot("app/User.php", 400));
        assert_eq!(
            jumps.back(spot("app/User.php", 420)),
            Some(Place::Panel("ClaudhubDiff")),
            "back leaves the editor for the screen one came from"
        );
        // And forward returns to where reading had got to, not to the frame's
        // line: the trail keeps the place one left, in a file as on a screen.
        assert_eq!(
            jumps.forward(Place::Panel("ClaudhubDiff")),
            Some(spot("app/User.php", 420))
        );
    }

    /// Reading a second error on the screen one came back to by hand leaves
    /// one way back to it, not two.
    #[test]
    fn the_same_place_twice_running_is_one_place() {
        let mut jumps = Jumps::default();
        jumps.jump(Place::Panel("ClaudhubDiff"), spot("a.php", 0));
        // Back to Sentry by hand — nothing recorded — then another frame.
        jumps.jump(Place::Panel("ClaudhubDiff"), spot("b.php", 0));
        assert_eq!(
            jumps.back(spot("b.php", 0)),
            Some(Place::Panel("ClaudhubDiff"))
        );
        assert!(!jumps.can_back(), "the screen is on the trail once");
    }

    /// The console's case: following a foreign key replaces the query one was
    /// reading, and one step back is that query — not the databases screen,
    /// which one never left.
    #[test]
    fn a_query_is_a_place_of_its_own() {
        let query = |sql: &str| Place::Query {
            connection: "sqlite:/tmp/app.sqlite".into(),
            database: None,
            sql: sql.into(),
        };
        let mut jumps = Jumps::default();
        jumps.jump(
            query("SELECT * FROM orders"),
            query("SELECT * FROM users WHERE id = 7"),
        );
        assert_eq!(
            jumps.back(query("SELECT * FROM users WHERE id = 7")),
            Some(query("SELECT * FROM orders"))
        );
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
