// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests for `playbook cc launch`, with a fake `claude` on PATH
//! that records its argv, and for the notice the removed `shell-init` prints. They replace the shell launcher
//! suites that covered `dispatch.sh`, `config-drift.sh` and the bash/zsh parity.

#[path = "support/collector_guard.rs"]
mod collector_guard;
#[path = "support/isolated.rs"]
mod isolated;

use playbook::cc::telemetry::TEST_SPAWN_VAR;
use playbook::usage::daemon::{lock_state, stamp_path, LockState};
use playbook::usage::run::Paths;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static COUNTER: AtomicU64 = AtomicU64::new(0);
const SID: &str = "11111111-2222-3333-4444-555555555555";

struct Env {
    root: PathBuf,
    home: PathBuf,
    work: PathBuf,
}

fn make_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn env(tag: &str) -> Env {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "pb-launch-{tag}-{}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).unwrap();
    let root = fs::canonicalize(&dir).unwrap();
    let home = root.join("home");
    let work = root.join("proj");
    let bin = root.join("bin");
    for d in [&home, &work, &bin, &root.join("tmp")] {
        fs::create_dir_all(d).unwrap();
    }
    make_exec(
        &bin.join("claude"),
        "#!/bin/sh\nprintf '%s\\n' \"$PATH\" >> \"$FAKE_LOG.path\"\nprintf '%s\\n' \"$*\" >> \"$FAKE_LOG\"\nprintf '%s\\n' \"$PWD\" >> \"$FAKE_LOG.pwd\"\nprintf '%s\\n' \"${CLAUDE_CODE_DISABLE_AUTO_MEMORY:-unset}\" >> \"$FAKE_LOG.mem\"\nprintf '%s\\n' \"${PLAYBOOK_AGENT_VARIANTS:-unset}\" >> \"$FAKE_LOG.var\"\nprintf '%s\\n' \"${ANTHROPIC_DEFAULT_SONNET_MODEL:-unset}\" >> \"$FAKE_LOG.env\"\nprintf '%s\\n' \"${PLAYBOOK_AGENT_VARIANTS_PLUGIN:-unset}\" >> \"$FAKE_LOG.vplug\"\nprintf '%s\\n' \"${CLAUDE_CODE_EFFORT_LEVEL:-unset}\" >> \"$FAKE_LOG.eff\"\nprev=; for a in \"$@\"; do if [ \"$prev\" = --plugin-dir ]; then cp -R \"$a\" \"$FAKE_LOG.plug\"; fi; prev=$a; done\nexit ${FAKE_EXIT:-0}\n",
    );
    // A real home has the upgrade record from install, so the first-run step
    // (ADR 0034 decision 13) stays quiet during a launch.
    let seeded = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .arg("migrate")
        .env("HOME", &home)
        .env("TMPDIR", root.join("tmp"))
        .output()
        .expect("run playbook migrate");
    assert!(seeded.status.success(), "{seeded:?}");
    Env { root, home, work }
}

impl Env {
    fn log(&self) -> PathBuf {
        self.root.join("claude.log")
    }
    /// Every call, as the fake `claude` saw it.
    fn raw_calls(&self) -> Vec<String> {
        fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }
    /// Every call without the fallback pair the launcher always adds, so the
    /// other tests keep reading the flags they care about.
    fn calls(&self) -> Vec<String> {
        const PAIR: &str = "--fallback-model claude-opus-5,claude-sonnet-5,claude-haiku-4-5";
        self.raw_calls()
            .into_iter()
            .map(|l| {
                let l = l.replacen(&format!("{PAIR} "), "", 1);
                let l = l.replacen(&format!(" {PAIR}"), "", 1).replacen(PAIR, "", 1);
                l.replacen("--effort medium ", "", 1)
                    .replacen(" --effort medium", "", 1)
                    .replacen("--effort medium", "", 1)
            })
            .collect()
    }
    fn path(&self) -> String {
        format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }
    fn launch(&self, args: &[&str]) -> Output {
        self.launch_with(&["--"], args, &[])
    }
    fn launch_with(&self, pre: &[&str], args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut cmd = self.playbook(&["cc", "launch"]);
        cmd.args(pre).args(args);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().expect("run playbook")
    }
    /// The binary with `head` as its first arguments and the test env set.
    fn playbook(&self, head: &[&str]) -> Command {
        self.run_as(Path::new(env!("CARGO_BIN_EXE_playbook")), head)
    }
    /// Like `playbook`, started through `program`, such as a `pb` symlink.
    fn run_as(&self, program: &Path, head: &[&str]) -> Command {
        let mut cmd = Command::new(program);
        cmd.args(head)
            .current_dir(&self.work)
            .env("PWD", &self.work)
            .env("HOME", &self.home)
            // A launch scans the temp dir for old variants folders. Point it at
            // an empty dir so a crowded real `$TMPDIR` (a macOS one can hold a
            // million entries) cannot make every launch take minutes.
            .env("TMPDIR", self.root.join("tmp"))
            .env("PATH", self.path())
            .env_remove("CLAUDE_CODE_DISABLE_AUTO_MEMORY")
            .env_remove("CLAUDE_CODE_EFFORT_LEVEL")
            .env_remove("CLAUDE_CODE_ENABLE_TELEMETRY")
            .env_remove("OTEL_LOGS_EXPORTER")
            .env_remove("OTEL_METRICS_EXPORTER")
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
            .env_remove("CLAUDE_PLUGIN_ROOT")
            .env_remove("PLAYBOOK_PLUGIN_DIR")
            .env("FAKE_LOG", self.log())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null");
        cmd
    }
    /// Writes a transcript titled `proj` for the work dir.
    fn session(&self) -> PathBuf {
        let slug: String = self
            .work
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let dir = self.home.join(".claude/projects").join(slug);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("{SID}.jsonl")),
            format!("{{\"customTitle\":\"proj\",\"sessionId\":\"{SID}\"}}\n"),
        )
        .unwrap();
        dir
    }
}

