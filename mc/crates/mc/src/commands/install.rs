//! `mc install` and `mc upgrade`.
//!
//! The two share [`install_artifact`] so they cannot drift about where a jar
//! comes from or how it lands. What differs is the safety around it: upgrade
//! takes a backup and stops the server first; install refuses to run over an
//! existing one at all.

use std::path::Path;

use mc_common::config::ServerType;
use mc_common::error::{Error, IoContext, Result};
use mc_common::paths::{MC_USER, SERVICE_UNIT};
use mc_common::plugin::{Event, Registry};
use mc_common::staging::Staging;
use mc_common::{Config, eula, java, properties, ui};

use crate::context::Ctx;
use crate::sources::{self, FetchCtx, Layout};

pub struct InstallArgs {
    pub server_type: Option<ServerType>,
    pub version: Option<String>,
    /// A pack file, handled by a plugin-provided source provider.
    pub pack: Option<std::path::PathBuf>,
    pub assume_yes: bool,
    pub accept_eula: bool,
    pub force: bool,
}

pub struct UpgradeArgs {
    pub server_type: Option<ServerType>,
    pub version: Option<String>,
    pub pack: Option<std::path::PathBuf>,
    pub assume_yes: bool,
    pub force: bool,
    /// Proceed without a pre-upgrade backup.
    ///
    /// Required when no backup provider is installed. The shell version could
    /// assume one was always there because backup lived in the same package;
    /// now that `mc-backup` is separately removable, "no backup was taken" has
    /// to be a decision rather than a silent consequence of a missing package.
    pub no_backup: bool,
}

pub fn install(ctx: &Ctx, args: InstallArgs) -> Result<()> {
    let mut cfg = Config::load(&ctx.paths)?;
    if let Some(server_type) = args.server_type {
        cfg.server.server_type = server_type;
    }
    if let Some(version) = &args.version {
        mc_common::version::validate(version, "Minecraft version")?;
        cfg.server.version = version.clone();
    }

    let _lock = mc_common::lock::acquire(&ctx.paths.lock_file())?;

    // Installing over a live server overwrites server.jar and repins the
    // version, with none of the protections upgrade has — no backup, no
    // graceful stop. Every other mutating command guards on "a server exists";
    // this is its inverse.
    if !args.force && ctx.paths.server_installed() {
        return Err(Error::config(format!(
            "A server is already installed in {}.\n\
             To change version:  mc upgrade [--version VER]\n\
             To reinstall over it (overwrites server.jar, no backup taken):\n\
             \x20                   mc install --force",
            ctx.paths.base().display()
        )));
    }

    // --accept-eula auto-populates eula.txt as a convenience, but install
    // proceeds regardless.
    if args.accept_eula {
        eula::accept(&ctx.paths)?;
    }

    std::fs::create_dir_all(ctx.paths.base()).at(ctx.paths.base())?;

    if let Some(pack) = &args.pack {
        crate::commands::pack::install_from_pack(ctx, pack, &mut cfg)?;
    } else {
        let resolved = install_artifact(ctx, &cfg)?;
        cfg.server.version = resolved;
    }
    cfg.save(&ctx.paths)?;

    // AHEAD OF THE PROPERTIES WRITE. `managed_value` derives enable-rcon and
    // rcon.password from whether a password file exists, so whatever provisions
    // one has to have run first — that is mc-rcon's `post-install` hook. Run it
    // in the other order and RCON comes out disabled on a fresh install, and
    // only switches on at the next one.
    run_hook(ctx, Event::PostInstall);

    if !ctx.paths.server_properties().exists() {
        properties::init(&ctx.paths)?;
    }

    let java_major = cfg.java_major();
    ensure_java(ctx, java_major, args.assume_yes)?;

    // Before initialise_settings, not after: that step runs the JVM as
    // $MC_USER, which must be able to write here. Anything root created in
    // MC_BASE up to this point is corrected by it.
    chown_tree(&ctx.paths.base())?;
    initialise_settings(ctx, java_major);

    ui::info(format!(
        "Installed {} {}",
        cfg.server.server_type, cfg.server.version
    ));
    ui::info(format!(
        "Review {} before the first start —",
        ctx.paths.server_properties().display()
    ));
    ui::info("the world is generated from it, and level-seed is fixed once it is.");
    ui::info("Start with: systemctl enable --now minecraft");
    Ok(())
}

