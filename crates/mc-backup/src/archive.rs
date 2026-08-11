//! Creating and validating backup archives.

use std::io::Read;
use std::path::{Path, PathBuf};

use mc_common::error::{Error, IoContext, Result};

/// Regenerated or derived data, excluded to shrink the archive and shorten the
/// window in which the world is held still.
///
/// `libraries/` and `mods/` are deliberately NOT excluded: restore is a plain
/// extraction with no re-download step, so dropping them would produce an
/// unbootable restore.
pub const EXCLUDED: [&str; 3] = ["logs", "crash-reports", "cache"];

/// True when a path inside the server directory should be left out.
pub fn excluded(relative: &Path) -> bool {
    relative
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .is_some_and(|first| EXCLUDED.contains(&first))
}

/// What an archive member is allowed to be.
///
/// ENTRY TYPES MATTER AS MUCH AS ENTRY NAMES. An entry named
/// `minecraft/passwd` hardlinked to `/etc/shadow` satisfies every name check,
/// and the ownership pass at the end of a restore then hands that inode to the
/// service account. Extraction runs as root, which is exempt from
/// `fs.protected_hardlinks`, so the kernel does not stop it either.
///
/// A real backup of a server directory contains nothing but regular files and
/// directories.
fn entry_type_allowed(kind: tar::EntryType) -> bool {
    matches!(kind, tar::EntryType::Regular | tar::EntryType::Directory)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Every member, relative to the archive root.
    pub members: Vec<PathBuf>,
}

/// Validate every member of an archive before anything is extracted.
///
/// Reads the whole listing first and refuses on the first problem, so a hostile
/// archive is rejected before a single byte is written — not partway through,
/// with half a tree already on disk.
pub fn validate<R: Read>(reader: R, expected_root: &str) -> Result<Plan> {
    let decoder = flate2::read::GzDecoder::new(reader);
    let mut archive = tar::Archive::new(decoder);
    let mut members = Vec::new();

    let entries = archive
        .entries()
        .map_err(|e| Error::rejected(format!("Failed to read the archive: {e}")))?;

    for entry in entries {
        let entry = entry.map_err(|e| Error::rejected(format!("Failed to read an entry: {e}")))?;
        let kind = entry.header().entry_type();

        if !entry_type_allowed(kind) {
            let path = entry
                .path()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            return Err(Error::rejected(format!(
                "Refusing archive containing a {kind:?} entry: {path:?}"
            )));
        }

        let path = entry
            .path()
            .map_err(|e| Error::rejected(format!("Archive entry has an unreadable path: {e}")))?
            .into_owned();

        // Reject absolute paths and `..` traversal. The archive is unpacked
        // into MC_BASE's PARENT, so an unchecked path is an arbitrary root file
        // write.
        let raw = path.to_string_lossy();
        mc_common::staging::safe_relative_path(&raw)?;

        // And require everything to live under the expected top-level
        // directory: a valid-looking archive of somebody else's tree must not
        // be unpacked over this one.
        if !path.starts_with(expected_root) {
            return Err(Error::rejected(format!(
                "Refusing archive with unexpected entry {raw:?} (expected everything under {expected_root:?})."
            )));
        }

        members.push(path);
    }

    if members.is_empty() {
        return Err(Error::rejected("Refusing an empty archive."));
    }
    Ok(Plan { members })
}

