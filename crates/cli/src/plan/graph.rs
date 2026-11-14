// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook plan graph check` and `playbook plan wave next`: the Work Unit
//! dependency graph of a plan, read from its Segments and Work Units tables.
//! `check` reports what would break a run; `next` picks the Work Units that
//! are safe to run together now.

use crate::brief::files_of;
use crate::json::envelope;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

/// Lockfiles two Work Units must never edit in the same wave.
const LOCKFILES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "Cargo.lock",
    "go.sum",
];
/// Path folders holding schema migrations.
const MIGRATION_DIRS: &[&str] = &["migrations", "migration"];
/// A Work Unit cell value meaning "no group" or "no requirement".
const NONE_CELL: &str = "none";
/// Printed by `wave next` when no Work Unit can start.
pub const NO_WAVE: &str = "NONE";
/// The failure code of the shared envelope.
const FAILED_CODE: i32 = 1;

#[derive(Debug)]
struct Unit {
    id: String,
    files: Vec<String>,
    requires: Vec<String>,
    group: Option<String>,
}

#[derive(Debug)]
struct Segment {
    id: String,
    wus: Vec<String>,
}

/// The Work Units and Segments of a plan, in table order.
#[derive(Debug)]
pub struct Plan {
    units: Vec<Unit>,
    segments: Vec<Segment>,
}

/// One problem `check` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    kind: &'static str,
    message: String,
}

/// The result of `check`.
#[derive(Debug)]
pub struct Report {
    findings: Vec<Finding>,
    units: usize,
    topology: &'static str,
}

struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    fn column(&self, name: &str) -> Option<usize> {
        self.header.iter().position(|h| h == name)
    }

    fn has(&self, names: &[&str]) -> bool {
        names.iter().all(|n| self.column(n).is_some())
    }

    fn cell<'a>(&self, row: &'a [String], name: &str) -> &'a str {
        self.column(name)
            .and_then(|i| row.get(i))
            .map_or("", String::as_str)
    }
}

fn split_row(line: &str) -> Vec<String> {
    let inner = line.trim().trim_start_matches('|').trim_end_matches('|');
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                chars.next();
                current.push('|');
            }
            '|' => cells.push(std::mem::take(&mut current)),
            other => current.push(other),
        }
    }
    cells.push(current);
    cells.iter().map(|c| c.trim().to_string()).collect()
}

fn is_separator(line: &str) -> bool {
    let line = line.trim();
    line.starts_with('|') && line.contains('-') && line.chars().all(|c| "|-: ".contains(c))
}

fn tables(text: &str) -> Vec<Table> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < lines.len() {
        if !(lines[i].trim_start().starts_with('|') && is_separator(lines[i + 1])) {
            i += 1;
            continue;
        }
        let header = split_row(lines[i])
            .iter()
            .map(|h| h.to_ascii_lowercase())
            .collect();
        let mut rows = Vec::new();
        i += 2;
        while i < lines.len() && lines[i].trim_start().starts_with('|') {
            rows.push(split_row(lines[i]));
            i += 1;
        }
        out.push(Table { header, rows });
    }
    out
}

pub(crate) fn is_wu_id(token: &str) -> bool {
    token
        .strip_prefix("WU-")
        .is_some_and(|rest| !rest.is_empty())
}

