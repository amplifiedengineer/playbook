// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `watch service install`: a timer that runs `watch once` every 60 seconds,
//! a launchd agent on macOS and a systemd user timer on Linux. No process
//! stays resident. Every call to `launchctl` or `systemctl` goes through
//! [`Manager`], and only `mod.rs` builds the real one.

use std::fs;
use std::path::{Path, PathBuf};

use crate::service_manager::Manager;

pub const LABEL: &str = "dev.playbook.watch";
const SERVICE_UNIT: &str = "playbook-watch.service";
const TIMER_UNIT: &str = "playbook-watch.timer";
/// Seconds between runs of `watch once`.
const INTERVAL: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Launchd,
    Systemd,
}

/// The service manager of this system, or why there is none.
pub fn kind_here() -> Result<Kind, String> {
    if cfg!(target_os = "macos") {
        Ok(Kind::Launchd)
    } else if cfg!(target_os = "linux") {
        Ok(Kind::Systemd)
    } else {
        Err("service install supports macOS (launchd) and Linux (systemd) only".to_string())
    }
}

/// What the unit runs and with which `PATH`, so `gh` is found when the timer
/// starts it.
pub struct Launch<'a> {
    pub exe: &'a Path,
    pub path_env: &'a str,
}

/// A file the service needs and its text.
type UnitFile = (PathBuf, String);

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Refuses text that cannot sit safely inside a unit file.
fn check_safe(what: &str, text: &str) -> Result<(), String> {
    if text
        .chars()
        .any(|c| c.is_control() || matches!(c, '"' | '%'))
    {
        return Err(format!(
            "{what} holds a control character, a quote or a percent sign, which a unit file cannot carry"
        ));
    }
    Ok(())
}

fn plist(launch: &Launch<'_>) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>watch</string>
    <string>once</string>
  </array>
  <key>StartInterval</key>
  <integer>{INTERVAL}</integer>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
  </dict>
</dict>
</plist>
"#,
        exe = xml_escape(&launch.exe.display().to_string()),
        path = xml_escape(launch.path_env),
    )
}

fn service_unit(launch: &Launch<'_>) -> String {
    format!(
        "[Unit]\nDescription=playbook watch, one poll\n\n[Service]\nType=oneshot\n\
         Environment=\"PATH={path}\"\nExecStart=\"{exe}\" watch once\n",
        exe = launch.exe.display(),
        path = launch.path_env,
    )
}

fn timer_unit() -> String {
    format!(
        "[Unit]\nDescription=playbook watch, every {INTERVAL} seconds\n\n[Timer]\n\
         OnBootSec={INTERVAL}\nOnUnitActiveSec={INTERVAL}\nUnit={SERVICE_UNIT}\n\n\
         [Install]\nWantedBy=timers.target\n"
    )
}

