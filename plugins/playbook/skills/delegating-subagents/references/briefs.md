# Briefs and forks

Why `SKILL.md` says to pass a `subagent_type` and write a stand-alone brief, and how each place playbook spawns a subagent is classified.

## Contents

- Fork cost
- Classification of spawn sites
- Brief template
- Return cap
- Review brief builder
- Implementer brief builder
- Writer brief builder
- Auditor brief
- Checks before dispatch

## Fork cost

An `Agent` call that omits `subagent_type` forks the parent: the child inherits the whole conversation. Over 30 days on one machine, 117 such forks averaged 372K tokens of first-turn context and cost $2.90 each, about $339 in total. The same work on a fresh agent with a brief starts at a few thousand tokens. Forks were not stalling or returning less; the cost is the inherited history, nothing else.

`context: fork` in a command's frontmatter is a different mechanism. It runs the command body in the named agent without the parent history, so it is already a fresh start and stays.

## Classification of spawn sites

| Site | Needs parent history? | Mechanism |
|---|---|---|
| `audit` | No | `context: fork`, `agent: auditor` (fresh) |
| `review` (both passes), `implement` Step 9 reviewers, `review-triage`, `cheap-checker` | No, diff and excerpts go in the brief | `subagent_type: playbook:<agent>` |
| `implement` implementers | No, the Work Unit brief is self-contained | `subagent_type: playbook:implementer` |
| `review` comment drafts | No, the swept facts go in the brief | `subagent_type: playbook:writer` |
| `plan`, `adr` critic, fact-checker, test-reviewer | No, they get the full artifact | `subagent_type: playbook:<agent>` |
| `plan` Step 2 and discovery agents | No, they get a question and paths | `subagent_type: Explore` or `general-purpose` |
| `learn` collectors and analysts | No | `subagent_type: playbook:<agent>` |
| Ad hoc fan-out from the system prompt | No by default | an explicit `subagent_type`, never omitted |

Nothing in this table needs history. A fork is justified only when a child must act on corrections the user gave earlier in the session and the corrections cannot be stated in the brief in a few lines.

## Brief template

```
Goal: <one sentence>
Inputs: <absolute paths with line ranges>
Output: write your full report to <absolute path>
Constraints: <scope, do not touch, read-only or not>
Return: status, findings as file:line, output path; at most about 300 words
```

Agents that cannot write (the read-only roster rows) have no `Output` line; they return their findings instead, and the orchestrator treats silence as NOT RUN.

## Return cap

A subagent's final message lands in the parent context and is re-read on every later turn. Capping it at about 300 words, as status, findings with `file:line` and the output file path, keeps that growth small. The cap is in each agent's prompt and in the `Return` line of the brief. It limits the message only: the report file is never capped, and a reviewer shortens findings before it drops any (blocking and important findings always stay, so recall is unaffected). Read the output file for detail instead of polling.

## Review brief builder

Reviewers (`reviewer`, `cheap-checker`) used to spend about 14 messages per run reading files the orchestrator had already read. The orchestrator builds the brief once per review run and every reviewer reuses it. A brief for a review MUST contain, in this order:

1. **Diff.** The full unified diff, or the hunks for the lens's files when the lens is narrow.
2. **Excerpts.** For each changed file, the changed ranges plus about 30 lines either side, with line numbers, under a `File <path>` header. Take the ranges from the `@@ -a,b +c,d @@` headers and merge overlapping ones. Number the lines from the file at `HEAD_SHA`:

   ```bash
   git -C "$WT" show "$HEAD_SHA:$path" | nl -ba -w1 -s': ' | sed -n "${start},${end}p"
   ```

   Cap one file at 400 numbered lines. For a larger file, send the hunks with their numbers and name the file as "Read on demand".
3. **Test command.** The scoped verify command, and the `CHECK_OUTPUT` the orchestrator already captured. Reviewers read it as context and never re-run it.
4. **Limit.** The sentence: "Everything needed to judge this change is above: the diff and the numbered excerpts. Read or search beyond it only when a claim cannot be settled from the excerpt (a caller, a definition, a test). Do not browse the repository."

**Callers, only when the user gave a token budget.** Reviewers have no Bash. With no budget, add no code facts to any brief. With one, when `playbook code status` reports `fresh`, after `playbook code index` when it said stale or missing, add the `playbook code refs <name>` output for each public name the diff changes, under a `Callers` header, ranked by the order of the changed names and cut from the last line to that many tokens (four bytes to a token). `playbook plan brief --facts-tokens <N>` writes the same block into implementer briefs.

