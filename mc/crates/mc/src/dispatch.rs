//! Parsing an invocation and routing it, including to plugins.

use clap::Parser as _;
use mc_common::error::{Error, Result};
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
            version: args.version,
            pack: args.pack.map(std::path::PathBuf::from),
            assume_yes: args.yes,
            accept_eula: args.accept_eula,
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
        ui::info("Available: mc-rcon (console), mc-backup (backups), mc-mrpack (modpacks)");
        return Ok(());
    }

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
            println!(
                "  provides: {} ({})",
                provider.name,
                provider.extensions.join(", ")
            );
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
        "backup" | "restore" => "\nInstall it with: apt install mc-backup",
        _ => "",
    };
    Err(Error::config(format!("Unknown command: {name}{hint}")))
}

fn completions(shell: clap_complete::Shell) -> Result<()> {
    use clap::CommandFactory as _;
    // Generated from the parser rather than hand-maintained, so a new
    // subcommand cannot ship with stale completions. Plugin subcommands are not
    // in here — they are discovered at runtime and completed by the shell
    // wrapper the package installs.
    let mut command = Cli::command();
    clap_complete::generate(shell, &mut command, "mc", &mut std::io::stdout());
    Ok(())
}
