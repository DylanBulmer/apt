//! `/usr/libexec/mc/mc-rcon` — the plugin binary.
//!
//! Two entry points, matching the ABI-1 contract:
//!
//!   `mc-rcon command rcon [args…]`  — the `mc rcon` subcommand
//!   `mc-rcon hook <event>`          — a hook, payload as JSON on stdin
//!
//! Not on `PATH` on purpose: it is invoked by core, and advertising
//! `mc-rcon command rcon` as a command surface would be advertising an
//! interface nobody should depend on. The operator-facing client is
//! `/usr/bin/rcon`.

use std::io::Read as _;

use mc_common::error::{Error, Result};
use mc_common::paths::{MC_USER, Paths, SERVICE_UNIT};
use mc_common::properties::{self, Properties};
use mc_common::{privilege, ui};
use mc_rcon::countdown::{self, MARKS};
use mc_rcon::players::{self, PlayerCount};
use mc_rcon::{password, session};

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = Paths::from_env();

    let result = match args.first().map(String::as_str) {
        Some("command") => command(&paths, args.get(1..).unwrap_or_default()),
        Some("hook") => hook(&paths, args.get(1).map(String::as_str).unwrap_or_default()),
        _ => Err(Error::config(
            "mc-rcon is a plugin for mc, not a command. Use: mc rcon",
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

// ── the `mc rcon` subcommand ───────────────────────────────────────────────

fn command(paths: &Paths, args: &[String]) -> Result<()> {
    // args[0] is the subcommand name core dispatched on.
    let rest = args.get(1..).unwrap_or_default();

    match rest.first().map(String::as_str) {
        // State verbs act on server.properties rather than talking to a running
        // server, so they are handled before any connection is attempted — you
        // must be able to enable RCON on a server that is stopped, or on one
        // where RCON is precisely what is currently off.
        //
        // They shadow any server command of the same name. None exist today,
        // and `mc rcon -- <command>` sends one literally if that changes.
        Some("enable") => verb(paths, rest, enable),
        Some("disable") => verb(paths, rest, disable),
        Some("status") => {
            // status only reads files the service group may already read, so it
            // does not demand root — that would protect nothing and leave `mc`
            // less capable than /usr/bin/rcon.
            privilege::require_root_or_group(&paths.mc_bin(), args)?;
            status(paths)
        }
        _ => {
            let command_words: Vec<&str> = rest
                .iter()
                .map(String::as_str)
                .skip_while(|w| *w == "--")
                .collect();
            interactive_or_once(paths, &command_words)
        }
    }
}

fn verb(paths: &Paths, rest: &[String], f: fn(&Paths) -> Result<()>) -> Result<()> {
    if rest.len() > 1 {
        return Err(Error::config(format!(
            "mc rcon {} takes no arguments.",
            rest.first().map(String::as_str).unwrap_or("")
        )));
    }
    // enable/disable rewrite server.properties and may provision the password,
    // so they need root: that file is 0640 owned by the service account, which
    // makes it readable by the minecraft group but writable only by its owner,
    // and /etc/minecraft is root-owned.
    privilege::require_root(&paths.mc_bin(), rest)?;
    f(paths)
}

fn enable(paths: &Paths) -> Result<()> {
    password::ensure(paths)?;
    let changed = set_enabled(paths, true)?;
    let port = properties::rcon_port(&Properties::load(&paths.server_properties()));

    if changed {
        ui::info(format!("RCON enabled on port {port}."));
        // NOT restarted automatically. The server reads server.properties at
        // startup, so a restart is required — but on a populated server that is
        // a five-minute countdown, and choosing when to spend it is the
        // operator's call, not a side effect of a config command.
        ui::info("Restart to apply: mc restart");
    } else {
        ui::info(format!("RCON is already enabled on port {port}."));
    }
    Ok(())
}

fn disable(paths: &Paths) -> Result<()> {
    if set_enabled(paths, false)? {
        ui::info("RCON disabled.");
        ui::info("Restart to apply: mc restart");
    } else {
        ui::info("RCON is already disabled.");
    }
    Ok(())
}

/// Bring the RCON block of `server.properties` to the requested state.
///
/// Returns whether anything moved, so callers can avoid recommending a restart
/// — or, in the package's own post-install, avoid *taking* one — for a no-op.
fn set_enabled(paths: &Paths, enabled: bool) -> Result<bool> {
    let file = paths.server_properties();
    let mut props = Properties::load(&file);
    let before = props.clone();

    props.set("enable-rcon", if enabled { "true" } else { "false" });

    if enabled {
        let port = properties::rcon_port(&before);
        props.set("rcon.port", &port.to_string());
        props.set("rcon.password", &password::read(paths)?);
    } else {
        // The password FILE is left alone, so `enable` restores the same secret
        // rather than inventing a new one every time RCON is toggled. Only the
        // copy the server reads is cleared.
        props.set("rcon.password", "");
    }

    if props == before {
        return Ok(false);
    }
    props.save(&file)?;
    Ok(true)
}

fn status(paths: &Paths) -> Result<()> {
    let props = Properties::load(&paths.server_properties());
    let enabled = props.get("enable-rcon").unwrap_or("unset");
    let port = properties::rcon_port(&props);
    ui::info(format!("enable-rcon: {enabled} (port {port})"));

    if !password::exists(paths) {
        ui::warn(format!(
            "No password file at {} — run: mc rcon enable",
            paths.passwd_file().display()
        ));
    } else if props.get("rcon.password") != Some(password::read(paths)?.as_str()) {
        // The two drift apart if server.properties was edited by hand, or
        // restored from a backup taken before the password was provisioned.
        ui::warn(format!(
            "server.properties disagrees with {} — run: mc rcon enable",
            paths.passwd_file().display()
        ));
    }

    if enabled == "true" {
        // Proves the whole path end to end — password, port, and a listening
        // server — rather than just what the file claims.
        match session::run(paths, "list") {
            Ok(_) => ui::info("Connection: OK"),
            Err(e) => ui::warn(format!(
                "Connection: FAILED ({e}) — the server may need a restart to pick up the settings."
            )),
        }
    }
    Ok(())
}

/// A one-shot command, or an interactive console.
fn interactive_or_once(paths: &Paths, words: &[&str]) -> Result<()> {
    // Everything this path touches is closed to a user outside the service
    // group — MC_BASE is 0750, the password file 0640 root:minecraft, and
    // server.properties (which carries the port) 0640. Checking for an
    // installed server first would fail its file test purely because the
    // directory is untraversable, and report "no server installed" to someone
    // whose server is installed and running.
    privilege::require_root_or_group(&paths.mc_bin(), &[])?;

    if !paths.server_installed() {
        return Err(Error::config("No server installed. Run: mc install"));
    }
    if !password::exists(paths) {
        return Err(Error::config("RCON is not enabled. Run: mc rcon enable"));
    }

    let mut connection = session::connect(paths)?;

    if !words.is_empty() {
        let reply = connection.exec(&words.join(" "))?;
        println!("{}", reply.trim_end());
        return Ok(());
    }

    ui::info(format!(
        "Connected to {SERVICE_UNIT}. Type 'exit' or Ctrl-D to leave."
    ));
    let stdin = std::io::stdin();
    loop {
        use std::io::Write as _;
        print!("rcon> ");
        let _ = std::io::stdout().flush();

        let mut line = String::new();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            println!();
            return Ok(());
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "exit" || line == "quit" {
            return Ok(());
        }
        match connection.exec(line) {
            Ok(reply) => println!("{}", reply.trim_end()),
            Err(e) => ui::error(e.to_string()),
        }
    }
}

// ── hooks ──────────────────────────────────────────────────────────────────

fn hook(paths: &Paths, event: &str) -> Result<()> {
    // Drained whether or not it is used: core writes the payload to our stdin
    // and a plugin that never reads it leaves core writing into a closed pipe.
    let mut payload = String::new();
    let _ = std::io::stdin().read_to_string(&mut payload);

    match event {
        "pre-stop" => pre_stop(paths),
        "pre-backup" => pre_backup(paths),
        "post-backup" => post_backup(paths),
        "post-install" | "post-upgrade" => post_install(paths),
        other => Err(Error::config(format!(
            "mc-rcon does not handle hook '{other}'."
        ))),
    }
}

/// Warn players, wait, then tell the server to stop.
///
/// Everything logs to stderr, which systemd routes to the journal: a stop can
/// occupy up to `TimeoutStopSec` and is otherwise opaque — an operator watching
/// `systemctl stop minecraft` hang for five minutes cannot otherwise tell a
/// countdown from a wedged connection. The announcements themselves are
/// `tellraw`, which the server does not echo to its console, so this is the
/// only record of what players were told.
fn pre_stop(paths: &Paths) -> Result<()> {
    if !session::configured(paths) {
        log("RCON is not configured — no in-game warning and no graceful stop.");
        return Ok(());
    }

    let mut connection = match session::connect(paths) {
        Ok(c) => c,
        Err(e) => {
            log(&format!(
                "Could not reach the server ({e}); systemd will signal it directly."
            ));
            return Ok(());
        }
    };

    log("Stop requested; asking the server who is online.");
    let count = match connection.exec("list") {
        Ok(reply) => players::parse(&reply),
        Err(e) => {
            log(&format!("Could not count players ({e})."));
            PlayerCount::Unknown
        }
    };

    match count {
        PlayerCount::Online(0) => {
            log("No players online — skipping the countdown and stopping immediately.");
        }
        PlayerCount::Online(n) => {
            log(&format!(
                "{n} player(s) online — triggering the 5-minute countdown."
            ));
            run_countdown(&mut connection);
        }
        // Logged differently from a counted zero on purpose: the journal must
        // distinguish "we counted" from "we could not count", because the
        // latter also indicates a console problem.
        PlayerCount::Unknown => {
            log(
                "Player count unavailable — assuming players are online; triggering the 5-minute countdown.",
            );
            run_countdown(&mut connection);
        }
    }

    log("Sending 'stop' to the server.");
    if let Err(e) = connection.exec("stop") {
        log(&format!("WARNING: the stop command failed: {e}"));
    }

    // Give the JVM time to flush chunks and exit cleanly before systemd sends
    // SIGTERM. TimeoutStopSec in the unit is the outer bound.
    log("Waiting 10s for the server to flush chunks and exit.");
    std::thread::sleep(std::time::Duration::from_secs(10));
    Ok(())
}

fn run_countdown(connection: &mut mc_rcon::protocol::Connection) {
    for step in countdown::schedule(&MARKS) {
        // A failed announcement is reported and then ignored: players silently
        // not being warned is exactly what an operator needs to know about,
        // and the shutdown proceeds on schedule either way.
        match session::announce(connection, &step.message) {
            Ok(_) => log(&format!("Announced to players: {}", step.message)),
            Err(e) => log(&format!(
                "WARNING: could not announce '{}': {e}",
                step.message
            )),
        }
        if !step.wait.is_zero() {
            // Logged so a long quiet stretch reads as "waiting on purpose"
            // rather than "wedged".
            log(&format!("Next warning in {}s.", step.wait.as_secs()));
            std::thread::sleep(step.wait);
        }
    }
}

/// Flush the world and hold it still for the duration of an archive.
fn pre_backup(paths: &Paths) -> Result<()> {
    let Ok(mut connection) = session::connect(paths) else {
        log("RCON unavailable — the backup will archive a world that was not flushed.");
        return Ok(());
    };
    let _ = session::announce(&mut connection, "[mc] Backup starting — brief lag possible");
    let _ = connection.exec("save-off");
    let _ = connection.exec("save-all");
    // Give the flush a moment to reach disk before the archive starts reading.
    std::thread::sleep(std::time::Duration::from_secs(3));
    Ok(())
}

/// Turn saving back on.
///
/// RUNS WHETHER OR NOT THE BACKUP SUCCEEDED — this hook can never be declared
/// fatal, and core invokes it on both paths. A live server left with saves
/// disabled loses everything since the last flush the moment it stops.
fn post_backup(paths: &Paths) -> Result<()> {
    let Ok(mut connection) = session::connect(paths) else {
        return Ok(());
    };
    let _ = connection.exec("save-on");
    let _ = session::announce(&mut connection, "[mc] Backup complete");
    Ok(())
}

/// Provision the password and switch RCON on for a newly installed server.
///
/// Replaces the maintainer script that used to source another package's private
/// shell library to do this — the arrangement that made a missed version-floor
/// bump leave the package half-installed.
fn post_install(paths: &Paths) -> Result<()> {
    password::ensure(paths)?;
    if !paths.server_properties().exists() {
        // Nothing to enable on yet; `mc install` writes the file and calls this
        // again.
        return Ok(());
    }
    if set_enabled(paths, true)? {
        ui::info(format!(
            "RCON enabled on port {}. Restart to apply: mc restart",
            properties::rcon_port(&Properties::load(&paths.server_properties()))
        ));
    }
    // Ownership matters as much as mode here: 0640 is readable only because the
    // owner is the service account.
    properties::secure(&paths.server_properties())?;
    let _ = MC_USER;
    Ok(())
}

fn log(message: &str) {
    eprintln!("[mc] {message}");
}
