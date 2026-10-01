// The project's Sentry errors, as a tab of every board — written end to end
// as a plugin: the settings, the requests, the reading, the drawing.
//
// What it needs from Claudhub is what any plugin can have:
// - its **data** (`storage_*`): the account — instance, organisation, query —
//   once for the machine, and the project once per repository;
// - its **secret** (`secret("token")`), kept in the system keyring;
// - the **network**: `sentry.io` is asked for in `claudhub.json`; a
//   self-hosted instance is asked for at run time (`request_network`). Either
//   is reached only once the user allowed it in the Plugins screen.

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
  send_to_agent,
  notify,
} from "claudhub";
import {
  DEFAULT_HOST,
  DEFAULT_QUERY,
  hostOf,
  issuesUrl,
  eventUrl,
  readJson,
  readIssues,
  readEvent,
  ago,
  prompt,
} from "./sentry.js";

const LEVELS = { fatal: "destructive", error: "destructive", warning: "primary" };

export default class Sentry extends View {
  init(_props, cx) {
    const tree = worktree();
    this.french = language().startsWith("fr");
    // The project belongs to the repository: every worktree of it reads the same.
    this.projectKey = `project:${tree.repository ?? tree.path}`;
    const account = storage_get("account") ?? {};
    const project = storage_get(this.projectKey) ?? {};
    this.host = InputState.new({ placeholder: DEFAULT_HOST, value: account.host ?? DEFAULT_HOST });
    this.org = InputState.new({ placeholder: "my-organisation", value: account.org ?? "" });
    this.project = InputState.new({ placeholder: "my-project", value: project.slug ?? "" });
    this.query = InputState.new({ placeholder: DEFAULT_QUERY, value: account.query ?? DEFAULT_QUERY });
    this.token = InputState.new({ placeholder: "sntryu_…" });
    this.token.set_masked(true);

    this.editing = !(account.org && project.slug);
    this.hasToken = false;
    this.issues = [];
    this.selected = null;
    this.event = null;
    this.loading = false;
    this.error = null;
    cx.spawn(async (cx) => {
      this.hasToken = (await secret("token")) !== null;
      if (!this.hasToken) this.editing = true;
      if (!this.editing) await this.load(cx);
      cx.notify();
    });
  }

  say(fr, en) {
    return this.french ? fr : en;
  }

  config() {
    return {
      host: this.host.value().trim() || DEFAULT_HOST,
      org: this.org.value().trim(),
      project: this.project.value().trim(),
      query: this.query.value().trim() || DEFAULT_QUERY,
    };
  }

  /** The host is allowed, or asked for: the Plugins screen mounts this again once it is. */
  reachable(config) {
    const host = hostOf(config.host);
    if (granted_hosts().includes(host)) return true;
    request_network(host);
    this.error = this.say(
      `Autorisez ${host} dans l'écran Plugins pour lire Sentry.`,
      `Allow ${host} in the Plugins screen to read Sentry.`,
    );
    return false;
  }

  async request(url) {
    const token = await secret("token");
    if (!token) throw new Error(this.say("Aucun jeton : ouvrez les réglages.", "No token: open the settings."));
    return readJson(await fetch(url, { headers: { Authorization: `Bearer ${token}` } }));
  }

  async load(cx) {
    const config = this.config();
    if (!this.reachable(config)) return;
    this.loading = true;
    this.error = null;
    cx.notify();
    try {
      this.issues = readIssues(await this.request(issuesUrl(config)));
      this.selected = this.issues.find((issue) => issue.id === this.selected?.id) ?? null;
    } catch (error) {
      this.error = String(error?.message ?? error);
    }
    this.loading = false;
    cx.notify();
  }

  async select(issue, cx) {
    this.selected = issue;
    this.event = null;
    cx.notify();
    try {
      const event = readEvent(await this.request(eventUrl(this.config(), issue.id)));
      // The hand may have moved on while it was asked.
      if (this.selected?.id === issue.id) this.event = event;
    } catch (error) {
      this.error = String(error?.message ?? error);
    }
    cx.notify();
  }

