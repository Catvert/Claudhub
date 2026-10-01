// What a request is, how it becomes a `fetch`, and how its answer is read —
// no drawing here.
//
// A request is plain JSON, kept as is in the script's storage:
//   { id, name, method, url, params, headers, bodyMode, body, form, auth }
// `params`, `headers` and `form` are pairs `{ on, k, v }`; `auth` is
// `{ mode, user }` — its token or password lives in the keyring, never here.
// `{{name}}` anywhere is a variable, replaced when the request goes.

export const METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
export const BODY_MODES = ["none", "json", "form", "multipart", "text"];
export const AUTH_MODES = ["none", "bearer", "basic"];
/** How many sent requests the history keeps. */
export const HISTORY = 60;
/** A line longer than this is cut in the response: a minified page is one line. */
const WRAP = 240;

export const newId = () => `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 7)}`;

export const blank = (fields = {}) => ({
  id: newId(),
  name: "",
  method: "GET",
  url: "",
  params: [],
  headers: [],
  bodyMode: "none",
  body: "",
  form: [],
  auth: { mode: "none", user: "" },
  ...fields,
});

/** A request read back from storage, whatever version wrote it. */
export const readRequest = (raw) => {
  const pairs = (list) =>
    (Array.isArray(list) ? list : []).map((p) => ({ on: p?.on !== false, k: String(p?.k ?? ""), v: String(p?.v ?? "") }));
  return blank({
    id: String(raw?.id ?? newId()),
    name: String(raw?.name ?? ""),
    method: METHODS.includes(raw?.method) ? raw.method : "GET",
    url: String(raw?.url ?? ""),
    params: pairs(raw?.params),
    headers: pairs(raw?.headers),
    bodyMode: BODY_MODES.includes(raw?.bodyMode) ? raw.bodyMode : "none",
    body: String(raw?.body ?? ""),
    form: pairs(raw?.form),
    auth: { mode: AUTH_MODES.includes(raw?.auth?.mode) ? raw.auth.mode : "none", user: String(raw?.auth?.user ?? "") },
  });
};

/** `{{name}}` replaced by its value; a name nobody defined stays as written, so the mistake shows. */
export const expand = (text, vars) =>
  String(text).replace(/\{\{\s*([\w.-]+)\s*\}\}/g, (all, name) => (Object.hasOwn(vars, name) ? vars[name] : all));

/** The names a text uses that no variable defines. */
export const unknownVars = (texts, vars) => {
  const missing = new Set();
  for (const text of texts) {
    for (const [, name] of String(text).matchAll(/\{\{\s*([\w.-]+)\s*\}\}/g)) if (!Object.hasOwn(vars, name)) missing.add(name);
  }
  return [...missing];
};

const live = (pairs) => pairs.filter((p) => p.on && p.k.trim() !== "");

