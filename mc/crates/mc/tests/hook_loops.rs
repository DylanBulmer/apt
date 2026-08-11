//! Tier 2 — the hook loop guard, driven through real process boundaries.
//!
//! The guard's whole subject is state that crosses a `fork`/`exec`, so these
//! tests never set `MC_HOOK_DEPTH` or `MC_HOOK_CHAIN` in this process: a
//! fixture plugin re-enters core by running the real `mc` binary, exactly as
//! `mc-backup`'s subcommand does, and the nesting state travels the way it does
//! in production. Where a chain has to be injected rather than grown, it is set
//! on the child's `Command` — never on the test process, whose environment
//! every other test in this binary reads concurrently.
//!
//! Every fixture logs the state it inherited, so "skipped" and "ran" are told
//! apart by a side effect rather than by the absence of an error.

// Integration tests: a panic IS the failure report here, so the workspace's
// no-unwrap/no-panic lints are relaxed for this crate only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;
use std::process::{Command, Output};

use common::*;
use mc::commands::shutdown;
use mc::context::Ctx;
use mc_common::Paths;
use mc_common::plugin::{Event, Registry};
use mc_common::service::UnitState;

/// The dispatcher a plugin re-enters, as installed.
const MC: &str = env!("CARGO_BIN_EXE_mc");

const PRE_STOP: &str = r#"
abi = 1
name = "{NAME}"
bin = "{BIN}"
[[hooks]]
event = "pre-stop"
"#;

/// A console: it answers `console probe`, so an election reaches it.
const CONSOLE: &str = r#"
abi = 1
name = "{NAME}"
bin = "{BIN}"
[[commands]]
name = "{NAME}"
[[hooks]]
event = "pre-stop"
[[providers]]
kind = "console"
name = "{NAME}"
priority = 10
"#;

/// A hook that may legitimately abort the operation around it — the case a
/// loop trip must NOT turn into a failure.
const FATAL_POST_INSTALL: &str = r#"
abi = 1
name = "{NAME}"
bin = "{BIN}"
[[hooks]]
event = "post-install"
fatal = true
"#;

fn ctx(sandbox: &Sandbox) -> Ctx {
    Ctx {
        paths: sandbox.paths.clone(),
        http: Box::new(mc_common::http::fake::FakeHttp::new()),
        service: Box::new(service(UnitState::Active)),
        packages: Box::new(mc_common::packages::fake::FakePackages::new()),
        argv: vec!["shutdown".to_string()],
    }
}

fn install(sandbox: &Sandbox, name: &str, manifest: &str) {
    sandbox.install_plugin(name, &manifest.replace("{NAME}", name));
}

/// Replace a fixture's binary with one that records the nesting state it
/// inherited and, when `reenter` is set, runs `mc shutdown` the way a plugin
/// that shells out to core does.
///
/// The re-entry is bounded by a counter shared across every level: without the
/// guard in core this recurses until the machine gives up, and a test that
/// hangs reports nothing at all. Six invocations is well past `MAX_HOOK_DEPTH`,
/// so the bound is only ever reached by a regression.
fn fixture(sandbox: &Sandbox, name: &str, reenter: bool, exit: u8) {
    let log = sandbox.dir.path().join(format!("{name}.log"));
    let counter = sandbox.dir.path().join("reentries");
    let nested = sandbox.dir.path().join("nested.err");
    let bin = sandbox.paths.libexec(&format!("mc-{name}"));

    // Guarded on `hook`: a probe that re-entered core would recurse through the
    // election rather than through the dispatch these tests are about.
    let reentry = if reenter {
        format!(
            "if [ \"$1\" = hook ]; then\n\
             n=$(cat {counter} 2>/dev/null || echo 0)\n\
             n=$((n+1))\n\
             echo $n > {counter}\n\
             if [ \"$n\" -lt 6 ]; then {MC} shutdown >> {nested} 2>&1; fi\n\
             fi\n",
            counter = counter.display(),
            nested = nested.display(),
        )
    } else {
        String::new()
    };

    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\n\
             printf '%s depth=%s chain=[%s]\\n' \"$*\" \"${{MC_HOOK_DEPTH-unset}}\" \"${{MC_HOOK_CHAIN-unset}}\" >> {log}\n\
             {reentry}exit {exit}\n",
            log = log.display(),
        ),
    )
    .unwrap();
    mc_common::fsx::apply_owner_mode(&bin, None, 0o755).unwrap();
}

