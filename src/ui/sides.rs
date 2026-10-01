//! The two versions of a file the review looks at rather than reads.
//!
//! git says "binary" and stops there. What a reviewer wants of a changed icon
//! is the icon, before and after; of a regenerated PDF or a bumped font, at
//! least what it is and how much it moved. The worker hands over both versions
//! (`git::diff::sides`) — a picture's bytes, anything else described — and
//! this paints them side by side, in place of the lines.
//!
//! **An SVG is both**: git diffs it as text, which is the truth of the change,
//! and the header's toggle shows what that text draws.
//!
//! The pictures are decoded **once**, when the diff arrives (`Shown::new`): a
//! `gpui_kit::Image` digests its bytes to key the texture, and a render closure
//! runs every frame.

use std::path::Path;
use std::sync::Arc;

use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Sizable};
use gpui_kit::{div, img, prelude::*, px, SharedString};

use crate::files::{Format, Picture};
use crate::git::diff::{Sides, Version};
use crate::tr;
use crate::ui::icons::icon;

/// Both versions as the view paints them.
pub struct Shown {
    pub old: Option<Side>,
    pub new: Option<Side>,
}

/// One version: what it is, and the picture when there is one to paint.
pub struct Side {
    pub size: u64,
    pub format: Option<Format>,
    pub dimensions: Option<(u32, u32)>,
    pub image: Option<Arc<gpui_kit::Image>>,
}

impl Shown {
    pub fn new(sides: Sides) -> Self {
        Self {
            old: sides.old.map(Side::new),
            new: sides.new.map(Side::new),
        }
    }
}

impl Side {
    fn new(version: Version) -> Self {
        let image = match (version.format, version.bytes) {
            (Some(Format::Picture(picture)), Some(bytes)) => Some(Arc::new(
                gpui_kit::Image::from_bytes(crate::ui::preview::format_of(picture), bytes),
            )),
            _ => None,
        };
        Self {
            size: version.size,
            format: version.format,
            dimensions: version.dimensions,
            image,
        }
    }
}

/// What the format is called: its signature's name, else the file's
/// extension, else nothing — the view then says "binary".
fn format_name(format: Option<Format>, path: &Path) -> Option<String> {
    format.map(|format| format.label().to_string()).or_else(|| {
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_uppercase)
    })
}

/// The icon a format is drawn with when there is no picture to show: its
/// family, as the tree would name it.
fn icon_of(format: Option<Format>) -> &'static str {
    match format {
        Some(Format::Picture(_) | Format::Avif) => "image",
        Some(
            Format::Zip
            | Format::Gzip
            | Format::Xz
            | Format::Bzip2
            | Format::Zstd
            | Format::SevenZip
            | Format::Rar
            | Format::Tar,
        ) => "archive",
        Some(Format::Woff | Format::Woff2 | Format::TrueType | Format::OpenType) => "file-type",
        Some(Format::Sqlite) => "database",
        Some(Format::Elf | Format::Exe | Format::MachO | Format::Wasm | Format::JavaClass) => "cpu",
        Some(Format::Mp3 | Format::Ogg | Format::Flac | Format::Wav) => "file-audio",
        Some(Format::Mp4 | Format::Matroska) => "file-video",
        Some(Format::Pdf) => "file-text",
        None => "file",
    }
}

/// How the size moved, signed, in the unit that fits the gap — `None` when it
/// did not move.
fn growth(old: u64, new: u64) -> Option<String> {
    let (sign, gap) = match new.cmp(&old) {
        std::cmp::Ordering::Equal => return None,
        std::cmp::Ordering::Greater => ('+', new - old),
        std::cmp::Ordering::Less => ('−', old - new),
    };
    let gap = crate::ui::preview::human_size(usize::try_from(gap).unwrap_or(usize::MAX));
    // A percentage of nothing is no percentage: a file grown from empty.
    Some(match (new.abs_diff(old) * 100).checked_div(old) {
        Some(percent) if percent > 0 => format!("{sign}{gap} ({sign}{percent} %)"),
        _ => format!("{sign}{gap}"),
    })
}

/// A side's caption: format, dimensions, weight — the facts that differ.
fn caption(side: &Side, path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.extend(format_name(side.format, path));
    if let Some((width, height)) = side.dimensions {
        parts.push(format!("{width} × {height}"));
    }
    parts.push(crate::ui::preview::human_size(
        usize::try_from(side.size).unwrap_or(usize::MAX),
    ));
    parts.join(" · ")
}

