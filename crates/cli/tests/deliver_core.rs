// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `deliver::run` against real temp repos with a bare local `origin` and a
//! fake `gh` script. Every child runs with a scratch HOME and no global git
//! configuration.

#[path = "support/deliver_gh.rs"]
mod deliver_gh;

use deliver_gh::FAKE_GH;
use playbook::deliver::{self, exit, Options, Stage};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static N: AtomicU64 = AtomicU64::new(0);

/// A diff far past the old 1500 line refusal.
const LARGE_DIFF_LINES: usize = 2000;

struct Rig {
    root: PathBuf,
    work: PathBuf,
    origin: PathBuf,
    fake: PathBuf,
}

fn sh(dir: &Path, env: &[(String, String)], program: &str, args: &[&str]) -> String {
    let mut c = Command::new(program);
    c.args(args).current_dir(dir);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        c.env_remove(k);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().expect("spawn");
    assert!(
        out.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl Rig {
    fn new() -> Rig {
        let root = std::env::temp_dir().join(format!(
            "playbook-deliver-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&root);
        let (home, work, origin, fake) = (
            root.join("home"),
            root.join("work"),
            root.join("origin.git"),
            root.join("fake"),
        );
        for d in [&home, &work, &origin, &fake] {
            fs::create_dir_all(d).unwrap();
        }
        let gh = fake.join("gh");
        fs::write(&gh, FAKE_GH).unwrap();
        sh(&fake, &[], "chmod", &["+x", gh.to_str().unwrap()]);
        let rig = Rig {
            root,
            work,
            origin,
            fake,
        };
        let env = rig.env();
        sh(
            &rig.origin,
            &env,
            "git",
            &["init", "-q", "--bare", "-b", "main"],
        );
        sh(&rig.work, &env, "git", &["init", "-q", "-b", "main"]);
        sh(
            &rig.work,
            &env,
            "git",
            &["remote", "add", "origin", rig.origin.to_str().unwrap()],
        );
        rig.write("a.txt", "one\n");
        rig.git(&["add", "-A"]);
        rig.git(&["commit", "-q", "-m", "chore: seed"]);
        rig.git(&["push", "-q", "-u", "origin", "main"]);
        rig.git(&["remote", "set-head", "origin", "main"]);
        rig.git(&["checkout", "-q", "-b", "feat/x"]);
        rig
    }

    fn env(&self) -> Vec<(String, String)> {
        let v = |k: &str, s: &str| (k.to_string(), s.to_string());
        vec![
            v("HOME", self.root.join("home").to_str().unwrap()),
            v("GIT_CONFIG_GLOBAL", "/dev/null"),
            v("GIT_CONFIG_SYSTEM", "/dev/null"),
            v("GIT_AUTHOR_NAME", "t"),
            v("GIT_AUTHOR_EMAIL", "t@t.test"),
            v("GIT_COMMITTER_NAME", "t"),
            v("GIT_COMMITTER_EMAIL", "t@t.test"),
            v("FAKE_GH_DIR", self.fake.to_str().unwrap()),
        ]
    }

    fn git(&self, args: &[&str]) -> String {
        sh(&self.work, &self.env(), "git", args)
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.work.join(name), text).unwrap();
    }

    fn fake_file(&self, name: &str, text: &str) {
        fs::write(self.fake.join(name), text).unwrap();
    }

    fn opts(&self) -> Options {
        let mut o = Options::new(self.work.clone());
        o.gh = self.fake.join("gh").to_string_lossy().into_owned();
        o.env = self.env();
        o.poll = Duration::from_millis(1);
        o
    }

    fn gh_log(&self) -> String {
        fs::read_to_string(self.fake.join("log")).unwrap_or_default()
    }

    fn remote_head(&self, branch: &str) -> String {
        sh(
            &self.origin,
            &self.env(),
            "git",
            &["rev-parse", &format!("refs/heads/{branch}")],
        )
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn dirty(rig: &Rig) -> Options {
    rig.write("b.txt", "new\n");
    let mut o = rig.opts();
    o.stage = Stage::All;
    o.message = Some("feat(x): add b".into());
    o
}

#[test]
fn a_new_branch_is_committed_pushed_and_opened_as_a_draft_pr() {
    let rig = Rig::new();
    let opts = dirty(&rig);

    let report = deliver::run(&opts).expect("delivered");

    assert_eq!(report.pr_number, Some(7));
    assert_eq!(report.draft, Some(true));
    assert!(rig.gh_log().contains("--draft"));
    assert_eq!(rig.remote_head("feat/x"), rig.git(&["rev-parse", "HEAD"]));
    assert!(rig
        .gh_log()
        .contains("pr create --head feat/x --base main --title feat(x): add b"));
    assert!(rig
        .git(&["log", "-1", "--format=%B"])
        .contains("Signed-off-by:"));
}

#[test]
fn ready_creates_a_non_draft_pr() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.ready = Some(true);

    let report = deliver::run(&opts).expect("delivered");

    assert_eq!(report.draft, Some(false));
    assert!(!rig.gh_log().contains("--draft"));
}

#[test]
fn the_default_branch_is_refused() {
    let rig = Rig::new();
    rig.git(&["checkout", "-q", "main"]);
    let mut opts = dirty(&rig);
    opts.protected = Vec::new();

    let err = deliver::run(&opts).expect_err("refused");

    assert_eq!(err.code, exit::REFUSED);
    assert!(err.message.contains("main"), "{}", err.message);
}

#[test]
fn a_detached_head_is_refused() {
    let rig = Rig::new();
    rig.git(&["checkout", "-q", "--detach"]);

    let err = deliver::run(&rig.opts()).expect_err("refused");

    assert!(err.message.contains("detached"), "got {}", err.message);
}

#[test]
fn a_rebased_branch_with_nothing_staged_is_pushed_with_a_lease() {
    let rig = Rig::new();
    deliver::run(&dirty(&rig)).expect("first delivery");
    let old_remote = rig.remote_head("feat/x");
    rig.git(&["checkout", "-q", "main"]);
    rig.write("c.txt", "main moved\n");
    rig.git(&["add", "-A"]);
    rig.git(&["commit", "-q", "-m", "chore: main moves"]);
    rig.git(&["push", "-q", "origin", "main"]);
    rig.git(&["checkout", "-q", "feat/x"]);

    let report = deliver::run(&rig.opts()).expect("push only");

    assert!(report.rebased && report.pushed);
    assert_ne!(rig.remote_head("feat/x"), old_remote);
    assert_eq!(rig.remote_head("feat/x"), rig.git(&["rev-parse", "HEAD"]));
}

#[test]
fn a_diverged_remote_without_our_rebase_is_refused() {
    let rig = Rig::new();
    deliver::run(&dirty(&rig)).expect("first delivery");
    rig.git(&["commit", "-q", "--amend", "-m", "feat(x): reworded"]);

    let err = deliver::run(&rig.opts()).expect_err("refused");

    assert_eq!(err.code, exit::REFUSED);
    assert!(err
        .message
        .contains("only force-pushes after its own rebase"));
}

#[test]
fn a_rebase_conflict_reports_files_and_leaves_the_tree_clean() {
    let rig = Rig::new();
    rig.write("a.txt", "mine\n");
    rig.git(&["commit", "-q", "-am", "feat: mine"]);
    rig.git(&["checkout", "-q", "main"]);
    rig.write("a.txt", "theirs\n");
    rig.git(&["commit", "-q", "-am", "fix: theirs"]);
    rig.git(&["push", "-q", "origin", "main"]);
    rig.git(&["checkout", "-q", "feat/x"]);

    let err = deliver::run(&rig.opts()).expect_err("conflict");

    assert_eq!(err.code, exit::CONFLICT);
    assert_eq!(err.details, vec!["a.txt".to_string()]);
    assert!(rig.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn a_long_non_conventional_title_is_refused_before_anything_changes() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.title = Some(format!("Add {}", "x".repeat(80)));

    let err = deliver::run(&opts).expect_err("refused");

    assert_eq!(err.code, exit::REFUSED);
    assert_eq!(err.details.len(), 2, "got {:?}", err.details);
    assert!(rig.git(&["status", "--porcelain"]).contains("b.txt"));
}

#[test]
fn an_em_dash_in_the_commit_message_is_refused() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.message = Some("feat: add b \u{2014} quickly".into());

    let err = deliver::run(&opts).expect_err("refused");

    assert!(
        err.details.iter().any(|d| d.contains("em dash")),
        "got {:?}",
        err.details
    );
}

#[test]
fn ai_attribution_in_the_pr_body_is_refused() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.body = Some("Summary\n\nGenerated with Claude Code".into());

    let err = deliver::run(&opts).expect_err("refused");

    assert!(
        err.details.iter().any(|d| d.contains("AI attribution")),
        "got {:?}",
        err.details
    );
}

#[test]
fn staged_content_without_a_message_is_refused() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.message = None;

    let err = deliver::run(&opts).expect_err("refused");

    assert!(err.message.contains("no commit message"));
}

#[test]
fn a_commit_is_signed_off_and_signed_only_when_signing_is_configured() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.dry_run = true;
    let plain = deliver::run(&opts).expect("plan");
    rig.git(&["config", "gpg.format", "ssh"]);
    rig.git(&["config", "user.signingkey", "/nonexistent.pub"]);

    let configured = deliver::run(&opts).expect("plan");

    assert!(plain.actions.iter().any(|a| a.contains("commit with -s (")));
    assert!(configured.actions.iter().any(|a| a.contains("-s -S")));
}

#[test]
fn a_stacked_branch_targets_its_parent_branch() {
    let rig = Rig::new();
    deliver::run(&dirty(&rig)).expect("parent");
    rig.git(&["checkout", "-q", "-b", "feat/y"]);
    rig.write("d.txt", "child\n");
    fs::remove_file(rig.fake.join("title")).unwrap();
    rig.fake_file(
        "heads",
        r#"[{"headRefName":"feat/x","baseRefName":"main"}]"#,
    );
    let mut opts = rig.opts();
    opts.stage = Stage::All;
    opts.message = Some("feat(y): add d".into());

    let report = deliver::run(&opts).expect("child");

    assert_eq!(report.base, "feat/x");
    assert!(rig.gh_log().contains("--base feat/x"));
}

#[test]
fn an_existing_pr_is_left_alone_unless_a_title_is_passed() {
    let rig = Rig::new();
    deliver::run(&dirty(&rig)).expect("create");
    rig.write("e.txt", "more\n");
    let mut opts = rig.opts();
    opts.stage = Stage::All;
    opts.message = Some("feat(x): add e".into());

    deliver::run(&opts).expect("second commit");
    let untouched = rig.gh_log().contains("pr edit");
    rig.write("f.txt", "more\n");
    opts.title = Some("feat(x): add b and more".into());
    deliver::run(&opts).expect("with title");

    assert!(!untouched);
    assert!(rig
        .gh_log()
        .contains("pr edit 7 --title feat(x): add b and more"));
}

#[test]
fn auto_merge_reports_queued_and_fails_loudly_when_it_is_not() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.ready = Some(true);
    opts.auto_merge = true;
    let report = deliver::run(&opts).expect("queued");
    rig.git(&["checkout", "-q", "main"]);
    rig.git(&["checkout", "-q", "-b", "feat/z"]);
    rig.write("z.txt", "z\n");
    rig.fake_file("noqueue", "");
    rig.fake_file("heads", "[]");
    fs::remove_file(rig.fake.join("title")).unwrap();
    fs::remove_file(rig.fake.join("queued")).unwrap();
    let mut second = rig.opts();
    second.stage = Stage::All;
    second.message = Some("feat(z): add z".into());
    second.ready = Some(true);
    second.auto_merge = true;

    let not_queued = deliver::run(&second).expect("report");

    assert!(report.auto_merge_queued);
    assert_eq!(not_queued.code, exit::NOT_QUEUED);
}

