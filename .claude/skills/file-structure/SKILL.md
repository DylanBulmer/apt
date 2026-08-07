---
name: file-structure
description: Map of and exploration guide for the apt.bulmer.dev repo — where every file lives, which file defines which shell function, targeted recipes for finding and reading code without loading whole files, and how to verify shell/packaging changes on a machine that cannot build a .deb. Use BEFORE ls/find/grep when locating code, deciding where a change belongs, or orienting in the mc-server / mc-rcon Debian packages.
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
repo/conf/distributions      reprepro config; repo/bulmer.asc is the public key
.github/workflows/publish.yml  builds per-arch, publishes, ships the nginx image
k8s/                         deployment of the repo web server
Dockerfile, nginx.conf       the container that serves the apt repo
```

| File | Lines | Owns |
|---|---|---|
| `mc-server/usr/lib/mc/lib.sh` | ~1600 | Every `cmd_*` implementation. |
| `mc-server/usr/lib/mc/common.sh` | ~208 | Definitions shared with the systemd-facing scripts. Side-effect free. |
| `mc-server/usr/lib/mc/start.sh` | ~211 | `ExecStart`. GC presets, EULA + properties gates, JVM launch. |
| `mc-server/usr/lib/mc/stop.sh` | ~169 | `ExecStop`. Player count, countdown tiers, graceful stop. |
| `mc-server/usr/lib/mc/reload.sh` | ~24 | `ExecReload`. RCON `reload`. |
| `mc-server/usr/bin/mc` | ~42 | Dispatcher only. Routes to `cmd_*`; sources `commands.d/*.sh`. |
| `mc-server/lib/systemd/system/minecraft.service` | ~100 | Hardening, exit-code policy, timeouts. |
| `mc-server/etc/minecraft/defaults.conf` | ~32 | Shipped defaults. A conffile. |
| `mc-server/etc/bash_completion.d/mc` | ~63 | Completion. **Add new subcommands here too.** |
| `mc-server/DEBIAN/postinst` | ~81 | Creates the user, sets ownership/modes, repairs prior installs. |
| `mc-rcon/src/rcon.c` | ~754 | Compiles to `/usr/bin/rcon`. |
| `mc-rcon/usr/lib/mc/commands.d/rcon.sh` | ~34 | Registers the `rcon` subcommand — the plugin pattern. |

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

`server.properties` is the source of truth for the keys the JVM owns
(`server-port`, `rcon.port`, `enable-rcon`, `rcon.password`). `mc_sprop_get` is
the only parser for them — read through it, never re-grep the file — and
`load_config`/`mc_rcon_port` resolve *through* it, so `SERVER_PORT` in
`server.conf` is only the seed for a server that has no properties file yet.

**`lib.sh`** — root-only; reached through `usr/bin/mc`.

> *output* `info` `warn` `error` `die`
> *guards* `require_root` `server_installed` `require_server`
> *plugins* `mc_register_command` `mc_is_plugin_command`
> *config* `write_config`
> *cleanup/lock* `mc_cleanup` `mc_cleanup_arm` `cleanup_register_dir` `cleanup_unregister_dir` `acquire_lock`
> *java/eula* `ensure_java` `accept_eula`
> *systemd/rcon* `is_running` `rcon_command` (availability and password generation are in `common.sh`)
> *properties* `sprop_secure` `sprop_set` `managed_property_value` `merge_server_properties` `init_server_properties` (reads go through `mc_sprop_get`)
> *download* `validate_version` `verify_sha` `download_paper` `download_vanilla` `download_fabric` `install_neoforge` `resolve_version` `version_identifies_artifact` `download_jar` `install_server_artifact` (the staging→MC_BASE step shared by install/upgrade)
> *mrpack* `mrpack_url_allowed` `make_staging_dir` `mrpack_safe_path` `mrpack_extract_overrides` `cmd_install_mrpack`
> *start helpers* `start_and_verify` `start_failed` `settled_running` `report_unit_failure`
> *commands* `cmd_install` `cmd_upgrade` `cmd_start` `cmd_stop` `cmd_restart` `cmd_status` `cmd_backup` `cmd_restore` `cmd_logs` `cmd_delete` `usage`

**Globals.** Paths (`MC_BASE` `MC_BACKUP` `MC_CONFIG` `DEFAULTS_CONF`
`SERVER_CONF` `PASSWD_FILE` `MRPACK_MANIFEST` `LOCK_FILE` `MC_USER`) are defined
once in `common.sh`. `load_config` sets `MINECRAFT_VERSION` `JAVA_VERSION`
`SERVER_RAM` `SERVER_FLAGS` `JAVA_OPTS` `SERVER_PORT` `BACKUP_KEEP`
`BACKUP_SCHEDULE` `SERVER_TYPE`. Never hardcode these paths.

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

**Trace a setting end to end** — default → config → properties → consumer:
```sh
grep -rn "SERVER_PORT" packages/mc-server/
```

**Find every caller of a helper before changing it:**
```sh
grep -rn "sprop_set\|init_server_properties" packages/
```

**Check both packages** when touching the `mc` dispatcher or `commands.d`
contract — `mc-rcon` registers into it and breaks silently otherwise.

### Anti-patterns

- **Don't `ls -R`** — the layout is above and does not change often.
- **Don't read `lib.sh` whole.** ~1600 lines. Use the section index.
- **Don't search generated trees.** `dist/`, `staging/`, `repo/db|dists|pool/`,
  and `packages/*/src/rcon` are gitignored build output.
- **Don't trust remembered line numbers** across edits; re-grep the marker.
- **Don't infer intent from code alone.** Comments here are dense and explain
  *why*, usually recording a specific past bug (sed injection in `sprop_set`,
  arithmetic-context injection in `mc_required_java`, ownership in
  `sprop_secure`). Read the comment before changing the code it guards.

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
- **New default setting** → `defaults.conf` + `load_config()` in `common.sh`

## Verifying changes

**Test in Docker, not on the host.** The `mc-tests:debian13` image (Debian 13,
bash 5.2, `rsync`/`curl`/`jq`/`unzip`/`gcc`/`dpkg-deb`, plus a `systemctl` stub
on PATH) runs the real thing end to end — build the `.deb`, install it, drive
the maintainer scripts. It has an ENTRYPOINT, so override it:

```sh
docker run --rm --entrypoint bash -v "$PWD":/work:ro mc-tests:debian13 -c '
  cp -r /work /build && cd /build && rm -rf dist staging
  bash scripts/build.sh mc-server && bash scripts/build.sh mc-rcon
  dpkg -i dist/mc-server_*.deb && dpkg -i dist/mc-rcon_*.deb
  # then: /var/lib/dpkg/info/mc-rcon.postinst configure
