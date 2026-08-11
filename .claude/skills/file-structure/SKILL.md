---
name: file-structure
description: Map of and exploration guide for the apt.bulmer.dev monorepo — the mc/ (product) and apt/ (distribution) split, which crate owns which concern, how crates/ and packages/ relate, where a given kind of change belongs, and cargo-oriented recipes for finding code without loading whole files. Use BEFORE ls/find/grep when locating code, deciding where a change belongs, or orienting in the workspace. For running or writing tests see the `testing` skill; for adding a plugin see `plugin-development`.
---

# apt.bulmer.dev structure & exploration

A monorepo with two components: **`mc/`**, a Minecraft server manager shipped as
four Debian packages, and **`apt/`**, the repository that publishes them. Read
this instead of running `ls -R` or broad greps.

```
mc/                          the product
  Cargo.toml Cargo.lock      the workspace; the lock IS tracked
  rust-toolchain.toml        pins rustc for local builds, CI and the container
  clippy.toml                relaxes the no-panic lints for #[cfg(test)] only
  crates/mc-common/          shared library, linked statically by everything
  crates/mc/                 → /usr/bin/mc                    (mc-server)
  crates/mc-rcon/            → /usr/libexec/mc/mc-rcon, /usr/bin/rcon
  crates/mc-backup/          → /usr/libexec/mc/mc-backup
  crates/mc-mrpack/          → /usr/libexec/mc/mc-mrpack
  crates/xtask/              build-time only; renders mc.1 from the clap tree
  crates/mc/man/             the prose half of mc.1 (raw roff fragments)
  packages/*/                DEBIAN metadata, units, conffiles, manifests,
                             usr/share/man/ (every page except mc.1)
  scripts/build.sh           builds one .deb
  tests/run.sh               tier-4 container suites; see the `testing` skill
  dist/ staging/ target/     build output, gitignored

apt/                         the distribution
  conf/distributions         reprepro config; db/ dists/ pool/ are generated
  bulmer.asc                 the public signing key
  scripts/publish.sh         signs a .deb and includes it (reprepro -b apt)
  Dockerfile nginx.conf      the image that serves the repo — apt/ IS its
                             build context, so its COPY paths have no prefix
  k8s/                       deployment of that image

.github/workflows/publish.yml  test gate → per-arch build → publish → image
CLAUDE.md README.md LICENSE .claude/skills/
```

**Inside `mc/`, two trees.** `crates/` is everything compiled;
`packages/<name>/` mirrors the target filesystem root — everything not.
`mc/packages/mc-server/etc/minecraft/config.toml` installs to
`/etc/minecraft/config.toml`. `DEBIAN/` is control metadata and the one
directory that is not a real install path.

`mc/scripts/build.sh` joins them: it cargo-builds the named binaries and copies
them into a staging copy of `packages/<name>/`. **The `[[bin]]` names in
`Cargo.toml` and the `case` in `build.sh` must agree** — a rename on one side
produces a `.deb` with a missing executable.

**Cargo commands run from `mc/`**, which is the workspace root; `tests/run.sh`
works from anywhere because it resolves its own root.

## Which crate owns what

**`mc-common`** — anything two binaries need. Nothing here knows about a
specific command.

| Module | Owns |
|---|---|
| `paths` | every filesystem location, derived from one root (`MC_ROOT`) |
| `config` | `/etc/minecraft/config.toml`, `ServerType` |
| `properties` | `server.properties`: parse, set, merge, `secure`, `rcon_port`, managed keys |
| `plugin` | the ABI-1 contract: manifest, `Registry`, hook dispatch, `exec_command` |
| `privilege` | `require_root`, `require_root_or_group`, sudo re-exec, `Requirement` |
| `lock` | the re-entrant RAII process lock |
| `staging` | RAII staging dirs, `safe_relative_path`, `resolve_under` |
| `http` | the `Http` trait, `UreqHttp`, `host_allowed`, `fake::FakeHttp` |
| `service` | the `ServiceManager` trait, `Systemctl`, `fake::FakeService` |
| `packages` | the `PackageManager` trait, `Apt`, `fake::FakePackages` |
| `java` | required version, banner parsing, binary lookup, GC flag presets |
| `hash`, `version`, `eula`, `chat`, `fsx`, `ui`, `error` | as named |

**`mc`** — the dispatcher and everything core does itself.

| Module | Owns |
|---|---|
| `cli` | the clap surface **and the privilege table** (`Command::requirement`) |
| `dispatch` | routing, including to plugins; `mc plugins`; completions |
| `commands/install` | `mc install` / `mc upgrade`, `install_artifact`, `initialise_settings` |
| `commands/lifecycle` | start/stop/restart/status/logs, `start_and_verify` |
| `commands/serve` | `ExecStart=` — the EULA and properties gates, the JVM launch |
| `commands/shutdown` | `ExecStop=` — dispatches `pre-stop` |
| `commands/reload` | `ExecReload=` |
| `commands/delete` | `mc delete` |
| `manual` | `mc man` — which page answers a topic, and the handoff to man(1) |
| `sources/{vanilla,paper,fabric,neoforge}` | one upstream each, behind the `Source` trait |

**Plugins** — `mc-rcon` (`protocol`, `session`, `password`, `players`,
`countdown`), `mc-backup` (`archive`, `rotation`), `mc-mrpack` (`manifest`).

## The privilege boundary

The shell version enforced it by physically splitting the library in two. Now it
is a declared value plus a test.

