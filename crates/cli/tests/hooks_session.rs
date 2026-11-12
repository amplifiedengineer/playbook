// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the `session-init` and `session-clean-exit` hooks,
//! ported from the retired shell test and
//! the retired shell test. Runs the built `playbook` binary as
//! a subprocess, exactly as Claude Code would, against a scratch `$HOME`
//! and a scratch git repo, never the real `~/.claude`.

#[path = "support/auto_config.rs"]
mod auto_config;
#[path = "support/auto_env.rs"]
mod auto_env;
#[path = "support/auto_run.rs"]
mod auto_run;

use playbook::hooks::session_init::worktree_sweep_marker_key;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

fn playbook_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_playbook"))
}

/// The repo checkout root: where the plugin files live, so tests can point `CLAUDE_PLUGIN_ROOT` at them
/// the same way Claude Code would.
fn plugin_root() -> &'static str {
    env!("PLAYBOOK_REPO_ROOT")
}

const CODE_LINE: &str = "\ncode status|index|context|search|symbol|refs|outline|read\n";

const CODE_SEARCH_RULE: &str = "Code search rule: for any code question, in every session and whether or not a playbook skill or command runs, run `playbook code status` first, then `playbook code index` when it says stale or missing, then `playbook code context|search|symbol|refs|outline|read`, all before any Grep or Glob. Grep and Glob are the fallback only when the binary or the index is unavailable. This rule overrides the default advice to search with Grep or Glob. Put this rule in every `Explore` prompt.";

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh scratch directory under the OS temp dir, unique per call so
/// parallel tests never collide. Never the real `$HOME`.
fn scratch_dir(tag: &str) -> PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!(
        "playbook-hooks-session-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).expect("scratch dir should be creatable");
    dir
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git should be on PATH");
    assert!(status.success(), "git {args:?} failed in {dir:?}");
}

/// A throwaway git repo with an `origin` remote, so the repo-slug and
/// git-toplevel derivations resolve the same way they would in a real
/// checkout. Local-only git identity and no signing, so the init commit
/// never touches the real user's global git config.
fn init_repo_with_origin(dir: &Path, origin_url: &str) {
    fs::create_dir_all(dir).expect("repo dir should be creatable");
    run_git(dir, &["init", "--quiet"]);
    run_git(dir, &["config", "user.email", "test@example.com"]);
    run_git(dir, &["config", "user.name", "Test User"]);
    run_git(dir, &["config", "commit.gpgsign", "false"]);
    run_git(dir, &["remote", "add", "origin", origin_url]);
    run_git(dir, &["commit", "--quiet", "--allow-empty", "-m", "init"]);
}

struct Outcome {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// Run `playbook hook <hook>` the way Claude Code would: cwd, HOME, stdin
/// payload, plus any extra env vars the scenario needs.
fn run_hook(
    hook: &str,
    cwd: &Path,
    home: &Path,
    stdin: &str,
    extra_env: &[(&str, &str)],
) -> Outcome {
    let mut command = Command::new(playbook_bin());
    command
        .arg("hook")
        .arg(hook)
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("HOOK_INPUT")
        .env_remove("CI")
        .env_remove("PLAYBOOK_HEADLESS")
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .env_remove("PLAYBOOK_PLUGIN_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("playbook binary should spawn");
    child
        .stdin
        .take()
        .expect("stdin should be piped")
        .write_all(stdin.as_bytes())
        .expect("writing stdin should succeed");
    let output = child
        .wait_with_output()
        .expect("playbook binary should exit");
    Outcome {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// `.hookSpecificOutput.additionalContext` from a hook's stdout, or empty
/// if absent or the stdout does not parse as JSON.
fn additional_context(stdout: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return String::new();
    };
    value
        .get("hookSpecificOutput")
        .and_then(|h| h.get("additionalContext"))
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string()
}

/// `.systemMessage` from a hook's stdout, or empty if absent or the stdout
/// does not parse as JSON.
fn system_message(stdout: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return String::new();
    };
    value
        .get("systemMessage")
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------
// session-init: project memory slice (the retired shell test cases 1-4)
// ---------------------------------------------------------------------

#[test]
fn session_init_injects_the_graph_backed_slice() {
    // Arrange: a fake HOME with a memory.graph.json carrying one fact in scope for
    // a repo whose origin matches REPO_SLUG.
    let work = scratch_dir("graph-slice");
    let repo_slug = "acme/widget";
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));

    let home = work.join("home-graph");
    let memory_dir = home.join(".config").join("playbook").join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    playbook::testing::write_graph_fixture(
        &memory_dir,
        &format!(
            r#"{{"nodes":[{{"id":"{repo_slug}/f1","file":"{repo_slug}/f1.md","scope":"project","type":"project","name":"widget-fact-one","description":"The widget module talks to the sprocket service.","project":"{repo_slug}","pinned":true}}],"edges":[]}}"#
        ),
    );

    // Act
    let outcome = run_hook(
        "session-init",
        &repo_dir,
        &home,
        "{}",
        &[("CLAUDE_PLUGIN_ROOT", plugin_root())],
    );
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        serde_json::from_str::<serde_json::Value>(&outcome.stdout).is_ok(),
        "stdout should be valid JSON: {}",
        outcome.stdout
    );
    assert!(
        context.contains("widget-fact-one"),
        "additionalContext should carry the fact name: {context}"
    );
}

/// The ranked memory block is capped near 2,500 characters. With 120 equally
/// pinned facts the tie breaks by name, so an early fact survives and one
/// placed past the cap does not.
#[test]
fn session_init_caps_the_ranked_memory_block() {
    // Arrange: 120 pinned facts, each with a ~150-char description, so the
    // block would run past 15,000 characters uncapped. Zero-padded names
    // break the score tie in order, so "fact-001" is early and "fact-120"
    // is guaranteed past the cap.
    let work = scratch_dir("graph-cap");
    let repo_slug = "acme/widget";
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));

    let home = work.join("home-cap");
    let memory_dir = home.join(".config").join("playbook").join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    let padding = "x".repeat(140);
    let nodes: Vec<String> = (1..=120)
        .map(|n| {
            format!(
                r#"{{"id":"{repo_slug}/f{n:03}","file":"{repo_slug}/f{n:03}.md","scope":"project","type":"project","name":"fact-{n:03}","description":"desc-{n:03}-{padding}","project":"{repo_slug}","pinned":true}}"#
            )
        })
        .collect();
    playbook::testing::write_graph_fixture(
        &memory_dir,
        &format!(r#"{{"nodes":[{}],"edges":[]}}"#, nodes.join(",")),
    );

    // Act
    let outcome = run_hook(
        "session-init",
        &repo_dir,
        &home,
        "{}",
        &[("CLAUDE_PLUGIN_ROOT", plugin_root())],
    );
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        context.contains("fact-001"),
        "an early fact, well within the cap, should survive: {context}"
    );
    assert!(
        !context.contains("fact-120"),
        "a fact placed past the cap should be left out: {context}"
    );
    let map = playbook::tools_map::render().trim_end().chars().count();
    let rule = CODE_SEARCH_RULE.chars().count();
    assert!(
        context.chars().count() <= 2600 + map + rule + 4,
        "the context here is the tool map, the rule and a memory block held near 2,500 chars: {}",
        context.chars().count()
    );
}

#[test]
fn session_init_falls_back_to_a_native_graph_read() {
    // Arrange: a fake HOME with memory.graph.json but no CLAUDE_PLUGIN_ROOT,
    // so the memory slice renders from the graph file alone.
    let work = scratch_dir("native-fallback");
    let repo_slug = "acme/widget";
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));

    let home = work.join("home-native");
    let memory_dir = home.join(".config").join("playbook").join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    playbook::testing::write_graph_fixture(
        &memory_dir,
        &format!(
            r#"{{"nodes":[{{"id":"{repo_slug}/f1","file":"{repo_slug}/f1.md","scope":"project","type":"project","name":"native-fact-one","description":"parsed straight from the graph file, no script involved","project":"{repo_slug}","pinned":true}}],"edges":[]}}"#
        ),
    );

    // Act: no CLAUDE_PLUGIN_ROOT; the slice never needed it.
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        context.contains("native-fact-one"),
        "additionalContext should carry the fact name: {context}"
    );
    assert!(
        context.contains("parsed straight from the graph file, no script involved"),
        "additionalContext should carry the fact description: {context}"
    );
}

#[test]
fn session_init_native_fallback_absent_graph_emits_no_memory_block() {
    // Arrange: no CLAUDE_PLUGIN_ROOT and no memory.graph.json at all.
    let work = scratch_dir("native-fallback-absent");
    let repo_slug = "acme/widget";
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));
    let home = scratch_dir("native-fallback-absent-home");

    // Act
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        outcome.stdout.trim().is_empty()
            || serde_json::from_str::<serde_json::Value>(&outcome.stdout).is_ok(),
        "stdout should be valid JSON: {}",
        outcome.stdout
    );
    assert!(
        !context.contains("Top memory facts for this repo"),
        "no memory block should be emitted when memory.graph.json is absent: {context}"
    );
}

#[test]
fn session_init_native_fallback_malformed_graph_emits_no_memory_block() {
    // Arrange: a memory.db that is not a database.
    let work = scratch_dir("native-fallback-malformed");
    let repo_slug = "acme/widget";
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));

    let home = work.join("home-malformed");
    let memory_dir = home.join(".config").join("playbook").join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    // The graph lives in memory.db now; a file that is not a database is the
    // unreadable case.
    let data_dir = home.join(".config").join("playbook").join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(data_dir.join("memory.db"), "{not valid json").unwrap();

    // Act
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        outcome.stdout.trim().is_empty()
            || serde_json::from_str::<serde_json::Value>(&outcome.stdout).is_ok(),
        "stdout should be valid JSON: {}",
        outcome.stdout
    );
    assert!(
        !context.contains("Top memory facts for this repo"),
        "no memory block should be emitted for a malformed graph file: {context}"
    );
}

/// Distinct from the unreadable-database scenario above: the graph is stored
/// but holds no nodes.
#[test]
fn session_init_native_fallback_empty_graph_emits_no_memory_block() {
    // Arrange: a stored graph with no nodes.
    let work = scratch_dir("native-fallback-no-nodes");
    let repo_slug = "acme/widget";
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));

    let home = work.join("home-no-nodes");
    let memory_dir = home.join(".config").join("playbook").join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    playbook::testing::write_graph_fixture(&memory_dir, r#"{"edges":[]}"#);

    // Act
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        outcome.stdout.trim().is_empty()
            || serde_json::from_str::<serde_json::Value>(&outcome.stdout).is_ok(),
        "stdout should be valid JSON: {}",
        outcome.stdout
    );
    assert!(
        !context.contains("Top memory facts for this repo"),
        "no memory block should be emitted when the graph file has no nodes array: {context}"
    );
}

