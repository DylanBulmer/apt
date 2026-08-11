//! The plugin contract.
//!
//! A plugin is a `.deb` that drops a TOML manifest into
//! `/usr/lib/mc/plugins.d/` and an executable into `/usr/libexec/mc/`. Core
//! discovers the manifest at startup and invokes the executable across a
//! process boundary.
//!
//! ## Why out-of-process
//!
//! Rust has no stable ABI, so a `dlopen`-based plugin would have to be pinned
//! to an exact core version and rebuilt in lockstep — which defeats the point
//! of "adding a plugin is installing another .deb". A process boundary and a
//! declared [`ABI`] number cost one `fork`/`exec` per invocation, which is
//! nothing at CLI latency, and buy an interface that survives a core rebuild.
//!
//! The [`ABI`] field replaces the versioned `Depends:` the shell packaging
//! needed. That mechanism was fragile in a specific way: `mc-rcon`'s postinst
//! sourced another package's private shell library, so a missed version-floor
//! bump left dpkg configuring the plugin against a library without the function
//! it called, dying with exit 127 and a half-installed package. Here core reads
//! the number and refuses by name.
//!
//! ## What must NOT move into a plugin
//!
//! Anything holding the lock, the EULA gate, ownership of `MC_BASE`, or the
//! managed keys of `server.properties`. A plugin contributes a *step*; core
//! keeps the ordering that makes the step safe.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::{MC_USER, Paths};

/// The plugin interface version core implements.
///
/// Bump this ONLY for a breaking change to the manifest schema, the hook
/// payloads, or the provider protocol. Every installed plugin declaring an
/// older number stops being loaded the moment this changes, so a bump is a
/// coordinated release of every package in the tree — treat it as such.
pub const ABI: u32 = 1;

/// A point in a core operation at which plugins get to act.
///
/// Kebab-case in the manifest. Unknown events are a manifest error rather than
/// a silent no-op: a plugin declaring `event = "pre-stopp"` would otherwise
/// install cleanly and never run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Event {
    /// Before the unit is started.
    PreStart,
    /// Before the server is told to stop. `mc-rcon` runs the player countdown
    /// here.
    ///
    /// MAY NEVER BE FATAL — see [`HookDecl::fatal`].
    PreStop,
    /// Before a backup archives the world. `mc-rcon` sends save-off/save-all.
    PreBackup,
    /// After a backup, whether or not it succeeded. `mc-rcon` sends save-on.
    ///
    /// Also never fatal: leaving a live server with saves disabled because a
    /// hook reported failure would be worse than the failure.
    PostBackup,
    /// After a server is installed. `mc-rcon` provisions the password and
    /// enables RCON here.
    PostInstall,
    /// After a server is upgraded.
    PostUpgrade,
}

impl Event {
    pub fn as_str(&self) -> &'static str {
        match self {
            Event::PreStart => "pre-start",
            Event::PreStop => "pre-stop",
            Event::PreBackup => "pre-backup",
            Event::PostBackup => "post-backup",
            Event::PostInstall => "post-install",
            Event::PostUpgrade => "post-upgrade",
        }
    }

    /// Events whose failure must never abort the operation around them.
    ///
    /// A shutdown does not stop because a warning could not be delivered, and a
    /// backup's save-on must run even after the archive failed. A manifest that
    /// marks one of these `fatal` is refused at load time rather than obeyed.
    pub fn may_be_fatal(&self) -> bool {
        !matches!(self, Event::PreStop | Event::PostBackup)
    }
}

impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDecl {
    /// The subcommand name, as typed: `mc rcon`.
    pub name: String,
    /// One-line description for `mc help`.
    #[serde(default)]
    pub about: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HookDecl {
    pub event: Event,
    /// Whether a non-zero exit aborts the operation. Defaults to false —
    /// contributing a step must not be a way to break the core path.
    #[serde(default)]
    pub fatal: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDecl {
    /// Only `source` today: something that can populate a staging directory
    /// from a file the operator names.
    pub kind: String,
    pub name: String,
    /// File extensions this provider claims, without the dot.
    #[serde(default)]
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub abi: u32,
    pub name: String,
    pub bin: PathBuf,
    #[serde(default)]
    pub commands: Vec<CommandDecl>,
    #[serde(default)]
    pub hooks: Vec<HookDecl>,
    #[serde(default)]
    pub providers: Vec<ProviderDecl>,
    /// Filled in by discovery, not by the manifest author.
    #[serde(skip)]
    pub source_file: PathBuf,
}

impl Manifest {
    fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(Error::config("plugin manifest has an empty name"));
        }
        for hook in &self.hooks {
            if hook.fatal && !hook.event.may_be_fatal() {
                return Err(Error::config(format!(
                    "plugin '{}' declares hook '{}' as fatal, which is not allowed: \
                     a failure there must never abort the operation around it",
                    self.name, hook.event
                )));
            }
        }
        for provider in &self.providers {
            if provider.kind != "source" {
                return Err(Error::config(format!(
                    "plugin '{}' declares unknown provider kind '{}'",
                    self.name, provider.kind
                )));
            }
        }
        Ok(())
    }
}

