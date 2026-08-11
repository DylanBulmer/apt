---
name: testing
description: How to test and verify changes to the mc packages — the four test tiers and how to pick one, the four injectable seams that keep most testing off Docker and off root, what still cannot be verified, and a catalogue of bugs this project has hit more than once (porting traps, packaging traps, upstream API drift) with the resolution for each. Use BEFORE writing a test, running one, claiming a change is verified, or debugging a suite that fails for an unclear reason.
---

# Testing apt.bulmer.dev

## Run it

```sh
cargo test --workspace            # tiers 1-3   ~1 s, no Docker, no root, no network
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check

tests/run.sh                      # tier 4      ~1 min, Docker, root
tests/run.sh --all                # + the live install matrix   ~3 min, real APIs
tests/run.sh integration/plugins  # one suite
tests/run.sh --shell              # container shell, packages installed
```

The toolchain is pinned by `rust-toolchain.toml`. Cargo is at `~/.cargo/bin`;
add `. "$HOME/.cargo/env"` to your profile if `cargo` is not on `PATH`.

## The four tiers, and how to pick one

**Default to the lowest tier that can express the assertion.** The most common
way to write a bad test here is reaching for a container when tier 2 would do:
it is a hundred times slower, it cannot script timing, and it makes the failure
harder to read.

| Tier | Where | Needs | For |
|---|---|---|---|
| 1 | `#[cfg(test)]` in the crate | nothing | pure logic: parsers, validators, schedules, encodings |
| 2 | `crates/*/tests/*.rs` | nothing | real command handlers against a temp root |
| 3 | `#[cfg(test)]` / `tests/` | nothing | the security regression corpus — every hostile input that must be refused |
| 4 | `tests/suites/integration/` | Docker + root | dpkg, real ownership, the service account, plugin install/removal |

Tier 4 also holds `install-types`, the only thing that reaches the real
upstream APIs.

## The four injectable seams

These exist so tiers 2 and 3 can drive real code without root or a network.
Reach for them before reaching for a container.

```rust
// 1. Paths — every location derives from a root. Never a constant path.
let paths = Paths::with_root(tempdir.path());     // production: Paths::system()

// 2. Http — scripted routes; an unregistered URL is an ERROR, not empty success
let http = FakeHttp::new().route(URL, body).fail(OTHER, "connection reset");
assert!(http.requests().is_empty());              // "this never hit the network"

// 3. ServiceManager — scripted unit states and recorded calls
let svc = FakeService::new(UnitState::Inactive)
    .script([UnitState::Inactive, UnitState::Active, UnitState::Failed]);
assert_eq!(svc.calls(), vec!["stop minecraft", "start minecraft"]);
assert_eq!(svc.slept(), Duration::from_secs(300)); // countdowns without waiting

// 4. PackageManager — records `apt install` requests instead of making them
assert_eq!(packages.installed(), vec!["openjdk-21-jre-headless"]);
```

`Ctx` holds all four, so a tier-2 test constructs one and calls the handler:

```rust
let ctx = Ctx { paths, http: Box::new(http), service: Box::new(svc),
                packages: Box::new(FakePackages::new()), argv: vec![] };
install::install(&ctx, args)?;
```

**`ServiceManager` is why `Type=simple` is testable at all.** A real systemd
cannot be asked to produce "start returned 0, then the unit failed half a second
later" on demand, and that is exactly the case `Type=simple` makes routine.

## What still cannot be verified

`systemd-analyze verify` on the unit (the container has a stub, not systemd);
anything needing a live Minecraft server with real players — the countdown is
tested against a fake, not a real one; and whether `--initSettings` works on a
launcher upstream has not been checked against. Say so rather than implying it
was covered.

---

# Recurring issues

Each of these has cost real debugging time in this repository.

## Porting traps

**`serde_json::Value`'s map is a `BTreeMap` and sorts keys as strings.** The
PaperMC v3 index is an object keyed by release family, documented as newest
first, and jq preserved document order. Without `serde_json/preserve_order` the
`"1.20"` family sorts before `"1.21"` and "newest" silently becomes
"alphabetically first" — an installable, verified, *wrong* version.
→ The feature is enabled in the `mc` crate and asserted by
`newest_means_document_order_not_alphabetical_order`.

**A partial pattern match must be decisive, not retried.** The player-count
parser tried each reply dialect in turn. `There are 99999999999 of a max of 20
players online` failed the first pattern's *number* check, fell through, and
matched the **max** — reporting 20 players on a server whose count was unknown.
→ When the shape matches, commit to it and return `Unknown` if the value does
not parse. Never let a failed sub-check fall into a looser pattern.

**A config field that records state is not a field that records a request.**
`server.version` is what is *installed* — install resolves `latest` and pins the
result. The shell version read it back as the *target*, so a bare `mc upgrade`
resolved the pin to itself and reported "nothing to upgrade" forever.
→ A bare `mc upgrade` targets `latest`; the pin is only ever compared against.

**A fake that does not mirror the real thing fails tests for the wrong reason.**
`FakeService::start` originally recorded the call without changing state, so
every start polled to timeout. → A fake's *happy path* must match reality; the
scripted queue is for producing the interesting cases, not the ordinary one.

