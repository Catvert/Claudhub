//! L'historique, et le graphe qui le rend lisible.
//!
//! Un historique git est un graphe orienté acyclique, pas une liste : deux
//! branches parallèles, un merge, et l'ordre chronologique seul ne dit plus
//! rien de ce qui descend de quoi. Le graphe est donc calculé ici, sous la
//! forme dont la vue a besoin — une colonne par ligne et les traits qui les
//! relient — et non délégué à `git log --graph`, dont la sortie est un dessin
//! en caractères qu'il faudrait re-parser pour en refaire des coordonnées.

use std::path::Path;

use anyhow::Result;

use super::git;

/// Un commit tel que la liste l'affiche.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    pub short: String,
    /// Parents dans l'ordre de git : le premier est la ligne principale.
    pub parents: Vec<String>,
    pub summary: String,
    pub author: String,
    /// Date relative, telle que git la formule.
    pub date: String,
    /// Branches et étiquettes pointant sur ce commit.
    pub refs: Vec<String>,
}

impl Commit {
    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }
}

/// Ce que l'historique montre.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogRange {
    /// L'historique du checkout courant.
    Head,
    /// Ce que la branche a ajouté depuis sa divergence d'avec `base` — le même
    /// domaine que la revue de branche, vu comme une suite de commits.
    Branch { base: String },
    /// Toutes les références : c'est là que le graphe prend son sens, les
    /// branches parallèles étant visibles côte à côte.
    All,
}

impl LogRange {
    fn args(&self) -> Vec<String> {
        match self {
            Self::Head => vec!["HEAD".into()],
            Self::Branch { base } => vec![format!("{base}..HEAD")],
            // `--all` sans `--topo-order` entrelacerait les branches par date,
            // ce qui donne un graphe illisible : les traits sauteraient d'une
            // branche à l'autre à chaque ligne.
            Self::All => vec!["--all".into(), "--topo-order".into()],
        }
    }
}

/// Séparateur de champs. Un caractère de contrôle qu'aucun message de commit
/// ne contient, là où `|` ou `\t` finissent toujours par apparaître dans un
/// sujet un jour ou l'autre.
const FIELD: char = '\u{1f}';

pub fn commits(dir: &Path, range: &LogRange, limit: usize) -> Result<Vec<Commit>> {
    let format = format!("--format=%H{f}%h{f}%P{f}%an{f}%ar{f}%D{f}%s", f = "%x1f");
    let mut args: Vec<String> = vec![
        "log".into(),
        "-z".into(),
        format,
        format!("--max-count={limit}"),
    ];
    args.extend(range.args());
    let out = git(dir, &args)?;
    Ok(parse(&out))
}

/// Les sujets des derniers commits, du plus récent au plus ancien.
///
/// Ils servent d'exemple à l'agent qui propose un message : la langue, la
/// personne du verbe et les préfixes éventuels d'un dépôt ne se devinent pas,
/// et une consigne écrite ici les imposerait à tous les dépôts. Un dépôt neuf
/// n'en a aucun, ce qui n'est pas une erreur — d'où la liste vide.
pub fn recent_subjects(dir: &Path, limit: usize) -> Vec<String> {
    super::git_opt(
        dir,
        &[
            "log".to_string(),
            "-z".to_string(),
            "--format=%s".to_string(),
            format!("--max-count={limit}"),
        ],
    )
    .map(|out| {
        super::split_nul(&out)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

fn parse(out: &str) -> Vec<Commit> {
    super::split_nul(out).filter_map(parse_commit).collect()
}

fn parse_commit(record: &str) -> Option<Commit> {
    // `git log -z` sépare les commits par un octet nul mais laisse le saut de
    // ligne que `--format` termine ; il déborderait sur le champ suivant.
    let record = record.trim_start_matches('\n');
    let mut f = record.split(FIELD);
    let id = f.next()?.to_string();
    if id.is_empty() {
        return None;
    }
    let short = f.next().unwrap_or_default().to_string();
    let parents = f
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let author = f.next().unwrap_or_default().to_string();
    let date = f.next().unwrap_or_default().to_string();
    let refs = parse_refs(f.next().unwrap_or_default());
    let summary = f.next().unwrap_or_default().to_string();

    Some(Commit {
        id,
        short,
        parents,
        summary,
        author,
        date,
        refs,
    })
}

/// `%D` rend « HEAD -> main, origin/main, tag: v1.2 ».
fn parse_refs(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            // `HEAD -> main` désigne la branche déployée : on garde le nom de
            // la branche, la flèche n'apprend rien de plus que la puce déjà
            // portée par la ligne courante.
            s.strip_prefix("HEAD -> ").unwrap_or(s).to_string()
        })
        .collect()
}

/// La place d'un commit dans le graphe.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GraphRow {
    /// Colonne où se trouve la puce du commit.
    pub column: usize,
    /// Colonnes traversées de part en part par un trait vertical, sans rapport
    /// avec ce commit-ci : d'autres branches qui continuent.
    pub through: Vec<usize>,
    /// Colonnes d'où descend un trait vers la puce : les rails que ce commit
    /// referme, c'est-à-dire ses enfants placés ailleurs.
    pub incoming: Vec<usize>,
    /// Colonnes vers lesquelles part un trait sous la puce : ses parents
    /// placés ailleurs, donc les branches qu'un merge rassemble.
    pub outgoing: Vec<usize>,
}

