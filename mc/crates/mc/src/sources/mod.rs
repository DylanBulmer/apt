//! Where a server artifact comes from.
//!
//! Core knows how to fetch four of these itself. A fifth kind — a `.mrpack`
//! modpack — arrives through a plugin-provided *source provider*, which is the
//! same shape seen from the outside: resolve a version, populate a staging
//! directory, report what was installed. Adding Forge or Quilt later is a new
//! module here; it is not a change to install or upgrade.

use std::path::Path;

use mc_common::config::ServerType;
use mc_common::error::Result;
use mc_common::http::Http;

pub mod fabric;
pub mod neoforge;
pub mod paper;
pub mod vanilla;

/// What a fetched artifact looks like on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// A single `server.jar`.
    Jar,
    /// A whole tree with a `run.sh` launcher — NeoForge's installer produces
    /// this, and `mc serve` launches it differently as a result.
    Tree,
}

/// Everything a fetch needs beyond the staging directory.
pub struct FetchCtx<'a> {
    pub http: &'a dyn Http,
    /// Used by sources that must execute an installer. `None` falls back to
    /// whatever `java` is on PATH.
    pub java_bin: Option<&'a Path>,
}

pub trait Source {
    /// Resolve `latest` to a concrete version WITHOUT downloading anything.
    ///
    /// Exists so an upgrade can tell it is a no-op *before* paying for a backup
    /// and the downtime of a stop. Returns `None` when resolution failed, which
    /// is deliberately NOT fatal: the caller falls through to the real fetch,
    /// which reports the network error properly.
    fn resolve(&self, http: &dyn Http, requested: &str) -> Option<String>;

    /// Populate `staging` and return the version actually installed.
    fn fetch(&self, ctx: &FetchCtx<'_>, version: &str, staging: &Path) -> Result<String>;

    fn layout(&self) -> Layout {
        Layout::Jar
    }
}

pub fn for_type(server_type: ServerType) -> Box<dyn Source> {
    match server_type {
        ServerType::Vanilla => Box::new(vanilla::Vanilla),
        ServerType::Paper => Box::new(paper::Paper),
        ServerType::Fabric => Box::new(fabric::Fabric),
        ServerType::Neoforge => Box::new(neoforge::Neoforge),
    }
}

/// Mojang's manifest, shared by vanilla (which installs from it) and Fabric
/// (which only needs it to learn the newest release).
pub(crate) const MOJANG_MANIFEST: &str =
    "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json";

pub(crate) fn latest_mojang_release(http: &dyn Http) -> Option<String> {
    let body = http.get(MOJANG_MANIFEST).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&body).ok()?;
    json.get("latest")?
        .get("release")?
        .as_str()
        .map(str::to_string)
}
