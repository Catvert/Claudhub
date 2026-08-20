//! Réglages persistants.
//!
//! Un seul fichier JSON dans le répertoire de configuration de l'utilisateur.
//! Tout y est optionnel : une clé absente reprend sa valeur par défaut, ce qui
//! rend l'ajout d'un réglage compatible avec les fichiers déjà écrits, et un
//! fichier illisible n'empêche jamais Claudhub de démarrer.
//!
//! Les réglages vivent dans un global gpui plutôt que dans `ClaudhubApp` : le
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

/// Ce qui rédige un message de commit quand rien n'est configuré.
///
/// `-p` fait de `claude` un filtre : il lit son prompt sur l'entrée standard
/// et écrit sa réponse sur la sortie, sans session ni interface. Le modèle est
/// nommé parce qu'un résumé de diff n'a pas de quoi occuper le plus gros, et
/// que celui-ci répond en quelques secondes.
pub const DEFAULT_COMMIT_MESSAGE_COMMAND: &str = "claude -p --model sonnet";

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

/// Un agent de codage qu'on sait lancer.
///
/// Plusieurs profils et non un seul réglage : dès qu'on envoie du texte à un
/// agent, on veut choisir lequel. `env` est ce qui porte le modèle
/// (`ANTHROPIC_MODEL`, une clé par profil…) — « configurer plusieurs modèles »
/// n'appelle donc aucune dépendance HTTP, seulement une variable de plus.
///
/// `command` et `args` sont **séparés** : découper une ligne de commande sur
/// les espaces casse sur tout chemin qui en contient un, et c'est le genre de
/// panne qu'on ne comprend qu'après avoir lu le code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentProfile {
    /// Ce que le menu affiche. Vide, c'est le nom du programme qui sert.
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// Variables ajoutées à l'environnement du pty.
    ///
    /// `BTreeMap` et non `HashMap` : JSON sérialisé dans un ordre différent à
    /// chaque écriture ferait un fichier qui change sans que rien n'ait changé.
    pub env: std::collections::BTreeMap<String, String>,
}

impl AgentProfile {
    /// Le profil livré par défaut, celui qui donne son nom au projet.
    pub fn claude() -> Self {
        Self {
            name: "claude".into(),
            command: "claude".into(),
            args: Vec::new(),
            env: Default::default(),
        }
    }

    /// Un profil bâti à partir d'une ligne de commande entière.
    pub fn from_command_line(line: &str) -> Option<Self> {
        let mut parts = split_command(line).into_iter();
        let command = parts.next()?;
        let name = command_name(&command).to_string();
        Some(Self {
            name,
            command,
            args: parts.collect(),
            env: Default::default(),
        })
    }

    /// Le nom affiché : le sien, sinon celui du programme.
    pub fn label(&self) -> &str {
        non_empty(&self.name, &self.command)
    }

    pub fn spawn(&self) -> (String, Vec<String>) {
        (self.command.clone(), self.args.clone())
    }

    /// La ligne de commande telle qu'on la saisit, guillemets remis.
    pub fn command_line(&self) -> String {
        join_command(
            std::iter::once(self.command.as_str()).chain(self.args.iter().map(String::as_str)),
        )
    }

    /// L'environnement tel qu'on le saisit : `CLÉ=valeur`, séparés par des
    /// espaces, avec les mêmes règles de guillemets que la ligne de commande.
    pub fn env_line(&self) -> String {
        join_command(self.env.iter().map(|(key, value)| format!("{key}={value}")))
    }

    pub fn set_env_line(&mut self, line: &str) {
        self.env = split_command(line)
            .into_iter()
            .filter_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                (!key.is_empty()).then(|| (key.to_string(), value.to_string()))
            })
            .collect();
    }
}

/// Le nom d'un programme, dépouillé de son chemin.
fn command_name(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}

/// Découpe une ligne de commande en honorant les guillemets.
///
/// `split_whitespace` casse sur tout chemin contenant une espace — et sous
/// Windows comme sous macOS, c'est le cas courant. Les règles sont celles d'un
/// shell POSIX réduites à l'essentiel : `'…'` littéral, `"…"` avec échappement
/// par contre-oblique, contre-oblique hors guillemets.
pub fn split_command(line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut chars = line.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('\''), c) => current.push(c),
            (Some(_), '\\') => current.push(chars.next().unwrap_or('\\')),
            (Some(_), c) => current.push(c),
            (None, '\'') | (None, '"') => {
                quote = Some(c);
                // Un argument vide est un argument : `--sep ''` en est un.
                started = true;
            }
            (None, '\\') => current.push(chars.next().unwrap_or('\\')),
            (None, c) if c.is_whitespace() => {
                if started || !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => current.push(c),
        }
    }
    if started || !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Recompose une ligne de commande à partir de ses morceaux.
