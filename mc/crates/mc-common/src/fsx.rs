//! Filesystem helpers whose *ownership* semantics are load-bearing.
//!
//! Mode alone is never the whole answer in this tree. `server.properties` is
//! 0640 and readable only because its owner is the service account; the same
//! file 0640 root:root is worse than 0644, because the JVM can then neither
//! read nor write it and comes up on compiled-in defaults.

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use nix::unistd::{Gid, Group, Uid, User};

use crate::error::{IoContext, Result};

/// Resolve a system account name to its uid/gid.
///
/// Returns `None` when the account does not exist, which is the normal case in
/// an unprivileged test: nothing here should hard-fail merely because the
/// `minecraft` user is absent, so callers treat `None` as "record the intent,
/// skip the syscall".
pub fn lookup_user(name: &str) -> Option<(Uid, Gid)> {
    let user = User::from_name(name).ok().flatten()?;
    Some((user.uid, user.gid))
}

pub fn lookup_group(name: &str) -> Option<Gid> {
    Group::from_name(name).ok().flatten().map(|g| g.gid)
}

/// True when this process is root.
pub fn is_root() -> bool {
    Uid::effective().is_root()
}

/// Apply mode, and owner when both the account exists and we are privileged
/// enough to set it.
///
/// The chown is tolerant by design: this is called from paths that already
/// required root, and from tests that never can. A refusal to chown must not
/// turn into a refusal to write the file — but the mode is NOT tolerant, since
/// leaving the RCON password world-readable is the failure this exists to
/// prevent.
pub fn apply_owner_mode(path: &Path, owner: Option<(Uid, Gid)>, mode: u32) -> Result<()> {
    if let Some((uid, gid)) = owner {
        // ENOENT here would mean the file vanished between write and chown;
        // EPERM means we are not root. Neither is worth failing the operation
        // over — the mode below is what keeps the secret from leaking.
        let _ = nix::unistd::chown(path, Some(uid), Some(gid));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).at(path)
}

/// Read a file's current mode, or `None` if it does not exist.
pub fn mode_of(path: &Path) -> Option<u32> {
    fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o7777)
}

/// Read a file's current owner, or `None` if it does not exist.
pub fn owner_of(path: &Path) -> Option<(Uid, Gid)> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path)
        .ok()
        .map(|m| (Uid::from_raw(m.uid()), Gid::from_raw(m.gid())))
}

/// Copy from `reader` to `writer`, refusing if more than `limit` bytes are available.
///
/// A bare `std::io::copy` has no budget: a highly compressible entry
/// expands to hundreds of gigabytes and takes the root filesystem.
pub fn copy_bounded<R: std::io::Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    limit: u64,
) -> std::io::Result<u64> {
    let mut limited = reader.take(limit);
    let copied = std::io::copy(&mut limited, writer)?;
    if copied == limit {
        // Source may still have data. Read one more byte to check.
        let mut buf = [0u8; 1];
        if limited.read(&mut buf).map(|n| n > 0).unwrap_or(false) {
            return Err(std::io::Error::other(format!(
                "Output exceeded {limit} byte limit",
            )));
        }
    }
    Ok(copied)
}

/// Replace a file's contents without ever exposing a partial write.
///
/// The temp file is created in the SAME directory so the rename is atomic —
/// across filesystems `rename` fails and a copy would leave a window where the
/// JVM could read a half-written config.
///
/// Owner and mode are carried across the swap from the file being replaced.
/// A fresh temp file is 0600 owned by whoever is running, so without this a
/// root-run `mc` would hand the service account a file it cannot read. When
/// there is no existing file, `fallback_mode` and `fallback_owner` apply.
pub fn write_atomic(
    path: &Path,
    contents: &str,
    fallback_owner: Option<(Uid, Gid)>,
    fallback_mode: u32,
) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).at(dir)?;

    // Inherit from the file we are about to replace, so an operator who
    // deliberately tightened the mode keeps it.
    let owner = owner_of(path).or(fallback_owner);
    let mode = mode_of(path).unwrap_or(fallback_mode);

    let tmp = tempfile::Builder::new()
        .prefix(".mc-")
        .suffix(".tmp")
        .tempfile_in(dir)
        .at(dir)?;
    fs::write(tmp.path(), contents).at(tmp.path())?;
    apply_owner_mode(tmp.path(), owner, mode)?;

    // persist() renames, which replaces the destination atomically.
    tmp.persist(path)
        .map_err(|e| crate::error::Error::io(path, e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_carries_mode_across_the_swap() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("server.properties");

        write_atomic(&f, "a=1\n", None, 0o640).unwrap();
        assert_eq!(
            mode_of(&f),
            Some(0o640),
            "fallback mode applies to a new file"
        );

        // An operator tightened it; a later rewrite must not loosen it back to
        // the fallback.
        fs::set_permissions(&f, fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&f, "a=2\n", None, 0o640).unwrap();
        assert_eq!(mode_of(&f), Some(0o600));
        assert_eq!(fs::read_to_string(&f).unwrap(), "a=2\n");
    }

    #[test]
    fn atomic_write_leaves_no_temp_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("server.properties");
        write_atomic(&f, "a=1\n", None, 0o640).unwrap();

        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "temp file should have been renamed, not left: {entries:?}"
        );
    }
}