#[test]
fn session_init_migrates_legacy_home_memory_root_before_recall() {
    // Arrange: memory still sitting at the pre-ADR-0012 ~/.claude/memory
    // location, nothing yet under ~/.config/playbook/memory.
    let work = scratch_dir("legacy-root");
    let repo_slug = "acme/widget";
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));

    let home = work.join("home-legacy-root");
    let legacy_dir = home.join(".claude").join("memory");
    fs::create_dir_all(&legacy_dir).unwrap();
    let fact_dir = legacy_dir.join("acme").join("widget");
    fs::create_dir_all(&fact_dir).unwrap();
    fs::write(
        fact_dir.join("legacy-root-fact.md"),
        "---\nname: legacy-root-fact\ntype: project\ndescription: still under the old home tree\npinned: true\n---\nBody.\n",
    )
    .unwrap();
    fs::write(
        legacy_dir.join("memory.graph.json"),
        r#"{"nodes":[],"edges":[]}"#,
    )
    .unwrap();

    // Act: no CLAUDE_PLUGIN_ROOT, so the fact surfaces via the native
    // fallback reading the migrated graph file, not a shelled-out script.
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        context.contains("legacy-root-fact"),
        "the hook should migrate ~/.claude/memory before reading it, so the fact still surfaces: {context}"
    );
    let new_graph = home
        .join(".config")
        .join("playbook")
        .join("data")
        .join("memory.db");
    assert!(
        new_graph.is_file(),
        "the graph should now live in the data folder: {}",
        new_graph.display()
    );
    assert!(
        !home.join(".claude").join("memory").exists(),
        "the legacy memory tree should be gone after a completed migration"
    );
}

#[test]
fn session_init_no_memory_store_emits_no_memory_block() {
    // Arrange: a fake HOME with no memory dir whatsoever.
    let work = scratch_dir("no-store");
    let repo_slug = "acme/widget";
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));
    let home = scratch_dir("no-store-home");

    // Act
    let outcome = run_hook(
        "session-init",
        &repo_dir,
        &home,
        "{}",
        &[("CLAUDE_PLUGIN_ROOT", plugin_root())],
    );
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        !context.contains("Top memory facts for this repo"),
        "no memory block should be emitted: {context}"
    );
}

#[test]
fn session_init_outside_a_git_repo_emits_no_memory_block() {
    // Arrange: the graph-backed HOME from the first scenario, but run from
    // outside any git repo, so the slug never resolves.
    let work = scratch_dir("non-repo");
    let non_repo_dir = work.join("not-a-repo");
    fs::create_dir_all(&non_repo_dir).unwrap();

    let home = work.join("home-graph");
    let memory_dir = home.join(".config").join("playbook").join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    playbook::testing::write_graph_fixture(
        &memory_dir,
        r#"{"nodes":[{"id":"acme/widget/f1","file":"acme/widget/f1.md","scope":"project","type":"project","name":"widget-fact-one","description":"desc","project":"acme/widget"}],"edges":[]}"#,
    );

    // Act
    let outcome = run_hook(
        "session-init",
        &non_repo_dir,
        &home,
        "{}",
        &[("CLAUDE_PLUGIN_ROOT", plugin_root())],
    );
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        !context.contains("Top memory facts for this repo"),
        "no memory block should be emitted outside a git repo: {context}"
    );
    assert!(
        !context.contains("widget-fact-one"),
        "the fact from the slice should be absent: {context}"
    );
    // A SessionStart outside any repo must not consume the sweep window
    // either: `git_toplevel()` reads empty there, so `maybe_sweep_worktrees`
    // must have returned before ever reaching the marker.
    let playbook_dir = home.join(".config").join("playbook");
    let wrote_a_sweep_marker = playbook_dir.join("data").join("config.db").exists()
        && !playbook::state::list(&playbook_dir, playbook::state::SWEEP_PREFIX)
            .unwrap_or_default()
            .is_empty();
    assert!(
        !wrote_a_sweep_marker,
        "a non-repo SessionStart must not write any sweep rate-limit marker"
    );
}

// ---------------------------------------------------------------------
// session-init: a failed migration is surfaced on stderr, not swallowed
// ---------------------------------------------------------------------

/// Whether this filesystem/user enforces Unix permission bits at all, so a
/// test run as root (which bypasses them) degrades to a skip instead of a
/// false failure. Mirrors `init_memory_migrate.rs`'s own probe.
#[cfg(unix)]
fn permission_checks_are_enforced(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let probe = dir.join(".perm-probe");
    fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
    let blocked = fs::write(&probe, "x").is_err();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    let _ = fs::remove_file(&probe);
    blocked
}

#[cfg(unix)]
#[test]
fn session_init_surfaces_a_failed_migration_on_stderr_not_stdout() {
    use std::os::unix::fs::PermissionsExt;

    // Arrange: a legacy memory tree with a subdirectory the migration
    // cannot enumerate, and a pre-existing new root so the move takes the
    // per-file copy path instead of a plain rename.
    let work = scratch_dir("migrate-fails");
    let repo_dir = work.join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    let home = work.join("home");
    let legacy_mem_dir = home.join(".claude").join("memory");
    fs::create_dir_all(&legacy_mem_dir).unwrap();
    fs::write(legacy_mem_dir.join("global-fact.md"), "global fact content").unwrap();
    let locked_dir = legacy_mem_dir.join("no-access");
    fs::create_dir_all(&locked_dir).unwrap();
    fs::write(locked_dir.join("secret.md"), "secret content").unwrap();
    fs::create_dir_all(home.join(".config").join("playbook").join("memory")).unwrap();

    if !permission_checks_are_enforced(&locked_dir) {
        eprintln!(
            "skipping session_init_surfaces_a_failed_migration_on_stderr_not_stdout: \
             this filesystem/user does not enforce permission bits (likely running as root)"
        );
        return;
    }
    fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o000)).unwrap();

    // Act
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);

    // Assert
    fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        outcome.stderr.contains("memory-migrate:"),
        "stderr should surface the migration failure: {}",
        outcome.stderr
    );
    assert!(
        !outcome.stdout.contains("memory-migrate:"),
        "the failure detail must not leak onto stdout: {}",
        outcome.stdout
    );
    assert!(
        outcome.stderr.contains("no-access"),
        "the failure detail should name the unreadable subdirectory: {}",
        outcome.stderr
    );
}

// ---------------------------------------------------------------------
// session-init: pinned and usage-promoted facts, injected unconditionally
// ---------------------------------------------------------------------

/// A fact marked `pinned: true` in `memory.graph.json` must inject even
/// though nothing anchors or prompt-matches it, and even with no
/// `CLAUDE_PLUGIN_ROOT` set: the pinned/promoted block reads the graph
/// directly and does not depend on the general memory-slice machinery at
/// all. Asserts on `append_promoted_facts`'s own header and bulleted
/// rendering, not just the bare fact name: with no `CLAUDE_PLUGIN_ROOT`,
/// `read_graph_slice_fallback` also renders this fixture's fact on this same
/// branch, as a plain "name: description" line with no header or bullet, so
/// the bare name alone would pass even if the promoted block were removed.
#[test]
fn session_init_injects_a_pinned_fact_independent_of_general_memory_slice() {
    // Arrange
    let work = scratch_dir("pinned-fact");
    let repo_slug = "acme/widget";
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));

    let home = work.join("home-pinned");
    let memory_dir = home.join(".config").join("playbook").join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    playbook::testing::write_graph_fixture(
        &memory_dir,
        &format!(
            r#"{{"nodes":[{{"id":"{repo_slug}/pinned-fact","file":"{repo_slug}/pinned-fact.md","scope":"project","type":"project","name":"pinned-fact-name","description":"Pinned fact description text.","project":"{repo_slug}","pinned":true}}],"edges":[]}}"#
        ),
    );

    // Act: no CLAUDE_PLUGIN_ROOT, so the general memory slice runs through
    // its native fallback rather than the script-backed path.
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        context.contains("Top memory facts for this repo"),
        "additionalContext should carry the promoted-facts block header: {context}"
    );
    assert!(
        context.contains("- pinned-fact-name: Pinned fact description text."),
        "the promoted block's own bulleted line should carry the pinned fact; the native \
        fallback's plain \"name: description\" rendering never produces this line: {context}"
    );
}

/// A fact marked `promoted: true` in `memory.signals.json` injects the same
/// way a pinned fact does. As above, the assertion checks the promoted
/// block's own header and bulleted rendering rather than the bare fact name,
/// since the native fallback renders the same fact on this branch too, just
/// without that header or bullet.
#[test]
fn session_init_injects_a_promoted_fact_independent_of_general_memory_slice() {
    // Arrange
    let work = scratch_dir("promoted-fact");
    let repo_slug = "acme/widget";
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));

    let home = work.join("home-promoted");
    let memory_dir = home.join(".config").join("playbook").join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    playbook::testing::write_graph_fixture(
        &memory_dir,
        &format!(
            r#"{{"nodes":[{{"id":"{repo_slug}/promoted-fact","file":"{repo_slug}/promoted-fact.md","scope":"project","type":"project","name":"promoted-fact-name","description":"Promoted fact description text.","project":"{repo_slug}"}}],"edges":[]}}"#
        ),
    );
    fs::write(
        memory_dir.join("memory.signals.json"),
        format!(r#"{{"nodes":{{"{repo_slug}/promoted-fact":{{"hits":3,"promoted":true}}}}}}"#),
    )
    .unwrap();

    // Act
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        context.contains("Top memory facts for this repo"),
        "additionalContext should carry the promoted-facts block header: {context}"
    );
    assert!(
        context.contains("- promoted-fact-name: Promoted fact description text."),
        "the promoted block's own bulleted line should carry the promoted fact; the native \
        fallback's plain \"name: description\" rendering never produces this line: {context}"
    );
}

