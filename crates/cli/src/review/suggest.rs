// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook review suggest`: recommend a quick or a deep review from facts
//! git and `gh` already hold. The rules are const data, each with a plain
//! reason, so the same list can be applied by hand without the binary. No
//! model runs here. When no rule is decisive the confidence is `low` and the
//! caller may ask a small model for a second opinion.
//!
//! A local diff (the branch against the default branch) needs no network. `gh`
//! is only used when a PR number is given.

use crate::common::re::static_regex;
use crate::pr::shared::{gh, git, reject_option_like};
use crate::pr::stack::{detect, Gh, StackPr, State, DEFAULT_MAX_PRS};
use crate::pr::triage::Review;
use regex::Regex;
use serde::Serialize;
use std::collections::BTreeSet;
use std::sync::LazyLock;

/// The cuts the rules compare against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cfg {
    /// More changed lines than this is a large change.
    pub deep_lines: u64,
    /// At most this many changed lines is a small change.
    pub quick_lines: u64,
    /// More files than this is a wide change.
    pub deep_files: u64,
    /// This many packages or crates touched is a cross-cutting change.
    pub package_span: u64,
    /// Code lines at which a change with almost no tests is a risk.
    pub untested_lines: u64,
}

impl Cfg {
    pub const DEFAULT: Cfg = Cfg {
        deep_lines: 400,
        quick_lines: 60,
        deep_files: 25,
        package_span: 3,
        untested_lines: 150,
    };
}

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub path: String,
    pub added: u64,
    pub removed: u64,
    pub renamed: bool,
}

/// What git and `gh` report about the change.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub files: Vec<File>,
    pub labels: Vec<String>,
    /// `Some(true)` when the PR or branch is yours, `None` when unknown.
    pub own: Option<bool>,
}

/// The measurements the rules read. Plain data, so a rule test builds one by
/// hand.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Signals {
    pub lines: u64,
    pub files: u64,
    /// Changed lines outside docs, tests, lock files and the plugin payload.
    pub code_lines: u64,
    pub test_lines: u64,
    pub payload_lines: u64,
    /// Changed lines of code outside Rust files, which hold their tests inline.
    pub untested_lines: u64,
    /// Changed lines of code, tests and plugin files: everything but docs and lock files.
    pub review_lines: u64,
    /// Test lines over test plus code lines, two decimals.
    pub test_ratio: f64,
    pub packages: Vec<String>,
    pub areas: Vec<String>,
    pub labels: Vec<String>,
    pub own: Option<bool>,
    pub docs_only: bool,
    pub lock_only: bool,
    pub tests_only: bool,
    pub renames_only: bool,
}

impl Signals {
    fn has(&self, area: &str) -> bool {
        self.areas.iter().any(|a| a == area)
    }
}

struct Pats {
    payload: Regex,
    test: Regex,
    workflows: Regex,
    release: Regex,
    hooks: Regex,
    migrations: Regex,
    security: Regex,
    cli: Regex,
    package: Regex,
    braces: Regex,
}

static PATS: LazyLock<Pats> = LazyLock::new(|| Pats {
    payload: static_regex(r"^(plugins/[^/]+/(commands|skills|agents|hooks)/|(seed/)?prompts/)"),
    test: static_regex(
        r"(^|/)(tests?|__tests__|spec|fixtures)/|(_test|_spec)\.[a-z]+$|\.(test|spec)\.[a-z]+$",
    ),
    workflows: static_regex(r"^\.github/(workflows|actions)/"),
    release: static_regex(r"(?i)(^|/)([^/]*release[^/]*|\.goreleaser[^/]*|dist-workspace\.toml)$"),
    hooks: static_regex(r"(?i)(^|/)(hooks?|guards?)/|(^|/)[^/]*guard[^/]*\.[a-z]+$"),
    migrations: static_regex(
        r"(?i)(^|/)(migrations?|schemas?|prisma)/|\.sql$|(^|/)schema\.[a-z]+$",
    ),
    security: static_regex(
        r"(?i)(^|/)([^/]*[-_.])?(auth|authn|authz|authentication|authorization|oauth|jwt|crypto|secrets?|credentials?|passwords?|permissions?|rbac|acl)([-_.][^/]*)?(/|$)|(^|/)\.env|\.(pem|key)$",
    ),
    cli: static_regex(
        r"(^|/)(main\.(rs|go|py|ts)|cli\.(rs|py|ts)|cmd|bin|command\.rs|intents\.json|openapi[^/]*|[^/]+\.proto)(/|$)",
    ),
    package: static_regex(r"^(crates|packages|apps|services|libs|plugins)/([^/]+)/"),
    braces: static_regex(r"\{[^}]* => ([^}]*)\}"),
});

#[rustfmt::skip]
const LOCKS: &[&str] = &[
    "Cargo.lock", "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "poetry.lock",
    "Gemfile.lock", "go.sum", "uv.lock", "composer.lock", "bun.lockb", "Pipfile.lock",
];

#[rustfmt::skip]
const MANIFESTS: &[&str] = &[
    "Cargo.toml", "package.json", "go.mod", "pyproject.toml", "Gemfile", "pom.xml",
    "build.gradle", "build.gradle.kts", "Pipfile",
];

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_lock(path: &str) -> bool {
    LOCKS.contains(&file_name(path))
}

fn is_manifest(path: &str) -> bool {
    let name = file_name(path);
    MANIFESTS.contains(&name) || (name.starts_with("requirements") && name.ends_with(".txt"))
}

fn is_payload(path: &str) -> bool {
    PATS.payload.is_match(path)
}

fn is_test(path: &str) -> bool {
    PATS.test.is_match(path)
}

fn is_doc(path: &str) -> bool {
    if is_payload(path) || is_manifest(path) {
        return false;
    }
    let name = file_name(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    path.starts_with("docs/")
        || ["md", "mdx", "txt", "rst"].contains(&ext)
        || ["LICENSE", "NOTICE", "CODEOWNERS"].contains(&name)
}

/// The risky areas a path belongs to. Docs, tests and lock files hold none.
fn areas_of(path: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    if is_manifest(path) {
        out.push("dependencies");
    }
    if is_doc(path) || is_test(path) || is_lock(path) {
        return out;
    }
    let checks: [(&Regex, &'static str); 6] = [
        (&PATS.workflows, "workflows"),
        (&PATS.release, "release"),
        (&PATS.hooks, "hooks"),
        (&PATS.migrations, "migrations"),
        (&PATS.security, "security-paths"),
        (&PATS.cli, "cli-surface"),
    ];
    for (re, name) in checks {
        if re.is_match(path) {
            out.push(name);
        }
    }
    if is_payload(path) {
        out.push("plugin-payload");
    }
    out
}

fn package_of(path: &str) -> String {
    PATS.package
        .captures(path)
        .and_then(|c| c.get(2))
        .map_or_else(|| "root".to_string(), |m| m.as_str().to_string())
}

/// The new path of a `git diff --numstat -M` row: `a/{b => c}/d` and
/// `a => b` both give the path after the rename.
fn new_path(raw: &str) -> String {
    if !raw.contains(" => ") {
        return raw.to_string();
    }
    if raw.contains('{') {
        return PATS.braces.replace_all(raw, "$1").replace("//", "/");
    }
    raw.rsplit(" => ").next().unwrap_or(raw).to_string()
}

/// Files from `git diff -M --numstat` output. A binary row (`-` counts) has
/// zero lines.
pub fn parse_numstat(text: &str) -> Vec<File> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|row| {
            let mut cols = row.splitn(3, '\t');
            let added = cols.next().unwrap_or("-").parse().unwrap_or(0);
            let removed = cols.next().unwrap_or("-").parse().unwrap_or(0);
            let raw = cols.next().unwrap_or("");
            File {
                path: new_path(raw),
                added,
                removed,
                renamed: raw.contains(" => "),
            }
        })
        .collect()
}

