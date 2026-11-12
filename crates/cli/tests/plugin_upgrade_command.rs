// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook plugin upgrade` runs against a fake `claude` on PATH that logs its
//! calls and, like the real one, moves the installed version. The isolated
//! home sets `PLAYBOOK_SKIP_PLUGIN_UPDATE`, so every test here also proves the
//! command ignores that opt-out.

#[path = "support/isolated.rs"]
mod isolated;

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const OLD_VERSION: &str = "0.1.0";
const NEW_ID: &str = "playbook@amplifiedengineer";
const OLD_ID: &str = "playbook@pragmatic-engineer";
const RELOAD: &str = "Restart Claude Code or run /reload-plugins";
const INSTALL_COMMAND: &str = "claude plugin marketplace add amplifiedengineer/marketplace && claude plugin install playbook@amplifiedengineer";

struct Fixture {
    home: PathBuf,
}

impl Fixture {
    /// A home whose fake `claude` ends each update as `update`: `bump` installs
    /// the binary's version, `same` changes nothing, `fail` exits 1.
    fn new(update: &str) -> Fixture {
        let home = isolated::fresh_home();
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let script = format!(
            r#"#!/bin/sh
echo "$*" >> "$HOME/claude-calls.log"
case "$*" in
"plugin marketplace update"*) ;;
"plugin update "*)
  id="$3"
  case "{update}" in
  fail) echo 'network down' >&2; exit 1 ;;
  bump)
    name="${{id#playbook@}}"
    dir="$HOME/.claude/plugins/cache/$name/playbook/{VERSION}"
    mkdir -p "$dir/.claude-plugin" "$dir/skills"
    printf '{{"name":"playbook","version":"{VERSION}"}}' > "$dir/.claude-plugin/plugin.json"
    sed "s#\"$id\":\[{{[^]]*}}\]#\"$id\":[{{\"scope\":\"user\",\"installPath\":\"$dir\"}}]#" "$HOME/.claude/plugins/installed_plugins.json" > "$HOME/installed.tmp"
    mv "$HOME/installed.tmp" "$HOME/.claude/plugins/installed_plugins.json"
    ;;
  esac
  ;;
