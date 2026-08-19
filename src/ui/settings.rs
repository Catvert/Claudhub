//! Réglages persistants.
//!
//! Un seul fichier JSON dans le répertoire de configuration de l'utilisateur.
//! Tout y est optionnel : une clé absente reprend sa valeur par défaut, ce qui
//! rend l'ajout d'un réglage compatible avec les fichiers déjà écrits, et un
//! fichier illisible n'empêche jamais Perch de démarrer.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Light,
    #[default]
    Dark,
    /// Suit le réglage clair/sombre du système.
    System,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    /// Programme lancé dans un nouvel onglet. Vide = le shell de connexion.
    pub shell: String,
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
            font_size: 13.0,
            // 10 000 lignes : de quoi remonter la sortie d'une suite de tests
            // sans que la mémoire d'une dizaine d'onglets se remarque.
            scrollback: 10_000,
            agent_command: "claude".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeMode,
    pub language: LanguageChoice,
    pub font_size: f32,
    pub start_maximized: bool,
    pub terminal: TerminalSettings,
    /// Dépôts rouverts au démarrage, dans l'ordre d'ouverture.
    pub repositories: Vec<PathBuf>,
    /// Nombre de lignes de contexte autour d'un hunk dans la revue.
    pub diff_context: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::default(),
            language: LanguageChoice::default(),
            font_size: 14.0,
            start_maximized: false,
            terminal: TerminalSettings::default(),
            repositories: Vec::new(),
            diff_context: 3,
        }
    }
}

impl Settings {
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
}
