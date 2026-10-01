// An HTTP client in the manner of HTTPie, as a tab of every board: the saved
// requests and the history against the left, the request being written in
// the middle, its answer at the right.
//
// What it keeps:
// - the **requests** (`storage_*`), saved as they are typed — no button to
//   forget; one opened from the history is a draft until it is saved;
// - the **secrets** — a request's token or password, a locked variable — in
//   one entry of the system keyring, `vault`, as JSON;
// - nothing for the **network**: a plugin may `fetch` any address, over
//   HTTP or HTTPS, on any port — a local server included.
//
// What it cannot see: `fetch` hands a plugin the status, the final address
// and the body — not the response's headers. The body's kind is guessed from
// the body itself.

import { View, div } from "gpui-kit";
import {
  h_flex,
  v_flex,
  Button,
  Input,
  InputState,
  Textarea,
  TextareaState,
  Popover,
  Scrollbar,
  v_virtual_list,
  h_resizable,
  resizable_panel,
} from "gpui-base";
import {
  language,
  storage_get,
  storage_set,
  storage_remove,
  secret,
  set_secret,
  copy_text,
  notify,
  ask_agent,
  relative_time,
  sizes,
  palette,
  CodeLine,
  Icon,
  Badge,
} from "claudhub";
import {
  METHODS,
  BODY_MODES,
  AUTH_MODES,
  HISTORY,
  newId,
  blank,
  readRequest,
  build,
  unknownVars,
  hostOf,
  shortUrl,
  byteLength,
  jsonError,
  kindOf,
  pathFor,
  linesOf,
  pretty,
  REASONS,
  statusTone,
  humanSize,
  humanTime,
  asCurl,
  asHttpie,
  prompt,
} from "./http.js";
import { textsFor } from "./texts.js";

/** The colour each method is known by, as every HTTP client paints it. */
const METHOD_TONES = {
  GET: "success",
  POST: "warning",
  PUT: "info",
  PATCH: "primary",
  DELETE: "danger",
  HEAD: "muted_foreground",
  OPTIONS: "muted_foreground",
};
const methodInk = (method) => palette()[METHOD_TONES[method] ?? "muted_foreground"];
const TABS = ["params", "headers", "body", "auth"];
/** How often what was typed reaches the storage. */
const SAVE_EVERY = 600;

/** What a first opening shows: two requests that answer without an account. */
const examples = () => [
  blank({ name: "httpbin · GET", url: "https://httpbin.org/get", params: [{ on: true, k: "hello", v: "world" }] }),
  blank({
    name: "httpbin · POST JSON",
    method: "POST",
    url: "https://httpbin.org/post",
    bodyMode: "json",
    body: '{\n  "name": "Ada",\n  "admin": true\n}',
  }),
];

const clone = (value) => JSON.parse(JSON.stringify(value));

let rowSeq = 0;

/**
 * Rows of name and value — parameters, headers, form fields, variables — each
 * two retained fields and a switch. There is always one blank row at the end:
 * typing in it is adding a pair.
 */
class Pairs {
  constructor(list, placeholders, changed, { secretable = false } = {}) {
    this.placeholders = placeholders;
    this.changed = changed;
    this.secretable = secretable;
    this.rows = [];
    for (const pair of list) this.add(pair);
    this.add({ on: true, k: "", v: "" });
  }

  add(pair) {
    const row = {
      id: ++rowSeq,
      on: pair.on !== false,
      secret: Boolean(pair.secret),
      key: InputState.new({ placeholder: this.placeholders.key, value: pair.k }),
      value: InputState.new({ placeholder: this.placeholders.value, value: pair.v }),
    };
    row.value.set_masked(row.secret);
    const change = (_event, cx) => {
      if (row === this.rows[this.rows.length - 1] && !this.blank(row)) this.add({ on: true, k: "", v: "" });
      this.changed(cx);
    };
    row.key.on("change", change);
    row.value.on("change", change);
    this.rows.push(row);
    return row;
  }

  blank(row) {
    return row.key.value() === "" && row.value.value() === "";
  }

  remove(row, cx) {
    row.key.release();
    row.value.release();
    this.rows = this.rows.filter((other) => other !== row);
    if (this.rows.length === 0 || !this.blank(this.rows[this.rows.length - 1])) this.add({ on: true, k: "", v: "" });
    this.changed(cx);
  }

  toggle(row, cx) {
    row.on = !row.on;
    this.changed(cx);
  }

  lock(row, cx) {
    row.secret = !row.secret;
    row.value.set_masked(row.secret);
    this.changed(cx);
  }

  list() {
    return this.rows
      .filter((row) => !this.blank(row))
      .map((row) => ({ on: row.on, k: row.key.value(), v: row.value.value(), ...(this.secretable ? { secret: row.secret } : {}) }));
  }

  /** The pairs that will go: switched on and named. */
  count() {
    return this.rows.filter((row) => row.on && row.key.value().trim() !== "").length;
  }

  release() {
    for (const row of this.rows) {
      row.key.release();
      row.value.release();
    }
    this.rows = [];
  }
}

