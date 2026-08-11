//! Every filesystem location the toolchain touches, derived from one root.
//!
//! NOTHING HERE IS A CONSTANT PATH, and that is the point. The shell version
//! assigned `MC_BASE=/opt/minecraft` at source time, so a test could only
//! reassign the globals *after* sourcing and hope nothing had read them yet.
//! Threading a `Paths` through instead means the install, upgrade, backup and
//! properties paths can all be driven against a temp dir by an unprivileged
//! test — which is what keeps the bulk of the suite off Docker and off root.
//!
//! `MC_ROOT` exists for tests and for a container image that stages a tree
//! somewhere other than `/`. Production is `Paths::system()`, whose root is `/`.

use std::path::{Path, PathBuf};

/// The service account. Owns `MC_BASE` and runs the JVM.
pub const MC_USER: &str = "minecraft";

/// Minecraft's stock port.
///
/// Ports belong to the server, so they live in server.properties; mc's own
/// config describes how to RUN the server, not what it is. This constant
/// applies only where that file does not exist yet: seeding `server-port` into
/// a new one, and standing in for the game port when computing the RCON port.
pub const STOCK_PORT: u16 = 25565;

/// Name of the systemd unit the CLI drives.
pub const SERVICE_UNIT: &str = "minecraft";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    root: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self::system()
    }
}

