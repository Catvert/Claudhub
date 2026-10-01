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
//! Pure but for [`discover`] and [`stamp`], which read a folder: the UI
//! calls both off its thread.

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
}

/// What a script asks for beyond drawing, as its manifest declares it under
/// `permissions`. Asking grants nothing: the hosts are reached only once
/// the user has allowed each, in the Plugins screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Permissions {
    /// The hosts it would send HTTPS requests to, lower-case.
    pub network: Vec<String>,
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

/// Whether a text names a host, and nothing else: no scheme, no port, no
/// path — a grant is per host, and `https` is the only scheme granted.
pub fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with('.')
        && !host.ends_with('.')
        && !host.contains("..")
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'))
}

/// A host as a manifest or a script writes it: trimmed, lower-case, and a
/// `https://` or a trailing `/` forgiven. `None` when it is not one.
pub fn host_of(text: &str) -> Option<String> {
    let text = text.trim();
    let text = text.strip_prefix("https://").unwrap_or(text);
    let host = text.trim_end_matches('/').to_ascii_lowercase();
    valid_host(&host).then_some(host)
}

/// The hosts among `wanted` the user has not allowed, in order, once each.
pub fn pending_hosts<'a>(
    wanted: impl IntoIterator<Item = &'a String>,
    granted: &[String],
) -> Vec<String> {
    let mut pending: Vec<String> = Vec::new();
    for host in wanted {
        if !granted.contains(host) && !pending.contains(host) {
            pending.push(host.clone());
        }
    }
    pending
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
    })
}

