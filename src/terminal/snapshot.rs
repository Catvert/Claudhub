//! Copying the terminal grid into a shape the view can draw.
//!
//! The view never touches the `Term`: it receives a `Snapshot`, that is, lines
//! of text and style runs. Two reasons: the grid's lock is shared with the I/O
//! loop and must not be held during rendering, and a snapshot is comparable
//! from one frame to the next, which makes it possible to redraw only what
//! changed.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::{Color, NamedColor};

/// A cell's colour.
///
/// `Default` is not resolved here: it is Claudhub's theme that decides what
/// "normal text colour" looks like, and it can change without the terminal
/// having anything to re-emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paint {
    Default,
    Rgb(u8, u8, u8),
}

/// A run of cells sharing a style, merged into one segment.
///
/// Merging divides by twenty the number of style runs an ordinary output line
/// produces — most of a terminal's cells share their neighbour's style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Byte offset in the line's text.
    pub start: usize,
    pub end: usize,
    pub fg: Paint,
    pub bg: Paint,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    /// Hidden text (`ESC[8m`): the passwords typed into some tools.
    pub hidden: bool,
    /// Video inversion: it is applied here, by swapping `fg` and `bg`, so the
    /// view has no need to know the notion.
    pub inverse: bool,
    /// Cell taken by the user's selection.
    ///
    /// It is a style attribute like the others, which is not a detail: run
    /// merging takes it into account by itself, so a selection splits the runs
    /// exactly where it must, with no dedicated code.
    pub selected: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// Line on screen, or `None` when the cursor is outside the visible area
    /// because the scrollback has been scrolled.
    pub line: Option<usize>,
    pub column: usize,
    pub visible: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub lines: Vec<Line>,
    pub cursor: Option<Cursor>,
    /// Number of scrollback lines above the view: what makes it possible to
    /// show "you are not looking at the bottom".
    pub display_offset: usize,
    pub total_history: usize,
}

pub(crate) fn capture<T: EventListener>(term: &Term<T>) -> Snapshot {
    let content = term.renderable_content();
    let colors = content.colors;
    let display_offset = content.display_offset;
    let selection = content.selection;

    let mut lines: Vec<Line> = Vec::new();
    let mut line = Line::default();
    let mut pending: Option<Segment> = None;
    let mut line_index: Option<usize> = None;

    for indexed in content.display_iter {
        // The lines of the walk are numbered from the **bottom** of the
        // scrollback: the first visible one is `-display_offset`, and therefore
        // negative as soon as the wheel has been scrolled up. The offset brings
        // them back into viewport coordinates, `0` being the top line.
        //
        // Without it, `max(0)` crushed every line from the past onto index 0:
        // they piled up into a single one, which made the screen "disappear"
        // as soon as you scrolled up.
        let index = viewport_line(indexed.point.line.0, display_offset).unwrap_or(0);
        if line_index != Some(index) {
            if line_index.is_some() {
                flush(&mut lines, &mut line, &mut pending);
            }
            line_index = Some(index);
        }

        let cell = indexed.cell;
        // The continuation cells of a wide character have no content of their
        // own: the previous glyph already occupies the space, and writing their
        // blank would shift everything after it.
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }

        let mut style = style_of(cell, colors);
        style.selected = selection.is_some_and(|range| range.contains(indexed.point));
        let start = line.text.len();
        line.text.push(cell.c);
        // A cell's combining marks (accents, emoji selectors) are part of the
        // same glyph and therefore of the same run.
        for zw in cell.zerowidth().into_iter().flatten() {
            line.text.push(*zw);
        }
        let end = line.text.len();

        match pending.as_mut() {
            Some(seg) if same_style(seg, &style) => seg.end = end,
            _ => {
                if let Some(seg) = pending.take() {
                    line.segments.push(seg);
                }
                pending = Some(Segment {
                    start,
                    end,
                    ..style
                });
            }
        }
    }
    if line_index.is_some() {
        flush(&mut lines, &mut line, &mut pending);
    }

    let cursor_point = content.cursor.point;
    // The cursor is located in the same numbering as the cells, and therefore
    // follows the same offset: scrolling back makes it move below the view,
    // where it must no longer be drawn.
    let screen_lines = lines.len();
    let cursor = Some(Cursor {
        line: viewport_line(cursor_point.line.0, display_offset).filter(|l| *l < screen_lines),
        column: cursor_point.column.0,
        visible: content.cursor.shape != alacritty_terminal::vte::ansi::CursorShape::Hidden,
    });

    Snapshot {
        lines,
        cursor,
        display_offset,
        total_history: term.grid().total_lines(),
    }
}

/// Turns a grid line into its viewport line.
///
/// Returns `None` for what stays above the view — impossible for the cells
/// walked, but not for the cursor, which keeps its place while the scrollback
/// is scrolled.
fn viewport_line(line: i32, display_offset: usize) -> Option<usize> {
    usize::try_from(line + display_offset as i32).ok()
}

