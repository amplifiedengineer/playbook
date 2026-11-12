// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Per-component effort ceilings (ADR-0017). A component is an agent, a
//! command or a skill, and its ceiling comes from `effort.<kind>.<name>`.
//!
//! The ceiling for a component is the lowest of Claude Code's own
//! `maxEffortLevel`, playbook's `effort.max` and the component's key. The
//! effective effort is the lower of the shipped effort and that ceiling, so a
//! ceiling never raises anything. For an agent the effective effort is the
//! value the orchestrator passes as the `effort` parameter of the `Agent`
//! call on the base agent.

use super::lower;
use crate::config;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// What a component is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Agent,
    Command,
    Skill,
}

impl Kind {
    /// Parse `agents`, `commands` or `skills` (singular forms work too).
    pub fn parse(text: &str) -> Option<Kind> {
        match text {
            "agent" | "agents" => Some(Kind::Agent),
            "command" | "commands" => Some(Kind::Command),
            "skill" | "skills" => Some(Kind::Skill),
            _ => None,
        }
    }

    /// The plural word used in config keys and plugin directories.
    pub fn plural(self) -> &'static str {
        match self {
            Kind::Agent => "agents",
            Kind::Command => "commands",
            Kind::Skill => "skills",
        }
    }

    /// The component's source file under the plugin root.
    fn file(self, root: &Path, name: &str) -> std::path::PathBuf {
        match self {
            Kind::Agent => root.join("agents").join(format!("{name}.md")),
            Kind::Command => root.join("commands").join(format!("{name}.md")),
            Kind::Skill => root.join("skills").join(name).join("SKILL.md"),
        }
    }
}

/// The config key that holds a component's ceiling.
fn config_key(kind: Kind, name: &str) -> String {
    format!("effort.{}.{name}", kind.plural())
}

/// The `effort:` value in a file's frontmatter, if any.
fn shipped_effort(path: &Path) -> Option<String> {
    frontmatter_value(path, "effort:")
}

/// The `model:` value in a file's frontmatter, if any.
fn shipped_model(path: &Path) -> Option<String> {
    frontmatter_value(path, "model:")
}

fn frontmatter_value(path: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "---" {
        return None;
    }
    for line in lines {
        if line == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix(key) {
            let value = rest.trim().trim_matches(['"', '\'']);
            return (!value.is_empty()).then(|| value.to_string());
        }
    }
    None
}

/// Everything known about one component's effort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub kind: Kind,
    pub name: String,
    /// Whether the component's source file exists under the plugin root.
    pub known: bool,
    pub shipped: Option<String>,
    /// The `model:` in the component's frontmatter.
    pub model: Option<String>,
    /// What that model allows for this component (see `model_cap`).
    pub model_cap: Option<String>,
    /// The component's own key, `auto` when unset.
    pub configured: String,
    /// playbook's `effort.max`.
    pub global: String,
    pub claude: Option<String>,
    /// The lowest ceiling in play, `None` when nothing limits.
    pub ceiling: Option<String>,
    /// Shipped effort lowered by the ceiling.
    pub effective: Option<String>,
    /// For an agent: the `subagent_type` to pass the `Agent` tool, always the
    /// base agent. The effort goes in the `effort` parameter.
    pub subagent_type: Option<String>,
}

fn level_of(home: &Path, key: &str) -> String {
    match config::resolve_valid(key, home, None) {
        Ok((Value::String(level), _, _)) => level,
        _ => "auto".to_string(),
    }
}

/// Plugin components that changed name: (kind, old name, new name). A ceiling
/// set under the old key keeps applying while the new key is unset, so a user's
/// config is not lost with the name. The rows go when `KEY_RENAMES` takes the
/// one-to-one renames.
const RENAMED_COMPONENTS: &[(Kind, &str, &str)] = &[
    (Kind::Command, "commit-and-push", "commit"),
    (Kind::Command, "create-pull-request", "pr-create"),
    (Kind::Command, "address-pr-comments", "pr-comments"),
];

/// Plugin components that merged into one: (kind, old names, new name). While
/// the new key is unset, the lower of the old keys that are set applies. A
/// ceiling caps spend, so the lower one is the safe reading; a user who wants
/// more sets the new key. This row stays until 1.0.0.
const MERGED_COMPONENTS: &[(Kind, &[&str], &str)] =
    &[(Kind::Command, &["quick-review", "deep-review"], "review")];