#[test]
fn no_session_starts_fresh_with_the_dir_name() {
    let e = env("fresh-default");
    let out = e.launch(&[]);
    assert!(out.status.success());
    assert_eq!(e.calls(), vec!["-n proj"]);
}

#[test]
fn an_existing_titled_session_is_resumed_and_forks_only_on_drift() {
    let e = env("resume");
    e.session();
    e.launch(&[]);
    e.launch(&[]);
    let calls = e.calls();
    assert!(
        calls[0].starts_with(&format!("-n proj --resume {SID}")),
        "{calls:?}"
    );
    // The first launch had no baseline and re-stamped, so the second is quiet.
    assert_eq!(calls[1], format!("-n proj --resume {SID}"));
}

#[test]
fn config_drift_forks_the_resume_once_per_change() {
    let e = env("drift");
    e.session();
    let settings = e.home.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, "{\"a\":1}").unwrap();

    e.launch(&[]);
    e.launch(&[]);
    fs::write(&settings, "{\"a\":2}").unwrap();
    e.launch(&[]);

    let calls = e.calls();
    assert_eq!(calls[0], format!("-n proj --resume {SID} --fork-session"));
    assert_eq!(calls[1], format!("-n proj --resume {SID}"));
    assert_eq!(calls[2], format!("-n proj --resume {SID} --fork-session"));
}

#[test]
fn launching_trusts_the_dir_but_listing_does_not() {
    let e = env("trust");
    let claude_json = e.home.join(".claude.json");
    fs::write(&claude_json, "{\"projects\":{}}").unwrap();

    e.launch(&["list"]);
    assert!(!fs::read_to_string(&claude_json)
        .unwrap()
        .contains("hasTrustDialogAccepted"));
    e.launch(&["fresh"]);

    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&claude_json).unwrap()).unwrap();
    assert_eq!(
        doc["projects"][e.work.to_str().unwrap()]["hasTrustDialogAccepted"],
        true
    );
}

#[test]
fn claude_does_not_inherit_the_cd_file_variable() {
    let e = env("cd-env");
    make_exec(
        &e.root.join("bin/claude"),
        "#!/bin/sh\nprintf '%s\\n' \"${PLAYBOOK_CC_CD_FILE:-unset}\" >> \"$FAKE_LOG\"\n",
    );
    e.launch_with(&["--"], &[], &[("PLAYBOOK_CC_CD_FILE", "/tmp/x")]);
    assert_eq!(e.calls(), vec!["unset"]);
}

#[test]
fn a_value_flag_is_not_taken_for_the_subcommand() {
    let e = env("flag-value");
    e.session();
    e.launch(&["--model", "list", "hello"]);
    let call = &e.calls()[0];
    assert!(call.starts_with("--model list -n proj --resume"), "{call}");
    assert!(call.ends_with("hello"), "{call}");
}

#[test]
fn fresh_skips_resume_and_raw_resumes_without_forking() {
    let e = env("fresh-raw");
    e.session();
    e.launch(&["fresh", "--x"]);
    e.launch(&["raw"]);
    e.launch(&["raw", "abc"]);
    assert_eq!(
        e.calls(),
        vec![
            "-n proj --x".to_string(),
            format!("--resume {SID} -n proj"),
            "--resume abc -n proj".to_string()
        ]
    );
}

#[test]
fn clean_resumes_a_clone_and_leaves_the_original() {
    let e = env("clean");
    let dir = e.session();
    let out = e.launch(&["clean"]);
    let call = &e.calls()[0];
    assert!(
        call.starts_with("--resume ") && call.ends_with(" -n proj"),
        "{call}"
    );
    assert!(!call.contains(SID));
    assert!(String::from_utf8_lossy(&out.stdout).contains("cc clean: cloned"));
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
}