/// The `permissions` of a manifest. A host that is not one is refused
/// rather than dropped: the agent that wrote it is told, and fixes it.
fn permissions(value: &Value, language: &str) -> Result<Permissions, String> {
    let Some(asked) = value.get("permissions").filter(|asked| asked.is_object()) else {
        return Ok(Permissions::default());
    };
    let mut network: Vec<String> = Vec::new();
    for host in crate::json::items(asked, "network") {
        let text = host.as_str().unwrap_or_default();
        let host = host_of(text).ok_or_else(|| {
            format!("{MANIFEST}: `{text}` is not a host — write `api.example.com`")
        })?;
        if !network.contains(&host) {
            network.push(host);
        }
    }
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
    Ok(Permissions { network, secrets })
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

/// The file, at the scripts' root, naming the examples already written once.
const OFFERED: &str = ".examples";

/// Writes the shipped examples `root` has never been offered — `files` are
/// `<id>/<path>` —, and says which. An example is offered once: deleted, it
/// does not come back; a new one arrives with the version that ships it.
pub fn seed(root: &Path, files: &[(&str, &str)]) -> std::io::Result<Vec<String>> {
    let offered_path = root.join(OFFERED);
    let offered = std::fs::read_to_string(&offered_path).unwrap_or_default();
    let offered: Vec<&str> = offered.lines().map(str::trim).collect();
    let mut written: Vec<String> = Vec::new();
    for (path, content) in files {
        let Some((id, _)) = path.split_once('/') else {
            continue;
        };
        if offered.contains(&id) {
            continue;
        }
        // A folder of that name already there is the user's.
        if !written.iter().any(|done| done == id) && root.join(id).exists() {
            continue;
        }
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, content)?;
        if !written.iter().any(|done| done == id) {
            written.push(id.to_string());
        }
    }
    let mut ids: Vec<String> = offered.iter().map(|id| id.to_string()).collect();
    for (path, _) in files {
        if let Some((id, _)) = path.split_once('/') {
            if !ids.iter().any(|known| known == id) {
                ids.push(id.to_string());
            }
        }
    }
    if ids.len() != offered.len() {
        std::fs::create_dir_all(root)?;
        std::fs::write(offered_path, ids.join("\n") + "\n")?;
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

/// The text of [`STATUS`]: each script and how it fared, the folders that
/// are not scripts and why, and what the Plugins screen has selected.
pub fn status_text(
    scripts: &[(&Script, Fared, Vec<String>)],
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
    for (script, fared, pending) in scripts {
        let kind = match script.kind {
            Kind::Tab => "tab",
            Kind::Home => "home",
        };
        out.push_str(&format!(
            "\n## `{}` ({kind}) — {}\n",
            script.id, script.title
        ));
        match fared {
            Fared::Loaded => out.push_str("\nLoaded.\n"),
            Fared::NotShown => out.push_str("\nNot shown since its last change.\n"),
            Fared::Failed(why) => {
                out.push_str("\n**Failed to load:**\n\n```\n");
                out.push_str(why.trim_end());
                out.push_str("\n```\n");
            }
        }
        if !pending.is_empty() {
            out.push_str(&format!(
                "\nWaiting for the user to allow these hosts in the Plugins screen \
                 (its `fetch` to them fails until then): {}.\n",
                pending
                    .iter()
                    .map(|host| format!("`{host}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
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

/// What the agent of the Plugins screen is told before anything: where it
/// is, what it may touch, and how it learns whether a save worked.
pub fn editing_prompt(root: &Path, selected: Option<&str>) -> String {
    let root = root.display();
    let selected = selected
        .map(|id| {
            format!(" The user has the script `{id}` selected: start there unless asked otherwise.")
        })
        .unwrap_or_default();
    format!(
        "You are editing Claudhub's own interface: the scripts of its focus view, \
         in {root} (also $CLAUDHUB_SCRIPTS). Each folder is one script — a `claudhub.json` \
         and a JavaScript entry run by Claudhub's embedded runtime; the `Scripts` section \
         of the claudhub skill gives the format and the `claudhub` module, and each \
         script's `gpui-kit.d.ts` gives every signature: read them before writing code. \
         Work only inside this folder. Claudhub reloads a script within a second of a \
         save; after each save, wait a second and read {root}/{STATUS}, which says \
         whether it loaded and, if not, the error — fix it before saying you are done. \
         Never edit {STATUS} or gpui-kit.d.ts: Claudhub writes them. A script that \
         needs the network declares its hosts in `permissions.network` and keeps \
         its credentials with `set_secret`: tell the user to allow the hosts in \
         the Plugins screen — the status lists those still waiting.{selected}"
    )
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
                (&tab, Fared::Loaded, vec!["sentry.io".into()]),
                (&home, Fared::Failed("SyntaxError: x\n".into()), Vec::new()),
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
        assert!(text.contains("allow these hosts in the Plugins screen"));
        assert!(text.contains("`sentry.io`"));
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
    fn the_permissions_are_read_and_hosts_forgiven_their_scheme() {
        let script = read(
            r#"{"permissions": {
                "network": ["https://Sentry.io/", "us.sentry.io", "sentry.io"],
                "secrets": ["token", {"name": "dsn", "label": {"fr": "Le DSN", "en": "DSN"}}]
            }}"#,
        )
        .unwrap();
        assert_eq!(script.permissions.network, ["sentry.io", "us.sentry.io"]);
        let secrets: Vec<(&str, &str)> = script
            .permissions
            .secrets
            .iter()
            .map(|secret| (secret.name.as_str(), secret.label.as_str()))
            .collect();
        assert_eq!(secrets, [("token", "token"), ("dsn", "Le DSN")]);
        assert_eq!(read("{}").unwrap().permissions, Permissions::default());
    }

    #[test]
    fn a_host_that_is_not_one_is_refused() {
        for host in ["http://x.io", "x.io:8080", "x.io/api", "*.x.io", "", "a..b"] {
            let manifest = format!(r#"{{"permissions": {{"network": [{host:?}]}}}}"#);
            assert!(read(&manifest).is_err(), "{host}");
        }
        assert!(read(r#"{"permissions": {"secrets": ["a/b"]}}"#).is_err());
    }

    #[test]
    fn what_is_pending_is_what_was_not_allowed() {
        let wanted = ["a.io".to_string(), "b.io".to_string(), "a.io".to_string()];
        assert_eq!(pending_hosts(&wanted, &["b.io".to_string()]), ["a.io"]);
        assert!(pending_hosts(&wanted, &wanted).is_empty());
    }

    #[test]
    fn an_id_names_one_folder_and_nothing_else() {
        assert!(valid_id("ci-board_2.1"));
        for id in ["", ".hidden", "a/b", "a b", "..", "é"] {
            assert!(!valid_id(id), "{id}");
        }
    }
}
