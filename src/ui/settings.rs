//! Réglages persistants.
//!
//! Un seul fichier JSON dans le répertoire de configuration de l'utilisateur.
//! Tout y est optionnel : une clé absente reprend sa valeur par défaut, ce qui
//! rend l'ajout d'un réglage compatible avec les fichiers déjà écrits, et un
//! fichier illisible n'empêche jamais Perch de démarrer.
//!
//! Les réglages vivent dans un global gpui plutôt que dans `PerchApp` : le
//! formulaire de gpui-component lit et écrit chaque champ à travers des
//! fermetures qui ne reçoivent qu'un `App`, sans accès à l'entité racine.
//! Passer par un global est ce qui permet d'écrire un réglage depuis
//! n'importe où — le formulaire, un raccourci de zoom, la molette.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{App, BorrowAppContext};
use serde::{Deserialize, Serialize};

/// Police d'interface embarquée. Elle est toujours disponible, ce qui en fait
/// le seul défaut qu'on puisse promettre.
pub const DEFAULT_UI_FONT: &str = "Inter";
/// Police à chasse fixe embarquée, celle du terminal et des diffs.
pub const DEFAULT_MONO_FONT: &str = "JetBrains Mono";

/// Palettes de repli : celles que gpui-component fournit, et qui existent donc
/// même si le répertoire de thèmes est vide ou illisible.
pub const DEFAULT_LIGHT_THEME: &str = "Default Light";
pub const DEFAULT_DARK_THEME: &str = "Default Dark";

/// Délai avant écriture du fichier après une modification.
///
/// Un champ de saisie émet une valeur par frappe et la molette un cran par
/// encoche : écrire à chaque fois ferait des dizaines d'ouvertures de fichier
/// pour un geste. Une demi-seconde est assez courte pour qu'une fermeture
/// brutale ne perde rien de visible.
const SAVE_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Light,
    #[default]
    Dark,
    /// Suit le réglage clair/sombre du système.
    System,
}

impl ThemeMode {
    /// Valeur telle qu'elle circule dans le formulaire, qui ne manipule que
    /// des chaînes.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "light" => Self::Light,
            "system" => Self::System,
            _ => Self::Dark,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LanguageChoice {
    #[default]
    System,
    Fr,
    En,
}

impl LanguageChoice {
    pub fn to_lang_id(self) -> &'static str {
        match self {
            Self::Fr => "fr",
            Self::En => "en",
            Self::System => {
                // `fr-BE`, `fr_FR.UTF-8` : seule la langue nous intéresse.
                let locale = std::env::var("LC_ALL")
                    .or_else(|_| std::env::var("LC_MESSAGES"))
                    .or_else(|_| std::env::var("LANG"))
                    .unwrap_or_default();
                if locale.starts_with("fr") {
                    "fr"
                } else {
                    "en"
                }
            }
        }
    }

    pub fn as_key(self) -> &'static str {
        match self {
            Self::Fr => "fr",
            Self::En => "en",
            Self::System => "system",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "fr" => Self::Fr,
            "en" => Self::En,
            _ => Self::System,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    /// Programme lancé dans un nouvel onglet. Vide = le shell de connexion.
    pub shell: String,
    /// Police du terminal. Vide = celle des diffs, pour que régler la chasse
    /// fixe une fois suffise au cas courant.
    pub font_family: String,
    pub font_size: f32,
    pub scrollback: usize,
    /// Commande de l'agent de codage, lancée par le bouton dédié dans un
    /// onglet du worktree sélectionné.
    pub agent_command: String,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            shell: String::new(),
            font_family: String::new(),
            font_size: 13.0,
            // 10 000 lignes : de quoi remonter la sortie d'une suite de tests
            // sans que la mémoire d'une dizaine d'onglets se remarque.
            scrollback: 10_000,
            agent_command: "claude".into(),
        }
    }
}

impl TerminalSettings {
    /// Le programme à lancer, découpé en commande et arguments.
    ///
    /// Rend `None` pour un réglage vide, ce qui laisse alacritty ouvrir le
    /// shell de connexion tel que `/etc/passwd` le déclare. Le réglage accepte
    /// une ligne de commande entière — `fish -l`, `tmux new-session -A -s
    /// perch` — parce qu'un shell nu n'est pas toujours ce qu'on veut ouvrir.
    pub fn program(&self) -> Option<(String, Vec<String>)> {
        let mut parts = self.shell.split_whitespace().map(str::to_string);
        let program = parts.next()?;
        Some((program, parts.collect()))
    }

