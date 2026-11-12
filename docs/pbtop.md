# pbtop

`pbtop` is a terminal dashboard for people who use Claude Code. It shows token use, estimated cost and your usage limits by day, model, project and session.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/pbtop/charts-dark.svg">
  <img src="images/pbtop/charts-light.svg" alt="pbtop Charts tab: spend per day, spend per day by model, tokens per day, spend per hour and a burn-up panel, sample data">
</picture>

## When to use it

- You want to see what your sessions cost, and which models and projects cost most.
- You want to know how far along you are against a Claude plan limit or a budget you set.
- You want a number for a script: `--json` and `--summary` print the same data without a terminal.

## Open it

```bash
pbtop
playbook usage
```

Both run the same code. `playbook usage` comes with Playbook. `pbtop` is the same dashboard as its own program: the Homebrew formula of `playbook` links it, and the [install guide](guides/00-install.md#optional-pbtop-alone) shows how to install it alone.

The terminal view opens on this week compared with last week. Without a terminal, or with `--summary`, it prints a text summary instead.

| Flag | What it does |
|---|---|
| `--range <name>` | Picks the range: `today`, `week`, `last-week`, `month`, `last-month`, `30d`, `60d`, `90d` or `all`. |
| `--summary` | Prints the text summary and exits. It covers every recorded session unless you give `--range`. It masks the account when the header does (`o` or `usage.maskAccount`). |
| `--json` | Prints the totals, the groups and the live view as JSON. The default range is `30d`. |
| `--utc`, `--local` | Group days and show times in UTC or in your local zone. Local is the default. |

## Views

The screen needs at least 80 columns and 24 rows. Below that it says so and draws nothing else. The pictures here use 150 columns and 44 rows, and show sample data.

Every tab has a header with the range, the cost, the tokens and the messages. For today, this week and this month it compares them with the previous period. The header also carries the budget, beside the comparison on a wide screen and under it on a narrow one. When it stacks, the comparison takes one row. The tab bar sits under the header. `F1` to `F3` pick a tab, and the default layout has three.

**Charts.** Every chart: spend per day and spend per day by model side by side, tokens per day and the Hourly chart side by side, with a burn-up panel below. The Hourly chart shows the cost of each of the last 24 hours with the total in its title. It has no data in the sample, because the sample sets no limit.

**Numbers.** The models table and the projects table. Each limit in the header also says when it reaches the limit at the current pace, so a separate Projection panel is gone.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/pbtop/numbers-dark.svg">
  <img src="images/pbtop/numbers-light.svg" alt="pbtop Numbers tab: models and projects under the header, sample data">
</picture>

**Sessions tab.** Live sessions, which have a message in the last 15 minutes, and every session of the range grouped by day.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/pbtop/sessions-dark.svg">
  <img src="images/pbtop/sessions-light.svg" alt="pbtop Sessions tab: live sessions and sessions per day, sample data">
</picture>

**Help.** `?` opens a window that lists every key. It scrolls when the screen is short.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/pbtop/help-dark.svg">
  <img src="images/pbtop/help-light.svg" alt="pbtop help window listing every key, sample data">
</picture>

**Layout editor.** `e` opens a window to add, move and remove tabs and panels. Each change applies and saves at once. The `usage.ui.layout` key holds the result.

Adding a panel takes one action: press `a`, choose the panel and press Enter. The panel lands in the next free place, and the tab arranges itself. A tab holds up to 8 panels, and the editor refuses a ninth with a message. The budget is not a panel: it is part of the header, which every tab shows, so the editor does not offer it. A saved layout that lists `budget` loads and skips the word; a tab left empty is dropped.

A tab with no row layout of its own draws this grid, which the editor edits as it is:

| Panels | Rows | Why |
|---|---|---|
| 1 | one full panel | The whole body for one panel. |
| 2 | two stacked, each full width | Live and Sessions are tables that want the width. |
| 3 | a pair, then one full width | Two small panels over one that gets the width. |
| 4 | 2 by 2 | Four equal panels. |
| 5 | 2, 2, then one full width | Rows of two, and the odd last one takes the row. |
| 6 | 2, 2, 2 | Three rows of two. |
| 7 | 2, 2, 2, then one full width | Four rows. |
| 8 | 2, 2, 2, 2 | Four rows of two. |

A tab keeps its own rows only when you set them with `f` (split or join a row) or when the stored value has a `|`. Then adding fills the last row that has room, opens a new row while the tab has fewer than 4, and otherwise fills the first row with room. Old stored layouts load and save as written.

A short screen keeps every row at 5 lines or more. When the rows do not fit, the tab draws the rows that do and hides the panels of the others. The footer then says how many panels are hidden and how many screen rows show all of them, for example `2 panels hidden, 28 rows show all`. The header is never left out, so the panels give way to it. At 150 by 44 every count from 1 to 8 fits.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/pbtop/layout-editor-dark.svg">
  <img src="images/pbtop/layout-editor-light.svg" alt="pbtop layout editor over the Charts tab, for arranging tabs and panels, sample data">
</picture>

**Budget window.** `b` asks for a manual monthly budget in dollars. Enter saves it to `usage.budget.monthlyUsd`, and `0` clears it.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/pbtop/budget-dark.svg">
  <img src="images/pbtop/budget-light.svg" alt="pbtop budget window over the Charts tab, asking for a monthly budget, sample data">
</picture>

**Full screen Sessions list.** `p` opens a table of recent sessions with their state, repo, branch, worktree, pull request link, last activity and cost. It uses the same records as `playbook sessions`. It only reads: nothing here changes a session. It asks `gh` for each branch, so it re-reads every 30 seconds instead of every 5.

## Keys

The table comes from the same list as the help window, so it lists every key.

<!-- keys:start -->
<!-- Generated by crates/cli/tests/docs_pbtop_keys.rs from the help window. Do not edit by hand.
Regenerate with: UPDATE_REFERENCE=1 cargo test -p playbook --test docs_pbtop_keys -->

**Navigation**

| Key | What it does | Where |
|---|---|---|
| `1 to 6` | Focus a panel by its number | all panels |
| `Tab` | Show the next tab | all panels |
| `Shift-Tab` | Show the previous tab | all panels |
| `F1 to F12` | Show a tab by its number | all panels |
| `]` | Focus the next panel of the tab | all panels |
| `[` | Focus the previous panel of the tab | all panels |
| `Down`, `j` | Select the next row | tables |
| `Up`, `k` | Select the previous row | tables |
| `PgDn` | Move five rows down | tables |
| `PgUp` | Move five rows up | tables |
| `Home`, `g` | First row, or first day of a chart | tables, charts |
| `End`, `G` | Last row, or last day of a chart | tables, charts |
| `Left`, `h` | Move the chart cursor one bar back | charts |
| `Right`, `l` | Move the chart cursor one bar forward | charts |

**Periods**

| Key | What it does | Where |
|---|---|---|
| `d` | Show today | all panels |
| `w` | Show this week | all panels |
| `W` | Show last week | all panels |
| `m` | Show this month | all panels |
| `M` | Show last month | all panels |
| `a` | Show all time | all panels |
| `r` | Switch between today and this week | all panels |
| `z` | Switch between local time and UTC | all panels |

**Views**

| Key | What it does | Where |
|---|---|---|
| `v` | Next view of the focused panel | Spend, Projects |
| `t` | Next color theme (off with NO_COLOR) | all panels |
| `o` | Mask the account email, and save it | all panels |
| `p` | Open the full screen Sessions list | all panels |
| `e` | Edit the tabs and the panels on each | all panels |
| `?` | Show this help | all panels |
| `i` | Toggle every limit's figures and notes | header |

**Sorting**

| Key | What it does | Where |
|---|---|---|
| `s` | Sort by the next column | tables |
| `S` | Reverse the sort order | tables |

**Filtering**

| Key | What it does | Where |
|---|---|---|
| `u`, `U` | Next account; U shows all, or the newest | all panels |
| `x` | Next machine, then all machines | all panels |
| `/` | Type a filter on any field | all panels |
| `Enter` | Filter to selected row; again to undo | tables 3-6 |
| `c` | Clear the filter | all panels |

**Refresh**

| Key | What it does | Where |
|---|---|---|
| `R`, `B` | Read usage and budget limits now | all panels |
| `b` | Set the manual monthly budget | all panels |
| `X` | Delete usage history, asks to confirm | all panels |

**Quit**

| Key | What it does | Where |
|---|---|---|
| `q`, `Esc` | Quit | all panels |
| `Ctrl-C` | Quit, also while typing a filter | all panels |

**Marks**

| Key | What it does | Where |
|---|---|---|
| `~` | Tools: count only, from transcripts |  |

<!-- keys:end -->

## Where the numbers come from

- **Claude Code transcripts.** The session files under `~/.claude/projects`, or under `$CLAUDE_CONFIG_DIR/projects` when that variable is set. Every total comes from these.
- **Codex rollouts.** The files under `~/.codex/sessions`, read the same way.
- **The OpenTelemetry receiver.** `playbook usage collect` listens on `127.0.0.1` only, on port 47318 by default. If another program holds that port on the first run, it picks a free one and keeps it. It feeds the cost per agent and per skill, tool timings and the health view. pbtop reads a session that sends nothing to it from transcripts. Totals ignore the receiver's rows.

Unless you turn on usage sync, nothing leaves your machine. The receiver stores no prompts, tool content, paths, emails or user ids.

## The store

Both tools keep their data in one file: `~/.config/playbook/data/usage.db`. It holds the events read from transcripts, one row per assistant message, with the model, the effort, the repo, the branch, the account, the token counts and the cost. It also holds tool calls, the telemetry tables, the machine row and the tables that [usage sync](guides/17-sync.md) fills.

The file has a schema version, stamped when it opens. `SCHEMA_VERSION` in `crates/telemetry/src/db.rs` is the current one, and an older build refuses a database that a newer one stamped. Events stay after Claude Code prunes its old sessions. To wipe the history, press `X`. It asks first, and the next read starts over from the transcripts.

## Machine id

Each row carries the id of the computer that wrote it. The id is a random version 4 UUID, made from 16 random bytes on first use and kept in the one-row `machine` table of `usage.db`. It does not come from a path or a hostname, so renaming the machine keeps it. The name defaults to the hostname and shows as the machine name in the dashboard.

```bash
playbook usage machine
playbook usage machine --name laptop
```

The first line shows the id and the name. The second sets a name, and an empty name restores the hostname.

## Cost math

Transcripts carry no cost, so pbtop prices every message from a price table in `crates/telemetry/src/pricing.rs`. A person copied the table from the published pricing page, and the file says when. For one message:

```text
cost = (input x in + output x out
      + cache write 5 min x in x 1.25 + cache write 1 hour x in x 2
      + cache read x in x read share) / 1,000,000
```

Here `in` and `out` are the model's dollars per million input and output tokens, and `read share` is its cache read price as a share of `in`. One model, Haiku 5.5, charges five times the base price when the prompt passes 100,000 tokens.

The cost is an estimate at API prices. It is not what a subscription plan bills. pbtop does not guess the price of a model that is not in the table: its messages count as $0, and `--summary` and `--json` name how many and which models.

## Limits

The budget part of the header compares your use with a limit. It has up to four rows, three on a narrow screen, and only the rows with data are drawn. `i` shows all of them. The `N notes, i` border hint shows only when `i` reveals them.

```
Monthly  c ████│█████████░░░░░░░░░░░░░░░░░░░   24%  $120 of $500  ▲ hits it in 6d 13h  resets in 27d (Thu 10-01)
Monthly    left $380   pace +17pt   allow $0.57/h $13.61/d   proj $1,736   resets Thu 10-01 00:00 UTC   cached 10m ago
Spend    h this week $3.05 █████░░░░░  last week $6.21 ██████████  -51%  avg $0.04/h $0.99/d
```

Each limit is one row: its name, a letter for its source, a bar that grows with the screen, the percent used, the amount, a verdict and the time left. The bar reddens as it fills. The `│` mark on the bar (`|` without UTF-8) is where you should be by now, the share of the period that has passed. A bar that runs past the mark is ahead of pace and a bar that stops short of it is behind. When the screen is narrow, pieces drop in this order: the date, the amount, the countdown, the time to the limit. The bar, the percent and the verdict stay.

The verdict is a mark and a word, colored by state. The marks are `▲`, `◆`, `●` and `○`, and `!`, `~`, `+` and `?` without UTF-8, so the state reads without color.

| Verdict | Meaning |
|---|---|
| `reached` | The limit is used up. |
| `hits it` | At the average pace the limit is reached before the period ends. The time follows, as in `hits it in 2d 4h`. |
| `close` | The projected total lands between 80 percent and the limit. |
| `ok` | The projected total stays under 80 percent. |
| `too early` | Under one hour of the period has passed, so there is no projection. |

After the verdict, `safe` means neither pace reaches the limit before it resets. `hits it in 7d (24h)` means the pace of the last 24 hours reaches it even though the average does not. `stale 7h` says the reading is old.

The line under the worst limit holds the figures that need a table, in columns: `left`, `pace` (the points ahead or behind the mark), `allow` per hour and per day, `proj` (the projected total), `avg` per hour and per day, the full reset time with its time zone, and where the number comes from. Press `i` to show that line for every limit, with the full text of any notes where the spend line was.

The spend line compares the spend of the period with the previous one over the same elapsed span, as two bars on one scale, the change in percent and the average rate.

The letter after the name says where the limit comes from:

- `m`, measured: the 5 hour and weekly windows of a Pro or Max plan, from the `rate_limits` data that Claude Code passes to the Playbook statusline. Behind a Claude apps gateway it can also hold a dollar limit.
- `c`, cached: usage numbers that Claude Code keeps in its own config file, with how old they are. Only a few plan and usage fields are read.
- `u`, manual: the dollars you set in `usage.budget.monthlyUsd` or `usage.budget.weeklyUsd`.
- `h`, local history: the spend line, estimated from your session transcripts.

The figures line spells the source out, for example `manual, local` or `cached 1d 22h ago, plus this machine since`.

A limit that Claude Code reports wins over a manual budget. A manual monthly budget covers the calendar month in your chosen time zone, and the spend is the estimated cost from your history. A weekly budget starts on the day in `usage.weekStart`. With no limit at all, the header shows an empty bar and says to press `b`.

Notes about the numbers go in one dim line, or in the bottom border of the header as a count when there is no room for the line. They are a missing 5 hour or weekly window, a manual budget beside a detected one, and the figure Claude Code last reported. Press `i` to read each in full. `--json` carries the same text.

The time to the limit needs at least one hour of elapsed time in the period. The [internals page](internals/04-usage-dashboard.md#the-math) has the math, and `playbook usage doctor` lists which limit sources exist on this machine.

## Refresh

A background thread owns the database. It reads new transcript lines and re-reads the store every 5 seconds. It also reads at once when you change the range or the filter, and when you press `R` or `B`. A refresh keeps your cursor, focus, sorts and selected rows. The footer shows when the last read happened. A first read of a large history runs in small batches, and the screen shows the store as it grows.

Live means a session whose last message is no older than 15 minutes.

## Settings

| Key | Default | What it does |
|---|---|---|
| `usage.theme` | `default` | The color theme. Choices are `default`, `spectrum`, `dark`, `light` and `mono`. `t` cycles them. |
| `usage.weekStart` | `monday` | The first day of the week. |
| `usage.timeZone` | `local` | `local` or `utc`. |
| `usage.telemetry` | `auto` | `off` stops Playbook from pointing sessions at the receiver. |
| `usage.budget.monthlyUsd` | `0` | A manual monthly budget. `0` means none. |
| `usage.budget.weeklyUsd` | `0` | The same for the week. |
| `usage.ui.layout` | empty | The tabs and panels, up to 8 panels per tab. Empty means the three default tabs. |

Set one with `playbook config set --global usage.theme dark`. The [config keys page](reference/config-keys.md) has the rest. Three environment variables matter too:

- `PLAYBOOK_TELEMETRY_TZ` sets the zone, `local` or `utc`. A flag beats it, and it beats `usage.timeZone`.
- `NO_COLOR` forces the `mono` theme.
- `CLAUDE_CONFIG_DIR` moves where transcripts are read from.

## pbtop on its own

The standalone program reads the same `usage.db` and the same settings. It does not start a collector. It reads transcripts and any telemetry rows that a collector already wrote to `usage.db`. It has no session records, so the full screen Sessions list is empty. To collect telemetry on such a machine, run `pbtop collect` yourself.

## More machines

Usage sync is an opt-in preview. It copies usage rows between your own machines through a database you own. See the [sync guide](guides/17-sync.md). Once other machines have synced rows, the dashboard reads a merged view of your own rows and theirs. Spend, tokens, models, budget, projection and burn-up count every machine. The live view, agents, skills, tools, health, sessions and projects follow the machine you pick. `x` picks the next machine and then all machines.

## Troubleshooting

- The numbers look low or stale. Run `playbook usage status` to see whether the receiver runs and how many telemetry rows exist.
- A limit looks wrong. Run `playbook usage doctor` and share its output. It prints field names and ages, with no email, name or token.
- The screen is blank or says it is too small. Make the window at least 80 columns by 24 rows.
- A model shows $0. It is not in the price table. `--summary` names it.

## Next

- [Recipe: watch your costs](recipes/watch-costs.md)
- [Config keys](reference/config-keys.md)
- [Dashboard internals](internals/04-usage-dashboard.md)