/// Measure the change.
pub fn measure(facts: &Facts) -> Signals {
    let mut s = Signals {
        files: facts.files.len() as u64,
        labels: facts.labels.clone(),
        own: facts.own,
        ..Signals::default()
    };
    let mut packages = BTreeSet::new();
    let mut areas = BTreeSet::new();
    for f in &facts.files {
        let n = f.added + f.removed;
        s.lines += n;
        if is_test(&f.path) {
            s.test_lines += n;
        } else if is_payload(&f.path) {
            s.payload_lines += n;
        } else if !is_doc(&f.path) && !is_lock(&f.path) {
            s.code_lines += n;
            if !f.path.ends_with(".rs") {
                s.untested_lines += n;
            }
        }
        if !is_doc(&f.path) && !is_lock(&f.path) {
            s.review_lines += n;
            packages.insert(package_of(&f.path));
        }
        areas.extend(areas_of(&f.path).into_iter().map(str::to_string));
    }
    let all = |pred: fn(&str) -> bool| {
        !facts.files.is_empty() && facts.files.iter().all(|f| pred(&f.path))
    };
    s.docs_only = all(is_doc);
    s.lock_only = all(is_lock);
    s.tests_only = all(is_test);
    s.renames_only =
        !facts.files.is_empty() && s.lines == 0 && facts.files.iter().all(|f| f.renamed);
    let code_and_tests = s.code_lines + s.test_lines;
    if code_and_tests > 0 {
        s.test_ratio = (s.test_lines as f64 / code_and_tests as f64 * 100.0).round() / 100.0;
    }
    s.packages = packages.into_iter().collect();
    s.areas = areas.into_iter().collect();
    s
}

/// How a rule weighs in. The first class with a hit decides, in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A label that asks for care: deep, whatever else is true.
    Label,
    /// A change that cannot alter behavior: quick.
    Downgrade,
    /// A risk: deep.
    Risk,
    /// A small change with no risk: quick.
    Small,
}

/// One row of the rule table.
pub struct Rule {
    pub id: &'static str,
    pub class: Class,
    /// Plain English. `{review_lines}`, `{payload_lines}`, `{untested_lines}`, `{files}` and `{packages}` fill in.
    pub reason: &'static str,
    pub hit: fn(&Signals, &Cfg) -> bool,
}

impl Rule {
    const fn new(
        id: &'static str,
        class: Class,
        reason: &'static str,
        hit: fn(&Signals, &Cfg) -> bool,
    ) -> Rule {
        Rule {
            id,
            class,
            reason,
            hit,
        }
    }
}

/// Every rule, in the order reasons are reported.
#[rustfmt::skip]
pub const RULES: &[Rule] = &[
    Rule::new("security-label", Class::Label, "The PR has a security label.",
        |s, _| s.labels.iter().any(|l| l.to_lowercase().contains("security"))),
    Rule::new("lock-only", Class::Downgrade, "Only lock files changed.", |s, _| s.lock_only),
    Rule::new("docs-only", Class::Downgrade, "Only docs changed.", |s, _| s.docs_only),
    Rule::new("renames-only", Class::Downgrade, "Files were only renamed or moved.",
        |s, _| s.renames_only && s.areas.is_empty()),
    Rule::new("tests-only", Class::Downgrade, "Only tests changed.", |s, _| s.tests_only),
    Rule::new("no-code", Class::Downgrade, "Only docs, tests and lock files changed.",
        |s, _| s.files > 0 && s.code_lines == 0 && s.payload_lines == 0 && s.areas.is_empty()),
    Rule::new("big-diff", Class::Risk, "Large change: {review_lines} changed lines.",
        |s, c| s.review_lines > c.deep_lines),
    Rule::new("many-files", Class::Risk, "Wide change: {files} files.",
        |s, c| s.files > c.deep_files),
    Rule::new("many-packages", Class::Risk, "Cuts across {packages} packages.",
        |s, c| s.packages.len() as u64 >= c.package_span),
    Rule::new("security-paths", Class::Risk, "Touches auth, crypto, secrets or permissions code.",
        |s, _| s.has("security-paths")),
    Rule::new("workflows", Class::Risk, "Changes CI workflows.", |s, _| s.has("workflows")),
    Rule::new("release", Class::Risk, "Changes release files.", |s, _| s.has("release")),
    Rule::new("hooks-guards", Class::Risk, "Changes hooks or guards.", |s, _| s.has("hooks")),
    Rule::new("migrations", Class::Risk, "Changes a migration or a schema.",
        |s, _| s.has("migrations")),
    Rule::new("dependencies", Class::Risk, "Changes a dependency manifest.",
        |s, _| s.has("dependencies")),
    Rule::new("cli-surface", Class::Risk, "Changes the public command or API surface.",
        |s, _| s.has("cli-surface")),
    Rule::new("plugin-payload", Class::Risk,
        "Changes plugin commands, skills or agents ({payload_lines} lines).",
        |s, c| s.has("plugin-payload") && s.payload_lines > c.quick_lines),
    Rule::new("untested-code", Class::Risk, "{untested_lines} changed lines of code with almost no tests.",
        |s, c| s.untested_lines >= c.untested_lines && s.test_lines * 10 < s.untested_lines + s.test_lines),
    Rule::new("small", Class::Small, "Small change: {review_lines} lines in {files} files.",
        |s, c| s.review_lines <= c.quick_lines),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Low,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Low => "low",
        }
    }
}

/// The answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suggestion {
    pub recommendation: Review,
    pub confidence: Confidence,
    /// At most four short lines.
    pub reasons: Vec<String>,
    /// The ids of the rules that decided.
    pub rules: Vec<&'static str>,
    pub signals: Signals,
    pub scope: Scope,
    /// The stack that was measured; `None` outside a stack run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<StackScope>,
}

const MAX_REASONS: usize = 4;

fn fill(template: &str, s: &Signals) -> String {
    template
        .replace("{review_lines}", &s.review_lines.to_string())
        .replace("{payload_lines}", &s.payload_lines.to_string())
        .replace("{untested_lines}", &s.untested_lines.to_string())
        .replace("{files}", &s.files.to_string())
        .replace("{packages}", &s.packages.len().to_string())
}

