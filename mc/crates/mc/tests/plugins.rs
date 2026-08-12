//! Tier 2 — the plugin host, driven against real fixture plugins on disk.
//!
//! The fixture plugin is a shell script that logs its argv and stdin, so these
//! assert what a plugin was actually asked to do rather than what core intended
//! to ask.

// Integration tests: a panic IS the failure report here, so the workspace's
// no-unwrap/no-panic lints are relaxed for this crate only. Shipped code keeps
// them — a panic in `mc serve` is an outage whose cause is an address in the
// journal.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use mc::commands::shutdown;
use mc::context::Ctx;
use mc_common::plugin::{Event, Registry};
use mc_common::service::UnitState;

fn ctx(sandbox: &Sandbox) -> Ctx {
    Ctx {
        paths: sandbox.paths.clone(),
        http: Box::new(mc_common::http::fake::FakeHttp::new()),
        service: Box::new(service(UnitState::Active)),
        packages: Box::new(mc_common::packages::fake::FakePackages::new()),
        argv: vec!["shutdown".to_string()],
    }
}

const RCON_MANIFEST: &str = r#"
abi = 1
name = "rcon"
bin = "{BIN}"
[[commands]]
name = "rcon"
about = "Open an RCON console"
[[hooks]]
event = "pre-stop"
"#;

#[test]
fn a_hook_receives_its_event_and_payload() {
    let sandbox = Sandbox::new();
    sandbox.install_plugin("rcon", RCON_MANIFEST);

    let registry = Registry::discover(&sandbox.paths);
    registry
        .run_hook(
            &sandbox.paths,
            Event::PreStop,
            &serde_json::json!({"reason": "stop"}),
        )
        .unwrap();

    let log = sandbox.plugin_log("rcon");
    assert!(log.contains("hook pre-stop"), "argv: {log}");
    assert!(
        log.contains(r#""reason":"stop""#),
        "payload on stdin: {log}"
    );
}

#[test]
fn a_plugin_not_registered_for_an_event_is_not_invoked() {
    let sandbox = Sandbox::new();
    sandbox.install_plugin("rcon", RCON_MANIFEST);

    let registry = Registry::discover(&sandbox.paths);
    registry
        .run_hook(&sandbox.paths, Event::PostBackup, &serde_json::json!({}))
        .unwrap();

    assert!(sandbox.plugin_log("rcon").is_empty());
}

#[test]
fn every_registered_plugin_runs_even_when_an_earlier_one_fails() {
    // Hooks are independent contributions, not a pipeline. One plugin failing
    // to warn players must not stop another from flushing the world.
    let sandbox = Sandbox::new();
    sandbox.install_plugin("aaa", &RCON_MANIFEST.replace("\"rcon\"", "\"aaa\""));
    sandbox.install_plugin("zzz", &RCON_MANIFEST.replace("\"rcon\"", "\"zzz\""));

    // Make the first one fail.
    let bin = sandbox.paths.libexec("mc-aaa");
    let log = sandbox.dir.path().join("aaa.log");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexit 1\n",
            log.display()
        ),
    )
    .unwrap();
    mc_common::fsx::apply_owner_mode(&bin, None, 0o755).unwrap();

    let registry = Registry::discover(&sandbox.paths);
    registry
        .run_hook(&sandbox.paths, Event::PreStop, &serde_json::json!({}))
        .unwrap();

    assert!(sandbox.plugin_log("aaa").contains("hook pre-stop"));
    assert!(
        sandbox.plugin_log("zzz").contains("hook pre-stop"),
        "the second plugin must still run"
    );
}

#[test]
fn a_failing_pre_stop_hook_never_aborts_the_shutdown() {
    // TimeoutStopSec is the outer bound on a stop. If a hook failure could
    // abort ExecStop, a broken plugin would leave the JVM to be SIGKILLed
    // mid-chunk-flush — the world corruption the countdown exists to prevent.
    let sandbox = Sandbox::new();
    sandbox.install_plugin("rcon", RCON_MANIFEST);

    let bin = sandbox.paths.libexec("mc-rcon");
    std::fs::write(&bin, "#!/bin/sh\nexit 3\n").unwrap();
    mc_common::fsx::apply_owner_mode(&bin, None, 0o755).unwrap();

    shutdown::run(&ctx(&sandbox)).unwrap();
}

