//! The scripts that dress the focus view: a folder each, a `claudhub.json`
//! saying what it is, and a JavaScript entry the gpui-shell runtime loads.
//!
//! A script is either a **tab** — one more view under a board's tabs, beside
//! the git, tests and notes ones — or a **home**, which takes the place of
//! the board's own when the settings name it. Both are written mostly by
//! the agents, so a manifest is read leniently, field by field: a field
//! missing or of another type falls back, and only what makes the folder
//! meaningless — an entry that is not there — refuses it.
//!
//! Some ship **inside Claudhub** — the builtins: written at each start to a
//! folder of their own, which an update rewrites, and **forked** by copying
//! them into the user's folder under the same name, where the copy takes
//! their place — their data, secrets and grants, all filed by id, with it.
//!
//! Pure but for [`discover`], [`stamp`] and the few that write a folder
//! ([`seed`], [`install_builtins`], [`retire`], [`fork`], [`set_aside`]):
//! the UI calls them off its thread.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

/// The file a script's folder is recognised by.
pub const MANIFEST: &str = "claudhub.json";

/// The entry a manifest that names none is loaded from.
pub const DEFAULT_ENTRY: &str = "main.js";

/// Where a script stands in a board.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// One more tab under a board's.
    Tab,
    /// The board's home, when the settings name it.
    Home,
}

/// A script found on disk, not yet run.
#[derive(Clone, Debug, PartialEq)]
pub struct Script {
    /// Its folder's name: what the settings and the store retain.
    pub id: String,
    pub dir: PathBuf,
    pub title: String,
    /// A Lucide icon name, as the interface's own icons are named.
    pub icon: Option<String>,
    pub kind: Kind,
    /// Relative to `dir`.
    pub entry: String,
    pub description: String,
    /// What it asks for beyond drawing — see [`Permissions`].
    pub permissions: Permissions,
    pub origin: Origin,
}

/// Where a script comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    /// The user's folder, and nothing of Claudhub's by that name.
    #[default]
    User,
    /// Shipped inside Claudhub, and brought up to date with it.
    Builtin,
    /// The user's copy of a builtin, standing in its place.
    Fork,
}

/// What a script declares beyond drawing, under its manifest's
/// `permissions`. The network needs no declaring: every script may send
/// HTTP and HTTPS requests anywhere, as an application may — a
/// `permissions.network` written before is read past.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Permissions {
    /// The secrets it keeps in the system keyring, by name, with what the
    /// user is told of each.
    pub secrets: Vec<Secret>,
}

/// A secret a script declares: its name, and what it is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Secret {
    pub name: String,
    pub label: String,
}

/// Whether a folder name can be a script's id: what the settings retain,
/// and what names its data — so nothing that could leave a folder.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// A manifest read: the script, or why the folder is not one. `language`
/// picks the title and the description when they are written per language
/// — `{"fr": "…", "en": "…"}` —, English failing that, then any.
pub fn parse(id: &str, dir: &Path, manifest: &str, language: &str) -> Result<Script, String> {
    if !valid_id(id) {
        return Err(format!(
            "`{id}` is not a script name: letters, digits, `-`, `_` and `.` only"
        ));
    }
    let value: Value =
        serde_json::from_str(manifest).map_err(|error| format!("{MANIFEST}: {error}"))?;
    if !value.is_object() {
        return Err(format!("{MANIFEST} is not a JSON object"));
    }
    let title = localized(&value, "title", language)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(id)
        .to_string();
    let kind = match crate::json::string(&value, "kind").map(str::trim) {
        Some("home") => Kind::Home,
        Some("tab") | None => Kind::Tab,
        Some(other) => {
            return Err(format!(
                "{MANIFEST}: unknown kind `{other}` (`tab` or `home`)"
            ))
        }
    };
    let entry = crate::json::string(&value, "entry")
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .unwrap_or(DEFAULT_ENTRY)
        .to_string();
    if !inside(&entry) {
        return Err(format!(
            "{MANIFEST}: the entry `{entry}` must be a path inside the script's folder"
        ));
    }
    Ok(Script {
        id: id.to_string(),
        dir: dir.to_path_buf(),
        title,
        icon: crate::json::string(&value, "icon")
            .map(str::trim)
            .filter(|icon| !icon.is_empty())
            .map(str::to_string),
        kind,
        entry,
        description: localized(&value, "description", language)
            .unwrap_or_default()
            .to_string(),
        permissions: permissions(&value, language)?,
        origin: Origin::User,
    })
}

