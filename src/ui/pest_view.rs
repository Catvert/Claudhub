//! The tests panel: a worktree's Pest suite, listed and launched.
//!
//! The list is `crate::pest`'s reading of `pest --list-tests`, asked on the
//! background queue when a worktree is first looked at and re-asked when a
//! file of `tests/` changes. Running goes through a terminal tab, like a
//! `just` recipe and for the same reason: a suite prints, colours, asks to be
//! watched — a terminal is what its output is written for. The row's whole
//! decision — which rows a query leaves, and where the class headers go — is
//! pure and tested at the bottom of the file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{div, prelude::*, uniform_list, App, Context, Entity, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    menu::{ContextMenuExt, PopupMenu, PopupMenuItem},
    v_flex, ActiveTheme, Disableable, Sizable,
};

use crate::pest::{Report, Test};
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::find::Pane;
use crate::ui::icons::icon;

/// What the panel knows of one worktree's suite.
#[derive(Default)]
pub struct PestState {
    /// Behind an `Rc`: the row closure runs for every visible row on every
    /// frame and cannot read the application back, so it has to capture the
    /// list. `None`: asked, not answered yet.
    pub report: Option<Rc<Report>>,
    /// A listing has gone out and has not come back. Without this guard, a
    /// burst of saves under `tests/` would boot one PHP per keystroke.
    pub pending: bool,
    /// The suite changed while a listing was out: ask again when it lands,
    /// rather than showing a list already known to be stale.
    pub stale: bool,
}

/// Does a change to this file call for re-reading the suite?
///
/// The tests themselves, and the two files that decide what a suite is:
/// `phpunit.xml`, and `composer.json` — through which Pest arrives and leaves.
/// Not every `.php` of the project: the watcher fires on each save, and a
/// listing boots PHP.
pub fn reloads(worktree: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(worktree) else {
        return false;
    };
    if rel.starts_with("tests") {
        return rel.extension().is_some_and(|ext| ext == "php");
    }
    matches!(
        rel.to_str(),
        Some("phpunit.xml" | "phpunit.xml.dist" | "composer.json")
    )
}

/// One row of the list: a class header, or a test. Both carry an index into
/// the report's tests — the header, that of the first kept test of its class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Class(usize),
    Test(usize),
}

/// Which rows a query leaves, headers placed.
///
/// A test stays when the query matches its name **or** its class: typing
/// `Unit\Math` is how one narrows to a file, and the file's tests are then
/// the answer. Headers separate runs of classes, which are contiguous in
/// Pest's order — the list is never re-sorted, the suite's order is the
/// file's.
pub fn rows(tests: &[Test], query: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut current: Option<&str> = None;
    for (index, test) in tests.iter().enumerate() {
        let kept = crate::ui::find::matches(query, &test.name)
            || crate::ui::find::matches(query, crate::pest::short_class(&test.class));
        if !kept {
            continue;
        }
        if current != Some(test.class.as_str()) {
            rows.push(Row::Class(index));
            current = Some(test.class.as_str());
        }
        rows.push(Row::Test(index));
    }
    rows
}

impl ClaudhubApp {
    /// Asks for a worktree's suite, once. On the worktree being looked at,
    /// like the justfile and beside it: the panel's visibility depends on the
    /// answer, so waiting for a first paint would wait forever.
    pub(super) fn ensure_pest(&mut self, worktree: &Path) {
        if self.pest.contains_key(worktree) {
            return;
        }
        self.pest.insert(
            worktree.to_path_buf(),
            PestState {
                pending: true,
                ..Default::default()
            },
        );
        self.git.send(Cmd::PestLoad {
            worktree: worktree.to_path_buf(),
        });
    }

    /// Reads it again — the suite has changed, or the retry button was
    /// pressed. The list is **kept** while the answer travels: a panel that
    /// blinks empty on every save reads as broken.
    pub(super) fn reload_pest(&mut self, worktree: &Path) {
        let Some(state) = self.pest.get_mut(worktree) else {
            self.ensure_pest(worktree);
            return;
        };
        if state.pending {
            state.stale = true;
            return;
        }
        state.pending = true;
        self.git.send(Cmd::PestLoad {
            worktree: worktree.to_path_buf(),
        });
    }