#[test]
fn auto_merge_on_a_draft_is_refused() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.auto_merge = true;

    let err = deliver::run(&opts).expect_err("refused");

    assert_eq!(err.code, exit::REFUSED);
    assert!(!rig.gh_log().contains("pr create"));
    assert!(!rig.gh_log().contains("pr merge"));
}

#[test]
fn wait_ends_merged_with_success() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.wait = Some(Duration::from_secs(5));
    rig.fake_file("state", "MERGED");

    let report = deliver::run(&opts).expect("report");

    assert_eq!((report.outcome.as_str(), report.code), ("merged", exit::OK));
}

#[test]
fn wait_names_the_failing_required_check() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.wait = Some(Duration::from_secs(5));
    rig.fake_file(
        "checks",
        r#"[{"name":"unit","bucket":"fail"},{"name":"lint","bucket":"pass"}]"#,
    );

    let report = deliver::run(&opts).expect("report");

    assert_eq!(report.code, exit::CHECKS);
    assert!(report.to_text().contains("Failing check: unit"));
}

#[test]
fn wait_times_out_while_checks_stay_pending() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.wait = Some(Duration::from_millis(20));
    rig.fake_file("checks", r#"[{"name":"unit","bucket":"pending"}]"#);

    let report = deliver::run(&opts).expect("report");

    assert_eq!(report.code, exit::TIMEOUT);
}

