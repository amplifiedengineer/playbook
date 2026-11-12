// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The first command after an upgrade updates the playbook plugin
//! (ADR 0034, 2026-10-19 amendment). A fake `claude` on PATH logs its calls.
//! `PLAYBOOK_TEST_ASSUME_TTY` stands in for a terminal on stdin and stderr;
//! the binary reads it only in debug builds, so the tests that need it fail
//! loudly in a release build instead of passing without checking anything.

#[path = "support/isolated.rs"]
mod isolated;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const PLUGIN_ID: &str = "playbook@amplifiedengineer";
const EXPECTED_CALL: &str =
    "plugin marketplace update amplifiedengineer\nplugin update playbook@amplifiedengineer\n";
const MARKER_KEY: &str = "versions/plugin-update";
const START_LINE: &str =
    "playbook: updating the playbook plugin (claude plugin update playbook@amplifiedengineer)";
const MANUAL_COMMAND: &str = "claude plugin update playbook@amplifiedengineer";
const CI_VARS: [&str; 7] = [
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "JENKINS_URL",
    "TF_BUILD",
];
const HOUR_SECS: u64 = 60 * 60;
const SKIP_ENV: &str = "PLAYBOOK_SKIP_PLUGIN_UPDATE";
const ASSUME_TTY_ENV: &str = "PLAYBOOK_TEST_ASSUME_TTY";
const MINUTE_SECS: u64 = 60;
const NOTICE_KEY: &str = "notices/plugin-update";
const SESSION_PAYLOAD: &str = r#"{"source":"startup"}"#;
const CHILD_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
const POLL_EVERY: std::time::Duration = std::time::Duration::from_millis(50);
const BACKGROUND_LINE: &str = concat!(
    "playbook: updating the playbook plugin to ",
    env!("CARGO_PKG_VERSION"),
    " in the background. Start Claude Code again to load it."
);
const UPDATED_LINE: &str = "plugin: updated playbook@amplifiedengineer from";
const HEADLESS_ENV: &str = "PLAYBOOK_HEADLESS";
const NO_SPAWN_ENV: &str = "PLAYBOOK_TEST_NO_SPAWN";
const STILL_OLDER_NOTICE: &str = "plugin: playbook@amplifiedengineer ";
const LAGGING_MARKETPLACE_TEXT: &str = "the marketplace has not published a newer plugin yet";

struct Fixture {
    home: PathBuf,
}

impl Fixture {
    /// A home with an older plugin installed next to an unrelated plugin, and
    /// a `claude` that installs `VERSION` only when `moves_to_current` and
    /// exits `exit`.
    fn new(moves_to_current: bool, exit: i32) -> Fixture {
        let home = isolated::fresh_home();
        assert_state_db_under(&home);
        let f = Fixture { home };
        let old = f.plugin_dir(&old_version());
        let current = f.plugin_dir(VERSION);
        f.write_installed(&old);
        f.write_fake_claude(moves_to_current.then_some(&current), exit);
        f
    }

    fn plugin_dir(&self, version: &str) -> PathBuf {
        let dir = self
            .home
            .join(".claude/plugins/cache/amplifiedengineer/playbook")
            .join(version);
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        fs::create_dir_all(dir.join("skills")).unwrap();
        fs::write(
            dir.join(".claude-plugin/plugin.json"),
            format!(
                r#"{{"name":"playbook","version":"{version}","metadata":{{"minBinaryVersion":"{version}"}}}}"#
            ),
        )
        .unwrap();
        dir
    }

    fn installed_json(dir: &Path) -> String {
        format!(
            r#"{{"version":2,"plugins":{{"{PLUGIN_ID}":[{{"scope":"user","installPath":"{}"}}],"other@elsewhere":[{{"scope":"user","installPath":"/nowhere"}}]}}}}"#,
            dir.display()
        )
    }

