// A tab written end to end, from what the `claudhub` module says of the
// worktree: its agents, what waits for a commit, its open tasks.
//
// The module's functions read — `worktree()`, `changes()`, `tasks()`,
// `terminals()` — and act — `open_terminal()`, `toggle_task(line, done)`,
// `open_tab(name)` —; Claudhub runs this again whenever what they read may
// have changed. Types: `gpui-kit.d.ts`, written beside this file.

import { View, div } from "gpui-kit";
import { h_flex, v_flex, Button, Checkbox } from "gpui-base";
import {
  worktree,
  changes,
  tasks,
  terminals,
  language,
  open_terminal,
  open_tab,
  toggle_task,
} from "claudhub";

/** A titled block. @param {string} title @param {any} cx */
const section = (title, cx) =>
  v_flex()
    .gap(8)
    .p(12)
    .border(1)
    .rounded(8)
    .border_color(cx.theme().colors.border)
    .bg(cx.theme().colors.surface)
    .child(div().text_size(13).font_semibold().child(title));

/** @param {string} text @param {any} cx */
const muted = (text, cx) => div().text_size(12).text_color(cx.theme().colors.muted_foreground).child(text);

/** @param {string} id @param {string} caption @param {() => void} act @param {any} cx */
const button = (id, caption, act, cx) =>
  Button.new(id)
    .flex()
    .items_center()
    .h(26)
    .px(10)
    .border(1)
    .rounded(6)
    .border_color(cx.theme().colors.border)
    .text_size(12)
    .hover((style) => style.bg(cx.theme().colors.muted))
    .on_click(() => act())
    .child(caption);

export default class Dashboard extends View {
  render(cx) {
    const colors = cx.theme().colors;
    const tree = worktree();
    const diff = changes();
    const open = tasks().filter((task) => !task.done);
    const agents = terminals();
    const french = language().startsWith("fr");
    const say = (fr, en) => (french ? fr : en);

    const agentRows = agents.length
      ? agents.map((terminal) =>
          h_flex()
            .gap(8)
            .items_center()
            .child(
              div()
                .w(8)
                .h(8)
                .rounded(4)
                .bg(
                  terminal.doing === "waiting"
                    ? colors.destructive
                    : terminal.doing === "working"
                      ? colors.primary
                      : colors.border,
                ),
            )
            .child(div().text_size(12).child(terminal.label)),
        )
      : [muted(say("Aucun terminal.", "No terminal."), cx)];

    const taskRows = open.length
      ? open.slice(0, 20).map((task) =>
          Checkbox.new(`task-${task.line}`)
            .checked(false)
            .flex()
            .items_center()
            .gap(8)
            .on_change(() => toggle_task(task.line, true))
            .child(div().text_size(12).child(task.label)),
        )
      : [muted(say("Rien d'ouvert.", "Nothing open."), cx)];

    return v_flex()
      .size_full()
      .gap(12)
      .overflow_y_scrollbar()
      .child(
        h_flex()
          .gap(8)
          .items_center()
          .child(div().text_size(16).font_semibold().child(tree.name))
          .child(muted(tree.branch ?? say("détachée", "detached"), cx)),
      )
      .child(
        section(say("Agents", "Agents"), cx)
          .children(agentRows)
          .child(button("open-terminal", say("Nouveau terminal", "New terminal"), open_terminal, cx)),
      )
      .child(
        section(say("À valider", "To commit"), cx)
          .child(
            muted(
              diff.files
                ? say(
                    `${diff.files} fichier(s), +${diff.added} −${diff.removed}`,
                    `${diff.files} file(s), +${diff.added} −${diff.removed}`,
                  )
                : say("Rien à valider.", "Nothing to commit."),
              cx,
            ),
          )
          .child(button("open-git", say("Ouvrir Git", "Open Git"), () => open_tab("git"), cx)),
      )
      .child(section(say("Tâches ouvertes", "Open tasks"), cx).children(taskRows));
  }
}
