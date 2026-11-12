# Code index evaluation

`playbook code` answers symbol and refs questions from a Tree-sitter index (see the [Tree-sitter ADR](../adr/0060-use-tree-sitter-only-for-the-code-index.md)). The evaluation asks one question: does a model answer code questions as well with the index as without it, while using fewer tokens? The harness is in `crates/code-bench/src/e2e/` and runs weekly from `.github/workflows/code-e2e.yml`.

## How a run works

The runner draws up to 30 tasks from the frozen task set. Each task asks where a symbol is declared. For every task and every one of 5 repeats the model answers twice, once without the index and once with it. The two arms always hold the same tasks and the same repeat count.

The model is pinned by a dated snapshot id and the request carries a fixed temperature. The model reaches the code through the installed `claude` CLI. The with-index arm may run `playbook code`, and the other arm may only read and search files.

A grader with no model scores each answer. The answer must be a JSON list of path and line. It scores when one entry has the expected path, after normalizing slashes and a leading `./`, and a line within 3 lines of the expected one. A task with no oracle answer is left out and counted as excluded.

## Cost cap

A cost meter reports the spend in whole cents. The run stops before a trial when the spend is above 5,000 cents, which is 50 USD. A run cut short is marked partial and never counts toward a decision.

## The acceptance rule

The rule works in integer basis points. The margin is 3.0 accuracy points, or 300 basis points. Accuracy may fall by up to that margin when tokens fall. All of these must hold:

- The report is complete, and both arms ran the same number of trials.
- The lower bound of the 95 percent interval on the accuracy difference is at least minus 300 basis points.
- The point estimate of the difference is at least 0.
- Tokens per task fall with the index.
- No query kind with at least 5 tasks loses more than 300 basis points on its point estimate. A kind with fewer tasks is listed and does not block.

`code-bench decide REPORT_FILE` applies the rule and prints the value for `playbook plan brief --facts-tokens`, which is 1,500 when the rule holds and 0 otherwise. It only reports. Changing the default is a committed change that follows a real report.

## Report figures

The report is JSON written by `code-bench e2e`. The table lists its figures as they appear in the committed fixture `crates/code-bench/data/report-fixture.json`. That file is a fixture and not a measured result: its numbers only let the docs test check this page. The test `crates/code-bench/tests/docs_numbers.rs` fails when a figure here differs from the fixture.

| Figure | Fixture value |
|---|---|
| Tasks scored | 20 |
| Tasks excluded | 2 |
| Repeats per task and arm | 5 |
| Trials per arm | 100 |
| Spend in cents | 1200 |
| Accuracy with the index, basis points | 8000 |
| Accuracy without the index, basis points | 8200 |
| Difference, basis points | -200 |
| Lower bound, basis points | -700 |
| Upper bound, basis points | 300 |
| Variance across repeats with the index | 400 |
| Variance across repeats without the index | 500 |
| Tokens per task with the index | 4200 |
| Tokens per task without the index | 6100 |

The fixture does not meet the rule: its lower bound of -700 is below -300. The default stays 0 for it.

## Status

No real report exists yet. A real run needs a model key and real oracle answers, so `--facts-tokens` stays at its default of 0. The owner adds the `code-e2e` environment with the secret `CODE_E2E_ANTHROPIC_API_KEY`, sets the repository variable `CODE_CI_PILOT_GATE_RAN` and reads the first report. When the rule holds on a committed real report, a separate change sets the default to 1,500.

The `claude` CLI has no temperature option, so the request carries the fixed temperature for tests to assert, but a real call cannot set it.
