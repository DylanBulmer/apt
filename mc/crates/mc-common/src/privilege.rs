//! Who may run what.
//!
//! Two guards, and the difference between them is the whole policy:
//!
//! * Anything that WRITES takes [`require_root`].
//! * Anything that only READS takes [`require_root_or_group`].
//!
//! The `minecraft` group is already the unit of access to this server's files —
//! `MC_BASE` is 0750 `minecraft:minecraft`, `server.properties` inside it 0640,
//! and the RCON password 0640 `root:minecraft`. A member can therefore read the
//! port and the password and drive the running server with `rcon` by hand.
//! Demanding root for a command that only reads those same files would protect
//! nothing and would leave `mc` less capable than the binary it wraps.

use std::path::Path;

use nix::unistd::{Gid, Uid};

use crate::error::{Error, Result};
use crate::paths::MC_USER;

/// Set on the re-executed process so an elevation can never recurse.
const ELEVATED_SENTINEL: &str = "MC_ELEVATED";

pub fn is_root() -> bool {
    Uid::effective().is_root()
}

/// True when this process actually holds the `minecraft` group.
///
/// From the groups the process HOLDS, not from a lookup by name. A group added
/// since login does not apply until the next one, and a name lookup would
/// report access the process does not have — it would tell a user their
/// commands will work, right up until they fail on a permission error.
#[cfg(not(target_vendor = "apple"))]
pub fn in_service_group() -> bool {
    let Some(target) = crate::fsx::lookup_group(MC_USER) else {
        return false;
    };
    nix::unistd::getgroups()
        .map(|groups| groups.contains(&target))
        .unwrap_or(false)
}

/// macOS has no safe `getgroups` wrapper in `nix`, and the target for these
/// packages is Debian — this exists so the suite compiles and runs on a
/// development Mac. It answers by roster lookup, which is exactly the weaker
/// question the Linux path avoids, so the behaviour under test on macOS is not
/// evidence about the real guard. The container suites are.
#[cfg(target_vendor = "apple")]
pub fn in_service_group() -> bool {
    let Some(group) = nix::unistd::Group::from_name(MC_USER).ok().flatten() else {
        return false;
    };
    if nix::unistd::getgid() == group.gid || nix::unistd::getegid() == group.gid {
        return true;
    }
    nix::unistd::User::from_uid(Uid::effective())
        .ok()
        .flatten()
        .is_some_and(|me| group.mem.contains(&me.name))
}

/// Re-run this invocation under sudo, replacing the current process.
///
/// Returns `Ok(())` only when elevation was NOT attempted; callers report the
/// refusal themselves. Nothing here weakens a privilege boundary — sudo still
/// applies its own policy, and a user with no sudo rights gets sudo's refusal
/// instead of ours. It only spares the operator retyping a command that was
/// always going to need root.
pub fn elevate(mc_bin: &Path, argv: &[String]) -> Result<()> {
    let already_elevated = std::env::var_os(ELEVATED_SENTINEL).is_some();
    if !should_elevate(already_elevated, which("sudo").is_some(), is_interactive()) {
        return Ok(());
    }

    crate::ui::warn("This needs root — re-running under sudo.");

    // The sentinel is passed through `env` as part of the command rather than
    // exported: sudo resets the environment by default, and --preserve-env
    // needs a sudoers grant this cannot assume.
    let err = exec_replacing(
        "sudo",
        &[
            "--".to_string(),
            "env".to_string(),
            format!("{ELEVATED_SENTINEL}=1"),
            mc_bin.display().to_string(),
        ],
        argv,
    );
    // Only reached if the exec itself failed.
    Err(Error::other(format!("could not re-run under sudo: {err}")))
}

/// The whole elevation policy, as a function of three facts.
///
/// Split out from [`elevate`] so it can be tested without mutating the process
/// environment — `std::env::set_var` is `unsafe` in edition 2024 (another
/// thread may be reading the environment), and this crate denies unsafe.
fn should_elevate(already_elevated: bool, sudo_available: bool, interactive: bool) -> bool {
    // Guards against a sudoers `runas_default` that is not root. Without it,
    // such a host re-enters this function as an unprivileged user and spawns
    // another sudo, forever.
    if already_elevated {
        return false;
    }
    if !sudo_available {
        return false;
    }
    // Only escalate where a human can answer the prompt. Under the backup
    // timer, a hook or a CI runner, sudo would block on a password nobody can
    // type; refusing outright is the honest outcome, and the caller's message
    // says what to do.
    interactive
}

