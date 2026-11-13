# Headless mode

Can playbook run with no person at the keyboard, for example in CI? Yes. Tested on 2026-10-03 with playbook 0.16.0 and Claude Code 2.1.286, in a scratch `HOME` and a scratch git repo with no remote. It was not rerun for 0.20.0. The `PLAYBOOK_HEADLESS` switch and the auto mode rules below are read from the current code.

## Short answer

- The compiled `playbook` binary already works unattended. No command read stdin or waited for input.
- No model is needed for useful CI checks: `agents check`, `manifest check`, `settings check`, and the `gate` commands.
- Slash commands also run under `claude -p` with `--plugin-dir`, and playbook's hooks fire.
- The risks are around the model: a denied permission is not an error exit, the memory Stop hook can add turns, and any command that asks a question has no one to answer.

## What works without a model

All run with `CI=true`, stdin from `/dev/null`, and an empty `HOME`.

| Command | Unattended | Notes |
|---|---|---|
| `playbook --version` | yes | exit 0, one line |
| `agents check` | yes | needs a checkout (`agents/`); exit 1 outside one |
| `manifest check .` | yes | exit 0 on a clean tree |
| `settings check <template> <root>` | yes | two required arguments |
| `ci [--json] [--dir <path>]` | yes | runs the three checks above in one step: one line each, exit 1 if any fails; outside a playbook checkout every check is skipped and it exits 0 |
| `gate record` and `gate check` | yes | need `--source <plan file>` and a git `origin` remote; no remote exits 1 with a clear message; an edited plan reads `STALE`; a missing phase reads `MISSING`, exit 1 |
| `usage` | yes | no transcripts: exit 0, "No usage recorded yet" |
| `handoff save`, `handoff load`, `handoff status` | yes | `save` reads stdin and refuses empty input |
| `trust <path>` | yes | always exit 0 |
| `pr prepare --help` | yes | exit 0, so a caller can detect support |
| `setup` | yes | no prompt; second run changes nothing; with no plugin root it wires hooks and skips the rest |

Nothing needed anything from `~/.claude` except `setup`, which writes `~/.claude/settings.json`. State created under an empty `HOME`: `data/` (`config.db`, `usage.db`, and `data/gate/<owner>/<repo>/<worktree>.db` for the gate), `runtime/` (handoffs, session folders, `session-start.log`), and `memory/`.

## Running Claude headless

Flags checked against `claude --help`: `-p`, `--output-format json|stream-json`, `--max-turns`, `--max-budget-usd`, `--allowedTools`, `--permission-mode`, `--permission-prompts`, `--plugin-dir`, `--setting-sources`, `--settings`, `--no-session-persistence`, `--session-id`.

Real exit codes and result fields (all with haiku, each call capped at 10 to 15 cents):

| Case | Exit | Result fields |
|---|---|---|
| Normal run | 0 | `is_error:false`, `subtype:success` |
| Budget exceeded (`--max-budget-usd 0.0001`) | 1 | `is_error:true`, `subtype:error_max_budget_usd`, `terminal_reason:budget_exhausted` |
| `--max-turns 1` on a prompt needing a tool | 1 | `subtype:error_max_turns`, `terminal_reason:max_turns` |
| A write the permissions deny | **0** | `is_error:false`; the denial is only in the `permission_denials` array |
| Not logged in | 1 | `result` is "Not logged in" |

The docs say a permission denial is a failure exit. It is not: a CI wrapper must read `permission_denials` and fail on a non-empty list.

Other facts:

- `--allowedTools` takes several values, so it swallows a prompt placed after it. Put the prompt first.
- `AskUserQuestion` does not exist under `-p` (the model said it has no such tool), with or without `--permission-prompts none`. The model falls back to asking in plain text, which no one reads, and the run still exits 0. A command that must ask has no headless path.
- Under a scratch `HOME`, `claude -p` says "Not logged in". Credentials live in the real `HOME`, so a runner needs `ANTHROPIC_API_KEY` or an OAuth token.
- `--bare` is excluded by decision: it skips hooks, plugins, skills, and keychain reads, and authenticates only with `ANTHROPIC_API_KEY`, so it would switch off the playbook plugin and its hooks. Design for a normal `claude -p` run with hooks on, the plugin loaded by `--plugin-dir`, and auth by `ANTHROPIC_API_KEY` or a subscription token (`CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token`).

