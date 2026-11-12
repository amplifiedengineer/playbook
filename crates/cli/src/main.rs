// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

use clap::CommandFactory;
use playbook::common::payload::Payload;
use playbook::init::run::{InitPaths, StepStatus};
use playbook::{
    adr, agents, atlassian, brief, cc, ci, commit, common, config, deliver, deps, doc, doctor,
    effort, eval, gate, handoff, hooks, init, json, learn, manifest, memory_import, mode,
    plan_cleanup, planning, pr, prose, review, sanitize, settings, skills_sync, statusline,
    storage, tools_map, trust, update, usage, worktree, AdrCommand, AgentsCommand,
    AtlassianCommand, CcCacheCommand, CcCommand, Cli, Command, CommitCommand, ConfigCommand,
    DepsCommand, DocCommand, DoctorCommand, EvalCommand, GateCommand, GlossaryCommand,
    HandoffCommand, LearnCommand, ManifestCommand, MemoryClaudeCommand, MemoryCommand, ModeArg,
    ModeCommand, PlanCommand, PluginCommand, PrCommand, PrReviewCommand, ProseCommand,
    ReviewCommand, ReviewTriageCommand, ReviewWorktreeCommand, SanitizeCommand, SegmentCommand,
    SegmentTopology, SessionsCommand, SettingsCommand, SkillsCommand, ToolsCommand,
    WorktreeCommand,
};
use std::io::{IsTerminal, Read, Write};

/// Restore the default SIGPIPE action so `playbook ... | head` ends quietly
/// instead of panicking on a closed pipe (Rust ignores SIGPIPE by default).
#[cfg(unix)]
fn restore_sigpipe() {
    // The always-true flag makes signal-hook run the default action, which
    // ends the process, whenever SIGPIPE arrives.
    let _ = signal_hook::flag::register_conditional_default(
        signal_hook::consts::SIGPIPE,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
    );
}

#[cfg(not(unix))]
fn restore_sigpipe() {}

/// Run one hook: read the payload from stdin, then dispatch it.
fn run_hook(name: playbook::HookName) {
    let raw = read_hook_input();
    // Plugin disabled in Claude Code: succeed silently, with no side effects.
    // The worktree hooks stand in for Claude Code's own worktree handling, so a
    // pause cannot skip them without leaving the call with no worktree path.
    let is_worktree = matches!(
        name,
        playbook::HookName::WorktreeCreate | playbook::HookName::WorktreeRemove
    );
    if !is_worktree && common::plugin_state::paused() {
        return;
    }
    playbook::cc::telemetry::on_hook(name);
    let payload = Payload::parse(&raw);
    if matches!(name, playbook::HookName::WorktreeCreate) {
        std::process::exit(hooks::worktree_create::execute(&payload));
    }
    hooks::dispatch(name, &payload);
}

/// The launcher arguments for a mode word followed by what the user typed.
fn mode_args(mode: &str, rest: Vec<String>) -> Vec<String> {
    let mut args = vec![mode.to_string()];
    args.extend(rest);
    args
}

/// `playbook gc --build-output`: asks on the terminal before it deletes.
fn run_build_output_flag(
    home: &std::path::Path,
    registry: &playbook::gc::Registry,
    clock: &dyn playbook::gc::Clock,
) -> (String, i32) {
    use playbook::gc::build_output::{
        current_repo_worktrees, flag_command, lsof_in_use, Probes, Terminal,
    };
    use std::io::{BufRead, IsTerminal, Write};
    let limits = registry
        .stores()
        .iter()
        .find(|store| store.name == "build output")
        .map(|store| store.limits)
        .unwrap_or_default();
    let mut confirm = |question: &str| {
        print!("{question}");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer).is_ok()
            && matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
    };
    let mut terminal = Terminal {
        is_terminal: std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        confirm: &mut confirm,
    };
    let probes = Probes {
        space: &playbook::gc::space::RealProbe,
        in_use: &lsof_in_use,
    };
    let (_, hand_made) = current_repo_worktrees(home);
    flag_command(
        (registry, clock),
        &hand_made,
        &limits,
        (&probes, &mut terminal),
    )
}

/// Runs `playbook doctor check`; a bare `playbook doctor` runs it too.
fn run_doctor_check(json: bool) {
    let home = common::home_dir();
    let claude_home = home.join(".claude");
    let slug = common::repo_slug();
    let env = doctor::check::Env {
        plugin_root: std::env::var_os("CLAUDE_PLUGIN_ROOT").map(Into::into),
        shell: std::env::var("SHELL").unwrap_or_default(),
        path_var: std::env::var_os("PATH").unwrap_or_default(),
        repo_root: manifest::check::toplevel(),
        repo_slug: (!slug.is_empty()).then_some(slug),
        home,
        claude_home,
    };
    let rows = doctor::check::run(&env);
    if json {
        println!("{}", doctor::check::to_json(&rows));
    } else if std::io::stdout().is_terminal() {
        print!(
            "{}",
            doctor::style::render(&playbook::ui::Caps::detect(), &rows)
        );
    } else {
        print!("{}", doctor::check::render(&rows));
    }
}