    pub(super) fn pest_arrived(
        &mut self,
        worktree: PathBuf,
        report: Report,
        cx: &mut Context<Self>,
    ) {
        let state = self.pest.entry(worktree.clone()).or_default();
        state.report = Some(Rc::new(report));
        state.pending = false;
        if state.stale {
            state.stale = false;
            state.pending = true;
            self.git.send(Cmd::PestLoad { worktree });
        }
        cx.notify();
    }

    /// The tab exists only where there is a suite: on everything that is not a
    /// Pest project the honest panel is no panel — there is nothing to run.
    /// `Failed` counts as a suite: Pest is installed and its message is the
    /// content.
    pub(super) fn tests_visible(&self) -> bool {
        if !self.panel_visible(crate::ui::panels::TestsPanel::NAME) {
            return false;
        }
        let Some(active) = self.active.as_deref() else {
            return false;
        };
        matches!(
            self.pest
                .get(active)
                .and_then(|state| state.report.as_deref()),
            Some(Report::Tests(_) | Report::Failed(_))
        )
    }

    /// Runs the suite, one class, or one test — the difference is the
    /// `--filter`, and building it is `crate::pest`'s tested business.
    fn run_pest(
        &mut self,
        label: SharedString,
        filter: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let line = match &filter {
            None => "vendor/bin/pest".to_string(),
            Some(filter) => format!(
                "vendor/bin/pest {}",
                crate::cmdline::join_command(["--filter", filter.as_str()])
            ),
        };
        self.open_terminal(
            &worktree,
            crate::ui::terminal_view::Launch {
                // Through a login shell, like a recipe: `php` is looked up on
                // the `PATH`, and a window opened from a desktop launcher does
                // not have the one the user's shell builds.
                command: Some(("sh".into(), vec!["-lc".into(), line])),
                env: HashMap::new(),
                label,
                agent: false,
            },
            window,
            cx,
        );
    }

    pub(super) fn render_pest(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(active) = self.active.clone() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(div().text_sm().child(tr!("no-worktree")))
                .into_any_element();
        };
        self.ensure_pest(&active);
        let state = self.pest.get(&active);
        let pending = state.is_some_and(|state| state.pending);
        let report = state.and_then(|state| state.report.clone());
        let tests: Rc<Vec<Test>> = match report.as_deref() {
            Some(Report::Tests(tests)) => {
                // One copy when the report lands would be better still, but the
                // report is shared and the list is what the rows capture.
                Rc::new(tests.clone())
            }
            Some(Report::Failed(message)) => {
                let message = SharedString::from(message.clone());
                return v_flex()
                    .size_full()
                    .child(self.render_pest_bar(0, pending, cx))
                    .child(failed_pest(message, cx))
                    .into_any_element();
            }
            // Missing, or not answered yet: the tab is normally not shown at
            // all (`tests_visible`), but the panel can still be painted for a
            // frame while the answer travels.
            Some(Report::Missing) | None => Rc::new(Vec::new()),
        };

        let query = self.query(Pane::Tests, cx);
        let find = self.render_find(Pane::Tests, cx);
        let bar = self.render_pest_bar(tests.len(), pending, cx);
        let rows: Rc<Vec<Row>> = Rc::new(rows(&tests, &query));
        if rows.is_empty() {
            return v_flex()
                .size_full()
                .child(bar)
                .children(find)
                .child(empty_pest(&query, pending, cx))
                .into_any_element();
        }

