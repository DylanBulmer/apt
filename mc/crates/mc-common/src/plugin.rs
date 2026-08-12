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

/// The environment variables carrying hook nesting state across the process
/// boundary.
///
/// A plugin is free to run `mc` again — `mc-backup`'s own subcommand does — and
/// that re-entry dispatches hooks of its own. Nothing in a single process can
/// see that, so the state travels in the environment, which every descendant
/// inherits whether it is a hook, a probe or a plugin subcommand.
pub const HOOK_DEPTH_ENV: &str = "MC_HOOK_DEPTH";
pub const HOOK_CHAIN_ENV: &str = "MC_HOOK_CHAIN";

/// How many levels of hook dispatch may nest before dispatch is refused.
///
/// One level is the normal case; two covers a hook that legitimately drives a
/// core operation with hooks of its own (a `post-install` that takes a backup).
/// Anything deeper is a plugin calling back into the operation that invoked it,
/// which does not terminate on its own: on the shutdown path it burns through
/// `TimeoutStopSec` until the JVM is SIGKILLed mid-flush, and every level costs
/// another fork.
pub const MAX_HOOK_DEPTH: u32 = 2;

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
    /// Before the server is told to stop. The elected console runs the player
    /// countdown here.
    ///
    /// MAY NEVER BE FATAL — see [`HookDecl::fatal`].
    PreStop,
    /// Before a backup archives the world. The elected console pauses saving
    /// and flushes.
    PreBackup,
    /// After a backup, whether or not it succeeded. The elected console turns
    /// saving back on.
    ///
    /// Also never fatal: leaving a live server with saves disabled because a
    /// hook reported failure would be worse than the failure.
    PostBackup,
    /// After a server is installed. A console provisions its own credentials
    /// here — EVERY installed console, not just the elected one, so that
    /// losing an election does not leave a machine with nothing that works.
    PostInstall,
    /// After a server is upgraded.
    PostUpgrade,
}

impl Event {
    /// Every event, for anything that has to enumerate the contract rather
    /// than react to one instance of it — `mc-plugins(5)` is checked against
    /// this list, so an event added here and documented nowhere fails a test.
    pub const ALL: [Event; 6] = [
        Event::PreStart,
        Event::PreStop,
        Event::PreBackup,
        Event::PostBackup,
        Event::PostInstall,
        Event::PostUpgrade,
    ];

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

