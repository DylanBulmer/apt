//! Installing from a pack file, through a plugin-provided source provider.
//!
//! CORE KEEPS THE ORDERING THAT MAKES THIS SAFE. The provider only populates a
//! staging directory and reports what it found; the lock, the EULA gate,
//! ownership of `MC_BASE`, and re-applying the keys the system owns in
//! `server.properties` all stay here. That last one is what stops a pack
//! enabling RCON with a password of its choosing.

use std::path::Path;

use mc_common::error::{Error, IoContext, Result};
use mc_common::plugin::Registry;
use mc_common::staging::Staging;
use mc_common::{Config, Paths, ServerType, properties, ui};

use crate::context::Ctx;

/// What a provider reports back on stdout.
#[derive(Debug, serde::Deserialize)]
pub struct Report {
    pub server_type: String,
    pub minecraft_version: String,
    #[allow(dead_code)] // core re-derives this; carried for diagnostics
    pub java_version: u32,
}

/// The plugin that claims this file's extension, or a refusal naming the
/// package that would provide one.
pub fn provider_for<'a>(
    registry: &'a Registry,
    pack: &Path,
) -> Result<(&'a mc_common::plugin::Manifest, &'a str)> {
    let extension = pack
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_string();

    if let Some((plugin, provider)) = registry.source_for_extension(&extension) {
        return Ok((plugin, provider.name.as_str()));
    }

    // Reported before the generic refusal: a plugin that failed to load is the
    // likeliest reason a capability an operator installed has gone missing.
    for problem in registry.problems() {
        ui::warn(problem);
    }
    let hint = match extension.as_str() {
        "mrpack" => "\nInstall it with: apt install mc-mrpack",
        _ => "",
    };
    Err(Error::config(format!(
        "No installed plugin can install a .{extension} file.{hint}"
    )))
}

/// Run the provider and land its output in `MC_BASE`.
///
/// Returns the report so the caller can pin the version and type it produced.
pub fn install_from_pack(ctx: &Ctx, pack: &Path, cfg: &mut Config) -> Result<Report> {
    // THE PROVIDER LOOKUP COMES FIRST, before the file is even checked for.
    // Both refusals are true when someone types a path to a .mrpack on a
    // machine with no mrpack plugin, but only one of them is actionable: "no
    // installed plugin can install a .mrpack file" is the operator's real
    // problem, and fixing the path would not help. A missing file is worth
    // reporting only once something could have opened it.
    let registry = Registry::discover(&ctx.paths);
    let (plugin, name) = provider_for(&registry, pack)?;

    if !pack.is_file() {
        return Err(Error::config(format!("File not found: {}", pack.display())));
    }
    ui::info(format!("Installing with the '{name}' provider..."));

    let staging = Staging::new(&ctx.paths.base())?;

    // stdout carries the report; stderr carries the provider's progress and is
    // passed straight through to the operator.
    let output = std::process::Command::new(&plugin.bin)
        .arg("provide")
        .arg(pack)
        .arg(staging.path())
        .env("MC_ROOT", ctx.paths.root())
        .env("MC_BASE", ctx.paths.base())
        .env("MC_CONFIG", ctx.paths.config_dir())
        .env("MC_ABI", mc_common::plugin::ABI.to_string())
        .output()
        .map_err(|e| Error::other(format!("running {}: {e}", plugin.bin.display())))?;

    // Passed through rather than swallowed: a provider's diagnostics are the
    // only explanation an operator gets when a pack is refused.
    if !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
    if !output.status.success() {
        return Err(Error::config(format!(
            "The '{name}' provider failed ({}).",
            output.status
        )));
    }

    let report: Report = serde_json::from_slice(&output.stdout).map_err(|e| {
        // A provider that succeeds but reports nothing usable must not leave a
        // half-installed server: the staging guard drops here and takes the
        // tree with it.
        Error::other(format!(
            "The '{name}' provider returned an unreadable report: {e}"
        ))
    })?;

    let server_type: ServerType = report.server_type.parse()?;
    mc_common::version::validate(&report.minecraft_version, "Minecraft version")?;

    // The pack ships its own server.properties in the staged tree. It is merged
    // through the properties engine, which re-applies the four keys the system
    // owns AFTERWARDS — so a pack cannot choose the RCON password, the RCON
    // port, or the game port.
    let staged_properties = staging.path().join("server.properties");
    // Read raw bytes and convert with lossy UTF-8 so a pack shipping invalid
    // encoding cannot skip the merge: `read_to_string` would fail on 0xFF,
    // `.ok()` would drop the error, and the file would survive `merge_tree`
    // verbatim, letting the pack choose managed credentials.
    let pack_properties = if staged_properties.is_file() {
        let bytes = std::fs::read(&staged_properties).at(&staged_properties)?;
        std::fs::remove_file(&staged_properties).at(&staged_properties)?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    } else if staged_properties.exists() {
        return Err(Error::rejected(
            "The pack ships a server.properties that is not a regular file.",
        ));
    } else {
        None
    };

    super::install::merge_tree(staging.path(), &ctx.paths.base())?;

    if let Some(text) = pack_properties {
        properties::merge(&ctx.paths, &text)?;
    }

    cfg.server.server_type = server_type;
    cfg.server.version = report.minecraft_version.clone();

    // CORE FETCHES THE BASE ARTIFACT, not the provider. A pack names a loader
    // and a Minecraft version; core already knows how to fetch all four, and
    // every one of those paths validates the version and verifies a published
    // digest. Letting each provider fetch its own would duplicate that — and
    // each copy could get it wrong differently.
    //
    // After the override merge, so a pack cannot ship a file that shadows the
    // server jar.
    let resolved = super::install::install_artifact(ctx, cfg)?;
    cfg.server.version = resolved;

    // Marks the server as pack-derived, so a later bare `mc upgrade --version`
    // is refused: it would replace the jar and strip every mod.
    record_pack(&ctx.paths, pack)?;

    Ok(report)
}

