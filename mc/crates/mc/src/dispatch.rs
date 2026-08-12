//! Parsing an invocation and routing it, including to plugins.

use clap::Parser as _;
use mc_common::error::{Error, Result};
use mc_common::paths::Paths;
use mc_common::plugin::Registry;
use mc_common::ui;

use crate::cli::{Cli, Command};
use crate::commands;
use crate::context::Ctx;

pub fn run(argv: Vec<String>) -> Result<()> {
    let cli = Cli::parse();
    let ctx = Ctx::system(argv);
    execute(&ctx, cli.command)
}

pub fn execute(ctx: &Ctx, command: Command) -> Result<()> {
    // Enforced here, once, from the table in `cli.rs` — rather than as the
    // first line of each handler, where a new command can forget it.
    command.requirement().enforce(&ctx.mc_bin(), &ctx.argv)?;

    match command {
        Command::Install(args) => install(ctx, args),
        Command::Upgrade(args) => upgrade(ctx, args),
        Command::Start(args) => commands::lifecycle::start(ctx, args.accept_eula),
        Command::Stop => commands::lifecycle::stop(ctx),
        Command::Restart(args) => commands::lifecycle::restart(ctx, args.accept_eula),
        Command::Status => commands::lifecycle::status(ctx),
        Command::Logs => commands::lifecycle::logs(ctx),
        Command::Delete => commands::delete::run(ctx),
        Command::Plugins => plugins(ctx),
        Command::Serve => commands::serve::run(ctx),
        Command::Shutdown => commands::shutdown::run(ctx),
        Command::Reload => commands::reload::run(ctx),
        Command::Man { topic } => man(ctx, topic.as_deref()),
        Command::Completions { shell } => completions(shell),
        Command::External(args) => external(ctx, args),
    }
}

fn install(ctx: &Ctx, args: crate::cli::InstallArgs) -> Result<()> {
    commands::install::install(
        ctx,
        commands::install::InstallArgs {
            server_type: args.server_type,
            version: args.version,
            pack: args.pack.map(std::path::PathBuf::from),
            assume_yes: args.yes,
            accept_eula: args.accept_eula,
            force: args.force,
        },
    )
}

fn upgrade(ctx: &Ctx, args: crate::cli::UpgradeArgs) -> Result<()> {
    commands::install::upgrade(
        ctx,
        commands::install::UpgradeArgs {
            server_type: args.server_type,
            version: args.version,
            pack: args.pack.map(std::path::PathBuf::from),
            assume_yes: args.yes,
            force: args.force,
            no_backup: args.no_backup,
        },
    )
}

/// `mc plugins` — what is installed, and what is broken.
fn plugins(ctx: &Ctx) -> Result<()> {
    let registry = Registry::discover(&ctx.paths);

    if registry.plugins().is_empty() && registry.problems().is_empty() {
        ui::info("No plugins installed.");
        ui::info(
            "Available: mc-rcon (console), mc-mgmt (console, 1.21.9+), \
             mc-backup (backups), mc-mrpack (modpacks)",
        );
        return Ok(());
    }

    // Resolved once, before the loop: electing probes each console, and
    // `mc plugins` is a read-only command an operator runs to find out why
    // something is not happening.
    let elected = registry.console(&ctx.paths).map(|p| p.name.as_str());

    for plugin in registry.plugins() {
        let commands: Vec<&str> = plugin.commands.iter().map(|c| c.name.as_str()).collect();
        let hooks: Vec<String> = plugin.hooks.iter().map(|h| h.event.to_string()).collect();
        println!("{}  (abi {})", plugin.name, plugin.abi);
        println!("  binary:   {}", plugin.bin.display());
        if !commands.is_empty() {
            println!("  commands: {}", commands.join(", "));
        }
        if !hooks.is_empty() {
            println!("  hooks:    {}", hooks.join(", "));
        }
        for provider in &plugin.providers {
            match provider.kind.as_str() {
                // Which console won is not visible anywhere else, and "my
                // countdown stopped happening" is otherwise a mystery with no
                // thread to pull.
                "console" => println!(
                    "  console:  {} (priority {}){}",
                    provider.name,
                    provider.priority,
                    match elected {
                        Some(name) if name == plugin.name => " — elected",
                        Some(_) => " — standing down",
                        None => " — not answering",
                    }
                ),
                "source" => println!(
                    "  provides: {} ({})",
                    provider.name,
                    provider.extensions.join(", ")
                ),
                // Reported through problems() as well; named here so the line
                // it appears on is next to the plugin it came from.
                other => println!("  provides: {} ({other}, unsupported)", provider.name),
            }
        }
    }

    // Reported, never silently dropped: a plugin that failed to load is a
    // command the operator expects to exist and does not.
    for problem in registry.problems() {
        ui::warn(problem);
    }
    Ok(())
}

