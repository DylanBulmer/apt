//! `mc reload` — systemd's `ExecReload=`.
//!
//! Asks the running server to re-read its configuration. That needs a console
//! connection, which core does not have: it is a `pre-start`-shaped capability
//! owned by whichever plugin can talk to the server. Today that is `mc-rcon`,
//! which exposes it as a subcommand rather than a hook, because a reload is a
//! whole operation rather than a step in one.

use mc_common::error::{Error, Result};
use mc_common::plugin::Registry;

use crate::context::Ctx;

pub fn run(ctx: &Ctx) -> Result<()> {
    let registry = Registry::discover(&ctx.paths);

    let Some((plugin, _)) = registry.command("rcon") else {
        // Not an error that should fail the unit: `systemctl reload minecraft`
        // on a server with no console plugin has nothing to do, and returning
        // non-zero would mark the unit failed for it.
        eprintln!("[mc] No console plugin installed; nothing to reload. Install mc-rcon.");
        return Ok(());
    };

    let status = std::process::Command::new(&plugin.bin)
        .args(["command", "rcon", "--", "reload"])
        .status()
        .map_err(|e| Error::other(format!("running {}: {e}", plugin.bin.display())))?;

    if status.success() {
        eprintln!("[mc] Reload sent.");
        Ok(())
    } else {
        Err(Error::other(format!("reload failed ({status})")))
    }
}