**`clippy.toml`'s `allow-unwrap-in-tests` does not cover `tests/`.** It applies
to `#[cfg(test)]` modules only; integration test crates are separate crates and
the workspace's `unwrap_used`/`panic` denials hit them.
→ Each file in `tests/` carries a crate-level
`#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`.

**Good libraries refuse to build hostile fixtures.** The `tar` crate will not
*write* a member path containing `..`, which is correct and exactly why the
traversal fixture cannot go through it.
→ Hand-write the 512-byte header (`build_raw` in `mc-backup/src/archive.rs`),
and keep `the_raw_fixture_builder_produces_a_readable_archive` beside it — a
malformed hand-built header would make every traversal case "fail validation"
for the wrong reason, and the checks would never actually run.

## Packaging traps

**`${shlibs:Depends}` is a debhelper variable and nothing substitutes it under
plain `dpkg-deb`.** Shipped literally, it produces a package apt refuses — and
the `.deb` builds, packages and uploads perfectly happily.
→ `scripts/build.sh` resolves it with `dpkg-shlibdeps`, falling back to `libc6`.
CI and `integration/packaging` both assert no `${` survives in `Depends`.

**A binary built on a newer glibc than the target fails at exec.** The symptom
is `version GLIBC_2.xx not found` — after a successful build, package and
install. → Build in `rust:1-trixie` (`tests/Dockerfile.build`), never on the
host and never on a newer base image.

**`systemctl is-system-running` is a health check, not a presence check.** It
succeeds only for exactly `running` and returns non-zero for `degraded` — any
machine with one unrelated failed unit. Gating a postinst on it silently skips
`daemon-reload`, and if the failed unit is your own the skip is self-sustaining.
→ Gate on `[ -d /run/systemd/system ]`. The stub reports degraded, so the
healthy case is never the only one tested.

**A change without a version bump never reaches installed systems.** CI
publishes on every push touching the build inputs and reprepro regenerates per
run, so several commits under one unchanged `Version:` republish that number as
different artifacts; whoever installed the earlier one is pinned to it forever.
→ Bump in the *same commit*. Check what the mirror actually holds:

```sh
curl -s https://apt.bulmer.dev/dists/stable/main/binary-amd64/Packages | grep -A1 '^Package: mc-'
```

**A plugin manifest and its binary are shipped by the same package and can still
drift.** A rename in `Cargo.toml`'s `[[bin]]` that is not made in
`scripts/build.sh` produces a `.deb` with no executable in it.
→ `build.sh` fails loudly on a missing binary; `integration/packaging` asserts
every manifest's `bin` exists and is executable.

**The ABI number can be got wrong the same way the old version floor was.**
Bumping `mc_common::plugin::ABI` silently disables every installed plugin that
has not been rebuilt. → Bump it only for a genuinely breaking contract change,
and release every package together when you do. `integration/plugins` asserts a
mismatch is refused *by name* and does not disturb anything else.

**Root-owned `server.properties` is a silent killer.** 0640 is readable only
because the owner is the service account; root-owned, the JVM can neither read
nor write it, falls back to compiled-in defaults, and generates a stray world
next to the real one. → Anything that writes it calls `properties::secure`.
Assert mode *and* owner.

## Upstream drift

**PaperMC v2 is sunset** — `api.papermc.io/v2` returns HTTP 410. v3 lives at
`fill.papermc.io/v3` with a different shape (see the porting trap above).
`tests/run.sh --all` is what catches this class of breakage; the offline
fixtures cannot.

**Don't judge a launcher by its exit code.** NeoForge's FML wrapper exits 1 even
when `--initSettings` succeeded: the flag returns without starting the server
thread, so FML logs `Initialized '…/server.properties'` and *then* throws.
→ Assert on the outcome — did the file gain `level-seed` — not on the proxy.

**Fabric publishes no hash for its server jar.** `/server/jar` is a
dynamically-assembled launcher with no sidecar and nothing in the meta JSON, so
it is the one artifact trusted on TLS alone. Every other source fails closed
without a published digest. → Documented in `sources/fabric.rs`; do not "fix"
it by making verification optional everywhere.

**`java::required_major` is wrong for two known inputs.** `1.17` resolves to
Java 8 (it needs 16) and an old pinned NeoForge like `21.1.66` parses as
minor=1 and lands on 8. Carried over deliberately from the shell version and
pinned by `known_gap_versions_that_resolve_to_the_wrong_runtime`, so a "fix"
that ignores NeoForge's overlapping version scheme fails loudly.

## Test-environment gotchas

- **Mount the repo read-only** and copy it inside before building —
  `scripts/build.sh` writes `dist/`, `staging/` and `target/`.
- **Cargo caches live in named Docker volumes** (`mc-cargo-registry`,
  `mc-cargo-target`). Without them every run recompiles the dependency graph,
  which dominates the suite's runtime far more than any test.
- **Bake the JREs into the image.** Letting `mc install` apt-install one per run
  dominates the install matrix.
- **Install core before any plugin.** Each plugin `Depends: mc-server`, and dpkg
  refuses to configure a package whose dependency is not configured yet.
- **Grep unit files for directives, not strings.** `check_lacks 'SuccessExitStatus'`
  matched the comment that explains why there is no `SuccessExitStatus=`.
  Anchor on `^[[:space:]]*Directive=`.
- **Assert the property, not the exit status.** `mc reload` legitimately fails
  with no server running; what the privilege suite actually tests is that it
  never fails *for lack of privilege*.
