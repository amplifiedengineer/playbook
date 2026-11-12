// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook commit prepare` and `commit run` against real git repos with a
//! local bare `origin`. They replace the two bash blocks of
//! `commands/commit.md`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

struct Repo {
    root: PathBuf,
    work: PathBuf,
    origin: PathBuf,
}

fn git_in(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = git_in(dir, args);
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

impl Repo {
    /// `main` pushed to a bare origin, and a local branch `feat` checked out.
    fn new(tag: &str) -> Repo {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "pb-commit-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&root);
        let origin = root.join("origin.git");
        let work = root.join("work");
        fs::create_dir_all(&origin).unwrap();
        fs::create_dir_all(&work).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        git(&work, &["init", "-q", "-b", "main"]);
        git(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        fs::write(work.join("a.txt"), "a\n").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "init"]);
        git(&work, &["push", "-q", "-u", "origin", "main"]);
        git(&work, &["checkout", "-q", "-b", "feat"]);
        Repo { root, work, origin }
    }

    fn pb(&self, args: &[&str], stdin: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .current_dir(&self.work)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn stage(&self, file: &str, body: &str) {
        fs::write(self.work.join(file), body).unwrap();
        git(&self.work, &["add", file]);
    }

    fn remote_head(&self, branch: &str) -> String {
        git(
            &self.origin,
            &["rev-parse", &format!("refs/heads/{branch}")],
        )
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

// prepare

#[test]
fn prepare_refuses_the_default_branch() {
    let r = Repo::new("prot");
    git(&r.work, &["checkout", "-q", "main"]);
    r.stage("b.txt", "b\n");
    let o = r.pb(&["commit", "prepare"], "");
    assert!(!o.status.success());
    assert!(err(&o).contains("HEAD is on 'main'"), "{}", err(&o));
}

#[test]
fn prepare_with_nothing_staged_says_so_and_exits_zero() {
    let r = Repo::new("none");
    let o = r.pb(&["commit", "prepare"], "");
    assert!(o.status.success());
    assert_eq!(out(&o).trim(), "NO_STAGED_CHANGES");
}

#[test]
fn prepare_prints_the_branch_the_files_and_the_diff() {
    let r = Repo::new("ctx");
    r.stage("b.txt", "hello\n");
    let t = out(&r.pb(&["commit", "prepare"], ""));
    assert!(t.starts_with("BRANCH=feat"), "{t}");
    assert!(
        t.contains("A\tb.txt") && t.contains("---DIFF_START---") && t.contains("+hello"),
        "{t}"
    );
}

#[test]
fn prepare_stages_with_the_flags() {
    let r = Repo::new("flags");
    fs::write(r.work.join("a.txt"), "changed\n").unwrap();
    fs::write(r.work.join("new.txt"), "new\n").unwrap();
    let t = out(&r.pb(&["commit", "prepare", "-u"], ""));
    assert!(t.contains("M\ta.txt") && !t.contains("new.txt"), "{t}");
    let t = out(&r.pb(&["commit", "prepare", "-A"], ""));
    assert!(t.contains("new.txt"), "{t}");
}

#[test]
fn prepare_amend_shows_the_last_commit() {
    let r = Repo::new("amend");
    let t = out(&r.pb(&["commit", "prepare", "-a"], ""));
    assert!(t.contains("AMENDING: ") && t.contains("init"), "{t}");
}

// run

#[test]
fn run_commits_with_a_signoff_and_pushes() {
    let r = Repo::new("run");
    r.stage("b.txt", "b\n");
    let o = r.pb(&["commit", "run"], "feat: add b\n");
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert!(
        out(&o).contains("Pushed:") && out(&o).contains("-> origin/feat"),
        "{}",
        out(&o)
    );
    let body = git(&r.work, &["log", "-1", "--format=%B"]);
    assert!(body.contains("Signed-off-by: t <t@t>"), "{body}");
    assert_eq!(r.remote_head("feat"), git(&r.work, &["rev-parse", "HEAD"]));
}

#[test]
fn run_leaves_the_signoff_off_when_asked_or_configured() {
    let r = Repo::new("nosign");
    r.stage("b.txt", "b\n");
    assert!(r
        .pb(&["commit", "run", "--no-signoff"], "feat: b\n")
        .status
        .success());
    assert!(!git(&r.work, &["log", "-1", "--format=%B"]).contains("Signed-off-by"));

    r.stage("c.txt", "c\n");
    assert!(r
        .pb(
            &["config", "set", "--global", "commit.signOff", "false"],
            ""
        )
        .status
        .success());
    assert!(r.pb(&["commit", "run"], "feat: c\n").status.success());
    assert!(!git(&r.work, &["log", "-1", "--format=%B"]).contains("Signed-off-by"));
}

#[test]
fn run_keeps_an_existing_trailer_without_doubling_it() {
    let r = Repo::new("trailer");
    r.stage("b.txt", "b\n");
    assert!(r
        .pb(
            &["commit", "run"],
            "feat: b\n\nSigned-off-by: Someone <s@s>\n"
        )
        .status
        .success());
    let body = git(&r.work, &["log", "-1", "--format=%B"]);
    assert_eq!(body.matches("Signed-off-by").count(), 1, "{body}");
}

#[test]
fn run_stops_when_the_commit_fails_and_pushes_nothing() {
    let r = Repo::new("hookfail");
    let hook = r.work.join(".git/hooks/commit-msg");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    r.stage("b.txt", "b\n");
    let o = r.pb(&["commit", "run"], "feat: b\n");
    assert!(!o.status.success());
    assert!(err(&o).contains("git commit failed"), "{}", err(&o));
    assert!(
        git_in(&r.origin, &["rev-parse", "refs/heads/feat"])
            .status
            .code()
            != Some(0)
    );
}

#[test]
fn run_rebases_when_behind_and_pushes_with_a_lease() {
    let r = Repo::new("rebase");
    // First push of feat, then main moves on.
    r.stage("b.txt", "b\n");
    assert!(r.pb(&["commit", "run"], "feat: b\n").status.success());
    let other = r.root.join("other");
    git(
        &r.root,
        &["clone", "-q", r.origin.to_str().unwrap(), "other"],
    );
    fs::write(other.join("m.txt"), "m\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "main moves"]);
    git(&other, &["push", "-q", "origin", "main"]);
    r.stage("c.txt", "c\n");
    let o = r.pb(&["commit", "run"], "feat: c\n");
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert!(
        out(&o).contains("commits behind origin/main. Rebasing"),
        "{}",
        out(&o)
    );
    assert_eq!(r.remote_head("feat"), git(&r.work, &["rev-parse", "HEAD"]));
    assert!(git(&r.work, &["log", "--format=%s"]).contains("main moves"));
}

#[test]
fn run_refuses_a_branch_with_a_merge_commit() {
    let r = Repo::new("merge");
    git(&r.work, &["checkout", "-q", "-b", "side"]);
    fs::write(r.work.join("s.txt"), "s\n").unwrap();
    git(&r.work, &["add", "."]);
    git(&r.work, &["commit", "-q", "-m", "side"]);
    git(&r.work, &["checkout", "-q", "feat"]);
    fs::write(r.work.join("f.txt"), "f\n").unwrap();
    git(&r.work, &["add", "."]);
    git(&r.work, &["commit", "-q", "-m", "feat work"]);
    git(
        &r.work,
        &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    );
    r.stage("c.txt", "c\n");
    let o = r.pb(&["commit", "run"], "feat: c\n");
    assert!(!o.status.success());
    assert!(
        err(&o).contains("merge commit(s) on this branch"),
        "{}",
        err(&o)
    );
}

#[test]
fn auto_mode_pushes_a_new_branch_plainly_and_parks_an_amend() {
    let r = Repo::new("auto");
    r.stage("b.txt", "b\n");
    let o = r.pb(&["commit", "run", "--auto"], "feat: b\n");
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    // The branch now exists on the remote, so an amend would need a force.
    r.stage("c.txt", "c\n");
    let o = r.pb(&["commit", "run", "--auto", "-a"], "feat: b again\n");
    assert!(!o.status.success());
    assert!(err(&o).contains("PARKED"), "{}", err(&o));
}

#[test]
fn a_rejected_plain_push_is_never_forced() {
    let r = Repo::new("reject");
    r.stage("b.txt", "b\n");
    assert!(r.pb(&["commit", "run"], "feat: b\n").status.success());
    // Someone else moves the remote branch.
    let other = r.root.join("other");
    git(
        &r.root,
        &[
            "clone",
            "-q",
            "-b",
            "feat",
            r.origin.to_str().unwrap(),
            "other",
        ],
    );
    fs::write(other.join("o.txt"), "o\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "other work"]);
    git(&other, &["push", "-q", "origin", "feat"]);
    let theirs = r.remote_head("feat");
    r.stage("c.txt", "c\n");
    let o = r.pb(&["commit", "run"], "feat: c\n");
    assert!(!o.status.success());
    assert!(err(&o).contains("never auto-escalates"), "{}", err(&o));
    assert_eq!(
        r.remote_head("feat"),
        theirs,
        "the remote must be untouched"
    );
}

#[test]
fn run_refuses_a_feat_type_on_a_docs_only_change_and_commits_nothing() {
    let r = Repo::new("typeguard");
    r.stage("guide.md", "words\n");
    let before = git(&r.work, &["rev-parse", "HEAD"]);
    let o = r.pb(&["commit", "run"], "feat(docs): explain things\n");
    assert!(!o.status.success());
    assert!(err(&o).contains("use 'docs'"), "{}", err(&o));
    assert!(err(&o).contains("Nothing was committed"), "{}", err(&o));
    assert_eq!(git(&r.work, &["rev-parse", "HEAD"]), before);
}

#[test]
fn run_accepts_the_right_type_and_the_override() {
    let r = Repo::new("typeguard-ok");
    r.stage("guide.md", "words\n");
    let o = r.pb(&["commit", "run"], "docs: explain things\n");
    assert!(o.status.success(), "{}{}", out(&o), err(&o));

    let r = Repo::new("typeguard-override");
    r.stage("guide.md", "words\n");
    let o = r.pb(
        &["commit", "run", "--no-type-check"],
        "feat(docs): a docs feature on purpose\n",
    );
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
}

#[test]
fn run_refuses_a_fix_type_on_a_lock_file_bump() {
    let r = Repo::new("typeguard-deps");
    r.stage("Cargo.lock", "# lock\n");
    let o = r.pb(&["commit", "run"], "fix(deps): bump serde\n");
    assert!(!o.status.success());
    assert!(err(&o).contains("build or chore"), "{}", err(&o));
}

#[test]
fn run_refuses_a_message_with_a_dash_and_commits_nothing() {
    let r = Repo::new("prose-dash");
    r.stage("code.rs", "fn main() {}\n");
    let before = git(&r.work, &["rev-parse", "HEAD"]);
    let o = r.pb(
        &["commit", "run"],
        "feat: add a thing \u{2014} fast\n\nIt's not slow, it's quick.\n",
    );
    assert!(!o.status.success());
    let e = err(&o);
    assert!(e.contains("line 1: an em or en dash"), "{e}");
    assert!(e.contains("line 3: contrast framing"), "{e}");
    assert!(e.contains("Nothing was committed"), "{e}");
    assert_eq!(git(&r.work, &["rev-parse", "HEAD"]), before);
}

#[test]
fn run_skips_the_prose_check_on_request() {
    let r = Repo::new("prose-skip");
    r.stage("code.rs", "fn main() {}\n");
    let o = r.pb(
        &["commit", "run", "--no-prose-check"],
        "feat: quote the \"utilize\" flag\n",
    );
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
}

#[test]
fn a_stacked_branch_without_the_key_in_the_scope_is_refused_with_the_fix() {
    let r = Repo::new("stack-block");
    git(&r.work, &["checkout", "-q", "-b", "stack-parent"]);
    r.stage("p.txt", "p\n");
    let first = r.pb(&["commit", "run"], "feat(PROJ-9): add p\n");
    assert!(first.status.success(), "{}{}", out(&first), err(&first));
    git(&r.work, &["checkout", "-q", "-b", "PROJ-9-child"]);
    r.stage("c.txt", "c\n");
    let before = git(&r.work, &["rev-parse", "HEAD"]);

    let o = r.pb(&["commit", "run"], "feat(sync): add c\n");

    assert!(!o.status.success());
    let e = err(&o);
    assert!(e.contains("`feat(PROJ-9): add c`"), "{e}");
    assert!(e.contains("Nothing was committed"), "{e}");
    assert_eq!(git(&r.work, &["rev-parse", "HEAD"]), before);
}

#[test]
fn a_stacked_branch_with_the_key_in_the_scope_commits() {
    let r = Repo::new("stack-ok");
    git(&r.work, &["checkout", "-q", "-b", "stack-parent"]);
    r.stage("p.txt", "p\n");
    r.pb(&["commit", "run"], "feat(PROJ-9): add p\n");
    git(&r.work, &["checkout", "-q", "-b", "PROJ-9-child"]);
    r.stage("c.txt", "c\n");

    let o = r.pb(&["commit", "run"], "feat(PROJ-9): add c\n");

    assert!(o.status.success(), "{}{}", out(&o), err(&o));
}

#[test]
fn a_branch_that_is_not_stacked_only_warns_about_a_missing_key() {
    let r = Repo::new("stack-warn");
    git(&r.work, &["checkout", "-q", "-b", "PROJ-9-solo"]);
    r.stage("c.txt", "c\n");

    let o = r.pb(&["commit", "run"], "feat(sync): add c\n");

    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert!(
        out(&o).contains("WARN: commit title `feat(sync): add c` must carry the ticket in its scope, for example `feat(PROJ-9): add c`"),
        "{}",
        out(&o)
    );
}

#[test]
fn no_known_ticket_means_no_ticket_rule_even_when_stacked() {
    let r = Repo::new("stack-none");
    git(&r.work, &["checkout", "-q", "-b", "stack-parent"]);
    r.stage("p.txt", "p\n");
    r.pb(&["commit", "run"], "feat(sync): add p\n");
    git(&r.work, &["checkout", "-q", "-b", "stack-child"]);
    r.stage("c.txt", "c\n");

    let o = r.pb(&["commit", "run"], "feat(sync): add c\n");

    assert!(o.status.success(), "{}{}", out(&o), err(&o));
}

#[test]
fn the_key_comes_from_the_commits_this_branch_added_and_not_from_the_parent() {
    let r = Repo::new("stack-own");
    git(&r.work, &["checkout", "-q", "-b", "stack-parent"]);
    r.stage("p.txt", "p\n");
    assert!(r
        .pb(&["commit", "run"], "feat(PROJ-8): add p\n")
        .status
        .success());
    git(&r.work, &["checkout", "-q", "-b", "stack-child"]);
    r.stage("c.txt", "c\n");

    let o = r.pb(&["commit", "run"], "feat(PROJ-9): add c\n");

    assert!(o.status.success(), "{}{}", out(&o), err(&o));
}

#[test]
fn a_key_found_in_the_branchs_own_commit_scopes_blocks_a_stacked_commit_without_it() {
    let r = Repo::new("stack-scopes");
    git(&r.work, &["checkout", "-q", "-b", "stack-parent"]);
    r.stage("p.txt", "p\n");
    assert!(r
        .pb(&["commit", "run"], "feat(PROJ-8): add p\n")
        .status
        .success());
    git(&r.work, &["checkout", "-q", "-b", "stack-child"]);
    r.stage("c.txt", "c\n");
    assert!(r
        .pb(&["commit", "run"], "feat(PROJ-9): add c\n")
        .status
        .success());
    r.stage("d.txt", "d\n");

    let o = r.pb(&["commit", "run"], "feat(sync): add d\n");

    assert!(!o.status.success());
    assert!(err(&o).contains("`feat(PROJ-9): add d`"), "{}", err(&o));
}

#[test]
fn an_amend_is_judged_without_the_commit_it_replaces() {
    let r = Repo::new("stack-amend");
    git(&r.work, &["checkout", "-q", "-b", "stack-parent"]);
    r.stage("p.txt", "p\n");
    assert!(r
        .pb(&["commit", "run"], "feat(PROJ-8): add p\n")
        .status
        .success());
    git(&r.work, &["checkout", "-q", "-b", "stack-child"]);
    r.stage("c.txt", "c\n");
    assert!(r
        .pb(&["commit", "run"], "feat(PROJ-8): add c\n")
        .status
        .success());

    let o = r.pb(&["commit", "run", "-a"], "feat(PROJ-9): add c\n");

    assert!(o.status.success(), "{}{}", out(&o), err(&o));
}

#[test]
fn turning_the_scope_rule_off_lets_a_stacked_commit_through() {
    let r = Repo::new("stack-off");
    assert!(r
        .pb(&["config", "set", "--global", "pr.rules", "off"], "")
        .status
        .success());
    git(&r.work, &["checkout", "-q", "-b", "stack-parent"]);
    r.stage("p.txt", "p\n");
    assert!(r
        .pb(&["commit", "run"], "feat(PROJ-9): add p\n")
        .status
        .success());
    git(&r.work, &["checkout", "-q", "-b", "PROJ-9-child"]);
    r.stage("c.txt", "c\n");

    let o = r.pb(&["commit", "run"], "feat(sync): add c\n");

    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert!(!out(&o).contains("WARN"), "{}", out(&o));
}

// commit all

/// Writes `file` under the work tree, creating its folders.
fn put(r: &Repo, file: &str, body: &str) {
    let path = r.work.join(file);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn last_subject(r: &Repo) -> String {
    git(&r.work, &["log", "-1", "--format=%s"])
}

#[test]
fn all_drafts_a_docs_message_for_a_docs_only_change() {
    let r = Repo::new("all-docs");
    put(&r, "docs/guide.md", "g\n");
    let o = r.pb(&["commit", "all"], "");
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert_eq!(last_subject(&r), "docs: add guide.md");
    assert_eq!(r.remote_head("feat"), git(&r.work, &["rev-parse", "HEAD"]));
    assert!(out(&o).contains("docs: add guide.md"), "{}", out(&o));
}

#[test]
fn all_drafts_a_test_message_for_test_files_only() {
    let r = Repo::new("all-test");
    put(&r, "crates/cli/tests/a.rs", "t\n");
    assert!(r.pb(&["commit", "all"], "").status.success());
    assert_eq!(last_subject(&r), "test(cli): add a.rs");
}

#[test]
fn all_drafts_a_fix_when_every_file_is_modified() {
    let r = Repo::new("all-fix");
    put(&r, "src/x.rs", "x\n");
    git(&r.work, &["add", "."]);
    git(&r.work, &["commit", "-q", "-m", "add x"]);
    put(&r, "src/x.rs", "y\n");
    assert!(r.pb(&["commit", "all"], "").status.success());
    assert_eq!(last_subject(&r), "fix(src): update src");
}

#[test]
fn all_drafts_a_feat_when_a_source_file_is_added() {
    let r = Repo::new("all-feat");
    put(&r, "crates/cli/src/new.rs", "n\n");
    assert!(r.pb(&["commit", "all"], "").status.success());
    assert_eq!(last_subject(&r), "feat(cli): add new.rs");
}

#[test]
fn all_drafts_a_dependency_message_for_a_lock_file() {
    let r = Repo::new("all-deps");
    put(&r, "Cargo.lock", "l\n");
    assert!(r.pb(&["commit", "all"], "").status.success());
    assert_eq!(last_subject(&r), "chore(deps): update dependencies");
}

#[test]
fn all_omits_the_scope_when_the_files_span_areas() {
    let r = Repo::new("all-mixed");
    put(&r, "crates/cli/src/a.rs", "a\n");
    put(&r, "plugins/p/b.md", "b\n");
    assert!(r.pb(&["commit", "all"], "").status.success());
    assert_eq!(last_subject(&r), "feat: update files");
}

#[test]
fn all_uses_the_given_message() {
    let r = Repo::new("all-msg");
    put(&r, "docs/guide.md", "g\n");
    let o = r.pb(&["commit", "all", "-m", "docs: my own words"], "");
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert_eq!(last_subject(&r), "docs: my own words");
}

#[test]
fn all_with_nothing_to_stage_says_so_and_exits_zero() {
    let r = Repo::new("all-none");
    let o = r.pb(&["commit", "all"], "");
    assert!(o.status.success());
    assert!(out(&o).contains("NO_STAGED_CHANGES"), "{}", out(&o));
}

#[test]
fn all_refuses_the_default_branch() {
    let r = Repo::new("all-prot");
    git(&r.work, &["checkout", "-q", "main"]);
    put(&r, "b.txt", "b\n");
    let o = r.pb(&["commit", "all"], "");
    assert!(!o.status.success());
    assert!(err(&o).contains("HEAD is on 'main'"), "{}", err(&o));
}

#[test]
fn all_rebases_when_behind_and_pushes() {
    let r = Repo::new("all-rebase");
    put(&r, "b.txt", "b\n");
    assert!(r.pb(&["commit", "all"], "").status.success());
    let other = r.root.join("other");
    git(
        &r.root,
        &["clone", "-q", r.origin.to_str().unwrap(), "other"],
    );
    fs::write(other.join("m.txt"), "m\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "main moves"]);
    git(&other, &["push", "-q", "origin", "main"]);
    put(&r, "c.txt", "c\n");
    let o = r.pb(&["commit", "all"], "");
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert!(out(&o).contains("Rebasing"), "{}", out(&o));
    assert_eq!(r.remote_head("feat"), git(&r.work, &["rev-parse", "HEAD"]));
}

#[test]
fn all_never_forces_a_rejected_push() {
    let r = Repo::new("all-reject");
    put(&r, "b.txt", "b\n");
    assert!(r.pb(&["commit", "all"], "").status.success());
    let other = r.root.join("other");
    git(
        &r.root,
        &[
            "clone",
            "-q",
            "-b",
            "feat",
            r.origin.to_str().unwrap(),
            "other",
        ],
    );
    fs::write(other.join("o.txt"), "o\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "other work"]);
    git(&other, &["push", "-q", "origin", "feat"]);
    let theirs = r.remote_head("feat");
    put(&r, "c.txt", "c\n");
    let o = r.pb(&["commit", "all"], "");
    assert!(!o.status.success());
    assert_eq!(r.remote_head("feat"), theirs);
}

#[test]
fn all_follows_the_signoff_setting() {
    let r = Repo::new("all-sign");
    put(&r, "b.txt", "b\n");
    assert!(r.pb(&["commit", "all"], "").status.success());
    assert!(git(&r.work, &["log", "-1", "--format=%B"]).contains("Signed-off-by"));

    put(&r, "c.txt", "c\n");
    assert!(r
        .pb(&["commit", "all", "--no-signoff"], "")
        .status
        .success());
    assert!(!git(&r.work, &["log", "-1", "--format=%B"]).contains("Signed-off-by"));

    assert!(r
        .pb(
            &["config", "set", "--global", "commit.signOff", "false"],
            ""
        )
        .status
        .success());
    put(&r, "d.txt", "d\n");
    assert!(r.pb(&["commit", "all"], "").status.success());
    assert!(!git(&r.work, &["log", "-1", "--format=%B"]).contains("Signed-off-by"));
}

// commit all --json --args-stdin

/// Runs `commit all --json --args-stdin` from `cwd` and parses the report.
/// The process always exits 0, whatever the verdict.
fn inline(r: &Repo, cwd: &Path, text: &str) -> serde_json::Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["commit", "all", "--json", "--args-stdin"])
        .env("HOME", r.root.join("home"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("PLAYBOOK_MODE")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "must exit 0: {}{}", out(&o), err(&o));
    serde_json::from_str(&out(&o)).unwrap_or_else(|e| panic!("bad json {e}: {}", out(&o)))
}

#[test]
fn inline_pushes_with_a_drafted_message_and_reports_it() {
    let r = Repo::new("inline-push");
    put(&r, "docs/guide.md", "g\n");

    let v = inline(&r, &r.work, "-A");

    assert_eq!(v["verdict"], "PUSHED", "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["pushed"], true);
    assert_eq!(v["branch"], "feat");
    assert_eq!(v["message"], "docs: add guide.md");
    assert_eq!(r.remote_head("feat"), git(&r.work, &["rev-parse", "HEAD"]));
}

#[test]
fn inline_with_no_flags_commits_only_what_is_staged() {
    let r = Repo::new("inline-staged");
    put(&r, "docs/staged.md", "s\n");
    git(&r.work, &["add", "docs/staged.md"]);
    put(&r, "docs/loose.md", "l\n");

    let v = inline(&r, &r.work, "");

    assert_eq!(v["verdict"], "PUSHED", "{v}");
    assert!(git(&r.work, &["status", "--porcelain", "-uall"]).contains("loose.md"));
}

#[test]
fn inline_reports_no_staged_changes_with_ok_true() {
    let r = Repo::new("inline-none");

    let v = inline(&r, &r.work, "");

    assert_eq!(v["verdict"], "NO_STAGED_CHANGES");
    assert_eq!(v["ok"], true);
}

#[test]
fn parity_default_branch_inline_refuses_main_with_exit_0() {
    let r = Repo::new("inline-main");
    git(&r.work, &["checkout", "-q", "main"]);
    put(&r, "docs/x.md", "x\n");

    let v = inline(&r, &r.work, "-A");

    assert_eq!(v["verdict"], "REFUSED", "{v}");
    assert_eq!(v["ok"], false);
    assert_eq!(v["code"], 1);
    assert!(
        git(&r.work, &["diff", "--cached", "--name-only"]).is_empty(),
        "a refused run must not stage anything"
    );
}

#[test]
fn inline_long_flags_stage_and_amend_like_the_short_ones() {
    let r = Repo::new("inline-long");
    put(&r, "docs/a.md", "a\n");
    assert_eq!(inline(&r, &r.work, "--all")["verdict"], "PUSHED");
    let first = git(&r.work, &["rev-parse", "HEAD"]);
    put(&r, "docs/b.md", "b\n");

    let v = inline(&r, &r.work, "--all --amend");

    assert_eq!(v["verdict"], "PUSHED", "{v}");
    assert_eq!(v["forced"], true);
    assert_ne!(git(&r.work, &["rev-parse", "HEAD"]), first);
    assert_eq!(git(&r.work, &["rev-list", "--count", "main..HEAD"]), "1");
}

#[test]
fn parity_worktree_guard_inline_a_wrong_branch_is_wrong_directory() {
    let r = Repo::new("inline-wrong");
    put(&r, "docs/x.md", "x\n");
    let text = format!("-A Worktree root: {}, branch: other", r.work.display());

    let v = inline(&r, &r.root, &text);

    assert_eq!(v["verdict"], "WRONG_DIRECTORY", "{v}");
    assert!(git(&r.work, &["status", "--porcelain", "-uall"]).contains("x.md"));
}

#[test]
fn parity_root_cwd_inline_acts_on_the_named_root_not_the_session_cwd() {
    let r = Repo::new("inline-root");
    put(&r, "docs/x.md", "x\n");
    let text = format!("-A Worktree root: {}, branch: feat", r.work.display());

    let v = inline(&r, &r.root, &text);

    assert_eq!(v["verdict"], "PUSHED", "{v}");
    assert_eq!(v["branch"], "feat");
    assert_eq!(v["root"], git(&r.work, &["rev-parse", "--show-toplevel"]));
}

#[test]
fn parity_no_force_inline_auto_amend_is_parked() {
    let r = Repo::new("inline-park");
    put(&r, "docs/a.md", "a\n");
    assert_eq!(inline(&r, &r.work, "-A")["verdict"], "PUSHED");
    let remote = r.remote_head("feat");
    put(&r, "docs/b.md", "b\n");

    let v = inline(&r, &r.work, "-A -a --auto");

    assert_eq!(v["verdict"], "PARKED", "{v}");
    assert_eq!(r.remote_head("feat"), remote);
}

#[test]
fn parity_precommit_warnings_inline_names_a_secret_shaped_file() {
    let r = Repo::new("inline-warn");
    put(&r, "creds/id_rsa", "k\n");

    let v = inline(&r, &r.work, "-A");

    let warnings = v["warnings"].to_string();
    assert!(warnings.contains("Secret-shaped"), "{v}");
}

#[test]
fn inline_conflicting_modes_are_refused_with_exit_0() {
    let r = Repo::new("inline-conflict");

    let v = inline(&r, &r.work, "--auto --ask");

    assert_eq!(v["verdict"], "REFUSED");
    assert!(v["error"].as_str().unwrap().contains("conflict"));
}
