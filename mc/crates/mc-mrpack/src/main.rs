//! `/usr/libexec/mc/mc-mrpack` — the modpack source provider.
//!
//! Implements the ABI-1 `source` provider protocol: core hands over a staging
//! directory and a pack file, this populates it, and reports back what was
//! installed as JSON on stdout.
//!
//! CORE KEEPS THE ORDERING THAT MAKES THIS SAFE — the lock, the EULA gate,
//! ownership of `MC_BASE`, and re-applying the managed keys of
//! `server.properties` afterwards so a pack cannot choose the RCON password.
//! This binary only fetches and stages.

use std::path::{Path, PathBuf};

use mc_common::error::{Error, IoContext, Result};
use mc_common::hash::{Algorithm, verify_file};
use mc_common::http::{Http, UreqHttp};
use mc_common::{Paths, ui};
use mc_mrpack::manifest;

/// Override files are config and resource packs, not server jars.
/// A 100 MiB per-file cap stops a zip bomb from filling the disk.
const OVERRIDE_FILE_LIMIT: u64 = 100 * 1024 * 1024;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = Paths::from_env();

    let result = match args.first().map(String::as_str) {
        Some("provide") => provide(&paths, &args),
        Some("hook") => Ok(()),
        _ => Err(Error::config(
            "mc-mrpack is a plugin for mc, not a command. Use: mc install <pack.mrpack>",
        )),
    };

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            ui::error(e.to_string());
            std::process::ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(1))
        }
    }
}

/// `mc-mrpack provide <pack-file> <staging-dir>`
fn provide(paths: &Paths, args: &[String]) -> Result<()> {
    let pack = PathBuf::from(
        args.get(1)
            .ok_or_else(|| Error::config("Usage: mc-mrpack provide <pack> <staging-dir>"))?,
    );
    let staging = PathBuf::from(
        args.get(2)
            .ok_or_else(|| Error::config("Usage: mc-mrpack provide <pack> <staging-dir>"))?,
    );

    if !pack.is_file() {
        return Err(Error::config(format!("File not found: {}", pack.display())));
    }

    let http = UreqHttp::new();
    let report = install(paths, &http, &pack, &staging)?;

    // The provider's answer goes to stdout as JSON; everything human-facing
    // goes to stderr, so core can parse this without stripping progress lines.
    println!(
        "{}",
        serde_json::to_string(&report)
            .map_err(|e| Error::other(format!("serialising report: {e}")))?
    );
    Ok(())
}

#[derive(serde::Serialize)]
struct Report {
    server_type: String,
    minecraft_version: String,
    java_version: u32,
}

fn install(_paths: &Paths, http: &dyn Http, pack: &Path, staging: &Path) -> Result<Report> {
    let mut zip = open_pack(pack)?;
    let index = read_index(&mut zip)?;
    let manifest = manifest::parse(&index)?;

    let java_version = mc_common::java::required_major(&manifest.minecraft_version);
    ui::info(format!(
        "Pack: {} {} (Java {java_version}+)",
        manifest.server_type, manifest.minecraft_version
    ));

    // THREE PHASES: parse and validate everything with no network I/O at all,
    // then fetch, then verify. `manifest::parse` did the first, so by the time
    // a single byte is downloaded an unsafe pack has already been refused.
    ui::info(format!("Downloading {} files...", manifest.files.len()));
    for file in &manifest.files {
        let destination = mc_common::staging::resolve_under(staging, &file.path)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).at(parent)?;
        }
        http.download(&file.url, &destination)?;
        verify_file(&destination, Some(&file.sha512), Algorithm::Sha512)?;
    }

    // Overrides last, so a pack's own config wins over a mod's default — and
    // both land in staging rather than in the live server directory.
    for tree in ["overrides", "server-overrides"] {
        extract_overrides(&mut zip, tree, staging)?;
    }

    // NO SERVER JAR IS FETCHED HERE. Core installs the base artifact for the
    // type and version reported below: it already knows how to fetch all four,
    // and each of those paths validates the version and verifies a published
    // digest. A copy of that logic per provider is a copy that can get it wrong
    // differently.
    Ok(Report {
        server_type: manifest.server_type.to_string(),
        minecraft_version: manifest.minecraft_version,
        java_version,
    })
}

type Zip = zip::ZipArchive<std::fs::File>;

fn open_pack(pack: &Path) -> Result<Zip> {
    let file = std::fs::File::open(pack).at(pack)?;
    zip::ZipArchive::new(file)
        .map_err(|e| Error::rejected(format!("{} is not a readable .mrpack: {e}", pack.display())))
}

fn read_index(zip: &mut Zip) -> Result<String> {
    use std::io::Read as _;
    let entry = zip
        .by_name("modrinth.index.json")
        .map_err(|e| Error::rejected(format!("No modrinth.index.json in the pack: {e}")))?;

    // Bounded: an index is kilobytes. Without a cap a zip bomb in this one
    // entry exhausts memory before anything else gets to refuse it.
    const MAX_INDEX: u64 = 16 * 1024 * 1024;
    let mut text = String::new();
    entry
        .take(MAX_INDEX)
        .read_to_string(&mut text)
        .map_err(|e| Error::rejected(format!("modrinth.index.json: {e}")))?;
    Ok(text)
}

/// Extract one override tree out of the pack into staging.
///
/// A pack normally ships only one of the two trees, so an absent one is the
/// normal case and not an error. Every other failure IS one: a truncated or
/// hostile archive that stops midway must abort rather than leave a
/// half-extracted tree to be merged anyway.
fn extract_overrides(zip: &mut Zip, tree: &str, staging: &Path) -> Result<()> {
    let prefix = format!("{tree}/");
    let names: Vec<String> = zip
        .file_names()
        .filter(|n| n.starts_with(&prefix))
        .map(str::to_string)
        .collect();

    if names.is_empty() {
        return Ok(());
    }
    ui::info(format!("Merging {tree}/ ({} entries)...", names.len()));

    for name in names {
        let relative = name.strip_prefix(&prefix).unwrap_or_default();
        if relative.is_empty() {
            continue;
        }
        // The zip's own entry names are as untrusted as the manifest's paths.
        let destination = mc_common::staging::resolve_under(staging, relative)?;

        let mut entry = zip
            .by_name(&name)
            .map_err(|e| Error::rejected(format!("reading {name}: {e}")))?;

        if entry.is_dir() {
            std::fs::create_dir_all(&destination).at(&destination)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).at(parent)?;
        }
        let mut out = std::fs::File::create(&destination).at(&destination)?;
        mc_common::fsx::copy_bounded(&mut entry, &mut out, OVERRIDE_FILE_LIMIT).map_err(|e| {
            let _ = std::fs::remove_file(&destination);
            Error::rejected(format!("extracting {name}: {e}"))
        })?;
    }
    Ok(())
}
