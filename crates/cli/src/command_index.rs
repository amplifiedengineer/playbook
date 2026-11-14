// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook commands`: the list of every visible command, built at run time
//! from the clap tree, so it shows exactly the commands this binary has.
//!
//! The clap tree gives the names, the flags and the purpose (the first
//! sentence of each command's doc comment). `META` adds what clap cannot
//! hold: who may run the command and, where the doc comment is too long or
//! too vague, a shorter purpose. A test walks the whole tree and fails when a
//! visible command has no entry here, or an entry names a command that is
//! gone, so a new command cannot ship without one. Hidden commands, which
//! include the deprecated names, are never listed. The intent ids come from
//! the tool map (`tools_map::TOOLS`), so they are not repeated here.

use clap::{Arg, Command};
use std::fmt::Write as _;

/// Who may run a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Changes the repo, the worktree or playbook's own state. A model runs it
    /// when the task needs it.
    Default,
    /// Reads and prints. A model may run it at any time.
    ReadOnly,
    /// Writes the user's home or Claude Code settings, needs their consent,
    /// runs a paid benchmark, or starts a session. Only the user runs it,
    /// typed with `!` in the prompt. A command a skill runs as one of its
    /// steps, such as a small model call, is not user-only.
    UserOnly,
}

impl Access {
    /// The word `--json` prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Default => "default",
            Access::ReadOnly => "read-only",
            Access::UserOnly => "user-only",
        }
    }
}

/// What the clap tree cannot say about one command.
pub(crate) struct Meta {
    /// The command words after `playbook`, for example `config set`.
    pub(crate) path: &'static str,
    pub(crate) access: Access,
    /// Used instead of the doc comment's first sentence when that is too
    /// long or too vague for one line.
    pub(crate) purpose: Option<&'static str>,
    /// Who the top-level command is for. Subcommands leave it unset.
    pub(crate) audience: Option<Audience>,
}

/// Who a top-level command is for, which decides its block in `--help`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Audience {
    User,
    Agent,
}

const USER_HEADING: &str = "Commands";
const AGENT_HEADING: &str = "Agent commands";
const HELP_NAME: &str = "help";
const OPTIONS_HEADING: &str = "Options";
const INDENT: &str = "  ";
const GAP: usize = 2;

/// The `--help` template that groups the top-level commands by audience: the
/// user's commands first, then the agent's. A command with no audience counts
/// as the user's. Clap help templates have no brace escape, so a purpose must
/// not contain `{` or `}`.
pub(crate) fn grouped_template(cmd: &Command) -> String {
    let (agent, user): (Vec<&Command>, Vec<&Command>) = visible(cmd)
        .partition(|c| meta(c.get_name()).and_then(|m| m.audience) == Some(Audience::Agent));
    let mut user_rows = rows(&user);
    let mut agent_rows = rows(&agent);
    let help_about = cmd
        .find_subcommand(HELP_NAME)
        .and_then(Command::get_about)
        .map(ToString::to_string);
    if let Some(about) = help_about {
        let help_row = (HELP_NAME.to_string(), about);
        if agent_rows.is_empty() {
            user_rows.push(help_row);
        } else {
            agent_rows.push(help_row);
        }
    }
    let styles = clap::builder::Styles::default();
    let header = styles.get_header();
    let mut template = String::from("{about-with-newline}\n{usage-heading} {usage}\n\n");
    for (heading, block) in [(USER_HEADING, user_rows), (AGENT_HEADING, agent_rows)] {
        if block.is_empty() {
            continue;
        }
        let _ = writeln!(template, "{header}{heading}:{header:#}");
        let width = block.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
        for (name, purpose) in &block {
            let _ = writeln!(template, "{INDENT}{name:<width$}{:GAP$}{}", "", purpose);
        }
        template.push('\n');
    }
    let _ = writeln!(template, "{header}{OPTIONS_HEADING}:{header:#}");
    template.push_str("{options}{after-help}");
    template
}

fn rows(cmds: &[&Command]) -> Vec<(String, String)> {
    cmds.iter()
        .map(|c| (c.get_name().to_string(), purpose(c.get_name(), c)))
        .collect()
}

impl Meta {
    const fn for_users(self) -> Meta {
        Meta {
            audience: Some(Audience::User),
            ..self
        }
    }

    const fn for_agents(self) -> Meta {
        Meta {
            audience: Some(Audience::Agent),
            ..self
        }
    }

