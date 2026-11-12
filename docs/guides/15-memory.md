# Memory

Playbook keeps one markdown store at `~/.config/playbook/memory/`, global and per project. It is local only and never committed. Playbook never writes Claude Code's own memory or settings.

`memory.source` is `both` (default) or `playbook`. `playbook memory claude import` copies Claude Code's notes for a project into playbook memory. It is read-only on the Claude side.

See [the memory system](../internals/13-memory-system.md) and [Decisions and memory](03-decisions-and-memory.md).