/// What the nested `mc` processes printed — where a refusal is reported.
fn nested_output(sandbox: &Sandbox) -> String {
    std::fs::read_to_string(sandbox.dir.path().join("nested.err")).unwrap_or_default()
}

fn reentries(sandbox: &Sandbox) -> u32 {
    std::fs::read_to_string(sandbox.dir.path().join("reentries"))
        .unwrap_or_default()
        .trim()
        .parse()
        .unwrap_or(0)
}

/// Lines a fixture logged for a hook invocation, one per time it actually ran.
fn hook_runs(sandbox: &Sandbox, name: &str) -> Vec<String> {
    sandbox
        .plugin_log(name)
        .lines()
        .filter(|l| l.starts_with("hook "))
        .map(str::to_owned)
        .collect()
}

/// The real dispatcher, with a chain injected on the child rather than here.
fn mc(sandbox: &Sandbox, args: &[&str], chain: &[(&str, &str)]) -> Output {
    let mut command = Command::new(MC);
    command
        .args(args)
        .env("MC_ROOT", sandbox.paths.root())
        .env_remove("MC_HOOK_DEPTH")
        .env_remove("MC_HOOK_CHAIN");
    for (key, value) in chain {
        command.env(key, value);
    }
    command.output().unwrap()
}

#[test]
fn a_hook_that_re_enters_its_own_event_is_skipped_instead_of_looping() {
    let sandbox = Sandbox::new();
    install(&sandbox, "rcon", PRE_STOP);
    fixture(&sandbox, "rcon", true, 0);

    shutdown::run(&ctx(&sandbox)).unwrap();

    assert_eq!(reentries(&sandbox), 1, "the hook really did re-enter mc");
    assert_eq!(
        hook_runs(&sandbox, "rcon").len(),
        1,
        "ran once, not once per level: {:?}",
        hook_runs(&sandbox, "rcon")
    );

    let nested = nested_output(&sandbox);
    assert!(
        nested.contains("skipping plugin 'rcon' hook pre-stop"),
        "the refusal names the plugin and the event: {nested}"
    );
    assert!(
        nested.contains("already running further up this chain"),
        "{nested}"
    );
    assert!(
        nested.contains("Chain: rcon:pre-stop"),
        "the warning shows the operator what looped: {nested}"
    );
}

#[test]
fn a_loop_trip_skips_only_the_offending_plugin() {
    // The guard is per plugin:event, not per dispatch. A console that re-enters
    // core must not take the backup plugin's hook down with it.
    let sandbox = Sandbox::new();
    install(&sandbox, "aaa", PRE_STOP);
    install(&sandbox, "zzz", PRE_STOP);
    fixture(&sandbox, "aaa", true, 0);
    fixture(&sandbox, "zzz", false, 0);

    shutdown::run(&ctx(&sandbox)).unwrap();

    assert_eq!(
        hook_runs(&sandbox, "aaa").len(),
        1,
        "the offender is skipped"
    );
    assert_eq!(
        hook_runs(&sandbox, "zzz").len(),
        2,
        "once inside the nested dispatch and once in the outer one: {:?}",
        hook_runs(&sandbox, "zzz")
    );
}

#[test]
fn the_chain_a_child_parses_back_is_the_one_its_parent_handed_it() {
    // Round trip across a real process boundary: the outer dispatch serialises
    // depth 1 and one link, the nested `mc` parses them, enters its own hook
    // and hands on depth 2 and both links.
    let sandbox = Sandbox::new();
    install(&sandbox, "aaa", PRE_STOP);
    install(&sandbox, "zzz", PRE_STOP);
    fixture(&sandbox, "aaa", true, 0);
    fixture(&sandbox, "zzz", false, 0);

    shutdown::run(&ctx(&sandbox)).unwrap();

    assert_eq!(
        hook_runs(&sandbox, "aaa"),
        vec!["hook pre-stop depth=1 chain=[aaa:pre-stop]"],
    );
    assert_eq!(
        hook_runs(&sandbox, "zzz"),
        vec![
            "hook pre-stop depth=2 chain=[aaa:pre-stop,zzz:pre-stop]",
            "hook pre-stop depth=1 chain=[zzz:pre-stop]",
        ],
    );
}

