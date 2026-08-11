//! `/usr/libexec/mc/mc-backup` — the plugin binary.

use std::io::Seek;
use std::path::{Path, PathBuf};

use mc_backup::{archive, rotation};
use mc_common::error::{Error, IoContext, Result};
use mc_common::paths::{MC_USER, Paths, SERVICE_UNIT};
use mc_common::plugin::{Event, Registry};
use mc_common::service::{ServiceManager, Systemctl};
use mc_common::{Config, fsx, privilege, ui};

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = Paths::from_env();

    // Completions list: outputs JSON for mc completions to consume
    if args.first().map(String::as_str) == Some("completions")
        && args.get(1).map(String::as_str) == Some("list")
    {
        // backup and restore have no subcommands
        println!(r#"{{"subcommands":[]}}"#);
        return std::process::ExitCode::SUCCESS;
    }

    // Handle --help and --version before any other checks
    if args.get(2).map(String::as_str) == Some("--help")
        || args.get(2).map(String::as_str) == Some("-h")
    {
        match args.get(1).map(String::as_str) {
            Some("backup") => {
                println!("{}", backup_usage());
                return std::process::ExitCode::SUCCESS;
            }
            Some("restore") => {
                println!("{}", restore_usage());
                return std::process::ExitCode::SUCCESS;
            }
            _ => {}
        }
    }

    if args.get(2).map(String::as_str) == Some("--version")
        || args.get(2).map(String::as_str) == Some("-V")
    {
        println!("mc-backup {}", env!("CARGO_PKG_VERSION"));
        return std::process::ExitCode::SUCCESS;
    }

    let result = match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) {
        (Some("command"), Some("backup")) => backup(&paths, &args),
        (Some("command"), Some("restore")) => restore(&paths, &args),
        (Some("hook"), _) => Ok(()), // contributes no hooks; it emits them
        _ => Err(Error::config(
            "mc-backup is a plugin for mc, not a command. Use: mc backup / mc restore",
        )),
    };

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            ui::error(e.to_string());
            std::process::ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(1))
        }
    }
}

fn backup(paths: &Paths, argv: &[String]) -> Result<()> {
    privilege::require_root(&paths.mc_bin(), argv)?;
    if !paths.server_installed() {
        return Err(Error::config("No server installed. Run: mc install"));
    }

    // Serialises against install/upgrade/restore. Without it the timer can tar
    // MC_BASE midway through an install or — worse — while a restore is
    // emptying the directory, and the rotation below would then prune a good
    // archive in favour of the truncated one.
    let _lock = mc_common::lock::acquire(&paths.lock_file())?;
    let config = Config::load(paths)?;
    let registry = Registry::discover(paths);

    let backup_dir = paths.backup_dir();
    std::fs::create_dir_all(&backup_dir).at(&backup_dir)?;
    // root:root 0700. Never owned by the service account: that account runs
    // untrusted mods, and owning this directory would let it pre-create the
    // next predictable archive name as a symlink for root's writer to follow.
    fsx::apply_owner_mode(&backup_dir, fsx::lookup_user("root"), 0o700)?;

    let running = Systemctl::new(paths.systemd_running()).is_active(SERVICE_UNIT);

    if running {
        // Flush the world and hold it still. Whoever can talk to the server
        // provides this; if nobody can, the archive is still taken — a backup
        // of an unflushed world beats no backup at all, and the hook says so.
        registry.run_hook(paths, Event::PreBackup, &serde_json::json!({}))?;
    }

    let target = backup_dir.join(rotation::archive_name(std::time::SystemTime::now()));
    ui::info(format!("Creating backup: {}", target.display()));

    let result = write_archive(&paths.base(), &target);

    if running {
        // RUNS WHETHER OR NOT THE ARCHIVE SUCCEEDED. A live server left with
        // saves disabled loses everything since the last flush the moment it
        // stops — worse than the failed backup that got us here. The manifest
        // loader refuses to let a plugin declare this hook fatal for the same
        // reason.
        let _ = registry.run_hook(paths, Event::PostBackup, &serde_json::json!({}));
    }

    // Only after save-on has been restored.
    result?;

    // Written by root, read only by root (`mc restore`). Handing an archive to
    // the account that runs untrusted mods would let a compromised server
    // rewrite what a later restore extracts as root.
    fsx::apply_owner_mode(&target, fsx::lookup_user("root"), 0o600)?;

    for stale in rotation::prune_list(&rotation::sorted_archives(&backup_dir), config.backup.keep) {
        let _ = std::fs::remove_file(&stale);
    }

    ui::info(format!("Backup complete: {}", target.display()));
    Ok(())
}

/// Build the archive, removing it on any failure.
///
/// A half-written archive left behind would be counted as the newest by the
/// rotation above and could be chosen by a restore.
fn write_archive(base: &Path, target: &Path) -> Result<()> {
    match write_archive_inner(base, target) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(target);
            Err(e)
        }
    }
}

fn write_archive_inner(base: &Path, target: &Path) -> Result<()> {
    let root_name = base
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| MC_USER.to_string());

    let file = std::fs::File::create(target).at(target)?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    // Never dereference: a symlink inside the server directory must be archived
    // as a symlink, not as a copy of whatever it points at — which could be
    // anything on the host.
    builder.follow_symlinks(false);

    append_tree(&mut builder, base, Path::new(""), &root_name)?;

    let encoder = builder
        .into_inner()
        .map_err(|e| Error::other(format!("finishing the archive: {e}")))?;
    encoder
        .finish()
        .map_err(|e| Error::other(format!("compressing the archive: {e}")))?;
    Ok(())
}