/// Windows gives the main thread 1 MiB, against 8 MiB on Linux and macOS. The
/// clap tree is deep enough to overflow that in a debug build (`doctor check`
/// died with a stack overflow on the windows-smoke job), so run on a thread
/// with the usual 8 MiB.
#[cfg(windows)]
fn main() {
    const STACK_BYTES: usize = 8 * 1024 * 1024;
    let spawned = std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(real_main);
    match spawned.map(std::thread::JoinHandle::join) {
        Ok(Ok(())) => {}
        // A panic already printed its message on the worker thread.
        Ok(Err(_)) => std::process::exit(101),
        Err(e) => {
            eprintln!("playbook: cannot start the main thread: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    real_main();
}

fn real_main() {
    restore_sigpipe();
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    // The `pbtop` link of the Homebrew formula is the standalone dashboard,
    // without playbook's host, as the separate `pbtop` binary runs.
    if playbook::fastpath::invoked_as_pbtop(&args) {
        playbook::usage::cli::main();
        return;
    }
    // Copy state keyed by an old org name to the current one, once.
    let _ = playbook::common::org_state::migrate(&playbook::common::paths::playbook_root());
    playbook::usage_host::install();
    // Hot paths skip building the clap tree. See `fastpath`.
    match playbook::fastpath::classify(&args) {
        Some(playbook::fastpath::Fast::Hook(name)) => {
            run_hook(name);
            return;
        }
        Some(playbook::fastpath::Fast::Statusline) => std::process::exit(statusline::run()),
        Some(playbook::fastpath::Fast::Version) => {
            print!("{}", playbook::fastpath::version_text());
            return;
        }
        Some(playbook::fastpath::Fast::RemovedShellInit) => {
            eprintln!("{}", playbook::fastpath::SHELL_INIT_REMOVED);
            return;
        }
        None => {}
    }
    let (cli, renamed) = playbook::parse::parse_or_exit(&args);
    // Hooks fail soft, and version, completions, json and commands write nothing under home; every other command refuses to write state relative to
    // the current directory when there is no home.
    if !matches!(
        cli.command,
        Command::Hook { .. }
            | Command::Version
            | Command::Completions { .. }
            | Command::Commands { .. }
    ) {
        if let Some(message) = common::home_error(&common::home_dir()) {
            eprintln!("playbook: {message}");
            std::process::exit(1);
        }
    }
    if let Some(line) = cli
        .command
        .deprecation_notice(std::io::stderr().is_terminal())
    {
        eprintln!("{line}");
    }
    if let Some(line) =
        playbook::renames::printable_notice(&args, renamed, std::io::stderr().is_terminal())
    {
        eprintln!("{line}");
    }
    let legacy = playbook::renames::notice_for(&args, playbook::renames::LEGACY_NOTICES);
    if let Some(line) =
        playbook::renames::printable_notice(&args, legacy, std::io::stderr().is_terminal())
    {
        eprintln!("{line}");
    }
    // The first-run step (ADR 0034 decision 13): heal once after an upgrade,
    // before most commands. Hot paths returned above or skip it.
    playbook::heal::on_start(&cli.command);
    playbook::plugin_update::on_start(&cli.command, &args);
    // Starts the collector daemon for most commands, `cc` included.
    playbook::cc::telemetry::on_start(&cli.command);

    update::check::on_start(&cli.command, &args);

    match cli.command {
        Command::Hook { name } => run_hook(name),
        // Launcher subcommands land in a later Work Unit; stub for now.
        Command::Cc { sub, args } => match sub {
            Some(CcCommand::Prune) => {
                cc::retention::prune(&cc::logical_cwd(), playbook::common::time::now_secs())
            }
            Some(CcCommand::Cache {
                sub: CcCacheCommand::Clear,
            }) => cc::bust_cache::bust(),
            Some(CcCommand::List) => {
                let cwd = cc::logical_cwd();
                let dir = cc::sessions::project_dir(&cc::claude_dir(), &cwd);
                print!("{}", cc::sessions::render_list(&dir, &cwd));
            }
            Some(CcCommand::Worktree { branch, env_base }) => {
                let cwd = std::env::current_dir().unwrap_or_default();
                let code = cc::worktree_run::run(&cwd, &branch, env_base.as_deref());
                std::process::exit(code);
            }
            Some(CcCommand::Launch {
                skip_permissions,
                args,
            }) => std::process::exit(cc::launch::run(skip_permissions, &args)),
            Some(CcCommand::Housekeep {
                repo_root,
                worktree,
                branch,
                no_push,
            }) => cc::worktree_run::Housekeep::new(
                std::path::Path::new(&repo_root),
                std::path::Path::new(&worktree),
                &branch,
                no_push,
            )
            .run(),
            Some(CcCommand::Clean { args }) => {
                std::process::exit(cc::launch::run(false, &mode_args("clean", args)))
            }
            Some(CcCommand::Fresh { args }) => {
                std::process::exit(cc::launch::run(false, &mode_args("fresh", args)))
            }
            Some(CcCommand::Raw { args }) => {
                std::process::exit(cc::launch::run(false, &mode_args("raw", args)))
            }
            None => std::process::exit(cc::launch::run(false, &args)),
        },
        Command::Ccd { args } => std::process::exit(cc::launch::run(true, &args)),
        Command::Effort { sub: Some(sub), .. } => {
            let home = common::home_dir();
            let claude_home = home.join(".claude");
            let cwd = std::env::current_dir().unwrap_or_default();
            let claude = effort::claude_cap(&claude_home, &cwd);
            let root = playbook::plugin_root::find_here();
            match sub {
                playbook::EffortCommand::List { json } => {
                    println!(
                        "{}",
                        effort::component::run_list(
                            &home,
                            claude.as_deref(),
                            root.as_deref(),
                            json
                        )
                    );
                }
                playbook::EffortCommand::Suggest { range, json, patch } => {
                    let report = suggest_report(&range, &home, claude.as_deref(), root.as_deref());
                    match (patch, json) {
                        (true, true) => println!("{}", effort::patch::build(&report).to_json()),
                        (true, false) => println!("{}", effort::patch::build(&report).to_text()),
                        (false, true) => println!("{}", report.to_json()),
                        (false, false) => println!("{}", report.to_text()),
                    }
                }
                playbook::EffortCommand::Apply { range, yes } => {
                    use std::io::IsTerminal;
                    let report = suggest_report(&range, &home, claude.as_deref(), root.as_deref());
                    let patch = effort::patch::build(&report);
                    let interactive = std::io::stdin().is_terminal();
                    let stdin = std::io::stdin();
                    match effort::patch::run_apply(
                        &home,
                        &patch,
                        yes,
                        interactive,
                        &mut stdin.lock(),
                        &mut std::io::stdout(),
                        common::time::now_secs(),
                    ) {
                        Ok(msg) => println!("{msg}"),
                        Err(err) => {
                            eprintln!("error: {err}");
                            std::process::exit(1);
                        }
                    }
                }
                playbook::EffortCommand::Choose {
                    agent,
                    base,
                    lines,
                    files,
                    bytes,
                    paths,
                    lens,
                    json,
                } => {
                    let res = effort::component::resolve(
                        effort::component::Kind::Agent,
                        &agent,
                        &home,
                        claude.as_deref(),
                        root.as_deref(),
                    );
                    let shape = match base.as_deref() {
                        Some(base) => effort_diff_shape(base),
                        None => Ok(effort::auto::Shape {
                            lines: lines.unwrap_or(0),
                            files: files.unwrap_or(paths.len() as u64),
                            bytes,
                            paths,
                        }),
                    };
                    let graph = hooks::rebuild_memory_graph::graph_text(
                        &common::paths::playbook_root_from(&home).join("memory"),
                    )
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
                    let evidence = || {
                        let report = effort_report(&home, claude.as_deref(), root.as_deref())?;
                        effort::auto::evidence_from(&report, &agent)
                    };
                    let choice = effort::auto::choose(
                        &home,
                        &res,
                        shape,
                        graph.as_ref(),
                        lens.as_deref(),
                        &evidence,
                        common::time::now_secs(),
                    );
                    if json {
                        println!("{}", choice.to_json());
                    } else {
                        match &choice.effort {
                            Some(effort) => println!("{} {effort}", choice.dispatch),
                            None => println!("{}", choice.dispatch),
                        }
                        if let Some(note) = &choice.note {
                            eprintln!("{note}");
                        }
                    }
                }
                playbook::EffortCommand::Log { limit, json } => {
                    let rows = effort::auto::read_log(&home, limit);
                    if json {
                        println!("{}", serde_json::Value::Array(rows));
                    } else {
                        println!("{}", effort::auto::log_text(&rows));
                    }
                }
                playbook::EffortCommand::Reset { all } => {
                    match effort::patch::reset(&home, all, common::time::now_secs()) {
                        Ok(msg) => println!("{msg}"),
                        Err(err) => {
                            eprintln!("error: {err}");
                            std::process::exit(1);
                        }
                    }
                }
                playbook::EffortCommand::Resolve {
                    kind,
                    name,
                    cap,
                    json,
                } => {
                    if let Some(level) = cap.as_deref() {
                        if !effort::LEVELS.contains(&level) || level == "auto" {
                            eprintln!(
                                "error: unknown cap '{level}', use low, medium, high, xhigh or max"
                            );
                            std::process::exit(1);
                        }
                    }
                    let Some(kind) = effort::component::Kind::parse(&kind) else {
                        eprintln!("error: unknown kind '{kind}', use agents, commands or skills");
                        std::process::exit(1);
                    };
                    let r = effort::component::resolve_capped(
                        kind,
                        &name,
                        &home,
                        claude.as_deref(),
                        root.as_deref(),
                        cap.as_deref(),
                    );
                    if json {
                        println!("{}", r.to_json());
                    } else {
                        println!("{}", r.to_text());
                    }
                }
            }
        }
        Command::Effort { level, json, .. } => {
            let home = common::home_dir();
            match level {
                Some(level) => match effort::run_set(&home, &level) {
                    Ok(msg) => println!("{msg}"),
                    Err(err) => {
                        eprintln!("error: {err}");
                        std::process::exit(1);
                    }
                },
                None => {
                    let cwd = std::env::current_dir().unwrap_or_default();
                    println!(
                        "{}",
                        effort::run_status(&home, &home.join(".claude"), &cwd, json)
                    );
                }
            }
        }
        Command::Uninstall {
            yes,
            dry_run,
            remove_binary,
        } => {
            let home = common::home_dir();
            let opts = playbook::uninstall::Options {
                bin_dir: playbook::uninstall::install_dir(&home),
                home,
                remove_binary,
                dry_run: dry_run || !yes,
            };
            let report = playbook::uninstall::run(&opts);
            if std::io::stdout().is_terminal() {
                print!(
                    "{}",
                    playbook::uninstall::style::render(
                        &playbook::ui::Caps::detect(),
                        &report,
                        opts.dry_run
                    )
                );
            } else {
                for line in &report.lines {
                    println!("{line}");
                }
                for error in &report.errors {
                    eprintln!("uninstall: {error}");
                }
            }
            if !yes && !dry_run {
                eprintln!("uninstall: nothing was changed; re-run with --yes to remove these");
                std::process::exit(1);
            }
            if !report.errors.is_empty() {
                std::process::exit(1);
            }
        }
        Command::Gc {
            yes,
            restore,
            build_output,
        } => {
            let home = playbook::common::home_dir();
            let clock = playbook::gc::default_clock();
            let registry = playbook::gc::registry(&home);
            if build_output {
                let (text, code) = run_build_output_flag(&home, &registry, &*clock);
                print!("{text}");
                std::process::exit(code);
            }
            if yes || restore.is_some() {
                let (text, code) = match restore {
                    Some(id) => playbook::gc::apply::restore_command(&registry, &*clock, &id),
                    None => playbook::gc::apply::apply_command(&registry, &*clock),
                };
                print!("{text}");
                std::process::exit(code);
            }
            let report = playbook::gc::report::report(&registry, clock.now());
            print!("{}", playbook::gc::report::render(&report));
            print!(
                "{}",
                playbook::gc::apply::render_anomalies(&registry, &*clock)
            );
        }
        Command::Statusline => std::process::exit(statusline::run()),
        Command::Version => print!("{}", Cli::command().render_version()),
        Command::Completions { shell } => {
            let script = match playbook::completions::render(shell) {
                Ok(script) => script,
                Err(e) => {
                    eprintln!("playbook: cannot build the completion script: {e}");
                    std::process::exit(1);
                }
            };
            match std::io::stdout().lock().write_all(script.as_bytes()) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
                Err(e) => {
                    eprintln!("playbook: cannot write the completion script: {e}");
                    std::process::exit(1);
                }
            }
        }
        Command::Setup { flags } | Command::Init { flags } if flags.explain.is_some() => {
            let mut flags = flags;
            init::choices::map_and_note(&mut flags);
            let which = flags.explain.as_ref().and_then(|w| w.as_deref());
            let named: Vec<String> = init::choices::only_parts(&flags)
                .into_iter()
                .filter(|n| !init::choices::RETIRED_PARTS.contains(&n.as_str()))
                .collect();
            let texts: Vec<Result<String, String>> = match (which, named.is_empty()) {
                (None, _) if init::choices::only_retired(&flags) => Vec::new(),
                (None, false) => named
                    .iter()
                    .map(|n| init::catalog::explain(Some(n)))
                    .collect(),
                _ => vec![init::catalog::explain(which)],
            };
            for text in texts {
                match text {
                    Ok(text) => print!("{text}"),
                    Err(err) => {
                        eprintln!("setup: {err}");
                        std::process::exit(2);
                    }
                }
            }
        }
        Command::Setup { flags } | Command::Init { flags } => {
            let mut flags = flags;
            init::choices::map_and_note(&mut flags);
            let security_config = config::resolve("security.defaults", &common::home_dir(), None)
                .map(|(value, _)| value.as_bool().unwrap_or(false))
                .unwrap_or(false);
            let only = match init::choices::only_choices(&flags) {
                Ok(only) => only,
                Err(err) => {
                    eprintln!("setup: {err}");
                    std::process::exit(2);
                }
            };
            let asking = init::choices::stdio_is_interactive()
                && !flags.yes
                && !flags.dry_run
                && only.is_none();
            // Telemetry already on, set up by you, or off for a reason init will
            // not override: say so in one line and do not ask. An explicit
            // --telemetry or --no-telemetry flag is still honoured.
            let home_dir = common::home_dir();
            let skip_telemetry_question = if flags.telemetry || flags.no_telemetry {
                None
            } else {
                init::telemetry::skip_line(
                    &home_dir,
                    &home_dir.join(".claude"),
                    &std::env::current_dir().unwrap_or_else(|_| home_dir.clone()),
                    init::telemetry::process_configured_beyond_settings(&home_dir.join(".claude")),
                )
            };
            if only.is_none() {
                if let Some(line) = skip_telemetry_question {
                    println!("{line}");
                }
            }
            if let Some(notice) = init::choices::defaults_notice(
                std::io::stdin().is_terminal(),
                std::io::stdout().is_terminal(),
                &flags,
            )
            .filter(|_| only.is_none())
            {
                eprintln!("{notice}");
            }
            let caps = playbook::ui::Caps::detect();
            let mut guided: Vec<(&'static str, bool)> = Vec::new();
            if asking {
                let security_on = flags.security || (!flags.no_security && security_config);
                let skipped: Vec<&str> = [("telemetry", skip_telemetry_question.is_some())]
                    .into_iter()
                    .filter_map(|(id, skip)| skip.then_some(id))
                    .collect();
                let said_no: Vec<&str> = [
                    ("system-prompt", !init::system_prompt::enabled(&home_dir)),
                    ("short-name", init::pb_link::declined(&home_dir)),
                    (
                        "plugin",
                        playbook::plugin_update::setup_blocked(&home_dir).is_some(),
                    ),
                ]
                .into_iter()
                .filter_map(|(id, no)| no.then_some(id))
                .collect();
                let chose_on: &[&str] = if init::attribution::enabled(&home_dir) {
                    &[init::attribution::PART]
                } else {
                    &[]
                };
                let items =
                    init::wizard::items_for(&flags, security_on, &skipped, &said_no, chose_on);
                print!("{}", init::wizard::welcome(&caps));
                let stdin = std::io::stdin();
                let mut input = stdin.lock();
                let mut out = std::io::stdout();
                let finish = playbook::ui::wizard::run(
                    &caps,
                    &items,
                    &init::wizard::summary_detail,
                    &mut input,
                    &mut out,
                );
                match finish {
                    Ok(playbook::ui::wizard::Finish::Confirmed(answers)) => {
                        for (item, on) in items.iter().zip(answers) {
                            if let Some(p) = init::catalog::part(&item.choice.id) {
                                guided.push((p.id, on));
                            }
                        }
                    }
                    Ok(_) => {
                        println!("Cancelled. Nothing was changed.");
                        std::process::exit(1);
                    }
                    Err(err) => {
                        eprintln!("setup: {err}");
                        std::process::exit(1);
                    }
                }
            }
            let terminal = init::choices::stdio_is_interactive();
            let mut choices = only.unwrap_or_else(|| {
                init::choices::resolve_with(
                    &flags,
                    asking,
                    !init::choices::unattended(&flags, terminal),
                    init::choices::plugin_default(
                        !init::choices::unattended(&flags, terminal),
                        playbook::plugin_update::setup_blocked(&home_dir),
                    ),
                    security_config,
                    &mut |question, default| {
                        init::wizard::guided_answer(&guided, question, default)
                    },
                )
            });
            let only_run = only.is_some();
            // Naming the part is consent that overrides the CI and config guards.
            let force_plugin = flags.plugin || flags.only.iter().any(|n| n == "plugin");
            // --yes takes the default, which an earlier no or install without
            // the prompt keeps off (ADR 0044 decision 5).
            if init::choices::prompt_may_be_held_off(
                &flags,
                choices.system_prompt,
                asking,
                only_run,
            ) {
                if let Some(reason) =
                    init::system_prompt::held_off(&home_dir, &home_dir.join(".claude"))
                {
                    choices.system_prompt = false;
                    eprintln!(
                        "setup: the system prompt stays off because {reason}; to turn it on, run: playbook setup --only system-prompt"
                    );
                }
            }
            if only_run && choices.telemetry {
                println!("playbook setup --only: this run changes only the parts you named.");
                if let Some(line) = skip_telemetry_question {
                    println!("  {line}");
                } else {
                    for line in init::telemetry::plan_lines(&common::home_dir().join(".claude")) {
                        println!("  telemetry: {line}");
                    }
                }
            }
            if !only_run && init::choices::unattended(&flags, terminal) && !flags.dry_run {
                let left = init::choices::LeftOff {
                    telemetry_skipped: skip_telemetry_question.is_some(),
                    prompt_missing: !init::system_prompt::enabled(&home_dir)
                        || !init::system_prompt::path(&home_dir).is_file(),
                    pb_missing: init::path::command_on_path(
                        "pb",
                        &std::env::var_os("PATH").unwrap_or_default(),
                    )
                    .is_none(),
                    attribution_on: init::attribution::enabled(&home_dir),
                };
                for line in init::choices::unattended_lines(&flags, &choices, &left) {
                    eprintln!("{line}");
                }
            }
            // Unattended runs already name the command; a guard that turned the
            // part off on its own says why.
            if !only_run
                && !choices.plugin
                && !flags.no_plugin
                && !flags.dry_run
                && !init::choices::unattended(&flags, terminal)
            {
                if let Some(why) = playbook::plugin_update::setup_blocked(&home_dir) {
                    println!(
                        "setup: the plugin was left as it is because {why}. To run it anyway: playbook setup --only plugin"
                    );
                }
            }
            let system_prompt = init::choices::system_prompt_answer(
                &flags,
                choices.system_prompt,
                asking && !only_run,
            );
            let home = common::home_dir();
            let claude_home = home.join(".claude");
            let plugin_root = std::env::var("CLAUDE_PLUGIN_ROOT").ok();
            let self_root = if flags.dry_run {
                // A dry run must not write the embedded copies.
                init::self_root::seed_root(plugin_root.as_deref())
            } else {
                init::self_root::resolve_or_embedded(plugin_root.as_deref(), &home)
            };
            let path_setup = std::env::current_exe()
                .ok()
                .filter(|_| choices.path)
                .map(|exe| init::path::Setup {
                    bin_dir: init::path::resolve_bin_dir(
                        &std::env::var_os("PATH").unwrap_or_default(),
                        &exe,
                    ),
                    shell: init::path::PathShell::detect(
                        &std::env::var("SHELL").unwrap_or_default(),
                    ),
                });
            let repo = manifest::check::toplevel().zip(common::paths::repo_scoped_dir(
                common::paths::RepoScope::Worktree,
            ));
            let paths = InitPaths {
                plugin: match (choices.plugin, force_plugin) {
                    (false, _) => init::run::PluginRun::Skip,
                    (true, false) => init::run::PluginRun::Guarded,
                    (true, true) => init::run::PluginRun::Forced,
                },
                plugin_root: playbook::plugin_root::find_here(),
                repo,
                self_root,
                claude_home,
                completions: choices.completions.then(|| init::completion::Setup {
                    shell: init::completion::Shell::detect(
                        &std::env::var("SHELL").unwrap_or_default(),
                    ),
                }),
                pb_target: std::env::current_exe()
                    .ok()
                    .map(|exe| init::pb_link::Target {
                        exe,
                        install_dir: playbook::uninstall::install_dir(&home),
                        path_var: std::env::var_os("PATH").unwrap_or_default(),
                    }),
                home,
                system_prompt,
                path_setup,
                hooks: choices.hooks,
                statusline: choices.statusline,
                security: choices.security,
                telemetry: choices.telemetry,
                telemetry_replace_off: init::choices::replace_off_ok(
                    &flags,
                    choices.telemetry,
                    only_run,
                ),
                only: only_run,
                short_name: init::choices::short_name_choice(
                    &flags,
                    choices.short_name,
                    asking,
                    init::pb_link::declined(&common::home_dir()),
                ),
                attribution: choices.attribution.resolve(&common::home_dir()),
                policy_guard: choices.policy_guard,
                codex_hooks: choices.codex_hooks,
            };
            if flags.dry_run {
                print!("{}", init::preview::dry_run(&paths, &choices));
                return;
            }
            let outcome = init::run::run(&paths);
            print!("{}", init::wizard::progress(&caps, &outcome.steps));
            for warning in &outcome.warnings {
                eprintln!("setup: warning: {warning}");
            }
            let changed: Vec<&str> = outcome
                .steps
                .iter()
                .filter(|s| s.status == StepStatus::Wired)
                .map(|s| s.name)
                .collect();
            let on_parts: Vec<&str> = [
                ("hooks", choices.hooks),
                ("statusline", choices.statusline),
                ("security", choices.security),
            ]
            .iter()
            .filter(|(_, on)| *on)
            .map(|(id, _)| *id)
            .collect();
            if !only_run {
                print!("{}", init::catalog::summary(&changed, &on_parts));
            }
            if outcome
                .steps
                .iter()
                .any(|s| s.name == "telemetry" && s.status == StepStatus::Wired)
            {
                println!(
                    "Telemetry is set up. Restart Claude Code (close it and open it again) so the new settings apply; sessions that are already open keep the old settings."
                );
            }
            if !outcome.ok() {
                let failed = outcome
                    .steps
                    .iter()
                    .filter(|s| s.status == StepStatus::Failed)
                    .count();
                eprintln!("setup: {failed} step(s) failed; see above");
                std::process::exit(1);
            }
        }
        Command::Hooks { sub } => match sub {
            playbook::HooksCommand::List => print!("{}", init::catalog::hooks_list()),
        },
        Command::Settings { sub } => match sub {
            SettingsCommand::Gen { src, seed } => match settings::gen::generate(&src, &seed) {
                Ok(output) => print!("{output}"),
                Err(err) => {
                    eprintln!("gen-shared-settings: {err}");
                    std::process::exit(2);
                }
            },
            // Exit 1: CI keys on it.
            SettingsCommand::Check {
                template,
                repo_root,
            } => match settings::check::check(&template, &repo_root) {
                Ok(msg) => println!("{msg}"),
                Err(err) => {
                    eprintln!("check-shared-settings: {err}");
                    std::process::exit(1);
                }
            },
        },
        Command::Memory { sub } => match sub {
            MemoryCommand::Rebuild { json } => {
                if let Err(err) = hooks::rebuild_memory_graph::rebuild_now() {
                    eprintln!("memory rebuild: {err}");
                    std::process::exit(1);
                }
                if !json {
                    println!("memory: memory.db rebuilt");
                }
                print_memory_stats("memory rebuild", json);
            }
            MemoryCommand::Stats { json } => print_memory_stats("memory stats", json),
            MemoryCommand::Export { json: _ } => {
                let root = common::paths::playbook_root();
                // Folds an older memory.graph.json into the database first.
                hooks::rebuild_memory_graph::migrate_json_graph(&root.join("memory"));
                let rendered = match hooks::rebuild_memory_graph::export_json(&root) {
                    Ok(Some(text)) => Ok(text),
                    Ok(None) => hooks::rebuild_memory_graph::rebuild_now().and_then(|()| {
                        hooks::rebuild_memory_graph::export_json(&root)
                            .map(|t| t.unwrap_or_default())
                    }),
                    Err(err) => Err(err),
                };
                match rendered {
                    Ok(text) => println!("{text}"),
                    Err(err) => {
                        eprintln!("memory export: {err}");
                        std::process::exit(1);
                    }
                }
            }
            MemoryCommand::Claude {
                sub: MemoryClaudeCommand::Import { dry_run, from },
            } => {
                let root = manifest::check::toplevel()
                    .unwrap_or_else(|| std::path::PathBuf::from(cc::logical_cwd()));
                let claude_memory_dir = from.unwrap_or_else(|| {
                    cc::sessions::project_dir(&cc::claude_dir(), &root.to_string_lossy())
                        .join("memory")
                });
                let slug = common::repo::repo_slug();
                let dest_dir = match slug.split_once('/') {
                    Some((owner, repo)) => common::paths::memory_dir().join(owner).join(repo),
                    None => common::paths::memory_dir(),
                };
                let params = memory_import::Params {
                    claude_memory_dir,
                    dest_dir,
                    state_file: common::paths::playbook_root()
                        .join("state")
                        .join("claude-memory-import.json"),
                    dry_run,
                };
                match memory_import::run(&params) {
                    Ok(report) => {
                        println!("{}", memory_import::render(&report, dry_run));
                        if !dry_run && !report.copied.is_empty() {
                            if let Err(err) = hooks::rebuild_memory_graph::rebuild_now() {
                                eprintln!("memory claude import: graph rebuild failed: {err}");
                            }
                        }
                    }
                    Err(err) => {
                        eprintln!("memory claude import: {err}");
                        std::process::exit(1);
                    }
                }
            }
            MemoryCommand::Context { repo, graph } => {
                let repo = repo
                    .filter(|r| !r.is_empty())
                    .unwrap_or_else(common::repo_slug);
                let rendered = if let Some(graph) = graph {
                    json::memorycontext::render_with_source(&graph, &repo)
                } else {
                    // The default graph is derived state: rebuild it from the
                    // fact files when it is missing, so a fresh store still
                    // answers.
                    let mem_dir = common::paths::memory_dir();
                    let mut text = hooks::rebuild_memory_graph::graph_text(&mem_dir);
                    if text.is_none() && hooks::rebuild_memory_graph::rebuild_now().is_ok() {
                        text = hooks::rebuild_memory_graph::graph_text(&mem_dir);
                    }
                    json::memorycontext::render_text_with_source(
                        text.as_deref(),
                        "no memory graph in data/memory.db",
                        "the graph in data/memory.db is unreadable",
                        &repo,
                    )
                };
                println!("{rendered}");
            }
        },
        Command::Manifest { sub } => match sub {
            // Exit 1, not 2: the shell original used it and both CI lanes key
            // on it, matching the settings check convention above.
            ManifestCommand::Check { repo_root } => {
                let Some(root) = repo_root.or_else(manifest::check::toplevel) else {
                    // The shell's own wording when it had neither
                    // (check-manifest.sh:22).
                    eprintln!(
                        "check-manifest: not inside a git repository and no REPO_ROOT argument given"
                    );
                    std::process::exit(1);
                };
                match manifest::check::check(&root) {
                    Ok(msg) => println!("{msg}"),
                    Err(err) => {
                        eprintln!("check-manifest: {err}");
                        std::process::exit(1);
                    }
                }
            }
        },
        Command::Agents { sub } => match sub {
            // Exit 1, not 2: the shell original used it and CI keys on it,
            // matching the manifest and settings checks' convention above.
            AgentsCommand::Check { agents_dir } => {
                let Some(dir) = agents_dir.or_else(agents::check::default_dir) else {
                    eprintln!(
                        "check-agents: not inside a git repository and no AGENTS_DIR argument given"
                    );
                    std::process::exit(1);
                };
                match agents::check::check(&dir) {
                    Ok(msg) => println!("{msg}"),
                    Err(err) => {
                        eprintln!("check-agents: {err}");
                        std::process::exit(1);
                    }
                }
            }
        },
        Command::Usage {
            sub,
            json,
            summary,
            range,
            account,
            utc,
            local,
        } => {
            usage::tz::init(match (utc, local) {
                (true, _) => Some(usage::tz::Zone::Utc),
                (_, true) => Some(usage::tz::Zone::Local),
                _ => None,
            });
            let paths = usage::run::Paths::real();
            let opts = usage::entry::Opts {
                json,
                summary,
                range,
                account,
            };
            match usage::entry::run(sub, &opts, &paths, usage::entry::interactive()) {
                Ok(output) => {
                    if !output.is_empty() {
                        println!("{}", output.trim_end());
                    }
                }
                Err(err) => {
                    eprintln!("usage: {err}");
                    std::process::exit(1);
                }
            }
        }
        Command::Code { sub } => match playbook::code::run(sub, &playbook::code::Env::real()) {
            Ok(output) => print!("{output}"),
            Err(err) => {
                eprintln!("code: {err}");
                std::process::exit(1);
            }
        },
        Command::Sessions { sub, json } => {
            let paths = usage::run::Paths::real();
            let home = common::session::home_dir();
            let result = match sub {
                Some(SessionsCommand::Sweep { .. }) => {
                    playbook::sessions::sweep_dry_run(&paths, &home)
                }
                Some(SessionsCommand::List { json: list_json }) => {
                    playbook::sessions::run(json || list_json, &paths, &home)
                }
                None => playbook::sessions::run(json, &paths, &home),
            };
            match result {
                Ok(output) => print!("{output}"),
                Err(err) => {
                    eprintln!("sessions: {err}");
                    std::process::exit(1);
                }
            }
        }
        Command::Watch { sub } => match playbook::watch::run(sub.as_ref()) {
            Ok(output) => print!("{output}"),
            Err(err) => {
                eprintln!("watch: {err}");
                std::process::exit(1);
            }
        },
        Command::Gate { sub } => match sub {
            GateCommand::Snapshot { plan_slug, phases } => {
                let result = gate_plans_dir()
                    .and_then(|dir| gate::snapshot::snapshot(&dir, &plan_slug, &phases));
                match result {
                    Ok(paths) => {
                        for p in paths {
                            println!("{}", p.display());
                        }
                    }
                    Err(err) => {
                        eprintln!("gate snapshot: {err}");
                        std::process::exit(1);
                    }
                }
            }
            GateCommand::Record {
                plan_slug,
                command,
                phase,
                input,
                source,
                stuck,
            } => match gate_source(source, &plan_slug, Some(&phase)).and_then(|source| {
                gate::record::run_with(&plan_slug, &command, &phase, &input, &source, stuck)
            }) {
                Ok(round) => {
                    let slug = common::repo_slug();
                    let cap = gate::max_rounds(
                        &common::home_dir(),
                        (!slug.is_empty()).then_some(slug.as_str()),
                    );
                    println!(
                        "gate record: recorded {phase} verdict for {plan_slug} (round {round} of {cap})"
                    );
                }
                Err(err) => {
                    eprintln!("gate record: {err}");
                    std::process::exit(1);
                }
            },
            GateCommand::Check {
                plan_slug,
                command,
                phases,
                source,
                json: true,
            } => match gate_source(source, &plan_slug, None)
                .and_then(|source| gate::check::run_json(&plan_slug, &command, &phases, &source))
            {
                Ok((doc, ok)) => {
                    println!("{doc}");
                    if !ok {
                        if phases.is_empty() {
                            eprintln!("gate check: {}", gate::check::NO_PHASES);
                        }
                        std::process::exit(1);
                    }
                }
                Err(err) => {
                    eprintln!("gate check: {err}");
                    std::process::exit(1);
                }
            },
            GateCommand::Check {
                plan_slug,
                command,
                phases,
                source,
                json: false,
            } => match gate_source(source, &plan_slug, None)
                .and_then(|source| gate::check::run(&plan_slug, &command, &phases, &source))
            {
                Ok(output) => println!("{output}"),
                Err(err) => {
                    eprintln!("gate check: {err}");
                    std::process::exit(1);
                }
            },
        },
        Command::Sanitize { sub } => {
            let (kind, file, check) = match sub {
                SanitizeCommand::Commit { file, check } => (sanitize::Kind::CommitMsg, file, check),
                SanitizeCommand::Pr { file, check } => (sanitize::Kind::PrText, file, check),
            };
            match sanitize::run(kind, &file, check) {
                Ok(lines) => {
                    lines.iter().for_each(|line| eprintln!("{line}"));
                    if check && !lines.is_empty() {
                        std::process::exit(1);
                    }
                }
                // A commit-msg hook must never block a commit, so only
                // `--check` turns an unreadable file into a failing exit, and
                // its own code tells CI the file was not read.
                Err(err) if check => {
                    eprintln!("sanitize: {err}");
                    std::process::exit(2);
                }
                Err(err) => eprintln!("sanitize: warning: {err}; leaving the file as it is"),
            }
        }
        Command::Ci { json, strict, dir } => match ci::run(dir.as_deref(), json, strict) {
            Ok((text, code)) => {
                println!("{text}");
                std::process::exit(code);
            }
            Err(err) => {
                eprintln!("ci: {err}");
                std::process::exit(1);
            }
        },
        Command::Segment { sub } => {
            let result = match sub {
                SegmentCommand::Branch {
                    naming,
                    topology,
                    prev_branch,
                    land,
                } => {
                    let args = playbook::segment::BranchArgs {
                        kind: &naming.kind,
                        plan_slug: &naming.plan_slug,
                        n: naming.n,
                        title: &naming.title,
                        topology: match topology {
                            SegmentTopology::Stacked => playbook::segment::Topology::Stacked,
                            SegmentTopology::Independent => {
                                playbook::segment::Topology::Independent
                            }
                            SegmentTopology::Single => playbook::segment::Topology::Single,
                        },
                        default_branch: &naming.default_branch,
                        prev_branch: prev_branch.as_deref(),
                        land,
                    };
                    playbook::segment::branch(
                        &naming.dir.clone().unwrap_or_else(|| ".".into()),
                        &args,
                    )
                }
                SegmentCommand::Size { base, dir } => {
                    playbook::segment::size(&dir.unwrap_or_else(|| ".".into()), &base)
                }
            };
            match result {
                Ok(line) => println!("{line}"),
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            }
        }
        Command::Eval { sub } => match sub {
            EvalCommand::Triage { fixtures, prompt } => {
                std::process::exit(eval::review_triage(fixtures, prompt));
            }
            EvalCommand::Bench {
                cases,
                role,
                id,
                model,
                effort,
                runs,
                max_cost_usd,
                jobs,
                json,
                list,
                strategy,
            } => {
                let root = eval::repo_root();
                let opts = eval::bench::Options {
                    cases: cases.unwrap_or_else(|| root.join(eval::bench::DEFAULT_CASES)),
                    roles: role,
                    ids: id,
                    models: model,
                    efforts: effort,
                    runs,
                    max_cost_usd,
                    jobs,
                    json,
                    list,
                    strategies: strategy,
                    plugin_root: playbook::plugin_root::in_checkout(&root),
                };
                std::process::exit(eval::bench::run(&opts));
            }
        },
        Command::Pr { sub } => {
            let gh = pr::shared::RealGhClient;
            match sub {
                PrCommand::Prepare {
                    base,
                    ticket,
                    dir,
                    json,
                    args_stdin,
                } => {
                    if json {
                        let text = args_stdin.then(read_stdin_to_string);
                        let (report, _) =
                            pr::prepare::run_json(&gh, text.as_deref(), base, ticket, dir);
                        // --json always exits 0; the report carries the code.
                        println!("{report}");
                        return;
                    }
                    enter_dir("pr prepare", dir.as_deref());
                    match pr::prepare::run(&gh, base.as_deref(), ticket.as_deref()) {
                        Ok(output) => println!("{output}"),
                        Err(err) => {
                            eprintln!("pr prepare: {err}");
                            std::process::exit(1);
                        }
                    }
                }
                PrCommand::Land {
                    pr,
                    timeout,
                    interval,
                    settle,
                    dir,
                } => {
                    let opts = pr::land::Opts {
                        timeout: std::time::Duration::from_secs(timeout),
                        interval: std::time::Duration::from_secs(interval),
                        settle: std::time::Duration::from_secs(settle),
                        // Test only: end the wait after this many polls.
                        max_polls: std::env::var("PB_LAND_MAX_POLLS")
                            .ok()
                            .and_then(|v| v.parse().ok()),
                        ..pr::land::Opts::default()
                    };
                    let report = pr::land::land_real(dir.map(std::path::PathBuf::from), &pr, &opts);
                    println!("{}", pr::land::render(&report));
                    std::process::exit(report.exit_code);
                }
                PrCommand::Create {
                    dir,
                    all,
                    update,
                    staged: _,
                    message,
                    message_file,
                    title,
                    body,
                    body_file,
                    base,
                    ready,
                    draft,
                    auto_merge,
                    wait,
                    timeout,
                    dry_run,
                    ticket,
                    issue,
                    json,
                    branch,
                    auto,
                } => {
                    let die = |message: String| -> ! {
                        if json {
                            let v = deliver::failure_json(&deliver::Failure {
                                code: deliver::exit::REFUSED,
                                message,
                                details: Vec::new(),
                            });
                            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                        } else {
                            eprintln!("pr create: {message}");
                        }
                        std::process::exit(deliver::exit::REFUSED)
                    };
                    let read = |file: Option<String>, text: Option<String>| -> Option<String> {
                        match file {
                            Some(f) if f == "-" => Some(read_stdin_to_string()),
                            Some(f) => match std::fs::read_to_string(&f) {
                                Ok(t) => Some(t),
                                Err(e) => die(format!("could not read {f}: {e}")),
                            },
                            None => text,
                        }
                    };
                    if message_file.as_deref() == Some("-") && body_file.as_deref() == Some("-") {
                        die("--message-file and --body-file cannot both read stdin".into());
                    }
                    let message = read(message_file, message);
                    let body = read(body_file, body);
                    let dir = dir.map(|d| absolute_from_cwd(&d));
                    if let Some(d) = &dir {
                        if let Err(e) = std::env::set_current_dir(d) {
                            die(format!("--dir {d}: {e}"));
                        }
                    }
                    let cwd = std::env::current_dir().unwrap_or_default();
                    if let Some(expected) = &branch {
                        if let Err(why) = common::inline_args::guard(&cwd, expected) {
                            if json {
                                let mut v = deliver::failure_json(&deliver::Failure {
                                    code: deliver::exit::REFUSED,
                                    message: why,
                                    details: Vec::new(),
                                });
                                v["verdict"] = "WRONG_DIRECTORY".into();
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&v).unwrap_or_default()
                                );
                            } else {
                                eprintln!("pr create: WRONG_DIRECTORY: {why}");
                            }
                            std::process::exit(deliver::exit::REFUSED);
                        }
                    }
                    let slug = common::repo_slug();
                    let repo_slug = (!slug.is_empty()).then_some(slug.as_str());
                    let mut opts = deliver::Options::new(cwd);
                    opts.auto = auto;
                    opts.stage = if all {
                        deliver::Stage::All
                    } else if update {
                        deliver::Stage::Tracked
                    } else {
                        deliver::Stage::Staged
                    };
                    opts.message = message;
                    opts.title = title;
                    opts.body = body;
                    opts.base = base;
                    opts.ready = if ready {
                        Some(true)
                    } else if draft {
                        Some(false)
                    } else {
                        None
                    };
                    opts.default_draft = pr::create::draft_setting(&common::home_dir(), repo_slug);
                    opts.auto_merge = auto_merge;
                    opts.wait = wait.then(|| std::time::Duration::from_secs(timeout));
                    opts.dry_run = dry_run;
                    opts.rules = Some(pr::body_rules::Rules::load(&common::home_dir(), repo_slug));
                    opts.ticket = ticket;
                    opts.issue = issue;
                    opts.signoff = commit_signoff_on();
                    match deliver::run(&opts) {
                        Ok(report) => {
                            if json {
                                println!("{}", report.to_json());
                            } else {
                                print!("{}", report.to_text());
                            }
                            std::process::exit(report.code);
                        }
                        Err(f) => {
                            if json {
                                let v = deliver::failure_json(&f);
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&v).unwrap_or_default()
                                );
                            } else {
                                eprintln!("pr create: {}", f.message);
                                for d in &f.details {
                                    eprintln!("  - {d}");
                                }
                            }
                            std::process::exit(f.code);
                        }
                    }
                }
                PrCommand::Comments { args } => match pr::comments::run(&args) {
                    Ok(text) => println!("{text}"),
                    Err(err) => {
                        eprintln!("{err}");
                        std::process::exit(1);
                    }
                },
                PrCommand::Reply {
                    pr,
                    thread,
                    issue: _,
                    body_file,
                    repo,
                } => {
                    let target = match thread {
                        Some(id) => pr::reply::Target::Thread(id),
                        None => pr::reply::Target::Issue,
                    };
                    let req = pr::reply::Request {
                        pr: &pr,
                        target,
                        body_file: &body_file,
                        repo: repo.as_deref(),
                    };
                    match pr::reply::run(&req) {
                        Ok(text) => println!("{text}"),
                        Err(err) => {
                            eprintln!("error: {err}");
                            std::process::exit(1);
                        }
                    }
                }
                PrCommand::Stack { args } => match pr::stack::run(&args) {
                    Ok(text) => println!("{text}"),
                    Err(err) => {
                        eprintln!("{err}");
                        std::process::exit(1);
                    }
                },
                PrCommand::Rules {
                    plugin_root,
                    commit,
                } => {
                    let home = common::home_dir();
                    let root = plugin_root
                        .or_else(|| {
                            let resolved = playbook::plugin_root::resolve_here().ok();
                            let plugin = resolved.as_ref().map(|r| r.root.display().to_string());
                            let found = playbook::plugin_root::rules_root(resolved, &home)?;
                            if found.source == playbook::plugin_root::Source::Embedded {
                                let why = match plugin {
                                    Some(dir) => format!("the plugin at {dir} lacks the rule files"),
                                    None => "no playbook plugin was found".to_string(),
                                };
                                eprintln!(
                                    "pr rules: {why}, so the copies embedded in playbook {} are used",
                                    env!("CARGO_PKG_VERSION")
                                );
                            }
                            Some(found.root)
                        })
                        .unwrap_or_default();
                    let dir = std::env::temp_dir()
                        .join(format!("create-pr-step0-{}", std::process::id()));
                    let printed = if commit {
                        pr::rules::extract_commit(&root, &dir).map(|p| p.display().to_string())
                    } else {
                        pr::rules::extract(&root, &dir).map(|ex| pr::rules::report(&ex))
                    };
                    match printed {
                        Ok(text) => println!("{text}"),
                        Err(err) => {
                            eprintln!("ERROR: {err}");
                            std::process::exit(1);
                        }
                    }
                }
                PrCommand::CiWait {
                    pr,
                    timeout,
                    interval,
                    all,
                    settle,
                } => {
                    std::thread::sleep(std::time::Duration::from_secs(settle));
                    let verdict = pr::wait::ci_wait(
                        std::time::Duration::from_secs(timeout),
                        &mut || pr::wait::read_checks(&pr, all),
                        &mut |line| println!("{line}"),
                        &mut || std::thread::sleep(std::time::Duration::from_secs(interval)),
                    );
                    println!("CI_VERDICT={verdict}");
                }
                PrCommand::LandWait {
                    pr,
                    timeout,
                    interval,
                } => {
                    let verdict = pr::wait::land_wait(
                        std::time::Duration::from_secs(timeout),
                        &mut || pr::wait::read_land_line(&pr),
                        &mut |line| println!("{line}"),
                        &mut || std::thread::sleep(std::time::Duration::from_secs(interval)),
                    );
                    println!("LAND_VERDICT={verdict}");
                }
                PrCommand::Merge {
                    pr,
                    no_wait,
                    admin,
                    match_head,
                    timeout,
                    interval,
                    settle,
                    dir,
                    json,
                } => {
                    let stack_dir = dir.as_deref().map(std::path::PathBuf::from);
                    let stacked = (no_wait || admin) && pr::land::is_stacked_real(stack_dir, &pr);
                    if admin && stacked {
                        eprintln!("{}", pr::merge::ADMIN_ON_STACK);
                        std::process::exit(2);
                    }
                    let direct = (no_wait || admin) && !stacked;
                    let merged = if direct {
                        enter_dir("pr merge", dir.as_deref());
                        let (rc, output) = pr::wait::merge(&pr, admin, match_head.as_deref());
                        if !json {
                            println!("{}", pr::wait::merge_text(admin, rc, &output));
                            std::process::exit(0);
                        }
                        pr::merge::from_direct(&pr, admin, rc, &output)
                    } else {
                        let opts = pr::land::Opts {
                            timeout: std::time::Duration::from_secs(timeout),
                            interval: std::time::Duration::from_secs(interval),
                            settle: std::time::Duration::from_secs(settle),
                            max_polls: None,
                            no_wait,
                            match_head,
                        };
                        let dir = dir.map(std::path::PathBuf::from);
                        pr::merge::from_land(&pr::land::land_real(dir, &pr, &opts))
                    };
                    if json {
                        println!("{}", merged.envelope());
                    } else {
                        println!("{}", merged.render());
                        std::process::exit(merged.outcome.code);
                    }
                }
                PrCommand::Checks {
                    pr,
                    watch,
                    fail_fast,
                    required,
                    timeout,
                    interval,
                    json,
                } => {
                    let opts = pr::checks::Opts {
                        watch,
                        fail_fast,
                        required,
                        timeout: std::time::Duration::from_secs(timeout),
                        interval: std::time::Duration::from_secs(interval),
                    };
                    let report = pr::checks::run(&pr, &opts);
                    if json {
                        println!("{}", report.envelope());
                    } else {
                        println!("{}", report.render());
                        std::process::exit(report.code());
                    }
                }
                PrCommand::Status {
                    pr,
                    json,
                    flag,
                    dir,
                } => std::process::exit(run_pr_status(pr, json, flag, dir.as_deref())),
                PrCommand::Review {
                    sub:
                        PrReviewCommand::Triage {
                            pr,
                            base,
                            dir,
                            scope,
                            model,
                        },
                } => {
                    enter_dir("pr review triage", dir.as_deref());
                    let pr_arg = pr.map(|n| n.to_string());
                    let rules =
                        |scope| review::suggest::run(pr_arg.as_deref(), base.as_deref(), scope);
                    let outcome = match pr::triage::collect(pr, base.as_deref()) {
                        Ok(facts) => pr::triage::triage(
                            &pr::triage::ClaudeRunner,
                            &rules,
                            &facts,
                            scope,
                            model,
                        ),
                        Err(err) => pr::triage::failed(format!("could not collect the PR: {err}")),
                    };
                    println!("{}", pr::triage::render(&outcome));
                }
            }
        }
        Command::Route {
            kind,
            tier,
            failures,
            json,
        } => {
            let home = common::home_dir();
            let cwd = std::env::current_dir().unwrap_or_default();
            match playbook::routing::run(
                &home,
                &home.join(".claude"),
                &cwd,
                &kind,
                &tier,
                failures,
                json,
            ) {
                Ok(out) => println!("{out}"),
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(2);
                }
            }
        }
        Command::Sync { sub } => std::process::exit(playbook::sync::run(sub)),
        Command::State { sub } => {
            let root = common::paths::playbook_root_from(&common::home_dir());
            match sub {
                playbook::StateCommand::List { prefix, json } => {
                    match playbook::state::list(&root, prefix.as_deref().unwrap_or("")) {
                        Ok(rows) if json => {
                            let map: serde_json::Map<String, serde_json::Value> = rows
                                .into_iter()
                                .map(|(k, v)| (k, serde_json::Value::String(v)))
                                .collect();
                            println!("{}", serde_json::Value::Object(map));
                        }
                        Ok(rows) => {
                            for (key, value) in rows {
                                println!("{key}\t{value}");
                            }
                        }
                        Err(err) => {
                            eprintln!("state: {err}");
                            std::process::exit(1);
                        }
                    }
                }
            }
        }
        Command::Skills { sub } => match sub {
            SkillsCommand::Sync {
                agent,
                dry_run,
                json,
                refresh,
            } => std::process::exit(if refresh {
                run_skills_refresh()
            } else {
                run_skills_sync(agent.as_deref(), dry_run, json)
            }),
        },
        Command::Config { sub } => {
            let home = common::home_dir();
            for warning in config::adopt_late_config(&home) {
                eprintln!("config: warning: {warning}");
            }
            let slug = common::repo_slug();
            let repo_slug = if slug.is_empty() {
                None
            } else {
                Some(slug.as_str())
            };
            match sub {
                ConfigCommand::Get { key } => {
                    let canonical = config::keys::canonical_key(&key);
                    print_key_notice(&args, &key, canonical);
                    match config::resolve_valid(canonical, &home, repo_slug) {
                        Ok(resolved) => print_resolved("config get", canonical, resolved),
                        Err(err) => {
                            eprintln!("config get: {err}");
                            std::process::exit(1);
                        }
                    }
                }
                ConfigCommand::Set {
                    key: given,
                    value,
                    org,
                    global,
                } => {
                    let key = config::keys::canonical_key(&given).to_string();
                    print_key_notice(&args, &given, &key);
                    if org && global {
                        eprintln!("config set: --org and --global are mutually exclusive");
                        std::process::exit(1);
                    }
                    // A per-user key goes to the global tier unless `--org`
                    // asks otherwise, which `write::set` then refuses.
                    let tier = if global || (config::keys::is_user_key(&key) && !org) {
                        config::write::Tier::Global
                    } else if org {
                        config::write::Tier::Org
                    } else {
                        config::write::Tier::Repo
                    };
                    let value_json = match config::parse::parse_config_value(&key, &value) {
                        Ok(value_json) => value_json,
                        Err(err) => {
                            eprintln!("config set: {err}");
                            std::process::exit(1);
                        }
                    };
                    let shown = value_json.to_string();
                    match config::write::set(tier, &key, &value_json, &home, repo_slug) {
                        Ok(()) => {
                            println!("config set: {key} updated to {shown}");
                            if tier == config::write::Tier::Global && config::keys::is_shared(&key)
                            {
                                refresh_sync_files(&home);
                            }
                            if key == "usage.telemetry" && tier != config::write::Tier::Global {
                                println!(
                                    "telemetry: Claude Code settings follow the global value, so they were not changed; to change them, run: playbook config set --global usage.telemetry {}",
                                    value.trim()
                                );
                            } else if key == "usage.telemetry" {
                                let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());
                                let epoch = common::time::now_epoch_secs();
                                match init::telemetry::sync_setting(
                                    &home,
                                    &home.join(".claude"),
                                    &cwd,
                                    epoch,
                                    shown == "\"auto\"",
                                ) {
                                    Ok(lines) => lines.iter().for_each(|l| println!("{l}")),
                                    Err(err) => {
                                        eprintln!("config set: telemetry: {err}");
                                        std::process::exit(1);
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            eprintln!("config set: {err}");
                            std::process::exit(1);
                        }
                    }
                }
                ConfigCommand::Export => {
                    let root = common::paths::playbook_root_from(&home);
                    match config::store::export(&root) {
                        Ok(doc) => println!("{doc:#}"),
                        Err(err) => {
                            eprintln!("config export: {err}");
                            std::process::exit(1);
                        }
                    }
                }
                ConfigCommand::Import { file } => {
                    let raw = if file == "-" {
                        common::stdin::read_piped()
                    } else {
                        std::fs::read_to_string(&file).map_err(|err| err.to_string())
                    };
                    let doc = raw
                        .map_err(|err| format!("cannot read {file}: {err}"))
                        .and_then(|raw| {
                            serde_json::from_str::<serde_json::Value>(&raw)
                                .map_err(|err| format!("{file} is not valid JSON: {err}"))
                        });
                    let root = common::paths::playbook_root_from(&home);
                    match doc {
                        Ok(doc) => match config::store::import(&root, &doc) {
                            Ok(count) => println!("config import: {count} setting(s) stored"),
                            Err(err) => {
                                eprintln!("config import: {err}");
                                std::process::exit(1);
                            }
                        },
                        Err(err) => {
                            eprintln!("config import: {err}");
                            std::process::exit(1);
                        }
                    }
                }
                ConfigCommand::List => {
                    for &key in config::keys::KNOWN_KEYS {
                        match config::resolve_valid(key, &home, repo_slug) {
                            Ok(resolved) => print_resolved("config list", key, resolved),
                            Err(err) => {
                                eprintln!("config list: {err}");
                                std::process::exit(1);
                            }
                        }
                    }
                }
            }
        }
        Command::Plan {
            sub:
                PlanCommand::Brief {
                    plan,
                    wus,
                    out,
                    worktree,
                    worktree_base,
                    verify,
                    repo,
                    graph,
                    facts_tokens,
                },
        } => {
            let text = std::fs::read_to_string(&plan).unwrap_or_else(|err| {
                eprintln!("error: {}: {err}", plan.display());
                std::process::exit(1);
            });
            let repo = repo
                .filter(|r| !r.is_empty())
                .unwrap_or_else(common::repo_slug);
            let graph_json = match graph {
                Some(graph) => std::fs::read_to_string(graph).ok(),
                None => hooks::rebuild_memory_graph::graph_text(&common::paths::memory_dir()),
            };
            let plan_path = plan.display().to_string();
            let cwd = std::env::current_dir().unwrap_or_default();
            let facts_source = |wu: &brief::Wu| brief::index_facts(wu, &cwd);
            let ctx = brief::Context {
                plan_path: &plan_path,
                worktree: worktree.as_deref(),
                worktree_base: worktree_base.as_deref(),
                verify: verify.as_deref(),
                repo: &repo,
                graph_json: graph_json.as_deref(),
                code_facts: (facts_tokens > 0).then_some(brief::CodeFacts {
                    budget_tokens: facts_tokens,
                    source: &facts_source,
                }),
            };
            match brief::write_briefs(&text, &wus, &out, &ctx) {
                Ok(paths) => {
                    for p in paths {
                        println!("{}", p.display());
                    }
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            }
        }
        Command::Plan {
            sub:
                PlanCommand::Cleanup {
                    slug,
                    dry_run,
                    json,
                    stale,
                    older_than,
                    yes,
                },
        } => {
            let dir = match gate_plans_dir() {
                Ok(dir) => dir,
                Err(err) => {
                    eprintln!("error: playbook path plans failed: {err}");
                    std::process::exit(1);
                }
            };
            let (result, dry) = if stale {
                (
                    plan_cleanup::cleanup_stale(
                        &dir,
                        older_than,
                        yes,
                        std::time::SystemTime::now(),
                    ),
                    !yes,
                )
            } else {
                let slug = slug.unwrap_or_default();
                if !plan_cleanup::valid_slug(&slug) {
                    eprintln!("error: invalid slug '{slug}': use letters, digits, '-', '_' and '.', not starting with '.'");
                    std::process::exit(1);
                }
                (plan_cleanup::cleanup_slug(&dir, &slug, dry_run), dry_run)
            };
            match result {
                Ok(names) => {
                    if json {
                        println!("{}", serde_json::json!({ "dry_run": dry, "files": names }));
                    } else {
                        for n in &names {
                            println!("{n}");
                        }
                        if dry && !names.is_empty() {
                            eprintln!("dry run: nothing removed");
                        }
                    }
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(2);
                }
            }
        }
        Command::Plan {
            sub: PlanCommand::List { json },
        } => {
            let Some(dir) = storage::kind_dir(storage::Kind::Plans) else {
                println!("PLAN_PATH_ERROR: could not resolve the plans folder; this repo needs a git 'origin' remote");
                std::process::exit(1);
            };
            let root = playbook::manifest::check::toplevel()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let plans = playbook::plans::list(&dir, &root);
            if json {
                println!("{}", playbook::plans::to_json(&plans));
            } else {
                println!("{}", playbook::plans::render(&plans));
            }
        }
        Command::Plan {
            sub: PlanCommand::Checkpoint { slug, write },
        } => {
            let Some(dir) = storage::kind_dir(storage::Kind::Plans) else {
                eprintln!(
                    "error: playbook path plans failed: this repo needs a git 'origin' remote"
                );
                std::process::exit(1);
            };
            let path = match planning::checkpoint_path(&dir, &slug) {
                Ok(path) => path,
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            };
            if write {
                let content = read_stdin_to_string();
                let saved = std::fs::create_dir_all(&dir)
                    .and_then(|()| planning::write_checkpoint(&path, &content));
                match saved {
                    Ok(()) => println!("{}", path.display()),
                    Err(err) => println!(
                        "couldn't save checkpoint: {err}, continuing without resume safety this turn"
                    ),
                }
            } else {
                if let Err(err) = std::fs::create_dir_all(&dir) {
                    eprintln!("error: could not create {}: {err}", dir.display());
                    std::process::exit(1);
                }
                println!("{}", path.display());
                println!("{}", if path.is_file() { "found" } else { "not-found" });
            }
        }
        Command::Glossary {
            sub: GlossaryCommand::Add { entry },
        } => {
            let Some(root) = manifest::check::toplevel() else {
                eprintln!("error: not in a git repo");
                std::process::exit(1);
            };
            println!("{}", planning::glossary_add(&root, &entry).display());
        }
        Command::Tools {
            sub: ToolsCommand::Map { json, check },
        } => {
            if json {
                println!("{}", tools_map::render_json());
            } else if check {
                let text = tools_map::render();
                let cost = tools_map::tokens(&text);
                println!(
                    "tool map: about {cost} tokens, budget {}",
                    tools_map::TOKEN_BUDGET
                );
                if cost > tools_map::TOKEN_BUDGET {
                    std::process::exit(1);
                }
            } else {
                print!("{}", tools_map::render());
            }
        }
        Command::Commands { group, json } => {
            let root = playbook::command_index::tree();
            let out = if json {
                playbook::command_index::render_json(&root, &group).map(|t| t + "\n")
            } else {
                playbook::command_index::render(&root, &group)
            };
            match out {
                Ok(text) => print!("{text}"),
                Err(err) => {
                    eprintln!("{err}");
                    std::process::exit(2);
                }
            }
        }
        Command::Adr {
            sub: AdrCommand::Next,
        } => {
            let Some(root) = manifest::check::toplevel() else {
                eprintln!("error: not in a git repo");
                std::process::exit(1);
            };
            let today = hooks::precompact_warn::current_timestamp();
            let today = today.split(' ').next().unwrap_or("");
            match adr::next_line(&root, today) {
                Ok(line) => println!("{line}"),
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            }
        }
        Command::Prose {
            sub: ProseCommand::Check { files, strict },
        } => {
            let github = std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true");
            match prose::check(&files, github) {
                Ok(report) => {
                    for line in &report.lines {
                        println!("{line}");
                    }
                    println!(
                        "{} findings, {} hard, {} files checked",
                        report.total,
                        report.hard,
                        files.len()
                    );
                    if strict && report.hard > 0 {
                        std::process::exit(1);
                    }
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            }
        }
        Command::Atlassian {
            sub: AtlassianCommand::Check,
        } => {
            let path = std::env::var_os("PATH").unwrap_or_default();
            let (text, usable) = atlassian::check(&path);
            println!("{text}");
            if !usable {
                std::process::exit(1);
            }
        }
        Command::Learn {
            sub: LearnCommand::Preflight,
        } => {
            let dir = std::env::current_dir().unwrap_or_default();
            match learn::preflight(&dir, &common::home_dir()) {
                Ok(text) => println!("{text}"),
                Err(err) => {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                }
            }
        }
        Command::Learn {
            sub: LearnCommand::Collect { roles },
        } => {
            let dir = std::env::current_dir().unwrap_or_default();
            let mut out = serde_json::Map::new();
            for role in &roles {
                let (key, value) = match role.as_str() {
                    "git" => ("git", learn::collect::git_history(&dir)),
                    _ => ("structure", learn::collect::code_structure(&dir)),
                };
                out.insert(key.to_string(), value);
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::Value::Object(out)).unwrap_or_default()
            );
        }
        Command::Commit { sub } => {
            let dir = std::env::current_dir().unwrap_or_default();
            match sub {
                CommitCommand::Prepare { all, update, amend } => {
                    let opts = commit::prepare::Options {
                        stage_all: all,
                        stage_update: update,
                        amend,
                    };
                    match commit::prepare::run(&dir, opts) {
                        Ok(text) => println!("{text}"),
                        Err(err) => {
                            eprintln!("ERROR: {err}");
                            std::process::exit(1);
                        }
                    }
                }
                CommitCommand::All {
                    message,
                    auto,
                    no_signoff,
                    json,
                    args_stdin,
                } => commit_drafted(
                    &dir,
                    commit::all::Options {
                        message,
                        auto,
                        no_signoff,
                        stage_all: true,
                        ..Default::default()
                    },
                    json,
                    args_stdin,
                ),
                CommitCommand::Run {
                    message_file: _,
                    all,
                    message,
                    json,
                    args_stdin,
                    amend,
                    auto,
                    no_signoff,
                    no_type_check,
                    no_prose_check,
                } if all || message.is_some() || json => commit_drafted(
                    &dir,
                    commit::all::Options {
                        message,
                        auto,
                        no_signoff,
                        stage_all: all,
                        amend,
                        no_type_check,
                        no_prose_check,
                        ..Default::default()
                    },
                    json,
                    args_stdin,
                ),
                CommitCommand::Run {
                    message_file,
                    amend,
                    auto,
                    no_signoff,
                    no_type_check,
                    no_prose_check,
                    ..
                } => {
                    let (path, temp) = match message_file {
                        Some(p) => (p, false),
                        None => {
                            let p = std::env::temp_dir()
                                .join(format!("playbook-commit-{}.txt", std::process::id()));
                            if let Err(e) = std::fs::write(&p, read_stdin_to_string()) {
                                eprintln!("ERROR: could not store the message: {e}");
                                std::process::exit(1);
                            }
                            (p, true)
                        }
                    };
                    let signoff_on = commit_signoff_on();
                    let opts = commit::run::Options {
                        amend,
                        auto,
                        no_signoff,
                        no_type_check,
                        no_prose_check,
                    };
                    let result = commit::run::run(&dir, &path, opts, signoff_on);
                    if temp {
                        let _ = std::fs::remove_file(&path);
                    }
                    match result {
                        Ok(text) => println!("{text}"),
                        Err(err) => {
                            eprintln!("{err}");
                            std::process::exit(1);
                        }
                    }
                }
            }
        }
        Command::Review { sub } => match sub {
            ReviewCommand::Prepare { kind, args, auto } => {
                let kind = if kind == "deep" {
                    review::Kind::Deep
                } else {
                    review::Kind::Quick
                };
                match review::prepare(kind, &args, auto) {
                    Ok(text) => println!("{text}"),
                    Err(err) => {
                        eprintln!("error: {err}");
                        std::process::exit(1);
                    }
                }
            }
            ReviewCommand::Suggest {
                pr,
                scope,
                base,
                dir,
                json,
            } => {
                enter_dir("review suggest", dir.as_deref());
                match review::suggest::run(pr.as_deref(), base.as_deref(), scope) {
                    Ok(sug) if json => println!(
                        "{}",
                        serde_json::to_string_pretty(&sug).unwrap_or_else(|_| "{}".to_string())
                    ),
                    Ok(sug) => println!("{}", review::suggest::render(&sug)),
                    Err(err) => {
                        eprintln!("error: {err}");
                        std::process::exit(1);
                    }
                }
            }
            ReviewCommand::Checks { dir } => println!("{}", review::checks::run_checks(&dir)),
            ReviewCommand::Size {
                base,
                head,
                all_lenses,
                dir,
            } => {
                let dir = dir.unwrap_or_else(|| std::path::PathBuf::from("."));
                match review::size::run(&dir, &base, &head, all_lenses) {
                    Ok(line) => println!("{line}"),
                    Err(err) => {
                        eprintln!("error: {err}");
                        std::process::exit(1);
                    }
                }
            }
            ReviewCommand::Triage {
                sub:
                    ReviewTriageCommand::Merge {
                        lenses,
                        tiers,
                        forced,
                    },
            } => {
                let forced: Result<Vec<_>, _> = forced
                    .iter()
                    .map(|f| review::lens::parse_forced(f))
                    .collect();
                let forced = forced.unwrap_or_else(|err| {
                    eprintln!("error: {err}");
                    std::process::exit(2);
                });
                let raw = match tiers.as_deref() {
                    None => String::new(),
                    Some("-") => read_stdin_to_string(),
                    Some(path) => std::fs::read_to_string(path).unwrap_or_default(),
                };
                let merged = review::lens::merge(&lenses, &raw, &forced);
                println!("{}", review::lens::render(&merged));
            }
        },
        Command::Path { kind, create } => {
            if matches!(kind, playbook::PathKind::PluginRoot) {
                match playbook::plugin_root::resolve_here() {
                    Ok(found) => {
                        for note in &found.skipped {
                            eprintln!("playbook path: skipped {note}");
                        }
                        println!("{}", found.root.display());
                    }
                    Err(err) => {
                        eprintln!("playbook path: {err}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            if matches!(kind, playbook::PathKind::Data) {
                let dir = common::data::data_dir();
                if create {
                    if let Err(err) = common::data::ensure_data_dir() {
                        eprintln!("playbook path: could not create {}: {err}", dir.display());
                        std::process::exit(1);
                    }
                }
                println!("{}", dir.display());
                return;
            }
            // The repo-local `.claude/` folders of very old versions move into the
            // worktree folder first; `kind_dir` then copies from there.
            if let (Some(repo_root), Some(base)) = (
                playbook::manifest::check::toplevel(),
                common::repo_scoped_dir(common::RepoScope::Worktree),
            ) {
                if let Err(err) = playbook::gate::db::migrate_legacy_repo_local(&repo_root, &base) {
                    eprintln!("playbook path: {err}");
                    std::process::exit(1);
                }
            }
            let stored = match kind {
                playbook::PathKind::Plans => Some(storage::Kind::Plans),
                playbook::PathKind::Implement => Some(storage::Kind::Implement),
                playbook::PathKind::Worktrees => Some(storage::Kind::Worktrees),
                playbook::PathKind::Handoff
                | playbook::PathKind::Data
                | playbook::PathKind::PluginRoot => None,
            };
            let resolved = if matches!(kind, playbook::PathKind::Handoff) {
                // Outside a repo this is the old shared folder, the same one
                // `handoff save` writes to there.
                Some(handoff::root())
            } else {
                stored.and_then(storage::kind_dir)
            };
            match resolved {
                Some(dir) => {
                    if create {
                        if let Err(err) = std::fs::create_dir_all(&dir) {
                            eprintln!("playbook path: could not create {}: {err}", dir.display());
                            std::process::exit(1);
                        }
                    }
                    println!("{}", dir.display());
                }
                None => {
                    eprintln!(
                        "playbook path: could not resolve the storage directory; this repo needs \
                     a git 'origin' remote, refusing to fall back to a repo-local path"
                    );
                    std::process::exit(1);
                }
            }
        }
        Command::Worktree {
            sub: WorktreeCommand::Review { sub },
        } => match sub {
            ReviewWorktreeCommand::Setup { pr, head_sha } => {
                match worktree::review::setup(&pr, &head_sha, playbook::gc::default_clock().now()) {
                    Ok(path) => println!("{path}"),
                    Err(err) => {
                        eprintln!("review worktree: {err}");
                        std::process::exit(1);
                    }
                }
            }
            ReviewWorktreeCommand::Teardown { path } => worktree::review::teardown(&path),
        },
        Command::Worktree { sub } => {
            let home = common::home_dir();
            let slug = common::repo_slug();
            let repo_slug = if slug.is_empty() {
                None
            } else {
                Some(slug.as_str())
            };
            let repo_root = std::env::current_dir().unwrap_or_default();
            match sub {
                WorktreeCommand::Sweep { dry_run } => {
                    let clock = playbook::gc::default_clock();
                    match playbook::gc::worktrees::sweep_locked(
                        &home,
                        &*clock,
                        (&repo_root, repo_slug),
                        dry_run,
                    ) {
                        Ok(playbook::gc::worktrees::SweepRun::Swept(report)) => {
                            for line in report {
                                println!("{line}");
                            }
                        }
                        Ok(playbook::gc::worktrees::SweepRun::Busy) => {
                            eprintln!("worktree sweep: gc is running; try again in a moment");
                            std::process::exit(1);
                        }
                        Err(err) => {
                            eprintln!("worktree sweep: {err}");
                            std::process::exit(1);
                        }
                    }
                }
                WorktreeCommand::Review { .. } => unreachable!("handled above"),
                WorktreeCommand::Remove { path } => {
                    let now_epoch = crate::common::time::now_secs();
                    match worktree::remove(&repo_root, &home, repo_slug, &path, now_epoch) {
                        Ok(line) => println!("{line}"),
                        Err(err) => {
                            eprintln!("worktree remove: {err}");
                            std::process::exit(1);
                        }
                    }
                }
            }
        }
        Command::Deps { sub } => match sub {
            DepsCommand::Ensure { brewfile, builtin } => {
                // `--builtin` reads the copy compiled into this binary, so
                // /playbook:setup needs no plugin file.
                let file = if builtin {
                    match init::self_root::embedded_brewfile(&common::home_dir()) {
                        Ok(path) => path,
                        Err(err) => {
                            eprintln!("ensure-deps: cannot write the built-in Brewfile: {err}");
                            std::process::exit(1);
                        }
                    }
                } else {
                    brewfile.unwrap_or_else(|| std::path::PathBuf::from("Brewfile"))
                };
                if !deps::ensure_all(&file) {
                    std::process::exit(1);
                }
            }
        },
        Command::Doc { sub } => {
            let home = common::home_dir();
            let code = match sub {
                DocCommand::Read {
                    file,
                    json,
                    offset,
                    limit,
                    section,
                } => doc::read(
                    &home,
                    &file,
                    json,
                    &doc::SliceRequest {
                        offset,
                        limit,
                        section,
                    },
                ),
                DocCommand::Status => doc::status(&home),
            };
            std::process::exit(code);
        }
        Command::Doctor {
            fix: true,
            sub: None,
        } => {
            let home = common::home_dir();
            // The one settings change doctor --fix makes, and only after a yes
            // at a terminal (ADR 0042). It runs first, so the heal report and
            // record below see the answer.
            let fixed = init::attribution::fix_with_consent(
                &home,
                &home.join(".claude").join("settings.json"),
                init::choices::stdio_is_interactive(),
                &mut init::attribution::ask_on_terminal,
            );
            let failed = match fixed {
                Ok(Some(step)) => {
                    println!("{}", step.render());
                    false
                }
                Ok(None) => false,
                Err(err) => {
                    eprintln!("doctor --fix: {err}");
                    true
                }
            };
            let code =
                playbook::migrate_cli::run(&home, playbook::migrate_cli::Mode::Run, false, false);
            std::process::exit(if failed { 1 } else { code });
        }
        Command::Migrate {
            status,
            dry_run,
            json,
            no_plugin,
        } => {
            let mode = match (status, dry_run) {
                (true, _) => playbook::migrate_cli::Mode::Status,
                (_, true) => playbook::migrate_cli::Mode::DryRun,
                _ => playbook::migrate_cli::Mode::Run,
            };
            std::process::exit(playbook::migrate_cli::run(
                &common::home_dir(),
                mode,
                json,
                !no_plugin,
            ));
        }
        Command::Doctor {
            fix: false,
            sub: None,
        } => run_doctor_check(false),
        Command::Doctor { sub: Some(sub), .. } => match sub {
            DoctorCommand::Check { json } => run_doctor_check(json),
            DoctorCommand::Models { json } => {
                let home = common::home_dir();
                let cwd = std::env::current_dir().unwrap_or_default();
                println!(
                    "{}",
                    playbook::models::report(&home, &home.join(".claude"), &cwd, json)
                );
            }
            DoctorCommand::Migrations => {
                let home = common::home_dir();
                let claude_home = home.join(".claude");
                let self_root = playbook::plugin_root::find_here();
                let ctx = init::migrate::Ctx {
                    home,
                    claude_home,
                    self_root,
                    repo: None,
                    epoch: common::time::now_epoch_secs(),
                };
                for line in init::migrate::pending_manual(&ctx) {
                    println!("{line}");
                }
            }
        },
        Command::Trust { path } => {
            let _ = trust::run(&path);
        }
        Command::Handoff { sub } => {
            let result = match sub {
                HandoffCommand::Save { dir, session } => {
                    let text = read_stdin_to_string();
                    handoff::run_save(dir.as_deref(), &text, session.as_deref())
                }
                HandoffCommand::Show { all, dir } => handoff::run_show(dir.as_deref(), all),
                HandoffCommand::Status => Ok(handoff::run_status()),
                HandoffCommand::Template => Ok(handoff::compose::TEMPLATE.to_string()),
                HandoffCommand::Facts { dir } => Ok(handoff::compose::facts(std::path::Path::new(
                    dir.as_deref().unwrap_or("."),
                ))),
                HandoffCommand::Check => {
                    let mut text = String::new();
                    match std::io::stdin().read_to_string(&mut text) {
                        Err(e) => Err(format!("could not read stdin: {e}")),
                        Ok(_) => {
                            let problems = handoff::compose::check(&text);
                            if problems.is_empty() {
                                Ok("handoff ok".to_string())
                            } else {
                                Err(problems.join("; "))
                            }
                        }
                    }
                }
            };
            match result {
                Ok(output) => println!("{output}"),
                Err(err) => {
                    eprintln!("handoff: {err}");
                    std::process::exit(1);
                }
            }
        }
        Command::Upgrade {
            version,
            check,
            list,
            pre,
            yes,
            json,
            refresh_check,
            refresh_plugin,
            print_notes,
            from,
            no_plugin,
        } => {
            if refresh_check {
                update::check::run_refresh();
                return;
            }
            if refresh_plugin {
                playbook::plugin_update::refresh_plugin(&common::home_dir());
                return;
            }
            // On stdout, so the install script can tell an answer from the
            // usage error of a binary without the flag.
            if let Some(file) = print_notes {
                print!(
                    "{}",
                    playbook::release_notes::print_notes(&file, from.as_deref())
                );
                return;
            }
            let opts = update::Options {
                version,
                check,
                list,
                pre,
                yes,
            };
            let mut plugin: Option<playbook::plugin_update::Ensured> = None;
            let result = update::Env::real().and_then(|env| {
                let root = common::paths::playbook_root_from(&common::home_dir());
                let _lock = if opts.check || opts.list {
                    None
                } else {
                    Some(update::auto::try_lock(&root).map_err(|e| e.to_string())?)
                };
                let mut outcome = update::run(
                    &opts,
                    &env,
                    &update::download::HttpFetcher {
                        local_http: false,
                        timeout: update::download::DOWNLOAD_TIMEOUT,
                    },
                    &update::verify::GhAttest,
                    &update::swap::run_version,
                )?;
                update::finish_with_heal(&mut outcome, &env);
                // After the binary and its heal, also when the binary was
                // already current (ADR 0034, 2026-10-19 amendment). A run that
                // left the binary alone, such as a Homebrew install or a
                // declined question, leaves the plugin alone too.
                let step_due = matches!(
                    outcome.status,
                    update::Status::Upgraded | update::Status::Current
                );
                if step_due && !opts.check {
                    let running = outcome
                        .installed
                        .clone()
                        .unwrap_or_else(|| env.current_version.clone());
                    plugin = playbook::plugin_update::run_for_upgrade(
                        &common::home_dir(),
                        no_plugin,
                        &running,
                    );
                    if !json {
                        if let Some(step) = &plugin {
                            outcome.out.extend(step.lines());
                        }
                    }
                }
                if opts.check {
                    outcome
                        .out
                        .push(update::check::status_at(&common::home_dir()).line);
                }
                Ok(outcome)
            });
            // Shown again after the swap and the heal, also when the heal
            // failed (ADR 0037).
            if let Ok(outcome) = &result {
                eprint!("{}", outcome.notes);
            }
            if json {
                println!("{}", update::json_report(&result, plugin.as_ref()));
                return;
            }
            match result {
                Ok(outcome) if std::io::stdout().is_terminal() => {
                    print!(
                        "{}",
                        update::style::render(&playbook::ui::Caps::detect(), &outcome)
                    );
                }
                Ok(outcome) => {
                    for line in &outcome.out {
                        println!("{line}");
                    }
                    for warning in &outcome.warnings {
                        eprintln!("warning: {warning}");
                    }
                }
                Err(err) => {
                    eprintln!("upgrade: {err}");
                    std::process::exit(1);
                }
            }
        }
        Command::Plugin { sub } => match sub {
            PluginCommand::Install { json } => {
                let probe = playbook::plugin_update::Probe::for_home(&common::home_dir());
                std::process::exit(playbook::plugin_update::print_install_command(&probe, json));
            }
            PluginCommand::Upgrade { json } => {
                let probe = playbook::plugin_update::Probe::for_home(&common::home_dir());
                std::process::exit(playbook::plugin_update::print_upgrade_command(&probe, json));
            }
        },
        Command::Mode { sub } => {
            let to_mode = |arg: ModeArg| match arg {
                ModeArg::Ask => common::mode::Mode::Ask,
                ModeArg::Auto => common::mode::Mode::Auto,
            };
            match sub {
                ModeCommand::Auto => exit_on_mode_error(mode::run_set(common::mode::Mode::Auto)),
                ModeCommand::Ask => exit_on_mode_error(mode::run_set(common::mode::Mode::Ask)),
                ModeCommand::Status {
                    json,
                    flag,
                    command,
                    args,
                } => {
                    if json && (command.is_some() || !args.is_empty()) {
                        print_run_context(command.as_deref(), &args);
                    } else {
                        println!("{}", mode::run_status(flag.map(to_mode), json));
                    }
                }
            }
        }
    }
}

/// Prints the run mode and, when `command` is named, its effort ceiling;
/// exits 2 when `args` holds both `--auto` and `--ask`.
fn print_run_context(command: Option<&str>, args: &str) {
    let ceiling = || {
        let home = common::home_dir();
        let claude_home = home.join(".claude");
        let cwd = std::env::current_dir().unwrap_or_default();
        let claude = effort::claude_cap(&claude_home, &cwd);
        let root = playbook::plugin_root::find_here();
        let resolved = effort::component::resolve_capped(
            effort::component::Kind::Command,
            command.unwrap_or_default(),
            &home,
            claude.as_deref(),
            root.as_deref(),
            None,
        );
        resolved.to_json()["ceiling"].as_str().map(str::to_string)
    };
    match mode::context::run_context(args, ceiling, command.is_some()) {
        Ok(json) => println!("{json}"),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(2);
        }
    }
}

/// Prints the memory graph summary for `playbook memory stats` and after a
/// rebuild; exits 1 when the database cannot be read.
fn print_memory_stats(label: &str, json: bool) {
    let repo = playbook::memory_stats::current_repo();
    match playbook::memory_stats::collect(&common::home_dir(), repo) {
        Ok(stats) if json => println!("{}", playbook::memory_stats::render_json(&stats)),
        Ok(stats) => print!("{}", playbook::memory_stats::render_text(&stats)),
        Err(err) => {
            eprintln!("{label}: {err}");
            std::process::exit(1);
        }
    }
}

/// This worktree's plans folder, where the gate source files live.
fn gate_plans_dir() -> Result<std::path::PathBuf, String> {
    storage::kind_dir(storage::Kind::Plans)
        .ok_or_else(|| "this repo needs a git 'origin' remote".to_string())
}

/// The `--source` given, else the phase's snapshot (or the shared draft when
/// no phase is named).
fn gate_source(given: Option<String>, slug: &str, phase: Option<&str>) -> Result<String, String> {
    if let Some(source) = given {
        return Ok(source);
    }
    let dir = gate_plans_dir()?;
    let path = match phase {
        Some(phase) => gate::snapshot::phase_path(&dir, slug, phase)?,
        None => gate::snapshot::shared_path(&dir, slug)?,
    };
    Ok(path.display().to_string())
}

fn exit_on_mode_error(result: Result<String, String>) {
    match result {
        Ok(output) => println!("{output}"),
        Err(err) => {
            eprintln!("mode: {err}");
            std::process::exit(1);
        }
    }
}

/// Lowercase tier label for `config get`/`config list` output, e.g. "repo"
/// rather than the raw `Source::Repo` debug casing.
fn source_label(source: config::Source) -> &'static str {
    match source {
        config::Source::Repo => "repo",
        config::Source::Org => "org",
        config::Source::Global => "global",
        config::Source::Default => "default",
    }
}

/// Refresh the files `pbsync` reads after a shared key changed. A failure only
/// warns, because the value itself is already stored.
fn refresh_sync_files(home: &std::path::Path) {
    if let Err(err) = config::sync_files::write_sync_files(home) {
        eprintln!("config set: warning: sync files not refreshed: {err}");
    }
}

/// Prints the alias line for a renamed config key, on a terminal and without
/// `--json`, as the command renames do.
fn print_key_notice(args: &[std::ffi::OsString], given: &str, canonical: &str) {
    let line = playbook::renames::key_notice(given, canonical);
    if let Some(line) =
        playbook::renames::printable_notice(args, line, std::io::stderr().is_terminal())
    {
        eprintln!("{line}");
    }
}

/// Print one `config get`/`list` line: a bare `deep`, not the JSON-quoted
/// `"deep"` a raw `Value`'s `Display` impl would print, so a shell script can
/// read it. An ignored invalid value is named in the source and warned about
/// on stderr.
fn print_resolved(
    command: &str,
    key: &str,
    (value, source, ignored): (
        serde_json::Value,
        config::Source,
        Option<config::IgnoredValue>,
    ),
) {
    let mut origin = source_label(source).to_string();
    if let Some(ignored) = ignored {
        eprintln!("{command}: warning: {}", ignored.warning);
        origin = format!("{origin}, {} value ignored", source_label(ignored.tier));
    }
    println!(
        "{key}: {} (source: {origin})",
        common::mode::value_text(&value)
    );
}

/// The detached body behind `skills sync --refresh`: prints nothing and
/// leaves its summary for the next session.
fn run_skills_refresh() -> i32 {
    let home = common::home_dir();
    let refreshed = skills_sync::refresh_report(&home, false);
    if let Some(line) = skills_sync::notice_line(&refreshed) {
        let root = common::paths::playbook_root_from(&home);
        playbook::notices::store_notice(&root, playbook::notices::SKILLS_SYNC_KEY, &line);
    }
    0
}

/// `playbook skills sync`: returns the exit code.
fn run_skills_sync(agent: Option<&str>, dry_run: bool, json: bool) -> i32 {
    use skills_sync::{apply, Agent};
    const NO_AGENTS: &str = "no other agents found (looked for Codex at ~/.codex)";
    let home = common::home_dir();
    let path = std::env::var("PATH").unwrap_or_default();
    let agents: Vec<Agent> = skills_sync::detect(&home, &path)
        .into_iter()
        .filter(|a| agent.is_none_or(|name| name == a.key()))
        .collect();
    if agents.is_empty() {
        println!("{NO_AGENTS}");
        return 0;
    }
    // Resolved after the agent check, so a machine with no other agent never
    // needs the plugin folder.
    let plugin = match playbook::plugin_root::resolve_here() {
        Ok(resolved) => resolved.root,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    let version = playbook::plugin_root::plugin_version(&plugin).unwrap_or_default();
    let rendered = skills_sync::plan::plan(&plugin)
        .and_then(|targets| skills_sync::render_targets(&plugin, &targets));
    let folders = match rendered {
        Ok(folders) => folders,
        Err(err) => {
            eprintln!("skills sync: {err}");
            return 1;
        }
    };
    if folders.is_empty() {
        // An empty plan would read as every folder dropped and remove them.
        eprintln!("skills sync: the plugin holds no skills or commands, nothing was changed");
        return 1;
    }
    let root = common::paths::playbook_root_from(&home);
    let target_root = home.join(skills_sync::TARGET_DIR);
    let mut lines = Vec::new();
    let mut clean = true;
    for agent in agents {
        let report = apply::apply(
            &root,
            agent,
            &target_root,
            &version,
            &folders,
            dry_run,
            &apply::Faults::default(),
        );
        clean &= report.is_clean();
        lines.extend(report.lines);
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&lines).unwrap_or_default()
        );
    } else {
        for line in &lines {
            println!("{line}");
        }
    }
    if !clean {
        eprintln!("skills sync: not every folder was synced");
    }
    i32::from(!clean)
}

/// Reads all of stdin into a `String`, for the `json` subcommands that
/// take their document piped in. Exits 1 on a read failure or when stdin is a
/// terminal: unlike [`read_hook_input`], these commands have no silent-empty
/// fallback that would make sense.
fn read_stdin_to_string() -> String {
    match common::stdin::read_piped() {
        Ok(input) => input,
        Err(err) => {
            eprintln!("playbook: {err}");
            std::process::exit(1);
        }
    }
}

/// Read the hook payload the same way common.py does: `HOOK_INPUT` env var
/// if set and non-empty, else all of stdin when stdin is not a tty, else
/// empty. Never panics; a read failure yields an empty payload.
fn read_hook_input() -> String {
    if let Ok(value) = std::env::var("HOOK_INPUT") {
        if !value.is_empty() {
            return value;
        }
    }
    if std::io::stdin().is_terminal() {
        return String::new();
    }
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    buf
}

/// Makes a relative path absolute against the current directory.
fn absolute_from_cwd(path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(p).to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

/// `pr status`: prints the report and returns the exit code. With
/// `json` every outcome goes through the envelope and the code is 0.
fn run_pr_status(pr: u64, json: bool, flag: Option<ModeArg>, dir: Option<&str>) -> i32 {
    use pr::status as fc;
    let home = common::home_dir();
    let mode_flag = flag.map(|arg| match arg {
        ModeArg::Ask => common::mode::Mode::Ask,
        ModeArg::Auto => common::mode::Mode::Auto,
    });
    let entered = match dir {
        Some(dir) => pr::shared::enter_dir(dir).map_err(fc::Failure::bad_input),
        None => Ok(()),
    };
    // The repo slug depends on the directory, so read it after entering --dir.
    let slug = common::repo_slug();
    let reader = fc::ConfigReader {
        home: home.clone(),
        repo_slug: Some(slug).filter(|s| !s.is_empty()),
    };
    let result = entered.and_then(|()| {
        fc::collect(
            pr,
            mode::report(mode_flag),
            &fc::RealGh,
            &reader,
            &fc::RealTriager,
        )
    });
    if json {
        return fc::emit(result);
    }
    match result {
        Ok(report) => {
            println!("{report:#}");
            0
        }
        Err(failure) => {
            eprintln!("pr status: {}", failure.error);
            failure.code
        }
    }
}

/// Moves into `--dir` before any git or gh call, so a shell that resets to
/// another checkout can still act on a worktree. Exits 1 on a bad path.
fn enter_dir(label: &str, dir: Option<&str>) {
    let Some(dir) = dir else { return };
    if let Err(err) = pr::shared::enter_dir(dir) {
        eprintln!("{label}: {err}");
        std::process::exit(1);
    }
}

/// The stage 1 report for `range`, or exit with the reason it cannot be made.
fn suggest_report(
    range: &str,
    home: &std::path::Path,
    claude: Option<&str>,
    root: Option<&std::path::Path>,
) -> effort::suggest::Report {
    let Some(range) = usage::query::Range::parse(Some(range)) else {
        eprintln!(
            "error: unknown range '{range}', use today, week, last-week, month, last-month, 30d, 60d, 90d or all"
        );
        std::process::exit(1);
    };
    match effort::suggest::run(
        home,
        claude,
        root,
        &common::paths::usage_db_dir().join("usage.db"),
        range,
        common::time::now_secs(),
    ) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }
}

/// The stage 1 report over 30 days, or the reason it cannot be made.
fn effort_report(
    home: &std::path::Path,
    claude: Option<&str>,
    root: Option<&std::path::Path>,
) -> Result<effort::suggest::Report, String> {
    let range = usage::query::Range::parse(Some("30d")).ok_or("bad range")?;
    effort::suggest::run(
        home,
        claude,
        root,
        &common::paths::usage_db_dir().join("usage.db"),
        range,
        common::time::now_secs(),
    )
}

/// The diff shape from `git diff --numstat <base>`, or why it cannot be read.
fn effort_diff_shape(base: &str) -> Result<effort::auto::Shape, String> {
    let mut command = std::process::Command::new("git");
    command.args(["diff", "--numstat", base, "--"]);
    let out = common::proc::run_with_timeout(&mut command, common::git::SLOW_TIMEOUT)
        .ok_or("could not run git (or it timed out)")?;
    if !out.status.success() {
        return Err(format!("git diff against {base} failed"));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    effort::auto::Shape::from_numstat(&text)
}

/// Runs the drafting commit shared by `commit run -A` and the legacy
/// `commit all`. With `--json` the report always exits 0; otherwise a failure
/// exits 1.
fn commit_drafted(dir: &std::path::Path, opts: commit::all::Options, json: bool, args_stdin: bool) {
    if json {
        let text = if args_stdin {
            read_stdin_to_string()
        } else {
            let flags = [
                (opts.auto, "--auto"),
                (opts.no_signoff, "--no-signoff"),
                (opts.stage_all, "-A"),
                (opts.amend, "-a"),
            ];
            flags
                .iter()
                .filter(|(on, _)| *on)
                .map(|(_, word)| format!(" {word}"))
                .collect()
        };
        println!(
            "{}",
            commit::all::run_inline(&text, opts, commit_signoff_on())
        );
        return;
    }
    match commit::all::run(dir, opts, commit_signoff_on()) {
        Ok(text) => println!("{text}"),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

/// Whether `commit.signOff` is on (the default).
fn commit_signoff_on() -> bool {
    let slug = common::repo_slug();
    config::resolve_valid(
        "commit.signOff",
        &common::home_dir(),
        (!slug.is_empty()).then_some(slug.as_str()),
    )
    .map(|(v, _, _)| v != serde_json::Value::Bool(false))
    .unwrap_or(true)
}
