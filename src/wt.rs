//! Ce que le `wt.toml` d'un projet ajoute à Claudhub.
//!
//! C'est le système d'extension, et il ne coûte rien : un projet déclare ses
//! dossiers à créer, ses fichiers à hériter, ses ports, ses hooks et ses
//! tâches, et Claudhub les affiche **sans les connaître**. Une commande de
//! démarrage Laravel et un `cargo watch` passent par le même code.
//!
//! `wt` est une dépendance, pas un sous-processus : le dépôt est le nôtre, et
//! parser la sortie de sa CLI — alignée, colorée et traduite — reviendrait à
//! lire ce qui est fait pour un humain.
//!
//! **Rien ici ne doit être appelé depuis le thread d'interface.** Un
//! `[[prompt]]` avec `source` lance un shell, un `post_new` peut durer des
//! minutes, et `up` démarre des conteneurs. Tout passe par un worker.
//!
//! Le partage des rôles avec le terminal, qui n'est pas évident : ce qui tient
//! une comptabilité — création, suppression, `up`, `down` — passe par la
//! bibliothèque, qui alloue les ports et écrit l'état ; les `[tasks.*]`, elles,
//! sont des commandes du projet, souvent interactives, et partent dans un
//! onglet de terminal, qui sait déjà transmettre les frappes et rendre les
//! couleurs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

// `::wt` désigne la bibliothèque, non ce module-ci : les deux portent le même
// nom, et le préfixe lève l'ambiguïté pour qui lit autant que pour le
// compilateur.
use ::wt::config::{Ask, Project, PromptKind};
use ::wt::ops::App;
use ::wt::{ops, state, tmpl, util};

/// Ce qu'un projet déclare, réduit à ce que la vue affiche.
///
/// Un instantané de données nues plutôt que l'`App` de `wt` : la vue n'a pas à
/// tenir un objet qui sait lancer des shells, et le reconstruire à chaque
/// opération ne coûte qu'une lecture de fichier.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub name: String,
    /// Répertoire qui accueille les worktrees du projet.
    pub root: PathBuf,
    /// Modèle de nom de branche, `wt/{{slug}}` par défaut.
    pub branch_template: String,
    pub tasks: Vec<TaskInfo>,
    pub has_up: bool,
    pub has_down: bool,
    pub has_open: bool,
    /// Le projet pose-t-il des questions avant de créer un worktree.
    pub has_prompts: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskInfo {
    pub name: String,
    pub description: String,
}

/// Une question déclarée par le projet, avec ses choix déjà résolus.
///
/// Les choix peuvent venir d'une commande shell (`source`) : ils sont donc
/// calculés dans le worker, jamais au moment de dessiner le dialogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub name: String,
    pub title: String,
    pub kind: Kind,
    pub choices: Vec<Choice>,
    pub default: Option<String>,
    /// Séparateur des valeurs d'un choix multiple.
    pub separator: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Choice,
    Multi,
    Confirm,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub label: String,
    pub detail: String,
}

/// De quoi lancer une tâche dans un onglet de terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// Les commandes du projet, modèles déjà résolus.
    pub commands: Vec<String>,
    pub cwd: PathBuf,
    /// `WT_SLUG`, `WT_PORT_*`, `WT_OPT_*` : un hook un peu long se lit mieux
    /// avec des variables d'environnement qu'avec des substitutions.
    pub env: BTreeMap<String, String>,
}

impl Launch {
    /// La ligne à donner à un shell. Les commandes sont enchaînées par `&&` :
    /// une tâche en plusieurs étapes s'arrête à la première qui échoue, comme
    /// elle le ferait sur la ligne de commande.
    pub fn shell_line(&self) -> String {
        self.commands.join(" && ")
    }
}

/// Une adresse que le projet sait ouvrir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub url: String,
    pub label: String,
}

/// Le projet d'un dépôt, ou `None` s'il n'a pas de `wt.toml`.
///
/// L'absence est le cas courant — la plupart des dépôts n'en ont pas — et ne
/// vaut pas une erreur : les gestes de `wt` disparaissent simplement du menu.
fn app(main: &Path) -> Option<App> {
    let project = Project::load(main).ok()?;
    App::new(project).ok()
}

pub fn snapshot(main: &Path) -> Option<Snapshot> {
    let app = app(main)?;
    let config = &app.project.config;
    Some(Snapshot {
        name: app.project.name(),
        root: app.root.clone(),
        branch_template: config
            .branch
            .clone()
            .unwrap_or_else(|| "wt/{{slug}}".into()),
        tasks: config
            .tasks
            .iter()
            .map(|(name, task)| TaskInfo {
                name: name.clone(),
                description: task.description.clone(),
            })
            .collect(),
        has_up: app.has_up(),
        has_down: app.has_down(),
        has_open: app.has_open(),
        has_prompts: !config.prompts.is_empty(),
    })
}