/// What a ceiling key holds.
#[derive(Debug, PartialEq, Eq)]
enum Setting {
    /// Nothing is set: the built-in default applies.
    Unset,
    /// A value outside the allowed levels was ignored; the text says which.
    Invalid(String),
    /// A valid level, `auto` included.
    Level(String),
}

fn setting_of(home: &Path, key: &str) -> Setting {
    match config::resolve_valid(key, home, None) {
        Ok((_, _, Some(ignored))) => Setting::Invalid(ignored.warning),
        Ok((Value::String(level), source, None)) if source != config::Source::Default => {
            Setting::Level(level)
        }
        _ => Setting::Unset,
    }
}

/// The ceiling set for a component, `auto` when none is. The new key wins when
/// it is set; only an unset new key falls back to the old name's key, and an
/// invalid new value never does.
fn configured_level(home: &Path, kind: Kind, name: &str) -> String {
    let level = |setting: Setting| match setting {
        Setting::Level(level) => Some(level),
        Setting::Invalid(_) | Setting::Unset => None,
    };
    let merged = || {
        let olds = MERGED_COMPONENTS
            .iter()
            .find(|(k, _, new)| *k == kind && *new == name)
            .map_or(&[][..], |(_, olds, _)| *olds);
        olds.iter()
            .filter_map(|old| level(setting_of(home, &config_key(kind, old))))
            .reduce(|a, b| lower(Some(&a), Some(&b)).unwrap_or("auto").to_string())
    };
    match setting_of(home, &config_key(kind, name)) {
        Setting::Unset => RENAMED_COMPONENTS
            .iter()
            .find(|(k, _, new)| *k == kind && *new == name)
            .and_then(|(_, old, _)| level(setting_of(home, &config_key(kind, old))))
            .or_else(merged)
            .unwrap_or_else(|| "auto".to_string()),
        set => level(set).unwrap_or_else(|| "auto".to_string()),
    }
}

/// `auto` and `max` set no ceiling.
fn as_ceiling(level: &str) -> Option<&str> {
    (level != "auto" && level != "max").then_some(level)
}

/// Resolve one component. `root` is the plugin root holding `agents/`,
/// `commands/` and `skills/`; `None` leaves `known` false and `shipped` empty.
pub fn resolve(
    kind: Kind,
    name: &str,
    home: &Path,
    claude: Option<&str>,
    root: Option<&Path>,
) -> Resolution {
    resolve_capped(kind, name, home, claude, root, None)
}

/// `resolve` with an extra ceiling `cap` from the calling command, such as
/// `review` holding its reviewer to `medium`. The lowest of the cap and
/// the user's ceilings applies, and a cap never raises anything.
pub fn resolve_capped(
    kind: Kind,
    name: &str,
    home: &Path,
    claude: Option<&str>,
    root: Option<&Path>,
    cap: Option<&str>,
) -> Resolution {
    let path = root.map(|r| kind.file(r, name));
    let known = path.as_deref().is_some_and(Path::is_file);
    let shipped = path.as_deref().and_then(shipped_effort);
    let model = path.as_deref().and_then(shipped_model);
    let model_cap = super::model_cap::cap_for(model.as_deref(), name);
    let configured = configured_level(home, kind, name);
    let global = level_of(home, super::KEY);

    let ceiling = lower(
        lower(
            lower(lower(claude, as_ceiling(&global)), as_ceiling(&configured)),
            cap.and_then(as_ceiling),
        ),
        model_cap,
    )
    .map(str::to_string);
    let effective = match (shipped.as_deref(), ceiling.as_deref()) {
        (Some(s), Some(c)) => lower(Some(s), Some(c)).map(str::to_string),
        (Some(s), None) => Some(s.to_string()),
        (None, _) => None,
    };

    let subagent_type =
        (kind == Kind::Agent && shipped.is_some()).then(|| format!("playbook:{name}"));

    Resolution {
        kind,
        name: name.to_string(),
        known,
        shipped,
        model,
        model_cap: model_cap.map(str::to_string),
        configured,
        global,
        claude: claude.map(str::to_string),
        ceiling,
        effective,
        subagent_type,
    }
}

