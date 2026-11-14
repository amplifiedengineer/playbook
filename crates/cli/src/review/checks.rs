// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook review checks <dir>`: run the project's own type check, lint and
//! tests in a review worktree and print what they say, for the deep review's
//! subagent prompts. A failing check is output, never an error. `--list`
//! prints the commands instead of running them.

use std::path::Path;
use std::process::Command;

/// One command to run.
struct Step {
    program: &'static str,
    args: &'static [&'static str],
    /// Keep only the last this many lines (install steps are noisy).
    tail: Option<usize>,
}

const fn step(program: &'static str, args: &'static [&'static str]) -> Step {
    Step {
        program,
        args,
        tail: None,
    }
}

impl Step {
    fn command_line(&self) -> String {
        std::iter::once(self.program)
            .chain(self.args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn plan(dir: &Path) -> Option<(Vec<Step>, Vec<Step>)> {
    let has = |f: &str| dir.join(f).is_file();
    if has("package.json") {
        Some((
            vec![Step {
                tail: Some(5),
                ..step("npm", &["install", "--prefer-offline"])
            }],
            vec![
                step("npm", &["run", "typecheck"]),
                step("npm", &["run", "lint"]),
                step("npm", &["test"]),
            ],
        ))
    } else if has("pyproject.toml") || has("setup.py") {
        Some((
            vec![Step {
                tail: Some(3),
                ..step("pip", &["install", "-e", ".", "-q"])
            }],
            vec![
                step("python", &["-m", "mypy", "."]),
                step("python", &["-m", "pytest"]),
            ],
        ))
    } else if has("go.mod") {
        Some((
            vec![],
            vec![
                step("go", &["vet", "./..."]),
                step("go", &["test", "./..."]),
            ],
        ))
    } else if has("Cargo.toml") {
        Some((
            vec![],
            vec![step("cargo", &["check"]), step("cargo", &["test"])],
        ))
    } else {
        None
    }
}

fn run(dir: &Path, s: &Step) -> String {
    let out = match Command::new(s.program)
        .args(s.args)
        .current_dir(dir)
        .output()
    {
        Ok(o) => o,
        Err(e) => return format!("[{} could not run: {e}]", s.program),
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    match s.tail {
        Some(n) => {
            let lines: Vec<&str> = text.lines().collect();
            lines[lines.len().saturating_sub(n)..].join("\n")
        }
        None => text.trim_end().to_string(),
    }
}

const NO_TOOLCHAIN: &str = "[no recognised toolchain; checks skipped]";

/// The commands `run_checks` would run under `dir`, one per line, setup first.
pub fn list_checks(dir: &Path) -> String {
    let Some((setup, checks)) = plan(dir) else {
        return NO_TOOLCHAIN.to_string();
    };
    setup
        .iter()
        .chain(&checks)
        .map(Step::command_line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The combined output of the project's checks under `dir`.
pub fn run_checks(dir: &Path) -> String {
    let Some((setup, checks)) = plan(dir) else {
        return NO_TOOLCHAIN.to_string();
    };
    let mut parts = Vec::new();
    for s in &setup {
        // Setup output is shown but is not part of the check results.
        parts.push(run(dir, s));
    }
    for s in &checks {
        parts.push(run(dir, s));
    }
    parts.retain(|p| !p.is_empty());
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use std::fs;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = scratch_dir(tag);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn an_unknown_toolchain_is_skipped_with_the_marker() {
        assert_eq!(
            run_checks(&dir("checks-none")),
            "[no recognised toolchain; checks skipped]"
        );
    }

    #[test]
    fn the_toolchain_is_picked_from_the_marker_file() {
        for (file, program) in [
            ("package.json", "npm"),
            ("pyproject.toml", "pip"),
            ("setup.py", "pip"),
            ("go.mod", "go"),
            ("Cargo.toml", "cargo"),
        ] {
            let d = dir("checks-pick");
            fs::write(d.join(file), "").unwrap();
            let (setup, checks) = plan(&d).unwrap();
            let first = setup.first().or(checks.first()).unwrap();
            assert_eq!(first.program, program, "{file}");
        }
    }

    #[test]
    fn node_wins_over_the_others_when_several_marker_files_exist() {
        let d = dir("checks-order");
        fs::write(d.join("package.json"), "").unwrap();
        fs::write(d.join("Cargo.toml"), "").unwrap();
        assert_eq!(plan(&d).unwrap().1[0].program, "npm");
    }

    #[test]
    fn a_missing_program_is_reported_not_raised() {
        let s = step("definitely-not-a-program-xyz", &[]);
        assert!(run(&dir("checks-miss"), &s).contains("could not run"));
    }
}
