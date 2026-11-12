// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The closing steps that stay allowed once the budget guard parks a caller:
//! the skills and commands that deliver finished work (commit, push, open a
//! pull request) and read-only checks of where the work stands. Anything that
//! starts new work (an edit, a build, a merge, a release, an `Agent` spawn)
//! is not on the list.
//!
//! A Bash call counts only when every simple command in it is on the list,
//! so `git status && rm -rf x` is denied. A word that only the shell can
//! evaluate (`$VAR`, `$(...)`, a backtick) is denied, since what it runs is
//! unknown here. This is a guardrail against drift, not a security boundary.

use crate::common::shell::{commands, program_name, Command};
use std::path::Path;

/// The skills a closing step loads: the two delivery skills and the ones they
/// depend on. A renamed skill is listed under both names until 1.0.0, so a new
/// binary still parks a caller that runs a plugin not yet updated. The old
/// names go at 1.0.0.
const CLOSING_SKILLS: [&str; 6] = [
    "playbook:pr-create",
    "playbook:create-pull-request",
    "playbook:commit",
    "playbook:commit-and-push",
    "playbook:writing-style",
    "playbook:engineering-standards",
];

/// Whether a `Skill` call names a closing skill.
pub(super) fn is_closing_skill(skill: &str) -> bool {
    CLOSING_SKILLS.contains(&skill.trim())
}

/// Whether every simple command of `command` is a closing step. `cwd` is where
/// the command runs: a `git push` that names no branch is allowed when the
/// branch checked out there is not protected.
pub(super) fn is_closing_command_in(command: &str, cwd: Option<&Path>) -> bool {
    let plain = without_stream_merges(command);
    let source: Vec<char> = plain.chars().collect();
    let parsed = commands(&plain);
    !parsed.is_empty()
        && parsed
            .iter()
            .all(|step| is_closing_step(step, &source, cwd))
}

/// The text without the stream merges (`2>&1`, `1>&2`, `>&2`, `&>/dev/null`)
/// that sit outside quotes. The shell reader treats `&` as a separator, so
/// they are taken out first. Quoted text is left as it is.
fn without_stream_merges(command: &str) -> String {
    const MERGES: [&str; 4] = ["2>&1", "1>&2", ">&2", "&>/dev/null"];
    let mut out = String::with_capacity(command.len());
    let mut quote: Option<char> = None;
    let mut rest = command;
    while let Some(c) = rest.chars().next() {
        if quote.is_none() && c != '\\' {
            if let Some(merge) = MERGES.iter().find(|m| ends_at_boundary(rest, m)) {
                rest = &rest[merge.len()..];
                continue;
            }
        }
        match (quote, c) {
            (None, '\'' | '"') => quote = Some(c),
            (Some(open), _) if open == c => quote = None,
            _ => {}
        }
        let width = if c == '\\' && quote != Some('\'') {
            c.len_utf8()
                + rest[c.len_utf8()..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8)
        } else {
            c.len_utf8()
        };
        out.push_str(&rest[..width]);
        rest = &rest[width..];
    }
    out
}

/// Whether `rest` starts with `merge` and the word ends right after it. A
/// merge followed by more word characters, as in `>&2x`, is a redirect to a file.
fn ends_at_boundary(rest: &str, merge: &str) -> bool {
    rest.strip_prefix(merge).is_some_and(|after| {
        after
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || matches!(c, ';' | '|' | '&' | ')' | '}'))
    })
}