#[test]
fn dispatch_is_refused_once_the_chain_reaches_the_depth_limit() {
    // `mmm` is in nobody's chain, so the only thing that can stop it is depth.
    let sandbox = Sandbox::new();
    for name in ["aaa", "mmm", "zzz"] {
        install(&sandbox, name, PRE_STOP);
    }
    fixture(&sandbox, "aaa", true, 0);
    fixture(&sandbox, "mmm", false, 0);
    fixture(&sandbox, "zzz", true, 0);

    shutdown::run(&ctx(&sandbox)).unwrap();

    let nested = nested_output(&sandbox);
    assert!(
        nested.contains("hook dispatch is already 2 levels deep (limit 2)"),
        "a plugin the chain has never seen is still refused: {nested}"
    );
    for name in ["aaa", "mmm", "zzz"] {
        assert!(
            hook_runs(&sandbox, name)
                .iter()
                .all(|line| line.contains("depth=1") || line.contains("depth=2")),
            "nothing runs beyond the limit: {:?}",
            hook_runs(&sandbox, name)
        );
    }
    // The positive control for that assertion: the second level really was
    // reached, so "never deeper than 2" is not "never ran".
    assert!(
        hook_runs(&sandbox, "mmm")
            .iter()
            .any(|line| line.contains("depth=2")),
        "{:?}",
        hook_runs(&sandbox, "mmm")
    );
}

#[test]
fn a_probe_inherits_the_chain_rather_than_starting_a_fresh_one() {
    // A console is probed on the way into every console-exclusive event. If the
    // probe reset the chain, a plugin could buy itself a fresh budget by being
    // elected, which is the loop this guard exists to stop.
    let sandbox = Sandbox::new();
    install(&sandbox, "aaa", PRE_STOP);
    install(&sandbox, "mgmt", CONSOLE);
    fixture(&sandbox, "aaa", true, 0);
    fixture(&sandbox, "mgmt", false, 0);

    shutdown::run(&ctx(&sandbox)).unwrap();

    let log = sandbox.plugin_log("mgmt");
    assert!(
        log.contains("console probe depth=0 chain=[]"),
        "the outer election sees an empty chain, and BOTH variables are set: {log}"
    );
    assert!(
        log.contains("console probe depth=1 chain=[aaa:pre-stop]"),
        "the nested election inherits it: {log}"
    );
}

#[test]
fn a_plugin_subcommand_inherits_the_chain_rather_than_starting_a_fresh_one() {
    // `mc-backup`'s hook runs `mc backup`; that subcommand is exec'd, not
    // spawned, and must carry the chain through unchanged rather than reset it.
    let sandbox = Sandbox::new();
    install(&sandbox, "mgmt", CONSOLE);
    fixture(&sandbox, "mgmt", false, 0);

    let output = mc(
        &sandbox,
        &["mgmt", "say", "hello"],
        &[
            ("MC_HOOK_DEPTH", "1"),
            ("MC_HOOK_CHAIN", "backup:pre-backup"),
        ],
    );
    assert!(output.status.success(), "{output:?}");

    let log = sandbox.plugin_log("mgmt");
    assert!(
        log.contains("command mgmt say hello depth=1 chain=[backup:pre-backup]"),
        "passed through unchanged: {log}"
    );
}