///
/// L'aller-retour avec `split_command` doit être fidèle : le formulaire écrit
/// des morceaux et les relit en une ligne, et un chemin avec une espace ne
/// doit pas se scinder en deux au premier passage.
pub fn join_command(parts: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    parts
        .into_iter()
        .map(|part| {
            let part = part.as_ref();
            // La contre-oblique aussi : hors guillemets elle échappe, et un
            // chemin Windows perdrait les siennes au premier aller-retour.
            if part.is_empty()
                || part
                    .chars()
                    .any(|c| c.is_whitespace() || c == '\'' || c == '"' || c == '\\')
            {
                format!("\"{}\"", part.replace('\\', "\\\\").replace('"', "\\\""))
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
    /// Ancien réglage, remplacé par `agents`. Il n'est plus lu qu'une fois, à
    /// la migration, puis vidé — mais il reste déclaré, faute de quoi le
    /// fichier d'un utilisateur qui n'a pas encore migré perdrait sa commande.
    pub agent_command: String,
    /// Les agents qu'on sait lancer, dans l'ordre du menu.
    pub agents: Vec<AgentProfile>,
    /// Nom du profil lancé par défaut. Vide, ou inconnu : le premier.
    pub default_agent: String,
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
            // Vide par défaut : `migrate_agents` la remplit, ce qui fait du
            // même code le chemin de l'installation neuve et celui de la
            // reprise d'un fichier écrit par une version antérieure.
            agents: Vec::new(),
            default_agent: String::new(),
        }
    }
}

impl TerminalSettings {
    /// Le programme à lancer, découpé en commande et arguments.
    ///
    /// Rend `None` pour un réglage vide, ce qui laisse alacritty ouvrir le
    /// shell de connexion tel que `/etc/passwd` le déclare. Le réglage accepte
    /// une ligne de commande entière — `fish -l`, `tmux new-session -A -s
    /// claudhub` — parce qu'un shell nu n'est pas toujours ce qu'on veut ouvrir.
    pub fn program(&self) -> Option<(String, Vec<String>)> {
        let mut parts = split_command(&self.shell).into_iter();
        let program = parts.next()?;
        Some((program, parts.collect()))
    }

    /// Reprend `agent_command` sous forme de profil.
    ///
    /// Sans risque : `#[serde(default)]` fait qu'un fichier sans `agents` est
    /// lu avec une liste vide, et c'est exactement le cas qu'on rattrape ici.
    /// L'ancien champ est vidé pour que la reprise n'ait lieu qu'une fois.
    pub fn migrate_agents(&mut self) {
        if !self.agents.is_empty() {
            self.agent_command.clear();
            return;
        }
        self.agents = AgentProfile::from_command_line(&self.agent_command)
            .map(|profile| vec![profile])
            .unwrap_or_else(|| vec![AgentProfile::claude()]);
        self.agent_command.clear();
    }

    /// Le profil lancé quand on ne dit pas lequel.
    pub fn default_profile(&self) -> Option<&AgentProfile> {
        self.agents
            .iter()
            .find(|profile| profile.label() == self.default_agent)
            .or_else(|| self.agents.first())
    }

    /// Les noms de programme à chercher dans `/proc`.
    ///
    /// Tous les profils et non le seul courant : un agent lancé depuis un
    /// terminal à côté compte autant que celui qu'on a démarré ici, et n'en
    /// chercher qu'un n'en verrait qu'un sur deux.
    pub fn agent_programs(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .agents
            .iter()
            .map(|profile| command_name(&profile.command).to_string())
            .filter(|name| !name.is_empty())
            .collect();
        names.sort();
        names.dedup();
        names
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
    /// Renvoyer les lignes longues à la ligne, en vue à deux colonnes.
    ///
    /// Vrai par défaut, et c'est là que ça compte : une colonne fait la moitié
    /// de la vue, et la moindre ligne un peu longue obligeait à défiler
    /// horizontalement tout le fichier — la largeur étant celle de la plus
    /// longue ligne, une seule suffisait à la donner à toutes. En une seule
    /// colonne, la ligne dispose de toute la largeur et le repli se discute ;
    /// il n'a donc pas lieu.
    pub diff_wrap: bool,
    /// « Mettre à jour depuis la base » rejoue la branche au lieu de fusionner.
    ///
    /// Les deux se défendent et le choix est une habitude d'équipe : un
    /// historique linéaire d'un côté, la trace de ce qui a été intégré et
    /// quand de l'autre. Merge par défaut, parce qu'il ne réécrit rien et ne
    /// casse donc jamais une branche déjà poussée.
    pub update_with_rebase: bool,
    /// Intégrer force un commit de fusion, même quand l'avance rapide serait
    /// possible : c'est ce qui garde une trace de la branche d'agent.
    pub integrate_no_ff: bool,
    /// Commande de l'éditeur externe, avec `{path}` et `{line}`.
    ///
    /// L'édition dans Claudhub reste légère : une retouche courte ici, le vrai
    /// travail dans l'éditeur de son choix. Vide, le geste disparaît des menus
    /// plutôt que d'échouer.
    pub external_editor: String,
    /// Montrer aussi les fichiers que `.gitignore` écarte, dans l'explorateur.
    pub show_ignored_files: bool,
    /// Minutes entre deux `git fetch` automatiques. `0` : aucun.
    ///
    /// Sans lui, « en retard de trois commits » n'apparaît qu'après un fetch
    /// demandé à la main — c'est-à-dire quand on se doutait déjà de quelque
    /// chose. Un relevé régulier est ce qui rend le compte du bouton digne de
    /// confiance ; c'est ce que font les clients git de bureau, et ils le
    /// règlent tous.
    pub auto_fetch_minutes: u32,
    /// Ce qui rédige un message de commit à partir du diff indexé.
    ///
    /// Un programme et non une API : c'est la décision de cadrage de Claudhub,
    /// et elle vaut ici comme pour les agents du terminal. Le diff lui arrive
    /// par l'entrée standard, sa sortie standard est le message. Un modèle
    /// rapide suffit — d'où `sonnet` par défaut, la rédaction d'un résumé
    /// n'ayant pas de quoi occuper le plus gros. Vide, le bouton disparaît.
    pub commit_message_command: String,
    /// Organisation Sentry. Le *projet*, lui, dépend du dépôt et vit dans le
    /// magasin d'état, pas ici.
    pub sentry_org: String,
    /// Jeton d'API, à défaut de `SENTRY_TOKEN`.
    ///
    /// L'environnement l'emporte : ce fichier est en 0600, ce qui ne fait pas
    /// de lui un coffre, et un jeton qui traîne dans une sauvegarde de
    /// configuration est un jeton qui fuit.
    pub sentry_token: String,
    /// Requête envoyée à Sentry. Vide : les issues non résolues.
    pub sentry_query: String,
    /// Parcourir les diffs et l'arborescence avec les touches de vim.
    ///
    /// Désactivé par défaut, et il faut que ça le reste : ces liaisons sont
    /// des **lettres nues**, et elles prennent la place de tout ce qu'une
    /// lettre pourrait faire ailleurs. Ce n'est pas un mode d'édition — il n'y
    /// a rien à éditer dans un diff — mais la main gauche sur la rangée de
    /// repos pour relire.
    pub vim_mode: bool,
    /// Où les notes de relecture sont écrites, un fichier Markdown par note.
    ///
    /// Vide : `<config>/notes`. Le champ existe pour pointer un coffre —
    /// Obsidian, ou n'importe quel dossier qu'on tient déjà — parce qu'une
    /// note de relecture est du texte qu'on écrit, et qu'elle n'a rien à faire
    /// enfermée dans un JSON d'état que rien d'autre ne sait lire.
    pub notes_dir: String,
    /// Vues que l'utilisateur a masquées, par nom de panneau du dock.
    ///
    /// Ici et non dans `layout.json` : c'est un choix qu'on fait une fois — je
    /// ne me sers pas de Sentry, je ne veux pas des branches — pas la
    /// géométrie d'une fenêtre. `LAYOUT_VERSION` jette la disposition dès que
    /// les panneaux changent de nom, et ce choix-là n'a pas à disparaître
    /// avec elle.
    ///
    /// Une liste de noms plutôt qu'un booléen par panneau : un panneau ajouté
    /// est visible sans que ce fichier ait à le savoir.
    pub hidden_panels: Vec<String>,
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
            diff_wrap: true,
            update_with_rebase: false,
            integrate_no_ff: true,
            external_editor: String::new(),
            show_ignored_files: false,
            auto_fetch_minutes: 10,
            commit_message_command: DEFAULT_COMMIT_MESSAGE_COMMAND.into(),
            sentry_org: String::new(),
            sentry_token: String::new(),
            sentry_query: String::new(),
            vim_mode: false,
            notes_dir: String::new(),
            hidden_panels: Vec::new(),
        }
    }
}