/// A pinned fact belonging to a DIFFERENT repo than the current session must
/// not inject: `pinned`/`promoted` are still subject to the same
/// global-or-matching-project scope check every other memory injection path
/// in this codebase applies. A regression that dropped that check would
/// leak one project's pinned facts into every other repo's sessions.
#[test]
fn a_pinned_fact_from_a_different_repo_does_not_inject() {
    // Arrange
    let work = scratch_dir("pinned-fact-other-repo");
    let repo_slug = "acme/widget";
    let other_slug = "acme/other-repo";
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));

    let home = work.join("home-pinned-other-repo");
    let memory_dir = home.join(".config").join("playbook").join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    playbook::testing::write_graph_fixture(
        &memory_dir,
        &format!(
            r#"{{"nodes":[{{"id":"{other_slug}/other-repo-fact","file":"{other_slug}/other-repo-fact.md","scope":"project","type":"project","name":"other-repo-fact-name","description":"Should never appear here.","project":"{other_slug}","pinned":true}}],"edges":[]}}"#
        ),
    );

    // Act
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        !context.contains("other-repo-fact-name"),
        "a pinned fact scoped to a different repo must not leak into this session: {context}"
    );
}

/// A global-scope promoted fact injects regardless of which repo the
/// session is in, the same global-always-in-scope rule every other memory
/// injection path in this codebase already follows.
#[test]
fn a_global_promoted_fact_injects_regardless_of_repo() {
    // Arrange
    let work = scratch_dir("promoted-fact-global");
    let repo_slug = "acme/widget";
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));

    let home = work.join("home-promoted-global");
    let memory_dir = home.join(".config").join("playbook").join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    playbook::testing::write_graph_fixture(
        &memory_dir,
        r#"{"nodes":[{"id":"global/global-promoted-fact","file":"global-promoted-fact.md","scope":"global","type":"reference","name":"global-promoted-fact-name","description":"Global fact, any repo."}],"edges":[]}"#,
    );
    fs::write(
        memory_dir.join("memory.signals.json"),
        r#"{"nodes":{"global/global-promoted-fact":{"hits":3,"promoted":true}}}"#,
    )
    .unwrap();

    // Act
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);
    let context = additional_context(&outcome.stdout);

    // Assert: the promoted block's own header and bulleted line, not just the
    // bare name, since the native fallback also renders this fact on this
    // no-CLAUDE_PLUGIN_ROOT branch, just without that header or bullet.
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        context.contains("Top memory facts for this repo"),
        "additionalContext should carry the promoted-facts block header: {context}"
    );
    assert!(
        context.contains("- global-promoted-fact-name: Global fact, any repo."),
        "a global-scope promoted fact should inject via the promoted block's own bulleted \
        rendering, regardless of repo: {context}"
    );
}

#[test]
fn an_org_scoped_promoted_fact_injects_for_a_sibling_repo_under_the_same_owner() {
    // Arrange: an org fact owned by "acme", session runs in a sibling repo that did not save it.
    let work = scratch_dir("promoted-fact-org");
    let repo_slug = "acme/widget";
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));

    let home = work.join("home-promoted-org");
    let memory_dir = home.join(".config").join("playbook").join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    playbook::testing::write_graph_fixture(
        &memory_dir,
        r#"{"nodes":[{"id":"acme/org-promoted-fact","file":"acme/org-promoted-fact.md","scope":"org","type":"reference","name":"org-promoted-fact-name","description":"Shared across every acme repo.","project":"acme"}],"edges":[]}"#,
    );
    fs::write(
        memory_dir.join("memory.signals.json"),
        r#"{"nodes":{"acme/org-promoted-fact":{"hits":3,"promoted":true}}}"#,
    )
    .unwrap();

    // Act
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);
    let context = additional_context(&outcome.stdout);

    // Assert: the promoted block's own header and bulleted line, not just the
    // bare name, since the native fallback also renders this fact on this
    // no-CLAUDE_PLUGIN_ROOT branch, just without that header or bullet.
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        context.contains("Top memory facts for this repo"),
        "additionalContext should carry the promoted-facts block header: {context}"
    );
    assert!(
        context.contains("- org-promoted-fact-name: Shared across every acme repo."),
        "an org-scoped promoted fact should inject via the promoted block's own bulleted \
        rendering, for any repo under its owner: {context}"
    );
}

#[test]
fn an_org_scoped_promoted_fact_does_not_leak_to_a_different_owner() {
    // Arrange: the org fact belongs to "acme", session runs in a repo owned by "other-org".
    let work = scratch_dir("promoted-fact-org-other-owner");
    let repo_slug = "other-org/widget";
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));

    let home = work.join("home-promoted-org-other-owner");
    let memory_dir = home.join(".config").join("playbook").join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    playbook::testing::write_graph_fixture(
        &memory_dir,
        r#"{"nodes":[{"id":"acme/org-promoted-fact","file":"acme/org-promoted-fact.md","scope":"org","type":"reference","name":"org-promoted-fact-name","description":"Shared across every acme repo.","project":"acme"}],"edges":[]}"#,
    );
    fs::write(
        memory_dir.join("memory.signals.json"),
        r#"{"nodes":{"acme/org-promoted-fact":{"hits":3,"promoted":true}}}"#,
    )
    .unwrap();

    // Act
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        !context.contains("org-promoted-fact-name"),
        "an org-scoped fact must not leak into a session under a different owner: {context}"
    );
}

// ---------------------------------------------------------------------
// session-init: ADR 0008 WU-3, reload a persisted handoff at SessionStart
// ---------------------------------------------------------------------

/// The exact slugify `src/cc/mod.rs::project_slug` uses: every
/// non-alphanumeric character becomes `-`. Duplicated here rather than
/// imported, since this is a black-box test of the compiled binary.
fn project_slug(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

static HANDOFF_SUFFIX_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write a handoff file the way `commands/handoff-save.md` does:
/// `<slug>-<unique-suffix>.md`, never a single fixed path, so two sessions
/// in the same directory get distinct files. The counter (rather than a
/// real timestamp+pid) keeps this deterministic and collision-free across
/// repeated calls within one test.
fn write_handoff(home: &Path, cwd: &Path, contents: &str) -> PathBuf {
    let n = HANDOFF_SUFFIX_COUNTER.fetch_add(1, Ordering::Relaxed);
    write_handoff_suffixed(home, cwd, contents, &format!("{n}-1"))
}

fn write_handoff_suffixed(home: &Path, cwd: &Path, contents: &str, suffix: &str) -> PathBuf {
    let slug = project_slug(&cwd.to_string_lossy());
    let dir = home
        .join(".config")
        .join("playbook")
        .join("runtime")
        .join("handoff");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{slug}-{suffix}.md"));
    fs::write(&path, contents).unwrap();
    path
}

/// Run session-init with `PWD` explicitly set to `cwd`. `Command::current_dir`
/// alone does not update the child's `PWD` env var (that is a shell
/// convention, not something chdir does), and `logical_cwd()` prefers `PWD`
/// over `current_dir()` specifically for symlink-consistency reasons
/// (macOS `/tmp` and `/var` resolve through `/private`). Real Claude Code
/// sets `PWD` to match; a test that omits it would silently exercise the
/// wrong path and pass for the wrong reason.
fn run_session_init_at(cwd: &Path, home: &Path) -> Outcome {
    run_hook(
        "session-init",
        cwd,
        home,
        "{}",
        &[
            ("CLAUDE_PLUGIN_ROOT", plugin_root()),
            ("PWD", cwd.to_str().unwrap()),
        ],
    )
}

fn run_session_init_with_source(cwd: &Path, home: &Path, source: &str) -> Outcome {
    run_hook(
        "session-init",
        cwd,
        home,
        &format!(r#"{{"source":"{source}"}}"#),
        &[
            ("CLAUDE_PLUGIN_ROOT", plugin_root()),
            ("PWD", cwd.to_str().unwrap()),
        ],
    )
}

#[test]
fn session_init_injects_a_present_handoff_and_deletes_it() {
    // Arrange
    let work = scratch_dir("handoff-present");
    let repo_dir = work.join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    let home = work.join("home");
    let handoff_path = write_handoff(&home, &repo_dir, "HANDOFFMARKER decisions from last time");

    // Act
    let outcome = run_session_init_at(&repo_dir, &home);
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        context.contains("HANDOFFMARKER"),
        "a present handoff should be injected: {context}"
    );
    assert!(
        !handoff_path.exists(),
        "the handoff file should be deleted after being read (read-once)"
    );
}

#[test]
fn session_init_no_handoff_file_emits_no_handoff_block_and_no_error() {
    // Arrange: no handoff file written at all.
    let work = scratch_dir("handoff-absent");
    let repo_dir = work.join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    let home = work.join("home");

    // Act
    let outcome = run_session_init_at(&repo_dir, &home);

    // Assert
    assert_eq!(outcome.exit_code, 0, "an absent handoff should not error");
    assert!(
        !additional_context(&outcome.stdout).contains("Handoff from your previous session"),
        "no handoff block should be emitted when there is nothing to reload"
    );
}

#[test]
fn session_init_reloads_the_handoff_identically_on_clear_and_on_startup() {
    // Arrange: two independent scratch setups, one per source value, since
    // the handoff is consumed (deleted) by the first read.
    for source in ["clear", "startup"] {
        let work = scratch_dir(&format!("handoff-source-{source}"));
        let repo_dir = work.join("repo");
        fs::create_dir_all(&repo_dir).unwrap();
        let home = work.join("home");
        write_handoff(&home, &repo_dir, "HANDOFFMARKER present for this source");

        // Act
        let outcome = run_session_init_with_source(&repo_dir, &home, source);

        // Assert
        let context = additional_context(&outcome.stdout);
        assert!(
            context.contains("HANDOFFMARKER"),
            "source={source} should inject the handoff exactly like any other source: {context}"
        );
    }
}

#[test]
fn session_init_treats_a_handoff_older_than_14_days_as_stale_and_clears_it() {
    // Arrange
    let work = scratch_dir("handoff-stale");
    let repo_dir = work.join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    let home = work.join("home");
    let handoff_path = write_handoff(&home, &repo_dir, "HANDOFFMARKER should never surface");

    let fifteen_days_ago =
        std::time::SystemTime::now() - std::time::Duration::from_secs(15 * 86400);
    let file = fs::File::open(&handoff_path).unwrap();
    file.set_modified(fifteen_days_ago).unwrap();

    // Act
    let outcome = run_session_init_at(&repo_dir, &home);
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "a stale handoff should not error");
    assert!(
        !context.contains("HANDOFFMARKER"),
        "a handoff older than the 14-day cap should not be injected: {context}"
    );
    assert!(
        !handoff_path.exists(),
        "a stale handoff should still be cleared, ending the backstop it exists for"
    );
}