  async save(cx) {
    const config = this.config();
    storage_set("account", { host: config.host, org: config.org, query: config.query });
    storage_set(this.projectKey, { slug: config.project });
    const token = this.token.value().trim();
    if (token) {
      await set_secret("token", token);
      this.token.set_value("");
      this.hasToken = true;
    }
    this.editing = !(config.org && config.project && this.hasToken);
    if (!this.editing) await this.load(cx);
    cx.notify();
  }

  render(cx) {
    const colors = cx.theme().colors;
    const config = this.config();
    const header = h_flex()
      .flex_none()
      .gap(8)
      .items_center()
      .child(div().text_size(15).font_semibold().child("Sentry"))
      .child(
        div()
          .flex_1()
          .min_w_0()
          .truncate()
          .text_size(12)
          .text_color(colors.muted_foreground)
          .child(config.org && config.project ? `${config.org} / ${config.project} · ${config.query}` : ""),
      )
      .when(this.loading, (el) => el.child(div().text_size(12).text_color(colors.muted_foreground).child("…")))
      .when(!this.editing, (el) =>
        el.child(this.button("refresh", this.say("Actualiser", "Refresh"), (cx) => this.load(cx), cx)),
      )
      .child(
        this.button(
          "settings",
          this.editing ? this.say("Fermer", "Close") : this.say("Réglages", "Settings"),
          (cx) => {
            this.editing = !this.editing;
            cx.notify();
          },
          cx,
        ),
      );
    const error = this.error
      ? div()
          .flex_none()
          .p(8)
          .rounded(6)
          .border(1)
          .border_color(colors.destructive)
          .text_size(12)
          .text_color(colors.destructive)
          .child(this.error)
      : null;
    const body = this.editing ? this.settings(cx) : this.browser(cx);
    return v_flex()
      .size_full()
      .gap(10)
      .child(header)
      .when(error !== null, (el) => el.child(error))
      .child(v_flex().flex_1().min_h_0().child(body));
  }

  button(id, caption, act, cx) {
    const colors = cx.theme().colors;
    return Button.new(id)
      .flex()
      .flex_none()
      .items_center()
      .h(26)
      .px(10)
      .border(1)
      .rounded(6)
      .border_color(colors.border)
      .text_size(12)
      .hover((style) => style.bg(colors.muted))
      .on_click((_event, cx) => {
        cx.spawn(async (cx) => {
          await act(cx);
          cx.notify();
        });
      })
      .child(caption);
  }

  field(label, state, help, cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .gap(4)
      .child(div().text_size(12).font_semibold().child(label))
      .child(
        Input.new(state)
          .h(28)
          .px(8)
          .border(1)
          .rounded(6)
          .border_color(colors.input)
          .bg(colors.surface)
          .text_size(12),
      )
      .when(Boolean(help), (el) => el.child(div().text_size(11).text_color(colors.muted_foreground).child(help)));
  }

  settings(cx) {
    return v_flex()
      .max_w(520)
      .gap(12)
      .overflow_y_scrollbar()
      .child(this.field(this.say("Instance", "Instance"), this.host, this.say("sentry.io, ou votre instance auto-hébergée.", "sentry.io, or your self-hosted instance."), cx))
      .child(this.field(this.say("Organisation", "Organisation"), this.org, "", cx))
      .child(this.field(this.say("Projet de ce dépôt", "This repository's project"), this.project, "", cx))
      .child(this.field(this.say("Requête", "Query"), this.query, "", cx))
      .child(
        this.field(
          this.say("Jeton d'API", "API token"),
          this.token,
          this.hasToken
            ? this.say("Un jeton est enregistré dans le trousseau ; laissez vide pour le garder.", "A token is kept in the keyring; leave empty to keep it.")
            : this.say("Un jeton avec la portée event:read, gardé dans le trousseau du système.", "A token with the event:read scope, kept in the system keyring."),
          cx,
        ),
      )
      .child(h_flex().child(this.button("save", this.say("Enregistrer", "Save"), (cx) => this.save(cx), cx)));
  }