/// The `permissions` of a manifest. A secret badly named is refused rather
/// than dropped: the agent that wrote it is told, and fixes it.
fn permissions(value: &Value, language: &str) -> Result<Permissions, String> {
    let Some(asked) = value.get("permissions").filter(|asked| asked.is_object()) else {
        return Ok(Permissions::default());
    };
    let mut secrets: Vec<Secret> = Vec::new();
    for secret in crate::json::items(asked, "secrets") {
        // A name alone, or `{"name": …, "label": …}`.
        let name = secret
            .as_str()
            .or_else(|| crate::json::string(secret, "name"))
            .unwrap_or_default()
            .trim()
            .to_string();
        if !valid_id(&name) {
            return Err(format!(
                "{MANIFEST}: the secret `{name}` needs a name of letters, digits, `-`, `_` and `.`"
            ));
        }
        let label = localized(secret, "label", language)
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .unwrap_or(&name)
            .to_string();
        if !secrets.iter().any(|known| known.name == name) {
            secrets.push(Secret { name, label });
        }
    }
    Ok(Permissions { secrets })
}

/// A text field written once, or once per language.
fn localized<'a>(value: &'a Value, key: &str, language: &str) -> Option<&'a str> {
    match value.get(key)? {
        Value::String(text) => Some(text),
        Value::Object(texts) => [language, "en"]
            .iter()
            .find_map(|language| texts.get(*language).and_then(Value::as_str))
            .or_else(|| texts.values().find_map(Value::as_str)),
        _ => None,
    }
}

/// A relative path that stays under its folder: no root, no `..`.
fn inside(entry: &str) -> bool {
    let path = Path::new(entry);
    !path.is_absolute()
        && !entry.starts_with('/')
        && !entry.starts_with('\\')
        && path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

/// The scripts of a folder, and the folders that meant to be one but are
/// not — `(id, why)` —, each sorted by id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub scripts: Vec<(Script, Stamp)>,
    pub broken: Vec<(String, String)>,
}

/// Every script under `root`, one folder deep. A folder without a manifest
/// is not a script and says nothing; one whose manifest does not read, or
/// whose entry is missing, is listed as broken — the agent that wrote it
/// is told why.
pub fn discover(root: &Path, language: &str) -> Found {
    let mut found = Found::default();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    let mut dirs: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .filter(|(name, _)| !name.starts_with('.'))
        .collect();
    dirs.sort();
    for (id, dir) in dirs {
        let Ok(manifest) = std::fs::read_to_string(dir.join(MANIFEST)) else {
            continue;
        };
        match parse(&id, &dir, &manifest, language) {
            Ok(script) if !dir.join(&script.entry).is_file() => {
                let why = format!("its entry `{}` is missing", script.entry);
                found.broken.push((id, why));
            }
            Ok(script) => {
                let stamp = stamp(&dir);
                found.scripts.push((script, stamp));
            }
            Err(why) => found.broken.push((id, why)),
        }
    }
    found
}

/// The builtins and the user's scripts as one list, by id. A folder of the
/// user's named after a builtin — a script or not — takes its place: it is
/// a fork, and a fork whose manifest breaks is said broken rather than
/// silently replaced by what it forked.
pub fn merge(builtins: Found, user: Found) -> Found {
    let taken: Vec<String> = user
        .scripts
        .iter()
        .map(|(script, _)| script.id.clone())
        .chain(user.broken.iter().map(|(id, _)| id.clone()))
        .collect();
    let taken = |id: &str| taken.iter().any(|known| known == id);
    let shipped: Vec<String> = builtins
        .scripts
        .iter()
        .map(|(script, _)| script.id.clone())
        .chain(builtins.broken.iter().map(|(id, _)| id.clone()))
        .collect();
    let mut found = Found::default();
    for (mut script, stamp) in builtins.scripts {
        if !taken(&script.id) {
            script.origin = Origin::Builtin;
            found.scripts.push((script, stamp));
        }
    }
    for (mut script, stamp) in user.scripts {
        if shipped.contains(&script.id) {
            script.origin = Origin::Fork;
        }
        found.scripts.push((script, stamp));
    }
    found.broken = builtins
        .broken
        .into_iter()
        .filter(|(id, _)| !taken(id))
        .chain(user.broken)
        .collect();
    found.scripts.sort_by(|(a, _), (b, _)| a.id.cmp(&b.id));
    found.broken.sort();
    found
}

