// The project's Sentry errors, as a tab of every board — the panel Claudhub
// draws in Rust, written as a plugin: the list against the left, the error
// being read beside it.
//
// What it takes from Claudhub is what any plugin can have:
// - its **data** (`storage_*`): the account — instance, organisation, query,
//   the prompt's introduction — once for the machine, the project once per
//   repository, the sections one folded;
// - its **secret** (`secret("token")`), in the system keyring;
// - the **network**: `sentry.io` is asked for in `claudhub.json`, another
//   instance at run time (`request_network`); either is reached once the user
//   allowed it in the Plugins screen, which mounts this again;
// - the **pieces** Claudhub lends: `CodeBlock` for the deployed code, `Icon`,
//   `Badge`, `ShareBar`; and its gestures — `open_file` for a frame of this
//   worktree, `ask_agent` for the dialog where the prompt is read before it
//   goes, `relative_time` for "3 min ago".

import { View, div } from "gpui-kit";
import { h_flex, v_flex, Button, Input, InputState } from "gpui-base";
import {
  worktree,
  language,
  storage_get,
  storage_set,
  secret,
  set_secret,
  granted_hosts,
  request_network,
  open_url,
  copy_text,
  notify,
  locate,
  open_file,
  ask_agent,
  relative_time,
  CodeBlock,
  Icon,
  Badge,
  ShareBar,
} from "claudhub";
import {
  DEFAULT_HOST,
  DEFAULT_QUERY,
  hostOf,
  issuesUrl,
  eventUrl,
  tagsUrl,
  readJson,
  readIssues,
  readEvent,
  readSpreads,
  matches,
  prompt,
} from "./sentry.js";
import { textsFor } from "./texts.js";

/** The sections of the page, by the key their fold is kept under. */
const SECTIONS = ["context", "trace", "spread", "crumbs"];

/** The glyph a level draws; an unknown one gets the neutral one rather than none. */
const levelIcon = (level) =>
  level === "fatal" || level === "error" ? "circle-x" : level === "warning" ? "triangle-alert" : "info";
/** What a level is worth in colour; an unknown one is neutral rather than alarming. */
const levelTone = (level) =>
  ({ fatal: "danger", error: "danger", warning: "warning", info: "info" })[level] ?? "muted";
/** And a status: resolved is the one piece of good news. */
const statusTone = (status) => ({ resolved: "success", ignored: "muted" })[status] ?? "warning";

export default class Sentry extends View {
  init(_props, cx) {
    this.t = textsFor(language());
    const tree = worktree();
    // The project belongs to the repository: every worktree of it reads the same.
    this.projectKey = `project:${tree.repository ?? tree.path}`;
    const account = storage_get("account") ?? {};
    this.host = InputState.new({ placeholder: DEFAULT_HOST, value: account.host ?? DEFAULT_HOST });
    this.org = InputState.new({ placeholder: "my-organisation", value: account.org ?? "" });
    this.query = InputState.new({ placeholder: DEFAULT_QUERY, value: account.query ?? DEFAULT_QUERY });
    this.intro = InputState.new({ placeholder: this.t.defaultIntro, value: account.intro ?? "" });
    this.token = InputState.new({ placeholder: "sntryu_…" });
    this.token.set_masked(true);
    // **The project's field is always there**: it is the one setting that
    // belongs to the repository, corrected as often as it is set.
    // A string; `{ slug }` as the first version of this plugin kept it.
    const stored = storage_get(this.projectKey);
    this.project = InputState.new({
      placeholder: this.t.projectPlaceholder,
      value: typeof stored === "string" ? stored : (stored?.slug ?? ""),
    });
    this.project.on("change", () => storage_set(this.projectKey, this.project.value().trim()));
    this.project.on("submit", (_event, cx) => cx.spawn(async (cx) => this.load(cx)));
    this.filter = InputState.new({ placeholder: this.t.filterPlaceholder });
    this.filter.on("change", (_event, cx) => cx.notify());

    // The distribution starts folded: seven tags unfolded push the trace,
    // which is what one came to read, off the screen.
    this.folded = new Set(storage_get("folded") ?? ["spread"]);
    this.finding = false;
    this.editing = false;
    this.hasToken = false;
    this.issues = [];
    this.chosen = null;
    this.event = null;
    this.spreads = [];
    this.loading = false;
    this.error = null;
    this.eventError = null;
    this.reading = 0;
    cx.spawn(async (cx) => {
      this.hasToken = (await secret("token")) !== null;
      await this.load(cx);
    });
  }