/// Apply the rule table. The first class with a hit decides; a change that
/// no class decides is a close call, `low` confidence, leaning deep unless
/// the change is yours.
pub fn decide(s: Signals, cfg: &Cfg) -> Suggestion {
    let fired: Vec<&Rule> = RULES.iter().filter(|r| (r.hit)(&s, cfg)).collect();
    let in_class =
        |c: Class| -> Vec<&Rule> { fired.iter().copied().filter(|r| r.class == c).collect() };
    let (recommendation, confidence, picked) = [
        (Class::Label, Review::Deep),
        (Class::Downgrade, Review::Quick),
        (Class::Risk, Review::Deep),
        (Class::Small, Review::Quick),
    ]
    .into_iter()
    .find_map(|(class, review)| {
        let rules = in_class(class);
        (!rules.is_empty()).then_some((review, Confidence::High, rules))
    })
    .unwrap_or_else(|| {
        let review = if s.own == Some(true) {
            Review::Quick
        } else {
            Review::Deep
        };
        (review, Confidence::Low, Vec::new())
    });
    let mut reasons: Vec<String> = picked.iter().map(|r| fill(r.reason, &s)).collect();
    if picked.is_empty() {
        reasons.push(format!(
            "{} changed lines in {} files, between the small and large cut, with no risky path.",
            s.review_lines, s.files
        ));
    }
    reasons.truncate(MAX_REASONS);
    Suggestion {
        recommendation,
        confidence,
        reasons,
        rules: picked.iter().map(|r| r.id).collect(),
        signals: s,
        scope: Scope::ThisPr,
        stack: None,
    }
}

/// `recommendation=`, `confidence=`, one `reason:` line each, the signals on
/// one line, then `scope=` and, for a stack run, a `stack:` line.
pub fn render(sug: &Suggestion) -> String {
    let mut out = vec![
        format!("recommendation={}", sug.recommendation.as_str()),
        format!("confidence={}", sug.confidence.as_str()),
    ];
    out.extend(sug.reasons.iter().map(|r| format!("reason: {r}")));
    let s = &sug.signals;
    out.push(format!(
        "signals: lines={} files={} packages={} test_ratio={} areas={}",
        s.lines,
        s.files,
        s.packages.len(),
        s.test_ratio,
        if s.areas.is_empty() {
            "none".to_string()
        } else {
            s.areas.join(",")
        }
    ));
    out.push(format!("scope={}", sug.scope.as_str()));
    if let Some(st) = &sug.stack {
        let list = |ns: &[u64]| {
            ns.iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        out.push(format!(
            "stack: open={} merged={} skipped={}",
            list(&st.open),
            list(&st.merged),
            list(&st.skipped)
        ));
    }
    out.join("\n")
}

fn ref_exists(name: &str) -> bool {
    git(&["rev-parse", "--verify", "--quiet", name]).is_ok()
}

/// The comparison ref for a local diff: the base the caller named, else
/// `origin/HEAD`, else `main` or `master`. Prefers the `origin/` copy. No
/// network call.
fn local_base(base: Option<&str>) -> Result<String, String> {
    let name = match base.filter(|b| !b.is_empty()) {
        Some(b) => b.to_string(),
        None => git(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
            .ok()
            .and_then(|r| r.strip_prefix("origin/").map(str::to_string))
            .or_else(|| {
                ["main", "master"]
                    .into_iter()
                    .find(|b| ref_exists(&format!("origin/{b}")) || ref_exists(b))
                    .map(str::to_string)
            })
            .ok_or("no default branch found; pass --base")?,
    };
    reject_option_like("base branch", &name)?;
    let remote = format!("origin/{name}");
    if ref_exists(&remote) {
        Ok(remote)
    } else if ref_exists(&name) {
        Ok(name)
    } else {
        Err(format!("base branch `{name}` not found"))
    }
}

fn local_facts(base: Option<&str>) -> Result<Facts, String> {
    let base = local_base(base)?;
    let text = git(&[
        "-c",
        "core.quotePath=false",
        "diff",
        "-M",
        "--numstat",
        &format!("{base}...HEAD"),
    ])?;
    Ok(Facts {
        files: parse_numstat(&text),
        labels: Vec::new(),
        own: Some(true),
    })
}

/// One PR's files, label names and author login. The login of the caller is
/// not read here, so a stack run can fetch it once.
type PrPart = (Vec<File>, Vec<String>, String);

fn pr_part(run: Gh, number: &str) -> Result<PrPart, String> {
    let files_jq =
        ".[] | {path:.filename,added:.additions,removed:.deletions,renamed:(.status==\"renamed\")}";
    let endpoint = format!("repos/{{owner}}/{{repo}}/pulls/{number}/files?per_page=100");
    let calls: [Vec<&str>; 2] = [
        vec!["api", "--paginate", &endpoint, "--jq", files_jq],
        vec!["pr", "view", number, "--json", "labels,author"],
    ];
    let mut results =
        crate::common::par::map(&calls, crate::common::par::MAX_CONCURRENT, |a| run(a)).into_iter();
    let (Some(files), Some(meta)) = (results.next(), results.next()) else {
        return Err("gh returned fewer results than calls".to_string());
    };
    parse_part(&files?, &meta?)
}

/// The caller's login, or `None` when it could not be read.
fn login(run: Gh) -> Option<String> {
    run(&["api", "/user", "-q", ".login"]).ok()
}

fn pr_facts(run: Gh, number: &str) -> Result<Facts, String> {
    let (part, me) = std::thread::scope(|s| {
        let me = s.spawn(|| login(run));
        let part = pr_part(run, number);
        (part, me.join().unwrap_or(None))
    });
    Ok(facts_of(part?, me.as_deref()))
}

/// The facts of one PR part for the caller `me`, when the login is known.
fn facts_of((files, labels, author): PrPart, me: Option<&str>) -> Facts {
    Facts {
        files,
        labels,
        own: me.map(|m| m == author),
    }
}

/// Which change `review suggest` measures. The `--scope` flag that picks it
/// arrives in the next change of this stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum Scope {
    /// The one PR given, or the local branch.
    ThisPr,
    /// Every open PR in the stack around the PR given.
    Stack,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::ThisPr => "this-pr",
            Scope::Stack => "stack",
        }
    }
}

/// The PRs of a stack by role: `open` are measured, `merged` are context
/// only, `skipped` are closed without merging.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StackScope {
    pub open: Vec<u64>,
    pub merged: Vec<u64>,
    pub skipped: Vec<u64>,
}

/// What a stack run found.
#[derive(Debug, Clone)]
pub enum StackFacts {
    /// The PR is not in a stack: its own facts.
    Alone(Facts),
    /// The merged facts of every open PR in the stack.
    Scored(Facts, StackScope),
    /// More open PRs than the stack limit: nothing was fetched, and the
    /// signals hold only the line and file sums `gh` already reported for the
    /// stack. They are sizes, not a classification, so they never go to `decide`.
    OverBudget(StackScope, Signals),
}

/// One `Facts` for several PRs measured together. Files merge by path (lines
/// add up, `renamed` holds only when every part renamed the file), labels
/// union, and `own` is yours only when every part is. Summing per path can
/// count a line edited in two PRs twice, which overstates size and leans
/// toward deep, the safe side.
pub fn merge_facts(parts: Vec<Facts>) -> Facts {
    let mut files: Vec<File> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut labels: Vec<String> = Vec::new();
    for part in &parts {
        for f in &part.files {
            match index.get(&f.path) {
                Some(&at) => {
                    let m = &mut files[at];
                    m.added += f.added;
                    m.removed += f.removed;
                    m.renamed &= f.renamed;
                }
                None => {
                    index.insert(f.path.clone(), files.len());
                    files.push(f.clone());
                }
            }
        }
        for l in &part.labels {
            if !labels.contains(l) {
                labels.push(l.clone());
            }
        }
    }
    let own = if parts.iter().any(|p| p.own == Some(false)) {
        Some(false)
    } else if !parts.is_empty() && parts.iter().all(|p| p.own == Some(true)) {
        Some(true)
    } else {
        None
    };
    Facts { files, labels, own }
}

