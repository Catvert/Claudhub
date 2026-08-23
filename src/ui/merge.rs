//! The three-way merge: what each side did to the base, and what comes out.
//!
//! A conflicted file is three files — the common ancestor and the two versions
//! that grew out of it — and git leaves the working tree holding a fourth: the
//! same text with markers cut through it. Resolving by hand means reading the
//! markers and deciding, which is the one gesture an editor cannot help with,
//! because the markers say *what disagrees* and never *what each side did*.
//!
//! So the file is read back from the index (`:1:`, `:2:`, `:3:`) and compared
//! here, twice: the base against ours, the base against theirs. What both
//! comparisons leave untouched is stable; what only one of them touched is
//! **taken without asking** — that is the whole of "smart", and it is what
//! leaves a handful of decisions in a file where git printed twenty markers;
//! what both touched, differently, is a conflict, and the only thing the view
//! has to ask about.
//!
//! No gpui in here, and none wanted. What goes wrong in a merge goes wrong
//! quietly — a chunk taken from the wrong side reads perfectly well — so the
//! decision lives in front of the view that paints it, and it is tested.

use crate::ui::hunks;

/// Which of the two versions a chunk is taken from.
///
/// "Ours" in the user's sense — the branch one is standing on. The translation
/// into git's stages, which swap over during a rebase, happens in the git
/// layer, as it does for `repo::resolve`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Ours,
    Theirs,
}

/// What became of one run of lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Neither side touched it.
    Stable,
    /// Only we touched it: taken, and no question asked.
    Ours,
    /// Only they touched it.
    Theirs,
    /// Both did the same thing. Taken too — agreement is not a conflict.
    Both,
    /// Both touched it, differently. The only kind that carries buttons.
    Conflict,
}

impl Kind {
    /// Whether the chunk is waiting on somebody.
    pub fn is_conflict(self) -> bool {
        self == Kind::Conflict
    }
}

/// One run of lines, in all three versions of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub kind: Kind,
    pub base: Vec<String>,
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
    /// For a conflict, the sides picked **in the order they were picked**: an
    /// empty list is a conflict nobody has answered, and two entries are
    /// "both", which is a real answer and a common one — two functions added at
    /// the same place, and the file wants them both.
    pub choice: Vec<Side>,
    /// What was typed into the middle column for this chunk, which beats
    /// everything else.
    ///
    /// A merge that comes out of two buttons is the common case and not the
    /// only one: two sides that each added a parameter want one line carrying
    /// both, and no combination of "take this side" writes it. Typing it is
    /// **an answer like the others** — a conflict with text in it is settled —
    /// and it is kept per chunk rather than as one buffer over the whole file,
    /// which is what keeps the three columns aligned.
    pub manual: Option<Vec<String>>,
}

impl Chunk {
    /// The lines this chunk contributes to the result, as it stands.
    pub fn result(&self) -> Vec<String> {
        if let Some(manual) = &self.manual {
            return manual.clone();
        }
        match self.kind {
            Kind::Stable => self.base.clone(),
            Kind::Ours | Kind::Both => self.ours.clone(),
            Kind::Theirs => self.theirs.clone(),
            // A conflict answers with nothing until it is answered: an empty
            // middle column is what makes an unresolved chunk impossible to
            // walk past, where a middle showing one of the two sides would read
            // as a decision already made.
            Kind::Conflict => self.picked(),
        }
    }

    /// The lines of a resolved conflict, in the order the sides were picked.
    fn picked(&self) -> Vec<String> {
        self.choice
            .iter()
            .flat_map(|side| match side {
                Side::Ours => self.ours.clone(),
                Side::Theirs => self.theirs.clone(),
            })
            .collect()
    }

    pub fn resolved(&self) -> bool {
        !self.kind.is_conflict() || !self.choice.is_empty() || self.manual.is_some()
    }

    pub fn takes(&self, side: Side) -> bool {
        self.choice.contains(&side)
    }

    /// Whether this chunk's outcome was typed rather than picked.
    pub fn edited(&self) -> bool {
        self.manual.is_some()
    }
}

