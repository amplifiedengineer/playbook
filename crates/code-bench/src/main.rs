// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `code-bench pilot` runs the pilot repos through the real binary and writes
//! the JSON report. It needs network for the first clone and never downloads
//! an oracle: a repo without an answers file reports no refs truth.
//! `code-bench gate` judges a report against the committed baseline.
//! `code-bench fixtures --write-baseline` rewrites the fixture baseline.
//! `code-bench mutants` judges a cargo-mutants run against the score floor.

use code_bench::corpus::{fetch, GitTransport, Manifest, ThreadSleeper, Tier};
use code_bench::e2e::{self, model::ClaudeCli, report::Report as E2eReport};
use code_bench::fixtures;
use code_bench::gate::{evaluate, Baseline, Floor, Inputs, Overrides, MIN_TASKS_PER_CELL};
use code_bench::mutants::{judge, score_from_outcomes, MutantsError, Verdict};
use code_bench::oracle::rg::Candidate;
use code_bench::process::{ProcessRunner, SystemRunner};
use code_bench::runner::{self, PlaybookBinary, RepoJob, Report};
use code_bench::scope::scope_to_language;
use code_bench::tasks::{TaskSet, TASKS_VERSION};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const DEFAULT_PROGRAM: &str = "playbook";
const ANSWERS_DIR: &str = "crates/code-bench/data/oracle-answers";
const HOME_DIR: &str = "home";
const DATA_DIR: &str = "crates/code-bench/data";
const WRITE_BASELINE_FLAG: &str = "--write-baseline";
const LANGUAGE_FLAG: &str = "--language";
const USAGE: &str = "usage: code-bench pilot [--language LANG] CACHE_DIR REPORT_FILE [PLAYBOOK_BINARY]\n       code-bench gate [--language LANG] REPORT_FILE RUN_DATE\n       code-bench mutants OUTCOMES_FILE\n       code-bench e2e CACHE_DIR REPORT_FILE [PLAYBOOK_BINARY]\n       code-bench decide REPORT_FILE\n       code-bench fixtures --write-baseline";

struct Args {
    language: Option<String>,
    cache: PathBuf,
    out: PathBuf,
    program: PathBuf,
}

/// Splits an optional leading `--language LANG` from the positional values.
fn split_language(args: &[String]) -> Result<(Option<String>, &[String]), String> {
    match args {
        [flag, language, rest @ ..] if flag == LANGUAGE_FLAG => Ok((Some(language.clone()), rest)),
        [flag] if flag == LANGUAGE_FLAG => Err(USAGE.to_string()),
        _ => Ok((None, args)),
    }
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let (language, args) = split_language(args)?;
    let [cache, out, rest @ ..] = args else {
        return Err(USAGE.to_string());
    };
    Ok(Args {
        language,
        cache: PathBuf::from(cache),
        out: PathBuf::from(out),
        program: PathBuf::from(rest.first().map_or(DEFAULT_PROGRAM, String::as_str)),
    })
}

/// The oracle answers of one repo. A missing file means no refs truth; a file
/// that does not parse is an error.
fn load_truth(dir: &Path, repo: &str) -> Result<BTreeMap<String, Vec<Candidate>>, String> {
    match std::fs::read_to_string(dir.join(format!("{repo}.json"))) {
        Ok(text) => runner::parse_answers(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e.to_string()),
    }
}

