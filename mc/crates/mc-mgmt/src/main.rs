//! `/usr/libexec/mc/mc-mgmt` — the plugin binary.
//!
//! Three entry points, matching the ABI-1 contract:
//!
//!   `mc-mgmt command mgmt [args…]`  — the `mc mgmt` subcommand
//!   `mc-mgmt hook <event>`          — a hook, payload as JSON on stdin
//!   `mc-mgmt console probe`         — "can I talk to the server?", by exit code
//!
//! Not on `PATH` on purpose: it is invoked by core, and advertising
//! `mc-mgmt command mgmt` as a command surface would be advertising an
//! interface nobody should depend on.

use std::io::Read as _;

use mc_common::error::{Error, Result};
use mc_common::paths::Paths;
use mc_common::properties::Properties;
use mc_common::{privilege, ui};
use mc_console::hooks;
use mc_mgmt::console::MgmtConsole;
use mc_mgmt::{endpoint, methods, provision};

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = Paths::from_env();

    // Completions list: outputs JSON for mc completions to consume
    if args.first().map(String::as_str) == Some("completions")
        && args.get(1).map(String::as_str) == Some("list")
    {
        println!(
            r#"{{"subcommands":[{{"name":"status","about":"Show management protocol configuration"}},{{"name":"players","about":"List connected players"}},{{"name":"say","about":"Broadcast a message to all players"}},{{"name":"enable","about":"Enable the management protocol"}},{{"name":"disable","about":"Disable the management protocol"}},{{"name":"allowlist","about":"Manage the allowlist"}},{{"name":"bans","about":"Manage the ban list"}},{{"name":"ip-bans","about":"Manage the IP ban list"}},{{"name":"operators","about":"Manage the operator list"}}]}}"#
        );
        return std::process::ExitCode::SUCCESS;
    }

    // Answered with an exit status and nothing else — core probes every
    // console on the shutdown path, and a server too old for this protocol is
    // not a fault worth a line in the journal each time it stops.
    if args.first().map(String::as_str) == Some("console") {
        let verb = args.get(1).map(String::as_str).unwrap_or_default();
        return mc_console::answer_probe(verb, || MgmtConsole::usable(&paths));
    }

    let result = match args.first().map(String::as_str) {
        Some("command") => command(&paths, args.get(1..).unwrap_or_default()),
        Some("hook") => hook(&paths, args.get(1).map(String::as_str).unwrap_or_default()),
        _ => Err(Error::config(
            "mc-mgmt is a plugin for mc, not a command. Use: mc mgmt",
        )),
    };

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            ui::error(e.to_string());
            std::process::ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(1))
        }
    }
}

// ── hooks ──────────────────────────────────────────────────────────────────

fn hook(paths: &Paths, event: &str) -> Result<()> {
    // Drained even when ignored: core writes the payload to this pipe, and a
    // reader that exits without reading leaves it writing into a closed one.
    let mut payload = String::new();
    let _ = std::io::stdin().read_to_string(&mut payload);

    match event {
        "pre-stop" => with_console(
            paths,
            "no in-game warning and no graceful stop",
            hooks::pre_stop,
        ),
        "pre-backup" => with_console(
            paths,
            "the backup will archive a world that was not flushed",
            hooks::pre_backup,
        ),
        "post-backup" => with_console(paths, "saving may still be paused", hooks::post_backup),

        // Provisioning runs for EVERY installed console, not just the elected
        // one, so that a server which later stops answering this protocol
        // still has a console that works.
        "post-install" | "post-upgrade" => post_install(paths),

        // An event this plugin did not register. Not an error: core may know
        // events this build does not.
        _ => Ok(()),
    }
}

/// The shape every hook body in `mc_console::hooks` has.
type HookBody = fn(&mut MgmtConsole, &dyn Fn(&str));

/// Run a hook body against a live console, or explain why nothing happened.
///
/// Never an error. A server this console cannot reach still stops and still
/// gets backed up — it just does so without the warning or the flush, which is
/// exactly what the message says.
fn with_console(paths: &Paths, without: &str, body: HookBody) -> Result<()> {
    match MgmtConsole::connect(paths) {
        Ok(mut console) => {
            body(&mut console, &|m: &str| log(m));
            Ok(())
        }
        Err(e) => {
            log(&format!(
                "Management protocol unavailable ({e}) — {without}."
            ));
            Ok(())
        }
    }
}