export default class HttpClient extends View {
  init(_props, cx) {
    this.t = textsFor(language());
    const stored = storage_get("requests");
    this.requests = Array.isArray(stored) ? stored.map(readRequest) : examples();
    if (!Array.isArray(stored)) storage_set("requests", this.requests);
    const history = storage_get("history");
    this.history = Array.isArray(history) ? history : [];
    const vars = storage_get("vars");
    this.vars = Array.isArray(vars) ? vars : [];
    this.tab = TABS.includes(storage_get("tab")) ? storage_get("tab") : "params";
    this.screen = "request";
    this.view = "pretty";
    this.methodOpen = false;
    this.dirty = false;
    this.vault = {};
    this.vaultReady = false;
    this.vaultDirty = false;
    /** The last answer of each request, by its id: lost when the tab is. */
    this.responses = new Map();
    /** The requests on their way, by id: `{ started }`. */
    this.sending = new Map();
    this.ed = null;
    this.varsEd = null;

    this.filter = InputState.new({ placeholder: this.t.filter });
    this.filter.on("change", (_event, cx) => cx.notify());

    const current = storage_get("current");
    const draft = storage_get("draft");
    const chosen = typeof current === "string" ? this.requests.find((req) => req.id === current) : null;
    this.mount(chosen ?? (draft && typeof draft === "object" ? readRequest(draft) : (this.requests[0] ?? this.fresh())));

    this.timer = cx.timer.every(SAVE_EVERY, (cx) => this.flush(cx));
    this.vaultLoading = (async () => {
      try {
        const kept = await secret("vault");
        this.vault = kept ? JSON.parse(kept) : {};
      } catch (_) {
        this.vault = {};
      }
      this.vaultReady = true;
    })();
    cx.spawn(async (cx) => {
      await this.vaultLoading;
      this.ed?.secret.set_value(this.vault[`auth:${this.req.id}`] ?? "");
      cx.notify();
    });
  }

  // — The request being written ————————————————————————————————————————————

  /** A new request, already in the collection. */
  fresh() {
    const req = blank();
    this.requests.push(req);
    storage_set("requests", this.requests);
    return req;
  }

  saved(req = this.req) {
    return this.requests.some((other) => other.id === req.id);
  }

  /** Opens a request in the editor: its fields made anew, the previous ones let go. */
  mount(req) {
    this.ed?.release();
    this.req = req;
    this.methodOpen = false;
    const t = this.t;
    const changed = (cx) => this.touch(cx);
    const name = InputState.new({ placeholder: t.untitled, value: req.name });
    const url = InputState.new({ placeholder: t.url, value: req.url });
    const body = TextareaState.new({ placeholder: req.bodyMode === "text" ? t.textPlaceholder : t.jsonPlaceholder, value: req.body });
    const user = InputState.new({ placeholder: t.user, value: req.auth.user });
    const secretField = InputState.new({ placeholder: "", value: this.vault[`auth:${req.id}`] ?? "" });
    secretField.set_masked(true);
    for (const state of [name, url, body, user]) state.on("change", (_event, cx) => changed(cx));
    url.on("submit", (_event, cx) => cx.spawn(async (cx) => this.send(cx)));
    body.on("submit", (event, cx) => {
      if (event.secondary) cx.spawn(async (cx) => this.send(cx));
    });
    secretField.on("change", (_event, cx) => {
      this.setVault(`auth:${req.id}`, secretField.value());
      cx.notify();
    });
    const pairs = (list, key) => new Pairs(list, { key, value: t.value }, changed);
    const params = pairs(req.params, t.key);
    const headers = pairs(req.headers, t.header);
    const form = pairs(req.form, t.key);
    this.ed = {
      name,
      url,
      body,
      user,
      secret: secretField,
      params,
      headers,
      form,
      release() {
        for (const state of [name, url, body, user, secretField]) state.release();
        for (const list of [params, headers, form]) list.release();
      },
    };
    if (this.saved(req)) {
      storage_set("current", req.id);
      storage_remove("draft");
    } else {
      storage_set("current", null);
      storage_set("draft", req);
    }
  }

  /** The request as the fields say it now. */
  collect() {
    const ed = this.ed;
    Object.assign(this.req, {
      name: ed.name.value(),
      url: ed.url.value(),
      body: ed.body.value(),
      params: ed.params.list(),
      headers: ed.headers.list(),
      form: ed.form.list(),
      auth: { mode: this.req.auth.mode, user: ed.user.value() },
    });
    return this.req;
  }

  touch(cx) {
    this.dirty = true;
    cx.notify();
  }

  /** What was typed, to the storage; the keyring once it has been read, so as not to write over it. */
  flush(cx) {
    if (this.dirty) {
      this.dirty = false;
      this.collect();
      if (this.saved()) storage_set("requests", this.requests);
      else storage_set("draft", this.req);
      cx?.notify();
    }
    if (this.vaultDirty && this.vaultReady) {
      this.vaultDirty = false;
      set_secret("vault", JSON.stringify(this.vault));
    }
  }

  setVault(key, value) {
    if (value === "") delete this.vault[key];
    else this.vault[key] = value;
    this.vaultDirty = true;
  }

  open(req, cx) {
    this.flush(cx);
    this.screen = "request";
    this.mount(req);
    cx.notify();
  }

  create(cx) {
    this.flush(cx);
    this.screen = "request";
    this.mount(this.fresh());
    cx.notify();
  }

  duplicate(cx) {
    this.flush(cx);
    const copy = readRequest({ ...clone(this.collect()), id: newId() });
    copy.name = `${this.req.name || this.t.untitled} (${this.t.duplicate.toLowerCase()})`;
    const at = this.requests.findIndex((req) => req.id === this.req.id);
    this.requests.splice(at + 1, 0, copy);
    storage_set("requests", this.requests);
    const kept = this.vault[`auth:${this.req.id}`];
    if (kept) this.setVault(`auth:${copy.id}`, kept);
    this.mount(copy);
    cx.notify();
  }

