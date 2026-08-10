---
name: file-structure
description: Map of and exploration guide for the apt.bulmer.dev repo — where every file lives, which file defines which shell function, targeted recipes for finding and reading code without loading whole files, and where a given kind of change belongs. Use BEFORE ls/find/grep when locating code, deciding where a change belongs, or orienting in the mc-server / mc-rcon Debian packages. For running or writing tests, see the `testing` skill.
---

# apt.bulmer.dev structure & exploration

A Debian apt repository shipping two packages that manage a Minecraft server.
Read this instead of running `ls -R` or broad greps.

## Layout

Each `packages/<name>/` directory **mirrors the target filesystem root**. A file
at `packages/mc-server/usr/bin/mc` installs to `/usr/bin/mc`. `DEBIAN/` is the
control metadata and is the one directory that is not a real install path.

```
packages/mc-server/          the CLI, systemd units, config     (Architecture: all)
packages/mc-rcon/            the RCON client                    (compiled C, per-arch)
scripts/build.sh             builds one .deb; compiles src/ first if present
scripts/publish.sh           signs a .deb and adds it to reprepro
tests/run.sh                 regression suite (Docker); see the `testing` skill
repo/conf/distributions      reprepro config; repo/bulmer.asc is the public key
.github/workflows/publish.yml  builds per-arch, publishes, ships the nginx image
k8s/                         deployment of the repo web server
Dockerfile, nginx.conf       the container that serves the apt repo
```

| File | Lines | Owns |
|---|---|---|
| `mc-server/usr/lib/mc/lib.sh` | ~1800 | Every `cmd_*` implementation. |
| `mc-server/usr/lib/mc/common.sh` | ~300 | Definitions shared with the systemd-facing scripts. Side-effect free. |
| `mc-server/usr/lib/mc/start.sh` | ~211 | `ExecStart`. GC presets, EULA + properties gates, JVM launch. |
| `mc-server/usr/lib/mc/stop.sh` | ~210 | `ExecStop`. Player count, countdown tiers, graceful stop. |
| `mc-server/usr/lib/mc/reload.sh` | ~24 | `ExecReload`. RCON `reload`. |
| `mc-server/usr/bin/mc` | ~42 | Dispatcher only. Routes to `cmd_*`; sources `commands.d/*.sh`. |
| `mc-server/lib/systemd/system/minecraft.service` | ~100 | Hardening, exit-code policy, timeouts. |
| `mc-server/etc/minecraft/defaults.conf` | ~32 | Shipped defaults. A conffile. |
| `mc-server/etc/bash_completion.d/mc` | ~68 | Completion. **Add new subcommands here too.** |
| `mc-server/DEBIAN/postinst` | ~92 | Creates the user, sets ownership/modes, reloads systemd. |
| `mc-rcon/src/rcon.c` | ~754 | Compiles to `/usr/bin/rcon`. |
| `mc-rcon/usr/lib/mc/commands.d/rcon.sh` | ~115 | Registers `rcon` (the plugin pattern) and its enable/disable/status verbs. |

## Function index

Answers "where is X defined" with no tool call.

**`common.sh`** — sourced by `lib.sh` *and* by the three systemd scripts, which
run unprivileged under `ProtectSystem=strict`. Definitions only: no writes, no
network, no `systemctl`.

> `mc_sprop_get` `load_config` `mc_required_java` `java_major_version`
> `find_java_binary` `eula_accepted` `mc_rcon_port` `mc_rcon_available`
> `mc_rcon_call` `generate_rcon_password` `mc_say_command`

Broadcasts to players go through `mc_say_command` (a `tellraw`), never `say` —
`say` renders as "[Rcon] …" when it arrives over RCON.

**Two kinds of config.** `/etc/minecraft/` (`defaults.conf` → `server.conf`,
via `load_config`) is *mc's*: how to run the server — build, Java, heap, backup
policy. `/opt/minecraft/server.properties` is *the server's*: port, seed, MOTD,
difficulty, RCON. Nothing from the second is mirrored into the first, because
the JVM rewrites it. Read it with `mc_sprop_get` / `mc_rcon_port` at the point
of use; `set_rcon_enabled` in `lib.sh` is the only writer of `enable-rcon` /
`rcon.port` / `rcon.password`. `MC_STOCK_PORT` applies only before that file
exists.

