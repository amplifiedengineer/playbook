# pbtop

`pbtop` is the Agent Budget Dashboard of playbook as a command of its own. The Cargo package is `playbook-telemetry`.

```sh
brew install amplifiedengineer/tap/pbtop
pbtop
```

A token and cost dashboard for Claude Code sessions. It reads the session history Claude Code already keeps on your machine, prices it, and shows it in a terminal view or as a text or JSON report. Everything stays local.

It is part of the [playbook](https://github.com/amplifiedengineer/playbook) toolkit and also works on its own. If you use playbook, you do not need to download anything here: `playbook usage` runs this same code.

## What it shows

- Cost and tokens per day, model, repo, branch and session, with a week over week comparison.
- Cache read and write tokens, split into the five minute and one hour tiers.
- Tool, skill and agent breakdowns and latency, when Claude Code telemetry is on (see the receiver below).
- Tabs (Charts, Numbers, Sessions by default) with the panels you choose. Press `e` to edit the layout, `Tab` and `F1` to `F12` to switch tabs. The header, with the budget in it, stays on every tab.
- The budget, in the header, with the rate limits and, beside each limit, when it is reached at the current pace, and an Hourly chart of the cost of the last 24 hours. The limits come from the playbook statusline (Pro and Max windows), from the usage numbers Claude Code caches in its config file (the 5 hour and weekly windows, or the monthly spend limit on Enterprise, shown with its age), or from a budget you set.
- Your last tab, range, views, sorts and filter, restored on the next start.
- Codex session history, read the same way.

## Install

From playbook 0.35.0, with Homebrew (macOS and Linux):

```sh
brew install amplifiedengineer/tap/pbtop
```

Since Homebrew 6.0.0 a tap that is not official needs trust. The fully qualified name above trusts only that formula. The install records that trust. To install by short name without the full name first, run `brew tap amplifiedengineer/tap` and `brew trust --formula amplifiedengineer/tap/pbtop`. To trust the whole tap, run `brew trust amplifiedengineer/tap`.

Or with the install script, which checks the archive's checksum, its build attestation when `gh` can check it, and `pbtop --version`:

```sh
curl -fsSL https://raw.githubusercontent.com/amplifiedengineer/playbook/main/install-pbtop.sh | sh
```

Or download `pbtop-<version>-<target>.tar.gz` for your platform from the [latest release](https://github.com/amplifiedengineer/playbook/releases/latest): macOS (arm64 and x86_64), Linux (arm64 and x86_64, static musl) or Windows (x86_64). `SHA256SUMS` and the build provenance attestations come with it.

```sh
sha256sum -c --ignore-missing SHA256SUMS
gh attestation verify pbtop-<version>-<target>.tar.gz --repo amplifiedengineer/playbook
tar -xzf pbtop-<version>-<target>.tar.gz -C ~/.local/bin
```

Or build from source with the Rust version in `rust-toolchain.toml`:

```sh
cargo build --release
./target/release/pbtop
```

## Use

```sh
pbtop                 # terminal view, this week against last week
pbtop --summary       # text summary and exit
pbtop --json          # the same numbers as JSON
pbtop --range 30d     # today, week, last-week, month, last-month, 30d, 60d, 90d, all
pbtop status          # is the telemetry receiver running
pbtop collect         # run the local OpenTelemetry receiver in the foreground
pbtop prune --dry-run # say what old telemetry rows would be removed
pbtop doctor          # where the limits can be read on this machine (add --raw for the plan field values)
```

Times and days use your local time zone by default, daylight saving included, and a day runs from local midnight to local midnight. Every view says which zone it uses: the footer of the terminal view reads `local time (UTC+10)` or `UTC`, the text summary opens with `Times and days are in ...`, and `--json` carries a `time_zone` object with the `mode`, the `offset_seconds` and a `label`. To use UTC instead, pass `--utc` (or `--local` to force the default), set `PLAYBOOK_TELEMETRY_TZ=utc`, or run `playbook config set usage.timeZone utc`. The flag beats the environment variable, which beats the setting. A value that is not `local` or `utc` is ignored with a warning. Weeks start on Monday unless `usage.weekStart` says otherwise.

It finds Claude Code history in `~/.claude/projects`, or in `$CLAUDE_CONFIG_DIR/projects` when that variable is set.

## Limits on Enterprise and other plans

The budget in the header names where each limit came from: `measured` (the statusline, `measured, stale` when the reading is over six hours old), `cached` (the Claude Code config file, with its age; over six hours old it reads `cached ... ago, plus this machine since`: the amount Claude Code read stays as the floor and this machine's messages after the read are added, so other machines and the web apps since the read are missed; with no amount in the cache it reads `limit cached ... ago, used is this machine only`), `manual, local` (`manual, all machines` once synced rows exist) (the `usage.budget.*` keys, counted from this machine's transcripts, so it covers this machine and the local month, and never matches a percentage the Claude usage page shows for an organization limit). When the bar is a manual budget or a stale cache, a line under it says what Claude Code last reported as used this month and how old that is; `/usage` in Claude Code refreshes it. Whether that figure covers the whole organization is not documented, so it is never called the organization total. A detected monthly limit (statusline, or cache however old) wins over `usage.budget.monthlyUsd`, which is used only when nothing is detected. The cached numbers are not documented by Anthropic and can change. When a plan has no 5 hour and weekly windows, the header says so and does not show zeros. The plan comes from the rate limit tier names (Max) and the organization fields (Enterprise). Anything else shows as "Unknown plan".

Run `pbtop doctor` to see which sources exist on a machine. It lists field names, ages, the plan label, the window percentages and the manual budget amounts, and never an email, name, id, organization or token. `pbtop doctor --raw` adds the values of six plan fields and the cached spend limit.

## Where it keeps its data

The tool shares its data and settings with playbook, so both show the same numbers.

All playbook tools keep their databases in one folder, `~/.config/playbook/data`, whichever tool runs. This tool's file is `~/.config/playbook/data/usage.db` (folder `0700`, file `0600`). It is created on first use, so every command works on a new machine. Lock and receiver files sit next to it. Several processes can use it at once (the terminal view and the receiver); it runs in WAL mode with a busy timeout.

When an older database exists, the first run brings it in, and the view never prints while it is drawn:

- `~/.config/playbook/usage.db` (playbook 0.25, playbook-telemetry 0.1.5) and `~/.config/playbook/usage/usage.db` (older playbook) move into place with their `-wal` and `-shm` files, or merge when the data file already exists.
- `~/.local/state/playbook-telemetry/usage.db` and `~/.local/state/playbook-usage/usage.db` (older standalone releases, also under `$XDG_STATE_HOME`) are merged in one transaction, keyed on each table's primary key.
- Merged files are renamed to `*.migrated`. Nothing is deleted. One line on stderr says how many rows were merged, and only when some were.
- If an older playbook keeps writing to the old path, its new rows are merged quietly on later refreshes, and one line at exit asks you to upgrade it (`brew upgrade playbook` or `playbook upgrade`).
- A file that cannot be read is skipped with a warning and left untouched.

Settings are playbook's own, from the global tier of its config store (`~/.config/playbook/data/config.db`), and from the nested `usage` object of `~/.config/playbook/config.json` when that store does not exist. It reads them, and writes only `usage.ui.layout` and `usage.ui.state` to its own app tier of `~/.config/playbook/data/config.db`, and only when that store exists. Set them with `playbook config set usage.<key> <value>`. An invalid value is ignored with a warning, and a missing one means the default. An old `~/.config/playbook-telemetry/config.json` is no longer read; the tool names it on stderr if it exists.

| Key | Default | Values |
| --- | --- | --- |
| `usage.theme` | `default` | `default`, `spectrum`, `dark`, `light`, `mono` |
| `usage.maskAccount` | `false` | true or false |
| `usage.weekStart` | `monday` | `monday`, `sunday`, `saturday` |
| `usage.timeZone` | `local` | `local`, `utc`. Under `playbook`, a `playbook config` key. Pbtop reads it only from the `usage` object of `config.json`, and only when no config store exists |
| `usage.telemetry` | `auto` | `auto`, `off` |
| `usage.budget.monthlyUsd` | `0` | number |
| `usage.budget.weeklyUsd` | `0` | number |
| `usage.ui.layout` | empty | text like `Charts=spend.total,spend.model\|tokens,hourly\|burnup;Numbers=models,breakdown;Sessions=live,sessions`; press `e` in pbtop to edit, changes save at once |
| `usage.ui.state` | empty | text written by pbtop: the screen setup it restores |

## Telemetry receiver

`pbtop collect` listens on 127.0.0.1 only and stores API request cost and token rows, tool call timings and API errors from Claude Code's JSON telemetry. Prompts, tool content, paths, emails and user ids are never stored. The receiver checks a bearer token. Its port and token are in `collector.json` in the state folder (owner-only; `pbtop status` shows the port and never prints the token). To point a Claude Code session at it, set `CLAUDE_CODE_ENABLE_TELEMETRY=1`, `OTEL_LOGS_EXPORTER=otlp`, `OTEL_METRICS_EXPORTER=otlp`, `OTEL_EXPORTER_OTLP_PROTOCOL=http/json`, `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:<port>` and `OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer <token>"` for that session. Playbook's `cc` launcher does all of this for you. Without the receiver, every total still comes from the session transcripts.

## Use it as a library

The crate `playbook-telemetry` (library `playbook_telemetry`, built from the `crates/telemetry` folder of the playbook repository) exposes the same code the binary runs. A program that embeds it implements the `host::Host` trait (config lookup, state folder, program name, repo layout, sessions panel data) and installs it once with `host::install` before calling `entry::run`. `entry::run` picks the time zone itself when `tz::init` has not run, so an embedder gets the same local-time default and the same `usage.timeZone` setting. Without an installed host the standalone one described above is used.

```toml
playbook-telemetry = { git = "https://github.com/amplifiedengineer/playbook", tag = "v<version>", package = "playbook-telemetry" }
```

Pin an exact tag (`telemetry-v<version>` for releases before 0.34.0). The API is still settling, so expect changes between minor versions until 1.0.

## History

Releases before 0.34.0 were tagged `telemetry-vX.Y.Z` in the playbook repository. From 0.34.0 they are the playbook `vX.Y.Z` releases.

## Develop

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --lib --bins -- -D warnings -D clippy::unwrap_used -D clippy::expect_used
cargo test -p playbook-telemetry
```

See [CONTRIBUTING.md](../../CONTRIBUTING.md) for the contribution rules and [SECURITY.md](../../SECURITY.md) to report a vulnerability.

## License

Apache-2.0. See [LICENSE](../../LICENSE).