/// Everything installed, plus everything that could not be loaded.
///
/// Problems are CARRIED, not thrown. One bad manifest must not take down every
/// other command — the operator needs `mc status` to keep working while they
/// fix it — but it must also be visible, so `mc plugins` and the dispatcher's
/// "unknown command" path both report it.
#[derive(Debug, Default)]
pub struct Registry {
    plugins: Vec<Manifest>,
    problems: Vec<String>,
}

impl Registry {
    /// Read every `*.toml` in the plugins directory.
    pub fn discover(paths: &Paths) -> Self {
        Self::discover_in(&paths.plugins_dir())
    }

    pub fn discover_in(dir: &Path) -> Self {
        let mut registry = Self::default();

        let Ok(entries) = std::fs::read_dir(dir) else {
            // No directory means no plugins, which is a supported install.
            return registry;
        };
        // Sorted so hook order is deterministic: two plugins on the same event
        // must fire in the same order on every machine, or a bug reproduces on
        // one and not the next.
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        files.sort();

        for file in files {
            match load_manifest(&file) {
                Ok(manifest) => registry.plugins.push(manifest),
                Err(e) => registry.problems.push(e.to_string()),
            }
        }
        registry
    }

    pub fn plugins(&self) -> &[Manifest] {
        &self.plugins
    }

    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// The plugin providing a subcommand, if any.
    pub fn command(&self, name: &str) -> Option<(&Manifest, &CommandDecl)> {
        self.plugins.iter().find_map(|plugin| {
            plugin
                .commands
                .iter()
                .find(|c| c.name == name)
                .map(|c| (plugin, c))
        })
    }

    /// Every subcommand plugins contribute, for help output and completions.
    pub fn commands(&self) -> BTreeMap<&str, &CommandDecl> {
        self.plugins
            .iter()
            .flat_map(|p| p.commands.iter())
            .map(|c| (c.name.as_str(), c))
            .collect()
    }

    /// The source provider claiming a file extension.
    pub fn source_for_extension(&self, extension: &str) -> Option<(&Manifest, &ProviderDecl)> {
        self.plugins.iter().find_map(|plugin| {
            plugin
                .providers
                .iter()
                .find(|p| {
                    p.kind == "source"
                        && p.extensions
                            .iter()
                            .any(|e| e.eq_ignore_ascii_case(extension))
                })
                .map(|p| (plugin, p))
        })
    }

    /// Run every plugin registered for an event.
    ///
    /// Failures are reported and, unless the hook declared itself fatal AND the
    /// event permits it, swallowed. All hooks run even when an earlier one
    /// failed: they are independent contributions, not a pipeline.
    pub fn run_hook(&self, paths: &Paths, event: Event, payload: &serde_json::Value) -> Result<()> {
        let mut fatal_error = None;

        for plugin in &self.plugins {
            let Some(hook) = plugin.hooks.iter().find(|h| h.event == event) else {
                continue;
            };
            match invoke_hook(paths, plugin, event, payload) {
                Ok(()) => {}
                Err(e) => {
                    crate::ui::warn(format!("plugin '{}' hook {event} failed: {e}", plugin.name));
                    if hook.fatal && event.may_be_fatal() && fatal_error.is_none() {
                        fatal_error = Some(Error::other(format!(
                            "plugin '{}' hook {event} failed: {e}",
                            plugin.name
                        )));
                    }
                }
            }
        }

        match fatal_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

fn load_manifest(file: &Path) -> Result<Manifest> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| Error::config(format!("{}: {e}", file.display())))?;
    let mut manifest: Manifest =
        toml::from_str(&text).map_err(|e| Error::config(format!("{}: {e}", file.display())))?;
    manifest.source_file = file.to_path_buf();

    // The ABI gate. A plugin built against a newer core than this one is
    // refused BY NAME, so the message names the package to upgrade rather than
    // leaving an operator with a subcommand that has silently vanished.
    if manifest.abi != ABI {
        return Err(Error::config(format!(
            "{}: plugin '{}' declares ABI {} but this mc implements ABI {ABI}. \
             Upgrade both packages so they match.",
            file.display(),
            manifest.name,
            manifest.abi
        )));
    }
    manifest.validate()?;

