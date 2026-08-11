//! NeoForge, via its Maven repository.
//!
//! The odd one out: NeoForge ships an *installer* jar rather than a
//! ready-to-run server, and running it produces a whole tree — `run.sh`,
//! `libraries/`, `user_jvm_args.txt` — instead of a single `server.jar`. That
//! is why [`Layout::Tree`] exists and why `mc serve` has a `run.sh` branch.
//!
//! THE INSTALLER IS EXECUTED, so it is verified against the SHA-512 published
//! beside it in Maven before it is run. This is the one artifact in the tree
//! that becomes code on the machine at install time rather than at boot.

use std::path::Path;
use std::process::Command;

use mc_common::error::{Error, IoContext, Result};
use mc_common::hash::{Algorithm, verify_file};
use mc_common::http::Http;
use mc_common::ui;

use super::{FetchCtx, Layout, Source};

const MAVEN: &str = "https://maven.neoforged.net/releases/net/neoforged/neoforge";

pub struct Neoforge;

/// Pull `<latest>` out of a Maven `maven-metadata.xml`.
///
/// A deliberately small extraction rather than an XML parser: this reads one
/// element from a document with a fixed shape, and the value is validated
/// against the version charset immediately afterwards regardless.
fn latest_from_metadata(xml: &str) -> Option<String> {
    let start = xml.find("<latest>")? + "<latest>".len();
    let rest = xml.get(start..)?;
    let end = rest.find("</latest>")?;
    let value = rest.get(..end)?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

impl Source for Neoforge {
    fn layout(&self) -> Layout {
        Layout::Tree
    }

    fn resolve(&self, http: &dyn Http, requested: &str) -> Option<String> {
        if requested != "latest" {
            return Some(requested.to_string());
        }
        let xml = http
            .get_string(&format!("{MAVEN}/maven-metadata.xml"))
            .ok()?;
        latest_from_metadata(&xml)
    }

    fn fetch(&self, ctx: &FetchCtx<'_>, version: &str, staging: &Path) -> Result<String> {
        let version = if version == "latest" {
            let xml = ctx
                .http
                .get_string(&format!("{MAVEN}/maven-metadata.xml"))?;
            latest_from_metadata(&xml).ok_or_else(|| {
                Error::Network("Could not determine the latest NeoForge version.".into())
            })?
        } else {
            version.to_string()
        };
        // The resolved version is interpolated into a URL and a filename, and it
        // may equally have come from a modpack manifest. Validate before either.
        mc_common::version::validate(&version, "NeoForge version")?;

        let installer_url = format!("{MAVEN}/{version}/neoforge-{version}-installer.jar");

        // Staged INSIDE the staging dir, so the guard that owns that directory
        // removes the installer too if anything below fails. Deleted explicitly
        // before returning so it is not copied into MC_BASE with the tree.
        let installer = staging.join(".neoforge-installer.jar");

        ui::info(format!("Downloading NeoForge {version} installer..."));
        ctx.http.download(&installer_url, &installer)?;

        let published = ctx.http.get_string(&format!("{installer_url}.sha512"))?;
        // Maven sidecars are sometimes `<hash>  <filename>`.
        let expected = published.split_whitespace().next().unwrap_or_default();
        verify_file(&installer, Some(expected), Algorithm::Sha512)?;

        ui::info("Running NeoForge installer...");
        let java = ctx.java_bin.unwrap_or(Path::new("java"));
        let status = Command::new(java)
            .arg("-jar")
            .arg(&installer)
            .arg("--installServer")
            .arg(staging)
            .status()
            .map_err(|e| Error::other(format!("running the NeoForge installer: {e}")))?;
        if !status.success() {
            return Err(Error::other(format!(
                "The NeoForge installer failed ({status})."
            )));
        }

        let run_sh = staging.join("run.sh");
        if !run_sh.is_file() {
            return Err(Error::other(
                "The NeoForge installer finished but did not create run.sh.".to_string(),
            ));
        }
        mc_common::fsx::apply_owner_mode(&run_sh, None, 0o755)?;

        std::fs::remove_file(&installer).at(&installer)?;

        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_common::http::fake::FakeHttp;

    const METADATA: &str =
        "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml";

    fn metadata_xml() -> String {
        r#"<?xml version="1.0" encoding="UTF-8"?>
        <metadata>
          <groupId>net.neoforged</groupId>
          <versioning>
            <latest>21.4.20-beta</latest>
            <release>21.4.19</release>
          </versioning>
        </metadata>"#
            .to_string()
    }

    #[test]
    fn reads_latest_out_of_maven_metadata() {
        let http = FakeHttp::new().route(METADATA, metadata_xml());
        assert_eq!(
            Neoforge.resolve(&http, "latest"),
            Some("21.4.20-beta".to_string())
        );
    }

    #[test]
    fn metadata_without_a_latest_element_resolves_to_nothing() {
        assert_eq!(latest_from_metadata("<metadata></metadata>"), None);
        assert_eq!(latest_from_metadata("<latest></latest>"), None);
        assert_eq!(latest_from_metadata(""), None);
    }

    #[test]
    fn installs_into_a_tree_not_a_single_jar() {
        // Drives `mc serve`'s run.sh branch and `server_installed`.
        assert_eq!(Neoforge.layout(), Layout::Tree);
    }

    #[test]
    fn refuses_to_execute_an_installer_that_fails_verification() {
        // This artifact becomes code on the machine, so the hash gate is the
        // one that matters most in the tree.
        let dir = tempfile::tempdir().unwrap();
        let url = format!("{MAVEN}/21.4.19/neoforge-21.4.19-installer.jar");
        let http = FakeHttp::new()
            .route(&url, b"pretend-installer".to_vec())
            .route(format!("{url}.sha512"), "0".repeat(128));
        let ctx = FetchCtx {
            http: &http,
            java_bin: None,
        };

        let err = Neoforge.fetch(&ctx, "21.4.19", dir.path()).unwrap_err();
        assert!(matches!(err, Error::Rejected(_)), "{err}");
        assert!(
            !dir.path().join(".neoforge-installer.jar").exists(),
            "an unverified installer must not be left on disk"
        );
    }

    #[test]
    fn a_maven_sidecar_with_a_trailing_filename_is_parsed() {
        // Maven publishes `<hash>  <filename>` in places.
        let dir = tempfile::tempdir().unwrap();
        let url = format!("{MAVEN}/21.4.19/neoforge-21.4.19-installer.jar");
        // sha512 of "abc"
        let sha = "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
                   2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
            .replace(' ', "");
        let http = FakeHttp::new().route(&url, b"abc".to_vec()).route(
            format!("{url}.sha512"),
            format!("{sha}  neoforge-installer.jar\n"),
        );
        let ctx = FetchCtx {
            http: &http,
            java_bin: Some(Path::new("/nonexistent/java")),
        };

        // The hash passes; the run then fails because there is no JVM here.
        // That is the boundary this test cares about — verification happened
        // before anything was executed.
        let err = Neoforge.fetch(&ctx, "21.4.19", dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("running the NeoForge installer"),
            "should have got past verification: {err}"
        );
    }

    #[test]
    fn a_hostile_version_never_reaches_a_url() {
        let dir = tempfile::tempdir().unwrap();
        let http = FakeHttp::new();
        let ctx = FetchCtx {
            http: &http,
            java_bin: None,
        };

        for hostile in ["../../evil", "-Kfile", "a/b"] {
            let err = Neoforge.fetch(&ctx, hostile, dir.path()).unwrap_err();
            assert!(matches!(err, Error::Rejected(_)), "{hostile}: {err}");
        }
        assert!(
            http.requests().is_empty(),
            "nothing should have been fetched"
        );
    }
}