/// Both versions side by side, or the one there is.
pub fn render(shown: &Shown, path: &Path, cx: &mut gpui_kit::App) -> gpui_kit::AnyElement {
    let columns: Vec<(SharedString, &Side, Option<String>)> = match (&shown.old, &shown.new) {
        (Some(old), Some(new)) => vec![
            (tr!("sides-before"), old, None),
            (tr!("sides-after"), new, growth(old.size, new.size)),
        ],
        (Some(old), None) => vec![(tr!("sides-deleted"), old, None)],
        (None, Some(new)) => vec![(tr!("sides-added"), new, None)],
        (None, None) => {
            return div()
                .p_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("review-binary"))
                .into_any_element();
        }
    };
    h_flex()
        .id("diff-sides")
        .flex_1()
        .min_h_0()
        .w_full()
        .p_2()
        .gap_2()
        .children(
            columns
                .into_iter()
                .map(|(title, side, growth)| column(title, side, growth, path, cx)),
        )
        .into_any_element()
}

/// One version: its title and caption over it, the picture or the card below.
fn column(
    title: SharedString,
    side: &Side,
    growth: Option<String>,
    path: &Path,
    cx: &mut gpui_kit::App,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let body = match (&side.image, side.format) {
        // `max_w`/`max_h` and `ScaleDown`, as the preview does: a small icon
        // keeps its size, a screenshot shrinks to the column — two versions of
        // one picture are then drawn at the same scale whenever both fit.
        (Some(image), _) => div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p_2()
            .child(
                img(image.clone())
                    .max_w_full()
                    .max_h_full()
                    .object_fit(gpui_kit::ObjectFit::ScaleDown),
            )
            .into_any_element(),
        (None, format) => {
            let note = match format {
                Some(Format::Picture(_)) if side.size > crate::files::MAX_IMAGE_BYTES => {
                    tr!("sides-too-big", {
                        mb: crate::files::MAX_IMAGE_BYTES / (1024 * 1024)
                    })
                }
                _ => tr!("sides-no-preview"),
            };
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    icon(icon_of(format))
                        .large()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div().text_lg().child(
                        format_name(format, path)
                            .map_or_else(|| tr!("sides-binary"), SharedString::from),
                    ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(note),
                )
                .into_any_element()
        }
    };
    v_flex()
        .flex_1()
        .min_w_0()
        .h_full()
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius)
        .overflow_hidden()
        .child(
            h_flex()
                .h(crate::ui::theme::bar_height(cx))
                .px_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(theme.border)
                .text_xs()
                .child(
                    div()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(div().w(px(1.)).h(px(10.)).bg(theme.border))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(caption(side, path)),
                )
                .children(growth.map(|growth| {
                    div()
                        .flex_none()
                        .text_color(theme.muted_foreground)
                        .child(growth)
                })),
        )
        .child(div().flex_1().min_h_0().child(body))
}

/// Is this file one the header offers to draw rather than diff — a picture git
/// reads as text?
pub fn drawable(path: &Path) -> bool {
    crate::files::picture_of(path) == Some(Picture::Svg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_moves_by_a_signed_gap_and_its_share() {
        assert_eq!(growth(1024, 1024), None);
        assert_eq!(growth(1000, 1100).as_deref(), Some("+100 B (+10 %)"));
        assert_eq!(growth(2048, 1024).as_deref(), Some("−1.0 KB (−50 %)"));
        // From nothing, there is no share to give.
        assert_eq!(growth(0, 2048).as_deref(), Some("+2.0 KB"));
    }

    #[test]
    fn a_caption_says_what_it_knows() {
        let side = Side {
            size: 2048,
            format: Some(Format::Picture(Picture::Png)),
            dimensions: Some((64, 32)),
            image: None,
        };
        assert_eq!(caption(&side, Path::new("a.png")), "PNG · 64 × 32 · 2.0 KB");
        // No signature known: the extension speaks for it.
        let unknown = Side {
            size: 10,
            format: None,
            dimensions: None,
            image: None,
        };
        assert_eq!(caption(&unknown, Path::new("model.onnx")), "ONNX · 10 B");
        assert_eq!(caption(&unknown, Path::new("blob")), "10 B");
    }

    #[test]
    fn only_a_picture_read_as_text_is_drawn_on_demand() {
        assert!(drawable(Path::new("icons/logo.svg")));
        // A PNG is always drawn: git calls it binary.
        assert!(!drawable(Path::new("logo.png")));
        assert!(!drawable(Path::new("main.rs")));
    }
}
