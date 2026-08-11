//! `server.properties` — *the server's* configuration, as distinct from mc's.
//!
//! The JVM reads and rewrites this file, so mc keeps no copy of anything in it:
//! port, seed, MOTD, difficulty and RCON are read here at the point of use.
//! A game setting mirrored into mc's own config can only go stale.
//!
//! Values reaching this module are NOT trusted. A `.mrpack` ships its own
//! `server.properties` and it is merged into the live one as root.

use std::path::Path;

use nix::unistd::{Gid, Uid};

use crate::error::{IoContext, Result};
use crate::fsx;
use crate::paths::{MC_USER, Paths, STOCK_PORT};

/// server.properties holds the RCON password, so it must never be
/// world-readable. It is owned and rewritten by the JVM's own user, so 0640 is
/// the tightest mode that still lets the server read and write it.
pub const MODE: u32 = 0o640;

/// Keys the system owns. A pack override never gets to set these.
///
/// Both consoles' credentials are here for the same reason: a `.mrpack` is
/// attacker-controlled input merged into this file as root, and a pack that
/// could choose `rcon.password` or `management-server-secret` would be a pack
/// that hands itself a console on the operator's server. The management keys
/// carry no defaults of their own — see [`managed_value`] — so a pack's values
/// are discarded and whatever `mc mgmt enable` provisioned is restored.
pub const MANAGED_KEYS: [&str; 8] = [
    "server-port",
    "enable-rcon",
    "rcon.port",
    "rcon.password",
    "management-server-enabled",
    "management-server-host",
    "management-server-port",
    "management-server-secret",
];

/// One physical line. Comments, blanks and anything unparseable are carried
/// through verbatim: the JVM writes a dated comment header and operators edit
/// this file by hand, and a rewrite that dropped either would be a surprise.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Entry { key: String, value: String },
    Raw(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Properties {
    lines: Vec<Line>,
}

impl Properties {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse from text.
    ///
    /// Keys are matched by exact string prefix, never by pattern. The shell
    /// implementation had to escape `.` before every comparison because two
    /// managed keys contain one and an unescaped regex let `rcon.port` also
    /// match `rconXport`. There is no pattern here to get wrong.
    pub fn parse(text: &str) -> Self {
        let lines = text
            .lines()
            .map(|line| match line.split_once('=') {
                // A leading '#' means the whole line is a comment, even when it
                // contains an '='. Treating `#rcon.password=x` as an entry
                // would resurrect a setting the operator commented out.
                Some((key, value)) if !line.trim_start().starts_with('#') && !key.is_empty() => {
                    Line::Entry {
                        key: key.to_string(),
                        value: value.to_string(),
                    }
                }
                _ => Line::Raw(line.to_string()),
            })
            .collect();
        Self { lines }
    }

    /// Read from disk.
    ///
    /// An absent or unreadable file yields an EMPTY set, not an error. A key
    /// that is simply not there is a normal answer — the shell version had to
    /// go out of its way to guarantee this, because `grep | cut` under
    /// `pipefail` turned an absent key into an abort of the whole invocation.
    /// The gate that turns an *unreadable* file into a refusal lives in
    /// `mc serve`, where it can say so legibly.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(_) => Self::new(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.iter().find_map(|line| match line {
            Line::Entry { key: k, value } if k == key => Some(value.as_str()),
            _ => None,
        })
    }

    /// Parse a value as a port. Anything that is not a plain u16 is `None`.
    ///
    /// Every numeric read goes through here. The shell version had to force
    /// values to digits before any comparison because `[[ "$x" -ge 1 ]]` is an
    /// arithmetic context, and bash performs command substitution inside array
    /// subscripts while evaluating one — so a value of `PATH[$(rm -rf /)]` ran.
    /// Rust has no such context, but the validation stays: a garbage port must
    /// fall back rather than propagate into a URL or a socket address.
    pub fn get_port(&self, key: &str) -> Option<u16> {
        self.get(key)?.trim().parse().ok()
    }

    /// Set or replace a key.
    ///
    /// Duplicate definitions of the same key collapse onto the first, matching
    /// the JVM's own last-writer-wins-on-read behaviour without leaving a
    /// second line that a later hand-edit might "fix".
    pub fn set(&mut self, key: &str, value: &str) {
        let mut found = false;
        self.lines.retain_mut(|line| match line {
            Line::Entry { key: k, value: v } if k == key => {
                if found {
                    return false;
                }
                found = true;
                value.clone_into(v);
                true
            }
            _ => true,
        });
        if !found {
            self.lines.push(Line::Entry {
                key: key.to_string(),
                value: value.to_string(),
            });
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            match line {
                Line::Entry { key, value } => out.push_str(&format!("{key}={value}\n")),
                Line::Raw(raw) => {
                    out.push_str(raw);
                    out.push('\n');
                }
            }
        }
        out
    }

    /// Write to disk with the intended owner and mode.
    ///
    /// Never a plain `fs::write`: a fresh file created by root is 0600
    /// root:root, and the service account can then neither read nor write its
    /// own configuration.
    pub fn save(&self, path: &Path) -> Result<()> {
        let owner = fsx::lookup_user(MC_USER);
        fsx::write_atomic(path, &self.render(), owner, MODE)?;
        // write_atomic inherits from the file it replaced, which may be wrong
        // if the JVM last wrote it under its own umask, or if an editor left it
        // root-owned. Assert the intended state rather than whatever was there.
        secure(path)
    }
}