  browser(cx) {
    const colors = cx.theme().colors;
    const rows = this.issues.map((issue) => {
      const lit = this.selected?.id === issue.id;
      return Button.new(`issue-${issue.id}`)
        .flex()
        .flex_col()
        .w_full()
        .gap(2)
        .px(10)
        .py(6)
        .border_l(2)
        .border_color(lit ? colors.primary : colors.background)
        .bg(lit ? colors.muted : colors.background)
        .hover((style) => style.bg(colors.muted))
        .on_click((_event, cx) => {
          cx.spawn(async (cx) => this.select(issue, cx));
        })
        .child(
          h_flex()
            .gap(6)
            .items_center()
            .child(div().w(8).h(8).rounded(4).flex_none().bg(colors[LEVELS[issue.level] ?? "muted_foreground"]))
            .child(div().flex_1().min_w_0().truncate().text_size(12).font_semibold().child(issue.kind)),
        )
        .child(div().truncate().text_size(11).text_color(colors.muted_foreground).child(issue.value || issue.culprit))
        .child(
          div()
            .text_size(11)
            .text_color(colors.muted_foreground)
            .child(`${issue.shortId} · ${issue.count}× · ${issue.users} 👤 · ${ago(issue.lastSeen)}`),
        );
    });
    const list = v_flex()
      .w(360)
      .flex_none()
      .h_full()
      .border(1)
      .rounded(8)
      .border_color(colors.border)
      .overflow_y_scrollbar()
      .children(
        rows.length
          ? rows
          : [div().p(12).text_size(12).text_color(colors.muted_foreground).child(this.loading ? "…" : this.say("Aucune erreur.", "No error."))],
      );
    return h_flex().size_full().gap(10).child(list).child(v_flex().flex_1().min_w_0().h_full().child(this.detail(cx)));
  }

  detail(cx) {
    const colors = cx.theme().colors;
    const issue = this.selected;
    if (!issue) {
      return div().p(12).text_size(12).text_color(colors.muted_foreground).child(this.say("Choisissez une erreur.", "Pick an error."));
    }
    const event = this.event;
    const org = this.config().org;
    const actions = h_flex()
      .gap(6)
      .child(this.button("open", this.say("Ouvrir dans Sentry", "Open in Sentry"), () => open_url(issue.permalink), cx))
      .child(
        this.button(
          "copy",
          this.say("Copier l'identifiant", "Copy the id"),
          () => {
            copy_text(issue.shortId);
            notify(this.say(`${issue.shortId} copié.`, `${issue.shortId} copied.`));
          },
          cx,
        ),
      )
      .child(this.button("agent", this.say("Envoyer à l'agent", "Send to the agent"), () => send_to_agent(prompt(org, issue, event)), cx));
    const frames = (event?.frames ?? []).map((frame) =>
      div()
        .text_size(11)
        .font_family("monospace")
        .text_color(frame.inApp ? colors.foreground : colors.muted_foreground)
        .child(`${frame.file}:${frame.line}${frame.function ? ` · ${frame.function}` : ""}`),
    );
    const tags = (event?.tags ?? []).slice(0, 12).map((tag) =>
      h_flex()
        .gap(6)
        .text_size(11)
        .child(div().w(140).flex_none().truncate().text_color(colors.muted_foreground).child(tag.key))
        .child(div().flex_1().min_w_0().truncate().child(tag.value)),
    );
    return v_flex()
      .size_full()
      .gap(10)
      .overflow_y_scrollbar()
      .child(div().text_size(15).font_semibold().child(issue.kind))
      .when(issue.value !== "", (el) => el.child(div().text_size(12).child(issue.value)))
      .child(div().text_size(12).text_color(colors.muted_foreground).child(issue.culprit))
      .child(actions)
      .child(div().text_size(13).font_semibold().child(this.say("Trace", "Trace")))
      .children(frames.length ? frames : [div().text_size(12).text_color(colors.muted_foreground).child(event ? "—" : "…")])
      .when(tags.length > 0, (el) => el.child(div().text_size(13).font_semibold().child(this.say("Contexte", "Context"))).children(tags));
  }
}