#[test]
fn dry_run_changes_nothing() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.dry_run = true;
    let head = rig.git(&["rev-parse", "HEAD"]);

    let report = deliver::run(&opts).expect("plan");

    assert!(report.dry_run && !report.actions.is_empty());
    assert_eq!(rig.git(&["rev-parse", "HEAD"]), head);
    assert!(rig.git(&["status", "--porcelain"]).contains("?? b.txt"));
    assert!(!rig.gh_log().contains("pr create"));
    assert!(sh(
        &rig.origin,
        &rig.env(),
        "git",
        &["branch", "--list", "feat/x"]
    )
    .is_empty());
}

#[test]
fn running_twice_leaves_one_pr_and_the_second_run_has_nothing_to_do() {
    let rig = Rig::new();
    deliver::run(&dirty(&rig)).expect("first");

    let err = deliver::run(&rig.opts()).expect_err("second");

    assert_eq!(err.code, exit::NOTHING);
    assert_eq!(rig.gh_log().matches("pr create").count(), 1);
}

#[test]
fn a_branch_with_no_commits_beyond_the_base_has_nothing_to_do() {
    let rig = Rig::new();

    let err = deliver::run(&rig.opts()).expect_err("nothing");

    assert_eq!(err.code, exit::NOTHING);
}