/// Apply the intended owner AND mode.
///
/// The mode alone is not enough. 0640 is readable only because the owner is
/// `$MC_USER`; every writer in mc runs as root, so a file merely chmod'ed ends
/// up 0640 root:root — which the JVM can neither read nor write. That failure
/// is near-silent: the server logs a stack trace and "Failed to store
/// properties", then falls back to compiled-in defaults (stock port, RCON off,
/// level-name "world"), so a server that looks like it started fine is running
/// a configuration nobody chose — and, if level-name was customised, generating
/// a brand-new empty world beside the real one.
pub fn secure(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    fsx::apply_owner_mode(path, fsx::lookup_user(MC_USER), MODE)
}

/// Read one key from the live `server.properties`, with the "never fails"
/// contract of the shell `mc_sprop_get`.
pub fn read_key(paths: &Paths, key: &str) -> Option<String> {
    Properties::load(&paths.server_properties())
        .get(key)
        .map(str::to_string)
}

/// The port to dial for RCON.
///
/// `rcon.port` is checked FIRST and used verbatim, because it is the port the
/// JVM binds. The +10 convention is only this toolchain's default for a server
/// it is setting up; an operator who sets `rcon.port` by hand — or a modpack
/// that ships one — is not obliged to follow it, and deriving the port
/// unconditionally meant every such server had a working RCON listener that
/// nothing in mc could reach. That failure is quiet and expensive: a stop reads
/// an unreachable RCON as "player count unknown" and takes the full countdown
/// every time, and a backup loses save-off/save-all and archives a world that
/// was never flushed.
///
/// Order: what the JVM binds → the convention applied to the live game port →
/// the convention applied to the stock port. The last tier only applies before
/// a server.properties exists at all; it keeps this total rather than failing.
pub fn rcon_port(props: &Properties) -> u16 {
    if let Some(port) = props.get_port("rcon.port") {
        return port;
    }
    let game = props.get_port("server-port").unwrap_or(STOCK_PORT);
    game.saturating_add(10)
}

