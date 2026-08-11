# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Read the skills first

Three skills in `.claude/skills/` carry most of the operational detail, and
using them is much cheaper than rediscovering it:

- **`file-structure`** — which crate owns which concern, how `mc/crates/` and
  `packages/` relate, where a change belongs, and cargo-oriented recipes for
  reading one function instead of a whole file. Use it *before* `ls`/`find`/
  broad `grep`.
- **`testing`** — the four test tiers and how to pick one, the four injectable
  seams, and a catalogue of bugs this project has hit more than once. Several
  are ways a test can pass *vacuously*. Use it before writing a test or calling
  a change verified.
- **`plugin-development`** — the ABI-1 contract: manifest schema, hook events,
  the source-provider protocol, and what must stay in core. Use it before adding
  a subcommand, a hook, or a package.

The `README.md` is the user-facing manual (install, configure, troubleshoot).
Don't restate it here; do check it for drift when behaviour changes.

## Commands

```sh
cd mc                           # the cargo workspace root
cargo test --workspace          # tiers 1-3   ~1 s, no Docker, no root, no network
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check

mc/tests/run.sh                 # tier 4 container suites   ~1 min
mc/tests/run.sh --all           # + the live install matrix ~3 min, real APIs

bash mc/scripts/build.sh mc-server            # → mc/dist/*.deb (needs Debian)
bash apt/scripts/publish.sh mc/dist/<pkg>.deb # needs reprepro + the signing key
```

`mc/tests/run.sh` and both scripts resolve their own roots, so they run from
anywhere; `cargo` needs `mc/`. Cargo lives at `~/.cargo/bin`; the toolchain is
pinned by `mc/rust-toolchain.toml`. Publishing is normally CI's job — pushing to
`main` runs the test gate, builds per architecture, and indexes automatically.

## Repository layout

Two components. **`mc/`** is the product: the cargo workspace (`crates/`), the
Debian packaging trees (`packages/`), its tests and its build script.
**`apt/`** is the distribution: reprepro config, the signing key, `publish.sh`,
and the nginx image that serves the repository — `apt/` is that image's whole
build context, which is why its `COPY` paths carry no prefix.

## Architecture

**Four packages joined by a plugin contract.** `mc-server` ships the dispatcher
(`/usr/bin/mc`); `mc-rcon`, `mc-backup` and `mc-mrpack` each drop a TOML
manifest into `/usr/lib/mc/plugins.d/` and an executable into
`/usr/libexec/mc/`. Core discovers manifests at startup and invokes plugins
across a process boundary — `<bin> command <name>` for a subcommand,
`<bin> hook <event>` with JSON on stdin for a hook.

**Only a registered name is dispatchable.** Resolving to *some* executable is
necessary but not sufficient, or an internal entry point becomes callable from
the command line, skipping the guards, the lock and the config loading its real
entry point performs.

**The `abi` field replaces a versioned `Depends:`.** The old arrangement had
`mc-rcon`'s postinst source another package's private shell library, so a missed
version-floor bump left dpkg configuring the plugin against a library without
the function it called — exit 127, half-installed package. Core now reads the
number and refuses by name. Bump `mc_common::plugin::ABI` only for a genuinely
breaking contract change; it disables every plugin that has not been rebuilt.

**`mc serve` / `mc shutdown` / `mc reload` are a privilege boundary.** They are
systemd's `ExecStart=`/`ExecStop=`/`ExecReload=` and run as the `minecraft` user
under `ProtectSystem=strict`. They are declared `Requirement::ServiceAccount` in
`cli.rs`, must never take a root guard, and must write nothing outside
`MC_BASE`. A root guard on any of them means the server never starts, with a
failure that reads as a config problem rather than a permission one. Two tests
assert this; do not weaken either.

**Two kinds of configuration, split by what they describe.**
`/etc/minecraft/config.toml` is *mc's* — how to run the server: build, Java,
heap, backup policy. `/opt/minecraft/server.properties` is *the server's* —
port, seed, MOTD, difficulty, RCON. The JVM reads and rewrites the second, so mc
keeps no copy of anything in it: read with `properties::Properties::load` at the
point of use, and write the RCON keys only through `mc-rcon`. Preserve that
boundary — a game setting mirrored into `config.toml` can only go stale.

**systemd owns the lifecycle; `mc` drives it.** `systemctl start minecraft`
bypasses the CLI entirely, which is why the EULA and `server.properties` gates
live in `mc serve` rather than in `mc start`. The exit-code policy is
load-bearing: `mc serve` exits **78** (`EX_CONFIG`) for operator-fixable
problems, which the unit maps to `RestartPreventExitStatus=` so it fails visibly
without restart-looping, while every other non-zero exit is a genuine crash that
does restart. `mc start` then polls *and settles*, because `Type=simple` reports
success the moment the process forks.

**Two paths take untrusted input as root.** A `.mrpack` manifest and a backup
archive are attacker-controlled: versions, file paths and URLs are validated
before use, archive members are checked by *type* as well as by name (a hardlink
to `/etc/shadow` passes every name check and then meets `chown -R`), and the
system-managed keys of `server.properties` are re-applied after any merge so a
pack cannot enable RCON with a password of its choosing.

**RAII replaces the cleanup registry.** `lock::LockGuard` and `staging::Staging`
release on every exit path including `?`. Do not add a global registry or an
`atexit`; if something needs unwinding, give it a `Drop`.

## Conventions

**Comments state the constraint the code satisfies**, and are load-bearing —
read one before changing the code it guards, and match the density when adding
code. They describe the code that follows, never what it replaced: no "used
to", no migration notes.

**Tests are named after the property they protect.** `a_pack_cannot_choose_the_
rcon_password`, not `test_merge_2`. `grep -rn "fn a_\|fn the_" crates/` should
read as a list of promises.

**A new `mc` subcommand touches three places:** the `Command` variant in
`cli.rs`, its arm in `Command::requirement()`, and a handler in `commands/`.
Completions are generated from clap, so they cannot go stale. A new *capability*
should usually be a plugin instead — see the `plugin-development` skill.

**Never hardcode a runtime path.** Everything derives from `Paths`, which is a
struct rather than a set of constants so tests can point it at a temp root via
`MC_ROOT`. The same applies inside plugins: `Paths::from_env()`.

**The workspace denies `unwrap`, `expect`, `panic` and indexing.** A panic in
`mc serve` is an outage whose cause is an address in the journal. Tests are
exempt (`mc/clippy.toml`), but integration test crates under `tests/` need the
allow at crate level — the config option only covers `#[cfg(test)]` modules.

**Bump `Version:` in `DEBIAN/control` in the same commit as the change.** CI
publishes on every push touching the build inputs and reprepro regenerates per
run, so a change without a bump republishes the same version number with
different contents — and anyone who installed the earlier build is pinned to it
forever, because apt only upgrades on a higher version.

**`server.properties` is `minecraft:minecraft` 0640, always.** It holds the RCON
password, and 0640 is readable only because the owner is the service account. A
root-owned copy is a silent failure: the JVM can neither read nor write it,
comes up on compiled-in defaults, and generates a stray world beside the real
one. Anything that writes it calls `properties::secure`.
