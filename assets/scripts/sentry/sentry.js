// What Sentry's API is asked, and how its answers are read — no drawing here.
// The same reading as Claudhub's own `sentry.rs`, so the two agree.
//
// **An answer is read field by field.** Sentry writes `null` freely — a frame
// with no module, an issue with no culprit —; every field below falls back
// to empty rather than failing the whole list.

export const DEFAULT_HOST = "https://sentry.io";
export const DEFAULT_QUERY = "is:unresolved";
/** How far back the list looks: "how long has this been happening" without paging. */
const PERIOD = "14d";
const PER_PAGE = 50;
/** The last breadcrumbs: they describe the second before. */
const CRUMBS = 12;

const text = (value) => (typeof value === "string" ? value : value == null ? "" : String(value));
const count = (value) => (typeof value === "number" ? value : Number.parseInt(text(value), 10) || 0);
const list = (value) => (Array.isArray(value) ? value : []);

/** The host a grant names: `https://sentry.example.com/` → `sentry.example.com`. */
export const hostOf = (address) =>
  text(address).trim().replace(/^https?:\/\//, "").replace(/\/.*$/, "").toLowerCase();

const base = (host) => `https://${hostOf(host || DEFAULT_HOST)}`;
const part = encodeURIComponent;

/** The project's issues, most recent first. */
export const issuesUrl = ({ host, org, project, query }) =>
  `${base(host)}/api/0/projects/${part(org)}/${part(project)}/issues/` +
  `?query=${part(query || DEFAULT_QUERY)}&statsPeriod=${PERIOD}&limit=${PER_PAGE}`;

/**
 * An issue's latest event. `…/events/?full=true&per_page=1` and not
 * `…/events/latest/`, gone from the API; `full=true` brings the stack back.
 */
export const eventUrl = ({ host, org }, issue) =>
  `${base(host)}/api/0/organizations/${part(org)}/issues/${part(issue)}/events/?full=true&per_page=1`;

/** What its tags are worth across every occurrence. */
export const tagsUrl = ({ host, org }, issue) =>
  `${base(host)}/api/0/organizations/${part(org)}/issues/${part(issue)}/tags/`;

/** An answer's body as JSON, or why not — Sentry's own `detail` when it gave one. */
export async function readJson(response) {
  const body = await response.text();
  let json = null;
  try {
    json = JSON.parse(body);
  } catch (_) {
    // Not JSON: said below with the status.
  }
  if (!response.ok) {
    const detail = json && typeof json === "object" ? text(json.detail) : "";
    throw new Error(`${response.status} ${detail || body.slice(0, 200)}`.trim());
  }
  if (!Array.isArray(json)) throw new Error("unreadable Sentry response");
  return json;
}

/** The issues of a list answer. */
export const readIssues = (json) =>
  list(json)
    .filter((issue) => issue && typeof issue === "object")
    .map((issue) => {
      const title = text(issue.title);
      const metadata = issue.metadata ?? {};
      // The kind alone when the metadata has one: the title repeats it with
      // the message glued on.
      const kind = text(metadata.type);
      return {
        id: text(issue.id),
        shortId: text(issue.shortId),
        title,
        kind: kind || title,
        value: text(metadata.value),
        culprit: text(issue.culprit),
        level: text(issue.level),
        status: text(issue.status),
        count: count(issue.count),
        users: count(issue.userCount),
        firstSeen: text(issue.firstSeen),
        lastSeen: text(issue.lastSeen),
        permalink: text(issue.permalink),
      };
    });

const frames = (stacktrace, out) => {
  for (const frame of list(stacktrace?.frames)) {
    // The first that says anything: a path, an absolute path, a module.
    const file = [frame?.filename, frame?.absPath, frame?.module].map(text).find((name) => name !== "");
    if (!file) continue;
    out.push({
      file,
      function: text(frame?.function),
      line: count(frame?.lineNo),
      inApp: frame?.inApp === true,
      context: list(frame?.context)
        .filter((pair) => Array.isArray(pair) && pair.length >= 2)
        .map(([line, source]) => ({ line: count(line), text: text(source) })),
    });
  }
};

/** The latest event of an events answer, or null — both trace shapes read. Frames oldest first, Sentry's order. */
export const readEvent = (json) => {
  const raw = list(json)[0];
  if (!raw) return null;
  const trace = [];
  const crumbs = [];
  for (const entry of list(raw.entries)) {
    const data = entry?.data ?? {};
    if (entry?.type === "exception") {
      for (const value of list(data.values)) frames(value?.stacktrace, trace);
    } else if (entry?.type === "stacktrace") {
      frames(data, trace);
    } else if (entry?.type === "breadcrumbs") {
      for (const crumb of list(data.values).slice(-CRUMBS)) {
        const message = text(crumb?.message) || text(crumb?.type);
        const category = text(crumb?.category);
        if (message || category) crumbs.push({ category, message, level: text(crumb?.level) });
      }
    }
  }
  return {
    message: text(raw.message),
    tags: list(raw.tags)
      .map((tag) => ({ key: text(tag?.key), value: text(tag?.value) }))
      .filter((tag) => tag.key !== ""),
    frames: trace,
    crumbs,
  };
};

/** What each tag is worth across the issue's occurrences: those with more than one value. */
export const readSpreads = (json) =>
  list(json)
    .filter((spread) => list(spread?.topValues).length > 1)
    .map((spread) => {
      const total = count(spread.totalValues);
      return {
        name: text(spread.name) || text(spread.key),
        values: list(spread.topValues).map((value) => ({
          value: text(value?.value),
          share: total === 0 ? 0 : Math.min(100, Math.floor((count(value?.count) * 100) / total)),
        })),
      };
    });

/** Whether the filter's word is in what the row shows — smart case, as every panel reads it. */
export const matches = (issue, needle) => {
  const word = needle.trim();
  if (word === "") return true;
  const sensitive = word !== word.toLowerCase();
  const hay = (value) => (sensitive ? value : value.toLowerCase()).includes(sensitive ? word : word.toLowerCase());
  return hay(issue.title) || hay(issue.culprit);
};

/** A fence the text cannot close. */
const fence = (body) => {
  const longest = Math.max(0, ...(body.match(/`+/g) ?? []).map((run) => run.length));
  const marks = "`".repeat(Math.max(3, longest + 1));
  return `${marks}\n${body.endsWith("\n") ? body : `${body}\n`}${marks}\n`;
};

/**
 * What the agent is handed: the reference, the context, the trace, and the
 * code around our own frames — the prompt of Claudhub's own panel.
 * `locate(path)` brings a server's path back to the repository.
 */
export const prompt = (intro, org, issue, event, locate) => {
  let out = `${intro}\n\n`;
  const reference = [];
  if (issue.shortId) reference.push(`- Sentry issue: ${issue.shortId}`);
  if (issue.id) reference.push(`- Id: ${issue.id}`);
  if (org) reference.push(`- Organisation: ${org}`);
  if (issue.permalink) reference.push(`- ${issue.permalink}`);
  if (reference.length) {
    out += reference.join("\n");
    out += "\n\nIf you have Sentry's MCP server, you can ask it for the rest with this reference.\n\n";
  }
  out += `# ${issue.kind}\n`;
  if (issue.value) out += `${issue.value}\n`;
  if (issue.culprit) out += `${issue.culprit}\n`;
  out += `${issue.count} occurrences, ${issue.firstSeen} → ${issue.lastSeen}\n`;
  if (!event) return out.replace(/\n+$/, "");
  const path = (frame) => locate(frame.file) ?? frame.file.replaceAll("\\", "/");
  if (event.tags.length) {
    out += "\n## Context\n";
    for (const tag of event.tags) out += `- ${tag.key}: ${tag.value}\n`;
  }
  if (event.frames.length) {
    out += "\n## Trace\n";
    for (const frame of event.frames) {
      out += `- ${path(frame)}:${frame.line}${frame.function ? ` · ${frame.function}` : ""}\n`;
    }
  }
  for (const frame of event.frames.filter((frame) => frame.inApp && frame.context.length)) {
    out += `\n## ${path(frame)}:${frame.line}\n`;
    // The offending line is marked: the text reaches the agent without the gutter.
    const code = frame.context
      .map(({ line, text }) => `${line === frame.line ? ">" : " "} ${String(line).padStart(5)} ${text}\n`)
      .join("");
    out += fence(code);
  }
  if (event.crumbs.length) {
    out += "\n## Breadcrumbs\n";
    for (const crumb of event.crumbs) out += `- ${crumb.category} · ${crumb.message}\n`;
  }
  return out.replace(/\n+$/, "");
};
