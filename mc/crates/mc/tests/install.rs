//! Tier 2 — install and upgrade, driven through the real handlers.

// Integration tests: a panic IS the failure report here, so the workspace's
// no-unwrap/no-panic lints are relaxed for this crate only. Shipped code keeps
// them — a panic in `mc serve` is an outage whose cause is an address in the
// journal.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use mc::commands::install::{self, InstallArgs, UpgradeArgs};
use mc::context::Ctx;
use mc_common::error::Error;
use mc_common::service::UnitState;

fn install_args() -> InstallArgs {
    InstallArgs {
        server_type: None,
        version: None,
        pack: None,
        assume_yes: true,
        accept_eula: true,
        force: false,
    }
}

fn upgrade_args() -> UpgradeArgs {
    UpgradeArgs {
        version: None,
        pack: None,
        assume_yes: true,
        force: false,
        no_backup: true,
    }
}

fn ctx(sandbox: &Sandbox, http: impl mc_common::http::Http + 'static, state: UnitState) -> Ctx {
    Ctx {
        paths: sandbox.paths.clone(),
        http: Box::new(http),
        service: Box::new(service(state)),
        packages: Box::new(mc_common::packages::fake::FakePackages::new()),
        argv: vec!["install".to_string()],
    }
}

#[test]
fn installs_vanilla_end_to_end() {
    let sandbox = Sandbox::new();
    let ctx = ctx(&sandbox, vanilla_http(JAR_SHA1), UnitState::Inactive);

    install::install(&ctx, install_args()).unwrap();

    assert_eq!(std::fs::read(sandbox.paths.server_jar()).unwrap(), JAR);
    // The resolved version is PINNED, not left as "latest": a later upgrade has
    // to be able to tell whether anything moved.
    assert!(
        sandbox.read_config().contains("version = \"1.21.4\""),
        "{}",
        sandbox.read_config()
    );
    assert!(sandbox.paths.server_properties().exists());
}

#[test]
fn nothing_lands_in_the_server_directory_until_the_artifact_verifies() {
    // Both install and upgrade write into a LIVE server directory. A download
    // that fails verification must leave it exactly as it was.
    let sandbox = Sandbox::new();
    let ctx = ctx(&sandbox, vanilla_http(&"0".repeat(40)), UnitState::Inactive);

    let err = install::install(&ctx, install_args()).unwrap_err();
    assert!(matches!(err, Error::Rejected(_)), "{err}");
    assert!(!sandbox.paths.server_jar().exists());
    assert!(
        !sandbox.paths.config_file().exists(),
        "no version should be pinned"
    );
}

#[test]
fn an_aborted_download_leaves_no_staging_directory_behind() {
    let sandbox = Sandbox::new();
    let http =
        vanilla_http(JAR_SHA1).fail("https://piston-data.test/server.jar", "connection reset");
    let ctx = ctx(&sandbox, http, UnitState::Inactive);

    assert!(install::install(&ctx, install_args()).is_err());
    let leftovers: Vec<String> = sandbox
        .siblings_of_base()
        .into_iter()
        .filter(|name| name.starts_with(".mc-staging-"))
        .collect();
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
}

#[test]
fn refuses_to_install_over_an_existing_server() {
    // Installing over a live server overwrites server.jar and repins the
    // version with none of the protections upgrade has.
    let sandbox = Sandbox::new();
    sandbox.with_server();
    let ctx = ctx(&sandbox, vanilla_http(JAR_SHA1), UnitState::Inactive);

    let err = install::install(&ctx, install_args())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("mc upgrade"),
        "points at the safe command: {err}"
    );
    assert!(err.contains("--force"), "{err}");
    assert_eq!(
        std::fs::read(sandbox.paths.server_jar()).unwrap(),
        b"existing"
    );
}

#[test]
fn force_reinstalls_over_an_existing_server() {
    let sandbox = Sandbox::new();
    sandbox.with_server();
    let ctx = ctx(&sandbox, vanilla_http(JAR_SHA1), UnitState::Inactive);

    let mut args = install_args();
    args.force = true;
    install::install(&ctx, args).unwrap();
    assert_eq!(std::fs::read(sandbox.paths.server_jar()).unwrap(), JAR);
}

