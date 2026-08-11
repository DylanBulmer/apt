//! Fabric, via meta.fabricmc.net.
//!
//! THE ONLY SOURCE HERE WITH NO INDEPENDENT VERIFICATION. Fabric's
//! `/server/jar` endpoint is a dynamically-assembled launcher: it publishes no
//! sidecar hash and carries none in the meta JSON, so this download can only be
//! trusted via TLS. Every other source verifies a published digest and refuses
//! to install without one.
//!
//! If independent verification becomes a requirement, the route is the Fabric
//! *installer* jar from maven.fabricmc.net, which does ship `.sha512` sidecars —
//! at the cost of running an installer the way NeoForge does.

use std::path::Path;

use mc_common::error::{Error, Result};
use mc_common::http::Http;
use mc_common::ui;

use super::{FetchCtx, Source, latest_mojang_release};

const META: &str = "https://meta.fabricmc.net/v2";

pub struct Fabric;

/// The newest entry of a meta list, by a field path.
fn newest_field(body: &[u8], path: &[&str]) -> Option<String> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    let mut node = json.as_array()?.first()?;
    for key in path {
        node = node.get(key)?;
    }
    node.as_str().map(str::to_string)
}

impl Source for Fabric {
    fn resolve(&self, http: &dyn Http, requested: &str) -> Option<String> {
        if requested != "latest" {
            return Some(requested.to_string());
        }
        // Fabric follows Minecraft's own release train, so "latest" means the
        // latest Minecraft release rather than the latest loader.
        latest_mojang_release(http)
    }

    fn fetch(&self, ctx: &FetchCtx<'_>, version: &str, staging: &Path) -> Result<String> {
        let version = if version == "latest" {
            latest_mojang_release(ctx.http).ok_or_else(|| {
                Error::Network("Could not determine the latest Minecraft version.".into())
            })?
        } else {
            version.to_string()
        };
        mc_common::version::validate(&version, "Minecraft version")?;

        let loader = newest_field(
            &ctx.http.get(&format!("{META}/versions/loader/{version}"))?,
            &["loader", "version"],
        )
        .ok_or_else(|| {
            Error::config(format!(
                "Fabric publishes no loader for Minecraft {version}."
            ))
        })?;
        mc_common::version::validate(&loader, "Fabric loader version")?;

        let installer = newest_field(
            &ctx.http.get(&format!("{META}/versions/installer"))?,
            &["version"],
        )
        .ok_or_else(|| {
            Error::Network("Could not determine the Fabric installer version.".into())
        })?;
        mc_common::version::validate(&installer, "Fabric installer version")?;

        ui::info(format!("Downloading Fabric {version} (loader {loader})..."));
        let dest = staging.join("server.jar");
        ctx.http.download(
            &format!("{META}/versions/loader/{version}/{loader}/{installer}/server/jar"),
            &dest,
        )?;
        // Deliberately no verify_file here — see the module comment. This is
        // the one artifact upstream publishes no digest for.

        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    use super::super::MOJANG_MANIFEST;
    use super::*;
    use mc_common::http::fake::FakeHttp;

    fn http() -> FakeHttp {
        FakeHttp::new()
            .route(
                MOJANG_MANIFEST,
                r#"{"latest": {"release": "1.21.4"}, "versions": []}"#,
            )
            .route(
                format!("{META}/versions/loader/1.21.4"),
                r#"[{"loader": {"version": "0.16.10"}}, {"loader": {"version": "0.16.9"}}]"#,
            )
            .route(
                format!("{META}/versions/installer"),
                r#"[{"version": "1.0.1"}, {"version": "1.0.0"}]"#,
            )
            .route(
                format!("{META}/versions/loader/1.21.4/0.16.10/1.0.1/server/jar"),
                b"fabric-launcher".to_vec(),
            )
    }

    #[test]
    fn resolves_through_mojang_not_through_the_loader_train() {
        let http = http();
        assert_eq!(Fabric.resolve(&http, "latest"), Some("1.21.4".to_string()));
    }

    #[test]
    fn builds_the_url_from_the_newest_loader_and_installer() {
        let dir = tempfile::tempdir().unwrap();
        let http = http();
        let ctx = FetchCtx {
            http: &http,
            java_bin: None,
        };

        assert_eq!(Fabric.fetch(&ctx, "latest", dir.path()).unwrap(), "1.21.4");
        assert_eq!(
            std::fs::read(dir.path().join("server.jar")).unwrap(),
            b"fabric-launcher"
        );
    }

    #[test]
    fn a_minecraft_version_fabric_does_not_support_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let http = http().route(format!("{META}/versions/loader/1.99.9"), "[]");
        let ctx = FetchCtx {
            http: &http,
            java_bin: None,
        };

        let err = Fabric
            .fetch(&ctx, "1.99.9", dir.path())
            .unwrap_err()
            .to_string();
        assert!(err.contains("1.99.9"), "{err}");
    }

    #[test]
    fn a_hostile_loader_version_from_the_api_never_reaches_a_url() {
        // The loader and installer versions come from the network and are
        // interpolated into a URL path. A compromised or buggy meta service
        // must not be able to add path segments.
        let dir = tempfile::tempdir().unwrap();
        let http = http().route(
            format!("{META}/versions/loader/1.21.4"),
            r#"[{"loader": {"version": "../../../evil"}}]"#,
        );
        let ctx = FetchCtx {
            http: &http,
            java_bin: None,
        };

        let err = Fabric.fetch(&ctx, "1.21.4", dir.path()).unwrap_err();
        assert!(matches!(err, Error::Rejected(_)), "{err}");
    }
}