    fn write_installed(&self, dir: &Path) {
        fs::write(
            self.home.join(".claude/plugins/installed_plugins.json"),
            Self::installed_json(dir),
        )
        .unwrap();
    }

    fn write_fake_claude(&self, moves_to: Option<&PathBuf>, exit: i32) {
        let bin = self.home.join("bin/claude");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        let swap = moves_to.map_or(String::new(), |dir| {
            format!(
                "printf '%s' '{}' > \"$HOME/.claude/plugins/installed_plugins.json\"\n",
                Self::installed_json(dir)
            )
        });
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho \"$*\" >> \"$HOME/claude-calls.log\"\necho \"${{{SKIP_ENV}-unset}}\" >> \"$HOME/claude-env.log\"\n{swap}echo 'network is down' >&2\nexit {exit}\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.home.join("claude-calls.log")).unwrap_or_default()
    }

    fn claude_env(&self) -> String {
        fs::read_to_string(self.home.join("claude-env.log")).unwrap_or_default()
    }

    fn root(&self) -> PathBuf {
        self.home.join(".config/playbook")
    }

    fn marker(&self) -> Option<String> {
        playbook::plugin_update::seen(&self.root())
    }

    fn set_marker(&self, text: &str) {
        playbook::state::set(&self.root(), MARKER_KEY, text).unwrap();
    }

    /// `playbook <args>` with `stdin` as its input. `tty` turns on the
    /// terminal seam for both streams; `env` adds variables after the CI ones
    /// are removed.
    fn run(&self, args: &[&str], tty: bool, stdin: &str, env: &[(&str, &str)]) -> Output {
        let mut child = self.command(args, tty, env).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn command(&self, args: &[&str], tty: bool, env: &[(&str, &str)]) -> std::process::Command {
        let mut cmd = isolated::playbook_in(&self.home);
        cmd.args(args)
            .current_dir(&self.home)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.home.join("bin").display()),
            )
            .env_remove("PLAYBOOK_SKIP_PLUGIN_UPDATE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for var in CI_VARS {
            cmd.env_remove(var);
        }
        if (tty
            || env
                .iter()
                .any(|(k, _)| *k == ASSUME_TTY_ENV || *k == NO_SPAWN_ENV))
            && !cfg!(debug_assertions)
        {
            panic!(
                "the terminal seam exists only in debug builds; run these tests without --release"
            );
        }
        if tty {
            cmd.env(ASSUME_TTY_ENV, "1");
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd
    }

    /// A session start as the harness sends it.
    fn session(&self, env: &[(&str, &str)]) -> Output {
        self.run(&["hook", "session-init"], false, SESSION_PAYLOAD, env)
    }

    /// Waits for the detached child to end: it stores its notice last.
    fn wait_for_child(&self) {
        self.wait_for_child_after(&[]);
    }

    /// As [`Self::wait_for_child`] when other hooks ran beside the child. A
    /// hook that starts after the child stored its notice shows it and
    /// deletes it, so the notice in a hook's output counts as the child's end.
    fn wait_for_child_after(&self, hooks: &[&Output]) {
        let deadline = std::time::Instant::now() + CHILD_WAIT;
        let shown = || hooks.iter().any(|o| stdout(o).contains(UPDATED_LINE));
        while !shown()
            && playbook::state::get(&self.root(), NOTICE_KEY)
                .unwrap()
                .is_none()
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the update child did not finish; claude log: {:?}; state: {:?}\n{}",
                self.calls(),
                playbook::state::list(&self.root(), "").unwrap_or_default(),
                hooks
                    .iter()
                    .map(|o| format!("hook stdout: {}\nhook stderr: {}", stdout(o), stderr(o)))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            std::thread::sleep(POLL_EVERY);
        }
    }

    fn config_list(&self, tty: bool, env: &[(&str, &str)]) -> Output {
        self.run(&["config", "list"], tty, "", env)
    }

    fn set_config(&self, key: &str, value: &str) {
        let out = self.run(&["config", "set", key, value], false, "", &[]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    }
}

