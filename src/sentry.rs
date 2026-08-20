//! Les issues Sentry d'un projet, et de quoi les rapprocher du code.
//!
//! Claudhub **lit** Sentry ; il ne lui envoie jamais rien. Un rapport d'erreur
//! est un point de départ comme un autre — souvent meilleur qu'une intention,
//! parce qu'il porte déjà la trace et le fichier fautif — et le geste utile
//! est de le confier à un agent avec le code autour des frames de
//! l'application.
//!
//! Le jeton se lit **d'abord dans `SENTRY_TOKEN`**, et n'est rangé dans le
//! fichier de réglages qu'à défaut : ce fichier est en 0600, ce qui ne fait
//! pas de lui un coffre.
//!
//! Comme tous les formats que nous parsons, celui-ci est testé sur une
//! fixture : une API distante change sans prévenir, et un champ renommé se
//! voit ici plutôt qu'à l'exécution sous la forme d'une liste vide.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;

/// L'API publique de Sentry. Une instance auto-hébergée se règle par
/// `SENTRY_URL`, parce que c'est la seule chose qui change.
const DEFAULT_HOST: &str = "https://sentry.io";

/// Une API distante met parfois plusieurs secondes ; au-delà, c'est qu'elle ne
/// répondra pas. Le même raisonnement que le délai des commandes git.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Une issue, réduite à ce que le panneau affiche.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Issue {
    pub id: String,
    /// `ValueError`, `TypeError`… ce que Sentry appelle le type.
    pub title: String,
    /// Le message, quand il ajoute quelque chose au titre.
    pub culprit: String,
    /// `error`, `warning`, `fatal`…
    pub level: String,
    pub count: u64,
    /// Dernière occurrence, telle que Sentry l'écrit (ISO 8601).
    pub last_seen: String,
    pub permalink: String,
}

/// Une ligne d'une pile d'appels.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    /// Chemin tel que Sentry le connaît. Il n'est pas toujours relatif au
    /// dépôt — d'où `Frame::repo_path`, qui fait de son mieux.
    pub filename: String,
    pub function: String,
    pub line: usize,
    /// La frame appartient-elle au code de l'application, par opposition aux
    /// dépendances. C'est celle-là qu'on veut lire.
    pub in_app: bool,
    /// Le code autour, tel que Sentry le renvoie : `(numéro, ligne)`.
    ///
    /// Il vient de l'événement, donc du code **déployé** au moment de
    /// l'erreur : c'est justement ce qu'on veut citer, et le relire sur disque
    /// donnerait la version d'aujourd'hui.
    pub context: Vec<(usize, String)>,
}

impl Frame {
    /// Le chemin ramené au dépôt, quand on peut.
    ///
    /// Sentry écrit souvent un chemin absolu du serveur
    /// (`/var/www/app/Http/Kernel.php`) ou un module (`app.http.kernel`). On
    /// coupe au premier segment qui existe dans le worktree ; à défaut, on
    /// rend le chemin tel quel et l'utilisateur voit ce que Sentry a dit.
    pub fn repo_path(&self, worktree: &Path) -> String {
        let normalized = self.filename.replace('\\', "/");
        let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
        for start in 0..parts.len() {
            let candidate = parts[start..].join("/");
            if worktree.join(&candidate).exists() {
                return candidate;
            }
        }
        normalized
    }
}

/// L'événement le plus récent d'une issue : sa pile et son message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Event {
    pub message: String,
    /// Les frames, de la plus ancienne à la plus récente — l'ordre de Sentry,
    /// et celui d'une trace lue de haut en bas.
    pub frames: Vec<Frame>,
}

// — Ce que l'API renvoie ——————————————————————————————————————————
//
// Des structures à part, `#[serde(default)]` partout : l'API ajoute et retire
// des champs, et un champ manquant ne doit pas vider la liste entière.

#[derive(Deserialize)]
#[serde(default)]
struct RawIssue {
    id: String,
    title: String,
    culprit: String,
    level: String,
    count: serde_json::Value,
    #[serde(rename = "lastSeen")]
    last_seen: String,
    permalink: String,
}