impl Resolution {
    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind.plural(),
            "name": self.name,
            "known": self.known,
            "shipped": self.shipped,
            "model": self.model,
            "modelCap": self.model_cap,
            "configured": self.configured,
            "global": self.global,
            "claudeCode": self.claude,
            "ceiling": self.ceiling,
            "effective": self.effective,
            "effort": self.effective,
            "subagentType": self.subagent_type,
        })
    }

    pub fn to_text(&self) -> String {
        let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "none".to_string());
        let mut out = format!(
            "{} {}: shipped {}, ceiling {}, effective {}",
            self.kind.plural(),
            self.name,
            show(&self.shipped),
            show(&self.ceiling),
            show(&self.effective),
        );
        if let (Some(ty), Some(effort)) = (&self.subagent_type, &self.effective) {
            out.push_str(&format!("\ndispatch: {ty} with effort {effort}"));
        }
        if !self.known {
            out.push_str("\nwarning: no such component in the plugin files");
        }
        out
    }
}

/// Every component under `root`, sorted by kind then name.
pub fn components(root: &Path) -> Vec<(Kind, String)> {
    let mut found = Vec::new();
    let md_stems = |dir: &Path| -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                if path.extension()? != "md" {
                    return None;
                }
                Some(path.file_stem()?.to_string_lossy().into_owned())
            })
            .collect();
        names.sort();
        names
    };
    for name in md_stems(&root.join("agents")) {
        found.push((Kind::Agent, name));
    }
    for name in md_stems(&root.join("commands")) {
        found.push((Kind::Command, name));
    }
    let mut skills: Vec<String> = fs::read_dir(root.join("skills"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("SKILL.md").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    skills.sort();
    for name in skills {
        found.push((Kind::Skill, name));
    }
    found
}

/// Component keys set in the global config that match no component under
/// `root`, as `effort.<kind>.<name>`.
fn stale_keys(home: &Path, root: &Path) -> Vec<String> {
    let store_root = crate::common::paths::playbook_root_from(home);
    let Ok(keys) = config::store::global_keys(&store_root) else {
        return Vec::new();
    };
    let mut stale = Vec::new();
    for key in keys {
        let mut parts = key.splitn(3, '.');
        let (Some("effort"), Some(kind_name), Some(name)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Some(kind) = Kind::parse(kind_name) else {
            continue;
        };
        if !kind.file(root, name).is_file() {
            stale.push(config_key(kind, name));
        }
    }
    stale.sort();
    stale
}

/// `playbook effort list`: every component with its shipped effort, ceiling
/// and effective level.
pub fn run_list(home: &Path, claude: Option<&str>, root: Option<&Path>, json: bool) -> String {
    let Some(root) = root else {
        return "no plugin root found: run `playbook path plugin-root` to see what was tried"
            .to_string();
    };
    let rows: Vec<Resolution> = components(root)
        .into_iter()
        .map(|(kind, name)| resolve(kind, &name, home, claude, Some(root)))
        .collect();
    let stale = stale_keys(home, root);
    if json {
        return json!({
            "components": rows.iter().map(Resolution::to_json).collect::<Vec<_>>(),
            "unknownKeys": stale,
        })
        .to_string();
    }
    let dash = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".to_string());
    let mut out =
        String::from("component                         shipped  set     effective  model cap\n");
    for r in &rows {
        out.push_str(&format!(
            "{:<33} {:<8} {:<7} {:<10} {}\n",
            format!("{}/{}", r.kind.plural(), r.name),
            dash(&r.shipped),
            r.configured,
            dash(&r.effective),
            dash(&r.model_cap),
        ));
    }
    out.push_str(&format!(
        "model cap: Haiku never runs at max, stops at {} unless a role opts in (git: xhigh)\n",
        super::model_cap::HAIKU_DEFAULT_CAP
    ));
    for key in &stale {
        out.push_str(&format!("warning: {key} matches no component\n"));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effort::rank;

    use crate::common::test_support::scratch_dir;
    use crate::config::write::{self, Tier};

    fn plugin(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = scratch_dir(tag);
        let root = base.join("plugin");
        let home = base.join("home");
        for d in ["agents", "commands", "skills/writing"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        fs::create_dir_all(&home).unwrap();
        let agent =
            |effort: &str| format!("---\nname: x\ndescription: d\neffort: {effort}\n---\nBody\n");
        fs::write(root.join("agents/reviewer.md"), agent("high")).unwrap();
        fs::write(root.join("agents/critic.md"), agent("low")).unwrap();
        fs::write(root.join("commands/review.md"), agent("high")).unwrap();
        fs::write(
            root.join("skills/writing/SKILL.md"),
            "---\nname: w\n---\nBody\n",
        )
        .unwrap();
        (home, root)
    }

    fn set(home: &Path, key: &str, value: &str) {
        write::set(Tier::Global, key, &Value::String(value.into()), home, None).unwrap();
    }

    #[test]
    fn a_haiku_agent_gets_the_model_cap_as_a_ceiling() {
        let (home, root) = plugin("cmp-haiku-cap");
        fs::write(
            root.join("agents/cheap.md"),
            "---\nname: c\ndescription: d\nmodel: haiku\neffort: low\n---\nBody\n",
        )
        .unwrap();
        let r = resolve(Kind::Agent, "cheap", &home, None, Some(&root));
        assert_eq!(r.model.as_deref(), Some("haiku"));
        assert_eq!(r.model_cap.as_deref(), Some("medium"));
        assert_eq!(r.ceiling.as_deref(), Some("medium"));
    }

    #[test]
    fn a_haiku_agent_stops_at_medium() {
        let (home, root) = plugin("cmp-haiku-cap");
        fs::write(
            root.join("agents/critic.md"),
            "---\nname: g\ndescription: d\nmodel: haiku\neffort: low\n---\nBody\n",
        )
        .unwrap();
        let r = resolve(Kind::Agent, "critic", &home, None, Some(&root));
        assert_eq!(r.model_cap.as_deref(), Some("medium"));
        set(&home, "effort.max", "medium");
        let r = resolve(Kind::Agent, "critic", &home, None, Some(&root));
        assert_eq!(
            r.ceiling.as_deref(),
            Some("medium"),
            "the user ceiling still wins"
        );
    }

    #[test]
    fn a_component_ceiling_lowers_the_effective_effort() {
        let (home, root) = plugin("cmp-lower");
        set(&home, "effort.commands.review", "medium");
        let r = resolve(Kind::Command, "review", &home, None, Some(&root));
        assert_eq!(r.shipped.as_deref(), Some("high"));
        assert_eq!(r.ceiling.as_deref(), Some("medium"));
        assert_eq!(r.effective.as_deref(), Some("medium"));
    }

    #[test]
    fn a_ceiling_never_raises_the_shipped_effort() {
        let (home, root) = plugin("cmp-never-raise");
        set(&home, "effort.agents.critic", "xhigh");
        let r = resolve(Kind::Agent, "critic", &home, None, Some(&root));
        assert_eq!(r.effective.as_deref(), Some("low"));
    }

    #[test]
    fn claude_code_is_the_top_ceiling_and_auto_adds_none() {
        let (home, root) = plugin("cmp-claude");
        set(&home, "effort.commands.review", "xhigh");
        let r = resolve(Kind::Command, "review", &home, Some("low"), Some(&root));
        assert_eq!(r.ceiling.as_deref(), Some("low"));
        assert_eq!(r.effective.as_deref(), Some("low"));
        let auto = resolve(Kind::Command, "review", &home, None, Some(&root));
        set(&home, "effort.commands.review", "auto");
        let auto2 = resolve(Kind::Command, "review", &home, None, Some(&root));
        assert_eq!(auto2.ceiling, None);
        assert_eq!(auto2.effective.as_deref(), Some("high"));
        assert_eq!(auto.ceiling.as_deref(), Some("xhigh"));
    }

    #[test]
    fn the_global_ceiling_applies_to_a_component_without_its_own() {
        let (home, root) = plugin("cmp-global");
        write::set(
            Tier::Global,
            "effort.max",
            &Value::String("medium".into()),
            &home,
            None,
        )
        .unwrap();
        let r = resolve(Kind::Command, "review", &home, None, Some(&root));
        assert_eq!(r.effective.as_deref(), Some("medium"));
    }

    #[test]
    fn the_spawn_effort_is_the_effective_level_for_every_agent() {
        let (home, root) = plugin("cmp-spawn-effort");
        write::set(
            Tier::Global,
            "effort.max",
            &Value::String("medium".into()),
            &home,
            None,
        )
        .unwrap();
        for (kind, name) in components(&root) {
            if kind != Kind::Agent {
                continue;
            }
            let json = resolve(kind, &name, &home, None, Some(&root)).to_json();
            let level = json["effort"].as_str();
            assert_eq!(level, json["effective"].as_str(), "{name}");
            assert!(
                level.is_none_or(|l| rank(l) <= rank("medium")),
                "{name} spawns above the ceiling"
            );
        }
    }

    #[test]
    fn with_no_ceiling_the_spawn_effort_is_the_shipped_effort() {
        let (home, root) = plugin("cmp-spawn-shipped");
        let json = resolve(Kind::Agent, "reviewer", &home, None, Some(&root)).to_json();
        assert_eq!(json["effort"].as_str(), json["shipped"].as_str());
    }

    #[test]
    fn a_command_cap_lowers_the_spawn_effort_and_never_raises_it() {
        let (home, root) = plugin("cmp-cap");
        let capped = resolve_capped(
            Kind::Agent,
            "reviewer",
            &home,
            None,
            Some(&root),
            Some("medium"),
        );
        assert_eq!(capped.effective.as_deref(), Some("medium"));
        assert_eq!(capped.to_json()["effort"], "medium");
        // A lower user ceiling still wins over the cap.
        let low = resolve_capped(
            Kind::Agent,
            "reviewer",
            &home,
            Some("low"),
            Some(&root),
            Some("medium"),
        );
        assert_eq!(low.effective.as_deref(), Some("low"));
        // A cap above the shipped effort changes nothing.
        let high = resolve_capped(
            Kind::Agent,
            "reviewer",
            &home,
            None,
            Some(&root),
            Some("max"),
        );
        assert_eq!(high.effective.as_deref(), Some("high"));
    }

    #[test]
    fn an_agent_is_always_spawned_as_the_base_agent() {
        let (home, root) = plugin("cmp-base");
        set(&home, "effort.agents.reviewer", "low");
        let r = resolve(Kind::Agent, "reviewer", &home, None, Some(&root));
        assert_eq!(r.subagent_type.as_deref(), Some("playbook:reviewer"));
        assert_eq!(r.to_json()["effort"], "low");
        assert_eq!(
            r.to_text(),
            "agents reviewer: shipped high, ceiling low, effective low\ndispatch: playbook:reviewer with effort low"
        );
    }

    #[test]
    fn each_ceiling_alone_caps_the_agent_effort() {
        let (home, root) = plugin("cmp-each-ceiling");
        // Claude Code's maxEffortLevel.
        let r = resolve(Kind::Agent, "reviewer", &home, Some("medium"), Some(&root));
        assert_eq!(r.to_json()["effort"], "medium");
        // playbook's effort.max.
        set(&home, "effort.max", "low");
        let r = resolve(Kind::Agent, "reviewer", &home, None, Some(&root));
        assert_eq!(r.to_json()["effort"], "low");
        // The per-agent key, with the global ceiling off again.
        set(&home, "effort.max", "auto");
        set(&home, "effort.agents.reviewer", "medium");
        let r = resolve(Kind::Agent, "reviewer", &home, None, Some(&root));
        assert_eq!(r.to_json()["effort"], "medium");
    }

    #[test]
    fn a_skill_with_no_effort_has_no_effective_level() {
        let (home, root) = plugin("cmp-skill");
        let r = resolve(Kind::Skill, "writing", &home, None, Some(&root));
        assert!(r.known);
        assert_eq!(r.shipped, None);
        assert_eq!(r.effective, None);
    }

    #[test]
    fn an_unknown_component_is_reported_not_rejected() {
        let (home, root) = plugin("cmp-unknown");
        set(&home, "effort.agents.ghost", "low");
        let r = resolve(Kind::Agent, "ghost", &home, None, Some(&root));
        assert!(!r.known);
        assert!(r.to_text().contains("no such component"));
        assert_eq!(stale_keys(&home, &root), vec!["effort.agents.ghost"]);
    }

    #[test]
    fn list_shows_every_kind() {
        let (home, root) = plugin("cmp-list");
        let names: Vec<String> = components(&root)
            .into_iter()
            .map(|(k, n)| format!("{}/{n}", k.plural()))
            .collect();
        assert_eq!(
            names,
            vec![
                "agents/critic",
                "agents/reviewer",
                "commands/review",
                "skills/writing"
            ]
        );
        let text = run_list(&home, None, Some(&root), false);
        assert!(text.contains("agents/reviewer"), "{text}");
    }

    /// Write `config.json` with exactly these keys, bypassing the check that
    /// `write::set` runs, so a hand-edited invalid value can be tested.
    fn write_raw(home: &Path, pairs: &[(&str, &str)]) {
        let map: serde_json::Map<String, Value> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), Value::String((*v).to_string())))
            .collect();
        let file = home.join(".config/playbook/config.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, Value::Object(map).to_string()).unwrap();
    }

    #[test]
    fn an_effort_ceiling_falls_back_only_when_the_new_key_is_unset() {
        // Arrange
        let old = "effort.commands.commit-and-push";
        let new = "effort.commands.commit";
        let rows: [(&[(&str, &str)], &str); 6] = [
            (&[(old, "low")], "low"),
            (&[(new, "medium")], "medium"),
            (&[(old, "low"), (new, "medium")], "medium"),
            (&[(old, "low"), (new, "auto")], "auto"),
            (&[(old, "low"), (new, "bogus")], "auto"),
            (&[], "auto"),
        ];
        let mut problems = Vec::new();

        // Act
        for (index, (pairs, expected)) in rows.iter().enumerate() {
            let (home, root) = plugin(&format!("cmp-renamed-{index}"));
            fs::write(root.join("commands/commit.md"), "---\neffort: high\n---\n").unwrap();
            write_raw(&home, pairs);
            let got = resolve(Kind::Command, "commit", &home, None, Some(&root)).configured;
            if got != *expected {
                problems.push(format!("{pairs:?}: got {got}, want {expected}"));
            }
        }
        let (home, _) = plugin("cmp-renamed-invalid");
        write_raw(&home, &[(old, "low"), (new, "bogus")]);
        let invalid = setting_of(&home, new);

        // Assert
        assert!(problems.is_empty(), "{problems:#?}");
        assert!(
            matches!(&invalid, Setting::Invalid(w) if w.contains("bogus")),
            "{invalid:?}"
        );
    }

    #[test]
    fn every_renamed_component_keeps_its_old_ceiling_until_the_new_key_is_set() {
        // Arrange
        let mut problems = Vec::new();

        // Act
        for (index, (kind, old, new)) in RENAMED_COMPONENTS.iter().enumerate() {
            let (home, root) = plugin(&format!("cmp-renamed-row-{index}"));
            let file = kind.file(&root, new);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, "---\neffort: high\n---\n").unwrap();
            let old_key = config_key(*kind, old);
            let new_key = config_key(*kind, new);
            write_raw(&home, &[(old_key.as_str(), "low")]);
            let old_only = resolve(*kind, new, &home, None, Some(&root)).configured;
            let (both_home, _) = plugin(&format!("cmp-renamed-both-{index}"));
            write_raw(
                &both_home,
                &[(old_key.as_str(), "low"), (new_key.as_str(), "medium")],
            );
            let both = resolve(*kind, new, &both_home, None, Some(&root)).configured;
            if old_only != "low" || both != "medium" {
                problems.push(format!("{old} to {new}: old only {old_only}, both {both}"));
            }
        }

        // Assert
        assert!(problems.is_empty(), "{problems:#?}");
    }

    #[test]
    fn a_merged_component_takes_the_lower_old_ceiling_until_the_new_key_is_set() {
        // Arrange
        let new = "effort.commands.review";
        let cases: [(&[(&str, &str)], &str); 6] = [
            (
                &[
                    ("effort.commands.deep-review", "xhigh"),
                    ("effort.commands.quick-review", "low"),
                ],
                "low",
            ),
            (&[("effort.commands.deep-review", "medium")], "medium"),
            (
                &[
                    ("effort.commands.quick-review", "high"),
                    ("effort.commands.deep-review", "auto"),
                ],
                "high",
            ),
            (
                &[("effort.commands.quick-review", "low"), (new, "high")],
                "high",
            ),
            (
                &[("effort.commands.quick-review", "low"), (new, "bogus")],
                "auto",
            ),
            (&[], "auto"),
        ];
        let mut problems = Vec::new();

        // Act
        for (index, (pairs, want)) in cases.iter().enumerate() {
            let (home, root) = plugin(&format!("cmp-merged-{index}"));
            write_raw(&home, pairs);
            let got = resolve(Kind::Command, "review", &home, None, Some(&root)).configured;
            if got != *want {
                problems.push(format!("case {index}: want {want}, got {got}"));
            }
        }

        // Assert
        assert!(problems.is_empty(), "{problems:#?}");
    }
}