#[test]
fn a_large_diff_is_delivered_without_an_override() {
    let rig = Rig::new();
    let opts = dirty(&rig);
    rig.write("big.txt", &"line\n".repeat(LARGE_DIFF_LINES));

    let delivered = deliver::run(&opts);

    assert!(delivered.is_ok());
}

#[test]
fn closing_lines_in_commits_survive_into_the_pr_body() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.message = Some("feat(x): add b\n\nCloses #41\nTicket: ABC-9".into());

    deliver::run(&opts).expect("delivered");

    let body = fs::read_to_string(rig.fake.join("body")).unwrap();
    assert!(
        body.contains("Closes #41") && body.contains("Ticket: ABC-9"),
        "got {body}"
    );
}

#[test]
fn a_remote_commit_we_never_had_is_not_overwritten_after_a_rebase() {
    let rig = Rig::new();
    deliver::run(&dirty(&rig)).expect("first delivery");
    let other = rig.root.join("other");
    let env = rig.env();
    let origin = rig.origin.to_str().unwrap();
    sh(&rig.root, &env, "git", &["clone", "-q", origin, "other"]);
    sh(&other, &env, "git", &["checkout", "-q", "feat/x"]);
    fs::write(other.join("theirs.txt"), "x\n").unwrap();
    sh(&other, &env, "git", &["add", "-A"]);
    sh(
        &other,
        &env,
        "git",
        &["commit", "-q", "-m", "feat(x): theirs"],
    );
    sh(&other, &env, "git", &["push", "-q", "origin", "feat/x"]);
    let theirs = rig.remote_head("feat/x");
    rig.git(&["checkout", "-q", "main"]);
    rig.write("c.txt", "main moved\n");
    rig.git(&["add", "-A"]);
    rig.git(&["commit", "-q", "-m", "chore: main moves"]);
    rig.git(&["push", "-q", "origin", "main"]);
    rig.git(&["checkout", "-q", "feat/x"]);

    let err = deliver::run(&rig.opts()).expect_err("refused");

    assert_eq!(err.code, exit::REFUSED);
    assert_eq!(rig.remote_head("feat/x"), theirs);
}

