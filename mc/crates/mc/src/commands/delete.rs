//! `mc delete` — remove the server and its secrets, keeping the backups.

use mc_common::error::{Error, IoContext, Result};
use mc_common::paths::SERVICE_UNIT;
use mc_common::ui;

use crate::context::Ctx;

pub fn run(ctx: &Ctx) -> Result<()> {
    let _lock = mc_common::lock::acquire(&ctx.paths.lock_file())?;

    // Nothing to delete is not a failure, but it should not prompt for a
    // destructive confirmation either.
    if !ctx.paths.server_installed() {
        ui::info("No server installed — nothing to delete.");
        return Ok(());
    }

    ui::error("WARNING: This will permanently delete the server and all its data.");
    if !ui::confirm_typed("Type 'delete' to confirm:", "delete") {
        return Err(Error::denied("Confirmation did not match. Aborting."));
    }

    if ctx.service.is_active(SERVICE_UNIT) {
        ctx.service.stop(SERVICE_UNIT)?;
    }
    ctx.service.disable(SERVICE_UNIT)?;

    let base = ctx.paths.base();
    std::fs::remove_dir_all(&base).at(&base)?;

    // The secrets go with the server. A later reinstall provisions a new RCON
    // password rather than resurrecting one whose value may have been shared.
    for path in [
        ctx.paths.config_file(),
        ctx.paths.passwd_file(),
        ctx.paths.mrpack_manifest(),
    ] {
        let _ = std::fs::remove_file(path);
    }

    ui::info("Server deleted.");
    ui::info(format!(
        "Backups in {} were preserved.",
        ctx.paths.backup_dir().display()
    ));
    Ok(())
}
