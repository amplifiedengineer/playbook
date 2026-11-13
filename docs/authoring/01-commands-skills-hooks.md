# Authoring Commands, Skills, and Hooks

Three extension points let you add behavior to this config: slash commands, skills, and hooks. Each has a specific directory and a specific format. Templates below are derived from the real files.

## Slash commands

Commands live in `commands/<name>.md` and run as `/<name>` inside a session.

Frontmatter fields that appear in the existing commands:

| Field | Purpose |
|---|---|
| `description` | Both the picker summary and the trigger. Claude Code lists commands in the same available-skills listing it uses for `skills/`, matching this field against what the user asked for, so a command can load from a plain-English request with no slash typed. Write it as a "use when..." sentence, the same as a skill's. |
| `allowed-tools` | Comma-separated tools the command is permitted to use. |
| `effort` | Effort level: `low`, `medium`, `high`, `xhigh`, `max`. |
| `model` | Optional. Pin to a model (e.g., `opus`). Omit to inherit the session default. |
| `argument-hint` | Optional. Usage hint shown for `$ARGUMENTS`. |

The body is the instruction set Claude runs when the command is invoked. Write it as a numbered procedure or a set of rules. Reference `$ARGUMENTS` to access anything the user typed after the command name.

```markdown
---
description: One-line description of what this command does
allowed-tools: Bash, Read, Edit
effort: medium
---

# My Command

What it does and when to use it.

## Step 1

Instruction for Claude. Reference `$ARGUMENTS` here if needed.

## Step 2

Instruction for Claude.
```

`plan.md` adds `model: opus` because the planning interview needs that capability. Use an alias (`haiku`, `sonnet`, `opus`), never a model id. `commit.md` omits `model` and inherits the session default. Only set `model` when the command always needs a specific model.

A command's `description` does double duty. It is the one line a human reads in the picker, and it is the text Claude matches a plain-English request against when deciding whether to reach for the command. `adr.md` shows the shape: "Use when recording a significant, hard-to-reverse architectural decision." Name the situation, not the mechanics. The mechanics belong in the body, which only loads once the command actually runs. A description that is a feature list ("Executes X, delegating to Y, then Z") reads fine in the picker and matches nothing.

Weigh the cost of a wrong match before sharpening a trigger. A skill loads text and nothing else, so a false positive costs a few tokens. A command runs a procedure, sometimes a long interview or a write-and-commit pass, so a false positive costs the user's turn. For a heavyweight command, describe the precondition ("Use when an approved plan already exists") rather than the casual phrase that might have prompted it, and let `seed/prompts/SYSTEM_PROMPT.md` decide whether a match launches or offers. A description names its own trigger; the system prompt sets the policy for what happens on a match.

## Skills

Skills live in `skills/<name>/SKILL.md` and load on demand when the session decides a task matches.

A skill and a command must never share a name. Claude Code exposes every skill as `/playbook:<name>` too, so a same-named command and skill collide. `crates/plugin-checks/tests/name_collisions.rs` fails on the pair.

The frontmatter has two required fields and one optional field:

| Field | Purpose |
|---|---|
| `name` | Machine identifier used to reference the skill. |
| `description` | The trigger. Claude reads this to decide whether to load the skill. |
| `user-invocable` | Optional. `false` hides the skill from the `/` menu. The model and subagents can still load it. |

The body is the content Claude gets when the skill loads: rules, templates, formats, decision tables. Write it as self-contained prose because the skill loads without surrounding context.

```markdown
---
name: my-skill
description: Use when doing X or Y. Covers rule-set Z.
---

# My Skill

Rules and formats go here.

## Section

- Rule one.
- Rule two.
```

The `description` field does all the targeting. Write it as a "use when..." sentence that names the task clearly. A vague description means the skill loads at the wrong time or not at all.

### Keep helper skills out of the slash menu

Typing `/playbook` lists every skill. Only entry points a user would type belong there, such as `playbook-usage`. A helper that commands and subagents load, such as `grounding-review` or `engineering-standards`, sets `user-invocable: false`. Claude Code documents this field for skills: the skill leaves the `/` menu and the model can still load it. `crates/plugin-checks/tests/slash_menu_visibility.rs` fails when a skill is neither on its public list nor hidden, so a new helper does not leak into the menu.

