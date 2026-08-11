//! PaperMC, via the v3 "fill" API.
//!
//! v2 is SUNSET — `api.papermc.io/v2` returns HTTP 410 with
//! `{"ok":false,"error":"sunset"}`. v3 lives on a different host and has a
//! different shape, and the difference drives the awkward indexing below:
//!
//!   * `.versions` is an OBJECT keyed by release family
//!     (`"1.21": ["1.21.4", "1.21.3"]`), newest family first and newest version
//!     first within it. Hence "flatten, then take the first" rather than an
//!     index into a flat array.
//!   * builds are a bare array, newest first.
//!   * each build carries a ready-made download URL on a *separate* host
//!     (`fill-data.papermc.io`), so the URL is used as given rather than built
//!     from a filename.

use std::path::Path;

use mc_common::error::{Error, Result};
use mc_common::hash::{Algorithm, verify_file};
use mc_common::http::Http;
use mc_common::ui;

use super::{FetchCtx, Source};

const API: &str = "https://fill.papermc.io/v3/projects/paper";

pub struct Paper;

/// Flatten `.versions` and take the newest.
///
/// DEPENDS ON JSON DOCUMENT ORDER, which is why this crate enables
/// `serde_json/preserve_order`. Without it `Value`'s map is a `BTreeMap` and
/// sorts its keys as strings, so the `"1.20"` family sorts before `"1.21"` and
/// "newest" silently becomes "alphabetically first" — an installable, verified,
/// wrong version. The API documents newest-first ordering and there is nothing
/// else to sort on: comparing Minecraft versions numerically has to cope with
/// `1.21.4`, `24w45a` and `26.2` in one ordering.
fn newest_version(project: &serde_json::Value) -> Option<String> {
    project
        .get("versions")?
        .as_object()?
        .values()
        .filter_map(|family| family.as_array())
        .flatten()
        .find_map(|v| v.as_str())
        .map(str::to_string)
}

impl Source for Paper {
    fn resolve(&self, http: &dyn Http, requested: &str) -> Option<String> {
        if requested != "latest" {
            return Some(requested.to_string());
        }
        let project: serde_json::Value = serde_json::from_slice(&http.get(API).ok()?).ok()?;
        newest_version(&project)
    }

    fn fetch(&self, ctx: &FetchCtx<'_>, version: &str, staging: &Path) -> Result<String> {
        let version = if version == "latest" {
            let project: serde_json::Value = serde_json::from_slice(&ctx.http.get(API)?)
                .map_err(|e| Error::Network(format!("Paper project index: {e}")))?;
            newest_version(&project).ok_or_else(|| {
                Error::Network("Could not determine the latest Paper version.".into())
            })?
        } else {
            version.to_string()
        };
        mc_common::version::validate(&version, "Minecraft version")?;

        let builds: serde_json::Value =
            serde_json::from_slice(&ctx.http.get(&format!("{API}/versions/{version}/builds"))?)
                .map_err(|e| Error::Network(format!("Paper builds for {version}: {e}")))?;
        let builds = builds
            .as_array()
            .ok_or_else(|| Error::Network(format!("Paper builds for {version} is not a list.")))?;

        // Prefer the newest STABLE build, but fall back to the newest of any
        // channel so a version that has only experimental builds is still
        // installable — v2 had no channel concept and this has always taken
        // whatever was newest.
        let selected = builds
            .iter()
            .find(|b| b.get("channel").and_then(|c| c.as_str()) == Some("STABLE"))
            .or_else(|| builds.first())
            .ok_or_else(|| Error::config(format!("No Paper builds available for {version}.")))?;

        let build_id = selected.get("id").and_then(|i| i.as_u64()).unwrap_or(0);
        let download = selected
            .get("downloads")
            .and_then(|d| d.get("server:default"))
            .ok_or_else(|| {
                Error::Network(format!(
                    "Paper build {build_id} for {version} has no server download."
                ))
            })?;
        let url = download
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| {
                Error::Network(format!("Paper build {build_id} has no download URL."))
            })?;
        let sha256 = download
            .get("checksums")
            .and_then(|c| c.get("sha256"))
            .and_then(|s| s.as_str());

        ui::info(format!("Downloading Paper {version} build {build_id}..."));
        let dest = staging.join("server.jar");
        ctx.http.download(url, &dest)?;
        verify_file(&dest, sha256, Algorithm::Sha256)?;

        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_common::http::fake::FakeHttp;

    const JAR_BODY: &[u8] = b"abc";
    const JAR_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const JAR_URL: &str = "https://fill-data.papermc.io/paper-1.21.4-100.jar";

    /// The v3 shape: `.versions` keyed by family, newest first.
    fn project() -> String {
        r#"{"versions": {"1.21": ["1.21.4", "1.21.3"], "1.20": ["1.20.6"]}}"#.to_string()
    }