        let look = Look::of(cx);
        let entity = cx.entity();
        let scroll = self.pest_scroll.clone();
        let count = rows.len();
        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .child(
                div().flex_1().min_h_0().child(
                    self.scrolled(
                        "pest-bar",
                        &scroll,
                        crate::ui::motion::Axes::Vertical,
                        window,
                        uniform_list("pest-rows", count, move |visible, _window, _cx| {
                            visible
                                .map(|index| match rows.get(index) {
                                    Some(Row::Class(at)) => {
                                        render_class(index, &tests, *at, &look, &entity)
                                    }
                                    Some(Row::Test(at)) => {
                                        render_test(index, &tests, *at, &look, &entity)
                                    }
                                    None => div().into_any_element(),
                                })
                                .collect::<Vec<_>>()
                        })
                        .size_full()
                        .track_scroll(&scroll.clone()),
                        cx,
                    ),
                ),
            )
            .into_any_element()
    }

    fn render_pest_bar(
        &mut self,
        count: usize,
        pending: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("circle-check").xsmall())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("pest-count", { n: count })),
            )
            .child(
                Button::new("pest-run-all")
                    .ghost()
                    .xsmall()
                    .icon(icon("play"))
                    .tooltip(tr!("pest-run-all"))
                    .disabled(count == 0)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.run_pest(SharedString::from("pest"), None, window, cx);
                    })),
            )
            .child(
                Button::new("pest-refresh")
                    .ghost()
                    .xsmall()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-refresh"))
                    .disabled(pending)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        if let Some(active) = this.active.clone() {
                            this.reload_pest(&active);
                        }
                        cx.notify();
                    })),
            )
    }
}

/// What the theme gives a row, read once per frame and not per row.
#[derive(Clone, Copy)]
struct Look {
    row: gpui::Pixels,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
}

impl Look {
    fn of(cx: &App) -> Self {
        Self {
            row: crate::ui::theme::row_height(cx),
            muted: cx.theme().muted_foreground,
            accent: cx.theme().accent,
        }
    }
}

/// A class header: the file's tests, and the button that runs them together.
fn render_class(
    index: usize,
    tests: &Rc<Vec<Test>>,
    at: usize,
    look: &Look,
    entity: &Entity<ClaudhubApp>,
) -> gpui::AnyElement {
    let Some(test) = tests.get(at) else {
        return div().into_any_element();
    };
    let run = entity.clone();
    let for_run = tests.clone();
    h_flex()
        .id(("pest-class", index))
        .h(look.row)
        .w_full()
        .pl_1p5()
        .pr(crate::ui::theme::scroll_gutter())
        .gap_1()
        .items_center()
        .child(icon("folder").xsmall().text_color(look.muted))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(look.muted)
                .child(SharedString::from(
                    crate::pest::short_class(&test.class).to_string(),
                )),
        )
        .child(
            Button::new(("pest-class-run", index))
                .ghost()
                .xsmall()
                .icon(icon("play"))
                .tooltip(tr!("pest-run-class"))
                .on_click(move |_, window, cx| {
                    let Some(test) = for_run.get(at) else { return };
                    let label = SharedString::from(format!(
                        "pest {}",
                        crate::pest::short_class(&test.class)
                    ));
                    let filter = crate::pest::class_filter(&test.class);
                    run.update(cx, |this, cx| {
                        this.run_pest(label, Some(filter), window, cx);
                    });
                }),
        )
        .into_any_element()
}

/// One test. The click runs it: the list exists to make that one gesture.
fn render_test(
    index: usize,
    tests: &Rc<Vec<Test>>,
    at: usize,
    look: &Look,
    entity: &Entity<ClaudhubApp>,
) -> gpui::AnyElement {
    let Some(test) = tests.get(at) else {
        return div().into_any_element();
    };
    let run = entity.clone();
    let menu = entity.clone();
    let (for_click, for_menu) = (tests.clone(), tests.clone());
    h_flex()
        .id(("pest-row", index))
        .h(look.row)
        .w_full()
        .pl_6()
        .pr(crate::ui::theme::scroll_gutter())
        .gap_1()
        .items_center()
        .cursor_pointer()
        .hover(|s| s.bg(look.accent.opacity(0.4)))
        .on_click(move |_, window, cx| {
            let Some(test) = for_click.get(at) else {
                return;
            };
            let label = SharedString::from(format!("pest {}", test.name));
            let filter = crate::pest::test_filter(test);
            run.update(cx, |this, cx| {
                this.run_pest(label, Some(filter), window, cx);
            });
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .child(SharedString::from(test.name.clone())),
        )
        .when(test.datasets > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(look.muted)
                    .child(SharedString::from(format!("×{}", test.datasets))),
            )
        })
        .context_menu(move |popup, _window, _cx| match for_menu.get(at) {
            Some(test) => row_menu(popup, &menu, test),
            None => popup,
        })
        .into_any_element()
}