fn pilot(args: &Args) -> Result<(), String> {
    std::fs::create_dir_all(&args.cache).map_err(|e| e.to_string())?;
    let manifest = Manifest::bundled().map_err(|e| e.to_string())?;
    let tasks = TaskSet::bundled().map_err(|e| e.to_string())?;
    let mut jobs = Vec::new();
    let wanted = |language: &str| args.language.as_deref().is_none_or(|l| l == language);
    for repo in manifest.tier(Tier::Pilot).filter(|r| wanted(&r.language)) {
        jobs.push(RepoJob {
            repo: repo.name.clone(),
            checkout: fetch(repo, &args.cache, &GitTransport, &ThreadSleeper)
                .map_err(|e| e.to_string()),
            tasks: tasks
                .tasks
                .iter()
                .filter(|t| t.repo == repo.name)
                .cloned()
                .collect(),
            truth: load_truth(Path::new(ANSWERS_DIR), &repo.name)?,
        });
    }
    let binary = PlaybookBinary {
        program: args.program.clone(),
        home: args.cache.join(HOME_DIR),
    };
    let report = runner::run(&binary, &tasks, TASKS_VERSION, &jobs).map_err(|e| e.to_string())?;
    let text = report.to_json().map_err(|e| e.to_string())?;
    std::fs::write(&args.out, text).map_err(|e| e.to_string())
}

fn read_data(name: &str) -> Result<String, String> {
    let path = Path::new(DATA_DIR).join(name);
    std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Judges a report against the committed baseline, floor and overrides. A
/// language narrows the baseline to the repos of that language.
fn gate(args: &[String]) -> Result<(), String> {
    let (language, rest) = split_language(args)?;
    let [report_file, run_date] = rest else {
        return Err(USAGE.to_string());
    };
    let report: Report = serde_json::from_str(
        &std::fs::read_to_string(report_file).map_err(|e| format!("{report_file}: {e}"))?,
    )
    .map_err(|e| format!("{report_file}: {e}"))?;
    let baseline = Baseline::parse(&read_data("baseline.json")?).map_err(|e| e.to_string())?;
    let floor = Floor::parse(&read_data("floor.json")?).map_err(|e| e.to_string())?;
    let overrides = Overrides::parse(&read_data("overrides.json")?).map_err(|e| e.to_string())?;
    let (baseline, floor) = match language {
        Some(language) => {
            let manifest = Manifest::bundled().map_err(|e| e.to_string())?;
            scope_to_language(&manifest, &language, baseline, floor)
        }
        None => (baseline, floor),
    };
    let tasks = TaskSet::bundled().map_err(|e| e.to_string())?;
    let ids: BTreeSet<String> = tasks.tasks.iter().map(|t| t.id.clone()).collect();
    let failures = evaluate(&Inputs {
        baseline: &baseline,
        floor: &floor,
        overrides: &overrides,
        report: &report,
        run_date,
        prior_task_ids: &ids,
        task_ids: &ids,
        min_tasks_per_cell: MIN_TASKS_PER_CELL,
    });
    for failure in &failures {
        eprintln!("{failure}");
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("the gate found {} failures", failures.len()))
    }
}

/// Runs the fixtures through the real binary and writes `gate-baseline.json`.
fn write_fixture_baseline() -> Result<(), String> {
    let scratch = std::env::temp_dir().join(format!("code-bench-fixtures-{}", std::process::id()));
    let report = fixtures::binary(&scratch)
        .and_then(|binary| fixtures::measure(&binary, &scratch))
        .and_then(|report| fixtures::baseline_text(&report));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::write(fixtures::baseline_path(), report?).map_err(|e| e.to_string())
}

/// Judges the mutation score in a cargo-mutants `outcomes.json` against the
/// floor in `floor.json`. Without a committed floor the score is printed as
/// the first measurement and the run passes.
fn mutants(args: &[String]) -> Result<(), String> {
    let [outcomes_file] = args else {
        return Err(USAGE.to_string());
    };
    let floor = Floor::parse(&read_data("floor.json")?)
        .map_err(|e| e.to_string())?
        .mutation_floor_bps;
    let read = || {
        let text = std::fs::read_to_string(outcomes_file)
            .map_err(|e| MutantsError::Read(format!("{outcomes_file}: {e}")))?;
        score_from_outcomes(&text)
    };
    let verdict = judge(read, floor).map_err(|e| e.to_string())?;
    match &verdict {
        Verdict::FirstMeasurement(score) => {
            println!("mutation score {score} basis points, no floor is committed yet");
        }
        Verdict::Pass { score, floor } => {
            println!("mutation score {score} basis points, floor {floor}");
        }
        Verdict::Below { score, floor } => {
            return Err(format!(
                "mutation score {score} basis points is below the floor {floor}"
            ));
        }
    }
    Ok(())
}