#[test]
fn a_missing_conversation_falls_back_to_a_fresh_session() {
    let e = env("not-found");
    e.session();
    make_exec(
        &e.root.join("bin/claude"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FAKE_LOG\"\ncase \"$*\" in *--resume*) echo 'No conversation found' >&2; exit 1;; esac\n",
    );
    let out = e.launch(&[]);
    assert!(out.status.success());
    assert_eq!(e.calls().last().unwrap(), "-n proj");
}

#[test]
fn skip_permissions_and_the_system_prompt_lead_the_command_line() {
    let e = env("flags");
    let prompt = e.home.join(".config/playbook/prompts/SYSTEM_PROMPT.md");
    fs::create_dir_all(prompt.parent().unwrap()).unwrap();
    fs::write(&prompt, "p").unwrap();
    let out = e.launch_with(&["--skip-permissions", "--"], &["fresh"], &[]);
    assert!(out.status.success());
    assert_eq!(
        e.calls()[0],
        format!(
            "--dangerously-skip-permissions --system-prompt-file {} -n proj",
            prompt.display()
        )
    );
}

fn install_prompt(e: &Env) -> PathBuf {
    let prompt = e.home.join(".config/playbook/prompts/SYSTEM_PROMPT.md");
    fs::create_dir_all(prompt.parent().unwrap()).unwrap();
    fs::write(&prompt, "p").unwrap();
    prompt
}

#[test]
fn the_prompt_is_only_ever_passed_through_the_replace_flag() {
    let e = env("replace");
    let prompt = install_prompt(&e);
    let out = e.launch_with(&["--"], &["fresh"], &[]);
    assert!(out.status.success());
    let call = e.calls()[0].clone();
    assert_eq!(
        call,
        format!("--system-prompt-file {} -n proj", prompt.display())
    );
    assert!(!call.contains("--append-system-prompt"), "{call}");
    assert!(!call.contains("--settings"), "{call}");
}

#[test]
fn ccd_with_a_user_replace_flag_adds_only_skip_permissions() {
    let e = env("ccdonly");
    install_prompt(&e);
    let out = e.launch_with(
        &["--skip-permissions", "--"],
        &["--system-prompt-file=/y", "fresh"],
        &[],
    );
    assert!(out.status.success());
    assert_eq!(
        e.calls()[0],
        "--dangerously-skip-permissions --system-prompt-file=/y -n proj"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("playbook:"), "{stderr}");
}

#[test]
fn a_user_append_flag_is_refused_and_claude_is_not_started() {
    let e = env("appendrefused");
    install_prompt(&e);
    for args in [
        vec!["--append-system-prompt", "x", "fresh"],
        vec!["--append-system-prompt=x", "fresh"],
        vec!["--append-system-prompt-file", "/x", "fresh"],
        vec![
            "--system-prompt-file=/y",
            "--append-system-prompt=x",
            "fresh",
        ],
    ] {
        let out = e.launch_with(&["--"], &args, &[]);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("never appends"), "{err}");
    }
    assert!(e.raw_calls().is_empty());
}

#[test]
fn a_missing_prompt_file_adds_no_system_prompt_flag() {
    let e = env("noprompt");
    let out = e.launch_with(&["--"], &["fresh"], &[]);
    assert!(out.status.success());
    assert_eq!(e.calls()[0], "-n proj");
}

#[test]
fn a_user_system_prompt_flag_means_playbook_adds_no_prompt() {
    let e = env("userprompt");
    let prompt = e.home.join(".config/playbook/prompts/SYSTEM_PROMPT.md");
    fs::create_dir_all(prompt.parent().unwrap()).unwrap();
    fs::write(&prompt, "p").unwrap();
    for flag in ["--system-prompt=x", "--system-prompt-file=/x"] {
        let out = e.launch_with(&["--"], &[flag, "fresh"], &[]);
        assert!(out.status.success());
        let call = e.calls().pop().unwrap();
        assert!(!call.contains("SYSTEM_PROMPT.md"), "{call}");
    }
}