/// The ids of a list of shipped files — `<id>/<path>` —, in order, once each.
pub fn ids_of<'a>(files: &[(&'a str, &str)]) -> Vec<&'a str> {
    let mut ids: Vec<&str> = Vec::new();
    for (path, _) in files {
        if let Some((id, _)) = path.split_once('/') {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

/// The files of one shipped script, `<id>/` and all.
fn files_of<'a>(files: &[(&'a str, &'a str)], id: &str) -> Vec<(&'a str, &'a str)> {
    files
        .iter()
        .filter(|(path, _)| path.split_once('/').is_some_and(|(of, _)| of == id))
        .copied()
        .collect()
}

/// The file, in the builtins' folder, holding the fingerprint of what was
/// written there.
const BUILTINS_PRINT: &str = ".version";

/// Writes the builtins under `root` — a folder that is Claudhub's alone —
/// when what is there is not this build's: the folder emptied first, so a
/// file a newer version dropped goes too. True when it wrote.
pub fn install_builtins(root: &Path, files: &[(&str, &str)]) -> std::io::Result<bool> {
    let print = fingerprint(files.iter().map(|(path, content)| (*path, Some(*content))));
    let there = std::fs::read_to_string(root.join(BUILTINS_PRINT)).unwrap_or_default();
    if there.trim() == print && ids_of(files).iter().all(|id| root.join(id).is_dir()) {
        return Ok(false);
    }
    if root.exists() {
        std::fs::remove_dir_all(root)?;
    }
    for (path, content) in files {
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, content)?;
    }
    std::fs::write(root.join(BUILTINS_PRINT), format!("{print}\n"))?;
    Ok(true)
}

/// What the runtime writes beside a script's entry at each load: never the
/// user's work.
const GENERATED: [&str; 2] = ["gpui-kit.d.ts", "jsconfig.json"];

/// Takes away, from the user's folder `root`, a copy of builtin `id` that is
/// no fork: one that was written there as an example and never touched, one
/// whose files are the builtin's own and nothing more, or one that is an
/// `earlier` version of it, by fingerprint. A builtin that lived in the
/// user's folder once would otherwise stand still there, in the place of
/// the one that is brought up to date. True when it did.
pub fn retire(
    root: &Path,
    id: &str,
    files: &[(&str, &str)],
    earlier: &[&str],
) -> std::io::Result<bool> {
    let own = files_of(files, id);
    let dir = root.join(id);
    if own.is_empty() || !dir.is_dir() {
        return Ok(false);
    }
    let on_disk: Vec<(&str, Option<String>)> = own
        .iter()
        .map(|(path, _)| (*path, std::fs::read_to_string(root.join(path)).ok()))
        .collect();
    let current = fingerprint(on_disk.iter().map(|(path, text)| (*path, text.as_deref())));
    let offered_path = root.join(OFFERED);
    let offered = std::fs::read_to_string(&offered_path).unwrap_or_default();
    let seeded_untouched = offered.lines().any(|line| {
        line.split_once(' ')
            .is_some_and(|(of, print)| of == id && print.trim() == current)
    });
    let identical = on_disk
        .iter()
        .zip(&own)
        .all(|((_, there), (_, shipped))| there.as_deref() == Some(*shipped));
    // Anything else in it is the user's: a file of their own makes a fork.
    let named: Vec<String> = own
        .iter()
        .filter_map(|(path, _)| path.split_once('/').map(|(_, rest)| rest.to_string()))
        .chain(GENERATED.iter().map(|name| name.to_string()))
        .collect();
    let only_ours = std::fs::read_dir(&dir)?.flatten().all(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        named.contains(&name)
            || entry.file_type().is_ok_and(|kind| kind.is_dir())
                && own.iter().any(|(path, _)| {
                    path.split_once('/')
                        .is_some_and(|(_, rest)| rest.starts_with(&format!("{name}/")))
                })
    });
    let known = earlier.contains(&current.as_str());
    if !(seeded_untouched || identical || known) || !only_ours {
        return Ok(false);
    }
    std::fs::remove_dir_all(&dir)?;
    let kept: String = offered
        .lines()
        .filter(|line| line.split(' ').next() != Some(id))
        .map(|line| format!("{line}\n"))
        .collect();
    if kept != offered {
        std::fs::write(offered_path, kept)?;
    }
    Ok(true)
}