impl Paths {
    /// The real filesystem.
    pub fn system() -> Self {
        Self {
            root: PathBuf::from("/"),
        }
    }

    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `MC_ROOT` if set, otherwise `/`.
    ///
    /// Read once at startup by the binaries and threaded from there; nothing
    /// deeper in the tree consults the environment, so a test never has to
    /// worry about which call happened to read it first.
    pub fn from_env() -> Self {
        match std::env::var_os("MC_ROOT") {
            Some(root) if !root.is_empty() => Self::with_root(root),
            _ => Self::system(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Join an absolute-looking path onto the root.
    ///
    /// `Path::join` REPLACES the base when given an absolute path, so
    /// `root.join("/opt/minecraft")` silently yields `/opt/minecraft` and every
    /// test would write to the real system. The leading separator is stripped
    /// first for exactly that reason.
    fn at(&self, absolute: &str) -> PathBuf {
        self.root.join(absolute.trim_start_matches('/'))
    }

    // ── Server data ────────────────────────────────────────────────────────

    /// `minecraft:minecraft` 0750, and the only `ReadWritePaths=` entry in the
    /// unit. Not world-readable: server.properties inside it carries the RCON
    /// password.
    pub fn base(&self) -> PathBuf {
        self.at("/opt/minecraft")
    }

    pub fn server_properties(&self) -> PathBuf {
        self.base().join("server.properties")
    }

    pub fn eula(&self) -> PathBuf {
        self.base().join("eula.txt")
    }

    pub fn server_jar(&self) -> PathBuf {
        self.base().join("server.jar")
    }

    /// NeoForge installs a launcher script instead of a plain server.jar, so
    /// "is a server installed" is a test of either.
    pub fn run_sh(&self) -> PathBuf {
        self.base().join("run.sh")
    }

    // ── mc's own configuration ─────────────────────────────────────────────

    /// root:root 0755. Holds config.toml and the RCON password.
    pub fn config_dir(&self) -> PathBuf {
        self.at("/etc/minecraft")
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir().join("config.toml")
    }

    /// root:minecraft 0640 — the service account can read the secret, nobody
    /// else can.
    pub fn passwd_file(&self) -> PathBuf {
        self.config_dir().join("server.passwd")
    }

    /// Records that the server came from a modpack, so `mc upgrade` can refuse
    /// a bare version bump that would strip the mods.
    pub fn mrpack_manifest(&self) -> PathBuf {
        self.config_dir().join("server.mrpack.json")
    }

    // ── Backups ────────────────────────────────────────────────────────────

    /// root:root 0700 ON PURPOSE — never owned by the service account.
    ///
    /// Backups are written by root and read only by root (`mc restore`).
    /// Handing this directory to the account that runs untrusted mods would let
    /// a compromised server pre-create the next predictable archive name as a
    /// symlink for root's tar to follow, or swap an archive between restore's
    /// validation pass and its extraction.
    pub fn backup_dir(&self) -> PathBuf {
        self.at("/var/backups/minecraft")
    }

    // ── Runtime ────────────────────────────────────────────────────────────

    /// Serialises install/upgrade/backup/restore against each other.
    pub fn lock_file(&self) -> PathBuf {
        self.at("/run/minecraft/mc.lock")
    }

    // ── Installed program files ────────────────────────────────────────────

    /// Plugin manifests. Core scans this at startup; a package drops one file
    /// in to contribute subcommands and hooks.
    pub fn plugins_dir(&self) -> PathBuf {
        self.at("/usr/lib/mc/plugins.d")
    }

    /// Plugin executables. Not `/usr/bin`: these are invoked by core, not by
    /// the operator, and putting them on `PATH` would advertise a command
    /// surface (`mc-backup command backup`) that is not the supported one.
    pub fn libexec_dir(&self) -> PathBuf {
        self.at("/usr/libexec/mc")
    }

    pub fn libexec(&self, name: &str) -> PathBuf {
        self.libexec_dir().join(name)
    }

    /// The dispatcher itself, spelled out rather than taken from `argv[0]`:
    /// re-execing under sudo needs a path that survives a reset environment,
    /// and argv[0] is whatever the caller typed — possibly relative, possibly
    /// reached through a symlink, possibly resolved from a PATH the elevated
    /// process will not have.
    pub fn mc_bin(&self) -> PathBuf {
        self.at("/usr/bin/mc")
    }

    /// True when a server is present. NeoForge installs a `run.sh` instead of a
    /// plain server.jar, so both count.
    pub fn server_installed(&self) -> bool {
        self.server_jar().is_file() || self.run_sh().is_file()
    }

    /// `/run/systemd/system` is the canonical "systemd is the running init"
    /// test, and what `dh_installsystemd` generates.
    ///
    /// Deliberately NOT `which systemctl`: the binary is present in plenty of
    /// places systemd is not running (containers), where a reload is a
    /// guaranteed error rather than a no-op. Also deliberately not
    /// `systemctl is-system-running`, which is a HEALTH check — it returns
    /// non-zero for `degraded`, i.e. any machine with one unrelated failed
    /// unit.
    pub fn systemd_running(&self) -> bool {
        self.at("/run/systemd/system").is_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_prefixed_not_replaced() {
        // The bug this guards: Path::join with an absolute argument discards
        // the base, so a sandboxed test would write to the real /opt/minecraft.
        let p = Paths::with_root("/tmp/sandbox");
        assert_eq!(p.base(), Path::new("/tmp/sandbox/opt/minecraft"));
        assert_eq!(
            p.config_file(),
            Path::new("/tmp/sandbox/etc/minecraft/config.toml")
        );
        assert_eq!(
            p.lock_file(),
            Path::new("/tmp/sandbox/run/minecraft/mc.lock")
        );
        assert_eq!(
            p.backup_dir(),
            Path::new("/tmp/sandbox/var/backups/minecraft")
        );
    }

    #[test]
    fn system_paths_are_the_documented_ones() {
        let p = Paths::system();
        assert_eq!(p.base(), Path::new("/opt/minecraft"));
        assert_eq!(p.backup_dir(), Path::new("/var/backups/minecraft"));
        assert_eq!(p.config_dir(), Path::new("/etc/minecraft"));
        assert_eq!(p.passwd_file(), Path::new("/etc/minecraft/server.passwd"));
        assert_eq!(p.plugins_dir(), Path::new("/usr/lib/mc/plugins.d"));
        assert_eq!(p.mc_bin(), Path::new("/usr/bin/mc"));
        assert_eq!(
            p.server_properties(),
            Path::new("/opt/minecraft/server.properties")
        );
    }

    #[test]
    fn server_installed_accepts_either_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::with_root(dir.path());
        std::fs::create_dir_all(p.base()).unwrap();
        assert!(!p.server_installed());

        std::fs::write(p.run_sh(), "#!/bin/sh\n").unwrap();
        assert!(
            p.server_installed(),
            "NeoForge installs run.sh, not server.jar"
        );

        std::fs::remove_file(p.run_sh()).unwrap();
        std::fs::write(p.server_jar(), "").unwrap();
        assert!(p.server_installed());
    }
}