'
```

Copy to `/build` first and mount the repo read-only — `build.sh` writes `dist/`
and `staging/` and `chmod`s the tree in place.

**macOS bash is 3.2 and will mislead you.** Two traps specifically:

- `set -e` is *disabled inside* `( ... )` when the subshell is on the left of
  `||` — including `( ... ) || rc=$?`. A test written that way passes
  vacuously because the abort it checks for never fires. This is real bash
  behaviour, not a 3.2 quirk; it bites on 5.2 too. Test an abort path by
  running a **separate `bash` process** and capturing `$?` with `set +e`
  around it.
- `stat -f '%OLp'` (BSD) vs `stat -c '%a'` (GNU) — mode assertions need both.

**Syntax-check everything touched** (fine on the host):
```sh
for f in packages/mc-server/usr/lib/mc/*.sh packages/mc-server/usr/bin/mc \
         packages/mc-server/DEBIAN/postinst; do bash -n "$f" && echo "OK $f"; done
```

**Lint** with `shellcheck -s bash -S warning -e SC1091`. `SC2034` "appears
unused" fires constantly on `common.sh` — those globals are consumed by
`lib.sh`/`start.sh` and the warning is a false positive across file boundaries.
A hit on a *colour code* or a helper nothing sources, though, is real dead code.

**Exercise a helper in a sandbox** by sourcing just its section and overriding
the path globals — most of `lib.sh` is testable unprivileged this way. `chown`
failures are tolerated by design, so only modes are observable as non-root:
```sh
eval "$(sed -n '/^# ── server.properties helpers/,/^# ── Download helpers/p' lib.sh)"
MC_BASE=/tmp/sandbox   # then call init_server_properties, merge_server_properties, ...
```

**Test systemd-facing logic with mocked `systemctl`/`journalctl`** — shell
functions shadow real commands, so state machines like `start_and_verify` can be
driven through failure, success, and race cases without systemd present. (The
container's `systemctl` stub echoes its arguments, which is enough to assert
*whether* a restart was triggered — e.g. that a no-op reconfigure does not
restart a populated server.)

Still unverifiable, even in the container: `systemd-analyze verify` on the unit
(no running systemd, only the stub) and anything needing a real JVM or a real
Minecraft server. Say so rather than implying it was tested.

## Shipping

Version lives only in `DEBIAN/control`; there is no `DEBIAN/changelog`. CI
publishes on every push to `main` touching `packages/**`, and reprepro trees are
regenerated per run — so **any change without a version bump is published under
the old version and never reaches installed systems.** Bump `Version:` with the
change, not after.