#[test]
fn a_shutdown_with_no_pre_stop_handler_is_a_no_op_that_says_so() {
    // Without a console plugin, `mc stop` disconnects everyone with no in-game
    // warning. That has to be visible in the journal rather than silent.
    let sandbox = Sandbox::new();
    shutdown::run(&ctx(&sandbox)).unwrap();
}

#[test]
fn a_plugin_binary_receives_the_sandbox_root() {
    // Paths reach plugins through the environment, so MC_ROOT applies to them
    // too — otherwise an integration test could drive core against a temp root
    // while its plugins wrote to the real /opt/minecraft.
    let sandbox = Sandbox::new();
    sandbox.install_plugin("rcon", RCON_MANIFEST);

    let bin = sandbox.paths.libexec("mc-rcon");
    let log = sandbox.dir.path().join("rcon.log");
    std::fs::write(
        &bin,
        format!("#!/bin/sh\nprintf 'root=%s base=%s abi=%s\\n' \"$MC_ROOT\" \"$MC_BASE\" \"$MC_ABI\" >> {}\n", log.display()),
    )
    .unwrap();
    mc_common::fsx::apply_owner_mode(&bin, None, 0o755).unwrap();

    let registry = Registry::discover(&sandbox.paths);
    registry
        .run_hook(&sandbox.paths, Event::PreStop, &serde_json::json!({}))
        .unwrap();

    let log = sandbox.plugin_log("rcon");
    assert!(
        log.contains(&format!("root={}", sandbox.paths.root().display())),
        "{log}"
    );
    assert!(
        log.contains(&format!("base={}", sandbox.paths.base().display())),
        "{log}"
    );
    assert!(log.contains("abi=1"), "{log}");
}

#[test]
fn an_abi_mismatch_removes_only_the_offending_plugin() {
    let sandbox = Sandbox::new();
    sandbox.install_plugin("rcon", RCON_MANIFEST);
    sandbox.install_plugin(
        "future",
        r#"
        abi = 99
        name = "future"
        bin = "{BIN}"
        [[commands]]
        name = "future"
        "#,
    );

    let registry = Registry::discover(&sandbox.paths);
    assert!(
        registry.command("rcon").is_some(),
        "the healthy plugin survives"
    );
    assert!(registry.command("future").is_none());
    assert_eq!(registry.problems().len(), 1);
    let problem = registry.problems().first().unwrap();
    assert!(problem.contains("future"), "named: {problem}");
    assert!(problem.contains("ABI 99"), "{problem}");
}

#[test]
fn only_a_registered_name_is_dispatchable() {
    // Resolving to *some* executable is not sufficient. Without the registry,
    // an internal helper would be reachable from the command line, skipping the
    // guards, the lock and the config loading its real entry point performs.
    let sandbox = Sandbox::new();
    sandbox.install_plugin("rcon", RCON_MANIFEST);

    // The binary exists and is executable, but declares only the name "rcon".
    let registry = Registry::discover(&sandbox.paths);
    assert!(registry.command("rcon").is_some());
    for undeclared in ["hook", "command", "mc-rcon", "install_mrpack", "enable"] {
        assert!(registry.command(undeclared).is_none(), "{undeclared}");
    }
}

#[test]
fn a_source_provider_is_discoverable_by_extension() {
    let sandbox = Sandbox::new();
    sandbox.install_plugin(
        "mrpack",
        r#"
        abi = 1
        name = "mrpack"
        bin = "{BIN}"
        [[providers]]
        kind = "source"
        name = "mrpack"
        extensions = ["mrpack"]
        "#,
    );

    let registry = Registry::discover(&sandbox.paths);
    let (plugin, provider) = registry.source_for_extension("mrpack").unwrap();
    assert_eq!(plugin.name, "mrpack");
    assert_eq!(provider.kind, "source");
}
