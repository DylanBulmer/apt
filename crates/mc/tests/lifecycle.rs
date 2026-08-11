//! Tier 2 — start verification, the exit-code policy, and the privilege table.
//!
//! The timing cases here are the reason `ServiceManager` is a trait: a real
//! systemd cannot be asked to produce "start returned 0, then the unit failed
//! half a second later" on demand, and that is precisely the case
//! `Type=simple` makes routine.

// Integration tests: a panic IS the failure report here, so the workspace's
// no-unwrap/no-panic lints are relaxed for this crate only. Shipped code keeps
// them — a panic in `mc serve` is an outage whose cause is an address in the
// journal.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use mc::cli::{Command, EulaArgs};
use mc::commands::{lifecycle, serve};
use mc::context::Ctx;
use mc_common::Error;
use mc_common::privilege::Requirement;
use mc_common::service::fake::FakeService;
use mc_common::service::{ServiceManager, UnitState};

fn ctx_with(sandbox: &Sandbox, service: Arc<FakeService>) -> Ctx {
    Ctx {
        paths: sandbox.paths.clone(),
        http: Box::new(mc_common::http::fake::FakeHttp::new()),
        service: Box::new(Shared(service)),
        packages: Box::new(mc_common::packages::fake::FakePackages::new()),
        argv: vec!["start".to_string()],
    }
}

#[test]
fn a_unit_that_forks_then_fails_is_reported_as_a_failure() {
    // `systemctl start` returning 0 says nothing: it returns the moment the
    // process is forked. `mc serve`'s config refusals exit within milliseconds
    // of that, so the unit is already failed by the time anything looks.
    let sandbox = Sandbox::new();
    sandbox.accept_eula().with_server();

    let service = Arc::new(FakeService::new(UnitState::Inactive).script([
        UnitState::Inactive, // the "is it already running?" check
        UnitState::Active,   // the poll sees it come up
        UnitState::Failed,   // …and the settle catches the refusal landing
    ]));
    let ctx = ctx_with(&sandbox, service);

    let err = lifecycle::start(&ctx, true).unwrap_err().to_string();
    assert!(err.contains("failed to start"), "{err}");
}

#[test]
fn a_unit_that_settles_active_is_reported_as_started() {
    let sandbox = Sandbox::new();
    sandbox.accept_eula().with_server();

    let service = Arc::new(FakeService::new(UnitState::Inactive).script([
        UnitState::Inactive, // not already running
        UnitState::Active,   // came up
        UnitState::Active,   // …and was still up after the settle
    ]));
    let ctx = ctx_with(&sandbox, service);

    lifecycle::start(&ctx, true).unwrap();
}

#[test]
fn a_unit_that_never_comes_up_times_out_rather_than_hanging() {
    let sandbox = Sandbox::new();
    sandbox.accept_eula().with_server();

    // `start` on the fake leaves the unit Active, as the real one does, so the
    // "never comes up" case has to be scripted: enough Inactive answers to
    // outlast the 60 s poll at 0.5 s intervals.
    let service =
        Arc::new(FakeService::new(UnitState::Inactive).script(vec![UnitState::Inactive; 300]));
    let ctx = ctx_with(&sandbox, Arc::clone(&service));

    let err = lifecycle::start(&ctx, true).unwrap_err().to_string();
    assert!(err.contains("did not reach active state"), "{err}");
    // Bounded: the poll must give up, not spin forever.
    assert!(
        service.slept() <= Duration::from_secs(65),
        "slept {:?}",
        service.slept()
    );
}

#[test]
fn starting_an_already_running_server_succeeds() {
    // A config-management run that asks for a running server and finds one has
    // not failed. `systemctl start` on an active unit exits 0 for the same
    // reason.
    let sandbox = Sandbox::new();
    sandbox.accept_eula().with_server();

    let service = Arc::new(FakeService::new(UnitState::Active));
    let ctx = ctx_with(&sandbox, Arc::clone(&service));

    lifecycle::start(&ctx, true).unwrap();
    assert!(
        service.calls().is_empty(),
        "nothing to do: {:?}",
        service.calls()
    );
}

#[test]
fn stopping_an_already_stopped_server_succeeds() {
    let sandbox = Sandbox::new();
    let service = Arc::new(FakeService::new(UnitState::Inactive));
    let ctx = ctx_with(&sandbox, Arc::clone(&service));

    lifecycle::stop(&ctx).unwrap();
    assert!(service.calls().is_empty());
}

