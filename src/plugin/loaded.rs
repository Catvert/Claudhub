//! One plugin as the window holds it: its script, its state, its last tree.

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use rune::Value;

use crate::plugin::host::{Compiler, Effect, Host, Request, Shared};
use crate::plugin::manifest::Manifest;
use crate::plugin::view::Node;

/// How long a call runs before the window says it is running.
///
/// Long enough that a gesture with no round trip in it never shows anything,
/// short enough that a fetch says so before one wonders whether the click
/// landed. See `Plugin::working`.
pub const SHOW_AFTER: std::time::Duration = std::time::Duration::from_millis(200);

pub struct Plugin {
    pub manifest: Manifest,
    shared: Arc<Shared>,
    /// What the script compiles against, built once and kept across reloads:
    /// Rune's standard library and the claudhub module do not change when a
    /// file is saved. See `host::Compiler`.
    compiler: Option<Compiler>,
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
    /// The last tree the script produced **for each panel**, behind an `Rc`:
    /// the render closure runs on every frame and must compute nothing. The
    /// rule of `diff_view::Rendered`.
    ///
    /// Keyed by panel id and not by dock name: this is the script's vocabulary,
    /// and it is what `view`'s second argument carries.
    trees: HashMap<String, Rc<Node>>,
    /// A gesture has gone out and not come back.
    pub busy: bool,
    /// When it went out. What tells a round trip worth a spinner from one that
    /// is over before the eye catches it — see `Plugin::working`.
    since: Option<std::time::Instant>,
    /// The panel it went out from, when a gesture is what sent it.
    ///
    /// `None` for a first load, which nobody asked for from anywhere. What it
    /// decides is `waiting_in`.
    from: Option<&'static str>,
}

impl Plugin {
    /// Loads a plugin. A script that does not compile still gives a `Plugin`:
    /// the panel exists, and it says why it is empty.
    pub fn load(manifest: Manifest, outbox: async_channel::Sender<Request>) -> Self {
        let shared = Shared::new(&manifest, outbox);
        let compiler = match Compiler::new(&shared) {
            Ok(compiler) => Some(compiler),
            Err(message) => {
                log::warn!(target: "plugin", "{}: {message}", manifest.id);
                None
            }
        };
        let mut plugin = Self {
            manifest,
            shared,
            compiler,
            host: None,
            error: None,
            state: None,
            trees: HashMap::new(),
            busy: false,
            since: None,
            from: None,
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
            self.trees.clear();
        }
        self.settled();
    }

