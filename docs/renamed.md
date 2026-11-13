# Renamed and removed commands

Deprecated: a renamed command keeps its old name as a stub until 1.0.0. The stub prints one line that names the new command, then runs it, so a saved note or prompt keeps working. The stub is removed in 1.0.0, and that removal is a breaking change. A command removed with no replacement has no stub.

| Old name | New name | Removed in |
| --- | --- | --- |
| `/playbook:commit-and-push` | `/playbook:commit` | 1.0.0 |
| `/playbook:create-pull-request` | `/playbook:pr-create` | 1.0.0 |
| `/playbook:address-pr-comments` | `/playbook:pr-comments` | 1.0.0 |
| `/playbook:quick-review` | Removed, use `/playbook:review --quick` | Removed, no stub |
| `/playbook:deep-review` | Removed, use `/playbook:review --deep` | Removed, no stub |
| `/playbook:handoff-save` | Removed, use `/playbook:handoff` | Removed, no stub |
| `/playbook:handoff-show` | Removed, use `/playbook:handoff load` | Removed, no stub |
| `/playbook:learn-project` | `/playbook:learn` | 1.0.0 |
| `/playbook:repo-audit` | `/playbook:audit` | 1.0.0 |

`/playbook:session-handoff` and `/playbook:session-start` are not renamed. They stay as aliases that run `/playbook:handoff` and `/playbook:handoff load`.