  remove(cx) {
    const at = this.requests.findIndex((req) => req.id === this.req.id);
    if (at < 0) return;
    const name = this.req.name || this.t.untitled;
    this.requests.splice(at, 1);
    storage_set("requests", this.requests);
    this.setVault(`auth:${this.req.id}`, "");
    this.responses.delete(this.req.id);
    this.dirty = false;
    this.mount(this.requests[Math.min(at, this.requests.length - 1)] ?? this.fresh());
    notify(this.t.removed(name));
    cx.notify();
  }

  /** A draft from the history, into the collection. */
  keep(cx) {
    this.collect();
    this.requests.push(this.req);
    storage_set("requests", this.requests);
    storage_set("current", this.req.id);
    storage_remove("draft");
    this.dirty = false;
    cx.notify();
  }

  /** A sent request again, as it went — a draft, so replaying it changes nothing saved. */
  replay(entry, cx) {
    this.flush(cx);
    const req = readRequest({ ...clone(entry.req), id: newId() });
    const kept = this.vault[`auth:${entry.from}`];
    if (kept) this.setVault(`auth:${req.id}`, kept);
    this.screen = "request";
    this.mount(req);
    cx.notify();
  }

  // — Variables ——————————————————————————————————————————————————————————

  /** Names to values, the locked ones from the keyring. */
  varMap() {
    const map = {};
    for (const v of this.vars) {
      const name = String(v.k ?? "").trim();
      if (v.on === false || name === "") continue;
      map[name] = v.secret ? (this.vault[`var:${name}`] ?? "") : String(v.v ?? "");
    }
    return map;
  }

  openVars(cx) {
    this.flush(cx);
    this.varsEd?.release();
    const list = this.vars.map((v) => ({ ...v, v: v.secret ? (this.vault[`var:${String(v.k).trim()}`] ?? "") : v.v }));
    this.varsEd = new Pairs(list, { key: this.t.key, value: this.t.value }, (cx) => this.saveVars(cx), { secretable: true });
    this.screen = "vars";
    cx.notify();
  }

  closeVars(cx) {
    this.varsEd?.release();
    this.varsEd = null;
    this.screen = "request";
    cx.notify();
  }

  /** The variables to the storage — a locked one's value to the keyring, never to the file. */
  saveVars(cx) {
    for (const key of Object.keys(this.vault)) if (key.startsWith("var:")) delete this.vault[key];
    this.vars = this.varsEd.list().map((v) => {
      if (!v.secret) return v;
      if (v.k.trim() !== "" && v.v !== "") this.vault[`var:${v.k.trim()}`] = v.v;
      return { ...v, v: "" };
    });
    this.vaultDirty = true;
    storage_set("vars", this.vars);
    cx.notify();
  }

  // — Sending ——————————————————————————————————————————————————————————————

  async send(cx) {
    const req = this.collect();
    if (this.sending.has(req.id)) return;
    this.dirty = true;
    this.flush(cx);
    const t = this.t;
    const answer = (result) => {
      this.responses.set(req.id, { at: new Date().toISOString(), ...result });
      cx.notify();
    };
    if (req.url.trim() === "") return answer({ error: t.noUrl });
    await this.vaultLoading;
    const sent = build(req, this.varMap(), this.vault[`auth:${req.id}`]);
    const host = hostOf(sent.url);
    if (host === "") return answer({ error: t.badUrl(sent.url), sent });
    const ticket = { started: Date.now() };
    this.sending.set(req.id, ticket);
    cx.notify();
    let result;
    try {
      const response = await fetch(sent.url, { method: sent.method, headers: sent.headers, body: sent.body });
      const text = await response.text();
      result = {
        status: response.status,
        ok: response.ok,
        url: response.url,
        text,
        kind: kindOf(text),
        size: byteLength(text),
        ms: Date.now() - ticket.started,
        sent,
      };
    } catch (error) {
      result = { error: `${t.failed} — ${String(error?.message ?? error)}`, ms: Date.now() - ticket.started, sent };
    }
    // Cancelled while it was on its way: the answer is dropped.
    if (this.sending.get(req.id) !== ticket) return;
    this.sending.delete(req.id);
    answer(result);
    this.remember(req, result);
  }

  cancel(cx) {
    this.sending.delete(this.req.id);
    cx.notify();
  }

  remember(req, result) {
    this.history.unshift({
      id: newId(),
      at: new Date().toISOString(),
      from: req.id,
      method: req.method,
      url: result.sent.url,
      status: result.status ?? 0,
      ms: result.ms,
      req: clone(req),
    });
    this.history.length = Math.min(this.history.length, HISTORY);
    storage_set("history", this.history);
  }

  /** The command that sends the same thing, to the clipboard. */
  copyAs(kind, cx) {
    const sent = build(this.collect(), this.varMap(), this.vault[`auth:${this.req.id}`]);
    copy_text(kind === "curl" ? asCurl(sent) : asHttpie(sent));
    notify(this.t.copied(kind === "curl" ? "curl" : "HTTPie"));
    cx.notify();
  }

  // — Drawing ——————————————————————————————————————————————————————————————

