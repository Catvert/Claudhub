// The agent library, as a tab of every board: the skills an agent started in
// this worktree can read against the left, the one chosen beside it — what it
// is for, where it lies, its instructions — and an agent launched on it.
//
// What it takes from Claudhub:
// - the **skills** (`skills()`), listed by Claudhub where Claude Code and
//   Codex look — the project's `.claude/skills/`, `.agents/skills/`, the
//   user's, the installed plugins' —, and the text of the one being read
//   (`skill_body`), which does not travel with the list;
// - **sharing** (`share_skill`): a personal or plugin skill copied into the
//   project, where a commit hands it to the team — the project's library is
//   its repository's;
// - **launching** (`start_agent`): a chat with one of the agents the settings
//   offer, opened on this worktree, the skill as its first message; it shows
//   under the Agents face (`Terminals`), as on the board's home;
// - the **companion prompt** of each skill (`save_skill_prompt`): what this
//   team asks of it, written once into the project — `.claudhub/prompts/`,
//   versioned — and laid in the field each time the skill is chosen, to be
//   completed before it goes. `{{branch}}` and the others are filled in at
//   the launch (`expand`);
// - **personal prompts** beside it (`storage_*`): kept on this machine, in
//   the plugin's data and never in the repository, for every skill of that
//   name — tried for oneself, then written into the project when it is
//   worth sharing.
//
// The prompt names the `SKILL.md` rather than counting on the agent to find
// the skill itself: Codex reads a skill of Claude's folders that way, and a
// skill launched by name only works where its agent looks.

import { View, div } from "gpui-kit";
import {
  h_flex,
  v_flex,
  Button,
  Input,
  InputState,
  Textarea,
  TextareaState,
  TextView,
  Scrollbar,
  v_virtual_list,
} from "gpui-base";
import {
  language,
  skills,
  skill_body,
  reload_skills,
  share_skill,
  save_skill_prompt,
  chat_agents,
  start_agent,
  terminals,
  worktree,
  storage_get,
  storage_set,
  copy_text,
  notify,
  sizes,
  palette,
  Icon,
  Badge,
  Terminals,
} from "claudhub";
import {
  SCOPES,
  SCOPE_TONES,
  SCOPE_ICONS,
  VARIABLES,
  PROJECT,
  projectFolder,
  promptFile,
  expand,
  countByScope,
  shown,
  personalOf,
  withPersonal,
  newPromptId,
  labelOf,
  savedOf,
} from "./library.js";
import { textsFor } from "./texts.js";

/** A stored value as the object it should be — anything else, an empty one. */
const record = (value) => (value !== null && typeof value === "object" && !Array.isArray(value) ? value : {});

export default class AgentLibrary extends View {
  init(_props, cx) {
    this.t = textsFor(language());
    this.filter = InputState.new({ placeholder: this.t.filter });
    this.filter.on("change", (_event, cx) => cx.notify());
    this.task = TextareaState.new({ placeholder: this.t.task, rows: 4 });
    this.task.on("submit", (_event, cx) => this.launch(cx));
    this.task.on("change", (_event, cx) => cx.notify());
    // The skill and the prompt the field holds, and that prompt as it was
    // laid there: one saved or pulled since replaces what was not touched.
    this.laid = { dir: null, key: null, text: "" };
    this.scope = null;
    this.chosen = null;
    this.face = "sheet";
    this.agent = storage_get("agent");
    // Listed again at each opening: a skill written, pulled or installed
    // since is there without asking.
    reload_skills();
  }

  /** The skill being read, if the list still holds it. */
  current(all) {
    return all.find((skill) => skill.dir === this.chosen) ?? null;
  }

  /** The agent a launch goes to: the one chosen last, while the settings still offer it. */
  agentOf(agents) {
    return agents.includes(this.agent) ? this.agent : (agents[0] ?? null);
  }

