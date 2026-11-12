// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook plan brief`: the per-Work-Unit brief files `/playbook:implement`
//! hands its implementer subagents. Each brief is the WU's own section of the
//! plan, verbatim, under a short header (worktree path, scoped verify), plus
//! the memory facts anchored to the WU's files. It used to be a Haiku agent
//! copying a table into files, which is a split on headings, not a judgment.

use crate::json::memorycontext::{parse_graph, Graph, Node};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// The heading of the code-facts block in a brief. `briefs.md` names the
/// same heading for review briefs.
pub const CODE_FACTS_HEADING: &str = "## Callers";
/// Most names one brief looks up in the index.
const MAX_FACT_NAMES: usize = 8;
/// Most use sites kept for one name.
const MAX_REFS_PER_NAME: usize = 3;
/// A name shorter than this is too common to look up.
const MIN_FACT_NAME_LEN: usize = 3;
/// The role `playbook code refs` gives a plain use of a name.
const USE_ROLE: &str = "use";

/// One Work Unit's section of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wu {
    /// `WU-3`, as written in the heading.
    pub id: String,
    pub title: String,
    /// The heading line and everything under it, up to the next heading of the
    /// same or a higher level.
    pub section: String,
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    (hashes > 0 && line[hashes..].starts_with(' ')).then(|| (hashes, line[hashes..].trim()))
}

/// `WU-3: Title`, `WU-3 - Title` or `WU-3 Title` to (`WU-3`, `Title`).
fn split_wu(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix("WU-")?;
    let digits = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
        .unwrap_or(rest.len());
    if digits == 0 {
        return None;
    }
    let id = format!("WU-{}", &rest[..digits]);
    let title = rest[digits..]
        .trim_start_matches([':', '-', ' ', '\t'])
        .trim()
        .to_string();
    Some((id, title))
}

/// Every `### WU-N` or `#### WU-N` section in `plan`, in file order.
pub fn parse_wus(plan: &str) -> Vec<Wu> {
    let lines: Vec<&str> = plan.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some((level, text)) = heading(lines[i]) else {
            i += 1;
            continue;
        };
        let Some((id, title)) = split_wu(text).filter(|_| (3..=4).contains(&level)) else {
            i += 1;
            continue;
        };
        let mut end = i + 1;
        while end < lines.len() && !heading(lines[end]).is_some_and(|(l, _)| l <= level) {
            end += 1;
        }
        let section = lines[i..end].join("\n").trim_end().to_string();
        out.push(Wu { id, title, section });
        i = end;
    }
    out
}

fn clean_path(token: &str) -> Option<String> {
    let t = token
        .replace(['`', '*'], "")
        .split(" | ")
        .next()
        .unwrap_or("")
        .split(" (")
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    (!t.is_empty() && (t.contains('/') || t.contains('.')) && !t.contains(' ')).then_some(t)
}

/// The paths a section lists under `Files:`: comma separated on the line
/// itself (`src/a.rs, src/b.rs`), or as indented bullets below it
/// (`- src/a.rs | create | note`).
pub fn files_of(section: &str) -> Vec<String> {
    let lines: Vec<&str> = section.lines().collect();
    let Some(at) = lines
        .iter()
        .position(|l| l.to_ascii_lowercase().contains("files:"))
    else {
        return Vec::new();
    };
    let line = lines[at];
    let indent = line.len() - line.trim_start().len();
    let from = line.to_ascii_lowercase().find("files:").unwrap_or(0) + "files:".len();
    let mut out: Vec<String> = line[from..].split(',').filter_map(clean_path).collect();
    for next in &lines[at + 1..] {
        let trimmed = next.trim_start();
        let deeper = next.len() - trimmed.len() > indent;
        if !deeper || !(trimmed.starts_with("- ") || trimmed.starts_with("* ")) {
            break;
        }
        out.extend(clean_path(&trimmed[2..]));
    }
    out
}

fn is_test_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    ["test", "spec"].iter().any(|w| p.contains(w))
}