    const fn purpose(self, text: &'static str) -> Meta {
        Meta {
            purpose: Some(text),
            ..self
        }
    }
}

const fn free(path: &'static str) -> Meta {
    Meta {
        path,
        access: Access::Default,
        purpose: None,
        audience: None,
    }
}

const fn ro(path: &'static str) -> Meta {
    Meta {
        path,
        access: Access::ReadOnly,
        purpose: None,
        audience: None,
    }
}

const fn user(path: &'static str) -> Meta {
    Meta {
        path,
        access: Access::UserOnly,
        purpose: None,
        audience: None,
    }
}

/// One entry for every visible command and subcommand, sorted by path. A
/// group's own entry describes the group when it runs with no subcommand.
pub(crate) const META: &[Meta] = &[
    free("adr")
        .purpose("Helpers for ADRs, such as the next free record number")
        .for_agents(),
    free("adr next"),
    ro("agents")
        .purpose("Check that agent definition files follow the playbook rules")
        .for_agents(),
    ro("agents check"),
    ro("atlassian")
        .purpose("Check the Atlassian tools the `playbook:atlassian` skill uses")
        .for_agents(),
    ro("atlassian check").purpose("Check the `acli` version and whether `twg` is installed"),
    user("cc").for_users(),
    free("cc cache"),
    user("cc cache clear"),
    user("cc clean").purpose("Launcher mode: clean up and resume a session"),
    user("cc fresh").purpose("Launcher mode: start a fresh session"),
    user("cc launch"),
    ro("cc list"),
    free("cc prune"),
    user("cc raw").purpose("Launcher mode: resume a session as it was"),
    free("cc worktree"),
    user("ccd").for_users(),
    ro("ci").for_agents(),
    free("code").for_agents(),
    ro("code context"),
    free("code index"),
    ro("code outline"),
    ro("code read"),
    ro("code refs"),
    ro("code search"),
    ro("code status"),
    ro("code symbol"),
    ro("commands").for_users(),
    free("commit")
        .purpose("Prepare and run the signed commit of `/playbook:commit`")
        .for_agents(),
    free("commit prepare"),
    free("commit run"),
    ro("completions").for_users(),
    free("config")
        .purpose("Read and change playbook settings in the repo, org or global tier")
        .for_users(),
    ro("config export"),
    ro("config get"),
    user("config import"),
    ro("config list"),
    free("config set")
        .purpose("Change a setting; protected keys such as `pr.rules.*` are the user's"),
    user("deps").for_users(),
    user("deps ensure"),
    ro("doc")
        .purpose("Read a PDF or Office file as text with the bundled converter")
        .for_agents(),
    ro("doc read"),
    ro("doc status"),
    user("doctor")
        .purpose("Check the playbook setup, or read one fact the check uses")
        .for_users(),
    ro("doctor check"),
    ro("doctor migrations"),
    ro("doctor models"),
    user("effort").for_users(),
    user("effort apply"),
    free("effort choose"),
    ro("effort list"),
    ro("effort log"),
    user("effort reset"),
    ro("effort resolve"),
    ro("effort suggest"),
    user("eval").for_agents(),
    user("eval bench"),
    user("eval triage").purpose("Run the review triage classifier on known cases"),
    free("gate").for_agents(),
    ro("gate check"),
    free("gate record"),
    free("gate snapshot"),
    user("gc").for_users(),
    free("glossary")
        .purpose("Append one term to the repo's `GLOSSARY.md`")
        .for_agents(),
    free("glossary add"),
    free("handoff").for_agents(),
    ro("handoff check"),
    ro("handoff facts"),
    ro("handoff load"),
    free("handoff save"),
    ro("handoff status"),
    ro("handoff template"),
    ro("hooks")
        .purpose("List the hooks that setup adds to settings.json")
        .for_users(),
    ro("hooks list"),
    ro("learn")
        .purpose("Gather repo facts for the `/playbook:learn` collectors")
        .for_agents(),
    ro("learn collect"),
    ro("learn preflight"),
    ro("manifest").for_agents(),
    ro("manifest check"),
    free("memory").for_users(),
    free("memory claude"),
    user("memory claude import"),
    ro("memory context").purpose("Print the memory facts, edges and anchors in scope for a repo"),
    ro("memory export"),
    free("memory rebuild"),
    ro("memory stats"),
    user("migrate").for_users(),
    free("mode").for_users(),
    free("mode ask"),
    user("mode auto"),
    ro("mode status"),
    free("path")
        .purpose("Print a playbook storage directory of this repo, or the plugin directory")
        .for_agents(),
    free("plan")
        .purpose("Plan helpers: the plan list, checkpoints, Work Unit briefs and scratch cleanup")
        .for_agents(),
    free("plan brief"),
    free("plan checkpoint"),
    free("plan cleanup"),
    free("plan graph"),
    ro("plan graph check"),
    free("plan ledger"),
    free("plan ledger set"),
    ro("plan ledger show"),
    ro("plan list"),
    free("plan wave"),
    ro("plan wave next"),
    ro("plugin").for_users(),
    user("plugin install"),
    user("plugin upgrade"),
    free("pr").for_agents(),
    ro("pr checks"),
    ro("pr comments"),
    free("pr create"),
    free("pr merge"),
    free("pr prepare"),
    free("pr reply"),
    free("pr review"),
    free("pr review post"),
    free("pr review submit"),
    free("pr review triage"),
    ro("pr rules"),
    ro("pr stack"),
    ro("pr status"),
    ro("prose").for_agents(),
    ro("prose check"),
    free("review")
        .purpose("Prepare a review: size the diff, pick a depth, run checks")
        .for_agents(),
    free("review checks"),
    free("review lenses"),
    ro("review lenses pick"),
    free("review prepare"),
    ro("review size"),
    free("review triage"),
    ro("review triage merge"),
    ro("route").for_agents(),
    free("sanitize").for_agents(),
    free("sanitize commit"),
    free("sanitize pr"),
    free("segment").for_agents(),
    free("segment branch"),
    ro("segment size"),
    free("segment unit"),
    free("segment unit integrate"),
    ro("segment unit resume"),
    free("segment unit start"),
    free("segment verify"),
    ro("sessions")
        .purpose("List recent sessions with their branch, PR, state and cost")
        .for_users(),
    ro("sessions list"),
    ro("sessions sweep"),
    free("settings").for_agents(),
    ro("settings check"),
    free("settings gen"),
    user("setup").for_users(),
    ro("skills").for_users(),
    user("skills sync"),
    ro("state").for_agents(),
    ro("state list"),
    ro("statusline").for_agents(),
    user("sync").for_users(),
    user("sync config"),
    user("sync config apply"),
    user("sync config pull"),
    user("sync config push"),
    user("sync disable"),
    user("sync enable"),
    user("sync pull"),
    user("sync purge"),
    user("sync push"),
    user("sync run"),
    ro("sync status"),
    ro("tools").for_agents(),
    ro("tools map"),
    user("trust").for_users(),
    user("uninstall").for_users(),
    user("upgrade").for_users(),
    ro("usage").for_users(),
    ro("usage account"),
    user("usage account assign"),
    ro("usage account fingerprint"),
    ro("usage account list"),
    user("usage account name"),
    ro("usage account reconcile"),
    ro("usage account sessions"),
    user("usage account undo"),
    user("usage collect"),
    ro("usage doctor"),
    free("usage machine"),
    user("usage prune"),
    ro("usage status"),
    user("usage stop"),
    ro("version").for_users(),
    user("watch")
        .purpose("Poll GitHub and propose or run actions by your rules")
        .for_users(),
    user("watch approve"),
    ro("watch log"),
    user("watch once"),
    user("watch pause"),
    user("watch reject"),
    user("watch resume"),
    user("watch retry"),
    user("watch service"),
    user("watch service install"),
    user("watch service uninstall"),
    ro("watch status"),
    free("worktree").for_agents(),
    free("worktree remove"),
    free("worktree review"),
    free("worktree review setup"),
    free("worktree review teardown"),
    free("worktree sweep"),
];