pub fn upgrade(ctx: &Ctx, args: UpgradeArgs) -> Result<()> {
    let mut cfg = Config::load(&ctx.paths)?;
    crate::commands::lifecycle::require_server(ctx)?;

    // A modpack server needs a new pack, not a bare version bump — that would
    // replace the jar and strip the mods.
    if ctx.paths.mrpack_manifest().exists() && args.pack.is_none() {
        return Err(Error::config(
            "This server was installed from a .mrpack file. Provide a new .mrpack to upgrade: mc upgrade <new.mrpack>",
        ));
    }

    if let Some(version) = &args.version {
        mc_common::version::validate(version, "Minecraft version")?;
    }
    // A BARE `mc upgrade` TARGETS "latest", not the pinned version.
    //
    // `server.version` records what is INSTALLED — install resolves "latest"
    // and pins the concrete result so a later upgrade can tell whether anything
    // moved. Reading it back as the *request* conflates the two, and makes a
    // bare `mc upgrade` resolve the pin to itself and report "nothing to
    // upgrade" forever. The shell implementation did exactly that, so on a
    // vanilla or NeoForge server `mc upgrade` could never move a version
    // without `--version`, contradicting its own documentation.
    let target = args.version.clone().unwrap_or_else(|| "latest".to_string());

    // The target type is the explicitly requested one, or the currently
    // installed one. `--type` lets an operator switch from vanilla to fabric
    // in a single upgrade, keeping the backup and graceful stop.
    let target_type = args.server_type.unwrap_or(cfg.server.server_type);

    // Decide whether there is anything to do BEFORE the backup and the stop.
    // Those are the expensive parts — a full archive of the world, then
    // downtime a populated server stretches to five minutes — and an upgrade
    // run on a schedule lands here with nothing to do most times it fires.
    // A pack is always installed: it pins every mod as well as the version, so
    // there is nothing cheaper to compare against.
    if args.pack.is_none() && !args.force && target_type.version_identifies_artifact() {
        let source = sources::for_type(target_type);
        if let Some(resolved) = source.resolve(ctx.http.as_ref(), &target)
            && resolved == cfg.server.version && target_type == cfg.server.server_type
        {
            ui::info(format!(
                "Already running {} {resolved} — nothing to upgrade.",
                cfg.server.server_type
            ));
            ui::info("Reinstall this same version with: mc upgrade --force");
            return Ok(());
        }
    }

    // The backup runs as a separate process (`mc-backup command backup`) which
    // acquires its own lock. Holding our lock here would collide with it
    // because the re-entrancy guard only covers the same PID. Run the backup
    // without holding the lock, then re-acquire for the file mutations below.
    if args.no_backup {
        ui::warn("Skipping the pre-upgrade backup (--no-backup).");
    } else {
        take_backup(ctx)?;
    }

    let _lock = mc_common::lock::acquire(&ctx.paths.lock_file())?;

    let was_running = ctx.service.is_active(SERVICE_UNIT);
    if was_running {
        ui::info("Stopping server for upgrade...");
        ctx.service.stop(SERVICE_UNIT)?;
    }

    if let Some(pack) = &args.pack {
        crate::commands::pack::install_from_pack(ctx, pack, &mut cfg)?;
    } else {
        cfg.server.server_type = target_type;
        cfg.server.version = target;
        let resolved = install_artifact(ctx, &cfg)?;
        cfg.server.version = resolved;
    }
    cfg.save(&ctx.paths)?;

    ensure_java(ctx, cfg.java_major(), args.assume_yes)?;
    run_hook(ctx, Event::PostUpgrade);
    chown_tree(&ctx.paths.base())?;

    if was_running {
        ui::info("Restarting server...");
        crate::commands::lifecycle::start_and_verify(ctx)?;
    }

    ui::info("Upgrade complete.");
    Ok(())
}

/// Fetch the configured build into `MC_BASE` through a staging directory, and
/// return the version actually resolved.
///
/// Staging exists because both callers write into a LIVE server directory:
/// nothing lands there until the artifact is complete and verified. The guard
/// removes it on any early return, so an aborted download leaves no tree
/// behind.
pub(super) fn install_artifact(ctx: &Ctx, cfg: &Config) -> Result<String> {
    let source = sources::for_type(cfg.server.server_type);
    let staging = Staging::new(&ctx.paths.base())?;

    // NeoForge's installer needs a JVM before anything is installed, so the
    // runtime is resolved from the version being requested rather than from an
    // already-installed server.
    let java_bin = java::find_binary(ctx.paths.root(), cfg.java_major());
    let fetch_ctx = FetchCtx {
        http: ctx.http.as_ref(),
        java_bin: java_bin.as_deref(),
    };

    let resolved = source.fetch(&fetch_ctx, &cfg.server.version, staging.path())?;

    match source.layout() {
        Layout::Jar => {
            let from = staging.path().join("server.jar");
            let to = ctx.paths.server_jar();
            std::fs::rename(&from, &to).at(&to)?;
            // Switching from NeoForge (Tree) to a Jar-based type leaves
            // `run.sh` behind, which would cause `mc serve` to launch through
            // the wrong path. Clean up the NeoForge tree artefacts.
            clean_neoforge_tree(&ctx.paths.base())?;
        }
        Layout::Tree => {
            // The installer populated a whole tree; merge it over MC_BASE
            // rather than replacing, so an existing world survives an upgrade.
            merge_tree(staging.path(), &ctx.paths.base())?;
        }
    }
    Ok(resolved)
}

