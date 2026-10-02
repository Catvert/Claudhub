//! The plugin marketplaces, from the interface — see `crate::market` for
//! what one is and where its files go.
//!
//! The settings name the marketplaces (`plugin_markets`); each is fetched on
//! the network queue at start-up and when the user asks, and what comes back
//! is written, off the thread, as its **catalog**. A catalog is read and
//! shown, never mounted: a plugin runs once the user has read its card —
//! where it comes from, what it may do — and pressed Install. An installed
//! plugin stays at its commit; a newer one in the catalog is offered, never
//! taken.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _,
};
use gpui_kit::{div, prelude::*, px, AnyElement, Context, SharedString, Window};

use crate::market::{self, CatalogMeta, Fetched, Provenance};
use crate::scripts::Script;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;

/// Where the catalogs are written, one folder per marketplace.
pub fn catalogs_root() -> Option<PathBuf> {
    super::settings::config_dir().map(|dir| dir.join("plugin-markets"))
}

/// Where the installed plugins are, one folder per plugin, by its id.
pub fn installed_root() -> Option<PathBuf> {
    super::settings::config_dir().map(|dir| dir.join("scripts-market"))
}

/// The marketplaces as the interface knows them.
#[derive(Default)]
pub(crate) struct Markets {
    pub catalogs: Vec<Catalog>,
    /// Where each installed plugin came from, by its id.
    pub installed: HashMap<String, Provenance>,
    /// The sources whose fetch has not answered yet.
    pub fetching: HashSet<String>,
    /// Why a source's last fetch failed.
    pub errors: HashMap<String, String>,
    /// The fetches the user asked for: theirs is said in a bubble, the
    /// start-up's only in the catalog — offline is a normal day.
    asked: HashSet<String>,
    /// The catalogs have been read once: what a fetch compares its head to.
    loaded: bool,
    /// A fetch asked before they were — `Some(asked)` —, run once they are.
    fetch_pending: Option<bool>,
}

/// One marketplace's catalog, read.
#[derive(Clone, Debug)]
pub(crate) struct Catalog {
    /// As the settings name it.
    pub source: String,
    pub slug: String,
    /// What the last fetch wrote; `None` before the first.
    pub meta: Option<CatalogMeta>,
    pub entries: Vec<Entry>,
}

/// A plugin of a catalog.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub folder: String,
    /// The id it is installed under.
    pub id: String,
    /// Its manifest read, or why it cannot be installed.
    pub script: Result<Script, String>,
    /// The fingerprint of its files in the catalog.
    pub print: String,
}

impl Catalog {
    /// The commit its catalog was fetched at, when it is this source's — a
    /// branch changed in the settings makes it another's.
    fn known_commit(&self) -> Option<String> {
        self.meta
            .as_ref()
            .filter(|meta| meta.source == self.source)
            .map(|meta| meta.commit.clone())
    }
}