/// The first line `playbook commands` prints.
pub const LEGEND: &str = "playbook commands: one line per command. `(read-only)` runs freely. `(user-only)`, or a name ending in `!`, is for the user: never run it yourself, ask them to type it with `!`. `playbook commands <group>` gives flags, `playbook <command> --help` the details.";

/// The entry for `path`, if any.
pub(crate) fn meta(path: &str) -> Option<&'static Meta> {
    META.iter().find(|m| m.path == path)
}

/// The visible subcommands of `cmd`, in `--help` order. The reference page
/// generator uses the same rule.
pub fn visible(cmd: &Command) -> impl Iterator<Item = &Command> {
    cmd.get_subcommands()
        .filter(|s| !s.is_hide_set() && s.get_name() != HELP_NAME)
}

/// The visible arguments of `cmd`, help and version left out. The reference
/// page generator uses the same rule.
pub fn visible_args(cmd: &Command) -> impl Iterator<Item = &Arg> {
    cmd.get_arguments()
        .filter(|a| !a.is_hide_set() && !matches!(a.get_id().as_str(), "help" | "version"))
}

/// One visible command with its path.
pub(crate) struct Entry<'a> {
    pub(crate) path: String,
    pub(crate) cmd: &'a Command,
}

/// Every visible command under `cmd`, depth first, `cmd` itself excluded.
pub(crate) fn walk<'a>(cmd: &'a Command, prefix: &str, out: &mut Vec<Entry<'a>>) {
    for sub in visible(cmd) {
        let path = if prefix.is_empty() {
            sub.get_name().to_string()
        } else {
            format!("{prefix} {}", sub.get_name())
        };
        out.push(Entry {
            path: path.clone(),
            cmd: sub,
        });
        walk(sub, &path, out);
    }
}