    /// Police effective : la sienne, sinon celle des diffs.
    pub fn family<'a>(&'a self, mono: &'a str) -> &'a str {
        if self.font_family.is_empty() {
            mono
        } else {
            &self.font_family
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeMode,
    /// Palettes nommées, une par apparence. Deux réglages et non un seul :
    /// `theme` dit s'il fait clair ou sombre — le système peut en décider —
    /// et ceux-ci disent *quelle* palette porte chacune des deux apparences.
    pub light_theme: String,
    pub dark_theme: String,
    pub language: LanguageChoice,
    /// Police de l'interface : libellés, menus, listes.
    pub ui_font_family: String,
    /// Police à chasse fixe : diffs, sha, chemins alignés.
    pub mono_font_family: String,
    pub font_size: f32,
    /// Taille du texte des diffs, indépendante de celle de l'interface : on
    /// grossit du code pour le relire sans vouloir grossir toute la fenêtre.
    pub diff_font_size: f32,
    pub start_maximized: bool,
    pub terminal: TerminalSettings,
    /// Dépôts rouverts au démarrage, dans l'ordre d'ouverture.
    pub repositories: Vec<PathBuf>,
    /// Nombre de lignes de contexte autour d'un hunk dans la revue.
    pub diff_context: usize,
    /// Liste des fichiers en arborescence de dossiers plutôt qu'à plat.
    pub review_tree: bool,
    /// Diff en deux colonnes — ancienne version à gauche, nouvelle à droite —
    /// plutôt qu'en une seule liste.
    pub diff_split: bool,
    /// Afficher le fichier entier autour des modifications, et non seulement
    /// leurs quelques lignes de contexte.
    pub diff_whole_file: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::default(),
            light_theme: DEFAULT_LIGHT_THEME.into(),
            dark_theme: DEFAULT_DARK_THEME.into(),
            language: LanguageChoice::default(),
            ui_font_family: DEFAULT_UI_FONT.into(),
            mono_font_family: DEFAULT_MONO_FONT.into(),
            font_size: 14.0,
            diff_font_size: 13.0,
            start_maximized: false,
            terminal: TerminalSettings::default(),
            repositories: Vec::new(),
            diff_context: 3,
            review_tree: true,
            diff_split: false,
            diff_whole_file: false,
        }
    }
}

/// Contexte demandé à git pour « tout le fichier ».
///
/// `git diff` n'a pas d'option « fichier entier » : on lui demande un contexte
/// plus grand que n'importe quel fichier, qu'il ramène de lui-même à ce qui
/// existe.
pub const WHOLE_FILE_CONTEXT: usize = 1_000_000;

impl Settings {
    /// Lignes de contexte à demander pour le fichier affiché.
    pub fn context_lines(&self) -> usize {
        if self.diff_whole_file {
            WHOLE_FILE_CONTEXT
        } else {
            self.diff_context
        }
    }

    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                // Écraser un fichier qu'on n'a pas su lire ferait perdre des
                // réglages à cause d'une seule clé mal formée : on repart des
                // valeurs par défaut sans rien effacer.
                log::warn!("réglages illisibles ({}) : {e}", path.display());
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                log::warn!("lecture des réglages : {e}");
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        let Some(path) = settings_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = write_private(&path, &json) {
                    log::warn!("écriture des réglages : {e}");
                }
            }
            Err(e) => log::warn!("sérialisation des réglages : {e}"),
        }
    }

    /// Ajoute un dépôt à la liste rouverte au démarrage, sans doublon et en
    /// tête : le dernier ouvert est celui qu'on rouvrira en premier.
    pub fn remember_repository(&mut self, main: &Path) {
        self.repositories.retain(|p| p != main);
        self.repositories.insert(0, main.to_path_buf());
        self.repositories.truncate(20);
    }

    pub fn forget_repository(&mut self, main: &Path) {
        self.repositories.retain(|p| p != main);
    }

    /// Police effective de l'interface, jamais vide.
    pub fn ui_font(&self) -> &str {
        non_empty(&self.ui_font_family, DEFAULT_UI_FONT)
    }

    /// Police effective à chasse fixe, jamais vide.
    pub fn mono_font(&self) -> &str {
        non_empty(&self.mono_font_family, DEFAULT_MONO_FONT)
    }

    /// Police effective du terminal.
    pub fn terminal_font(&self) -> &str {
        self.terminal.family(self.mono_font())
    }
}