#[test]
fn the_default_branch_is_found_from_the_remote_when_origin_head_is_missing() {
    let rig = Rig::new();
    rig.git(&["push", "-q", "origin", "main:dev"]);
    sh(
        &rig.origin,
        &rig.env(),
        "git",
        &["symbolic-ref", "HEAD", "refs/heads/dev"],
    );
    rig.git(&["fetch", "-q", "origin"]);
    rig.git(&["remote", "set-head", "origin", "-d"]);
    rig.git(&["checkout", "-q", "-B", "dev", "origin/dev"]);
    let mut opts = dirty(&rig);
    opts.protected = Vec::new();
    let head = Command::new("git")
        .args(["symbolic-ref", "-q", "refs/remotes/origin/HEAD"])
        .current_dir(&rig.work)
        .envs(rig.env())
        .output()
        .unwrap();
    assert!(!head.status.success(), "origin/HEAD must be missing");

    let err = deliver::run(&opts).expect_err("refused");

    assert_eq!(err.code, exit::REFUSED);
    assert!(err.message.contains("dev"), "{}", err.message);
}

#[test]
fn auto_merge_on_a_pr_that_already_merged_reports_merged() {
    let rig = Rig::new();
    rig.fake_file("state", "MERGED");
    let mut opts = dirty(&rig);
    opts.ready = Some(true);
    opts.auto_merge = true;

    let report = deliver::run(&opts).expect("delivered");

    assert_eq!(report.outcome, "merged");
    assert_eq!(report.code, exit::OK);
}

#[test]
fn an_earlier_commit_with_ai_attribution_is_refused_before_the_push() {
    let rig = Rig::new();
    rig.write("b.txt", "new\n");
    rig.git(&["add", "-A"]);
    let trailer = format!("Co-Authored-By: {} <noreply@{}.com>", "Claude", "anthropic");
    rig.git(&[
        "commit",
        "-q",
        "-m",
        &format!("feat(x): add b\n\n{trailer}"),
    ]);

    let err = deliver::run(&rig.opts()).expect_err("refused");

    assert_eq!(err.code, exit::REFUSED);
    assert!(!rig.gh_log().contains("pr create"));
    assert!(rig.git(&["ls-remote", "origin", "feat/x"]).is_empty());
}

fn with_rules(mut opts: Options) -> Options {
    opts.rules = Some(playbook::pr::body_rules::Rules::default());
    opts
}

fn pushed(rig: &Rig, branch: &str) -> bool {
    !rig.git(&["ls-remote", "origin", &format!("refs/heads/{branch}")])
        .is_empty()
}

#[test]
fn a_body_that_breaks_the_template_is_refused_before_anything_is_pushed() {
    let rig = Rig::new();
    let mut opts = with_rules(dirty(&rig));
    opts.body = Some("## Test plan\n\n- run it\n".into());

    let err = deliver::run(&opts).expect_err("refused");

    assert!(
        err.details.iter().any(|d| d.contains("## Summary")),
        "got {:?}",
        err.details
    );
    assert!(!pushed(&rig, "feat/x"), "nothing is pushed");
    assert!(
        rig.git(&["status", "--porcelain"]).contains("b.txt"),
        "nothing is committed"
    );
}

#[test]
fn the_composed_body_leads_with_the_ticket_and_names_the_issue() {
    let rig = Rig::new();
    rig.git(&["checkout", "-q", "-b", "feat/PROJ-9-b"]);
    let mut opts = with_rules(dirty(&rig));
    opts.message = Some("feat(PROJ-9): add b".into());
    opts.issue = Some("12".into());

    deliver::run(&opts).expect("delivered");

    let body = fs::read_to_string(rig.fake.join("body")).expect("body");
    assert!(body.starts_with("PROJ-9\n\n## Summary\n"), "got {body}");
    assert!(body.contains("Refs #12"), "got {body}");
}

#[test]
fn a_commit_title_without_the_key_is_refused_before_committing_on_a_stacked_branch() {
    let rig = Rig::new();
    rig.git(&["push", "-q", "origin", "main:refs/heads/feat/parent"]);
    rig.git(&["checkout", "-q", "-b", "feat/PROJ-9-b"]);
    let mut opts = with_rules(dirty(&rig));
    opts.base = Some("feat/parent".into());
    opts.message = Some("feat(x): add b".into());

    let err = deliver::run(&opts).expect_err("refused");

    assert!(
        err.details.iter().any(|d| d.contains("PROJ-9")),
        "got {:?}",
        err.details
    );
    assert!(
        rig.git(&["status", "--porcelain"]).contains("b.txt"),
        "nothing is committed"
    );
}