/** An address as HTTPie reads one: `:3000/x` is localhost, a bare host is https. */
export const withScheme = (url) => {
  const u = url.trim();
  if (/^https?:\/\//i.test(u)) return u;
  if (u.startsWith(":")) return `http://localhost${u}`;
  if (/^(localhost|127\.|0\.0\.0\.0)/i.test(u)) return `http://${u}`;
  return `https://${u}`;
};

/** The host a grant names: `https://user@api.example.com:8443/x` → `api.example.com`. */
export const hostOf = (url) =>
  (url.match(/^https?:\/\/([^/?#]+)/i)?.[1] ?? "")
    .replace(/^.*@/, "")
    .replace(/:\d+$/, "")
    .toLowerCase();

/** The address and the path alone, for a row of the list. */
export const shortUrl = (url) => url.trim().replace(/^https?:\/\//i, "");

/** UTF-8 bytes of a text: this runtime has no TextEncoder. */
const utf8 = (text) => {
  const out = [];
  for (const char of text) {
    const c = char.codePointAt(0);
    if (c < 0x80) out.push(c);
    else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
    else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
    else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
  }
  return out;
};

/** How many bytes a text weighs once sent. */
export const byteLength = (text) => {
  let n = 0;
  for (const char of text) {
    const c = char.codePointAt(0);
    n += c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4;
  }
  return n;
};

const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const base64 = (text) => {
  const bytes = utf8(text);
  let out = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const [a, b, c] = [bytes[i], bytes[i + 1], bytes[i + 2]];
    out += B64[a >> 2] + B64[((a & 3) << 4) | ((b ?? 0) >> 4)];
    out += b === undefined ? "=" : B64[((b & 15) << 2) | ((c ?? 0) >> 6)];
    out += c === undefined ? "=" : B64[c & 63];
  }
  return out;
};

const has = (headers, name) => Object.keys(headers).some((key) => key.toLowerCase() === name.toLowerCase());

const formEncode = (pairs, vars) =>
  pairs.map((p) => `${encodeURIComponent(expand(p.k, vars))}=${encodeURIComponent(expand(p.v, vars))}`).join("&");

/**
 * What `fetch` is given: `{ method, url, headers, body }`, every variable
 * replaced. `secretValue` is the request's token or password, from the keyring.
 */
export const build = (req, vars, secretValue) => {
  // The fragment is the browser's: it never reaches the server.
  let url = withScheme(expand(req.url, vars)).replace(/#.*$/, "");
  const params = live(req.params);
  if (params.length) url += `${url.includes("?") ? "&" : "?"}${formEncode(params, vars)}`;
  const headers = {};
  for (const p of live(req.headers)) {
    const key = expand(p.k, vars).trim();
    const value = expand(p.v, vars);
    // The same header twice is one header, its values joined: HTTP's own reading.
    const known = Object.keys(headers).find((k) => k.toLowerCase() === key.toLowerCase());
    if (known) headers[known] = `${headers[known]}, ${value}`;
    else headers[key] = value;
  }
  const secretText = expand(secretValue ?? "", vars);
  if (!has(headers, "Authorization")) {
    if (req.auth.mode === "bearer" && secretText) headers.Authorization = `Bearer ${secretText}`;
    if (req.auth.mode === "basic") headers.Authorization = `Basic ${base64(`${expand(req.auth.user, vars)}:${secretText}`)}`;
  }
  let body;
  let type = null;
  switch (req.bodyMode) {
    case "json":
      body = expand(req.body, vars);
      type = "application/json";
      if (!has(headers, "Accept")) headers.Accept = "application/json, */*;q=0.5";
      break;
    case "text":
      body = expand(req.body, vars);
      type = "text/plain; charset=utf-8";
      break;
    case "form":
      body = formEncode(live(req.form), vars);
      type = "application/x-www-form-urlencoded; charset=utf-8";
      break;
    case "multipart": {
      const boundary = `----claudhub${newId()}`;
      body = live(req.form)
        .map((p) => {
          const name = expand(p.k, vars).replace(/"/g, "%22");
          return `--${boundary}\r\nContent-Disposition: form-data; name="${name}"\r\n\r\n${expand(p.v, vars)}\r\n`;
        })
        .join("");
      body += `--${boundary}--\r\n`;
      type = `multipart/form-data; boundary=${boundary}`;
      break;
    }
  }
  if (type && !has(headers, "Content-Type")) headers["Content-Type"] = type;
  if (body === "" && req.bodyMode !== "form") body = undefined;
  return { method: req.method, url, headers, body };
};

/** Whether a JSON body reads: null when it does, the parser's complaint when not. */
export const jsonError = (text) => {
  if (text.trim() === "") return null;
  try {
    JSON.parse(text);
    return null;
  } catch (error) {
    return String(error?.message ?? error);
  }
};

/** What a body is, guessed from itself: `fetch` here gives no headers to ask. */
export const kindOf = (text) => {
  const head = text.trimStart().slice(0, 200).toLowerCase();
  if (/^[[{]/.test(head) && jsonError(text) === null) return "json";
  if (head.startsWith("<?xml") || head.startsWith("<svg")) return "xml";
  if (head.startsWith("<")) return "html";
  return "text";
};

/** The file name a code line is coloured by. */
export const pathFor = (kind) => ({ json: "response.json", xml: "response.xml", html: "response.html" })[kind] ?? "response.txt";

/** The response's lines for `CodeLine`: numbered from 1, a long one cut in several. */
export const linesOf = (text) => {
  const out = [];
  for (const line of text.split(/\r?\n/)) {
    if (line.length <= WRAP) out.push({ line: out.length + 1, text: line });
    else for (let at = 0; at < line.length; at += WRAP) out.push({ line: out.length + 1, text: line.slice(at, at + WRAP) });
  }
  return out;
};

/** The body as one reads it: JSON indented, anything else as it came. */
export const pretty = (text, kind) => {
  if (kind !== "json") return text;
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch (_) {
    return text;
  }
};

export const REASONS = {
  100: "Continue", 101: "Switching Protocols",
  200: "OK", 201: "Created", 202: "Accepted", 204: "No Content", 206: "Partial Content",
  301: "Moved Permanently", 302: "Found", 303: "See Other", 304: "Not Modified", 307: "Temporary Redirect", 308: "Permanent Redirect",
  400: "Bad Request", 401: "Unauthorized", 403: "Forbidden", 404: "Not Found", 405: "Method Not Allowed", 406: "Not Acceptable",
  408: "Request Timeout", 409: "Conflict", 410: "Gone", 413: "Payload Too Large", 415: "Unsupported Media Type",
  422: "Unprocessable Entity", 429: "Too Many Requests",
  500: "Internal Server Error", 501: "Not Implemented", 502: "Bad Gateway", 503: "Service Unavailable", 504: "Gateway Timeout",
};

/** What a status is worth in colour. */
export const statusTone = (status) =>
  status >= 500 ? "danger" : status >= 400 ? "warning" : status >= 300 ? "info" : status >= 200 ? "success" : "muted";

/** A size as one reads it: 1.2 kB. */
export const humanSize = (bytes) =>
  bytes < 1024 ? `${bytes} B` : bytes < 1048576 ? `${(bytes / 1024).toFixed(1)} kB` : `${(bytes / 1048576).toFixed(1)} MB`;

/** A duration as one reads it: 84 ms, 1.42 s. */
export const humanTime = (ms) => (ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(2)} s`);

// — Copying it out ————————————————————————————————————————————————————————

const quote = (text) => (/^[\w@%+=:,./-]+$/.test(text) ? text : `'${text.replace(/'/g, `'\\''`)}'`);

export const asCurl = ({ method, url, headers, body }) => {
  let first = "curl";
  if (method === "HEAD") first += " -I";
  else if (method !== "GET" || body !== undefined) first += ` -X ${method}`;
  const parts = [`${first} ${quote(url)}`];
  for (const [key, value] of Object.entries(headers)) parts.push(`-H ${quote(`${key}: ${value}`)}`);
  if (body !== undefined) parts.push(`--data-raw ${quote(body)}`);
  return parts.join(" \\\n  ");
};

export const asHttpie = ({ method, url, headers, body }) => {
  const parts = [`http ${method} ${quote(url)}`];
  for (const [key, value] of Object.entries(headers)) parts.push(quote(`${key}:${value}`));
  if (body !== undefined) parts.push(`--raw ${quote(body)}`);
  return parts.join(" \\\n  ");
};

/** What the agent is handed: the request as it went, the answer as it came — the secrets masked. */
export const prompt = (intro, sent, result) => {
  const fence = (text) => {
    const longest = Math.max(0, ...(text.match(/`+/g) ?? []).map((run) => run.length));
    const marks = "`".repeat(Math.max(3, longest + 1));
    return `${marks}\n${text.endsWith("\n") ? text : `${text}\n`}${marks}\n`;
  };
  let out = `${intro}\n\n## Request\n`;
  let head = `${sent.method} ${sent.url}\n`;
  for (const [key, value] of Object.entries(sent.headers)) {
    head += `${key}: ${/^(authorization|cookie|x-api-key)$/i.test(key) ? "‹hidden›" : value}\n`;
  }
  if (sent.body !== undefined) head += `\n${sent.body.slice(0, 4000)}\n`;
  out += fence(head);
  out += "\n## Response\n";
  if (result.error) return `${out}${result.error}\n`;
  out += `${result.status} ${REASONS[result.status] ?? ""} — ${humanTime(result.ms)}, ${humanSize(result.size)}\n\n`;
  const body = result.text.length > 12000 ? `${result.text.slice(0, 12000)}\n… (${humanSize(result.size)} in all)` : result.text;
  out += fence(body);
  return out;
};
