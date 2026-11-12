// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The upgrade notice from a user report (ADR 0034, 2026-10-19 amendment):
//! a session start listed one item and named `playbook migrate`, and the
//! same message came back after `playbook migrate` and after a plugin
//! update. Each item is now one bullet with its own command, `migrate`
//! prints the same list and counts it as seen, and a fixed item is gone.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const VERSION: &str = env!("CARGO_PKG_VERSION");

struct Home {
    home: PathBuf,
}

impl Home {
    fn new(tag: &str) -> Home {
        let home = std::env::temp_dir().join(format!(
            "playbook-upgrade-notice-{tag}-{}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        // Settings with nothing open, so only the plugin item is.
        fs::write(
            home.join(".claude/settings.json"),
            r#"{"attribution":{"commit":"","pr":"","sessionUrl":false}}"#,
        )
        .unwrap();
        Home { home }
    }

    /// A cached plugin `version` that needs binary `min`.
    fn plugin(&self, version: &str, min: &str) -> PathBuf {
        let dir = self
            .home
            .join(".claude/plugins/cache/amplifiedengineer/playbook")
            .join(version);
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        fs::create_dir_all(dir.join("skills")).unwrap();
        fs::write(
            dir.join(".claude-plugin/plugin.json"),
            format!(
                r#"{{"name":"playbook","version":"{version}","metadata":{{"minBinaryVersion":"{min}"}}}}"#
            ),
        )
        .unwrap();
        dir
    }

    /// What `claude plugin install` or `update` leaves in installed_plugins.json.
    fn install(&self, dir: &Path, version: &str) {
        fs::write(
            self.home.join(".claude/plugins/installed_plugins.json"),
            format!(
                r#"{{"version":2,"plugins":{{"playbook@amplifiedengineer":[{{"scope":"user","installPath":"{}","version":"{version}"}}]}}}}"#,
                dir.display()
            ),
        )
        .unwrap();
    }

    /// A fake `claude` on this home's PATH. It logs its arguments to
    /// `claude-calls.log`, points installed_plugins.json at `to` when
    /// given, and exits with `exit`.
    fn fake_claude(&self, to: Option<(&Path, &str)>, exit: i32) {
        let bin = self.home.join("bin/claude");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        let swap = to.map_or(String::new(), |(dir, version)| {
            format!(
                "printf '%s' '{{\"version\":2,\"plugins\":{{\"playbook@amplifiedengineer\":[{{\"scope\":\"user\",\"installPath\":\"{}\",\"version\":\"{version}\"}}]}}}}' > \"$HOME/.claude/plugins/installed_plugins.json\"\n",
                dir.display()
            )
        });
        fs::write(
            &bin,
            format!("#!/bin/sh\necho \"$*\" >> \"$HOME/claude-calls.log\"\n{swap}exit {exit}\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn claude_calls(&self) -> String {
        fs::read_to_string(self.home.join("claude-calls.log")).unwrap_or_default()
    }

    fn playbook(&self, args: &[&str], stdin: &str, plugin_root: Option<&Path>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_playbook"));
        command
            .args(args)
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.home.join("bin").display()),
            )
            .env_remove("PLAYBOOK_SKIP_PLUGIN_UPDATE")
            .env_remove("CLAUDE_PLUGIN_ROOT")
            .env_remove("PLAYBOOK_PLUGIN_DIR")
            .env_remove("HOOK_INPUT")
            .env_remove("CI")
            .env_remove("PLAYBOOK_HEADLESS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(root) = plugin_root {
            command.env("CLAUDE_PLUGIN_ROOT", root);
        }
        // These tests check the pending list, not the hook's own plugin update.
        if args.first() == Some(&"hook") {
            command.env("PLAYBOOK_SKIP_PLUGIN_UPDATE", "1");
        }
        let mut child = command.spawn().unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn session(&self, plugin_root: Option<&Path>) -> String {
        let out = self.playbook(
            &["hook", "session-init"],
            r#"{"source":"startup"}"#,
            plugin_root,
        );
        assert_eq!(out.status.code(), Some(0));
        let value: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        value["systemMessage"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn stdout(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.playbook(args, "", None).stdout).into_owned()
    }
}

/// A plugin one minor line behind the binary, or `None` on a `0.x.0`
/// binary with no line before it.
fn older_line() -> Option<String> {
    let mut parts = VERSION.split('.').map(|p| p.parse::<u64>().unwrap());
    let (major, minor) = (parts.next()?, parts.next()?);
    (minor > 0).then(|| format!("{major}.{}.0", minor - 1))
}

#[test]
fn the_reported_notice_lists_the_item_and_migrate_updates_the_plugin_once() {
    let Some(old) = older_line() else {
        return;
    };
    // Arrange: a fresh install whose plugin is one line behind, and a
    // `claude` that installs the current plugin when asked.
    let h = Home::new("report");
    let stale = h.plugin(&old, &old);
    h.install(&stale, &old);
    let current = h.plugin(VERSION, VERSION);
    h.fake_claude(Some((&current, VERSION)), 0);

    // Act: the session from the report, `playbook migrate`, then two more
    // sessions.
    let first = h.session(None);
    let status = h.stdout(&["migrate", "--status"]);
    let calls_before = h.claude_calls();
    let migrate = h.playbook(&["migrate"], "", None);
    let second = h.session(None);
    let third = h.session(None);

    // Assert
    assert!(first.contains("1 item needs you:\n  - "), "{first}");
    assert!(
        first.contains("    Fix: claude plugin update playbook@amplifiedengineer"),
        "{first}"
    );
    assert!(
        first.ends_with("To update the plugin and see what is left, run: playbook migrate"),
        "{first}"
    );
    let bullet = |text: &str| {
        text.lines()
            .skip_while(|l| !l.starts_with("  - "))
            .take(3)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(bullet(&status), bullet(&first), "one renderer: {status}");
    assert_eq!(calls_before, "", "--status changes nothing");
    assert_eq!(migrate.status.code(), Some(0));
    assert_eq!(
        h.claude_calls(),
        "plugin marketplace update amplifiedengineer\nplugin update playbook@amplifiedengineer\n"
    );
    let text = String::from_utf8_lossy(&migrate.stdout);
    assert!(
        text.contains(&format!(
            "plugin: updated playbook@amplifiedengineer from {old} to {VERSION}. Restart Claude Code to load it\n"
        )),
        "{text}"
    );
    assert!(text.ends_with("nothing else needs you\n"), "{text}");
    assert_eq!(second, "", "the plugin is current");
    assert_eq!(third, "");
}

#[test]
fn migrate_lists_a_report_it_could_not_fix_once_and_the_session_stays_quiet() {
    let Some(old) = older_line() else {
        return;
    };
    // Arrange: no `claude` on PATH, so the plugin stays old.
    let h = Home::new("report-no-cli");
    let stale = h.plugin(&old, &old);
    h.install(&stale, &old);

    // Act
    let first = h.session(None);
    let migrate = h.playbook(&["migrate"], "", None);
    let second = h.session(None);

    // Assert
    assert_eq!(
        migrate.status.code(),
        Some(0),
        "a missing CLI does not fail"
    );
    let text = String::from_utf8_lossy(&migrate.stdout);
    assert!(
        text.starts_with(
            "plugin: the claude command is not on PATH, so the plugin was not updated; \
             run: claude plugin update playbook@amplifiedengineer\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("Still left for you, 1 item needs you:"),
        "{text}"
    );
    assert!(first.contains("1 item needs you:"), "{first}");
    assert_eq!(second, "", "shown less than a day ago");
}

#[test]
fn no_plugin_a_false_config_key_or_a_failed_update_leave_the_exit_code_alone() {
    let Some(old) = older_line() else {
        return;
    };
    // Arrange
    let h = Home::new("plugin-opt-out");
    let stale = h.plugin(&old, &old);
    h.install(&stale, &old);
    h.fake_claude(None, 1);

    // Act
    let skipped = h.playbook(&["migrate", "--no-plugin"], "", None);
    let dry_run = h.playbook(&["migrate", "--dry-run"], "", None);
    let calls_after_opt_out = h.claude_calls();
    let failed = h.playbook(&["migrate"], "", None);
    let set = h.playbook(&["config", "set", "upgrade.plugin", "off"], "", None);
    let off = h.playbook(&["migrate"], "", None);
    let bad = h.playbook(&["config", "set", "upgrade.plugin", "sometimes"], "", None);

    // Assert
    assert_eq!(
        calls_after_opt_out, "",
        "--no-plugin and --dry-run run nothing"
    );
    assert!(!String::from_utf8_lossy(&skipped.stdout).contains("plugin:"));
    assert_eq!(failed.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&failed.stdout).contains(
            "plugin: the update failed (exit 1); run: claude plugin update playbook@amplifiedengineer"
        ),
        "{}",
        String::from_utf8_lossy(&failed.stdout)
    );
    assert_eq!(
        set.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    assert!(
        String::from_utf8_lossy(&off.stdout).contains("plugin: not updated, upgrade.plugin is off"),
        "{}",
        String::from_utf8_lossy(&off.stdout)
    );
    assert_eq!(
        h.claude_calls(),
        "plugin marketplace update amplifiedengineer\nplugin update playbook@amplifiedengineer\n"
    );
    assert_ne!(
        bad.status.code(),
        Some(0),
        "upgrade.plugin takes auto, ask or off"
    );
    assert_eq!(dry_run.status.code(), Some(0));
}

#[test]
fn a_stored_old_plugin_row_of_off_still_stops_the_plugin_step() {
    let Some(old) = older_line() else {
        return;
    };
    // Arrange: the row an older binary stored, under the old name
    let h = Home::new("plugin-old-row-off");
    let stale = h.plugin(&old, &old);
    h.install(&stale, &old);
    h.fake_claude(None, 1);
    playbook::config::store::put(
        &playbook::common::paths::playbook_root_from(&h.home),
        playbook::config::write::Tier::Global,
        None,
        "update.plugin",
        &serde_json::json!("off"),
    )
    .unwrap();

    // Act
    let off = h.playbook(&["migrate"], "", None);

    // Assert
    let text = String::from_utf8_lossy(&off.stdout);
    assert!(
        text.contains("plugin: not updated, upgrade.plugin is off"),
        "{text}"
    );
    assert_eq!(h.claude_calls(), "");
}

#[test]
fn a_failed_marketplace_refresh_leaves_the_migrate_exit_code_alone() {
    let Some(old) = older_line() else {
        return;
    };
    // Arrange: the marketplace refresh fails, the plugin update succeeds and
    // leaves the old plugin in place.
    let h = Home::new("market-fail");
    let stale = h.plugin(&old, &old);
    h.install(&stale, &old);
    let bin = h.home.join("bin/claude");
    fs::create_dir_all(bin.parent().unwrap()).unwrap();
    fs::write(
        &bin,
        "#!/bin/sh\necho \"$*\" >> \"$HOME/claude-calls.log\"\ncase \"$*\" in\n*marketplace*) echo 'git pull failed' >&2; exit 1;;\nesac\nexit 0\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();

    // Act
    let out = h.playbook(&["migrate"], "", None);

    // Assert
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        h.claude_calls(),
        "plugin marketplace update amplifiedengineer\nplugin update playbook@amplifiedengineer\n"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("the marketplace refresh failed (git pull failed)"),
        "{text}"
    );
}

#[test]
fn a_session_from_a_stale_plugin_folder_says_to_restart() {
    let Some(old) = older_line() else {
        return;
    };
    // Arrange: Claude Code installed the current plugin, and this session
    // still runs from the old cache folder.
    let h = Home::new("stale");
    let stale = h.plugin(&old, &old);
    let current = h.plugin(VERSION, VERSION);
    h.install(&current, VERSION);

    // Act
    let from_stale = h.session(Some(&stale));
    let from_current = Home::new("stale-current");
    let dir = from_current.plugin(VERSION, VERSION);
    from_current.install(&dir, VERSION);
    let fresh = from_current.session(Some(&dir));

    // Assert
    assert!(
        from_stale.contains(&format!("plugin {old} from an old folder")),
        "{from_stale}"
    );
    assert!(
        from_stale.contains("Fix: exit this session and start Claude Code again"),
        "{from_stale}"
    );
    assert!(
        !from_stale.contains("claude plugin update"),
        "the installed plugin is current: {from_stale}"
    );
    assert_eq!(fresh, "", "a fresh install has nothing to say");
}

#[test]
fn a_consent_item_shows_once_then_stays_quiet_for_the_day() {
    // Arrange: the status line still runs the old script, a setup change
    // the user has not made yet.
    let h = Home::new("consent");
    let script = playbook::init::statusline::playbook_statusline_path(&h.home);
    fs::write(
        h.home.join(".claude/settings.json"),
        format!(
            r#"{{"statusLine":{{"type":"command","command":"bash {}"}}}}"#,
            script.display()
        ),
    )
    .unwrap();

    // Act
    let first = h.session(None);
    let second = h.session(None);
    let status = h.stdout(&["migrate", "--status"]);

    // Assert
    assert!(first.contains("1 item needs you:\n  - "), "{first}");
    assert!(
        first.contains("Fix: playbook setup --statusline"),
        "{first}"
    );
    assert_eq!(second, "", "shown less than a day ago");
    assert!(
        status.contains("Fix: playbook setup --statusline"),
        "{status}"
    );
}