fn non_empty<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

/// Zone dont la taille de texte se règle indépendamment.
///
/// Deux zones et non une seule : grossir la sortie d'un agent pour la lire ne
/// doit pas déplacer le code qu'on relit à côté.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    Diff,
    Terminal,
}

/// En dessous de huit points le texte n'est plus lisible, au-dessus de
/// trente-deux une seule ligne de diff occupe la vue : ce sont les deux façons
/// de rendre l'interface inutilisable, et la molette y arrive vite.
pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 32.0;

pub fn clamp_font_size(value: f32) -> f32 {
    value.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
}

impl Settings {
    fn size_of(&mut self, zone: Zoom) -> &mut f32 {
        match zone {
            Zoom::Diff => &mut self.diff_font_size,
            Zoom::Terminal => &mut self.terminal.font_size,
        }
    }

    /// Ajoute `steps` points à la taille d'une zone. Rend vrai si quelque
    /// chose a changé — au bout de la course, il n'y a rien à réafficher.
    pub fn zoom(&mut self, zone: Zoom, steps: f32) -> bool {
        let size = self.size_of(zone);
        let next = clamp_font_size(*size + steps);
        let changed = next != *size;
        *size = next;
        changed
    }

    pub fn reset_zoom(&mut self, zone: Zoom) -> bool {
        let default = Settings::default();
        let target = match zone {
            Zoom::Diff => default.diff_font_size,
            Zoom::Terminal => default.terminal.font_size,
        };
        let size = self.size_of(zone);
        let changed = *size != target;
        *size = target;
        changed
    }
}

// --- Global -----------------------------------------------------------------

pub struct SettingsStore {
    settings: Settings,
    /// Vrai quand une écriture différée est déjà programmée : les
    /// modifications suivantes s'y agrègent au lieu d'en programmer une autre.
    saving: bool,
}

impl gpui::Global for SettingsStore {}

impl Settings {
    /// Installe les réglages chargés. À appeler une fois, au démarrage.
    pub fn init_global(self, cx: &mut App) {
        cx.set_global(SettingsStore {
            settings: self,
            saving: false,
        });
    }

    pub fn global(cx: &App) -> &Settings {
        &cx.global::<SettingsStore>().settings
    }

    /// Modifie les réglages, applique ce qui se voit tout de suite et
    /// programme l'écriture.
    ///
    /// Le thème est ré-appliqué à chaque modification, y compris celles qui ne
    /// le concernent pas : c'est lui qui porte polices et taille de texte, et
    /// distinguer les champs qui l'affectent des autres coûterait plus de code
    /// qu'un `refresh_windows` de trop.
    pub fn update_global(cx: &mut App, f: impl FnOnce(&mut Settings)) {
        let before = Self::global(cx).clone();
        cx.update_global::<SettingsStore, _>(|store, _| f(&mut store.settings));
        let after = Self::global(cx).clone();
        if after == before {
            return;
        }
        if after.language != before.language {
            crate::ui::set_language(after.language);
        }
        crate::ui::theme::apply(&after, None, cx);
        schedule_save(cx);
    }
}

fn schedule_save(cx: &mut App) {
    let already_scheduled =
        cx.update_global::<SettingsStore, _>(|store, _| std::mem::replace(&mut store.saving, true));
    if already_scheduled {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(SAVE_DELAY).await;
        let settings = cx.update(|cx| {
            cx.update_global::<SettingsStore, _>(|store, _| {
                store.saving = false;
                store.settings.clone()
            })
        });
        if let Ok(settings) = settings {
            settings.save();
        }
    })
    .detach();
}

// --- Choix proposés dans le formulaire --------------------------------------

