# Release channels

A release reaches users through these channels: the GitHub release assets (built by the `build` and `publish` jobs of `release.yml`), `install.sh`, which downloads those assets, the Homebrew tap `amplifiedengineer/homebrew-tap`, the Nix flake in `amplifiedengineer/nix` and the plugin marketplace `amplifiedengineer/marketplace`. Nothing else ships playbook: there is no AUR package, apt or dnf repository, scoop bucket or winget package. On Windows the route is the raw `.exe` release asset.

After a release is published and checked, the `downstream` job of `release.yml` opens a pull request in the tap and in the marketplace, as the section on the release workflow describes. The tap also checks every 30 minutes for a newer release and bumps a formula itself when the `downstream` job has not done it within an hour.

A release attaches five `playbook` binaries, five `pbtop` archives, five `pbsync` archives, four `pbdoc` archives (macOS and Linux only), `release-notes.json` and `SHA256SUMS`, which lists the other twenty. It builds no plugin archive. The plugin in `plugins/playbook` ships with the same `vX.Y.Z`, as described next.

## The plugin release

The binary and the plugin share one version line ([ADR 0038](../adr/0038-one-release-line.md)).

- The `version` in `plugin.json` equals the workspace version in `Cargo.toml`. `metadata.minBinaryVersion` is the `X.Y.0` of that minor line.
- The `version-sync` check in `plugin-guards.yml` fails a pull request or a push to `main` when they differ. The `plan` job of `release.yml` checks again before a `v` release.
- The release pull request runs `scripts/bump-version.sh X.Y.Z`, which sets all of them and the `PLAYBOOK_REF` pins together. A plugin change between releases needs no bump.

The marketplace repository `amplifiedengineer/marketplace` lists the plugin as a `git-subdir` source that points at `plugins/playbook` in this repository.

- The `downstream` job pins its `ref` to the release tag `vX.Y.Z` and sets `metadata.version` once the release is published.
- Claude Code copies only that folder at the tag and updates an installed plugin when the `version` at the new pin is higher.
- Until the first release that runs `downstream`, the entry has no `ref` and follows the default branch.
- There is no separate plugin release. The `playbook-plugin-v` tags made before 0.34.0 stay as history. The `payload-guard` check keeps the folder to markdown and the manifest.

## The release workflow

`.github/workflows/release.yml` releases playbook ([ADR 0039](../adr/0039-release-workflow.md)). A maintainer starts it from the Actions tab or `gh`:

```bash
gh workflow run release.yml -f bump=patch          # or minor, or major
gh workflow run release.yml -f version=0.35.0
gh workflow run release.yml -f bump=minor -f dry_run=true
```

`plan` runs `scripts/release-plan.sh`. It reads the versions on `main` and the `v` tags and picks where the run starts:

- `prepare` (the files are below the version): `scripts/release-prepare.sh` runs `scripts/bump-version.sh` and adds the `## playbook X.Y.Z` section to `CHANGELOG.md`. `scripts/release-gh.sh pr` opens the pull request `chore(release): playbook X.Y.Z` on `release/vX.Y.Z` with one commit that GitHub signs and turns on auto-merge. The job waits for the merge queue to land it.
- `tag` (`main` carries the version, the tag is missing): the job creates the lightweight tag `vX.Y.Z` on the release commit with the igorjs app token, which the "Protect release tags" ruleset lets through.
- `publish` (the tag exists): the run builds and publishes. If the release is already published, it only checks it.

### The macOS gate

Before the tag, `macos` calls `rust-ci.yml` to build and test the release commit on macOS. Pull requests and the merge queue test on Linux only, so this is the one macOS check every release must pass.

- It tests the release commit, or the tag when the run starts at `publish`. A dry run that starts at `prepare` tests the dispatch commit. `tag` and `draft` stop the run unless it passed on that same commit.
- The release pull request also builds and tests on macOS, so a macOS break stops the bump before `main`.

When the gate still fails on the release commit, land the fix and tag `vX.Y.Z` on the fixed commit by hand. Then run the workflow again with `version=X.Y.Z`: it starts at `publish` and tests the tag.

### After the tag