  /** The personal prompts, every skill's: read at each use, another board may have written them. */
  kept() {
    return record(storage_get("personal"));
  }

  /** The prompt chosen for a skill — the one chosen last, while it is there; the project's otherwise. */
  keyFor(skill, personal) {
    const key = record(storage_get("chosen"))[skill.name];
    return key && personal.some((prompt) => prompt.id === key) ? key : PROJECT;
  }

  choose(skill, key) {
    storage_set("chosen", { ...record(storage_get("chosen")), [skill.name]: key });
  }

  /**
   * Lays the chosen prompt in the field — once per skill and prompt, and
   * again when it changed under a field left as it was: saved from another
   * board, pulled.
   */
  lay(skill) {
    const personal = personalOf(this.kept(), skill.name);
    const key = this.keyFor(skill, personal);
    const saved = savedOf(skill, personal, key);
    const typed = this.task.value().trim();
    if (this.laid.dir === skill.dir && this.laid.key === key) {
      // Saved as it stands: what is laid is what is kept now.
      if (typed === saved.trim()) {
        this.laid.text = saved;
        return;
      }
      if (this.laid.text === saved || typed !== this.laid.text.trim()) return;
    }
    this.task.set_value(saved);
    this.laid = { dir: skill.dir, key, text: saved };
  }

  launch(cx) {
    const skill = this.current(skills() ?? []);
    const agent = this.agentOf(chat_agents());
    if (!skill || !agent) return;
    start_agent(agent, this.t.prompt(skill, expand(this.task.value().trim(), worktree())));
    this.face = "agents";
    cx.notify();
  }

  savePrompt(skill) {
    save_skill_prompt(skill.name, this.task.value().trim());
  }

  /** Writes `text` — the field's by default — into the personal prompt `key`, or into a new one when `key` is null; then chooses it. */
  keep(skill, key, text = this.task.value().trim()) {
    const kept = this.kept();
    const list = personalOf(kept, skill.name);
    const id = key ?? newPromptId(list);
    const next = key ? list.map((prompt) => (prompt.id === key ? { id, text } : prompt)) : [...list, { id, text }];
    storage_set("personal", withPersonal(kept, skill.name, next));
    this.choose(skill, id);
  }

  forget(skill, key) {
    const kept = this.kept();
    const list = personalOf(kept, skill.name).filter((prompt) => prompt.id !== key);
    storage_set("personal", withPersonal(kept, skill.name, list));
    this.choose(skill, PROJECT);
    notify(this.t.forgotten);
  }

  // — Drawing ——————————————————————————————————————————————————————————

  render(cx) {
    const all = skills();
    const list = shown(all ?? [], this.scope, this.filter.value());
    if (this.current(all ?? []) === null && list.length > 0) this.chosen = list[0].dir;
    return h_flex()
      .size_full()
      .gap_3()
      .child(this.listPane(all, list, cx))
      .child(v_flex().flex_1().min_w_0().h_full().gap_2().child(this.faces(cx)).child(this.page(all, cx)));
  }

  /** A small button: an icon, a label or both. */
  button(id, { icon, label, tone, primary = false, disabled = false }, act, cx) {
    const colors = cx.theme().colors;
    return Button.new(id)
      .flex()
      .flex_none()
      .items_center()
      .gap_1()
      .h(26)
      .px_2()
      .rounded(6)
      .text_xs()
      .when(primary, (el) => el.bg(colors.primary).text_color(colors.primary_foreground))
      .when(!primary, (el) => el.text_color(colors.foreground))
      .when(!disabled, (el) => el.hover((style) => style.bg(primary ? colors.primary : colors.secondary)))
      .when(disabled, (el) => el.opacity(0.4))
      .disabled(disabled)
      .when(!disabled, (el) =>
        el.on_click((_event, cx) => {
          act(cx);
          cx.notify();
        }),
      )
      .when(Boolean(icon), (el) => el.child(Icon.new(`${id}-icon`, { name: icon, size: "xsmall", tone })))
      .when(Boolean(label), (el) => el.child(label));
  }

