---
name: plugin-development
description: How to build an mc plugin — the ABI-1 manifest schema, the hook event list and their payloads, the source-provider protocol, what must stay in core versus what belongs in a plugin, which mc-common APIs to reuse rather than reimplement, and the packaging checklist. Use BEFORE adding a subcommand, a lifecycle hook, a new server source, or a new package to this repo.
---

# Building an mc plugin

A plugin is a `.deb` that drops a TOML manifest into `/usr/lib/mc/plugins.d/`
and an executable into `/usr/libexec/mc/`. Core discovers the manifest at
startup and invokes the executable across a process boundary.

**Adding capability to mc means adding a package, not editing core.** If a
change makes core know about a specific plugin by name, it is in the wrong
place.

## Why out-of-process

Rust has no stable ABI, so a `dlopen`-based plugin would have to be pinned to an
exact core version and rebuilt in lockstep — which defeats "installing a plugin
is installing another `.deb`". A process boundary plus a declared ABI number
costs one `fork`/`exec` per invocation (nothing at CLI latency) and buys an
interface that survives a core rebuild.

The `abi` field replaces the versioned `Depends:` the shell packaging needed.
That mechanism failed in a specific way: `mc-rcon`'s postinst sourced another
package's *private shell library*, so a missed version-floor bump left dpkg
configuring the plugin against a library without the function it called, dying
with exit 127 and a half-installed package. Now core reads the number and
refuses by name.

## The manifest

```toml
# /usr/lib/mc/plugins.d/<name>.toml
abi  = 1                              # must equal mc_common::plugin::ABI
name = "rcon"
bin  = "/usr/libexec/mc/mc-rcon"

[[commands]]
name  = "rcon"                        # `mc rcon`
about = "Open an RCON console"        # shown by `mc plugins`

[[hooks]]
event = "pre-stop"
# fatal = true                        # opt-in; refused for pre-stop/post-backup

[[providers]]
kind       = "source"                 # only kind today
name       = "mrpack"
extensions = ["mrpack"]               # `mc install pack.mrpack` routes here
```

Discovery is **sorted by filename**, so two plugins on the same event fire in
the same order on every machine. Rely on that only if you must; prefer hooks
that do not care.

A manifest is refused — with the plugin named, and without disturbing any other
— when: the `abi` is not core's, an event name is unknown, `bin` is not an
executable file, a provider `kind` is unknown, or a non-fatal-only event is
marked `fatal`. `mc plugins` reports every refusal.

## Invocation contract

| What | Core runs | Notes |
|---|---|---|
| Subcommand | `<bin> command <name> [args…]` | `exec`, so the plugin owns the terminal |
| Hook | `<bin> hook <event>` | JSON payload on **stdin** |
| Source provider | `<bin> provide <file> <staging-dir>` | JSON report on **stdout** |

Environment on every invocation: `MC_ABI`, `MC_ROOT`, `MC_BASE`, `MC_CONFIG`,
`MC_USER`. Read paths with `Paths::from_env()` — never hardcode them, or the
plugin ignores `MC_ROOT` and an integration test drives core against a temp root
while the plugin writes to the real `/opt/minecraft`.

The hook payload goes on stdin rather than in argv because argv is
world-readable through `/proc/<pid>/cmdline` — the same reason the RCON password
is passed by file. **Always drain stdin**, even if you ignore it, or core is
left writing into a closed pipe.

## Hook events

| Event | When | May be fatal |
|---|---|---|
| `pre-start` | before the unit starts | yes |
| `pre-stop` | before the server is told to stop | **no** |
| `pre-backup` | before the world is archived | yes |
| `post-backup` | after an archive, success or not | **no** |
| `post-install` | after a server is installed | yes |
| `post-upgrade` | after a server is upgraded | yes |

`pre-stop` and `post-backup` **can never be fatal**, and the manifest loader
refuses to let a plugin claim otherwise:

- A shutdown must not abort because a warning could not be delivered.
  `TimeoutStopSec` bounds the whole stop, and overrunning it means the JVM is
  SIGKILLed mid-chunk-flush — the world corruption the countdown exists to
  prevent.
- `post-backup` restores saving. A live server left with saves disabled loses
  everything since the last flush the moment it stops — worse than the failed
  backup that got you there.

Every registered plugin runs even when an earlier one failed: hooks are
independent contributions, not a pipeline.

## What must stay in core

A plugin contributes a **step**. Core keeps the ordering that makes the step
safe. Never move into a plugin:

- **The lock** (`mc_common::lock`). Core takes it before install/upgrade;
  `mc-backup` takes it for its own commands. It is re-entrant within a process.
- **The EULA gate.** Consent comes from `--accept-eula` or an interactive yes,
  and from nowhere else — in particular not from `--yes`, which consents to
  installing a package, not to a licence.
- **Ownership of `MC_BASE`.** Core chowns the tree after any write.
- **The managed keys of `server.properties`** (`server-port`, `enable-rcon`,
  `rcon.port`, `rcon.password`). Core re-applies them after any merge, which is
  what stops a modpack enabling RCON with a password of its choosing.

