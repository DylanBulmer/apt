//! Verifying a downloaded artifact against the hash its index published.

use std::io::Read;
use std::path::Path;

use sha1::Digest as _;

use crate::error::{Error, IoContext, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Sha1,
    Sha256,
    Sha512,
}

impl Algorithm {
    pub fn as_str(&self) -> &'static str {
        match self {
            Algorithm::Sha1 => "sha1",
            Algorithm::Sha256 => "sha256",
            Algorithm::Sha512 => "sha512",
        }
    }

    fn hex_len(&self) -> usize {
        match self {
            Algorithm::Sha1 => 40,
            Algorithm::Sha256 => 64,
            Algorithm::Sha512 => 128,
        }
    }
}

/// Hash a file, streaming rather than reading it whole: a modpack's server jar
/// is hundreds of megabytes and this runs on machines sized for a game server.
pub fn digest_file(path: &Path, algorithm: Algorithm) -> Result<String> {
    let mut file = std::fs::File::open(path).at(path)?;
    let mut buf = vec![0u8; 64 * 1024];

    macro_rules! stream {
        ($hasher:expr) => {{
            let mut hasher = $hasher;
            loop {
                let n = file.read(&mut buf).at(path)?;
                if n == 0 {
                    break;
                }
                match buf.get(..n) {
                    Some(chunk) => hasher.update(chunk),
                    None => break,
                }
            }
            format!("{:x}", hasher.finalize())
        }};
    }

    Ok(match algorithm {
        Algorithm::Sha1 => stream!(sha1::Sha1::new()),
        Algorithm::Sha256 => stream!(sha2::Sha256::new()),
        Algorithm::Sha512 => stream!(sha2::Sha512::new()),
    })
}

/// Verify a downloaded file, deleting it on any failure.
///
/// FAIL-CLOSED. An absent, empty, or literal-`"null"` expected hash is a
/// FAILURE, not a skip — those are exactly what an index returns when it does
/// not know the hash, and treating them as "nothing to check" installs an
/// unverified artifact. `null` in particular is what `jq -r` prints for a
/// missing JSON field, so it reached the shell version as a plausible-looking
/// string rather than as an empty one.
///
/// The file is removed on mismatch so a later run cannot mistake a rejected
/// download for a cached good one.
pub fn verify_file(path: &Path, expected: Option<&str>, algorithm: Algorithm) -> Result<()> {
    let expected = match expected.map(str::trim) {
        Some(h) if !h.is_empty() && h != "null" => h,
        _ => {
            let _ = std::fs::remove_file(path);
            return Err(Error::rejected(format!(
                "No {} hash published for {} — refusing to install an unverified artifact.",
                algorithm.as_str(),
                path.display()
            )));
        }
    };

    if expected.len() != algorithm.hex_len() || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        let _ = std::fs::remove_file(path);
        return Err(Error::rejected(format!(
            "Malformed {} hash for {}: {expected:?}",
            algorithm.as_str(),
            path.display()
        )));
    }

    let actual = digest_file(path, algorithm)?;
    if !actual.eq_ignore_ascii_case(expected) {
        let _ = std::fs::remove_file(path);
        return Err(Error::rejected(format!(
            "{} mismatch for {}: expected {expected}, got {actual}",
            algorithm.as_str(),
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sha1/256/512 of "abc", from the FIPS test vectors.
    const ABC_SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const ABC_SHA512: &str = "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
                              2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f";

    fn abc() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("artifact.jar");
        std::fs::write(&f, b"abc").unwrap();
        (dir, f)
    }

    #[test]
    fn digests_match_the_published_vectors() {
        let (_d, f) = abc();
        assert_eq!(digest_file(&f, Algorithm::Sha1).unwrap(), ABC_SHA1);
        assert_eq!(digest_file(&f, Algorithm::Sha256).unwrap(), ABC_SHA256);
        assert_eq!(
            digest_file(&f, Algorithm::Sha512).unwrap(),
            ABC_SHA512.replace(' ', "")
        );
    }

    #[test]
    fn accepts_a_correct_hash_in_either_case() {
        let (_d, f) = abc();
        verify_file(&f, Some(ABC_SHA256), Algorithm::Sha256).unwrap();
        verify_file(&f, Some(&ABC_SHA256.to_uppercase()), Algorithm::Sha256).unwrap();
        verify_file(&f, Some(&format!("  {ABC_SHA256}  ")), Algorithm::Sha256).unwrap();
        assert!(f.exists(), "a verified artifact is kept");
    }

    #[test]
    fn a_missing_hash_is_a_refusal_not_a_skip() {
        // `jq -r` prints "null" for an absent field, which is how this reached
        // the shell version looking like a real value.
        for absent in [None, Some(""), Some("   "), Some("null")] {
            let (_d, f) = abc();
            let err = verify_file(&f, absent, Algorithm::Sha512).unwrap_err();
            assert!(matches!(err, Error::Rejected(_)), "{absent:?} -> {err}");
            assert!(!f.exists(), "the unverified file must not be left behind");
        }
    }

    #[test]
    fn a_mismatch_deletes_the_download() {
        let (_d, f) = abc();
        let wrong = "0".repeat(64);
        assert!(verify_file(&f, Some(&wrong), Algorithm::Sha256).is_err());
        assert!(
            !f.exists(),
            "a rejected download must not be mistaken for a cached good one"
        );
    }

    #[test]
    fn a_malformed_hash_is_refused_before_it_is_compared() {
        for bad in ["deadbeef", "zzzz", &"a".repeat(63), &"a".repeat(65)] {
            let (_d, f) = abc();
            assert!(
                verify_file(&f, Some(bad), Algorithm::Sha256).is_err(),
                "{bad:?}"
            );
            assert!(!f.exists());
        }
    }
}