/// Route a subcommand core does not implement to the plugin that declared it.
fn external(ctx: &Ctx, args: Vec<String>) -> Result<()> {
    let Some(name) = args.first() else {
        return Err(Error::config("No command given. Try: mc help"));
    };

    let registry = Registry::discover(&ctx.paths);

    // ONLY a name a plugin registered is dispatchable. Resolving to *some*
    // executable is not sufficient: without the registry, an internal helper
    // would be reachable from the command line, skipping the guards, the lock
    // and the config loading its real entry point performs first.
    if let Some((plugin, _)) = registry.command(name) {
        // exec replaces this process, so this only returns on failure.
        return Err(mc_common::plugin::exec_command(&ctx.paths, plugin, &args));
    }

    // A plugin that failed to load is the likeliest reason a command an
    // operator knows exists has just vanished. Say so before the generic
    // refusal.
    for problem in registry.problems() {
        ui::warn(problem);
    }

    let hint = match name.as_str() {
        "rcon" => "\nInstall it with: apt install mc-rcon",
        "mgmt" => "\nInstall it with: apt install mc-mgmt",
        "backup" | "restore" => "\nInstall it with: apt install mc-backup",
        _ => "",
    };
    Err(Error::config(format!("Unknown command: {name}{hint}")))
}

/// `mc man` — hand off to man(1) with the page that answers the question.
fn man(ctx: &Ctx, topic: Option<&str>) -> Result<()> {
    use clap::CommandFactory as _;
    // The core command list comes from the parser for the same reason
    // completions do: a subcommand added to `cli.rs` must resolve to mc(1)
    // without anyone remembering to add it here.
    let command = Cli::command();
    let core: Vec<&str> = command.get_subcommands().map(|s| s.get_name()).collect();

    let page = crate::manual::page_for(&Registry::discover(&ctx.paths), &core, topic)?;
    // exec replaces this process, so this only returns on failure.
    Err(crate::manual::open(&page))
}