- `draft` creates a draft release with the notes and attaches `release-notes.json`.
- `build` compiles the five targets with `--locked` and attaches the `playbook` binary and `pbtop-X.Y.Z-<target>.tar.gz` and `pbsync-X.Y.Z-<target>.tar.gz` for each target. The four macOS and Linux targets also get `pbdoc-X.Y.Z-<target>.tar.gz`. Each build job first runs its binaries with `--version` (all but the cross built x86_64 macOS one), runs `playbook sync status` against the built `pbsync` in an empty home and expects exit 0 with sync off, checks the pbsync and pbdoc size budgets and attests the assets it built.
- `publish` checks every draft asset against that attestation, writes `SHA256SUMS` over every asset, attests it and the notes, and makes the release public and the latest.
- `verify` downloads the published release, checks the sums and the attestations of a run from `main`, and runs `--version` on the x86_64 Linux binaries.

Every step can run again: an existing pull request, tag or release is reused, a tag is never moved, and a published release is never rebuilt or uploaded again. After a failed run, run the workflow again with `version=X.Y.Z`. When GitHub refuses to create the tag, the run summary prints the signed `git tag` command to run by hand, and the next run with `version=X.Y.Z` goes on from the tag.

### Downstream: tap and marketplace

After `verify`, `downstream` moves the tap and the marketplace to the release ([ADR 0039](../adr/0039-release-workflow.md) decision 9).

- The tap's `.github/scripts/bump-formula.sh` runs for `playbook` and `pbtop` in a `formulas` job with no secrets and reads the release's `SHA256SUMS`. The marketplace `ref` becomes `vX.Y.Z` and `metadata.version` becomes `X.Y.Z`.
- Each change goes to a `release/vX.Y.Z` pull request titled `chore(release): playbook X.Y.Z`, made with `scripts/release-gh.sh pr` and the app token, with auto-merge on. When it merges, a workflow in that repository tags the merge commit `vX.Y.Z` and publishes a release.
- The job opens both pull requests first, then waits up to 30 minutes for both merges, and the run summary links them.
- When a repository has not published its release five minutes after its merge, the job tags and releases it through the API. It does this only when the merge commit changes nothing but the formulas or the marketplace file.
- When something here fails, the playbook release stays published, the job fails, and the summary prints the commands to finish it. Running the workflow again with `version=X.Y.Z` goes on from where it stopped.
- When `BOT_APP_CLIENT_ID` or `BOT_APP_PRIVATE_KEY` is not set, the job skips this step with a notice. The summary says to move the tap and the marketplace by hand or run again once the secrets are set.

When `downstream` has moved the tap (its `moved` output, not set when the app secrets are missing), `verify-homebrew-linux` runs `brew tap` and `brew install` for `playbook` and `pbtop` on an Ubuntu runner. It checks that `playbook`, `pbtop` and `pbsync` report the release version, then runs `playbook doctor check` in an empty `HOME`. It blocks no other job. A red result fails the run, and running again with `version=X.Y.Z` starts it again. A dry run skips it, because nothing is published to install.

### Nix flake

