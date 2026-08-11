//! `mc shutdown` — systemd's `ExecStop=`.
//!
//! Runs as the `minecraft` user, like `mc serve`. Its whole job is to give
//! plugins a chance to warn players and flush the world before systemd signals
//! the process, so almost all of the behaviour lives in the `pre-stop` hook —
//! today that is `mc-rcon`, which runs the countdown and sends `stop`.
//!
//! WITHOUT A PLUGIN THIS IS A NO-OP, and says so. That is the honest outcome:
//! with nothing able to talk to the server, `mc stop` disconnects everyone with
//! no in-game warning, and the journal would otherwise show nothing between
//! "Stopping..." and the SIGTERM.
//!
//! Everything here logs to stderr, which systemd routes to the journal. A stop
//! can occupy up to `TimeoutStopSec` (375 s) and is otherwise opaque — an
//! operator watching `systemctl stop minecraft` hang for five minutes cannot
//! otherwise tell a countdown from a wedged connection.

use mc_common::error::Result;
use mc_common::plugin::{Event, Registry};

use crate::context::Ctx;

pub fn run(ctx: &Ctx) -> Result<()> {
    let registry = Registry::discover(&ctx.paths);

    let handlers = registry
        .plugins()
        .iter()
        .filter(|p| p.hooks.iter().any(|h| h.event == Event::PreStop))
        .count();

    if handlers == 0 {
        log("No pre-stop handler is installed — no in-game warning and no graceful stop;");
        log("systemd will signal the server directly. Install mc-rcon for the countdown.");
        return Ok(());
    }

    log("Stop requested; handing off to pre-stop handlers.");
    // `pre-stop` can never be fatal (the manifest loader refuses to let a
    // plugin declare it so), which is why this result is a formality: a failed
    // warning must not stop the shutdown.
    let payload = serde_json::json!({ "reason": "stop" });
    registry.run_hook(&ctx.paths, Event::PreStop, &payload)?;
    log("Pre-stop handlers finished; handing back to systemd.");
    Ok(())
}

fn log(msg: &str) {
    eprintln!("[mc] {msg}");
}
