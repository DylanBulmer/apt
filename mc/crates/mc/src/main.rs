//! `/usr/bin/mc` — the dispatcher.

use mc::dispatch;
use mc_common::error::Error;

fn main() -> std::process::ExitCode {
    // Captured before clap consumes it: a privilege guard fires after the
    // command's own parsing, and a refusal that cannot echo what was typed is
    // one the operator has to reconstruct by hand.
    let argv: Vec<String> = std::env::args().skip(1).collect();

    match dispatch::run(argv) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            mc_common::ui::error(e.to_string());
            exit_code(&e)
        }
    }
}

/// The mapping is part of the contract with systemd: exit 78 (`EX_CONFIG`) is
/// what `minecraft.service` maps to `RestartPreventExitStatus=`, so an
/// operator-fixable problem fails visibly without restart-looping, while every
/// other non-zero exit stays a genuine crash that systemd restarts.
fn exit_code(error: &Error) -> std::process::ExitCode {
    std::process::ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(1))
}
