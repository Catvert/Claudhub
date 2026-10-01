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
// - the **pieces** Claudhub lends: `CodeLine` for the deployed code, `Icon`,
//   `Badge`, `ShareBar`; and its gestures — `open_file` for a frame of this
//   worktree, `ask_agent` for the dialog where the prompt is read before it
//   goes, `relative_time` for "3 min ago".
//
// **Both lists are virtual**, as the panel's are: the list of errors, and the
// error's page below its header, laid out as rows **one line tall** — a
// section's heading, a tag, a frame, one line of its code — so a trace two
// hundred frames deep costs what is on screen (`v_virtual_list`, told the
// heights Claudhub draws with by `sizes()`). A row holds no handler: a click
// reaches the list with the row's key (`on_item_click`). Claudhub smooths
// their wheel as it smooths its own panels.

import { View, div } from "gpui-kit";
import { h_flex, v_flex, Button, Input, InputState, Scrollbar, v_virtual_list } from "gpui-base";
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
  sizes,
  palette,
  CodeLine,
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
/** How loud a volume is: the count's pill is coloured by it. */
const volumeTone = (count) => (count >= 1000 ? "danger" : count >= 100 ? "warning" : "muted_foreground");
/** A count as one reads it at a glance: 2484 → 2.5k. */
const compact = (n) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e4 ? `${Math.round(n / 1e3)}k` : n >= 1e3 ? `${(n / 1e3).toFixed(1)}k` : String(n));
/** Whether an instant is within the last day: an error first seen then is new. */
const lately = (iso) => Date.now() - Date.parse(iso) < 86400e3;
/** The orders the list can be read in, and what each compares. */
const ORDERS = {
  recent: (a, b) => Date.parse(b.lastSeen) - Date.parse(a.lastSeen),
  frequent: (a, b) => b.count - a.count,
  widespread: (a, b) => b.users - a.users,
};
/** The levels, loudest first, and the palette entry each is drawn in. */
const LEVELS = [
  ["fatal", "danger"],
  ["error", "danger"],
  ["warning", "warning"],
  ["info", "info"],
];
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
    this.order = ORDERS[storage_get("order")] ? storage_get("order") : "recent";
    this.only = null;
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
    const shown = this.issues
      .filter((issue) => matches(issue, needle) && (this.only === null || issue.level === this.only))
      .sort(ORDERS[this.order]);
    const body = note
      ? v_flex()
          .flex_1()
          .items_center()
          .justify_center()
          .gap_2()
          .p_4()
          .child(Icon.new("note-icon", { name: "triangle-alert", size: "large", tone: "muted" }))
          .child(div().text_sm().text_center().text_color(colors.muted_foreground).child(note))
      : v_flex().flex_1().min_h_0().child(this.listHead(shown, cx)).child(this.issueList(shown, cx));
    return v_flex()
      .w(400)
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

  /** The rows of the list, virtual: two storeys each, as the panel's. */
  issueList(shown, cx) {
    const height = Math.round(sizes().row * 2.6);
    return v_flex()
      .relative()
      .flex_1()
      .min_h_0()
      .child(
        v_virtual_list(
          "sentry-issues",
          shown.length,
          height,
          (index) => shown[index].id,
          (range) => shown.slice(range.start, range.end).map((issue) => this.row(issue, height, cx)),
        )
          .size_full()
          .on_item_click((key, cx) => {
            const issue = this.issues.find((issue) => issue.id === key);
            if (issue) cx.spawn(async (cx) => this.open(issue, cx));
          }),
      )
      .child(Scrollbar.vertical("sentry-issues").absolute().inset_0());
  }

  /** A chip of the list's head: a word, a dot of colour, lit when chosen. */
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

  /** What the list holds by level — each a filter —, and the order it is read in. */
  listHead(shown, cx) {
    const colors = cx.theme().colors;
    const levels = LEVELS.map(([level, tone]) => [level, tone, this.issues.filter((issue) => issue.level === level).length]).filter(
      ([, , count]) => count > 0,
    );
    const orders = Object.keys(ORDERS);
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
          .child(this.chip("only-all", { label: this.t.all, lit: this.only === null }, () => (this.only = null), cx))
          .children(
            levels.map(([level, tone, count]) =>
              this.chip(`only-${level}`, { label: `${count} ${this.t.level[level]}`, tone, lit: this.only === level }, () => {
                this.only = this.only === level ? null : level;
              }, cx),
            ),
          ),
      )
      .child(
        h_flex()
          .gap_2()
          .items_center()
          .child(
            div()
              .flex_1()
              .text_xs()
              .text_color(colors.muted_foreground)
              .child(
                shown.length === this.issues.length
                  ? this.t.count(this.issues.length)
                  : this.t.countFiltered(shown.length, this.issues.length),
              ),
          )
          // The order as a segmented control: one of three, always one.
          .child(
            h_flex()
              .flex_none()
              .p(2)
              .gap(2)
              .rounded(8)
              .bg(colors.muted)
              .children(
                orders.map((order) =>
                  Button.new(`order-${order}`)
                    .flex()
                    .items_center()
                    .h(20)
                    .px_2()
                    .rounded(6)
                    .text_xs()
                    .when(this.order === order, (el) => el.bg(colors.background).text_color(colors.foreground))
                    .when(this.order !== order, (el) =>
                      el.text_color(colors.muted_foreground).hover((style) => style.text_color(colors.foreground)),
                    )
                    .on_click((_event, cx) => {
                      this.order = order;
                      storage_set("order", order);
                      cx.notify();
                    })
                    .child(this.t[order]),
                ),
              ),
          ),
      );
  }

  /**
   * One row — three storeys: what was raised and when, what it said, where;
   * and at the right how loud. Its level is a band of colour down its edge
   * and the tint of its icon; its volume colours the count.
   */
  row(issue, height, cx) {
    const colors = cx.theme().colors;
    const tones = palette();
    const selected = this.chosen?.id === issue.id;
    // `muted` names a ground in the palette; a level's ink is its foreground.
    const tone = tones[levelTone(issue.level) === "muted" ? "muted_foreground" : levelTone(issue.level)];
    const volume = tones[volumeTone(issue.count)];
    const pill = (text, ink) =>
      div()
        .flex_none()
        .px(6)
        .rounded(8)
        .text_xs()
        .font_semibold()
        .bg(`${ink}26`)
        .text_color(ink)
        .child(text);
    return h_flex()
      .w_full()
      .h(height)
      .items_center()
      .cursor_pointer()
      .border_b(1)
      .border_color(`${tones.border}80`)
      .bg(selected ? `${tones.primary}1f` : colors.background)
      .hover((style) => style.bg(selected ? `${tones.primary}2e` : tones.list_hover))
      // The band: the level, read before anything.
      .child(div().flex_none().w(3).h_full().bg(selected ? tones.primary : tone))
      .child(
        h_flex()
          .flex_1()
          .min_w_0()
          .h_full()
          .px_2()
          .gap_2()
          .items_center()
          .child(
            div()
              .flex_none()
              .flex()
              .items_center()
              .justify_center()
              .w(26)
              .h(26)
              .rounded(13)
              .bg(`${tone}26`)
              .child(Icon.new(`issue-${issue.id}-level`, { name: levelIcon(issue.level), size: "xsmall", tone: levelTone(issue.level) })),
          )
          .child(
            v_flex()
              .flex_1()
              .min_w_0()
              .gap(1)
              .child(
                h_flex()
                  .gap_2()
                  .items_center()
                  .child(div().flex_1().min_w_0().truncate().text_sm().font_semibold().child(issue.kind))
                  .when(lately(issue.firstSeen), (el) => el.child(pill(this.t.fresh, tones.success)))
                  .child(div().flex_none().text_xs().text_color(colors.muted_foreground).child(relative_time(issue.lastSeen))),
              )
              .when(issue.value !== "", (el) =>
                el.child(div().truncate().text_xs().text_color(colors.foreground).opacity(0.85).child(issue.value)),
              )
              .child(
                h_flex()
                  .gap_2()
                  .items_center()
                  .child(div().flex_1().min_w_0().truncate().text_xs().text_color(colors.muted_foreground).child(issue.culprit || issue.shortId))
                  .when(issue.users > 0, (el) =>
                    el.child(
                      h_flex()
                        .flex_none()
                        .gap(3)
                        .items_center()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child(Icon.new(`issue-${issue.id}-users`, { name: "users", size: "xsmall", tone: "muted" }))
                        .child(compact(issue.users)),
                    ),
                  )
                  .child(pill(`${compact(issue.count)}×`, volume)),
              ),
          ),
      );
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
      body = this.page(cx);
    }
    return v_flex().size_full().child(head).child(body);
  }

  /**
   * The page's rows, one line tall each: the context, then the trace — what
   * one came for, so nothing long goes above it —, the distribution, the
   * trail. Each is `{ key, kind, … }`; the key is what a click reports.
   */
  rows() {
    const event = this.event;
    const rows = [];
    const section = (key, title) => rows.push({ key: `section:${key}`, kind: "section", section: key, title });
    if (event.tags.length) {
      section("context", this.t.context);
      if (!this.folded.has("context")) {
        event.tags.forEach((tag, at) => rows.push({ key: `tag:${at}`, kind: "pair", label: tag.key, value: tag.value }));
      }
    }
    if (event.frames.length) {
      section("trace", this.t.trace(event.frames.length));
      if (!this.folded.has("trace")) {
        // Newest first: what one comes for is the line that raised.
        [...event.frames].reverse().forEach((frame, at) => {
          const file = locate(frame.file);
          rows.push({ key: `frame:${at}`, kind: "frame", frame, file, at });
          frame.context.forEach((_, index) =>
            rows.push({ key: `code:${at}:${index}`, kind: "code", frame, file, index }),
          );
        });
      }
    }
    if (this.spreads.length) {
      section("spread", this.t.spread);
      if (!this.folded.has("spread")) {
        this.spreads.forEach((spread, at) => {
          rows.push({ key: `spread:${at}`, kind: "name", text: spread.name });
          spread.values.forEach((value, rank) =>
            rows.push({ key: `share:${at}:${rank}`, kind: "share", label: value.value, share: value.share }),
          );
        });
      }
    }
    if (event.crumbs.length) {
      section("crumbs", this.t.crumbs(event.crumbs.length));
      if (!this.folded.has("crumbs")) {
        event.crumbs.forEach((crumb, at) =>
          rows.push({ key: `crumb:${at}`, kind: "pair", label: crumb.category, value: crumb.message }),
        );
      }
    }
    return rows;
  }

  page(cx) {
    const rows = this.rows();
    const height = sizes().line;
    return v_flex()
      .relative()
      .flex_1()
      .min_h_0()
      .px_3()
      .child(
        v_virtual_list(
          "sentry-page",
          rows.length,
          height,
          (index) => rows[index].key,
          (range) => rows.slice(range.start, range.end).map((row) => this.pageRow(row, height, cx)),
        )
          .size_full()
          .on_item_click((key, cx) => {
            const row = rows.find((row) => row.key === key);
            if (row?.kind === "section") this.fold(row.section, cx);
            // A frame of this worktree opens; one Sentry names by a module, or
            // one of a dependency not checked out here, opens nothing.
            else if (row?.kind === "frame" && row.file !== null) open_file(row.file, row.frame.line);
          }),
      )
      .child(Scrollbar.vertical("sentry-page").absolute().inset_0());
  }

  /** One row of the page, exactly one line tall: a virtual list reserves what it is told. */
  pageRow(row, height, cx) {
    const colors = cx.theme().colors;
    const base = () => h_flex().h(height).w_full().items_center().gap_2().text_xs();
    switch (row.kind) {
      case "section":
        return base()
          .cursor_pointer()
          .text_color(colors.muted_foreground)
          .child(
            Icon.new(`${row.key}-fold`, {
              name: this.folded.has(row.section) ? "chevron-right" : "chevron-down",
              size: "xsmall",
              tone: "muted",
            }),
          )
          .child(row.title);
      case "pair":
        return base()
          .child(div().flex_none().w(180).truncate().text_color(colors.muted_foreground).child(row.label))
          .child(div().flex_1().min_w_0().truncate().child(row.value));
      case "name":
        return base().text_color(colors.muted_foreground).child(row.text);
      case "share":
        return base().child(ShareBar.new(row.key, { label: row.label, share: row.share }));
      case "frame": {
        const frame = row.frame;
        const shown = row.file ?? frame.file.replaceAll("\\", "/");
        return base()
          .when(row.file !== null, (el) => el.cursor_pointer().hover((style) => style.bg(colors.secondary)))
          .child(Icon.new(`${row.key}-icon`, { name: frame.inApp ? "file-code" : "file", size: "xsmall", tone: row.file === null ? "muted" : undefined }))
          .child(
            div()
              .flex_none()
              .truncate()
              .text_color(row.file === null ? colors.muted_foreground : colors.foreground)
              .child(`${shown}:${frame.line}`),
          )
          .when(frame.function !== "", (el) =>
            el.child(div().flex_1().min_w_0().truncate().text_color(colors.muted_foreground).child(frame.function)),
          )
          // Ours, said out loud: three frames of a hundred are the application's.
          .when(frame.inApp, (el) => el.child(Badge.new(`${row.key}-app`, { text: "app", tone: "info" })));
      }
      case "code":
        return CodeLine.new(row.key, {
          path: row.file ?? row.frame.file,
          lines: row.frame.context,
          index: row.index,
          mark: row.frame.line,
        });
      default:
        return base();
    }
  }
}