/// Provision the endpoint for a newly installed or upgraded server.
///
/// Silent about servers that cannot support it: every install of a Minecraft
/// older than 1.21.9 would otherwise print a warning about a protocol the
/// operator never asked for.
fn post_install(paths: &Paths) -> Result<()> {
    if provision::enable(paths)? {
        log("Management protocol enabled — applies on next start.");
    }
    Ok(())
}

// ── the `mc mgmt` subcommand ───────────────────────────────────────────────

fn command(paths: &Paths, args: &[String]) -> Result<()> {
    // args[0] is the subcommand name core dispatched on.
    let rest = args.get(1..).unwrap_or_default();

    if rest.first().map(String::as_str) == Some("--help")
        || rest.first().map(String::as_str) == Some("-h")
    {
        println!("{}", usage());
        return Ok(());
    }

    if rest.first().map(String::as_str) == Some("--version")
        || rest.first().map(String::as_str) == Some("-V")
    {
        println!("mc-mgmt {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    match rest.first().map(String::as_str) {
        // State verbs act on server.properties rather than on a running
        // server: you must be able to enable the protocol on a server that is
        // stopped, or on one where the protocol is precisely what is off.
        Some("enable") => root_verb(paths, args, rest, enable),
        Some("disable") => root_verb(paths, args, rest, disable),
        Some("status") => {
            // Reads only files the service group can already read.
            privilege::require_root_or_group(&paths.mc_bin(), args)?;
            status(paths)
        }
        Some("say") => say(paths, args, rest.get(1..).unwrap_or_default()),
        Some("players") => players(paths, args),

        // Moderation. Reading a list needs only the service group; changing
        // one needs root, because it changes who can reach the server.
        Some("allowlist") => allowlist(paths, rest),
        Some("bans") => bans(paths, rest),
        Some("ip-bans") => ip_bans(paths, rest),
        Some("operators") => operators(paths, rest),

        None => Err(Error::config(usage())),
        Some(other) => Err(Error::config(format!(
            "Unknown subcommand: mc mgmt {other}\n{}",
            usage()
        ))),
    }
}

fn usage() -> String {
    "Usage: mc mgmt <COMMAND>\n\n \
     Commands:\n   \
     status    Show management protocol configuration\n   \
     players   List connected players\n   \
     say <msg> Broadcast a message to all players\n   \
     enable    Enable the management protocol\n   \
     disable   Disable the management protocol\n   \
     allowlist [add|remove <name>]  Manage the allowlist\n   \
     bans      [add|remove <name>]  Manage the ban list\n   \
     ip-bans   [add|remove <addr>]  Manage the IP ban list\n   \
     operators [add|remove <name>]  Manage the operator list"
        .to_string()
}

// ── moderation ─────────────────────────────────────────────────────────────

/// What a moderation subcommand was asked to do.
enum Change<'a> {
    List,
    Add(&'a str),
    Remove(&'a str),
}

/// Parse `[add|remove <subject>]`, shared by all four lists.
fn parse_change<'a>(verb: &str, args: &'a [String]) -> Result<Change<'a>> {
    match (args.first().map(String::as_str), args.get(1)) {
        (None, _) => Ok(Change::List),
        (Some("add"), Some(subject)) => Ok(Change::Add(subject)),
        (Some("remove"), Some(subject)) => Ok(Change::Remove(subject)),
        (Some(action @ ("add" | "remove")), None) => Err(Error::config(format!(
            "mc mgmt {verb} {action} needs a name."
        ))),
        (Some(other), _) => Err(Error::config(format!(
            "Unknown: mc mgmt {verb} {other}\n       Try: add, remove, or no argument to list."
        ))),
    }
}

/// Reading a list is a group operation; changing one is not.
///
/// Splitting the guard by verb rather than by subcommand keeps `mc mgmt bans`
/// as usable as `mc rcon status` for anyone in the `minecraft` group, while
/// still refusing to let them change who can connect.
fn guard(paths: &Paths, change: &Change<'_>, args: &[String]) -> Result<()> {
    match change {
        Change::List => privilege::require_root_or_group(&paths.mc_bin(), args),
        Change::Add(_) | Change::Remove(_) => privilege::require_root(&paths.mc_bin(), args),
    }
}