/// The clap tree, built once per call.
pub fn tree() -> Command {
    let mut cmd = <crate::Cli as clap::CommandFactory>::command();
    cmd.build();
    cmd
}

/// The one-line purpose of a command: the override, or the first sentence of
/// its doc comment.
pub(crate) fn purpose(path: &str, cmd: &Command) -> String {
    if let Some(text) = meta(path).and_then(|m| m.purpose) {
        return text.to_string();
    }
    let about = cmd.get_about().map(|a| a.to_string()).unwrap_or_default();
    let first = about.lines().next().unwrap_or_default();
    let sentence = first.split(". ").next().unwrap_or_default();
    sentence.trim().trim_end_matches('.').to_string()
}

fn access(path: &str) -> Access {
    meta(path).map_or(Access::Default, |m| m.access)
}

/// Whether `cmd` has subcommands and also runs without one (`doctor --fix`,
/// `cc`), so its own name counts as a command.
fn runs_bare(cmd: &Command) -> bool {
    visible(cmd).next().is_some() && !cmd.is_subcommand_required_set()
}

/// Every runnable name at or below `path`: the leaves, plus each group that
/// also runs bare.
fn runnable<'a>(path: &str, cmd: &'a Command) -> Vec<Entry<'a>> {
    let mut all = vec![Entry {
        path: path.to_string(),
        cmd,
    }];
    walk(cmd, path, &mut all);
    all.into_iter()
        .filter(|e| visible(e.cmd).next().is_none() || runs_bare(e.cmd))
        .collect()
}

/// The tag a line ends with, from the access of every name it covers.
fn tag(accesses: &[Access]) -> &'static str {
    if accesses.iter().all(|a| *a == Access::ReadOnly) {
        " (read-only)"
    } else if accesses.iter().all(|a| *a == Access::UserOnly) {
        " (user-only)"
    } else {
        ""
    }
}

/// `!` when every runnable name at or below `path` is user-only.
fn bang(path: &str, cmd: &Command) -> &'static str {
    if runnable(path, cmd)
        .iter()
        .all(|e| access(&e.path) == Access::UserOnly)
    {
        "!"
    } else {
        ""
    }
}

/// The overview line of one top-level command: its name, its subcommands,
/// its purpose and a tag. In a line with mixed access, each user-only name
/// ends in `!`.
fn overview_line(cmd: &Command) -> String {
    let name = cmd.get_name();
    let covered: Vec<Access> = runnable(name, cmd)
        .iter()
        .map(|e| access(&e.path))
        .collect();
    let tag = tag(&covered);
    let mixed = tag.is_empty();
    let mut line = name.to_string();
    if mixed && runs_bare(cmd) && access(name) == Access::UserOnly {
        line.push('!');
    }
    let names: Vec<String> = visible(cmd)
        .map(|s| {
            let path = format!("{name} {}", s.get_name());
            let flag = if mixed { bang(&path, s) } else { "" };
            format!("{}{flag}", s.get_name())
        })
        .collect();
    if !names.is_empty() {
        let _ = write!(line, " {}", names.join("|"));
    }
    let _ = write!(line, ": {}{tag}", purpose(name, cmd));
    line
}

