//! The command surface.
//!
//! Every variant declares the privilege it needs, in one place, so that adding
//! a subcommand cannot skip the question. `mc serve`, `mc shutdown` and
//! `mc reload` are the ones that matter: systemd runs them as the `minecraft`
//! user under `ProtectSystem=strict`, and a root guard on any of them means the
//! server never starts — with a failure that looks like a config problem.

use clap::{Parser, Subcommand};
use mc_common::config::ServerType;
use mc_common::privilege::Requirement;

#[derive(Debug, Parser)]
#[command(
    name = "mc",
    version,
    about = "Minecraft server lifecycle manager",
    long_about = None,
    // clap's help lists only what core implements. Both lines below are the
    // parts an operator cannot get from it: what a plugin added, and where the
    // prose lives.
    after_help = "Plugins contribute commands of their own; 'mc plugins' lists them.\n\
                  Manual: mc man   (or man mc, man mc-config)",
    // Plugins contribute subcommands that clap knows nothing about, so an
    // unrecognised name must reach the dispatcher instead of being refused
    // here.
    allow_external_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install the server
    Install(InstallArgs),

    /// Upgrade the server to a new version
    Upgrade(UpgradeArgs),

    /// Start the server
    Start(EulaArgs),

    /// Stop the server (graceful if a console plugin is installed)
    Stop,

    /// Restart the server
    Restart(EulaArgs),

    /// Show the service state
    Status,

    /// Follow the server log
    Logs,

    /// Permanently remove the server
    Delete,

    /// List installed plugins and what they contribute
    Plugins,

    /// Run the server in the foreground (systemd ExecStart=)
    #[command(hide = true)]
    Serve,

    /// Warn players and stop gracefully (systemd ExecStop=)
    #[command(hide = true)]
    Shutdown,

    /// Ask the running server to reload its configuration (systemd ExecReload=)
    #[command(hide = true)]
    Reload,

    /// Open the manual page for mc, a command, or a topic
    Man {
        /// A command name, or "config" for the configuration file format
        #[arg(value_name = "TOPIC")]
        topic: Option<String>,
    },

    /// Print a shell completion script
    #[command(hide = true)]
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// A subcommand contributed by a plugin.
    #[command(external_subcommand)]
    External(Vec<String>),
}

#[derive(Debug, clap::Args)]
pub struct InstallArgs {
    /// Server type: vanilla, paper, fabric or neoforge
    #[arg(long = "type", short = 't', value_name = "TYPE")]
    pub server_type: Option<ServerType>,

    /// Minecraft version, or "latest"
    #[arg(long, short = 'v', value_name = "VER")]
    pub version: Option<String>,

    /// A modpack file, handled by a source-provider plugin
    #[arg(long = "file", short = 'f', value_name = "PACK")]
    pub pack: Option<String>,

    /// Install a missing Java runtime without prompting
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Accept the Minecraft EULA (https://www.minecraft.net/eula)
    #[arg(long)]
    pub accept_eula: bool,

    /// Reinstall over an existing server (overwrites server.jar, no backup)
    #[arg(long, short = 'F')]
    pub force: bool,
}

#[derive(Debug, clap::Args)]
pub struct UpgradeArgs {
    /// Server type: vanilla, paper, fabric or neoforge
    #[arg(long = "type", short = 't', value_name = "TYPE")]
    pub server_type: Option<ServerType>,

    /// Minecraft version, or "latest"
    #[arg(long, short = 'v', value_name = "VER")]
    pub version: Option<String>,

    /// A new modpack file, handled by a source-provider plugin
    #[arg(long = "file", short = 'f', value_name = "PACK")]
    pub pack: Option<String>,

    /// Install a missing Java runtime without prompting
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Reinstall even when already at the target version
    #[arg(long, short = 'F')]
    pub force: bool,