/// Forks builtin `id`: its files written into the user's folder `root`,
/// under its name. Refused when that folder exists — it is a fork already,
/// or the user's.
pub fn fork(root: &Path, id: &str, files: &[(&str, &str)]) -> std::io::Result<PathBuf> {
    let own = files_of(files, id);
    if own.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("`{id}` is not a builtin"),
        ));
    }
    let dir = root.join(id);
    if dir.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} exists already", dir.display()),
        ));
    }
    for (path, content) in own {
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, content)?;
    }
    Ok(dir)
}

/// Where a fork goes when the user goes back to the builtin: a hidden
/// folder the reading passes by, so nothing written is lost.
pub const SET_ASIDE: &str = ".forks";

/// Moves the user's folder `id` out of the way — under [`SET_ASIDE`], named
/// with `stamp` — and says where.
pub fn set_aside(root: &Path, id: &str, stamp: u64) -> std::io::Result<PathBuf> {
    let aside = root.join(SET_ASIDE);
    std::fs::create_dir_all(&aside)?;
    let target = (1..)
        .map(|n| match n {
            1 => aside.join(format!("{id}-{stamp}")),
            n => aside.join(format!("{id}-{stamp}-{n}")),
        })
        .find(|path| !path.exists())
        .expect("an unbounded range has a free name");
    std::fs::rename(root.join(id), &target)?;
    Ok(target)
}

/// The file, at the scripts' root, naming the examples written, each with
/// the fingerprint of what was written.
const OFFERED: &str = ".examples";

/// A fingerprint of an example's files, stable from one build to the next
/// (FNV-1a) — `std`'s hasher may change with the compiler, and a changed
/// fingerprint reads as an example the user edited.
fn fingerprint<'a>(files: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (path, content) in files {
        for byte in path
            .bytes()
            .chain([0])
            .chain(content.unwrap_or("\u{0}missing").bytes())
            .chain([0])
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

/// Writes the shipped examples — `files` are `<id>/<path>` — and says
/// which. An example is written when the folder has never had it, and
/// **brought up to date** when what is on disk is still exactly what was
/// written; one the user changed, or deleted, is left as it is.
pub fn seed(root: &Path, files: &[(&str, &str)]) -> std::io::Result<Vec<String>> {
    let offered_path = root.join(OFFERED);
    let offered_text = std::fs::read_to_string(&offered_path).unwrap_or_default();
    // `id fingerprint`, or a bare `id` from before fingerprints.
    let mut offered: Vec<(String, Option<String>)> = offered_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| match line.split_once(' ') {
            Some((id, print)) => (id.to_string(), Some(print.trim().to_string())),
            None => (line.to_string(), None),
        })
        .collect();
    let mut written: Vec<String> = Vec::new();
    for id in ids_of(files) {
        let own = files_of(files, id);
        let shipped = fingerprint(own.iter().map(|(path, content)| (*path, Some(*content))));
        let there = root.join(id).exists();
        let write = match offered.iter().find(|(known, _)| known == id) {
            // Never offered: written, unless a folder of that name is the user's.
            None => !there,
            Some((_, recorded)) => {
                let on_disk: Vec<(&str, Option<String>)> = own
                    .iter()
                    .map(|(path, _)| (*path, std::fs::read_to_string(root.join(path)).ok()))
                    .collect();
                let current =
                    fingerprint(on_disk.iter().map(|(path, text)| (*path, text.as_deref())));
                // Gone is the user's choice; changed is the user's work.
                there
                    && recorded.as_deref() != Some(shipped.as_str())
                    && recorded
                        .as_ref()
                        .is_none_or(|recorded| *recorded == current)
            }
        };
        if write {
            for (path, content) in &own {
                let target = root.join(path);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(target, content)?;
            }
            written.push(id.to_string());
        }
        let record = match offered.iter_mut().find(|(known, _)| known == id) {
            Some(entry) => entry,
            None => {
                offered.push((id.to_string(), None));
                offered.last_mut().expect("just pushed")
            }
        };
        if write || record.1.is_none() && !there {
            record.1 = Some(shipped);
        }
    }
    let text: String = offered
        .iter()
        .map(|(id, print)| match print {
            Some(print) => format!("{id} {print}\n"),
            None => format!("{id}\n"),
        })
        .collect();
    if text != offered_text {
        std::fs::create_dir_all(root)?;
        std::fs::write(offered_path, text)?;
    }
    Ok(written)
}