/// The catalogs of `sources` and the installed plugins' provenance, off the
/// thread.
fn read_all(sources: &[String], language: &str) -> (Vec<Catalog>, HashMap<String, Provenance>) {
    let catalogs = catalogs_root()
        .map(|root| {
            sources
                .iter()
                .filter_map(|source| {
                    let slug = market::parse_source(source).ok()?.slug();
                    let dir = root.join(&slug);
                    let mut folders: Vec<String> = std::fs::read_dir(&dir)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .filter_map(|entry| entry.file_name().into_string().ok())
                        .filter(|name| {
                            crate::scripts::valid_id(name)
                                && dir.join(name).join(crate::scripts::MANIFEST).is_file()
                        })
                        .collect();
                    folders.sort();
                    let entries = folders
                        .into_iter()
                        .map(|folder| Entry {
                            id: market::plugin_id(&slug, &folder),
                            script: market::read_entry(&dir, &folder, language),
                            print: market::print_of(&dir.join(&folder)),
                            folder,
                        })
                        .collect();
                    Some(Catalog {
                        source: source.clone(),
                        meta: market::catalog_meta(&dir),
                        slug,
                        entries,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let installed = installed_root()
        .and_then(|root| std::fs::read_dir(root).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let id = entry.file_name().into_string().ok()?;
            Some((id, market::provenance(&entry.path())?))
        })
        .collect();
    (catalogs, installed)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

impl ClaudhubApp {
    /// Reads the catalogs and the installed plugins again, off the thread.
    pub(super) fn reload_markets(&mut self, cx: &mut Context<Self>) {
        let sources = super::settings::Settings::global(cx).plugin_markets.clone();
        let language = rust_i18n::locale().to_string();
        cx.spawn(async move |this, cx| {
            let (catalogs, installed) = cx
                .background_executor()
                .spawn(async move { read_all(&sources, &language) })
                .await;
            let _ = this.update(cx, |app, cx| {
                let markets = &mut app.scripts.markets;
                markets.catalogs = catalogs;
                markets.installed = installed;
                markets.loaded = true;
                if let Some(asked) = markets.fetch_pending.take() {
                    app.fetch_markets(asked, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Fetches every marketplace — `asked` by the user, or at start-up.
    pub(super) fn fetch_markets(&mut self, asked: bool, cx: &mut Context<Self>) {
        if !self.scripts.markets.loaded {
            let pending = &mut self.scripts.markets.fetch_pending;
            *pending = Some(pending.unwrap_or(false) || asked);
            self.reload_markets(cx);
            return;
        }
        for source in super::settings::Settings::global(cx).plugin_markets.clone() {
            self.fetch_market(&source, asked, cx);
        }
    }

    fn fetch_market(&mut self, source: &str, asked: bool, cx: &mut Context<Self>) {
        let Ok(parsed) = market::parse_source(source) else {
            return;
        };
        let markets = &mut self.scripts.markets;
        if asked {
            markets.asked.insert(source.to_string());
        }
        if !markets.fetching.insert(source.to_string()) {
            return;
        }
        let known = markets
            .catalogs
            .iter()
            .find(|catalog| catalog.source == source)
            .and_then(Catalog::known_commit);
        self.git.send(crate::runtime::Cmd::FetchMarket {
            source: source.to_string(),
            url: parsed.url,
            branch: parsed.branch,
            known,
        });
        cx.notify();
    }

    /// A fetch's answer: a new commit is written as the catalog, off the
    /// thread.
    pub(super) fn market_fetched(
        &mut self,
        source: String,
        fetched: Fetched,
        cx: &mut Context<Self>,
    ) {
        let markets = &mut self.scripts.markets;
        markets.fetching.remove(&source);
        let asked = markets.asked.remove(&source);
        match fetched {
            Fetched::Unchanged => {
                markets.errors.remove(&source);
                if asked {
                    self.announce(tr!("market-up-to-date", { source: source }), cx);
                }
            }
            Fetched::Failed(why) => {
                log::info!("marketplace {source}: {why}");
                markets.errors.insert(source.clone(), why.clone());
                if asked {
                    self.announce_error(tr!("market-failed", { source: source, why: why }), cx);
                }
            }
            Fetched::Snapshot(snapshot) => {
                markets.errors.remove(&source);
                let (Some(root), Ok(parsed)) = (catalogs_root(), market::parse_source(&source))
                else {
                    return;
                };
                let slug = parsed.slug();
                cx.spawn(async move |this, cx| {
                    let written_source = source.clone();
                    let written = cx
                        .background_executor()
                        .spawn(async move {
                            market::write_catalog(&root, &slug, &written_source, &snapshot, now())
                        })
                        .await;
                    let _ = this.update(cx, |app, cx| {
                        match written {
                            Ok(()) if asked => {
                                app.announce(tr!("market-fetched", { source: source }), cx)
                            }
                            Ok(()) => {}
                            Err(error) => app.announce_error(
                                tr!("market-failed", { source: source, why: error.to_string() }),
                                cx,
                            ),
                        }
                        app.reload_markets(cx);
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    /// Adds a marketplace as the user wrote it, and fetches it.
    pub(super) fn add_market(&mut self, text: &str, cx: &mut Context<Self>) {
        let source = text.trim().to_string();
        let parsed = match market::parse_source(&source) {
            Ok(parsed) => parsed,
            Err(why) => return self.announce_error(SharedString::from(why), cx),
        };
        let taken = super::settings::Settings::global(cx)
            .plugin_markets
            .iter()
            .find(|known| {
                market::parse_source(known).is_ok_and(|known| known.slug() == parsed.slug())
            })
            .cloned();
        if let Some(taken) = taken {
            return self.announce_error(tr!("market-already", { source: taken }), cx);
        }
        let added = source.clone();
        super::settings::Settings::update_global(cx, |settings| {
            settings.plugin_markets.push(added);
        });
        self.scripts.markets.catalogs.push(Catalog {
            source: source.clone(),
            slug: parsed.slug(),
            meta: None,
            entries: Vec::new(),
        });
        self.fetch_market(&source, true, cx);
    }

    /// Forgets a marketplace and its catalog. What was installed from it
    /// stays, without its updates.
    pub(super) fn remove_market(&mut self, source: &str, cx: &mut Context<Self>) {
        super::settings::Settings::update_global(cx, |settings| {
            settings.plugin_markets.retain(|known| known != source);
        });
        let markets = &mut self.scripts.markets;
        let slug = markets
            .catalogs
            .iter()
            .find(|catalog| catalog.source == source)
            .map(|catalog| catalog.slug.clone());
        markets.catalogs.retain(|catalog| catalog.source != source);
        markets.errors.remove(source);
        if let (Some(slug), Some(root)) = (slug, catalogs_root()) {
            if self
                .scripts
                .screen
                .catalog
                .as_ref()
                .is_some_and(|(of, _)| *of == slug)
            {
                self.scripts.screen.catalog = None;
            }
            cx.background_executor()
                .spawn(async move {
                    let dir = root.join(slug);
                    if dir.exists() {
                        if let Err(error) = std::fs::remove_dir_all(&dir) {
                            log::warn!("removing the catalog {}: {error}", dir.display());
                        }
                    }
                })
                .detach();
        }
        cx.notify();
    }

    /// Installs plugin `folder` of marketplace `slug` — or brings it up to
    /// the catalog's version —, off the thread.
    pub(super) fn install_plugin(&mut self, slug: &str, folder: &str, cx: &mut Context<Self>) {
        let (Some(from), Some(root)) = (catalogs_root(), installed_root()) else {
            return;
        };
        let Some(catalog) = self
            .scripts
            .markets
            .catalogs
            .iter()
            .find(|catalog| catalog.slug == slug)
        else {
            return;
        };
        let Some(commit) = catalog.meta.as_ref().map(|meta| meta.commit.clone()) else {
            return;
        };
        let title = catalog
            .entries
            .iter()
            .find(|entry| entry.folder == folder)
            .and_then(|entry| entry.script.as_ref().ok())
            .map_or_else(|| folder.to_string(), |script| script.title.clone());
        let (slug, folder) = (slug.to_string(), folder.to_string());
        let id = market::plugin_id(&slug, &folder);
        let updating = self.scripts.markets.installed.contains_key(&id);
        cx.spawn(async move |this, cx| {
            let installed = cx
                .background_executor()
                .spawn(async move {
                    market::install(&from.join(&slug), &slug, &folder, &commit, &root, &id)
                })
                .await;
            let _ = this.update(cx, |app, cx| {
                match installed {
                    Ok(_) if updating => app.announce(tr!("market-updated", { title: title }), cx),
                    Ok(_) => app.announce(tr!("market-installed", { title: title }), cx),
                    Err(error) => app.announce_error(SharedString::from(error.to_string()), cx),
                }
                app.reload_markets(cx);
            });
        })
        .detach();
    }

    /// Uninstalls plugin `id`: its folder, its data and its declared secrets
    /// go — someone else's code leaves nothing behind —, off the thread.
    pub(super) fn uninstall_plugin(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(root) = installed_root() else {
            return;
        };
        let script = self.scripts.script(id).cloned();
        let secrets: Vec<String> = script
            .as_ref()
            .map(|script| {
                script
                    .permissions
                    .secrets
                    .iter()
                    .map(|secret| secret.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        let title = script.map_or_else(|| id.to_string(), |script| script.title);
        let data = Self::store_path(id);
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let of = id.clone();
            let removed = cx
                .background_executor()
                .spawn(async move {
                    std::fs::remove_dir_all(root.join(&of))?;
                    if let Some(data) = data.filter(|data| data.exists()) {
                        std::fs::remove_file(data)?;
                    }
                    for name in &secrets {
                        super::scripts::forget_secret(&of, name);
                    }
                    std::io::Result::Ok(())
                })
                .await;
            let _ = this.update(cx, |app, cx| {
                match removed {
                    Ok(()) => {
                        app.forget_script(&id, cx);
                        app.announce(tr!("market-uninstalled", { title: title }), cx);
                    }
                    Err(error) => app.announce_error(SharedString::from(error.to_string()), cx),
                }
                app.reload_markets(cx);
            });
        })
        .detach();
    }

    /// The catalog's version of installed plugin `id`, when it is another:
    /// `(slug, folder)`.
    pub(super) fn plugin_update(&self, id: &str) -> Option<(String, String)> {
        let installed = self.scripts.markets.installed.get(id)?;
        let catalog = self
            .scripts
            .markets
            .catalogs
            .iter()
            .find(|catalog| catalog.slug == installed.market)?;
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.folder == installed.folder)?;
        (entry.print != installed.print && entry.script.is_ok())
            .then(|| (catalog.slug.clone(), entry.folder.clone()))
    }

    /// Where installed plugin `id` comes from, said short: `owner/repo@abc1234`.
    pub(super) fn plugin_provenance(&self, id: &str) -> Option<SharedString> {
        let installed = self.scripts.markets.installed.get(id)?;
        let source = self
            .scripts
            .markets
            .catalogs
            .iter()
            .find(|catalog| catalog.slug == installed.market)
            .map_or(installed.market.as_str(), |catalog| catalog.source.as_str());
        let commit: String = installed.commit.chars().take(7).collect();
        Some(format!("{source}@{commit}").into())
    }

    /// The catalogs in the Plugins screen's list: each marketplace, its
    /// plugins under it, and what adds or fetches one.
    pub(super) fn market_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let chosen = self.scripts.screen.catalog.clone();
        let fetching = !self.scripts.markets.fetching.is_empty();
        let add = Button::new("market-add")
            .ghost()
            .xsmall()
            .icon(icon("plus"))
            .tooltip(tr!("market-add"))
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_text_dialog(
                    tr!("market-add-title"),
                    tr!("market-add-placeholder"),
                    window,
                    cx,
                    |this, text, _, cx| this.add_market(&text, cx),
                );
            }));
        let refresh = Button::new("market-refresh")
            .ghost()
            .xsmall()
            .icon(icon("refresh-cw"))
            .tooltip(tr!("market-refresh"))
            .disabled(fetching || self.scripts.markets.catalogs.is_empty())
            .on_click(cx.listener(|this, _, _, cx| this.fetch_markets(true, cx)));
        let mut rows: Vec<AnyElement> = Vec::new();
        for catalog in self.scripts.markets.catalogs.clone() {
            let state: SharedString = if self.scripts.markets.fetching.contains(&catalog.source) {
                tr!("market-fetching")
            } else if let Some(why) = self.scripts.markets.errors.get(&catalog.source) {
                // git's own words: an authentication refused, a repository
                // not found — what to fix.
                SharedString::from(why.clone())
            } else if catalog.meta.is_none() {
                tr!("market-never")
            } else {
                tr!("market-plugins", { n: catalog.entries.len() })
            };
            let failed = self.scripts.markets.errors.get(&catalog.source).cloned();
            let source = catalog.source.clone();
            rows.push(
                v_flex()
                    .w_full()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1()
                            .items_center()
                            .child(icon("globe").xsmall().text_color(theme.muted_foreground))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(SharedString::from(catalog.source.clone())),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "market-remove-{}",
                                    catalog.slug
                                )))
                                .ghost()
                                .xsmall()
                                .icon(icon("x"))
                                .tooltip(tr!("market-remove"))
                                .on_click(cx.listener(
                                    move |this, _, _, cx| this.remove_market(&source, cx),
                                )),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(if failed.is_some() {
                                theme.danger
                            } else {
                                theme.muted_foreground
                            })
                            .child(state),
                    )
                    .into_any_element(),
            );
            for entry in &catalog.entries {
                let lit = chosen.as_ref() == Some(&(catalog.slug.clone(), entry.folder.clone()));
                let installed = self.scripts.markets.installed.contains_key(&entry.id);
                let update = installed && self.plugin_update(&entry.id).is_some();
                let title = entry.script.as_ref().map_or_else(
                    |_| SharedString::from(entry.folder.clone()),
                    |script| SharedString::from(script.title.clone()),
                );
                let mark: Option<SharedString> = if update {
                    Some(tr!("market-update-mark"))
                } else if installed {
                    Some(tr!("market-installed-mark"))
                } else {
                    None
                };
                let key = (catalog.slug.clone(), entry.folder.clone());
                rows.push(
                    h_flex()
                        .id(SharedString::from(format!(
                            "market-entry-{}-{}",
                            catalog.slug, entry.folder
                        )))
                        .w_full()
                        .pl_6()
                        .pr_3()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .cursor_pointer()
                        .border_l_2()
                        .border_color(if lit {
                            theme.ring
                        } else {
                            gpui_kit::transparent_black()
                        })
                        .when(lit, |el| el.bg(theme.list_active))
                        .when(!lit, |el| el.hover(|style| style.bg(theme.list_hover)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.scripts.screen.catalog = Some(key.clone());
                            this.scripts.screen.selected = None;
                            this.write_scripts_status(cx);
                            cx.notify();
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .when(entry.script.is_err(), |el| {
                                    el.text_color(theme.muted_foreground)
                                })
                                .child(title),
                        )
                        .children(mark.map(|mark| {
                            div()
                                .text_xs()
                                .text_color(if update { theme.warning } else { theme.ring })
                                .child(mark)
                        }))
                        .into_any_element(),
                );
            }
        }
        v_flex()
            .w_full()
            .mt_2()
            .border_t_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .pt_2()
                    .gap_1()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child(tr!("market-title")),
                    )
                    .child(refresh)
                    .child(add),
            )
            .when(rows.is_empty(), |el| {
                el.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(tr!("market-empty")),
                )
            })
            .children(rows)
            .into_any_element()
    }

    /// The card of a catalog's plugin, in the preview's place: what it is,
    /// where it comes from, what it may do — read before Install, which is
    /// the consent. Nothing of it runs here.
    pub(super) fn market_card(
        &mut self,
        slug: &str,
        folder: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let found = self
            .scripts
            .markets
            .catalogs
            .iter()
            .find(|catalog| catalog.slug == slug)
            .and_then(|catalog| {
                let entry = catalog
                    .entries
                    .iter()
                    .find(|entry| entry.folder == folder)?;
                Some((catalog.clone(), entry.clone()))
            });
        let Some((catalog, entry)) = found else {
            return super::theme::centered_note(tr!("plugins-select"), cx);
        };
        let commit: String = catalog
            .meta
            .as_ref()
            .map(|meta| meta.commit.chars().take(7).collect())
            .unwrap_or_default();
        let source = format!("{}@{commit}  ·  {}", catalog.source, entry.folder);
        let installed = self.scripts.markets.installed.contains_key(&entry.id);
        let update = installed && self.plugin_update(&entry.id).is_some();
        let line = |label: SharedString, text: SharedString| {
            h_flex()
                .w_full()
                .gap_2()
                .items_start()
                .text_sm()
                .child(
                    div()
                        .flex_none()
                        .w(px(110.))
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(text))
        };
        let body = match &entry.script {
            Err(why) => v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(tr!("market-unusable")),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .child(SharedString::from(why.clone())),
                )
                .into_any_element(),
            Ok(script) => {
                let secrets = if script.permissions.secrets.is_empty() {
                    tr!("market-no-secrets")
                } else {
                    SharedString::from(
                        script
                            .permissions
                            .secrets
                            .iter()
                            .map(|secret| secret.label.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                };
                let kind = match script.kind {
                    crate::scripts::Kind::Tab => tr!("settings-script-tab"),
                    crate::scripts::Kind::Home => tr!("settings-script-home"),
                };
                let (slug, folder, id) =
                    (catalog.slug.clone(), entry.folder.clone(), entry.id.clone());
                let action = if !installed {
                    Some(
                        Button::new("market-install")
                            .primary()
                            .small()
                            .icon(icon("download"))
                            .label(tr!("market-install"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.install_plugin(&slug, &folder, cx)
                            }))
                            .into_any_element(),
                    )
                } else if update {
                    Some(
                        Button::new("market-update")
                            .primary()
                            .small()
                            .icon(icon("download"))
                            .label(tr!("market-update"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.install_plugin(&slug, &folder, cx)
                            }))
                            .into_any_element(),
                    )
                } else {
                    None
                };
                let open = installed.then(|| {
                    Button::new("market-open-installed")
                        .ghost()
                        .small()
                        .label(tr!("market-show-installed"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.scripts.screen.selected = Some(id.clone());
                            this.scripts.screen.catalog = None;
                            this.write_scripts_status(cx);
                            cx.notify();
                        }))
                        .into_any_element()
                });
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .child(SharedString::from(script.description.clone())),
                    )
                    .child(line(tr!("market-kind"), kind))
                    .children(script.version.as_ref().map(|version| {
                        line(tr!("market-version"), SharedString::from(version.clone()))
                    }))
                    .child(line(tr!("market-id"), SharedString::from(entry.id.clone())))
                    .child(
                        v_flex()
                            .mt_2()
                            .gap_1p5()
                            .p_3()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.warning)
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(tr!("market-may")),
                            )
                            .child(line(
                                tr!("market-may-network"),
                                tr!("market-may-network-what"),
                            ))
                            .child(line(tr!("market-may-read"), tr!("market-may-read-what")))
                            .child(line(tr!("market-may-secrets"), secrets))
                            .child(line(tr!("market-may-agent"), tr!("market-may-agent-what"))),
                    )
                    .child(
                        h_flex()
                            .mt_2()
                            .gap_2()
                            .children(action)
                            .children(open)
                            .when(installed && !update, |el| {
                                el.child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child(tr!("market-up-to-date-plugin")),
                                )
                            }),
                    )
                    .into_any_element()
            }
        };
        let title = entry.script.as_ref().map_or_else(
            |_| SharedString::from(entry.folder.clone()),
            |script| SharedString::from(script.title.clone()),
        );
        v_flex()
            .size_full()
            .gap_3()
            .child(
                h_flex()
                    .flex_none()
                    .w_full()
                    .h(super::theme::toolbar_height(cx))
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(title),
                    ),
            )
            .child(
                v_flex()
                    .id("market-card")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_2()
                    .p_3()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(source)),
                    )
                    .child(body),
            )
            .into_any_element()
    }
}
