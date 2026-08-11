//! `mc start` / `stop` / `restart` / `status` / `logs`.

use std::time::Duration;

use mc_common::error::{Error, Result};
use mc_common::paths::SERVICE_UNIT;
use mc_common::service::UnitState;
use mc_common::{eula, ui};

use crate::context::Ctx;

/// How long to wait for the unit to reach a settled active state.
const START_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Guard against `Type=simple`'s optimism.
///
/// systemd marks the unit active the moment it forks `mc serve`, so a bare
/// is-active check can land in the window between the fork and a config refusal
/// exiting — and report "Server started." about a server that is already gone.
/// Re-check after a settle so those exits have landed.
///
/// Paid only on the transition to active, not on every poll, and only once: a
/// server that clears this is genuinely running, and two seconds is invisible
/// next to the world load that follows.
const SETTLE: Duration = Duration::from_secs(2);

pub fn require_server(ctx: &Ctx) -> Result<()> {
    if ctx.paths.server_installed() {
        Ok(())
    } else {
        Err(Error::config("No server installed. Run: mc install"))
    }
}

pub fn start(ctx: &Ctx, accept_eula: bool) -> Result<()> {
    require_server(ctx)?;
    // `mc serve` refuses to launch a server that has not accepted, so the flag
    // is offered at the point of failure. This is also the only way to accept
    // on an existing server — re-running install would re-download the jar.
    consent(ctx, accept_eula)?;

    // Desired state already reached — say so and succeed. `systemctl start` on
    // an active unit exits 0 for the same reason: a config-management run that
    // asks for a running server and finds one has not failed.
    if ctx.service.is_active(SERVICE_UNIT) {
        ui::info("Server is already running.");
        return Ok(());
    }

    start_and_verify(ctx)?;
    ui::info("Server started.");
    Ok(())
}

pub fn stop(ctx: &Ctx) -> Result<()> {
    // As in start: already stopped is the requested state, not a failure.
    if !ctx.service.is_active(SERVICE_UNIT) {
        ui::info("Server is not running.");
        return Ok(());
    }
    // The in-game countdown is `ExecStop=`'s job, so that it also runs for a
    // bare `systemctl stop minecraft`.
    ctx.service.stop(SERVICE_UNIT)?;
    ui::info("Server stopped.");
    Ok(())
}

pub fn restart(ctx: &Ctx, accept_eula: bool) -> Result<()> {
    require_server(ctx)?;
    // Restart starts the server, so it meets the same gate as start.
    consent(ctx, accept_eula)?;

    if ctx.service.is_active(SERVICE_UNIT) {
        ctx.service.stop(SERVICE_UNIT)?;
    }
    start_and_verify(ctx)?;
    ui::info("Server restarted.");
    Ok(())
}

pub fn status(ctx: &Ctx) -> Result<()> {
    match ctx.service.state(SERVICE_UNIT) {
        UnitState::Active => ui::info("minecraft: active (running)"),
        UnitState::Failed => {
            ui::error("minecraft: failed");
            if let Some(log) = ctx.service.recent_log(SERVICE_UNIT, 15) {
                eprintln!("{log}");
            }
        }
        UnitState::Inactive => ui::info("minecraft: inactive (stopped)"),
        UnitState::Absent => ui::warn("systemd is not running here; no unit state to report."),
    }
    Ok(())
}

pub fn logs(ctx: &Ctx) -> Result<()> {
    use std::os::unix::process::CommandExt as _;
    if !ctx.paths.systemd_running() {
        return Err(Error::config(
            "systemd is not running here, so there is no journal to follow.",
        ));
    }
    let err = std::process::Command::new("journalctl")
        .args(["-u", SERVICE_UNIT, "-f", "--no-pager"])
        .exec();
    Err(Error::other(format!("could not run journalctl: {err}")))
}

/// Accept the EULA, prompting when there is a terminal and no flag.
fn consent(ctx: &Ctx, accepted: bool) -> Result<()> {
    if eula::accepted(&ctx.paths.eula()) {
        return Ok(());
    }
    if !accepted {
        // Non-interactive with no flag: refuse rather than assume consent.
        eprintln!(
            "Minecraft's End User Licence Agreement must be accepted before the server\ncan run: {}",
            eula::EULA_URL
        );
        if !ui::confirm("Do you accept the Minecraft EULA?") {
            return Err(Error::config(format!(
                "The Minecraft EULA has not been accepted. Re-run with --accept-eula to accept it ({}).",
                eula::EULA_URL
            )));
        }
    }
    eula::accept(&ctx.paths)
}

/// Start the unit and report what actually happened.
///
/// `systemctl start` on a `Type=simple` unit returns as soon as the process is
/// forked, not when the server is up, so its success says nothing about whether
/// the server survived the next half second.
pub fn start_and_verify(ctx: &Ctx) -> Result<()> {
    if let Err(e) = ctx.service.start(SERVICE_UNIT) {
        report_failure(ctx);
        return Err(e);
    }

    let mut waited = Duration::ZERO;
    while waited < START_TIMEOUT {
        // Checked FIRST, and every iteration: `mc serve`'s config refusals exit
        // within milliseconds of the fork, so the unit can already be failed
        // here. Nothing about a missing jar or an unaccepted EULA resolves by
        // waiting, and polling out the full 60 s before saying so buries the
        // one line that explains it.
        if ctx.service.is_failed(SERVICE_UNIT) {
            report_failure(ctx);
            return Err(Error::other("Server failed to start."));
        }
        if ctx.service.is_active(SERVICE_UNIT) {
            ctx.service.sleep(SETTLE);
            if ctx.service.is_failed(SERVICE_UNIT) {
                report_failure(ctx);
                return Err(Error::other("Server failed to start."));
            }
            if ctx.service.is_active(SERVICE_UNIT) {
                return Ok(());
            }
        }
        ctx.service.sleep(POLL_INTERVAL);
        waited += POLL_INTERVAL;
    }

    if ctx.service.is_failed(SERVICE_UNIT) {
        report_failure(ctx);
        return Err(Error::other("Server failed to start."));
    }
    if ctx.service.is_active(SERVICE_UNIT) {
        return Ok(());
    }
    Err(Error::other(
        "Server did not reach active state within 60 s.\nCheck logs with: mc logs",
    ))
}

/// Surface why the unit failed.
///
/// `mc serve` writes its refusals to stderr, which systemd routes to the
/// journal, so the reason is already recorded — print it here rather than
/// making the operator go and find it.
fn report_failure(ctx: &Ctx) {
    ui::error("Server failed to start.");
    if let Some(log) = ctx.service.recent_log(SERVICE_UNIT, 15) {
        eprintln!("{log}");
    } else {
        ui::error("Check logs with: mc logs");
    }
}