fn row_menu(popup: PopupMenu, entity: &Entity<ClaudhubApp>, test: &Test) -> PopupMenu {
    let popup = popup.item({
        let entity = entity.clone();
        let test = test.clone();
        PopupMenuItem::new(tr!("pest-run-test"))
            .icon(icon("play"))
            .on_click(move |_, window, cx| {
                let label = SharedString::from(format!("pest {}", test.name));
                let filter = crate::pest::test_filter(&test);
                entity.update(cx, |this, cx| {
                    this.run_pest(label, Some(filter), window, cx);
                });
            })
    });
    popup.item({
        let filter = crate::pest::test_filter(test);
        // For the terminal one already has open: the panel's gesture, portable.
        PopupMenuItem::new(tr!("pest-copy-filter"))
            .icon(icon("copy"))
            .on_click(move |_, _window, cx| {
                let line = format!(
                    "vendor/bin/pest {}",
                    crate::cmdline::join_command(["--filter", filter.as_str()])
                );
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(line));
            })
    })
}

/// Pest refused to list — a parse error in a test file, most days. Its
/// message is the content: it names the file and the line.
fn failed_pest(message: SharedString, cx: &App) -> gpui::AnyElement {
    v_flex()
        .size_full()
        .p_4()
        .gap_2()
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .text_color(cx.theme().danger)
                .child(icon("alert-circle").xsmall())
                .child(div().text_sm().child(tr!("pest-failed"))),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(cx.theme().muted_foreground)
                .child(message),
        )
        .into_any_element()
}

/// Nothing to show: a listing under way, a search that found nothing, or a
/// suite with no test at all — three different things, and saying the wrong
/// one is how a panel reads as broken.
fn empty_pest(query: &str, pending: bool, cx: &App) -> gpui::AnyElement {
    let message = if pending {
        tr!("pest-loading")
    } else if query.trim().is_empty() {
        tr!("pest-empty")
    } else {
        tr!("find-no-match")
    };
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(icon("circle-check"))
        .child(div().text_sm().px_4().child(message))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test(class: &str, name: &str) -> Test {
        Test {
            class: class.to_string(),
            name: name.to_string(),
            pattern: String::new(),
            datasets: 0,
        }
    }

    fn suite() -> Vec<Test> {
        vec![
            test("Tests\\Unit\\MathTest", "it sums"),
            test("Tests\\Unit\\MathTest", "it divides"),
            test("Tests\\Feature\\HttpTest", "it answers"),
        ]
    }

    #[test]
    fn a_header_opens_each_class() {
        assert_eq!(
            rows(&suite(), ""),
            [
                Row::Class(0),
                Row::Test(0),
                Row::Test(1),
                Row::Class(2),
                Row::Test(2)
            ]
        );
    }

    /// The query filters, and a class left with nothing loses its header —
    /// a title over an empty run would read as a row that lost its tests.
    #[test]
    fn the_query_takes_the_headers_with_it() {
        assert_eq!(rows(&suite(), "answers"), [Row::Class(2), Row::Test(2)]);
        assert!(rows(&suite(), "nothing here").is_empty());
    }

    /// Typing a class narrows to the file: its tests all stay, name matched
    /// or not.
    #[test]
    fn a_class_query_keeps_the_whole_file() {
        assert_eq!(
            rows(&suite(), "Unit\\Math"),
            [Row::Class(0), Row::Test(0), Row::Test(1)]
        );
    }

    /// The suite is re-read for what changes it: the tests, and the two files
    /// that decide what a suite is. Not the application code — a listing
    /// boots PHP, and the watcher fires on every save.
    #[test]
    fn only_the_suites_own_files_reload_it() {
        let wt = Path::new("/p/site");
        assert!(reloads(wt, Path::new("/p/site/tests/Unit/MathTest.php")));
        assert!(reloads(wt, Path::new("/p/site/tests/Pest.php")));
        assert!(reloads(wt, Path::new("/p/site/phpunit.xml")));
        assert!(reloads(wt, Path::new("/p/site/composer.json")));
        assert!(!reloads(wt, Path::new("/p/site/app/Models/User.php")));
        assert!(!reloads(wt, Path::new("/p/site/tests/fixtures/data.json")));
        assert!(!reloads(wt, Path::new("/elsewhere/tests/T.php")));
    }
}