#[test]
fn the_claude_exit_code_is_the_launchers_exit_code() {
    let e = env("exit");
    let out = e.launch_with(&["--"], &[], &[("FAKE_EXIT", "7")]);
    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn list_launches_nothing() {
    let e = env("list");
    e.session();
    let out = e.launch(&["list"]);
    assert!(out.status.success());
    assert!(e.calls().is_empty());
    assert!(String::from_utf8_lossy(&out.stdout).contains(&SID[..8]));
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

#[test]
fn worktree_launches_inside_the_new_tree_and_prints_the_cd_to_run() {
    let e = env("worktree");
    for args in [
        vec!["init", "-q", "-b", "master"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "T"],
    ] {
        git(&e.work, &args);
    }
    fs::write(e.work.join("f"), "x").unwrap();
    git(&e.work, &["add", "f"]);
    git(&e.work, &["commit", "-q", "-m", "init"]);
    // A protected branch needs no `origin`, keeping the test hermetic.
    git(&e.work, &["branch", "develop"]);

    let out = e.launch_with(
        &["--skip-permissions", "--"],
        &["worktree", "develop"],
        &[("WORKTREE_NO_PUSH", "1")],
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let pwd = fs::read_to_string(format!("{}.pwd", e.log().display())).unwrap();
    let target = pwd.trim();
    assert!(target.ends_with("develop"), "{target}");
    assert!(Path::new(target).is_dir());
    assert!(stderr.contains(&format!("run: cd {target}")), "{stderr}");
    assert_eq!(e.calls()[0], "--dangerously-skip-permissions -n develop");
}

#[test]
fn worktree_outside_a_repo_fails_without_launching() {
    let e = env("worktree-norepo");
    let out = e.launch_with(&["--"], &["worktree", "x"], &[]);
    assert_eq!(out.status.code(), Some(10));
    assert!(e.calls().is_empty());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("run: cd"));
}

/// The scratch home a removed `shell-init` test runs in, with a `bin`
/// folder that holds `playbook` as a link to the built binary.
fn removed_shell_init_home(tag: &str) -> (PathBuf, PathBuf) {
    let home = isolated::fresh_home();
    let bin = home.join(format!("bin-{tag}"));
    fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_playbook"), bin.join("playbook")).unwrap();
    (home, bin)
}

#[test]
fn the_removed_shell_init_prints_one_stderr_line_and_exits_zero() {
    for args in [&["shell-init"][..], &["shell-init", "--shell", "zsh"]] {
        let home = isolated::fresh_home();

        let out = isolated::playbook_in(&home).args(args).output().unwrap();

        assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("{}\n", playbook::fastpath::SHELL_INIT_REMOVED),
            "{args:?}"
        );
        assert_eq!(
            fs::read_dir(&home).unwrap().count(),
            0,
            "{args:?} wrote under home"
        );
    }
}

#[test]
fn the_removed_shell_init_keeps_a_shell_with_the_old_rc_line_working() {
    let line = playbook::init::rcfile::SOURCE_LINE;
    for (shell, rc, flags) in [
        ("zsh", ".zshrc", &["-i", "-c"][..]),
        ("bash", ".bashrc", &["-c"]),
    ] {
        if Command::new(shell).arg("--version").output().is_err() {
            eprintln!("SKIP: {shell} not available");
            continue;
        }
        let (home, bin) = removed_shell_init_home(shell);
        fs::write(home.join(rc), format!("{line}\n")).unwrap();
        // zsh -i reads the rc file itself; bash -c gets it sourced.
        let script = match shell {
            "zsh" => "echo rc=$?; echo alive".to_string(),
            _ => format!(". {}/{rc}; echo rc=$?; echo alive", home.display()),
        };
        let mut cmd = Command::new(shell);
        isolated::isolate(&mut cmd, &home);

        let out = cmd
            .args(flags)
            .arg(&script)
            .env("ZDOTDIR", &home)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{shell}: {stdout} {stderr}");
        assert_eq!(stdout, "rc=0\nalive\n", "{shell}: {stderr}");
        assert!(
            stderr.contains(playbook::fastpath::SHELL_INIT_REMOVED),
            "{shell}: {stderr}"
        );
        assert!(!stderr.contains("unrecognized"), "{shell}: {stderr}");
    }
}

fn set_effort(e: &Env, level: &str) {
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["effort", level])
        .current_dir(&e.work)
        .env("HOME", &e.home)
        .output()
        .expect("run playbook effort");
    assert!(out.status.success(), "{out:?}");
}

fn hash_of(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_default()
}

fn seed_settings(e: &Env, body: &str) -> PathBuf {
    let settings = e.home.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, body).unwrap();
    settings
}