`mc serve`, `mc shutdown` and `mc reload` are systemd's `ExecStart=`/`ExecStop=`/
`ExecReload=` and **run as the `minecraft` user under `ProtectSystem=strict`**.
They are `Requirement::ServiceAccount` in `cli.rs`, must never take a root
guard, and must write nothing outside `MC_BASE`. A root guard on any of them
means the server never starts, with a failure that reads as a config problem
rather than a permission one. Asserted by
`the_systemd_exec_targets_run_unprivileged` (tier 2) and by
`integration/access-control` (tier 4).

## Two kinds of configuration

`/etc/minecraft/config.toml` is **mc's**: how to run the server — build, Java,
heap, backup policy. `/opt/minecraft/server.properties` is **the server's**:
port, seed, MOTD, difficulty, RCON. The JVM reads and rewrites the second, so
nothing from it is mirrored into the first — read it with
`properties::Properties::load` at the point of use. `rcon_port` resolves
`rcon.port` → game port + 10 → stock port + 10.

## Exploration recipes

```sh
# What a symbol is and where — faster and more accurate than grep.
(cd mc && cargo doc --workspace --no-deps --open)

# Every caller of a function, with types.
grep -rn "properties::secure" mc/crates/

# One module, not the whole crate.
sed -n '/^pub fn merge/,/^}/p' mc/crates/mc-common/src/properties.rs

# What a crate exposes.
grep -n "^pub " mc/crates/mc-common/src/lib.rs

# Trace a setting end to end: default → config → consumer.
grep -rn "backup.keep\|BackupConfig" mc/crates/ mc/packages/

# Does this change need a version bump? (It does.)
grep -rn "^Version:" mc/packages/*/DEBIAN/control
```

**Read the tests to learn the contract.** Every non-obvious behaviour has a
test named after the property it protects
(`a_pack_cannot_choose_the_rcon_password`,
`newest_means_document_order_not_alphabetical_order`). `grep -rn "fn a_\|fn the_"
mc/crates/` is a readable index of what this code promises.

### Anti-patterns

- **Don't `ls -R`** — the layout is above and does not change often.
- **Don't hardcode a path.** Everything comes from `Paths`, which is a struct so
  tests can point it at a temp root.
- **Don't search generated trees.** `mc/{target,dist,staging}/` and
  `apt/{db,dists,pool}/` are build output.
- **Don't infer intent from code alone.** Comments here state the constraint the
  code satisfies — the sentinel ordering in `mc/crates/mc-rcon/src/protocol.rs`, the
  managed-key re-apply in `properties::merge`, the `ZGenerational` version
  window in `java::default_flags`. Read the comment before changing the code it
  guards.
- **Don't write changelog comments.** A comment describes the code that follows,
  not what it replaced. No "used to", no migration notes.

## Where a change belongs

| Change | Goes |
|---|---|
| New `mc` subcommand | `mc/crates/mc/src/cli.rs` (variant **and** `requirement()`) + `dispatch.rs` + a handler in `commands/` — mc.1 picks it up on its own |
| Prose in mc(1) — a new section, a file, an exit code | `mc/crates/mc/man/mc.1.{head,tail}.roff` |
| A plugin's manual page | `mc/packages/<pkg>/usr/share/man/man1/mc-<plugin>.1` — the name `mc man` resolves to |
| A new config key's documentation | `mc/packages/mc-server/usr/share/man/man5/mc-config.5` **and** the shipped `config.toml` (a test compares them) |
| New server type | a module in `mc/crates/mc/src/sources/` + a `ServerType` variant |
| New capability, new command, or a new hook consumer | **a new plugin package** — see `plugin-development` |
| New hook event | `plugin::Event` + the dispatch site + the `plugin-development` table |
| Launch/JVM/GC behaviour | `mc/crates/mc/src/commands/serve.rs`, `java::default_flags` |
| Shutdown behaviour | `mc-rcon`'s `pre-stop` hook (recompute `TimeoutStopSec` if timings move) |
| Install-time ownership | `mc/packages/mc-server/DEBIAN/postinst` |
| New setting for how mc runs the server | `mc-common/src/config.rs` + `mc/packages/mc-server/etc/minecraft/config.toml` |
| New setting for what the game does | `server.properties` — never mirrored into config.toml |

## Runtime paths

| Accessor | Path | Notes |
|---|---|---|
| `base()` | `/opt/minecraft` | `minecraft:minecraft` 0750. The only `ReadWritePaths=`. |
| `config_dir()` | `/etc/minecraft` | `config.toml` (conffile), `server.passwd` |
| `backup_dir()` | `/var/backups/minecraft` | **root:root 0700 on purpose** — never the service account |
| `lock_file()` | `/run/minecraft/mc.lock` | serialises install/upgrade/backup/restore |
| `plugins_dir()` | `/usr/lib/mc/plugins.d` | manifests |
| `libexec_dir()` | `/usr/libexec/mc` | plugin binaries; deliberately not on `PATH` |

`server.properties` is always `minecraft:minecraft` 0640. Owner matters as much
as mode — root-owned means the JVM silently falls back to compiled-in defaults.

## Shipping

Version lives only in `DEBIAN/control`; there is no `DEBIAN/changelog`. CI runs
the test gate, builds per architecture, then publishes — and reprepro
regenerates its trees per run, so **any change without a version bump is
published under the old version and never reaches installed systems.** Bump with
the change, not after.

All four packages are architecture-specific now: each ships compiled binaries,
so every architecture leg builds all four.
