//! Les workers git.
//!
//! Un petit groupe de threads OS consomme le même canal de commandes et
//! répond par des événements. Des threads plutôt qu'un exécuteur async parce
//! que `std::process::Command` bloque de toute façon : une commande git, c'est
//! un `fork`, une attente, et rien à entrelacer entre les deux.
//!
//! Plusieurs threads plutôt qu'un seul parce qu'un `git fetch` sur un dépôt
//! distant lent gèlerait sinon le rafraîchissement du statut et l'affichage
//! des diffs. Git protège lui-même l'index par un verrou `index.lock` ; deux
//! écritures concurrentes échouent proprement au lieu de se corrompre.

pub mod protocol;
pub mod watch;

use std::path::{Path, PathBuf};

use anyhow::Result;

pub use protocol::{Action, Cmd, Evt, WorktreeId};

use crate::git::{branch, diff, history, repo, status};

/// Workers dédiés aux lectures : statut, diffs, branches, et les écritures
/// locales, qui se comptent toutes en millisecondes.
const READERS: usize = 3;

/// Les opérations qui parlent au réseau ont leur propre file.
///
/// Un `fetch` sur un dépôt distant lent, une authentification qui attend, une
/// connexion qui expire : ces commandes-là se comptent en secondes, parfois en
/// dizaines de secondes. Partager la file avec les lectures faisait qu'un
/// `pull` malheureux emportait un worker sur trois, et trois de suite figeaient
/// l'interface entière — plus de statut, plus de diff, plus rien, sans que
/// personne puisse relier cela au bouton qui l'a déclenché.
fn is_network(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::Fetch { .. }
            | Cmd::Pull { .. }
            | Cmd::Push { .. }
            | Cmd::LoadIssues { .. }
            | Cmd::LoadIssueEvent { .. }
    )
}

/// Le balayage de fond : résumé de chaque worktree et recherche des agents.
///
/// Il a sa propre file pour la même raison que le réseau : il porte sur tous
/// les worktrees ouverts, il revient toutes les quelques secondes, et il ne
/// doit jamais passer devant le diff qu'on vient de demander.
fn is_background(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::LoadSummaries { .. } | Cmd::ScanAgents { .. } | Cmd::WtScan { .. }
    )
}

/// Les opérations de `wt` qui lancent les hooks du projet.
///
/// Elles ont la file du réseau, et pour la même raison : un `post_new` qui
/// installe des dépendances, un `up` qui démarre des conteneurs, cela dure des
/// minutes. Les mettre avec les lectures reviendrait à figer la revue le temps
/// d'un `composer install`.
fn is_long(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::WtCreate { .. } | Cmd::WtRemove { .. } | Cmd::WtUp { .. } | Cmd::WtDown { .. }
    )
}

pub struct Handle {
    reads: async_channel::Sender<Cmd>,
    network: async_channel::Sender<Cmd>,
    background: async_channel::Sender<Cmd>,
}

impl Handle {
    /// Envoie une commande dans la file qui lui convient. L'échec (canal
    /// fermé) n'arrive qu'à l'extinction : il n'y a rien à en dire à
    /// l'utilisateur, dont la fenêtre se ferme.
    pub fn send(&self, cmd: Cmd) {
        let queue = if is_network(&cmd) || is_long(&cmd) {
            &self.network
        } else if is_background(&cmd) {
            &self.background
        } else {
            &self.reads
        };
        if let Err(err) = queue.try_send(cmd) {
            log::debug!("commande abandonnée : {err}");
        }
    }
}

/// Démarre les workers et rend de quoi leur parler et les écouter.
pub fn spawn() -> (Handle, async_channel::Receiver<Evt>) {
    let (read_tx, read_rx) = async_channel::unbounded::<Cmd>();
    let (net_tx, net_rx) = async_channel::unbounded::<Cmd>();
    let (bg_tx, bg_rx) = async_channel::unbounded::<Cmd>();
    let (evt_tx, evt_rx) = async_channel::unbounded::<Evt>();

    for n in 0..READERS {
        worker(format!("claudhub-git-{n}"), read_rx.clone(), evt_tx.clone());
    }
    // Un seul pour le réseau : deux `fetch` simultanés sur le même dépôt se
    // disputeraient le verrou des références sans rien accélérer.
    worker("claudhub-git-net".into(), net_rx, evt_tx.clone());
    worker("claudhub-scan".into(), bg_rx, evt_tx);

    (
        Handle {
            reads: read_tx,
            network: net_tx,
            background: bg_tx,
        },
        evt_rx,
    )
}