  config() {
    return {
      host: this.host.value().trim() || DEFAULT_HOST,
      org: this.org.value().trim(),
      project: this.project.value().trim(),
      query: this.query.value().trim() || DEFAULT_QUERY,
    };
  }

  /** Why the list shows nothing, when that is not simply "no errors". The sentence says what to do. */
  note() {
    const config = this.config();
    if (!config.org || !this.hasToken) return this.t.noAccount;
    if (!config.project) return this.t.noProject;
    if (this.error) return this.error;
    if (this.loading && this.issues.length === 0) return this.t.loading;
    return this.issues.length === 0 ? this.t.empty : null;
  }

  async request(url) {
    const token = await secret("token");
    if (!token) throw new Error(this.t.noToken);
    return readJson(await fetch(url, { headers: { Authorization: `Bearer ${token}` } }));
  }

  /** The host is allowed, or asked for: the Plugins screen mounts this again once it is. */
  reachable(config) {
    const host = hostOf(config.host);
    if (granted_hosts().includes(host)) return true;
    request_network(host);
    this.error = this.t.allow(host);
    return false;
  }

  async load(cx) {
    const config = this.config();
    this.error = null;
    if (!config.org || !config.project || !this.hasToken || !this.reachable(config)) {
      cx.notify();
      return;
    }
    this.loading = true;
    cx.notify();
    try {
      this.issues = readIssues(await this.request(issuesUrl(config)));
      // What one is reading survives a reading again, if the list still holds it.
      const kept = this.issues.find((issue) => issue.id === this.chosen?.id) ?? null;
      if (kept === null) {
        this.chosen = null;
        this.event = null;
        this.spreads = [];
      } else {
        this.chosen = kept;
      }
    } catch (error) {
      this.issues = [];
      this.error = String(error?.message ?? error);
    }
    this.loading = false;
    cx.notify();
  }

  /** An error chosen: its latest event and its distribution, asked for side by side. */
  async open(issue, cx) {
    const reading = ++this.reading;
    this.chosen = issue;
    this.event = null;
    this.eventError = null;
    this.spreads = [];
    cx.notify();
    const config = this.config();
    const [event, spreads] = await Promise.allSettled([
      this.request(eventUrl(config, issue.id)),
      this.request(tagsUrl(config, issue.id)),
    ]);
    // A late answer is dropped: one has moved on to another error.
    if (reading !== this.reading) return;
    if (event.status === "fulfilled") {
      this.event = readEvent(event.value);
      if (this.event === null) this.eventError = this.t.expired;
    } else {
      this.eventError = String(event.reason?.message ?? event.reason);
    }
    // The bars are the one reading of the page one can do without: a failure says nothing.
    if (spreads.status === "fulfilled") this.spreads = readSpreads(spreads.value);
    cx.notify();
  }

  async save(cx) {
    const config = this.config();
    storage_set("account", {
      host: config.host,
      org: config.org,
      query: config.query,
      intro: this.intro.value().trim(),
    });
    const token = this.token.value().trim();
    if (token) {
      await set_secret("token", token);
      this.token.set_value("");
      this.hasToken = true;
    }
    this.editing = false;
    await this.load(cx);
  }

  fold(section, cx) {
    if (this.folded.has(section)) this.folded.delete(section);
    else this.folded.add(section);
    storage_set("folded", [...this.folded]);
    cx.notify();
  }