#[test]
fn session_init_never_reads_a_handoff_written_under_a_different_worktree() {
    // Arrange: two distinct working directories under the same HOME
    // (simulating two worktrees of one repo), a handoff only under A's slug.
    let work = scratch_dir("handoff-worktree");
    let home = work.join("home");
    let worktree_a = work.join("worktree-a");
    let worktree_b = work.join("worktree-b");
    fs::create_dir_all(&worktree_a).unwrap();
    fs::create_dir_all(&worktree_b).unwrap();
    let path_a = write_handoff(
        &home,
        &worktree_a,
        "HANDOFFMARKER belongs only to worktree A",
    );

    // Act: SessionStart runs from worktree B.
    let outcome = run_session_init_at(&worktree_b, &home);
    let context = additional_context(&outcome.stdout);

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        !context.contains("HANDOFFMARKER"),
        "worktree B must never see worktree A's handoff: {context}"
    );
    // Worktree A's own handoff must be untouched by B's run: not read, not deleted.
    assert!(
        path_a.exists(),
        "worktree B's SessionStart must not delete worktree A's handoff file"
    );
}

#[test]
fn session_init_injects_both_handoffs_from_two_concurrent_sessions_in_the_same_directory() {
    // Arrange: two `cc` sessions working the same directory both ran
    // /playbook:handoff-save close together. Each writes its own
    // uniquely-suffixed file (the `playbook handoff save` design),
    // so neither overwrites the other the way a single fixed path would.
    let work = scratch_dir("handoff-concurrent");
    let repo_dir = work.join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    let home = work.join("home");
    let path_1 = write_handoff_suffixed(&home, &repo_dir, "HANDOFFMARKER-ONE", "1000-111");
    let path_2 = write_handoff_suffixed(&home, &repo_dir, "HANDOFFMARKER-TWO", "1001-222");

    // Act
    let outcome = run_session_init_at(&repo_dir, &home);
    let context = additional_context(&outcome.stdout);

    // Assert: both surface in one SessionStart, neither silently lost.
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        context.contains("HANDOFFMARKER-ONE"),
        "the first concurrent session's handoff should still surface: {context}"
    );
    assert!(
        context.contains("HANDOFFMARKER-TWO"),
        "the second concurrent session's handoff should still surface: {context}"
    );
    assert!(
        context.contains("2 of 2 found"),
        "more than one match should say so, not read like a single handoff: {context}"
    );
    assert!(
        !path_1.exists() && !path_2.exists(),
        "both matched files should be consumed (read-once, mirroring the single-handoff case)"
    );
}

#[test]
fn session_init_caps_injected_handoffs_but_still_deletes_every_match() {
    // Arrange: four fresh handoffs in the same directory, more than
    // HANDOFF_MAX_INJECTED (3). Explicit, spread-out mtimes make the
    // freshest-first ordering deterministic instead of relying on write
    // order landing in the same filesystem-time quantum.
    let work = scratch_dir("handoff-cap");
    let repo_dir = work.join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    let home = work.join("home");

    let now = std::time::SystemTime::now();
    let mut paths = Vec::new();
    for (n, age_hours) in [(1, 4), (2, 3), (3, 2), (4, 1)] {
        let marker = format!("HANDOFFMARKER-{n}");
        let path = write_handoff_suffixed(&home, &repo_dir, &marker, &format!("{}-9", 2000 + n));
        let file = fs::File::open(&path).unwrap();
        file.set_modified(now - std::time::Duration::from_secs(age_hours * 3600))
            .unwrap();
        paths.push(path);
    }

    // Act
    let outcome = run_session_init_at(&repo_dir, &home);
    let context = additional_context(&outcome.stdout);

    // Assert: only the 3 freshest (4, 3, 2) are injected; the oldest (1) is
    // not, but every file is still gone afterward.
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        context.contains("HANDOFFMARKER-4")
            && context.contains("HANDOFFMARKER-3")
            && context.contains("HANDOFFMARKER-2"),
        "the 3 freshest matches should be injected: {context}"
    );
    assert!(
        !context.contains("HANDOFFMARKER-1"),
        "a 4th match beyond the injection cap should not be injected: {context}"
    );
    assert!(
        paths.iter().all(|p| !p.exists()),
        "every matched file should still be deleted, capped or not"
    );
}

// ---------------------------------------------------------------------
// session-init: the six zeroed counters plus start-ts
// ---------------------------------------------------------------------