/// A conflicted file, as three versions and the decisions taken so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merge {
    pub chunks: Vec<Chunk>,
}

/// One line of one column, with the number it carries in its own version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// One-based, in the side this line belongs to.
    pub number: usize,
    pub text: String,
}

/// One row of the three-column view: what each column shows on that line.
///
/// The three columns are aligned chunk by chunk and padded with nothing, which
/// is what makes a chunk readable across the three — a row is a row of the
/// *merge*, not of any one of the files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub chunk: usize,
    pub kind: Kind,
    /// The chunk's first row: where its buttons go, and nowhere else.
    pub first: bool,
    /// Its chunk's outcome was typed rather than picked.
    pub edited: bool,
    pub ours: Option<Line>,
    pub result: Option<Line>,
    pub theirs: Option<Line>,
}

/// Splits the way the comparison counts lines: the trailing empty part **is**
/// the final newline and takes part like any other line, so a side that only
/// adds the missing newline at the end still shows up as a change.
fn split(text: &str) -> Vec<&str> {
    text.split('\n').collect()
}

fn owned(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_string()).collect()
}

/// For each base line, where the other side has it — `None` when that side
/// changed it away.
fn matched(base: &[&str], side: &[&str]) -> Vec<Option<usize>> {
    let mut map = vec![None; base.len()];
    let (mut b, mut s) = (0usize, 0usize);
    for (x, y) in hunks::regions(base, side) {
        while b < x.start {
            map[b] = Some(s);
            b += 1;
            s += 1;
        }
        b = x.end;
        s = y.end;
    }
    while b < base.len() {
        map[b] = Some(s);
        b += 1;
        s += 1;
    }
    map
}

impl Merge {
    /// Compares the three versions and lays out the chunks.
    pub fn new(base: &str, ours: &str, theirs: &str) -> Self {
        let (base, ours, theirs) = (split(base), split(ours), split(theirs));
        let ours_at = matched(&base, &ours);
        let theirs_at = matched(&base, &theirs);
        let mut chunks: Vec<Chunk> = Vec::new();
        let (mut i, mut o, mut t) = (0usize, 0usize, 0usize);

        while i < base.len() || o < ours.len() || t < theirs.len() {
            // A stable run: base lines both sides still have, in step with
            // where both sides have got to. Being in step is the point — a line
            // that reappears further down is not the same line.
            let mut run = 0;
            while i + run < base.len()
                && ours_at[i + run] == Some(o + run)
                && theirs_at[i + run] == Some(t + run)
            {
                run += 1;
            }
            if run > 0 {
                push(&mut chunks, Kind::Stable, &base[i..i + run], &[], &[]);
                i += run;
                o += run;
                t += run;
                continue;
            }

            // Otherwise: everything up to the next line the three agree on.
            let (ni, no, nt) = sync(&base, &ours, &theirs, &ours_at, &theirs_at, i, o, t);
            let (b, u, h) = (&base[i..ni], &ours[o..no], &theirs[t..nt]);
            let kind = if u == h {
                // Both wrote the same thing — including both deleting it.
                if u == b {
                    Kind::Stable
                } else {
                    Kind::Both
                }
            } else if u == b {
                Kind::Theirs
            } else if h == b {
                Kind::Ours
            } else {
                Kind::Conflict
            };
            push(&mut chunks, kind, b, u, h);
            i = ni;
            o = no;
            t = nt;
        }

        Merge { chunks }
    }

    /// How many chunks are still waiting on a decision.
    pub fn unresolved(&self) -> usize {
        self.chunks.iter().filter(|c| !c.resolved()).count()
    }

    pub fn conflicts(&self) -> usize {
        self.chunks.iter().filter(|c| c.kind.is_conflict()).count()
    }

