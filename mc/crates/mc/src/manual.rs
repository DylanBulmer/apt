//! `mc man` — which page answers a question, and handing off to man(1).
//!
//! Resolution goes through the plugin registry rather than a hardcoded table,
//! for the same reason dispatch does: `mc man backup` must fail the way
//! `mc backup` fails when mc-backup is not installed, naming the package,
//! rather than opening a page describing a command this system does not have.

use std::ffi::OsString;

use mc_common::error::{Error, Result};
use mc_common::plugin::Registry;

/// A page as man(1) is asked for it.
#[derive(Debug, PartialEq, Eq)]
pub struct Page {
    pub name: String,
    /// Passed to man before the name. `None` lets man search its sections in
    /// order, which is what an unambiguous name wants.
    pub section: Option<&'static str>,
}

impl Page {
    fn one(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            section: None,
        }
    }

    fn five(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            section: Some("5"),
        }
    }

    fn args(&self) -> Vec<OsString> {
        let mut args = Vec::new();
        if let Some(section) = self.section {
            args.push(OsString::from(section));
        }
        args.push(OsString::from(&self.name));
        args
    }
}

/// Pages core itself ships, reachable by a name that is not a command.
///
/// `config` earns an alias because it is what an operator looking for
/// `config.toml` types. `plugins` deliberately does NOT: it is a real
/// subcommand, and `mc man plugins` documenting the command an operator just
/// ran is less surprising than silently answering a different question. The
/// page is still reachable as `mc man mc-plugins`.
const TOPICS: [(&str, &str); 3] = [
    ("config", "mc-config"),
    ("mc-config", "mc-config"),
    ("mc-plugins", "mc-plugins"),
];

/// Which page documents `topic`, or the whole manual when there is none.
///
/// `core` is the set of subcommands mc implements itself; they are all
/// documented in mc(1) rather than in a page each.
pub fn page_for(registry: &Registry, core: &[&str], topic: Option<&str>) -> Result<Page> {
    let Some(topic) = topic else {
        return Ok(Page::one("mc"));
    };

    // A plugin command first: a plugin that contributed `backup` owns the
    // answer even if core were ever to grow a command by that name, because
    // the plugin is what would run.
    if let Some((plugin, _)) = registry.command(topic) {
        return Ok(Page::one(format!("mc-{}", plugin.name)));
    }

    if core.contains(&topic) {
        return Ok(Page::one("mc"));
    }

    if let Some((_, page)) = TOPICS.iter().find(|(alias, _)| *alias == topic) {
        return Ok(Page::five(*page));
    }

    // The same hint dispatch gives for an unknown subcommand: a page that is
    // missing because its package is not installed is the likeliest reason
    // somebody is here.
    let hint = match topic {
        "rcon" => "\nInstall it with: apt install mc-rcon",
        "backup" | "restore" => "\nInstall it with: apt install mc-backup",
        "mrpack" => "\nInstall it with: apt install mc-mrpack",
        _ => "\nRun 'mc man' for the manual, or 'mc plugins' for what is installed.",
    };
    Err(Error::config(format!(
        "No manual page for '{topic}'.{hint}"
    )))
}

/// Replace this process with man(1).
///
/// Exec rather than spawn-and-wait: man runs a pager on the terminal mc was
/// given, and an mc sitting in the middle of that pipeline would only be
/// something for the shell to signal around.
pub fn open(page: &Page) -> Error {
    use std::os::unix::process::CommandExt as _;

    let error = std::process::Command::new("man").args(page.args()).exec();

    // Only reachable when exec failed.
    if error.kind() == std::io::ErrorKind::NotFound {
        return Error::config(
            "man is not installed. Install man-db, or read the manual at \
             https://apt.bulmer.dev",
        );
    }
    Error::other(format!("man {}: {error}", page.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORE: [&str; 3] = ["install", "status", "plugins"];

    /// A registry over fixture manifests, each pointing at a file that really
    /// exists: discovery refuses a manifest whose `bin` does not, so a plugin
    /// whose package is half-installed contributes nothing.
    fn registry(manifests: &[&str]) -> Registry {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("plugin");
        std::fs::write(&bin, "#!/bin/sh\n").expect("write plugin binary");

        for (i, manifest) in manifests.iter().enumerate() {
            let manifest = manifest.replace("{BIN}", &bin.to_string_lossy());
            std::fs::write(dir.path().join(format!("{i}.toml")), manifest).expect("write manifest");
        }
        Registry::discover_in(dir.path())
    }

    const BACKUP: &str = r#"
abi = 1
name = "backup"
bin = "{BIN}"
[[commands]]
name = "backup"
[[commands]]
name = "restore"
"#;

    #[test]
    fn no_topic_opens_the_whole_manual() {
        let page = page_for(&registry(&[]), &CORE, None).expect("mc(1)");
        assert_eq!(page, Page::one("mc"));
    }

    #[test]
    fn a_core_command_is_documented_in_mc_1() {
        // Core ships one page, not one per subcommand, so every core command
        // resolves to the same place.
        for topic in CORE {
            let page = page_for(&registry(&[]), &CORE, Some(topic)).expect("mc(1)");
            assert_eq!(page, Page::one("mc"), "{topic}");
        }
    }

    #[test]
    fn a_plugin_command_opens_the_page_its_package_ships() {
        // Both of mc-backup's commands resolve to the one page named after the
        // plugin — the naming rule packaging has to keep to.
        for topic in ["backup", "restore"] {
            let page = page_for(&registry(&[BACKUP]), &CORE, Some(topic)).expect("mc-backup(1)");
            assert_eq!(page, Page::one("mc-backup"), "{topic}");
        }
    }

    #[test]
    fn a_command_whose_plugin_is_not_installed_names_the_package() {
        // Not a bare "no such page": the page really is absent, and the reason
        // is a package that was never installed.
        let error = page_for(&registry(&[]), &CORE, Some("backup")).expect_err("no page");
        assert!(
            error.to_string().contains("apt install mc-backup"),
            "{error}"
        );
    }

    #[test]
    fn the_config_format_is_reachable_by_the_name_operators_use() {
        let page = page_for(&registry(&[]), &CORE, Some("config")).expect("mc-config(5)");
        assert_eq!(page, Page::five("mc-config"));
        assert_eq!(
            page.args(),
            vec![OsString::from("5"), OsString::from("mc-config")]
        );
    }

    #[test]
    fn a_subcommand_wins_over_a_topic_of_the_same_name() {
        // `plugins` is a command an operator can run; documenting the manifest
        // format instead would answer a question they did not ask.
        let page = page_for(&registry(&[]), &CORE, Some("plugins")).expect("mc(1)");
        assert_eq!(page, Page::one("mc"));
        assert_eq!(
            page_for(&registry(&[]), &CORE, Some("mc-plugins")).expect("mc-plugins(5)"),
            Page::five("mc-plugins")
        );
    }

    #[test]
    fn an_unknown_topic_points_back_at_the_manual() {
        let error = page_for(&registry(&[]), &CORE, Some("wat")).expect_err("no page");
        assert!(error.to_string().contains("mc man"), "{error}");
    }
}