fn child_effort_env(e: &Env) -> Vec<String> {
    fs::read_to_string(format!("{}.eff", e.log().display()))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

#[test]
fn no_effort_or_config_path_writes_claude_settings_or_passes_settings() {
    let e = env("effort-ceiling");
    let settings = seed_settings(&e, r#"{"theme":"dark","effortLevel":"xhigh"}"#);
    let before = hash_of(&settings);

    set_effort(&e, "low");
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["config", "set", "--global", "effort.max", "low"])
        .current_dir(&e.work)
        .env("HOME", &e.home)
        .output()
        .expect("run playbook config set");
    assert!(out.status.success(), "{out:?}");
    e.launch(&["fresh"]);
    e.launch_with(&["--skip-permissions", "--"], &["fresh"], &[]);

    for raw in e.raw_calls() {
        assert!(!raw.contains("--settings"), "{raw}");
    }
    // A level stated in settings is the user's own and is left alone.
    assert!(
        !e.raw_calls()[0].contains("--effort"),
        "{:?}",
        e.raw_calls()
    );
    assert_eq!(hash_of(&settings), before, "settings.json was modified");
}

#[test]
fn a_ceiling_with_no_stated_effort_lowers_the_default() {
    let e = env("effort-default-lowered");
    set_effort(&e, "low");
    e.launch(&["fresh"]);
    let raw = e.raw_calls();
    assert!(raw[0].contains("--effort low"), "{raw:?}");
}

#[test]
fn an_unset_ceiling_keeps_the_current_level() {
    let e = env("effort-current-level");
    let settings = seed_settings(&e, r#"{"effortLevel":"high"}"#);
    let before = hash_of(&settings);
    e.launch(&["fresh"]);
    let raw = e.raw_calls();
    assert!(!raw[0].contains("--effort"), "{raw:?}");
    assert_eq!(hash_of(&settings), before);
}

#[test]
fn an_effort_env_of_max_moves_to_the_flag_so_a_component_set_to_low_stays_low() {
    let e = env("effort-env-max");
    let settings = seed_settings(&e, r#"{"maxEffortLevel":"max"}"#);
    let before = hash_of(&settings);

    let out = e.launch_with(&["--"], &["fresh"], &[("CLAUDE_CODE_EFFORT_LEVEL", "max")]);

    assert!(out.status.success());
    let raw = e.raw_calls();
    assert!(raw[0].contains("--effort max"), "{raw:?}");
    assert!(!raw[0].contains("--settings"), "{raw:?}");
    assert_eq!(
        child_effort_env(&e),
        vec!["unset"],
        "the child must not see the variable"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("moved to --effort max"), "{err}");
    assert_eq!(hash_of(&settings), before);
}

#[test]
fn the_env_level_is_trimmed_and_an_equals_form_flag_still_wins() {
    let e = env("effort-env-forms");
    e.launch_with(
        &["--"],
        &["fresh"],
        &[("CLAUDE_CODE_EFFORT_LEVEL", " MAX ")],
    );
    e.launch_with(
        &["--"],
        &["--effort=low", "fresh"],
        &[("CLAUDE_CODE_EFFORT_LEVEL", "max")],
    );
    let raw = e.raw_calls();
    assert!(raw[0].contains("--effort max"), "{raw:?}");
    assert_eq!(raw[1].matches("--effort").count(), 1, "{raw:?}");
    assert!(raw[1].contains("--effort=low"), "{raw:?}");
}

#[test]
fn a_playbook_ceiling_does_not_change_a_level_the_session_states() {
    let e = env("effort-env-over-ceiling");
    set_effort(&e, "low");
    e.launch_with(&["--"], &["fresh"], &[("CLAUDE_CODE_EFFORT_LEVEL", "max")]);
    let raw = e.raw_calls();
    assert!(raw[0].contains("--effort max"), "{raw:?}");
    assert!(!raw[0].contains("--effort low"), "{raw:?}");
    // Nothing caps the session here, so the fallback chain still knows max is in use.
    assert!(
        raw[0].contains("--fallback-model claude-haiku-4-5 "),
        "{raw:?}"
    );
}

#[test]
fn an_effort_env_that_is_not_a_level_is_dropped_without_a_flag() {
    let e = env("effort-env-auto");
    e.launch_with(&["--"], &["fresh"], &[("CLAUDE_CODE_EFFORT_LEVEL", "auto")]);
    let raw = e.raw_calls();
    assert!(!raw[0].contains("--effort"), "{raw:?}");
    assert_eq!(child_effort_env(&e), vec!["unset"]);
}

#[test]
fn an_own_effort_flag_beats_the_env_and_the_env_is_still_dropped() {
    let e = env("effort-env-and-flag");
    e.launch_with(
        &["--"],
        &["--effort", "low", "fresh"],
        &[("CLAUDE_CODE_EFFORT_LEVEL", "max")],
    );
    let raw = e.raw_calls();
    assert_eq!(raw[0].matches("--effort").count(), 1, "{raw:?}");
    assert!(raw[0].contains("--effort low"), "{raw:?}");
    assert_eq!(child_effort_env(&e), vec!["unset"]);
}

#[test]
fn no_effort_env_leaves_the_child_environment_alone() {
    let e = env("effort-no-env");
    let out = e.launch(&["fresh"]);
    assert!(!String::from_utf8_lossy(&out.stderr).contains("CLAUDE_CODE_EFFORT_LEVEL"));
    assert!(
        e.raw_calls()[0].contains("--effort medium"),
        "{:?}",
        e.raw_calls()
    );
}

#[test]
fn a_lower_claude_code_ceiling_wins_and_is_never_edited() {
    let e = env("effort-claude-lower");
    let settings = e.home.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, r#"{"maxEffortLevel":"low"}"#).unwrap();
    set_effort(&e, "high");
    e.launch(&["fresh"]);
    let raw = e.raw_calls();
    assert!(raw[0].contains("--effort low"), "{raw:?}");
    assert!(!raw[0].contains("--settings"), "{raw:?}");
    assert_eq!(
        fs::read_to_string(&settings).unwrap(),
        r#"{"maxEffortLevel":"low"}"#
    );
}

#[test]
fn an_own_settings_flag_is_passed_through_untouched() {
    let e = env("effort-own-settings");
    set_effort(&e, "low");
    e.launch_with(&["--"], &["--settings", "my.json", "fresh"], &[]);
    let call = e.calls().pop().unwrap();
    assert_eq!(call.matches("--settings").count(), 1, "{call}");
    assert!(call.contains("--settings my.json"), "{call}");
}

#[test]
fn the_launcher_passes_the_previous_generation_as_the_fallback() {
    let e = env("fallback");
    e.launch(&["fresh"]);
    let raw = e.raw_calls();
    assert!(
        raw[0].contains("--fallback-model claude-opus-5,claude-sonnet-5,claude-haiku-4-5"),
        "{raw:?}"
    );
}

#[test]
fn a_session_that_states_no_effort_runs_at_medium() {
    let e = env("effort-default");
    e.launch(&["fresh"]);
    let raw = e.raw_calls();
    assert_eq!(raw[0].matches("--effort").count(), 1, "{raw:?}");
    assert!(raw[0].contains("--effort medium"), "{raw:?}");
}

#[test]
fn an_effort_the_user_asked_for_is_left_alone() {
    let e = env("effort-asked");
    e.launch_with(&["--effort", "high", "--"], &["fresh"], &[]);
    let raw = e.raw_calls();
    assert_eq!(raw[0].matches("--effort").count(), 1, "{raw:?}");
    assert!(raw[0].contains("--effort high"), "{raw:?}");
    assert!(!raw[0].contains("--effort medium"), "{raw:?}");
}

#[test]
fn an_effort_level_in_the_user_settings_is_left_alone_and_never_written() {
    let e = env("effort-settings");
    let settings = e.home.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, r#"{"effortLevel":"high"}"#).unwrap();
    e.launch(&["fresh"]);
    let raw = e.raw_calls();
    assert!(!raw[0].contains("--effort"), "{raw:?}");
    assert_eq!(
        fs::read_to_string(&settings).unwrap(),
        r#"{"effortLevel":"high"}"#,
        "the launcher must never edit Claude Code settings"
    );
}

#[test]
fn a_fallback_the_user_chose_is_left_alone() {
    let e = env("fallback-own");
    e.launch_with(&["--fallback-model", "haiku", "--"], &["fresh"], &[]);
    let raw = e.raw_calls();
    assert_eq!(raw[0].matches("--fallback-model").count(), 1, "{raw:?}");
    assert!(raw[0].contains("--fallback-model haiku"), "{raw:?}");
}

fn set_memory_source(e: &Env, value: &str) {
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["config", "set", "--global", "memory.source", value])
        .current_dir(&e.work)
        .env("HOME", &e.home)
        .output()
        .expect("run playbook config set");
    assert!(out.status.success(), "{out:?}");
}

fn mem_env_seen(e: &Env) -> String {
    fs::read_to_string(e.root.join("claude.log.mem")).unwrap_or_default()
}

#[test]
fn playbook_only_memory_turns_claude_auto_memory_off_for_the_session() {
    let e = env("mem-playbook");
    set_memory_source(&e, "playbook");
    e.launch(&["fresh"]);
    assert_eq!(mem_env_seen(&e).trim(), "1");
}

#[test]
fn the_default_leaves_claude_auto_memory_alone() {
    let e = env("mem-both");
    e.launch(&["fresh"]);
    assert_eq!(mem_env_seen(&e).trim(), "unset");
    set_memory_source(&e, "both");
    e.launch(&["fresh"]);
    assert!(mem_env_seen(&e).lines().all(|l| l == "unset"));
}

#[test]
fn choosing_a_memory_source_never_edits_claude_code_settings() {
    let e = env("mem-no-write");
    let settings = e.home.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let body = r#"{"autoMemoryEnabled":true,"theme":"dark"}"#;
    fs::write(&settings, body).unwrap();
    set_memory_source(&e, "playbook");
    e.launch(&["fresh"]);
    set_memory_source(&e, "both");
    assert_eq!(fs::read_to_string(&settings).unwrap(), body);
}

#[test]
fn the_session_can_find_the_binary_that_launched_it() {
    let e = env("child-path");
    e.launch(&["fresh"]);
    let exe_dir = Path::new(env!("CARGO_BIN_EXE_playbook")).parent().unwrap();
    let seen = fs::read_to_string(format!("{}.path", e.log().display())).unwrap();
    let first = std::env::split_paths(seen.lines().next().unwrap())
        .next()
        .unwrap();
    assert_eq!(first, exe_dir, "{seen}");
}

#[test]
fn a_path_that_already_has_the_binary_is_left_alone() {
    let e = env("child-path-same");
    let exe_dir = Path::new(env!("CARGO_BIN_EXE_playbook")).parent().unwrap();
    let path = format!("{}:{}", exe_dir.display(), e.path());
    e.launch_with(&["--"], &["fresh"], &[("PATH", path.as_str())]);
    let seen = fs::read_to_string(format!("{}.path", e.log().display())).unwrap();
    assert_eq!(seen.lines().next().unwrap(), path);
}

// --- Effort-tier variants are gone (ADR-0017 amendment) ---

fn plugin_with_agents(e: &Env) -> PathBuf {
    let root = e.root.join("plugin");
    fs::create_dir_all(root.join("agents")).unwrap();
    fs::write(
        root.join("agents/reviewer.md"),
        "---\nname: reviewer\ndescription: d\ntools: Read, Grep\nmodel: sonnet\neffort: high\n---\n\nReview.\n",
    )
    .unwrap();
    root
}

fn launch_with_plugin(e: &Env, root: &Path, args: &[&str]) {
    let out = e.launch_with(
        &["--"],
        args,
        &[("CLAUDE_PLUGIN_ROOT", root.to_str().unwrap())],
    );
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn the_launcher_adds_no_variant_agents_or_plugin() {
    let e = env("variants");
    let root = plugin_with_agents(&e);
    launch_with_plugin(&e, &root, &["fresh"]);
    let line = e.raw_calls().remove(0);
    assert!(!line.contains("--plugin-dir"), "{line}");
    assert!(!line.contains("--agents"), "{line}");
}

#[test]
fn the_retired_variants_setting_is_refused_and_changes_nothing() {
    let e = env("variants-off");
    let root = plugin_with_agents(&e);
    for value in ["off", "all"] {
        let set = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["config", "set", "--global", "agents.variants", value])
            .env("HOME", &e.home)
            .output()
            .unwrap();
        assert!(!set.status.success(), "{set:?}");
    }
    launch_with_plugin(&e, &root, &["fresh"]);
    let line = e.raw_calls().remove(0);
    assert!(
        !line.contains("--plugin-dir") && !line.contains("--agents"),
        "{line}"
    );
}

#[test]
fn a_leftover_variants_folder_of_a_dead_session_is_removed_on_launch() {
    let e = env("variants-leftover");
    let tmp = e.root.join("tmp");
    let old = tmp.join("playbook-variants-4000000");
    fs::create_dir_all(old.join("agents")).unwrap();
    fs::write(old.join("agents/reviewer-low.md"), "x").unwrap();
    // No TMPDIR override here: the harness default must already be `tmp`, or a
    // crowded real temp dir would slow every launch in this file.
    let out = e.launch(&["fresh"]);
    assert!(out.status.success(), "{out:?}");
    assert!(!old.exists(), "the leftover folder survived: the launch did not scan the harness tmp dir, or the cleanup broke");
}

#[test]
fn at_xhigh_the_fallback_chain_starts_at_the_model_that_accepts_it() {
    let e = env("fallback-xhigh");
    e.launch_with(&["--effort", "xhigh", "--"], &["fresh"], &[]);
    let raw = e.raw_calls();
    assert!(
        raw[0].contains("--fallback-model claude-haiku-4-5 "),
        "{raw:?}"
    );
    assert!(!raw[0].contains("claude-sonnet-5,"), "{raw:?}");
}

#[test]
fn a_models_override_reaches_claude_as_the_alias_variable() {
    let e = env("models-override");
    let set = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args([
            "config",
            "set",
            "--global",
            "models.sonnet",
            "claude-sonnet-5",
        ])
        .current_dir(&e.work)
        .env("HOME", &e.home)
        .output()
        .unwrap();
    assert!(set.status.success(), "{set:?}");
    e.launch(&["fresh"]);
    let logged = fs::read_to_string(format!("{}.env", e.log().display())).unwrap();
    assert_eq!(logged.trim(), "claude-sonnet-5");
}

#[test]
fn without_an_override_the_alias_variable_is_not_set() {
    let e = env("models-no-override");
    e.launch(&["fresh"]);
    let logged = fs::read_to_string(format!("{}.env", e.log().display())).unwrap();
    assert_eq!(logged.trim(), "unset");
}

/// How long a collector a test or `cc` started may take to come up.
const UP_BOUND: Duration = Duration::from_secs(10);

/// Kills any collector in `paths` when the test ends, passing or not.
struct CollectorGuard(Paths);

impl Drop for CollectorGuard {
    fn drop(&mut self) {
        let settled = collector_guard::kill_all(&self.0);
        if !std::thread::panicking() {
            assert!(settled, "a collector kept the lock after SIGKILL");
        }
    }
}

/// Records a collector on a port that is free now, and returns the port.
fn record_collector(e: &Env) -> u16 {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let data = e.home.join(".config/playbook/data");
    fs::create_dir_all(&data).unwrap();
    fs::write(
        data.join("collector.json"),
        format!(r#"{{"pid":0,"port":{port},"token":"t"}}"#),
    )
    .unwrap();
    port
}

/// Points the user settings at the collector on `port`, as `setup` does.
fn point_settings_here(e: &Env, port: u16) {
    fs::create_dir_all(e.home.join(".claude")).unwrap();
    fs::write(
        e.home.join(".claude/settings.json"),
        format!(r#"{{"env":{{"CLAUDE_CODE_ENABLE_TELEMETRY":"1","OTEL_EXPORTER_OTLP_ENDPOINT":"http://127.0.0.1:{port}"}}}}"#),
    )
    .unwrap();
}

/// `cc launch` with no headless switch and no telemetry variable from the
/// caller, so only the home decides.
fn cc_not_headless(e: &Env) -> Command {
    let mut cmd = e.playbook(&["cc", "launch", "--"]);
    cmd.env_remove("CI").env_remove("PLAYBOOK_HEADLESS");
    for (name, _) in std::env::vars() {
        if name.starts_with("OTEL_") {
            cmd.env_remove(name);
        }
    }
    cmd
}

/// Writes each session's OTLP endpoint to `$FAKE_LOG.otel`.
fn log_otel_endpoint(e: &Env) {
    make_exec(
        &e.root.join("bin/claude"),
        "#!/bin/sh\nprintf '%s\\n' \"${OTEL_EXPORTER_OTLP_ENDPOINT:-unset}\" >> \"$FAKE_LOG.otel\"\nexit 0\n",
    );
}

#[test]
fn cc_starts_the_collector_daemon_when_the_settings_point_at_it() {
    let e = env("collector-spawn");
    let port = record_collector(&e);
    point_settings_here(&e, port);
    let paths = Paths::from_home(&e.home);
    let _guard = CollectorGuard(paths.clone());
    let mut cmd = cc_not_headless(&e);
    cmd.env(TEST_SPAWN_VAR, "1");

    let out = cmd.output().unwrap();
    let up = collector_guard::within(UP_BOUND, || {
        matches!(lock_state(&paths), LockState::Held(_))
    });

    assert!(out.status.success(), "{out:?}");
    assert!(up, "cc started no collector");
}

#[test]
fn cc_starts_no_collector_with_telemetry_off() {
    let e = env("collector-off");
    log_otel_endpoint(&e);
    let paths = Paths::from_home(&e.home);
    let _guard = CollectorGuard(paths.clone());
    // Before the files that set telemetry up, so this command cannot start one.
    let off = e
        .playbook(&["config", "set", "--global", "usage.telemetry", "off"])
        .output()
        .unwrap();
    assert!(off.status.success(), "{off:?}");
    let port = record_collector(&e);
    point_settings_here(&e, port);
    let mut cmd = cc_not_headless(&e);
    cmd.env(TEST_SPAWN_VAR, "1");

    let out = cmd.output().unwrap();

    assert!(out.status.success(), "{out:?}");
    assert_eq!(lock_state(&paths), LockState::Free);
    assert!(!stamp_path(&paths).exists(), "a spawn was attempted");
    let seen = fs::read_to_string(e.root.join("claude.log.otel")).unwrap();
    assert_eq!(seen, "unset\n");
}

#[test]
fn a_cc_session_with_no_telemetry_setting_is_pointed_at_the_running_collector() {
    let e = env("collector-env");
    log_otel_endpoint(&e);
    let port = record_collector(&e);
    let paths = Paths::from_home(&e.home);
    let _guard = CollectorGuard(paths.clone());
    let mut collect = e.playbook(&["usage", "collect", "--idle-seconds", "0"]);
    collect
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut collector = collect.spawn().unwrap();
    let state = playbook::usage::collector::state_path(&paths);
    let up = collector_guard::within(UP_BOUND, || {
        matches!(lock_state(&paths), LockState::Held(_))
            && playbook::usage::collector::read_state(&state).is_some_and(|s| s.pid != 0)
            && std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    });

    let out = cc_not_headless(&e).output().unwrap();
    let _ = collector.kill();
    let _ = collector.wait();

    assert!(up, "the collector did not come up");
    assert!(out.status.success(), "{out:?}");
    let seen = fs::read_to_string(e.root.join("claude.log.otel")).unwrap();
    assert_eq!(seen, format!("http://127.0.0.1:{port}\n"));
}

#[test]
fn pb_cc_loads_the_system_prompt_by_default_and_not_when_it_is_off() {
    // Arrange
    let e = env("pb-prompt");
    let prompt = e.home.join(".config/playbook/prompts/SYSTEM_PROMPT.md");
    fs::create_dir_all(prompt.parent().unwrap()).unwrap();
    fs::write(&prompt, "prompt\n").unwrap();
    let pb = e.root.join("pbbin/pb");
    fs::create_dir_all(pb.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_playbook"), &pb).unwrap();

    // Act
    let on = e.run_as(&pb, &["cc", "fresh"]).output().unwrap();
    let off_set = e
        .playbook(&["config", "set", "systemPrompt.enabled", "false"])
        .output()
        .unwrap();
    let off = e.run_as(&pb, &["cc", "fresh"]).output().unwrap();
    let append = e
        .run_as(&pb, &["cc", "--append-system-prompt=x", "fresh"])
        .output()
        .unwrap();

    // Assert
    for out in [&on, &off_set, &off, &append] {
        assert!(out.status.success(), "{out:?}");
    }
    let calls = e.calls();
    assert_eq!(
        calls[0],
        format!("--system-prompt-file {} -n proj", prompt.display())
    );
    assert_eq!(calls[1], "-n proj");
    assert_eq!(calls[2], "--append-system-prompt=x -n proj");
}

#[test]
fn cc_and_ccd_launch_from_the_binary_with_no_shell_function() {
    // Arrange
    let e = env("direct");

    // Act
    let cc = e
        .playbook(&["cc", "fresh", "--model", "x"])
        .output()
        .unwrap();
    let ccd = e.playbook(&["ccd", "fresh"]).output().unwrap();

    // Assert
    assert!(cc.status.success(), "{cc:?}");
    assert!(ccd.status.success(), "{ccd:?}");
    let calls = e.calls();
    assert!(calls[0].contains("--model x"), "{calls:?}");
    assert!(
        !calls[0].contains("--dangerously-skip-permissions"),
        "{calls:?}"
    );
    assert!(
        calls[1].contains("--dangerously-skip-permissions"),
        "{calls:?}"
    );
}