/// The facts for the stack around PR `number`. Merged PRs are left out and
/// closed ones skipped. A stack with more than `max_prs` open PRs is answered
/// from the sums `gh` already reported, so a long chain costs no further
/// call. Any failed fetch fails the whole call: no partial stack score.
pub fn stack_facts(run: Gh, number: &str, max_prs: u64) -> Result<StackFacts, String> {
    let n: u64 = number
        .parse()
        .map_err(|_| format!("`{number}` is not a PR number"))?;
    let stack = detect(run, Some(n))?;
    for w in &stack.warnings {
        eprintln!("warning: {w}");
    }
    if !stack.ask() {
        return Ok(StackFacts::Alone(pr_facts(run, &n.to_string())?));
    }
    let given = stack
        .prs
        .iter()
        .find(|p| p.number == n)
        .ok_or_else(|| format!("PR #{n} was not found in its stack"))?;
    match given.state {
        State::Open => {}
        State::Merged => return Err(format!("PR #{n} is merged; nothing to review")),
        State::Closed => return Err(format!("PR #{n} is closed; nothing to review")),
    }
    let numbers = |state: State| -> Vec<u64> {
        stack
            .prs
            .iter()
            .filter(|p| p.state == state)
            .map(|p| p.number)
            .collect()
    };
    let scope = StackScope {
        open: numbers(State::Open),
        merged: numbers(State::Merged),
        skipped: numbers(State::Closed),
    };
    if scope.open.len() as u64 > max_prs {
        let lines: u64 = stack.open().map(StackPr::lines).sum();
        let signals = Signals {
            lines,
            files: stack.open().map(|p| p.changed_files).sum(),
            code_lines: lines,
            review_lines: lines,
            ..Signals::default()
        };
        return Ok(StackFacts::OverBudget(scope, signals));
    }
    // Each PR runs two calls at once, so half the cap keeps `gh` processes within it.
    let parts =
        crate::common::par::map(&scope.open, crate::common::par::MAX_CONCURRENT / 2, |pr| {
            pr_part(run, &pr.to_string())
        })
        .into_iter()
        .collect::<Result<Vec<_>, String>>()?;
    let me = login(run);
    let facts = parts
        .into_iter()
        .map(|part| facts_of(part, me.as_deref()))
        .collect();
    Ok(StackFacts::Scored(merge_facts(facts), scope))
}

#[derive(serde::Deserialize)]
struct PrFile {
    path: String,
    added: u64,
    removed: u64,
    renamed: bool,
}

#[derive(serde::Deserialize)]
struct PrMeta {
    labels: Vec<PrLabel>,
    author: PrAuthor,
}

#[derive(serde::Deserialize)]
struct PrLabel {
    name: String,
}

#[derive(serde::Deserialize)]
struct PrAuthor {
    login: String,
}

/// The files, labels and author from the two per-PR `gh` outputs: one JSON
/// object per file line, and the `pr view` JSON. A missing field is an error,
/// never a silent zero.
fn parse_part(files: &str, meta: &str) -> Result<PrPart, String> {
    let files = files
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let f: PrFile =
                serde_json::from_str(l).map_err(|e| format!("gh returned bad file JSON: {e}"))?;
            Ok(File {
                path: f.path,
                added: f.added,
                removed: f.removed,
                renamed: f.renamed,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let meta: PrMeta =
        serde_json::from_str(meta).map_err(|e| format!("gh pr view returned bad JSON: {e}"))?;
    let labels = meta.labels.into_iter().map(|l| l.name).collect();
    Ok((files, labels, meta.author.login))
}

/// The default answer for a stack with more open PRs than `max_prs`: deep,
/// without fetching their files.
fn over_budget(stack: StackScope, signals: Signals, max_prs: u64) -> Suggestion {
    Suggestion {
        recommendation: Review::Deep,
        confidence: Confidence::High,
        reasons: vec![format!(
            "{} open PRs in the stack, over the limit of {max_prs}.",
            stack.open.len()
        )],
        rules: vec!["stack-over-budget"],
        signals,
        scope: Scope::Stack,
        stack: Some(stack),
    }
}

/// Decide on `facts`, tagging the answer with the scope that was measured.
fn decided(
    facts: Facts,
    cfg: &Cfg,
    scope: Scope,
    stack: Option<StackScope>,
) -> Result<Suggestion, String> {
    if facts.files.is_empty() {
        return Err("no changed files found; nothing to review".to_string());
    }
    Ok(Suggestion {
        scope,
        stack,
        ..decide(measure(&facts), cfg)
    })
}

fn from_stack(found: StackFacts, cfg: &Cfg, max_prs: u64) -> Result<Suggestion, String> {
    match found {
        StackFacts::Alone(facts) => decided(facts, cfg, Scope::ThisPr, None),
        StackFacts::Scored(facts, stack) => decided(facts, cfg, Scope::Stack, Some(stack)),
        StackFacts::OverBudget(stack, signals) => Ok(over_budget(stack, signals, max_prs)),
    }
}

/// Collect the facts for PR `pr`, or for the branch against its base when
/// `pr` is `None`, and decide. With `Scope::Stack` the facts are those of
/// every open PR in the PR's stack.
pub fn run(pr: Option<&str>, base: Option<&str>, scope: Scope) -> Result<Suggestion, String> {
    let number = pr.map(|p| p.trim_start_matches('#'));
    if let (Some(n), Some(given)) = (number, pr) {
        if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!("`{given}` is not a PR number"));
        }
    }
    if number.is_none() && scope == Scope::Stack {
        return Err("--scope stack needs a PR number".to_string());
    }
    let cfg = Cfg::DEFAULT;
    match (number, scope) {
        (Some(n), Scope::Stack) => from_stack(
            stack_facts(&|a| gh(a), n, DEFAULT_MAX_PRS)?,
            &cfg,
            DEFAULT_MAX_PRS,
        ),
        (Some(n), Scope::ThisPr) => decided(pr_facts(&|a| gh(a), n)?, &cfg, Scope::ThisPr, None),
        (None, _) => decided(local_facts(base)?, &cfg, Scope::ThisPr, None),
    }
}

#[cfg(test)]
#[rustfmt::skip]
mod tests {
    use super::*;

    const C: Cfg = Cfg::DEFAULT;

    #[test]
    fn run_rejects_a_bad_pr_number_and_a_missing_one_before_any_call() {
        let bad = run(Some("#abc"), None, Scope::Stack).unwrap_err();
        let empty = run(Some("#"), None, Scope::ThisPr).unwrap_err();
        let missing = run(None, None, Scope::Stack).unwrap_err();

        assert_eq!(bad, "`#abc` is not a PR number");
        assert_eq!(empty, "`#` is not a PR number");
        assert_eq!(missing, "--scope stack needs a PR number");
    }

    fn file(path: &str, lines: u64) -> File {
        File { path: path.into(), added: lines, removed: 0, renamed: false }
    }

    fn sug_with(files: Vec<File>, labels: &[&str], own: Option<bool>) -> Suggestion {
        let labels = labels.iter().map(|l| l.to_string()).collect();
        decide(measure(&Facts { files, labels, own }), &C)
    }