/// `{files}` and `{tests}` in `template` replaced by the WU's files and by its
/// test files, space separated.
pub fn scoped_verify(template: &str, files: &[String]) -> String {
    let tests: Vec<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|f| is_test_path(f))
        .collect();
    template
        .replace("{files}", &files.join(" "))
        .replace("{tests}", &tests.join(" "))
}

/// An anchor (`dir`, `file` or `file#symbol`) names the same place as a file
/// when it is that file, a folder holding it, or a path inside that file's
/// folder when the WU lists a folder.
fn overlaps(anchor: &str, file: &str) -> bool {
    let a = anchor
        .split('#')
        .next()
        .unwrap_or(anchor)
        .trim_end_matches('/');
    let f = file.trim_end_matches('/');
    !a.is_empty()
        && !f.is_empty()
        && (a == f || f.starts_with(&format!("{a}/")) || a.starts_with(&format!("{f}/")))
}

fn in_scope(node: &Node<'_>, repo: &str) -> bool {
    let project = node.project.as_deref();
    let owner = repo
        .split('/')
        .next()
        .filter(|o| !repo.is_empty() && !o.is_empty());
    match node.scope.as_deref() {
        Some("global") => true,
        Some("project") => project == Some(repo),
        Some("org") => owner.is_some() && project == owner,
        _ => false,
    }
}

/// Title words worth matching: five or more letters, lower case.
fn keywords(title: &str) -> Vec<String> {
    title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() >= 5)
        .map(str::to_ascii_lowercase)
        .collect()
}

/// `name: description` for each in-scope fact anchored to one of `files`, then
/// each other in-scope fact whose description mentions a word of `title`.
pub fn relevant_facts(graph: &Graph<'_>, repo: &str, files: &[String], title: &str) -> Vec<String> {
    let Some(nodes) = graph.nodes.as_deref() else {
        return Vec::new();
    };
    let by_id: BTreeMap<&str, &Node<'_>> = nodes
        .iter()
        .filter_map(|n| n.id.as_deref().map(|id| (id, n)))
        .collect();
    let mut anchored: HashSet<&str> = HashSet::new();
    for e in &graph.edges {
        if e.relation.or_empty() != "anchors" {
            continue;
        }
        let (Some(from), Some(to)) = (e.from.as_deref(), e.to.as_deref()) else {
            continue;
        };
        let path = by_id.get(to).map_or("", |n| n.file.or_empty());
        if files.iter().any(|f| overlaps(path, f)) {
            anchored.insert(from);
        }
    }
    let words = keywords(title);
    let mut facts: Vec<&Node<'_>> = nodes.iter().filter(|n| in_scope(n, repo)).collect();
    facts.sort_by_key(|n| n.name.or_empty());
    let line = |n: &Node<'_>| format!("- {}: {}", n.name.or_empty(), n.description.or_empty());
    let mut out: Vec<String> = facts
        .iter()
        .filter(|n| n.id.as_deref().is_some_and(|id| anchored.contains(id)))
        .map(|n| line(n))
        .collect();
    for n in &facts {
        let desc = n.description.or_empty().to_ascii_lowercase();
        let hit = n.id.as_deref().is_some_and(|id| !anchored.contains(id))
            && words.iter().any(|w| desc.contains(w.as_str()));
        if hit {
            out.push(line(n));
        }
    }
    out
}

/// Where to work and how to verify, for the header of each brief.
pub struct Context<'a> {
    pub plan_path: &'a str,
    /// One path for every WU, or `base/<wu-id>` per WU.
    pub worktree: Option<&'a str>,
    pub worktree_base: Option<&'a str>,
    pub verify: Option<&'a str>,
    pub repo: &'a str,
    pub graph_json: Option<&'a str>,
    pub code_facts: Option<CodeFacts<'a>>,
}

/// The code facts of a brief and the token budget that bounds them.
pub struct CodeFacts<'a> {
    /// The `--facts-tokens` budget. 0 means no block.
    pub budget_tokens: usize,
    /// Ranked facts for one Work Unit, best first.
    pub source: &'a dyn Fn(&Wu) -> Vec<String>,
}

