//! Looking at a file instead of editing it: images, and SVG.
//!
//! An image opens the way a file opens — a tab in the editor's dock, closed by
//! the same cross, remembered by the same session. What changes is what fills
//! it: the editor is replaced by the picture, and everything the editor drags
//! along with it — the language server, the gutter's base, the modal harness —
//! is simply not asked for. There is nothing to type in and nothing to save.
//!
//! **The bytes travel undecoded.** The worker reads the file and hands over
//! what was in it; gpui decodes and caches by the digest of those very bytes.
//! Decoding in the worker would send a frame's worth of pixels down a wire that
//! crosses into WSL, and would do the work twice.
//!
//! **SVG is one of the formats and not a special case**: gpui rasters it
//! through its own renderer, from bytes, like the rest. The one thing that
//! follows from it — the raster is made at the file's own size, so blowing an
//! icon up to full width shows its pixels rather than resampling the curves —
//! is why "actual size" is the honest default for anything smaller than the
//! panel, and why the toggle exists at all.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{div, img, prelude::*, px, Context, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Sizable,
};

use crate::files;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;

/// An image held open in a tab.
pub struct Preview {
    pub kind: files::Picture,
    /// The size of the file, for the footer. Read off the bytes rather than
    /// carried alongside: it is the same number and one field fewer to keep
    /// truthful.
    pub bytes: usize,
    /// Built **once**, when the image arrives: `gpui::Image::from_bytes`
    /// digests the whole file to key the decoded texture, and a render closure
    /// runs sixty times a second.
    pub image: Arc<gpui::Image>,
    /// Scaled down to the panel, or painted at its own size in a scrollable
    /// area. Never scaled **up**: an icon stretched to the width of a window is
    /// a wall of pixels, and it is the one thing a preview must not invent.
    pub fit: bool,
}

/// Our vocabulary translated into gpui's — the only place the two meet.
fn format_of(kind: files::Picture) -> gpui::ImageFormat {
    use files::Picture;
    match kind {
        Picture::Png => gpui::ImageFormat::Png,
        Picture::Jpeg => gpui::ImageFormat::Jpeg,
        Picture::Gif => gpui::ImageFormat::Gif,
        Picture::Webp => gpui::ImageFormat::Webp,
        Picture::Bmp => gpui::ImageFormat::Bmp,
        Picture::Ico => gpui::ImageFormat::Ico,
        Picture::Tiff => gpui::ImageFormat::Tiff,
        Picture::Svg => gpui::ImageFormat::Svg,
    }
}