  render(cx) {
    return h_flex()
      .size_full()
      .gap_3()
      .on_key_down((event, cx) => {
        if (event.key === "enter" && (event.modifiers.control || event.modifiers.platform)) {
          cx.stop_propagation();
          cx.spawn(async (cx) => this.send(cx));
        }
      })
      .child(this.sidebar(cx))
      .child(
        v_flex()
          .flex_1()
          .min_w_0()
          .h_full()
          .gap_2()
          .child(this.screen === "vars" ? this.varsPane(cx) : this.editor(cx)),
      );
  }

  /** A small button: an icon, a label or both. */
  button(id, { icon, label, tone, tip, primary = false, danger = false, disabled = false, height = 26 }, act, cx) {
    const colors = cx.theme().colors;
    const tones = palette();
    return Button.new(id)
      .flex()
      .flex_none()
      .items_center()
      .justify_center()
      .gap_1()
      .h(height)
      .px_2()
      .rounded(6)
      .text_xs()
      .when(primary, (el) => el.bg(colors.primary).text_color(colors.primary_foreground).font_semibold())
      .when(!primary, (el) => el.text_color(danger ? tones.danger : colors.foreground))
      .when(!disabled, (el) =>
        el.hover((style) => (primary ? style.opacity(0.9) : style.bg(danger ? `${tones.danger}1f` : colors.secondary))),
      )
      .when(disabled, (el) => el.opacity(0.4))
      .when(Boolean(tip), (el) => el.tooltip(tip))
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

  field(state, cx, { height = 26, mono = false } = {}) {
    const colors = cx.theme().colors;
    return Input.new(state)
      .flex_1()
      .min_w_0()
      .h(height)
      .px_2()
      .rounded(6)
      .border(1)
      .border_color(colors.input)
      .bg(colors.background)
      .text_xs()
      .when(mono, (el) => el.font_family(cx.theme().typography.mono));
  }

  /** One of several, always one: a segmented control. */
  segmented(id, options, current, choose, cx) {
    const colors = cx.theme().colors;
    return h_flex()
      .flex_none()
      .p(2)
      .gap(2)
      .rounded(8)
      .bg(colors.muted)
      .children(
        options.map(([value, label]) =>
          Button.new(`${id}-${value}`)
            .flex()
            .items_center()
            .gap_1()
            .h(22)
            .px_2()
            .rounded(6)
            .text_xs()
            .when(current === value, (el) => el.bg(colors.background).text_color(colors.foreground).font_semibold())
            .when(current !== value, (el) =>
              el.text_color(colors.muted_foreground).hover((style) => style.text_color(colors.foreground)),
            )
            .on_click((_event, cx) => {
              choose(value, cx);
              cx.notify();
            })
            .child(label),
        ),
      );
  }

  /** A method in its colour, at a fixed width so the rows' addresses line up. */
  methodLabel(method, width = 52) {
    return div()
      .flex_none()
      .w(width)
      .text_xs()
      .font_semibold()
      .text_color(methodInk(method))
      .child(method === "DELETE" ? "DEL" : method === "OPTIONS" ? "OPT" : method);
  }

  heading(text, cx, extra) {
    const colors = cx.theme().colors;
    return h_flex()
      .flex_none()
      .items_center()
      .gap_2()
      .px_2()
      .pt_3()
      .pb_1()
      .child(div().flex_1().text_xs().font_semibold().text_color(colors.muted_foreground).child(text))
      .when(Boolean(extra), (el) => el.child(extra));
  }

  // — The left: the collection and the history ————————————————————————————

  sidebar(cx) {
    const colors = cx.theme().colors;
    const t = this.t;
    const needle = this.filter.value().trim().toLowerCase();
    const matches = (req) => needle === "" || `${req.name} ${req.method} ${req.url}`.toLowerCase().includes(needle);
    const shown = this.requests.filter(matches);
    const past = this.history.filter((entry) => needle === "" || `${entry.method} ${entry.url}`.toLowerCase().includes(needle)).slice(0, 30);
    const list = v_flex()
      .size_full()
      .overflow_y_scrollbar()
      .pb_2()
      .child(this.heading(`${t.collection} · ${this.requests.length}`, cx))
      .children(shown.map((req) => this.requestRow(req, cx)))
      .when(shown.length === 0, (el) => el.child(div().px_3().py_2().text_xs().text_color(colors.muted_foreground).child(t.noMatch)))
      .child(
        this.heading(
          t.history,
          cx,
          this.history.length > 0
            ? this.button("clear-history", { label: t.clearHistory, height: 20 }, () => {
                this.history = [];
                storage_set("history", []);
              }, cx)
            : null,
        ),
      )
      .children(past.map((entry) => this.historyRow(entry, cx)))
      .when(this.history.length === 0, (el) =>
        el.child(div().px_3().py_2().text_xs().text_color(colors.muted_foreground).child(t.noHistory)),
      );
    const variables = this.vars.filter((v) => v.on !== false && String(v.k ?? "").trim() !== "").length;
    return v_flex()
      .w(290)
      .flex_none()
      .h_full()
      .gap_2()
      .child(
        h_flex()
          .flex_none()
          .gap_1()
          .items_center()
          .child(this.field(this.filter, cx))
          .child(this.button("new", { icon: "plus", label: t.newRequest, primary: true }, (cx) => this.create(cx), cx)),
      )
      .child(
        v_flex()
          .flex_1()
          .min_h_0()
          .rounded(8)
          .border(1)
          .border_color(colors.border)
          .bg(colors.background)
          .overflow_hidden()
          .child(list),
      )
      .child(
        h_flex().flex_none().child(
          this.button(
            "vars",
            {
              icon: "braces",
              label: variables > 0 ? `${t.variables} · ${variables}` : t.variables,
              tone: this.screen === "vars" ? "primary" : undefined,
            },
            (cx) => (this.screen === "vars" ? this.closeVars(cx) : this.openVars(cx)),
            cx,
          ),
        ),
      );
  }

  requestRow(req, cx) {
    const colors = cx.theme().colors;
    const tones = palette();
    const current = req.id === this.req.id && this.screen === "request";
    // The one being written reads its fields: the storage is a beat behind.
    const name = (current ? this.ed.name.value() : req.name) || this.t.untitled;
    const url = current ? this.ed.url.value() : req.url;
    const result = this.responses.get(req.id);
    return Button.new(`req-${req.id}`)
      .flex()
      .flex_none()
      .items_center()
      .gap_2()
      .w_full()
      .h(44)
      .px_2()
      .border_l(3)
      .border_color(current ? tones.primary : "#00000000")
      .bg(current ? `${tones.primary}1f` : colors.background)
      .hover((style) => style.bg(current ? `${tones.primary}2e` : tones.list_hover))
      .on_click((_event, cx) => {
        if (!current) this.open(req, cx);
      })
      .child(this.methodLabel(req.method))
      .child(
        v_flex()
          .flex_1()
          .min_w_0()
          .child(div().truncate().text_sm().text_color(colors.foreground).child(name))
          .child(div().truncate().text_xs().text_color(colors.muted_foreground).child(shortUrl(url) || "—")),
      )
      .when(this.sending.has(req.id), (el) => el.child(div().flex_none().w(7).h(7).rounded(4).bg(tones.info)))
      .when(!this.sending.has(req.id) && result?.status > 0, (el) =>
        el.child(div().flex_none().w(7).h(7).rounded(4).bg(tones[statusTone(result.status)])),
      );
  }

  historyRow(entry, cx) {
    const colors = cx.theme().colors;
    const tones = palette();
    const ink = entry.status > 0 ? tones[statusTone(entry.status)] : tones.danger;
    return Button.new(`hist-${entry.id}`)
      .flex()
      .flex_none()
      .items_center()
      .gap_2()
      .w_full()
      .h(30)
      .px_2()
      .text_xs()
      .hover((style) => style.bg(tones.list_hover))
      .on_click((_event, cx) => this.replay(entry, cx))
      .child(this.methodLabel(entry.method, 36))
      .child(div().flex_1().min_w_0().truncate().text_color(colors.foreground).child(shortUrl(entry.url)))
      .child(
        div()
          .flex_none()
          .px(6)
          .rounded(8)
          .font_semibold()
          .bg(`${ink}26`)
          .text_color(ink)
          .child(entry.status > 0 ? String(entry.status) : "ERR"),
      )
      .child(div().flex_none().text_color(colors.muted_foreground).child(relative_time(entry.at)));
  }

  // — The middle: the request ——————————————————————————————————————————————

  editor(cx) {
    const colors = cx.theme().colors;
    const t = this.t;
    const saved = this.saved();
    const sending = this.sending.has(this.req.id);
    const head = h_flex()
      .flex_none()
      .gap_1()
      .items_center()
      .child(
        Input.new(this.ed.name)
          .flex_1()
          .min_w_0()
          .h(30)
          .px_2()
          .rounded(6)
          .text_base()
          .font_semibold()
          .hover((style) => style.bg(colors.secondary)),
      )
      .when(!saved, (el) =>
        el
          .child(Badge.new("draft", { text: t.draft, tone: "warning" }))
          .child(this.button("keep", { icon: "save", label: t.save, tip: t.saveHelp, primary: true }, (cx) => this.keep(cx), cx)),
      )
      .child(this.button("copy-curl", { icon: "copy", label: t.copyCurl }, (cx) => this.copyAs("curl", cx), cx))
      .child(this.button("copy-httpie", { icon: "copy", label: t.copyHttpie }, (cx) => this.copyAs("httpie", cx), cx))
      .when(saved, (el) =>
        el
          .child(this.button("duplicate", { icon: "copy", tip: t.duplicate }, (cx) => this.duplicate(cx), cx))
          .child(this.button("remove", { icon: "trash-2", tip: t.remove, danger: true }, (cx) => this.remove(cx), cx)),
      );
    const line = h_flex()
      .flex_none()
      .gap_2()
      .items_center()
      .child(this.methodPicker(cx))
      .child(this.field(this.ed.url, cx, { height: 34, mono: true }).text_sm())
      .child(
        sending
          ? this.button("cancel", { icon: "x", label: t.cancel, height: 34 }, (cx) => this.cancel(cx), cx)
          : this.button("send", { icon: "send", label: t.send, primary: true, height: 34 }, (cx) => this.send(cx), cx).px_4(),
      );
    return v_flex()
      .size_full()
      .gap_2()
      .child(head)
      .child(line)
      .child(
        v_flex()
          .flex_1()
          .min_h_0()
          .child(
            h_resizable("http-split")
              .size_full()
              .child(resizable_panel().size(480).size_range(320, 1200).child(this.requestPane(cx)))
              .child(resizable_panel().child(div().size_full().pl_2().child(this.responsePane(cx)))),
          ),
      );
  }

  methodPicker(cx) {
    const colors = cx.theme().colors;
    const method = this.req.method;
    const ink = methodInk(method);
    const trigger = h_flex()
      .flex_none()
      .items_center()
      .justify_between()
      .gap_1()
      .w(104)
      .h(34)
      .px_3()
      .rounded(6)
      .border(1)
      .border_color(`${ink}66`)
      .bg(`${ink}1a`)
      .cursor_pointer()
      .text_sm()
      .font_semibold()
      .text_color(ink)
      .child(method)
      .child(Icon.new("method-chevron", { name: "chevron-down", size: "xsmall", tone: "muted" }));
    const menu = v_flex()
      .w(150)
      .p_1()
      .gap(2)
      .rounded(8)
      .border(1)
      .border_color(colors.border)
      .bg(colors.background)
      .shadow_lg()
      .children(
        METHODS.map((option) =>
          Button.new(`method-${option}`)
            .flex()
            .items_center()
            .gap_2()
            .h(28)
            .px_2()
            .rounded(6)
            .text_sm()
            .font_semibold()
            .text_color(methodInk(option))
            .when(option === method, (el) => el.bg(colors.secondary))
            .hover((style) => style.bg(colors.secondary))
            .on_click((_event, cx) => {
              this.req.method = option;
              this.methodOpen = false;
              this.touch(cx);
            })
            .child(option),
        ),
      );
    return Popover.new("method")
      .anchor("top_left")
      .open(this.methodOpen)
      .on_open_change((open, cx) => {
        this.methodOpen = open;
        cx.notify();
      })
      .trigger(trigger)
      .content(menu);
  }

  requestPane(cx) {
    const colors = cx.theme().colors;
    const t = this.t;
    const ed = this.ed;
    const counted = (label, n) => (n > 0 ? `${label} · ${n}` : label);
    const labels = {
      params: counted(t.tabs.params, ed.params.count()),
      headers: counted(t.tabs.headers, ed.headers.count()),
      body: this.req.bodyMode === "none" ? t.tabs.body : `${t.tabs.body} · ${t.bodyModes[this.req.bodyMode]}`,
      auth: this.req.auth.mode === "none" ? t.tabs.auth : `${t.tabs.auth} · ${t.authModes[this.req.auth.mode]}`,
    };
    let content;
    if (this.tab === "params") content = this.pairsEditor("params", ed.params, cx);
    else if (this.tab === "headers") content = this.pairsEditor("headers", ed.headers, cx);
    else if (this.tab === "body") content = this.bodyEditor(cx);
    else content = this.authEditor(cx);
    // What will go, every variable replaced: the address as the server reads it.
    const req = this.collect();
    const vars = this.varMap();
    const sent = req.url.trim() ? build(req, vars, this.vault[`auth:${req.id}`]) : null;
    const missing = unknownVars(
      [req.url, ...req.params.flatMap((p) => [p.k, p.v]), ...req.headers.flatMap((p) => [p.k, p.v]), req.body, ...req.form.flatMap((p) => [p.k, p.v]), req.auth.user],
      vars,
    );
    return v_flex()
      .size_full()
      .rounded(8)
      .border(1)
      .border_color(colors.border)
      .bg(colors.background)
      .overflow_hidden()
      .child(
        h_flex()
          .flex_none()
          .p_2()
          .border_b(1)
          .border_color(colors.border)
          .child(
            this.segmented("tab", TABS.map((tab) => [tab, labels[tab]]), this.tab, (tab) => {
              this.tab = tab;
              storage_set("tab", tab);
            }, cx),
          ),
      )
      .child(v_flex().flex_1().min_h_0().p_3().child(content))
      .when(sent !== null, (el) =>
        el.child(
          v_flex()
            .flex_none()
            .gap_1()
            .px_3()
            .py_2()
            .border_t(1)
            .border_color(colors.border)
            .bg(colors.muted)
            .child(
              h_flex()
                .gap_2()
                .text_xs()
                .child(div().flex_none().text_color(colors.muted_foreground).child(t.finalUrl))
                .child(
                  div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(cx.theme().typography.mono)
                    .child(`${sent.method} ${sent.url}`),
                ),
            )
            .when(missing.length > 0, (el) =>
              el.child(div().text_xs().text_color(palette().warning).child(t.unknownVars(missing))),
            ),
        ),
      );
  }

  /** A switch drawn small: a pill and its knob. */
  switcher(id, on, act, cx) {
    const colors = cx.theme().colors;
    return Button.new(id)
      .flex()
      .flex_none()
      .items_center()
      .w(26)
      .h(16)
      .px(2)
      .rounded(8)
      .bg(on ? colors.primary : colors.muted)
      .border(1)
      .border_color(on ? colors.primary : colors.border)
      .when(on, (el) => el.justify_end())
      .on_click((_event, cx) => act(cx))
      .child(div().w(10).h(10).rounded(5).bg(on ? colors.primary_foreground : colors.muted_foreground));
  }

  pairsEditor(id, pairs, cx) {
    const last = pairs.rows[pairs.rows.length - 1];
    return v_flex()
      .size_full()
      .gap_1()
      .overflow_y_scrollbar()
      .children(
        pairs.rows.map((row) => {
          const empty = row === last && pairs.blank(row);
          return h_flex()
            .flex_none()
            .gap_1()
            .items_center()
            .when(!row.on, (el) => el.opacity(0.5))
            .child(
              empty
                ? div().flex_none().w(26)
                : this.switcher(`${id}-${row.id}-on`, row.on, (cx) => pairs.toggle(row, cx), cx),
            )
            .child(this.field(row.key, cx, { mono: true }).flex_none().w(180))
            .child(this.field(row.value, cx, { mono: true }))
            .when(pairs.secretable, (el) =>
              el.child(
                this.button(`${id}-${row.id}-lock`, { icon: "lock", tone: row.secret ? "primary" : "muted", tip: this.t.inKeyring }, (cx) =>
                  pairs.lock(row, cx), cx),
              ),
            )
            .child(
              empty
                ? div().flex_none().w(26)
                : this.button(`${id}-${row.id}-remove`, { icon: "x", tone: "muted" }, (cx) => pairs.remove(row, cx), cx),
            );
        }),
      );
  }

  bodyEditor(cx) {
    const colors = cx.theme().colors;
    const t = this.t;
    const mode = this.req.bodyMode;
    const modes = this.segmented("body-mode", BODY_MODES.map((m) => [m, t.bodyModes[m]]), mode, (m, cx) => {
      this.req.bodyMode = m;
      this.touch(cx);
    }, cx);
    const top = h_flex().flex_none().gap_2().items_center().child(modes);
    if (mode === "none") {
      return v_flex()
        .size_full()
        .gap_3()
        .child(top)
        .child(div().text_xs().text_color(colors.muted_foreground).child(t.noBody));
    }
    if (mode === "form" || mode === "multipart") {
      return v_flex()
        .size_full()
        .gap_2()
        .child(top)
        .child(div().flex_none().text_xs().text_color(colors.muted_foreground).child(mode === "form" ? t.formHelp : t.multipartHelp))
        .child(v_flex().flex_1().min_h_0().child(this.pairsEditor("form", this.ed.form, cx)));
    }
    const text = this.ed.body.value();
    const problem = mode === "json" ? jsonError(text) : null;
    const tones = palette();
    return v_flex()
      .size_full()
      .gap_2()
      .child(
        top
          .child(div().flex_1())
          .when(mode === "json" && text.trim() !== "", (el) =>
            el
              .child(
                h_flex()
                  .flex_none()
                  .gap_1()
                  .items_center()
                  .text_xs()
                  .text_color(problem ? tones.danger : tones.success)
                  .child(Icon.new("json-state", { name: problem ? "circle-x" : "check", size: "xsmall", tone: problem ? "danger" : "success" }))
                  .child(div().max_w(220).truncate().child(problem ? t.jsonBad(problem) : t.jsonOk)),
              )
              .child(
                this.button("format", { icon: "braces", label: t.format, disabled: problem !== null }, (cx) => {
                  this.ed.body.set_value(JSON.stringify(JSON.parse(this.ed.body.value()), null, 2));
                  this.touch(cx);
                }, cx),
              ),
          ),
      )
      .child(
        Textarea.new(this.ed.body)
          .flex_1()
          .min_h(120)
          .w_full()
          .p_2()
          .rounded(6)
          .border(1)
          .border_color(problem ? `${tones.danger}99` : colors.input)
          .bg(colors.background)
          .text_xs()
          .font_family(cx.theme().typography.mono),
      );
  }

  authEditor(cx) {
    const colors = cx.theme().colors;
    const t = this.t;
    const mode = this.req.auth.mode;
    const labelled = (label, el) =>
      v_flex().gap_1().child(div().text_xs().font_semibold().child(label)).child(h_flex().child(el));
    return v_flex()
      .size_full()
      .gap_3()
      .child(
        h_flex().child(
          this.segmented("auth-mode", AUTH_MODES.map((m) => [m, t.authModes[m]]), mode, (m, cx) => {
            this.req.auth.mode = m;
            this.touch(cx);
          }, cx),
        ),
      )
      .when(mode === "none", (el) => el.child(div().text_xs().text_color(colors.muted_foreground).child(t.noAuth)))
      .when(mode === "bearer", (el) => el.child(labelled(t.token, this.field(this.ed.secret, cx, { mono: true }))))
      .when(mode === "basic", (el) =>
        el
          .child(labelled(t.user, this.field(this.ed.user, cx, { mono: true })))
          .child(labelled(t.password, this.field(this.ed.secret, cx, { mono: true }))),
      )
      .when(mode !== "none", (el) =>
        el.child(
          h_flex()
            .gap_1()
            .items_center()
            .text_xs()
            .text_color(colors.muted_foreground)
            .child(Icon.new("auth-lock", { name: "lock", size: "xsmall", tone: "muted" }))
            .child(t.inKeyring),
        ),
      );
  }

  // — The right: the answer ————————————————————————————————————————————————

  responsePane(cx) {
    const colors = cx.theme().colors;
    const t = this.t;
    const frame = v_flex().size_full().rounded(8).border(1).border_color(colors.border).bg(colors.background).overflow_hidden();
    const centred = (icon, text, hint, extra) =>
      v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_2()
        .p_4()
        .child(Icon.new("response-icon", { name: icon, size: "large", tone: "muted" }))
        .child(div().text_sm().text_center().text_color(colors.muted_foreground).child(text))
        .when(Boolean(hint), (el) => el.child(div().text_xs().text_center().text_color(colors.muted_foreground).opacity(0.7).child(hint)))
        .when(Boolean(extra), (el) => el.child(extra));
    const ticket = this.sending.get(this.req.id);
    if (ticket) {
      return frame.child(
        centred("send", t.sending, null, this.button("cancel-response", { icon: "x", label: t.cancel }, (cx) => this.cancel(cx), cx)),
      );
    }
    const result = this.responses.get(this.req.id);
    if (!result) return frame.child(centred("send", t.emptyResponse, t.emptyHint));
    if (result.error) {
      return frame.child(
        v_flex()
          .p_3()
          .gap_2()
          .child(
            h_flex()
              .gap_2()
              .items_center()
              .child(Badge.new("response-error", { text: "Error", tone: "danger" }))
              .when(result.ms !== undefined, (el) =>
                el.child(div().text_xs().text_color(colors.muted_foreground).child(humanTime(result.ms))),
              ),
          )
          .child(div().text_sm().text_color(colors.destructive).child(result.error)),
      );
    }
    const tones = palette();
    const reason = REASONS[result.status] ?? "";
    const views = this.viewsOf(result);
    const view = views[this.view] ? this.view : "raw";
    const meta = (text) => div().flex_none().text_xs().text_color(colors.muted_foreground).child(text);
    const head = v_flex()
      .flex_none()
      .gap_1()
      .px_3()
      .py_2()
      .border_b(1)
      .border_color(colors.border)
      .child(
        h_flex()
          .gap_2()
          .items_center()
          .child(
            div()
              .flex_none()
              .px_2()
              .py(1)
              .rounded(6)
              .text_sm()
              .font_semibold()
              .bg(`${tones[statusTone(result.status)]}26`)
              .text_color(tones[statusTone(result.status)])
              .child(`${result.status}${reason ? ` ${reason}` : ""}`),
          )
          .child(meta(humanTime(result.ms)))
          .child(meta("·"))
          .child(meta(humanSize(result.size)))
          .when(result.kind !== "text", (el) => el.child(Badge.new("response-kind", { text: result.kind.toUpperCase(), tone: "info" })))
          .child(div().flex_1())
          .when(views.pretty !== undefined, (el) =>
            el.child(this.segmented("response-view", [["pretty", t.pretty], ["raw", t.raw]], view, (v) => (this.view = v), cx)),
          )
          .child(
            this.button("copy-body", { icon: "copy", tip: t.copyBody }, () => {
              copy_text(views[view].text);
              notify(t.bodyCopied);
            }, cx),
          )
          .child(this.button("ask", { icon: "bot", tip: t.ask }, () => ask_agent(prompt(t.askIntro, result.sent, result)), cx)),
      )
      .when(Boolean(result.url) && result.url !== result.sent.url, (el) =>
        el.child(
          h_flex()
            .gap_2()
            .text_xs()
            .child(div().flex_none().text_color(colors.muted_foreground).child(t.redirected))
            .child(div().flex_1().min_w_0().truncate().child(result.url)),
        ),
      );
    if (result.text === "") return frame.child(head).child(centred("file", t.emptyBody, null));
    return frame.child(head).child(this.responseBody(result, views[view], cx));
  }

  /** The body's readings — raw always, pretty when indenting changes it — laid out once per answer. */
  viewsOf(result) {
    if (!result.views) {
      const path = pathFor(result.kind);
      const formatted = pretty(result.text, result.kind);
      result.views = { raw: { text: result.text, lines: linesOf(result.text), path } };
      if (formatted !== result.text) result.views.pretty = { text: formatted, lines: linesOf(formatted), path };
    }
    return result.views;
  }

  /** The body, one virtual row a line: a page of megabytes costs what is on screen. */
  responseBody(result, shown, cx) {
    const height = sizes().line;
    const lines = shown.lines;
    const stamp = `${result.at}-${shown === result.views.pretty ? "p" : "r"}`;
    return v_flex()
      .relative()
      .flex_1()
      .min_h_0()
      .px_2()
      .py_1()
      .child(
        v_virtual_list(
          "http-response",
          lines.length,
          height,
          (index) => `${stamp}:${index}`,
          (range) =>
            lines
              .slice(range.start, range.end)
              .map((_, at) => CodeLine.new(`${stamp}:${range.start + at}`, { path: shown.path, lines, index: range.start + at, mark: 0 })),
        ).size_full(),
      )
      .child(Scrollbar.vertical("http-response").absolute().inset_0());
  }

  // — Variables ————————————————————————————————————————————————————————————

  varsPane(cx) {
    const colors = cx.theme().colors;
    const t = this.t;
    return v_flex()
      .size_full()
      .gap_3()
      .p_3()
      .rounded(8)
      .border(1)
      .border_color(colors.border)
      .bg(colors.background)
      .child(
        h_flex()
          .flex_none()
          .gap_2()
          .items_center()
          .child(Icon.new("vars-icon", { name: "braces", size: "small" }))
          .child(div().flex_1().text_lg().font_semibold().child(t.variables))
          .child(this.button("vars-back", { icon: "chevron-left", label: t.back }, (cx) => this.closeVars(cx), cx)),
      )
      .child(div().flex_none().text_xs().text_color(colors.muted_foreground).child(t.variablesHelp))
      .child(v_flex().flex_1().min_h_0().child(this.pairsEditor("vars", this.varsEd, cx)));
  }
}
