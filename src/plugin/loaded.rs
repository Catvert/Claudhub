//! One plugin as the window holds it: its script, its state, its last tree.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use rune::Value;

use crate::plugin::host::{Effect, Host, Request, Shared};
use crate::plugin::manifest::Manifest;
use crate::plugin::view::Node;

pub struct Plugin {
    pub manifest: Manifest,
    shared: Arc<Shared>,
    /// Behind an `Rc` so a task can hold it across an `await` without holding
    /// a borrow of the application. There is one machine and one script; the
    /// `Rc` is about lifetimes, not about sharing.
    host: Option<Rc<Host>>,
    /// What went wrong, shown **in the panel** and not in the status bar — the
    /// bar is overwritten by the next message, and a script that does not
    /// compile is exactly what one comes back to read twice. It is the same
    /// choice the database tree makes for its `DbResult`.
    pub error: Option<String>,
    state: Option<Value>,
    /// The last tree the script produced, behind an `Rc`: the render closure
    /// runs on every frame and must compute nothing. The rule of
    /// `diff_view::Rendered`.
    pub tree: Rc<Node>,
    /// A gesture has gone out and not come back.
    pub busy: bool,
}

impl Plugin {
    /// Loads a plugin. A script that does not compile still gives a `Plugin`:
    /// the panel exists, and it says why it is empty.
    pub fn load(manifest: Manifest, outbox: async_channel::Sender<Request>) -> Self {
        let shared = Shared::new(&manifest, outbox);
        let mut plugin = Self {
            manifest,
            shared,
            host: None,
            error: None,
            state: None,
            tree: Rc::new(Node::nothing()),
            busy: false,
        };
        plugin.compile();
        plugin
    }

    /// Recompiles the script and forgets the state.
    ///
    /// **The state does not survive a reload**, and that is deliberate:
    /// recompiling gives new shapes to the same names, and stale data read by
    /// fresh code fails in ways nobody can explain. A reload replays `init`,
    /// which is what one wants after editing the code that fetches.
    ///
    /// A compilation that fails **keeps the previous machine**: an editor saves
    /// halfway through a word, and losing a working panel on every keystroke
    /// would make the reload worse than a restart.
    pub fn reload(&mut self) {
        let had = self.host.is_some();
        self.shared.fail_all("the plugin was reloaded");
        let compiled = self.compile();
        if compiled || !had {
            self.state = None;
            self.tree = Rc::new(Node::nothing());
        }
        self.busy = false;
    }