impl Settings {
    /// Le délai entre deux fetch automatiques, ou `None` quand il n'y en a pas.
    pub fn auto_fetch_period(&self) -> Option<std::time::Duration> {
        (self.auto_fetch_minutes > 0)
            .then(|| std::time::Duration::from_secs(u64::from(self.auto_fetch_minutes) * 60))
    }

    /// La racine des notes, `~` développé.
    ///
    /// Un chemin qu'on saisit dans un formulaire s'écrit `~/Coffre/…` — c'est
    /// ainsi qu'on le donne à un shell —, et le passer tel quel à `std::fs`
    /// créerait un dossier nommé `~` dans le répertoire courant.
    pub fn notes_root(&self) -> Option<PathBuf> {
        let text = self.notes_dir.trim();
        if text.is_empty() {
            return config_dir().map(|dir| dir.join("notes"));
        }
        match text.strip_prefix("~/") {
            Some(rest) => directories::UserDirs::new().map(|dirs| dirs.home_dir().join(rest)),
            None => Some(PathBuf::from(text)),
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
        let mut settings = Self::read();
        settings.terminal.migrate_agents();
        settings
    }

    fn read() -> Self {
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
        settings.save();
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
/// Le répertoire où Claudhub range ce qu'il retient : réglages, disposition
/// des panneaux, thèmes.
pub fn config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("be", "acetics", "claudhub")
        .map(|dirs| dirs.config_dir().to_path_buf())
}

pub fn layout_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("layout.json"))
}