/// The file, at the scripts' root, where Claudhub says how each script
/// fared: the agent that edits them reads it after a save rather than ask.
/// Hidden, so that the reading of the folder passes it by.
pub const STATUS: &str = ".status.md";

/// How a script fared, as far as the boards and the Plugins screen know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fared {
    /// Mounted from its current sources, somewhere.
    Loaded,
    /// Its current sources did not mount: why.
    Failed(String),
    /// Not shown since its sources last changed.
    NotShown,
}

/// One script as [`status_text`] tells it.
pub struct Report<'a> {
    pub script: &'a Script,
    pub fared: Fared,
    /// Whether the user has it on: off, the boards do not show it.
    pub enabled: bool,
}

/// The text of [`STATUS`]: each script and how it fared, the folders that
/// are not scripts and why, and what the Plugins screen has selected.
pub fn status_text(
    scripts: &[Report],
    broken: &[(String, String)],
    selected: Option<&str>,
    preview: Option<&str>,
) -> String {
    let mut out = String::from(
        "# Claudhub scripts — status\n\n\
         Rewritten by Claudhub whenever a script loads, fails or changes. \
         A script is loaded only where it is shown: select it in the Plugins \
         screen to have it loaded and its errors reported here.\n\n",
    );
    match selected {
        Some(id) => out.push_str(&format!("Selected in the Plugins screen: `{id}`")),
        None => out.push_str("Nothing selected in the Plugins screen"),
    }
    if let Some(preview) = preview {
        out.push_str(&format!(", previewed on the worktree `{preview}`"));
    }
    out.push_str(".\n");
    for Report {
        script,
        fared,
        enabled,
    } in scripts
    {
        let kind = match script.kind {
            Kind::Tab => "tab",
            Kind::Home => "home",
        };
        let origin = match script.origin {
            Origin::User => String::new(),
            Origin::Builtin => format!(
                ", builtin — read-only in `{}`, fork it to change it",
                script.dir.display()
            ),
            Origin::Fork => ", fork of the builtin".to_string(),
        };
        out.push_str(&format!(
            "\n## `{}` ({kind}{origin}) — {}\n",
            script.id, script.title
        ));
        if !enabled {
            out.push_str("\nDisabled by the user: no board shows it, the Plugins screen still previews it.\n");
        }
        match fared {
            Fared::Loaded => out.push_str("\nLoaded.\n"),
            Fared::NotShown => out.push_str("\nNot shown since its last change.\n"),
            Fared::Failed(why) => {
                out.push_str("\n**Failed to load:**\n\n```\n");
                out.push_str(why.trim_end());
                out.push_str("\n```\n");
            }
        }
    }
    for (id, why) in broken {
        out.push_str(&format!("\n## `{id}` — not a script\n\n{why}\n"));
    }
    out
}

/// The first free folder name under `root` for a new script: `base`, then
/// `base-2`, `base-3`…
pub fn free_id(root: &Path, base: &str) -> String {
    (1..)
        .map(|n| match n {
            1 => base.to_string(),
            n => format!("{base}-{n}"),
        })
        .find(|id| !root.join(id).exists())
        .expect("an unbounded range has a free name")
}

