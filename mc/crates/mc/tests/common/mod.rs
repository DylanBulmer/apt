//! The harness for tier-2 integration tests.
//!
//! Drives REAL command handlers against a temp root, with scripted upstreams
//! and a scripted systemd. No Docker, no root, no network. What a container is
//! still needed for is the things that genuinely require a Debian root — real
//! `chown` outcomes, dpkg behaviour, the service group — and those live in
//! `tests/suites/integration/`.

#![allow(dead_code)] // each test file uses a different subset

use std::path::Path;

use mc_common::Paths;
use mc_common::http::fake::FakeHttp;
use mc_common::service::UnitState;
use mc_common::service::fake::FakeService;

pub const MOJANG_MANIFEST: &str =
    "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json";
pub const PAPER_API: &str = "https://fill.papermc.io/v3/projects/paper";

/// Body of every fixture jar, and its digests.
pub const JAR: &[u8] = b"abc";
pub const JAR_SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";
pub const JAR_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

pub struct Sandbox {
    pub dir: tempfile::TempDir,
    pub paths: Paths,
}

impl Sandbox {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::with_root(dir.path());
        std::fs::create_dir_all(paths.base()).expect("base");
        std::fs::create_dir_all(paths.config_dir()).expect("config");
        Self { dir, paths }
    }

    /// Pretend the EULA was already accepted, for tests that are not about it.
    pub fn accept_eula(&self) -> &Self {
        std::fs::write(self.paths.eula(), "eula=true\n").expect("eula");
        self
    }

    /// Pretend a server is installed, without downloading one.
    pub fn with_server(&self) -> &Self {
        std::fs::write(self.paths.server_jar(), b"existing").expect("server.jar");
        self
    }

    pub fn write_config(&self, toml: &str) -> &Self {
        std::fs::write(self.paths.config_file(), toml).expect("config.toml");
        self
    }

    pub fn read_config(&self) -> String {
        std::fs::read_to_string(self.paths.config_file()).unwrap_or_default()
    }

    /// Install a fixture plugin: a shell script plus its manifest.
    ///
    /// The script appends its argv to a log file, so a test can assert exactly
    /// which hooks fired, in which order, and what payload they were handed.
    pub fn install_plugin(&self, name: &str, manifest_body: &str) -> std::path::PathBuf {
        let plugins = self.paths.plugins_dir();
        let libexec = self.paths.libexec_dir();
        std::fs::create_dir_all(&plugins).expect("plugins.d");
        std::fs::create_dir_all(&libexec).expect("libexec");

        let log = self.dir.path().join(format!("{name}.log"));
        let bin = libexec.join(format!("mc-{name}"));
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\ncat >> {log}\nprintf '\\n' >> {log}\nexit ${{MC_FIXTURE_EXIT:-0}}\n",
                log = log.display()
            ),
        )
        .expect("plugin binary");
        mc_common::fsx::apply_owner_mode(&bin, None, 0o755).expect("chmod");

        std::fs::write(
            plugins.join(format!("{name}.toml")),
            manifest_body.replace("{BIN}", &bin.display().to_string()),
        )
        .expect("manifest");
        log
    }

    pub fn plugin_log(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(format!("{name}.log"))).unwrap_or_default()
    }

    pub fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    /// Everything directly inside the server's parent, so a test can assert no
    /// staging directory was left behind.
    pub fn siblings_of_base(&self) -> Vec<String> {
        let parent = self
            .paths
            .base()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        std::fs::read_dir(parent)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A `FakeHttp` serving a complete, self-consistent vanilla install.
pub fn vanilla_http(sha1: &str) -> FakeHttp {
    FakeHttp::new()
        .route(
            MOJANG_MANIFEST,
            r#"{"latest": {"release": "1.21.4"},
                "versions": [
                  {"id": "1.21.4", "url": "https://piston-meta.test/1.21.4.json"},
                  {"id": "1.21.3", "url": "https://piston-meta.test/1.21.3.json"}
                ]}"#,
        )
        .route(
            "https://piston-meta.test/1.21.4.json",
            format!(
                r#"{{"downloads": {{"server": {{"url": "https://piston-data.test/server.jar", "sha1": "{sha1}"}}}}}}"#
            ),
        )
        .route(
            "https://piston-meta.test/1.21.3.json",
            format!(
                r#"{{"downloads": {{"server": {{"url": "https://piston-data.test/old.jar", "sha1": "{sha1}"}}}}}}"#
            ),
        )
        .route("https://piston-data.test/server.jar", JAR.to_vec())
        .route("https://piston-data.test/old.jar", JAR.to_vec())
}

/// A `FakeHttp` serving a complete Paper install.
pub fn paper_http() -> FakeHttp {
    FakeHttp::new()
        .route(PAPER_API, r#"{"versions": {"1.21": ["1.21.4"]}}"#)
        .route(
            format!("{PAPER_API}/versions/1.21.4/builds"),
            format!(
                r#"[{{"id": 100, "channel": "STABLE",
                  "downloads": {{"server:default": {{"url": "https://fill-data.test/paper.jar",
                    "checksums": {{"sha256": "{JAR_SHA256}"}}}}}}}}]"#
            ),
        )
        .route("https://fill-data.test/paper.jar", JAR.to_vec())
}

pub fn service(initial: UnitState) -> FakeService {
    FakeService::new(initial)
}
