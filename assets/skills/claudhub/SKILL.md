---
name: claudhub
description: Use when working inside a Claudhub worktree (the CLAUDHUB_WORKTREE environment variable is set) and the user asks to add, read or edit a note, a code review or a node on the Claudhub home screen, or asks what the environment is — branch, base, changes, other worktrees, open terminals. Explains where Claudhub's nodes live on disk and the exact file format.
---
<!-- claudhub-skill-version: 2 -->

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