    /// True when a new machine took the old one's place.
    fn compile(&mut self) -> bool {
        match Host::load(&self.manifest, self.shared.clone()) {
            Ok(host) => {
                self.host = Some(Rc::new(host));
                self.error = None;
                true
            }
            Err(message) => {
                log::warn!(target: "plugin", "{}: {message}", self.manifest.id);
                self.error = Some(message);
                false
            }
        }
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// The machine, when there is one to run.
    pub fn host(&self) -> Option<Rc<Host>> {
        self.host.clone()
    }

    /// Has it been started on this worktree yet.
    pub fn started(&self) -> bool {
        self.state.is_some()
    }

    pub fn set_worktree(&mut self, worktree: Option<&Path>) {
        self.shared.set_worktree(worktree.map(|p| p.to_path_buf()));
        // A plugin's panel speaks about the worktree the window shows. Changing
        // it is starting over, not refreshing: what was fetched described
        // somewhere else.
        self.state = None;
        self.tree = Rc::new(Node::nothing());
        self.busy = false;
        // The failure goes with it, and that is not tidiness: `init` is only
        // replayed while there is no error to show, so a plugin that failed for
        // want of a worktree would never try again once one is open.
        if self.host.is_some() {
            self.error = None;
        }
        self.shared.fail_all("the worktree changed");
    }

    /// Records a new state and repaints from it.
    ///
    /// The two go together on purpose: a state nobody rendered is a gesture
    /// that changed nothing on screen, which reads as a dead button.
    pub fn settle(&mut self, state: Value) {
        self.state = Some(state);
        self.repaint();
    }

    /// `view(state)`, once, outside any frame.
    pub fn repaint(&mut self) {
        let (Some(host), Some(state)) = (self.host.clone(), self.state.as_ref()) else {
            return;
        };
        match host.view(state) {
            Ok(tree) => {
                self.tree = Rc::new(tree);
                self.error = None;
            }
            Err(message) => {
                log::warn!(target: "plugin", "{}: {message}", self.manifest.id);
                self.error = Some(message);
            }
        }
    }

    pub fn state(&self) -> Option<Value> {
        self.state.clone()
    }

    pub fn fail(&mut self, message: String) {
        log::warn!(target: "plugin", "{}: {message}", self.manifest.id);
        self.error = Some(message);
        self.busy = false;
    }

    /// What the script asked of the window while it ran.
    pub fn take_effects(&self) -> Vec<Effect> {
        self.shared.take_effects()
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        // A request nobody will ever answer must not leave a future waiting for
        // good: the rule the language client lives by, one lane over.
        self.shared.fail_all("the plugin went away");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manifest;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("claudhub-reload-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("demo")).expect("mkdir");
        std::fs::write(dir.join("demo").join("plugin.toml"), "title = \"Demo\"\n")
            .expect("manifest");
        dir
    }

    fn write_script(dir: &Path, source: &str) {
        std::fs::write(dir.join("demo").join("main.rn"), source).expect("script");
    }

    const GOOD: &str = r#"
        use claudhub::*;
        pub async fn init(worktree) { Ok("bonjour") }
        pub fn view(s) { text(s) }
    "#;

    /// A reload that fails **keeps the machine that worked**.
    ///
    /// An editor saves halfway through a word: losing a working panel on every
    /// keystroke would make the hot reload worse than a restart. What the user
    /// gets instead is the previous tree, and the error beside it.
    #[test]
    fn a_broken_reload_keeps_what_was_working() {
        let root = scratch("keeps");
        write_script(&root, GOOD);
        let found = manifest::discover(&root);
        let (sender, _outbox) = async_channel::unbounded();
        let mut plugin = Plugin::load(found[0].clone(), sender);
        assert!(plugin.error.is_none(), "{:?}", plugin.error);

        let state =
            crate::runtime::executor::block_on(plugin.host().expect("a machine").init(None))
                .expect("init");
        plugin.settle(state);
        assert_eq!(
            *plugin.tree,
            Node::Text {
                text: "bonjour".into(),
                style: crate::plugin::view::TextStyle::Body
            }
        );

        // Saved mid-word.
        write_script(&root, "use claudhub::*; pub fn view(s) { text( }");
        plugin.reload();
        assert!(plugin.error.is_some(), "the failure has to be said");
        assert!(plugin.host().is_some(), "the working machine stays");
        // And the state with it: repainting still gives the previous tree
        // rather than an empty panel.
        assert_eq!(
            *plugin.tree,
            Node::Text {
                text: "bonjour".into(),
                style: crate::plugin::view::TextStyle::Body
            }
        );
    }

    /// A reload that succeeds **forgets the state**: recompiling gives new
    /// shapes to the same names, and `init` is replayed — which is what one
    /// wants after editing the code that fetches.
    #[test]
    fn a_good_reload_starts_over() {
        let root = scratch("starts-over");
        write_script(&root, GOOD);
        let found = manifest::discover(&root);
        let (sender, _outbox) = async_channel::unbounded();
        let mut plugin = Plugin::load(found[0].clone(), sender);
        let state =
            crate::runtime::executor::block_on(plugin.host().expect("a machine").init(None))
                .expect("init");
        plugin.settle(state);
        assert!(plugin.started());

        write_script(&root, GOOD.replace("bonjour", "bonsoir").as_str());
        plugin.reload();
        assert!(plugin.error.is_none(), "{:?}", plugin.error);
        assert!(!plugin.started(), "the state is forgotten");
        assert_eq!(*plugin.tree, Node::nothing());
    }
}