#[test]
fn session_init_zeroes_exactly_the_six_counter_files() {
    // Arrange: a session directory pre-seeded with non-empty content in
    // every file the hook is expected to zero, plus one unrelated file that
    // must survive untouched.
    let home = scratch_dir("zero-counters");
    let session_id = "sid-zero";
    let session_dir = home
        .join(".config")
        .join("playbook")
        .join("runtime")
        .join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    for name in [
        "search-count",
        "tool-count",
        "edit-count",
        "edits.jsonl",
        "seen-reads",
        "read-state.jsonl",
        "capture-crossings",
        "code-hint",
    ] {
        fs::write(session_dir.join(name), "stale-content").unwrap();
    }
    fs::write(session_dir.join("clean-exit"), "logout").unwrap();

    let repo_dir = scratch_dir("zero-counters-repo");

    // Act
    let outcome = run_hook(
        "session-init",
        &repo_dir,
        &home,
        &format!(r#"{{"session_id":"{session_id}"}}"#),
        &[],
    );

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    for name in [
        "search-count",
        "tool-count",
        "edit-count",
        "edits.jsonl",
        "seen-reads",
        "read-state.jsonl",
        "capture-crossings",
    ] {
        let contents = fs::read_to_string(session_dir.join(name)).unwrap();
        assert_eq!(contents, "", "{name} should be zeroed, got '{contents}'");
    }
    assert!(
        !session_dir.join("code-hint").exists(),
        "the code-hint marker should be removed, not left behind empty"
    );
    let start_ts = fs::read_to_string(session_dir.join("start-ts")).unwrap();
    assert!(
        start_ts.parse::<u64>().is_ok(),
        "start-ts should hold a unix timestamp, got '{start_ts}'"
    );
    let untouched = fs::read_to_string(session_dir.join("clean-exit")).unwrap();
    assert_eq!(
        untouched, "logout",
        "clean-exit is not one of the five counters and must survive untouched"
    );
}

#[test]
fn the_code_hint_fires_at_the_first_grep_after_session_init() {
    // Arrange: a session that already sent the hint once, then restarts.
    let home = scratch_dir("hint-after-init");
    let session_id = "sid-hint";
    let session_dir = home
        .join(".config")
        .join("playbook")
        .join("runtime")
        .join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(session_dir.join("code-hint"), "").unwrap();
    let repo_dir = scratch_dir("hint-after-init-repo");
    let session_payload = format!(r#"{{"session_id":"{session_id}"}}"#);
    let grep_payload = format!(r#"{{"session_id":"{session_id}","tool_name":"Grep"}}"#);

    // Act
    run_hook("session-init", &repo_dir, &home, &session_payload, &[]);
    let first = run_hook("search-counter", &repo_dir, &home, &grep_payload, &[]);
    let second = run_hook("search-counter", &repo_dir, &home, &grep_payload, &[]);

    // Assert
    assert!(
        first.stdout.contains("playbook code status"),
        "the first Grep after session-init should print the hint: {}",
        first.stdout
    );
    assert_eq!(second.stdout, "", "the second Grep should stay silent");
}

// ---------------------------------------------------------------------
// session-init: no toolkit primer or async note
// ---------------------------------------------------------------------

#[test]
fn session_init_injects_no_toolkit_primer_even_with_user_commands() {
    // Arrange: one command file under the scratch HOME, which used to make
    // the toolkit primer fire. Claude Code already lists skills and commands.
    let home = scratch_dir("toolkit-primer");
    let commands_root = home.join(".claude").join("commands");
    fs::create_dir_all(&commands_root).unwrap();
    fs::write(
        commands_root.join("foo.md"),
        "---\ndescription: a test command\n---\nBody\n",
    )
    .unwrap();
    let repo_dir = scratch_dir("toolkit-primer-repo");

    // Act
    let outcome = run_hook(
        "session-init",
        &repo_dir,
        &home,
        r#"{"session_id":"sid-toolkit","source":"startup"}"#,
        &[],
    );

    // Assert
    let context = additional_context(&outcome.stdout);
    assert!(
        !context.contains("Your toolkit") && !context.contains("a test command"),
        "the toolkit primer is retired: {context}"
    );
    assert!(
        !context.contains("Async and deferred-tool discipline"),
        "the async note is retired: {context}"
    );
}

// ---------------------------------------------------------------------
// session-init: resume-only config drift warning
// ---------------------------------------------------------------------

/// The config-hash value for an empty `$HOME/.claude` tree, computed by the
/// same function the hook calls. The known-answer test in
/// `src/common/config_hash.rs` pins the algorithm itself.
fn empty_config_hash(home: &Path) -> String {
    playbook::common::config_hash(&home.join(".claude"))
}

#[test]
fn session_init_drift_warning_fires_only_on_resume() {
    // Arrange: a session directory whose stored config-hash is deliberately
    // stale, so the current hash (of this empty scratch HOME) will not
    // match it.
    let home = scratch_dir("drift-resume");
    let repo_dir = scratch_dir("drift-resume-repo");
    let session_id = "sid-resume";
    let session_dir = home
        .join(".config")
        .join("playbook")
        .join("runtime")
        .join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(session_dir.join("config-hash"), "stale0000stale00").unwrap();
    let expected_hash = empty_config_hash(&home);
    assert_ne!(
        expected_hash, "stale0000stale00",
        "test setup requires the real hash to differ from the seeded stale one"
    );

    // Act: resume.
    let resumed = run_hook(
        "session-init",
        &repo_dir,
        &home,
        &format!(r#"{{"session_id":"{session_id}","source":"resume"}}"#),
        &[("CLAUDE_PLUGIN_ROOT", plugin_root())],
    );

    // Assert: the drift warning fires.
    assert_eq!(resumed.exit_code, 0, "hook should exit 0 on resume");
    let message = system_message(&resumed.stdout);
    assert!(
        message.contains("drifted"),
        "resume with a mismatched hash should warn: {message}"
    );
    let context = additional_context(&resumed.stdout);
    assert!(
        context.contains("config hash has changed"),
        "resume additionalContext should explain the drift: {context}"
    );
    // The stale hash on disk is untouched by a resume (only startup rewrites it).
    let hash_after_resume = fs::read_to_string(session_dir.join("config-hash")).unwrap();
    assert_eq!(hash_after_resume, "stale0000stale00");

    // Arrange: reseed the same stale hash for the startup case.
    fs::write(session_dir.join("config-hash"), "stale0000stale00").unwrap();

    // Act: startup, with the exact same mismatched hash on disk.
    let started = run_hook(
        "session-init",
        &repo_dir,
        &home,
        &format!(r#"{{"session_id":"{session_id}","source":"startup"}}"#),
        &[("CLAUDE_PLUGIN_ROOT", plugin_root())],
    );

    // Assert: startup never warns, but refreshes the stored hash.
    assert_eq!(started.exit_code, 0, "hook should exit 0 on startup");
    assert!(
        system_message(&started.stdout).is_empty(),
        "startup must never emit the drift systemMessage"
    );
    let hash_after_startup = fs::read_to_string(session_dir.join("config-hash")).unwrap();
    assert_eq!(
        hash_after_startup, expected_hash,
        "startup should refresh the stored hash to the freshly computed one"
    );
}

#[test]
fn session_init_resume_with_matching_hash_emits_no_drift_warning() {
    // Arrange: the session's stored config-hash is seeded with the CURRENT
    // hash for this empty scratch HOME, so there is no drift at all. An
    // inverted `!=` in the drift check would still warn here.
    let home = scratch_dir("drift-resume-match");
    let repo_dir = scratch_dir("drift-resume-match-repo");
    let session_id = "sid-resume-match";
    let session_dir = home
        .join(".config")
        .join("playbook")
        .join("runtime")
        .join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    let current_hash = empty_config_hash(&home);
    fs::write(session_dir.join("config-hash"), &current_hash).unwrap();

    // Act: resume, with the stored hash already matching.
    let resumed = run_hook(
        "session-init",
        &repo_dir,
        &home,
        &format!(r#"{{"session_id":"{session_id}","source":"resume"}}"#),
        &[("CLAUDE_PLUGIN_ROOT", plugin_root())],
    );

    // Assert: no drift warning anywhere in the output.
    assert_eq!(resumed.exit_code, 0, "hook should exit 0 on resume");
    let message = system_message(&resumed.stdout);
    assert!(
        !message.contains("drifted"),
        "resume with a matching hash should not warn: {message}"
    );
    let context = additional_context(&resumed.stdout);
    assert!(
        !context.contains("config hash has changed"),
        "resume additionalContext should not mention drift when the hash matches: {context}"
    );
}

// ---------------------------------------------------------------------
// session-init: an unreachable plugin root degrades quietly
// ---------------------------------------------------------------------

#[test]
fn session_init_degrades_quietly_when_both_shell_outs_are_unreachable() {
    // Arrange: a plugin root that does not exist.
    // Every other additionalContext source is disabled so the only thing
    // left that could emit is the (failed) memory slice, proving the
    // failure produces nothing rather than malformed output.
    let home = scratch_dir("shellout-fail");
    let repo_slug = "acme/widget";
    let repo_dir = scratch_dir("shellout-fail-repo");
    init_repo_with_origin(&repo_dir, &format!("git@github.com:{repo_slug}.git"));

    // Act
    let outcome = run_hook(
        "session-init",
        &repo_dir,
        &home,
        r#"{"session_id":"sid-fail","source":"startup"}"#,
        &[
            ("CLAUDE_PLUGIN_ROOT", "/nonexistent-plugin-root-xyz"),
            ("AUTO_LEARN_NUDGE", "0"),
        ],
    );

    // Assert: the hook still exits cleanly and says nothing but the tool map
    // and the code search rule, since there was nothing else left to say once
    // both shell-outs failed.
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert_eq!(
        additional_context(&outcome.stdout),
        format!(
            "{}\n\n{CODE_SEARCH_RULE}",
            playbook::tools_map::render().trim_end()
        ),
        "with everything else disabled, a missing plugin root should yield only the map and the rule: {}",
        outcome.stdout
    );
}

// session-init: a runtime write lands under the new playbook-owned root.
#[test]
fn runtime_and_cc_state_migration_leaves_old_root_untouched() {
    // Arrange: seed the OLD and NEW runtime/cc-state trees with distinct
    // content, so a byte comparison proves the old tree stays untouched.
    let home = scratch_dir("legacy-untouched");
    let session_id = "sid-legacy";

    let old_runtime = home.join(".claude").join("runtime").join(session_id);
    fs::create_dir_all(&old_runtime).unwrap();
    fs::write(old_runtime.join("start-ts"), "1111111111").unwrap();

    let old_cc_state = home.join(".claude").join("cc-state");
    fs::create_dir_all(&old_cc_state).unwrap();
    fs::write(old_cc_state.join("legacy-project"), "old-hash-content").unwrap();

    let new_runtime_root = home.join(".config").join("playbook").join("runtime");
    fs::create_dir_all(&new_runtime_root).unwrap();
    fs::write(new_runtime_root.join("sentinel"), "new-root-marker").unwrap();

    let new_cc_state = home.join(".config").join("playbook").join("cc-state");
    fs::create_dir_all(&new_cc_state).unwrap();
    fs::write(new_cc_state.join("other-project"), "new-hash-content").unwrap();

    let repo_dir = scratch_dir("legacy-untouched-repo");

    // Act: a real session write, at the new root.
    let outcome = run_hook(
        "session-init",
        &repo_dir,
        &home,
        &format!(r#"{{"session_id":"{session_id}"}}"#),
        &[],
    );

    // Assert: the write landed at the NEW runtime root.
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    let new_session_dir = new_runtime_root.join(session_id);
    assert!(
        new_session_dir.join("start-ts").is_file(),
        "session-init should write start-ts under the NEW runtime root"
    );
    assert_eq!(
        fs::read_to_string(new_runtime_root.join("sentinel")).unwrap(),
        "new-root-marker",
        "unrelated pre-existing content at the new root must survive untouched"
    );
    assert_eq!(
        fs::read_to_string(new_cc_state.join("other-project")).unwrap(),
        "new-hash-content",
        "the new cc-state tree must be left byte-unchanged by a runtime-only write"
    );

    // Assert: the OLD tree is byte-unchanged.
    assert_eq!(
        fs::read_to_string(old_runtime.join("start-ts")).unwrap(),
        "1111111111",
        "the old .claude/runtime tree must be left byte-unchanged"
    );
    assert_eq!(
        fs::read_to_string(old_cc_state.join("legacy-project")).unwrap(),
        "old-hash-content",
        "the old .claude/cc-state tree must be left byte-unchanged"
    );
}

// ---------------------------------------------------------------------
// session-clean-exit: the three `.reason` cases
// (the retired shell test cases 1-2, plus the absent case)
// ---------------------------------------------------------------------

fn seeded_session_dir(home: &Path, session_id: &str) -> PathBuf {
    let dir = home
        .join(".config")
        .join("playbook")
        .join("runtime")
        .join(session_id);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn session_clean_exit_reason_absent_refreshes_ts_without_marker() {
    // Arrange: a Stop event, which carries no `.reason` at all.
    let home = scratch_dir("reason-absent");
    let session_dir = seeded_session_dir(&home, "sid-absent");
    let repo_dir = scratch_dir("reason-absent-repo");
    init_repo_with_origin(&repo_dir, "https://github.com/acme/widget.git");

    // Act
    let outcome = run_hook(
        "session-clean-exit",
        &repo_dir,
        &home,
        r#"{"session_id":"sid-absent"}"#,
        &[],
    );

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        session_dir.join("last-clean-ts").is_file(),
        "last-clean-ts should be refreshed even with no reason"
    );
    assert!(
        !session_dir.join("clean-exit").is_file(),
        "no reason should write no clean-exit marker"
    );
}

#[test]
fn session_clean_exit_reason_other_refreshes_ts_without_marker() {
    // Arrange
    let home = scratch_dir("reason-other");
    let session_dir = seeded_session_dir(&home, "sid-other");
    let repo_dir = scratch_dir("reason-other-repo");
    init_repo_with_origin(&repo_dir, "https://github.com/acme/widget.git");

    // Act
    let outcome = run_hook(
        "session-clean-exit",
        &repo_dir,
        &home,
        r#"{"session_id":"sid-other","reason":"other"}"#,
        &[],
    );

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    assert!(
        session_dir.join("last-clean-ts").is_file(),
        "last-clean-ts should be refreshed"
    );
    assert!(
        !session_dir.join("clean-exit").is_file(),
        "reason 'other' should write no clean-exit marker"
    );
}

#[test]
fn session_clean_exit_real_reason_writes_the_marker() {
    // Arrange
    let home = scratch_dir("reason-real");
    let session_dir = seeded_session_dir(&home, "sid-real");
    let repo_dir = scratch_dir("reason-real-repo");
    init_repo_with_origin(&repo_dir, "https://github.com/acme/widget.git");

    // Act
    let outcome = run_hook(
        "session-clean-exit",
        &repo_dir,
        &home,
        r#"{"session_id":"sid-real","reason":"logout"}"#,
        &[],
    );

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    let marker = fs::read_to_string(session_dir.join("clean-exit")).unwrap();
    assert_eq!(
        marker, "logout\n",
        "marker should hold the reason plus a trailing newline, byte for byte"
    );
}

// ---------------------------------------------------------------------
// session-clean-exit: auto-learn queueing
// (the retired shell test cases 3-5)
// ---------------------------------------------------------------------

/// The real git worktree root for `dir`, resolved via `git rev-parse
/// --show-toplevel`, so a test can predict the auto-learn queue's slugified
/// filename without hard-coding a path that may differ once the OS resolves
/// symlinks (e.g. macOS's `/tmp` -> `/private/tmp`).
fn git_toplevel(dir: &Path) -> String {
    let output = Command::new("git")
        .args(["-C", dir.to_str().unwrap(), "rev-parse", "--show-toplevel"])
        .output()
        .expect("git should be available");
    assert!(
        output.status.success(),
        "git rev-parse --show-toplevel failed"
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Mirrors `session_clean_exit::slugify`: replace every character outside
/// `[A-Za-z0-9_.-]` with `_`, matching the regex in
/// the retired shell original.
fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[test]
fn session_clean_exit_queues_auto_learn_flag_with_expected_shape() {
    // Arrange: enough edits recorded to clear the default threshold.
    let home = scratch_dir("auto-learn-flag");
    let session_dir = seeded_session_dir(&home, "sid-flag");
    fs::write(session_dir.join("edit-count"), "9").unwrap();
    let repo_dir = scratch_dir("auto-learn-flag-repo");
    init_repo_with_origin(&repo_dir, "https://github.com/acme/widget.git");
    let expected_filename = format!("{}.json", slugify(&git_toplevel(&repo_dir)));

    // Act
    let outcome = run_hook(
        "session-clean-exit",
        &repo_dir,
        &home,
        r#"{"session_id":"sid-flag","reason":"logout"}"#,
        &[],
    );

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    let to_learn_dir = home
        .join(".config")
        .join("playbook")
        .join("runtime")
        .join("to-learn");
    let entries: Vec<_> = fs::read_dir(&to_learn_dir)
        .expect("to-learn dir should exist")
        .flatten()
        .collect();
    assert_eq!(entries.len(), 1, "exactly one flag should be queued");
    assert_eq!(
        entries[0].file_name().to_string_lossy(),
        expected_filename,
        "queued flag filename should be the slugified repo root"
    );
    let contents = fs::read_to_string(entries[0].path()).unwrap();
    let flag: serde_json::Value = serde_json::from_str(&contents).unwrap();
    let object = flag.as_object().unwrap();
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, vec!["edits", "repo_root", "session_id", "ts"]);
    assert_eq!(flag["edits"], 9);
    assert_eq!(flag["session_id"], "sid-flag");
}

#[test]
fn session_clean_exit_queues_correctly_when_the_lock_is_already_held() {
    // Arrange: the queue is keyed by repo, not session, so two sessions in
    // the same repo can race here. Pre-create the lock directory to
    // simulate another session already writing this repo's flag. The lock
    // is advisory and fails open after its retry budget (a hook must never
    // hang on contention), so this must still succeed and queue a correct,
    // complete flag, not a half-written or missing one.
    let home = scratch_dir("auto-learn-lock-held");
    let session_dir = seeded_session_dir(&home, "sid-lock-held");
    fs::write(session_dir.join("edit-count"), "9").unwrap();
    let repo_dir = scratch_dir("auto-learn-lock-held-repo");
    init_repo_with_origin(&repo_dir, "https://github.com/acme/widget.git");
    let slug = slugify(&git_toplevel(&repo_dir));
    let to_learn_dir = home
        .join(".config")
        .join("playbook")
        .join("runtime")
        .join("to-learn");
    fs::create_dir_all(&to_learn_dir).unwrap();
    let lock_dir = to_learn_dir.join(format!("{slug}.json.lock"));
    fs::create_dir(&lock_dir).expect("pre-creating the lock dir should succeed");

    // Act
    let outcome = run_hook(
        "session-clean-exit",
        &repo_dir,
        &home,
        r#"{"session_id":"sid-lock-held","reason":"logout"}"#,
        &[],
    );

    // Assert
    assert_eq!(
        outcome.exit_code, 0,
        "the hook must fail open, not hang or error, when the lock is already held"
    );
    let dest = to_learn_dir.join(format!("{slug}.json"));
    let contents = fs::read_to_string(&dest).expect("the flag should still be queued correctly");
    let flag: serde_json::Value = serde_json::from_str(&contents)
        .expect("the queued flag must be complete, valid JSON, not a torn write");
    assert_eq!(flag["edits"], 9);
    assert!(
        lock_dir.exists(),
        "a writer that did not acquire the lock must not remove it"
    );
}

#[test]
fn session_clean_exit_at_default_threshold_queues_a_flag() {
    // Arrange: edit-count sits exactly at the default threshold of 5,
    // pinning the `<` comparison `queue_auto_learn` uses against a `<=`
    // mutation that would wrongly skip queuing right at the boundary.
    let home = scratch_dir("at-threshold");
    let session_dir = seeded_session_dir(&home, "sid-at-threshold");
    fs::write(session_dir.join("edit-count"), "5").unwrap();
    let repo_dir = scratch_dir("at-threshold-repo");
    init_repo_with_origin(&repo_dir, "https://github.com/acme/widget.git");

    // Act
    let outcome = run_hook(
        "session-clean-exit",
        &repo_dir,
        &home,
        r#"{"session_id":"sid-at-threshold","reason":"logout"}"#,
        &[],
    );

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    let to_learn_dir = home
        .join(".config")
        .join("playbook")
        .join("runtime")
        .join("to-learn");
    let entries: Vec<_> = fs::read_dir(&to_learn_dir)
        .expect("to-learn dir should exist")
        .flatten()
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "exactly at the default threshold of 5 edits should still queue a flag"
    );
}

#[test]
fn session_clean_exit_below_threshold_queues_no_flag() {
    // Arrange: fewer edits than the default threshold of 5.
    let home = scratch_dir("below-threshold");
    let session_dir = seeded_session_dir(&home, "sid-low");
    fs::write(session_dir.join("edit-count"), "2").unwrap();
    let repo_dir = scratch_dir("below-threshold-repo");
    init_repo_with_origin(&repo_dir, "https://github.com/acme/widget.git");

    // Act
    let outcome = run_hook(
        "session-clean-exit",
        &repo_dir,
        &home,
        r#"{"session_id":"sid-low","reason":"clear"}"#,
        &[],
    );

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    let to_learn_dir = home
        .join(".config")
        .join("playbook")
        .join("runtime")
        .join("to-learn");
    assert!(
        !to_learn_dir.is_dir() || fs::read_dir(&to_learn_dir).unwrap().next().is_none(),
        "below the threshold, no flag should be queued"
    );
}

#[test]
fn session_clean_exit_padded_min_edits_env_var_still_parses() {
    // Arrange: AUTO_LEARN_MIN_EDITS carries leading whitespace, the same way
    // python's `int(...)` would still accept it and parse 3. edit-count sits
    // exactly at that padded threshold, so a Rust parse that instead fell
    // back to the default of 5 would wrongly skip the queue.
    let home = scratch_dir("padded-min-edits");
    let session_dir = seeded_session_dir(&home, "sid-padded");
    fs::write(session_dir.join("edit-count"), "3").unwrap();
    let repo_dir = scratch_dir("padded-min-edits-repo");
    init_repo_with_origin(&repo_dir, "https://github.com/acme/widget.git");

    // Act
    let outcome = run_hook(
        "session-clean-exit",
        &repo_dir,
        &home,
        r#"{"session_id":"sid-padded","reason":"logout"}"#,
        &[("AUTO_LEARN_MIN_EDITS", " 3")],
    );

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    let to_learn_dir = home
        .join(".config")
        .join("playbook")
        .join("runtime")
        .join("to-learn");
    let entries: Vec<_> = fs::read_dir(&to_learn_dir)
        .expect("to-learn dir should exist")
        .flatten()
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "a padded threshold should still parse to 3, so 3 edits should queue a flag"
    );
}

#[test]
fn session_clean_exit_auto_learn_nudge_disabled_skips_queue() {
    // Arrange: enough edits, but AUTO_LEARN_NUDGE=0.
    let home = scratch_dir("nudge-disabled");
    let session_dir = seeded_session_dir(&home, "sid-off");
    fs::write(session_dir.join("edit-count"), "9").unwrap();
    let repo_dir = scratch_dir("nudge-disabled-repo");
    init_repo_with_origin(&repo_dir, "https://github.com/acme/widget.git");

    // Act
    let outcome = run_hook(
        "session-clean-exit",
        &repo_dir,
        &home,
        r#"{"session_id":"sid-off","reason":"clear"}"#,
        &[("AUTO_LEARN_NUDGE", "0")],
    );

    // Assert
    assert_eq!(outcome.exit_code, 0, "hook should exit 0");
    let to_learn_dir = home
        .join(".config")
        .join("playbook")
        .join("runtime")
        .join("to-learn");
    assert!(
        !to_learn_dir.is_dir() || fs::read_dir(&to_learn_dir).unwrap().next().is_none(),
        "AUTO_LEARN_NUDGE=0 should disable the queue even above threshold"
    );
}

/// A landed, unlocked Wu-convention worktree under `home`, for a repo whose
/// origin is `owner/repo`: `playbook worktree sweep`'s simplest removable
/// fixture, matching `tests/worktree_cli.rs`'s own convention.
fn build_sweepable_wu_worktree(repo_dir: &Path, home: &Path, owner: &str, repo: &str) -> PathBuf {
    let head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo_dir)
            .output()
            .expect("git rev-parse should run")
            .stdout,
    )
    .expect("git output should be utf8")
    .trim()
    .to_string();
    run_git(repo_dir, &["update-ref", "refs/remotes/origin/main", &head]);

    let wu_path = home
        .join(".config")
        .join("playbook")
        .join("repos")
        .join(owner)
        .join(repo)
        .join("wt-abc123")
        .join("worktrees")
        .join("plan-slug")
        .join("wu-1");
    fs::create_dir_all(wu_path.parent().expect("wu path has a parent")).unwrap();
    run_git(
        repo_dir,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            wu_path.to_str().expect("utf8 path"),
            "HEAD",
        ],
    );
    // A long-lived worktree: brand new ones are never swept (#631).
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
    fs::File::open(wu_path.join(".git"))
        .expect("open .git file")
        .set_modified(old)
        .expect("backdate worktree");
    wu_path.canonicalize().expect("wu worktree should resolve")
}

#[test]
fn session_init_sweeps_worktrees_when_no_marker_exists() {
    // Arrange: no prior marker, a landed unlocked Wu-convention worktree.
    let work = scratch_dir("sweep-no-marker");
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, "git@github.com:acme/sweep-a.git");
    let home = work.join("home");
    fs::create_dir_all(&home).unwrap();
    let repo_dir = repo_dir.canonicalize().expect("repo dir should resolve");
    let home = home.canonicalize().expect("home should resolve");
    let wu_path = build_sweepable_wu_worktree(&repo_dir, &home, "acme", "sweep-a");

    // Act
    let outcome = run_hook(
        "session-init",
        &repo_dir,
        &home,
        "{}",
        &[("CLAUDE_PLUGIN_ROOT", plugin_root())],
    );

    // Assert
    assert_eq!(
        outcome.exit_code, 0,
        "hook should exit 0: {}",
        outcome.stderr
    );
    assert!(
        fs::symlink_metadata(&wu_path).is_err(),
        "the landed worktree should have been swept on the first-ever SessionStart"
    );
    assert!(
        sweep_marker(&home, &repo_dir).is_some(),
        "the rate-limit marker should be written after a sweep runs"
    );
}

fn sweep_marker(home: &Path, repo: &Path) -> Option<i64> {
    let root = home.join(".config").join("playbook");
    playbook::state::get(&root, &worktree_sweep_marker_key(repo))
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
}

fn set_sweep_marker(home: &Path, repo: &Path, epoch: i64) {
    let root = home.join(".config").join("playbook");
    playbook::state::set(&root, &worktree_sweep_marker_key(repo), &epoch.to_string()).unwrap();
}

#[test]
fn session_init_skips_sweep_when_marker_is_fresh() {
    // Arrange: a marker just written (well within the rate-limit interval).
    let work = scratch_dir("sweep-fresh-marker");
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, "git@github.com:acme/sweep-b.git");
    let home = work.join("home");
    fs::create_dir_all(&home).unwrap();
    let repo_dir = repo_dir.canonicalize().expect("repo dir should resolve");
    let home = home.canonicalize().expect("home should resolve");
    let wu_path = build_sweepable_wu_worktree(&repo_dir, &home, "acme", "sweep-b");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should read after the unix epoch")
        .as_secs();
    set_sweep_marker(&home, &repo_dir, now as i64);
    let before = sweep_marker(&home, &repo_dir);

    // Act
    let outcome = run_hook(
        "session-init",
        &repo_dir,
        &home,
        "{}",
        &[("CLAUDE_PLUGIN_ROOT", plugin_root())],
    );

    // Assert: left alone, no real sleep needed since the marker was just written.
    assert_eq!(
        outcome.exit_code, 0,
        "hook should exit 0: {}",
        outcome.stderr
    );
    assert!(
        fs::symlink_metadata(&wu_path).is_ok(),
        "a fresh rate-limit marker should skip the sweep entirely"
    );
    // A skipped sweep must not touch the marker either: refreshing it here
    // would slide the window forward on every SessionStart and the sweep
    // would never actually run again after the first one.
    let after = sweep_marker(&home, &repo_dir);
    assert_eq!(
        after, before,
        "a skipped sweep must not refresh the rate-limit marker"
    );
}

#[test]
fn session_init_sweeps_again_when_marker_is_stale() {
    // Arrange: a marker backdated past the rate-limit interval.
    const WORKTREE_SWEEP_INTERVAL_SECS: i64 = 86_400;
    let work = scratch_dir("sweep-stale-marker");
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, "git@github.com:acme/sweep-c.git");
    let home = work.join("home");
    fs::create_dir_all(&home).unwrap();
    let repo_dir = repo_dir.canonicalize().expect("repo dir should resolve");
    let home = home.canonicalize().expect("home should resolve");
    let wu_path = build_sweepable_wu_worktree(&repo_dir, &home, "acme", "sweep-c");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should read after the unix epoch")
        .as_secs() as i64;
    let stale_epoch = now - (WORKTREE_SWEEP_INTERVAL_SECS + 1);
    set_sweep_marker(&home, &repo_dir, stale_epoch);

    // Act
    let outcome = run_hook(
        "session-init",
        &repo_dir,
        &home,
        "{}",
        &[("CLAUDE_PLUGIN_ROOT", plugin_root())],
    );

    // Assert: swept again, marker refreshed.
    assert_eq!(
        outcome.exit_code, 0,
        "hook should exit 0: {}",
        outcome.stderr
    );
    assert!(
        fs::symlink_metadata(&wu_path).is_err(),
        "a stale rate-limit marker should let the sweep run again"
    );
    let refreshed = sweep_marker(&home, &repo_dir).expect("marker should exist");
    assert!(
        now - refreshed < 60 && refreshed > stale_epoch,
        "the marker should have been refreshed to roughly now"
    );
}