**`lib.sh`** — root-only; reached through `usr/bin/mc`.

> *output* `info` `warn` `error` `die`
> *guards* `require_root` `server_installed` `require_server`
> *plugins* `mc_register_command` `mc_is_plugin_command`
> *config* `write_config`
> *cleanup/lock* `mc_cleanup` `mc_cleanup_arm` `cleanup_register_dir` `cleanup_unregister_dir` `acquire_lock`
> *java/eula* `ensure_java` `accept_eula`
> *systemd/rcon* `is_running` `rcon_command` `ensure_rcon_password` `set_rcon_enabled` (availability and password generation are in `common.sh`)
> *properties* `sprop_secure` `sprop_set` `managed_property_value` `merge_server_properties` `init_server_properties` (reads go through `mc_sprop_get`)
> *download* `validate_version` `verify_sha` `download_paper` `download_vanilla` `download_fabric` `install_neoforge` `resolve_version` `version_identifies_artifact` `download_jar` `install_server_artifact` (the staging→MC_BASE step shared by install/upgrade) `initialize_server_settings` (runs the jar with `--initSettings` so server.properties exists, fully populated, before any world does)
> *mrpack* `mrpack_url_allowed` `make_staging_dir` `mrpack_safe_path` `mrpack_extract_overrides` `cmd_install_mrpack`
> *start helpers* `start_and_verify` `start_failed` `settled_running` `report_unit_failure`
> *commands* `cmd_install` `cmd_upgrade` `cmd_start` `cmd_stop` `cmd_restart` `cmd_status` `cmd_backup` `cmd_restore` `cmd_logs` `cmd_delete` `usage`

**Globals.** Paths (`MC_BASE` `MC_BACKUP` `MC_CONFIG` `DEFAULTS_CONF`
`SERVER_CONF` `PASSWD_FILE` `MRPACK_MANIFEST` `LOCK_FILE` `MC_USER`) are defined
once in `common.sh`, alongside the `MC_STOCK_PORT` constant. `load_config` sets
`MINECRAFT_VERSION` `JAVA_VERSION` `SERVER_RAM` `SERVER_FLAGS` `JAVA_OPTS`
`BACKUP_KEEP` `BACKUP_SCHEDULE` `SERVER_TYPE` — no ports. Never hardcode paths.

## Exploration recipes

**Read one function, not the file:**
```sh
sed -n '/^cmd_backup()/,/^}/p' packages/mc-server/usr/lib/mc/lib.sh
```

**List sections with current line numbers** (they drift — always re-derive
rather than trusting a remembered number):
```sh
grep -n "^# ── " packages/mc-server/usr/lib/mc/lib.sh
```
Order: output · plugins · config · cleanup · lock · java · eula · systemd · rcon
· **server.properties (~336)** · download · Modrinth allowlist · staging ·
mrpack · `cmd_install` (~1036) · `cmd_upgrade` · `cmd_start` · `cmd_stop` ·
`cmd_restart` · `cmd_status` · `cmd_backup` · `cmd_restore` · `cmd_logs` ·
`cmd_delete` · `usage` (~1571). Then `Read` with `offset`/`limit`.

**Trace a setting end to end** — default → config → consumer:
```sh
grep -rn "BACKUP_SCHEDULE" packages/mc-server/
```

**Find every caller of a helper before changing it:**
```sh
grep -rn "sprop_set\|init_server_properties" packages/
```

**Check both packages** when touching the `mc` dispatcher or `commands.d`
contract — `mc-rcon` registers into it and breaks silently otherwise.

### Anti-patterns

- **Don't `ls -R`** — the layout is above and does not change often.
- **Don't read `lib.sh` whole.** ~1800 lines. Use the section index.
- **Don't search generated trees.** `dist/`, `staging/`, `repo/db|dists|pool/`,
  and `packages/*/src/rcon` are gitignored build output.