/// Closes the current line and files it, starting again from an empty one.
fn flush(lines: &mut Vec<Line>, line: &mut Line, pending: &mut Option<Segment>) {
    if let Some(seg) = pending.take() {
        line.segments.push(seg);
    }
    push_line(lines, std::mem::take(line));
}

fn push_line(lines: &mut Vec<Line>, mut line: Line) {
    // Trailing spaces are what the terminal puts everywhere nothing was
    // written; keeping them would make every line pay for the grid's full
    // width, for an identical result on screen.
    let trimmed = line.text.trim_end_matches(' ').len();
    if trimmed < line.text.len() {
        line.text.truncate(trimmed);
        line.segments.retain_mut(|seg| {
            seg.end = seg.end.min(trimmed);
            seg.start < seg.end
        });
    }
    lines.push(line);
}

fn same_style(a: &Segment, b: &Segment) -> bool {
    a.fg == b.fg
        && a.bg == b.bg
        && a.bold == b.bold
        && a.italic == b.italic
        && a.underline == b.underline
        && a.strikethrough == b.strikethrough
        && a.hidden == b.hidden
        && a.inverse == b.inverse
        && a.selected == b.selected
}

fn style_of(
    cell: &alacritty_terminal::term::cell::Cell,
    colors: &alacritty_terminal::term::color::Colors,
) -> Segment {
    let flags = cell.flags;
    let bold = flags.contains(Flags::BOLD);
    let dim = flags.contains(Flags::DIM);
    let inverse = flags.contains(Flags::INVERSE);

    let mut fg = resolve(cell.fg, colors, bold, dim);
    let mut bg = resolve(cell.bg, colors, false, false);
    if inverse {
        std::mem::swap(&mut fg, &mut bg);
    }

    Segment {
        start: 0,
        end: 0,
        fg,
        bg,
        bold,
        italic: flags.contains(Flags::ITALIC),
        underline: flags.intersects(
            Flags::UNDERLINE | Flags::DOUBLE_UNDERLINE | Flags::UNDERCURL | Flags::DOTTED_UNDERLINE,
        ),
        strikethrough: flags.contains(Flags::STRIKEOUT),
        hidden: flags.contains(Flags::HIDDEN),
        inverse,
        selected: false,
    }
}

/// Resolves a cell's colour.
///
/// The order is the specification's: what the program redefined through OSC 4
/// wins over the built-in palette. `bold` promotes the first eight colours to
/// their bright variant, as every terminal has done since "bold" meant
/// "intense" on a cathode-ray tube.
fn resolve(
    color: Color,
    colors: &alacritty_terminal::term::color::Colors,
    bold: bool,
    dim: bool,
) -> Paint {
    let index = match color {
        Color::Spec(rgb) => return Paint::Rgb(rgb.r, rgb.g, rgb.b),
        Color::Named(NamedColor::Foreground) if !bold && !dim => return Paint::Default,
        Color::Named(named) => {
            let named = if bold {
                named.to_bright()
            } else if dim {
                named.to_dim()
            } else {
                named
            };
            if named == NamedColor::Background {
                return Paint::Default;
            }
            if named == NamedColor::Foreground {
                return Paint::Default;
            }
            named as usize
        }
        Color::Indexed(ix) => {
            let ix = if bold && ix < 8 { ix + 8 } else { ix };
            ix as usize
        }
    };

    if let Some(rgb) = colors[index] {
        return Paint::Rgb(rgb.r, rgb.g, rgb.b);
    }
    let (r, g, b) = default_palette(index);
    Paint::Rgb(r, g, b)
}