## Plugin and hooks under `-p`

- `--plugin-dir <checkout>` loads playbook with no marketplace install. The binary finds plugin files through `playbook path plugin-root`, which prefers the installed plugin. Set `PLAYBOOK_PLUGIN_DIR=<checkout>/plugins/playbook` when a marketplace install is also present. `system/init` in `stream-json` listed it as `playbook@inline`, version 0.16.0, with 23 playbook slash commands.
- `claude -p "/playbook:handoff load"` ran the command: it called `playbook handoff load`, read the log, and checked git (4 turns, 14 cents).
- Hooks that fired: SessionStart (`session-init`, source `startup`), UserPromptSubmit (`auto-model-detect`, `memory-anchors`), PreToolUse (`preread-*`, `bg-await-guard`, `memory-anchors`), PostToolUse (`search-counter`, `post-edit-track`), Stop (`memory-capture`, `session-clean-exit`). The `statusLine` never renders.
- Folder trust: runs happened in a fresh scratch directory with no trust entry, and the project hooks and plugin still ran. `playbook trust` is not needed for CI.

### The memory Stop hook

`memory-capture` blocks Stop only when a `capture-due` marker exists. That marker is written by the status line renderer (`playbook statusline`), which does not run headless, so in normal headless runs it never fires (checked: zero markers after four runs).

Worst case, tested with a marker created for a fixed `--session-id`: the hook blocked Stop twice, then released (the re-block cap is 2). The run took 3 turns instead of 1, cost about 2 cents, exited 0, and did not loop or hang. One side effect: the final `result` text was the model's answer to the nudge, not to the prompt. Do not trust `result` as the answer when this hook may fire.

## The headless switch

One switch covers every row below. `PLAYBOOK_HEADLESS` set to `1`, `true`, `yes`, or `on` turns headless mode on, and `0`, `false`, `no`, or `off` forces it off. When it is unset, `CI=true` (or `CI=1`) turns it on, so a developer who runs with `CI=true` locally can opt out with `PLAYBOOK_HEADLESS=0`. The check lives in `crates/cli/src/common/headless.rs`.

| Hook | Behavior when headless |
|---|---|
| `session-init` | Writes `session-start.log`. Skips every nudge, the worktree sweep, and the handoff load. Skipping the auto-learn nudge has the same effect as `AUTO_LEARN_NUDGE=0`. Injects no memory unless `PLAYBOOK_HEADLESS_MEMORY=1` opts it in. Still injects the tool map (see `playbook tools map`). |
| `memory-capture` (Stop) | Never blocks, even if a `capture-due` marker exists. |
| `auto-model-detect` | Silent headless. |
| Safety guards (`preread-*`, `bg-await-guard`, `rm-workspace-guard`, `precommit-check`) | Fire, stay quiet, and behave exactly as they do interactively. They are the reason to run with hooks on. |
| `search-counter`, `post-edit-track`, `session-clean-exit` | Write small files under `runtime/`. Harmless. |

No hook reads a TTY or prompts. Only `crates/cli/src/main.rs` reads stdin, for the hook payload.

Interactive runs keep the bounded re-block from ADR 0009: `memory-capture` blocks at most twice, the second block escalates the handoff nudge, then it releases. It cannot loop. It deliberately does not read `stop_hook_active`, because that field would end the escalation after the first block.

## Auto mode and unattended runs

Design only; not part of the tested runs above.

`PLAYBOOK_HEADLESS` is independent of the `mode` setting, which decides whether a command may ask questions. Headless only quiets the session nudges and the memory Stop hook, and never turns auto on. Auto never turns headless on either. Under `claude -p` the question tool is absent, so set `PLAYBOOK_MODE=auto` as well when a command must run with no one to answer. See [Auto mode](../guides/04-auto-mode.md) for the setting, the spend cap, and the limits.

### Supported in auto

These commands read the mode first and take the recommended answer instead of asking. Each records those answers in an Assumptions list in its final output.