  hand() {
    const intro = this.intro.value().trim() || this.t.defaultIntro;
    ask_agent(prompt(intro, this.config().org, this.chosen, this.event, locate));
  }

  // — Drawing ——————————————————————————————————————————————————————————

  render(cx) {
    const list = this.listPane(cx);
    const page = this.editing ? this.settingsPane(cx) : this.issuePane(cx);
    return h_flex()
      .size_full()
      .gap_3()
      .child(list)
      .child(v_flex().flex_1().min_w_0().h_full().child(page));
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
          cx.spawn(async (cx) => {
            await act(cx);
            cx.notify();
          });
        }),
      )
      .when(Boolean(icon), (el) => el.child(Icon.new(`${id}-icon`, { name: icon, size: "xsmall", tone })))
      .when(Boolean(label), (el) => el.child(label));
  }

  field(state, cx) {
    const colors = cx.theme().colors;
    return Input.new(state)
      .flex_1()
      .min_w_0()
      .h(26)
      .px_2()
      .rounded(6)
      .border(1)
      .border_color(colors.input)
      .bg(colors.background)
      .text_xs();
  }

  // — The list ———————————————————————————————————————————————————————————

  listPane(cx) {
    const colors = cx.theme().colors;
    const config = this.config();
    const bar = h_flex()
      .flex_none()
      .w_full()
      .gap_1()
      .items_center()
      .child(Icon.new("bar-icon", { name: "triangle-alert", size: "xsmall" }))
      .when(config.org !== "", (el) =>
        el.child(div().flex_none().max_w(120).truncate().text_xs().text_color(colors.muted_foreground).child(config.org)),
      )
      .child(this.field(this.project, cx))
      .child(
        this.button("find", { icon: "search", tone: this.finding ? "primary" : undefined }, () => {
          this.finding = !this.finding;
          if (!this.finding) this.filter.set_value("");
        }, cx),
      )
      .child(this.button("refresh", { icon: "refresh-cw", disabled: this.loading }, (cx) => this.load(cx), cx))
      .child(
        this.button("settings", { icon: "settings", tone: this.editing ? "primary" : undefined }, () => {
          this.editing = !this.editing;
        }, cx),
      );
    const note = this.note();
    const needle = this.finding ? this.filter.value() : "";
    const shown = this.issues.filter((issue) => matches(issue, needle));
    const body = note
      ? v_flex()
          .flex_1()
          .items_center()
          .justify_center()
          .gap_2()
          .p_4()
          .child(Icon.new("note-icon", { name: "triangle-alert", size: "large", tone: "muted" }))
          .child(div().text_sm().text_center().text_color(colors.muted_foreground).child(note))
      : v_flex()
          .flex_1()
          .min_h_0()
          .child(
            div()
              .flex_none()
              .px_2()
              .py_1()
              .text_xs()
              .text_color(colors.muted_foreground)
              .child(
                shown.length === this.issues.length
                  ? this.t.count(this.issues.length)
                  : this.t.countFiltered(shown.length, this.issues.length),
              ),
          )
          .child(v_flex().flex_1().min_h_0().overflow_y_scrollbar().children(shown.map((issue) => this.row(issue, cx))));
    return v_flex()
      .w(380)
      .flex_none()
      .h_full()
      .gap_1()
      .child(bar)
      .when(this.finding, (el) => el.child(h_flex().flex_none().child(this.field(this.filter, cx))))
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

  /** One row: what it is, where and when, how often. A band, not a pill. */
  row(issue, cx) {
    const colors = cx.theme().colors;
    const selected = this.chosen?.id === issue.id;
    const subtitle = [issue.culprit, relative_time(issue.lastSeen)].filter((part) => part !== "").join(" · ");
    return Button.new(`issue-${issue.id}`)
      .flex()
      .w_full()
      .px_2()
      .py_1()
      .gap_2()
      .items_center()
      .when(selected, (el) => el.bg(colors.accent))
      .hover((style) => style.bg(colors.secondary))
      .on_click((_event, cx) => {
        cx.spawn(async (cx) => this.open(issue, cx));
      })
      .child(Icon.new(`issue-${issue.id}-level`, { name: levelIcon(issue.level), size: "xsmall" }))
      .child(
        v_flex()
          .flex_1()
          .min_w_0()
          .child(div().truncate().text_sm().child(issue.title))
          .child(div().truncate().text_xs().text_color(colors.muted_foreground).child(subtitle)),
      )
      .child(div().flex_none().text_xs().text_color(colors.muted_foreground).child(String(issue.count)));
  }

  // — The account ——————————————————————————————————————————————————————————

  settingsPane(cx) {
    const colors = cx.theme().colors;
    const labelled = (label, state, help) =>
      v_flex()
        .gap_1()
        .child(div().text_xs().font_semibold().child(label))
        .child(h_flex().child(this.field(state, cx)))
        .when(Boolean(help), (el) => el.child(div().text_xs().text_color(colors.muted_foreground).child(help)));
    return v_flex()
      .size_full()
      .gap_3()
      .p_3()
      .overflow_y_scrollbar()
      .child(div().text_lg().child(this.t.settings))
      .child(labelled(this.t.instance, this.host, this.t.instanceHelp))
      .child(labelled(this.t.organisation, this.org, ""))
      .child(labelled(this.t.query, this.query, this.t.queryHelp))
      .child(labelled(this.t.token, this.token, this.hasToken ? this.t.tokenKept : this.t.tokenHelp))
      .child(labelled(this.t.intro, this.intro, this.t.introHelp))
      .child(
        h_flex()
          .gap_2()
          .child(this.button("save", { icon: "save", label: this.t.save, primary: true }, (cx) => this.save(cx), cx))
          .child(this.button("close-settings", { label: this.t.close }, () => {
            this.editing = false;
          }, cx)),
      );
  }

  // — The error being read ——————————————————————————————————————————————————

  issuePane(cx) {
    const issue = this.chosen;
    if (!issue) return div();
    const colors = cx.theme().colors;
    const pair = (label, value) =>
      h_flex()
        .gap_1()
        .text_xs()
        .child(div().text_color(colors.muted_foreground).child(label))
        .child(div().child(value));
    const head = v_flex()
      .flex_none()
      .gap_1()
      .p_3()
      .child(div().text_lg().child(issue.kind))
      .when(issue.value !== "", (el) => el.child(div().text_sm().child(issue.value)))
      .when(issue.culprit !== "", (el) => el.child(div().text_xs().text_color(colors.muted_foreground).child(issue.culprit)))
      // What one reads before reading anything: how bad, and whether somebody has dealt with it.
      .child(
        h_flex()
          .gap_1()
          .items_center()
          .child(Badge.new("level", { text: issue.level, tone: levelTone(issue.level) }))
          .when(issue.status !== "", (el) => el.child(Badge.new("status", { text: issue.status, tone: statusTone(issue.status) }))),
      )
      .child(
        h_flex()
          .gap_2()
          .items_center()
          .pt_1()
          .child(pair(this.t.events, String(issue.count)))
          .when(issue.users > 0, (el) => el.child(pair(this.t.users, String(issue.users))))
          .when(issue.firstSeen !== "", (el) => el.child(pair(this.t.first, relative_time(issue.firstSeen))))
          .when(issue.lastSeen !== "", (el) => el.child(pair(this.t.last, relative_time(issue.lastSeen)))),
      )
      .child(
        h_flex()
          .gap_2()
          .pt_1()
          .child(this.button("hand", { icon: "bot", label: this.t.hand, primary: true }, () => this.hand(), cx))
          // A button and not a text: the short id is the reference one carries elsewhere.
          .when(issue.shortId !== "", (el) =>
            el.child(
              this.button("copy-id", { icon: "copy", label: issue.shortId }, () => {
                copy_text(issue.shortId);
                notify(this.t.copied(issue.shortId));
              }, cx),
            ),
          )
          .when(issue.permalink !== "", (el) =>
            el.child(this.button("open", { icon: "external-link", label: this.t.openInSentry }, () => open_url(issue.permalink), cx)),
          ),
      );
    let body;
    if (this.eventError) {
      body = div().p_3().text_sm().text_color(colors.destructive).child(this.eventError);
    } else if (!this.event) {
      body = div().p_3().text_sm().text_color(colors.muted_foreground).child(this.t.loadingEvent);
    } else {
      body = v_flex().flex_1().min_h_0().px_3().pb_3().gap_1().overflow_y_scrollbar().children(this.sections(cx));
    }
    return v_flex().size_full().child(head).child(body);
  }

  /** The context, then the trace — what one came for, so nothing long goes above it —, the distribution, the trail. */
  sections(cx) {
    const colors = cx.theme().colors;
    const event = this.event;
    const out = [];
    const heading = (section, title) =>
      Button.new(`section-${section}`)
        .flex()
        .w_full()
        .gap_1()
        .items_center()
        .pt_2()
        .text_xs()
        .text_color(colors.muted_foreground)
        .on_click((_event, cx) => this.fold(section, cx))
        .child(
          Icon.new(`section-${section}-fold`, {
            name: this.folded.has(section) ? "chevron-right" : "chevron-down",
            size: "xsmall",
            tone: "muted",
          }),
        )
        .child(title);
    const pair = (key, value) =>
      h_flex()
        .w_full()
        .gap_2()
        .text_xs()
        .child(div().flex_none().w(180).truncate().text_color(colors.muted_foreground).child(key))
        .child(div().flex_1().min_w_0().truncate().child(value));

    if (event.tags.length) {
      out.push(heading("context", this.t.context));
      if (!this.folded.has("context")) for (const tag of event.tags) out.push(pair(tag.key, tag.value));
    }
    if (event.frames.length) {
      out.push(heading("trace", this.t.trace(event.frames.length)));
      if (!this.folded.has("trace")) {
        // Newest first: what one comes for is the line that raised.
        [...event.frames].reverse().forEach((frame, at) => {
          out.push(this.frameHeading(frame, at, cx));
          if (frame.context.length) {
            out.push(CodeBlock.new(`frame-${at}-code`, { path: locate(frame.file) ?? frame.file, lines: frame.context, mark: frame.line }));
          }
        });
      }
    }
    if (this.spreads.length) {
      out.push(heading("spread", this.t.spread));
      if (!this.folded.has("spread")) {
        this.spreads.forEach((spread, at) => {
          out.push(div().pt_1().text_xs().text_color(colors.muted_foreground).child(spread.name));
          spread.values.forEach((value, rank) =>
            out.push(ShareBar.new(`spread-${at}-${rank}`, { label: value.value, share: value.share })),
          );
        });
      }
    }
    if (event.crumbs.length) {
      out.push(heading("crumbs", this.t.crumbs(event.crumbs.length)));
      if (!this.folded.has("crumbs")) for (const crumb of event.crumbs) out.push(pair(crumb.category, crumb.message));
    }
    return out;
  }

  /** A frame's heading: its file — which opens, when it is one of this worktree —, its function, "app". */
  frameHeading(frame, at, cx) {
    const colors = cx.theme().colors;
    const file = locate(frame.file);
    const shown = file ?? frame.file.replaceAll("\\", "/");
    return h_flex()
      .w_full()
      .gap_2()
      .items_center()
      .pt_1()
      .text_xs()
      .child(
        this.button(
          `frame-${at}`,
          { icon: frame.inApp ? "file-code" : "file", label: `${shown}:${frame.line}`, disabled: file === null },
          () => open_file(file, frame.line),
          cx,
        ),
      )
      .when(frame.function !== "", (el) =>
        el.child(div().flex_1().min_w_0().truncate().text_color(colors.muted_foreground).child(frame.function)),
      )
      // Ours, said out loud: three frames of a hundred are the application's.
      .when(frame.inApp, (el) => el.child(Badge.new(`frame-${at}-app`, { text: "app", tone: "info" })));
  }
}
