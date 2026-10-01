// The boards' home, rearranged: three columns of Claudhub's own cards.
//
// Every card is painted by Claudhub — `BranchCard.new(id)` and the others come
// from the `claudhub` module —; this script only decides where each stands.
// Move a line, save: the boards reload it. To use it, pick it as the boards'
// home in Settings → Scripts.

import { View } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import {
  BranchCard,
  PullRequestCard,
  ChangesCard,
  RunCard,
  ReviewCard,
  TasksCard,
  NoteCard,
  Terminals,
} from "claudhub";

/** A column of cards, scrolling on its own. */
const column = () => v_flex().flex_1().min_w_0().gap(12).overflow_y_scrollbar();

export default class ColumnsHome extends View {
  render() {
    return h_flex()
      .size_full()
      .gap(12)
      .child(
        column()
          .child(BranchCard.new("branch"))
          .child(PullRequestCard.new("pull-request"))
          .child(ChangesCard.new("changes"))
          .child(RunCard.new("run")),
      )
      .child(
        column()
          .child(ReviewCard.new("review"))
          .child(TasksCard.new("tasks"))
          .child(NoteCard.new("note")),
      )
      .child(v_flex().flex_1().min_w_0().child(Terminals.new("terminals")));
  }
}