/// The files of a new script of `kind`: a manifest and an entry that draws
/// something, to be changed.
pub fn skeleton(kind: Kind) -> [(&'static str, String); 2] {
    let (kind_name, title_fr, title_en, body) = match kind {
        Kind::Tab => (
            "tab",
            "Nouvel onglet",
            "New tab",
            "v_flex()\n      .size_full()\n      .gap(12)\n      .p(12)\n      \
             .child(div().text_size(16).font_semibold().child(tree.name))\n      \
             .child(div().text_size(12).text_color(colors.muted_foreground).child(tree.branch ?? \"\"))",
        ),
        Kind::Home => (
            "home",
            "Nouvel accueil",
            "New home",
            "h_flex()\n      .size_full()\n      .gap(12)\n      \
             .child(v_flex().flex_1().min_w_0().gap(12).overflow_y_scrollbar()\n        \
             .child(BranchCard.new(\"branch\"))\n        .child(ChangesCard.new(\"changes\")))\n      \
             .child(v_flex().flex_1().min_w_0().child(Terminals.new(\"terminals\")))",
        ),
    };
    let manifest = format!(
        "{{\n  \"title\": {{ \"fr\": \"{title_fr}\", \"en\": \"{title_en}\" }},\n  \
         \"kind\": \"{kind_name}\",\n  \"icon\": \"layout-dashboard\"\n}}\n"
    );
    let entry = format!(
        "// A Claudhub script — see the `Scripts` section of the claudhub skill,\n\
         // and `gpui-kit.d.ts` beside this file for every signature.\n\n\
         import {{ View, div }} from \"gpui-kit\";\n\
         import {{ h_flex, v_flex }} from \"gpui-base\";\n\
         import {{ worktree, BranchCard, ChangesCard, Terminals }} from \"claudhub\";\n\n\
         export default class Script extends View {{\n  \
         render(cx) {{\n    \
         const colors = cx.theme().colors;\n    \
         const tree = worktree();\n    \
         return {body};\n  \
         }}\n\
         }}\n"
    );
    [(MANIFEST, manifest), (DEFAULT_ENTRY, entry)]
}

/// The files an agent reads of itself in the folder it starts in — Claude
/// Code's, Codex's, Gemini's —: how the Plugins screen's agent is told what
/// it is there for, whichever it is. A chat has no system prompt to carry it.
pub const INSTRUCTIONS: [&str; 3] = ["CLAUDE.md", "AGENTS.md", "GEMINI.md"];

/// The first line of the instructions Claudhub writes: a file of one of
/// those names without it is the user's, and is never written over.
const INSTRUCTIONS_MARK: &str =
    "<!-- Written by Claudhub, rewritten when the Plugins agent starts. -->";

/// Whether an instructions file is one Claudhub wrote.
pub fn is_our_instructions(text: &str) -> bool {
    text.starts_with(INSTRUCTIONS_MARK)
}

/// What the agent of the Plugins screen is told before anything: where it
/// is, what it may touch, how it learns which script the user is looking at
/// and whether a save worked.
pub fn instructions(root: &Path, builtins: &Path) -> String {
    let root = root.display();
    let builtins = builtins.display();
    format!(
        "{INSTRUCTIONS_MARK}\n\n\
         # Claudhub scripts\n\n\
         You are editing Claudhub's own interface: the scripts of its focus view, in \
         `{root}` (also `$CLAUDHUB_SCRIPTS`). Each folder is one script — a `claudhub.json` \
         and a JavaScript entry run by Claudhub's embedded runtime.\n\n\
         - The `Scripts` section of the claudhub skill gives the format and the `claudhub` \
         module; each script's `gpui-kit.d.ts` gives every signature. Read them before \
         writing code. `{builtins}/sentry/` is a complete example.\n\
         - Work only inside this folder.\n\
         - The builtin scripts ship with Claudhub, in `{builtins}` (also \
         `$CLAUDHUB_BUILTIN_SCRIPTS`), rewritten at each update: never edit them there. To \
         change one, fork it — copy its folder here under the same name (the Plugins \
         screen's Fork does it) —: the copy takes its place, its data and secrets with it, \
         and stops following updates.\n\
         - `{STATUS}` says which script the user has selected in the Plugins screen — start \
         there unless asked otherwise — and how each script fared.\n\
         - Claudhub reloads a script within a second of a save. After each save, wait a \
         second and read `{STATUS}`: it says whether the script loaded and, if not, the \
         error. Fix it before saying you are done.\n\
         - Never edit `{STATUS}`, `gpui-kit.d.ts` nor this file: Claudhub writes them.\n\
         - A script reaches the network with `fetch`, to any host, nothing to declare; it \
         keeps its credentials with `set_secret`, never in its storage.\n"
    )
}

/// Writes the instructions in `root` under each of [`INSTRUCTIONS`]' names,
/// over a file Claudhub wrote and never over the user's.
pub fn write_instructions(root: &Path, builtins: &Path) -> std::io::Result<()> {
    let text = instructions(root, builtins);
    std::fs::create_dir_all(root)?;
    for name in INSTRUCTIONS {
        let path = root.join(name);
        match std::fs::read_to_string(&path) {
            Ok(there) if !is_our_instructions(&there) => continue,
            Ok(there) if there == text => continue,
            _ => std::fs::write(path, &text)?,
        }
    }
    Ok(())
}

/// What a script's sources look like from outside: a change to one of them
/// changes this, which is what a reload waits for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stamp {
    newest: Option<SystemTime>,
    files: usize,
    bytes: u64,
}