#[test]
fn session_init_skips_sweep_entirely_when_policy_disabled() {
    // Arrange: worktree.cleanup.enabled set false at the repo tier, no prior
    // marker.
    let work = scratch_dir("sweep-disabled");
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, "git@github.com:acme/sweep-d.git");
    let home = work.join("home");
    fs::create_dir_all(&home).unwrap();
    let repo_dir = repo_dir.canonicalize().expect("repo dir should resolve");
    let home = home.canonicalize().expect("home should resolve");
    let wu_path = build_sweepable_wu_worktree(&repo_dir, &home, "acme", "sweep-d");
    let set_out = Command::new(playbook_bin())
        .args(["config", "set", "worktree.cleanup.enabled", "false"])
        .current_dir(&repo_dir)
        .env("HOME", &home)
        .output()
        .expect("playbook config set should run");
    assert!(
        set_out.status.success(),
        "config set should succeed: {}",
        String::from_utf8_lossy(&set_out.stderr)
    );

    // Act
    let outcome = run_hook(
        "session-init",
        &repo_dir,
        &home,
        "{}",
        &[("CLAUDE_PLUGIN_ROOT", plugin_root())],
    );

    // Assert: no sweep attempt at all, and no marker written either, so
    // re-enabling the policy later does not have to wait out a stale marker.
    assert_eq!(
        outcome.exit_code, 0,
        "hook should exit 0: {}",
        outcome.stderr
    );
    assert!(
        fs::symlink_metadata(&wu_path).is_ok(),
        "sweep should not run at all while the policy is disabled"
    );
    assert!(
        sweep_marker(&home, &repo_dir).is_none(),
        "no marker should be written when the sweep is skipped for being disabled"
    );
}

