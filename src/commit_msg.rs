//! Proposer un message de commit à partir de ce qui est indexé.
//!
//! **Pas d'API, un sous-processus.** C'est la même décision de cadrage que
//! partout ailleurs : l'IA de Claudhub passe par un programme que
//! l'utilisateur a déjà installé et authentifié — `claude -p`, ou ce qu'il
//! préfère —, jamais par une clé d'API et un client HTTP à nous. Le prix d'un
//! `fork` est sans commune mesure avec celui d'une dépendance qui aurait sa
//! propre authentification, ses propres quotas et son propre format d'erreur.
//!
//! Le diff part par **l'entrée standard** et non en argument : une ligne de
//! commande a une longueur maximale — de l'ordre de deux mégaoctets sous
//! Linux, mais c'est le tout qui compte, environnement compris — et un diff de
//! relecture d'agent la frôle. Un tube n'a pas de bord.
//!
//! Rien ici ne connaît gpui, et `prompt` comme `clean` sont libres de toute
//! entrée-sortie : c'est ce qui les rend testables.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

/// Au-delà, le diff est tronqué.
///
/// Ce n'est pas la limite du modèle mais celle du bon sens : un message de
/// commit se déduit de ce qui change, et les cent premiers kilo-octets d'un
/// diff en disent déjà tout. Envoyer les cinq mégaoctets d'un `composer.lock`
/// régénéré coûterait des jetons pour une ligne de résumé.
pub const MAX_DIFF: usize = 100_000;

/// Combien de messages récents servent d'exemple.
pub const RECENT: usize = 10;

/// Un agent qui réfléchit met dix à trente secondes ; passé deux minutes, il
/// est bloqué et le worker avec lui.
const TIMEOUT: Duration = Duration::from_secs(120);

/// Ce qu'on demande à l'agent.
///
/// Les messages récents y figurent parce que la convention d'un dépôt ne se
/// devine pas : la langue d'abord, mais aussi la personne du verbe et les
/// préfixes que l'équipe s'est donnés. Une consigne écrite ici les imposerait
/// à tous les dépôts, ce qui est exactement ce qu'on ne veut pas.
pub fn prompt(recent: &[String], diff: &str) -> String {
    let (diff, truncated) = truncate(diff, MAX_DIFF);
    let mut out = String::with_capacity(diff.len() + 1024);
    out.push_str(
        "Tu écris le message du commit que ces modifications indexées vont produire.\n\n\
         Réponds par le message seul : aucune phrase d'introduction, aucun bloc de code, \
         aucun guillemet autour.\n\
         Une ligne de résumé de moins de 72 caractères ; puis, si et seulement si le \
         changement le demande, une ligne vide et un corps qui dit pourquoi plutôt que quoi.\n",
    );
    if recent.is_empty() {
        out.push_str(
            "\nCe dépôt n'a pas encore de commit : écris le message dans la langue du code.\n",
        );
    } else {
        out.push_str("\nSuis la langue et les conventions des messages récents de ce dépôt :\n\n");
        for subject in recent {
            out.push_str("  ");
            out.push_str(subject);
            out.push('\n');
        }
    }
    out.push_str("\nDiff indexé :\n\n");
    out.push_str(diff);
    if truncated {
        out.push_str("\n\n[diff tronqué : seul le début est montré]");
    }
    out.push('\n');
    out
}