/// How deep a script's sources are looked for.
const STAMP_DEPTH: usize = 6;
/// Past this many sources, a folder is not stamped further.
const STAMP_FILES: usize = 2048;

/// The stamp of a script's folder: its `.js`, `.mjs` and manifest, without
/// `node_modules` nor hidden folders — the runtime writes `gpui-kit.d.ts`
/// in it at every load, which must not reload it again.
pub fn stamp(dir: &Path) -> Stamp {
    let mut stamp = Stamp::default();
    let mut pending = vec![(dir.to_path_buf(), 0usize)];
    while let Some((folder, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth < STAMP_DEPTH && name != "node_modules" {
                    pending.push((entry.path(), depth + 1));
                }
                continue;
            }
            if !is_source(&name) || stamp.files >= STAMP_FILES {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            stamp.files += 1;
            stamp.bytes = stamp.bytes.saturating_add(metadata.len());
            if let Ok(modified) = metadata.modified() {
                stamp.newest = stamp.newest.max(Some(modified));
            }
        }
    }
    stamp
}

fn is_source(name: &str) -> bool {
    name == MANIFEST || name.ends_with(".js") || name.ends_with(".mjs")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(manifest: &str) -> Result<Script, String> {
        parse("board", Path::new("/scripts/board"), manifest, "fr")
    }

    #[test]
    fn a_title_per_language_is_read_in_the_interfaces() {
        let manifest = r#"{"title": {"en": "Agents", "fr": "Les agents"},
                           "description": {"de": "Nur Deutsch"}}"#;
        let script = read(manifest).unwrap();
        assert_eq!(script.title, "Les agents");
        assert_eq!(script.description, "Nur Deutsch");
        let english = parse("board", Path::new("/b"), manifest, "it").unwrap();
        assert_eq!(english.title, "Agents");
    }

    #[test]
    fn an_empty_manifest_is_a_tab_named_after_its_folder() {
        let script = read("{}").unwrap();
        assert_eq!(script.title, "board");
        assert_eq!(script.kind, Kind::Tab);
        assert_eq!(script.entry, DEFAULT_ENTRY);
        assert_eq!(script.icon, None);
    }

    #[test]
    fn the_fields_are_read_one_by_one() {
        let script = read(
            r#"{"title": " CI ", "kind": "home", "icon": "activity",
                "entry": "src/app.js", "description": null, "extra": 1}"#,
        )
        .unwrap();
        assert_eq!(script.title, "CI");
        assert_eq!(script.kind, Kind::Home);
        assert_eq!(script.icon.as_deref(), Some("activity"));
        assert_eq!(script.entry, "src/app.js");
        assert_eq!(script.description, "");
    }

    #[test]
    fn a_field_of_another_type_falls_back() {
        let script = read(r#"{"title": 3, "icon": "", "entry": false}"#).unwrap();
        assert_eq!(script.title, "board");
        assert_eq!(script.icon, None);
        assert_eq!(script.entry, DEFAULT_ENTRY);
    }

    #[test]
    fn an_unknown_kind_is_refused_rather_than_guessed() {
        assert!(read(r#"{"kind": "panel"}"#).unwrap_err().contains("panel"));
    }

    #[test]
    fn an_entry_cannot_leave_its_folder() {
        for entry in ["../x.js", "/etc/x.js", "a/../../x.js", "\\\\host\\x.js"] {
            let manifest = format!(r#"{{"entry": {entry:?}}}"#);
            assert!(read(&manifest).is_err(), "{entry}");
        }
        assert!(read(r#"{"entry": "lib/main.mjs"}"#).is_ok());
    }

    #[test]
    fn a_manifest_that_is_not_an_object_is_refused() {
        assert!(read("[]").is_err());
        assert!(read("not json").is_err());
    }

    #[test]
    fn the_status_says_each_script_and_its_error() {
        let (tab, home) = (read("{}").unwrap(), read(r#"{"kind": "home"}"#).unwrap());
        let text = status_text(
            &[
                Report {
                    script: &tab,
                    fared: Fared::Loaded,
                    enabled: true,
                },
                Report {
                    script: &home,
                    fared: Fared::Failed("SyntaxError: x\n".into()),
                    enabled: false,
                },
            ],
            &[("old".into(), "its entry `main.js` is missing".into())],
            Some("board"),
            Some("/w"),
        );
        assert!(text
            .contains("Selected in the Plugins screen: `board`, previewed on the worktree `/w`."));
        assert!(text.contains("## `board` (tab) — board\n\nLoaded."));
        assert!(text.contains("**Failed to load:**\n\n```\nSyntaxError: x\n```"));
        assert!(text.contains("## `old` — not a script"));
        assert!(text.contains("Disabled by the user"));
    }

    #[test]
    fn a_skeleton_reads_as_a_script_of_its_kind() {
        for kind in [Kind::Tab, Kind::Home] {
            let [(manifest_name, manifest), (entry_name, entry)] = skeleton(kind);
            assert_eq!((manifest_name, entry_name), (MANIFEST, DEFAULT_ENTRY));
            let script = parse("new", Path::new("/s/new"), &manifest, "en").unwrap();
            assert_eq!(script.kind, kind);
            assert!(entry.contains("export default class"));
        }
    }

    #[test]
    fn the_secrets_are_read_and_a_network_list_read_past() {
        let script = read(
            r#"{"permissions": {
                "network": ["sentry.io", "not a host"],
                "secrets": ["token", {"name": "dsn", "label": {"fr": "Le DSN", "en": "DSN"}}]
            }}"#,
        )
        .unwrap();
        let secrets: Vec<(&str, &str)> = script
            .permissions
            .secrets
            .iter()
            .map(|secret| (secret.name.as_str(), secret.label.as_str()))
            .collect();
        assert_eq!(secrets, [("token", "token"), ("dsn", "Le DSN")]);
        assert_eq!(read("{}").unwrap().permissions, Permissions::default());
        assert!(read(r#"{"permissions": {"secrets": ["a/b"]}}"#).is_err());
    }

    #[test]
    fn the_instructions_are_ours_and_say_where_to_look() {
        let text = instructions(Path::new("/c/scripts"), Path::new("/c/scripts-builtin"));
        assert!(is_our_instructions(&text));
        assert!(text.contains("`/c/scripts`"));
        assert!(text.contains("`/c/scripts-builtin/sentry/`"));
        assert!(text.contains(STATUS));
        assert!(!is_our_instructions("# My own CLAUDE.md\n"));
    }

    #[test]
    fn a_users_folder_takes_a_builtins_place() {
        let at = |id: &str, root: &str| {
            let script = parse(id, &Path::new(root).join(id), "{}", "en").unwrap();
            (script, Stamp::default())
        };
        let builtins = Found {
            scripts: vec![at("http", "/b"), at("sentry", "/b"), at("tetris", "/b")],
            broken: Vec::new(),
        };
        let user = Found {
            scripts: vec![at("mine", "/u"), at("sentry", "/u")],
            broken: vec![("tetris".into(), "no entry".into())],
        };
        let found = merge(builtins, user);
        let listed: Vec<(&str, &str, Origin)> = found
            .scripts
            .iter()
            .map(|(script, _)| {
                let root = if script.dir.starts_with("/b") {
                    "/b"
                } else {
                    "/u"
                };
                (script.id.as_str(), root, script.origin)
            })
            .collect();
        assert_eq!(
            listed,
            [
                ("http", "/b", Origin::Builtin),
                ("mine", "/u", Origin::User),
                ("sentry", "/u", Origin::Fork),
            ]
        );
        // A fork that breaks is said broken, not replaced by the builtin.
        assert_eq!(
            found.broken,
            [("tetris".to_string(), "no entry".to_string())]
        );
    }

    #[test]
    fn an_id_names_one_folder_and_nothing_else() {
        assert!(valid_id("ci-board_2.1"));
        for id in ["", ".hidden", "a/b", "a b", "..", "é"] {
            assert!(!valid_id(id), "{id}");
        }
    }
}