/// Used when the binary is on a `X.0.Y` line and has no earlier minor line.
const FALLBACK_OLD_VERSION: &str = "0.0.1";

/// The plugin one minor line behind the binary, the shape the pending list
/// reports as out of range.
fn old_version() -> String {
    let mut parts = VERSION.split('.').map(|p| p.parse::<u64>().unwrap());
    let (major, minor) = (parts.next().unwrap(), parts.next().unwrap());
    match minor.checked_sub(1) {
        Some(previous) => format!("{major}.{previous}.0"),
        None => FALLBACK_OLD_VERSION.to_string(),
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// The state table lives in `config.db` under the home the test owns, never
/// the real one.
fn assert_state_db_under(home: &Path) {
    let root = home.join(".config/playbook");
    let db = playbook::config::store::db_path(&root);
    assert!(
        db.starts_with(home),
        "{} is outside {}",
        db.display(),
        home.display()
    );
    assert!(
        home.starts_with(std::env::temp_dir()),
        "home is not scratch"
    );
}

#[test]
fn the_first_command_after_an_upgrade_updates_the_plugin_once() {
    // Arrange
    let f = Fixture::new(true, 0);

    // Act
    let first = f.config_list(true, &[]);
    let after_first = f.calls();
    let second = f.config_list(true, &[]);

    // Assert
    assert_eq!(first.status.code(), Some(0));
    assert_eq!(after_first, EXPECTED_CALL);
    let err = stderr(&first);
    assert!(err.contains(START_LINE), "{err}");
    assert!(
        err.contains(&format!(
            "plugin: updated {PLUGIN_ID} from {} to {VERSION}. Restart Claude Code to load it",
            old_version()
        )),
        "{err}"
    );
    assert!(f.marker().unwrap().starts_with(&format!("{VERSION} done ")));
    assert_eq!(second.status.code(), Some(0));
    assert_eq!(f.calls(), EXPECTED_CALL, "the second command does not call");
}

#[test]
fn a_piped_run_without_a_terminal_does_not_call_claude() {
    let f = Fixture::new(true, 0);

    let out = f.config_list(false, &[]);

    assert_eq!(out.status.code(), Some(0));
    assert_eq!(f.calls(), "");
    assert_eq!(f.marker(), None);
}

#[test]
fn a_ci_run_does_not_call_claude() {
    let f = Fixture::new(true, 0);

    // A vendor variable alone, so the headless rule does not hide the CI rule.
    let out = f.config_list(true, &[("GITHUB_ACTIONS", "true")]);

    assert_eq!(out.status.code(), Some(0));
    assert_eq!(f.calls(), "");
    assert_eq!(f.marker(), None);
}

#[test]
fn a_plugin_turned_off_in_claude_code_is_not_updated() {
    let f = Fixture::new(true, 0);
    fs::write(
        f.home.join(".claude/settings.json"),
        format!(r#"{{"enabledPlugins":{{"{PLUGIN_ID}":false}}}}"#),
    )
    .unwrap();

    let out = f.config_list(true, &[]);

    assert_eq!(out.status.code(), Some(0));
    assert_eq!(f.calls(), "");
}

#[test]
fn update_plugin_off_does_not_call_claude() {
    let f = Fixture::new(true, 0);
    f.set_config("upgrade.plugin", "off");

    let out = f.config_list(true, &[]);

    assert_eq!(out.status.code(), Some(0));
    assert_eq!(f.calls(), "");
}

#[test]
fn the_notice_leaves_standard_output_as_it_was() {
    // Arrange
    let updating = Fixture::new(true, 0);
    let quiet = Fixture::new(true, 0);

    // Act
    let with_update = updating.run(&["config", "get", "upgrade.plugin"], true, "", &[]);
    let without = quiet.run(&["config", "get", "upgrade.plugin"], false, "", &[]);

    // Assert
    assert_eq!(updating.calls(), EXPECTED_CALL);
    assert_eq!(
        stdout(&with_update),
        "upgrade.plugin: auto (source: default)\n"
    );
    assert_eq!(stdout(&with_update), stdout(&without));
}

#[test]
fn a_failing_claude_leaves_the_exit_code_and_a_fix_line() {
    // Arrange
    let f = Fixture::new(false, 1);

    // Act
    let out = f.config_list(true, &[]);
    let session = f.run(
        &["hook", "session-init"],
        false,
        r#"{"source":"startup"}"#,
        &[],
    );

    // Assert
    assert_eq!(out.status.code(), Some(0));
    assert!(stderr(&out).contains("plugin: the update failed (network is down)"));
    assert!(f
        .marker()
        .unwrap()
        .starts_with(&format!("{VERSION} failed ")));
    assert!(
        stdout(&session).contains(&format!("Fix: {MANUAL_COMMAND}")),
        "{}",
        stdout(&session)
    );
}

#[test]
fn only_the_playbook_id_is_passed_to_claude() {
    let f = Fixture::new(true, 0);

    f.config_list(true, &[]);

    assert_eq!(f.calls(), EXPECTED_CALL);
}

#[test]
fn an_update_that_changes_nothing_ends_behind_and_is_due_again_later() {
    // Arrange: claude exits 0 and leaves the old plugin in place.
    let f = Fixture::new(false, 0);

    // Act and Assert: scenario 10, a day later it tries again.
    f.config_list(true, &[]);
    assert!(f
        .marker()
        .unwrap()
        .starts_with(&format!("{VERSION} behind ")));
    f.config_list(true, &[]);
    assert_eq!(f.calls(), EXPECTED_CALL, "behind recently, so not due");
    let day_and_an_hour_ago = now() - 25 * HOUR_SECS;
    f.set_marker(&format!("{VERSION} behind {day_and_an_hour_ago}"));
    f.config_list(true, &[]);
    assert_eq!(f.calls(), EXPECTED_CALL.repeat(2));

    // Act and Assert: scenario 11, a `running` marker 11 minutes old is stale.
    let stale = now() - 11 * MINUTE_SECS;
    f.set_marker(&format!("{VERSION} running {stale}"));
    f.config_list(true, &[]);
    assert_eq!(f.calls(), EXPECTED_CALL.repeat(3));
}

#[test]
fn a_behind_marker_waits_a_day_to_the_minute() {
    // Arrange: claude exits 0 and leaves the old plugin in place.
    let f = Fixture::new(false, 0);
    let just_inside = now() - (24 * HOUR_SECS - MINUTE_SECS);
    let just_past = now() - (24 * HOUR_SECS + MINUTE_SECS);

    // Act: 23 h 59 m after the last try
    f.set_marker(&format!("{VERSION} behind {just_inside}"));
    f.config_list(true, &[]);

    // Assert
    assert_eq!(f.calls(), "", "23 h 59 m is not due");

    // Act: 24 h 1 m after the last try
    f.set_marker(&format!("{VERSION} behind {just_past}"));
    f.config_list(true, &[]);

    // Assert
    assert_eq!(f.calls(), EXPECTED_CALL, "24 h 1 m is due");
    assert!(f
        .marker()
        .unwrap()
        .starts_with(&format!("{VERSION} behind ")));
}

#[test]
fn ask_yes_runs_the_update() {
    // Arrange
    let f = Fixture::new(true, 0);
    f.set_config("upgrade.plugin", "ask");

    // Act
    let out = f.run(&["config", "list"], true, "y\n", &[]);

    // Assert
    assert_eq!(f.calls(), EXPECTED_CALL);
    let err = stderr(&out);
    assert!(
        err.contains("Update the playbook plugin now? [Y/n]"),
        "{err}"
    );
    assert!(err.contains("plugin: updated"), "{err}");
    assert!(!stdout(&out).contains("Update the playbook plugin"));
}

#[test]
fn ask_no_claims_the_version_and_prints_the_manual_command() {
    // Arrange
    let f = Fixture::new(true, 0);
    f.set_config("upgrade.plugin", "ask");

    // Act
    let declined = f.run(&["config", "list"], true, "n\n", &[]);
    let again = f.run(&["config", "list"], true, "y\n", &[]);

    // Assert
    assert_eq!(f.calls(), "");
    assert!(stderr(&declined).contains(&format!("run: {MANUAL_COMMAND}")));
    assert!(f.marker().unwrap().starts_with(&format!("{VERSION} done ")));
    assert!(!stderr(&again).contains("Update the playbook plugin now?"));
}

#[test]
fn ask_with_an_unclear_reply_claims_nothing_so_the_question_comes_back() {
    let f = Fixture::new(true, 0);
    f.set_config("upgrade.plugin", "ask");

    let unclear = f.run(&["config", "list"], true, "maybe\n", &[]);
    let eof = f.run(&["config", "list"], true, "", &[]);

    assert_eq!(f.calls(), "");
    assert_eq!(f.marker(), None);
    assert!(stderr(&unclear).contains("Update the playbook plugin now?"));
    assert!(stderr(&eof).contains("Update the playbook plugin now?"));
}

#[test]
fn ask_yes_after_another_process_took_the_claim_does_not_update() {
    let f = Fixture::new(true, 0);
    f.set_config("upgrade.plugin", "ask");
    f.set_marker(&format!("{VERSION} running {}", now()));

    let out = f.run(&["config", "list"], true, "y\n", &[]);

    assert_eq!(f.calls(), "");
    assert!(!stderr(&out).contains("Update the playbook plugin now?"));
}

#[test]
fn ask_without_a_terminal_on_stdin_asks_nothing_and_claims_nothing() {
    let f = Fixture::new(true, 0);
    f.set_config("upgrade.plugin", "ask");

    // Only stderr is a terminal, so the Ask arm is reached with stdin piped.
    let out = f.run(
        &["config", "list"],
        false,
        "y\n",
        &[(ASSUME_TTY_ENV, "stderr")],
    );

    assert_eq!(f.calls(), "");
    assert!(!stderr(&out).contains("Update the playbook plugin now?"));
    assert_eq!(f.marker(), None);
}

#[test]
fn the_claude_child_gets_the_re_entry_guard() {
    let f = Fixture::new(true, 0);

    f.config_list(true, &[]);

    assert_eq!(f.claude_env(), "1\n1\n");
}

#[test]
fn a_json_command_never_calls_claude() {
    let f = Fixture::new(true, 0);

    let out = f.run(&["sessions", "--json"], true, "", &[]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(f.calls(), "");
    assert_eq!(f.marker(), None);
    assert!(!stderr(&out).contains(START_LINE));
}

#[test]
fn the_detached_refresh_stores_the_updated_line_and_prints_nothing() {
    // Arrange
    let f = Fixture::new(true, 0);

    f.set_marker(&format!("{VERSION} running {}", now()));

    // Act
    let out = f.run(&["upgrade", "--refresh-plugin"], false, "", &[]);

    // Assert
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(stderr(&out), "");
    assert_eq!(f.calls(), EXPECTED_CALL);
    assert_eq!(
        f.claude_env(),
        "1\n1\n",
        "the claude child gets the skip variable"
    );
    let notice = playbook::state::get(&f.root(), NOTICE_KEY)
        .unwrap()
        .unwrap();
    assert!(
        notice.starts_with(&format!("plugin: updated {PLUGIN_ID} from ")),
        "{notice}"
    );
    assert!(f.marker().unwrap().starts_with(&format!("{VERSION} done ")));
}

#[test]
fn a_refresh_started_by_hand_without_a_claim_runs_nothing() {
    let f = Fixture::new(true, 0);

    let out = f.run(&["upgrade", "--refresh-plugin"], false, "", &[]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(f.calls(), "");
    assert_eq!(f.marker(), None);
}

#[test]
fn a_failed_detached_refresh_stores_the_failure_line_and_marks_failed() {
    // Arrange
    let f = Fixture::new(false, 1);

    f.set_marker(&format!("{VERSION} running {}", now()));

    // Act
    let out = f.run(&["upgrade", "--refresh-plugin"], false, "", &[]);

    // Assert
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(stderr(&out), "");
    let notice = playbook::state::get(&f.root(), NOTICE_KEY)
        .unwrap()
        .unwrap();
    assert!(
        notice.starts_with("plugin: the update failed (network is down)"),
        "{notice}"
    );
    assert!(f
        .marker()
        .unwrap()
        .starts_with(&format!("{VERSION} failed ")));

    // Act: a session right after the failed refresh
    let session = f.session(&[]);

    // Assert: it neither announces an update nor retries before the wait
    assert!(
        !stdout(&session).contains(BACKGROUND_LINE),
        "{}",
        stdout(&session)
    );
    assert!(f
        .marker()
        .unwrap()
        .starts_with(&format!("{VERSION} failed ")));
}

#[test]
fn a_refresh_that_leaves_the_plugin_older_stores_the_still_older_notice() {
    // Arrange: claude exits 0 and leaves the old plugin in place
    let f = Fixture::new(false, 0);

    f.set_marker(&format!("{VERSION} running {}", now()));

    // Act
    f.run(&["upgrade", "--refresh-plugin"], false, "", &[]);

    // Assert
    let notice = playbook::state::get(&f.root(), NOTICE_KEY)
        .unwrap()
        .unwrap();
    assert!(notice.starts_with(STILL_OLDER_NOTICE), "{notice}");
    assert!(notice.contains(LAGGING_MARKETPLACE_TEXT), "{notice}");
    assert!(!notice.contains(MANUAL_COMMAND), "{notice}");
    assert!(!notice.contains("up to date"), "{notice}");
}

#[test]
fn a_hook_with_the_plugin_already_current_records_done_and_adds_no_line() {
    // Arrange
    let f = Fixture::new(false, 0);
    f.write_installed(&f.plugin_dir(VERSION));

    // Act
    let out = f.session(&[]);

    // Assert
    assert!(!stdout(&out).contains(BACKGROUND_LINE), "{}", stdout(&out));
    assert_eq!(f.calls(), "");
    assert!(f.marker().unwrap().starts_with(&format!("{VERSION} done ")));
}

#[test]
fn the_no_spawn_seam_keeps_the_hook_from_starting_a_child_but_it_still_claims() {
    // Arrange
    let f = Fixture::new(true, 0);

    // Act
    let out = f.session(&[(NO_SPAWN_ENV, "1")]);
    std::thread::sleep(POLL_EVERY * 10);

    // Assert
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(f.calls(), "", "no child ran");
    assert!(f
        .marker()
        .unwrap()
        .starts_with(&format!("{VERSION} running ")));
}

#[test]
fn a_stale_plugin_session_gives_one_coherent_message_set() {
    // Arrange: stale plugin, no skip variable, so the hook claims and spawns
    let f = Fixture::new(true, 0);

    // Act
    let out = f.session(&[]);
    f.wait_for_child();

    // Assert: the manual Fix line and the background line must not both show
    let text = stdout(&out);
    let says_updating = text.contains(BACKGROUND_LINE);
    let says_run_by_hand = text.contains(MANUAL_COMMAND);
    assert!(says_updating, "{text}");
    assert!(!says_run_by_hand, "contradicts the background line: {text}");
}

#[test]
fn the_hook_starts_the_update_and_the_next_session_shows_it_once() {
    // Arrange
    let f = Fixture::new(true, 0);

    // Act
    let first = f.session(&[]);
    f.wait_for_child();
    let after_child = f.calls();
    let second = f.session(&[]);
    let notice_after_second = playbook::state::get(&f.root(), NOTICE_KEY).unwrap();
    let third = f.session(&[]);

    // Assert
    assert!(
        stdout(&first).contains(BACKGROUND_LINE),
        "{}",
        stdout(&first)
    );
    assert!(!stdout(&first).contains(UPDATED_LINE));
    assert_eq!(after_child, EXPECTED_CALL);
    assert!(
        stdout(&second).contains(UPDATED_LINE),
        "{}",
        stdout(&second)
    );
    assert!(!stdout(&second).contains(BACKGROUND_LINE));
    assert_eq!(notice_after_second, None, "showing the line deletes it");
    assert!(!stdout(&third).contains(UPDATED_LINE));
    assert!(!stdout(&third).contains(BACKGROUND_LINE));
    assert_eq!(f.calls(), EXPECTED_CALL, "later sessions do not call");
}

#[test]
fn ask_mode_adds_no_background_line_and_starts_nothing() {
    // Arrange
    let f = Fixture::new(true, 0);
    f.set_config("upgrade.plugin", "ask");

    // Act
    let out = f.session(&[]);

    // Assert
    assert!(!stdout(&out).contains(BACKGROUND_LINE), "{}", stdout(&out));
    assert_eq!(f.calls(), "");
    assert_eq!(f.marker(), None, "ask does not claim the version");
}

#[test]
fn a_headless_session_adds_nothing_and_starts_nothing() {
    // Arrange
    let f = Fixture::new(true, 0);

    // Act
    let out = f.session(&[(HEADLESS_ENV, "1")]);

    // Assert
    assert!(!stdout(&out).contains(BACKGROUND_LINE), "{}", stdout(&out));
    assert_eq!(f.calls(), "");
    assert_eq!(f.marker(), None);
}

#[test]
fn two_hooks_started_together_start_one_update() {
    // Arrange
    let f = Fixture::new(true, 0);
    let spawn = || {
        let mut child = f
            .command(&["hook", "session-init"], false, &[])
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(SESSION_PAYLOAD.as_bytes())
            .unwrap();
        child
    };

    // Act
    let (a, b) = (spawn(), spawn());
    let (a, b) = (a.wait_with_output().unwrap(), b.wait_with_output().unwrap());
    // The hook that lost the claim can run after the child stored its notice
    // and take it, so the notice is not always left in the store.
    f.wait_for_child_after(&[&a, &b]);

    // Assert
    let shown = [a, b]
        .iter()
        .filter(|o| stdout(o).contains(BACKGROUND_LINE))
        .count();
    assert_eq!(shown, 1, "only the hook that claimed the version says so");
    assert_eq!(f.calls(), EXPECTED_CALL);
}

#[test]
fn the_upgrade_path_never_writes_auto_update() {
    // Arrange
    let f = Fixture::new(true, 0);
    let settings = f.home.join(".claude/settings.json");
    let text = r#"{"extraKnownMarketplaces":{"amplifiedengineer":{"source":{"source":"github","repo":"amplifiedengineer/marketplace"}}}}"#;
    fs::write(&settings, text).unwrap();

    // Act
    let command = f.config_list(true, &[]);
    let after_command = fs::read_to_string(&settings).unwrap();
    f.set_marker(&format!("{VERSION} running {}", now()));
    let refresh = f.run(&["upgrade", "--refresh-plugin"], false, "", &[]);

    // Assert
    assert_eq!(command.status.code(), Some(0), "{}", stderr(&command));
    assert_eq!(refresh.status.code(), Some(0), "{}", stderr(&refresh));
    assert!(f.calls().contains("plugin update"), "the update ran");
    assert_eq!(after_command, text);
    assert_eq!(fs::read_to_string(&settings).unwrap(), text);
}
