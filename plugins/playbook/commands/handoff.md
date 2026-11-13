---
description: Use at the end of a session, before /clear or compaction, or when asked to wrap up, to save a handoff. With `load`, use at the start of a session after /clear, or when the saved handoff did not load.
allowed-tools: Bash, Read
argument-hint: "[path | load [--all]]"
model: haiku
effort: low
---

# Handoff

One command for session handoffs. If `$ARGUMENTS` starts with `load`, follow "Load". Otherwise follow "Save". Run every step now and do not ask for confirmation.

## Save

Write a handoff for the next session. The facts come from git. You write only the judgment parts.

1. Read the rules and template. Intent: handoff-template. If the session's tool map (printed at session start) lists it, use that command. If the tool map lacks it, follow `without-binary.md#handoff-template` in `skills/delegating-subagents/references/without-binary.md` in this plugin.
2. Gather the facts for "Where we are now". Intent: handoff-facts. If the session's tool map lists it, use that command (point it at `<path>` when step 5 saves for another directory). If the tool map lacks it, follow `without-binary.md#handoff-facts` in the same file.
3. Write the handoff following the template. Decisions need a reason, next steps need a why-now, and open questions name what surfaced them.
4. Check the shape. Intent: handoff-check. If the session's tool map lists it, use that command, feeding the document through a quoted heredoc (`<<'HANDOFF_EOF'`), never an unquoted heredoc or `echo`, because the facts hold repo text. If the tool map lacks it, follow `without-binary.md#handoff-check` in the same file. Fix every problem and check again.
5. Save it. Intent: handoff-save. If the session's tool map lists it, use that command with the same quoted heredoc; when a context-size nudge gave you a session id, pass it, and pass `<path>` to save for another directory. If the tool map lacks it, follow `without-binary.md#handoff-save` in the same file.
6. If `$ARGUMENTS` holds a path, also write the document there, creating parent directories, and confirm the path in one line. A bare filename goes in the current working directory. Never write to a path the user did not provide.

Print the handoff, then end with one line: tell the user to run `/clear` and `/playbook:handoff load` to load it. When the session-start hook is installed, the next session loads it on its own after `/clear` and shows a `Loaded handoff:` line.

## Load

Pick up where the last session in this directory stopped. When the session-start hook is installed, it loads the handoff on its own after `/clear`. Run this when no handoff appeared, or to read it again.

1. Load the handoff without deleting it. Intent: handoff-load. If the session's tool map lists it, use that command, with `--all` when `$ARGUMENTS` has it. A handoff the hook already loaded still prints, labelled "already loaded by the last session start". If the tool map lacks it, follow `without-binary.md#handoff-load` in the same file.
2. If nothing was found, say `No handoff saved for this directory` in one line and stop. When the folder holds handoffs for other directories, name the one that looks closest and tell the user to `cd` into that directory and run this command again.
3. Orient in three to five lines: where we are, the decisions that still matter, and the next steps in order. Use only what the handoff says.
4. Check it against reality with `git branch --show-current` and `git status --short`. Compare the branch and the working tree with the handoff's "Where we are now". Name each difference in one line. Do not fix anything.
5. Finish with the first next step as the proposed action. Do not start it.