#[test]
fn installs_paper_from_the_v3_api() {
    let sandbox = Sandbox::new();
    let ctx = ctx(&sandbox, paper_http(), UnitState::Inactive);

    let mut args = install_args();
    args.server_type = Some(mc_common::ServerType::Paper);
    install::install(&ctx, args).unwrap();

    assert_eq!(std::fs::read(sandbox.paths.server_jar()).unwrap(), JAR);
    assert!(sandbox.read_config().contains("type = \"paper\""));
}

// ── upgrade ────────────────────────────────────────────────────────────────

#[test]
fn a_bare_upgrade_targets_latest_not_the_installed_pin() {
    // `server.version` records what is INSTALLED, not what is wanted. The shell
    // implementation read it back as the request, so it resolved the pin to
    // itself and reported "nothing to upgrade" forever — a bare `mc upgrade`
    // could never move a vanilla server, contradicting its own documentation.
    let sandbox = Sandbox::new();
    sandbox.accept_eula().with_server();
    sandbox.write_config("[server]\ntype = \"vanilla\"\nversion = \"1.21.3\"\n");
    let ctx = ctx(&sandbox, vanilla_http(JAR_SHA1), UnitState::Inactive);

    install::upgrade(&ctx, upgrade_args()).unwrap();
    assert!(
        sandbox.read_config().contains("1.21.4"),
        "should have moved to the newest release: {}",
        sandbox.read_config()
    );
}

#[test]
fn a_no_op_upgrade_costs_no_backup_and_no_downtime() {
    // The expensive parts are a full archive of the world and downtime a
    // populated server stretches to five minutes. An upgrade run on a schedule
    // lands here with nothing to do most times it fires.
    let sandbox = Sandbox::new();
    sandbox.accept_eula().with_server();
    sandbox.write_config("[server]\ntype = \"vanilla\"\nversion = \"1.21.4\"\n");

    let fake = service(UnitState::Active);
    let ctx = Ctx {
        paths: sandbox.paths.clone(),
        http: Box::new(vanilla_http(JAR_SHA1)),
        service: Box::new(fake),
        packages: Box::new(mc_common::packages::fake::FakePackages::new()),
        argv: vec!["upgrade".to_string()],
    };

    install::upgrade(&ctx, upgrade_args()).unwrap();
    assert_eq!(
        std::fs::read(sandbox.paths.server_jar()).unwrap(),
        b"existing"
    );
}

#[test]
fn a_no_op_upgrade_never_stops_the_server() {
    let sandbox = Sandbox::new();
    sandbox.accept_eula().with_server();
    sandbox.write_config("[server]\ntype = \"vanilla\"\nversion = \"1.21.4\"\n");

    // Held separately so the recorded calls can be inspected afterwards.
    let recorded = {
        let fake = std::sync::Arc::new(service(UnitState::Active));
        let ctx = Ctx {
            paths: sandbox.paths.clone(),
            http: Box::new(vanilla_http(JAR_SHA1)),
            service: Box::new(ArcService(fake.clone())),
            packages: Box::new(mc_common::packages::fake::FakePackages::new()),
            argv: vec!["upgrade".to_string()],
        };
        install::upgrade(&ctx, upgrade_args()).unwrap();
        fake.calls()
    };
    assert!(
        recorded.is_empty(),
        "a no-op upgrade must not touch the unit: {recorded:?}"
    );
}

#[test]
fn a_real_upgrade_stops_then_starts_in_that_order() {
    let sandbox = Sandbox::new();
    sandbox.accept_eula().with_server();
    sandbox.write_config("[server]\ntype = \"vanilla\"\nversion = \"1.21.3\"\n");

    let fake = std::sync::Arc::new(service(UnitState::Active));
    let ctx = Ctx {
        paths: sandbox.paths.clone(),
        http: Box::new(vanilla_http(JAR_SHA1)),
        service: Box::new(ArcService(fake.clone())),
        packages: Box::new(mc_common::packages::fake::FakePackages::new()),
        argv: vec!["upgrade".to_string()],
    };

    install::upgrade(&ctx, upgrade_args()).unwrap();

    let calls = fake.calls();
    assert_eq!(
        calls.first().map(String::as_str),
        Some("stop minecraft"),
        "{calls:?}"
    );
    assert!(calls.iter().any(|c| c == "start minecraft"), "{calls:?}");
    assert_eq!(std::fs::read(sandbox.paths.server_jar()).unwrap(), JAR);
    assert!(sandbox.read_config().contains("1.21.4"));
}