fn append_tree<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    dir: &Path,
    relative: &Path,
    root_name: &str,
) -> Result<()> {
    for entry in std::fs::read_dir(dir).at(dir)? {
        let entry = entry.at(dir)?;
        let path = entry.path();
        let relative = relative.join(entry.file_name());

        if archive::excluded(&relative) {
            continue;
        }
        let in_archive = Path::new(root_name).join(&relative);
        let file_type = entry.file_type().at(&path)?;

        if file_type.is_dir() {
            append_tree(builder, &path, &relative, root_name)?;
        } else if file_type.is_file() {
            builder
                .append_path_with_name(&path, &in_archive)
                .map_err(|e| Error::other(format!("archiving {}: {e}", path.display())))?;
        }
        // Anything else — symlinks, sockets, FIFOs — is skipped rather than
        // archived. `validate` refuses to restore them, so archiving one would
        // produce a backup that cannot be restored.
    }
    Ok(())
}

fn restore(paths: &Paths, argv: &[String]) -> Result<()> {
    privilege::require_root(&paths.mc_bin(), argv)?;

    let source = argv
        .get(2)
        .ok_or_else(|| Error::config("Usage: mc restore <backup-file>"))?;
    let source = PathBuf::from(source);
    if !source.is_file() {
        return Err(Error::config(format!(
            "Backup file not found: {}",
            source.display()
        )));
    }

    let _lock = mc_common::lock::acquire(&paths.lock_file())?;
    let service = Systemctl::new(paths.systemd_running());

    let base = paths.base();
    let root_name = base
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| MC_USER.to_string());

    // VALIDATED IN FULL BEFORE ANYTHING IS TOUCHED — before the server is even
    // stopped. A hostile archive is rejected while the server is still running
    // and its directory still intact, rather than after both have been
    // dismantled.
    //
    // Open ONCE — validated and extracted from the same handle, so a concurrent
    // process cannot swap the archive between the two passes (TOCTOU).
    let mut file = std::fs::File::open(&source).at(&source)?;
    let plan = archive::validate(&mut file, &root_name)?;
    ui::info(format!(
        "Archive validated: {} entries.",
        plan.members.len()
    ));

    if service.is_active(SERVICE_UNIT) {
        ui::warn("Stopping server for restore...");
        service.stop(SERVICE_UNIT)?;
    }

    ui::info(format!("Restoring from {}...", source.display()));

    // Clear the existing contents, including dotfiles. Use `file_type()`
    // (an lstat from the DirEntry) rather than `is_dir()` (which follows
    // symlinks): a link planted by the service account would otherwise
    // take the `remove_dir_all` branch, fail on modern Rust, and leave
    // a half-emptied server after the stop.
    if base.is_dir() {
        for entry in std::fs::read_dir(&base).at(&base)? {
            let entry = entry.at(&base)?;
            let file_type = entry.file_type().at(entry.path())?;
            if file_type.is_dir() {
                std::fs::remove_dir_all(entry.path()).at(entry.path())?;
            } else {
                std::fs::remove_file(entry.path()).at(entry.path())?;
            }
        }
    }

    let parent = base.parent().unwrap_or(Path::new("/"));
    // Rewind to the start of the file for extraction.
    file.seek(std::io::SeekFrom::Start(0)).at(&source)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    // Do NOT honour the uid/gid stored in the archive: ownership is asserted
    // below from the account that exists on THIS machine.
    tar.set_preserve_permissions(false);
    tar.set_overwrite(true);
    tar.unpack(parent)
        .map_err(|e| Error::other(format!("extracting the archive: {e}")))?;

    chown_tree(&base)?;

    // Re-assert the mode the postinst asserts, because the archive stores
    // whatever mode it was created with, and `set_preserve_permissions(false)`
    // drops only setuid/setgid/sticky, not the umask.
    fsx::apply_owner_mode(&base, fsx::lookup_user(MC_USER), 0o750)?;

    // Re-apply managed keys and secure the file: a hostile archive could store
    // server.properties at 0644 or with attacker-chosen credentials. `merge`
    // resolves managed values from the passwd file (not the archive), re-applies
    // all MANAGED_KEYS unconditionally, and calls `secure` through `save`.
    // Passing the archive's own contents as the override preserves all
    // non-managed keys while correcting the managed ones.
    if paths.server_properties().is_file() {
        let text = std::fs::read_to_string(paths.server_properties())
            .map_err(|e| Error::io("reading server.properties after restore", e))?;
        mc_common::properties::merge(paths, &text)?;
    }

    ui::info("Restore complete — start with: mc start");
    Ok(())
}

fn chown_tree(base: &Path) -> Result<()> {
    let Some(owner) = privilege::service_account() else {
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
                // Never follow a symlink while chowning as root. Validation
                // refuses to extract one, so this is belt and braces — but it
                // is the step that would hand an arbitrary inode to the service
                // account if validation were ever weakened.
                let _ = nix::unistd::chown(&path, Some(owner.0), Some(owner.1));
            }
        }
        Ok(())
    }
    walk(base, owner)
}

fn backup_usage() -> &'static str {
    "Usage: mc backup\n\n \
     Create a backup of the Minecraft server.\n \
     The backup is stored in /opt/minecraft/backups/.\n \
     Old backups are rotated according to the keep count in config.toml."
}

fn restore_usage() -> &'static str {
    "Usage: mc restore <backup-file>\n\n \
     Restore the Minecraft server from a backup archive.\n \
     The server will be stopped during restoration."
}