  /** A chip: a word, a dot of colour, lit when chosen. */
  chip(id, { label, tone, lit }, act, cx) {
    const colors = cx.theme().colors;
    const ink = tone ? palette()[tone] : null;
    return Button.new(id)
      .flex()
      .flex_none()
      .items_center()
      .gap_1()
      .h(22)
      .px_2()
      .rounded(11)
      .text_xs()
      .border(1)
      .border_color(lit ? (ink ?? colors.ring) : colors.border)
      .bg(lit ? `${ink ?? palette().primary}26` : colors.background)
      .text_color(lit ? colors.foreground : colors.muted_foreground)
      .hover((style) => style.bg(colors.secondary))
      .on_click((_event, cx) => {
        act();
        cx.notify();
      })
      .when(ink !== null, (el) => el.child(div().w(7).h(7).rounded(4).bg(ink)))
      .child(label);
  }

  /** A sentence in place of what is not there, and what to do about it. */
  note(id, icon, text, cx) {
    return v_flex()
      .flex_1()
      .items_center()
      .justify_center()
      .gap_2()
      .p_4()
      .child(Icon.new(id, { name: icon, size: "large", tone: "muted" }))
      .child(div().text_sm().text_center().text_color(cx.theme().colors.muted_foreground).child(text));
  }

  // — The list ———————————————————————————————————————————————————————————

  listPane(all, list, cx) {
    const colors = cx.theme().colors;
    const bar = h_flex()
      .flex_none()
      .w_full()
      .gap_1()
      .items_center()
      .child(Icon.new("bar-icon", { name: "book-open", size: "xsmall" }))
      .child(
        Input.new(this.filter)
          .flex_1()
          .min_w_0()
          .h(26)
          .px_2()
          .rounded(6)
          .border(1)
          .border_color(colors.input)
          .bg(colors.background)
          .text_xs(),
      )
      .child(this.button("reload", { icon: "refresh-cw" }, () => reload_skills(), cx));
    let body;
    if (all === null) body = this.note("loading-icon", "book-open", this.t.loading, cx);
    else if (all.length === 0) body = this.note("empty-icon", "book-open", this.t.empty, cx);
    else body = v_flex().flex_1().min_h_0().child(this.listHead(all, list, cx)).child(this.rows(list, cx));
    return v_flex()
      .w(360)
      .flex_none()
      .h_full()
      .gap_1()
      .child(bar)
      .child(
        v_flex()
          .flex_1()
          .min_h_0()
          .rounded(8)
          .border(1)
          .border_color(colors.border)
          .bg(colors.background)
          .overflow_hidden()
          .child(body),
      );
  }

  /** The scopes — each a filter — and how many are on show. */
  listHead(all, list, cx) {
    const colors = cx.theme().colors;
    const counts = countByScope(all);
    return v_flex()
      .flex_none()
      .gap_2()
      .px_2()
      .py_2()
      .border_b(1)
      .border_color(colors.border)
      .child(
        h_flex()
          .gap_1()
          .flex_wrap()
          .child(this.chip("scope-all", { label: this.t.all, lit: this.scope === null }, () => (this.scope = null), cx))
          .children(
            SCOPES.filter((scope) => counts[scope] > 0).map((scope) =>
              this.chip(
                `scope-${scope}`,
                { label: `${counts[scope]} ${this.t.scopes[scope]}`, tone: SCOPE_TONES[scope], lit: this.scope === scope },
                () => (this.scope = this.scope === scope ? null : scope),
                cx,
              ),
            ),
          ),
      )
      .child(
        div()
          .text_xs()
          .text_color(colors.muted_foreground)
          .child(list.length === all.length ? this.t.count(all.length) : this.t.countFiltered(list.length, all.length)),
      );
  }