#[test]
fn worktree_sweep_due_covers_absent_and_aged_markers() {
    // A pure boundary check, matching `cc::worktree::cleanup_due`'s own
    // signature and inclusive-boundary convention: the interval itself
    // already counts as due.
    const NOW: i64 = 1_700_000_000;
    const INTERVAL: i64 = 86_400;

    assert!(
        playbook::hooks::session_init::worktree_sweep_due(None, NOW),
        "no marker at all should always be due"
    );
    assert!(
        !playbook::hooks::session_init::worktree_sweep_due(Some(NOW - 3600), NOW),
        "an hour-old marker should not be due"
    );
    assert!(
        !playbook::hooks::session_init::worktree_sweep_due(Some(NOW - (INTERVAL - 1)), NOW),
        "just under the interval should not be due"
    );
    assert!(
        playbook::hooks::session_init::worktree_sweep_due(Some(NOW - INTERVAL), NOW),
        "exactly at the interval should be due"
    );
    assert!(
        playbook::hooks::session_init::worktree_sweep_due(Some(NOW - INTERVAL - 1), NOW),
        "past the interval should be due"
    );
}

// ---------------------------------------------------------------------
// session-init: auto-mode block
// ---------------------------------------------------------------------

const SESSION_START: &str =
    r#"{"hook_event_name":"SessionStart","source":"startup","session_id":"auto-session"}"#;
const OFF_SWITCH: &str = "playbook mode ask";
const ENV_OFF_SWITCH: &str = "unset `PLAYBOOK_MODE`";
const AUTO_RULE: &str = "wherever the command allows it, take the recommended answer and log it as an assumption instead of asking";

/// SessionStart `additionalContext` from the real binary under the isolated
/// auto env, with the scratch config and the extra variables applied.
fn auto_session_context(tag: &str, config_mode: Option<&str>, env: &[(&str, &str)]) -> String {
    let s = auto_env::scratch(tag);
    if let Some(mode) = config_mode {
        s.seed_mode_config(mode);
    }
    let (out, code) = auto_run::run_hook(&s, "session-init", SESSION_START, env);
    assert_eq!(code, 0, "session-init should exit 0");
    additional_context(&out)
}

#[test]
fn session_init_in_auto_from_config_names_the_source_and_the_off_switch() {
    // Arrange / Act
    let context = auto_session_context("auto-config", Some("auto"), &[]);

    // Assert
    assert!(
        context.contains("AUTO MODE"),
        "an auto block should be injected: {context}"
    );
    assert!(
        context.contains("source: config, set for this repo"),
        "the block should name the config source: {context}"
    );
    assert!(
        context.contains(&format!("To turn it off, run `{OFF_SWITCH}`.")),
        "the block should name the off switch: {context}"
    );
    assert!(
        context.contains(AUTO_RULE),
        "the block should carry the standing rule: {context}"
    );
    assert!(
        !context.contains("never ask"),
        "plain --auto still lets a command ask: {context}"
    );
}

#[test]
fn session_init_in_auto_from_env_names_the_env_source() {
    // Arrange / Act
    let context = auto_session_context("auto-env", None, &[("PLAYBOOK_MODE", "auto")]);

    // Assert
    assert!(
        context.contains("source: env"),
        "the block should name the env source: {context}"
    );
    assert!(
        context.contains(&format!("To turn it off, {ENV_OFF_SWITCH}.")),
        "the env off switch is unsetting the variable: {context}"
    );
    assert!(
        !context.contains(OFF_SWITCH),
        "`playbook mode ask` cannot undo an env source: {context}"
    );
}

#[test]
fn session_init_in_ask_emits_no_auto_block() {
    // Arrange / Act
    let context = auto_session_context("ask", None, &[]);

    // Assert
    assert!(
        !context.contains("AUTO MODE") && !context.contains(OFF_SWITCH),
        "ask mode must add no auto block: {context}"
    );
}

#[test]
fn session_init_in_auto_with_headless_keeps_the_auto_block() {
    // Arrange / Act
    let context =
        auto_session_context("auto-headless", Some("auto"), &[("PLAYBOOK_HEADLESS", "1")]);

    // Assert
    assert!(
        context.contains("source: config") && context.contains(OFF_SWITCH),
        "headless must not drop the auto block: {context}"
    );
}

// ---------------------------------------------------------------------
// session-init: the ranked memory block
// ---------------------------------------------------------------------

/// A scratch HOME holding `graph` and `signals`, plus a repo whose origin is
/// `acme/widget`. Returns `(repo_dir, home)`.
fn ranked_world(tag: &str, graph: &str, signals: Option<&str>) -> (PathBuf, PathBuf) {
    let work = scratch_dir(tag);
    let repo_dir = work.join("repo");
    init_repo_with_origin(&repo_dir, "git@github.com:acme/widget.git");
    let home = work.join("home");
    let memory_dir = home.join(".config").join("playbook").join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    playbook::testing::write_graph_fixture(&memory_dir, graph);
    if let Some(signals) = signals {
        fs::write(memory_dir.join("memory.signals.json"), signals).unwrap();
    }
    (repo_dir, home)
}