fn is_closing_step(command: &Command, source: &[char], cwd: Option<&Path>) -> bool {
    // A command inside a substitution feeds another command's words.
    if command.substitution || command.spans.iter().any(|span| span.dynamic) {
        return false;
    }
    // An unquoted heredoc expands `$` and backticks in its body.
    if command
        .heredocs
        .iter()
        .any(|doc| !doc.quoted && doc.text.contains(['$', '`']))
    {
        return false;
    }
    if is_pr_body_write(command) {
        return true;
    }
    let Some(words) = without_redirects(command, source) else {
        return false;
    };
    let words = unwrap_group(&words);
    // A lone `}` closes a group the skill's version probe opened.
    if words.is_empty() && command.words.first().is_some_and(|word| word == "}") {
        return true;
    }
    let Some((program, args)) = words.split_first() else {
        return false;
    };
    match program_name(program).as_str() {
        "git" => is_git_step(args, cwd),
        "gh" => is_gh_read(args),
        "playbook" => is_playbook_step(args),
        // The skills' version probe ends in `|| { echo "..." >&2; exit 1; }`.
        "echo" | "exit" | "true" => true,
        // Output of a closing step may be trimmed.
        "head" | "tail" | "wc" => command.piped,
        _ => false,
    }
}

/// The words without a leading `{` or a trailing `}` of a command group.
fn unwrap_group<'a>(words: &[&'a str]) -> Vec<&'a str> {
    let mut words = words;
    while let Some((&"{", rest)) = words.split_first() {
        words = rest;
    }
    while let Some((&"}", rest)) = words.split_last() {
        words = rest;
    }
    words.to_vec()
}

/// `cat > <dir>/pr-body.md <<'EOF'`: the one file write the pull request
/// skill makes before `playbook pr create`. The body must be a quoted heredoc.
fn is_pr_body_write(command: &Command) -> bool {
    let words: Vec<&str> = command.words.iter().map(String::as_str).collect();
    let [cat, ">", target, ..] = words.as_slice() else {
        return false;
    };
    let rest = &words[3..];
    program_name(cat) == "cat"
        && target.ends_with("/pr-body.md")
        && !target.contains("..")
        && command.heredocs.iter().all(|doc| doc.quoted)
        && rest.iter().all(|word| word.starts_with("<<"))
}

/// The words with the redirections removed, or `None` when one writes or reads
/// a file. Sending output to `/dev/null` or to the other stream, and a
/// heredoc, are fine. A `<` or `>` inside quotes is text, not a redirection.
fn without_redirects<'a>(command: &'a Command, source: &[char]) -> Option<Vec<&'a str>> {
    let mut kept = Vec::new();
    let mut rest = command.words.iter().zip(&command.spans);
    while let Some((word, span)) = rest.next() {
        if !has_unquoted_angle(source, span.start, span.end) {
            kept.push(word.as_str());
            continue;
        }
        let word = word.as_str();
        let operator = word.trim_start_matches(|c: char| c.is_ascii_digit());
        if !operator.starts_with(['<', '>']) {
            // A redirect glued to a word, as in `status>out.txt`.
            return None;
        }
        let symbols = operator.trim_start_matches(['<', '>']);
        let target = if symbols.is_empty() {
            rest.next()?.0.as_str()
        } else {
            symbols
        };
        let is_heredoc = operator.starts_with("<<") && !operator.starts_with("<<<");
        if !is_heredoc && !matches!(target, "/dev/null" | "&1" | "&2") {
            return None;
        }
    }
    Some(kept)
}

/// Whether `source[start..end]` holds a `<` or `>` outside quotes.
fn has_unquoted_angle(source: &[char], start: usize, end: usize) -> bool {
    let mut quote: Option<char> = None;
    let mut ansi = false;
    let mut chars = source
        .get(start..end.min(source.len()))
        .unwrap_or(&[])
        .iter();
    while let Some(&c) = chars.next() {
        // `$'...'` lets a backslash escape the closing quote.
        if ansi {
            match c {
                '\\' => {
                    chars.next();
                }
                '\'' => ansi = false,
                _ => {}
            }
            continue;
        }
        if quote.is_none() && c == '$' && chars.clone().next() == Some(&'\'') {
            chars.next();
            ansi = true;
            continue;
        }
        match (quote, c) {
            (None, '\\') => {
                chars.next();
            }
            (Some('"'), '\\') => {
                chars.next();
            }
            (None, '\'' | '"') => quote = Some(c),
            (Some(open), _) if open == c => quote = None,
            (None, '<' | '>') => return true,
            _ => {}
        }
    }
    false
}

