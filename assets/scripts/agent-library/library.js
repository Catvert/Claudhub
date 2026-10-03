// What the library decides without drawing: which skills a filter keeps, how
// many each scope holds, where a shared skill lands.

/** The scopes, in the order the list reads them: what travels with the repository first. */
export const SCOPES = ["project", "personal", "plugin"];

/** The tone each scope is told by. */
export const SCOPE_TONES = { project: "success", personal: "info", plugin: "primary" };

/** The icon each scope is told by. */
export const SCOPE_ICONS = { project: "git-branch", personal: "user", plugin: "sparkles" };

/** Where the project keeps a skill for its agent — `crate::library::Reader::project_folder`. */
export const projectFolder = (reader) => (reader === "codex" ? ".agents/skills" : ".claude/skills");

/**
 * Whether a skill answers what is typed: every word somewhere in its name,
 * its description or its plugin — case ignored, as the panels' filters do
 * when the words are in lower case.
 */
export const matches = (skill, needle) => {
  const words = needle.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return true;
  const haystack = `${skill.name} ${skill.description} ${skill.plugin ?? ""}`.toLowerCase();
  return words.every((word) => haystack.includes(word));
};

/** Where the project keeps a skill's companion prompt — `crate::library::prompt_file`. */
export const promptFile = (name) => {
  const slug = name
    .trim()
    .replace(/[^\p{L}\p{N}_.-]/gu, "-")
    .replace(/^[.-]+|[.-]+$/g, "");
  return `.claudhub/prompts/${slug}.md`;
};

/** What a companion prompt may name, filled in at the launch from the worktree it is launched on. */
export const VARIABLES = ["branch", "worktree", "path", "repository"];

/** A companion prompt, its `{{variables}}` filled in; an unknown one is left as written. */
export const expand = (text, tree) => {
  const values = {
    branch: tree.branch ?? "",
    worktree: tree.name,
    path: tree.path,
    repository: tree.repository ?? tree.path,
  };
  return text.replace(/\{\{\s*(\w+)\s*\}\}/g, (whole, name) => (name in values ? values[name] : whole));
};

/** How many skills each scope holds. */
export const countByScope = (skills) => {
  const counts = { project: 0, personal: 0, plugin: 0 };
  for (const skill of skills) counts[skill.scope] = (counts[skill.scope] ?? 0) + 1;
  return counts;
};

/** The skills on show: the scope chosen — `null` for all —, then the filter. */
export const shown = (skills, scope, needle) =>
  skills.filter((skill) => (scope === null || skill.scope === scope) && matches(skill, needle));

/** The key of the project's prompt among a skill's prompts; a personal one is keyed by its id. */
export const PROJECT = "project";

/**
 * The personal prompts of a skill, by its name, in the plugin's data —
 * `{ [name]: [{ id, text }] }`: on this machine only, for every skill of that
 * name whatever the project.
 */
export const personalOf = (kept, name) => (Array.isArray(kept?.[name]) ? kept[name] : []);

/** The data with skill `name`'s personal prompts replaced — dropped when none is left. */
export const withPersonal = (kept, name, list) => {
  const next = { ...(kept ?? {}) };
  if (list.length === 0) delete next[name];
  else next[name] = list;
  return next;
};

/** A new personal prompt's id: never one the list holds. */
export const newPromptId = (list) => {
  let id = Date.now().toString(36);
  while (list.some((prompt) => prompt.id === id)) id += "x";
  return id;
};

/** What a prompt's chip says: its first line, cut. */
export const labelOf = (text, untitled) => {
  const line = text.trim().split("\n")[0].trim();
  if (line === "") return untitled;
  return line.length > 28 ? `${line.slice(0, 27)}…` : line;
};

/** The prompt saved under `key` for a skill: the project's, or one of the personal ones. */
export const savedOf = (skill, personal, key) =>
  key === PROJECT ? skill.prompt : (personal.find((prompt) => prompt.id === key)?.text ?? "");