/// The value the system wants for a managed key.
///
/// Whatever the live file already says, or — when there is no live file yet —
/// the correct value derived from whether mc-rcon has provisioned a password.
/// One definition, shared by the initial write and by every pack merge, so the
/// two cannot disagree about whether RCON should be on.
pub fn managed_value(paths: &Paths, live: &Properties, key: &str) -> String {
    if let Some(current) = live.get(key)
        && !current.is_empty()
    {
        return current.to_string();
    }

    let passwd = paths.passwd_file();
    match key {
        "server-port" => STOCK_PORT.to_string(),
        "rcon.port" => rcon_port(live).to_string(),
        // RCON is on only when mc-rcon has generated a password.
        "enable-rcon" => passwd.is_file().to_string(),
        "rcon.password" => std::fs::read_to_string(&passwd)
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Merge a pack-supplied `server.properties` into the live one, protecting the
/// keys the system owns.
///
/// Runs even when there is NO existing file, so the managed keys are always
/// re-applied. Skipping that on a first install would put the pack's own file
/// in place verbatim, letting it set `enable-rcon=true` with a password of its
/// choosing — and the server then binds RCON with a secret the pack author
/// knows.
///
/// Managed values are written unconditionally, empty ones included: skipping an
/// empty value is exactly how a pack-supplied `rcon.password` survives.
pub fn merge(paths: &Paths, override_text: &str) -> Result<()> {
    let dest = paths.server_properties();
    let live = Properties::load(&dest);

    // Resolved BEFORE the merge, because resolution reads the file that is
    // about to be replaced.
    let saved: Vec<(&str, String)> = MANAGED_KEYS
        .iter()
        .map(|key| (*key, managed_value(paths, &live, key)))
        .collect();

    let mut merged = Properties::parse(override_text);
    for (key, value) in saved {
        merged.set(key, &value);
    }
    merged.save(&dest)
}

/// Write the initial `server.properties`, RCON off unless a password exists.
///
/// Does NOT touch `eula.txt` — accepting a licence is a separate decision,
/// gated on `--accept-eula` or an interactive yes.
pub fn init(paths: &Paths) -> Result<()> {
    let dest = paths.server_properties();
    let live = Properties::load(&dest);

    let mut props = Properties::new();
    for key in MANAGED_KEYS {
        let value = managed_value(paths, &live, key);
        props.set(key, &value);
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).at(parent)?;
    }
    props.save(&dest)
}

/// The owner `server.properties` is meant to have, for tests and diagnostics.
pub fn intended_owner() -> Option<(Uid, Gid)> {
    fsx::lookup_user(MC_USER)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());
        std::fs::create_dir_all(paths.base()).unwrap();
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        (dir, paths)
    }

    #[test]
    fn round_trips_comments_and_blank_lines() {
        let text = "#Minecraft server properties\n#Mon Jan 01 00:00:00 UTC 2026\n\nmotd=hi\nserver-port=25565\n";
        let props = Properties::parse(text);
        assert_eq!(props.render(), text);
        assert_eq!(props.get("motd"), Some("hi"));
    }

    #[test]
    fn a_dot_in_a_key_is_not_a_wildcard() {
        // The shell version matched keys with a regex and had to escape '.'
        // first; unescaped, `rcon.port` also matched `rconXport`.
        let props = Properties::parse("rconXport=1\nrcon.port=25575\n");
        assert_eq!(props.get("rcon.port"), Some("25575"));
        assert_eq!(props.get("rconXport"), Some("1"));
    }

    #[test]
    fn absent_key_and_absent_file_are_not_errors() {
        assert_eq!(Properties::parse("a=1\n").get("b"), None);
        assert_eq!(
            Properties::load(Path::new("/nonexistent/server.properties")).get("a"),
            None
        );
    }

    #[test]
    fn commented_out_key_stays_commented_out() {
        let props = Properties::parse("#rcon.password=leaked\n");
        assert_eq!(props.get("rcon.password"), None);
    }

    #[test]
    fn set_survives_values_that_broke_the_sed_implementation() {
        // The shell version rewrote with `sed "s|^k=.*|k=$v|"`, so a value
        // containing the '|' delimiter closed the s/// and the rest was parsed
        // as sed syntax — `x|w /etc/cron.d/pwn|` became a write-file command
        // running as root. These values come from pack-supplied files.
        for hostile in [
            "x|w /etc/cron.d/pwn|",
            "a&b",
            "back\\slash",
            "$(touch /tmp/pwned)",
            "`id`",
            "",
        ] {
            let mut props = Properties::parse("motd=old\n");
            props.set("motd", hostile);
            assert_eq!(props.get("motd"), Some(hostile));
            assert_eq!(props.render(), format!("motd={hostile}\n"));
        }
    }

    #[test]
    fn set_collapses_duplicate_keys_onto_the_first() {
        let mut props = Properties::parse("a=1\nb=2\na=3\n");
        props.set("a", "9");
        assert_eq!(props.render(), "a=9\nb=2\n");
    }

    #[test]
    fn set_appends_a_missing_key_in_place() {
        let mut props = Properties::parse("a=1\n");
        props.set("b", "2");
        assert_eq!(props.render(), "a=1\nb=2\n");
    }

    #[test]
    fn rcon_port_prefers_what_the_jvm_binds() {
        let props = Properties::parse("server-port=25700\nrcon.port=30000\n");
        assert_eq!(rcon_port(&props), 30000);
    }

    #[test]
    fn rcon_port_falls_back_through_the_three_tiers() {
        assert_eq!(rcon_port(&Properties::parse("server-port=25700\n")), 25710);
        assert_eq!(rcon_port(&Properties::new()), STOCK_PORT + 10);
    }

    #[test]
    fn rcon_port_never_evaluates_a_non_numeric_value() {
        // Not merely "returns a default": the point is that a hostile string
        // reaches no evaluator on the way. Each of these must fall through to
        // the next tier exactly as an absent key would.
        for hostile in [
            "PATH[$(touch /tmp/mc-canary)]",
            "",
            "  ",
            "-1",
            "99999999",
            "25575x",
        ] {
            let props = Properties::parse(&format!("rcon.port={hostile}\nserver-port=25700\n"));
            assert_eq!(rcon_port(&props), 25710, "rcon.port={hostile:?}");
        }
        assert!(!Path::new("/tmp/mc-canary").exists());
    }

    #[test]
    fn a_pack_cannot_choose_the_rcon_password() {
        let (_dir, paths) = sandbox();
        std::fs::write(paths.passwd_file(), "the-real-secret\n").unwrap();
        init(&paths).unwrap();

        merge(
            &paths,
            "motd=Modpack\nenable-rcon=true\nrcon.password=attacker-chosen\nrcon.port=31337\nserver-port=1234\n",
        )
        .unwrap();

        let live = Properties::load(&paths.server_properties());
        assert_eq!(live.get("rcon.password"), Some("the-real-secret"));
        assert_eq!(live.get("rcon.port"), Some("25575"));
        assert_eq!(live.get("server-port"), Some("25565"));
        // Everything the pack is *allowed* to set still lands.
        assert_eq!(live.get("motd"), Some("Modpack"));
    }

    #[test]
    fn a_pack_cannot_choose_the_management_secret() {
        // The same attack as the RCON one, against the newer console: a pack
        // that could set `management-server-secret` would hand itself a
        // console on the operator's server, and one that could move the host
        // off loopback would publish it.
        let (_dir, paths) = sandbox();
        init(&paths).unwrap();

        // What `mc mgmt enable` provisioned.
        let mut live = Properties::load(&paths.server_properties());
        live.set("management-server-enabled", "true");
        live.set("management-server-host", "localhost");
        live.set("management-server-port", "25585");
        live.set("management-server-secret", "the-real-secret");
        live.save(&paths.server_properties()).unwrap();

        merge(
            &paths,
            "motd=Modpack\n\
             management-server-enabled=true\n\
             management-server-host=0.0.0.0\n\
             management-server-port=31337\n\
             management-server-secret=attacker-chosen\n",
        )
        .unwrap();

        let live = Properties::load(&paths.server_properties());
        assert_eq!(
            live.get("management-server-secret"),
            Some("the-real-secret")
        );
        assert_eq!(live.get("management-server-host"), Some("localhost"));
        assert_eq!(live.get("management-server-port"), Some("25585"));
        assert_eq!(live.get("motd"), Some("Modpack"));
    }

    #[test]
    fn a_pack_cannot_switch_the_management_protocol_on_by_itself() {
        // With nothing provisioned there is no secret to preserve, so the
        // danger is the opposite one: a pack turning the endpoint ON, which
        // would leave it listening with whatever secret the pack chose.
        let (_dir, paths) = sandbox();
        init(&paths).unwrap();

        merge(
            &paths,
            "management-server-enabled=true\nmanagement-server-secret=attacker-chosen\n",
        )
        .unwrap();

        let live = Properties::load(&paths.server_properties());
        assert_ne!(
            live.get("management-server-secret"),
            Some("attacker-chosen"),
            "a pack's secret must never survive a merge"
        );
        assert_ne!(
            live.get("management-server-enabled"),
            Some("true"),
            "a pack must not switch the endpoint on"
        );
    }

    #[test]
    fn merge_protects_managed_keys_even_with_no_existing_file() {
        // The dangerous case: a first install from a pack. With no live file to
        // preserve values from, a merge that skipped the re-apply would install
        // the pack's file verbatim.
        let (_dir, paths) = sandbox();
        assert!(!paths.server_properties().exists());

        merge(&paths, "enable-rcon=true\nrcon.password=attacker-chosen\n").unwrap();

        let live = Properties::load(&paths.server_properties());
        assert_eq!(
            live.get("enable-rcon"),
            Some("false"),
            "no password file provisioned"
        );
        assert_eq!(live.get("rcon.password"), Some(""));
    }

    #[test]
    fn init_enables_rcon_only_once_a_password_exists() {
        let (_dir, paths) = sandbox();
        init(&paths).unwrap();
        let live = Properties::load(&paths.server_properties());
        assert_eq!(live.get("enable-rcon"), Some("false"));
        assert_eq!(live.get("rcon.password"), Some(""));

        // mc-rcon installed afterwards: re-running init picks it up, whatever
        // order the two packages were installed in.
        let (_dir2, paths2) = sandbox();
        std::fs::write(paths2.passwd_file(), "s3cret\n").unwrap();
        init(&paths2).unwrap();
        let live2 = Properties::load(&paths2.server_properties());
        assert_eq!(live2.get("enable-rcon"), Some("true"));
        assert_eq!(live2.get("rcon.password"), Some("s3cret"));
    }

    #[test]
    fn saved_file_is_never_world_readable() {
        let (_dir, paths) = sandbox();
        init(&paths).unwrap();
        let mode = fsx::mode_of(&paths.server_properties()).unwrap();
        assert_eq!(mode, MODE, "server.properties carries the RCON password");
        assert_eq!(mode & 0o007, 0, "no access for other");
    }

    #[test]
    fn init_preserves_an_operator_chosen_port() {
        let (_dir, paths) = sandbox();
        std::fs::write(paths.server_properties(), "server-port=25700\n").unwrap();
        init(&paths).unwrap();
        assert_eq!(
            Properties::load(&paths.server_properties()).get("server-port"),
            Some("25700")
        );
    }
}