fn settings_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("settings.json"))
}

/// Reprend la configuration écrite sous l'ancien nom du projet.
///
/// Perch est devenu Claudhub, et le répertoire de configuration porte le nom
/// du projet : sans reprise, un utilisateur retrouverait au lancement une
/// fenêtre neuve, ses dépôts et sa disposition oubliés. Le déplacement est
/// fait une seule fois — s'il existe déjà un répertoire au nouveau nom, c'est
/// que la reprise a eu lieu, ou que l'utilisateur a commencé avec Claudhub.
///
/// À appeler avant la première lecture des réglages. Ce code n'a de raison
/// d'être que le temps que les installations existantes soient passées.
pub fn migrate_from_perch() {
    let (Some(new), Some(old)) = (
        config_dir(),
        directories::ProjectDirs::from("be", "acetics", "perch")
            .map(|d| d.config_dir().to_path_buf()),
    ) else {
        return;
    };
    if new == old || new.exists() || !old.exists() {
        return;
    }
    if let Err(e) = std::fs::rename(&old, &new) {
        log::warn!("reprise de l'ancienne configuration : {e}");
        return;
    }
    // La disposition nomme ses panneaux, et ces noms ont changé avec le
    // projet : sans cette réécriture, le dock ne retrouverait aucun de ses
    // panneaux et repartirait de la disposition par défaut.
    if let Some(path) = layout_path() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let _ = std::fs::write(&path, text.replace("Perch", "Claudhub"));
        }
    }
    // Les thèmes livrés sont réécrits sous leur nouveau nom au démarrage : les
    // anciens feraient double emploi dans la liste, avec les mêmes noms.
    if let Ok(entries) = std::fs::read_dir(new.join("themes")) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("perch-") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    log::info!("configuration reprise depuis {}", old.display());
}