fn worker(
    name: String,
    commands: async_channel::Receiver<Cmd>,
    events: async_channel::Sender<Evt>,
) {
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            while let Ok(cmd) = commands.recv_blocking() {
                for evt in handle(cmd) {
                    if events.send_blocking(evt).is_err() {
                        return; // la fenêtre est partie
                    }
                }
            }
        })
        .expect("le système refuse de créer un thread");
}

/// Exécute une commande et rend les événements à publier.
///
/// Rendre un `Vec` plutôt que d'émettre au fil de l'eau garde cette fonction
/// pure et testable : elle ne connaît pas le canal.
fn handle(cmd: Cmd) -> Vec<Evt> {
    match cmd {
        Cmd::OpenRepo(path) => match open_repo(&path) {
            Ok(evt) => vec![evt],
            Err(e) => vec![fail(None, Action::Open, e)],
        },
        Cmd::RefreshRepo { main } => match (repo::Repo { main: main.clone() }).worktrees() {
            Ok(worktrees) => vec![Evt::Worktrees { main, worktrees }],
            Err(e) => vec![fail(None, Action::Refresh, e)],
        },
        Cmd::RefreshStatus { worktree } => match status::status(&worktree) {
            Ok(status) => vec![Evt::Status { worktree, status }],
            Err(e) => vec![fail(Some(worktree), Action::Refresh, e)],
        },
        Cmd::LoadDiffFiles { worktree, range } => match diff::files(&worktree, &range) {
            Ok(files) => vec![Evt::DiffFiles {
                worktree,
                range,
                files,
            }],
            Err(e) => vec![fail(Some(worktree), Action::Diff, e)],
        },
        Cmd::LoadFileDiff {
            worktree,
            range,
            path,
            context,
            untracked,
        } => {
            let result = if untracked {
                diff::untracked_file(&worktree, &path)
            } else {
                diff::file(&worktree, &range, &path, context)
            };
            match result {
                Ok(diff) => vec![Evt::FileDiff {
                    worktree,
                    path,
                    diff,
                }],
                Err(e) => vec![fail(Some(worktree), Action::Diff, e)],
            }
        }
        Cmd::LoadHistory {
            worktree,
            range,
            limit,
        } => match history::commits(&worktree, &range, limit) {
            Ok(commits) => {
                let graph = history::layout(&commits);
                vec![Evt::History {
                    worktree,
                    range,
                    commits,
                    graph,
                }]
            }
            Err(e) => vec![fail(Some(worktree), Action::History, e)],
        },
        Cmd::LoadSummaries { worktrees } => {
            let summaries = worktrees
                .into_iter()
                .filter_map(|worktree| {
                    // Un worktree effacé sous nos pieds n'est pas une erreur à
                    // afficher : il disparaîtra de la liste au prochain
                    // `git worktree list`.
                    status::summary(&worktree)
                        .ok()
                        .map(|summary| (worktree, summary))
                })
                .collect();
            vec![Evt::Summaries { summaries }]
        }
        Cmd::ScanAgents {
            worktrees,
            programs,
        } => vec![Evt::Agents {
            agents: crate::agent::scan(&worktrees, &programs),
        }],
        Cmd::LoadBranches { main } => match branch::list(&main) {
            Ok(branches) => {
                let default_base = branch::default_base(&main);
                vec![Evt::Branches {
                    main,
                    branches,
                    default_base,
                }]
            }
            Err(e) => vec![fail(None, Action::Branch, e)],
        },

        Cmd::Stage { worktree, paths } => write_then_refresh(worktree, Action::Stage, |dir| {
            repo::stage(dir, &paths).map(|_| String::new())
        }),
        Cmd::Unstage { worktree, paths } => write_then_refresh(worktree, Action::Unstage, |dir| {
            repo::unstage(dir, &paths).map(|_| String::new())
        }),
        Cmd::Discard { worktree, paths } => write_then_refresh(worktree, Action::Discard, |dir| {
            repo::discard(dir, &paths).map(|_| String::new())
        }),
        Cmd::Delete { worktree, paths } => write_then_refresh(worktree, Action::Delete, |dir| {
            repo::clean(dir, &paths).map(|_| String::new())
        }),
        Cmd::ApplyHunk {
            worktree,
            patch,
            reverse,
        } => {
            let action = if reverse {
                Action::Unstage
            } else {
                Action::Stage
            };
            write_then_refresh(worktree, action, |dir| {
                repo::apply_patch(dir, &patch, reverse).map(|_| String::new())
            })
        }
        Cmd::Commit {
            worktree,
            message,
            amend,
            all,
        } => write_then_refresh(worktree, Action::Commit, |dir| {
            repo::commit(
                dir,
                repo::CommitOptions {
                    message: &message,
                    amend,
                    all,
                },
            )
        }),
        Cmd::Fetch { worktree } => {
            write_then_refresh(worktree, Action::Fetch, |dir| repo::fetch(dir, true))
        }
        Cmd::Pull { worktree } => write_then_refresh(worktree, Action::Pull, repo::pull),
        Cmd::Push {
            worktree,
            force_with_lease,
        } => write_then_refresh(worktree, Action::Push, |dir| {
            repo::push(dir, force_with_lease)
        }),
        Cmd::Checkout { worktree, branch } => {
            write_then_refresh(worktree, Action::Checkout, |dir| {
                repo::checkout(dir, &branch).map(|_| String::new())
            })
        }
        Cmd::CreateBranch {
            worktree,
            name,
            from,
        } => write_then_refresh(worktree, Action::Branch, |dir| {
            repo::create_branch(dir, &name, from.as_deref()).map(|_| String::new())
        }),
        Cmd::DeleteBranch { main, name, force } => match repo::delete_branch(&main, &name, force) {
            Ok(()) => {
                let mut evts = vec![done(None, Action::Branch, String::new())];
                let default_base = branch::default_base(&main);
                evts.extend(branch::list(&main).ok().map(|branches| Evt::Branches {
                    main,
                    branches,
                    default_base,
                }));
                evts
            }
            Err(e) => vec![fail(None, Action::Branch, e)],
        },
        Cmd::Merge {
            worktree,
            from,
            no_ff,
        } => write_then_refresh(worktree, Action::Merge, |dir| {
            repo::merge(dir, &from, no_ff)
        }),
        Cmd::Integrate {
            main,
            branch,
            base,
            no_ff,
        } => write_then_refresh(main, Action::Integrate, |dir| {
            integrate(dir, &branch, &base, no_ff)
        }),
        Cmd::Rebase { worktree, onto } => {
            write_then_refresh(worktree, Action::Rebase, |dir| repo::rebase(dir, &onto))
        }
        Cmd::AbortPending { worktree } => write_then_refresh(worktree, Action::Abort, repo::abort),
        Cmd::ResumePending { worktree } => {
            write_then_refresh(worktree, Action::Resume, repo::resume)
        }
        Cmd::ResolveConflict {
            worktree,
            path,
            ours,
        } => write_then_refresh(worktree, Action::Resolve, |dir| {
            repo::resolve(dir, &path, ours).map(|_| String::new())
        }),

        Cmd::LoadIssues {
            org,
            project,
            query,
        } => match sentry_token() {
            Some(token) => match crate::sentry::issues(&org, &project, &query, &token) {
                Ok(issues) => vec![Evt::Issues { issues }],
                Err(e) => vec![fail(None, Action::Sentry, e)],
            },
            None => vec![fail(
                None,
                Action::Sentry,
                anyhow::anyhow!("aucun jeton Sentry : SENTRY_TOKEN ou les réglages"),
            )],
        },
        Cmd::LoadIssueEvent { issue } => match sentry_token() {
            Some(token) => match crate::sentry::latest_event(&issue, &token) {
                Ok(event) => vec![Evt::IssueEvent { issue, event }],
                Err(e) => vec![fail(None, Action::Sentry, e)],
            },
            None => vec![fail(
                None,
                Action::Sentry,
                anyhow::anyhow!("aucun jeton Sentry : SENTRY_TOKEN ou les réglages"),
            )],
        },
        Cmd::ListFiles { worktree, ignored } => match repo::list_files(&worktree, ignored) {
            Ok(files) => vec![Evt::ProjectFiles { worktree, files }],
            Err(e) => vec![fail(Some(worktree), Action::Read, e)],
        },
        Cmd::ReadFile { worktree, path } => match crate::files::read(&worktree, &path) {
            Ok(content) => vec![Evt::FileContent {
                worktree,
                path,
                content,
            }],
            Err(e) => vec![fail(Some(worktree), Action::Read, e)],
        },
        Cmd::WriteFile {
            worktree,
            path,
            content,
            expect,
        } => write_then_refresh(worktree, Action::Write, |dir| {
            crate::files::write(dir, &path, &content, expect).map(|_| String::new())
        }),
        Cmd::FileOp { worktree, op } => write_then_refresh(worktree, Action::FileOp, |dir| {
            crate::files::apply(dir, &op).map(|_| op.target().display().to_string())
        }),
        Cmd::ReadNotes { worktree, dir } => match crate::files::read_notes(&dir) {
            Ok(files) => vec![Evt::NotesRead { worktree, files }],
            Err(e) => vec![fail(Some(worktree), Action::Notes, e)],
        },
        // La réponse ne porte pas le contenu — la vue tient déjà ce qu'elle
        // vient d'écrire — mais le fait que le dossier existe : il peut venir
        // de naître avec ce fichier, et jusque-là il n'y avait rien à
        // surveiller.
        Cmd::WriteNotes {
            worktree,
            dir,
            files,
        } => match crate::files::sync_notes(&dir, &files) {
            Ok(()) => vec![Evt::VaultWritten { worktree }],
            Err(e) => vec![fail(None, Action::Notes, e)],
        },
        Cmd::WriteTodo {
            worktree,
            path,
            text,
            expect,
        } => match crate::files::write_at(&path, &text, expect) {
            Ok(()) => vec![Evt::VaultWritten { worktree }],
            Err(e) => vec![fail(None, Action::Notes, e)],
        },
        Cmd::OpenExternal {
            worktree,
            path,
            line,
        } => {
            let template = editor_template();
            match crate::files::open_external(&worktree, &template, &path, line) {
                Ok(program) => vec![done(Some(worktree), Action::OpenExternal, program)],
                Err(e) => vec![fail(Some(worktree), Action::OpenExternal, e)],
            }
        }
        Cmd::WtLoad { main } => {
            let project = crate::wt::snapshot(&main);
            vec![Evt::WtProject { main, project }]
        }
        Cmd::WtQuestions {
            main,
            slug,
            answers,
        } => match crate::wt::questions(&main, &slug, &answers) {
            Ok(questions) => vec![Evt::WtQuestions {
                main,
                slug,
                answers,
                questions,
            }],
            Err(e) => vec![fail(None, Action::Worktree, e)],
        },
        Cmd::WtCreate {
            main,
            slug,
            from,
            answers,
        } => {
            let r = repo::Repo { main: main.clone() };
            match crate::wt::create(&main, &slug, from.as_deref(), &answers) {
                Ok((_, output)) => worktree_changed(&r, output),
                Err(e) => vec![fail(None, Action::Worktree, e)],
            }
        }
        Cmd::WtRemove { main, slug } => {
            let r = repo::Repo { main: main.clone() };
            match crate::wt::remove(&main, &slug) {
                Ok(output) => worktree_changed(&r, output),
                Err(e) => vec![fail(None, Action::Worktree, e)],
            }
        }
        Cmd::WtUp { main, slug } => match crate::wt::up(&main, &slug) {
            Ok(output) => vec![done(None, Action::WtUp, output)],
            Err(e) => vec![fail(None, Action::WtUp, e)],
        },
        Cmd::WtDown { main, slug } => match crate::wt::down(&main, &slug) {
            Ok(output) => vec![done(None, Action::WtDown, output)],
            Err(e) => vec![fail(None, Action::WtDown, e)],
        },
        Cmd::WtTask {
            main,
            worktree,
            slug,
            task,
        } => match crate::wt::task(&main, &slug, &task) {
            Ok(launch) => vec![Evt::WtTask {
                worktree,
                task,
                launch,
            }],
            Err(e) => vec![fail(Some(worktree), Action::Worktree, e)],
        },
        Cmd::WtScan { targets } => {
            let states = targets
                .into_iter()
                .filter_map(|(main, worktree)| {
                    let slug = crate::wt::slug_of(&main, &worktree)?;
                    Some((
                        worktree,
                        protocol::WtWorktree {
                            up: crate::wt::is_up(&main, &slug),
                            endpoints: crate::wt::endpoints(&main, &slug),
                        },
                    ))
                })
                .collect();
            vec![Evt::WtStates { states }]
        }

        Cmd::AddWorktree {
            main,
            path,
            branch,
            from,
        } => {
            let r = repo::Repo { main: main.clone() };
            match r.add_worktree(&path, &branch, from.as_deref()) {
                Ok(()) => worktree_changed(&r, path.display().to_string()),
                Err(e) => vec![fail(None, Action::Worktree, e)],
            }
        }
        Cmd::RemoveWorktree { main, path, force } => {
            let r = repo::Repo { main: main.clone() };
            match r.remove_worktree(&path, force) {
                Ok(()) => worktree_changed(&r, path.display().to_string()),
                Err(e) => vec![fail(None, Action::Worktree, e)],
            }
        }
    }
}