#[test]
fn a_malformed_depth_degrades_to_zero_rather_than_refusing_every_hook() {
    // The variables are ours, so a bad one means an operator set it by hand.
    // Refusing every hook over that would break a shutdown far more surely than
    // the loop the guard exists to stop.
    for value in ["banana", "-1", "", "   ", "99999999999999999999"] {
        let sandbox = Sandbox::new();
        install(&sandbox, "rcon", PRE_STOP);
        fixture(&sandbox, "rcon", false, 0);

        let output = mc(&sandbox, &["shutdown"], &[("MC_HOOK_DEPTH", value)]);
        assert!(output.status.success(), "{value:?}: {output:?}");
        assert_eq!(
            hook_runs(&sandbox, "rcon").len(),
            1,
            "MC_HOOK_DEPTH={value:?} must not disable hooks"
        );
    }

    // The control: a well-formed value at the limit does refuse, so the case
    // above is a degradation and not a guard that never fires.
    let sandbox = Sandbox::new();
    install(&sandbox, "rcon", PRE_STOP);
    fixture(&sandbox, "rcon", false, 0);
    let output = mc(&sandbox, &["shutdown"], &[("MC_HOOK_DEPTH", "2")]);
    assert!(output.status.success(), "{output:?}");
    assert!(hook_runs(&sandbox, "rcon").is_empty());
}

#[test]
fn a_loop_trip_is_never_fatal_even_for_an_event_whose_hooks_may_be() {
    // The single case that must not regress. `post-install` permits a fatal
    // hook and this one declares itself fatal and exits non-zero, so if a
    // refused hook were reported like a failed one, a plugin that shelled out
    // to `mc` would abort the install it was contributing to.
    let sandbox = Sandbox::new();
    install(&sandbox, "prov", FATAL_POST_INSTALL);
    fixture(&sandbox, "prov", false, 1);

    for chain in [
        vec![
            ("MC_HOOK_CHAIN", "prov:post-install"),
            ("MC_HOOK_DEPTH", "1"),
        ],
        vec![("MC_HOOK_DEPTH", "2")],
    ] {
        std::fs::remove_file(sandbox.dir.path().join("prov.log")).ok();
        assert_eq!(
            nested_post_install(&sandbox, &chain),
            "ok",
            "a loop trip is a skip, not a failure: {chain:?}"
        );
        assert!(
            hook_runs(&sandbox, "prov").is_empty(),
            "and the hook really was skipped: {:?}",
            hook_runs(&sandbox, "prov")
        );
    }

    // The positive control. Without a chain the same fixture runs and its
    // failure IS fatal — so "ok" above is the refusal, not a hook that could
    // never have failed in the first place.
    std::fs::remove_file(sandbox.dir.path().join("prov.log")).ok();
    let result = nested_post_install(&sandbox, &[]);
    assert!(result.starts_with("err"), "{result}");
    assert_eq!(hook_runs(&sandbox, "prov").len(), 1);
}

/// Dispatch `post-install` in a child process that inherited `chain`.
///
/// `run_hook` reads the chain from the environment, and no `mc` subcommand
/// dispatches `post-install` without root, so the child is this test binary
/// re-run against one helper test. Setting the variables in-process instead
/// would hand them to every other test in this binary.
fn nested_post_install(sandbox: &Sandbox, chain: &[(&str, &str)]) -> String {
    let result = sandbox.dir.path().join("nested-result");
    std::fs::remove_file(&result).ok();

    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "the_far_side_of_a_nested_post_install_dispatch"])
        .env("MC_TEST_NESTED_ROOT", sandbox.paths.root())
        .env_remove("MC_HOOK_DEPTH")
        .env_remove("MC_HOOK_CHAIN");
    for (key, value) in chain {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "the helper test failed: {output:?}"
    );

    std::fs::read_to_string(&result).expect("the helper wrote its outcome")
}

/// The far side of [`nested_post_install`]. A no-op in an ordinary run.
#[test]
fn the_far_side_of_a_nested_post_install_dispatch() {
    let Ok(root) = std::env::var("MC_TEST_NESTED_ROOT") else {
        return;
    };
    let paths = Paths::with_root(&root);
    let outcome = match Registry::discover(&paths).run_hook(
        &paths,
        Event::PostInstall,
        &serde_json::json!({}),
    ) {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("err: {e}"),
    };
    std::fs::write(Path::new(&root).join("nested-result"), outcome).unwrap();
}