    fn sug(files: Vec<File>) -> Suggestion {
        sug_with(files, &[], Some(false))
    }

    /// One case per rule: the files that make it fire and the answer.
    fn cases() -> Vec<(&'static str, Vec<File>, Review)> {
        let many = (0..26).map(|i| file(&format!("src/f{i}.rs"), 1)).collect();
        let renamed = File { renamed: true, ..file("b.rs", 0) };
        vec![
            ("lock-only", vec![file("Cargo.lock", 900)], Review::Quick),
            ("docs-only", vec![file("docs/guide.md", 500), file("README.md", 5)], Review::Quick),
            ("renames-only", vec![renamed], Review::Quick),
            ("tests-only", vec![file("crates/a/tests/x.rs", 300), file("src/foo_test.go", 20)], Review::Quick),
            ("big-diff", vec![file("src/a.rs", 401), file("tests/a.rs", 100)], Review::Deep),
            ("many-files", many, Review::Deep),
            ("many-packages", vec![file("crates/a/x.rs", 5), file("crates/b/x.rs", 5), file("plugins/p/x.json", 5)], Review::Deep),
            ("security-paths", vec![file("src/auth/login.rs", 5)], Review::Deep),
            ("workflows", vec![file(".github/workflows/ci.yml", 3)], Review::Deep),
            ("release", vec![file("scripts/release.sh", 3)], Review::Deep),
            ("hooks-guards", vec![file("src/rm_guard.rs", 3)], Review::Deep),
            ("migrations", vec![file("db/migrations/001.sql", 3)], Review::Deep),
            ("dependencies", vec![file("requirements-dev.txt", 3)], Review::Deep),
            ("cli-surface", vec![file("crates/cli/src/main.rs", 3)], Review::Deep),
            ("plugin-payload", vec![file("plugins/p/skills/x/SKILL.md", 61)], Review::Deep),
            ("seed-prompt", vec![file("seed/prompts/SYSTEM_PROMPT.md", 61)], Review::Deep),
            ("untested-code", vec![file("src/a.py", 150)], Review::Deep),
            ("no-code", vec![file("README.md", 500), file("Cargo.lock", 500), file("tests/a.rs", 90)], Review::Quick),
            ("small", vec![file("src/a.rs", 40), file("tests/a.rs", 20)], Review::Quick),
        ]
    }

    #[test]
    fn every_rule_has_a_case_that_makes_it_fire_with_high_confidence() {
        let cases = cases();
        for rule in RULES.iter().filter(|r| r.id != "security-label") {
            let (_, files, want) = cases.iter().find(|c| c.0 == rule.id).unwrap_or_else(|| panic!("no case for {}", rule.id));
            let s = sug(files.clone());
            assert!(s.rules.contains(&rule.id), "{}: {:?}", rule.id, s.rules);
            assert_eq!((s.recommendation, s.confidence), (*want, Confidence::High), "{}", rule.id);
        }
    }

    #[test]
    fn the_security_label_forces_deep_even_for_docs() {
        let s = sug_with(vec![file("README.md", 2)], &["Security"], None);
        assert_eq!((s.recommendation, s.confidence), (Review::Deep, Confidence::High));
        assert_eq!(s.rules, ["security-label"]);
    }

    #[test]
    fn each_cut_is_exclusive_at_its_default() {
        let none = |files| sug(files).rules;
        assert!(!none(vec![file("src/a.rs", 300), file("tests/a.rs", 100)]).contains(&"big-diff"));
        let files = (0..25).map(|i| file(&format!("src/f{i}.rs"), 1)).collect();
        assert!(!none(files).contains(&"many-files"));
        assert!(!none(vec![file("crates/a/x.rs", 5), file("crates/b/x.rs", 5)]).contains(&"many-packages"));
        assert!(!none(vec![file("plugins/p/skills/x/SKILL.md", 60)]).contains(&"plugin-payload"));
        assert!(!none(vec![file("src/a.py", 149)]).contains(&"untested-code"));
        assert!(!none(vec![file("src/a.py", 150), file("tests/a.py", 30)]).contains(&"untested-code"));
        assert!(!none(vec![file("src/a.rs", 300)]).contains(&"untested-code"));
        assert!(none(vec![file("src/a.py", 226), file("tests/a.py", 24)]).contains(&"untested-code"));
        assert!(!none(vec![File { added: 3, ..File { renamed: true, ..file("b.rs", 0) } }]).contains(&"renames-only"));
    }

    #[test]
    fn plugin_markdown_is_not_docs_and_manifests_are_not_docs() {
        assert!(!sug(vec![file("plugins/p/commands/x.md", 200)]).rules.contains(&"docs-only"));
        assert!(!sug(vec![file("requirements.txt", 2)]).rules.contains(&"docs-only"));
    }

    #[test]
    fn security_paths_match_words_not_substrings() {
        for p in ["src/crypto.rs", "config/secrets.yml", ".env.prod", "src/oauth_client.py", "app/authentication.rs", "middleware/Authorization.py"] {
            assert!(sug(vec![file(p, 5)]).rules.contains(&"security-paths"), "{p}");
        }
        for p in ["src/author.rs", "src/tokens.rs", "src/authority.rs"] {
            assert!(!sug(vec![file(p, 5)]).rules.contains(&"security-paths"), "{p}");
        }
    }

    #[test]
    fn docs_tests_and_locks_hold_no_risky_area() {
        let s = sug(vec![file("docs/auth.md", 5), file("tests/auth/login.rs", 5), file("Cargo.lock", 5)]);
        assert!(s.signals.areas.is_empty(), "{:?}", s.signals.areas);
    }

    #[test]
    fn a_risk_beats_a_small_diff_and_reasons_stop_at_four() {
        let s = sug(vec![
            file(".github/workflows/ci.yml", 2), file("scripts/release.sh", 2), file("src/hooks/a.rs", 2),
            file("db/migrations/1.sql", 2), file("src/auth/a.rs", 2), file("Cargo.toml", 2),
        ]);
        assert!(!s.rules.contains(&"small") && s.rules.len() > 4);
        assert_eq!((s.recommendation, s.reasons.len()), (Review::Deep, 4));
    }

    #[test]
    fn a_close_call_is_low_confidence_and_leans_by_owner() {
        let files = vec![file("src/a.rs", 100), file("tests/a.rs", 50)];
        let theirs = sug_with(files.clone(), &[], Some(false));
        assert_eq!((theirs.recommendation, theirs.confidence), (Review::Deep, Confidence::Low));
        assert!(theirs.rules.is_empty());
        let mine = sug_with(files, &[], Some(true));
        assert_eq!((mine.recommendation, mine.confidence), (Review::Quick, Confidence::Low));
    }

    #[test]
    fn thresholds_come_from_the_config() {
        let cfg = Cfg { deep_lines: 10, ..C };
        let facts = Facts { files: vec![file("src/a.rs", 11), file("tests/a.rs", 5)], ..Facts::default() };
        assert!(decide(measure(&facts), &cfg).rules.contains(&"big-diff"));
    }