/// Default xterm palette: 16 named colours, a 6×6×6 cube, then 24 greys.
fn default_palette(index: usize) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00), // black
        (0xcd, 0x31, 0x31), // red
        (0x0d, 0xbc, 0x79), // green
        (0xe5, 0xe5, 0x10), // yellow
        (0x24, 0x72, 0xc8), // blue
        (0xbc, 0x3f, 0xbc), // magenta
        (0x11, 0xa8, 0xcd), // cyan
        (0xe5, 0xe5, 0xe5), // white
        (0x66, 0x66, 0x66),
        (0xf1, 0x4c, 0x4c),
        (0x23, 0xd1, 0x8b),
        (0xf5, 0xf5, 0x43),
        (0x3b, 0x8e, 0xea),
        (0xd6, 0x70, 0xd6),
        (0x29, 0xb8, 0xdb),
        (0xff, 0xff, 0xff),
    ];
    match index {
        0..=15 => BASE[index],
        16..=231 => {
            // Colour cube: each component takes six values, the first step
            // jumping to 0x5f — that is xterm's table, not a linear
            // progression.
            const LEVELS: [u8; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
            let n = index - 16;
            (LEVELS[(n / 36) % 6], LEVELS[(n / 6) % 6], LEVELS[n % 6])
        }
        232..=255 => {
            let v = 8 + 10 * (index as u8 - 232);
            (v, v, v)
        }
        // Past 255 come Foreground/Background/Cursor, already handled above; an
        // unknown index takes the default colour.
        _ => (0xe5, 0xe5, 0xe5),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    use crate::terminal::TermSize;

    /// Feeds a real `Term` a stream of bytes and returns the snapshot.
    fn render(input: &str) -> Snapshot {
        let size = TermSize::new(20, 4, 8, 16);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        parser.advance(&mut term, input.as_bytes());
        capture(&term)
    }

    #[test]
    fn plain_text_becomes_one_run() {
        let snap = render("hello");
        assert_eq!(snap.lines[0].text, "hello");
        assert_eq!(
            snap.lines[0].segments.len(),
            1,
            "text with no attribute must not be split"
        );
        // The empty lines at the bottom exist but are empty.
        assert_eq!(snap.lines[1].text, "");
    }

    #[test]
    fn colors_split_runs_and_survive_reset() {
        // red, then back to the default style.
        let snap = render("\x1b[31mred\x1b[0mnormal");
        let line = &snap.lines[0];
        assert_eq!(line.text, "rednormal");
        assert_eq!(line.segments.len(), 2);
        assert_eq!(line.segments[0].fg, Paint::Rgb(0xcd, 0x31, 0x31));
        assert_eq!(
            &line.text[line.segments[0].start..line.segments[0].end],
            "red"
        );
        assert_eq!(line.segments[1].fg, Paint::Default);
    }

    #[test]
    fn inverse_swaps_foreground_and_background() {
        let snap = render("\x1b[7minverse");
        let seg = &snap.lines[0].segments[0];
        assert!(seg.inverse);
        // The default background has moved to the foreground and vice versa.
        assert_eq!(seg.fg, Paint::Default);
        assert_eq!(seg.bg, Paint::Default);
    }

    #[test]
    fn bold_brightens_the_basic_palette() {
        let snap = render("\x1b[1;31mbold");
        // Bold red = bright red, as in every terminal.
        assert_eq!(snap.lines[0].segments[0].fg, Paint::Rgb(0xf1, 0x4c, 0x4c));
        assert!(snap.lines[0].segments[0].bold);
    }

    /// The reported gesture: scrolling the wheel up "erased" the screen.
    ///
    /// Lines coming from the scrollback are numbered negatively; without the
    /// viewport offset they were all crushed onto index 0, and the snapshot
    /// returned a single line instead of four.
    #[test]
    fn scrolling_back_shows_the_history_instead_of_collapsing_it() {
        use alacritty_terminal::grid::Scroll;

        let size = TermSize::new(20, 4, 8, 16);
        let mut term = Term::new(
            Config {
                scrolling_history: 100,
                ..Default::default()
            },
            &size,
            VoidListener,
        );
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        for n in 1..=8 {
            parser.advance(&mut term, format!("line{n}\r\n").as_bytes());
        }

        // At the bottom: the last lines written.
        let bottom = capture(&term);
        assert_eq!(bottom.lines.len(), 4);
        assert_eq!(bottom.lines[0].text, "line6");
        assert_eq!(bottom.display_offset, 0);

        term.scroll_display(Scroll::Delta(3));
        let scrolled = capture(&term);
        assert_eq!(scrolled.display_offset, 3);
        assert_eq!(
            scrolled.lines.len(),
            4,
            "the screen keeps its height when scrolled up"
        );
        let texts: Vec<&str> = scrolled.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["line3", "line4", "line5", "line6"]);

        // The cursor is three lines below the view: out of frame.
        assert_eq!(scrolled.cursor.unwrap().line, None);
    }

    #[test]
    fn cursor_follows_the_text() {
        let snap = render("abc");
        let cursor = snap.cursor.unwrap();
        assert_eq!(cursor.line, Some(0));
        assert_eq!(cursor.column, 3);
    }

    #[test]
    fn a_selection_splits_the_runs_it_covers() {
        use alacritty_terminal::index::{Column, Line, Point, Side};
        use alacritty_terminal::selection::{Selection, SelectionType};

        let size = TermSize::new(20, 4, 8, 16);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        parser.advance(&mut term, b"abcdef");

        // Selects "bcd".
        let mut selection = Selection::new(
            SelectionType::Simple,
            Point::new(Line(0), Column(1)),
            Side::Left,
        );
        selection.update(Point::new(Line(0), Column(3)), Side::Right);
        term.selection = Some(selection);

        let snap = capture(&term);
        let line = &snap.lines[0];
        assert_eq!(line.text, "abcdef");
        let selected: String = line
            .segments
            .iter()
            .filter(|s| s.selected)
            .map(|s| &line.text[s.start..s.end])
            .collect();
        assert_eq!(selected, "bcd", "runs = {:?}", line.segments);
    }

    #[test]
    fn palette_matches_xterm() {
        assert_eq!(default_palette(16), (0x00, 0x00, 0x00));
        assert_eq!(default_palette(21), (0x00, 0x00, 0xff));
        assert_eq!(default_palette(46), (0x00, 0xff, 0x00));
        assert_eq!(default_palette(232), (8, 8, 8));
        assert_eq!(default_palette(255), (238, 238, 238));
    }
}