/// Fragments de noms qui trahissent une police à chasse fixe.
///
/// Il n'existe pas d'interrogation portable de cette propriété dans gpui : on
/// se rabat sur la convention de nommage, qui couvre les familles qu'un
/// développeur a installées. La liste reste modifiable à la main dans le
/// fichier de réglages pour les cas qu'elle rate.
const MONO_HINTS: [&str; 14] = [
    "mono",
    "code",
    "consol",
    "courier",
    "menlo",
    "hack",
    "iosevka",
    "terminus",
    "inconsolata",
    "monaco",
    "andale",
    "jetbrains",
    "cascadia",
    "monospace",
];

pub fn is_monospace_name(name: &str) -> bool {
    let name = name.to_lowercase();
    MONO_HINTS.iter().any(|hint| name.contains(hint))
}

/// Familles proposées pour un champ de police.
///
/// La police embarquée est toujours présente, même si le système ne la connaît
/// pas : c'est celle qui sert de recours, et un choix qu'on ne peut pas
/// reprendre après en être parti serait un piège.
pub fn font_choices(installed: &[String], monospace_only: bool, embedded: &str) -> Vec<String> {
    let mut names: Vec<String> = installed
        .iter()
        // Les polices d'icônes et les variantes privées commencent par un
        // point sur macOS ; elles n'ont rien à faire dans une liste de choix.
        .filter(|name| !name.starts_with('.'))
        .filter(|name| !monospace_only || is_monospace_name(name))
        .cloned()
        .collect();
    names.push(embedded.to_string());
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup_by_key(|name| name.to_lowercase());
    names
}

/// Shells déclarés par le système, tels que `/etc/shells` les liste.
pub fn parse_shells(text: &str) -> Vec<String> {
    let mut shells: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('/'))
        .map(str::to_string)
        .collect();
    shells.dedup();
    shells
}

/// Shells proposés : ceux du système qui existent encore sur le disque.
pub fn available_shells() -> Vec<String> {
    let text = std::fs::read_to_string("/etc/shells").unwrap_or_default();
    parse_shells(&text)
        .into_iter()
        .filter(|shell| Path::new(shell).exists())
        .collect()
}

/// Où la disposition des panneaux est enregistrée.
///
/// À part des réglages : ce n'est pas une préférence qu'on écrit à la main
/// mais l'état d'une fenêtre, volumineux et illisible, et un utilisateur qui
/// ouvre `settings.json` n'a pas à le trouver là.
pub fn layout_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("be", "acetics", "perch")
        .map(|dirs| dirs.config_dir().join("layout.json"))
}

fn settings_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("be", "acetics", "perch")
        .map(|dirs| dirs.config_dir().join("settings.json"))
}