    if !manifest.bin.is_file() {
        return Err(Error::config(format!(
            "{}: plugin '{}' points at {}, which is not an executable file.",
            file.display(),
            manifest.name,
            manifest.bin.display()
        )));
    }
    Ok(manifest)
}

/// Environment every plugin invocation carries.
///
/// Paths go through the environment rather than being recompiled into each
/// plugin, so `MC_ROOT` reaches them too and an integration test can drive a
/// plugin against a temp root exactly as it drives core.
fn plugin_env(paths: &Paths) -> Vec<(&'static str, String)> {
    vec![
        ("MC_ABI", ABI.to_string()),
        ("MC_ROOT", paths.root().display().to_string()),
        ("MC_BASE", paths.base().display().to_string()),
        ("MC_CONFIG", paths.config_dir().display().to_string()),
        ("MC_USER", MC_USER.to_string()),
    ]
}

fn invoke_hook(
    paths: &Paths,
    plugin: &Manifest,
    event: Event,
    payload: &serde_json::Value,
) -> Result<()> {
    use std::io::Write as _;

    let mut child = Command::new(&plugin.bin)
        .arg("hook")
        .arg(event.as_str())
        .envs(plugin_env(paths))
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| Error::other(format!("spawning {}: {e}", plugin.bin.display())))?;

    // The payload goes on stdin rather than in argv: it can be large, and argv
    // is world-readable through /proc/<pid>/cmdline — the same reason the RCON
    // password is passed by file.
    if let Some(mut stdin) = child.stdin.take() {
        let body = serde_json::to_vec(payload).unwrap_or_else(|_| b"{}".to_vec());
        let _ = stdin.write_all(&body);
    }

    let status = child
        .wait()
        .map_err(|e| Error::other(format!("waiting for {}: {e}", plugin.bin.display())))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::other(format!("exited with {status}")))
    }
}