## Reuse, do not reimplement

Every crate links `mc-common` statically — you get the real implementations with
no runtime coupling to core's build.

| Need | Use | Never |
|---|---|---|
| Paths | `Paths::from_env()` | string literals |
| Read/write `server.properties` | `properties::{Properties, secure, merge}` | your own parser |
| RCON port | `properties::rcon_port` | game port + 10 |
| Untrusted archive path | `staging::safe_relative_path`, `staging::resolve_under` | `Path::join` |
| A temp tree | `staging::Staging` (RAII) | `mktemp` + cleanup |
| Download URL from untrusted input | `http::host_allowed` | substring search |
| Verify an artifact | `hash::verify_file` | skipping when no hash is published |
| Broadcast to players | `chat::say` (builds a `tellraw`) | `say` — renders as `[Rcon] …` |
| Privilege | `privilege::{require_root, require_root_or_group}` | `getuid() == 0` |
| Version from untrusted input | `version::validate` | interpolating it into a URL |
| Output | `ui::{info, warn, error}` | `println!` (stdout is for data) |

**Core does not guard plugin subcommands.** Only the plugin knows whether its
subcommand reads or writes, so it applies the guard itself — read-only verbs
take `require_root_or_group`, anything that writes takes `require_root`.

## Adding a plugin: the checklist

1. **`mc/crates/mc-<name>/`** — a `[[bin]]` named `mc-<name>`, `mc-common` as a
   dependency, `[lints] workspace = true`.
2. **`mc/packages/mc-<name>/`** — mirrors the target root:
   `DEBIAN/control` (`Depends: mc-server, ${shlibs:Depends}`),
   `usr/lib/mc/plugins.d/<name>.toml`, any units.
3. **`mc/scripts/build.sh`** — add the package to the `case`, mapping each
   `[[bin]]` name to its install path. A rename on one side and not the other
   produces a `.deb` with a missing executable and a build that still reports
   success, so the copy fails loudly instead.
4. **`mc/tests/run.sh`** — add the package to the build loop and bump the expected
   `.deb` count.
5. **`mc/Cargo.toml`** — add the crate to `members`.
6. **`mc/packages/mc-<name>/usr/share/man/man1/mc-<name>.1`** — hand-written
   roff. **The name is not a convention you may vary:** `mc man <command>`
   resolves a plugin command through the registry to `mc-<plugin name>(1)`, and
   `every_plugin_command_has_a_manual_page_in_its_own_package` (tier 1) fails
   on any other name, on a page shipped by a different package, or on one that
   does not mention every command the manifest registers. A second command gets
   a `.so man1/mc-<name>.1` stub file, which `build.sh` turns into a symlink
   when it compresses — see `mc-restore.1`.
   Model it on `mc-backup.1`: SYNOPSIS, COMMANDS, the security properties that
   matter, FILES, SEE ALSO. `.TH` takes an empty version field so a release
   never has to touch it.
7. **Tests.** Tier 1 for the logic; a case in
   `mc/tests/suites/integration/plugins.sh` for install → command appears →
   `apt remove` → command withdrawn, core still working; and a row in the
   `Manual pages` section of `integration/packaging.sh` asserting the `.deb`
   ships the page, gzipped.
8. **Bump `Version:`** in `DEBIAN/control` in the same commit. CI publishes on
   every push touching these paths and reprepro regenerates per run, so a change
   without a bump is served under the old version and never reaches an installed
   system.

## The smallest useful plugin

```rust
// mc/crates/mc-hello/src/main.rs
use mc_common::{Paths, error::{Error, Result}, ui};
use std::io::Read as _;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = Paths::from_env();

    let result = match (args.first().map(String::as_str), args.get(1).map(String::as_str)) {
        (Some("command"), Some("hello")) => {
            mc_common::privilege::require_root_or_group(&paths.mc_bin(), &args)
                .and_then(|()| { ui::info("hello"); Ok(()) })
        }
        (Some("hook"), Some(event)) => {
            // Drained even though it is unused: core writes the payload here,
            // and not reading it leaves core writing into a closed pipe.
            let mut payload = String::new();
            let _ = std::io::stdin().read_to_string(&mut payload);
            ui::info(format!("hook {event} fired"));
            Ok(())
        }
        _ => Err(Error::config("mc-hello is a plugin for mc. Use: mc hello")),
    };

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            ui::error(e.to_string());
            std::process::ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(1))
        }
    }
}
```

```toml
# mc/packages/mc-hello/usr/lib/mc/plugins.d/hello.toml
abi  = 1
name = "hello"
bin  = "/usr/libexec/mc/mc-hello"

[[commands]]
name  = "hello"
about = "Say hello"

[[hooks]]
event = "post-install"
```

## Changing the ABI

Bump `mc_common::plugin::ABI` **only** for a breaking change to the manifest
schema, a hook payload, or the provider protocol. Every installed plugin
declaring the old number stops loading the moment it changes, so a bump is a
coordinated release of every package in the tree. Adding a new *optional*
manifest field or a new *event* is not breaking and must not bump it.