/// Record that this server came from a pack.
fn record_pack(paths: &Paths, pack: &Path) -> Result<()> {
    let manifest = paths.mrpack_manifest();
    let body = serde_json::json!({
        "source": pack.file_name().map(|n| n.to_string_lossy().into_owned()),
    });
    let dir = paths.config_dir();
    std::fs::create_dir_all(&dir).at(&dir)?;
    std::fs::write(&manifest, body.to_string()).at(&manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());
        std::fs::create_dir_all(paths.base()).unwrap();
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        (dir, paths)
    }

    #[test]
    fn a_non_utf8_server_properties_in_a_pack_is_merged_not_copied() {
        // SEC-H4: a pack shipping invalid UTF-8 in server.properties would
        // bypass the merge if read with `read_to_string` (which errors on
        // 0xFF), leaving the file to survive `merge_tree` verbatim.
        let (_dir, paths) = sandbox();
        std::fs::write(paths.passwd_file(), "the-real-secret\n").unwrap();
        properties::init(&paths).unwrap();

        // Simulate a pack's server.properties with invalid UTF-8 (0xFF) in a
        // comment, alongside attacker-chosen managed keys.
        let malicious: Vec<u8> = b"# pack comment \xff\n\
            rcon.password=attacker-chosen\n\
            management-server-secret=evil\n\
            server-port=1234\n\
            motd=EvilPack\n"
            .to_vec();

        // This is what the fix does: read raw bytes, convert lossy, then merge.
        let pack_properties = String::from_utf8_lossy(&malicious).into_owned();
        properties::merge(&paths, &pack_properties).unwrap();

        let live = properties::Properties::load(&paths.server_properties());
        assert_ne!(
            live.get("rcon.password"),
            Some("attacker-chosen"),
            "managed rcon.password must not come from a non-UTF-8 pack file"
        );
        assert_ne!(
            live.get("management-server-secret"),
            Some("evil"),
            "managed management-server-secret must not come from a non-UTF-8 pack file"
        );
        // The pack's non-managed keys still land.
        assert_eq!(live.get("motd"), Some("EvilPack"));
    }
}