The flake lives in [amplifiedengineer/nix](https://github.com/amplifiedengineer/nix), with the `playbook` and `pbtop` outputs. After `verify`, the `nix` job moves it, the way `downstream` moves the tap:

1. It checks `SHA256SUMS` and the four Nix assets against their attestation.
2. `scripts/render-flake.sh` rewrites the `releases` block of `flake.nix` in a checkout of that repository (the version and one SRI hash per system).
3. The job opens `release/nix-vX.Y.Z` there titled `chore(release): nix flake X.Y.Z` with the app token, auto-merge on, and waits up to 30 minutes for the merge. Strict status checks hold back a pull request that fell behind `main`, so while it waits, `scripts/release-gh.sh wait` runs `gh pr update-branch` when its `mergeStateStatus` is `BEHIND`, at most once every four minutes. The `downstream` job does the same for the tap and marketplace pull requests. The wait for the release pull request in this repository does not, because that repository has a merge queue.

- `scripts/release-notes.sh` still leaves such a commit out of the notes, for the history from before the flake moved.
- The job skips when the release is not the latest or the flake is already ahead, so a backport does not move the flake back. Without the app secrets it warns and passes.
- The app token covers `amplifiedengineer/nix` only. When the igorjs app installation cannot reach that repository, the job fails and the summary names the owner step: add the repository to the installation, then run again with `version=X.Y.Z`.
- If the flake repository moved between the checkout and the commit, `release-gh.sh` fails with "Run again": run the workflow with `version=X.Y.Z`.
- A dry run renders from a stand-in `SHA256SUMS`, checks the token with read calls, shows the diff and opens nothing.
- The flake has no release tags, so users pin the merge commit of the flake pull request.
- To roll the flake back, open a pull request in that repository that reverts the flake change. The older hashes stay valid because release assets do not change.

Then `verify-nix-linux` and `verify-nix-macos` run `nix run github:amplifiedengineer/nix/<commit>#playbook -- --version` at the commit that carries the flake and compare it with the version. The Linux job also builds a copy with a wrong hash, which must fail. Both are leaf jobs, skipped in a dry run and when `nix` moved nothing. The flake repository also builds both outputs on Linux and macOS in its own CI.

### Dry run

A dry run plans, shows the release pull request diff in the run summary, and builds the five targets from `main`. It shows the tap and marketplace diffs (placeholder checksums for the formulas) and checks the app tokens with read-only calls. It opens, tags, uploads and publishes nothing.

### Release notes

`scripts/release-notes.sh` builds the body of the release from the Conventional Commits since the previous `v` tag that touch the playbook paths, the plugin included (ADR 0037).

The sections are Breaking changes, Features, Fixes, Performance and Other changes, in that order. An empty one is left out, and so is the release commit itself. The same notes go into `release-notes.json`, which `SHA256SUMS` lists and the workflow attests.

After `publish`, the `notes` job writes a plain-English summary with a model and puts it above the commit list in the release body: a few headings of prose, then `### All changes` with the list's own headings one level down. The summary must pass checks: only allowed headings, no HTML or mentions, links only into this repository, and `playbook prose check --strict` run with the binary of the release itself, which the release attests. A failed check gets one retry. Without the secret the job skips. The job does not block the release: the list and `release-notes.json` stay the record, and a failed or skipped `notes` leaves the body as it was. A rerun of `notes` rebuilds the body from `release-notes.json` and replaces any hand edit, and the owner can still edit the body by hand. A full rerun after `publish` stops at `draft`, so rerun the `notes` job itself. The model, its settings and the prompt live in `scripts/release-summary.sh` and `scripts/release-summary.prompt.md`.

No check fails a release for a breaking change: the notes list the commits marked breaking first. Releases no longer carry `breaking-changes.json` (ADR 0047). A 0.34 binary, the last that read it, treats the missing asset as a release with no breaking changes.

## Credentials

The workflow uses two tokens and one model key:

- `GITHUB_TOKEN`: read, build the draft and publish.
- igorjs GitHub App token: the release pull request, the tag and the `downstream` pull requests.
- `ANTHROPIC_API_KEY`: the one secret of the `release-notes` environment, read only by `notes`. Without it the job skips. The environment must have no required reviewers and no wait timer, or `notes` waits for approval after every release. The environment allows deployment only from `main`, so a workflow on another branch cannot read the key.

The app secrets and their guards:

- `BOT_APP_CLIENT_ID` and `BOT_APP_PRIVATE_KEY` are organization secrets that the playbook, tap and marketplace repositories can read.
- The jobs that read these secrets run in the `cicd` environment: `prepare`, `tag`, `downstream` and `nix` in `release.yml`, and `auto-update-pr.yml`. The Dependabot auto-merge job runs in the `dependabot` environment. The secrets stay shared. The environments are where protection rules go, and the workflow change alone adds none.
- Each step asks the app only for the permissions it needs. `downstream` mints one token per repository.
- The workflow runs only from `workflow_dispatch`, anything but a dry run only from `main`, and `downstream` only from `main`.
- Any workflow in those three repositories can read the key. The guards are the rulesets and the checks in each workflow: the downstream release workflows act only on a merged pull request from the app on a `release/v*` branch.
- An organization admin rotates the key and updates both secrets. Pull requests made with the app token run CI, which pull requests made with `GITHUB_TOKEN` do not.
- The app is on the bypass list of the "Protect release tags" ruleset, so it can create `v*` tags. The tag is a lightweight ref on the squash commit of the merge queue, which GitHub signs.

## Rolling back a release

A published release is not deleted and its tag is not moved, because installs and the tap may already point at them. To pull a bad release:

1. Mark it as a pre-release, so it stops being the latest: `gh release edit vX.Y.Z --prerelease --latest=false`. Then mark the previous release as the latest with `gh release edit vA.B.C --latest`. `install.sh` and `playbook upgrade` read the latest release.
2. Add a warning at the top of its notes, saying what is wrong and which version to use. `--notes-file` replaces the whole body, so start from the current notes: `gh release view vX.Y.Z --json body --jq .body > notes.md`, add the warning at the top of `notes.md`, then `gh release edit vX.Y.Z --notes-file notes.md`.
3. Move the plugin back: open a pull request in `amplifiedengineer/marketplace` that sets the `ref` of the `playbook` entry to `vA.B.C` and `metadata.version` to `A.B.C`. New installs then get the previous plugin. Claude Code never installs a lower plugin version over an installed one, so users who already updated keep the bad plugin until the next patch.
4. Move the formulas back: in `amplifiedengineer/homebrew-tap`, run `SOURCE_REPO=playbook TAG_PREFIX=v .github/scripts/bump-formula.sh A.B.C Formula/<formula>.rb` for `playbook` and `pbtop`, and open a pull request with the result.
   - The tap's scheduled check never moves a formula back to a lower version, and it skips a pre-release.
   - Before 0.35.0 the tap had no `pbtop` formula: for such an A.B.C, move only `playbook` this way and remove `Formula/pbtop.rb` by hand.
5. Ship the fix as the next patch release, which moves the tap and the marketplace forward again.

## Internal tool crates

`playbook-telemetry` and `playbook-code` are workspace members under `crates/`. `playbook` depends on them by path, so there is one `Cargo.lock`.

`playbook-code` is a library of the `playbook` binary and has no release of its own. `playbook-telemetry` uses the workspace version, and its `pbtop` binary ships in the `vX.Y.Z` release (ADR 0038). The `telemetry-v` tags before 0.34.0 stay as history.

Release assets by tool:

- playbook: `playbook-<version>-<target>`, the raw binary.
- pbtop: `pbtop-<version>-<target>.tar.gz` holding `pbtop` alone.
- pbsync: `pbsync-<version>-<target>.tar.gz` holding `pbsync` alone.
- pbdoc: `pbdoc-<version>-<target>.tar.gz` holding `pbdoc` and `NOTICE-pbdoc`, for the four macOS and Linux targets.
- pbsync: `pbsync-<version>-<target>.tar.gz` holding `pbsync`, for the four macOS and Linux targets.
- Windows: `playbook-<version>-x86_64-pc-windows-msvc.exe`, the raw binary and the only Windows route.

The Homebrew tap maps each formula to its asset.

### pbsync

`pbsync`, the opt-in usage sync helper, ships in its own archive and installs only with the `playbook` formula in the tap, which adds it as a `resource "pbsync"`. There is no `pbsync` formula. The `formulas` job fails when the tap's `Formula/playbook.rb` has no `resource "pbsync"`, so merge that tap change before the release that adds the archives.

`install.sh` and `playbook upgrade` install `pbsync` beside `playbook` the same way as `pbdoc`: the same download, SHA256 check, attestation check and atomic swap, with the old file kept as `pbsync.<version>.bak`. `playbook uninstall` removes it. A release without a `pbsync` archive, or a failed `pbsync` update, only warns and never fails the `playbook` install or update, because sync is optional and off by default. When the version of `playbook` is unchanged, `playbook upgrade` still installs a missing or older `pbsync`.

### pbdoc

Only `install.sh` and Homebrew ship `pbdoc`, the document converter. The Nix flake and the raw release assets give `playbook` without it, and `playbook doc` then reports that no converter is installed. This is a known limit.

Release checklist for the release that adds `pbdoc`: the tap pull request that adds `resource "pbdoc"` to `Formula/playbook.rb` (homebrew-tap #53) merges first, and the release workflow and the installer changes ship together. The `downstream` job fails when the tap formula has no `resource "pbdoc"`, so a tag cut before that pull request merges cannot ship silently.

### Raw asset names

`install.sh`, `playbook upgrade` and tools that read the release by name, such as `cargo binstall` and mise, depend on the raw `playbook-<version>-<target>` name. Those two tools are not a supported channel and no job installs with them.

- `[package.metadata.binstall]` in `crates/cli/Cargo.toml` holds the URL template.
- `scripts/asset-names.test.sh` expands the template for every target of the build matrix and fails when it differs from the name `release.yml` gives the asset.
- The crates.io name `playbook` belongs to another project, so a `cargo binstall` command needs `--git`, and the metadata disables the quick-install strategy.
