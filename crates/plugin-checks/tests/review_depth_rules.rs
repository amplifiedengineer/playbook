// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Pins how `/playbook:review` picks its depth: a flag, then the repo setting,
//! then the triage, with a failed triage always meaning deep. The checks read
//! text. Whether the model obeys it is the manual check of the review merge.

use std::fs;
use std::path::PathBuf;

/// One rule: a label and the phrases that must all appear in the depth steps.
struct Rule {
    label: &'static str,
    phrases: &'static [&'static str],
}

const RULES: &[Rule] = &[
    Rule {
        label: "all and preset mean deep",
        phrases: &["`--all` and `--preset` mean `deep`"],
    },
    Rule {
        label: "a repo setting of quick or deep is used as is",
        phrases: &["`quick` or `deep` is used as is", "`repo setting`"],
    },
    Rule {
        label: "a setting read error or an unknown value gives deep",
        phrases: &["A read error or any other value is `deep`"],
    },
    Rule {
        label: "the triage intent",
        phrases: &["Intent: pr-review-triage"],
    },
    Rule {
        label: "the triage first line gives quick or deep",
        phrases: &["`review=quick`", "`review=deep`"],
    },
    Rule {
        label: "a failed triage gives deep",
        phrases: &[
            "non-zero exit",
            "any other output",
            "no intent in the tool map",
        ],
    },
    Rule {
        label: "a whole stack without a flag gives deep",
        phrases: &["A whole-stack scope without a flag is `deep`"],
    },
    Rule {
        label: "the one Depth line",
        phrases: &["`Depth: <depth> (<reason>)`"],
    },
    Rule {
        label: "wait only in ask mode, interactive, without self",
        phrases: &[
            "only in ask mode",
            "interactive session",
            "without `--self`",
        ],
    },
    Rule {
        label: "never wait in auto mode, with self, with a flag or headless",
        phrases: &["Auto mode, `--self`, a flag or a headless run never waits"],
    },
    Rule {
        label: "an empty answer runs the stated depth",
        phrases: &["An empty answer runs the stated depth"],
    },
];

/// The reason order, as the text states it.
const REASON_ORDER: [&str; 3] = ["`signal:` line", "`note:` line", "the triage chose"];

/// The text of the sections that start with one of `titles`, each cut at the
/// next `##` heading.
fn sections(text: &str, titles: &[&str]) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            inside = titles.iter().any(|t| title.starts_with(t));
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The labels of the rules the text lacks, plus the structural problems.
fn problems(file: &str, depth_text: &str) -> Vec<String> {
    let mut out: Vec<String> = RULES
        .iter()
        .filter(|r| r.phrases.iter().any(|p| !depth_text.contains(p)))
        .map(|r| format!("missing rule: {}", r.label))
        .collect();
    if file.contains("pr.review.enabled") {
        out.push("review.md must not name pr.review.enabled".to_owned());
    }
    let at: Vec<Option<usize>> = REASON_ORDER.iter().map(|p| depth_text.find(p)).collect();
    if at.iter().any(Option::is_none) || !at.windows(2).all(|w| w[0] < w[1]) {
        out.push("the reasons must come in the order signal, note, the triage chose".to_owned());
    }
    let template = "`Depth: <depth> (<reason>)`";
    if file.matches(template).count() != 1 {
        out.push("the Depth line template must appear exactly once".to_owned());
    }
    out
}

fn real_review() -> String {
    let path = PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT")).join("commands/review.md");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn depth_steps(file: &str) -> String {
    sections(file, &["Step 2", "Step 3"])
}

#[test]
fn a_file_missing_one_depth_rule_fails_naming_that_rule() {
    // Arrange
    let real = real_review();
    let mut failures = Vec::new();

    // Act
    for rule in RULES {
        let mut text = real.clone();
        for phrase in rule.phrases {
            text = text.replace(phrase, "removed");
        }
        let found = problems(&text, &depth_steps(&text));
        if !found.iter().any(|p| p.contains(rule.label)) {
            failures.push(format!("{}: not reported, got {found:?}", rule.label));
        }
    }
    let enabled = format!("{real}\npr.review.enabled\n");
    let twice = format!("{real}\n`Depth: <depth> (<reason>)`\n");
    let reordered = real.replace("`signal:` line", "`zzz`");

    // Assert
    assert!(failures.is_empty(), "{failures:#?}");
    assert!(problems(&enabled, &depth_steps(&enabled))
        .iter()
        .any(|p| p.contains("pr.review.enabled")));
    assert!(problems(&twice, &depth_steps(&twice))
        .iter()
        .any(|p| p.contains("exactly once")));
    assert!(problems(&reordered, &depth_steps(&reordered))
        .iter()
        .any(|p| p.contains("order")));
}