fn completions(shell: clap_complete::Shell) -> Result<()> {
    use clap::CommandFactory as _;
    // Generated from the parser rather than hand-maintained, so a new
    // subcommand cannot ship with stale completions. Plugin subcommands are
    // discovered at runtime: the completion script reflects what is installed.
    let mut command = Cli::command();
    let paths = Paths::from_env();
    let registry = Registry::discover(&paths);

    for plugin in registry.plugins() {
        for cmd_decl in &plugin.commands {
            // Ask the plugin for its subcommands via `completions list`.
            // Falls back to no subcommands if the plugin doesn't support it.
            let subcommands = plugin_subcommands(plugin);

            let mut subcmd = clap::Command::new(
                Box::leak(cmd_decl.name.clone().into_boxed_str()) as &'static str
            )
            .about(Box::leak(cmd_decl.about.clone().into_boxed_str()) as &'static str);

            for sc in subcommands {
                subcmd = subcmd.subcommand(clap::Command::new(sc.name).about(sc.about));
            }

            command = command.subcommand(subcmd);
        }
    }

    // Generate to a buffer first so we can post-process.
    let mut buf = Vec::new();
    clap_complete::generate(shell, &mut command, "mc", &mut buf);
    let script = String::from_utf8(buf).map_err(|e| Error::other(e.to_string()))?;

    // Post-process for bash: only show subcommands unless a dash is typed,
    // and hide short flags to reduce noise.
    let output = match shell {
        clap_complete::Shell::Bash => postprocess_bash(&script),
        _ => script,
    };

    print!("{output}");
    Ok(())
}

/// Post-process a bash completion script to improve usability:
/// - Only show subcommands when no dash is typed
/// - Only show long flags when a dash is typed (short flags hidden)
fn postprocess_bash(script: &str) -> String {
    // First pass: split opts= lines into flags= and cmds= variables.
    let mut result = String::with_capacity(script.len());

    for line in script.lines() {
        if let Some(opts_match) = extract_opts(line) {
            let (flags, cmds) = split_opts(&opts_match);
            let indent = line.find('o').unwrap_or(0);
            let prefix = &line[..indent];
            result.push_str(&format!(
                "{}opts=\"{}\"\n{}flags=\"{}\"\n{}cmds=\"{}\"",
                prefix, opts_match, prefix, flags, prefix, cmds
            ));
            result.push('\n');
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }

    // Second pass: replace the completion logic line-by-line.
    // Match: if [[ ${cur} == -* || ${COMP_CWORD} -eq N ]] ; then
    let lines: Vec<&str> = result.lines().collect();
    let mut output = String::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines.get(i).copied().unwrap_or("");
        if line.contains("${cur} == -* || ${COMP_CWORD} -eq ") {
            let indent = line.find("if").unwrap_or(0);
            let prefix = &line[..indent];
            let cword = extract_comp_cword(line);
            output.push_str(prefix);
            output.push_str("if [[ ${cur} == --* ]]; then\n");
            output.push_str(prefix);
            output.push_str("    COMPREPLY=( $(compgen -W \"${flags}\" -- \"${cur}\") )\n");
            output.push_str(prefix);
            output.push_str("    return 0\n");
            output.push_str(prefix);
            output.push_str("elif [[ ${COMP_CWORD} -eq ");
            output.push_str(&cword.to_string());
            output.push_str(" ]]; then\n");
            output.push_str(prefix);
            output.push_str("    COMPREPLY=( $(compgen -W \"${cmds}\" -- \"${cur}\") )\n");
            output.push_str(prefix);
            output.push_str("    return 0\n");
            // Skip the next 2 lines (COMPREPLY and return 0)
            i += 3;
            // The fi line follows
            if lines.get(i).is_some_and(|l| l.trim() == "fi") {
                output.push_str(&format!("{}fi\n", prefix));
                i += 1;
            }
        } else {
            output.push_str(line);
            output.push('\n');
            i += 1;
        }
    }

    output
}

/// Extract the COMP_CWORD value from a line like "if [[ ${cur} == -* || ${COMP_CWORD} -eq 1 ]] ; then".
fn extract_comp_cword(line: &str) -> u8 {
    for (cword, pattern) in [
        (1, "-eq 1"),
        (2, "-eq 2"),
        (3, "-eq 3"),
        (4, "-eq 4"),
        (5, "-eq 5"),
    ] {
        if line.contains(pattern) {
            return cword;
        }
    }
    1
}

/// Extract the value from an opts= line.
fn extract_opts(line: &str) -> Option<String> {
    let opts_start = line.find("opts=\"")?;
    let value_start = opts_start + 6;
    let value_end = line.rfind('"')?;
    if value_end > value_start {
        Some(line[value_start..value_end].to_string())
    } else {
        None
    }
}

/// Split opts into flags (starting with -) and commands (not starting with -).
fn split_opts(opts: &str) -> (String, String) {
    let mut flags = Vec::new();
    let mut cmds = Vec::new();

    for word in opts.split_whitespace() {
        if word.starts_with('-') {
            flags.push(word);
        } else {
            cmds.push(word);
        }
    }

    (flags.join(" "), cmds.join(" "))
}

/// Query a plugin binary for its subcommands.
///
/// Runs `<bin> completions list` and parses the JSON output. Silently
/// returns an empty list if the plugin doesn't support the command —
/// older plugins ship without it.
struct Subcommand {
    name: &'static str,
    about: &'static str,
}

fn plugin_subcommands(plugin: &mc_common::plugin::Manifest) -> Vec<Subcommand> {
    let Ok(output) = std::process::Command::new(&plugin.bin)
        .args(["completions", "list"])
        .output()
    else {
        return Vec::new();
    };

    if !output.status.success() {
        return Vec::new();
    }

    let Ok(stdout) = String::from_utf8(output.stdout) else {
        return Vec::new();
    };

    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stdout) else {
        return Vec::new();
    };

    parsed
        .get("subcommands")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|obj| {
                    let name = obj.get("name")?.as_str()?;
                    let about = obj.get("about")?.as_str()?;
                    Some(Subcommand {
                        name: Box::leak(name.to_owned().into_boxed_str()) as &'static str,
                        about: Box::leak(about.to_owned().into_boxed_str()) as &'static str,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod completions_tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn completions_output_contains_core_commands() {
        let mut command = Cli::command();
        let paths = Paths::from_env();
        let registry = Registry::discover(&paths);

        for plugin in registry.plugins() {
            for cmd_decl in &plugin.commands {
                let mut subcmd = clap::Command::new(Box::leak(
                    cmd_decl.name.clone().into_boxed_str(),
                ) as &'static str)
                .about(Box::leak(cmd_decl.about.clone().into_boxed_str()) as &'static str);

                for sc in plugin_subcommands(plugin) {
                    subcmd = subcmd.subcommand(clap::Command::new(sc.name).about(sc.about));
                }

                command = command.subcommand(subcmd);
            }
        }

        let mut buf = Vec::new();
        clap_complete::generate(clap_complete::Shell::Bash, &mut command, "mc", &mut buf);
        let output = String::from_utf8(buf).unwrap();

        for core_cmd in ["install", "start", "stop", "status", "plugins"] {
            assert!(
                output.contains(core_cmd),
                "completions missing core command: {core_cmd}"
            );
        }
    }
}