fn exec_replacing(program: &str, prefix: &[String], argv: &[String]) -> std::io::Error {
    use std::os::unix::process::CommandExt as _;
    std::process::Command::new(program)
        .args(prefix)
        .args(argv)
        .exec()
}

fn is_interactive() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn which(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Guard for anything that writes.
pub fn require_root(mc_bin: &Path, argv: &[String]) -> Result<()> {
    if is_root() {
        return Ok(());
    }
    elevate(mc_bin, argv)?;
    Err(Error::denied(format!(
        "This command must be run as root: sudo mc {}",
        argv.join(" ")
    )))
}

/// Guard for anything that only reads.
pub fn require_root_or_group(mc_bin: &Path, argv: &[String]) -> Result<()> {
    if is_root() || in_service_group() {
        return Ok(());
    }
    // Elevation is the fallback, not the fix: joining the group makes this and
    // every later read work with no prompt at all, so the refusal leads with it.
    elevate(mc_bin, argv)?;
    Err(Error::denied(format!(
        "This command must be run as root, or by a member of the '{MC_USER}' group.\n\
         Add yourself:  sudo usermod -aG {MC_USER} $USER   (then log out and back in)"
    )))
}

/// What privilege a command needs. Declared per command so that a new
/// subcommand cannot ship without the question having been answered — see the
/// table-driven test in the `mc` crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    /// Writes something: config, the server directory, a unit file.
    Root,
    /// Only reads files the service group can already reach.
    RootOrGroup,
    /// Runs as the service account under `ProtectSystem=strict`, as systemd's
    /// `ExecStart=`/`ExecStop=`/`ExecReload=`. MUST NOT take a root guard: the
    /// unit runs these as `minecraft`, and a guard here means the server never
    /// starts.
    ServiceAccount,
    /// Needs no privilege at all — help, version, listing plugins.
    None,
}

impl Requirement {
    pub fn enforce(&self, mc_bin: &Path, argv: &[String]) -> Result<()> {
        match self {
            Requirement::Root => require_root(mc_bin, argv),
            Requirement::RootOrGroup => require_root_or_group(mc_bin, argv),
            Requirement::ServiceAccount | Requirement::None => Ok(()),
        }
    }
}

/// The uid/gid the service runs as, when the account exists.
pub fn service_account() -> Option<(Uid, Gid)> {
    crate::fsx::lookup_user(MC_USER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_exec_targets_never_take_a_privilege_guard() {
        // The unit runs these as the `minecraft` user under
        // ProtectSystem=strict. A root guard here means the server never
        // starts, and the failure looks like a config problem.
        let bin = Path::new("/usr/bin/mc");
        let argv = vec!["serve".to_string()];
        assert!(Requirement::ServiceAccount.enforce(bin, &argv).is_ok());
        assert!(Requirement::None.enforce(bin, &argv).is_ok());
    }

    #[test]
    fn the_elevation_sentinel_stops_a_sudo_loop() {
        // A sudoers runas_default that is not root would otherwise re-enter
        // this as an unprivileged user and spawn another sudo, forever.
        assert!(!should_elevate(true, true, true));
    }

    #[test]
    fn elevation_needs_a_terminal_and_a_sudo_to_run() {
        // Under the backup timer, a hook or a CI runner, sudo would block on a
        // password nobody can type.
        assert!(!should_elevate(false, true, false), "no terminal");
        assert!(!should_elevate(false, false, true), "no sudo installed");
        assert!(
            should_elevate(false, true, true),
            "the one case that elevates"
        );
    }

    #[test]
    fn elevate_returns_rather_than_execs_under_a_captured_stdio() {
        // `cargo test` captures stdio, so this exercises the real function on
        // the non-interactive path — it must return, not replace the process.
        assert!(!is_interactive());
        assert!(elevate(Path::new("/usr/bin/mc"), &["backup".to_string()]).is_ok());
    }

    #[test]
    fn a_refusal_names_the_command_the_operator_typed() {
        // The guard fires after the command's own option parsing has consumed
        // the arguments, so the message has to come from a captured argv rather
        // than from whatever is left in it.
        if is_root() {
            return; // nothing to refuse
        }
        let argv = vec![
            "install".to_string(),
            "--type".to_string(),
            "paper".to_string(),
        ];
        let err = require_root(Path::new("/usr/bin/mc"), &argv).unwrap_err();
        assert!(err.to_string().contains("mc install --type paper"), "{err}");
    }
}