/// Validate an archive on disk.
pub fn validate_file(archive: &Path, expected_root: &str) -> Result<Plan> {
    let file = std::fs::File::open(archive).at(archive)?;
    validate(file, expected_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Build a .tar.gz in memory from (path, type) pairs.
    fn build(entries: &[(&str, tar::EntryType)]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        {
            let mut builder = tar::Builder::new(&mut encoder);
            for (path, kind) in entries {
                let body = b"data";
                let mut header = tar::Header::new_gnu();
                header.set_size(if *kind == tar::EntryType::Regular {
                    body.len() as u64
                } else {
                    0
                });
                header.set_entry_type(*kind);
                header.set_mode(0o644);
                if matches!(kind, tar::EntryType::Link | tar::EntryType::Symlink) {
                    header.set_link_name("/etc/shadow").unwrap();
                }
                header.set_cksum();
                builder
                    .append_data(
                        &mut header,
                        path,
                        &body[..if *kind == tar::EntryType::Regular {
                            body.len()
                        } else {
                            0
                        }],
                    )
                    .unwrap();
            }
            builder.finish().unwrap();
        }
        let mut out = encoder.finish().unwrap();
        out.flush().unwrap();
        out
    }

    #[test]
    fn accepts_an_ordinary_backup() {
        let archive = build(&[
            ("minecraft/", tar::EntryType::Directory),
            ("minecraft/server.jar", tar::EntryType::Regular),
            ("minecraft/world/level.dat", tar::EntryType::Regular),
        ]);
        let plan = validate(&archive[..], "minecraft").unwrap();
        assert_eq!(plan.members.len(), 3);
    }

    #[test]
    fn refuses_a_hardlink_to_a_file_outside_the_tree() {
        // The attack a name check cannot see: `minecraft/passwd` hardlinked to
        // /etc/shadow passes every path test, and the restore's ownership pass
        // then hands that inode to the service account. Root extraction is
        // exempt from fs.protected_hardlinks.
        let archive = build(&[
            ("minecraft/server.jar", tar::EntryType::Regular),
            ("minecraft/passwd", tar::EntryType::Link),
        ]);
        let err = validate(&archive[..], "minecraft").unwrap_err();
        assert!(matches!(err, Error::Rejected(_)), "{err}");
        assert!(err.to_string().contains("Link"), "{err}");
    }

    #[test]
    fn refuses_a_symlink() {
        let archive = build(&[("minecraft/evil", tar::EntryType::Symlink)]);
        assert!(validate(&archive[..], "minecraft").is_err());
    }

    #[test]
    fn refuses_device_nodes_and_fifos() {
        for kind in [
            tar::EntryType::Fifo,
            tar::EntryType::Char,
            tar::EntryType::Block,
        ] {
            let archive = build(&[("minecraft/thing", kind)]);
            assert!(validate(&archive[..], "minecraft").is_err(), "{kind:?}");
        }
    }

    /// Build a tar with a hand-written header.
    ///
    /// The `tar` crate refuses to *write* a `..` or absolute path, which is a
    /// good default and exactly why the hostile fixture cannot go through it. A
    /// real attacker is under no such constraint: the 512-byte header below is
    /// what they would produce, and it is what the validator has to survive.
    fn build_raw(name: &str) -> Vec<u8> {
        let mut header = [0u8; 512];
        let name_bytes = name.as_bytes();
        header[..name_bytes.len()].copy_from_slice(name_bytes);
        header[100..108].copy_from_slice(b"0000644\0"); // mode
        header[108..116].copy_from_slice(b"0000000\0"); // uid
        header[116..124].copy_from_slice(b"0000000\0"); // gid
        header[124..136].copy_from_slice(b"00000000000\0"); // size = 0
        header[136..148].copy_from_slice(b"00000000000\0"); // mtime
        header[156] = b'0'; // typeflag: regular
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");

        // The checksum is computed with the checksum field read as spaces.
        header[148..156].copy_from_slice(b"        ");
        let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
        let checksum = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());

        let mut tar_bytes = header.to_vec();
        tar_bytes.extend_from_slice(&[0u8; 1024]); // two zero blocks = end of archive

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&tar_bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn refuses_traversal_and_absolute_paths() {
        for path in [
            "../etc/cron.d/x",
            "minecraft/../../etc/passwd",
            "/etc/cron.d/x",
            "minecraft/../../../root/.ssh/authorized_keys",
        ] {
            let archive = build_raw(path);
            let err = validate(&archive[..], "minecraft");
            assert!(err.is_err(), "{path} should be refused");
        }
    }

    #[test]
    fn the_raw_fixture_builder_produces_a_readable_archive() {
        // Guards the test above from passing vacuously: if the hand-written
        // header were malformed, every case would "fail validation" for the
        // wrong reason and the traversal checks would never run.
        let archive = build_raw("minecraft/server.jar");
        let plan = validate(&archive[..], "minecraft").unwrap();
        assert_eq!(plan.members, vec![PathBuf::from("minecraft/server.jar")]);
    }

    #[test]
    fn refuses_an_archive_of_somebody_elses_tree() {
        let archive = build(&[("etc/passwd", tar::EntryType::Regular)]);
        let err = validate(&archive[..], "minecraft").unwrap_err();
        assert!(
            err.to_string().contains("expected everything under"),
            "{err}"
        );
    }

    #[test]
    fn refuses_an_empty_archive() {
        // A truncated or zero-entry archive would otherwise "restore" by
        // emptying the server directory and putting nothing back.
        let archive = build(&[]);
        assert!(validate(&archive[..], "minecraft").is_err());
    }

    #[test]
    fn refuses_something_that_is_not_a_gzip_archive_at_all() {
        assert!(validate(&b"not an archive"[..], "minecraft").is_err());
    }

    #[test]
    fn excludes_only_regenerated_data() {
        assert!(excluded(Path::new("logs/latest.log")));
        assert!(excluded(Path::new("crash-reports/x.txt")));
        assert!(excluded(Path::new("cache/y")));
        // Never excluded: restore is a plain extraction with no re-download, so
        // dropping these produces an unbootable server.
        assert!(!excluded(Path::new("mods/sodium.jar")));
        assert!(!excluded(Path::new("libraries/a/b.jar")));
        assert!(!excluded(Path::new("world/level.dat")));
        assert!(!excluded(Path::new("server.jar")));
        // Only a leading component counts.
        assert!(!excluded(Path::new("world/logs/x")));
    }
}