    /// True when a new machine took the old one's place.
    fn compile(&mut self) -> bool {
        let loaded = match &self.compiler {
            Some(compiler) => Host::load(&self.manifest, self.shared.clone(), compiler),
            // Rune's own standard library did not come up. Nothing will
            // compile, and the panel says so like any other failure.
            None => Err("Rune's context could not be built".to_string()),
        };
        match loaded {
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
        self.trees.clear();
        self.settled();
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
        self.settled();
        self.repaint();
    }

    /// Something has gone out. `from` is the panel whose gesture sent it, and
    /// `None` a first load.
    pub fn start(&mut self, from: Option<&'static str>) {
        self.busy = true;
        self.since = Some(std::time::Instant::now());
        self.from = from;
    }

    fn settled(&mut self) {
        self.busy = false;
        self.since = None;
        self.from = None;
    }

    /// Is there something out **long enough to say so**.
    ///
    /// Not `busy`, and the difference is a grace period: a gesture that changes
    /// no state — copying an identifier, opening a browser — comes back in a
    /// frame or two, and saying it is running blanks the panels one is reading
    /// for the length of a blink. What that reads as is a flicker on every
    /// click, which is worse than the second of silence it was meant to
    /// explain. A call that really is a round trip crosses the threshold and
    /// says so, as it always did.
    ///
    /// `busy` stays what it is — the guard that keeps a first load from being
    /// asked for twice — and this is only ever what is **painted**.
    pub fn working(&self) -> bool {
        self.busy
            && self
                .since
                .is_some_and(|since| since.elapsed() >= SHOW_AFTER)
    }

    /// Pretends the grace period has gone by, for a test that cannot wait.
    #[cfg(test)]
    fn aged(&mut self) {
        self.since = self
            .since
            .map(|since| since.checked_sub(SHOW_AFTER).unwrap_or(since));
    }

    /// Does this panel show a spinner where its tree was.
    ///
    /// **Every panel but the one the gesture came from.** A gesture moves the
    /// state and each panel is a reading of it, so the others are stale by
    /// construction — keeping them painted shows the detail of the row one has
    /// just stopped looking at. The panel one clicked in is the exception, and
    /// it is not a detail: it is what one is looking at and what one just used,
    /// and blanking it takes away the context of one's own gesture. A first
    /// load has no exception to make, which is right — nothing is known yet.
    pub fn waiting_in(&self, panel: &str) -> bool {
        self.working() && self.from != Some(panel)
    }

    /// What a panel last painted. An empty column while nothing has run yet.
    pub fn tree_of(&self, panel: &str) -> Rc<Node> {
        self.trees
            .get(panel)
            .cloned()
            .unwrap_or_else(|| Rc::new(Node::nothing()))
    }

    /// `view(state, panel)` for every panel the plugin declares, once, outside
    /// any frame.
    ///
    /// All of them and not only the one on screen: two panels of a plugin are
    /// two readings of one state, and painting the visible one alone would
    /// leave the other showing what the state said before the gesture. The cost
    /// is one pure call per panel, which is the reason `view` is required to be
    /// pure and synchronous.
    pub fn repaint(&mut self) {
        let (Some(host), Some(state)) = (self.host.clone(), self.state.clone()) else {
            return;
        };
        let panels: Vec<String> = self.manifest.panels.iter().map(|p| p.id.clone()).collect();
        for panel in panels {
            match host.view(&state, &panel) {
                Ok(tree) => {
                    self.trees.insert(panel, Rc::new(tree));
                    self.error = None;
                }
                Err(message) => {
                    log::warn!(target: "plugin", "{}: {message}", self.manifest.id);
                    self.error = Some(message);
                }
            }
        }
    }

    pub fn state(&self) -> Option<Value> {
        self.state.clone()
    }

    pub fn fail(&mut self, message: String) {
        log::warn!(target: "plugin", "{}: {message}", self.manifest.id);
        self.error = Some(message);
        self.settled();
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
        scratch_with(name, "title = \"Demo\"\n")
    }

    fn scratch_with(name: &str, manifest: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("claudhub-reload-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("demo")).expect("mkdir");
        std::fs::write(dir.join("demo").join("plugin.toml"), manifest).expect("manifest");
        dir
    }

    fn write_script(dir: &Path, source: &str) {
        std::fs::write(dir.join("demo").join("main.rn"), source).expect("script");
    }

    const GOOD: &str = r#"
        use claudhub::*;
        pub async fn init(worktree) { Ok("bonjour") }
        pub fn view(s, panel) { text(s) }
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
            *plugin.tree_of("main"),
            Node::Text {
                text: "bonjour".into(),
                style: crate::plugin::view::TextStyle::Body
            }
        );

        // Saved mid-word.
        write_script(&root, "use claudhub::*; pub fn view(s, panel) { text( }");
        plugin.reload();
        assert!(plugin.error.is_some(), "the failure has to be said");
        assert!(plugin.host().is_some(), "the working machine stays");
        // And the state with it: repainting still gives the previous tree
        // rather than an empty panel.
        assert_eq!(
            *plugin.tree_of("main"),
            Node::Text {
                text: "bonjour".into(),
                style: crate::plugin::view::TextStyle::Body
            }
        );
    }

