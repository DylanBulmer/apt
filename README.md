# apt.bulmer.dev

![MIT License](https://img.shields.io/github/license/DylanBulmer/apt)

APT packages by Dylan Bulmer, hosted at
[apt.bulmer.dev](https://apt.bulmer.dev).

## Contents

- [Requirements](#requirements)
- [Adding the repository](#adding-the-repository)
  - [Verifying the signing key](#verifying-the-signing-key)
- [Quick start](#quick-start)
- [Packages](#packages)
  - [`mc-server`](#mc-server)
    - [Choosing the world before it exists](#choosing-the-world-before-it-exists)
    - [Repeat runs](#repeat-runs)
    - [Configuration](#configuration)
    - [Backups](#backups)
    - [Modpacks](#modpacks)
  - [`mc-rcon`](#mc-rcon)
- [File locations](#file-locations)
- [Uninstalling](#uninstalling)
- [Troubleshooting](#troubleshooting)
- [Security](#security)
- [Development](#development)
  - [Building packages](#building-packages)
  - [Running the tests](#running-the-tests)
  - [Adding a package](#adding-a-package)
  - [Publishing to the repo](#publishing-to-the-repo)
  - [CI/CD](#cicd)
- [License](#license)

## Requirements

- Debian or Ubuntu with `systemd`
- `amd64` or `arm64` — the repository publishes no other architectures
- Java is **not** a prerequisite. `mc install` picks the right major version
  (8, 17, 21, or 25) for the Minecraft version you asked for, and installs
  `openjdk-<N>-jre-headless` for you when run with `--yes`

## Adding the repository

```bash
sudo apt install -y curl gpg
sudo mkdir -p /etc/apt/keyrings

curl -fsSL https://apt.bulmer.dev/bulmer.asc \
  | sudo gpg --dearmor -o /etc/apt/keyrings/bulmer.gpg

echo "deb [signed-by=/etc/apt/keyrings/bulmer.gpg] https://apt.bulmer.dev stable main" \
  | sudo tee /etc/apt/sources.list.d/bulmer.list

sudo apt update
```

### Verifying the signing key

Before trusting the repository, confirm the key you downloaded is the one below:

```bash
gpg --show-keys --with-fingerprint /etc/apt/keyrings/bulmer.gpg
```

```
pub   rsa4096 2026-04-26 [SC]
      14A3 D54F EE9C 6286 09B6  F6C7 75C5 0E95 16D5 6229
uid                      apt.bulmer.dev <apt@bulmer.dev>
```

If the fingerprint does not match, stop and remove the keyring file.

## Quick start

```bash
sudo apt install mc-server mc-rcon   # mc-rcon is optional
sudo mc install --type paper         # prompts for the EULA, then downloads
sudoedit /opt/minecraft/server.properties   # optional: level-seed, motd, difficulty
sudo mc start
mc status
```

Then open port `25565` to your players and **not** port `25575` — see
[`mc-rcon`](#mc-rcon) below.

> [!IMPORTANT]
> `mc install` will not set up a server until you accept the
> [Minecraft EULA](https://www.minecraft.net/eula). It asks before downloading
> anything; pass `--accept-eula` to accept it up front. Acceptance is recorded
> in `/opt/minecraft/eula.txt`, and the server refuses to start without it.

## Packages

### `mc-server`

Manages a Minecraft server instance on bare-metal or inside VMs/LXC containers.
Supports Paper, Vanilla, Fabric, and NeoForge server types with systemd-based
lifecycle management, automated backups, and multi-version Java support.

```bash
sudo apt install mc-server
```

**Commands**

```
mc install [--type TYPE] [--version VER]   Install the server jar
mc install <pack.mrpack>                   Install from a Modrinth modpack
mc upgrade [--version VER]                 Upgrade to a newer version
mc upgrade <new.mrpack>                    Upgrade from a new Modrinth modpack
mc start [--accept-eula]                   Start the server
mc stop                                    Stop the server gracefully
mc restart [--accept-eula]                 Restart the server
mc status                                  Show systemd service status
mc backup                                  Create a timestamped backup
mc restore <file>                          Restore from a backup archive
mc logs                                    Follow the server log
mc delete                                  Permanently remove the server
```

Server types: `vanilla` (default), `paper`, `fabric`, `neoforge`.

`mc` takes two consent flags, both of which it otherwise asks about
interactively:

| Flag | Consents to | Accepted by |
| --- | --- | --- |
| `--accept-eula` | The [Minecraft EULA](https://www.minecraft.net/eula) | `install`, `upgrade`, `start`, `restart` |
| `--yes` / `-y` | Installing a missing `openjdk-<N>-jre-headless` | `install`, `upgrade` |

They are deliberately separate — asking for a JRE is not the same as accepting a
licence. An unattended install (cloud-init, Ansible, Docker) needs both, since
without them `mc` has no terminal to ask at and fails rather than assuming:

```bash
sudo mc install --type paper --accept-eula --yes
```

Everything except `status` and `logs` requires root. Tab completion for
subcommands and flags is installed automatically; install the `bash-completion`
package if it is not already present.

The server runs as the `minecraft` system user under `systemd`. RCON is
**disabled by default**; install `mc-rcon` to enable it automatically.

#### Choosing the world before it exists

`mc install` leaves you a complete `/opt/minecraft/server.properties` — every
setting at its default — **without generating the world**. Nothing is created
until you start the server, so this is the window in which to pick a seed:

```bash
sudo mc install --type paper --accept-eula --yes
sudoedit /opt/minecraft/server.properties     # level-seed=..., motd=..., difficulty=...
sudo mc start                                 # the world is generated from it, now
```

`level-seed` only matters here. It is consumed when the world is first created
and has no effect afterwards, so a seed set later is silently ignored — the only
way to change it on an existing server is to delete the world.

Settings that are *not* world-creation-time (`motd`, `difficulty`, `max-players`,
…) can be changed whenever you like; restart the server to apply them.

`mc` keeps ownership of four keys in that file — `server-port`, `rcon.port`,
`enable-rcon` and `rcon.password` — so a modpack cannot enable RCON with a
password of its choosing. Your `server-port` and `rcon.port` are honoured and
survive upgrades; `enable-rcon` and `rcon.password` are managed by `mc-rcon`,
which keeps them in step with `/etc/minecraft/server.passwd`.

#### Repeat runs

Commands are safe to run when their work is already done — useful when `mc` is
driven from Ansible, cloud-init, or a cron job rather than by hand.

| Command | When the state already holds |
| --- | --- |
| `mc start` / `mc stop` | Says so and exits `0` |
| `mc upgrade` | Skips the backup, the downtime, and the download |
| `mc delete` | Says so and exits `0` without prompting |
| `mc install` | **Refuses** — see below |
| `mc install <pack.mrpack>` | Reuses unchanged mods instead of refetching them |

`mc install` is the exception: run against a server that already exists it
refuses outright, because it would overwrite `server.jar` and repin the version
with none of the protections `upgrade` has. Use `mc upgrade` to change version,
or `mc install --force` to deliberately reinstall over the top.

`--force` on `upgrade` reinstalls the version you are already on.

> [!NOTE]
> The `mc upgrade` skip applies to `vanilla` and `neoforge` only. Paper
> publishes new *builds* and Fabric new *loader* versions against an unchanged
> Minecraft version, and `server.conf` records only the Minecraft version — so
> for those two, "same version" does not mean "same jar" and the upgrade always
> proceeds.

#### Configuration

There are two kinds of configuration, in two places, and they do not overlap:

| | Configures | Where |
| --- | --- | --- |
| **`mc`** | How the server is installed and run — which build, which Java, how much heap, when to back up | `/etc/minecraft/` |
| **The server** | What the game does — port, seed, MOTD, difficulty, game rules, RCON | `/opt/minecraft/server.properties` |

This section covers the first. For the second, edit `server.properties` and
restart; see [Choosing the world before it
exists](#choosing-the-world-before-it-exists).

`mc`'s own configuration is two files, layered. `/etc/minecraft/defaults.conf`
ships with the package and holds the site-wide defaults — it is a `conffile`, so
your edits survive upgrades. `/etc/minecraft/server.conf` is written by
`mc install` / `mc upgrade` with the settings that install actually resolved, and
it **overrides** `defaults.conf`.

Edit `defaults.conf` to change what a *future* install picks up; edit
`server.conf` to change the server you already have.

| Setting | Default | Applies |
| --- | --- | --- |
| `DEFAULT_SERVER_TYPE` | `vanilla` | At install (`--type` overrides) |
| `MINECRAFT_VERSION` | `latest` | At install; pinned to a concrete version in `server.conf` |
| `JAVA_VERSION` | *(auto)* | On restart — empty means "pick from the Minecraft version" |
| `SERVER_RAM` | `4G` | On restart — sets both `-Xmx` and `-Xms` |
| `SERVER_FLAGS` | *(auto)* | On restart — Generational ZGC on Java 21+, Aikar's G1GC on 17 |
| `JAVA_OPTS` | *(empty)* | On restart — extra JVM options |
| `BACKUP_KEEP` | `7` | Next backup |
| `BACKUP_SCHEDULE` | `daily` | Next `mc install`/`mc upgrade` — see [Backups](#backups) |

A typical tune-and-restart:

```bash
sudoedit /etc/minecraft/server.conf   # SERVER_RAM="8G"
sudo mc restart
```

> [!NOTE]
> **Ports belong to the server, so they live in `server.properties`.**
>
> ```bash
> sudoedit /opt/minecraft/server.properties   # server-port=..., rcon.port=...
> sudo mc restart
> ```
>
> `mc` reads that file whenever it needs one of those values rather than keeping
> a copy of its own. For RCON it dials `rcon.port` when the server sets one, and
> the game port `+10` otherwise — so a port you choose is honoured and survives
> upgrades. `mc rcon status` reports what it resolved.

#### Backups

`mc-server` installs and enables `minecraft-backup.timer`, which runs
`mc backup` on a schedule. Backups are gzipped tarballs in
`/var/backups/minecraft/`, owned by root and mode `600`. If the server is
running and RCON is available, `mc` issues `save-off` / `save-all` / `save-on`
around the archive so the world is not captured mid-write — another reason to
install `mc-rcon`.

```bash
mc backup                                          # on demand
systemctl list-timers minecraft-backup.timer       # when does it next run
mc restore /var/backups/minecraft/minecraft-20260807-020000.tar.gz
```

`mc restore` stops the server, validates every member of the archive before
unpacking it as root, and restores in place. `BACKUP_KEEP` archives are
retained; older ones are pruned after each successful backup.

Changing the schedule needs one extra step, because the timer drop-in is only
regenerated by `mc install` / `mc upgrade`. To change it now, edit the drop-in
directly:

```bash
sudoedit /etc/systemd/system/minecraft-backup.timer.d/schedule.conf
sudo systemctl daemon-reload
```

Keep `BACKUP_SCHEDULE` in `server.conf` in sync, or the next upgrade will
overwrite the drop-in with the old value. The syntax is systemd's `OnCalendar=`
— `daily`, `weekly`, `Mon *-*-* 02:00:00`.

#### Modpacks

`mc install` and `mc upgrade` accept a Modrinth `.mrpack` file, which pins the
loader, the Minecraft version, and every mod in one shot. Requires `unzip`.

```bash
sudo apt install unzip
sudo mc install ~/Downloads/cobblemon-1.6.1.mrpack --accept-eula --yes
sudo mc upgrade ~/Downloads/cobblemon-1.7.0.mrpack
```

Only `formatVersion: 1` packs are supported, and mod downloads are restricted to
an allowlist of Modrinth CDN hosts — a pack that points somewhere else is
rejected rather than fetched.

A server installed from a `.mrpack` can only be upgraded with another
`.mrpack`; plain `mc upgrade --version` is refused, since the mods would not
match the new Minecraft version. `mc upgrade` always takes a backup first and
aborts if that backup fails.

---

### `mc-rcon`

Adds the `mc rcon` subcommand, providing interactive RCON console access and
single-command execution against a running server.

```bash
sudo apt install mc-rcon
```

```
mc rcon                    Open an interactive RCON session
mc rcon <command>          Run a single command and print the response
mc rcon status             Show whether RCON is on, on which port, and whether it answers
sudo mc rcon enable        Turn RCON on in server.properties
sudo mc rcon disable       Turn RCON off
```

The first three are open to root **and to members of the `minecraft` group** —
they only read files that group can already read: the port in
`server.properties` (`0640 minecraft:minecraft`) and the password in
`/etc/minecraft/server.passwd` (`0640 root:minecraft`). To give an operator
in-game admin without handing them the host:

```bash
sudo usermod -aG minecraft alice     # takes effect at alice's next login
```

`enable` and `disable` stay root-only, because they *write* `server.properties`
— which the group can read but not write — and may create the password file in
root-owned `/etc/minecraft`.

Note what that group grants: full operator control of the game server, `/stop`
included. It is not a way to hand out a limited console.

`enable`, `disable` and `status` act on `server.properties` rather than talking
to the server, so they work while it is stopped — and while RCON is precisely
what is currently off. The server reads that file at startup, so `mc restart` is
needed to apply a change; `mc` tells you when that is the case rather than
restarting a populated server on your behalf.

Installing `mc-rcon` automatically enables RCON on the managed server and
generates a random password stored in `/etc/minecraft/server.passwd` (readable
only by root and the `minecraft` user). `mc install` provisions the same
password if the plugin is present, so either install order works. Removing the
package disables RCON again and restarts the server, leaving the password file
in place so a reinstall restores the same secret.

With RCON available, `mc stop` also becomes graceful: it warns players in-game
and counts down before shutting the JVM down.

> [!WARNING]
> RCON is an **unencrypted protocol** — the password and all commands travel in
> plaintext. The `rcon` binary enforces loopback-only connections and will
> refuse any host that does not resolve to `127.0.0.0/8` or `::1`.
>
> That check binds the **client** only. Minecraft itself has no RCON bind
> address setting: the server listens on every interface it can, so enabling
> `mc-rcon` opens port `25575` (game port + 10) to the network. Anyone who
> reaches that port and has the password gets full console access. **Firewall
> the RCON port**, e.g.:
>
> ```bash
> sudo ufw deny 25575/tcp
> ```

## File locations

| Path | Contents |
| --- | --- |
| `/opt/minecraft` | Server jar, world, mods, `server.properties` |
| `/etc/minecraft/defaults.conf` | Package defaults (a `conffile` — safe to edit) |
| `/etc/minecraft/server.conf` | Resolved per-server config, written by `mc install` |
| `/etc/minecraft/server.passwd` | RCON password, root + `minecraft` only |
| `/etc/minecraft/server.mrpack.json` | Manifest of the installed modpack, if any |
| `/var/backups/minecraft` | Backup archives, root-only `0700` |
| `/usr/lib/mc/` | `mc` implementation and plugin commands |
| `/run/minecraft/mc.lock` | Lock held by mutating `mc` commands |
| `/lib/systemd/system/minecraft.service` | The server unit |
| `/lib/systemd/system/minecraft-backup.{service,timer}` | Scheduled backups |

## Uninstalling

```bash
sudo apt remove mc-rcon      # disables RCON, restarts the server
sudo apt remove mc-server    # stops and disables the service and backup timer
```

Removing `mc-server` leaves your world alone: `/opt/minecraft`,
`/var/backups/minecraft`, and `/etc/minecraft/server.conf` all survive, so
reinstalling picks up where you left off. `apt purge` additionally removes
`/etc/minecraft/defaults.conf`.

To destroy the server itself:

```bash
sudo mc delete    # prompts for confirmation; backups are preserved
```

## Troubleshooting

**`mc stop` takes five minutes.** By design. When players are online — or when
the player count cannot be read for any reason — `stop.sh` runs a full
five-minute in-game countdown before shutting down, because a JVM killed
mid-chunk-flush corrupts the world. An empty server stops immediately. The unit
allows `TimeoutStopSec=375s` for the whole procedure.

The shutdown narrates itself to the journal, so you can tell a countdown from a
hung connection — `journalctl -u minecraft -f` shows the player count, each
warning as it is broadcast, and how long the next wait is. If RCON is
unavailable it says that too, and stops without warning anyone.

**"Could not acquire lock /run/minecraft/mc.lock".** Another `mc` command is
mutating the server. `mc` reports the holding PID and command; stale locks are
cleared automatically when the holder is gone.

**"the Minecraft EULA has not been accepted".** The server refuses to launch
unless `/opt/minecraft/eula.txt` contains `eula=true` — including when started
with `systemctl start minecraft` rather than `mc start`. Servers installed by
`mc` have already accepted; this only shows up if the file was removed or set
back to `false`. To accept it on a server that already exists:

```bash
sudo mc start --accept-eula
```

**The service will not start.** `mc status` shows the systemd state, `mc logs`
follows the journal. `journalctl -u minecraft -n 100 --no-pager` gets you the
tail without following.

**Java errors after a Minecraft upgrade.** `JAVA_VERSION` in `server.conf` may
be pinned to an older runtime. Clear it to restore auto-selection, then
`mc restart`.

**Mods cannot write outside `/opt/minecraft`.** Also by design — the unit runs
under `ProtectSystem=strict` with `ReadWritePaths=/opt/minecraft`. A mod that
needs another writable path needs that path added to the unit via a drop-in.

## Security

The `minecraft` service runs unprivileged and sandboxed: `NoNewPrivileges`,
`ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, `PrivateDevices`, dropped
kernel-tunable and module access, and an address-family allowlist. The intent is
that a compromised mod or JVM cannot reach anything outside the server's own
data directory. (`MemoryDenyWriteExecute` is deliberately omitted — the JIT
needs W+X pages and the server would not start under it.)

Backups are written and read by root only, never handed to the `minecraft` user,
so a compromised server cannot rewrite an archive that a later `mc restore`
unpacks as root. Archive members are validated for traversal, absolute paths,
and non-regular entry types before extraction.

All packages are signed; `apt` verifies them against the key above.

## Development

### Building packages

```bash
bash scripts/build.sh mc-server
bash scripts/build.sh mc-rcon
```

Built `.deb` files are written to `dist/`. `mc-server` is `Architecture: all`;
`mc-rcon` is compiled C and builds for whatever architecture you are on.

Install a build locally to test it:

```bash
sudo dpkg -i dist/mc-server_0.5.0_all.deb
sudo apt-get install -f     # pull in missing dependencies
```

### Running the tests

```bash
tests/run.sh              # unit + integration      ~30 s, no network
tests/run.sh --all        # + a real install of every server type (network)
tests/run.sh unit/ports   # a single suite
```

Docker is required. `run.sh` builds a Debian image, builds both `.deb` files,
installs them in a container, and runs the suites against the installed
packages — so it covers the maintainer scripts and file modes, not just the
shell functions. See [`tests/README.md`](tests/README.md) for the layout and
how to add a suite.

### Adding a package

Create `packages/<name>/` mirroring the target filesystem — `DEBIAN/control`
plus the paths the package installs (`usr/bin/`, `etc/`, `lib/systemd/system/`).
Add a `src/` directory if it needs compiling, and `build.sh` will run `make` and
stage the result for you. Subcommand plugins for `mc` drop a file into
`usr/lib/mc/commands.d/` and register themselves with `mc_register_command`, as
`mc-rcon` does.

Bump `Version:` in `DEBIAN/control` for every change you publish — `reprepro`
refuses to include a version that is already in the pool.

### Publishing to the repo

```bash
bash scripts/publish.sh dist/mc-server_0.5.0_all.deb
bash scripts/publish.sh dist/mc-rcon_0.4.2_amd64.deb
```

Requires `reprepro` and the private signing key imported into your GPG keyring.

### CI/CD

Pushing to `main` (with changes under `packages/`) or pushing a `v*` tag
triggers the [publish workflow](.github/workflows/publish.yml), which builds
packages on native `amd64` and `arm64` runners, signs and indexes them with
`reprepro`, and publishes a new multi-arch Docker image to
`ghcr.io/dylanbulmer/apt`.

Pull requests build and verify packages but never sign or publish. A
`workflow_dispatch` run with `republish: true` re-signs and re-publishes the
`.deb` artifacts from the last successful build without rebuilding them.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