    /// Proceed without a pre-upgrade backup
    #[arg(long, short = 'n')]
    pub no_backup: bool,
}

#[derive(Debug, clap::Args)]
pub struct EulaArgs {
    /// Accept the Minecraft EULA (https://www.minecraft.net/eula)
    #[arg(long)]
    pub accept_eula: bool,
}

impl Command {
    /// What this command needs to be allowed to run.
    ///
    /// Exhaustive by construction — a new variant will not compile until it
    /// answers this.
    pub fn requirement(&self) -> Requirement {
        match self {
            // Writes MC_BASE, the config, or the unit state.
            Command::Install(_)
            | Command::Upgrade(_)
            | Command::Start(_)
            | Command::Stop
            | Command::Restart(_)
            | Command::Delete => Requirement::Root,

            // Reads files the service group can already reach.
            Command::Status | Command::Logs | Command::Plugins => Requirement::RootOrGroup,

            // systemd exec targets. MUST stay unprivileged: the unit runs these
            // as the minecraft user.
            Command::Serve | Command::Shutdown | Command::Reload => Requirement::ServiceAccount,

            // Prints text, or hands the terminal to man(1). Reading the manual
            // as root would be an odd thing to require, and man is a pager
            // running a shell — not something to hand elevated privileges to.
            Command::Completions { .. } | Command::Man { .. } => Requirement::None,

            // The plugin enforces its own guard: only it knows whether the
            // subcommand reads or writes. Core refusing on its behalf would
            // either be wrong or duplicate a decision it cannot see.
            Command::External(_) => Requirement::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn the_cli_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn systemd_exec_targets_are_unprivileged() {
        // The single most consequential row of the table: a root guard on any
        // of these means the unit never starts, and the failure reads as a
        // config problem rather than a permission one.
        for command in [Command::Serve, Command::Shutdown, Command::Reload] {
            assert_eq!(
                command.requirement(),
                Requirement::ServiceAccount,
                "{command:?} runs as the minecraft user under ProtectSystem=strict"
            );
        }
    }

    #[test]
    fn every_mutating_command_takes_the_root_guard() {
        for command in [
            Command::Install(InstallArgs {
                server_type: None,
                version: None,
                pack: None,
                yes: false,
                accept_eula: false,
                force: false,
            }),
            Command::Upgrade(UpgradeArgs {
                server_type: None,
                version: None,
                pack: None,
                yes: false,
                force: false,
                no_backup: false,
            }),
            Command::Start(EulaArgs { accept_eula: false }),
            Command::Stop,
            Command::Restart(EulaArgs { accept_eula: false }),
            Command::Delete,
        ] {
            assert_eq!(command.requirement(), Requirement::Root, "{command:?}");
        }
    }

    #[test]
    fn read_only_commands_are_reachable_by_the_service_group() {
        // The group is already the unit of access to these files, so demanding
        // root would protect nothing and leave mc less capable than the tools
        // it wraps.
        for command in [Command::Status, Command::Logs, Command::Plugins] {
            assert_eq!(
                command.requirement(),
                Requirement::RootOrGroup,
                "{command:?}"
            );
        }
    }

    #[test]
    fn an_unknown_subcommand_reaches_the_dispatcher() {
        // Plugins contribute names clap knows nothing about; refusing here
        // would make every plugin subcommand unreachable.
        let cli = Cli::try_parse_from(["mc", "rcon", "list"]).unwrap();
        match cli.command {
            Command::External(args) => assert_eq!(args, vec!["rcon", "list"]),
            other => panic!("expected External, got {other:?}"),
        }
    }

    #[test]
    fn install_short_hand_v_parses_version() {
        let cli = Cli::try_parse_from(["mc", "install", "-v", "1.21"]).unwrap();
        match cli.command {
            Command::Install(args) => assert_eq!(args.version.as_deref(), Some("1.21")),
            other => panic!("expected Install, got {other:?}"),
        }
    }

    #[test]
    fn install_short_hand_t_parses_type() {
        let cli = Cli::try_parse_from(["mc", "install", "-t", "paper"]).unwrap();
        match cli.command {
            Command::Install(args) => assert_eq!(args.server_type, Some(ServerType::Paper)),
            other => panic!("expected Install, got {other:?}"),
        }
    }

    #[test]
    fn install_short_hand_f_parses_pack() {
        let cli = Cli::try_parse_from(["mc", "install", "-f", "pack.mrpack"]).unwrap();
        match cli.command {
            Command::Install(args) => assert_eq!(args.pack.as_deref(), Some("pack.mrpack")),
            other => panic!("expected Install, got {other:?}"),
        }
    }

    #[test]
    fn install_short_hand_y_sets_yes() {
        let cli = Cli::try_parse_from(["mc", "install", "-y"]).unwrap();
        match cli.command {
            Command::Install(args) => assert!(args.yes),
            other => panic!("expected Install, got {other:?}"),
        }
    }

    #[test]
    fn install_short_hand_capital_f_sets_force() {
        let cli = Cli::try_parse_from(["mc", "install", "-F"]).unwrap();
        match cli.command {
            Command::Install(args) => assert!(args.force),
            other => panic!("expected Install, got {other:?}"),
        }
    }

    #[test]
    fn install_all_short_hands_together() {
        let cli = Cli::try_parse_from([
            "mc", "install", "-t", "fabric", "-v", "1.21", "-y", "-F",
        ]).unwrap();
        match cli.command {
            Command::Install(args) => {
                assert_eq!(args.server_type, Some(ServerType::Fabric));
                assert_eq!(args.version.as_deref(), Some("1.21"));
                assert!(args.yes);
                assert!(args.force);
            }
            other => panic!("expected Install, got {other:?}"),
        }
    }

    #[test]
    fn upgrade_short_hand_v_parses_version() {
        let cli = Cli::try_parse_from(["mc", "upgrade", "-v", "1.21"]).unwrap();
        match cli.command {
            Command::Upgrade(args) => assert_eq!(args.version.as_deref(), Some("1.21")),
            other => panic!("expected Upgrade, got {other:?}"),
        }
    }

    #[test]
    fn upgrade_short_hand_t_parses_type() {
        let cli = Cli::try_parse_from(["mc", "upgrade", "-t", "paper"]).unwrap();
        match cli.command {
            Command::Upgrade(args) => assert_eq!(args.server_type, Some(ServerType::Paper)),
            other => panic!("expected Upgrade, got {other:?}"),
        }
    }

    #[test]
    fn upgrade_short_hand_f_parses_pack() {
        let cli = Cli::try_parse_from(["mc", "upgrade", "-f", "pack.mrpack"]).unwrap();
        match cli.command {
            Command::Upgrade(args) => assert_eq!(args.pack.as_deref(), Some("pack.mrpack")),
            other => panic!("expected Upgrade, got {other:?}"),
        }
    }

    #[test]
    fn upgrade_short_hand_y_sets_yes() {
        let cli = Cli::try_parse_from(["mc", "upgrade", "-y"]).unwrap();
        match cli.command {
            Command::Upgrade(args) => assert!(args.yes),
            other => panic!("expected Upgrade, got {other:?}"),
        }
    }

    #[test]
    fn upgrade_short_hand_capital_f_sets_force() {
        let cli = Cli::try_parse_from(["mc", "upgrade", "-F"]).unwrap();
        match cli.command {
            Command::Upgrade(args) => assert!(args.force),
            other => panic!("expected Upgrade, got {other:?}"),
        }
    }

    #[test]
    fn upgrade_short_hand_n_sets_no_backup() {
        let cli = Cli::try_parse_from(["mc", "upgrade", "-n"]).unwrap();
        match cli.command {
            Command::Upgrade(args) => assert!(args.no_backup),
            other => panic!("expected Upgrade, got {other:?}"),
        }
    }

    #[test]
    fn upgrade_all_short_hands_together() {
        let cli = Cli::try_parse_from([
            "mc", "upgrade", "-t", "vanilla", "-v", "1.21.9", "-y", "-F", "-n",
        ]).unwrap();
        match cli.command {
            Command::Upgrade(args) => {
                assert_eq!(args.server_type, Some(ServerType::Vanilla));
                assert_eq!(args.version.as_deref(), Some("1.21.9"));
                assert!(args.yes);
                assert!(args.force);
                assert!(args.no_backup);
            }
            other => panic!("expected Upgrade, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_server_type_is_refused_at_parse_time() {
        assert!(Cli::try_parse_from(["mc", "install", "--type", "sponge"]).is_err());
    }
}