#[test]
fn a_title_only_update_of_an_open_pr_keeps_the_scope_rule_on_a_stacked_branch() {
    let rig = Rig::new();
    rig.git(&["checkout", "-q", "-b", "feat/PROJ-9-b"]);
    let mut first = with_rules(dirty(&rig));
    first.message = Some("feat(PROJ-9): add b".into());
    first.base = Some("main".into());
    deliver::run(&first).expect("opened");
    rig.git(&["push", "-q", "origin", "main:refs/heads/feat/parent"]);
    fs::write(rig.fake.join("base"), "feat/parent\n").unwrap();
    let mut update = with_rules(rig.opts());
    update.title = Some("feat(sync): rename".into());

    let err = deliver::run(&update).expect_err("refused");

    assert!(
        err.details
            .iter()
            .any(|d| d.contains("PR title must carry")),
        "got {:?}",
        err.details
    );
}

#[test]
fn a_dry_run_reports_the_same_refusal_as_a_real_update() {
    let rig = Rig::new();
    deliver::run(&dirty(&rig)).expect("opened");
    let mut update = with_rules(rig.opts());
    update.body = Some("no sections here\n".into());
    update.dry_run = true;

    let err = deliver::run(&update).expect_err("refused");

    assert!(
        err.details.iter().any(|d| d.contains("## Summary")),
        "got {:?}",
        err.details
    );
}

#[test]
fn an_unreadable_issue_is_refused() {
    let rig = Rig::new();
    let mut opts = with_rules(dirty(&rig));
    opts.issue = Some("PROJ-12".into());

    let err = deliver::run(&opts).expect_err("refused");

    assert!(
        err.details
            .iter()
            .any(|d| d.contains("--issue must be a number")),
        "got {:?}",
        err.details
    );
}

#[test]
fn parity_message_prose_a_curly_quote_in_the_message_is_refused() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.message = Some("feat(x): it\u{2019}s b".into());

    let err = deliver::run(&opts).expect_err("refused");

    assert_eq!(err.code, exit::REFUSED);
    assert!(
        err.details.iter().any(|d| d.contains("curly quote")),
        "got {:?}",
        err.details
    );
    assert!(!pushed(&rig, "feat/x"));
}

#[test]
fn parity_commit_type_a_docs_only_diff_cannot_be_a_feat() {
    let rig = Rig::new();
    rig.write("README.md", "docs\n");
    let mut opts = rig.opts();
    opts.stage = Stage::All;
    opts.message = Some("feat: x".into());

    let err = deliver::run(&opts).expect_err("refused");

    assert_eq!(err.code, exit::REFUSED);
    assert!(
        err.details.iter().any(|d| d.contains("commit type check")),
        "got {:?}",
        err.details
    );
    assert!(!pushed(&rig, "feat/x"));
}

fn trailers(rig: &Rig) -> usize {
    rig.git(&["log", "-1", "--format=%B"])
        .lines()
        .filter(|l| l.starts_with("Signed-off-by:"))
        .count()
}

#[test]
fn parity_signoff_follows_the_setting_the_default_and_a_writing_hook() {
    let off = Rig::new();
    let mut opts = dirty(&off);
    opts.signoff = false;
    deliver::run(&opts).expect("delivered");
    assert_eq!(trailers(&off), 0);

    let on = Rig::new();
    deliver::run(&dirty(&on)).expect("delivered");
    assert_eq!(trailers(&on), 1);

    let hooked = Rig::new();
    let hook = hooked.work.join(".git/hooks/commit-msg");
    fs::write(
        &hook,
        "#!/bin/sh\ngit interpret-trailers --in-place --trailer 'Signed-off-by: t <t@t.test>' \"$1\"\n",
    )
    .unwrap();
    sh(&hooked.work, &[], "chmod", &["+x", hook.to_str().unwrap()]);
    deliver::run(&dirty(&hooked)).expect("delivered");
    assert_eq!(trailers(&hooked), 1);
}

#[test]
fn parity_signing_an_unset_key_adds_no_sign_flag() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.dry_run = true;

    let report = deliver::run(&opts).expect("plan");

    assert!(report
        .actions
        .iter()
        .any(|a| a.contains("commit with -s (")));
    assert!(!report.actions.iter().any(|a| a.contains(" -S")));
}