/// Le slug d'un checkout : le nom de son dossier sous la racine du projet.
///
/// `None` pour le dépôt principal et pour tout worktree posé ailleurs : `wt`
/// ne connaît que ce qu'il a créé, et lui demander d'agir sur le reste
/// produirait des chemins qui n'existent pas.
pub fn slug_of(main: &Path, worktree: &Path) -> Option<String> {
    let root = app(main)?.root;
    let rest = worktree.strip_prefix(&root).ok()?;
    let mut parts = rest.components();
    let slug = parts.next()?.as_os_str().to_str()?.to_string();
    parts.next().is_none().then_some(slug)
}

/// Les questions qui s'appliquent, compte tenu des réponses déjà données.
///
/// Appelée en boucle par le dialogue : un `when` peut dépendre d'une réponse
/// précédente, et poser toutes les questions d'un coup ferait sauter celles
/// qu'une autre débloque. La boucle converge — chaque tour ne peut qu'ajouter
/// des questions déjà répondues à la liste des connues.
pub fn questions(
    main: &Path,
    slug: &str,
    answers: &BTreeMap<String, String>,
) -> Result<Vec<Question>> {
    let Some(app) = app(main) else {
        return Ok(Vec::new());
    };
    Ok(app
        .prompts_for(Ask::New, answers)
        .into_iter()
        .filter(|prompt| app.prompt_applies(prompt, slug, answers, Ask::New))
        .map(|prompt| {
            let choices = app
                .prompt_choices(&prompt, slug, answers, Ask::New)
                .into_iter()
                .map(|option| Choice {
                    label: if option.label.is_empty() {
                        option.value.clone()
                    } else {
                        option.label
                    },
                    value: option.value,
                    detail: option.detail,
                })
                .collect();
            Question {
                title: prompt.title().to_string(),
                name: prompt.name,
                kind: match prompt.kind {
                    PromptKind::Choice => Kind::Choice,
                    PromptKind::Multi => Kind::Multi,
                    PromptKind::Confirm => Kind::Confirm,
                    PromptKind::Text => Kind::Text,
                },
                choices,
                default: prompt.default,
                separator: prompt.separator,
            }
        })
        .collect())
}

/// Crée un worktree avec tout ce que le projet demande : la branche selon son
/// modèle, les dossiers, les copies, les ports, puis `post_new`.
pub fn create(
    main: &Path,
    slug: &str,
    from: Option<&str>,
    answers: &BTreeMap<String, String>,
) -> Result<(PathBuf, String)> {
    let app = app(main).ok_or_else(|| anyhow::anyhow!("ce dépôt n'a pas de wt.toml"))?;
    let sets = sets(answers);
    let (output, result) = capturing(&app, |app| app.cmd_new(slug, None, from, &sets));
    result?;
    Ok((app.dir(slug), output))
}

pub fn remove(main: &Path, slug: &str) -> Result<String> {
    let app = app(main).ok_or_else(|| anyhow::anyhow!("ce dépôt n'a pas de wt.toml"))?;
    // `yes` : la confirmation est du ressort de la vue, qui l'a déjà demandée.
    let (output, result) = capturing(&app, |app| app.cmd_rm(slug, true));
    result?;
    Ok(output)
}

pub fn up(main: &Path, slug: &str) -> Result<String> {
    let app = app(main).ok_or_else(|| anyhow::anyhow!("ce dépôt n'a pas de wt.toml"))?;
    let (output, result) = capturing(&app, |app| app.cmd_up(slug, &[]));
    result?;
    Ok(output)
}

pub fn down(main: &Path, slug: &str) -> Result<String> {
    let app = app(main).ok_or_else(|| anyhow::anyhow!("ce dépôt n'a pas de wt.toml"))?;
    let (output, result) = capturing(&app, |app| app.cmd_down(slug));
    result?;
    Ok(output)
}