    /// Takes a side into a conflict, or takes it back out.
    ///
    /// A toggle and not a setter: taking both sides is clicking both buttons,
    /// and changing one's mind is clicking the one that is lit. The order is
    /// kept, so "theirs then ours" is reachable and says so on screen.
    pub fn toggle(&mut self, chunk: usize, side: Side) {
        let Some(chunk) = self.chunks.get_mut(chunk) else {
            return;
        };
        if !chunk.kind.is_conflict() {
            return;
        }
        // Picking a side is answering the question again, so what was typed
        // goes: two answers for one chunk, one of them invisible, is the state
        // in which one resolves a file into something one has not read.
        chunk.manual = None;
        match chunk.choice.iter().position(|s| *s == side) {
            Some(at) => {
                chunk.choice.remove(at);
            }
            None => chunk.choice.push(side),
        }
    }

    /// Answers every open conflict with the same side — the gesture of a file
    /// one has already decided about as a whole.
    pub fn take_all(&mut self, side: Side) {
        for chunk in &mut self.chunks {
            if chunk.kind.is_conflict() && chunk.manual.is_none() {
                chunk.choice = vec![side];
            }
        }
    }

    /// The merged file, exactly as it would be written.
    pub fn text(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        for chunk in &self.chunks {
            lines.extend(chunk.result());
        }
        lines.join("\n")
    }

    /// What the middle column of a chunk says, for an editor to start from.
    pub fn resolution(&self, chunk: usize) -> String {
        self.chunks
            .get(chunk)
            .map(|chunk| chunk.result().join("\n"))
            .unwrap_or_default()
    }

    /// Takes what was typed into a chunk's middle column.
    ///
    /// **Empty is not one empty line**: a document splits into lines on `\n`,
    /// so `""` would be a blank line in the file. Clearing the editor means
    /// taking nothing here, which is a real answer — it is how one drops a
    /// chunk both sides argue about.
    pub fn set_manual(&mut self, chunk: usize, text: &str) {
        let Some(chunk) = self.chunks.get_mut(chunk) else {
            return;
        };
        chunk.manual = Some(if text.is_empty() {
            Vec::new()
        } else {
            text.split('\n').map(|line| line.to_string()).collect()
        });
    }

    /// Gives a chunk back to the buttons.
    pub fn clear_manual(&mut self, chunk: usize) {
        if let Some(chunk) = self.chunks.get_mut(chunk) {
            chunk.manual = None;
        }
    }

    /// The conflicts, in file order: what the arrows walk.
    pub fn conflict_chunks(&self) -> Vec<usize> {
        self.chunks
            .iter()
            .enumerate()
            .filter(|(_, chunk)| chunk.kind.is_conflict())
            .map(|(index, _)| index)
            .collect()
    }

    /// The conflict one step away from `from`, wrapping round.
    ///
    /// Wrapping rather than stopping at the ends: what one walks here is a
    /// handful of places in one file, and the gesture is "show me the next one"
    /// — a button that stops answering at the last conflict reads as broken.
    pub fn step(&self, from: usize, delta: isize) -> Option<usize> {
        let conflicts = self.conflict_chunks();
        if conflicts.is_empty() {
            return None;
        }
        let at = match conflicts.binary_search(&from) {
            Ok(at) => (at as isize + delta).rem_euclid(conflicts.len() as isize) as usize,
            // Standing outside a conflict — on the chunk one has just left, or
            // nowhere yet: the nearest one in the direction of travel.
            Err(next) => {
                if delta > 0 {
                    next % conflicts.len()
                } else {
                    (next + conflicts.len() - 1) % conflicts.len()
                }
            }
        };
        conflicts.get(at).copied()
    }

    /// The three columns, aligned.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let (mut o, mut t, mut r) = (0usize, 0usize, 0usize);
        for (index, chunk) in self.chunks.iter().enumerate() {
            let ours = &chunk.ours_lines();
            let theirs = &chunk.theirs_lines();
            let result = chunk.result();
            // At least one row, always: a chunk with nothing on any side — a
            // conflict where each deleted a different run, answered with
            // neither — still has to carry its buttons somewhere.
            let height = [ours.len(), theirs.len(), result.len()]
                .into_iter()
                .max()
                .unwrap_or(0)
                .max(1);
            for line in 0..height {
                rows.push(Row {
                    chunk: index,
                    kind: chunk.kind,
                    first: line == 0,
                    edited: chunk.edited(),
                    ours: ours.get(line).map(|text| numbered(&mut o, text)),
                    result: result.get(line).map(|text| numbered(&mut r, text)),
                    theirs: theirs.get(line).map(|text| numbered(&mut t, text)),
                });
            }
        }
        rows
    }
}