/// Intègre une branche dans la base, depuis le dépôt principal.
///
/// Les deux vérifications préalables ne sont pas de la prudence de principe :
/// fusionner dans un checkout sale mêle les modifications en cours au travail
/// intégré, et fusionner alors qu'on est sur une autre branche écrit dans
/// celle-là — deux dégâts qu'on découvre après coup, et qu'un message évite.
fn integrate(main: &Path, branch: &str, base: &str, no_ff: bool) -> Result<String> {
    if repo::is_dirty(main) {
        anyhow::bail!("le dépôt principal a des modifications en cours : validez-les ou rangez-les avant d'intégrer");
    }
    let current = branch::current(main);
    if current.as_deref() != Some(base) {
        anyhow::bail!(
            "le dépôt principal est sur « {} » et non sur « {base} »",
            current.as_deref().unwrap_or("HEAD détachée")
        );
    }
    repo::merge(main, branch, no_ff)
}

/// Le jeton Sentry, lu **dans le worker** et jamais transporté par une
/// commande : un secret n'a rien à faire dans une énumération qu'on
/// journalise. `SENTRY_TOKEN` l'emporte sur le fichier de réglages, qui est en
/// 0600 mais n'est pas un coffre pour autant.
fn sentry_token() -> Option<String> {
    crate::sentry::token(&crate::ui::Settings::load().sentry_token)
}