/// `git` with at most `-C <dir>` before the subcommand.
fn is_git_step(args: &[&str], cwd: Option<&Path>) -> bool {
    let (dir, args) = match args {
        ["-C", dir, rest @ ..] => (cwd.map(|cwd| cwd.join(dir)), rest),
        rest => (cwd.map(Path::to_path_buf), rest),
    };
    match args {
        [] => false,
        ["add" | "commit" | "status", ..] => true,
        ["diff" | "log", rest @ ..] => !rest.iter().any(|arg| arg.starts_with("--output")),
        ["worktree", "list", ..] => true,
        ["push", rest @ ..] => is_safe_push(rest, dir.as_deref()),
        _ => false,
    }
}

/// Branches a closing step never pushes to.
const PROTECTED_BRANCHES: [&str; 4] = ["main", "master", "trunk", "develop"];

/// The branch checked out in `dir`, unless it is protected or HEAD is detached.
fn unprotected_current_branch(dir: &Path) -> Option<String> {
    let branch = crate::common::gitfacts::current_branch(dir)??;
    (!PROTECTED_BRANCHES.contains(&branch.as_str())).then_some(branch)
}

/// Where a push with no refspec goes, which depends on `push.default` and
/// the branch's upstream: the branch name on the remote, from
/// `git rev-parse --abbrev-ref @{push}`.
fn push_destination(dir: &Path) -> Option<String> {
    let mut git = std::process::Command::new("git");
    git.arg("-C")
        .arg(dir)
        .args(["rev-parse", "--abbrev-ref", "@{push}"]);
    let out = crate::common::run_with_timeout(&mut git, std::time::Duration::from_secs(10))?;
    let text = String::from_utf8(out.stdout).ok()?;
    let name = text.trim();
    (out.status.success() && !name.is_empty()).then(|| {
        name.split_once('/')
            .map_or(name, |(_, branch)| branch)
            .to_string()
    })
}

/// Whether `refs/heads/<name>` exists in the repo at `dir`.
fn is_local_branch(dir: &Path, name: &str) -> bool {
    let mut git = std::process::Command::new("git");
    git.arg("-C").arg(dir).args([
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/heads/{name}"),
    ]);
    crate::common::run_with_timeout(&mut git, std::time::Duration::from_secs(10))
        .is_some_and(|out| out.status.success())
}

/// A push to a remote. It must not force, delete, mirror, push every branch or
/// tag, run a program of its choosing, or target a protected branch. Every
/// refspec must name a local branch (or `HEAD`). A push with no refspec goes
/// where git's `push.default` sends it, so that destination must not be
/// protected either. Both need `dir`, the repo the push runs in.
fn is_safe_push(args: &[&str], dir: Option<&Path>) -> bool {
    let unsafe_flag = args.iter().any(|arg| {
        let short = arg.starts_with('-') && !arg.starts_with("--");
        arg.starts_with("--force")
            || [
                "--delete",
                "--mirror",
                "--prune",
                "--all",
                "--tags",
                "--follow-tags",
            ]
            .contains(arg)
            || ["--receive-pack", "--exec", "--repo"]
                .iter()
                .any(|flag| arg.starts_with(flag))
            || (short && arg.contains(['f', 'd']))
            || arg.starts_with(['+', ':'])
    });
    // A push option takes the next word as its value, which is not a remote.
    let mut positional: Vec<&str> = Vec::new();
    let mut words = args.iter().copied();
    while let Some(arg) = words.next() {
        match arg {
            "-o" | "--push-option" => {
                words.next();
            }
            _ if arg.starts_with('-') => {}
            _ => positional.push(arg),
        }
    }
    let refspecs = match positional.as_slice() {
        [] => &[][..],
        [_remote, refspecs @ ..] => refspecs,
    };
    let (false, Some(dir)) = (unsafe_flag, dir) else {
        return false;
    };
    let current = unprotected_current_branch(dir);
    if refspecs.is_empty() {
        return current.is_some()
            && push_destination(dir).is_some_and(|branch| !is_protected(&branch));
    }
    refspecs.iter().all(|spec| {
        let (source, destination) = spec.split_once(':').unwrap_or((spec, spec));
        let branch = destination
            .strip_prefix("refs/heads/")
            .or_else(|| destination.strip_prefix("heads/"))
            .unwrap_or(destination);
        if destination.starts_with("refs/") && !destination.starts_with("refs/heads/") {
            return false;
        }
        if branch.is_empty() || is_protected(branch) {
            return false;
        }
        if source == "HEAD" {
            return spec.contains(':') || current.is_some();
        }
        let source = source.strip_prefix("refs/heads/").unwrap_or(source);
        is_local_branch(dir, source)
    })
}

