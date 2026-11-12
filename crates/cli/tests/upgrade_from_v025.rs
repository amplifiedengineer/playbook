// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Upgrade simulation (ADR 0034): a home laid out the way playbook 0.25 left
//! it meets this binary. The session hook heals playbook's own files and
//! names what needs the user, `doctor --fix` lists the same, and only `init`
//! changes settings.json and the shell rc file.

use playbook::init::run::{run, InitPaths};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const DEAD_SESSION: &str = "bash ~/.claude/hooks/session-init.sh";
const DEAD_GUARD: &str = "~/.claude/hooks/pre-tool-guard.sh";
const MINE: &str = "bash ~/.claude/hooks/mine.sh";

struct V025Home {
    home: PathBuf,
}

impl V025Home {
    fn new(tag: &str) -> V025Home {
        let home = std::env::temp_dir().join(format!(
            "playbook-upgrade-v025-{tag}-{}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&home);
        let claude = home.join(".claude");
        let statusline = playbook::init::statusline::playbook_statusline_path(&home);
        write(&claude.join("hooks/mine.sh"), "#!/bin/sh\n");
        write(
            &claude.join("settings.json"),
            &format!(
                "{:#}\n",
                json!({
                    "model": "opus",
                    "statusLine": {"type": "command", "command": format!("bash {}", statusline.display())},
                    "hooks": {
                        "SessionStart": [{"hooks": [{"type": "command", "command": DEAD_SESSION}]}],
                        "PreToolUse": [{"matcher": "Bash", "hooks": [
                            {"type": "command", "command": DEAD_GUARD},
                            {"type": "command", "command": MINE}
                        ]}]
                    }
                })
            ),
        );
        write(
            &home.join(".zshrc"),
            "export EDITOR=vim\nsource ~/.claude/shell/zsh/cc.zsh\n",
        );
        write(&home.join("notes.md"), "my own notes\n");
        let cache = claude.join("plugins/cache/pragmatic-engineer/playbook");
        for version in ["0.24.0", "0.25.0"] {
            write(
                &cache.join(version).join(".claude-plugin/plugin.json"),
                &format!(r#"{{"name":"playbook","version":"{version}"}}"#),
            );
            fs::create_dir_all(cache.join(version).join("skills")).unwrap();
        }
        write(&cache.join("0.24.0/.orphaned_at"), "1");
        write(
            &claude.join("plugins/installed_plugins.json"),
            &format!(
                r#"{{"version":2,"plugins":{{"playbook@pragmatic-engineer":[{{"scope":"user","installPath":"{}","version":"0.25.0"}}],"playbook-hooks@pragmatic-engineer":[{{"scope":"user"}}]}}}}"#,
                cache.join("0.25.0").display()
            ),
        );
        let root = playbook::common::paths::playbook_root_from(&home);
        write(
            &root.join("hooks/lib/config-hash.sh"),
            "config_hash() { :; }\n",
        );
        V025Home { home }
    }

    fn settings(&self) -> PathBuf {
        self.home.join(".claude/settings.json")
    }

    fn user_files(&self) -> Vec<Vec<u8>> {
        let claude = self.home.join(".claude/plugins");
        [
            self.settings(),
            self.home.join(".zshrc"),
            self.home.join("notes.md"),
            claude.join("installed_plugins.json"),
            claude.join("cache/pragmatic-engineer/playbook/0.24.0/.orphaned_at"),
        ]
        .iter()
        .map(|p| fs::read(p).unwrap())
        .collect()
    }

    fn playbook(&self, args: &[&str], stdin: &str) -> Output {
        self.playbook_env(args, stdin, &[])
    }

    fn playbook_env(&self, args: &[&str], stdin: &str, envs: &[(&str, &str)]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env("SHELL", "/bin/zsh")
            // No real `claude plugin update` from a test.
            .env("PLAYBOOK_SKIP_PLUGIN_UPDATE", "1")
            .env_remove("CLAUDE_PLUGIN_ROOT")
            .env_remove("PLAYBOOK_PLUGIN_DIR")
            .env_remove("HOOK_INPUT")
            .env_remove("CI")
            .env_remove("PLAYBOOK_HEADLESS")
            .envs(envs.iter().copied())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn session_message(&self) -> String {
        let out = self.playbook(&["hook", "session-init"], r#"{"source":"startup"}"#);
        assert_eq!(out.status.code(), Some(0));
        let value: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        value["systemMessage"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn init(&self) {
        let outcome = run(&InitPaths {
            plugin: playbook::init::run::PluginRun::Skip,
            plugin_root: None,
            self_root: Some(PathBuf::from(env!("PLAYBOOK_REPO_ROOT")).join("seed")),
            claude_home: self.home.join(".claude"),
            home: self.home.clone(),
            system_prompt: playbook::init::system_prompt::Answer::Unasked,
            hooks: true,
            statusline: true,
            security: false,
            telemetry: false,
            telemetry_replace_off: false,
            only: false,
            short_name: playbook::init::pb_link::Choice::Skip,
            policy_guard: false,
            codex_hooks: false,
            pb_target: None,
            completions: None,
            attribution: playbook::init::attribution::Choice::Off { replace: false },
            path_setup: None,
            repo: None,
        });
        assert!(outcome.ok());
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn hook_commands(settings: &Path) -> Vec<String> {
    let value: Value = serde_json::from_str(&fs::read_to_string(settings).unwrap()).unwrap();
    let mut out = Vec::new();
    for groups in value["hooks"].as_object().unwrap().values() {
        for group in groups.as_array().unwrap() {
            for entry in group["hooks"].as_array().unwrap() {
                out.push(entry["command"].as_str().unwrap().to_string());
            }
        }
    }
    out
}

#[test]
fn a_v025_home_heals_its_own_files_and_leaves_the_rest_to_init() {
    // Arrange
    let h = V025Home::new("heal");
    let user_files = h.user_files();
    let config_hash =
        playbook::common::paths::playbook_root_from(&h.home).join("hooks/lib/config-hash.sh");

    // Act: the first session after the binary upgrade, then a second one.
    let first = h.session_message();
    let second = h.session_message();
    let fix = h.playbook(&["doctor", "--fix"], "");

    // Assert: playbook's own file healed, user files untouched, one command each.
    assert!(!config_hash.exists());
    assert_eq!(h.user_files(), user_files);
    assert!(
        first.contains("of its own files after the upgrade"),
        "{first}"
    );
    // One bullet per item, each with its own command; the second session
    // the same day stays quiet (ADR 0034, 2026-10-19 amendment).
    assert!(first.contains("5 items need you:\n"), "{first}");
    for fix in [
        "Fix: playbook setup --statusline\n",
        "Fix: playbook setup --hooks\n",
        "Fix: playbook setup\n",
        "Fix: claude plugin uninstall playbook-hooks@pragmatic-engineer\n",
        "Fix: claude plugin update playbook@pragmatic-engineer\n",
    ] {
        assert!(first.contains(fix), "{fix}: {first}");
    }
    assert!(!first.contains("item(s) need you"), "{first}");
    assert_eq!(second, "", "shown less than a day ago");
    let report = String::from_utf8_lossy(&fix.stdout);
    for line in [
        "Fix: claude plugin uninstall playbook-hooks@pragmatic-engineer",
        "Fix: claude plugin update playbook@pragmatic-engineer",
        "orphaned-plugin-caches: 1 old plugin folder(s)",
        "Fix: playbook setup --hooks",
    ] {
        assert!(report.contains(line), "{report}");
    }
    assert!(
        !report.contains("attribution-off"),
        "the item is retired: {report}"
    );
    assert_eq!(h.user_files(), user_files);

    // Act: the user runs init.
    h.init();
    let after_init = fs::read(h.settings()).unwrap();
    h.init();

    // Assert: dead entries gone, the user's own hook kept, settings backed up,
    // the legacy launcher line removed, and a second init changes nothing.
    let commands = hook_commands(&h.settings());
    assert!(commands.iter().any(|c| c == MINE), "{commands:?}");
    assert!(!commands
        .iter()
        .any(|c| c == DEAD_SESSION || c == DEAD_GUARD));
    let backups: Vec<_> = fs::read_dir(h.home.join(".claude"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("settings.json.bak.")
        })
        .collect();
    assert!(!backups.is_empty());
    let zshrc = fs::read_to_string(h.home.join(".zshrc")).unwrap();
    assert_eq!(zshrc, "export EDITOR=vim\n");
    let rc_backup = fs::read_dir(&h.home)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with(".zshrc.bak-"))
        .expect("the rc file is backed up first");
    assert_eq!(
        fs::read_to_string(rc_backup.path()).unwrap(),
        "export EDITOR=vim\nsource ~/.claude/shell/zsh/cc.zsh\n"
    );
    assert_eq!(
        fs::read(h.home.join("notes.md")).unwrap(),
        b"my own notes\n"
    );
    assert_eq!(fs::read(h.settings()).unwrap(), after_init);
    assert_eq!(h.user_files()[2..], user_files[2..]);
    let settled = h.session_message();
    assert!(!settled.contains("Fix: playbook setup\n"), "{settled}");
    assert!(
        settled.contains("Fix: playbook setup --only system-prompt"),
        "an earlier install keeps the prompt question open: {settled}"
    );
    assert!(
        !settled.contains("claude plugin"),
        "the report items showed less than a day ago: {settled}"
    );
}

/// Every file under `dir` with its bytes, sorted by path.
fn tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((path.clone(), fs::read(&path).unwrap_or_default()));
            }
        }
    }
    out.sort();
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn migrate_on_a_v025_home_previews_runs_once_and_leaves_user_files_alone() {
    // Arrange
    let h = V025Home::new("migrate");
    let user_files = h.user_files();
    let before = tree(&h.home);

    // Act: look first, without writing.
    let status = h.playbook(&["migrate", "--status"], "");
    let dry = h.playbook(&["migrate", "--dry-run"], "");
    let dry_json = h.playbook(&["migrate", "--dry-run", "--json"], "");

    // Assert: both previews exit 0, name the work, and write nothing.
    assert_eq!(tree(&h.home), before);
    assert_eq!(status.status.code(), Some(0));
    let status = stdout(&status);
    assert!(status.contains("recorded: nothing yet"), "{status}");
    let dead = status
        .lines()
        .find(|l| l.starts_with("0.34.0  dead-hook-scripts "))
        .unwrap_or_default();
    assert!(
        dead.contains("consent") && dead.contains("needs you"),
        "{status}"
    );
    let dry = stdout(&dry);
    assert!(dry.starts_with("would run: "), "{dry}");
    for line in [
        "Fix: playbook setup --hooks",
        "note: orphaned-plugin-caches: 1 old plugin folder(s)",
        "claude plugin uninstall playbook-hooks@pragmatic-engineer",
    ] {
        assert!(dry.contains(line), "{dry}");
    }
    assert!(dry.ends_with("dry run: nothing was written\n"), "{dry}");
    let preview: Value = serde_json::from_slice(&dry_json.stdout).unwrap();
    assert_eq!(preview["written"], false);
    assert!(!preview["would_run"].as_array().unwrap().is_empty());

    // Act: run it, then again.
    let first = h.playbook(&["migrate", "--json"], "");
    let second = h.playbook(&["migrate", "--json"], "");
    let healed = tree(&h.home);
    let after = h.playbook(&["migrate", "--dry-run"], "");
    let status_after = stdout(&h.playbook(&["migrate", "--status"], ""));

    // Assert: the first run heals and records, the second changes nothing,
    // and Claude Code's settings and the shell rc file stay as they were.
    assert_eq!(first.status.code(), Some(0), "{}", stdout(&first));
    assert_eq!(second.status.code(), Some(0), "{}", stdout(&second));
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(first["recorded"], true);
    assert!(!first["changed"].as_array().unwrap().is_empty(), "{first}");
    assert_eq!(second["changed"], json!([]), "{second}");
    assert_eq!(second["failed"], json!([]), "{second}");
    assert_eq!(second["recorded"], true, "{second}");
    assert!(second["open"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["command"] == "playbook setup --hooks"));
    assert!(
        stdout(&after).starts_with("would run: memory-store-move"),
        "{}",
        stdout(&after)
    );
    let recorded = format!("recorded: playbook {}\n", env!("CARGO_PKG_VERSION"));
    assert!(status_after.contains(&recorded), "{status_after}");
    assert!(!status_after.contains("not checked"), "{status_after}");
    assert_eq!(tree(&h.home), healed);
    assert_eq!(h.user_files(), user_files);
}

#[test]
fn migrate_previews_write_nothing_in_a_half_set_up_home_and_fail_on_a_bad_store() {
    // Arrange: a store made by the config store alone, next to an old state
    // file.
    let h = V025Home::new("migrate-half");
    let root = playbook::common::paths::playbook_root_from(&h.home);
    drop(playbook::config::store::open(&root).unwrap());
    write(
        &root.join("migrations.state"),
        "applied memory-store-move\n",
    );
    let before = tree(&h.home);

    // Act
    let status = h.playbook(&["migrate", "--status"], "");
    let dry = h.playbook(&["migrate", "--dry-run", "--json"], "");

    // Assert
    assert_eq!(status.status.code(), Some(0));
    assert_eq!(dry.status.code(), Some(0));
    assert!(
        stdout(&status).contains("not checked"),
        "{}",
        stdout(&status)
    );
    assert_eq!(tree(&h.home), before);

    // Arrange: the same store with no old state file left.
    fs::remove_file(root.join("migrations.state")).unwrap();
    let before = tree(&h.home);

    // Act
    let status = h.playbook(&["migrate", "--status"], "");
    let dry = h.playbook(&["migrate", "--dry-run"], "");

    // Assert
    assert!(
        !stdout(&status).contains("not checked"),
        "{}",
        stdout(&status)
    );
    assert!(stdout(&dry).starts_with("would run: "), "{}", stdout(&dry));
    assert_eq!(tree(&h.home), before);

    // Arrange: a store that is not a database.
    fs::write(playbook::config::store::db_path(&root), "not a database").unwrap();

    // Act
    let bad = h.playbook(&["migrate", "--status"], "");

    // Assert
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("could not be read"));
}

/// Whether the record in `home` names this binary.
fn recorded_here(home: &Path) -> bool {
    let root = playbook::common::paths::playbook_root_from(home);
    playbook::heal::Record::read_only(&root)
        .is_ok_and(|r| r.binary.as_deref() == Some(env!("CARGO_PKG_VERSION")))
}

#[test]
fn the_first_run_step_heals_once_before_any_command_and_keeps_stdout_clean() {
    // Each class: the command, its stdin, and whether it heals first. Hooks
    // and the status line defer to the session-init hook.
    let classes: [(&str, &[&str], &str, bool); 5] = [
        ("plain", &["mode", "status"], "", true),
        ("json", &["mode", "status", "--json"], "", true),
        ("hook", &["hook", "search-counter"], "{}", false),
        ("statusline", &["statusline"], "{}", false),
        (
            "session-init",
            &["hook", "session-init"],
            r#"{"source":"startup"}"#,
            true,
        ),
    ];
    for (class, args, stdin, heals) in classes {
        // Arrange
        let h = V025Home::new(&format!("start-{class}"));
        let config_hash =
            playbook::common::paths::playbook_root_from(&h.home).join("hooks/lib/config-hash.sh");
        let user_files = h.user_files();

        // Act
        let first = h.playbook(args, stdin);
        let after_first = tree(&h.home);
        let second = h.playbook(args, stdin);

        // Assert: it heals exactly once, says so on stderr only, and leaves
        // stdout and the user's files alone.
        assert_eq!(first.status.code(), second.status.code(), "{class}");
        assert_eq!(config_hash.exists(), !heals, "{class}");
        assert_eq!(recorded_here(&h.home), heals, "{class}");
        assert_eq!(h.user_files(), user_files, "{class}");
        let notice = String::from_utf8_lossy(&first.stderr);
        let again = String::from_utf8_lossy(&second.stderr);
        assert!(!again.contains("of its own files"), "{class}: {again}");
        if class == "session-init" {
            continue;
        }
        assert_eq!(stdout(&first), stdout(&second), "{class}");
        assert!(!stdout(&first).contains("of its own files"), "{class}");
        if heals {
            assert_eq!(first.status.code(), Some(0), "{class}: {notice}");
            assert!(stdout(&first).contains("ask"), "{class}");
            assert_eq!(again, "", "{class}");
            assert_eq!(tree(&h.home), after_first, "{class}");
        }
        assert_eq!(
            notice.contains("updated 1 of its own files after the upgrade"),
            heals,
            "{class}: {notice}"
        );
        assert_eq!(
            notice.contains("Fix: playbook setup --hooks"),
            heals,
            "{class}: {notice}"
        );
    }
}

#[test]
fn the_first_run_step_skips_homeless_commands_and_does_nothing_after_a_downgrade() {
    // Arrange
    let h = V025Home::new("start-edge");
    let before = tree(&h.home);

    // Act: commands that run without a home never heal.
    let version = h.playbook(&["version"], "");
    let completions = h.playbook(&["completions", "bash"], "");

    // Assert
    assert_eq!(version.status.code(), Some(0));
    assert_eq!(completions.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&completions.stderr), "");
    assert_eq!(tree(&h.home), before);

    // Arrange: a home with a playbook folder and no record yet, as an
    // install that skipped `migrate` leaves it.
    let fresh = V025Home::new("start-fresh");
    let _ = fs::remove_dir_all(&fresh.home);
    fs::create_dir_all(playbook::common::paths::playbook_root_from(&fresh.home)).unwrap();

    // Act
    let first = fresh.playbook(&["mode", "status"], "");
    let written = tree(&fresh.home);
    let second = fresh.playbook(&["mode", "status"], "");

    // Assert: the first command records this binary quietly, the next one
    // changes nothing.
    assert_eq!(String::from_utf8_lossy(&first.stderr), "");
    assert_eq!(String::from_utf8_lossy(&second.stderr), "");
    assert!(recorded_here(&fresh.home));
    assert_eq!(tree(&fresh.home), written);

    // Arrange: a newer playbook ran here.
    let root = playbook::common::paths::playbook_root_from(&h.home);
    playbook::state::set(&root, "versions/binary", "999.0.0").unwrap();
    playbook::state::set(&root, "versions/schema", "999").unwrap();
    let downgraded = tree(&h.home);

    // Act
    let out = h.playbook(&["mode", "status"], "");

    // Assert
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    assert_eq!(tree(&h.home), downgraded);
}

/// A v0.25 home whose state has a migration record and no version record,
/// as an older binary leaves it.
fn ran_before(tag: &str) -> V025Home {
    let h = V025Home::new(tag);
    let root = playbook::common::paths::playbook_root_from(&h.home);
    playbook::state::set(&root, "migrations/shipped/system-prompt", "abc").unwrap();
    h
}

/// The line an upgrade from a home without a version record shows.
fn notes_line() -> String {
    playbook::release_notes::updated_line(None, env!("CARGO_PKG_VERSION"))
}

#[test]
fn the_release_notes_line_shows_once_in_the_session_and_on_stderr() {
    // Arrange
    let line = notes_line();
    let session = ran_before("notice-hook");
    let migrate = ran_before("notice-migrate");
    let shown = ran_before("notice-shown");
    let start = ran_before("notice-start");

    // Act
    let first = session.session_message();
    let second = session.session_message();
    let run = migrate.playbook(&["migrate"], "");
    let again = migrate.playbook(&["migrate"], "");
    let printed = shown.playbook(&["update", "--print-notes", "missing.json"], "");
    let after = shown.playbook(&["migrate"], "");
    let first_run = start.playbook(&["mode", "status"], "");
    let later = start.playbook(&["mode", "status"], "");

    // Assert: shown once per channel, never on stdout, and not again after
    // the install script printed the notes.
    assert!(first.contains(&line), "{first}");
    assert!(!second.contains("playbook updated"), "{second}");
    assert!(String::from_utf8_lossy(&run.stderr).contains(&line));
    assert!(!stdout(&run).contains("playbook updated"));
    assert!(!String::from_utf8_lossy(&again.stderr).contains("playbook updated"));
    assert!(
        stdout(&printed).starts_with("Full notes: https://"),
        "{}",
        stdout(&printed)
    );
    assert!(String::from_utf8_lossy(&first_run.stderr).contains(&line));
    assert_eq!(stdout(&first_run), stdout(&later));
    for out in [&after, &later] {
        assert!(!String::from_utf8_lossy(&out.stderr).contains("playbook updated"));
    }
}

#[test]
fn setup_prints_the_release_notes_line_once() {
    // Arrange
    let h = ran_before("notice-init");
    let init = || h.playbook(&["setup", "--yes", "--no-telemetry"], "");

    // Act
    let first = init();
    let second = init();

    // Assert
    assert_eq!(first.status.code(), Some(0), "{}", stdout(&first));
    assert!(String::from_utf8_lossy(&first.stderr).contains(&notes_line()));
    assert!(!String::from_utf8_lossy(&second.stderr).contains("playbook updated"));
}