/// Calcule la disposition du graphe.
///
/// L'algorithme est celui de tous les visualiseurs d'historique : on tient une
/// liste de rails, chacun attendant un commit précis. Un commit prend le rail
/// qui l'attendait — ou en ouvre un —, y installe son premier parent, et place
/// ses autres parents sur des rails voisins. Les rails libérés sont réutilisés
/// avant d'en ouvrir de nouveaux, ce qui garde le graphe étroit.
///
/// La sortie a exactement autant d'entrées que l'entrée : la vue les affiche
/// côte à côte, un décalage d'une ligne ferait pointer chaque trait sur le
/// mauvais commit.
pub fn layout(commits: &[Commit]) -> Vec<GraphRow> {
    // Chaque case est le commit attendu par ce rail, ou `None` si libre.
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());

    for commit in commits {
        // Tous les rails qui attendaient ce commit : plusieurs enfants peuvent
        // l'attendre, et ils convergent tous vers la même puce.
        let waiting: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(_, lane)| lane.as_deref() == Some(commit.id.as_str()))
            .map(|(ix, _)| ix)
            .collect();

        let column = match waiting.first() {
            Some(&first) => first,
            // Personne ne l'attendait : c'est une pointe de branche.
            None => free_lane(&mut lanes),
        };

        // Les rails traversants sont ceux qui restent occupés et ne touchent
        // pas cette ligne. Relevés avant la mise à jour, sinon le premier
        // parent qu'on va installer y figurerait à tort.
        let through: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(ix, lane)| *ix != column && lane.is_some() && !waiting.contains(ix))
            .map(|(ix, _)| ix)
            .collect();

        // Les rails surnuméraires qui attendaient ce commit se referment.
        let incoming: Vec<usize> = waiting.iter().skip(1).copied().collect();
        for &ix in &incoming {
            lanes[ix] = None;
        }

        // Le premier parent continue sur le rail du commit ; sans parent, le
        // rail se libère (commit racine).
        lanes[column] = commit.parents.first().cloned();

        let mut outgoing = Vec::new();
        for parent in commit.parents.iter().skip(1) {
            // Un parent déjà attendu ailleurs ne mérite pas un rail de plus :
            // le trait rejoint celui qui existe.
            let target = lanes
                .iter()
                .position(|lane| lane.as_deref() == Some(parent.as_str()))
                .unwrap_or_else(|| {
                    let ix = free_lane(&mut lanes);
                    lanes[ix] = Some(parent.clone());
                    ix
                });
            outgoing.push(target);
        }

        rows.push(GraphRow {
            column,
            through,
            incoming,
            outgoing,
        });
    }

    rows
}