  rows(list, cx) {
    if (list.length === 0) return this.note("no-match-icon", "search", this.t.noMatch, cx);
    const height = Math.round(sizes().row * 2.4);
    return v_flex()
      .relative()
      .flex_1()
      .min_h_0()
      .child(
        v_virtual_list(
          "library-skills",
          list.length,
          height,
          (index) => list[index].dir,
          (range) => list.slice(range.start, range.end).map((skill) => this.row(skill, height, cx)),
        )
          .size_full()
          .on_item_click((key, cx) => {
            this.chosen = key;
            this.face = "sheet";
            cx.notify();
          }),
      )
      .child(Scrollbar.vertical("library-skills").absolute().inset_0());
  }

  /** One row, two storeys: the name and where it comes from, then what it is for. */
  row(skill, height, cx) {
    const colors = cx.theme().colors;
    const tones = palette();
    const selected = this.chosen === skill.dir;
    const ink = tones[SCOPE_TONES[skill.scope]];
    return h_flex()
      .w_full()
      .h(height)
      .items_center()
      .cursor_pointer()
      .border_b(1)
      .border_color(`${tones.border}80`)
      .bg(selected ? `${tones.primary}1f` : colors.background)
      .hover((style) => style.bg(selected ? `${tones.primary}2e` : tones.list_hover))
      // The band: the scope, read before anything.
      .child(div().flex_none().w(3).h_full().bg(selected ? tones.primary : ink))
      .child(
        v_flex()
          .flex_1()
          .min_w_0()
          .px_2()
          .gap(2)
          .child(
            h_flex()
              .gap_2()
              .items_center()
              .child(Icon.new(`${skill.dir}-scope`, { name: SCOPE_ICONS[skill.scope], size: "xsmall", tone: SCOPE_TONES[skill.scope] }))
              .child(div().flex_1().min_w_0().truncate().text_sm().font_semibold().child(skill.name))
              .when(skill.plugin !== null, (el) =>
                el.child(div().flex_none().max_w(110).truncate().text_xs().text_color(colors.muted_foreground).child(skill.plugin)),
              )
              .child(Badge.new(`${skill.dir}-reader`, { text: this.t.readers[skill.reader], tone: "muted" })),
          )
          .child(div().truncate().text_xs().text_color(colors.muted_foreground).child(skill.description)),
      );
  }

  // — The right side ————————————————————————————————————————————————————————

  /** The two faces: the skill being read, and the agents running on the worktree. */
  faces(cx) {
    const colors = cx.theme().colors;
    const running = terminals().length;
    const face = (key, label) =>
      Button.new(`face-${key}`)
        .flex()
        .items_center()
        .h(22)
        .px_3()
        .rounded(6)
        .text_xs()
        .when(this.face === key, (el) => el.bg(colors.background).text_color(colors.foreground))
        .when(this.face !== key, (el) =>
          el.text_color(colors.muted_foreground).hover((style) => style.text_color(colors.foreground)),
        )
        .on_click((_event, cx) => {
          this.face = key;
          cx.notify();
        })
        .child(label);
    return h_flex()
      .flex_none()
      .child(
        h_flex()
          .p(2)
          .gap(2)
          .rounded(8)
          .bg(colors.muted)
          .child(face("sheet", this.t.sheet))
          .child(face("agents", running > 0 ? `${this.t.agents} · ${running}` : this.t.agents)),
      );
  }

  page(all, cx) {
    if (this.face === "agents") {
      if (terminals().length === 0) return this.note("no-agent-icon", "bot", this.t.noTerminal, cx);
      return v_flex().flex_1().min_h_0().child(Terminals.new("library-terminals"));
    }
    const skill = this.current(all ?? []);
    if (!skill) return this.note("pick-icon", "book-open", this.t.pick, cx);
    this.lay(skill);
    return v_flex()
      .flex_1()
      .min_h_0()
      .gap_3()
      .child(this.head(skill, cx))
      .child(this.launcher(skill, cx))
      .child(this.instructions(skill, cx));
  }