    /// Events where two consoles doing the work would be worse than one doing
    /// it.
    ///
    /// A second countdown announces a shutdown that already has a schedule,
    /// and a second save-off/save-on pair can turn saving back on halfway
    /// through another console's archive. Only the elected console runs these
    /// — see [`Registry::console`].
    ///
    /// Provisioning events are deliberately NOT here. Every installed console
    /// keeps itself configured and ready, so losing one election does not
    /// leave a machine with no working console the day the winner's probe
    /// starts failing.
    pub fn is_console_exclusive(&self) -> bool {
        matches!(self, Event::PreStop | Event::PreBackup | Event::PostBackup)
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

/// Provider kinds core knows how to use.
///
/// An unknown kind is REPORTED AND IGNORED rather than refused: a plugin built
/// against a newer core must still contribute the parts this one understands,
/// or every future kind becomes a flag day for the whole plugin set. The
/// report is what keeps a typo (`kind = "sauce"`) from being silently inert.
pub const PROVIDER_KINDS: [&str; 2] = ["source", "console"];

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDecl {
    /// `source` — populates a staging directory from a file the operator
    /// names. `console` — talks to the running server.
    pub kind: String,
    pub name: String,
    /// File extensions a `source` provider claims, without the dot.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// For `console`: which provider wins when more than one is installed.
    ///
    /// Higher is better, and the winner must still pass its own probe — see
    /// [`Registry::console`]. Existing consoles sit at 10, so a new one that
    /// should take precedence declares 20 rather than renumbering anything.
    #[serde(default)]
    pub priority: i32,
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
        Ok(())
    }

    /// Provider kinds this core does not implement.
    ///
    /// Not an error — see [`PROVIDER_KINDS`]. Discovery turns each of these
    /// into a reported problem while keeping the plugin's commands, hooks and
    /// understood providers working.
    fn unknown_provider_kinds(&self) -> Vec<String> {
        self.providers
            .iter()
            .filter(|p| !PROVIDER_KINDS.contains(&p.kind.as_str()))
            .map(|p| {
                format!(
                    "{}: plugin '{}' declares provider kind '{}', which this mc does not \
                     implement — that provider is ignored.",
                    self.source_file.display(),
                    self.name,
                    p.kind
                )
            })
            .collect()
    }

    /// This plugin's console provider, if it declares one.
    pub fn console_provider(&self) -> Option<&ProviderDecl> {
        self.providers.iter().find(|p| p.kind == "console")
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
    /// The elected console, resolved at most once per process.
    ///
    /// Electing means probing a plugin, which is a fork and exec: a backup
    /// dispatches two console events and a shutdown one more, and none of them
    /// should pay for it twice.
    elected: std::cell::OnceCell<Option<String>>,
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
                Ok(manifest) => {
                    registry.problems.extend(manifest.unknown_provider_kinds());
                    registry.plugins.push(manifest);
                }
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

    /// The console this machine should use, or `None` if nothing can talk to
    /// the server.
    ///
    /// The highest-priority console whose probe succeeds. The probe is what
    /// makes the answer depend on the server rather than on what is installed:
    /// a console for a protocol this server's version does not implement
    /// reports itself unusable and the next one down takes over, with no
    /// package conflict and nothing in core naming either of them.
    pub fn console(&self, paths: &Paths) -> Option<&Manifest> {
        let name = self.elected.get_or_init(|| self.elect(paths)).as_deref()?;
        self.plugins.iter().find(|p| p.name == name)
    }

    fn elect(&self, paths: &Paths) -> Option<String> {
        let mut candidates: Vec<(&Manifest, i32)> = self
            .plugins
            .iter()
            .filter_map(|p| p.console_provider().map(|c| (p, c.priority)))
            .collect();
        // Highest priority first. The sort is stable and discovery is by
        // filename, so two consoles at the same priority still elect the same
        // one on every machine.
        candidates.sort_by_key(|&(_, priority)| std::cmp::Reverse(priority));

        candidates
            .into_iter()
            .find(|(plugin, _)| probe(paths, plugin))
            .map(|(plugin, _)| plugin.name.clone())
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

        // Where this dispatch sits in a chain that may already have crossed
        // several process boundaries — a plugin is free to invoke `mc`, and
        // that re-entry lands back here.
        let chain = HookChain::from_env();

        // Resolved only for the events that need it: electing costs a probe
        // per console, and `post-install` has no reason to pay for one.
        let elected = event
            .is_console_exclusive()
            .then(|| self.console(paths).map(|p| p.name.as_str()))
            .flatten();

        for plugin in &self.plugins {
            let Some(hook) = plugin.hooks.iter().find(|h| h.event == event) else {
                continue;
            };
            // A console that lost the election stands down for this event
            // rather than announcing a second countdown over the winner's.
            if event.is_console_exclusive()
                && plugin.console_provider().is_some()
                && elected != Some(plugin.name.as_str())
            {
                continue;
            }
            // A loop is skipped, never fatal, and never propagated — not even
            // for an event that permits a fatal hook. Aborting a shutdown or an
            // install because a plugin called back into it leaves the operator
            // worse off than the missing step does, and the warning names the
            // plugin, the event and the chain so they can see what looped.
            if let Some(reason) = chain.refusal(&plugin.name, event) {
                crate::ui::warn(format!(
                    "skipping plugin '{}' hook {event}: {reason}. Chain: {}",
                    plugin.name,
                    chain.describe()
                ));
                continue;
            }
            match invoke_hook(paths, plugin, event, payload, &chain) {
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

/// Where this process sits in a chain of hook dispatches.
///
/// `depth` bounds recursion that grows without repeating itself; `links` — the
/// `plugin:event` pairs already active — catches the shorter and more likely
/// case, two plugins whose hooks trigger each other's events and ping-pong
/// forever at a depth that never exceeds the limit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookChain {
    depth: u32,
    links: Vec<String>,
}

impl HookChain {
    /// Read the state this process inherited.
    ///
    /// A missing or malformed value is depth 0 with an empty chain: the
    /// variables are ours, so the only way to see a bad one is an operator
    /// setting it by hand, and refusing every hook over that would break a
    /// shutdown far more surely than the loop the guard exists to stop.
    pub fn from_env() -> Self {
        let depth = std::env::var(HOOK_DEPTH_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(0);
        let links = std::env::var(HOOK_CHAIN_ENV)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        Self { depth, links }
    }

    fn link(plugin: &str, event: Event) -> String {
        format!("{plugin}:{event}")
    }

    /// Why dispatching this hook would loop, or `None` if it is safe.
    fn refusal(&self, plugin: &str, event: Event) -> Option<String> {
        if self.links.contains(&Self::link(plugin, event)) {
            return Some(format!(
                "plugin '{plugin}' hook {event} is already running further up this chain"
            ));
        }
        if self.depth >= MAX_HOOK_DEPTH {
            return Some(format!(
                "hook dispatch is already {} levels deep (limit {MAX_HOOK_DEPTH})",
                self.depth
            ));
        }
        None
    }

    /// The state a hook's own child processes inherit.
    fn enter(&self, plugin: &str, event: Event) -> Self {
        let mut links = self.links.clone();
        links.push(Self::link(plugin, event));
        Self {
            depth: self.depth.saturating_add(1),
            links,
        }
    }

    /// The chain as an operator reads it in a warning.
    pub fn describe(&self) -> String {
        if self.links.is_empty() {
            "(none)".to_string()
        } else {
            self.links.join(" > ")
        }
    }

    fn env(&self) -> [(&'static str, String); 2] {
        [
            (HOOK_DEPTH_ENV, self.depth.to_string()),
            (HOOK_CHAIN_ENV, self.links.join(",")),
        ]
    }
}

/// Environment every plugin invocation carries.
///
/// Paths go through the environment rather than being recompiled into each
/// plugin, so `MC_ROOT` reaches them too and an integration test can drive a
/// plugin against a temp root exactly as it drives core.
///
/// The chain goes the same way, and is PASSED THROUGH unchanged for anything
/// that is not itself a hook — a probe, or a plugin subcommand invoked from
/// inside a hook. Resetting it there would hand a plugin a fresh budget simply
/// by shelling out to `mc`, which is the loop this guard is for.
fn plugin_env(paths: &Paths, chain: &HookChain) -> Vec<(&'static str, String)> {
    let mut env = vec![
        ("MC_ABI", ABI.to_string()),
        ("MC_ROOT", paths.root().display().to_string()),
        ("MC_BASE", paths.base().display().to_string()),
        ("MC_CONFIG", paths.config_dir().display().to_string()),
        ("MC_USER", MC_USER.to_string()),
    ];
    env.extend(chain.env());
    env
}

/// How long a console gets to answer `console probe`.
///
/// Bounded because this runs on the shutdown path: a console whose endpoint is
/// black-holed must cost a couple of seconds, not the unit's whole
/// `TimeoutStopSec`. Both probes together fit inside the safety buffer that
/// value already carries.
pub const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

/// Ask a console whether it can talk to THIS server, right now.
///
/// Anything other than a clean exit 0 inside the deadline is "no": a missing
/// binary, a protocol the server's version does not implement, an endpoint
/// that is switched off, a hang. The caller then tries the next console down,
/// so being wrong here costs a fallback rather than a failure.
fn probe(paths: &Paths, plugin: &Manifest) -> bool {
    let Ok(mut child) = Command::new(&plugin.bin)
        .arg("console")
        .arg("probe")
        .envs(plugin_env(paths, &HookChain::from_env()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
    else {
        return false;
    };

    let deadline = std::time::Instant::now() + PROBE_DEADLINE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => return false,
        }
        if std::time::Instant::now() >= deadline {
            // Reaped as well as killed: an unreaped child of `mc serve` would
            // sit in the process table for the lifetime of the server.
            let _ = child.kill();
            let _ = child.wait();
            crate::ui::warn(format!(
                "console '{}' did not answer its probe within {}s; trying the next one",
                plugin.name,
                PROBE_DEADLINE.as_secs()
            ));
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// How long one hook gets to finish before it is killed.
///
/// `TimeoutStopSec=380s` has to cover the whole stop path — the console
/// election, the 300 s countdown with its announcements, the stop command and
/// the chunk-flush sleep, 358 s worst case with a 22 s buffer. The elected
/// console's `pre-stop` hook IS most of that path, so this bound cannot be a
/// handful of seconds: it must clear the 355 s the hook legitimately spends
/// once the 3 s election outside it is subtracted. 360 s does, and still leaves
/// the unit's buffer for `mc shutdown` to report the kill and exit, so a hook
/// that black-holes costs a bounded overrun instead of every second systemd
/// has before it SIGKILLs the JVM mid-chunk-flush.
pub const HOOK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(360);

fn invoke_hook(
    paths: &Paths,
    plugin: &Manifest,
    event: Event,
    payload: &serde_json::Value,
    chain: &HookChain,
) -> Result<()> {
    let mut child = Command::new(&plugin.bin)
        .arg("hook")
        .arg(event.as_str())
        .envs(plugin_env(paths, &chain.enter(&plugin.name, event)))
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| Error::other(format!("spawning {}: {e}", plugin.bin.display())))?;

    // The payload goes on stdin rather than in argv: it can be large, and argv
    // is world-readable through /proc/<pid>/cmdline — the same reason the RCON
    // password is passed by file.
    //
    // Handed to a thread that drops the pipe when it is done, because a plugin
    // that never drains its stdin blocks the write once the pipe buffer fills.
    // Writing inline would spend that time BEFORE the deadline loop starts, so
    // the hang the deadline exists to bound would happen outside it. The thread
    // needs no join: killing the child closes the read end, so the write fails
    // rather than outliving the wait.
    if let Some(mut stdin) = child.stdin.take() {
        let body = serde_json::to_vec(payload).unwrap_or_else(|_| b"{}".to_vec());
        std::thread::spawn(move || {
            use std::io::Write as _;
            let _ = stdin.write_all(&body);
        });
    }

    let deadline = std::time::Instant::now() + HOOK_DEADLINE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(Error::other(format!("exited with {status}"))),
            Ok(None) => {}
            Err(e) => {
                return Err(Error::other(format!(
                    "waiting for {}: {e}",
                    plugin.bin.display()
                )));
            }
        }
        if std::time::Instant::now() >= deadline {
            // Reaped as well as killed: an unreaped child of `mc serve` would
            // sit in the process table for the lifetime of the server.
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::other(format!(
                "plugin '{}' hook {event} exceeded the {}s deadline and was killed",
                plugin.name,
                HOOK_DEADLINE.as_secs()
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
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
        .envs(plugin_env(paths, &HookChain::from_env()))
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
    fn an_unknown_provider_kind_is_reported_but_costs_the_plugin_nothing_else() {
        // A plugin built against a newer core keeps working here, minus the
        // part this core cannot use. Refusing the whole manifest instead would
        // make every future provider kind a flag day: the operator would lose
        // the plugin's commands and hooks too, over a provider they were not
        // relying on yet.
        let (_d, plugins, bin) = plugin_dir();
        write_manifest(
            &plugins,
            "odd",
            &format!(
                "abi = 1\nname = \"odd\"\nbin = \"{}\"\n\
                 [[commands]]\nname = \"odd\"\n\
                 [[providers]]\nkind = \"transport\"\nname = \"x\"\n",
                bin.display()
            ),
        );

        let registry = Registry::discover_in(&plugins);
        assert_eq!(registry.plugins().len(), 1, "the plugin still loads");
        assert!(registry.command("odd").is_some(), "its commands still work");
        assert!(
            registry.problems().iter().any(|p| p.contains("transport")),
            "the kind it could not use is named: {:?}",
            registry.problems()
        );
    }

    /// A console fixture: answers `console probe` with `probe_exit`, and logs
    /// every other invocation so a suppressed hook is visible by its absence.
    fn console_bin(dir: &Path, name: &str, probe_exit: u8) -> PathBuf {
        let bin = dir.join(format!("{name}-bin"));
        let log = dir.join(format!("{name}.log"));
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\n\
                 [ \"$1\" = console ] && exit {probe_exit}\n\
                 echo \"$@\" >> {}\n\
                 exit 0\n",
                log.display()
            ),
        )
        .unwrap();
        crate::fsx::apply_owner_mode(&bin, None, 0o755).unwrap();
        bin
    }

    #[test]
    fn the_highest_priority_console_that_answers_its_probe_is_elected() {
        let (_d, plugins, _) = plugin_dir();
        let paths = Paths::with_root(plugins.parent().unwrap().parent().unwrap());

        for (name, priority, exit) in [("rcon", 10, 0u8), ("mgmt", 20, 0u8)] {
            let bin = console_bin(&plugins, name, exit);
            write_manifest(
                &plugins,
                name,
                &format!(
                    "abi = 1\nname = \"{name}\"\nbin = \"{}\"\n\
                     [[providers]]\nkind = \"console\"\nname = \"{name}\"\npriority = {priority}\n",
                    bin.display()
                ),
            );
        }

        let registry = Registry::discover_in(&plugins);
        assert_eq!(
            registry.console(&paths).map(|p| p.name.as_str()),
            Some("mgmt")
        );
    }

    #[test]
    fn a_console_that_cannot_reach_the_server_loses_to_one_that_can() {
        // The whole point of probing rather than ranking statically: mc-mgmt
        // outranks mc-rcon, but on a server too old to speak its protocol it
        // must stand aside rather than leave the machine with no console.
        let (_d, plugins, _) = plugin_dir();
        let paths = Paths::with_root(plugins.parent().unwrap().parent().unwrap());

        for (name, priority, exit) in [("rcon", 10, 0u8), ("mgmt", 20, 1u8)] {
            let bin = console_bin(&plugins, name, exit);
            write_manifest(
                &plugins,
                name,
                &format!(
                    "abi = 1\nname = \"{name}\"\nbin = \"{}\"\n\
                     [[providers]]\nkind = \"console\"\nname = \"{name}\"\npriority = {priority}\n",
                    bin.display()
                ),
            );
        }

        let registry = Registry::discover_in(&plugins);
        assert_eq!(
            registry.console(&paths).map(|p| p.name.as_str()),
            Some("rcon")
        );
    }

    #[test]
    fn only_the_elected_console_runs_a_console_exclusive_hook() {
        // The property the whole election exists for: with two consoles
        // installed, players are warned once. Twice would be worse than not at
        // all — the second countdown contradicts the first.
        let (_d, plugins, _) = plugin_dir();
        let paths = Paths::with_root(plugins.parent().unwrap().parent().unwrap());

        for (name, priority) in [("rcon", 10), ("mgmt", 20)] {
            let bin = console_bin(&plugins, name, 0);
            write_manifest(
                &plugins,
                name,
                &format!(
                    "abi = 1\nname = \"{name}\"\nbin = \"{}\"\n\
                     [[hooks]]\nevent = \"pre-stop\"\n\
                     [[hooks]]\nevent = \"post-install\"\n\
                     [[providers]]\nkind = \"console\"\nname = \"{name}\"\npriority = {priority}\n",
                    bin.display()
                ),
            );
        }

        let registry = Registry::discover_in(&plugins);
        registry
            .run_hook(&paths, Event::PreStop, &serde_json::json!({}))
            .unwrap();

        let log = |name: &str| {
            std::fs::read_to_string(plugins.join(format!("{name}.log"))).unwrap_or_default()
        };
        assert!(log("mgmt").contains("hook pre-stop"), "the winner acts");
        assert!(
            !log("rcon").contains("hook pre-stop"),
            "the loser stands down: {}",
            log("rcon")
        );

        // But provisioning is not exclusive: both consoles keep themselves
        // ready, so the day mgmt's probe starts failing rcon still works.
        registry
            .run_hook(&paths, Event::PostInstall, &serde_json::json!({}))
            .unwrap();
        assert!(log("rcon").contains("hook post-install"));
        assert!(log("mgmt").contains("hook post-install"));
    }

    #[test]
    fn a_plugin_that_is_not_a_console_is_never_suppressed() {
        // mc-backup declares no console provider, so the election has nothing
        // to say about it — its hooks run regardless of who won.
        let (_d, plugins, bin) = plugin_dir();
        let paths = Paths::with_root(plugins.parent().unwrap().parent().unwrap());

        let console = console_bin(&plugins, "mgmt", 0);
        write_manifest(
            &plugins,
            "mgmt",
            &format!(
                "abi = 1\nname = \"mgmt\"\nbin = \"{}\"\n\
                 [[providers]]\nkind = \"console\"\nname = \"mgmt\"\npriority = 20\n",
                console.display()
            ),
        );
        write_manifest(
            &plugins,
            "other",
            &format!(
                "abi = 1\nname = \"other\"\nbin = \"{}\"\n[[hooks]]\nevent = \"pre-backup\"\n",
                bin.display()
            ),
        );

        Registry::discover_in(&plugins)
            .run_hook(&paths, Event::PreBackup, &serde_json::json!({}))
            .unwrap();
    }

    // The chain tests below build a `HookChain` directly rather than through
    // `from_env`. Reading is what production does; WRITING the variables here
    // would set them for every other test in this binary, several of which
    // dispatch hooks concurrently and would start refusing them. The parsing
    // half is pinned end-to-end instead, by a real nested dispatch in
    // `crates/mc/tests/hook_loops.rs`.

    #[test]
    fn the_chain_refuses_a_plugin_and_event_it_is_already_running() {
        let chain = HookChain::default().enter("rcon", Event::PreStop);

        let reason = chain.refusal("rcon", Event::PreStop).unwrap();
        assert!(
            reason.contains("already running further up this chain"),
            "{reason}"
        );
        assert!(reason.contains("rcon"), "names the plugin: {reason}");
        assert!(reason.contains("pre-stop"), "names the event: {reason}");

        // A link is a plugin AND an event: a plugin whose pre-stop is running
        // has no reason to be barred from a pre-backup it has not entered, and
        // another plugin's pre-stop is not this one's.
        assert!(chain.refusal("rcon", Event::PreBackup).is_none());
        assert!(chain.refusal("backup", Event::PreStop).is_none());
    }

    #[test]
    fn the_chain_refuses_everything_once_it_reaches_the_depth_limit() {
        // Recursion that never repeats a plugin:event pair — a hook that
        // installs a plugin that hooks the install — is bounded by depth
        // alone, and the bound applies to plugins the chain has never seen.
        let mut chain = HookChain::default();
        for level in 0..MAX_HOOK_DEPTH {
            chain = chain.enter(&format!("plugin{level}"), Event::PostInstall);
        }

        let reason = chain.refusal("never-seen", Event::PreStart).unwrap();
        assert!(
            reason.contains(&format!("limit {MAX_HOOK_DEPTH}")),
            "{reason}"
        );

        // One level short of the limit still dispatches: nesting is bounded,
        // not forbidden.
        let shallower = HookChain::default().enter("plugin0", Event::PostInstall);
        assert!(shallower.refusal("never-seen", Event::PreStart).is_none());
    }

    #[test]
    fn the_environment_a_child_inherits_carries_the_depth_and_every_link() {
        let chain = HookChain::default()
            .enter("rcon", Event::PreStop)
            .enter("backup", Event::PreBackup);

        assert_eq!(
            chain.env(),
            [
                (HOOK_DEPTH_ENV, "2".to_string()),
                (
                    HOOK_CHAIN_ENV,
                    "rcon:pre-stop,backup:pre-backup".to_string()
                ),
            ]
        );
        // What an operator reads in the warning that names the loop.
        assert_eq!(chain.describe(), "rcon:pre-stop > backup:pre-backup");
        assert_eq!(HookChain::default().describe(), "(none)");
    }

    #[test]
    fn every_plugin_invocation_carries_the_nesting_state() {
        // Hooks, `console probe` and plugin subcommands all build their
        // environment here, so a plugin cannot be handed a fresh budget simply
        // by being invoked for something that is not itself a hook.
        let paths = Paths::with_root("/tmp/sandbox");
        let env = plugin_env(&paths, &HookChain::default().enter("rcon", Event::PreStop));

        let value = |key: &str| {
            env.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.as_str())
                .unwrap()
        };
        assert_eq!(value(HOOK_DEPTH_ENV), "1");
        assert_eq!(value(HOOK_CHAIN_ENV), "rcon:pre-stop");
    }

    #[test]
    fn an_empty_chain_still_sets_both_variables() {
        // Set-but-empty, not absent: a plugin reading an unset variable cannot
        // tell "no chain" from "this core is too old to track one".
        let env = plugin_env(&Paths::with_root("/tmp/sandbox"), &HookChain::default());
        assert!(env.iter().any(|(k, v)| *k == HOOK_DEPTH_ENV && v == "0"));
        assert!(
            env.iter()
                .any(|(k, v)| *k == HOOK_CHAIN_ENV && v.is_empty())
        );
    }

    #[test]
    fn no_console_answers_and_nothing_is_elected() {
        let (_d, plugins, _) = plugin_dir();
        let paths = Paths::with_root(plugins.parent().unwrap().parent().unwrap());

        let bin = console_bin(&plugins, "rcon", 1);
        write_manifest(
            &plugins,
            "rcon",
            &format!(
                "abi = 1\nname = \"rcon\"\nbin = \"{}\"\n\
                 [[providers]]\nkind = \"console\"\nname = \"rcon\"\npriority = 10\n",
                bin.display()
            ),
        );

        // Not an error: a server with no console still stops, backs up and
        // installs — it just does so without warning players first.
        assert!(Registry::discover_in(&plugins).console(&paths).is_none());
    }
}
