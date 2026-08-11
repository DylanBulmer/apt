# apt.bulmer.dev

![MIT License](https://img.shields.io/github/license/DylanBulmer/apt)

APT packages by Dylan Bulmer, hosted at
[apt.bulmer.dev](https://apt.bulmer.dev).

**`mc`** is a plugin-first Minecraft server manager for bare-metal Debian.
The core package installs, upgrades and runs a server; everything else —
console access, backups, modpacks — is another `.deb`.

## Contents

- [Requirements](#requirements)
- [Adding the repository](#adding-the-repository)
- [Quick start](#quick-start)
- [Packages](#packages)
- [Commands](#commands)
- [Configuration](#configuration)
- [Plugins](#plugins)
  - [`mc-rcon` — console and graceful shutdown](#mc-rcon--console-and-graceful-shutdown)
  - [`mc-backup` — backups and restores](#mc-backup--backups-and-restores)
  - [`mc-mrpack` — Modrinth modpacks](#mc-mrpack--modrinth-modpacks)
  - [Writing your own](#writing-your-own)
- [File locations](#file-locations)
- [Docker](#docker)
- [Uninstalling](#uninstalling)
- [Troubleshooting](#troubleshooting)
- [Security](#security)
- [Development](#development)
- [License](#license)

## Requirements

- Debian or Ubuntu with `systemd`
- `amd64` or `arm64` — the repository publishes no other architectures
- Java is **not** a prerequisite. `mc install` picks the right major version
  (8, 17, 21 or 25) for the Minecraft version you asked for, and installs
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
sudo apt install mc-server                  # pulls in the recommended plugins
sudo mc install --type paper                # prompts for the EULA, then downloads
sudoedit /opt/minecraft/server.properties   # optional: level-seed, motd, difficulty
sudo systemctl enable --now minecraft
mc status
```

Open port `25565` to your players and **not** port `25575` — see
[`mc-rcon`](#mc-rcon--console-and-graceful-shutdown).

> [!IMPORTANT]
> `mc install` will not set up a server until you accept the
> [Minecraft EULA](https://www.minecraft.net/eula). It asks before downloading
> anything; pass `--accept-eula` to accept it up front. Acceptance is recorded
> in `/opt/minecraft/eula.txt`, and the server refuses to start without it.

### Choosing the world before it exists

`mc install` writes a **complete** `server.properties` without generating a
world, so `level-seed`, `motd`, `difficulty` and the rest can be set before the
first start. The seed is consumed when the world is created and has no effect
afterwards — edit the file before `systemctl enable --now minecraft`.

## Packages

| Package | What it adds |
|---|---|
| **`mc-server`** | the `mc` command, the systemd unit, install/upgrade/lifecycle. Everything below is optional. |
| `mc-rcon` | `mc rcon`, the in-game shutdown countdown, world flushing around backups, `/usr/bin/rcon` |
| `mc-backup` | `mc backup`, `mc restore`, and the scheduled backup timer |
| `mc-mrpack` | `mc install pack.mrpack` — Modrinth modpacks |

`mc-server` *Recommends* all three, so a plain `apt install mc-server` gives you
everything. Install only what you want with `--no-install-recommends`, and add
or remove a plugin at any time:

```bash
sudo apt install --no-install-recommends mc-server   # core alone
sudo apt install mc-backup                           # add backups later
sudo apt remove mc-mrpack                            # drop modpack support
mc plugins                                           # what is installed, and what it adds
```

Removing a plugin withdraws its commands and leaves core working. A command
that needs a plugin you do not have says which package provides it.

## Commands

```
mc install [--type TYPE] [--version VER]   Install the server
mc install <pack.mrpack>                   Install from a Modrinth modpack  (mc-mrpack)
mc upgrade [--version VER]                 Upgrade to the newest version
mc upgrade <new.mrpack>                    Upgrade from a new modpack       (mc-mrpack)
mc delete                                  Permanently remove the server

mc start [--accept-eula]                   Start the server
mc stop                                    Stop it (with a countdown, if mc-rcon)
mc restart [--accept-eula]                 Restart it
mc status                                  Service state
mc logs                                    Follow the server log
mc plugins                                 List installed plugins

mc backup                                  Create a timestamped backup      (mc-backup)
mc restore <file>                          Restore from an archive          (mc-backup)
mc rcon [command]                          Console, or one command          (mc-rcon)
mc rcon enable | disable | status          Manage RCON                      (mc-rcon)
```

**Flags**

| Flag | Consents to | Commands |
|---|---|---|
| `--accept-eula` | the [Minecraft EULA](https://www.minecraft.net/eula) | `install`, `upgrade`, `start`, `restart` |
| `--yes` / `-y` | installing a missing `openjdk-<N>-jre-headless` | `install`, `upgrade` |
| `--force` | reinstalling over an existing server, or at the same version | `install`, `upgrade` |
| `--no-backup` | upgrading with no pre-upgrade backup | `upgrade` |

The two consent flags are deliberately separate: `--yes` agrees to install a
package, `--accept-eula` agrees to a licence. Both are needed for a fully
unattended install (cloud-init, Ansible, Docker):

```bash
sudo mc install --type paper --accept-eula --yes
```

`mc` re-runs itself under `sudo` when you forget it and there is a terminal to
prompt on. Read-only commands (`status`, `logs`, `plugins`, `mc rcon status`)
need no root at all if you are in the `minecraft` group:

```bash
sudo usermod -aG minecraft "$USER"   # then log out and back in
```

### Upgrades

`mc upgrade` moves to the newest version of your configured server type. It
decides whether there is anything to do **before** paying for a backup and the
downtime of a stop, so running it on a schedule is cheap.

> [!NOTE]
> The skip applies to `vanilla` and `neoforge` only. Paper publishes new
> *builds* against an unchanged Minecraft version, and Fabric ships new *loader*
> versions the same way — for those two, "same version" does not mean "same
> jar", so the upgrade always runs.

`mc upgrade` takes a backup first, and refuses to run without a backup provider
unless you pass `--no-backup`. A server installed from a `.mrpack` can only be
upgraded with another `.mrpack`.

## Configuration

There are two config files, and the split is deliberate.

**`/etc/minecraft/config.toml`** — how to *run* the server. A conffile: your
edits survive upgrades.

```toml
[server]
type = "paper"          # vanilla, paper, fabric, neoforge
version = "1.21.4"      # written by mc install; "latest" resolves and pins

[java]
# version = 21          # omit to select from the Minecraft version
ram = "4G"
flags = []              # empty auto-configures ZGC (21+) or Aikar's G1GC (17)
opts = []               # e.g. ["-Dfile.encoding=UTF-8"]

[backup]
keep = 7                # 0 disables rotation (it does not delete everything)
schedule = "daily"      # systemd OnCalendar= syntax
```

**`/opt/minecraft/server.properties`** — what the *game* is: port, seed, MOTD,
difficulty, RCON. The server reads and rewrites this file, so `mc` keeps no copy
of anything in it. Edit it directly; `mc` re-applies only the four keys it owns
(`server-port`, `enable-rcon`, `rcon.port`, `rcon.password`).

Changing `backup.schedule` takes effect on the next `mc install`/`mc upgrade`,
which regenerates the timer drop-in at
`/etc/systemd/system/minecraft-backup.timer.d/schedule.conf`.

## Plugins

### `mc-rcon` — console and graceful shutdown

```bash
mc rcon                    # interactive console
mc rcon list               # one command
mc rcon status             # is RCON configured, and does it actually connect?
sudo mc rcon enable        # provision a password and switch it on
```

RCON is what lets `mc` talk to a running server, and installing this package
changes three things beyond the console:

- **Stopping warns players.** With at least one player online — or if the count
  cannot be determined — a stop announces at 5, 3 and 1 minutes before it
  happens. A provably empty server stops immediately.
- **Backups are consistent.** The world is flushed and held still for the
  duration of the archive, then saving is turned back on.
- **RCON is configured for you** on install, with a generated password.

> [!WARNING]
> **Do not expose the RCON port.** The protocol is unencrypted and unauthenticated
> beyond a single password. The client refuses any host that is not loopback, and
> the password lives in `/etc/minecraft/server.passwd` (0640 `root:minecraft`),
> never in a command line. Firewall port `25575` (or whatever `rcon.port` says).

The port is `rcon.port` from `server.properties` if set, otherwise your game
port + 10. `mc rcon status` reports what it resolved.

### `mc-backup` — backups and restores

```bash
sudo mc backup                                            # now
sudo mc restore /var/backups/minecraft/minecraft-*.tar.gz # from an archive
```

Backups run on `backup.schedule` and keep the newest `backup.keep` archives.
`logs/`, `crash-reports/` and `cache/` are excluded; `mods/` and `libraries/`
are not, because a restore is a plain extraction with no re-download step.

Archives are written by root into a root-owned `0700` directory and are never
handed to the service account. A restore validates every member of the archive —
names **and** entry types — before extracting anything, so an archive containing
a symlink, hardlink or device node is refused rather than unpacked as root.

### `mc-mrpack` — Modrinth modpacks

```bash
sudo mc install ~/Downloads/cobblemon-1.7.0.mrpack --accept-eula
sudo mc upgrade ~/Downloads/cobblemon-1.8.0.mrpack
```

A `.mrpack` pins the Minecraft version, the loader and every mod in one file.
Downloads are restricted to Modrinth's CDN over https, every file must publish a
sha512 that is verified after download, and no pack file may be written outside
the server directory. Forge and Quilt packs are refused, not silently mangled.

### Writing your own

A plugin is a `.deb` that drops a TOML manifest into `/usr/lib/mc/plugins.d/`
and an executable into `/usr/libexec/mc/`:

```toml
abi  = 1
name = "hello"
bin  = "/usr/libexec/mc/mc-hello"

[[commands]]
name  = "hello"
about = "Say hello"

[[hooks]]
event = "post-install"
```

Available events: `pre-start`, `pre-stop`, `pre-backup`, `post-backup`,
`post-install`, `post-upgrade`. See
[`.claude/skills/plugin-development`](.claude/skills/plugin-development/SKILL.md)
for the full contract.

## File locations

| Path | Owner/mode | What |
|---|---|---|
| `/usr/bin/mc` | `root:root 0755` | the dispatcher |
| `/usr/bin/rcon` | `root:root 0755` | standalone RCON client (`mc-rcon`) |
| `/usr/libexec/mc/` | `root:root 0755` | plugin executables |
| `/usr/lib/mc/plugins.d/` | `root:root 0644` | plugin manifests |
| `/etc/minecraft/config.toml` | `root:root 0644` | mc's configuration (conffile) |
| `/etc/minecraft/server.passwd` | `root:minecraft 0640` | the RCON password |
| `/opt/minecraft/` | `minecraft:minecraft 0750` | the server and its world |
| `/opt/minecraft/server.properties` | `minecraft:minecraft 0640` | the game's configuration |
| `/var/backups/minecraft/` | `root:root 0700` | backup archives |
| `/lib/systemd/system/minecraft.service` | `root:root 0644` | the unit |

## Docker

`mc` works without systemd. `mc serve` runs the server in the foreground, which
is what an entrypoint wants:

```dockerfile
FROM debian:13-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl gpg && \
    curl -fsSL https://apt.bulmer.dev/bulmer.asc | gpg --dearmor -o /etc/apt/keyrings/bulmer.gpg && \
    echo "deb [signed-by=/etc/apt/keyrings/bulmer.gpg] https://apt.bulmer.dev stable main" \
      > /etc/apt/sources.list.d/bulmer.list && \
    apt-get update && apt-get install -y mc-server mc-rcon
RUN mc install --type paper --accept-eula --yes
USER minecraft
CMD ["mc", "serve"]
```

`mc install`, `mc serve` and `mc shutdown` all work with no systemd present;
commands that drive the unit (`mc start`, `mc stop`, `mc status`) report that
there is nothing to drive.

## Uninstalling

```bash
sudo mc delete                          # the server and its secrets; keeps backups
sudo apt remove mc-server mc-rcon mc-backup mc-mrpack
sudo apt purge  mc-server               # also removes /etc/minecraft/config.toml
```

`/var/backups/minecraft` and the `minecraft` user are deliberately left alone —
the archives are your data, not the package's. Remove them by hand if you mean
to.

## Troubleshooting

**The server will not start.** `mc logs`, or `systemctl status minecraft`. An
exit code of **78** means an operator-fixable problem — the EULA is not
accepted, `server.properties` is unreadable, or there is no server jar — and
systemd deliberately does *not* restart it. Any other non-zero exit is a real
crash and does get restarted.

**"server.properties is not readable and writable".** It is root-owned. Editing
it as root with an editor that writes and renames leaves it that way, and the
server would otherwise start on compiled-in defaults and generate a stray world
beside the real one.

```bash
sudo chown minecraft:minecraft /opt/minecraft/server.properties
sudo chmod 640 /opt/minecraft/server.properties
```

Use `sudoedit`, which preserves ownership.

**`mc rcon` says "Connection: FAILED".** The server reads `server.properties` at
startup, so RCON settings need a restart to take effect: `sudo mc restart`.

**A command says "Unknown command".** It comes from a plugin you do not have.
The message names the package; `mc plugins` shows what is installed, including
any plugin that failed to load and why.

**"plugin declares ABI N but this mc implements ABI M".** The packages are out
of step. `sudo apt update && sudo apt upgrade` brings them back in line.

**Java errors after a Minecraft upgrade.** Set `java.version` in
`/etc/minecraft/config.toml`, or remove it to let mc choose.

**`mc stop` takes five minutes.** That is the countdown, with players online or
a player count that could not be determined. `mc logs` shows which.

## Security

- The server runs as the unprivileged `minecraft` user under a hardened systemd
  unit — `ProtectSystem=strict`, `NoNewPrivileges`, `PrivateDevices`, and
  `/opt/minecraft` as the only writable path.
- The RCON password is generated (192 bits), stored `0640 root:minecraft`, and
  passed to the client by file — never in argv, which is world-readable through
  `/proc/<pid>/cmdline`.
- Downloaded artifacts are verified against the digest their index publishes,
  and an index that publishes none is a refusal rather than a skip. (The one
  exception is Fabric's server jar, which upstream publishes no hash for; it is
  trusted on TLS alone, and this is documented in the source.)
- Modpacks and backup archives are treated as attacker-controlled: paths that
  escape the server directory, downloads from unlisted hosts, and archive
  members that are links or device nodes are all refused before anything is
  written.
- Report a vulnerability to <dylan@bulmer.dev>.

## Development

```bash
cargo test --workspace     # unit, integration and security suites — ~1 s
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check

tests/run.sh               # container suites (Docker) — packaging, ACLs, plugins
tests/run.sh --all         # + one real install of each server type

bash scripts/build.sh mc-server   # → dist/*.deb  (needs Debian, or the container)
```

The workspace is `crates/`; `packages/<name>/` mirrors the target filesystem
root and holds everything not compiled. `scripts/build.sh` joins them.

**Bump `Version:` in `DEBIAN/control` in the same commit as any change to a
package.** CI publishes on every push and reprepro regenerates its indexes per
run, so a change without a bump is served under the old version and never
reaches an installed system.

CI runs the test suite, builds every package on a native runner per
architecture, then signs and indexes. A red test gate blocks publishing.

Repository guidance for contributors — and for Claude Code — lives in
[`CLAUDE.md`](CLAUDE.md) and `.claude/skills/`.

## License

[MIT](LICENSE) © Dylan Bulmer