Commands stay visible. The Claude Code docs state `user-invocable` for skills only, and hiding a command could break a path that runs one. The test also fails if a command sets the field. Hide commands only after a live session confirms the field works there.

## Names

A command file or a visible skill folder gets one name, and `/playbook:<name>` is what the user types. The name is one lowercase word (`review`, `plan`, `fix`) or a group and a word joined by one hyphen (`pr-create`, `pr-comments`). The group is `pr`, and it matches the binary command groups that the recipe drives, so a recipe reads like the commands it calls. A depth or a mode is a flag, not a second name: one `/playbook:review` takes `--quick` and `--deep`.

Names are flat. Claude Code turns `commands/pr/create.md` into a name with a second segment, so a subfolder changes what the user types. The plugin keeps every command file directly under `commands/`, and a check fails on a subfolder.

Hidden skills (`user-invocable: false`) are ids that the model and other prompts read, not names a user types. They follow the skill name rule in `skills_lint.rs` (lowercase letters, digits and hyphens, 64 characters at most) and keep their names unless a name misleads.

A renamed command or skill keeps its old name as a deprecated stub until 1.0.0. The stub sets `description: Deprecated, use /playbook:<new>. Removed in 1.0.0.`, sets `disable-model-invocation: true` so the model never picks it, copies the new file's `allowed-tools`, `argument-hint`, `model` and `effort`, prints one line that names the new command, then reads the new file and follows it with the same arguments. It stays under 12 lines. A removal with no replacement, such as the two review commands that became flags, has no stub. To rename or remove one:

1. Add a `RENAMES` row (with its stub) or a `REMOVED` entry in `crates/plugin-checks/tests/support/renames.rs`, and a row in `docs/renamed.md` with the release that removes the stub.
2. Change every caller to the new name. A recipe never calls the old name. The leftover check fails on the old name anywhere in `plugins/`, `seed/`, `README.md`, `CONTRIBUTING.md`, `docs/` and `crates/*/src`, except in its own stub, `CHANGELOG.md`, `docs/adr/` and `docs/renamed.md`. Name those files as code spans, not links.
3. Open the PR as `feat(plugin):` with a `Deprecated:` footer that lists the old and new names. The old name still works, so the PR is not breaking. The 1.0.0 removal of the stubs is `feat(plugin)!:` with a `BREAKING CHANGE:` footer. A removal with no stub is breaking at once.

## Calls between recipes and skills

A recipe is a command or a skill with steps. A rule skill holds knowledge: standards, evidence rules, style, routing tables. The catalog skill `playbook-usage` may name every command and nothing else. Five rules keep the call graph small:

- R1. A rule skill's `SKILL.md` has no steps that run tools or skills.
- R2. A recipe may load rule skills, dispatch agents and call the binary. It never invokes another recipe. To send the user on, write the pointer line "Tell the user to run `/playbook:<x>`".
- R3. A rule skill's `SKILL.md` never loads or invokes another skill. The recipe or agent that needs two rule skills loads both.
- R4. A reference file is a file, not a skill, so reading one runs nothing. A recipe may read reference files in any skill folder. A reference file follows the rule of the recipe that reads it: it may load rule skills, and it invokes no recipe except the rows on the list.
- R5. Chaining moves into the binary or into one command, not into a recipe that calls a recipe.

`crates/plugin-checks/tests/skill_call_graph.rs` scans every command, skill and agent file. In a command or skill file, a line that names a recipe counts as an invocation unless it is a pointer line or a reviewed `mention` row covers it, so a new edge cannot pass by leaving out a keyword. An agent file counts only when a line pairs a recipe name with an invoke verb, and no row allows that. The rows live in `crates/plugin-checks/tests/call_graph_allowlist.txt`. A found edge that is not listed fails, and so does a listed row the scan no longer finds. `scripts/check-size-budget-shrink.sh` fails a change that adds a row. Stubs and the catalog skill are skipped.