    fn builds(channel: &str) -> String {
        format!(
            r#"[
              {{"id": 100, "channel": "{channel}",
               "downloads": {{"server:default": {{"url": "{JAR_URL}",
                 "checksums": {{"sha256": "{JAR_SHA256}"}}}}}}}},
              {{"id": 99, "channel": "STABLE",
               "downloads": {{"server:default": {{"url": "https://fill-data.papermc.io/old.jar",
                 "checksums": {{"sha256": "{JAR_SHA256}"}}}}}}}}
            ]"#
        )
    }

    fn http(channel: &str) -> FakeHttp {
        FakeHttp::new()
            .route(API, project())
            .route(format!("{API}/versions/1.21.4/builds"), builds(channel))
            .route(JAR_URL, JAR_BODY.to_vec())
            .route("https://fill-data.papermc.io/old.jar", JAR_BODY.to_vec())
    }

    #[test]
    fn flattens_the_family_keyed_version_object() {
        // The v3 shape that made the shell version's jq awkward: `.versions` is
        // an object of arrays, not a flat list, so "newest" is the first entry
        // of the first family rather than an index into an array.
        let http = FakeHttp::new().route(API, project());
        assert_eq!(Paper.resolve(&http, "latest"), Some("1.21.4".to_string()));
    }

    #[test]
    fn newest_means_document_order_not_alphabetical_order() {
        // Guards `serde_json/preserve_order`. Without that feature the map is a
        // BTreeMap and sorts its keys as strings, so "1.20" wins over "1.21"
        // and the wrong version installs — verified, successfully, silently.
        // The fixture below is ordered exactly as the API returns it.
        let ordered = r#"{"versions": {"1.9": ["1.9.4"], "1.21": ["1.21.4"], "1.10": ["1.10.2"]}}"#;
        let project: serde_json::Value = serde_json::from_str(ordered).unwrap();
        assert_eq!(newest_version(&project), Some("1.9.4".to_string()));
    }

    #[test]
    fn prefers_a_stable_build() {
        let dir = tempfile::tempdir().unwrap();
        let http = http("EXPERIMENTAL");
        let ctx = FetchCtx {
            http: &http,
            java_bin: None,
        };

        Paper.fetch(&ctx, "1.21.4", dir.path()).unwrap();
        // Build 100 is EXPERIMENTAL here, so 99 (STABLE) must be chosen.
        assert!(
            http.requests()
                .contains(&"https://fill-data.papermc.io/old.jar".to_string())
        );
        assert!(!http.requests().contains(&JAR_URL.to_string()));
    }

    #[test]
    fn falls_back_to_the_newest_build_of_any_channel() {
        // A version with only experimental builds must still be installable.
        let dir = tempfile::tempdir().unwrap();
        let only_experimental = format!(
            r#"[{{"id": 100, "channel": "EXPERIMENTAL",
              "downloads": {{"server:default": {{"url": "{JAR_URL}",
                "checksums": {{"sha256": "{JAR_SHA256}"}}}}}}}}]"#
        );
        let http = FakeHttp::new()
            .route(API, project())
            .route(format!("{API}/versions/1.21.4/builds"), only_experimental)
            .route(JAR_URL, JAR_BODY.to_vec());
        let ctx = FetchCtx {
            http: &http,
            java_bin: None,
        };

        assert_eq!(Paper.fetch(&ctx, "1.21.4", dir.path()).unwrap(), "1.21.4");
    }

    #[test]
    fn verifies_the_published_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let wrong = format!(
            r#"[{{"id": 100, "channel": "STABLE",
              "downloads": {{"server:default": {{"url": "{JAR_URL}",
                "checksums": {{"sha256": "{}"}}}}}}}}]"#,
            "0".repeat(64)
        );
        let http = FakeHttp::new()
            .route(API, project())
            .route(format!("{API}/versions/1.21.4/builds"), wrong)
            .route(JAR_URL, JAR_BODY.to_vec());
        let ctx = FetchCtx {
            http: &http,
            java_bin: None,
        };

        assert!(Paper.fetch(&ctx, "1.21.4", dir.path()).is_err());
        assert!(!dir.path().join("server.jar").exists());
    }

    #[test]
    fn a_build_with_no_checksum_is_refused() {
        // Fail-closed: an index that does not publish a hash must not yield an
        // installed-but-unverified jar.
        let dir = tempfile::tempdir().unwrap();
        let no_hash = format!(
            r#"[{{"id": 100, "channel": "STABLE",
              "downloads": {{"server:default": {{"url": "{JAR_URL}", "checksums": {{}}}}}}}}]"#
        );
        let http = FakeHttp::new()
            .route(API, project())
            .route(format!("{API}/versions/1.21.4/builds"), no_hash)
            .route(JAR_URL, JAR_BODY.to_vec());
        let ctx = FetchCtx {
            http: &http,
            java_bin: None,
        };

        assert!(Paper.fetch(&ctx, "1.21.4", dir.path()).is_err());
    }
}
