# Agent Budget Dashboard

`playbook usage` shows how many tokens and how many dollars your sessions use, split by day, week, model, repo, branch, effort, account, skill and agent. It reads Claude Code's own session history and needs no setup and no new instrumentation.

## Commands

| Command | What it does |
|---|---|
| `playbook usage` | Opens the terminal view. When stdout is not a terminal, prints the text summary instead. |
| `playbook usage --summary` | Reads anything new, prints the text summary and exits. With `--range` it covers only that range and names the dates. For `today`, `week` and `month` it ends with the [period comparison](#period-comparison). |
| `playbook usage --json` | Prints the totals, the groups and the live view as JSON, for scripts. For `today`, `week` and `month` it adds a `comparison` object. |
| `playbook usage --range <name>` | Sets the range for the terminal view, `--json` and `--summary` (see [Ranges](#ranges)). Default: `week` for the terminal view, `30d` for `--json`, all time for `--summary`. |
| `playbook usage collect` | Runs the local OpenTelemetry receiver in the foreground. `cc` or the `session-init` hook starts it when needed. See [The telemetry receiver](#the-telemetry-receiver). |
| `playbook usage status [--json]` | Shows whether the receiver runs, the telemetry row counts and the retention limits. Starts nothing. |
| `playbook usage prune [--dry-run]` | Deletes telemetry rows past the retention limits. See [Checking and running the collector](#checking-and-running-the-collector). |
| `playbook usage stop` | Stops the running collector through its authenticated shutdown route. It waits up to 3 seconds before it kills one that does not answer (only after checking the pid is the playbook collector). Prints `stopped` or `not running`. |
| `playbook usage doctor [--raw]` | Lists where `pbtop` can read your limits on this machine: plan fields and cached usage numbers in the Claude Code config file, the statusline reading and the manual budgets. It prints field names, ages, the plan label, window percentages and manual budget amounts, never an email, name, id, organization or token. It also says what the statusline budget line will show and which source decided it. `--raw` adds the plan field values and the raw numbers the readers saw. Run it on another machine and share the output when the limits look wrong. |
| `playbook usage account list` | Lists each account key with its events, cost and time span. A display name shows first, with the key in brackets. |
| `playbook usage account name <KEY> <NAME>` | Gives an account key a display name. An empty name clears it. It warns when no event carries the key. Names show only in `account list`. |
| `playbook usage account fingerprint` | Prints the account key of `CLAUDE_CODE_OAUTH_TOKEN` in this shell, like `token:ab12cd34`, and never the token. It fails when the variable is not set. |
| `playbook usage account sessions [--from <T>] [--to <T>]` | Lists the sessions that start in the window (the last 7 days by default), oldest first, with first and last event time, repo, event count, cost and account label. Use an id as a boundary or as `--session`. |
| `playbook usage account reconcile --from <T> --to <T> --dashboard <DAY>=<AMOUNT>` | For each session start in the window, prints the local cost per listed day before it and from it on, and how far the later side is from the dashboard totals you give. The closest boundaries print first; `--top` sets how many. |
| `playbook usage account assign --from <T> --to <T> --as <KEY_OR_NAME> [--session <ID>] [--was <KEY_OR_NAME>] [--confirm]` | Moves the events in a time range to another account. With `--session` it moves only that session's events, inside the range when `--from` or `--to` is given and the whole session when neither is. Without `--confirm` it only prints how many events and how much cost would move. The end of the range is exclusive. Times use your zone, or UTC with `--utc`. Running the same move again moves nothing. It also moves the skill and agent records. |
| `playbook usage account undo [ID]` | Reverses the newest applied move, or the one with this number. It leaves records that were relabeled since. |
| `playbook usage machine [--name NAME]` | Prints this machine's id and name. The id is a random UUID made on first use and kept in the `machine` table of `usage.db`. An older `machine.json` in the data folder is read once to carry its id over, then left in place and ignored; a damaged one never causes an error. The name defaults to the hostname, read when shown; `--name` stores a name in the table and an empty name restores the hostname. Neither leaves this machine. |

Every command that shows data, except `account`, reads new history first. You never ingest by hand.

## The terminal view

The view is styled after a classic terminal system monitor: black ground, rounded boxes with the key hint and title in the top border (`╭─┤¹Spend per day├──╮`), gradients on graphs and meters, a highlighted selected row. The `default` theme uses the Okabe and Ito (2008) color-blind safe palette on black: sky blue for the accent, orange for warnings, vermilion for errors. Text and lines meet the WCAG contrast ratios, and unit tests check the contrast and the distance between series colors under simulated color blindness. State is never color alone: limit states carry a mark and a word, and the selected row and the editor cursor carry a marker.

It needs at least 80 columns by 24 rows. Below that it says so and draws nothing else. Every redraw reads the current size, so resizing needs no handling.

**Views.** Tabs, the header, the panels and the refresh are described with pictures on the [pbtop page](../pbtop.md#views). The tabs and panels come from the `usage.ui.layout` config key (see the [config keys page](../reference/config-keys.md)). The header comparison follows [Period comparison](#period-comparison), and the [Ranges](#ranges) below list every range. Tool rows and health are described under [Tool latency and health](#tool-latency-and-health).

### The charts

Both charts use eighth blocks, one bar per day in the chosen time zone. Idle days are zero and days still to come in a week or month in progress are empty. Today charts the last seven days, since one bar says nothing, and the title says so (`Spend per day, USD, 7 days`).

- **y axis.** Labels at the bottom, middle and top, with a blank column between them and the box, in dollars or tokens with units (`$0`, `$2.5`, `$250`, `$2.5K`, `500K`, `2.5M`). The top is a round value above the peak (1, 1.2, 1.6, 2, 2.5, 3, 4, 5, 6 or 8 times a power of ten), so the labels are round and the middle is half the top. Faint dotted guides mark the top and the middle.
- **x axis.** With one bar a day (a week, a month that fits, today's seven days) the weekday is on one row and the day of the month, 1 to 31, directly below it, when each cell fits the label and a gap. The weekday row needs a letter and a gap in every cell, so a month in a narrow panel has only the numbers. When the numbers do not fit, every second, third or seventh day is labeled, always with the first and the last bar (a number that would touch the last one is left out). No full dates sit on the axis: the header carries the period's dates in the chosen time zone (`this week Oct 6 to Oct 12 until 12:00`, `last 30 days Sep 13 to Oct 12, 2026`), and the readout line names the day under the cursor in full. When a bar holds several days (a wide range), the axis keeps the short first, middle and last date. Today's labels are bold and underlined, the peak day's are yellow, and the cursor's are highlighted.
- **Totals line.** The total, the peak (with its date) and the average per day that has happened, for the range.
- **Cursor.** `Left` and `Right` (or `h` and `l`) move it one bar, `Home` and `End` jump to the first and last bar while a chart has the focus. A readout line gives the day, cost, messages, tokens and top model with its share (the token chart leads with tokens). It starts on today, both charts share it, and a new range resets it.
- **Legend.** Bar color follows height along the theme gradient (dark to light blue for cost), and the legend in the bottom border says so. The mono theme has neither.
- **Fill.** A week or more of bars always fills the width of the plot, and fewer bars are never wider than 16 columns.
- **Wide ranges.** When the range has more days than the chart has columns, days are added up into equal bars (the title says `Spend per 3 days`) and nothing is cut off. The axis, readout and cursor then speak of those bars.
- **Small panels.** When the panel is short, rows go in this order: weekday letters, totals line, readout, day numbers. The plot keeps three rows. With no totals line the peak sits in the bottom border, which leaves the top border room for the `v view: <name>` tag. The y axis goes when the panel is narrower than 16 columns, leaving the bars.

### Daily cost and cost per model

`v` switches the spend panel between three views of the same days.

| View | What it shows |
|---|---|
| Total | One gradient color per bar, as above. |
| By model | Each bar is stacked by model, the biggest model at the bottom. The legend in the bottom border names them (`■ opus-5 ■ sonnet-5 +1` when they do not all fit). The edge between two models is exact to an eighth of a cell: a cell that holds an edge is drawn as a block in the lower model's color on the upper model's. Without color (the mono theme) the models are told apart by pattern (`█ ▓ ▒ ░ ▚ ▞ ▛ ▟`). |
| Daily | A table with a row for each day up to today, newest first: date with weekday, messages, tokens, cost and the model that cost the most that day. `s` and `S` sort it by any column, and the selection works as in the other tables. Today is bold and underlined, the peak day is bold and yellow, and the legend in the bottom border says so. Idle days stay as zero rows, so it reads as a calendar. |

A model keeps one color in its Models swatch, stacked band, legend entry and daily table name. The color is a hash of the model name, stable across ranges. Two models that want the same color in one range get the next free one, and with more than eight models colors are shared.

The keys, with what each does, are on the [pbtop page](../pbtop.md#keys). That table comes from the help window, so it cannot drift. Four keys change what the view reads or what the store holds, and their details stay here:

| Key | Action |
|---|---|
| `X` | Reset the usage history. A window asks first; `y` (or `Y`) deletes, and any other key cancels. The events, the tool events and the read progress are deleted in one transaction, so another reader sees the old history or none, never half. The worker then reads every transcript again like a first run, newest first, and the panels fill while the footer shows the progress. The budget, the limit readings, the account names, the applied account moves, the telemetry rows and the layout stay, and the events you moved get their moved account again when they are read. |
| `u`, `U` | Pick the account. The view opens on the account of the newest event, and the title names it (a name set with `playbook usage account name`, else the account key). `u` steps to the next account, then to all accounts, then back to the newest. `U` jumps to all accounts and back. The totals, the charts and the budget in the header count only the account shown. The live, agents, skills and health panels keep only the telemetry rows of sessions that have an event with that account, so they follow the account too. `playbook usage --account <KEY_OR_NAME>` opens the view on that account. |
| `x` | Pick the machine for the machine panels. The view opens on this machine, and the title names it after the account (the name set with `playbook usage machine`, or the first eight characters of another machine's id). `x` steps to each other machine that wrote events, newest first, then to all machines, then back to this one. With one machine `x` changes nothing and says so. Live, agents, skills, tools, health, sessions and projects follow the pick. The budget in the header, burnup, spend, tokens and models count every machine of the account shown. The pick is saved in `usage.ui.state` and a stored value that cannot be read means this machine. |
| `/` | Type a filter, a case insensitive substring of model, repo, branch or session id. `Enter` applies it, `Esc` cancels, `Backspace` edits. While you type, every key is text, so `q` and `r` do not quit or change the range until you press `Enter` or `Esc`. |

The key handler, the footer hints and the help window read one table, `BINDINGS` in `crates/telemetry/src/tui/keys.rs`, so the help cannot list a dead key or miss a live one. A test presses every key on every panel and fails if one changes anything without a row in the table. The footer shows each hint with its verb (`m month`, `/ filter`, `? help`). On a narrow screen it drops hints in the table's `drop` order, always keeping `v view`, `/ filter`, `? help` and `q quit`, and `Tab tab` when there is more than one tab.

### Ranges

Every range is a set of whole days in the chosen time zone, shown in the header with its name and exact dates (`last week Aug 24 to Aug 30, 2026`). The zone is local time unless `--utc`, `PLAYBOOK_TELEMETRY_TZ` or `usage.timeZone` picks UTC, and the view, the summary and the JSON name it. In local time a day can be 23 or 25 hours long around a daylight saving change.

| Name (`--range`) | Days |
|---|---|
| `today` | Today only. |
| `week` | The calendar week that holds today, from its first day for seven days (it can run into next month, and the graph stops at today). |
| `last-week` | The seven days before that. A week can span a month boundary. |
| `month` | The 1st to the last day of this month. |
| `last-month` | The whole previous calendar month, from December when it is January. |
| `30d`, `60d`, `90d` | The last N days, including today. |
| `all` | Everything stored. |

The terminal view opens on `week`, compared with last week, and `r` steps between `today` and `week`. `today`, `week` and `month` (month to date) are periods, shown against the previous one. `last-week`, `last-month`, `30d`, `60d`, `90d` and `all` do not conflict with the comparison and stay. They show their own totals with no comparison: `W`, `M` and `a` jump to them and `--range` opens on any of them.

Weeks start on Monday. Set `usage.weekStart` to `sunday` or `saturday` to change it (`playbook config set usage.weekStart sunday`). The terminal view, `--json`, `--summary` and the web `range=` parameter all read the key, so they agree. The key is in camel case like the other config keys.

### Period comparison

Each period (`today`, `week`, `month`) is compared with the one before it (yesterday, last week, last month) over the same elapsed span, so a Thursday afternoon is never set against a whole week. The code is `crates/telemetry/src/period.rs`.

- The current span runs from the start of the period through the second of `now`. The previous span starts at the start of the previous period and lasts as long, but never runs past that period (on March 31 the month so far is compared with all of a 28 day February). The header and `--summary` state the elapsed span.
- Times are seconds. A week starts at midnight in the chosen time zone on the day `usage.weekStart` names. An event belongs to the day it falls on in that zone. In local time a day can be 23 or 25 hours long.
- Cost, tokens and messages each get the amount of change and the percent. The percent is `n/a` (JSON `null`) when the previous value was zero.
- `--json` adds `comparison`: `period`, `span_seconds`, `current` and `previous` (`start`, `end`, `start_date`, `end_date`, totals), `change` for the three numbers, and `models` with each model's current and previous group.

### Themes

| Theme | Look |
|---|---|
| `default` | The default look: black ground, `#cc` text, `#ee` titles, `#56b4e9` key hints, `#1f4e79` selected row with white text, per box outline colors, and three point gradients (start, mid, end) for the graphs and meters. |
| `dark` | The terminal's own ground with single colors. |
| `light` | The same with darker colors for a white ground. |
| `mono` | No color. The selected row is shown in reverse video. |
| `spectrum` | The default look with usage colored along the hue wheel, green to yellow to orange to red, so a rising share reads as rising danger. It is opt in: set `usage.theme` to `spectrum` or press `t`. |

The theme comes from the `usage.theme` config key (`default`, `spectrum`, `dark`, `light`, `mono`; see [config keys](../reference/config-keys.md)) and `t` changes it for the session. `NO_COLOR` set to anything but empty forces `mono` and locks it.

Colors follow the terminal. With `COLORTERM=truecolor` or `24bit` the `default` values are drawn exactly. Otherwise, when `TERM` names `256color`, each value goes to the nearest of the 256 color cube and gray ramp, and with neither to the nearest of the 16 basic colors. The values come from the Okabe and Ito (2008) color universal design palette, which stays readable with common color vision differences.

**TV icon.** A small TV icon shows before the title in the top-left corner of the dashboard and the Sessions screen, in a fixed three-cell block that holds at emoji width 1 or 2. Set `PBTOP_ICON_WIDTH=1` if your terminal draws it one cell wide. With a non-UTF-8 locale, `NO_COLOR` set or `TERM` `dumb`, the icon is left out and only plain text shows.

**First read of a large history.** The view opens at once and shows what the store holds. On a store with no watermark yet, a background thread reads transcripts in batches of up to 250 files, newest file first.

- Each batch commits its rows and finished files in one transaction. The 5 second refresh shows the store growing and the footer reads `indexing 120/800 files` (counts only).
- An interrupted run resumes from the files not yet recorded. The source watermark moves only after the last batch, so an unread older file is never skipped.
- While it runs, an empty range says `Indexing` and that data will appear.
- `usage --summary` and `--json` still ingest fully before they print. Past two seconds they write the same file count to stderr.
- Readers and writers share the store: WAL mode, 5 second busy timeout, each batch a short immediate transaction.

**Empty and failed states.** With nothing to chart, the body says why instead of showing empty panels.

- Range empty, store not: it names the dates the store covers and the keys that widen the range (`a` all time, `m` month, `w` week), and, when a filter is set, how to clear it (`c`).
- Store empty: it names the folder transcripts are read from, how to check it, and `CLAUDE_CONFIG_DIR`, which moves that folder when Claude Code uses it.
- Failed read: the error fills the body before the first data, and a banner shows over the last good numbers after it.

The usage code lives in `crates/telemetry` (package `playbook-telemetry`), which playbook depends on by path. The paths below are relative to that folder.

## Where the data comes from

The source is `~/.claude/projects/**/*.jsonl`, the session transcripts Claude Code already writes. Per assistant message they carry the model, the effort, the working directory, the git branch and the token counts (input, output, cache write, cache read). Skill and agent calls appear as `tool_use` entries.

Four details:

- **One message spans several lines.** Each content block of a message is its own transcript line, and each line repeats the same usage. Counting lines would roughly double every number, so events are deduped by message id (and by tool call id).
- **Transcripts have no cost field.** Cost is computed from a per-model price table in `crates/telemetry/src/pricing.rs`, copied by hand from the published pricing page (the source and date are in the file). Input, output, cache reads and both cache write lifetimes (five minute and one hour) are priced separately. Cost is worked out on each read, so a price fix applies to old events without a re-ingest. It is still an estimate and can drift from published prices. A model that is not in the table is never guessed: its messages count as $0, and the summary and the JSON (`totals.unpriced_messages`, `totals.unpriced_models`) say how many and which models.
- **The repo is the real repository, not the last folder.** A worktree or agent folder would otherwise show up as its own repo. `crates/telemetry/src/repo.rs` maps a directory to its repo: first by path (`.claude/worktrees/<name>`, `.git/...`, and playbook's own `repos/<owner>/<repo>` storage, which still works after the directory is deleted), then by asking git for the shared git dir, then by the folder's own name.
- **The account is not in the transcripts.** It is read once per ingest from `oauthAccount.emailAddress` in `~/.claude.json` and tagged onto every event and stored in `usage_events` on this machine. If you are not signed in, it is `unknown`. When `CLAUDE_CODE_OAUTH_TOKEN` is set, Claude Code bills that token's account first, so the `SessionStart` hook records a `token:` key instead (see Account per event). It leaves the machine only when you turn on [usage sync](../guides/17-sync.md), and then as a salted hash, never as the email.

## Storage

One SQLite database at `~/.config/playbook/data/usage.db`, tied to no repo because spend only makes sense as a total. The standalone [pbtop](../../crates/telemetry/README.md) tool uses the same file, so both show the same numbers. Events stay after Claude Code or `playbook cc` prunes old sessions.

- **Migration.** The first run moves an older database from `~/.config/playbook/usage.db`, `~/.config/playbook/usage/usage.db` or `~/.local/state/playbook-usage/usage.db` into place and renames the old file to `usage.db.migrated`.
- **Connection.** Setup follows `crates/cli/src/gate/db.rs`: `busy_timeout` first, then WAL mode, because a refresh reads while `ingest` writes from other processes.
- **One-time repair.** The first ingest after upgrading from an older version rescans the transcripts (`crates/telemetry/src/backfill.rs`) and corrects the repo and the cache lifetime split on rows stored earlier. A row whose transcript is gone keeps its old repo and is priced as five minute cache.
- **Watermark.** One per source (the newest timestamp read). Ingest skips transcript files last modified before it, so a repeat is cheap. It is inclusive and events are deduped by id, so re-reading the newest event is harmless.
- **Account per event.** The `SessionStart` hook records the account in effect (a token fingerprint, else the signed in email) and the time in `session-accounts.log`. Ingest turns the lines into `session_account_spans` and stamps each event by its own timestamp, so one session can list two accounts in `usage account sessions` and its totals sum across both. Only `CLAUDE_CODE_OAUTH_TOKEN` is recorded, an API key session shows the signed in email, and `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1` hides the token from the hook, so it records the email. Re-ingest and the raise-only upsert never rewrite an event's `account`, so `usage account assign` changes stick. Each ingest also repairs keys that an older build wrote as the account joined to a time (`email<TAB>digits`) back to the plain key. The [account label recipe](../recipes/fix-account-labels.md) walks through a correction.
- **Machine.** All six usage and telemetry tables (`usage_events`, `tool_invocation_events`, `telemetry_requests`, `telemetry_tools`, `telemetry_errors`, `telemetry_metrics`) carry a `machine` column holding the id of the computer that wrote the row (`machine_rows.rs`). Only `usage_events` and `tool_invocation_events` carry an account. Ingest and the live telemetry writer stamp the id on each insert. The column arrives with `usage.db` schema version 2 and the id moves into a `machine` table in version 3. Version 4 adds the `session_account_spans` table, and version 5 adds the sync tables and the `usage_events_all` view. An older build refuses a newer database. See the schema version section of the data folder page.
- **Reset.** `X` in the view runs `db::reset_usage`: one transaction that empties `usage_events`, `tool_invocation_events`, `usage_backfill_files` and `usage_watermarks`. With no watermark, the next ingest is a first run. The move tables stay, and the re-read labels each event with the account of the newest applied move that covers it, if any. No table or column changes, so an existing database needs no migration.
- **Transaction.** Each ingest is one `IMMEDIATE` transaction: rows and watermark commit together or not at all, and a second ingest waits for the first instead of failing.

## Adding another agent

`UsageSource` (`crates/telemetry/src/lib.rs`) is the extension point. A source returns normalized `UsageEvent` and `ToolInvocationEvent` values newer than a watermark. Aggregation, storage, the summary and the terminal view never see an agent's file format, so a new source only implements the trait. Two sources exist today: Claude Code (`crates/telemetry/src/claude_code.rs`) and Codex (`crates/telemetry/src/codex.rs`). Every stored event carries an `agent` column (`claude-code` or `codex`, old rows default to `claude-code`).

The Codex source reads `~/.codex/sessions/**/rollout-*.jsonl` read-only and is skipped silently when that directory is missing. Format source: openai/codex `codex-rs/protocol/src/protocol.rs` (`TokenCountEvent`, `TokenUsageInfo`, `SessionMeta`, `TurnContextItem`).

- Model, effort and cwd come from `turn_context`, the session id, cwd and branch from `session_meta`, and tokens from `event_msg` records of type `token_count`.
- One event is one change of the cumulative `total_token_usage.total_tokens`, with tokens from `last_token_usage`. Its id is `codex:<session>:<timestamp>:<total>`, so re-reads dedupe. Cached input is reported as cache reads.
- OpenAI models are not in the price table, so Codex events show as unpriced rather than guessed.
- Compressed `.jsonl.zst` rollouts are not read. The account is `unknown` because rollouts carry none.

## Tests

Tests run against hand-written transcripts in `crates/telemetry/tests/fixtures/usage/` and a scratch `HOME`, never your real `~/.claude`. The fixtures are synthetic: no real project, model spend or session goes into a snapshot. Terminal view tests render into a `TestBackend` buffer, with no real terminal.

| File or test | What it checks |
|---|---|
| `crates/telemetry/src/tui/snapshots.rs` | Whole screens at 80x24, 100x30 and 160x50, the daily table and the stacked view at those sizes, and a week range, against the text files in `crates/telemetry/tests/fixtures/usage_tui/`. After an intended layout change, regenerate them with `UPDATE_SNAPSHOTS=1 cargo test -p playbook-telemetry --lib tui::snapshots` and review the diff. |
| `crates/telemetry/src/tui/parity.rs` | For every range, the view reports the same totals and model, repo and day groups as the web JSON, and again for events on both sides of every week and month edge (a week that spans a month boundary, a Sunday week start, a leap February). The comparison in the view and in `--json` agrees with dates worked out by hand: the same elapsed span in the previous period, the second before and after midnight UTC, and a Sunday week start. |
| `crates/telemetry/src/period.rs` | The period math. |
| Unit tests | The scale math, the chart geometry and every range's bounds. A chart draw test runs the chart at every size from nothing up to 70x22. |

## Limits

- Cost is an estimate (see above), at the published API rate and not what a subscription plan charges you.
- Until you turn on [usage sync](../guides/17-sync.md), it reads one machine's history. With sync, spend, tokens, models, budget, projection and burn-up count every machine of your account, and `x` picks one machine for the other panels.
- Live shows what the transcripts record, so a session that is thinking but has not written a message yet does not appear until it does.
- The terminal view needs a real terminal of 80x24 or more. Use `--json` or `--summary` where there is none.
- No LLM call is built in. The output is plain text and JSON, so the agent already running your session can read it and suggest where to cut cost.

## Spending limits and budget headroom

The budget part of the header, `--summary` and `--json` (`limits`) show how much of a limit you have used and how fast you are using it. The code is `crates/telemetry/src/limits.rs` (the math) and `crates/telemetry/src/limits_source.rs` (where limits come from). Limits that Claude reports use UTC months. A manual budget and the local spend use the chosen time zone.

### Where a limit comes from

| Account | What can be read | Source | Tag in the UI |
|---|---|---|---|
| Claude Pro or Max | The 5 hour and the weekly window: percent used and reset time. | The `rate_limits` object Claude Code passes to the statusline command on stdin ([statusline docs](https://code.claude.com/docs/en/statusline)). The playbook statusline keeps the latest copy in `~/.config/playbook/data/rate_limits.json` and a short history of the two percentages in `rate_limits.jsonl` next to it (both owner only; see [The reading history](#the-reading-history)). | `measured` |
| Behind a Claude apps gateway | The spend limit: percent, reset time and, when the gateway and Claude Code are new enough, dollars used, the dollar limit and the period. | `rate_limits.spend_limit` in the same statusline input. | `measured` |
| Enterprise, or Claude Code with a cache | The monthly spend limit in dollars and the dollars used, and the two windows when the plan has them. This source is not documented and can change or be missing. | The usage numbers Claude Code caches in its config file (`~/.claude.json`, or `$CLAUDE_CONFIG_DIR/.claude.json`): `cachedUsageUtilization.utilization.spend.limit` and `.used` (minor units, dollars only, skipped when `spend.enabled` is false), its `utilization.five_hour` and `utilization.seven_day` windows and `fetchedAtMs`, plus the two rate limit tier names under `oauthAccount` for the plan. Only these fields are read. | `cached`, with how old it is |
| API organization | Nothing automatic. The organization budget is set in the Console and no documented API reads it. The Admin API needs an organization admin key and returns usage and cost reports, not the limit. | `usage.budget.monthlyUsd` and `usage.budget.weeklyUsd`, set by you. | `manual` |
| Any account | Your own estimated spend, from the session transcripts. | The usage store. | `local history` |

Notes:

- A statusline reading that carries one window keeps the other window as stored, with its own read time, until that window resets. A reading is stale (tagged `stale`) as soon as one part held is over 6 hours old.
- Every Claude Code session sends the limits from its own last reply, so an idle session sends old, lower numbers. Per part (5 hour, weekly, spend limit), a new reading replaces the stored one only when it is newer: the same reset time and an equal or higher figure (usage only grows inside a window), or a later reset time (a new window). A lower figure or an earlier reset time is dropped and the stored part keeps its read time, so only a reading that confirms a part keeps it fresh. A stored part whose reset time has passed is replaced by any reading.
- The statusline input has no limit data on an API key account, and a Pro or Max account gets it only after the first response of a session. The plan windows show up once you have used Claude Code with the playbook statusline (`playbook statusline`). Each window can be missing, and Claude Code drops a window after its reset time, so playbook does the same.
- Playbook reads only what Claude Code already passes to a local command and a few plan and usage fields of the Claude Code config file. It calls no endpoint with your login, reads no credential store, keeps no email, name, id or organization in the limits code (the account tag on each event is described under ingest), and writes nothing into Claude Code settings or memory. The stored file holds percentages, reset times and the gateway dollar limit, and no account, token or session id.

**Which monthly limit wins**, in order:

1. A detected monthly limit: a monthly spend limit from the statusline (behind a gateway), or a cached spend limit fetched after that reading, whatever its age.
2. With no statusline reading, the cached spend limit at any age.
3. `usage.budget.monthlyUsd`, only when nothing is detected. It does not draw a bar beside a detected limit. When both are set and differ, the header adds a line with both amounts and says the detected one applies.

A cached limit over six hours old, whether it won under item 1 or item 2, is tagged `cached <age> ago, plus this machine since` when the cache has a used amount from this UTC month. The bar then shows that amount plus this machine's messages after the read, because spend only grows within a month. Without such an amount the tag is `limit cached <age> ago, used is this machine only` and the bar is this machine's estimate for the UTC month. A note in the header, read in full with `i`, shows the amount Claude Code last reported, with its age and the word stale. The tag sits on the figures line under the limit, and the row itself carries a one letter source glyph and a `stale` mark. A manual budget is tagged `manual, local` for the same reason and gets the same note. Once other machines have synced events into `usage_events_remote`, the account scope panels (the budget in the header, Burnup, Spend, Tokens, Models) and the account list read the `usage_events_all` view, so those events count, and the manual tag reads `manual, all machines`. The machine scope panels keep reading this machine's tables.

Cache rules:

- A cached window is used only while the cache is under six hours old and only when the statusline has no reading for that window.
- The cap of a cached limit holds across months. Only its `used` amount is ignored when it was fetched before the month began.
- A fresh cached `used` replaces this machine's estimate, because it can cover every machine on the account (it is ignored when it was fetched before the month began). The "last 24h" pace of a limit always comes from this machine.
- The cache is read again only when the file changes, and a half written file keeps the last good reading.
- The period of the cached limit is assumed to be the calendar month.

**Plan name.** It comes from the rate limit tier names in the config file. The two tiers seen on Max accounts are recognized as Max. An account whose organizationType is claude_enterprise, or whose seatTier names enterprise, is Enterprise: the panels hide the 5 hour and weekly windows and say the whole limit is the monthly extra usage budget. Everything else shows as "Unknown plan". When a plan has no 5 hour and weekly windows, the header says "5 hour and weekly windows: not on this plan" and `i` shows the full note, which adds that Pro and Max get them from the playbook statusline. An unknown plan, such as Pro, gets the same text. On a Max plan with no reading yet the header says a reading appears after a session with the playbook statusline. Run `playbook usage doctor` to see which sources exist on a machine.

**Manual budget.** It covers the calendar month or week in the chosen time zone (the week starts on `usage.weekStart`). The spend is the estimated cost from your local history, not your bill. Set it with `playbook config set --global usage.budget.monthlyUsd 200`. `0` means no budget.

### What you see in each case

You set no limit for any of these. The rows come from what Claude Code reports on this machine, and the dashboard works out the burn rate, the projection and the time each of 50, 80 and 100 percent is crossed. The Burn-up panel charts the most used limit. Nothing is sent anywhere and no login token is read.

| Case | What Claude Code reports | Rows shown | Burn-up chart |
|---|---|---|---|
| A. Pro or Max with windows | The 5 hour and weekly windows, from the statusline or the cache. | One row per window: percent used, reset in, burn per hour, projected percent at reset, crossings. The plan label comes from the tier names. | A chart per window, from the readings the statusline saved while it ran (or a straight line from the window start to the reported percent when there are none), with the projection to the reset and the cutoff lines. |
| B. Max with extra usage on, cap not known | A, and the extra usage flag. | The same as A. The extra usage flag is kept in the plan fields that `--raw` shows. | As A. |
| C. Max with the extra usage cap known | A, and the cache `spend` block. | A, and a dollar row with used, cap, left, reset and crossings. | A, and a monthly dollar chart from this machine's cost, with the reported figure marked and the cutoff lines at the cap. |
| D. No windows, dollar cap known | The cache `spend` block, or the statusline `spend_limit`. | One dollar row, tagged `measured` or `cached`. When a reported figure and this machine's estimate both exist, a line next to it shows both, the reported one labeled "reported by Claude". | Monthly spend, the projection to the end of the period and the cutoff lines. |
| E. Cap not reported | Nothing. | One line on what is missing: a window plan is told to run a session with the playbook statusline, a dollar or unknown plan that the cap is not reported by Claude Code on this login. Local spend and burn still show. | Spend and projection, and no cutoff lines. A limit of zero is never drawn. |
| F. API key, Bedrock, Vertex or Foundry | Nothing. | As E, and the `usage.budget.*` lines when you set them. | As E, or with the budget as the 100 percent line. |
| G. Reading over 6 hours old | Any. | A statusline reading stays, tagged `measured, stale`. A cached dollar limit over 6 hours old is still used when nothing newer was detected, tagged `cached <age> ago, plus this machine since` (or `limit cached <age> ago, used is this machine only` when the cache has no amount). A cached window over 6 hours old is dropped. The comparison line says "stale, read N ago". | The monthly dollar chart marks the time of the reported figure. A window chart ends at the last reading, and its saved readings stay as they were. |
| H. Window already reset | Any. | The window is dropped, as Claude Code drops it. | None. |
| I. A rate limit error seen | An `api_error` with status 429 in the Claude Code telemetry. | A line "Claude Code was rate limited at T (HTTP 429)" with the newest time. A 429 counts for the line and the reason for 7 days. When a window plan has no reading and a 429 was seen, the reason is `rate-limited-seen`; other reasons keep their word. | A `×` marker at each 429 inside the period shown. A 429 before the period starts or at its end is not drawn. A retried call counts once. |

The 429 is the only proof without a token that a cap was hit. It is drawn and never turned into a limit value. When the reported dollar figure and this machine's estimate both exist, the summary shows both and says whether they differ by at least the larger of 1 dollar and 10 percent of the reported figure.

### The math

For a limit over a period from `start` to `end`, with `used` so far at `now` (all UTC seconds):

| Value | Rule |
|---|---|
| elapsed | `now` minus `start`, clamped to the period. Before the period starts, it is zero. |
| left | `end` minus `now`, clamped the same way. |
| average per hour, per day | `used / elapsed hours` and `used / elapsed days`. The first day is partial, so days are a fraction of 24 hours, not whole days. With zero elapsed time there is no average (`n/a`, JSON `null`), never a division by zero. |
| remaining | `limit` minus `used`, never below zero. |
| allowed per hour, per day | `remaining / hours left` and `remaining / days left`. With no time left there is none. |
| projected | `used + average * time left`, the total at the end of the period at the current burn rate. It needs at least one hour of elapsed time, because the first minutes say nothing. |

The state is one of:

| State | When |
|---|---|
| `limit reached` | `used` is at or above the limit. |
| `will exceed` | The projection is above the limit. It shows the UTC time the limit is reached at the current rate. |
| `on track` | The projection is between 80 percent and 100 percent of the limit. |
| `under` | Below 80 percent. |
| `too early to project` | Before the first hour. |

Each state also starts with a mark, so it reads without color: `▲` for `limit reached` and `will exceed` (bold), `◆` for `on track`, `●` for `under` and `○` for `too early to project`. Where the icon is left out (non-UTF-8 locale, `NO_COLOR` or `TERM` `dumb`) the marks are `!`, `~`, `+` and `?`. A refusal in the layout editor starts with the alert mark in the same way.

A month has 28 to 31 days (a leap February has 29) and a reported limit ends at the next UTC midnight, so it has no daylight saving. A manual budget follows the chosen zone, so in local time a day can be 23 or 25 hours long. The same math serves the 5 hour and weekly windows, where the limit is 100 percent and rates are percent per hour and per day.

For the period on screen (today, this week or this month, this week for other ranges) the panel also shows the average cost per hour and per day against the previous period over the same elapsed span, the same rule as the period comparison.

### Where it shows

- **Terminal view.** The budget is part of the header, which every tab shows. It sits in a column beside the cost, tokens and messages rows on a wide screen and under them on a narrow one, and draws only the rows it has data for. When the header stacks, the comparison folds into one row and the budget keeps three rows (`STACKED_ROWS`), the rest behind `i`, so the default Charts tab shows every panel at 80x24. It is drawn at every size from the 80 by 24 minimum, and a short screen hides tab panels before it touches the header. It lists at most two limits, worst first, and says how many more `--json` has. With no known limit it says how to set one. It ignores the project filter: a limit belongs to the account, not to a repo.
- **`--summary`.** A "Limits and burn rate" section. After the limits it adds:
  - the amount Claude reports as used next to the local estimate, with its age and a stale marker once it is over 6 hours old (or why the two were not compared);
  - one `<period> crossings` line per limit, with the time each of 50, 80 and 100 percent was or will be crossed;
  - the time of the newest 429 of the last 7 days, when there is one;
  - one line saying why no limit comes from Claude, when none does.

  With a monthly dollar limit it also prints a plain `burn-up` table, one row per day through today with the cumulative local spend and the share of the limit, in ASCII, so a pipe and `NO_COLOR` read it as is. Before the period starts it says `the period has not started`. Days split at midnight in the time zone of the summary, or in UTC when the period came from Claude.
- **`--json`.** `limits` with these fields, and `burn` for today, week and month, each with the current and previous period:

| Field | Content |
|---|---|
| `known`, `hint` | `hint` is set when nothing is known. |
| `snapshot_captured_at`, `snapshot_stale` | Snapshot fields. |
| `plan`, `plan_label` | `plan` is `max_5x`, `max_20x`, `enterprise` or `unknown`. |
| `windows_note` | Null when a window is known. |
| `reason` | Why no limit comes from Claude: `no-reading`, `stale`, `cap-unknown`, `windows-reset` or `rate-limited-seen`, else null. |
| `comparison` | Null, `skipped` with the reason, or the amount Claude reports and the local estimate with `delta_usd`, `delta_pct`, `disagrees`, `read_at` and `stale`. |
| `cache` | `fetched_at`, `age_seconds` and `fresh`, or null. |
| `limits` | Per limit: unit, period, origin (`measured`, `manual` or `cached`), used, limit, start, end, remaining, average and allowed per hour and per day, projected, state, `exceeds_at`, `thresholds` and `series`. |

`thresholds` has three entries for 50, 80 and 100 percent with `pct`, `state` (`crossed`, `projected`, `beyond` or `unknown`), `at` for the first two and `period_end` for `beyond`. `series` depends on the limit:

- Dollar limit: `basis` (`daily`), `days` (`day`, `cumulative_usd` and `percent_of_limit`) and `projected_end_usd`.
- Percent window: `basis` (`readings` or `line`), `points` (`at` and `percent`) and `projected_end_percent`.
- Both kinds list `rate_limited_at`, the UTC seconds of each 429 in the period.

### The statusline budget line

When the 7 day or 5 hour window is reached, or its pace reaches the cap before it resets, the statusline prints a fourth line. It uses the same pace maths as the budget in the header and reads only the `rate_limits` the statusline already receives on stdin, so it adds no network call and no database scan.

| Window | Reached | On pace to reach it |
|---|---|---|
| 7 day | `! 7d CAP HIT` | `! 7d cap in 2d3h` |
| 5 hour | `! 5h CAP HIT` | `! 5h cap in 1h20m` |

- **Time left.** Days and hours when it is a day or more (`4d9h`, `2d`), hours and minutes under a day (`1h20m`, `3h`), minutes under an hour (`45m`) and `<1m` under a minute. Each rounds down.
- **When it is absent.** On a window within its cap, past its reset time or with no reset time, the line is left out and the statusline keeps its other lines. A window that is not yet reached also stays out of the line in its first hour, because the pace is not known yet; a reached window shows at any time. Line 3 keeps its own percentages.
- **Colour.** The line is red, and the leading `!` and the words carry the meaning, so it reads the same without colour.
- **Width.** The line is fitted to `COLUMNS` (120 when unset, and 0 when it is not a number, which leaves only the 7 day segment). The 7 day segment comes first and is kept, and the 5 hour segment is dropped whole on a terminal too narrow for both.
- **No switch.** There is no setting that hides the line.

#### The monthly dollar figure

A seat billed by usage has no 5 hour or 7 day window. It has a monthly dollar limit, which Claude Code keeps in the `spend` block of the `cachedUsageUtilization` entry of its config file (`~/.claude.json`, or `$CLAUDE_CONFIG_DIR/.claude.json`). The statusline reads that block, so the same fourth line carries one more segment. It reads the file only when stdin has no dollar figure, and skips a file over 512 KiB.

| State | Text |
|---|---|
| Pace passes the limit | `! acct $986 / $1,500 mo, cap in 1d` |
| Limit reached | `! acct $1,600 / $1,500 mo CAP HIT` |
| Pace stays under | `acct $410 / $1,500 mo, on pace` |
| Pace lands near the limit | `acct $1,200 / $1,500 mo, near cap` |
| Under an hour into the month | `acct $986 / $1,500 mo` |
| Cache older than six hours | `acct $986 / $1,500 mo (10d14h old)` |

- **This chat.** Beside a figure Claude reports, the segment before it shows this chat's cost, `cost.total_cost_usd` from stdin, as `chat $1.20 | acct $412 / $1,000 mo, on pace`. It is left out when the chat has cost nothing yet, and beside the local figure, which is not the account's. It is the first segment dropped on a narrow terminal, since line 3 already shows the same cost.
- **No personal data.** Only the dollar amounts are read from the config file. The email, names and ids next to them are never shown.
- **Which source wins.** The amount used and the limit come from the first source that has them: the statusline reading when it carries dollars and was read within six hours (a Claude apps gateway sends it), then the cache. The cache must have been fetched this UTC month and not in the future, because a figure from last month says nothing about this one. With neither, a monthly budget falls back to this machine's usage database (see "A budget alone"); without a budget the segment is left out, and so is a figure of zero used.
- **A stale cache.** Claude Code writes the cache when it fetches usage, and the `/usage` view does that, so it can be days old. After six hours (`STALE_SECONDS`) the figure is still shown, because spend only grows within a month, but with its age and without a projection. It is red only when the amount already passed the limit.
- **Your own budget.** `usage.budget.monthlyUsd` replaces the limit when it is lower than the one Claude reports, and the text then says `mo budget` instead of `mo`. A higher budget changes nothing.
- **A budget alone.** With no gateway reading and no cache figure, a monthly budget is measured against this machine's cost for the UTC calendar month, read from the usage database, and the text says `mo budget (local)` because other machines are not counted. All accounts on the machine are summed. The total is kept for 10 seconds (`LOCAL_TOTAL_CACHE_SECONDS`) in `monthly-local-total` under the statusline cache folder, the database is opened read only and never created, and if it is missing or busy the segment is left out.
- **Not the whole account.** Apart from the local figure, the amount comes from Claude Code, not from this machine's transcripts. Whether it counts the whole organization or only your seat is not documented, so the line never calls it the organization total.
- **No 5 hour note.** When the data has no 5 hour window the line says nothing about it.
- **Narrow terminals.** The words after the figure go first, then the chat cost, then the whole segment. The 7 day alarm is kept.

#### Where each window lands on line 3

Line 3 shows each window's percent and, once the window has run for an hour, where it lands at its reset if use goes on at the same pace. The pace maths is the same as line 4 and the budget in the header.

| Line 3 reads | Meaning |
|---|---|
| `5h 42% → 88% at 14:00` | The 5 hour window is at 42 percent and lands at 88 when it resets at 14:00. |
| `7d 31% → 70%` | The 7 day window is at 31 percent and lands at 70 when it resets. |
| `5h 42% (4h30m left)` | The 5 hour window has no projection: it is under an hour old, or it has reached its cap. |
| `Rate 7d: 60%` | No pace yet, or the window has no reset time. Shown above 50 percent only. |

- **Colour.** The projection is red past 100 percent, yellow from 80 and green below. A projection past the cap also gets a red `!` alarm on line 4.
- **Reached windows.** A window at its cap shows no projection. The 7 day window shows only its percent and the 5 hour window adds the time left to reset. Line 4 says `CAP HIT`.
- **Large numbers.** A projection over 999 percent is written `>999%`.
- **Clock.** The reset time uses `PLAYBOOK_TELEMETRY_TZ` (`utc` or `local`) and is local time otherwise.
- **When it is absent.** Without `rate_limits` on stdin (an API key, Bedrock, Vertex, or before the first reply) the window segments are left out.

`playbook usage doctor` prints the same decision under "What the statusline budget line will show": where the figure comes from and how old it is, the limit in force and why, and the order the sources are tried. `--raw` adds the numbers behind each choice. The plain output stays short.

### Next message estimate

Line 3 of the statusline shows what the next message will cost at least, and warns when the prompt cache has gone cold. It replaced the per minute rate.

| Line 3 reads | Meaning |
|---|---|
| `$1.2345 (next ≥$0.10, warm 4m left)` | The cache is warm. The next message reads the current context from the cache at the cache read price, and the cache expires in 4 minutes. |
| `$1.2345 ! cold cache, next ≥$1.25` | The cache expired, or the model changed. The next message writes the whole context to the cache again, so it pays the write price. The line is red. |
| `$1.2345 (next ≥$0.10)` | The price is known but the cache state is not (no usable transcript, or no cache write in it). |
| `$1.2345` | No estimate: the model has no price in the table, or the context is empty. After `/clear` the context is empty until the first reply. |

What the figure counts:

- **A lower bound.** It prices only the input side of the context the session already holds. The next message also writes the previous reply and the new prompt to the cache and pays for its output, so the message costs more than the figure. The figure rounds down to the cent, and a value under one cent shows as `<$0.01`.
- **Where the numbers come from.** The token counts and the model id come from the statusline input. The cache state comes from the `prompt_cache` object Claude Code sends on stdin (v2.1.251 and later): `warm`, `ttl`, `expires_at` and `recache_tokens_if_cold`. The countdown is `expires_at` minus now, and under a minute it shows seconds (`warm 45s left`). The cold figure prices `recache_tokens_if_cold` at the write price of `ttl`. The cache is cold when `warm` is false or `expires_at` has passed, and unknown when `caching_observed` is false (caching off or not reported).
- **Older Claude Code.** Without `prompt_cache`, or when it is warm with no `expires_at`, the time of the last request and the cache tier (5 minutes or 1 hour) come from the tail of the session transcript, which the statusline reads on each render (at most 2.25 MiB). The tier is the one of the last cache write the session made. A cache read refreshes the cache lifetime, so the countdown starts at the last request.
- **Refresh.** `playbook setup` sets `statusLine.refreshInterval` to 5 seconds whenever the status line runs `playbook statusline`, replacing any other value, so the countdown keeps ticking while you are idle. A status line that runs another command is left alone. `playbook doctor` warns on layer 5 when the interval differs. Without a refresh interval the countdown stays stale until the next event.
- **Model switch.** A different model starts a new cache, so a switch seen in the transcript shows the cold warning even when `prompt_cache` says warm. Its price uses the 5 minute write, because the tier of the new model's cache is not known.
- **Not seen.** A tool call or subagent that runs longer than the cache lifetime turns the line red during the turn, because the transcript records a message only when it ends. A change to the system prompt also breaks the cache, and the estimate cannot see it.
- **Prices.** They come from the same table as the usage dashboard (`crates/telemetry/src/pricing.rs`). A model that is not in the table shows no estimate.

### When a limit is reached

The header says when each limit is reached at your current pace, in the time zone shown in the header title (UTC or local). It was a separate Projection panel before, and the math is the same. It shows the sooner of the two paces below as `hits it in 2d 4h`, with `(24h)` added when the 24 hour pace is the sooner one, or `safe` when neither reaches the limit before it resets. The older layout word `projection` still loads and draws nothing. A dollar limit has two paces:

1. The last 24 hours, which follows what you are doing now. When the period started less than 24 hours ago, it uses the spend of the period over the age of the period, so spend from before the period never counts (under one hour of data it says "no pace yet").
2. The period average since the period began, which is steadier and shows the long view.

A limit that is a percentage of a window (the 5 hour and weekly windows of a plan) has the window average only, because the window has no per day cost. The header shows no clause for a limit that is reached already (its state says so) or when no pace is known yet. With no limit known, the header shows an empty bar and says to press `b`.

### The burn-up panel

The Burn-up panel is on the Charts tab. It plots the spend of the period so far against a limit that Claude Code reported, with a dotted projection at the current pace, a cutoff line for the limit and lines for 50, 80 and 100 percent, each marked where it is or will be crossed. It uses the most used limit that has a burn-up. Where the terminal shows Braille the plot is drawn as dots, four times as fine vertically: the used color blends smoothly with the share of the limit. It holds the first color below 50 percent, moves to the next at 70 percent and the next at 90 percent, and holds the last from 100 percent. The default theme runs bluish green, yellow, orange, vermilion. The spectrum theme runs green, yellow, orange, red. The used line is one unbroken stroke, with no gaps between columns of different height. The dots are half filled below 80 percent and fully filled from there, so the danger zone reads without color. Crossing markers stay whole cell glyphs. `PBTOP_DOTS=0` keeps the solid chart for a font that draws Braille badly, and the mono theme, `NO_COLOR`, `TERM=dumb` and a non UTF-8 locale use it too.

- A monthly dollar limit is drawn from the daily cost of this machine.
- A window in percent is drawn from the readings saved in `rate_limits.jsonl`, or as a straight line from the start of the window to the figure Claude Code reported when none were saved.
- With no limit known the panel says so and charts the spend alone.

The used area is drawn as Braille dots, four times as fine as a whole cell in height. Its color follows the share of the limit as described above, and from 80 percent every dot of a cell is set, so the danger zone reads denser without color. Set `PBTOP_DOTS=0` if your font draws Braille badly; the chart then keeps the solid glyphs, as it does in the mono theme and when symbols are off.

Nothing here is set by hand: the limit and the used figure come from what Claude Code reports. The math is in `crates/telemetry/src/burnup.rs` and the drawing in `crates/telemetry/src/tui/burnup.rs`.

### The reading history

`rate_limits.jsonl` sits next to `rate_limits.json` in the data folder. Each line is one reading: a format version `v` (now 1), the time and the two window percentages, for example `{"v":1,"t":1788400000,"five_hour":55.4,"seven_day":60.0}`, with `null` for a window the reading did not carry or whose figure was dropped as stale. It holds no account, token or session id.

- **Versions.** A line without `v` (from before the field existed) is read as version 1. A line with a newer `v`, or one that is not valid, is skipped.
- **Adding.** The statusline adds a line only when a percentage the reading changed or confirmed differs from the last line and at least 300 seconds have passed since it, so a busy session writes at most 288 lines a day.
- **Age.** Lines older than 8 days are dropped when the file is rewritten, which happens once the oldest line is a day past that (so the file can hold up to 9 days). If the clock goes back, lines from the future are dropped.
- **Size.** The file never grows past 256 KiB. A rewrite removes the oldest lines first, and a file that is bigger anyway is ignored by readers and replaced by the next write.
- **Readers.** Only the usage views read it, and a window chart uses the lines inside that window. A view keeps the parsed lines until the modified time or size changes, so a refresh of an unchanged file parses nothing.
- **Deleting.** You can delete it at any time. The chart goes back to the straight line.

Writing is best effort: an unwritable folder or failed write never changes the statusline output or its exit code. The common case reads the first and last line and appends one line, and a test checks that a full file adds no more than 5 milliseconds to a render.

### What is not automatic

- Enterprise and API organization budgets, as above.
- Pro and Max windows need the playbook statusline to be the Claude Code status line. If you use another status line, set a manual budget instead, or wire `playbook statusline` in.
- Headless `--output-format json`, OpenTelemetry and `/usage` do not carry limits in any documented way that a background process can read, so they are not used.

## Telemetry storage

The usage database also holds tables for OpenTelemetry data, created next to the transcript tables when it is opened:

| Table | One row per |
|---|---|
| `telemetry_requests` | API request: session id, request id, model, effort, agent name, skill name, token counts, cost, call duration. |
| `telemetry_tools` | Tool call: tool name, success, duration. |
| `telemetry_errors` | Failed API attempt: status code, attempt number, duration. |
| `telemetry_metrics` | Metric point of four counters: active time, lines of code, commits, pull requests. |
| `session_sources` | Session: which source it reads from, `telemetry` or `transcript`. |
| `session_reconcile` | Session with telemetry: the verdict and the numbers compared. |

Totals never read them, so every total is unchanged. The rows never hold prompts, tool content, paths, emails or user ids. A session with no row in `session_sources` reads from transcripts. Dropping the tables reverts the feature and loses no transcript data (`telemetry_db::migrate_down`). Telemetry follows [issue 769](https://github.com/amplifiedengineer/playbook/issues/769): playbook writes Claude Code settings only through `setup`, with consent (see Telemetry by default), and transcripts stay the fallback. Rows are kept for 90 days and 500000 rows per table unless you change the [retention keys](../reference/config-keys.md).

Plain `claude` sessions (not started through `cc`) send nothing to the receiver unless the OpenTelemetry variables are set for them. `playbook setup --telemetry` writes them into your Claude Code settings, with your consent, see [Telemetry by default](#telemetry-by-default). Without them, such sessions are read from transcripts.

## Background warm-up

The `usage-warm` hook (SessionStart and SessionEnd, wired by `playbook setup`) keeps the store current without you opening the view. It checks a stamp file, then starts `playbook usage warm` detached and returns in a few milliseconds with no output.

- The child takes `auto-ingest.lock` in the usage directory so one read runs at a time (a lock older than 20 minutes counts as dead).
- It lowers its priority, runs the same resume-safe batched ingest as the usage view, and stops after two minutes.
- A finished read writes `auto-ingest.stamp`. Hooks skip while the stamp is younger than 10 minutes.
- A read that hit the time limit leaves no stamp, keeps its committed batches, and the next hook resumes it.
- It stores only what ingest already stores and logs nothing. The plugin-disabled pause applies, since no hook runs then.

## The telemetry receiver

`playbook usage collect` runs a local receiver for Claude Code's OpenTelemetry export. It is optional: without it every number comes from transcripts, as before. Playbook starts it for you and keeps it running, see [Checking and running the collector](#checking-and-running-the-collector).

- **Where it listens.** 127.0.0.1 only, `http/json` only. The port is chosen once (default 47318, or a free one when that is taken on the first run) and recorded. Later starts reuse it and fail with a message when another program owns it. An install that already recorded a port (often 4318) keeps it. `--port N` asks for a specific port, fails when it is busy, and replaces the recorded port. Run `playbook setup --only telemetry` afterwards to rewrite the endpoint in Claude Code settings.
- **State.** `collector.json` next to the usage database (owner-only) holds the pid, the port and a bearer token. The collector holds an exclusive lock on `collector.lock` for its whole life and writes its pid and version into that file. A second start finds the lock busy, says so and exits 0. A stale file (dead pid) is replaced. The token and port survive restarts, so a session started earlier keeps sending to the right place with the right token.
- **Idle shutdown.** A collector you start by hand stops after 600 seconds with no telemetry request (`--idle-seconds`, 0 means never) and clears the pid. The collector that playbook starts runs with 0 and does not idle out. Only a request that was read counts: a bare connect, such as the liveness probe `cc` makes or `playbook usage status`, does not reset the timer (a test polls status while the collector idles out to prove it). A session still open when the collector stops loses its export from then on and is read from transcripts. Use `--idle-seconds 0` for a collector that should outlive quiet periods.
- **Limits.** Headers 16 KiB, body 1 MiB (also when chunked), 50 requests per second (then 429), 5 seconds to send a request, 64 open connections (then 503). Each connection has its own thread, and one writer thread stores rows in batches. Content type must be `application/json`, a compressed body is refused, and the bearer token is required. `/v1/logs` and `/v1/metrics` are stored, `/v1/traces` is accepted and dropped.
- **What is stored.** `api_request`, `tool_result` and `api_error` events, keyed by session id and request id, tool use id, or event sequence, and four delta counters from the metrics (see Active time and counters), keyed by their interval, so a redelivered batch changes nothing. Only an allowlist of attribute keys is read (event name, session id, request id, model, effort, agent name, skill name, tool name, tool use id, success, duration, status code, attempt, event sequence, the four token counts, cost). Every other attribute, including prompts, tool content, file paths, emails, user and organization ids and resource attributes, is never read. Kept names must be plain ASCII (letters, digits and `. _ - : /`), else they are dropped.
- **Data loss.** Claude Code may drop telemetry sent while no receiver listens. Such a session is read from transcripts.

## Telemetry by default

Telemetry is the default source for the per agent, skill and latency views. Transcripts stay the authority for every total. `usage.telemetry` is `auto` unless set to `off`. The effect depends on the client:

| Client | What happens |
| :- | :- |
| `playbook cc` (and `playbook ccd`) | Sets the variables for that session only, when you have set no telemetry yourself and a collector runs. Like most playbook commands, it starts the collector first when your user settings point at it. |
| Plain `claude`, the IDE, the desktop app | If you answered yes to the setup question (the default) or ran `playbook setup --only telemetry`, the variables are in the `env` block of `~/.claude/settings.json`, which these clients read. The `session-init` hook, the prompt hooks and most playbook commands start the collector when it is down. With the hooks part off, only a playbook command starts it. Otherwise they are read from transcripts. |

Playbook changes your Claude Code settings only when you ask. `playbook config set --global usage.telemetry auto` writes the telemetry keys and `off` removes them. `setup` writes them after you say yes (`--yes` counts, and the question defaults to yes), and only when no telemetry variable exists in your environment or in any settings file (user, project, local, managed).

- **The one exception** is a lone off switch (`CLAUDE_CODE_ENABLE_TELEMETRY` set to an off value and no other telemetry variable anywhere) in `~/.claude/settings.json`. A yes replaces it, and `playbook uninstall` puts the old value back.
- Any other value you set is never overridden. If you turned telemetry off elsewhere, or point it at your own endpoint, playbook leaves it and reads transcripts.
- `setup` records what it wrote in `~/.config/playbook/telemetry-env.json`, and `playbook uninstall` removes exactly those keys, unless you changed them since.
- The shared settings template no longer pins `CLAUDE_CODE_ENABLE_TELEMETRY` to `0`. An install made earlier keeps that value until you remove it, and it counts as your setting.

`playbook usage status` and the usage view's footer say which case you are in:

- `telemetry: on, collector running`
- `telemetry: on, collector not running`, with the fix: run any playbook command or start a session.
- `telemetry: off (disabled in your Claude Code settings, using transcripts)`, with the fix: run `playbook setup --telemetry`, which replaces an off switch that stands alone in `~/.claude/settings.json`, or remove `CLAUDE_CODE_ENABLE_TELEMETRY` from the `env` block of your settings yourself when it is set anywhere else.
- `telemetry: off (set up by you for another endpoint, using transcripts)`
- `telemetry: off (usage.telemetry is off, using transcripts)`, with the fix: `playbook config set --global usage.telemetry auto`.
- `telemetry: off (not configured)`, with the fix: `playbook setup --telemetry`, or start sessions with `cc`.

If the collector is not running when a session starts, Claude Code may drop what it sends, and that session is read from transcripts. After a reboot or a logout this lasts until the next playbook command or hook starts the collector.

## Telemetry privacy

- **What is stored.** Only `api_request` rows with the session id, request id, model, effort, agent name, the four token counts, the cost and the call duration, plus one row per tool call (tool name, outcome, duration), per failed API attempt (status code, attempt number, duration) and per metric point of active time, lines of code, commits or pull requests (session id, type, interval, value). Prompts, tool content, file paths, emails, user, account and organization ids, and resource attributes are dropped when the receiver parses a request and never reach the database.
- **What `cc` sets.** With `usage.telemetry` at `auto`, a running receiver and no telemetry setting of yours, `cc` sets these in the session's environment only: `CLAUDE_CODE_ENABLE_TELEMETRY`, `OTEL_LOGS_EXPORTER=otlp`, `OTEL_METRICS_EXPORTER=otlp`, the traces exporter to `none`, the `http/json` protocol, the loopback endpoint, an `Authorization` header with the receiver's token and `OTEL_METRICS_INCLUDE_ACCOUNT_UUID=false`. It never turns on `OTEL_LOG_USER_PROMPTS`, `OTEL_LOG_TOOL_DETAILS`, `OTEL_LOG_TOOL_CONTENT` or `OTEL_LOG_RAW_API_BODIES`.
- **What it never does.** `cc` never writes Claude Code settings and adds no `--settings` flag. Only `playbook setup --telemetry`, with your consent, or `playbook config set --global usage.telemetry auto`, which you run yourself, writes the same variables (and nothing else) into the `env` block of your settings. If any telemetry variable is already set in your environment or in a settings `env` block (user, project, local or managed), or such a file cannot be parsed, `cc` sets nothing and your own configuration wins. `setup` has one exception, described above: a yes replaces a lone off switch in the user settings.
- **How long it is kept.** 90 days and 500000 rows per table, pruned automatically and by `playbook usage prune`.
- **Where it goes.** The receiver listens on 127.0.0.1 only and the store file is private to you. Telemetry rows never leave the machine, and [usage sync](../guides/17-sync.md) does not send them either. `usage.telemetry` set to `off` stops `cc` from touching the environment at all.

## Choosing a source per session

After each ingest, `usage::reconcile` compares every session that has telemetry rows with its transcript totals and records one verdict in `session_reconcile`. Transcripts stay the authority, and every total (`--json`, `--summary`, the terminal view) still sums the transcript rows priced by playbook, so nothing is counted twice.

| Verdict | When | Source |
|---|---|---|
| Complete | Tokens within 1 percent and cost within 0.5 percent. | Becomes `telemetry`, which makes its per agent split a valid split of the verified total. |
| Partial | Telemetry has fewer tokens than the transcript (the receiver started late or missed a batch). | Stays `transcript`. |
| Disagree | Telemetry has more tokens, or the cost differs. | Stays `transcript`, and the verdict holds a note with the gap. |
| No telemetry rows | Nothing is recorded. | Reads from transcripts, so old history needs no backfill. |

The pass is idempotent. A failure leaves transcripts as the source.

## Cost per agent

The Agents view (`4`, then `v`) groups `telemetry_requests` by agent name for the range (`query::load_agents`). The main session has no agent name and shows as `main`. The title note `source telemetry N% of spend` is the telemetry cost over the transcript cost of the range, capped at 100 percent. Only telemetry knows the agent, so a range with no telemetry rows shows `Telemetry off`. Totals are unchanged. Claude Code reports an agent from a plugin it does not list as `custom`. On each ingest, `agent_names::resolve_custom` renames those rows to the `agentType` in the subagent's `meta.json`, found by the request id that the subagent transcript shares with the telemetry row. It reads only request ids and that name, and rows with no transcript keep `custom`.

## Cost per skill

The Skills view (`4`, then `v` twice) groups `telemetry_requests` by skill name (`query::load_skills`). A short headless trial on Claude Code 2.1.293 (Haiku, under one cent) checked the attribute. The `api_request` event carries `skill.name` on the requests made while a skill is active (the request that follows the skill load). Requests before it carry none. The skill row is the cost of the requests the skill shaped, not a share of the whole session. The separate `skill_activated` event redacts project and user skill names to `custom_skill`, but `api_request` showed the real name in the trial, and a plugin skill was checked later, see below. Names are stored only if they are plain ASCII, like every other name. Requests with no skill are left out of the view, so the rows are a share of telemetry cost and do not sum to the total. Tables made before this slice gain a `skill_name` column on open.

Plugin skills (a second trial on 2.1.293, Haiku, about 0.006 USD, a throwaway plugin loaded with `--plugin-dir`): the `api_request` event and the cost and token metrics carry `skill.name` for a plugin skill. A plugin that is not an official marketplace one is reported as `third-party` for both `skill.name` and `plugin.name`, and `skill_activated` shows `custom_skill` with `skill.source` `plugin`. The real name only appears when tool details are logged, which also exposes paths and commands, so playbook does not ask for it. The view shows all such skills as one row, `plugin skills (names hidden)`. Skills of official plugins were not tried.

## Tool latency and health

Two more views of the projects panel (`4`, then `v` three and four times) read `telemetry_tools` (one row per tool call: tool name, success, duration) and `telemetry_errors` (one row per failed API attempt: model, status code, attempt number, duration), plus the new `duration_ms` of `telemetry_requests`. Tables made earlier gain the column on open and the new tables are created next to the others.

- **Tools** lists each tool with calls, error rate, average and slowest call. For today, the week and the month it adds the previous period's average and the change over the same elapsed span, like the Models panel. Only the name, outcome and time are read, never the call or error text.
- **Tools merge two sources.** Live rows come from `telemetry_tools`. For calls the collector missed, the table also counts the Agent and Skill calls in `tool_invocation_events` (shown as the tools `Agent` and `Skill`). Per tool use id the live row wins, and a transcript call is counted only when no live row has that id, so the agent name (`custom` against `playbook:reviewer`) cannot double a call. Counts come from all calls. Error rate uses calls with a known result and the averages use calls with a known duration, because a transcript has neither. A `~` after the call count means some calls are transcript counts, and `-` means the figure is unknown for that row. The help window explains the mark. Other tools such as Bash have no transcript source, so they appear only for live days. The code is `telemetry_db::tool_stats`.
- **Health** shows API errors with their rate, retries, tool errors with their rate, active time (busy time when no metric arrived), API requests, lines changed, commits and pull requests, each against the previous period, and the cache hit rate. The cache hit rate is cache read tokens over all input side tokens and comes from the transcripts, so it shows with telemetry off too. Only a few rows fit in a small terminal and the table scrolls.
- **Retries.** An API error with attempt 2 or more is a failure that came after a retry, and that is what `Retries` counts. Claude Code exports no plain retry counter in the log events. A 404 for an unknown model was seen with attempt 1, so it is an error and not a retry. `api_retries_exhausted` did not appear in a trial and is not read.
- **Active time and counters.** The receiver also reads four delta counters from `/v1/metrics`: `active_time.total` (user and cli seconds, added up), `lines_of_code.count`, `commit.count` and `pull_request.count`. Only the `session.id` and `type` attributes are read, and a cumulative sum is skipped because adding it up would count it again. A point is keyed by session, metric, type and exact interval, so a redelivered point is stored once (table `telemetry_metrics`). Claude Code sends metrics every 60 seconds by default, so the last minute of a session can be missing. Health shows Active time when any was collected and otherwise Busy time, the sum of API call time and tool call time, which is a floor and not the metric. Cost and token metrics are not stored, since the `api_request` events carry them with a key.
- **Sources and empty state.** Every figure except the cache hit rate and the Tools counts is telemetry only. The Health panel says `Telemetry off` when the range has no telemetry rows, and its title note reads `source telemetry`. The Tools panel says it only when the range has no tool rows at all, and its note reads `source telemetry`, `source transcripts` or `source telemetry + transcripts` by what the rows are. Periods follow the chosen time zone, like the rest of the view.
- **Not verified.** Tool names of MCP tools and plugin tools, which Claude Code may redact without the tool details flag, and the attempt numbers of a real overload (an unknown model was the trial case).

## Checking and running the collector

`playbook usage status` prints whether a collector is running (pid, port, start time and idle limit), how many telemetry rows are stored (API requests, tool calls, API errors, metric points), the retention limits and the time of the newest event with its age. `--json` prints the same fields. Like most playbook commands it can start the collector first (see below). It creates no database when there is none, never prints the bearer token, and exits 0 whether or not a collector runs (a script reads the `running` field). With no collector running it shows the last port used.

**Starting it for you.** When the settings that `setup` wrote point at the collector, playbook starts `playbook usage collect --idle-seconds 0` itself, detached in its own session, from the `session-init` hook (wired by the hooks part of `setup`), the two hooks wired only on `UserPromptSubmit` (`auto-model-detect` and `context-hygiene`) and most playbook commands, `cc` included. These start nothing: `--version`, `version`, `doctor`, `update`, `uninstall`, `migrate`, `setup` and its old name, `state`, the tool hooks, `statusline`, `json`, `commands` and `usage collect`, `warm`, `prune` and `stop`. A headless run (`PLAYBOOK_HEADLESS` or `CI`) and `usage.telemetry` set to `off` start nothing either.

- A running collector holds `collector.lock`, so it costs a starter one open and one `try_lock`, and parallel starts leave one collector.
- The session hook prints one line when this session will not export telemetry. It reads `CLAUDE_CODE_ENABLE_TELEMETRY` from its own environment, because Claude Code removes every `OTEL_*` variable from the environment it gives to hooks. A session whose switch is not on started before setup wrote the keys (Claude Code reads `env` once, at start), so the line says to restart. An interactive session with `IS_DEMO` set to anything but `0` gets one line that names `IS_DEMO`, because Claude Code sends no telemetry from interactive sessions while it is set. A `claude -p` session (its `CLAUDE_CODE_ENTRYPOINT` starts with `sdk-`) hears nothing about `IS_DEMO`, since it still exports. The hook sees only the switch, so a session that started with the switch on but before setup added the `OTEL_*` keys hears nothing. It uses the consent check that starts the collector (`telemetry_points_here`: the user settings files with the home as the working directory, plus `usage.telemetry` and `collector.json`), so a repo's own settings are not read, and it is silent when `usage.telemetry` is `off`, when nothing is set up, in a headless run, and when the switch is on with no blocker. Both rules rest on Claude Code behaviour we observed, which Anthropic does not document.
- A spawn holds the next one off for 10 seconds (`collector.spawned`).
- The session hook waits at most 1.2 seconds for a new one and replaces a collector that holds the lock but does not answer. A prompt or a command waits for nothing.
- A collector from another playbook version is asked to stop and replaced. `playbook usage stop` stops it.
- The collector logs to `collector.log` next to `collector.json`. For a `cc` session with no telemetry setting of yours, `cc` points the session at a collector that already runs.

**After a reboot.** Nothing starts the collector at login. Telemetry from sessions that run before the next playbook command or hook is dropped, and those sessions are read from transcripts. A plain `claude`, the IDE or the desktop app with the hooks part off starts nothing. Earlier setups installed a launchd agent or a systemd user unit for this. The `collector-service-removed` migration removes it, and `playbook doctor check` warns while a service file is still installed. See [ADR 0048](../adr/0048-collector-daemon-started-by-commands.md).

To run it yourself, start it in a detached tmux session. On Windows playbook does not start it, so this is the only way there. The idle limit is off here so it does not stop in a quiet hour:

```
tmux new-session -d -s playbook-collect 'playbook usage collect --idle-seconds 0'
playbook usage status
tmux kill-session -t playbook-collect
```

`playbook usage collect` stays in the foreground, so any tool that runs a command in the background can run it (`nohup playbook usage collect --idle-seconds 0 &` also works). A second start reports the running collector and exits, so the command is safe in a shell profile or a tmux startup script.