/// One argument as usage text: `<NAME>`, `--flag` or `--flag <VALUE>`, with
/// `...` when it takes several values, in brackets when optional.
fn arg_text(arg: &Arg) -> String {
    let value = || {
        arg.get_value_names()
            .and_then(|v| v.first().map(ToString::to_string))
            .unwrap_or_else(|| arg.get_id().to_string().to_uppercase())
    };
    let many = arg.get_num_args().is_some_and(|n| n.max_values() > 1);
    let dots = if many { "..." } else { "" };
    let text = if arg.is_positional() {
        format!("<{}>{dots}", value())
    } else {
        let mut t = match (arg.get_long(), arg.get_short()) {
            (Some(long), _) => format!("--{long}"),
            (None, Some(short)) => format!("-{short}"),
            (None, None) => arg.get_id().to_string(),
        };
        if arg.get_action().takes_values() {
            let _ = write!(t, " <{}>{dots}", value());
        }
        t
    };
    if arg.is_required_set() {
        text
    } else {
        format!("[{text}]")
    }
}

/// The usage text of a command's own arguments.
fn args(cmd: &Command) -> Vec<String> {
    visible_args(cmd).map(arg_text).collect()
}

/// The detail line of one command: path, arguments, purpose and tag.
fn detail_line(path: &str, cmd: &Command) -> String {
    let mut line = path.to_string();
    for a in args(cmd) {
        let _ = write!(line, " {a}");
    }
    let _ = write!(line, ": {}{}", purpose(path, cmd), tag(&[access(path)]));
    line
}

/// The longest run of visible commands that `words` starts with: the path,
/// the command it ends on, and how many words it used.
fn deepest<'a, S: AsRef<str>>(root: &'a Command, words: &[S]) -> (String, &'a Command, usize) {
    let mut cmd = root;
    let mut path: Vec<&str> = Vec::new();
    for word in words {
        match visible(cmd).find(|s| s.get_name() == word.as_ref()) {
            Some(sub) => {
                cmd = sub;
                path.push(word.as_ref());
            }
            None => break,
        }
    }
    let used = path.len();
    (path.join(" "), cmd, used)
}

/// Splits the `GROUP` words, so `"worktree review"` quoted as one argument
/// works too, and finds the command they name.
fn find<'a>(root: &'a Command, group: &[String]) -> Result<(String, &'a Command), String> {
    let words: Vec<&str> = group.iter().flat_map(|g| g.split_whitespace()).collect();
    let (path, cmd, used) = deepest(root, &words);
    if used == words.len() {
        Ok((path, cmd))
    } else {
        Err(format!(
            "playbook commands: no command `{}`. Run `playbook commands` for the list.",
            words.join(" ")
        ))
    }
}

/// The text `playbook commands [GROUP]` prints, or an error line.
pub fn render(root: &Command, group: &[String]) -> Result<String, String> {
    let mut out = String::new();
    let (path, cmd) = find(root, group)?;
    if path.is_empty() {
        out.push_str(LEGEND);
        out.push('\n');
        let mut tops: Vec<&Command> = visible(root).collect();
        tops.sort_by_key(|c| c.get_name());
        for cmd in tops {
            out.push_str(&overview_line(cmd));
            out.push('\n');
        }
        return Ok(out);
    }
    out.push_str(&detail_line(&path, cmd));
    out.push('\n');
    let mut below = Vec::new();
    walk(cmd, &path, &mut below);
    for e in below {
        out.push_str(&detail_line(&e.path, e.cmd));
        out.push('\n');
    }
    Ok(out)
}

/// The intent ids of the tool map whose command is `path`.
fn intents(root: &Command, path: &str) -> Vec<&'static str> {
    crate::tools_map::TOOLS
        .iter()
        .filter(|t| {
            let words: Vec<&str> = t.sample.split_whitespace().collect();
            deepest(root, &words).0 == path
        })
        .map(|t| t.intent)
        .collect()
}