fn is_protected(branch: &str) -> bool {
    PROTECTED_BRANCHES.contains(&branch)
}

/// Read-only `gh`: viewing and listing pull requests and runs, and `api`
/// without a method other than GET or any body.
fn is_gh_read(args: &[&str]) -> bool {
    match args {
        ["pr", "view" | "list" | "checks" | "diff" | "status", ..] => true,
        ["run", "view" | "list", ..] => true,
        ["api", rest @ ..] => is_get_api(rest),
        _ => false,
    }
}

/// `gh api` that only reads. A short flag with its value attached (`-fkey=v`,
/// `-Fkey=v`, `-XPOST`) or in a cluster (`-if`) is judged like the spaced
/// form, and `graphql` is a POST by default.
fn is_get_api(args: &[&str]) -> bool {
    let mut at = 0;
    while let Some(&arg) = args.get(at) {
        let is_get = |value: &str| value == "GET";
        if arg == "graphql" {
            return false;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, value) = long.split_once('=').unwrap_or((long, ""));
            match name {
                "input" | "field" | "raw-field" => return false,
                "method" => {
                    let value = if long.contains('=') {
                        Some(value)
                    } else {
                        at += 1;
                        args.get(at).copied()
                    };
                    if !value.is_some_and(is_get) {
                        return false;
                    }
                }
                _ => {}
            }
        } else if let Some(short) = arg.strip_prefix('-').filter(|short| !short.is_empty()) {
            if short.contains(['f', 'F']) {
                return false;
            }
            if let Some(method) = short.strip_prefix('X') {
                let method = if method.is_empty() {
                    at += 1;
                    args.get(at).copied().unwrap_or_default()
                } else {
                    method
                };
                if !is_get(method) {
                    return false;
                }
            } else if short.contains('X') {
                return false;
            }
        }
        at += 1;
    }
    true
}

/// The `playbook` commands the commit and pull request skills run, and the
/// read-only status and config reads. `commit run` must carry `--auto`:
/// without it the command may force push after an amend or a rebase.
fn is_playbook_step(args: &[&str]) -> bool {
    match args {
        ["commit", "run", rest @ ..] => rest.contains(&"--auto"),
        // Delivery only: promoting a draft, queueing a merge or waiting on one is new work.
        // `deliver` is the old name of `pr create`.
        ["pr", "create", rest @ ..] | ["deliver", rest @ ..] => !rest
            .iter()
            .any(|a| matches!(*a, "--ready" | "--auto-merge" | "--wait")),
        // The read the delivery skills make after a budget stop.
        ["pr", "status", pr, "--json"] => !pr.starts_with('-'),
        // Reading the checks is safe; `--watch` can poll for a long time.
        ["pr", "checks", rest @ ..] => !rest.contains(&"--watch"),
        ["pr", "prepare" | "rules", ..]
        | ["commit", "prepare", ..]
        | ["worktree", "review", "teardown", ..]
        | ["config", "get", ..] => true,
        ["mode", "status", rest @ ..] => is_status_probe(rest),
        _ => false,
    }
}

