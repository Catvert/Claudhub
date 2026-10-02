---
name: claudhub
description: Use when working inside a Claudhub worktree (the CLAUDHUB_WORKTREE environment variable is set) and the user asks to add, read or edit a note, a code review or a node on the Claudhub home screen, to build or change a panel, a tab or the home of Claudhub's focus view (a script), or asks what the environment is — branch, base, changes, other worktrees, open terminals. Explains where Claudhub's nodes and scripts live on disk and their exact format.
---
<!-- claudhub-skill-version: 9 -->

# Claudhub

Claudhub is the window this worktree is open in. Its **home screen** shows
every worktree of the repository as a tree of **nodes**: a git node for the
repository, a card per worktree, the worktree's terminals — and **notes and
reviews, which are plain Markdown files** you can read and write.

## What you can see

- `$CLAUDHUB_WORKTREE` — the checkout you work in.
- `$CLAUDHUB_CONTEXT` — a Markdown file Claudhub rewrites as things change:
  branch, base, commits ahead, uncommitted changes, agents, terminals, the
  nodes already written, the repository's other worktrees. **Read it first**
  when asked about the environment. Never write to it.
- `$CLAUDHUB_TODO` — the worktree's task list (Markdown checkboxes). You may
  tick and add tasks.
- `$CLAUDHUB_SCRIPTS` — the folder of the focus view's scripts: see
  [Scripts](#scripts).

## Where nodes live

- **Shared** (the default): `$CLAUDHUB_WORKTREE/.claudhub/notes/`. Versioned
  with the code: it travels with the branch and teammates read it. Do not
  commit it unless asked — it shows in the user's changes like any file.
- **Private**: `$CLAUDHUB_NOTES_DIR/canvas/` — outside the repository. Use it
  only when the user says the note is for them alone.

One file per node. Name: `YYYY-MM-DD-HHMM-short-slug.md` (local time,
lower-case words joined by dashes). Never reuse an existing name. To change a
node, edit its file in place; keep the frontmatter keys you do not change.

## Format

Every node file starts with this frontmatter — flat `key: value` lines,
values quoted with `"…"` when they contain `: `:

```markdown
---
claudhub: note
anchor: worktree
title: Short title
author: <git config user.name>
agent: claude
created: 2026-09-24T21:30:00+02:00
---
The body, in Markdown.
```

- `claudhub:` is `note`, `review` or `diagram`. **Without it, Claudhub ignores the file.**
- `anchor:` is `worktree` (the node hangs from this worktree's card) or
  `repo` (it hangs from the repository's git node — for what concerns the
  whole project, not this branch).
- `title:` is optional for a note (its first line is used), required for a
  review.
- `agent: claude` says the node was written by you; `author:` is the human
  you work for (`git config user.name`).

## Code reviews

When asked for a code review, write **one** file with `claudhub: review`:

````markdown
---
claudhub: review
anchor: worktree
title: "Review: feature/x against main"
author: <git config user.name>
agent: claude
created: 2026-09-24T21:30:00+02:00
status: open
base: main
commit: <short sha of HEAD reviewed>
---
A short summary: what the change does, the overall verdict.

## Findings

### src/cache.rs:42
severity: warning
status: open

```rust
let key = format!("{}", id);
```

Why it is a problem, and what to do instead.

### src/api/routes.rs:108-112
severity: suggestion
status: open

…
````

- One `### path:line` (or `path:start-end`) heading per finding, the path
  relative to the repository root and the lines those of the reviewed commit.
- `severity:` is `blocker`, `warning`, `suggestion` or `question`.
- `status:` is `open`; the user resolves findings in Claudhub.
- Quote the lines you speak of in a fenced block under the heading: Claudhub
  uses the excerpt to find the lines again once the code has moved.
- Review what the branch changes against its base (`git diff <base>...HEAD`)
  unless told otherwise, and say so in the summary.

## Diagrams

A diagram is a picture shown as a node. Save it as **SVG** (PNG works too)
in the same folder as the node files, and write a node beside it:

```markdown
---
claudhub: diagram
anchor: worktree
title: Request flow
author: <git config user.name>
agent: claude
created: 2026-09-24T21:30:00+02:00
image: 2026-09-24-2130-request-flow.svg
---
A caption: what the diagram shows, and what it leaves out.
```

- `image:` is the picture's file name, relative to the node file's folder.
- An SVG saved there without a node is shown too, titled by its file name —
  but write the node: it carries the title, the author and the caption.
- Keep the SVG self-contained: no external fonts, images or scripts. Give it
  a `viewBox` and a sensible width and height; Claudhub scales it to its node.
- When a diagramming skill produces the drawing, save its SVG output here
  rather than only in a scratch folder.

## Scripts

The home screen's **focus view** shows one board per worktree, with tabs
(Home, Git, Tests, Notes, Terminals). A **script** adds a tab to every board,
or replaces the boards' Home. Scripts are JavaScript run by Claudhub's
embedded runtime (gpui-shell, QuickJS) — not a browser, not Node.

One folder per script in `$CLAUDHUB_SCRIPTS/<id>/`; `<id>` is lower-case
letters, digits, `-`, `_`, `.`. It holds:

- `claudhub.json` — `{"title": "…", "kind": "tab" | "home", "icon": "…",
  "entry": "main.js", "description": "…", "permissions": {…}}`. Only the
  file is required; the title defaults to the id, the kind to `tab`, the
  entry to `main.js`. A title or description can be `{"fr": "…", "en": "…"}`.
  `icon` is a Lucide name. `permissions` declares the secrets it keeps:
  `{"secrets": [{"name": "token", "label": "API token"}]}`. The network
  needs no declaring. `"version": "1.0.0"` is the script's own, shown to
  the user; `"claudhub": "0.16.0"` is the oldest Claudhub it runs on —
  an older one lists the script as not loadable and says why. Set it when
  the script uses something of the `claudhub` module that is new.
- the entry, an ES module whose default export is a class extending `View`.

Saving any file reloads the script on every board within a second. If it
fails to load, the previous version stays on screen and Claudhub shows the
error in a bubble. **After each save, wait a second and read
`$CLAUDHUB_SCRIPTS/.status.md`**: Claudhub rewrites it with each script's
state — loaded, or the load error in full. A script is loaded only where it
is shown; the user's **Plugins** screen (foot of the home screen's sidebar)
previews the selected one, and the status names it. Never write
`.status.md` nor `gpui-kit.d.ts` yourself. A `home` script is used once
the user picks it in Settings → Scripts. Claudhub writes `gpui-kit.d.ts`
beside the entry at each load: **read it** for the exact element, style and
module signatures before writing code.

```js
import { View, div } from "gpui-kit";
import { h_flex, v_flex, Button, Checkbox } from "gpui-base";
import { worktree, tasks, open_tab, BranchCard } from "claudhub";

export default class Example extends View {
  init(props, cx) { this.expanded = false; }   // once; no constructor
  render(cx) {                                  // returns one element
    const colors = cx.theme().colors;          // follows the user's theme
    return v_flex().size_full().gap(12).overflow_y_scrollbar()
      .child(div().text_size(16).font_semibold().child(worktree().name))
      .child(BranchCard.new("branch"))          // a card Claudhub paints
      .child(Button.new("git").child("Git").on_click(() => open_tab("git")));
  }
}
```

- Builders are GPUI's, in `snake_case`: `.gap(8)`, `.p(12)`, `.text_size(12)`,
  `.bg(colors.surface)`, `.border(1)`, `.rounded(8)`, `.flex_1()`, `.min_w_0()`,
  `.when(cond, el => …)`, `.child(x)`, `.children([…])`. Colors:
  `colors.background`, `foreground`, `surface`, `muted`, `muted_foreground`,
  `primary`, `destructive`, `border`.
- State is plain fields on `this`; after changing one in an event, call
  `cx.notify()`. There are no hooks and no signals.
- The `claudhub` module answers for the board's worktree. It **reads**
  `worktree()`, `changes()`, `tasks()`, `terminals()`, `scripts()`,
  `language()` — Claudhub runs `render` again when they may have changed —
  and **acts**: `open_tab(name)`, `open_script(id)`,
  `toggle_task(line, done)`, `open_terminal()`, `send_to_agent(text)`,
  `notify(text)`.
- Its **components** are Claudhub's own pieces, to rearrange rather than
  rewrite: `BranchCard`, `PullRequestCard`, `ChangesCard`, `ReviewCard`,
  `TasksCard`, `NoteCard`, `RunCard`, `Terminals`, `DefaultHome`, `GitTab`,
  `TestsTab`, `NotesTab`, `TerminalsTab` — each `X.new("an-id")`. Use each
  tab at most once per script.
- There are **no dialogs, toasts or tooltips** in a script: say things with
  `notify(text)` or in the view itself.
- **Network**: the standard `fetch(url, {method, headers, body})` reaches
  any address over HTTP or HTTPS, any port. Call it inside
  `cx.spawn(async (cx) => { …; cx.notify(); })`.
- **Storage**: `storage_get(key)`, `storage_set(key, json)`,
  `storage_remove(key)`, `storage_keys()` — this script's data, kept across
  restarts, shared by its views on every board. Key per-repository data by
  `worktree().repository`. There is no `localStorage`.
- **Secrets**: `await secret(name)` (null when unset), `await
  set_secret(name, value)`, `await delete_secret(name)` — the system
  keyring, per script. Declare them in `permissions.secrets`; never write a
  secret to storage, to a file or to `notify`.
- Also: `open_url(url)` (http/https), `copy_text(text)`.
- **Pieces Claudhub draws for you**, to match its own panels: `CodeBlock`
  (`{path, lines: [{line, text}], mark}` — coloured by the path's language,
  numbered, one line marked), `Icon` (`{name, size, tone}`, Claudhub's
  Lucide set), `Badge` (`{text, tone}`), `ShareBar` (`{label, share}`).
  Tones are names: `danger`, `warning`, `info`, `success`, `primary`,
  `muted`, `foreground`.
- **Gestures**: `locate(path)` brings a server's path back to a file of
  the worktree (null when none); `open_file(path, line)` opens it in the
  editor; `ask_agent(text)` shows a prompt in the dialog where the user
  completes it before it goes to the agent; `relative_time(iso)` says an
  instant as the interface does.
- No file, process or raw socket access, and never git or shell commands:
  what a script knows of the worktree comes from the module; ask the user,
  or use `send_to_agent`.
- **Builtins** ship with Claudhub in `$CLAUDHUB_BUILTIN_SCRIPTS/<id>/`
  (`sentry`, `http-client`) and are rewritten at each update: never edit
  them there. To change one, **fork** it — copy its folder into
  `$CLAUDHUB_SCRIPTS/` under the same id (the Plugins screen's Fork button
  does it): the copy replaces the builtin, keeps its storage and secrets,
  and no longer follows updates. `scripts()` says each one's `origin`
  (`builtin`, `user`, `fork`) and whether the user has it `enabled`.
- `$CLAUDHUB_BUILTIN_SCRIPTS/sentry` is a complete example: settings in
  storage, its token in the keyring, `fetch` to Sentry's API, a list and a
  detail.
- Keep `render` cheap — it runs on every refresh. **A long list is
  virtual**: `v_virtual_list(id, count, height, (i) => key, (range) =>
  rows)` from `gpui-base`, `.size_full()`, with `Scrollbar.vertical(id)
  .absolute().inset_0()` beside it in a `.relative()` parent. Rows hold no
  handler: `.on_item_click((key, cx) => …)` on the list. Give every row the
  same explicit `.h(height)`; `sizes()` gives Claudhub's (`row`, `line`).
  Code in a virtual list is one `CodeLine` per row. Claudhub smooths the
  wheel of every list and scroll area. Do not animate.
- **Colour**: `cx.theme().colors` has no warning, info or success tone;
  `palette()` gives Claudhub's as `#rrggbb` — append two hex digits for a
  tint (`palette().danger + "26"`).
- `$CLAUDHUB_SCRIPTS/home-columns` and `$CLAUDHUB_SCRIPTS/dashboard` are
  examples written at first launch: a home rearranged from the cards, and a
  tab built from the module's data.