/// Le premier rail libre, ou un nouveau. Réutiliser les trous plutôt que
/// d'empiler évite qu'un graphe s'élargisse indéfiniment le long d'un
/// historique un peu vivant.
fn free_lane(lanes: &mut Vec<Option<String>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(ix) => ix,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

/// Nombre de colonnes occupées par un graphe, pour dimensionner la gouttière.
pub fn width(rows: &[GraphRow]) -> usize {
    rows.iter()
        .map(|row| {
            let max = row
                .through
                .iter()
                .chain(row.incoming.iter())
                .chain(row.outgoing.iter())
                .copied()
                .max()
                .unwrap_or(0);
            max.max(row.column) + 1
        })
        .max()
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(id: &str, parents: &[&str]) -> Commit {
        Commit {
            id: id.into(),
            short: id.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            summary: format!("commit {id}"),
            author: "Autrice".into(),
            date: "hier".into(),
            refs: Vec::new(),
        }
    }

    fn record(fields: &[&str]) -> String {
        fields.join("\u{1f}")
    }

    #[test]
    fn reads_a_commit_with_its_refs_and_parents() {
        let out = format!(
            "{}\0{}\0",
            record(&[
                "abc123def",
                "abc123d",
                "parent1 parent2",
                "Une autrice",
                "il y a 2 heures",
                "HEAD -> main, origin/main, tag: v1.0",
                "Corrige le rendu du diff",
            ]),
            record(&[
                "parent1",
                "parent1",
                "",
                "Quelqu'un",
                "hier",
                "",
                "Le commit initial",
            ]),
        );
        let commits = parse(&out);
        assert_eq!(commits.len(), 2);

        let first = &commits[0];
        assert_eq!(first.id, "abc123def");
        assert_eq!(first.parents, vec!["parent1", "parent2"]);
        assert!(first.is_merge());
        assert_eq!(first.summary, "Corrige le rendu du diff");
        assert_eq!(first.author, "Une autrice");
        // La flèche de `HEAD -> main` est retirée, le reste est conservé.
        assert_eq!(first.refs, vec!["main", "origin/main", "tag: v1.0"]);

        // Un commit racine n'a pas de parent, et ce n'est pas une erreur.
        assert!(commits[1].parents.is_empty());
        assert!(commits[1].refs.is_empty());
    }

    #[test]
    fn a_straight_history_stays_on_one_column() {
        let commits = vec![commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])];
        let rows = layout(&commits);
        assert!(rows.iter().all(|r| r.column == 0));
        assert!(rows.iter().all(|r| r.through.is_empty()));
        assert!(rows.iter().all(|r| r.outgoing.is_empty()));
        assert_eq!(width(&rows), 1);
    }

    #[test]
    fn a_merge_opens_a_second_column_and_closes_it() {
        //   m      merge de f dans main
        //   |\
        //   | f    la branche
        //   |/
        //   b      la base commune
        let commits = vec![
            commit("m", &["b2", "f"]),
            commit("b2", &["b"]),
            commit("f", &["b"]),
            commit("b", &[]),
        ];
        let rows = layout(&commits);

        // Le merge est sur la colonne principale et envoie un trait vers la
        // colonne du second parent.
        assert_eq!(rows[0].column, 0);
        assert_eq!(rows[0].outgoing, vec![1]);

        // La branche vit sur la colonne ouverte pour elle.
        assert_eq!(rows[2].column, 1);
        // Pendant ce temps, la colonne principale continue.
        assert!(rows[2].through.contains(&0));

        // La base commune referme la seconde colonne : les deux rails
        // l'attendaient, seul le premier porte la puce.
        assert_eq!(rows[3].column, 0);
        assert_eq!(rows[3].incoming, vec![1]);

        assert_eq!(width(&rows), 2);
        assert_eq!(rows.len(), commits.len(), "une ligne par commit");
    }

    #[test]
    fn parallel_branches_reuse_freed_columns() {
        // Deux pointes indépendantes puis leur base : la colonne libérée par
        // la première doit resservir plutôt que d'en ouvrir une troisième.
        let commits = vec![
            commit("x", &["base"]),
            commit("y", &["base"]),
            commit("base", &[]),
            commit("z", &[]),
        ];
        let rows = layout(&commits);
        assert_eq!(rows[0].column, 0);
        assert_eq!(rows[1].column, 1);
        assert_eq!(rows[2].column, 0);
        assert_eq!(rows[2].incoming, vec![1], "les deux rails convergent");
        // `z` n'est attendu par personne : il reprend la colonne 0, libérée.
        assert_eq!(rows[3].column, 0);
        assert_eq!(width(&rows), 2);
    }

    #[test]
    fn an_octopus_merge_places_every_parent() {
        let commits = vec![commit("o", &["p1", "p2", "p3"])];
        let rows = layout(&commits);
        assert_eq!(rows[0].column, 0);
        assert_eq!(rows[0].outgoing, vec![1, 2]);
        assert_eq!(width(&rows), 3);
    }

    /// Lit l'historique d'un vrai dépôt — celui-ci — et vérifie que chaque
    /// champ arrive rempli. Le format et son séparateur sont exactement le
    /// genre de chose qui marche sur un exemple écrit à la main et se décale
    /// d'un champ sur la sortie réelle.
    #[test]
    fn reads_this_repository() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let Ok(commits) = commits(dir, &LogRange::Head, 5) else {
            return; // pas de dépôt git à la construction : rien à vérifier
        };
        assert!(!commits.is_empty(), "ce dépôt a des commits");

        for commit in &commits {
            assert_eq!(commit.id.len(), 40, "empreinte complète attendue");
            assert!(!commit.short.is_empty());
            assert!(
                !commit.summary.is_empty(),
                "le sujet du commit {} est vide — le format a glissé d'un champ",
                commit.short
            );
            assert!(!commit.author.is_empty(), "auteur manquant");
            assert!(!commit.date.is_empty(), "date manquante");
            // Le sujet ne doit pas avoir avalé les champs suivants.
            assert!(
                !commit.summary.contains('\u{1f}'),
                "le séparateur a fuité dans le sujet : {}",
                commit.summary
            );
        }

        let rows = layout(&commits);
        assert_eq!(rows.len(), commits.len());
    }

    #[test]
    fn ranges_use_the_right_revision_syntax() {
        assert_eq!(LogRange::Head.args(), vec!["HEAD"]);
        assert_eq!(
            LogRange::Branch {
                base: "main".into()
            }
            .args(),
            vec!["main..HEAD"]
        );
        // L'ordre topologique est ce qui garde les branches groupées.
        assert!(LogRange::All.args().contains(&"--topo-order".to_string()));
    }
}
