//! Operator-facing output.
//!
//! Two audiences, and they want different things. A terminal wants the colour;
//! the journal wants none, because `journalctl` renders ANSI escapes as noise
//! and half of this toolchain's output goes there — `mc serve` and
//! `mc shutdown` are systemd exec targets. Colour is therefore decided by
//! whether stderr is a terminal, not by a flag.

use std::io::{IsTerminal as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};

static COLOUR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
static QUIET: AtomicBool = AtomicBool::new(false);

fn colour() -> bool {
    *COLOUR.get_or_init(|| {
        // NO_COLOR is honoured because this runs in other people's pipelines.
        if std::env::var_os("NO_COLOR").is_some() {
            return false;
        }
        std::io::stderr().is_terminal()
    })
}

/// Suppress `info` output. Warnings and errors are never suppressed.
pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

const RED: &str = "\x1b[0;31m";
const GREEN: &str = "\x1b[0;32m";
const YELLOW: &str = "\x1b[1;33m";
const OFF: &str = "\x1b[0m";

fn emit(colour_code: &str, msg: &str) {
    let mut err = std::io::stderr().lock();
    let _ = if colour() {
        writeln!(err, "{colour_code}[mc]{OFF} {msg}")
    } else {
        writeln!(err, "[mc] {msg}")
    };
}

/// Progress and confirmations.
///
/// To stderr, not stdout, so that a command whose stdout is a value — an RCON
/// reply, a resolved version — stays pipeable.
pub fn info(msg: impl AsRef<str>) {
    if !QUIET.load(Ordering::Relaxed) {
        emit(GREEN, msg.as_ref());
    }
}

pub fn warn(msg: impl AsRef<str>) {
    emit(YELLOW, msg.as_ref());
}

pub fn error(msg: impl AsRef<str>) {
    emit(RED, msg.as_ref());
}

/// Ask a yes/no question. `false` when there is no terminal to ask on.
///
/// Callers must treat "no terminal" as a refusal, never as a yes: this is the
/// path that gates accepting a licence agreement and installing packages.
pub fn confirm(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "{question} [y/N] ");
    let _ = err.flush();
    drop(err);

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim(), "y" | "Y" | "yes" | "YES" | "Yes")
}

/// Ask for a literal typed confirmation, for irreversible operations.
pub fn confirm_typed(question: &str, expected: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "{question} ");
    let _ = err.flush();
    drop(err);

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    answer.trim() == expected
}