/// Coupe à `max` **octets, sur une frontière de caractère**.
///
/// En octets nus, un diff accentué se couperait au milieu d'un caractère et la
/// tranche ne serait plus de l'UTF-8 valide.
fn truncate(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// Ce que l'agent a répondu, ramené à un message de commit.
///
/// Un modèle encadre volontiers sa réponse d'un bloc de code ou de guillemets
/// malgré la consigne, et ce sont des caractères qui finiraient tels quels
/// dans l'historique du dépôt. Le nettoyage est ici, pur et testé, plutôt que
/// dans la vue.
pub fn clean(output: &str) -> String {
    let mut text = output.trim();

    // Un bloc de code : la première ligne porte la clôture et parfois un nom
    // de langage, la dernière la referme.
    if text.starts_with("```") {
        if let Some(rest) = text.split_once('\n').map(|(_, rest)| rest) {
            text = rest.trim_end();
            if let Some(cut) = text.rfind("```") {
                text = text[..cut].trim_end();
            }
        }
    }

    let text = text.trim();
    // Des guillemets autour du tout, et seulement s'ils encadrent vraiment :
    // un message qui commence *et* finit par une citation en garderait les
    // siens.
    let unquoted = ['"', '\'']
        .iter()
        .find_map(|q| {
            let inner = text.strip_prefix(*q)?.strip_suffix(*q)?;
            (!inner.contains(*q)).then_some(inner)
        })
        .unwrap_or(text);

    unquoted.trim().to_string()
}

/// Demande un message à l'agent configuré. **Jamais depuis le thread
/// d'interface** : cet appel dure des secondes.
pub fn suggest(worktree: &Path, command_line: &str) -> Result<String> {
    let diff = crate::git::diff::staged_text(worktree)?;
    if diff.trim().is_empty() {
        bail!("rien n'est indexé : cochez ce qui doit partir au commit");
    }
    let recent = crate::git::history::recent_subjects(worktree, RECENT);
    let answer = ask(worktree, command_line, &prompt(&recent, &diff))?;
    let message = clean(&answer);
    if message.is_empty() {
        bail!("l'agent n'a rien répondu");
    }
    Ok(message)
}

/// Lance le programme configuré, lui donne le prompt sur l'entrée standard, et
/// rend sa sortie standard.
///
/// Les trois flux passent par des threads : un tube plein bloque celui qui
/// écrit, et le prompt comme la réponse dépassent largement les soixante-quatre
/// kilo-octets d'un tube. Écrire d'abord puis attendre donnerait l'interblocage
/// classique — le processus attend qu'on lise, nous attendons qu'il finisse.
fn ask(worktree: &Path, command_line: &str, prompt: &str) -> Result<String> {
    let mut parts = crate::ui::split_command(command_line).into_iter();
    let program = parts
        .next()
        .ok_or_else(|| anyhow!("aucune commande de génération de message dans les réglages"))?;
    let args: Vec<String> = parts.collect();

    let mut child = Command::new(&program)
        .args(&args)
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("{program} : programme introuvable"))?;

    let mut stdin = child.stdin.take().expect("stdin demandé en piped");
    let text = prompt.to_string();
    // L'écriture ignore son échec : un programme qui sort avant d'avoir tout
    // lu ferme le tube, et c'est son code de retour qui doit être rapporté,
    // pas un `EPIPE` qui n'explique rien.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(text.as_bytes());
    });
    let out = read_thread(child.stdout.take().expect("stdout demandé en piped"));
    let err = read_thread(child.stderr.take().expect("stderr demandé en piped"));

    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("{program} n'a pas répondu en {TIMEOUT:?} et a été interrompu");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let _ = writer.join();
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();

    if !status.success() {
        let message = String::from_utf8_lossy(&stderr);
        let message = message.trim();
        bail!(
            "{program} a échoué : {}",
            if message.is_empty() {
                "aucun message".into()
            } else {
                message.to_string()
            }
        );
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

fn read_thread(
    mut source: impl std::io::Read + Send + 'static,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = source.read_to_end(&mut buffer);
        buffer
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_shows_the_conventions_of_the_repository() {
        let text = prompt(&["Retirer les coutures".into()], "diff --git a/x b/x\n");
        assert!(text.contains("Retirer les coutures"), "{text}");
        assert!(text.contains("diff --git a/x b/x"), "{text}");
        assert!(!text.contains("tronqué"), "{text}");
    }

    #[test]
    fn a_repository_without_commits_is_not_an_error() {
        let text = prompt(&[], "diff");
        assert!(text.contains("pas encore de commit"), "{text}");
    }

    /// La coupe se fait sur une frontière de caractère : en octets nus, un
    /// diff accentué produirait une tranche qui n'est pas de l'UTF-8.
    #[test]
    fn a_long_diff_is_cut_on_a_character_boundary() {
        let diff = "é".repeat(MAX_DIFF);
        let text = prompt(&[], &diff);
        assert!(text.contains("[diff tronqué"), "la coupe n'est pas dite");
    }

    #[test]
    fn a_code_fence_never_reaches_the_history() {
        assert_eq!(clean("```\nAjouter le bouton\n```"), "Ajouter le bouton");
        assert_eq!(
            clean("```text\nAjouter\n\nParce que.\n```"),
            "Ajouter\n\nParce que."
        );
    }

    #[test]
    fn surrounding_quotes_are_dropped_but_not_the_others() {
        assert_eq!(clean("\"Ajouter le bouton\""), "Ajouter le bouton");
        assert_eq!(
            clean("Ajouter « le bouton » et \"la case\""),
            "Ajouter « le bouton » et \"la case\""
        );
    }

    #[test]
    fn a_plain_message_comes_out_untouched() {
        assert_eq!(
            clean("  Ajouter le bouton\n\nParce que.\n"),
            "Ajouter le bouton\n\nParce que."
        );
    }
}