impl Default for RawIssue {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: String::new(),
            culprit: String::new(),
            level: String::new(),
            // Sentry écrit le compte en **chaîne** dans la liste des issues et
            // en nombre ailleurs : la valeur brute est gardée et convertie à
            // la main, sinon la moitié des réponses ne se lit pas.
            count: serde_json::Value::Null,
            last_seen: String::new(),
            permalink: String::new(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEvent {
    message: String,
    entries: Vec<RawEntry>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEntry {
    #[serde(rename = "type")]
    kind: String,
    data: serde_json::Value,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawFrame {
    filename: String,
    #[serde(rename = "absPath")]
    abs_path: String,
    module: String,
    function: String,
    #[serde(rename = "lineNo")]
    line_no: Option<usize>,
    #[serde(rename = "inApp")]
    in_app: bool,
    context: Vec<serde_json::Value>,
}

fn as_u64(value: &serde_json::Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

/// Lit la liste des issues d'un projet.
pub fn parse_issues(json: &str) -> Result<Vec<Issue>> {
    let raw: Vec<RawIssue> =
        serde_json::from_str(json).context("réponse Sentry illisible (issues)")?;
    Ok(raw
        .into_iter()
        .map(|issue| Issue {
            count: as_u64(&issue.count),
            id: issue.id,
            title: issue.title,
            culprit: issue.culprit,
            level: issue.level,
            last_seen: issue.last_seen,
            permalink: issue.permalink,
        })
        .collect())
}

/// Lit l'événement le plus récent d'une issue.
///
/// La pile vit dans l'entrée `exception` ou `stacktrace` de `entries` ; les
/// deux formes existent selon le SDK qui a envoyé l'événement, et n'en gérer
/// qu'une donne une trace vide sur la moitié des projets.
pub fn parse_event(json: &str) -> Result<Event> {
    let raw: RawEvent =
        serde_json::from_str(json).context("réponse Sentry illisible (événement)")?;
    let mut frames = Vec::new();
    for entry in &raw.entries {
        match entry.kind.as_str() {
            "exception" => {
                let values = entry.data.get("values").and_then(|v| v.as_array());
                for value in values.into_iter().flatten() {
                    collect_frames(value.get("stacktrace"), &mut frames);
                }
            }
            "stacktrace" => collect_frames(Some(&entry.data), &mut frames),
            _ => {}
        }
    }
    Ok(Event {
        message: raw.message,
        frames,
    })
}

fn collect_frames(stacktrace: Option<&serde_json::Value>, out: &mut Vec<Frame>) {
    let Some(list) = stacktrace
        .and_then(|s| s.get("frames"))
        .and_then(|f| f.as_array())
    else {
        return;
    };
    for value in list {
        let Ok(raw) = serde_json::from_value::<RawFrame>(value.clone()) else {
            continue;
        };
        let filename = [raw.filename, raw.abs_path, raw.module]
            .into_iter()
            .find(|candidate| !candidate.is_empty())
            .unwrap_or_default();
        if filename.is_empty() {
            continue;
        }
        out.push(Frame {
            filename,
            function: raw.function,
            line: raw.line_no.unwrap_or(0),
            in_app: raw.in_app,
            // `context` est une liste de paires `[numéro, source]` ; tout ce
            // qui n'a pas cette forme est ignoré plutôt que de faire échouer
            // la lecture de la trace entière.
            context: raw
                .context
                .iter()
                .filter_map(|pair| {
                    let pair = pair.as_array()?;
                    let line = as_u64(pair.first()?) as usize;
                    let text = pair.get(1)?.as_str().unwrap_or_default().to_string();
                    Some((line, text))
                })
                .collect(),
        });
    }
}

/// Le prompt qu'on livre à un agent : le titre, le message, la trace, et le
/// code autour des frames de l'application.
///
/// Les frames hors application sont **citées sans leur code** : une pile de
/// framework fait cent lignes, et ce n'est pas là qu'est le bug. Fonction
/// libre et testée, comme celle des notes : c'est la pièce à verrouiller.
pub fn prompt(issue: &Issue, event: &Event, worktree: &Path) -> String {
    let mut out = String::new();
    out.push_str(&crate::tr!("sentry-prompt-intro", { title: issue.title.clone() }));
    out.push_str("\n\n");
    if !issue.culprit.is_empty() {
        out.push_str(&issue.culprit);
        out.push('\n');
    }
    if !event.message.is_empty() && event.message != issue.title {
        out.push_str(&event.message);
        out.push('\n');
    }
    out.push('\n');

    for frame in &event.frames {
        let path = frame.repo_path(worktree);
        out.push_str(&format!("- {path}:{}", frame.line));
        if !frame.function.is_empty() {
            out.push_str(&format!(" · {}", frame.function));
        }
        out.push('\n');
    }

    for frame in event.frames.iter().filter(|frame| frame.in_app) {
        if frame.context.is_empty() {
            continue;
        }
        let path = frame.repo_path(worktree);
        out.push_str(&format!("\n## {path}:{}\n", frame.line));
        out.push_str("```\n");
        for (number, text) in &frame.context {
            // La ligne fautive est marquée : c'est la seule information que la
            // numérotation ne donne pas d'un coup d'œil.
            let marker = if *number == frame.line { ">" } else { " " };
            out.push_str(&format!("{marker} {number:>5} {text}\n"));
        }
        out.push_str("```\n");
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Le jeton d'API : l'environnement d'abord, le fichier de réglages à défaut.
pub fn token(fallback: &str) -> Option<String> {
    std::env::var("SENTRY_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| Some(fallback.trim().to_string()).filter(|value: &String| !value.is_empty()))
}

fn host() -> String {
    std::env::var("SENTRY_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
        .unwrap_or_else(|| DEFAULT_HOST.to_string())
}

fn get(url: &str, token: &str) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    let mut response = agent
        .get(url)
        .header("Authorization", &format!("Bearer {token}"))
        .call()
        .with_context(|| format!("Sentry : {url} injoignable"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("Sentry a répondu {status}");
    }
    response
        .body_mut()
        .read_to_string()
        .context("réponse Sentry illisible")
}

/// Les issues non résolues d'un projet, les plus fréquentes d'abord.
pub fn issues(org: &str, project: &str, query: &str, token: &str) -> Result<Vec<Issue>> {
    if org.trim().is_empty() || project.trim().is_empty() {
        bail!("organisation ou projet Sentry non configuré");
    }
    let query = if query.trim().is_empty() {
        "is:unresolved".to_string()
    } else {
        query.trim().to_string()
    };
    let url = format!(
        "{}/api/0/projects/{}/{}/issues/?query={}&statsPeriod=14d",
        host(),
        urlencode(org),
        urlencode(project),
        urlencode(&query)
    );
    parse_issues(&get(&url, token)?)
}

/// L'événement le plus récent d'une issue.
pub fn latest_event(issue: &str, token: &str) -> Result<Event> {
    let url = format!(
        "{}/api/0/issues/{}/events/latest/",
        host(),
        urlencode(issue)
    );
    parse_event(&get(&url, token)?)
}

/// Encodage minimal d'un composant d'URL.
///
/// Une requête Sentry contient des espaces et des deux-points
/// (`is:unresolved environment:production`) : les laisser passer tels quels
/// donne une URL invalide, et tirer une dépendance pour trois caractères
/// serait cher payé.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISSUES: &str = r#"[
      {
        "id": "4508",
        "title": "TypeError: Cannot read properties of undefined",
        "culprit": "app/Http/Controllers/DevisController.php in store",
        "level": "error",
        "count": "137",
        "lastSeen": "2026-08-19T10:12:00Z",
        "permalink": "https://sentry.io/organizations/acme/issues/4508/"
      },
      {
        "id": "4509",
        "title": "ValueError",
        "count": 3,
        "lastSeen": "2026-08-18T22:00:00Z"
      }
    ]"#;

    const EVENT: &str = r#"{
      "message": "Cannot read properties of undefined (reading 'total')",
      "entries": [
        {
          "type": "exception",
          "data": {
            "values": [
              {
                "stacktrace": {
                  "frames": [
                    {
                      "filename": "vendor/laravel/framework/src/Foundation/Http/Kernel.php",
                      "function": "handle",
                      "lineNo": 141,
                      "inApp": false
                    },
                    {
                      "filename": "app/Http/Controllers/DevisController.php",
                      "function": "store",
                      "lineNo": 88,
                      "inApp": true,
                      "context": [
                        [86, "    public function store(Request $request)"],
                        [87, "    {"],
                        [88, "        return $request->devis->total;"],
                        [89, "    }"]
                      ]
                    }
                  ]
                }
              }
            ]
          }
        }
      ]
    }"#;

    #[test]
    fn issues_survive_a_count_written_as_a_string() {
        // Sentry écrit le compte en chaîne dans la liste et en nombre
        // ailleurs : lire les deux est ce qui évite une liste vide un jour
        // sur deux.
        let issues = parse_issues(ISSUES).unwrap();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].id, "4508");
        assert_eq!(issues[0].count, 137);
        assert_eq!(issues[0].level, "error");
        // Une issue sans culprit ni permalink se lit quand même.
        assert_eq!(issues[1].count, 3);
        assert!(issues[1].culprit.is_empty());
    }

    #[test]
    fn a_stack_trace_keeps_its_order_and_its_context() {
        let event = parse_event(EVENT).unwrap();
        assert_eq!(event.frames.len(), 2);
        assert!(!event.frames[0].in_app);
        assert!(event.frames[1].in_app);
        assert_eq!(event.frames[1].line, 88);
        assert_eq!(event.frames[1].context.len(), 4);
        assert_eq!(event.frames[1].context[2].0, 88);
    }

    #[test]
    fn an_empty_response_is_not_an_error() {
        assert!(parse_issues("[]").unwrap().is_empty());
        assert!(parse_event("{}").unwrap().frames.is_empty());
    }

    #[test]
    fn the_prompt_lists_the_stack_and_quotes_only_the_application_code() {
        let issue = parse_issues(ISSUES).unwrap().remove(0);
        let event = parse_event(EVENT).unwrap();
        let text = prompt(&issue, &event, Path::new("/nulle-part"));
        // Toute la pile, frames de framework comprises : c'est le chemin qui
        // a mené là.
        assert!(text.contains("Kernel.php:141"), "{text}");
        assert!(text.contains("DevisController.php:88"), "{text}");
        // Mais le code seulement pour ce qui appartient à l'application : une
        // pile de framework fait cent lignes, et le bug n'y est pas.
        assert!(
            text.contains("## app/Http/Controllers/DevisController.php:88"),
            "{text}"
        );
        assert!(!text.contains("## vendor/"), "{text}");
        // La ligne fautive est marquée.
        assert!(text.contains(">    88 "), "{text}");
        assert!(!text.ends_with('\n'), "{text:?}");
    }

    #[test]
    fn a_frame_path_is_brought_back_to_the_repository() {
        let dir = std::env::temp_dir().join(format!("claudhub-sentry-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("app/Http")).unwrap();
        std::fs::write(dir.join("app/Http/Kernel.php"), "").unwrap();

        let frame = Frame {
            // Le chemin du serveur, tel que Sentry le connaît.
            filename: "/var/www/releases/42/app/Http/Kernel.php".into(),
            ..Default::default()
        };
        assert_eq!(frame.repo_path(&dir), "app/Http/Kernel.php");

        // Ce qu'on ne retrouve pas est rendu tel quel : mieux vaut montrer ce
        // que Sentry a dit qu'un chemin inventé.
        let unknown = Frame {
            filename: "node_modules/x/index.js".into(),
            ..Default::default()
        };
        assert_eq!(unknown.repo_path(&dir), "node_modules/x/index.js");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_query_survives_its_spaces_and_colons() {
        assert_eq!(
            urlencode("is:unresolved environment:production"),
            "is%3Aunresolved%20environment%3Aproduction"
        );
    }
}
