//! The RCON password: generating it, storing it, reading it back.

use std::io::Read as _;
use std::path::Path;

use mc_common::error::{Error, IoContext, Result};
use mc_common::fsx;
use mc_common::paths::{MC_USER, Paths};

/// Generate a password: 24 random bytes in base64url.
///
/// THE CHARSET IS LOAD-BEARING, not cosmetic. base64url (`A-Za-z0-9-_`)
/// excludes every character that would need escaping downstream — in
/// particular `=`, which would be ambiguous in a `key=value` properties line,
/// and the shell metacharacters the previous implementation had to escape when
/// writing this value into `server.properties` with `sed`.
pub fn generate() -> Result<String> {
    let mut bytes = [0u8; 24];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| Error::io("/dev/urandom", e))?;
    Ok(base64url(&bytes))
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk.first().copied().unwrap_or(0);
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let triple = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);

        // No '=' padding: it is dropped rather than emitted, which is what
        // keeps the value safe to write into a properties file unquoted.
        let take = chunk.len() + 1;
        for i in 0..take {
            let index = ((triple >> (18 - 6 * i)) & 0x3f) as usize;
            if let Some(c) = ALPHABET.get(index) {
                out.push(char::from(*c));
            }
        }
    }
    out
}

/// Read the stored password.
pub fn read(paths: &Paths) -> Result<String> {
    let file = paths.passwd_file();
    let raw = std::fs::read_to_string(&file).at(&file)?;
    let password = raw.trim().to_string();
    if password.is_empty() {
        return Err(Error::config(format!("{} is empty.", file.display())));
    }
    Ok(password)
}

pub fn exists(paths: &Paths) -> bool {
    paths.passwd_file().is_file()
}

/// Provision a password if there is not one already.
///
/// `root:minecraft` 0640 — the service account can read the secret and nobody
/// else can. The umask covers the window between creation and the chmod.
///
/// Idempotent, because either the package install or `mc install` can be the
/// first to see both a server and this plugin present. Toggling RCON off and on
/// restores the SAME secret rather than inventing a new one every time.
pub fn ensure(paths: &Paths) -> Result<bool> {
    let file = paths.passwd_file();
    if file.is_file() {
        return Ok(false);
    }
    let dir = paths.config_dir();
    std::fs::create_dir_all(&dir).at(&dir)?;

    let password = generate()?;
    write_secret(&file, &password)?;
    Ok(true)
}

fn write_secret(file: &Path, password: &str) -> Result<()> {
    // Written through a same-directory temp file with a restrictive mode from
    // the start, so the secret is never briefly world-readable.
    let owner = fsx::lookup_group(MC_USER)
        .and_then(|gid| fsx::lookup_user("root").map(|(uid, _)| (uid, gid)))
        .or_else(|| fsx::lookup_user(MC_USER));
    fsx::write_atomic(file, &format!("{password}\n"), owner, 0o640)?;
    fsx::apply_owner_mode(file, owner, 0o640)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_charset_needs_no_escaping_anywhere_downstream() {
        // This value is written into server.properties as `rcon.password=...`
        // unquoted, and passed to a server that parses it. Anything outside
        // base64url would need escaping in at least one of those places.
        for _ in 0..64 {
            let password = generate().unwrap();
            assert!(
                password
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "{password}"
            );
            assert!(!password.contains('='), "no padding: {password}");
            assert!(!password.contains('+'), "{password}");
            assert!(!password.contains('/'), "{password}");
        }
    }

    #[test]
    fn is_long_enough_to_be_worth_generating() {
        // 24 bytes -> 32 base64 characters, 192 bits of entropy.
        assert_eq!(generate().unwrap().len(), 32);
    }

    #[test]
    fn two_passwords_are_not_the_same() {
        assert_ne!(generate().unwrap(), generate().unwrap());
    }

    #[test]
    fn base64url_matches_the_reference_encoding() {
        // RFC 4648 test vectors, with padding stripped and the URL alphabet.
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(b"fooba"), "Zm9vYmE");
        assert_eq!(base64url(b"foobar"), "Zm9vYmFy");
        // The two bytes that differ between the standard and URL alphabets.
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn ensure_is_idempotent_and_never_rotates_a_live_secret() {
        // Toggling RCON off and on must restore the same secret; rotating it
        // would silently break anything holding the old one.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());

        assert!(ensure(&paths).unwrap(), "provisioned");
        let first = read(&paths).unwrap();

        assert!(!ensure(&paths).unwrap(), "already provisioned");
        assert_eq!(read(&paths).unwrap(), first);
    }

    #[test]
    fn the_stored_secret_is_not_world_readable() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());
        ensure(&paths).unwrap();

        let mode = fsx::mode_of(&paths.passwd_file()).unwrap();
        assert_eq!(mode & 0o007, 0, "mode {mode:o} grants access to other");
        assert_eq!(mode, 0o640);
    }

    #[test]
    fn an_empty_password_file_is_an_error_not_an_empty_password() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(paths.passwd_file(), "\n").unwrap();

        assert!(read(&paths).is_err());
    }
}