fn unit_files(kind: Kind, home: &Path, launch: &Launch<'_>) -> Vec<UnitFile> {
    match kind {
        Kind::Launchd => vec![(
            home.join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
            plist(launch),
        )],
        Kind::Systemd => {
            let dir = home.join(".config/systemd/user");
            vec![
                (dir.join(SERVICE_UNIT), service_unit(launch)),
                (dir.join(TIMER_UNIT), timer_unit()),
            ]
        }
    }
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

/// The commands that load the unit.
fn load_commands(kind: Kind, files: &[UnitFile], uid: u32) -> Vec<(&'static str, Vec<String>)> {
    match kind {
        Kind::Launchd => vec![(
            "launchctl",
            vec![
                "bootstrap".into(),
                format!("gui/{uid}"),
                files[0].0.display().to_string(),
            ],
        )],
        Kind::Systemd => vec![
            ("systemctl", words(&["--user", "daemon-reload"])),
            (
                "systemctl",
                words(&["--user", "enable", "--now", TIMER_UNIT]),
            ),
        ],
    }
}

/// The commands that stop the unit.
fn unload_commands(kind: Kind, uid: u32) -> Vec<(&'static str, Vec<String>)> {
    match kind {
        Kind::Launchd => vec![(
            "launchctl",
            vec!["bootout".into(), format!("gui/{uid}/{LABEL}")],
        )],
        Kind::Systemd => vec![(
            "systemctl",
            words(&["--user", "disable", "--now", TIMER_UNIT]),
        )],
    }
}

fn unit_paths(kind: Kind, home: &Path) -> Vec<PathBuf> {
    let empty = Launch {
        exe: Path::new(""),
        path_env: "",
    };
    unit_files(kind, home, &empty)
        .into_iter()
        .map(|(path, _)| path)
        .collect()
}

/// Whether the unit files are there.
pub fn installed(kind: Kind, home: &Path) -> bool {
    unit_paths(kind, home).iter().all(|p| p.is_file())
}

/// Writes the unit files and loads them. A run of this again replaces the
/// files and loads them again.
pub fn install(
    kind: Kind,
    home: &Path,
    launch: &Launch<'_>,
    manager: &dyn Manager,
    uid: u32,
) -> Result<String, String> {
    check_safe(
        "the path of the playbook binary",
        &launch.exe.display().to_string(),
    )?;
    check_safe("PATH", launch.path_env)?;
    let files = unit_files(kind, home, launch);
    for (path, text) in &files {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        fs::write(path, text).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    }
    // A loaded older copy has to go first, or launchd refuses the new one.
    for (program, args) in unload_commands(kind, uid) {
        let _ = manager.run(program, &args);
    }
    for (program, args) in load_commands(kind, &files, uid) {
        manager.run(program, &args)?;
    }
    Ok(format!(
        "watch installed: it runs `watch once` every {INTERVAL} seconds\n"
    ))
}

/// Stops the timer and removes its files. The audit log stays unless
/// `delete_log` is set, because it is the record of what watch did.
pub fn uninstall(
    kind: Kind,
    home: &Path,
    manager: &dyn Manager,
    uid: u32,
    log_dir: &Path,
    delete_log: bool,
) -> Result<String, String> {
    for (program, args) in unload_commands(kind, uid) {
        // Not loaded is fine: the files go either way.
        let _ = manager.run(program, &args);
    }
    let mut removed = 0;
    for path in unit_paths(kind, home).iter().filter(|p| p.is_file()) {
        fs::remove_file(path).map_err(|e| format!("could not remove {}: {e}", path.display()))?;
        removed += 1;
    }
    if kind == Kind::Systemd {
        let _ = manager.run("systemctl", &words(&["--user", "daemon-reload"]));
    }
    let mut out = if removed == 0 {
        "no watch service was installed\n".to_string()
    } else {
        "watch service removed\n".to_string()
    };
    if delete_log {
        out.push_str(&remove_log(log_dir)?);
    } else {
        out.push_str(&format!(
            "the audit log stays in {}: add --delete-log to remove it\n",
            log_dir.display()
        ));
    }
    Ok(out)
}

fn remove_log(dir: &Path) -> Result<String, String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(String::new());
    };
    let mut count = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("audit") {
            fs::remove_file(entry.path())
                .map_err(|e| format!("could not remove {}: {e}", entry.path().display()))?;
            count += 1;
        }
    }
    Ok(format!("audit log removed ({count} files)\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Fake {
        ran: RefCell<Vec<String>>,
    }

    impl Manager for Fake {
        fn run(&self, program: &str, args: &[String]) -> Result<String, String> {
            self.ran
                .borrow_mut()
                .push(format!("{program} {}", args.join(" ")));
            Ok(String::new())
        }
    }

    fn launch() -> Launch<'static> {
        Launch {
            exe: Path::new("/opt/pb/playbook"),
            path_env: "/usr/bin:/opt/homebrew/bin",
        }
    }

    fn home(tag: &str) -> PathBuf {
        crate::common::test_support::scratch_dir(tag)
    }

    #[test]
    fn the_launchd_agent_runs_watch_once_every_60_seconds() {
        // Arrange
        let home = home("watch-service-launchd");
        let fake = Fake::default();

        // Act
        install(Kind::Launchd, &home, &launch(), &fake, 501).unwrap();

        // Assert
        let text =
            fs::read_to_string(home.join("Library/LaunchAgents/dev.playbook.watch.plist")).unwrap();
        assert!(text.contains("<key>StartInterval</key>\n  <integer>60</integer>"));
        assert!(text.contains(
            "<string>/opt/pb/playbook</string>\n    <string>watch</string>\n    <string>once</string>"
        ));
        assert!(text.contains("/opt/homebrew/bin"));
        assert_eq!(
            fake.ran.borrow().last().map(String::as_str),
            Some(
                format!(
                    "launchctl bootstrap gui/501 {}",
                    home.join("Library/LaunchAgents/dev.playbook.watch.plist")
                        .display()
                )
                .as_str()
            )
        );
    }

    #[test]
    fn the_systemd_timer_fires_a_oneshot_service_every_60_seconds() {
        // Arrange
        let home = home("watch-service-systemd");
        let fake = Fake::default();

        // Act
        install(Kind::Systemd, &home, &launch(), &fake, 1000).unwrap();

        // Assert
        let dir = home.join(".config/systemd/user");
        let timer = fs::read_to_string(dir.join("playbook-watch.timer")).unwrap();
        let service = fs::read_to_string(dir.join("playbook-watch.service")).unwrap();
        assert!(timer.contains("OnUnitActiveSec=60"));
        assert!(timer.contains("Unit=playbook-watch.service"));
        assert!(service.contains("Type=oneshot"));
        assert!(service.contains("ExecStart=\"/opt/pb/playbook\" watch once"));
        assert_eq!(
            fake.ran.borrow().as_slice(),
            [
                "systemctl --user disable --now playbook-watch.timer",
                "systemctl --user daemon-reload",
                "systemctl --user enable --now playbook-watch.timer",
            ]
        );
    }

    #[test]
    fn a_path_a_unit_file_cannot_carry_is_refused_and_nothing_is_written() {
        let home = home("watch-service-unsafe");
        let fake = Fake::default();
        let bad = Launch {
            exe: Path::new("/opt/pb\"/playbook"),
            path_env: "/usr/bin",
        };

        let err = install(Kind::Systemd, &home, &bad, &fake, 1).unwrap_err();

        assert!(err.contains("cannot carry"));
        assert!(!installed(Kind::Systemd, &home));
        assert!(fake.ran.borrow().is_empty());
    }

    #[test]
    fn markup_in_the_path_is_escaped_in_the_plist() {
        let bad = Launch {
            exe: Path::new("/a&b/<pb>"),
            path_env: "/usr/bin",
        };
        assert!(plist(&bad).contains("/a&amp;b/&lt;pb&gt;"));
    }

    #[test]
    fn uninstall_removes_the_unit_and_keeps_the_audit_log_until_asked() {
        // Arrange
        let home = home("watch-service-uninstall");
        let log_dir = home.join("data/watch");
        fs::create_dir_all(&log_dir).unwrap();
        fs::write(log_dir.join("audit.jsonl"), "{}\n").unwrap();
        let fake = Fake::default();
        install(Kind::Systemd, &home, &launch(), &fake, 1).unwrap();

        // Act
        let kept = uninstall(Kind::Systemd, &home, &fake, 1, &log_dir, false).unwrap();

        // Assert
        assert!(!installed(Kind::Systemd, &home));
        assert!(log_dir.join("audit.jsonl").exists());
        assert!(kept.contains("--delete-log"));

        // Act again, now confirmed.
        let gone = uninstall(Kind::Systemd, &home, &fake, 1, &log_dir, true).unwrap();
        assert!(!log_dir.join("audit.jsonl").exists());
        assert!(gone.contains("no watch service was installed"));
        assert!(gone.contains("audit log removed (1 files)"));
    }

    #[test]
    fn installing_again_replaces_the_files_and_loads_them_again() {
        let home = home("watch-service-again");
        let fake = Fake::default();
        install(Kind::Launchd, &home, &launch(), &fake, 501).unwrap();
        install(Kind::Launchd, &home, &launch(), &fake, 501).unwrap();

        let ran = fake.ran.borrow();
        assert_eq!(ran.iter().filter(|c| c.contains("bootstrap")).count(), 2);
        assert_eq!(ran.iter().filter(|c| c.contains("bootout")).count(), 2);
        assert!(installed(Kind::Launchd, &home));
    }
}