/// What a file's size reads as under the picture. Binary units, as `ls -h`
/// gives them.
fn human_size(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024. && unit < UNITS.len() - 1 {
        size /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

impl ClaudhubApp {
    /// The command that opens a file: the editor's, or the preview's.
    ///
    /// **Decided from the name alone**, before anything is read, and decided
    /// here rather than in the worker: what comes back has to land in the right
    /// half of the tab, and the two answers are two events. Every opening goes
    /// through this — the tree, a jump, a session being restored — so a `.png`
    /// remembered from the last session comes back as a picture and not as
    /// "binary file".
    pub(super) fn read_file_cmd(&self, worktree: PathBuf, path: PathBuf) -> Cmd {
        if files::picture_of(&path).is_some() {
            Cmd::ReadImage { worktree, path }
        } else {
            Cmd::ReadFile { worktree, path }
        }
    }

    /// Receives an image and installs its tab.
    ///
    /// The editor's own arrival — `file_content_arrived` — with everything that
    /// belongs to text left out: no highlighter, no change subscription, no
    /// language server, no base to compare against. What is kept is the tab: a
    /// file already open is **reused** rather than opened twice, which is the
    /// one thing a tab bar must never do.
    pub(super) fn image_content_arrived(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        image: files::Image,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let restored = self.take_restored_editing(&worktree, &path);
        let preview = Preview {
            kind: image.kind,
            bytes: image.bytes.len(),
            image: Arc::new(gpui::Image::from_bytes(format_of(image.kind), image.bytes)),
            // Fitted on arrival: a screenshot is larger than the panel far more
            // often than an icon is smaller than it, and `fit` never scales up
            // anyway — so the two agree on everything but the big picture.
            fit: true,
        };
        // An `EditorState` all the same, empty and never drawn: it is what the
        // tab holds its focus handle by, and what the dock asks for when the
        // panel takes the keyboard. Building a second kind of panel to avoid
        // one unused entity would be two of everything for one field.
        let input = cx.new(|cx| gpui_component::input::EditorState::new(window, cx));
        // The document the language server was holding is dropped: arriving in
        // a file closes the one that was there, and an image is an arrival like
        // any other.
        self.close_previous_document(&worktree, &path);
        let host = crate::ui::surface::VimHost::new(&input, cx);
        // A picture is looked at more than anything else in a tree: it takes
        // the preview tab like a file, and by the same rule.
        let ephemeral = self.asked_for(&worktree, &path).unwrap_or(false);
        self.make_tab_room(&worktree, &path, ephemeral, window, cx);
        let (reopened, panel) = self.tab_panel(&worktree, &path, &input, window, cx);
        let editing = super::explorer::Editing {
            worktree: worktree.clone(),
            path: path.clone(),
            scroll_key: crate::ui::surface::Surface::file_scroll_key(&path),
            input,
            hash: 0,
            dirty: false,
            lsp_pending: false,
            reveal_at: None,
            reveal_tries: 0,
            host,
            panel,
            base: None,
            // Said to have been asked for, and it is what stops it being asked:
            // an image has no base to compare a buffer against, and the gutter
            // it would feed is not drawn.
            base_asked: true,
            hunks: std::rc::Rc::default(),
            last_line: 0,
            hunk_open: None,
            preview: Some(preview),
            ephemeral,
            // A picture has no words to light: `git grep` does not look inside
            // one, so nothing here ever comes from a hit.
            lit: false,
            used: self.touch_tab(),
        };
        self.place_tab(&worktree, reopened, editing);
        // A jump that landed on an image has nowhere to put a caret — hence the
        // `false` — but the trail still records the place, which is the file
        // itself. See `finish_tab`.
        self.finish_tab(&worktree, &path, restored, false, window, cx);
    }

    /// Fitted to the panel, or at its own size. The button says which.
    pub(super) fn toggle_preview_fit(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(preview) = self
            .editing_at_mut(&path)
            .and_then(|editing| editing.preview.as_mut())
        {
            preview.fit = !preview.fit;
        }
        cx.notify();
    }

    /// The picture, its bar and its footer — the editor's place, when the file
    /// being read is one to look at.
    pub(super) fn render_image_preview(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let editing = self.editing()?;
        let preview = editing.preview.as_ref()?;
        let (fit, kind, bytes) = (preview.fit, preview.kind, preview.bytes);
        let image = preview.image.clone();
        let path = editing.path.clone();
        let label = SharedString::from(path.display().to_string());
        let mono = cx.theme().mono_font_family.clone();
        let for_external = path.clone();
        let for_fit = path.clone();
        Some(
            v_flex()
                .size_full()
                .child(
                    h_flex()
                        .h(crate::ui::theme::bar_height(cx))
                        .w_full()
                        .px_2()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(icon("image").xsmall())
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .text_sm()
                                .font_family(mono.clone())
                                .child(label),
                        )
                        .children(self.render_jump_buttons(cx))
                        .child(
                            Button::new("preview-fit")
                                .ghost()
                                .small()
                                .icon(icon(if fit { "maximize" } else { "minimize" }))
                                .tooltip(if fit {
                                    tr!("preview-actual-size")
                                } else {
                                    tr!("preview-fit")
                                })
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.toggle_preview_fit(for_fit.clone(), cx);
                                })),
                        )
                        .child(
                            Button::new("preview-external")
                                .ghost()
                                .small()
                                .icon(icon("external-link"))
                                .tooltip(tr!("editor-external"))
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.open_externally(for_external.clone(), 1, cx);
                                })),
                        )
                        .child(
                            Button::new("preview-close")
                                .ghost()
                                .small()
                                .icon(icon("x"))
                                .tooltip(tr!("editor-close"))
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.close_editor(window, cx)
                                    }),
                                ),
                        ),
                )
                .child(
                    div()
                        .id("preview-area")
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        // Scrollable in both directions, and it is the whole
                        // point of "actual size": a screenshot at 1:1 is wider
                        // than the panel, and what one came to look at is the
                        // corner of it. Fitted, nothing overflows and the bars
                        // never show.
                        .overflow_scroll()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p_2()
                        .child(
                            // `max_w`/`max_h` and not `w`/`h`: an image smaller
                            // than the panel keeps its own size in both modes —
                            // scaling a 16-pixel icon up to a thousand is not a
                            // preview of anything.
                            img(image)
                                .when(fit, |el| el.max_w_full().max_h_full())
                                .object_fit(gpui::ObjectFit::ScaleDown),
                        ),
                )
                .child(
                    h_flex()
                        .h(crate::ui::theme::bar_height(cx))
                        .w_full()
                        .px_2()
                        .gap_2()
                        .items_center()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(kind.label()))
                        .child(div().w(px(1.)).h(px(10.)).bg(cx.theme().border))
                        .child(SharedString::from(human_size(bytes))),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::human_size;

    #[test]
    fn a_size_reads_in_the_unit_that_fits_it() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
