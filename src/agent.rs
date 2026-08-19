//! Détection des agents de codage qui tournent dans les worktrees.
//!
//! Perch ne lance pas tous les agents : on en démarre depuis un onglet de
//! Perch, mais aussi depuis un terminal à côté, et c'est le même travail qu'on
//! veut voir. La détection passe donc par `/proc` — le répertoire courant d'un
//! processus dit dans quel worktree il travaille — plutôt que par les seuls
//! onglets que nous avons ouverts.
//!
//! Linux seulement. Ailleurs, la liste est vide et la barre latérale n'affiche
//! rien de plus : ce n'est pas une fonctionnalité dont l'absence casse quoi que
//! ce soit.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Un processus d'agent trouvé dans un worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    /// Temps processeur consommé depuis le démarrage, en tics d'horloge.
    ///
    /// C'est une mesure cumulée, sans intérêt en soi : c'est sa *variation*
    /// entre deux relevés qui distingue un agent au travail d'un agent qui
    /// attend une réponse de l'utilisateur.
    pub cpu: u64,
}

/// Les agents trouvés, par worktree.
pub type Agents = HashMap<PathBuf, Vec<Process>>;

/// Ailleurs que sous Linux, il n'y a pas de `/proc` : la liste est vide, et la
/// barre latérale n'affiche simplement aucun agent.
///
/// Le stub est explicite plutôt qu'accidentel : le parcours ci-dessous
/// compilerait partout et échouerait en silence à l'ouverture de `/proc`, ce
/// qui se lit comme une détection cassée plutôt que comme une absence assumée.
#[cfg(not(target_os = "linux"))]
pub fn scan(_worktrees: &[PathBuf], _program: &str) -> Agents {
    Agents::new()
}

/// Parcourt `/proc` à la recherche des agents lancés dans ces worktrees.
///
/// `program` est le nom de la commande configurée — `claude` par défaut, mais
/// rien n'oblige à ce que ce soit celle-là.
#[cfg(target_os = "linux")]
pub fn scan(worktrees: &[PathBuf], program: &str) -> Agents {
    let mut found: Agents = HashMap::new();
    let program = command_name(program);
    if program.is_empty() {
        return found;
    }
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let dir = entry.path();
        if !matches_program(&dir, program) {
            continue;
        }
        // Le répertoire courant d'un processus est le worktree où il
        // travaille ; un lien symbolique non résolu — processus disparu,
        // permissions — le fait simplement ignorer.
        let Ok(cwd) = std::fs::read_link(dir.join("cwd")) else {
            continue;
        };
        let Some(worktree) = owning_worktree(worktrees, &cwd) else {
            continue;
        };
        let cpu = std::fs::read_to_string(dir.join("stat"))
            .ok()
            .and_then(|stat| parse_cpu_ticks(&stat))
            .unwrap_or(0);
        found
            .entry(worktree)
            .or_default()
            .push(Process { pid, cpu });
    }
    found
}

/// Le nom de la commande, dépouillé de son chemin et de ses arguments.
pub fn command_name(command: &str) -> &str {
    let program = command.split_whitespace().next().unwrap_or("");
    program.rsplit('/').next().unwrap_or(program)
}

/// Vrai si ce processus est l'agent cherché.
///
/// Le nom seul (`comm`) ne suffit pas : un agent lancé par un script ou par un
/// gestionnaire de versions de node s'appelle `node`, et c'est sa ligne de
/// commande qui porte `claude`.
#[cfg(target_os = "linux")]
fn matches_program(proc_dir: &Path, program: &str) -> bool {
    if let Ok(comm) = std::fs::read_to_string(proc_dir.join("comm")) {
        if comm.trim() == program {
            return true;
        }
    }
    let Ok(cmdline) = std::fs::read(proc_dir.join("cmdline")) else {
        return false;
    };
    cmdline_matches(&cmdline, program)
}

/// `/proc/<pid>/cmdline` sépare les arguments par des octets nuls.
#[cfg(target_os = "linux")]
fn cmdline_matches(cmdline: &[u8], program: &str) -> bool {
    cmdline
        .split(|b| *b == 0)
        .filter_map(|arg| std::str::from_utf8(arg).ok())
        .any(|arg| arg.rsplit('/').next().unwrap_or(arg) == program)
}

/// Le worktree le plus profond qui contient ce répertoire.
///
/// Le plus profond, et non le premier trouvé : un worktree imbriqué dans un
/// autre attribuerait sinon ses agents au mauvais.
#[cfg(target_os = "linux")]
fn owning_worktree(worktrees: &[PathBuf], cwd: &Path) -> Option<PathBuf> {
    worktrees
        .iter()
        .filter(|worktree| cwd.starts_with(worktree))
        .max_by_key(|worktree| worktree.as_os_str().len())
        .cloned()
}

/// Temps processeur cumulé d'un processus, d'après `/proc/<pid>/stat`.
///
/// Le nom du programme est le deuxième champ, entre parenthèses, et **peut
/// contenir des espaces et des parenthèses** : découper la ligne sur les
/// espaces donne des champs décalés dès qu'un programme s'appelle « (mon
/// agent) ». On repart donc de la dernière parenthèse fermante.
#[cfg(target_os = "linux")]
pub fn parse_cpu_ticks(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // Après le nom viennent l'état, ppid, pgrp, session, tty, tpgid, flags,
    // puis les quatre compteurs de fautes de page : `utime` est le 12ᵉ champ
    // de ce reste, `stime` le 13ᵉ.
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_name_ignores_its_path_and_arguments() {
        assert_eq!(command_name("claude"), "claude");
        assert_eq!(command_name("/usr/bin/claude --resume"), "claude");
        assert_eq!(command_name(""), "");
    }
}

/// Ce qui parle le format de `/proc` ne se compile — et ne se teste — que là
/// où `/proc` existe.
#[cfg(all(test, target_os = "linux"))]
mod proc_tests {
    use super::*;

    #[test]
    fn the_command_line_is_matched_argument_by_argument() {
        // Un agent lancé par node : c'est le second argument qui le nomme.
        let cmdline = b"/nix/store/x/bin/node\0/home/a/.bun/bin/claude\0--resume\0";
        assert!(cmdline_matches(cmdline, "claude"));
        assert!(!cmdline_matches(cmdline, "aider"));
        // Une correspondance partielle n'en est pas une : `claudia` n'est pas
        // `claude`.
        assert!(!cmdline_matches(b"/usr/bin/claudia\0", "claude"));
    }

    #[test]
    fn cpu_ticks_survive_a_program_name_full_of_parentheses() {
        // Le cas que tout parseur naïf de /proc rate.
        let stat = "42 (mon (drôle) d'agent) S 1 42 42 0 -1 4194304 100 0 0 0 \
                    130 27 0 0 20 0 12 0 999";
        assert_eq!(parse_cpu_ticks(stat), Some(157));
    }

    #[test]
    fn the_deepest_worktree_claims_the_process() {
        let worktrees = vec![PathBuf::from("/p/repo"), PathBuf::from("/p/repo/nested")];
        assert_eq!(
            owning_worktree(&worktrees, Path::new("/p/repo/nested/src")),
            Some(PathBuf::from("/p/repo/nested"))
        );
        assert_eq!(
            owning_worktree(&worktrees, Path::new("/p/repo/src")),
            Some(PathBuf::from("/p/repo"))
        );
        assert_eq!(owning_worktree(&worktrees, Path::new("/ailleurs")), None);
    }
}