pub(crate) fn is_segment_id(token: &str) -> bool {
    token
        .strip_prefix('S')
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

/// The ids in a table cell that `is_id` accepts, in order.
fn ids(cell: &str, is_id: fn(&str) -> bool) -> Vec<String> {
    cell.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
        .map(|t| t.trim_matches('.'))
        .filter(|t| is_id(t))
        .map(str::to_string)
        .collect()
}

impl Plan {
    /// Reads the Work Units table and, when present, the Segments table.
    pub fn parse(text: &str) -> Result<Self, String> {
        let tables = tables(text);
        let wu_table = tables
            .iter()
            .find(|t| t.has(&["wu", "files", "requires"]))
            .ok_or("no Work Units table found: it needs WU, Files and Requires columns")?;
        let units = wu_table
            .rows
            .iter()
            .filter_map(|row| {
                let id = ids(wu_table.cell(row, "wu"), is_wu_id).into_iter().next()?;
                let group = wu_table.cell(row, "parallel group").trim();
                Some(Unit {
                    id,
                    files: files_of(&format!("Files: {}", wu_table.cell(row, "files"))),
                    requires: ids(wu_table.cell(row, "requires"), is_wu_id),
                    group: (!group.is_empty() && !group.eq_ignore_ascii_case(NONE_CELL))
                        .then(|| group.to_string()),
                })
            })
            .collect();
        let segments = tables
            .iter()
            .find(|t| t.has(&["seg", "wus"]))
            .map(|t| {
                t.rows
                    .iter()
                    .filter_map(|row| {
                        let id = ids(t.cell(row, "seg"), is_segment_id).into_iter().next()?;
                        Some(Segment {
                            id,
                            wus: ids(t.cell(row, "wus"), is_wu_id),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self { units, segments })
    }

    fn unit(&self, id: &str) -> Option<&Unit> {
        self.units.iter().find(|u| u.id == id)
    }

    fn segment_of(&self, wu: &str) -> Option<&str> {
        self.segments
            .iter()
            .find(|s| s.wus.iter().any(|w| w == wu))
            .map(|s| s.id.as_str())
    }

    fn segment_index(&self, wu: &str) -> Option<usize> {
        self.segments
            .iter()
            .position(|s| s.wus.iter().any(|w| w == wu))
    }
}

fn touches_shared_surface(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    let name = parts.pop().unwrap_or("");
    LOCKFILES.contains(&name) || parts.iter().any(|p| MIGRATION_DIRS.contains(p))
}

fn overlaps(a: &str, b: &str) -> bool {
    let within = |outer: &str, inner: &str| {
        inner
            .strip_prefix(outer.trim_end_matches('/'))
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    };
    within(a, b) || within(b, a)
}

/// Why two Work Units cannot share a wave, or `None` when they can.
fn clash(a: &Unit, b: &Unit) -> Option<String> {
    for fa in &a.files {
        for fb in &b.files {
            if overlaps(fa, fb) {
                let shorter = if fa.len() <= fb.len() { fa } else { fb };
                return Some(format!("share {shorter}"));
            }
        }
    }
    let shared = |u: &Unit| u.files.iter().any(|f| touches_shared_surface(f));
    (shared(a) && shared(b)).then(|| "both touch a lockfile or migration folder".to_string())
}

/// Every Work Unit `id` needs, directly or through others.
fn ancestors(plan: &Plan, id: &str) -> HashSet<String> {
    let mut seen = HashSet::new();
    let mut stack = vec![id.to_string()];
    while let Some(current) = stack.pop() {
        for req in plan.unit(&current).map_or(&[][..], |u| &u.requires[..]) {
            if seen.insert(req.clone()) {
                stack.push(req.clone());
            }
        }
    }
    seen
}

/// Kahn's algorithm: the Work Units that never resolve, in table order.
fn unresolved(plan: &Plan) -> Vec<String> {
    let known: HashSet<&str> = plan.units.iter().map(|u| u.id.as_str()).collect();
    let mut waiting: HashMap<&str, usize> = HashMap::new();
    let mut enables: HashMap<&str, Vec<&str>> = HashMap::new();
    for unit in &plan.units {
        let reqs: HashSet<&str> = unit
            .requires
            .iter()
            .map(String::as_str)
            .filter(|r| known.contains(r))
            .collect();
        waiting.insert(unit.id.as_str(), reqs.len());
        for req in reqs {
            enables.entry(req).or_default().push(unit.id.as_str());
        }
    }
    let mut queue: VecDeque<&str> = plan
        .units
        .iter()
        .map(|u| u.id.as_str())
        .filter(|id| waiting[id] == 0)
        .collect();
    let mut resolved = HashSet::new();
    while let Some(id) = queue.pop_front() {
        resolved.insert(id);
        for next in enables.get(id).into_iter().flatten() {
            if let Some(count) = waiting.get_mut(next) {
                *count -= 1;
                if *count == 0 {
                    queue.push_back(next);
                }
            }
        }
    }
    plan.units
        .iter()
        .map(|u| u.id.clone())
        .filter(|id| !resolved.contains(id.as_str()))
        .collect()
}

fn finding(kind: &'static str, message: String) -> Finding {
    Finding { kind, message }
}

fn unknown_findings(plan: &Plan) -> Vec<Finding> {
    plan.units
        .iter()
        .flat_map(|u| {
            u.requires
                .iter()
                .filter(|r| plan.unit(r).is_none())
                .map(|r| {
                    finding(
                        "unknown",
                        format!("{} requires {r}, which is not in the plan", u.id),
                    )
                })
        })
        .collect()
}

fn order_findings(plan: &Plan) -> Vec<Finding> {
    let mut out = Vec::new();
    for unit in &plan.units {
        for req in &unit.requires {
            if let (Some(mine), Some(theirs)) =
                (plan.segment_index(&unit.id), plan.segment_index(req))
            {
                if theirs > mine {
                    out.push(finding(
                        "order",
                        format!(
                            "{} ({}) requires {req} ({}), a later Segment",
                            unit.id, plan.segments[mine].id, plan.segments[theirs].id
                        ),
                    ));
                }
            }
        }
    }
    out
}

fn coverage_findings(plan: &Plan) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for unit in &plan.units {
        if !seen.insert(unit.id.as_str()) {
            out.push(finding("coverage", format!("{} is listed twice", unit.id)));
        }
        if plan.segment_of(&unit.id).is_none() {
            out.push(finding("coverage", format!("{} is in no Segment", unit.id)));
        }
    }
    for seg in &plan.segments {
        for wu in seg.wus.iter().filter(|w| plan.unit(w).is_none()) {
            out.push(finding(
                "coverage",
                format!("{} lists {wu}, which has no Work Units row", seg.id),
            ));
        }
    }
    out
}

fn parallel_findings(plan: &Plan) -> Vec<Finding> {
    let mut groups: Vec<(&str, Vec<&Unit>)> = Vec::new();
    for unit in &plan.units {
        let Some(group) = unit.group.as_deref() else {
            continue;
        };
        match groups.iter_mut().find(|(g, _)| *g == group) {
            Some((_, members)) => members.push(unit),
            None => groups.push((group, vec![unit])),
        }
    }
    let mut out = Vec::new();
    for (group, members) in groups {
        for (i, a) in members.iter().enumerate() {
            for b in &members[i + 1..] {
                let reason = if ancestors(plan, &b.id).contains(&a.id) {
                    Some(format!("{} requires {}", b.id, a.id))
                } else if ancestors(plan, &a.id).contains(&b.id) {
                    Some(format!("{} requires {}", a.id, b.id))
                } else {
                    clash(a, b).map(|why| format!("{} and {} {why}", a.id, b.id))
                };
                if let Some(reason) = reason {
                    out.push(finding("parallel", format!("{group}: {reason}")));
                }
            }
        }
    }
    out
}

/// `stacked` when a Work Unit needs one from another Segment, `independent`
/// when the Segments stand alone, `single` without a Segments table.
fn topology(plan: &Plan) -> &'static str {
    if plan.segments.is_empty() {
        return "single";
    }
    let crosses = plan.units.iter().any(|u| {
        u.requires.iter().any(|r| {
            matches!(
                (plan.segment_of(&u.id), plan.segment_of(r)),
                (Some(a), Some(b)) if a != b
            )
        })
    });
    if crosses {
        "stacked"
    } else {
        "independent"
    }
}

/// Runs every check on `plan`.
pub fn check(plan: &Plan) -> Report {
    let mut findings = unknown_findings(plan);
    let stuck = unresolved(plan);
    if !stuck.is_empty() {
        findings.push(finding("cycle", stuck.join(", ")));
    }
    if !plan.segments.is_empty() {
        findings.extend(order_findings(plan));
        findings.extend(coverage_findings(plan));
    }
    findings.extend(parallel_findings(plan));
    Report {
        findings,
        units: plan.units.len(),
        topology: topology(plan),
    }
}

impl Report {
    pub fn passed(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn render(&self) -> String {
        let mut lines: Vec<String> = self
            .findings
            .iter()
            .map(|f| format!("{}: {}", f.kind, f.message))
            .collect();
        lines.push(if self.passed() {
            format!("Graph check: PASS ({} Work Units)", self.units)
        } else {
            format!("Graph check: FAIL ({} findings)", self.findings.len())
        });
        lines.push(format!("topology={}", self.topology));
        lines.join("\n")
    }

    pub fn to_envelope(&self) -> Value {
        let findings: Vec<Value> = self
            .findings
            .iter()
            .map(|f| json!({"kind": f.kind, "message": f.message}))
            .collect();
        let (code, error) = if self.passed() {
            (0, String::new())
        } else {
            (FAILED_CODE, format!("{} findings", self.findings.len()))
        };
        envelope::wrap(
            code,
            &error,
            json!({"findings": findings, "work_units": self.units, "topology": self.topology}),
        )
    }
}

/// The Work Units to run next: the ready units of the first Segment with work
/// left, taken in table order while their files stay clear of each other.
pub fn next_wave(plan: &Plan, done: &HashSet<String>) -> Vec<String> {
    let open = |u: &&Unit| !done.contains(&u.id);
    let current = plan
        .segments
        .iter()
        .find(|s| s.wus.iter().any(|w| plan.unit(w).is_some_and(|u| open(&u))))
        .map(|s| s.id.as_str());
    let mut wave: Vec<&Unit> = Vec::new();
    for unit in plan.units.iter().filter(open) {
        let in_segment = plan.segments.is_empty() || plan.segment_of(&unit.id) == current;
        let ready = unit.requires.iter().all(|r| done.contains(r));
        if in_segment && ready && wave.iter().all(|w| clash(w, unit).is_none()) {
            wave.push(unit);
        }
    }
    wave.iter().map(|u| u.id.clone()).collect()
}

fn load(path: &Path) -> Result<Plan, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Plan::parse(&text)
}

fn fail(json: bool, error: &str) -> i32 {
    if json {
        println!("{}", envelope::wrap(FAILED_CODE, error, Value::Null));
        0
    } else {
        eprintln!("error: {error}");
        FAILED_CODE
    }
}

/// `plan graph check`: prints the report and returns the exit code.
pub fn run_check(path: &Path, json: bool) -> i32 {
    match load(path) {
        Ok(plan) => {
            let report = check(&plan);
            if json {
                println!("{}", report.to_envelope());
                0
            } else {
                println!("{}", report.render());
                i32::from(!report.passed())
            }
        }
        Err(error) => fail(json, &error),
    }
}

/// `plan wave next`: prints the wave and returns the exit code.
pub fn run_next(path: &Path, done: &[String], json: bool) -> i32 {
    match load(path) {
        Ok(plan) => {
            let done: HashSet<String> = done.iter().map(|d| d.trim().to_string()).collect();
            let wave = next_wave(&plan, &done);
            if json {
                println!("{}", envelope::wrap(0, "", json!({"wave": wave})));
            } else if wave.is_empty() {
                println!("{NO_WAVE}");
            } else {
                println!("{}", wave.join("\n"));
            }
            0
        }
        Err(error) => fail(json, &error),
    }
}