esac
"#
        );
        let staged = bin.join("claude.tmp");
        fs::write(&staged, script).unwrap();
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&staged, bin.join("claude")).unwrap();
        Fixture { home }
    }

    /// Lists `ids` as installed at `OLD_VERSION`.
    fn install(&self, ids: &[&str]) {
        let mut plugins = serde_json::Map::new();
        for id in ids {
            let name = id.trim_start_matches("playbook@");
            let dir = self.home.join(format!(
                ".claude/plugins/cache/{name}/playbook/{OLD_VERSION}"
            ));
            fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
            fs::create_dir_all(dir.join("skills")).unwrap();
            fs::write(
                dir.join(".claude-plugin/plugin.json"),
                format!(r#"{{"name":"playbook","version":"{OLD_VERSION}"}}"#),
            )
            .unwrap();
            plugins.insert(
                (*id).to_string(),
                serde_json::json!([{ "scope": "user", "installPath": dir }]),
            );
        }
        let file = self.home.join(".claude/plugins/installed_plugins.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            file,
            serde_json::json!({ "version": 2, "plugins": plugins }).to_string(),
        )
        .unwrap();
    }

    fn run(&self, args: &[&str], with_claude: bool) -> Output {
        let mut cmd: Command = isolated::playbook_in(&self.home);
        let path = if with_claude {
            format!("{}:/usr/bin:/bin", self.home.join("bin").display())
        } else {
            "/usr/bin:/bin".to_string()
        };
        cmd.args(["plugin", "upgrade"])
            .args(args)
            .current_dir(&self.home)
            .env("PATH", path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.output().unwrap()
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.home.join("claude-calls.log")).unwrap_or_default()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn an_update_refreshes_the_marketplace_and_reports_the_version_change() {
    let f = Fixture::new("bump");
    f.install(&[NEW_ID]);
    let o = f.run(&[], true);
    let text = out(&o);
    assert!(o.status.success(), "{text}");
    assert!(
        text.contains(&format!(
            "plugin: updated {NEW_ID} from {OLD_VERSION} to {VERSION}"
        )),
        "{text}"
    );
    assert!(text.contains(RELOAD), "{text}");
    let calls = f.calls();
    let refresh = calls
        .find("plugin marketplace update amplifiedengineer")
        .unwrap();
    let update = calls.find(&format!("plugin update {NEW_ID}")).unwrap();
    assert!(refresh < update, "refresh first: {calls}");
}

#[test]
fn both_installed_ids_are_updated() {
    let f = Fixture::new("bump");
    f.install(&[NEW_ID, OLD_ID]);
    let o = f.run(&[], true);
    let text = out(&o);
    assert!(o.status.success(), "{text}");
    let calls = f.calls();
    assert!(
        calls.contains(&format!("plugin update {NEW_ID}")),
        "{calls}"
    );
    assert!(
        calls.contains(&format!("plugin update {OLD_ID}")),
        "{calls}"
    );
    assert!(
        calls.contains("plugin marketplace update pragmatic-engineer"),
        "{calls}"
    );
    assert_eq!(text.matches("plugin: updated").count(), 2, "{text}");
    assert_eq!(text.matches(RELOAD).count(), 1, "{text}");
}

#[test]
fn an_update_ignores_the_config_opt_out() {
    let f = Fixture::new("bump");
    f.install(&[NEW_ID]);
    let mut set = isolated::playbook_in(&f.home);
    assert!(set
        .args(["config", "set", "upgrade.plugin", "off"])
        .status()
        .unwrap()
        .success());
    let o = f.run(&[], true);
    assert!(out(&o).contains("plugin: updated"), "{}", out(&o));
}

#[test]
fn an_unchanged_plugin_the_marketplace_has_not_published_exits_zero() {
    let f = Fixture::new("same");
    f.install(&[NEW_ID]);
    let o = f.run(&[], true);
    let text = out(&o);
    assert!(o.status.success(), "{text}");
    assert!(
        text.contains("the marketplace has not published a newer plugin yet"),
        "{text}"
    );
    assert!(!text.contains(RELOAD), "{text}");
}

#[test]
fn a_failed_update_exits_one_and_names_the_command() {
    let f = Fixture::new("fail");
    f.install(&[NEW_ID]);
    let o = f.run(&[], true);
    let text = out(&o);
    assert_eq!(o.status.code(), Some(1), "{text}");
    assert!(
        text.contains(&format!(
            "the update failed (network down); run: claude plugin update {NEW_ID}"
        )),
        "{text}"
    );
}

#[test]
fn no_plugin_installed_prints_the_exact_install_command_and_exits_zero() {
    let f = Fixture::new("bump");
    let o = f.run(&[], true);
    let text = out(&o);
    assert!(o.status.success(), "{text}");
    assert!(text.contains(INSTALL_COMMAND), "{text}");
    assert_eq!(f.calls(), "", "claude must not run");
}

#[test]
fn no_claude_on_path_exits_one_with_the_command() {
    let f = Fixture::new("bump");
    f.install(&[NEW_ID]);
    let o = f.run(&[], false);
    let text = out(&o);
    assert_eq!(o.status.code(), Some(1), "{text}");
    assert!(text.contains("the claude command is not on PATH"), "{text}");
    assert!(
        text.contains(&format!("claude plugin update {NEW_ID}")),
        "{text}"
    );
}

#[test]
fn json_wraps_the_result_in_the_envelope_and_exits_zero_on_failure() {
    let f = Fixture::new("fail");
    f.install(&[NEW_ID]);
    let o = f.run(&["--json"], true);
    assert!(o.status.success());
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["code"], 1);
    assert_eq!(v["plugins"][0]["status"], "failed");

    let f = Fixture::new("bump");
    f.install(&[NEW_ID]);
    let v: Value = serde_json::from_str(&out(&f.run(&["--json"], true))).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["reload"], true);
    assert_eq!(v["plugins"][0]["status"], "updated");
}