/// Écrit en 0600 sous Unix : le fichier porte les chemins des dépôts et la
/// ligne de commande de l'agent, qui ne regardent pas les autres comptes de la
/// machine.
///
/// Partagé avec le magasin d'état (`store.rs`), qui porte des notes de
/// relecture et n'a pas plus à être lisible par tout le monde.
#[cfg(unix)]
pub(super) fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
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
pub(super) fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
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
        s.shell = "tmux new-session -A -s claudhub".into();
        assert_eq!(
            s.program(),
            Some((
                "tmux".into(),
                vec![
                    "new-session".into(),
                    "-A".into(),
                    "-s".into(),
                    "claudhub".into()
                ]
            ))
        );
    }

    #[test]
    fn a_command_line_survives_quotes_and_spaces() {
        // Le défaut que ce découpage corrige : un chemin contenant une espace.
        assert_eq!(
            split_command(r#""/opt/mon agent/bin/agent" --model "gpt 5""#),
            vec!["/opt/mon agent/bin/agent", "--model", "gpt 5"]
        );
        // Guillemets simples, littéraux.
        assert_eq!(
            split_command("sh -c 'echo un deux'"),
            vec!["sh", "-c", "echo un deux"]
        );
        // Un argument vide en est un.
        assert_eq!(split_command("agent --sep ''"), vec!["agent", "--sep", ""]);
        assert_eq!(split_command("   "), Vec::<String>::new());
    }

    #[test]
    fn a_command_line_round_trips() {
        for line in [
            "claude",
            r#""/opt/mon agent/bin/agent" --model "gpt 5""#,
            r#"agent --say "il dit \"non\"""#,
            r#""C:\Program Files\agent.exe""#,
        ] {
            let parts = split_command(line);
            assert_eq!(split_command(&join_command(&parts)), parts, "{line}");
        }
    }

    #[test]
    fn the_old_agent_command_becomes_a_profile() {
        // Le fichier d'un utilisateur qui n'a jamais vu les profils. Le chemin
        // y est entre guillemets, ce que l'ancien découpage ne savait pas
        // lire : la reprise en profite pour le remettre d'aplomb.
        let mut terminal: TerminalSettings =
            serde_json::from_str(r#"{"agent_command":"\"/opt/a b/claude\" --resume"}"#).unwrap();
        terminal.migrate_agents();
        assert_eq!(terminal.agents.len(), 1);
        assert_eq!(terminal.agents[0].command, "/opt/a b/claude");
        assert_eq!(terminal.agents[0].args, vec!["--resume"]);
        assert_eq!(terminal.agents[0].label(), "claude");
        // Vidé : la reprise n'a lieu qu'une fois.
        assert!(terminal.agent_command.is_empty());

        // Une installation neuve passe par le même chemin.
        let mut fresh = TerminalSettings::default();
        fresh.migrate_agents();
        assert_eq!(fresh.agents, vec![AgentProfile::claude()]);

        // Un fichier qui a déjà des profils n'est pas touché.
        let mut kept = TerminalSettings {
            agents: vec![AgentProfile {
                name: "aider".into(),
                command: "aider".into(),
                ..Default::default()
            }],
            agent_command: "claude".into(),
            ..Default::default()
        };
        kept.migrate_agents();
        assert_eq!(kept.agents.len(), 1);
        assert_eq!(kept.agents[0].name, "aider");
    }

    #[test]
    fn the_programs_to_look_for_are_all_the_profiles() {
        let terminal = TerminalSettings {
            agents: vec![
                AgentProfile {
                    name: "opus".into(),
                    command: "/usr/bin/claude".into(),
                    ..Default::default()
                },
                AgentProfile {
                    name: "sonnet".into(),
                    command: "claude".into(),
                    ..Default::default()
                },
                AgentProfile {
                    name: "aider".into(),
                    command: "aider".into(),
                    ..Default::default()
                },
            ],
            default_agent: "sonnet".into(),
            ..Default::default()
        };
        // Dédoublonné sur le nom du programme : deux profils du même agent ne
        // le font pas compter deux fois dans /proc.
        assert_eq!(terminal.agent_programs(), vec!["aider", "claude"]);
        assert_eq!(terminal.default_profile().unwrap().name, "sonnet");
    }

    #[test]
    fn an_environment_line_round_trips() {
        let mut profile = AgentProfile::claude();
        profile.set_env_line(r#"ANTHROPIC_MODEL=opus PROMPT="deux mots""#);
        assert_eq!(profile.env.get("ANTHROPIC_MODEL").unwrap(), "opus");
        assert_eq!(profile.env.get("PROMPT").unwrap(), "deux mots");
        let mut again = AgentProfile::claude();
        again.set_env_line(&profile.env_line());
        assert_eq!(again.env, profile.env);
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