fn allowlist(paths: &Paths, args: &[String]) -> Result<()> {
    let rest = args.get(1..).unwrap_or_default();
    let change = parse_change("allowlist", rest)?;
    guard(paths, &change, args)?;

    let mut console = MgmtConsole::connect(paths)?;
    let players = match change {
        Change::List => methods::allowlist(console.client())?,
        Change::Add(name) => {
            methods::allowlist_add(console.client(), &[methods::Player::named(name)])?
        }
        Change::Remove(name) => {
            methods::allowlist_remove(console.client(), &[methods::Player::named(name)])?
        }
    };
    print_list("allowlist", players.iter().map(methods::Player::label));
    Ok(())
}

fn bans(paths: &Paths, args: &[String]) -> Result<()> {
    let rest = args.get(1..).unwrap_or_default();
    let change = parse_change("bans", rest)?;
    guard(paths, &change, args)?;

    let mut console = MgmtConsole::connect(paths)?;
    let bans = match change {
        Change::List => methods::bans(console.client())?,
        Change::Add(name) => methods::ban_add(
            console.client(),
            &[methods::UserBan {
                player: methods::Player::named(name),
                reason: None,
                source: None,
                expires: None,
            }],
        )?,
        Change::Remove(name) => {
            methods::ban_remove(console.client(), &[methods::Player::named(name)])?
        }
    };
    print_list(
        "bans",
        bans.iter().map(|ban| match &ban.reason {
            Some(reason) => format!("{} ({reason})", ban.player.label()),
            None => ban.player.label(),
        }),
    );
    Ok(())
}

fn ip_bans(paths: &Paths, args: &[String]) -> Result<()> {
    let rest = args.get(1..).unwrap_or_default();
    let change = parse_change("ip-bans", rest)?;
    guard(paths, &change, args)?;

    let mut console = MgmtConsole::connect(paths)?;
    let bans = match change {
        Change::List => methods::ip_bans(console.client())?,
        Change::Add(ip) => methods::ip_ban_add(
            console.client(),
            &[methods::IpBan {
                ip: ip.to_string(),
                reason: None,
                source: None,
                expires: None,
            }],
        )?,
        Change::Remove(ip) => methods::ip_ban_remove(console.client(), &[ip.to_string()])?,
    };
    print_list("ip-bans", bans.iter().map(|ban| ban.ip.clone()));
    Ok(())
}

fn operators(paths: &Paths, args: &[String]) -> Result<()> {
    let rest = args.get(1..).unwrap_or_default();
    let change = parse_change("operators", rest)?;
    guard(paths, &change, args)?;

    let mut console = MgmtConsole::connect(paths)?;
    let operators = match change {
        Change::List => methods::operators(console.client())?,
        Change::Add(name) => methods::operator_add(
            console.client(),
            &[methods::Operator {
                player: methods::Player::named(name),
                // Left to the server's own default rather than guessed at:
                // `operator_user_permission_level` is a server setting, and
                // choosing one here would silently override it.
                permission_level: None,
                bypasses_player_limit: None,
            }],
        )?,
        Change::Remove(name) => {
            methods::operator_remove(console.client(), &[methods::Player::named(name)])?
        }
    };
    print_list(
        "operators",
        operators.iter().map(|op| match op.permission_level {
            Some(level) => format!("{} (level {level})", op.player.label()),
            None => op.player.label(),
        }),
    );
    Ok(())
}

/// Print the resulting list, one entry per line on stdout so it can be piped.
///
/// Every method returns the whole list after a change, so an add and a list
/// print the same thing — which is what makes the result of a change visible
/// without a second call.
fn print_list(what: &str, entries: impl Iterator<Item = String>) {
    let mut empty = true;
    for entry in entries {
        empty = false;
        println!("{entry}");
    }
    if empty {
        ui::info(format!("The {what} is empty."));
    }
}

