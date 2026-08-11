//! Minecraft's End User Licence Agreement gate.
//!
//! Consent is never implicit and never a side effect of installing. It comes
//! from exactly one of two places: `--accept-eula`, or an interactive yes.
//!
//! Deliberately separate from `--yes`. That flag consents to installing a
//! package; this one consents to a licence. Folding them together would let
//! someone accept a legal agreement by asking for a JRE.

use std::path::Path;

use crate::error::{IoContext, Result};
use crate::fsx;
use crate::paths::{MC_USER, Paths};

pub const EULA_URL: &str = "https://www.minecraft.net/eula";

/// True when `eula.txt` records acceptance.
///
/// Mojang's own file is a comment header followed by `eula=true`, and operators
/// edit it by hand, so surrounding whitespace and `TRUE`/`True` are tolerated.
/// Anything else — absent file, `eula=false`, a value commented out — is a
/// refusal: this decides whether a licence was accepted, so it FAILS CLOSED.
pub fn accepted(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|line| {
        let line = line.trim();
        if line.starts_with('#') {
            return false;
        }
        match line.split_once('=') {
            Some((key, value)) => {
                key.trim().eq_ignore_ascii_case("eula") && value.trim().eq_ignore_ascii_case("true")
            }
            None => false,
        }
    })
}

/// Record acceptance in `$MC_BASE/eula.txt`.
///
/// No-ops when already accepted, so reinstalls and upgrades never re-prompt.
pub fn accept(paths: &Paths) -> Result<()> {
    let file = paths.eula();
    if accepted(&file) {
        return Ok(());
    }

    let base = paths.base();
    std::fs::create_dir_all(&base).at(&base)?;

    let body = format!("# Accepted through mc.\n# {EULA_URL}\neula=true\n");
    std::fs::write(&file, body).at(&file)?;
    // The JVM rewrites this file when it disagrees with it, so it has to be
    // writable by the service account like everything else in MC_BASE.
    fsx::apply_owner_mode(&file, fsx::lookup_user(MC_USER), 0o644)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("eula.txt");
        std::fs::write(&f, text).unwrap();
        (dir, f)
    }

    #[test]
    fn accepts_what_an_operator_plausibly_types() {
        for text in [
            "eula=true\n",
            "eula=TRUE\n",
            "eula=True\n",
            "  eula = true  \n",
            "#By changing the setting below to TRUE you agree\neula=true\n",
        ] {
            let (_d, f) = write(text);
            assert!(accepted(&f), "should accept {text:?}");
        }
    }

    #[test]
    fn fails_closed_on_everything_else() {
        for text in [
            "eula=false\n",
            "#eula=true\n",
            "eula=true extra\n",
            "eulaX=true\n",
            "",
            "true\n",
        ] {
            let (_d, f) = write(text);
            assert!(!accepted(&f), "should refuse {text:?}");
        }
        assert!(
            !accepted(Path::new("/nonexistent/eula.txt")),
            "absent file is a refusal"
        );
    }

    #[test]
    fn accept_is_idempotent_and_does_not_re_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());
        accept(&paths).unwrap();
        assert!(accepted(&paths.eula()));

        // An operator's own comment header survives a second accept: the file
        // is left entirely alone once it already says yes.
        std::fs::write(paths.eula(), "# hand written\neula=true\n").unwrap();
        accept(&paths).unwrap();
        assert_eq!(
            std::fs::read_to_string(paths.eula()).unwrap(),
            "# hand written\neula=true\n"
        );
    }
}