Every skill folder has one row in `crates/plugin-checks/tests/support/skill_kinds.rs`: `rule`, `catalog` or `recipe`. The rule skills, the catalog and the public skills in the slash menu are read from that list, and `skill_kinds.rs` fails a folder with no row, a row with no folder and a second recipe. Add a row when you add a skill. Only the catalog skill is visible.

## Hooks

Hooks are commands registered in `settings.json` under the `hooks` key. Each entry maps an event to one or more commands.

Events wired in this config: `SessionStart`, `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `PreCompact`, `Stop`, `SessionEnd`, `WorktreeCreate`, `WorktreeRemove`. `PreToolUse` and `PostToolUse` accept an optional `matcher` to filter by tool name (e.g., `"Bash"`, `"Read|Grep|Glob|Edit|Write|NotebookEdit"`).

### One binary, one module per hook

Every hook is a Rust function compiled into the single `playbook` binary, one module per hook under `crates/cli/src/hooks/<name>.rs`. `crates/cli/src/hooks/mod.rs` declares every module and an exhaustive `dispatch` match over the `HookName` enum in `crates/cli/src/lib.rs`, so adding a hook the CLI cannot invoke fails the build instead of silently doing nothing at runtime.

There is no language split left to choose between. The data-shaping hooks (memory graph rebuild, anchor lookup, frontmatter parsing) and the fast safety guards (`rm-workspace-guard`, `bg-await-guard`, `precommit-check`) are all plain functions sharing the same `crates/cli/src/common/` helpers (payload field extraction, session dir, atomic append, the `emit_*` JSON shapes) and the same compiled binary, so there is no per-hook cold start to weigh a language choice against.

**Registering a hook in `settings.json`:**

```json
"hooks": {
  "PreToolUse": [
    {
      "matcher": "Bash",
      "hooks": [
        {
          "type": "command",
          "command": "playbook hook rm-workspace-guard"
        }
      ]
    }
  ]
}
```

`crates/cli/src/init/wire.rs` writes this bare `playbook hook <name>` form for every hook when it generates `settings.json`: no absolute path to a script, since the binary is already on `PATH`.

**Input/output contract:**

Every hook receives a JSON payload on stdin. `main.rs` parses it once into a `Payload` and passes it to the dispatched module, which reads fields with `field`:

```rust
let tool = payload.field(".tool_name");            // PreToolUse: which tool fired
let path = payload.field(".tool_input.file_path"); // Read: the file being read
let source = payload.field(".source");              // SessionStart: "startup" or "resume"
```

To inject output back to Claude, write JSON to stdout:

```json
{
  "systemMessage": "Text shown to the user in the session.",
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "additionalContext": "Context injected into Claude's next turn."
  }
}
```

The `crates/cli/src/common/emit.rs` helpers (`emit_pre_context`, `emit_pre_deny`, `emit_prompt_context`, `emit_system_message`, `emit_block`) build this JSON for you. A hook must never break Claude Code, so swallow errors and emit nothing rather than panicking; `hooks::dispatch` is tested against malformed and missing-field payloads for every hook name to hold that guarantee.

**Minimal hook template:**

```rust
// SPDX-FileCopyrightText: 2026 Your Name
// SPDX-License-Identifier: Apache-2.0

use crate::common::emit_pre_context;
use crate::common::payload::Payload;

pub fn run(payload: &Payload) {
    let tool = payload.field(".tool_name");

    // Your logic here.

    // Emit context when needed.
    emit_pre_context("PreToolUse", "your message");
}
```

Wire it in by adding a `pub mod` line and a match arm in `crates/cli/src/hooks/mod.rs`'s `dispatch`, plus a variant on `HookName` in `crates/cli/src/lib.rs`. Both are exhaustive, so a missing arm fails the build rather than the hook silently doing nothing.

Hook and settings changes take effect on a fresh session, not a resumed one. After editing `settings.json` or rebuilding the binary, run `playbook cc fresh` (or plain `claude`). Resumed sessions run the config snapshot from their original startup; `playbook cc` warns you when the config has drifted.

## See also

- [Internals: Launcher and Hooks](../internals/01-launcher-and-hooks.md): the hook lifecycle and launcher internals.
- [Decisions and Memory](../guides/03-decisions-and-memory.md): authoring memory facts.
- [Docs index](../README.md)