impl Chunk {
    /// What the left column shows: our version, whatever the chunk's kind. A
    /// stable run is the same text on all three sides, and showing it there is
    /// what lets the eye read across.
    fn ours_lines(&self) -> Vec<String> {
        match self.kind {
            Kind::Stable => self.base.clone(),
            Kind::Theirs => self.base.clone(),
            _ => self.ours.clone(),
        }
    }

    fn theirs_lines(&self) -> Vec<String> {
        match self.kind {
            Kind::Stable => self.base.clone(),
            Kind::Ours => self.base.clone(),
            Kind::Both => self.ours.clone(),
            _ => self.theirs.clone(),
        }
    }
}

fn numbered(counter: &mut usize, text: &str) -> Line {
    *counter += 1;
    Line {
        number: *counter,
        text: text.to_string(),
    }
}

fn push(chunks: &mut Vec<Chunk>, kind: Kind, base: &[&str], ours: &[&str], theirs: &[&str]) {
    // A stable run that follows a stable run is the same run: the walk emits
    // one per pass, and a chunk boundary the view would draw across nothing is
    // a boundary that lies.
    if kind == Kind::Stable {
        if let Some(last) = chunks.last_mut() {
            if last.kind == Kind::Stable {
                last.base.extend(owned(base));
                return;
            }
        }
    }
    chunks.push(Chunk {
        kind,
        base: owned(base),
        ours: owned(ours),
        theirs: owned(theirs),
        choice: Vec::new(),
        manual: None,
    });
}