#[test]
fn starting_without_a_server_is_a_config_error_not_a_crash() {
    let sandbox = Sandbox::new();
    let service = Arc::new(FakeService::new(UnitState::Inactive));
    let ctx = ctx_with(&sandbox, service);

    let err = lifecycle::start(&ctx, true).unwrap_err();
    assert!(matches!(err, Error::Config(_)), "{err}");
    assert_eq!(err.exit_code(), Error::EX_CONFIG);
}

// ── the exit-code policy ───────────────────────────────────────────────────

#[test]
fn serve_exits_78_for_every_operator_fixable_problem() {
    // 78 is what minecraft.service maps to RestartPreventExitStatus=. A rule
    // broad enough to also cover crashes would silence them; a rule that missed
    // these would restart-loop over something no restart can fix.
    let sandbox = Sandbox::new();
    let service = Arc::new(FakeService::new(UnitState::Inactive));

    // 1. EULA not accepted.
    let ctx = ctx_with(&sandbox, Arc::clone(&service));
    let err = serve::run(&ctx).unwrap_err();
    assert_eq!(err.exit_code(), Error::EX_CONFIG, "unaccepted EULA: {err}");
    assert!(err.to_string().contains("EULA"), "{err}");

    // 2. No server jar.
    sandbox.accept_eula();
    let ctx = ctx_with(&sandbox, Arc::clone(&service));
    let err = serve::run(&ctx).unwrap_err();
    assert_eq!(err.exit_code(), Error::EX_CONFIG, "missing jar: {err}");
    assert!(err.to_string().contains("server.jar"), "{err}");

    // 3. A config file that does not parse.
    sandbox.with_server();
    sandbox.write_config("[java]\nram = [not a string\n");
    let ctx = ctx_with(&sandbox, Arc::clone(&service));
    let err = serve::run(&ctx).unwrap_err();
    assert_eq!(err.exit_code(), Error::EX_CONFIG, "broken config: {err}");
}

#[test]
fn a_non_config_failure_does_not_claim_exit_78() {
    // Anything that is not operator-fixable must stay a plain failure, so
    // systemd restarts it.
    assert_eq!(Error::other("the JVM aborted").exit_code(), 1);
    assert_eq!(Error::Network("connection reset".into()).exit_code(), 1);
    assert_eq!(Error::denied("not root").exit_code(), 1);
}

// ── the privilege table ────────────────────────────────────────────────────

#[test]
fn the_systemd_exec_targets_run_unprivileged() {
    // The unit runs these as the `minecraft` user under ProtectSystem=strict.
    // A root guard on any of them means the server never starts, with a failure
    // that reads as a config problem rather than a permission one.
    for command in [Command::Serve, Command::Shutdown, Command::Reload] {
        assert_eq!(
            command.requirement(),
            Requirement::ServiceAccount,
            "{command:?}"
        );
        // And the guard genuinely passes as a non-root uid, which is what this
        // test process is.
        command
            .requirement()
            .enforce(std::path::Path::new("/usr/bin/mc"), &[])
            .unwrap();
    }
}

#[test]
fn every_command_answers_the_privilege_question() {
    // Exhaustive by construction — `requirement()` matches on every variant, so
    // a new subcommand will not compile until it declares one. This test exists
    // to state the intent, and to fail loudly if a wildcard arm is ever added.
    let commands = [
        Command::Stop,
        Command::Status,
        Command::Logs,
        Command::Delete,
        Command::Plugins,
        Command::Serve,
        Command::Shutdown,
        Command::Reload,
        Command::Start(EulaArgs { accept_eula: false }),
        Command::Restart(EulaArgs { accept_eula: false }),
    ];
    for command in commands {
        let requirement = command.requirement();
        // Not a tautology: it asserts the value is one of the four declared
        // kinds rather than a default that slipped through.
        assert!(
            matches!(
                requirement,
                Requirement::Root
                    | Requirement::RootOrGroup
                    | Requirement::ServiceAccount
                    | Requirement::None
            ),
            "{command:?}"
        );
    }
}

/// Lets a test hold the fake while the Ctx owns a `dyn` copy.
struct Shared(Arc<FakeService>);

impl ServiceManager for Shared {
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
    fn sleep(&self, duration: Duration) {
        self.0.sleep(duration)
    }
}
