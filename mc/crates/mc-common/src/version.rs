//! Validation for version strings that end up in URLs and filenames.

use crate::error::{Error, Result};

/// Reject a version string before it is interpolated into a download URL or a
/// path.
///
/// Real versions look like `1.21.4`, `24w45a`, `21.1.66`, `21.4.0-beta`, or the
/// literal `latest`. The charset excludes `/` so a malicious `.mrpack` cannot
/// smuggle extra URL path segments (`../../evil`) into a fetch, and excludes
/// everything that could be read as a URL delimiter or a shell metacharacter by
/// something downstream.
pub fn validate(version: &str, what: &str) -> Result<()> {
    if version.is_empty() {
        return Err(Error::rejected(format!("{what} must not be empty.")));
    }
    // Bounded so a manifest cannot hand a caller a megabyte-long "version" that
    // ends up in an error message, a filename, or a URL.
    if version.len() > 64 {
        return Err(Error::rejected(format!(
            "{what} is implausibly long: {} bytes.",
            version.len()
        )));
    }
    if !version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '+')
    {
        return Err(Error::rejected(format!(
            "{what} contains characters that are not allowed in a version: {version:?}"
        )));
    }
    // Even within the charset, a leading '-' would be read as an option by
    // anything taking this as an argument, and `..` is a path component.
    if version.starts_with('-') {
        return Err(Error::rejected(format!(
            "{what} must not start with '-': {version:?}"
        )));
    }
    if version.contains("..") {
        return Err(Error::rejected(format!(
            "{what} must not contain '..': {version:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_versions_upstreams_actually_publish() {
        for good in [
            "1.21.4",
            "24w45a",
            "21.1.66",
            "21.4.0-beta",
            "latest",
            "26.2",
            "1.20.5",
            "0.16.10",
            "26.2.0.54-beta",
        ] {
            assert!(
                validate(good, "version").is_ok(),
                "{good} should be accepted"
            );
        }
    }

    #[test]
    fn rejects_anything_that_could_leave_the_url_path() {
        for bad in [
            "../../evil",
            "1.21/../../etc/passwd",
            "a/b",
            "1.21.4?x=1",
            "1.21.4#frag",
            "https://evil/",
            "1.21.4 ",
            " 1.21.4",
            "1.21.4;id",
            "$(id)",
            "`id`",
            "1.21.4\nx",
            "",
        ] {
            assert!(
                validate(bad, "version").is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn rejects_a_value_that_would_be_read_as_an_option() {
        // curl and every other fetcher parses a leading '-' as a flag; the
        // shell version was caught by exactly this shape once already.
        assert!(validate("-K/etc/passwd", "version").is_err());
        assert!(validate("--version", "version").is_err());
    }

    #[test]
    fn rejects_an_implausibly_long_value() {
        assert!(validate(&"1".repeat(65), "version").is_err());
        assert!(validate(&"1".repeat(64), "version").is_ok());
    }
}
