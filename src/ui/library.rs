//! The agent library, from the application's side: the skills of each
//! worktree as the worker listed them (`crate::library`), what the
//! `claudhub` module answers of them, and the chat a skill is launched in.
//!
//! The list is asked for the first time a script reads it, and again when
//! one says so (`reload_skills`, which the library plugin calls as it
//! mounts): skills change by hand or by a pull, not by the second, and
//! nothing sweeps them.
//!
//! **A skill's text does not travel with the list a script reads**: the
//! script runs again at every notification of the application, and the
//! bodies of eighty skills copied into it each time would be most of what
//! it does. `skill_body` hands over the one being read.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::{Context, Window};
use gpui_shell::{HostObject, HostValue};

use crate::library::{Reader, Scope, Skill};
use crate::runtime::protocol::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;

/// The skills listed, per worktree.
#[derive(Default)]
pub(crate) struct Library {
    lists: HashMap<PathBuf, Rc<Vec<Skill>>>,
    /// Asked for and not answered: a read does not ask again meanwhile.
    asked: HashSet<PathBuf>,
}

impl ClaudhubApp {
    /// A worktree's skills, for a script: `null` while the first listing is
    /// on its way — asked for here, the first time.
    pub(super) fn script_skills(&mut self, worktree: &Path) -> HostValue {
        let Some(skills) = self.library.lists.get(worktree).cloned() else {
            if !self.library.asked.contains(worktree) {
                self.reload_library(worktree);
            }
            return HostValue::Null;
        };
        HostValue::Array(skills.iter().map(skill_value).collect())
    }

    /// The text of the skill whose folder is `dir`, past its front matter.
    pub(super) fn script_skill_body(&self, worktree: &Path, dir: &str) -> HostValue {
        self.library
            .lists
            .get(worktree)
            .and_then(|skills| skills.iter().find(|skill| skill.dir == Path::new(dir)))
            .map_or(HostValue::Null, |skill| HostValue::from(skill.body.clone()))
    }

    /// Lists a worktree's skills again. Always sent: a listing asked while
    /// the WSL server was still starting is lost, and the gesture that asks
    /// again must not wait on its answer.
    pub(super) fn reload_library(&mut self, worktree: &Path) {
        self.library.asked.insert(worktree.to_path_buf());
        self.git.send(Cmd::LibraryLoad {
            worktree: worktree.to_path_buf(),
        });
    }

    pub(super) fn library_arrived(
        &mut self,
        worktree: PathBuf,
        skills: Vec<Skill>,
        cx: &mut Context<Self>,
    ) {
        self.library.asked.remove(&worktree);
        let skills = Rc::new(skills);
        if self.library.lists.get(&worktree) != Some(&skills) {
            self.library.lists.insert(worktree, skills);
            cx.notify();
        }
    }

    /// Copies a skill into the project. Only one the library listed for
    /// this worktree, and not the project's own: the folder comes from a
    /// script, and any folder copied into a checkout is one commit away
    /// from being published.
    pub(super) fn share_skill(&mut self, worktree: &Path, dir: &str) -> Result<(), String> {
        let skill = self
            .library
            .lists
            .get(worktree)
            .and_then(|skills| skills.iter().find(|skill| skill.dir == Path::new(dir)))
            .ok_or_else(|| format!("`{dir}` is not a skill of this worktree's library"))?;
        if skill.scope == Scope::Project {
            return Err(format!("`{}` is the project's already", skill.name));
        }
        self.git.send(Cmd::LibraryShare {
            worktree: worktree.to_path_buf(),
            dir: skill.dir.clone(),
            reader: skill.reader,
        });
        Ok(())
    }

    /// Writes skill `name`'s companion prompt into the project, where a
    /// commit shares it — removed when blank.
    pub(super) fn save_skill_prompt(&mut self, worktree: &Path, name: &str, text: &str) {
        self.git.send(Cmd::LibraryPrompt {
            worktree: worktree.to_path_buf(),
            name: name.to_string(),
            text: text.to_string(),
        });
    }

    /// The chat agents the settings offer, by the name a script gives.
    pub(super) fn script_chat_agents(cx: &gpui_kit::App) -> HostValue {
        HostValue::Array(
            super::settings::Settings::global(cx)
                .terminal
                .chat_agents()
                .iter()
                .map(|agent| HostValue::from(agent.label()))
                .collect(),
        )
    }

    /// Opens a chat with the agent named `agent` on `worktree`, sends it
    /// `prompt` once its session is up, and makes it the terminal the
    /// board's `Terminals` show — where the library shows it.
    pub(super) fn start_library_agent(
        &mut self,
        worktree: &Path,
        agent: &str,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let settings = super::settings::Settings::global(cx);
        let placement = settings.terminal.placement;
        let Some(chosen) = settings
            .terminal
            .chat_agents()
            .into_iter()
            .find(|known| known.label() == agent)
        else {
            self.announce_error(tr!("library-no-agent", { agent: agent }), cx);
            return;
        };
        let before = self.chats.len();
        self.open_chat(worktree, chosen, placement, None, window, cx);
        if self.chats.len() == before {
            return;
        }
        let Some(open) = self.chats.last() else {
            return;
        };
        let (id, view) = (open.id(), open.view.clone());
        view.update(cx, |view, _| view.open_with(prompt));
        self.home_terminal.insert(worktree.to_path_buf(), id);
        cx.notify();
    }
}

/// A skill as a script reads it, without its text.
fn skill_value(skill: &Skill) -> HostValue {
    HostObject::new()
        .field("name", skill.name.clone())
        .field("description", skill.description.clone())
        .field("argument_hint", skill.argument_hint.clone())
        .field("prompt", skill.prompt.clone())
        .field("dir", skill.dir.to_string_lossy().into_owned())
        .field("file", skill.file().to_string_lossy().into_owned())
        .field(
            "scope",
            match skill.scope {
                Scope::Project => "project",
                Scope::Personal => "personal",
                Scope::Plugin => "plugin",
            },
        )
        .field(
            "reader",
            match skill.reader {
                Reader::Claude => "claude",
                Reader::Codex => "codex",
            },
        )
        .field(
            "plugin",
            skill
                .plugin
                .clone()
                .map_or(HostValue::Null, HostValue::from),
        )
        .into()
}