/// The arguments after `mode status`: `--json` and `--flag auto|ask`, in any
/// order, which is how the delivery skills read the run mode.
fn is_status_probe(args: &[&str]) -> bool {
    let mut args = args.iter();
    while let Some(&arg) = args.next() {
        match arg {
            "--json" => {}
            "--flag" if matches!(args.next(), Some(&("auto" | "ask"))) => {}
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    /// A scratch repo on `feat/x` with a second local branch `feat/y`.
    fn repo() -> &'static PathBuf {
        static REPO: OnceLock<PathBuf> = OnceLock::new();
        REPO.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!(
                "playbook-closing-{}-{}",
                crate::testing::run_id(),
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("repo dir");
            let git = |args: &[&str]| {
                let status = std::process::Command::new("git")
                    .args(args)
                    .current_dir(&dir)
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_SYSTEM", "/dev/null")
                    .env("GIT_AUTHOR_NAME", "t")
                    .env("GIT_AUTHOR_EMAIL", "t@t")
                    .env("GIT_COMMITTER_NAME", "t")
                    .env("GIT_COMMITTER_EMAIL", "t@t")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .expect("git");
                assert!(status.success(), "git {args:?}");
            };
            git(&["init", "-q", "-b", "feat/x"]);
            git(&["commit", "-q", "--allow-empty", "-m", "seed"]);
            git(&["branch", "feat/y"]);
            dir
        })
    }

    fn closing(command: &str) -> bool {
        is_closing_command_in(command, Some(repo()))
    }

    #[test]
    fn every_listed_command_is_a_closing_step_and_new_work_is_not() {
        let closing_steps = [
            "git add -A",
            "git add src/a.rs docs/x.md",
            "git commit -S -s -m \"feat: x\"",
            "git push origin feat/x",
            "git push origin feat/y",
            "git push -u origin feat/x",
            "git push origin HEAD",
            "git push -u origin HEAD",
            "git push -o ci.skip origin feat/x",
            "git -C ../wt status --short",
            "git status",
            "git diff HEAD~1 --stat",
            "git log --oneline -5",
            "git worktree list",
            "gh pr view 12 --json state",
            "gh pr list --state open",
            "gh pr checks 12",
            "gh run view 99 --log-failed",
            "gh api repos/o/r/pulls/12",
            "gh api -X GET repos/o/r/pulls",
            "playbook pr create --title \"fix: x\" --body-file b.md",
            "playbook deliver --json --title \"fix: x\" --body-file b.md",
            "cat > \"/tmp/state/pr-body.md\" <<'PRBODY_EOF'\nbody\nPRBODY_EOF\nplaybook deliver --json --title \"fix: x\" --body-file /tmp/state/pr-body.md",
            "playbook commit run --auto",
            "git push origin HEAD:refs/heads/feat/x",
            "git log --oneline 2>&1 | head -5",
            "playbook pr prepare --help >/dev/null 2>&1 || { echo \"ERROR: upgrade\" >&2; exit 1; }",
            "cat > \"/tmp/state/pr-body.md\" <<'PRBODY_EOF'\nbody $x `y`\nPRBODY_EOF\nplaybook pr create --title \"fix: x\" --body-file /tmp/state/pr-body.md",
            "git commit -m \"feat: a b\" 2>&1",
            "git commit -m \"a 2>&1 b\"",
            "git commit -m \"feat: a -> b\"",
            "git commit -m 'fix: x < y and y > z'",
            "playbook mode status --flag auto --json",
            "playbook mode status --json --flag ask",
            "playbook worktree review teardown /tmp/wt",
            "playbook pr prepare --base main",
            "playbook pr status 12 --json",
            "playbook pr checks 12",
            "playbook pr checks 12 --json --required",
            "playbook mode status",
            "playbook mode status --json",
            "git status && git diff",
            "git log --oneline 2>&1 | head -5",
            "playbook commit run --auto <<'MSG'\nfeat: x\nMSG",
        ];
        let new_work = [
            "playbook deliver --ready --auto-merge",
            "echo `git push origin feat/x`",
            "git add -A `git push origin feat/x`",
            "playbook deliver --wait",
            "playbook deliver --ready",
            "playbook pr create --ready --auto-merge",
            "playbook pr create --wait",
            "playbook pr merge 12",
            "playbook pr merge 12 --no-wait",
            "playbook pr land 12",
            "playbook pr ci-wait 12",
            "playbook pr land-wait 12",
            "playbook pr checks 12 --watch",
            "playbook pr checks 12 --json --watch --fail-fast",
            "playbook pr create --ready",
            "cargo build",
            "npm test",
            "rm -rf target",
            "gh pr merge 12 --auto",
            "gh release create v1",
            "gh api -X POST repos/o/r/issues",
            "gh api repos/o/r/issues -f title=x",
            "playbook mode ask",
            "playbook config set auto.budget.mainUsd 50",
            "git status && rm -rf x",
            "git push --force origin main",
            "git push -f",
            "git push --force-with-lease",
            "git push origin :main",
            "git push origin +main",
            "git push --delete origin x",
            "git reset --hard",
            "git checkout main",
            "git commit -m \"$(cat <<'EOF'\nx\nEOF\n)\"",
            "git status > out.txt",
            "git diff --output=x",
            "gh api repos/o/r/issues -fkey=v",
            "gh api repos/o/r/issues -Fkey=v",
            "gh api repos/o/r/issues -XPOST",
            "gh api repos/o/r/issues -if",
            "gh api graphql",
            "gh api repos/o/r --method=POST",
            "cat <<EOF | playbook commit run --auto\n$HOME\nEOF",
            "playbook commit run --auto <<EOF\n$(id)\nEOF",
            "git status>out.txt",
            "git log>>out.txt",
            "git log --output=x",
            "git log --output x",
            "git push --receive-pack=evil origin feat/x",
            "git push --exec=evil origin feat/x",
            "git push origin main",
            "git push origin HEAD:main",
            "git push origin feat/x:refs/heads/master",
            "git push",
            "git push origin",
            "playbook mode status --flag bogus --json",
            "playbook mode status --flag",
            "playbook mode status --json extra",
            "git status \"x\" > out.txt",
            "git status >out.txt",
            "git log 2>err.txt",
            "git log < in.txt",
            "git push --all origin",
            "git push origin heads/main",
            "git push origin feat/x:heads/main",
            "git push -o ci.skip origin",
            "git push --push-option ci.skip origin",
            "git push origin v1.4.0",
            "git push origin refs/tags/v1.4.0",
            "git push --follow-tags origin feat/x",
            "git push origin feat/x:refs/tags/v1",
            "echo x >&2x",
            "git status >&2out.txt",
            "echo x $'\\'' >out.txt",
            "echo \"a\" $'\\''>out.txt",
            "playbook commit run",
            "playbook commit run -a",
            "git commit -m \"x\" \"$(echo 2>&1)\"",
            "git status '2>&1' > out.txt",
            "git status \"2>&1\" > out.txt",
            "cat > /tmp/other.txt <<'EOF'\nx\nEOF",
            "cat > ../pr-body.md <<'EOF'\nx\nEOF",
            "cat > /tmp/pr-body.md <<EOF\n$(id)\nEOF",
            "echo hi > out.txt",
            "head -5 file",
            "",
        ];
        for command in closing_steps {
            assert!(closing(command), "{command:?} must be allowed");
        }
        for command in new_work {
            assert!(!closing(command), "{command:?} must be denied");
        }
    }

    #[test]
    fn only_the_delivery_skills_and_their_dependencies_are_closing_skills() {
        for skill in [
            "playbook:pr-create",
            "playbook:create-pull-request",
            "playbook:commit",
            "playbook:commit-and-push",
            "playbook:writing-style",
            "playbook:engineering-standards",
        ] {
            assert!(is_closing_skill(skill), "{skill}");
        }
        for skill in [
            "playbook:implement",
            "playbook:fix",
            "commit",
            "pr-create",
            "",
        ] {
            assert!(!is_closing_skill(skill), "{skill}");
        }
    }
}