/// Hand control to a plugin's subcommand, replacing this process.
///
/// `exec` rather than spawn-and-wait so an interactive plugin — the RCON
/// console is one — owns the terminal outright, and so signals reach it
/// directly instead of through a shim that would have to forward them.
pub fn exec_command(paths: &Paths, plugin: &Manifest, args: &[String]) -> Error {
    use std::os::unix::process::CommandExt as _;
    let err = Command::new(&plugin.bin)
        .arg("command")
        .args(args)
        .envs(plugin_env(paths))
        .exec();
    Error::other(format!("could not run {}: {err}", plugin.bin.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest plus a real executable for it to point at.
    fn plugin_dir() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("plugins.d");
        let bin = dir.path().join("mc-fixture");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        crate::fsx::apply_owner_mode(&bin, None, 0o755).unwrap();
        (dir, plugins, bin)
    }

    fn write_manifest(plugins: &Path, name: &str, body: &str) {
        std::fs::write(plugins.join(format!("{name}.toml")), body).unwrap();
    }

    #[test]
    fn discovers_a_well_formed_plugin() {
        let (_d, plugins, bin) = plugin_dir();
        write_manifest(
            &plugins,
            "rcon",
            &format!(
                r#"
                abi = 1
                name = "rcon"
                bin = "{}"
                [[commands]]
                name = "rcon"
                about = "Open an RCON console"
                [[hooks]]
                event = "pre-stop"
                "#,
                bin.display()
            ),
        );

        let registry = Registry::discover_in(&plugins);
        assert!(registry.problems().is_empty(), "{:?}", registry.problems());
        assert_eq!(registry.plugins().len(), 1);
        assert!(registry.command("rcon").is_some());
        assert!(registry.command("backup").is_none());
    }

    #[test]
    fn an_unknown_abi_is_refused_by_name_without_disabling_anything_else() {
        let (_d, plugins, bin) = plugin_dir();
        write_manifest(
            &plugins,
            "future",
            &format!("abi = 99\nname = \"future\"\nbin = \"{}\"\n", bin.display()),
        );
        write_manifest(
            &plugins,
            "rcon",
            &format!(
                "abi = 1\nname = \"rcon\"\nbin = \"{}\"\n[[commands]]\nname = \"rcon\"\n",
                bin.display()
            ),
        );

        let registry = Registry::discover_in(&plugins);
        assert_eq!(registry.problems().len(), 1);
        let problem = registry.problems().first().unwrap();
        assert!(
            problem.contains("future"),
            "names the offending plugin: {problem}"
        );
        assert!(problem.contains("ABI 99"), "{problem}");
        // The healthy plugin is unaffected.
        assert!(registry.command("rcon").is_some());
    }

    #[test]
    fn a_manifest_pointing_at_a_missing_binary_is_refused() {
        // Catches a packaging slip — a manifest shipped without its executable,
        // or a path that drifted — at discovery rather than at first use.
        let (_d, plugins, _bin) = plugin_dir();
        write_manifest(
            &plugins,
            "broken",
            "abi = 1\nname = \"broken\"\nbin = \"/nonexistent/mc-broken\"\n",
        );

        let registry = Registry::discover_in(&plugins);
        assert_eq!(registry.plugins().len(), 0);
        let problem = registry.problems().first().unwrap();
        assert!(problem.contains("not an executable file"), "{problem}");
    }

    #[test]
    fn a_typo_in_an_event_name_is_an_error_not_a_silent_no_op() {
        let (_d, plugins, bin) = plugin_dir();
        write_manifest(
            &plugins,
            "typo",
            &format!(
                "abi = 1\nname = \"typo\"\nbin = \"{}\"\n[[hooks]]\nevent = \"pre-stopp\"\n",
                bin.display()
            ),
        );

        let registry = Registry::discover_in(&plugins);
        assert_eq!(registry.plugins().len(), 0);
        assert_eq!(registry.problems().len(), 1);
    }

    #[test]
    fn pre_stop_may_not_declare_itself_fatal() {
        // A shutdown must never abort because a warning could not be delivered.
        let (_d, plugins, bin) = plugin_dir();
        write_manifest(
            &plugins,
            "greedy",
            &format!(
                "abi = 1\nname = \"greedy\"\nbin = \"{}\"\n[[hooks]]\nevent = \"pre-stop\"\nfatal = true\n",
                bin.display()
            ),
        );

        let registry = Registry::discover_in(&plugins);
        let problem = registry.problems().first().unwrap();
        assert!(problem.contains("not allowed"), "{problem}");
    }

    #[test]
    fn post_backup_may_not_declare_itself_fatal_either() {
        // save-on has to run even after the archive failed.
        let (_d, plugins, bin) = plugin_dir();
        write_manifest(
            &plugins,
            "greedy",
            &format!(
                "abi = 1\nname = \"greedy\"\nbin = \"{}\"\n[[hooks]]\nevent = \"post-backup\"\nfatal = true\n",
                bin.display()
            ),
        );
        assert_eq!(Registry::discover_in(&plugins).plugins().len(), 0);
    }

    #[test]
    fn discovery_order_is_deterministic() {
        // Two plugins on one event must fire in the same order on every
        // machine, or a bug reproduces on one and not the next. readdir order
        // is not sorted.
        let (_d, plugins, bin) = plugin_dir();
        for name in ["zzz", "aaa", "mmm"] {
            write_manifest(
                &plugins,
                name,
                &format!("abi = 1\nname = \"{name}\"\nbin = \"{}\"\n", bin.display()),
            );
        }
        let registry = Registry::discover_in(&plugins);
        let names: Vec<&str> = registry.plugins().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["aaa", "mmm", "zzz"]);
    }

    #[test]
    fn no_plugins_directory_is_a_supported_install() {
        let registry = Registry::discover_in(Path::new("/nonexistent/plugins.d"));
        assert!(registry.plugins().is_empty());
        assert!(registry.problems().is_empty());
    }

    #[test]
    fn a_source_provider_claims_its_extension() {
        let (_d, plugins, bin) = plugin_dir();
        write_manifest(
            &plugins,
            "mrpack",
            &format!(
                r#"
                abi = 1
                name = "mrpack"
                bin = "{}"
                [[providers]]
                kind = "source"
                name = "mrpack"
                extensions = ["mrpack"]
                "#,
                bin.display()
            ),
        );

        let registry = Registry::discover_in(&plugins);
        assert!(registry.source_for_extension("mrpack").is_some());
        // Case-insensitive: an operator types whatever the file is named.
        assert!(registry.source_for_extension("MRPACK").is_some());
        assert!(registry.source_for_extension("zip").is_none());
    }

    #[test]
    fn an_unknown_provider_kind_is_refused() {
        let (_d, plugins, bin) = plugin_dir();
        write_manifest(
            &plugins,
            "odd",
            &format!(
                "abi = 1\nname = \"odd\"\nbin = \"{}\"\n[[providers]]\nkind = \"transport\"\nname = \"x\"\n",
                bin.display()
            ),
        );
        assert_eq!(Registry::discover_in(&plugins).plugins().len(), 0);
    }
}