/// The next base line the three versions agree on, and where it sits on each
/// side. The end of all three when there is none left.
#[allow(clippy::too_many_arguments)]
fn sync(
    base: &[&str],
    ours: &[&str],
    theirs: &[&str],
    ours_at: &[Option<usize>],
    theirs_at: &[Option<usize>],
    from: usize,
    o: usize,
    t: usize,
) -> (usize, usize, usize) {
    for i in from..base.len() {
        let (Some(oi), Some(ti)) = (ours_at[i], theirs_at[i]) else {
            continue;
        };
        if oi >= o && ti >= t {
            return (i, oi, ti);
        }
    }
    (base.len(), ours.len(), theirs.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merge(base: &str, ours: &str, theirs: &str) -> Merge {
        Merge::new(base, ours, theirs)
    }

    fn kinds(m: &Merge) -> Vec<Kind> {
        m.chunks.iter().map(|c| c.kind).collect()
    }

    #[test]
    fn a_change_only_one_side_made_is_taken_without_asking() {
        let m = merge("a\nb\nc\n", "a\nB\nc\n", "a\nb\nc\n");
        assert_eq!(m.unresolved(), 0);
        assert_eq!(m.text(), "a\nB\nc\n");
        assert!(kinds(&m).contains(&Kind::Ours));
    }

    #[test]
    fn both_sides_editing_different_places_merge_on_their_own() {
        let m = merge("a\nb\nc\nd\ne\n", "A\nb\nc\nd\ne\n", "a\nb\nc\nd\nE\n");
        assert_eq!(m.unresolved(), 0);
        assert_eq!(m.text(), "A\nb\nc\nd\nE\n");
    }

    #[test]
    fn the_same_change_on_both_sides_is_not_a_conflict() {
        let m = merge("a\nb\n", "a\nB\n", "a\nB\n");
        assert_eq!(m.conflicts(), 0);
        assert_eq!(m.text(), "a\nB\n");
    }

    #[test]
    fn two_edits_to_the_same_line_are_a_conflict_and_the_file_waits() {
        let mut m = merge("a\nb\nc\n", "a\nMINE\nc\n", "a\nTHEIRS\nc\n");
        assert_eq!(m.conflicts(), 1);
        assert_eq!(m.unresolved(), 1);
        // Unanswered, the middle column holds nothing at all.
        assert_eq!(m.text(), "a\nc\n");

        let at = m.chunks.iter().position(|c| c.kind.is_conflict()).unwrap();
        m.toggle(at, Side::Ours);
        assert_eq!(m.text(), "a\nMINE\nc\n");
        assert_eq!(m.unresolved(), 0);
    }

    #[test]
    fn both_sides_can_be_taken_and_the_order_is_the_order_of_the_clicks() {
        let mut m = merge("a\nb\nc\n", "a\nMINE\nc\n", "a\nTHEIRS\nc\n");
        let at = m.chunks.iter().position(|c| c.kind.is_conflict()).unwrap();
        m.toggle(at, Side::Theirs);
        m.toggle(at, Side::Ours);
        assert_eq!(m.text(), "a\nTHEIRS\nMINE\nc\n");
        // And clicking a lit button takes it back out.
        m.toggle(at, Side::Theirs);
        assert_eq!(m.text(), "a\nMINE\nc\n");
    }

    #[test]
    fn one_side_deleting_what_the_other_rewrote_is_a_conflict() {
        let m = merge("a\nb\nc\n", "a\nc\n", "a\nB\nc\n");
        assert_eq!(m.conflicts(), 1);
    }

    #[test]
    fn take_all_answers_every_open_conflict() {
        let mut m = merge("a\nb\nc\nd\n", "A\nb\nC\nd\n", "X\nb\nY\nd\n");
        assert_eq!(m.unresolved(), 2);
        m.take_all(Side::Theirs);
        assert_eq!(m.unresolved(), 0);
        assert_eq!(m.text(), "X\nb\nY\nd\n");
    }

    #[test]
    fn a_file_neither_side_touched_comes_back_verbatim() {
        let text = "a\nb\nc\n";
        assert_eq!(merge(text, text, text).text(), text);
    }

    #[test]
    fn the_final_newline_is_a_line_like_any_other() {
        // Ours adds the missing newline, theirs leaves it alone: a one-sided
        // change, taken without asking, and it must survive the round trip.
        let m = merge("a\nb", "a\nb\n", "a\nb");
        assert_eq!(m.unresolved(), 0);
        assert_eq!(m.text(), "a\nb\n");
    }

    #[test]
    fn a_file_born_on_both_sides_conflicts_whole() {
        let m = merge("", "mine\n", "theirs\n");
        assert_eq!(m.conflicts(), 1);
    }

    #[test]
    fn what_is_typed_into_a_chunk_settles_it_and_lands_in_the_file() {
        let mut m = merge("a\nb\nc\n", "a\nMINE\nc\n", "a\nTHEIRS\nc\n");
        let at = m.chunks.iter().position(|c| c.kind.is_conflict()).unwrap();
        // What the editor starts from is what the middle column says now.
        m.toggle(at, Side::Ours);
        assert_eq!(m.resolution(at), "MINE");
        m.set_manual(at, "MINE and THEIRS");
        assert_eq!(m.unresolved(), 0);
        assert_eq!(m.text(), "a\nMINE and THEIRS\nc\n");
        // And picking a side afterwards is answering again: the typed text goes,
        // and what is left is what the buttons say — here both of them, ours
        // having stayed lit under the editor.
        m.toggle(at, Side::Theirs);
        assert_eq!(m.text(), "a\nMINE\nTHEIRS\nc\n");
    }

    #[test]
    fn clearing_the_editor_takes_nothing_rather_than_one_blank_line() {
        let mut m = merge("a\nb\nc\n", "a\nMINE\nc\n", "a\nTHEIRS\nc\n");
        let at = m.chunks.iter().position(|c| c.kind.is_conflict()).unwrap();
        m.set_manual(at, "");
        assert_eq!(m.unresolved(), 0);
        assert_eq!(m.text(), "a\nc\n");
    }

    #[test]
    fn a_chunk_the_merge_settled_can_still_be_written_into() {
        // Nothing conflicts here — theirs alone changed the line — and typing
        // over the outcome is still allowed: an automatic answer one disagrees
        // with is exactly what a middle column one can edit is for.
        let mut m = merge("a\nb\nc\n", "a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(m.conflicts(), 0);
        let at = m
            .chunks
            .iter()
            .position(|c| c.kind == Kind::Theirs)
            .unwrap();
        m.set_manual(at, "written");
        assert_eq!(m.text(), "a\nwritten\nc\n");
        m.clear_manual(at);
        assert_eq!(m.text(), "a\nB\nc\n");
    }

    #[test]
    fn the_arrows_walk_the_conflicts_and_wrap_round() {
        let m = merge(
            "keep\na\nb\nc\nd\ne\n",
            "keep\nA\nb\nC\nd\ne\n",
            "keep\nX\nb\nY\nd\ne\n",
        );
        let conflicts = m.conflict_chunks();
        assert_eq!(conflicts.len(), 2);
        // The first chunk is the untouched line, so this really does start from
        // outside a conflict.
        assert_eq!(conflicts[0], 1);
        assert_eq!(m.step(conflicts[0], 1), Some(conflicts[1]));
        // Past the last one, round to the first: what one walks is a handful of
        // places in one file, and an arrow that stops answering reads as broken.
        assert_eq!(m.step(conflicts[1], 1), Some(conflicts[0]));
        assert_eq!(m.step(conflicts[0], -1), Some(conflicts[1]));
        // From a chunk that is not a conflict, the nearest one that way.
        assert_eq!(m.step(0, 1), Some(conflicts[0]));
    }

    #[test]
    fn the_three_columns_stay_aligned_chunk_by_chunk() {
        let m = merge("a\nb\nc\n", "a\nMINE\nc\n", "a\nT1\nT2\nc\n");
        let rows = m.rows();
        // The conflict is two lines tall on the right and one on the left: the
        // taller half sets the height, and the short one is padded.
        let conflict: Vec<_> = rows.iter().filter(|r| r.kind.is_conflict()).collect();
        assert_eq!(conflict.len(), 2);
        assert!(conflict[0].first && !conflict[1].first);
        assert!(conflict[1].ours.is_none());
        assert_eq!(conflict[1].theirs.as_ref().unwrap().text, "T2");
        // And every chunk carries exactly one row that holds its buttons.
        for (index, _) in m.chunks.iter().enumerate() {
            assert_eq!(
                rows.iter().filter(|r| r.chunk == index && r.first).count(),
                1
            );
        }
    }

    #[test]
    fn line_numbers_are_those_of_the_side_the_column_shows() {
        let m = merge("a\nb\nc\n", "a\nMINE\nc\n", "a\nT1\nT2\nc\n");
        let rows = m.rows();
        let last = rows.iter().rev().find(|r| r.ours.is_some()).unwrap();
        // Ours is "a / MINE / c / <trailing>": four lines, the last of them the
        // empty one the final newline leaves behind.
        assert_eq!(last.ours.as_ref().unwrap().number, 4);
        let last = rows.iter().rev().find(|r| r.theirs.is_some()).unwrap();
        assert_eq!(last.theirs.as_ref().unwrap().number, 5);
    }

    #[test]
    fn changes_on_neighbouring_lines_are_one_conflict_as_they_are_for_git() {
        // No line the three agree on between the two edits, so they belong to
        // the same chunk and one answer settles both. It is not an
        // approximation: git prints exactly one pair of markers here too, and a
        // view that split them would offer a decision git will not honour.
        let base = "one\ntwo\nthree\nfour\nfive\n";
        let ours = "one\nTWO\nthree\nFOUR\nfive\n";
        let theirs = "one\n2\nthree\nfour\n5\n";
        let mut m = merge(base, ours, theirs);
        assert_eq!(m.conflicts(), 2);
        m.take_all(Side::Ours);
        assert_eq!(m.text(), "one\nTWO\nthree\nFOUR\nfive\n");
    }
}