/// The code-facts block for `facts`, best ranked first. Facts are cut from
/// the lowest ranked end until the block fits `budget_tokens`, counted with
/// the tool map's estimator. Empty when the budget is 0 or no fact fits.
pub fn code_facts_block(facts: &[String], budget_tokens: usize) -> String {
    let block = |n: usize| format!("\n{CODE_FACTS_HEADING}\n\n{}\n", facts[..n].join("\n"));
    if budget_tokens == 0 {
        return String::new();
    }
    (1..=facts.len())
        .rev()
        .map(block)
        .find(|text| crate::tools_map::tokens(text) <= budget_tokens)
        .unwrap_or_default()
}

/// Names a section puts in backticks and that look like identifiers, in the
/// order they first appear.
fn backticked_names(section: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (i, token) in section.split('`').enumerate() {
        let name = token.trim();
        let ident = i % 2 == 1
            && name.len() >= MIN_FACT_NAME_LEN
            && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if ident && seen.insert(name.to_string()) {
            out.push(name.to_string());
        }
    }
    out
}

/// Use sites from the index of the names a Work Unit's section lists, one
/// line each, in the order the names appear. An unusable index gives none.
pub fn index_facts(wu: &Wu, cwd: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for name in backticked_names(&wu.section)
        .into_iter()
        .take(MAX_FACT_NAMES)
    {
        let mut opts = playbook_code::refs::RefsOptions::new(&name);
        opts.limit = MAX_REFS_PER_NAME;
        let Ok(found) = playbook_code::find_refs(cwd, &opts) else {
            return Vec::new();
        };
        out.extend(
            found
                .result
                .refs
                .iter()
                .filter(|r| r.role == USE_ROLE)
                .map(|r| format!("- `{name}` at {}:{}: {}", r.path, r.line, r.text.trim())),
        );
    }
    out
}

/// The text of one WU's brief.
pub fn render(wu: &Wu, ctx: &Context<'_>) -> String {
    let files = files_of(&wu.section);
    let worktree = match (ctx.worktree, ctx.worktree_base) {
        (Some(w), _) => w.to_string(),
        (None, Some(base)) => format!(
            "{}/{}",
            base.trim_end_matches('/'),
            wu.id.to_ascii_lowercase()
        ),
        (None, None) => "the current tree".to_string(),
    };
    let mut out = format!(
        "# Brief: {} {}\n\nPlan: {}\nWorktree: {worktree}\n",
        wu.id, wu.title, ctx.plan_path
    );
    if let Some(v) = ctx.verify {
        out.push_str(&format!("Scoped verify: `{}`\n", scoped_verify(v, &files)));
    }
    out.push('\n');
    out.push_str(&wu.section);
    out.push('\n');
    let facts = ctx
        .graph_json
        .and_then(parse_graph)
        .map(|g| relevant_facts(&g, ctx.repo, &files, &wu.title))
        .unwrap_or_default();
    if !facts.is_empty() {
        out.push_str("\n## Relevant memory\n\n");
        out.push_str(&facts.join("\n"));
        out.push('\n');
    }
    if let Some(code) = &ctx.code_facts {
        out.push_str(&code_facts_block(&(code.source)(wu), code.budget_tokens));
    }
    out
}