The lens, the output shape and the voice rules stay exactly as the command specifies. The limit narrows where the reviewer looks for evidence, never what it must look for: every lens still covers its whole scope, and a reviewer that needs a caller or a test still reads it.

Measured on the seeded-defect fixture (`crates/cli/tests/fixtures/bench/reviewer-real-diffs.json`, 7 seeded bugs and 3 clean cases, 3 repeats each, Opus medium, tools on, checkout present): recall 21 of 21 before and after, clean cases 9 of 9 before and after, 3.4 to 2.2 turns per run, $0.0447 to $0.0395 per run. The fixture files are small, so the saving on a real diff is larger.

## Implementer brief builder

An implementer brief is the per-Work-Unit file. Intent: work-unit-briefs. If the session's tool map (printed at session start) lists it, use that command to write it. Otherwise do this: write it by hand from the plan's Work Unit (its files, changes, test scenarios and done-when). Either way it also carries: the excerpts, with line numbers, of the files the step edits and of the one or two neighbours it must match (an interface it implements, the test it extends), the scoped verify command, and the limit: "The excerpts above are the context you need. Read a file only to edit it or when the excerpt cannot settle a question. Do not explore the repository." The Edit tool needs the file read first, so a file the step changes is still read; the saving is the exploratory reads around it. Take the excerpts from `playbook code context "<Work Unit title>"`, trimmed to the step's files, when the code search rule finds an index; else from Read.

## Writer brief builder

The writer turns decided facts into the final text. The orchestrator has done the analysis and sends no draft. A writer brief MUST contain, in this order:

1. **Kind.** One of `pr-body`, `review-comment`, `reply`, `commit-message`, `ticket`.
2. **Facts.** Only the values the text may use: the label and decorations, the failure in at most 12 words, the fix in at most 12 words, the evidence as `path:line`, ticket keys, numbers. Nothing else may appear in the text.
3. **Reference.** The absolute path of the one reference for the kind, resolved by the orchestrator (the writer has no Bash): `github-review-comments.md` for a review comment or reply, `commits-and-prs.md` for a PR body, `conventional-commits.md` for a commit message.
4. **Return.** "Return only the text. If a fact is missing, return one line `NEED: <fact>`."

Save the same facts, with a first line `kind: <kind>`, as `<file>.facts` next to the returned text, then run `playbook prose check <file>`. It reports a number, identifier, path or reference that the facts do not hold, a text over its `LEN-*` limit and a passive voice hit.

**Fallback template, no model.** When the check still fails after one resend, write the text by hand:

| Kind | Template |
|---|---|
| `review-comment` | `<label>[ (<decorations>)]: <failure>. <fix>.` Leave out the parentheses when there are no decorations. |
| `reply` | `Fixed.` or `Done, <what changed>.` or `<reason>.` |
| `commit-message` | `<type>(<scope>): <fact one line>` |
| `ticket` | one sentence of what and why, then the acceptance bullets as given |

Measured in the pilot of 18 cases (Sonnet writing from the old rules against Haiku writing from the new rules): total length 2934 against 2856 characters, 0 invented facts for both, and the Haiku texts passed the length limits more often (2 over against 6). The strong model's output only shrinks when it stops writing the text: a decision record for a reply was longer than the reply, so replies stay with the orchestrator.

## Auditor brief

`audit` runs in the `auditor` agent, which has Bash. Its Phase 1 starts with the code search rule: `playbook code status`, then `playbook code index` when it says stale or missing, then `playbook code context` and `playbook code outline` to map the core modules before reading files. Grep and Glob only when the binary or the index is unavailable.

## Checks before dispatch

- Is `subagent_type` set, with the `playbook:` prefix for plugin agents?
- Could the child do this task if it saw only the brief? If not, add what it lacks, by path first.
- Is the output file named, for an agent that can write?
- Does the agent run shell commands, and did the session enter a worktree? Then set `isolation: worktree` on the spawn and name the base branch and delivery path in the brief (not for the implementer, which works in the session checkout), or call `ExitWorktree` with `keep` first (see "Worktree guard" in `SKILL.md`).
- For a reviewer or implementer: are the diff or excerpts, the test command and the limit sentence in the brief?
