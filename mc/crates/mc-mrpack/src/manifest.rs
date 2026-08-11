//! Parsing `modrinth.index.json`.
//!
//! EVERY VALUE HERE IS ATTACKER-CONTROLLED. A `.mrpack` is a file an operator
//! downloaded from the internet, and installing one runs as root. Versions
//! reach URLs and filenames, file paths reach a tree that is copied into
//! `MC_BASE`, and download URLs reach a fetcher. Each is validated at the point
//! it is parsed, not at the point it is used.

use mc_common::config::ServerType;
use mc_common::error::{Error, Result};

/// Hosts a pack is allowed to name.
///
/// Modrinth's own CDN only. A pack that wants a file from elsewhere is asking
/// this machine to fetch an arbitrary URL as root.
pub const ALLOWED_HOSTS: [&str; 2] = ["cdn.modrinth.com", "cdn-raw.modrinth.com"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackFile {
    /// Where it goes, relative to the server directory. Already validated.
    pub path: String,
    pub url: String,
    pub sha512: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub minecraft_version: String,
    pub server_type: ServerType,
    /// Present only for NeoForge, whose version is separate from Minecraft's.
    pub loader_version: Option<String>,
    pub files: Vec<PackFile>,
}

pub fn parse(json: &str) -> Result<Manifest> {
    let root: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| Error::rejected(format!("modrinth.index.json: {e}")))?;

    // Refused before anything else is read: a future format may move every
    // field this code depends on, and guessing would be worse than refusing.
    match root.get("formatVersion").and_then(|v| v.as_u64()) {
        Some(1) => {}
        other => {
            return Err(Error::rejected(format!(
                "Unsupported .mrpack formatVersion: {} (only version 1 is supported)",
                other
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "absent".into())
            )));
        }
    }

    let dependencies = root
        .get("dependencies")
        .ok_or_else(|| Error::rejected("modrinth.index.json has no dependencies block."))?;

    let minecraft_version = dependencies
        .get("minecraft")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::rejected("The pack does not name a Minecraft version."))?
        .to_string();
    // VALIDATED IMMEDIATELY, before this value is used for anything at all.
    mc_common::version::validate(&minecraft_version, "Minecraft version")?;

    let (server_type, loader_version) = if dependencies.get("fabric-loader").is_some() {
        (ServerType::Fabric, None)
    } else if let Some(version) = dependencies.get("neoforge").and_then(|v| v.as_str()) {
        mc_common::version::validate(version, "NeoForge version")?;
        (ServerType::Neoforge, Some(version.to_string()))
    } else if dependencies.get("forge").is_some() {
        return Err(Error::config("Forge server type is not supported yet."));
    } else if dependencies.get("quilt-loader").is_some() {
        return Err(Error::config("Quilt server type is not supported yet."));
    } else {
        (ServerType::Vanilla, None)
    };

    let mut files = Vec::new();
    if let Some(entries) = root.get("files").and_then(|f| f.as_array()) {
        for entry in entries {
            // A pack marks client-only files `unsupported` on the server side.
            // An absent `env` means required, which is the conservative reading.
            let server_env = entry
                .get("env")
                .and_then(|e| e.get("server"))
                .and_then(|s| s.as_str())
                .unwrap_or("required");
            if server_env == "unsupported" {
                continue;
            }

            let path = entry
                .get("path")
                .and_then(|p| p.as_str())
                .ok_or_else(|| Error::rejected("A pack file entry has no path."))?;
            // A malicious pack can set an arbitrary path
            // (`../../../../etc/cron.d/x`); the staged tree is copied into
            // MC_BASE as root, so an unchecked path is an arbitrary root write.
            mc_common::staging::safe_relative_path(path)?;

            let url = entry
                .get("downloads")
                .and_then(|d| d.as_array())
                .and_then(|d| d.first())
                .and_then(|u| u.as_str())
                .ok_or_else(|| {
                    Error::rejected(format!("Pack file {path:?} has no download URL."))
                })?;
            if !mc_common::http::host_allowed(url, &ALLOWED_HOSTS) {
                return Err(Error::rejected(format!(
                    "Pack file {path:?} names a URL outside the allowlist: {url:?}"
                )));
            }

            let sha512 = entry
                .get("hashes")
                .and_then(|h| h.get("sha512"))
                .and_then(|s| s.as_str())
                .ok_or_else(|| {
                    // Fail-closed: no hash means no way to know what was
                    // downloaded, and this file lands in a directory the server
                    // loads code from.
                    Error::rejected(format!("Pack file {path:?} publishes no sha512."))
                })?;

            files.push(PackFile {
                path: path.to_string(),
                url: url.to_string(),
                sha512: sha512.to_string(),
            });
        }
    }

    Ok(Manifest {
        minecraft_version,
        server_type,
        loader_version,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(files: &str, dependencies: &str) -> String {
        format!(r#"{{"formatVersion": 1, "dependencies": {dependencies}, "files": [{files}]}}"#)
    }

    const VANILLA: &str = r#"{"minecraft": "1.21.4"}"#;

    fn good_file(path: &str) -> String {
        format!(
            r#"{{"path": "{path}",
               "downloads": ["https://cdn.modrinth.com/data/x/y.jar"],
               "hashes": {{"sha512": "{}"}}}}"#,
            "a".repeat(128)
        )
    }

    #[test]
    fn reads_an_ordinary_pack() {
        let manifest = parse(&pack(&good_file("mods/sodium.jar"), VANILLA)).unwrap();
        assert_eq!(manifest.minecraft_version, "1.21.4");
        assert_eq!(manifest.server_type, ServerType::Vanilla);
        assert_eq!(manifest.files.len(), 1);
    }

    #[test]
    fn detects_the_loader() {
        let fabric = parse(&pack(
            "",
            r#"{"minecraft": "1.21.4", "fabric-loader": "0.16.10"}"#,
        ))
        .unwrap();
        assert_eq!(fabric.server_type, ServerType::Fabric);

        let neo = parse(&pack(
            "",
            r#"{"minecraft": "1.21.4", "neoforge": "21.4.19"}"#,
        ))
        .unwrap();
        assert_eq!(neo.server_type, ServerType::Neoforge);
        assert_eq!(neo.loader_version.as_deref(), Some("21.4.19"));
    }

    #[test]
    fn refuses_loaders_that_are_not_supported_yet() {
        for dependencies in [
            r#"{"minecraft": "1.21.4", "forge": "50.0.0"}"#,
            r#"{"minecraft": "1.21.4", "quilt-loader": "0.26.0"}"#,
        ] {
            let err = parse(&pack("", dependencies)).unwrap_err().to_string();
            assert!(err.contains("not supported yet"), "{err}");
        }
    }

    #[test]
    fn refuses_a_format_version_it_does_not_understand() {
        for json in [
            r#"{"formatVersion": 2, "dependencies": {"minecraft": "1.21.4"}}"#,
            r#"{"dependencies": {"minecraft": "1.21.4"}}"#,
        ] {
            assert!(parse(json).is_err(), "{json}");
        }
    }

    #[test]
    fn refuses_a_path_that_escapes_the_server_directory() {
        for path in [
            "../../../../etc/cron.d/x",
            "/etc/passwd",
            "~/.ssh/authorized_keys",
            "mods/../../../root/.bashrc",
        ] {
            let err = parse(&pack(&good_file(path), VANILLA));
            assert!(err.is_err(), "{path} should be refused");
        }
    }

    #[test]
    fn refuses_a_download_from_outside_the_allowlist() {
        let entry = format!(
            r#"{{"path": "mods/x.jar", "downloads": ["https://evil.test/x.jar"],
                "hashes": {{"sha512": "{}"}}}}"#,
            "a".repeat(128)
        );
        let err = parse(&pack(&entry, VANILLA)).unwrap_err().to_string();
        assert!(err.contains("allowlist"), "{err}");
    }

    #[test]
    fn refuses_the_option_injection_url_that_defeated_the_shell_version() {
        // It found "https://" anywhere in the string, extracted an allowed
        // host, and handed the whole value to curl as an argv element starting
        // with '-'.
        let entry = format!(
            r#"{{"path": "mods/x.jar",
                "downloads": ["-Ksomefile#https://cdn.modrinth.com/x"],
                "hashes": {{"sha512": "{}"}}}}"#,
            "a".repeat(128)
        );
        assert!(parse(&pack(&entry, VANILLA)).is_err());
    }

    #[test]
    fn refuses_a_lookalike_host() {
        let entry = format!(
            r#"{{"path": "mods/x.jar",
                "downloads": ["https://cdn.modrinth.com.evil.test/x.jar"],
                "hashes": {{"sha512": "{}"}}}}"#,
            "a".repeat(128)
        );
        assert!(parse(&pack(&entry, VANILLA)).is_err());
    }

    #[test]
    fn refuses_a_file_with_no_published_hash() {
        // These land in a directory the server loads code from.
        let entry = r#"{"path": "mods/x.jar", "downloads": ["https://cdn.modrinth.com/x.jar"], "hashes": {}}"#;
        assert!(parse(&pack(entry, VANILLA)).is_err());
    }

    #[test]
    fn a_hostile_minecraft_version_is_refused_before_it_is_used() {
        // In the shell version this value reached an arithmetic context, which
        // performs command substitution inside array subscripts.
        for version in ["PATH[$(touch /tmp/mrpack-canary)]", "../../evil", "-Kfile"] {
            let dependencies = format!(r#"{{"minecraft": "{version}"}}"#);
            assert!(parse(&pack("", &dependencies)).is_err(), "{version}");
        }
        assert!(!std::path::Path::new("/tmp/mrpack-canary").exists());
    }

    #[test]
    fn client_only_files_are_skipped_but_unmarked_ones_are_not() {
        let client_only = format!(
            r#"{{"path": "mods/client.jar", "env": {{"server": "unsupported"}},
                "downloads": ["https://cdn.modrinth.com/x.jar"],
                "hashes": {{"sha512": "{}"}}}}"#,
            "a".repeat(128)
        );
        let entries = format!("{client_only}, {}", good_file("mods/server.jar"));
        let manifest = parse(&pack(&entries, VANILLA)).unwrap();
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].path, "mods/server.jar");
    }
}