- **Don't trust remembered line numbers** across edits; re-grep the marker.
- **Don't infer intent from code alone.** Comments here are dense and state
  the constraint the code satisfies — sed injection in `sprop_set`,
  arithmetic-context injection in `mc_required_java`, ownership in
  `sprop_secure`. Read the comment before changing the code it guards.
- **Don't write changelog comments.** A comment describes the code that
  follows, not what it replaced. No "used to", no migration notes.

## Runtime paths

| Var | Path | Notes |
|---|---|---|
| `MC_BASE` | `/opt/minecraft` | `minecraft:minecraft` 0750. The only `ReadWritePaths=` entry. |
| `MC_CONFIG` | `/etc/minecraft` | `server.conf` (generated), `defaults.conf` (conffile), `server.passwd`. |
| `MC_BACKUP` | `/var/backups/minecraft` | **root:root 0700 on purpose** — never owned by `MC_USER`. |
| `LOCK_FILE` | `/run/minecraft/mc.lock` | Serialises install/upgrade/backup/restore. |

`server.properties` holds the RCON password: always `minecraft:minecraft` 0640
via `sprop_secure()`. Owner matters as much as mode — root-owned means the JVM
silently falls back to compiled-in defaults.

## Where a change belongs

- **New `mc` subcommand** → `cmd_*` in `lib.sh` + case in `usr/bin/mc` + `bash_completion.d/mc` + `usage`
- **Launch/JVM/GC behaviour** → `start.sh`
- **Shutdown behaviour** → `stop.sh` (recompute `TimeoutStopSec` in the unit if timings change)
- **Anything the systemd scripts also need** → `common.sh`, not `lib.sh`
- **Install-time ownership or repair of existing installs** → `DEBIAN/postinst`
- **New setting for how mc runs the server** → `defaults.conf` + `load_config()`
- **New setting for what the game does** → `server.properties`; do not add a
  mirror in `server.conf`

## Verifying changes

There is a regression suite: **`tests/run.sh`** (`--all` adds the install-type
matrix). It builds both `.deb`s, installs them in a Debian 13 container, and
runs unit + integration suites.

**Read `.claude/skills/testing` before writing a test, running one, or calling a
change verified.** It covers the fixtures for testing `lib.sh` without
installing it, why results from a different bash cannot be trusted, what still
cannot be verified, and a catalogue of bugs this project has hit repeatedly —
several of which are ways a test can pass *vacuously*.

Cheap checks that need no container:

```sh
for f in packages/mc-server/usr/lib/mc/*.sh packages/mc-server/usr/bin/mc \
         packages/mc-server/DEBIAN/postinst; do bash -n "$f" && echo "OK $f"; done
```

## Shipping

Version lives only in `DEBIAN/control`; there is no `DEBIAN/changelog`. CI
publishes on every push to `main` touching `packages/**`, and reprepro trees are
regenerated per run — so **any change without a version bump is published under
the old version and never reaches installed systems.** Bump `Version:` with the
change, not after.

**Bump in the same commit as the change**, not in a follow-up. A run of commits
that each change `packages/**` but leave `Version:` alone republishes one
version number as several different artifacts: whoever installed the earlier one
is pinned to it forever, because apt only upgrades on a higher version. Check
what the mirror is really serving before assuming a fix landed:

```sh
curl -s https://apt.bulmer.dev/dists/stable/main/binary-amd64/Packages | grep -A1 '^Package: mc-'
curl -sO https://apt.bulmer.dev/pool/main/m/mc-server/mc-server_<ver>_all.deb
dpkg-deb -x mc-server_<ver>_all.deb /tmp/x && grep -r <a-function-you-added> /tmp/x/usr/lib/mc/
```

**`mc-rcon` calls shell functions out of `mc-server`'s `common.sh`**, so its
`Depends:` carries a version floor (`mc-server (>= X.Y.Z)`). Raise it whenever
the postinst or `commands.d/rcon.sh` starts using a function that older
mc-server versions lack — otherwise dpkg configures the plugin against a
library without it and the maintainer script dies with `command not found`
(exit 127), leaving the package half-installed. `DEBIAN/control` takes no comments; the
reasoning lives in `mc-rcon/DEBIAN/postinst`.
