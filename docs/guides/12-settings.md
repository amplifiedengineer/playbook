# Settings

Settings live in one SQLite store, `~/.config/playbook/data/config.db`, next to every other database playbook keeps (see [the data folder](../internals/11-data-folder.md)). Settings resolve from repo, then org, then global, then the default.

```bash
playbook config list                                  # every key, value and source tier
playbook config set --global pr.review.fix true      # fix confirmed review findings, then push
playbook config set --global pr.merge.enabled true   # mark ready, wait for green checks, merge
playbook config set --global pr.draft false           # open PRs ready for review
playbook config export > playbook-config.json         # back up; load with `config import`
playbook config set --global routing.escalate ask      # ask before the top model runs (default is auto)
playbook state list                                   # inspect playbook's own bookkeeping
```

Every key is in [Config keys](../reference/config-keys.md). Every command and flag is in the [CLI and config reference](../reference/cli-and-config.md).