  /** What it is, where it lies, and whether the project has it. */
  head(skill, cx) {
    const colors = cx.theme().colors;
    const shared =
      skill.scope === "project"
        ? h_flex()
            .gap_1()
            .items_center()
            .text_xs()
            .text_color(palette().success)
            .child(Icon.new("shared-icon", { name: "git-branch", size: "xsmall", tone: "success" }))
            .child(this.t.shared)
        : h_flex()
            .gap_2()
            .items_center()
            .child(this.button("share", { icon: "folder-input", label: this.t.share }, () => share_skill(skill.dir), cx))
            .child(div().flex_1().min_w_0().text_xs().text_color(colors.muted_foreground).child(this.t.shareHelp(projectFolder(skill.reader))));
    return v_flex()
      .flex_none()
      .gap_2()
      .child(
        h_flex()
          .gap_2()
          .items_center()
          .child(div().text_lg().font_semibold().child(skill.name))
          .child(Badge.new("head-scope", { text: this.t.scope[skill.scope], tone: SCOPE_TONES[skill.scope] }))
          .child(Badge.new("head-reader", { text: this.t.readers[skill.reader], tone: "muted" }))
          .when(skill.plugin !== null, (el) => el.child(Badge.new("head-plugin", { text: skill.plugin, tone: "primary" }))),
      )
      .when(skill.description !== "", (el) => el.child(div().text_sm().child(skill.description)))
      .child(
        h_flex()
          .gap_1()
          .items_center()
          .child(
            div()
              .flex_1()
              .min_w_0()
              .truncate()
              .text_xs()
              .font_family(cx.theme().typography.mono)
              .text_color(colors.muted_foreground)
              .child(skill.file),
          )
          .child(
            this.button("copy-path", { icon: "copy" }, () => {
              copy_text(skill.file);
              notify(this.t.copied);
            }, cx),
          ),
      )
      .child(shared);
  }

  /** The agent, the companion prompt — saved with the project or not —, and the button. */
  launcher(skill, cx) {
    const colors = cx.theme().colors;
    const agents = chat_agents();
    const agent = this.agentOf(agents);
    const box = v_flex().flex_none().gap_2().p_3().rounded(8).border(1).border_color(colors.border).bg(colors.background);
    const promptHead = this.promptHead(skill, cx);
    if (agent === null) {
      return box
        .child(promptHead)
        .child(this.promptField(cx))
        .child(div().text_xs().text_color(colors.muted_foreground).child(this.t.noAgent));
    }
    return box
      .child(
        h_flex()
          .gap_1()
          .flex_wrap()
          .items_center()
          .child(div().text_xs().text_color(colors.muted_foreground).mr_1().child(this.t.launchWith))
          .children(
            agents.map((name, at) =>
              this.chip(`agent-${at}`, { label: name, lit: name === agent }, () => {
                this.agent = name;
                storage_set("agent", name);
              }, cx),
            ),
          ),
      )
      .child(promptHead)
      .child(this.promptField(cx))
      .child(div().text_xs().text_color(colors.muted_foreground).child(this.t.variables(VARIABLES)))
      .child(
        h_flex()
          .gap_2()
          .items_center()
          .child(
            div()
              .flex_1()
              .min_w_0()
              .text_xs()
              .text_color(colors.muted_foreground)
              .child(skill.argument_hint !== "" ? this.t.taskHint(skill.argument_hint) : this.t.launchHelp),
          )
          .child(this.button("launch", { icon: "play", label: `${this.t.launch} · ${agent}`, primary: true }, (cx) => this.launch(cx), cx)),
      );
  }

