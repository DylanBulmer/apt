//! Tier 1 — the shipped manual pages against the code they describe.
//!
//! mc.1 is generated and cannot drift (see `src/man.rs`). These are the pages
//! that are prose, where nothing but a test stops a new hook event, a new
//! config key or a new plugin command from being documented nowhere. They read
//! `packages/` directly, because what ships is what is in that tree.

// Integration tests: a panic IS the failure report here, so the workspace's
// no-unwrap/no-panic lints are relaxed for this crate only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use mc_common::plugin::{Event, Manifest};

/// `mc/packages/` — the trees that mirror the target filesystem root.
fn packages() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages")
        .canonicalize()
        .expect("packages/ is two levels above this crate")
}

fn read(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every plugin manifest in the tree, with the package that ships it.
fn manifests() -> Vec<(String, Manifest)> {
    let mut found = Vec::new();
    for package in std::fs::read_dir(packages()).expect("packages/") {
        let package = package.expect("package dir").path();
        let dir = package.join("usr/lib/mc/plugins.d");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let file = entry.expect("manifest").path();
            let name = package
                .file_name()
                .expect("package name")
                .to_string_lossy()
                .into_owned();
            found.push((name, toml::from_str(&read(&file)).expect("manifest parses")));
        }
    }
    assert!(
        !found.is_empty(),
        "no plugin manifests found under packages/"
    );
    found
}

#[test]
fn every_plugin_command_has_a_manual_page_in_its_own_package() {
    // `mc man <command>` resolves a plugin command to mc-<plugin name>(1) —
    // the naming rule in mc-plugins(5). A page shipped under any other name,
    // or by any other package, is a page `mc man` will not find.
    for (package, manifest) in manifests() {
        let page = packages()
            .join(&package)
            .join("usr/share/man/man1")
            .join(format!("mc-{}.1", manifest.name));

        assert!(
            page.is_file(),
            "{package} registers plugin '{}' but ships no {}",
            manifest.name,
            page.display()
        );

        let text = read(&page);
        for command in &manifest.commands {
            assert!(
                text.contains(&command.name),
                "mc-{}.1 does not mention the '{}' command it registers",
                manifest.name,
                command.name
            );
        }
    }
}

#[test]
fn every_hook_event_is_documented() {
    let page = read(packages().join("mc-server/usr/share/man/man5/mc-plugins.5"));
    for event in Event::ALL {
        assert!(
            page.contains(event.as_str()),
            "mc-plugins(5) does not document the '{event}' event"
        );
    }
}

#[test]
fn the_documented_abi_is_the_one_core_implements() {
    // The number an operator reads before writing a manifest. Wrong, it
    // produces a plugin core refuses by name.
    let page = read(packages().join("mc-server/usr/share/man/man5/mc-plugins.5"));
    assert!(
        page.contains(&format!("\n.BR {}", mc_common::plugin::ABI)),
        "mc-plugins(5) does not name ABI {} as the implemented version",
        mc_common::plugin::ABI
    );
}

#[test]
fn every_setting_in_the_shipped_config_is_documented() {
    // The conffile is the closest thing to a schema an operator sees; a key
    // that ships in it and appears in no page is one they can only learn about
    // by reading the source. Commented-out keys count — `# version = 21` is
    // still a setting.
    let shipped = read(packages().join("mc-server/etc/minecraft/config.toml"));
    let page = read(packages().join("mc-server/usr/share/man/man5/mc-config.5"));

    for line in shipped.lines() {
        let line = line.trim_start_matches("# ").trim();
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        // A bare TOML key and nothing else. Prose in the comments contains
        // '=' too, and "e.g. [\"-Dfile.encoding" is not a setting.
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        assert!(
            page.contains(&format!("\n.B {key}\n")),
            "mc-config(5) does not document '{key}'"
        );
    }
}

#[test]
fn the_shipped_config_is_one_mc_can_load() {
    // Not strictly a manual-page property, but it is the same class of drift
    // and there was nowhere else asserting it: `deny_unknown_fields` means a
    // renamed setting makes the conffile unloadable on every installed system.
    let shipped = read(packages().join("mc-server/etc/minecraft/config.toml"));
    let config: mc_common::Config = toml::from_str(&shipped).expect("the shipped conffile parses");
    config.validate().expect("the shipped conffile validates");
}

#[test]
fn every_package_that_ships_a_page_ships_it_under_usr_share_man() {
    // Anywhere else and man(1) never sees it: dpkg installs the tree verbatim,
    // and there is no debhelper here to move files into place.
    for package in std::fs::read_dir(packages()).expect("packages/") {
        let package = package.expect("package dir").path();
        for entry in walk(&package) {
            let name = entry.to_string_lossy();
            if name.ends_with(".1") || name.ends_with(".5") {
                assert!(
                    name.contains("/usr/share/man/man"),
                    "{name} is not in a directory man(1) searches"
                );
            }
        }
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}