#[test]
fn an_upgrade_leaves_a_stopped_server_stopped() {
    let sandbox = Sandbox::new();
    sandbox.accept_eula().with_server();
    sandbox.write_config("[server]\ntype = \"vanilla\"\nversion = \"1.21.3\"\n");

    let fake = std::sync::Arc::new(service(UnitState::Inactive));
    let ctx = Ctx {
        paths: sandbox.paths.clone(),
        http: Box::new(vanilla_http(JAR_SHA1)),
        service: Box::new(ArcService(fake.clone())),
        packages: Box::new(mc_common::packages::fake::FakePackages::new()),
        argv: vec!["upgrade".to_string()],
    };

    install::upgrade(&ctx, upgrade_args()).unwrap();
    assert!(
        !fake.calls().iter().any(|c| c == "start minecraft"),
        "a server that was down must not come up as a side effect: {:?}",
        fake.calls()
    );
}

#[test]
fn refuses_a_bare_version_upgrade_on_a_modpack_server() {
    // It would replace the jar and strip every mod.
    let sandbox = Sandbox::new();
    sandbox.accept_eula().with_server();
    std::fs::write(sandbox.paths.mrpack_manifest(), "{}").unwrap();
    let ctx = ctx(&sandbox, vanilla_http(JAR_SHA1), UnitState::Inactive);

    let err = install::upgrade(&ctx, upgrade_args())
        .unwrap_err()
        .to_string();
    assert!(err.contains(".mrpack"), "{err}");
}

#[test]
fn refuses_to_upgrade_without_a_backup_provider() {
    // The shell version could assume a backup was always available because it
    // lived in the same package. Now that mc-backup is separately removable,
    // "no backup was taken" has to be a decision rather than a consequence.
    let sandbox = Sandbox::new();
    sandbox.accept_eula().with_server();
    sandbox.write_config("[server]\ntype = \"vanilla\"\nversion = \"1.21.3\"\n");
    let ctx = ctx(&sandbox, vanilla_http(JAR_SHA1), UnitState::Inactive);

    let mut args = upgrade_args();
    args.no_backup = false;
    let err = install::upgrade(&ctx, args).unwrap_err().to_string();
    assert!(err.contains("apt install mc-backup"), "{err}");
    assert!(
        err.contains("--no-backup"),
        "offers the explicit override: {err}"
    );
    assert_eq!(
        std::fs::read(sandbox.paths.server_jar()).unwrap(),
        b"existing"
    );
}

#[test]
fn paper_never_skips_an_upgrade_as_a_no_op() {
    // Paper publishes new BUILDS against an unchanged Minecraft version, so
    // "same version" does not mean "same jar" and skipping would pin the server
    // to a stale build.
    let sandbox = Sandbox::new();
    sandbox.accept_eula().with_server();
    sandbox.write_config("[server]\ntype = \"paper\"\nversion = \"1.21.4\"\n");

    let ctx = ctx(&sandbox, paper_http(), UnitState::Inactive);
    install::upgrade(&ctx, upgrade_args()).unwrap();
    assert_eq!(std::fs::read(sandbox.paths.server_jar()).unwrap(), JAR);
}

/// Lets a test keep a handle on the fake while the Ctx owns a `dyn` copy.
struct ArcService(std::sync::Arc<mc_common::service::fake::FakeService>);

impl mc_common::service::ServiceManager for ArcService {
    fn state(&self, unit: &str) -> UnitState {
        self.0.state(unit)
    }
    fn start(&self, unit: &str) -> mc_common::Result<()> {
        self.0.start(unit)
    }
    fn stop(&self, unit: &str) -> mc_common::Result<()> {
        self.0.stop(unit)
    }
    fn enable(&self, unit: &str) -> mc_common::Result<()> {
        self.0.enable(unit)
    }
    fn disable(&self, unit: &str) -> mc_common::Result<()> {
        self.0.disable(unit)
    }
    fn daemon_reload(&self) -> mc_common::Result<()> {
        self.0.daemon_reload()
    }
    fn recent_log(&self, unit: &str, lines: u32) -> Option<String> {
        self.0.recent_log(unit, lines)
    }
    fn sleep(&self, duration: std::time::Duration) {
        self.0.sleep(duration)
    }
}