const E2E_SOURCE: &str = "scheduled end-to-end run";

/// Runs the drawn tasks with and without the index and writes the report.
/// Each checkout is indexed first so the with-index arm has an index to ask.
fn end_to_end(args: &[String]) -> Result<(), String> {
    let [cache, out, rest @ ..] = args else {
        return Err(USAGE.to_string());
    };
    let program = rest.first().map_or(DEFAULT_PROGRAM, String::as_str);
    let cache = Path::new(cache);
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let manifest = Manifest::bundled().map_err(|e| e.to_string())?;
    let mut checkouts = BTreeMap::new();
    for repo in manifest.tier(Tier::Pilot) {
        let path = fetch(repo, cache, &GitTransport, &ThreadSleeper).map_err(|e| e.to_string())?;
        let index = ["code".to_string(), "index".to_string()];
        SystemRunner
            .run(program, &index, &path)
            .map_err(|e| format!("{}: {e:?}", repo.name))?;
        checkouts.insert(repo.name.clone(), path);
    }
    let mut set = TaskSet::bundled().map_err(|e| e.to_string())?;
    set.tasks.retain(|t| checkouts.contains_key(&t.repo));
    let tasks = e2e::draw(&set, e2e::TASK_COUNT);
    let cli = ClaudeCli::new(SystemRunner, checkouts);
    let report =
        e2e::run::run(&cli, &cli, &tasks, e2e::REPEATS, E2E_SOURCE).map_err(|e| e.to_string())?;
    std::fs::write(out, report.to_json().map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Prints the value `--facts-tokens` should take for a real report.
/// It only reports the decision; the flip is a committed change.
fn decide(args: &[String]) -> Result<(), String> {
    let [file] = args else {
        return Err(USAGE.to_string());
    };
    let text = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
    let report: E2eReport = serde_json::from_str(&text).map_err(|e| format!("{file}: {e}"))?;
    for failure in e2e::rule::failures(&report) {
        eprintln!("{failure:?}");
    }
    println!("{}", e2e::rule::default_tokens(&report));
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.split_first() {
        Some((command, rest)) if command == "pilot" => parse_args(rest).and_then(|a| pilot(&a)),
        Some((command, rest)) if command == "gate" => gate(rest),
        Some((command, [flag])) if command == "fixtures" && flag == WRITE_BASELINE_FLAG => {
            write_fixture_baseline()
        }
        Some((command, rest)) if command == "mutants" => mutants(rest),
        Some((command, rest)) if command == "e2e" => end_to_end(rest),
        Some((command, rest)) if command == "decide" => decide(rest),
        _ => Err(USAGE.to_string()),
    };
    if let Err(message) = result {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_leading_language_flag_is_split_off() {
        let args = strings(&["--language", "go", "cache", "out"]);
        let (language, rest) = split_language(&args).unwrap();
        assert_eq!(language.as_deref(), Some("go"));
        assert_eq!(rest, strings(&["cache", "out"]));
    }

    #[test]
    fn no_flag_keeps_every_value() {
        let args = strings(&["cache", "out"]);
        let (language, rest) = split_language(&args).unwrap();
        assert_eq!(language, None);
        assert_eq!(rest, args);
    }

    #[test]
    fn a_flag_without_a_language_fails() {
        assert!(split_language(&strings(&["--language"])).is_err());
    }

    #[test]
    fn pilot_needs_a_cache_and_a_report() {
        assert!(parse_args(&strings(&["cache"])).is_err());
    }
}