- `/playbook:plan`: answers the convergent phase (Work Units and Segments) on its own. Plain auto stops at the design approval. Only `--auto-design` approves a design.
- `/playbook:implement` and `/playbook:fix`: pick the recommended delivery options and run to a pull request.
- `/playbook:commit` and `/playbook:pr-create`: run end to end.
- `/playbook:review` reviews this PR only when the PR sits in a stack, takes its own recommendation for that scope, and passes `--auto` on to `/playbook:review --quick` or `/playbook:review --deep`. Those reviews still ask what to post unless the caller passes `--self` or `--no-post`.
- `/playbook:learn`: runs as `--stage`, so candidates go to the staging area and nothing reaches the live memory store.
- `/playbook:audit`, `/playbook:doctor`, and `/playbook:handoff load`: behave the same in either mode.

### Refused in auto

`/playbook:setup`, `/playbook:adr`, and `/playbook:pr-comments` stop with one line. Each needs a person: setup changes your global settings and shell files, an ADR records a decision someone has to own, and replies on GitHub speak in your name.

### Never done in auto

- Auto never force-pushes and never uses a forced lease. A push that needs a force is parked and reported.
- `/playbook:implement` never picks the `land` boundary on its own. `land` merges each Segment with no human review, so it runs only with an explicit `--boundary=land`.
- Reviews are never posted.

### Still unsafe unattended

- Anything that merges, such as `gh pr merge`, or `/playbook:implement` with `--boundary=land`.
- A model that reads untrusted pull request text (titles, bodies, comments, diffs from forks) with write or shell tools. That text is prompt injection input.

In ask mode, these commands still ask a person:

| Command | Asks |
|---|---|
| `implement` | delivery questions unless flags preset them |
| `plan` | the divergent phase |
| `adr`, `setup`, `pr-comments` | still ask a person |
| `review` | the scope when the PR is in a stack (unless `--this-pr` or `--whole-stack` is passed), then quick or deep (unless `--quick` or `--deep` is passed) |
| `review --quick` | how to submit |
| `review --deep` | which findings to post |
| `learn` | if the Jira or Confluence target is ambiguous |
| `fix` | confirms before commit |

These run without questions: `commit`, `pr-create`, `handoff`, `doctor`, `audit`.

## GitHub Actions

The documented action is `anthropics/claude-code-action@v1`. It takes `anthropic_api_key` or `claude_code_oauth_token`, and `plugin_marketplaces`, `plugins`, and `claude_args`. It needs a write-access actor, and pull requests from forks get no secrets. Not run here.

First use case, no model, proven locally (checksum check passes, a wrong checksum fails, each check exits 0, nothing was written to `HOME`). This snippet is documentation, not a workflow in this repo:

```yaml
jobs:
  playbook-checks:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install playbook
        run: |
          V=v<version>; A=playbook-<version>-x86_64-unknown-linux-musl
          gh release download "$V" --repo amplifiedengineer/playbook --pattern "$A" --pattern SHA256SUMS
          grep " $A\$" SHA256SUMS | sha256sum -c -
          install -m 0755 "$A" /usr/local/bin/playbook
        env:
          GH_TOKEN: ${{ github.token }}
      - run: playbook ci
```

`playbook ci` prints `PASS`, `FAIL`, or `SKIP` with a reason for each check, then `ci: N passed, M failed, K skipped`. Add `--strict` to fail the run when any check is skipped, so a job pointed at the wrong directory cannot pass by checking nothing. A check whose inputs are missing is skipped, so the step is safe in any repository. State between runs: these checks create nothing. A gate check needs the gate database under `~/.config/playbook/data/gate/...`, so cache that directory if a pipeline records gates in one job and checks them in another.

## Running commands under `claude -p`

Use a wrapper that sets `--max-turns` and `--max-budget-usd`, fails on a non-empty `permission_denials`, and never passes `bypassPermissions`. `playbook gate check --json` prints `{"version":1,"slug":...,"ok":...,"phases":[...]}` with the same exit codes as the text output.

## Open items

- The GitHub Action itself was not run.
- Only macOS arm64 was used.
- Whether a `capture-due` marker can appear headless by another path is not proven.
- The cost of a real review run under `-p` was not measured.