/// Écrit en 0600 sous Unix : le fichier porte les chemins des dépôts et la
/// ligne de commande de l'agent, qui ne regardent pas les autres comptes de la
/// machine.
#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `mode` ne s'applique qu'à la création : un fichier écrit par une version
    // antérieure garderait ses permissions d'origine.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(contents.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keys_take_their_defaults() {
        // Un fichier écrit par une version qui ignorait `terminal` doit
        // continuer de se charger.
        let s: Settings = serde_json::from_str(r#"{"theme":"light"}"#).unwrap();
        assert_eq!(s.theme, ThemeMode::Light);
        assert_eq!(s.terminal.scrollback, 10_000);
        assert_eq!(s.diff_context, 3);
        // Les champs ajoutés après coup aussi : c'est le cas de tous les
        // fichiers déjà écrits sur le disque des utilisateurs.
        assert_eq!(s.ui_font_family, DEFAULT_UI_FONT);
        assert_eq!(s.mono_font_family, DEFAULT_MONO_FONT);
    }

    #[test]
    fn recent_repositories_stay_unique_and_ordered() {
        let mut s = Settings::default();
        s.remember_repository(Path::new("/a"));
        s.remember_repository(Path::new("/b"));
        s.remember_repository(Path::new("/a"));
        assert_eq!(
            s.repositories,
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
        s.forget_repository(Path::new("/a"));
        assert_eq!(s.repositories, vec![PathBuf::from("/b")]);
    }

    #[test]
    fn system_language_follows_the_environment() {
        // La valeur exacte dépend de l'environnement de test ; seule la forme
        // compte : deux lettres connues du catalogue.
        assert!(matches!(LanguageChoice::System.to_lang_id(), "fr" | "en"));
        assert_eq!(LanguageChoice::Fr.to_lang_id(), "fr");
    }

    #[test]
    fn an_emptied_font_falls_back_instead_of_disappearing() {
        // Vider le champ dans le formulaire ne doit pas rendre le texte
        // invisible : c'est le geste naturel avant de saisir autre chose.
        let mut s = Settings {
            ui_font_family: "  ".into(),
            mono_font_family: String::new(),
            ..Default::default()
        };
        assert_eq!(s.ui_font(), DEFAULT_UI_FONT);
        assert_eq!(s.mono_font(), DEFAULT_MONO_FONT);
        // Le terminal sans police propre suit celle des diffs.
        assert_eq!(s.terminal_font(), DEFAULT_MONO_FONT);
        s.mono_font_family = "Iosevka".into();
        assert_eq!(s.terminal_font(), "Iosevka");
        s.terminal.font_family = "Terminus".into();
        assert_eq!(s.terminal_font(), "Terminus");
    }

    #[test]
    fn zooming_stops_at_the_bounds_and_says_so() {
        let mut s = Settings::default();
        assert!(s.zoom(Zoom::Diff, 2.));
        assert_eq!(s.diff_font_size, 15.0);
        // Le terminal ne bouge pas avec les diffs.
        assert_eq!(s.terminal.font_size, 13.0);

        assert!(s.zoom(Zoom::Diff, 1_000.));
        assert_eq!(s.diff_font_size, MAX_FONT_SIZE);
        // Au bout de la course, rien à réafficher : c'est ce que dit le faux.
        assert!(!s.zoom(Zoom::Diff, 1.));

        assert!(s.reset_zoom(Zoom::Diff));
        assert_eq!(s.diff_font_size, Settings::default().diff_font_size);
        assert!(!s.reset_zoom(Zoom::Diff));
    }

    #[test]
    fn the_shell_setting_becomes_a_command_line() {
        let mut s = TerminalSettings::default();
        // Vide : c'est le shell de connexion, et personne d'autre n'a à
        // décider lequel.
        assert_eq!(s.program(), None);
        s.shell = "   ".into();
        assert_eq!(s.program(), None);

        s.shell = "fish".into();
        assert_eq!(s.program(), Some(("fish".into(), vec![])));

        // Une ligne de commande entière : ouvrir directement une session tmux
        // est un usage courant, et un shell nu n'est pas toujours ce qu'on
        // veut.
        s.shell = "tmux new-session -A -s perch".into();
        assert_eq!(
            s.program(),
            Some((
                "tmux".into(),
                vec![
                    "new-session".into(),
                    "-A".into(),
                    "-s".into(),
                    "perch".into()
                ]
            ))
        );
    }

    #[test]
    fn monospace_names_are_recognised_by_convention() {
        for name in ["JetBrains Mono", "Fira Code", "Consolas", "Iosevka Term"] {
            assert!(is_monospace_name(name), "{name}");
        }
        for name in ["Inter", "Fira Sans", "Helvetica"] {
            assert!(!is_monospace_name(name), "{name}");
        }
    }

    #[test]
    fn font_choices_are_sorted_deduplicated_and_always_offer_the_default() {
        let installed = vec![
            "Fira Code".to_string(),
            "Inter".to_string(),
            ".SF NS Mono".to_string(),
            "fira code".to_string(),
        ];
        let mono = font_choices(&installed, true, DEFAULT_MONO_FONT);
        assert_eq!(mono, vec!["Fira Code", DEFAULT_MONO_FONT]);

        // Sans le filtre, l'interface voit tout — sauf les familles privées.
        let all = font_choices(&installed, false, DEFAULT_UI_FONT);
        assert_eq!(all, vec!["Fira Code", "Inter"]);
    }

    #[test]
    fn shells_ignore_comments_and_blank_lines() {
        let text = "# /etc/shells\n\n/bin/sh\n/bin/bash\n /usr/bin/fish \n";
        assert_eq!(
            parse_shells(text),
            vec!["/bin/sh", "/bin/bash", "/usr/bin/fish"]
        );
    }
}