  /**
   * The prompts one can lay in the field — the project's, then one's own —,
   * where the chosen one is kept, and what can be done with what is typed.
   */
  promptHead(skill, cx) {
    const colors = cx.theme().colors;
    const personal = personalOf(this.kept(), skill.name);
    const key = this.keyFor(skill, personal);
    const ours = key === PROJECT;
    const saved = savedOf(skill, personal, key);
    const typed = this.task.value().trim();
    const changed = typed !== saved.trim();
    const status = changed
      ? this.t.unsaved
      : ours
        ? skill.prompt === ""
          ? this.t.noCompanion
          : this.t.savedIn(promptFile(skill.name))
        : this.t.keptHere;
    const chips = h_flex()
      .gap_1()
      .flex_wrap()
      .items_center()
      .child(div().text_xs().font_semibold().mr_1().child(this.t.companion))
      .child(this.chip("prompt-project", { label: this.t.projectPrompt, tone: "success", lit: ours }, () => this.choose(skill, PROJECT), cx))
      .children(
        personal.map((prompt) =>
          this.chip(`prompt-${prompt.id}`, { label: labelOf(prompt.text, this.t.untitled), tone: "info", lit: key === prompt.id }, () =>
            this.choose(skill, prompt.id), cx),
        ),
      )
      .child(this.button("prompt-new", { icon: "plus", label: this.t.newPersonal }, () => this.keep(skill, null, ""), cx));
    // What the typed text becomes: the chosen prompt saved, or the other kind.
    const gestures = h_flex()
      .gap_1()
      .flex_wrap()
      .items_center()
      .child(
        div()
          .flex_1()
          .min_w_0()
          .truncate()
          .text_xs()
          .text_color(changed ? palette().warning : colors.muted_foreground)
          .child(status),
      )
      .when(changed && saved !== "", (el) =>
        el.child(this.button("revert-prompt", { icon: "undo-2", label: this.t.revert }, () => this.task.set_value(saved), cx)),
      )
      .when(ours, (el) =>
        el
          .child(
            this.button(
              "save-prompt",
              { icon: "save", label: typed === "" && saved !== "" ? this.t.removePrompt : this.t.savePrompt, disabled: !changed },
              () => this.savePrompt(skill),
              cx,
            ),
          )
          .child(this.button("keep-prompt", { icon: "user", label: this.t.keepForMe, disabled: typed === "" }, () => this.keep(skill, null), cx)),
      )
      .when(!ours, (el) =>
        el
          .child(this.button("save-mine", { icon: "save", label: this.t.saveForMe, disabled: !changed }, () => this.keep(skill, key), cx))
          .child(
            this.button(
              "to-project",
              { icon: "git-branch", label: this.t.toProject, disabled: typed === "" || typed === skill.prompt.trim() },
              () => this.savePrompt(skill),
              cx,
            ),
          )
          .child(this.button("forget-mine", { icon: "trash-2", label: this.t.forget }, () => this.forget(skill, key), cx)),
      );
    return v_flex().gap_1().child(chips).child(gestures);
  }

  promptField(cx) {
    const colors = cx.theme().colors;
    return Textarea.new(this.task)
      .w_full()
      .min_h(80)
      .p_2()
      .rounded(6)
      .border(1)
      .border_color(colors.input)
      .bg(colors.background)
      .text_xs();
  }

  /** The `SKILL.md` past its header, as Markdown. */
  instructions(skill, cx) {
    const colors = cx.theme().colors;
    const body = skill_body(skill.dir) ?? "";
    return v_flex()
      .flex_1()
      .min_h_0()
      .gap_1()
      .child(div().flex_none().text_xs().font_semibold().text_color(colors.muted_foreground).child(this.t.instructions))
      .child(
        v_flex()
          .flex_1()
          .min_h_0()
          .p_3()
          .rounded(8)
          .border(1)
          .border_color(colors.border)
          .bg(colors.background)
          .overflow_y_scrollbar()
          .child(
            body.trim() === ""
              ? div().text_xs().text_color(colors.muted_foreground).child(this.t.noBody)
              : TextView.markdown(`body:${skill.dir}`, body).selectable(true).w_full().text_sm(),
          ),
      );
  }
}
