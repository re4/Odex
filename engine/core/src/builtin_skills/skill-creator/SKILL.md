---
name: skill-creator
description: Create, update or validate an Odex skill (a folder with a SKILL.md that has name/description front matter). Use when the user asks for a new skill or wants to turn a workflow into one.
---

# Creating an Odex skill

A skill is a folder holding a `SKILL.md` file plus any helper files it needs. Odex lists every
enabled skill (name, description, path) in the system prompt; when a task matches a skill's
description, the agent reads its `SKILL.md` and follows it. Users can also invoke a skill with
`$name` or `@name` in the composer.

## Where skills live

| Scope | Folder | Notes |
|---|---|---|
| User | `~/.odex/skills/<name>/SKILL.md` | Available in every thread |
| Project | `<project>/.odex/skills/<name>/SKILL.md` | Only loaded when the project is trusted; commit it to share with the team |

Ask the user which scope they want when it is not obvious. Default to the project scope for
project-specific workflows (build, deploy, release steps) and the user scope for personal habits.

## Steps

1. Agree on the skill's job in one sentence and pick a short name: lowercase letters, digits and
   hyphens (`release-notes`, `db-migration`). The folder name must equal the `name` field.
2. Create the folder and write `SKILL.md`:

   ```markdown
   ---
   name: release-notes
   description: Draft release notes from the commits since the last tag. Use when asked for a changelog or release notes.
   ---

   # Release notes

   1. Find the last tag: `git describe --tags --abbrev=0`.
   2. List commits since it: `git log <tag>..HEAD --oneline --no-merges`.
   3. Group them under Features, Fixes and Other; skip chores.
   4. Write the result to `CHANGELOG.md` above the previous entry.
   ```

3. Put longer material in extra files next to `SKILL.md` and reference them by relative path:
   `scripts/` for helper scripts, `references/` for docs or examples, `templates/` for files to copy.
   Keep `SKILL.md` itself short (under ~150 lines) so it fits small context windows.
4. Validate (below), then tell the user where the skill is and how to invoke it.

## Writing a good skill

- The `description` is what decides whether the skill gets used: say what it does **and when to
  use it**, in one line (no line breaks), under ~200 characters.
- Write the body as numbered, imperative steps with the exact commands, paths and checks to run.
- State inputs the skill needs from the user and what "done" looks like.
- Prefer portable commands; note OS differences (PowerShell vs bash) when they matter.
- Never put secrets, tokens or personal data in a skill; read them from the environment instead.

## Validation checklist

- [ ] The file is named exactly `SKILL.md` and sits directly inside `<scope folder>/<name>/`.
- [ ] It starts with a `---` line, then `name:` and `description:` lines, then a closing `---` line.
- [ ] `name` matches the folder name and uses only `a-z`, `0-9` and `-`.
- [ ] `description` is a single non-empty line that says when to use the skill.
- [ ] Every file the body references exists (check relative paths with `list_dir`).
- [ ] No other skill in the same scope already has this name (list the scope folder first).

Re-read the file after writing it to check the front matter. New and edited skills show up in
Settings → Skills and are picked up by the next turn.

## Updating a skill

Read the current `SKILL.md`, change only what the user asked for, keep the `name` stable (renaming
breaks `$name` invocations) and re-run the checklist.