/// `playbook commands --json [GROUP]`: a JSON array with one object per
/// command, the stable form for tools. `access` is null for a group that
/// cannot run without a subcommand.
pub fn render_json(root: &Command, group: &[String]) -> Result<String, String> {
    let (prefix, base) = find(root, group)?;
    let mut entries = Vec::new();
    if !prefix.is_empty() {
        entries.push(Entry {
            path: prefix.clone(),
            cmd: base,
        });
    }
    walk(base, &prefix, &mut entries);
    // Sorted by path, so a parent comes before its children and reordering
    // the enums does not change the output.
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let rows: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            let subs: Vec<&str> = visible(e.cmd).map(Command::get_name).collect();
            let access = if subs.is_empty() || runs_bare(e.cmd) {
                serde_json::Value::from(access(&e.path).as_str())
            } else {
                serde_json::Value::Null
            };
            serde_json::json!({
                "command": e.path,
                "purpose": purpose(&e.path, e.cmd),
                "access": access,
                "args": args(e.cmd),
                "subcommands": subs,
                "intents": intents(root, &e.path),
            })
        })
        .collect();
    serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn all() -> Vec<(String, Command)> {
        let root = tree();
        let mut out = Vec::new();
        walk(&root, "", &mut out);
        out.into_iter().map(|e| (e.path, e.cmd.clone())).collect()
    }

    fn words(text: &str) -> usize {
        text.split_whitespace().count()
    }

    fn overview() -> String {
        render(&tree(), &[]).unwrap()
    }

    fn line_of<'a>(text: &'a str, name: &str) -> &'a str {
        text.lines()
            .find(|l| {
                l.starts_with(&format!("{name} "))
                    || l.starts_with(&format!("{name}:"))
                    || l.starts_with(&format!("{name}!"))
            })
            .unwrap_or_else(|| panic!("no line for {name}:\n{text}"))
    }

    #[test]
    fn every_visible_command_has_an_entry() {
        let missing: Vec<String> = all()
            .into_iter()
            .filter(|(p, _)| meta(p).is_none())
            .map(|(p, _)| p)
            .collect();
        assert!(
            missing.is_empty(),
            "add an entry to META in crates/cli/src/command_index.rs for: {missing:?}"
        );
    }

    #[test]
    fn every_entry_names_a_visible_command_once() {
        let visible: BTreeSet<String> = all().into_iter().map(|(p, _)| p).collect();
        let mut seen = BTreeSet::new();
        for m in META {
            assert!(
                visible.contains(m.path),
                "META names `{}`, which is not a visible command",
                m.path
            );
            assert!(seen.insert(m.path), "META lists `{}` twice", m.path);
        }
    }

    #[test]
    fn meta_is_sorted_by_path() {
        let paths: Vec<&str> = META.iter().map(|m| m.path).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted, "keep META sorted by path");
    }

    /// Top-level purposes take 6 to 12 words and subcommand ones 3 to 14, so
    /// each line stays short and says something. A doc comment outside that
    /// range needs a purpose in META.
    #[test]
    fn every_purpose_fits_one_short_line() {
        let mut bad = Vec::new();
        for (path, cmd) in all() {
            let text = purpose(&path, &cmd);
            let n = words(&text);
            let range = if path.contains(' ') { 3..=14 } else { 6..=12 };
            if !range.contains(&n) {
                bad.push(format!("`{path}`: {n} words, want {range:?}: {text}"));
            }
        }
        assert!(
            bad.is_empty(),
            "set a purpose of the right length in META for:\n{}",
            bad.join("\n")
        );
    }

    #[test]
    fn hidden_and_deprecated_commands_are_never_listed() {
        let root = tree();
        let text = overview();
        let json = render_json(&root, &[]).unwrap();
        for name in ["init", "hook"] {
            let listed = text.lines().any(|l| {
                [" ", ":", "!"]
                    .iter()
                    .any(|end| l.starts_with(&format!("{name}{end}")))
            });
            assert!(!listed, "{name} is listed:\n{text}");
            assert!(
                !json.contains(&format!("\"command\": \"{name}\"")),
                "{name} in json"
            );
        }
        assert!(
            !json.contains("\"cc housekeep\""),
            "a hidden subcommand is listed"
        );
        fn deprecated_are_hidden(cmd: &Command) {
            for sub in cmd.get_subcommands() {
                let about = sub.get_about().map(|a| a.to_string()).unwrap_or_default();
                if about.starts_with("Deprecated") {
                    assert!(
                        sub.is_hide_set(),
                        "{} is deprecated but visible",
                        sub.get_name()
                    );
                }
                deprecated_are_hidden(sub);
            }
        }
        deprecated_are_hidden(&root);
    }

    #[test]
    fn the_overview_has_one_line_per_top_level_command() {
        let root = tree();
        let text = overview();
        assert!(text.starts_with(LEGEND));
        assert_eq!(text.lines().count(), visible(&root).count() + 1);
    }

    #[test]
    fn commands_that_touch_the_users_machine_are_user_only() {
        for path in [
            "setup",
            "uninstall",
            "upgrade",
            "trust",
            "migrate",
            "config import",
            "ccd",
            "deps ensure",
            "effort apply",
            "mode auto",
            "doctor",
            "cc cache clear",
        ] {
            assert_eq!(access(path), Access::UserOnly, "{path}");
        }
        let text = overview();
        assert!(line_of(&text, "setup").ends_with("(user-only)"), "{text}");
        let names = |name: &str| -> Vec<String> {
            let head = line_of(&text, name).split(':').next().unwrap_or_default();
            head.split([' ', '|']).map(String::from).collect()
        };
        assert!(names("config").contains(&"import!".to_string()), "{text}");
        assert!(names("config").contains(&"get".to_string()), "{text}");
        assert!(names("mode").contains(&"auto!".to_string()), "{text}");
    }

    #[test]
    fn a_line_is_read_only_only_when_every_name_on_it_is() {
        let text = overview();
        for name in ["sessions", "hooks", "route", "prose"] {
            assert!(
                line_of(&text, name).ends_with("(read-only)"),
                "{name}: {text}"
            );
        }
        for name in ["config", "handoff", "doctor", "pr"] {
            assert!(
                !line_of(&text, name).ends_with("(read-only)"),
                "{name}: {text}"
            );
        }
        assert!(line_of(&text, "cc").starts_with("cc! "), "{text}");
    }

    #[test]
    fn a_group_lists_each_subcommand_with_its_flags() {
        let root = tree();
        let text = render(&root, &["config".to_string()]).unwrap();
        assert!(text.starts_with("config: "), "{text}");
        assert!(text.contains("\nconfig set"), "{text}");
        assert!(text.contains("(user-only)"), "{text}");
        let nested = render(&root, &["worktree".into(), "review".into()]).unwrap();
        assert!(nested.contains("\nworktree review setup <"), "{nested}");
        let quoted = render(&root, &["worktree review".to_string()]).unwrap();
        assert_eq!(quoted, nested);
        assert!(render(&root, &["nope".to_string()]).is_err());
        assert!(render(&root, &["pr".into(), "nope".into()]).is_err());
        let cc = render(&root, &["cc".to_string()]).unwrap();
        assert!(cc.starts_with("cc [<ARGS>...]: "), "{cc}");
    }

    #[test]
    fn json_carries_access_args_and_intents() {
        let root = tree();
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&render_json(&root, &[]).unwrap()).unwrap();
        let row = |c: &str| rows.iter().find(|r| r["command"] == c).unwrap().clone();
        assert_eq!(row("setup")["access"], "user-only");
        assert_eq!(row("sessions")["access"], "read-only");
        assert_eq!(row("pr reply")["intents"][0], "pr-reply");
        assert!(row("path")["intents"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("plans-dir")));
        assert!(row("route")["args"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("[--json]")));
        let group: Vec<serde_json::Value> =
            serde_json::from_str(&render_json(&root, &["pr".to_string()]).unwrap()).unwrap();
        assert_eq!(group[0]["command"], "pr");
        assert!(group.iter().all(|r| {
            let c = r["command"].as_str().unwrap();
            c == "pr" || c.starts_with("pr ")
        }));
        assert_eq!(row("config")["access"], serde_json::Value::Null);
        assert_eq!(row("doctor")["access"], "user-only");
    }

    #[test]
    fn every_tool_map_intent_lands_on_a_command() {
        let root = tree();
        for tool in crate::tools_map::TOOLS {
            let words: Vec<&str> = tool.sample.split_whitespace().collect();
            let own = format!("{} {}", tool.group, tool.verb);
            let landed = deepest(&root, &words).0;
            assert!(
                landed == tool.group || landed == own.trim(),
                "{} lands on `{landed}`, not on its own command",
                tool.intent
            );
        }
    }

    const USER: &[&str] = &[
        "setup",
        "gc",
        "hooks",
        "uninstall",
        "upgrade",
        "version",
        "deps",
        "doctor",
        "migrate",
        "skills",
        "plugin",
        "cc",
        "ccd",
        "usage",
        "sessions",
        "watch",
        "trust",
        "commands",
        "completions",
        "config",
        "effort",
        "mode",
        "memory",
        "sync",
    ];
    const AGENT: &[&str] = &[
        "doc",
        "state",
        "route",
        "statusline",
        "settings",
        "manifest",
        "agents",
        "gate",
        "code",
        "sanitize",
        "ci",
        "path",
        "plan",
        "prose",
        "atlassian",
        "learn",
        "segment",
        "pr",
        "review",
        "commit",
        "worktree",
        "handoff",
        "adr",
        "glossary",
        "tools",
        "eval",
    ];

    fn top_level_names() -> BTreeSet<String> {
        visible(&tree()).map(|c| c.get_name().to_string()).collect()
    }

    #[test]
    fn every_top_level_command_names_an_audience() {
        // Arrange
        let names = top_level_names();

        // Act
        let unset: Vec<&String> = names
            .iter()
            .filter(|n| meta(n).and_then(|m| m.audience).is_none())
            .collect();

        // Assert
        assert!(unset.is_empty(), "no audience for: {unset:?}");
    }

    #[test]
    fn only_top_level_rows_name_an_audience() {
        // Arrange
        let nested = META.iter().filter(|m| m.path.contains(' '));

        // Act
        let named: Vec<&str> = nested
            .filter(|m| m.audience.is_some())
            .map(|m| m.path)
            .collect();

        // Assert
        assert!(named.is_empty(), "nested rows with an audience: {named:?}");
    }

    #[test]
    fn the_user_and_agent_sets_are_the_agreed_constants() {
        // Arrange
        let names = top_level_names();
        let user: BTreeSet<String> = USER.iter().map(|s| s.to_string()).collect();
        let agent: BTreeSet<String> = AGENT.iter().map(|s| s.to_string()).collect();

        // Act
        let shared: Vec<&String> = user.intersection(&agent).collect();
        let union: BTreeSet<String> = user.union(&agent).cloned().collect();

        // Assert
        assert!(shared.is_empty(), "in both lists: {shared:?}");
        assert_eq!(union, names);
    }

    #[test]
    fn the_constants_agree_with_each_commands_audience() {
        // Arrange
        let audience = |name: &str| meta(name).and_then(|m| m.audience);

        // Act
        let wrong_user: Vec<&&str> = USER
            .iter()
            .filter(|n| audience(n) != Some(Audience::User))
            .collect();
        let wrong_agent: Vec<&&str> = AGENT
            .iter()
            .filter(|n| audience(n) != Some(Audience::Agent))
            .collect();

        // Assert
        assert!(wrong_user.is_empty(), "not for users: {wrong_user:?}");
        assert!(wrong_agent.is_empty(), "not for agents: {wrong_agent:?}");
    }

    #[test]
    fn the_template_styles_all_three_headings() {
        // Arrange
        let root = tree();
        let styles = clap::builder::Styles::default();
        let header = styles.get_header();

        // Act
        let template = grouped_template(&root);

        // Assert
        for heading in [USER_HEADING, AGENT_HEADING, OPTIONS_HEADING] {
            let styled = format!("{header}{heading}:{header:#}");
            assert!(template.contains(&styled), "unstyled {heading}: {template}");
        }
    }

    #[test]
    fn no_top_level_purpose_holds_a_brace() {
        // Arrange
        let root = tree();
        let mut texts: Vec<String> = visible(&root).map(|c| purpose(c.get_name(), c)).collect();
        texts.extend(
            root.find_subcommand(HELP_NAME)
                .and_then(Command::get_about)
                .map(ToString::to_string),
        );

        // Act
        let braced: Vec<&String> = texts.iter().filter(|t| t.contains(['{', '}'])).collect();

        // Assert
        assert!(
            braced.is_empty(),
            "help templates cannot hold braces: {braced:?}"
        );
    }

    #[test]
    fn the_grouped_block_matches_clap_for_a_one_group_tree() {
        // Arrange
        let mut cmd = Command::new("demo")
            .subcommand(Command::new("alpha").about("First one"))
            .subcommand(Command::new("long-name").about("Second one"))
            .subcommand(Command::new("gamma").about("Third one"));
        cmd.build();
        let command_lines = |text: &str| -> Vec<String> {
            text.lines()
                .filter(|l| {
                    ["alpha", "long-name", "gamma", HELP_NAME]
                        .iter()
                        .any(|n| l.trim_start().starts_with(n))
                })
                .map(str::to_string)
                .collect()
        };
        let clap_text = cmd.clone().render_help().to_string();
        let mut grouped = cmd.clone().help_template(grouped_template(&cmd));

        // Act
        let grouped_text = grouped.render_help().to_string();

        // Assert
        assert_eq!(command_lines(&grouped_text), command_lines(&clap_text));
        assert_eq!(command_lines(&grouped_text).len(), 4, "{grouped_text}");
    }

    #[test]
    fn the_json_row_keeps_exactly_its_current_keys() {
        // Arrange
        let root = tree();

        // Act
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&render_json(&root, &[]).unwrap()).unwrap();

        // Assert
        let mut keys: Vec<&str> = rows[0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "access",
                "args",
                "command",
                "intents",
                "purpose",
                "subcommands"
            ]
        );
    }
}