#[test]
fn review_states_every_depth_rule() {
    // Arrange
    let file = real_review();

    // Act
    let found = problems(&file, &depth_steps(&file));

    // Assert
    assert!(found.is_empty(), "{found:#?}");
    assert!(
        file.contains("--quick and --deep conflict"),
        "Step 1 lost the conflict stop"
    );
}

/// The files under `crates/cli/src/pr/` that define all four review type
/// constants, with their text.
fn constant_files() -> Vec<(String, String)> {
    let root = PathBuf::from(env!("PLAYBOOK_REPO_ROOT")).join("crates/cli/src/pr");
    let mut stack = vec![root];
    let mut found = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("pr/ is readable").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = fs::read_to_string(&path).unwrap_or_default();
                let all = ["AUTO_REVIEW_TYPE", "TYPE_AUTO", "TYPE_QUICK", "TYPE_DEEP"]
                    .iter()
                    .all(|c| text.contains(&format!("const {c}")));
                if all {
                    found.push((path.display().to_string(), text));
                }
            }
        }
    }
    found
}

/// The string a `const NAME: &str = "value";` line holds.
fn const_value(text: &str, name: &str) -> Option<String> {
    let line = text
        .lines()
        .find(|l| l.contains(&format!("const {name}:")))?;
    Some(line.split('"').nth(1)?.to_owned())
}

/// The problems between the finish flow's constants and the review text.
fn depth_rule_problems(constants: &str, depth_text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let key = const_value(constants, "AUTO_REVIEW_TYPE").unwrap_or_default();
    if !depth_text.contains(&format!("`{key}`")) {
        out.push(format!("review.md does not name the key `{key}`"));
    }
    for (name, line) in [
        ("TYPE_QUICK", "`quick` or `deep` is used as is"),
        ("TYPE_DEEP", "`quick` or `deep` is used as is"),
    ] {
        let value = const_value(constants, name).unwrap_or_default();
        if !depth_text.contains(&format!("`{value}`")) || !depth_text.contains(line) {
            out.push(format!("review.md does not map `{value}` to itself"));
        }
    }
    let auto = const_value(constants, "TYPE_AUTO").unwrap_or_default();
    if !depth_text.contains(&format!("`{auto}`, the default, goes to the triage")) {
        out.push(format!("review.md does not map `{auto}` to the triage"));
    }
    for fail in [
        "A read error or any other value is `deep`",
        "A non-zero exit, any other output, or no intent in the tool map means `deep`",
    ] {
        if !depth_text.contains(fail) {
            out.push(format!("review.md lost `{fail}`"));
        }
    }
    out
}

#[test]
fn review_depth_rule_matches_the_finish_decision() {
    // Arrange
    let files = constant_files();
    let review = depth_steps(&real_review());
    let other_key = "const AUTO_REVIEW_TYPE: &str = \"pr.review.mode\";\n\
                     const TYPE_AUTO: &str = \"auto\";\n\
                     const TYPE_QUICK: &str = \"quick\";\n\
                     const TYPE_DEEP: &str = \"deep\";\n";
    let auto_to_quick = review.replace("goes to the triage", "runs a quick review");

    // Act
    let real = depth_rule_problems(&files[0].1, &review);
    let wrong_key = depth_rule_problems(other_key, &review);
    let wrong_auto = depth_rule_problems(&files[0].1, &auto_to_quick);

    // Assert
    assert_eq!(
        files.len(),
        1,
        "{:?}",
        files.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    assert!(real.is_empty(), "{real:#?}");
    assert!(!wrong_key.is_empty(), "another key must fail");
    assert!(!wrong_auto.is_empty(), "auto mapped to quick must fail");
}

#[test]
fn each_pass_keeps_its_routing_line_and_its_auto_posting_rule() {
    // Arrange
    let root =
        PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT")).join("skills/grounding-review/references");
    let mut failures = Vec::new();

    // Act
    for pass in ["quick-pass.md", "deep-pass.md"] {
        let text = fs::read_to_string(root.join(pass)).expect("a pass file");
        for needle in ["route the `review` kind", "### Posting in auto mode"] {
            if !text.contains(needle) {
                failures.push(format!("{pass} lacks `{needle}`"));
            }
        }
    }

    // Assert
    assert!(failures.is_empty(), "{failures:#?}");
}