/// Copy `from` over `to`, creating directories as needed and overwriting files.
pub(super) fn merge_tree(from: &Path, to: &Path) -> Result<()> {
    for entry in std::fs::read_dir(from).at(from)? {
        let entry = entry.at(from)?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let file_type = entry.file_type().at(&src)?;

        // NEVER FOLLOW A SYMLINK AT THE DESTINATION. The source-side check
        // above guards against symlinks in the installer's output; this guards
        // against symlinks the service account plants in MC_BASE — a dangling
        // link to /etc/cron.d/mc would otherwise let root's write escape.
        if std::fs::symlink_metadata(&dst)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(Error::rejected(format!(
                "Refusing to write through a symlink at destination: {}",
                dst.display()
            )));
        }
        if file_type.is_dir() {
            std::fs::create_dir_all(&dst).at(&dst)?;
            merge_tree(&src, &dst)?;
        } else if file_type.is_symlink() {
            // The installer is upstream code running as root, but a symlink in
            // its output would still let a later write escape MC_BASE. Nothing
            // NeoForge produces needs one.
            return Err(Error::rejected(format!(
                "Refusing to install a symlink from the installer output: {}",
                src.display()
            )));
        } else {
            std::fs::copy(&src, &dst).at(&dst)?;
        }
    }
    Ok(())
}

/// Remove the NeoForge tree artefacts when switching to a Jar-based type.
///
/// NeoForge installs `run.sh`, `libraries/`, `versions/` and `user_jvm_args.txt`.
/// A Jar-based type (Vanilla, Paper, Fabric) uses `java -jar server.jar` instead,
/// and `mc serve` picks the launcher by checking for `run.sh` — so a leftover
/// file would cause it to launch through the wrong path.
///
/// Only removes known NeoForge files; user-generated content (worlds,
/// playerdata, whitelist, etc.) is never touched.
fn clean_neoforge_tree(base: &Path) -> Result<()> {
    // Best-effort: a missing file is not an error (already cleaned, or never
    // installed NeoForge in the first place).
    let _ = std::fs::remove_file(base.join("run.sh"));
    let _ = std::fs::remove_file(base.join("user_jvm_args.txt"));
    let _ = std::fs::remove_dir_all(base.join("libraries"));
    let _ = std::fs::remove_dir_all(base.join("versions"));
    Ok(())
}

/// Materialise a COMPLETE server.properties without generating the world.
///
/// `--initSettings` is a stock server flag: "Initializes 'server.properties' and
/// 'eula.txt', then quits". It writes every key at its default and exits before
/// any level is created, which is the only window in which `level-seed` is
/// still meaningful — the seed is consumed at world creation and inert
/// thereafter. It preserves keys that already have a value, so the managed
/// `server-port` and `rcon.*` survive.
///
/// RUNS AS `$MC_USER`. As root the JVM would create a root-owned
/// server.properties, which the service account can then neither read nor write
/// — a server that comes up on compiled-in defaults and generates a stray world
/// beside the real one.
///
/// A failure here WARNS rather than aborts. The managed-key file is still a
/// working configuration and the server fills in the rest on first boot; only
/// the chance to pre-set the seed is lost. That matters most for launchers this
/// flag is not verified against — vanilla is confirmed; Paper, Fabric and
/// NeoForge are not.
fn initialise_settings(ctx: &Ctx, java_major: u32) {
    let base = ctx.paths.base();
    ui::info("Writing default server.properties...");

    let java_bin = java::find_binary(ctx.paths.root(), java_major)
        .unwrap_or_else(|| std::path::PathBuf::from("java"));

    let mut command = std::process::Command::new("runuser");
    command.current_dir(&base).args(["-u", MC_USER, "--"]);
    if ctx.paths.run_sh().is_file() {
        if let Some(home) = java_bin.parent().and_then(Path::parent) {
            command.env("JAVA_HOME", home);
        }
        command.args(["bash", "run.sh", "--initSettings", "nogui"]);
    } else {
        command
            .arg(&java_bin)
            .args(["-jar", "server.jar", "--initSettings", "--nogui"]);
    }

    let output = command.output();

    // JUDGED ON THE OUTCOME, NOT THE EXIT CODE. NeoForge's FML wrapper exits 1
    // even when this worked: --initSettings returns without ever starting the
    // server thread, which FML reports as a fatal startup AFTER having written
    // the file. `level-seed` is the key this step exists to expose and nothing
    // else writes it, so the presence of the line — not a value; it is
    // legitimately empty — is the honest test of success.
    // The JVM rewrote the file under its own umask, and it may carry the RCON
    // password. Secure it before checking the outcome — the warning branch
    // below would otherwise leave it 0644.
    let _ = properties::secure(&ctx.paths.server_properties());
    let props = properties::Properties::load(&ctx.paths.server_properties());
    if props.get("level-seed").is_some() {
        return;
    }

    ui::warn("Could not pre-generate server.properties.");
    ui::warn("The server will write its own on first start — level-seed cannot be set after that.");
    if let Ok(output) = output {
        let combined = String::from_utf8_lossy(&output.stderr);
        for line in combined
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            eprintln!("{line}");
        }
    }
}

