// What Sentry's API is asked, and how its answers are read — no drawing here.
//
// **An answer is read field by field.** Sentry writes `null` freely — a frame
// with no module, an issue with no culprit —; every field below falls back
// to empty rather than failing the whole list.

export const DEFAULT_HOST = "https://sentry.io";
export const DEFAULT_QUERY = "is:unresolved";
const PERIOD = "14d";
const PER_PAGE = 50;

const text = (value) => (typeof value === "string" ? value : value == null ? "" : String(value));
const number = (value) => (typeof value === "number" ? value : Number.parseInt(text(value), 10) || 0);
const list = (value) => (Array.isArray(value) ? value : []);

/** The host a grant names: `https://sentry.example.com/` → `sentry.example.com`. */
export const hostOf = (address) =>
  text(address).trim().replace(/^https?:\/\//, "").replace(/\/.*$/, "").toLowerCase();

const base = (host) => `https://${hostOf(host || DEFAULT_HOST)}`;

/** The project's issues, most recent first. */
export const issuesUrl = ({ host, org, project, query }) =>
  `${base(host)}/api/0/projects/${encodeURIComponent(org)}/${encodeURIComponent(project)}/issues/` +
  `?query=${encodeURIComponent(query || DEFAULT_QUERY)}&statsPeriod=${PERIOD}&limit=${PER_PAGE}`;

/**
 * An issue's latest event. `…/events/?full=true&per_page=1` and not
 * `…/events/latest/`, gone from the API; `full=true` brings the stack back.
 */
export const eventUrl = ({ host, org }, issue) =>
  `${base(host)}/api/0/organizations/${encodeURIComponent(org)}/issues/${encodeURIComponent(issue)}/events/?full=true&per_page=1`;

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
  return json;
}

/** The issues of a list answer. */
export const readIssues = (json) =>
  list(json).map((issue) => {
    const metadata = issue?.metadata ?? {};
    return {
      id: text(issue?.id),
      shortId: text(issue?.shortId),
      title: text(issue?.title),
      kind: text(metadata.type) || text(issue?.title),
      value: text(metadata.value),
      culprit: text(issue?.culprit),
      level: text(issue?.level),
      count: number(issue?.count),
      users: number(issue?.userCount),
      firstSeen: text(issue?.firstSeen),
      lastSeen: text(issue?.lastSeen),
      permalink: text(issue?.permalink),
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
      line: number(frame?.lineNo),
      inApp: frame?.inApp === true,
      context: list(frame?.context)
        .filter((pair) => Array.isArray(pair) && pair.length >= 2)
        .map(([line, source]) => ({ line: number(line), source: text(source) })),
    });
  }
};

/** The latest event of an events answer, or null — both trace shapes read. */
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
      for (const crumb of list(data.values)) {
        crumbs.push({ category: text(crumb?.category), message: text(crumb?.message) });
      }
    }
  }
  return {
    message: text(raw.message),
    tags: list(raw.tags)
      .map((tag) => ({ key: text(tag?.key), value: text(tag?.value) }))
      .filter((tag) => tag.key !== ""),
    // Sentry's order is the call's, oldest first; read newest first.
    frames: trace.reverse(),
    crumbs: crumbs.slice(-20),
  };
};

/** "3 h", "2 d": how long ago an ISO date was. */
export const ago = (iso) => {
  const seconds = (Date.now() - Date.parse(iso)) / 1000;
  if (!Number.isFinite(seconds)) return "";
  if (seconds < 3600) return `${Math.max(1, Math.round(seconds / 60))} min`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)} h`;
  return `${Math.round(seconds / 86400)} d`;
};

/** What the agent is handed: the reference first, then what the event says. */
export const prompt = (org, issue, event) => {
  const out = [
    "Here is a Sentry error from this project. Find its cause in the code and fix it; say what you changed.",
    "",
    `- Sentry issue: ${issue.shortId}`,
    `- Organisation: ${org}`,
    issue.permalink ? `- ${issue.permalink}` : "",
    "",
    `# ${issue.kind}`,
    issue.value,
    issue.culprit,
    `${issue.count} occurrences, ${issue.firstSeen} → ${issue.lastSeen}`,
  ].filter((line, index, lines) => line !== "" || lines[index - 1] !== "");
  if (event) {
    if (event.tags.length) {
      out.push("", "## Context", ...event.tags.map((tag) => `- ${tag.key}: ${tag.value}`));
    }
    if (event.frames.length) {
      out.push("", "## Trace", ...event.frames.map((frame) => `- ${frame.file}:${frame.line}${frame.function ? ` · ${frame.function}` : ""}`));
    }
    for (const frame of event.frames.filter((frame) => frame.inApp && frame.context.length)) {
      out.push("", `## ${frame.file}:${frame.line}`, "````");
      for (const { line, source } of frame.context) {
        out.push(`${line === frame.line ? ">" : " "} ${String(line).padStart(5)} ${source}`);
      }
      out.push("````");
    }
    if (event.crumbs.length) {
      out.push("", "## Breadcrumbs", ...event.crumbs.map((crumb) => `- ${crumb.category} · ${crumb.message}`));
    }
  }
  return out.join("\n").trim();
};