    /// **Both panels are painted, from one state, on every settling.**
    ///
    /// Not only the one on screen: two panels of a plugin are two readings of
    /// the same thing, and painting the visible one alone would leave the other
    /// showing what the state said before the gesture — which on a master/detail
    /// plugin is the detail of the row one just stopped looking at.
    #[test]
    fn two_panels_read_one_state() {
        let root = scratch_with(
            "two-panels",
            "title = \"Demo\"\n\n[[panel]]\nid = \"list\"\ntitle = \"L\"\n\n[[panel]]\nid = \"detail\"\ntitle = \"D\"\nplace = \"centre\"\n",
        );
        write_script(
            &root,
            r#"
            use claudhub::*;
            pub async fn init(worktree) { Ok("un") }
            pub fn view(s, panel) { text(`${panel}:${s}`) }
            pub async fn update(s, action, payload) { Ok(payload) }
            "#,
        );
        let found = manifest::discover(&root);
        let (sender, _outbox) = async_channel::unbounded();
        let mut plugin = Plugin::load(found[0].clone(), sender);

        let said = |plugin: &Plugin, panel: &str| match &*plugin.tree_of(panel) {
            Node::Text { text, .. } => text.clone(),
            other => panic!("expected text, got {other:?}"),
        };

        let host = plugin.host().expect("a machine");
        let state = crate::runtime::executor::block_on(host.init(None)).expect("init");
        plugin.settle(state);
        assert_eq!(said(&plugin, "list"), "list:un");
        assert_eq!(said(&plugin, "detail"), "detail:un");

        // One gesture, and **both** move.
        let state = plugin.state().expect("a state");
        let state =
            crate::runtime::executor::block_on(host.update(&state, "set", "deux")).expect("update");
        plugin.settle(state);
        assert_eq!(said(&plugin, "list"), "list:deux");
        assert_eq!(said(&plugin, "detail"), "detail:deux");

        // A panel nobody declared has nothing to say, rather than the last
        // thing some other panel said.
        assert_eq!(*plugin.tree_of("nowhere"), Node::nothing());
    }

    /// **Only the panel the answer will land in waits.**
    ///
    /// Opening an issue from the list must leave the list alone: it is what one
    /// is looking at and what one just used, and blanking it takes away the
    /// context of one's own gesture. A first load has no exception to make.
    #[test]
    fn waiting_is_shown_where_the_answer_will_land() {
        let root = scratch_with(
            "waiting",
            "title = \"Demo\"\n\n[[panel]]\nid = \"list\"\ntitle = \"L\"\n\n[[panel]]\nid = \"detail\"\ntitle = \"D\"\nplace = \"centre\"\n",
        );
        write_script(&root, GOOD);
        let found = manifest::discover(&root);
        let (sender, _outbox) = async_channel::unbounded();
        let mut plugin = Plugin::load(found[0].clone(), sender);
        let (list, detail) = (found[0].panels[0].name, found[0].panels[1].name);

        // Nothing out: nobody waits.
        assert!(!plugin.waiting_in(list));
        assert!(!plugin.waiting_in(detail));

        // A first load: nothing is known, so both are blank — **once the
        // grace period is over**. Before it, a gesture that comes back in a
        // frame has blanked nothing at all.
        plugin.start(None);
        assert!(
            !plugin.waiting_in(list),
            "the grace period holds the spinner"
        );
        assert!(!plugin.waiting_in(detail));
        plugin.aged();
        assert!(plugin.waiting_in(list));
        assert!(plugin.waiting_in(detail));

        let state =
            crate::runtime::executor::block_on(plugin.host().expect("a machine").init(None))
                .expect("init");
        plugin.settle(state);
        assert!(!plugin.waiting_in(list), "settling ends the wait");
        assert!(!plugin.waiting_in(detail));

        // A gesture made in the list: the list keeps its rows, the detail waits
        // for what the click asked for.
        plugin.start(Some(list));
        plugin.aged();
        assert!(!plugin.waiting_in(list));
        assert!(plugin.waiting_in(detail));

        // And a failure ends it as surely as an answer: a panel that spins for
        // good says less than one that says why.
        plugin.fail("nope".into());
        assert!(!plugin.waiting_in(detail));
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
        assert_eq!(*plugin.tree_of("main"), Node::nothing());
    }
}