/// Write `<out>/<wu-id>.brief.md` for each of `ids`. All ids are checked
/// before anything is written; `Err` names the ones the plan lacks.
pub fn write_briefs(
    plan: &str,
    ids: &[String],
    out_dir: &Path,
    ctx: &Context<'_>,
) -> Result<Vec<PathBuf>, String> {
    let wus = parse_wus(plan);
    let mut picked = Vec::new();
    let mut missing = Vec::new();
    for id in ids {
        match wus.iter().find(|w| w.id.eq_ignore_ascii_case(id)) {
            Some(w) => picked.push(w),
            None => missing.push(id.as_str()),
        }
    }
    if !missing.is_empty() {
        let have: Vec<&str> = wus.iter().map(|w| w.id.as_str()).collect();
        return Err(format!(
            "the plan has no section for {} (it has: {})",
            missing.join(", "),
            if have.is_empty() {
                "none".into()
            } else {
                have.join(", ")
            }
        ));
    }
    std::fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let mut paths = Vec::new();
    for wu in picked {
        let path = out_dir.join(format!("{}.brief.md", wu.id.to_ascii_lowercase()));
        std::fs::write(&path, render(wu, ctx)).map_err(|e| format!("{}: {e}", path.display()))?;
        paths.push(path);
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = "## Implementation Plan\n\n### Deliverables (Work Units)\n| WU | Title |\n|----|----|\n| WU-0 | Types |\n\n### Per-Work-Unit Detail\n\n#### WU-0: Types and schema\n- **Requires:** nothing\n- **Files:** `src/types.rs`, `src/types_test.rs` (new)\n- **Changes:** add the types\n- **Done When:**\n  - [ ] compiles\n\n#### WU-1: Wire it up\n- **Files:** src/app.rs\n- **Changes:** call the types\n\n### Testing Strategy\n- later\n";

    #[test]
    fn sections_split_on_wu_headings_and_stop_at_the_next_heading() {
        let wus = parse_wus(PLAN);
        assert_eq!(wus.len(), 2);
        assert_eq!(
            (wus[0].id.as_str(), wus[0].title.as_str()),
            ("WU-0", "Types and schema")
        );
        assert!(wus[0].section.contains("add the types") && wus[0].section.contains("compiles"));
        assert!(!wus[0].section.contains("Wire it up"));
        assert!(!wus[1].section.contains("Testing Strategy"));
        assert!(wus[1].section.ends_with("call the types"));
    }

    #[test]
    fn blueprint_headings_one_level_up_parse_too() {
        let text = "## Work Units\n\n### WU-0: Native fallback\nbody a\n\n### WU-1: Docs\nbody b\n\n## Ordering\nx\n";
        let wus = parse_wus(text);
        assert_eq!(wus.len(), 2);
        assert_eq!(wus[1].section, "### WU-1: Docs\nbody b");
    }

    #[test]
    fn a_table_row_or_a_wu_in_prose_is_not_a_section() {
        assert!(parse_wus("| WU-0 | x |\nSee WU-1 below.\n").is_empty());
        assert!(parse_wus("## WU-1: too shallow\nx\n").is_empty());
    }

    #[test]
    fn files_come_from_the_files_line_without_backticks_or_notes() {
        let wus = parse_wus(PLAN);
        assert_eq!(
            files_of(&wus[0].section),
            ["src/types.rs", "src/types_test.rs"]
        );
        assert_eq!(files_of(&wus[1].section), ["src/app.rs"]);
        assert!(files_of("no such line").is_empty());
    }

    #[test]
    fn files_listed_as_nested_bullets_parse_and_stop_at_the_next_field() {
        let section = "### WU-1: x\n- Requires: WU-0\n- Files:\n  - `src/a.rs` | create | the a\n  - src/b.rs | modify | the b\n- Verification: `cargo test`\n- Tests: c.rs";
        assert_eq!(files_of(section), ["src/a.rs", "src/b.rs"]);
    }

    #[test]
    fn the_verify_template_fills_files_and_test_files() {
        let files = vec!["src/a.ts".to_string(), "src/a.test.ts".to_string()];
        assert_eq!(
            scoped_verify("vitest run {tests}", &files),
            "vitest run src/a.test.ts"
        );
        assert_eq!(
            scoped_verify("lint {files}", &files),
            "lint src/a.ts src/a.test.ts"
        );
    }

    #[test]
    fn anchors_overlap_by_file_folder_and_symbol() {
        assert!(overlaps("src/a.rs", "src/a.rs"));
        assert!(overlaps("src/a.rs#main", "src/a.rs"));
        assert!(overlaps("src", "src/a.rs"));
        assert!(overlaps("src/a.rs", "src"));
        assert!(!overlaps("src/ab.rs", "src/a.rs"));
        assert!(!overlaps("srcs", "src/a.rs"));
        assert!(!overlaps("", "src/a.rs"));
    }

    const GRAPH: &str = r#"{"nodes":[
      {"id":"g/anchored","name":"anchored","description":"covers the types","scope":"global"},
      {"id":"g/keyword","name":"keyword","description":"notes on the schema layer","scope":"global"},
      {"id":"g/other","name":"other","description":"unrelated","scope":"global"},
      {"id":"p/mine","name":"mine","description":"types in this repo","scope":"project","project":"o/r"},
      {"id":"p/theirs","name":"theirs","description":"types elsewhere","scope":"project","project":"o/x"},
      {"id":"a1","file":"src/types.rs"}
    ],"edges":[
      {"from":"g/anchored","to":"a1","relation":"anchors"},
      {"from":"p/theirs","to":"a1","relation":"anchors"}
    ]}"#;

    #[test]
    fn memory_is_anchored_facts_first_then_title_keywords_and_only_in_scope() {
        let g = parse_graph(GRAPH).unwrap();
        let files = vec!["src/types.rs".to_string()];
        let out = relevant_facts(&g, "o/r", &files, "Types and schema");
        assert_eq!(
            out,
            [
                "- anchored: covers the types",
                "- keyword: notes on the schema layer",
                "- mine: types in this repo"
            ]
        );
        // Nothing anchored, no keyword in common: no section at all.
        assert!(relevant_facts(&g, "o/r", &["docs/x.md".into()], "Docs").is_empty());
    }

    #[test]
    fn a_brief_carries_the_header_the_section_and_the_memory() {
        let wu = &parse_wus(PLAN)[0];
        let ctx = Context {
            plan_path: "plans/p.md",
            worktree: None,
            worktree_base: Some("/wt/p/"),
            verify: Some("cargo test {tests}"),
            repo: "o/r",
            graph_json: Some(GRAPH),
            code_facts: None,
        };
        let text = render(wu, &ctx);
        assert!(text.starts_with("# Brief: WU-0 Types and schema\n"));
        assert!(text.contains("Worktree: /wt/p/wu-0\n"));
        assert!(text.contains("Scoped verify: `cargo test src/types_test.rs`"));
        assert!(text.contains("#### WU-0: Types and schema"));
        assert!(text.contains("## Relevant memory\n\n- anchored: covers the types"));
    }

    #[test]
    fn a_brief_with_no_memory_has_no_empty_section() {
        let wu = &parse_wus(PLAN)[1];
        let ctx = Context {
            plan_path: "p.md",
            worktree: Some("/main"),
            worktree_base: None,
            verify: None,
            repo: "o/r",
            graph_json: None,
            code_facts: None,
        };
        let text = render(wu, &ctx);
        assert!(!text.contains("Relevant memory") && !text.contains("Scoped verify"));
        assert!(text.contains("Worktree: /main"));
    }

    #[test]
    fn write_briefs_checks_every_id_before_writing() {
        let dir = std::env::temp_dir().join(format!("pb-brief-{}", crate::testing::run_id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = Context {
            plan_path: "p.md",
            worktree: None,
            worktree_base: None,
            verify: None,
            repo: "o/r",
            graph_json: None,
            code_facts: None,
        };
        let err = write_briefs(PLAN, &["WU-0".into(), "WU-9".into()], &dir, &ctx).unwrap_err();
        assert!(err.contains("WU-9") && err.contains("WU-0, WU-1"), "{err}");
        assert!(
            !dir.exists(),
            "nothing may be written when an id is missing"
        );
        let paths = write_briefs(PLAN, &["wu-1".into(), "WU-0".into()], &dir, &ctx).unwrap();
        assert_eq!(paths.len(), 2);
        assert!(dir.join("wu-0.brief.md").is_file() && dir.join("wu-1.brief.md").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn facts(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| format!("- fact {i} at src/a.rs:{i}"))
            .collect()
    }

    fn block_tokens(facts: &[String]) -> usize {
        crate::tools_map::tokens(&code_facts_block(facts, usize::MAX))
    }

    #[test]
    fn a_block_exactly_at_the_budget_is_not_cut() {
        let f = facts(4);
        let exact = block_tokens(&f);
        let text = code_facts_block(&f, exact);
        assert!(text.contains("fact 3"), "{text}");
        assert!(text.starts_with(&format!("\n{CODE_FACTS_HEADING}\n\n")));
    }

    #[test]
    fn one_token_over_the_budget_cuts_the_lowest_ranked_fact() {
        let f = facts(4);
        let text = code_facts_block(&f, block_tokens(&f) - 1);
        assert!(
            text.contains("fact 2") && !text.contains("fact 3"),
            "{text}"
        );
    }

    #[test]
    fn facts_above_the_budget_are_cut_from_the_end_and_stay_within_it() {
        let f = facts(30);
        let budget = block_tokens(&f) / 2;
        let text = code_facts_block(&f, budget);
        assert!(crate::tools_map::tokens(&text) <= budget);
        assert!(
            text.contains("fact 0") && !text.contains("fact 29"),
            "{text}"
        );
    }

    #[test]
    fn a_zero_budget_or_no_fact_gives_no_block() {
        assert_eq!(code_facts_block(&facts(3), 0), "");
        assert_eq!(code_facts_block(&[], 100), "");
        assert_eq!(code_facts_block(&facts(3), 1), "");
    }

    #[test]
    fn the_brief_has_the_block_only_with_a_budget() {
        let wu = &parse_wus(PLAN)[1];
        let source = |_: &Wu| vec!["- `run` at src/app.rs:7: run();".to_string()];
        let mut ctx = Context {
            plan_path: "p.md",
            worktree: Some("/main"),
            worktree_base: None,
            verify: None,
            repo: "o/r",
            graph_json: None,
            code_facts: Some(CodeFacts {
                budget_tokens: 0,
                source: &source,
            }),
        };
        assert!(!render(wu, &ctx).contains("Callers"));
        ctx.code_facts = Some(CodeFacts {
            budget_tokens: 100,
            source: &source,
        });
        let text = render(wu, &ctx);
        assert!(
            text.ends_with("\n## Callers\n\n- `run` at src/app.rs:7: run();\n"),
            "{text}"
        );
        assert_eq!(text.matches("## Callers").count(), 1);
    }

    #[test]
    fn names_come_from_backticks_that_look_like_identifiers() {
        let names = backticked_names("call `run_it` and `src/a.rs`, then `run_it`, `ab`, `Foo`");
        assert_eq!(names, vec!["run_it", "Foo"]);
    }

    #[test]
    fn the_brief_with_a_budget_matches_the_golden_text() {
        let wu = &parse_wus(PLAN)[1];
        let source = |_: &Wu| {
            vec![
                "- `types` at src/app.rs:7: types::load();".to_string(),
                "- `types` at src/main.rs:12: types::save();".to_string(),
            ]
        };
        let ctx = Context {
            plan_path: "plans/p.md",
            worktree: Some("/main"),
            worktree_base: None,
            verify: Some("cargo test {tests}"),
            repo: "o/r",
            graph_json: None,
            code_facts: Some(CodeFacts {
                budget_tokens: 100,
                source: &source,
            }),
        };
        assert_eq!(
            render(wu, &ctx),
            include_str!("../tests/golden/brief_with_callers.md")
        );
    }

    #[test]
    fn briefs_md_has_one_callers_rule_gated_by_the_flag() {
        let rules = include_str!(
            "../../../plugins/playbook/skills/delegating-subagents/references/briefs.md"
        );
        assert_eq!(rules.matches("**Callers,").count(), 1);
        assert!(rules.contains("--facts-tokens"));
    }
}