    #[test]
    fn numstat_reads_renames_with_and_without_braces() {
        let files = parse_numstat("3\t1\tsrc/a.rs\n0\t0\tsrc/{old => new}/b.rs\n0\t0\tx.rs => y.rs\n-\t-\timg.png\n");
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["src/a.rs", "src/new/b.rs", "y.rs", "img.png"]);
        assert_eq!(files.iter().map(|f| f.renamed).collect::<Vec<_>>(), [false, true, true, false]);
        assert_eq!(files[0].added + files[0].removed, 4);
        assert_eq!(new_path("{ => sub}/f.rs"), "sub/f.rs");
    }

    #[test]
    fn render_and_json_carry_the_answer() {
        let s = sug(vec![file("src/a.rs", 10)]);
        let text = render(&s);
        assert!(text.starts_with("recommendation=quick\nconfidence=high\nreason: Small change: 10 lines in 1 files."));
        assert!(text.contains("signals: lines=10 files=1 packages=1 test_ratio=0 areas=none"));
        let v = serde_json::to_value(s).unwrap();
        assert_eq!((v["recommendation"].as_str(), v["confidence"].as_str()), (Some("quick"), Some("high")));
        assert!(v["reasons"].is_array() && v["signals"]["lines"] == 10);
    }

    #[test]
    fn the_command_surface_covers_cmd_bin_and_command_files() {
        for p in ["cmd/root.go", "src/bin/tool.rs", "crates/telemetry/src/command.rs"] {
            assert!(sug(vec![file(p, 5)]).rules.contains(&"cli-surface"), "{p}");
        }
    }

    #[test]
    fn a_renamed_workflow_is_not_a_quiet_rename() {
        let mv = File { renamed: true, ..file(".github/workflows/ci.yml", 0) };
        let s = sug(vec![mv]);
        assert_eq!(s.recommendation, Review::Deep);
        assert!(!s.rules.contains(&"renames-only") && !s.rules.contains(&"no-code"));
    }

    #[test]
    fn docs_and_lock_lines_do_not_make_a_change_large_or_wide() {
        let s = sug(vec![file("crates/a/src/x.py", 10), file("README.md", 900), file("Cargo.lock", 900)]);
        assert!(!s.rules.contains(&"big-diff"));
        assert_eq!(s.signals.packages, ["a"]);
        assert!(s.rules.contains(&"small"));
    }

    #[test]
    fn reasons_name_the_lines_each_rule_checked() {
        let s = sug(vec![file("src/a.py", 150), file("docs/a.md", 500)]);
        assert!(s.reasons.iter().any(|r| r == "150 changed lines of code with almost no tests."), "{:?}", s.reasons);
    }

    #[test]
    fn the_by_hand_reference_lists_every_lock_manifest_and_cut() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins/playbook/skills/grounding-review/references/review-routing.md");
        let text = std::fs::read_to_string(path).unwrap();
        for name in LOCKS.iter().chain(MANIFESTS) {
            assert!(text.contains(&format!("`{name}`")), "review-routing.md lacks `{name}`");
        }
        let d = Cfg::DEFAULT;
        for (cut, n) in [("Large change", d.deep_lines), ("Small change", d.quick_lines), ("Wide change", d.deep_files), ("Cross-cutting", d.package_span), ("Untested code", d.untested_lines)] {
            let row = text.lines().find(|l| l.starts_with(&format!("| {cut} |"))).unwrap_or_else(|| panic!("no row for {cut}"));
            assert!(row.contains(&n.to_string()), "{cut}: `{row}` lacks the value {n}");
            assert!(!row.contains("review.suggest"), "{cut}: `{row}` names a retired key");
        }
        for word in ["authentication", "oauth", "jwt", "rbac", "acl", "bun.lockb", "CODEOWNERS", "command.rs"] {
            assert!(text.contains(word), "review-routing.md lacks {word}");
        }
    }

    #[test]
    fn gh_output_becomes_parts_and_a_missing_field_is_an_error() {
        let files = "{\"path\":\"a.rs\",\"added\":3,\"removed\":1,\"renamed\":false}\n\n{\"path\":\"b.rs\",\"added\":0,\"removed\":0,\"renamed\":true}\n";
        let meta = r#"{"labels":[{"name":"security"}],"author":{"login":"ann"}}"#;
        let (f, labels, author) = parse_part(files, meta).unwrap();
        assert_eq!((f.len(), f[1].renamed), (2, true));
        assert_eq!((labels, author.as_str()), (vec!["security".to_string()], "ann"));
        assert!(parse_part("{\"path\":\"a.rs\"}", meta).unwrap_err().contains("bad file JSON"));
        assert!(parse_part(files, "{}").unwrap_err().contains("bad JSON"));
    }

    const FILES_LINE: &str = "{\"path\":\"a.rs\",\"added\":3,\"removed\":1,\"renamed\":false}";
    const META: &str = r#"{"labels":[{"name":"security"}],"author":{"login":"ann"}}"#;

    /// A fake `gh` that records every call and serves one PR.
    fn seam<'a>(calls: &'a std::sync::Mutex<Vec<String>>, me: Option<&'static str>) -> impl Fn(&[&str]) -> Result<String, String> + Sync + 'a {
        move |args: &[&str]| {
            calls.lock().unwrap().push(args.join(" "));
            match args {
                ["api", "--paginate", ..] => Ok(FILES_LINE.to_string()),
                ["pr", "view", ..] => Ok(META.to_string()),
                ["api", "/user", ..] => me.map(str::to_string).ok_or_else(|| "no login".to_string()),
                _ => Err("unexpected gh call".to_string()),
            }
        }
    }

    #[test]
    fn pr_part_returns_files_labels_and_author_without_asking_for_the_login() {
        let calls = std::sync::Mutex::new(Vec::new());
        let (files, labels, author) = pr_part(&seam(&calls, Some("ann")), "7").unwrap();
        assert_eq!(files, [File { path: "a.rs".into(), added: 3, removed: 1, renamed: false }]);
        assert_eq!((labels, author.as_str()), (vec!["security".to_string()], "ann"));
        assert!(calls.lock().unwrap().iter().all(|c| !c.contains("/user")));
    }

    #[test]
    fn login_returns_the_fake_login_and_none_on_failure() {
        let calls = std::sync::Mutex::new(Vec::new());
        assert_eq!(login(&seam(&calls, Some("bob"))).as_deref(), Some("bob"));
        assert_eq!(login(&seam(&calls, None)), None);
    }

    #[test]
    fn pr_facts_combines_the_part_and_the_login() {
        let calls = std::sync::Mutex::new(Vec::new());
        let mine = pr_facts(&seam(&calls, Some("ann")), "7").unwrap();
        assert_eq!((mine.own, mine.files.len(), mine.labels), (Some(true), 1, vec!["security".to_string()]));
        assert_eq!(pr_facts(&seam(&calls, Some("bob")), "7").unwrap().own, Some(false));
        assert_eq!(pr_facts(&seam(&calls, None), "7").unwrap().own, None);
    }

    macro_rules! fx {
        ($n:literal) => {
            include_str!(concat!("../../tests/fixtures/pr_stack/", $n))
        };
    }

    /// A fake `gh` over the `pr_stack` fixtures that records every call. PR
    /// `n` changes `f<n>.rs` by `n * 10` added lines, and the caller is `me`.
    fn stack_gh<'a>(calls: &'a std::sync::Mutex<Vec<String>>, api: &'static str, me: &'static str) -> impl Fn(&[&str]) -> Result<String, String> + Sync + 'a {
        move |args: &[&str]| {
            calls.lock().unwrap().push(args.join(" "));
            match args {
                ["repo", "view", ..] => Ok(fx!("repo.json").to_string()),
                ["api", "graphql", ..] => Ok(match api {
                    "api-none" => fx!("api-none.json"),
                    "api-5" => fx!("api-5.json"),
                    "api-bottom-merged" => fx!("api-bottom-merged.json"),
                    "api-one-open" => fx!("api-one-open.json"),
                    "api-top-closed" => fx!("api-top-closed.json"),
                    other => panic!("no fixture {other}"),
                }.to_string()),
                ["api", "--paginate", endpoint, ..] => {
                    let n: u64 = endpoint.split('/').nth(4).and_then(|n| n.parse().ok()).unwrap();
                    Ok(format!("{{\"path\":\"f{n}.rs\",\"added\":{},\"removed\":0,\"renamed\":false}}", n * 10))
                }
                ["pr", "view", _, "--json", "labels,author"] => Ok(r#"{"labels":[],"author":{"login":"ana"}}"#.to_string()),
                ["pr", "view", "3", ..] => Ok(fx!("pr-3.json").to_string()),
                ["pr", "list", ..] => Ok("[]".to_string()),
                ["api", "/user", ..] => Ok(me.to_string()),
                _ => Err("unexpected gh call".to_string()),
            }
        }
    }

    fn fetched(calls: &std::sync::Mutex<Vec<String>>) -> Vec<String> {
        calls.lock().unwrap().iter().filter(|c| c.contains("/files")).cloned().collect()
    }

    fn scored(r: Result<StackFacts, String>) -> (Facts, StackScope) {
        match r.unwrap() {
            StackFacts::Scored(f, s) => (f, s),
            other => panic!("not scored: {other:?}"),
        }
    }

    fn part(files: Vec<File>, labels: &[&str], own: Option<bool>) -> Facts {
        Facts { files, labels: labels.iter().map(|l| l.to_string()).collect(), own }
    }

    #[test]
    fn merge_sums_lines_per_path() {
        let a = File { added: 30, removed: 5, ..file("a.rs", 0) };
        let b = File { added: 20, removed: 1, ..file("a.rs", 0) };
        let m = merge_facts(vec![part(vec![a], &[], None), part(vec![b], &[], None)]);
        assert_eq!(m.files.len(), 1);
        assert_eq!((m.files[0].added, m.files[0].removed), (50, 6));
        let s = measure(&m);
        assert_eq!((s.files, s.lines), (1, 56));
    }

    #[test]
    fn merge_keeps_a_rename_only_when_every_part_renamed() {
        let mv = File { renamed: true, ..file("a.rs", 0) };
        let both = merge_facts(vec![part(vec![mv.clone()], &[], None), part(vec![mv.clone()], &[], None)]);
        let one = merge_facts(vec![part(vec![mv], &[], None), part(vec![file("a.rs", 3)], &[], None)]);
        assert_eq!((both.files[0].renamed, one.files[0].renamed), (true, false));
    }

    #[test]
    fn merge_unions_labels_once() {
        let m = merge_facts(vec![part(vec![], &["security"], None), part(vec![], &["security", "bug"], None)]);
        assert_eq!(m.labels, ["security", "bug"]);
    }

    #[test]
    fn merge_owner_needs_every_part_to_be_yours() {
        let own = |owns: &[Option<bool>]| merge_facts(owns.iter().map(|o| part(vec![], &[], *o)).collect()).own;
        assert_eq!(own(&[Some(true), Some(true)]), Some(true));
        assert_eq!(own(&[Some(true), None]), None);
        assert_eq!(own(&[Some(true), Some(false)]), Some(false));
        assert_eq!(own(&[Some(false), Some(false)]), Some(false));
        assert_eq!(own(&[Some(false), None]), Some(false));
        assert_eq!(own(&[None, None]), None);
    }

    #[test]
    fn merge_skips_an_empty_part_and_keeps_the_rest() {
        let m = merge_facts(vec![part(vec![], &["a"], Some(true)), part(vec![file("b.rs", 10)], &["b"], Some(true))]);
        assert_eq!((m.files.iter().map(|f| (f.path.as_str(), f.added)).collect::<Vec<_>>(), m.labels, m.own), (vec![("b.rs", 10)], vec!["a".to_string(), "b".to_string()], Some(true)));
    }

    #[test]
    fn a_stack_is_scored_as_one_change_not_a_sum_of_changes() {
        let stack = merge_facts((0..3).map(|i| part(vec![file(&format!("crates/a/src/x{i}.rs"), 30)], &[], Some(false))).collect());
        let single = Facts { files: vec![file("crates/a/src/x.rs", 90)], labels: vec![], own: Some(false) };
        let (a, b) = (decide(measure(&stack), &C), decide(measure(&single), &C));
        assert_eq!((a.recommendation, a.confidence, a.rules), (b.recommendation, b.confidence, b.rules));
    }

    #[test]
    fn the_stack_holds_open_prs_only_and_asks_for_the_login_once() {
        let calls = std::sync::Mutex::new(Vec::new());
        let (facts, scope) = scored(stack_facts(&stack_gh(&calls, "api-bottom-merged", "ana"), "3", 6));
        assert_eq!(scope, StackScope { open: vec![2, 3], merged: vec![1], skipped: vec![] });
        let mut paths: Vec<_> = facts.files.iter().map(|f| (f.path.as_str(), f.added)).collect();
        paths.sort();
        assert_eq!(paths, [("f2.rs", 20), ("f3.rs", 30)]);
        assert_eq!(facts.own, Some(true));
        assert_eq!(calls.lock().unwrap().iter().filter(|c| c.starts_with("api /user")).count(), 1);
        assert!(fetched(&calls).iter().all(|c| !c.contains("pulls/1/")));
    }

    #[test]
    fn one_open_pr_in_a_two_pr_stack_is_its_own_facts() {
        let calls = std::sync::Mutex::new(Vec::new());
        let (facts, scope) = scored(stack_facts(&stack_gh(&calls, "api-one-open", "ana"), "2", 6));
        assert_eq!(scope, StackScope { open: vec![2], merged: vec![1], skipped: vec![] });
        let alone = pr_facts(&stack_gh(&calls, "api-one-open", "ana"), "2").unwrap();
        assert_eq!((facts.files, facts.labels, facts.own), (alone.files, alone.labels, alone.own));
    }

    #[test]
    fn a_closed_pr_is_skipped_and_never_fetched() {
        let calls = std::sync::Mutex::new(Vec::new());
        let (facts, scope) = scored(stack_facts(&stack_gh(&calls, "api-top-closed", "ana"), "2", 6));
        assert_eq!(scope, StackScope { open: vec![1, 2], merged: vec![], skipped: vec![3] });
        assert_eq!(facts.files.len(), 2);
        assert!(fetched(&calls).iter().all(|c| !c.contains("pulls/3/")));
    }

    #[test]
    fn a_closed_or_merged_given_pr_is_an_error_before_any_fetch() {
        let calls = std::sync::Mutex::new(Vec::new());
        let closed = stack_facts(&stack_gh(&calls, "api-top-closed", "ana"), "3", 6).unwrap_err();
        assert_eq!(closed, "PR #3 is closed; nothing to review");
        let merged = stack_facts(&stack_gh(&calls, "api-bottom-merged", "ana"), "1", 6).unwrap_err();
        assert_eq!(merged, "PR #1 is merged; nothing to review");
        assert!(fetched(&calls).is_empty());
    }

    #[test]
    fn a_pr_outside_a_stack_falls_back_to_its_own_facts() {
        let calls = std::sync::Mutex::new(Vec::new());
        let got = stack_facts(&stack_gh(&calls, "api-none", "ana"), "3", 6).unwrap();
        match got {
            StackFacts::Alone(f) => assert_eq!(f.files.len(), 1),
            other => panic!("not alone: {other:?}"),
        }
    }

    #[test]
    fn over_the_pr_budget_nothing_is_fetched_and_the_sums_come_from_the_stack() {
        let calls = std::sync::Mutex::new(Vec::new());
        match stack_facts(&stack_gh(&calls, "api-5", "ana"), "3", 4).unwrap() {
            StackFacts::OverBudget(scope, signals) => {
                assert_eq!(scope.open.len(), 5);
                assert_eq!((signals.lines, signals.files), (60, 5));
            }
            other => panic!("not over budget: {other:?}"),
        }
        assert!(fetched(&calls).is_empty());
    }

    #[test]
    fn the_over_budget_sums_count_open_prs_only() {
        let calls = std::sync::Mutex::new(Vec::new());
        match stack_facts(&stack_gh(&calls, "api-bottom-merged", "ana"), "3", 1).unwrap() {
            StackFacts::OverBudget(scope, signals) => {
                assert_eq!((scope.open, scope.merged), (vec![2, 3], vec![1]));
                assert_eq!((signals.lines, signals.review_lines), (24, 24));
            }
            other => panic!("not over budget: {other:?}"),
        };
    }

    #[test]
    fn ownership_follows_the_login_and_is_unknown_without_one() {
        let calls = std::sync::Mutex::new(Vec::new());
        let other = stack_facts(&stack_gh(&calls, "api-bottom-merged", "bob"), "3", 6);
        assert_eq!(scored(other).0.own, Some(false));
        let inner = stack_gh(&calls, "api-bottom-merged", "ana");
        let no_login = |args: &[&str]| if args.contains(&"/user") { Err("no".to_string()) } else { inner(args) };
        assert_eq!(scored(stack_facts(&no_login, "3", 6)).0.own, None);
    }

    #[test]
    fn a_non_numeric_pr_is_refused_before_any_call() {
        let calls = std::sync::Mutex::new(Vec::new());
        let bad = stack_facts(&stack_gh(&calls, "api-bottom-merged", "ana"), "abc", 6).unwrap_err();
        assert!(bad.contains("is not a PR number"), "{bad}");
        assert!(fetched(&calls).is_empty());
    }

    #[test]
    fn at_the_pr_budget_the_stack_is_still_fetched() {
        let calls = std::sync::Mutex::new(Vec::new());
        let (facts, scope) = scored(stack_facts(&stack_gh(&calls, "api-5", "ana"), "3", 5));
        assert_eq!((scope.open.len(), facts.files.len(), fetched(&calls).len()), (5, 5, 5));
    }

    #[test]
    fn a_failed_fetch_fails_the_whole_stack() {
        let calls = std::sync::Mutex::new(Vec::new());
        let inner = stack_gh(&calls, "api-bottom-merged", "ana");
        let failing = |args: &[&str]| if args.iter().any(|a| a.contains("pulls/2/files")) { Err("boom".to_string()) } else { inner(args) };
        assert_eq!(stack_facts(&failing, "3", 6).unwrap_err(), "boom");
    }

    #[test]
    fn scope_serialises_in_kebab_case() {
        assert_eq!(serde_json::to_string(&[Scope::ThisPr, Scope::Stack]).unwrap(), r#"["this-pr","stack"]"#);
    }

    #[test]
    fn a_branch_name_is_refused_as_a_pr_number() {
        assert!(run(Some("feat/x"), None, Scope::ThisPr).unwrap_err().contains("not a PR number"));
    }

    #[test]
    fn a_stack_scope_needs_a_pr_number() {
        assert_eq!(run(None, None, Scope::Stack).unwrap_err(), "--scope stack needs a PR number");
    }

    #[test]
    fn render_adds_the_scope_line_after_the_signals_and_the_stack_line_last() {
        // Arrange
        let mut s = sug(vec![file("src/a.rs", 10)]);
        let alone = render(&s);
        s.scope = Scope::Stack;
        s.stack = Some(StackScope { open: vec![2, 3], merged: vec![1], skipped: vec![] });
        // Act
        let stacked = render(&s);
        // Assert
        let lines: Vec<&str> = alone.lines().collect();
        assert!(lines[lines.len() - 2].starts_with("signals: "));
        assert_eq!(lines[lines.len() - 1], "scope=this-pr");
        let lines: Vec<&str> = stacked.lines().collect();
        assert_eq!(lines[lines.len() - 2], "scope=stack");
        assert_eq!(lines[lines.len() - 1], "stack: open=#2,#3 merged=#1 skipped=");
    }

    #[test]
    fn json_names_the_scope_and_drops_an_absent_stack() {
        // Arrange
        let mut s = sug(vec![file("src/a.rs", 10)]);
        // Act
        let alone = serde_json::to_value(&s).unwrap();
        s.stack = Some(StackScope { open: vec![2], merged: vec![], skipped: vec![] });
        let stacked = serde_json::to_value(&s).unwrap();
        // Assert
        assert_eq!(alone["scope"], "this-pr");
        assert!(alone.get("stack").is_none());
        assert_eq!(stacked["stack"]["open"][0], 2);
    }

    #[test]
    fn over_budget_answers_deep_and_high_without_a_rule_from_the_table() {
        // Arrange
        let found = StackFacts::OverBudget(
            StackScope { open: vec![1, 2, 3], merged: vec![], skipped: vec![] },
            Signals { lines: 36, files: 3, ..Signals::default() },
        );
        // Act
        let s = from_stack(found, &C, 2).unwrap();
        // Assert
        assert_eq!((s.recommendation, s.confidence, s.scope), (Review::Deep, Confidence::High, Scope::Stack));
        assert_eq!(s.rules, vec!["stack-over-budget"]);
        assert_eq!(s.reasons, vec!["3 open PRs in the stack, over the limit of 2."]);
        assert_eq!(s.signals.lines, 36);
    }

    #[test]
    fn a_scored_stack_reports_its_scope_and_an_alone_pr_reports_this_pr() {
        // Arrange
        let facts = || part(vec![file("src/a.rs", 10)], &[], Some(true));
        let stack = StackScope { open: vec![2, 3], merged: vec![1], skipped: vec![] };
        // Act
        let scored = from_stack(StackFacts::Scored(facts(), stack.clone()), &C, 6).unwrap();
        let alone = from_stack(StackFacts::Alone(facts()), &C, 6).unwrap();
        // Assert
        assert_eq!((scored.scope, scored.stack), (Scope::Stack, Some(stack)));
        assert_eq!((alone.scope, alone.stack), (Scope::ThisPr, None));
    }

    #[test]
    fn a_stack_with_no_changed_files_is_an_error() {
        let found = StackFacts::Alone(part(vec![], &[], None));
        assert!(from_stack(found, &C, 6).unwrap_err().contains("no changed files"));
    }
}