fn node(name: &str, extra: &str) -> String {
    format!(
        r#"{{"id":"acme/widget/{name}","file":"acme/widget/{name}.md","scope":"project","type":"project","name":"{name}","description":"About {name}.","project":"acme/widget"{extra}}}"#
    )
}

#[test]
fn memory_block_ranks_pinned_then_used_then_linked_and_drops_the_rest() {
    // Arrange: one fact per rank, plus a fact with no signal at all.
    let nodes = [
        node("zz-linked", ""),
        node("yy-used", ""),
        node("xx-pinned", r#","pinned":true"#),
        node("ww-quiet", ""),
        node("vv-partner", ""),
    ]
    .join(",");
    let graph = format!(
        r#"{{"nodes":[{nodes}],"edges":[{{"from":"acme/widget/zz-linked","to":"acme/widget/vv-partner","relation":"relates_to"}}]}}"#
    );
    let signals = r#"{"nodes":{"acme/widget/yy-used":{"hits":4,"promoted":true}}}"#;
    let (repo_dir, home) = ranked_world("rank-order", &graph, Some(signals));

    // Act
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);
    let context = additional_context(&outcome.stdout);

    // Assert
    let at = |name: &str| context.find(&format!("- {name}:"));
    assert!(
        at("xx-pinned") < at("yy-used") && at("yy-used") < at("zz-linked"),
        "pinned, then used, then linked: {context}"
    );
    assert!(at("xx-pinned").is_some());
    assert!(
        at("ww-quiet").is_none(),
        "a fact with no pin, use or link is left out: {context}"
    );
}

#[test]
fn memory_block_is_empty_when_no_fact_has_any_signal() {
    // Arrange
    let graph = format!(r#"{{"nodes":[{}],"edges":[]}}"#, node("quiet-fact", ""));
    let (repo_dir, home) = ranked_world("rank-none", &graph, None);

    // Act
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);

    // Assert
    assert!(
        !additional_context(&outcome.stdout).contains("quiet-fact"),
        "{}",
        outcome.stdout
    );
}

#[test]
fn session_init_never_writes_to_the_memory_store() {
    // Arrange
    let graph = format!(
        r#"{{"nodes":[{}],"edges":[]}}"#,
        node("pinned-fact", r#","pinned":true"#)
    );
    let signals = r#"{"nodes":{"acme/widget/pinned-fact":{"hits":2}}}"#;
    let (repo_dir, home) = ranked_world("rank-readonly", &graph, Some(signals));
    let memory_dir = home.join(".config").join("playbook").join("memory");
    // The ranking reads these two files; the one-off migration marker that
    // session start has always written is outside this check.
    let snapshot = |dir: &Path| -> Vec<Vec<u8>> {
        [
            dir.join("memory.signals.json"),
            dir.parent().unwrap().join("data").join("memory.db"),
        ]
        .iter()
        .map(|path| fs::read(path).unwrap_or_default())
        .collect()
    };
    let before = snapshot(&memory_dir);

    // Act
    let outcome = run_hook("session-init", &repo_dir, &home, "{}", &[]);

    // Assert
    assert!(additional_context(&outcome.stdout).contains("pinned-fact"));
    assert_eq!(before, snapshot(&memory_dir));
}

#[test]
fn a_huge_handoff_is_cut_so_the_whole_context_stays_under_the_harness_cap() {
    // Arrange: a 30,000 character handoff plus a pinned fact.
    let graph = format!(
        r#"{{"nodes":[{}],"edges":[]}}"#,
        node("pinned-fact", r#","pinned":true"#)
    );
    let (repo_dir, home) = ranked_world("rank-handoff-cap", &graph, None);
    write_handoff(&home, &repo_dir, &"handoff line\n".repeat(2500));

    // Act
    let outcome = run_session_init_at(&repo_dir, &home);
    let context = additional_context(&outcome.stdout);

    // Assert: the harness replaces anything past 10,000 characters.
    assert!(
        context.chars().count() <= 9000,
        "context is {} chars",
        context.chars().count()
    );
    assert!(context.contains("handoff cut to fit"), "{context}");
    assert!(
        context.contains("pinned-fact"),
        "memory still fits: {context}"
    );
    assert_eq!(context.matches(CODE_LINE).count(), 1, "{context}");
    assert_eq!(context.matches(CODE_SEARCH_RULE).count(), 1, "{context}");
}

#[test]
fn a_resume_clears_the_auto_cost_state_but_a_compaction_keeps_it() {
    // Arrange
    let repo_dir = scratch_dir("resume-repo");
    let home = scratch_dir("resume-home");
    init_repo_with_origin(&repo_dir, "https://github.com/acme/widgets.git");
    let session = home.join(".config/playbook/runtime/sid-resume");
    let markers = [
        "auto-cost.json",
        "warned",
        "warned-abc",
        "budget-note",
        "agent-budget-note",
        "park-note.md",
        "park-note-abc.md",
        "auto-advice-given",
        "parked",
    ];
    let seed = || {
        fs::create_dir_all(&session).expect("session dir");
        for name in markers {
            fs::write(session.join(name), "x").expect("marker");
        }
    };

    // Act and Assert: compaction is the same session again
    seed();
    let compact = r#"{"session_id":"sid-resume","source":"compact"}"#;
    run_hook("session-init", &repo_dir, &home, compact, &[]);
    for name in markers {
        assert!(
            session.join(name).exists(),
            "{name} must survive a compaction"
        );
    }

    // Act and Assert: a resume starts a new count
    let resume = r#"{"session_id":"sid-resume","source":"resume"}"#;
    run_hook("session-init", &repo_dir, &home, resume, &[]);
    for name in markers {
        assert!(!session.join(name).exists(), "{name} must go on a resume");
    }
}

#[test]
fn session_init_injects_the_tool_map_on_every_kind_of_start() {
    // Arrange
    let work = scratch_dir("tool-map");
    let repo_dir = work.join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    let home = work.join("home");

    for source in ["startup", "resume", "compact"] {
        // Act
        let outcome = run_session_init_with_source(&repo_dir, &home, source);
        let context = additional_context(&outcome.stdout);

        // Assert
        assert_eq!(outcome.exit_code, 0, "{source}: hook should exit 0");
        assert!(
            context.contains("playbook tools: when an intent is covered here"),
            "{source}: the tool map header is missing: {context}"
        );
        assert!(
            context.contains("\npath plans|implement|worktrees --create"),
            "{source}: the tool map lines are missing: {context}"
        );
        assert_eq!(
            context.matches(CODE_LINE).count(),
            1,
            "{source}: the code line should appear once: {context}"
        );
        assert_eq!(
            context.matches(CODE_SEARCH_RULE).count(),
            1,
            "{source}: the rule sentence should appear once: {context}"
        );
    }
}

#[test]
fn session_init_injects_the_tool_map_before_a_handoff() {
    // Arrange
    let work = scratch_dir("tool-map-order");
    let repo_dir = work.join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    let home = work.join("home");
    write_handoff(&home, &repo_dir, "HANDOFFMARKER decisions from last time");

    // Act
    let context = additional_context(&run_session_init_at(&repo_dir, &home).stdout);

    // Assert
    let position = |marker: &str| {
        context
            .find(marker)
            .unwrap_or_else(|| panic!("{marker} is missing: {context}"))
    };
    let map = position("playbook tools:");
    let code_line = position(CODE_LINE);
    let rule = position("Code search rule:");
    let handoff = position("HANDOFFMARKER");
    assert!(
        map < code_line && code_line < rule && rule < handoff,
        "expected map, code line, rule, handoff in that order: {context}"
    );
}

#[test]
fn session_init_names_the_upgrade_when_the_plugin_needs_a_newer_binary() {
    // Arrange: the installed plugin asks for a binary far newer than this one.
    let home = scratch_dir("min-binary");
    let plugin = home.join(".claude/plugins/cache/amplifiedengineer/playbook/0.31.0");
    fs::create_dir_all(plugin.join("skills")).unwrap();
    fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
    fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        r#"{"name":"playbook","version":"0.31.0","metadata":{"minBinaryVersion":"999.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        home.join(".claude/plugins/installed_plugins.json"),
        format!(
            r#"{{"version":2,"plugins":{{"playbook@amplifiedengineer":[{{"scope":"user","installPath":"{}"}}]}}}}"#,
            plugin.display()
        ),
    )
    .unwrap();

    // Act
    let out = run_hook("session-init", &home, &home, r#"{"source":"startup"}"#, &[]);

    // Assert
    assert_eq!(out.exit_code, 0, "{}", out.stderr);
    let value: serde_json::Value = serde_json::from_str(&out.stdout).expect("json payload");
    let message = value["systemMessage"].as_str().unwrap_or_default();
    assert!(
        message.contains(&format!(
            "  - Playbook binary {} is older than this plugin needs (999.0.0).\n",
            env!("CARGO_PKG_VERSION")
        )),
        "{message}"
    );
    assert!(message.contains("    Fix: playbook upgrade\n"), "{message}");
}

#[test]
fn session_init_names_the_setup_command_for_a_pending_settings_change() {
    // Arrange: settings.json still runs the old status line script.
    let home = scratch_dir("heal-line");
    let script = playbook::init::statusline::playbook_statusline_path(&home);
    fs::create_dir_all(home.join(".claude")).unwrap();
    let settings = home.join(".claude/settings.json");
    fs::write(
        &settings,
        format!(
            r#"{{"attribution":{{"commit":"","pr":"","sessionUrl":false}},"statusLine":{{"type":"command","command":"bash {}"}}}}"#,
            script.display()
        ),
    )
    .unwrap();
    let before = fs::read(&settings).unwrap();

    // Act
    let out = run_hook("session-init", &home, &home, r#"{"source":"startup"}"#, &[]);

    // Assert
    assert_eq!(out.exit_code, 0, "{}", out.stderr);
    let value: serde_json::Value = serde_json::from_str(&out.stdout).expect("json payload");
    let message = value["systemMessage"].as_str().unwrap_or_default();
    assert!(
        message.contains("    Fix: playbook setup --statusline\n"),
        "{message}"
    );
    assert_eq!(fs::read(&settings).unwrap(), before);
}