/// Ensure a JRE of the given major version is present, installing it via apt
/// when it is not.
fn ensure_java(ctx: &Ctx, major: u32, assume_yes: bool) -> Result<()> {
    if java::find_binary(ctx.paths.root(), major).is_some() {
        return Ok(());
    }
    let package = java::jre_package(major);

    if !assume_yes {
        // Deliberately NOT folded into the EULA flag: this consents to
        // installing a package, that one consents to a licence agreement.
        let question = format!(
            "Minecraft requires Java {major}, which isn't installed. Install {package} now?"
        );
        if !ui::confirm(&question) {
            return Err(Error::config(format!(
                "Java {major} is required but not installed. Re-run with --yes, or install manually: apt install {package}"
            )));
        }
    }

    ui::info(format!("Installing {package}..."));
    ctx.packages.install(&package)?;
    Ok(())
}

/// Give plugins their turn at an event.
///
/// Never fatal here. A plugin failing to configure itself must not undo an
/// otherwise successful install — the server is on disk and working, and the
/// failure is reported rather than rolled back into a half-installed state.
fn run_hook(ctx: &Ctx, event: Event) {
    let registry = Registry::discover(&ctx.paths);
    let payload = serde_json::json!({ "event": event.as_str() });
    if let Err(e) = registry.run_hook(&ctx.paths, event, &payload) {
        ui::warn(format!("{event} hook: {e}"));
    }
}

/// Hand the whole server directory to the service account.
fn chown_tree(base: &Path) -> Result<()> {
    let Some(owner) = mc_common::privilege::service_account() else {
        // No such account — an unprivileged test, or a machine where the
        // postinst has not run. Recording the intent is all that is possible.
        return Ok(());
    };
    fn walk(dir: &Path, owner: (nix::unistd::Uid, nix::unistd::Gid)) -> Result<()> {
        let _ = nix::unistd::chown(dir, Some(owner.0), Some(owner.1));
        for entry in std::fs::read_dir(dir).at(dir)? {
            let entry = entry.at(dir)?;
            let path = entry.path();
            let file_type = entry.file_type().at(&path)?;
            if file_type.is_dir() {
                walk(&path, owner)?;
            } else if !file_type.is_symlink() {
                // Never follow a symlink while chowning as root: a link planted
                // by the service account would otherwise hand it an inode it
                // does not own.
                let _ = nix::unistd::chown(&path, Some(owner.0), Some(owner.1));
            }
        }
        Ok(())
    }
    walk(base, owner)
}

/// Run a pre-upgrade backup through whichever plugin provides one.
///
/// Refuses rather than silently skipping: losing the world to a bad upgrade is
/// the failure this protects against, and "mc-backup was not installed" is not
/// a reason to proceed without asking.
fn take_backup(ctx: &Ctx) -> Result<()> {
    let backup = ctx.paths.libexec("mc-backup");
    if !backup.is_file() {
        return Err(Error::config(
            "No backup provider is installed, so no pre-upgrade backup can be taken.\n\
             Install one:      apt install mc-backup\n\
             Or accept the risk: mc upgrade --no-backup",
        ));
    }
    ui::info("Creating pre-upgrade backup...");
    let status = std::process::Command::new(&backup)
        .args(["command", "backup"])
        .status()
        .map_err(|e| Error::other(format!("running {}: {e}", backup.display())))?;
    if !status.success() {
        return Err(Error::other("Pre-upgrade backup failed. Aborting upgrade."));
    }
    Ok(())
}
