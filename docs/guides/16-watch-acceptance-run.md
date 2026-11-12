# Watch mode acceptance run

This is the run that proves `playbook watch` end to end: three rules, a full day, one laptop sleep, each event handled once and the audit log complete. It writes to a real GitHub repository and takes a day of wall time, so a person runs it. Record the result in the pull request that adds watch mode.

Use a repository you own and can fill with test issues, for example `YOU/watch-acceptance`. Do not use a repository other people work in.

## Before you start

```sh
gh auth login            # watch finds the token through gh when GITHUB_TOKEN is not set
playbook watch status    # expect: enabled: false
```

Set up three rules. The first two run on their own, the third proposes.

```sh
playbook config set watch.repos '["YOU/watch-acceptance"]' --global
playbook config set watch.autonomy auto --global
playbook config set watch.rule.label-new '{"on":"issue.opened","do":"label","autonomy":"auto","with":{"labels":["triage"]}}' --global
playbook config set watch.rule.thanks '{"on":"issue.opened","do":"comment","autonomy":"auto","with":{"text":"Thanks, we will look."}}' --global
playbook config set watch.rule.pick-label '{"on":"issue.opened","do":"triage_issue","decide":"model","with":{"labels":["bug","docs"]},"limits":{"usd_per_day":1}}' --global
playbook config set watch.enabled true --global
playbook watch dry-run
```

Expected: `dry-run` lists the three rules with their level and prints no write. Create the labels `triage`, `bug` and `docs` in the repository first, or the label calls fail.

## Run

1. Start the loop in a terminal you will leave open, and note the time: `playbook watch`.
2. Or install the timer instead (one of the two, not both): `playbook watch install-service`, then `playbook doctor check` and expect the row `watch timer installed`.
3. The first poll of each source prints `watching from now` and queues nothing. Wait for it before creating issues.
4. Create 6 issues over the day, a few in the first hour and the rest spread out. Title and body can be anything.
5. Close the laptop lid for at least 2 hours in the middle of the day with the loop (or the timer) installed, then open it. Create 2 more issues while it is asleep, from a phone or another machine.
6. Leave it running until 24 hours have passed.

## Check

For each of the 8 issues:

```sh
playbook watch log --json
playbook watch status
gh issue view N --repo YOU/watch-acceptance --json labels,comments
```

Expected:

- Each issue got the `triage` label once and exactly one comment with the text `Thanks, we will look.` (the `label-new` and `thanks` rules). A second run of the loop adds neither.
- The `pick-label` rule left a proposal per issue (`proposed` in `watch status`) and wrote nothing to GitHub. Approve one with `playbook watch approve ID` and expect `item ID done` and one more label from the list. The class stays held by the trust ladder until 10 approvals over 28 days, so a rule set to `auto` would still propose.
- The 2 issues created during the sleep are handled after the wake-up, with no `stale` record, because the gap is under `watch.catchup.maxHours`.
- `playbook watch status` shows no dead items, and `today` is under the limits.
- `playbook watch log` holds one record per decision: for every issue and rule an outcome (`queued_auto`, `proposed`, `done`), nothing missing and nothing twice.

## Record

Paste these into the pull request:

- The date, the playbook version (`playbook version`) and whether you used the loop or the timer.
- The output of `playbook watch status`.
- The output of `playbook watch log --json | wc -l` and the expected count (8 issues times 3 rules is 24 decisions, plus the `done` records of the two automatic rules).
- For each issue, the label and comment counts from `gh issue view`.
- Anything that differed from the expected lines above.

## Clean up

```sh
playbook watch uninstall-service       # only if you installed the timer
playbook config set watch.enabled false --global
```

The audit log stays until you add `--delete-log` to `uninstall-service`.