/// Ce qu'il faut pour lancer une tâche dans un terminal.
///
/// Les commandes sont rendues ici — modèles résolus, environnement calculé —
/// et non exécutées : c'est l'onglet de terminal qui les lance, parce qu'une
/// tâche est souvent interactive et qu'un panneau de sortie ne transmet ni les
/// frappes ni les couleurs.
pub fn task(main: &Path, slug: &str, name: &str) -> Result<Launch> {
    let app = app(main).ok_or_else(|| anyhow::anyhow!("ce dépôt n'a pas de wt.toml"))?;
    let task = app
        .project
        .config
        .tasks
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("tâche inconnue : {name}"))?;
    let saved = state::load(&app.root, slug);
    let mut vars = app.vars(slug, &saved);
    vars.insert("args".into(), String::new());
    let cwd = match task.cwd {
        ::wt::config::Cwd::Main => app.project.main.clone(),
        ::wt::config::Cwd::Worktree => app.dir(slug),
    };
    Ok(Launch {
        commands: task
            .run
            .0
            .iter()
            .map(|raw| tmpl::render(raw, &vars))
            .collect(),
        env: state::env(&vars),
        cwd,
    })
}

/// Le worktree tourne-t-il, d'après `[status] up` ?
///
/// `None` quand le projet ne le déclare pas : il n'y a alors rien à démarrer,
/// et afficher « arrêté » serait une information fausse.
pub fn is_up(main: &Path, slug: &str) -> Option<bool> {
    let app = app(main)?;
    let status = app.project.config.status.up.as_ref()?;
    let saved = state::load(&app.root, slug);
    let vars = app.vars(slug, &saved);
    Some(util::succeeds(
        &tmpl::render(status, &vars),
        &app.dir(slug),
        &state::env(&vars),
    ))
}

/// Les adresses que le projet expose pour ce worktree.
pub fn endpoints(main: &Path, slug: &str) -> Vec<Endpoint> {
    let Some(app) = app(main) else {
        return Vec::new();
    };
    let saved = state::load(&app.root, slug);
    app.links(slug, &saved)
        .into_iter()
        .map(|link: ops::Link| Endpoint {
            url: link.url,
            label: link.label,
        })
        .collect()
}

/// Les réponses, sous la forme `clé=valeur` qu'attend `wt`.
fn sets(answers: &BTreeMap<String, String>) -> Vec<String> {
    answers
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

/// Exécute une opération en récoltant ce qu'elle raconte.
///
/// `set_sink` est prévu pour cela : sans lui, les messages partiraient sur la
/// sortie standard d'une application graphique, c'est-à-dire nulle part. Ce
/// qui en revient devient le texte de la notification — celui que `wt` a
/// écrit, et non une reformulation qui n'apporterait que des approximations.
fn capturing<T>(app: &App, run: impl FnOnce(&App) -> Result<T>) -> (String, Result<T>) {
    let (tx, rx) = std::sync::mpsc::channel();
    app.set_sink(Some(tx));
    let result = run(app);
    // Le sink est relâché avant de vider le canal : tant qu'il tient
    // l'émetteur, `try_iter` ne verrait jamais la fin.
    app.set_sink(None);
    let lines: Vec<String> = rx
        .try_iter()
        .filter_map(|msg| match msg {
            util::Msg::Info(m) | util::Msg::Ok(m) | util::Msg::Warn(m) | util::Msg::Out(m) => {
                Some(m)
            }
            util::Msg::Done(_) => None,
        })
        .collect();
    // La dernière ligne seulement : la barre d'état n'en montre qu'une, et
    // c'est le résultat qu'on veut y lire, pas la première étape.
    (lines.last().cloned().unwrap_or_default(), result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slug_is_the_directory_right_under_the_root() {
        let root = Path::new("/p/repo-wt");
        // Ce que `slug_of` fait une fois la racine connue. La fonction elle-même
        // demande un `wt.toml` ; c'est la règle qu'elle applique qui compte.
        let check = |path: &str| -> Option<String> {
            let rest = Path::new(path).strip_prefix(root).ok()?;
            let mut parts = rest.components();
            let slug = parts.next()?.as_os_str().to_str()?.to_string();
            parts.next().is_none().then_some(slug)
        };
        assert_eq!(check("/p/repo-wt/demo"), Some("demo".into()));
        // Un sous-dossier n'est pas un worktree.
        assert_eq!(check("/p/repo-wt/demo/src"), None);
        // Le dépôt principal n'est pas sous la racine : `wt` ne le connaît pas.
        assert_eq!(check("/p/repo"), None);
    }

    #[test]
    fn the_answers_become_the_sets_wt_expects() {
        let mut answers = BTreeMap::new();
        answers.insert("tenants".to_string(), "a,b".to_string());
        answers.insert("queue".to_string(), "1".to_string());
        assert_eq!(sets(&answers), vec!["queue=1", "tenants=a,b"]);
    }
}
