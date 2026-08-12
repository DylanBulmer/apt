//! Staging directories, and validating paths taken from untrusted archives.
//!
//! Nothing lands in `MC_BASE` until it is complete and verified. Both install
//! and upgrade write into a live server directory, so a download that fails
//! halfway must leave the running server untouched.

use std::path::{Path, PathBuf};

use crate::error::{Error, IoContext, Result};

/// A staging directory that removes itself unless it is explicitly kept.
///
/// RAII rather than a cleanup registry and an `EXIT` trap. An abort mid-download
/// — including a `?` several frames up — cannot leave a half-extracted tree
/// behind, and there is no global registry to forget to register with.
#[derive(Debug)]
pub struct Staging {
    path: PathBuf,
    keep: bool,
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

impl Staging {
    /// Create a staging directory ON THE SAME FILESYSTEM as `MC_BASE`.
    ///
    /// Same filesystem so the final move is a rename rather than a copy: a copy
    /// across filesystems is neither atomic nor free, and for a modpack it
    /// means writing several hundred megabytes twice.
    pub fn new(base: &Path) -> Result<Self> {
        let parent = base.parent().unwrap_or(Path::new("/"));
        std::fs::create_dir_all(parent).at(parent)?;
        let path = tempfile::Builder::new()
            .prefix(".mc-staging-")
            .tempdir_in(parent)
            .at(parent)?
            // into_path hands ownership to this guard, so there is exactly one
            // thing responsible for removing it.
            .keep();
        Ok(Self { path, keep: false })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Give up ownership: the caller has moved the contents somewhere real.
    pub fn keep(mut self) -> PathBuf {
        self.keep = true;
        self.path.clone()
    }
}

/// Validate a file path taken from an untrusted archive or manifest before it
/// is used to build a destination.
///
/// A malicious `.mrpack` can set an arbitrary `path` — `../../../../etc/cron.d/x`
/// — and since install runs as root and the staged tree is copied into
/// `MC_BASE`, an unchecked path is an arbitrary root file write.
///
/// Returns the path relative to the staging root when it is safe.
pub fn safe_relative_path(raw: &str) -> Result<PathBuf> {
    if raw.is_empty() {
        return Err(Error::rejected("Archive entry has an empty path."));
    }
    // A NUL truncates the path for any syscall that receives it, so the name
    // checked here would not be the name written.
    if raw.contains('\0') {
        return Err(Error::rejected(format!(
            "Archive entry path contains NUL: {raw:?}"
        )));
    }
    if raw.starts_with('/') {
        return Err(Error::rejected(format!("Refusing absolute path: {raw:?}")));
    }
    if raw.starts_with('~') {
        return Err(Error::rejected(format!("Refusing path with '~': {raw:?}")));
    }
    // Backslash is not a separator on Unix, but plenty of pack tooling emits
    // Windows paths and something downstream may normalise them. Refuse rather
    // than guess which.
    if raw.contains('\\') {
        return Err(Error::rejected(format!(
            "Refusing path with a backslash: {raw:?}"
        )));
    }
    // A Windows drive letter would be absolute to anything that normalises it.
    if raw.len() >= 2 && raw.chars().nth(1) == Some(':') {
        return Err(Error::rejected(format!(
            "Refusing path with a drive letter: {raw:?}"
        )));
    }

    let path = Path::new(raw);
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                return Err(Error::rejected(format!("Refusing path with '..': {raw:?}")));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(Error::rejected(format!("Refusing absolute path: {raw:?}")));
            }
            _ => {}
        }
    }
    Ok(path.to_path_buf())
}

/// Resolve an untrusted relative path against a root, refusing anything that
/// would escape it.
///
/// The path check above is the first gate; this is the second. They are not
/// redundant: the first rejects the name, this rejects the *result*, which is
/// what catches an escape through a symlink that was already staged.
pub fn resolve_under(root: &Path, raw: &str) -> Result<PathBuf> {
    let relative = safe_relative_path(raw)?;
    let joined = root.join(&relative);

    // Compare against the root as the caller gave it. Canonicalising the
    // joined path would require it to exist, and it does not yet; canonicalise
    // only the part that does.
    let anchor = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut walked = anchor.clone();
    for component in relative.components() {
        walked.push(component);
        // A symlink already present in the staging tree — from an earlier entry
        // in the same archive — is the escape route a name check cannot see.
        if walked.is_symlink() {
            return Err(Error::rejected(format!(
                "Refusing to write through a symlink in the archive: {raw:?}"
            )));
        }
    }
    if !walked.starts_with(&anchor) {
        return Err(Error::rejected(format!(
            "Refusing path outside the staging root: {raw:?}"
        )));
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_removes_itself_on_an_early_return() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("opt/minecraft");
        std::fs::create_dir_all(&base).unwrap();

        let leaked: PathBuf = {
            let staging = Staging::new(&base).unwrap();
            let path = staging.path().to_path_buf();
            std::fs::write(path.join("server.jar"), b"partial").unwrap();
            assert!(path.exists());
            path
        };
        assert!(
            !leaked.exists(),
            "an aborted download must not leave a tree behind"
        );
    }

    #[test]
    fn staging_can_be_kept_once_its_contents_are_moved() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("opt/minecraft");
        std::fs::create_dir_all(&base).unwrap();

        let staging = Staging::new(&base).unwrap();
        let path = staging.keep();
        assert!(path.exists());
        std::fs::remove_dir_all(&path).unwrap();
    }

    #[test]
    fn staging_shares_a_filesystem_with_the_server_directory() {
        // Same parent, so the final move is a rename rather than a copy of
        // several hundred megabytes.
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("opt/minecraft");
        std::fs::create_dir_all(&base).unwrap();
        let staging = Staging::new(&base).unwrap();
        assert_eq!(staging.path().parent(), base.parent());
    }

    #[test]
    fn accepts_the_paths_a_real_pack_contains() {
        for good in [
            "mods/sodium.jar",
            "config/foo/bar.toml",
            "server.properties",
            "mods/a-b_c.1.jar",
            "./mods/x.jar",
        ] {
            assert!(
                safe_relative_path(good).is_ok(),
                "{good} should be accepted"
            );
        }
    }

    #[test]
    fn rejects_every_way_out_of_the_staging_root() {
        for bad in [
            "../../../../etc/cron.d/x",
            "..",
            "../x",
            "mods/../../x",
            "/etc/passwd",
            "/etc/cron.d/x",
            "~/.ssh/authorized_keys",
            "~root/.bashrc",
            "mods\\..\\..\\x",
            "C:/windows/x",
            "",
            "mods/\0/x",
        ] {
            assert!(
                safe_relative_path(bad).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn rejects_a_write_through_a_symlink_staged_by_an_earlier_entry() {
        // The escape a name check cannot see: entry one creates
        // `mods -> /etc`, entry two writes `mods/cron.d/x`. Both names are
        // clean; the second write lands outside the tree.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("staging");
        std::fs::create_dir_all(&root).unwrap();

        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("mods")).unwrap();

        let err = resolve_under(&root, "mods/evil.jar").unwrap_err();
        assert!(matches!(err, Error::Rejected(_)), "{err}");
        assert!(err.to_string().contains("symlink"), "{err}");
    }

    #[test]
    fn resolves_a_clean_path_under_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("staging");
        std::fs::create_dir_all(root.join("mods")).unwrap();
        let resolved = resolve_under(&root, "mods/sodium.jar").unwrap();
        assert_eq!(resolved, root.join("mods/sodium.jar"));
    }
}