#[test]
fn parity_merge_commit_a_merge_on_the_branch_is_refused() {
    let rig = Rig::new();
    rig.git(&["checkout", "-q", "-b", "side", "main"]);
    rig.write("s.txt", "side\n");
    rig.git(&["add", "-A"]);
    rig.git(&["commit", "-q", "-m", "feat: side"]);
    rig.git(&["checkout", "-q", "feat/x"]);
    rig.write("f.txt", "feat\n");
    rig.git(&["add", "-A"]);
    rig.git(&["commit", "-q", "-m", "feat: own"]);
    rig.git(&["merge", "-q", "--no-ff", "-m", "chore: merge side", "side"]);

    let mut opts = rig.opts();
    opts.title = Some("feat: merge test".into());

    let err = deliver::run(&opts).expect_err("refused");

    assert_eq!(err.code, exit::REFUSED);
    assert!(err.message.contains("merge"), "{}", err.message);
    assert!(!pushed(&rig, "feat/x"));
}

fn moved_main(rig: &Rig) {
    rig.git(&["checkout", "-q", "main"]);
    rig.write("c.txt", "main moved\n");
    rig.git(&["add", "-A"]);
    rig.git(&["commit", "-q", "-m", "chore: main moves"]);
    rig.git(&["push", "-q", "origin", "main"]);
    rig.git(&["checkout", "-q", "feat/x"]);
}

#[test]
fn parity_no_force_auto_mode_parks_a_pushed_branch_that_needs_a_rebase() {
    let rig = Rig::new();
    deliver::run(&dirty(&rig)).expect("first delivery");
    let remote = rig.remote_head("feat/x");
    moved_main(&rig);
    let mut opts = rig.opts();
    opts.auto = true;

    let err = deliver::run(&opts).expect_err("parked");

    assert_eq!(err.code, exit::REFUSED);
    assert!(err.message.starts_with("PARKED"), "{}", err.message);
    assert_eq!(rig.remote_head("feat/x"), remote);
}

#[test]
fn parity_no_force_auto_mode_pushes_plainly_when_the_remote_branch_is_absent() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.auto = true;

    let report = deliver::run(&opts).expect("delivered");

    assert!(report.pushed);
    assert!(!report.actions.iter().any(|a| a.contains("force")));
}

#[test]
fn parity_no_force_ask_mode_pushes_with_a_lease_on_the_remote_sha() {
    let rig = Rig::new();
    deliver::run(&dirty(&rig)).expect("first delivery");
    let remote = rig.remote_head("feat/x");
    moved_main(&rig);

    let report = deliver::run(&rig.opts()).expect("lease push");

    assert!(report
        .actions
        .iter()
        .any(|a| a == "push with --force-with-lease"));
    assert_ne!(rig.remote_head("feat/x"), remote);
}

#[test]
fn parity_no_force_a_remote_that_moved_during_the_lease_is_exit_5_never_a_force() {
    let rig = Rig::new();
    deliver::run(&dirty(&rig)).expect("first delivery");
    moved_main(&rig);
    // A pre-push hook that moves the remote branch after deliver read it.
    let other = rig.root.join("other");
    sh(
        &rig.root,
        &rig.env(),
        "git",
        &[
            "clone",
            "-q",
            rig.origin.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    sh(&other, &rig.env(), "git", &["checkout", "-q", "feat/x"]);
    fs::write(other.join("z.txt"), "z\n").unwrap();
    sh(&other, &rig.env(), "git", &["add", "-A"]);
    sh(
        &other,
        &rig.env(),
        "git",
        &["commit", "-q", "-m", "feat: other"],
    );
    let hook = rig.work.join(".git/hooks/pre-push");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\ncd {} && git push -q origin feat/x\n",
            other.display()
        ),
    )
    .unwrap();
    sh(&rig.work, &[], "chmod", &["+x", hook.to_str().unwrap()]);
    let theirs = sh(&other, &rig.env(), "git", &["rev-parse", "HEAD"]);

    let err = deliver::run(&rig.opts()).expect_err("lease fails");

    assert_eq!(err.code, exit::FAILED);
    assert_eq!(rig.remote_head("feat/x"), theirs);
}