/// La commande de l'éditeur externe, telle que les réglages la portent.
///
/// Lue ici et non passée dans la commande : c'est un réglage global, et le
/// faire voyager dans chaque `Cmd::OpenExternal` reviendrait à le recopier
/// pour rien. Le worker n'a pas accès au global gpui — il passe donc par le
/// fichier, ce que fait déjà `Settings::load`.
fn editor_template() -> String {
    crate::ui::Settings::load().external_editor
}

fn open_repo(path: &Path) -> Result<Evt> {
    let r = repo::Repo::discover(path)?;
    let worktrees = r.worktrees()?;
    // Le chemin demandé peut être un sous-dossier du checkout ; on retient le
    // worktree le plus profond qui le contient, sinon un worktree imbriqué
    // dans un autre serait attribué au mauvais.
    let requested = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let opened_at = worktrees
        .iter()
        .filter(|w| requested.starts_with(&w.path))
        .max_by_key(|w| w.path.as_os_str().len())
        .map(|w| w.path.clone());
    Ok(Evt::RepoOpened {
        name: r.name(),
        worktrees,
        opened_at,
        main: r.main,
    })
}

/// Toute écriture est suivie d'une relecture du statut : c'est ce qui garde le
/// panneau de revue exact sans que la vue ait à savoir quelle commande touche
/// quoi. Le coût est un `git status` de plus par action déclenchée à la main.
fn write_then_refresh(
    worktree: PathBuf,
    action: Action,
    op: impl FnOnce(&Path) -> Result<String>,
) -> Vec<Evt> {
    match op(&worktree) {
        Ok(output) => {
            let mut evts = vec![done(Some(worktree.clone()), action, output)];
            if let Ok(status) = status::status(&worktree) {
                evts.push(Evt::Status { worktree, status });
            }
            evts
        }
        Err(e) => vec![fail(Some(worktree), action, e)],
    }
}

fn worktree_changed(r: &repo::Repo, output: String) -> Vec<Evt> {
    let mut evts = vec![done(None, Action::Worktree, output)];
    if let Ok(worktrees) = r.worktrees() {
        evts.push(Evt::Worktrees {
            main: r.main.clone(),
            worktrees,
        });
    }
    evts
}

fn done(worktree: Option<PathBuf>, action: Action, output: String) -> Evt {
    Evt::Done {
        worktree,
        action,
        output,
    }
}

/// Aplatit la chaîne de causes d'`anyhow` en une phrase : la vue affiche un
/// message, pas une trace.
fn fail(worktree: Option<PathBuf>, action: Action, err: anyhow::Error) -> Evt {
    let message = err
        .chain()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(" : ");
    log::warn!("{action:?} : {message}");
    Evt::Failed {
        worktree,
        action,
        message,
    }
}
