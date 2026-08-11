//! Mojang's own server jar.

use std::path::Path;

use mc_common::error::{Error, Result};
use mc_common::hash::{Algorithm, verify_file};
use mc_common::http::Http;
use mc_common::ui;

use super::{FetchCtx, MOJANG_MANIFEST, Source, latest_mojang_release};

pub struct Vanilla;

impl Source for Vanilla {
    fn resolve(&self, http: &dyn Http, requested: &str) -> Option<String> {
        if requested != "latest" {
            return Some(requested.to_string());
        }
        latest_mojang_release(http)
    }

    fn fetch(&self, ctx: &FetchCtx<'_>, version: &str, staging: &Path) -> Result<String> {
        let manifest: serde_json::Value =
            serde_json::from_slice(&ctx.http.get(MOJANG_MANIFEST)?)
                .map_err(|e| Error::Network(format!("Mojang version manifest: {e}")))?;

        let version = if version == "latest" {
            manifest
                .get("latest")
                .and_then(|l| l.get("release"))
                .and_then(|r| r.as_str())
                .ok_or_else(|| Error::Network("Mojang manifest has no latest.release.".into()))?
                .to_string()
        } else {
            version.to_string()
        };
        // Re-validated after resolution, not only before: the value that
        // reaches a URL is this one, and it came from the network.
        mc_common::version::validate(&version, "Minecraft version")?;

        let version_url = manifest
            .get("versions")
            .and_then(|v| v.as_array())
            .and_then(|versions| {
                versions
                    .iter()
                    .find(|v| v.get("id").and_then(|i| i.as_str()) == Some(version.as_str()))
            })
            .and_then(|v| v.get("url"))
            .and_then(|u| u.as_str())
            .ok_or_else(|| {
                Error::config(format!(
                    "Minecraft version '{version}' not found in the manifest."
                ))
            })?;

        let meta: serde_json::Value = serde_json::from_slice(&ctx.http.get(version_url)?)
            .map_err(|e| Error::Network(format!("version metadata for {version}: {e}")))?;
        let server = meta
            .get("downloads")
            .and_then(|d| d.get("server"))
            .ok_or_else(|| {
                // Every version before 1.2.5 is client-only. A legible refusal
                // beats a null-pointer-shaped error from further down.
                Error::config(format!("Minecraft {version} publishes no server download."))
            })?;

        let url = server.get("url").and_then(|u| u.as_str()).ok_or_else(|| {
            Error::Network(format!("Minecraft {version} server download has no URL."))
        })?;
        let sha1 = server.get("sha1").and_then(|s| s.as_str());

        ui::info(format!("Downloading Vanilla {version}..."));
        let dest = staging.join("server.jar");
        ctx.http.download(url, &dest)?;
        verify_file(&dest, sha1, Algorithm::Sha1)?;

        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_common::http::fake::FakeHttp;

    /// Shape as Mojang publishes it, trimmed to the fields this reads.
    fn manifest() -> String {
        r#"{
          "latest": {"release": "1.21.4", "snapshot": "25w01a"},
          "versions": [
            {"id": "1.21.4", "url": "https://piston-meta.mojang.com/v1/packages/aaa/1.21.4.json"},
            {"id": "1.21.3", "url": "https://piston-meta.mojang.com/v1/packages/bbb/1.21.3.json"}
          ]
        }"#
        .to_string()
    }

    /// sha1 of the body registered below.
    const JAR_BODY: &[u8] = b"abc";
    const JAR_SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

    fn version_meta(sha1: &str) -> String {
        format!(
            r#"{{"downloads": {{"server": {{"url": "https://piston-data.mojang.com/server.jar", "sha1": "{sha1}"}}}}}}"#
        )
    }

    fn http(sha1: &str) -> FakeHttp {
        FakeHttp::new()
            .route(MOJANG_MANIFEST, manifest())
            .route(
                "https://piston-meta.mojang.com/v1/packages/aaa/1.21.4.json",
                version_meta(sha1),
            )
            .route(
                "https://piston-data.mojang.com/server.jar",
                JAR_BODY.to_vec(),
            )
    }

    #[test]
    fn resolves_latest_without_downloading_anything() {
        let http = FakeHttp::new().route(MOJANG_MANIFEST, manifest());
        assert_eq!(Vanilla.resolve(&http, "latest"), Some("1.21.4".to_string()));
        assert_eq!(http.requests(), vec![MOJANG_MANIFEST]);
    }

    #[test]
    fn a_pinned_version_needs_no_network_at_all() {
        let http = FakeHttp::new();
        assert_eq!(Vanilla.resolve(&http, "1.20.1"), Some("1.20.1".to_string()));
        assert!(
            http.requests().is_empty(),
            "resolution must not fetch for a pinned version"
        );
    }

    #[test]
    fn a_resolution_failure_is_not_fatal() {
        // Callers fall through to the real fetch, which reports the network
        // error properly rather than as "could not resolve".
        let http = FakeHttp::new().fail(MOJANG_MANIFEST, "connection refused");
        assert_eq!(Vanilla.resolve(&http, "latest"), None);
    }

    #[test]
    fn fetches_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let http = http(JAR_SHA1);
        let ctx = FetchCtx {
            http: &http,
            java_bin: None,
        };

        let version = Vanilla.fetch(&ctx, "latest", dir.path()).unwrap();
        assert_eq!(version, "1.21.4");
        assert_eq!(
            std::fs::read(dir.path().join("server.jar")).unwrap(),
            JAR_BODY
        );
    }

    #[test]
    fn a_tampered_jar_is_deleted_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let http = http(&"0".repeat(40));
        let ctx = FetchCtx {
            http: &http,
            java_bin: None,
        };

        assert!(Vanilla.fetch(&ctx, "1.21.4", dir.path()).is_err());
        assert!(
            !dir.path().join("server.jar").exists(),
            "a rejected artifact must not be left where a retry could reuse it"
        );
    }

    #[test]
    fn a_version_absent_from_the_manifest_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let http = http(JAR_SHA1);
        let ctx = FetchCtx {
            http: &http,
            java_bin: None,
        };

        let err = Vanilla
            .fetch(&ctx, "1.99.9", dir.path())
            .unwrap_err()
            .to_string();
        assert!(err.contains("1.99.9"), "{err}");
    }

    #[test]
    fn a_hostile_version_never_reaches_a_url() {
        let dir = tempfile::tempdir().unwrap();
        let http = http(JAR_SHA1);
        let ctx = FetchCtx {
            http: &http,
            java_bin: None,
        };

        let err = Vanilla
            .fetch(&ctx, "../../etc/passwd", dir.path())
            .unwrap_err();
        assert!(matches!(err, Error::Rejected(_)), "{err}");
    }
}