#[test]
fn a_title_a_body_and_a_missing_ticket_are_all_listed_at_once() {
    let rig = Rig::new();
    rig.git(&["push", "-q", "origin", "main:refs/heads/feat/parent"]);
    rig.git(&["checkout", "-q", "-b", "feat/PROJ-9-b"]);
    let mut opts = with_rules(dirty(&rig));
    opts.base = Some("feat/parent".into());
    opts.message = Some("feat(PROJ-9): add b".into());
    opts.title = Some(format!("feat: {}", "x".repeat(75)));
    opts.body = Some("## Test plan\n\n- run it\n".into());

    let err = deliver::run(&opts).expect_err("refused");

    assert_eq!(err.code, exit::REFUSED);
    let json = deliver::failure_json(&err);
    let rules: Vec<&str> = json["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["rule"].as_str().unwrap())
        .collect();
    assert!(rules.contains(&"title-length"), "{rules:?}");
    assert!(rules.contains(&"template"), "{rules:?}");
    assert!(rules.len() >= 3, "{rules:?}");
    assert!(!rig.gh_log().contains("pr create"));
    assert!(!pushed(&rig, "feat/PROJ-9-b"));
}

#[test]
fn parity_staged_refusal_a_staged_file_without_a_message_has_the_staged_rule() {
    let rig = Rig::new();
    rig.write("b.txt", "new\n");
    rig.git(&["add", "b.txt"]);

    let err = deliver::run(&rig.opts()).expect_err("refused");

    assert_eq!(err.code, exit::REFUSED);
    let json = deliver::failure_json(&err);
    assert_eq!(json["problems"][0]["rule"], "staged");
    assert_eq!(json["verdict"], "REFUSED");
}

#[test]
fn a_rule_set_to_warn_delivers_and_reports_the_warning() {
    let rig = Rig::new();
    let mut opts = with_rules(dirty(&rig));
    opts.body = Some(format!("## Summary\n\n{}\n", "word ".repeat(300)));

    let report = deliver::run(&opts).expect("delivered");

    assert!(!report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(report.to_json().contains("\"verdict\": \"DELIVERED\""));
}

#[test]
fn parity_pr_draft_the_setting_decides_when_no_flag_is_passed() {
    let rig = Rig::new();
    let mut opts = dirty(&rig);
    opts.default_draft = false;

    let report = deliver::run(&opts).expect("delivered");

    assert_eq!(report.draft, Some(false));
}

#[test]
fn a_pr_create_that_failed_after_the_push_is_finished_by_a_second_run() {
    let rig = Rig::new();
    rig.fake_file("fail_create", "1");

    let first = deliver::run(&dirty(&rig)).expect_err("gh pr create fails");
    assert_eq!(first.code, exit::FAILED);
    let remote = rig.remote_head("feat/x");
    fs::remove_file(rig.fake.join("fail_create")).unwrap();

    let second = deliver::run(&rig.opts()).expect("second run opens the PR");

    assert_eq!(second.pr_number, Some(7));
    assert_eq!(rig.remote_head("feat/x"), remote, "no force, same head");
    assert!(!rig.gh_log().contains("--force"));
}

#[test]
fn classify_maps_each_real_message_to_its_rule_and_place() {
    for (message, rule, place) in [
        ("PR title is 81 characters, the limit is 72", "title-length", "title"),
        ("PR title is not a conventional commit title (type(scope): description)", "title-format", "title"),
        ("commit title is not a conventional commit title (type(scope): description)", "title-format", "commit"),
        ("commit message is empty", "title-format", "commit"),
        ("commit message line 3 has an em dash", "dash", "commit"),
        ("PR body line 2 has an en dash", "dash", "body"),
        ("PR body line 2 carries AI attribution (trailer)", "attribution", "body"),
        ("PR body has text before ## Summary; only the ticket line may come first", "template", "body"),
        ("PR body must start its sections with ## Summary (fix it)", "template", "body"),
        ("PR body is 1300 characters, the cap is 1200; cut it", "body-length", "body"),
        ("the first line of the PR body must carry the ticket PROJ-1, for example: Ticket: PROJ-1", "ticket-line", "body"),
        ("commit title `handle the merge commit case` must carry the ticket in its scope, for example `feat(PROJ-1): x`", "ticket-scope", "commit"),
        ("PR title must carry the ticket in its scope, for example `feat(PROJ-1): x`", "ticket-scope", "title"),
        ("the PR body must name issue #4: add `Closes #4`", "issue", "body"),
        ("something is staged but no commit message was given (rule staged)", "staged", "body"),
    ] {
        assert_eq!(deliver::classify(message), (rule, place), "{message}");
    }
}