fn root_verb(
    paths: &Paths,
    argv: &[String],
    rest: &[String],
    f: fn(&Paths) -> Result<()>,
) -> Result<()> {
    if rest.len() > 1 {
        return Err(Error::config(format!(
            "mc mgmt {} takes no arguments.",
            rest.first().map(String::as_str).unwrap_or("")
        )));
    }
    // The FULL invocation, not the fragment after it: the guard echoes this
    // back when it refuses and replays it under sudo when it re-execs, so a
    // fragment turns `mc mgmt enable` into the advice `sudo mc enable`.
    //
    // Rewrites server.properties, which is 0640 owned by the service account.
    privilege::require_root(&paths.mc_bin(), argv)?;
    f(paths)
}

fn enable(paths: &Paths) -> Result<()> {
    let changed = provision::enable(paths)?;
    let props = Properties::load(&paths.server_properties());
    let port = provision::port(&props);

    if changed {
        ui::info(format!(
            "Management protocol enabled on port {port} — restart to apply"
        ));
    } else {
        ui::info(format!(
            "Management protocol already enabled on port {port}."
        ));
    }
    Ok(())
}

fn disable(paths: &Paths) -> Result<()> {
    if provision::disable(paths)? {
        ui::info("Management protocol disabled — restart to apply");
    } else {
        ui::info("Management protocol already disabled.");
    }
    Ok(())
}

fn status(paths: &Paths) -> Result<()> {
    let props = Properties::load(&paths.server_properties());

    let Some(resolved) = endpoint::resolve(&props) else {
        ui::info("Management protocol: disabled");
        ui::info("Enable with: mc mgmt enable (needs Minecraft 1.21.9 or newer)");
        return Ok(());
    };

    ui::info(format!("Management protocol: enabled at {}", resolved.url));
    if !resolved.is_loopback() {
        // Not a refusal — the protocol authenticates every connection — but an
        // operator should know the endpoint is reachable off this machine.
        ui::warn(format!(
            "{} is not loopback; anything that can reach it needs only the secret.",
            resolved.host
        ));
    }
    if resolved.secret.is_empty() {
        ui::warn("No secret is set — nothing can authenticate. Run: mc mgmt enable");
        return Ok(());
    }

    // Proves the whole path end to end rather than what the file claims.
    match MgmtConsole::connect(paths) {
        Ok(mut console) => match methods::status(console.client()) {
            Ok(state) => {
                let version = state
                    .version
                    .map(|v| v.name)
                    .unwrap_or_else(|| "unknown".to_string());
                ui::info(format!(
                    "Connection: OK — {} player(s) online, Minecraft {version}",
                    state.players.len()
                ));
            }
            Err(e) => ui::warn(format!("Connected, but the server refused a call: {e}")),
        },
        // A rejected secret and an unreachable endpoint need different advice,
        // and "restart to pick up the settings" is actively wrong for the
        // first. The transport reports an operator-fixable refusal — a 401, or
        // a secret a header cannot carry — as a config error, so the exit code
        // is what tells the two apart.
        Err(e) if e.exit_code() == 78 => ui::warn(format!("Connection: REFUSED — {e}")),
        Err(e) => ui::warn(format!(
            "Connection: FAILED ({e}) — the server may need a restart to pick up the settings."
        )),
    }
    Ok(())
}

fn players(paths: &Paths, argv: &[String]) -> Result<()> {
    privilege::require_root_or_group(&paths.mc_bin(), argv)?;
    let mut console = MgmtConsole::connect(paths)?;
    let players = methods::players(console.client())?;

    if players.is_empty() {
        ui::info("Nobody is online.");
        return Ok(());
    }
    for player in &players {
        println!("{}", player.label());
    }
    Ok(())
}

fn say(paths: &Paths, argv: &[String], words: &[String]) -> Result<()> {
    if words.is_empty() {
        return Err(Error::config("Usage: mc mgmt say <message>"));
    }
    privilege::require_root_or_group(&paths.mc_bin(), argv)?;

    let mut console = MgmtConsole::connect(paths)?;
    methods::say(console.client(), &words.join(" "))
}

/// Hook output goes to stderr, because core's stdout may be a pipe
/// something else is parsing. `ui::info` already tags every line with `[mc]`.
fn log(message: &str) {
    ui::info(message);
}
