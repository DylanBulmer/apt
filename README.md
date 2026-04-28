# apt.bulmer.dev

APT packages by Dylan Bulmer, hosted at
[apt.bulmer.dev](https://apt.bulmer.dev).

## Adding the repository

```bash
curl -fsSL https://apt.bulmer.dev/bulmer.asc \
  | sudo gpg --dearmor -o /etc/apt/keyrings/bulmer.gpg

echo "deb [signed-by=/etc/apt/keyrings/bulmer.gpg] https://apt.bulmer.dev stable main" \
  | sudo tee /etc/apt/sources.list.d/bulmer.list

sudo apt update
```

## Packages

### `mc-server`

Manages a Minecraft server instance on bare-metal or inside VMs/LXC containers.
Supports Paper, Vanilla, Fabric, and NeoForge server types with systemd-based
lifecycle management, automated backups, and multi-version Java support.

```bash
sudo apt install mc-server
```

Requires Java 17+ (Java 21 recommended for Minecraft 1.20.5+). `mc install` will
tell you exactly which version to install for your chosen Minecraft version.

**Commands**

```
mc install [--type TYPE] [--version VER]   Install the server jar
mc install <pack.mrpack>                   Install from a Modrinth modpack
mc upgrade [--version VER]                 Upgrade to a newer version
mc upgrade <new.mrpack>                    Upgrade from a new Modrinth modpack
mc start                                   Start the server
mc stop                                    Stop the server gracefully
mc restart                                 Restart the server
mc status                                  Show systemd service status
mc backup                                  Create a timestamped backup
mc restore <file>                          Restore from a backup archive
mc logs                                    Follow the server log
mc delete                                  Permanently remove the server
```

Server types: `vanilla` (default), `paper`, `fabric`, `neoforge`.

The server runs as the `minecraft` system user under `systemd`. Data lives in
`/opt/minecraft`, backups in `/var/backups/minecraft`, and configuration in
`/etc/minecraft/server.conf`. RCON is **disabled by default**; install `mc-rcon`
to enable it automatically.

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
```

Installing `mc-rcon` automatically enables RCON on the managed server and
generates a random password stored in `/etc/minecraft/server.passwd` (readable
only by root and the `minecraft` user).

> [!WARNING]
> RCON is an **unencrypted protocol** — the password and all commands travel in
> plaintext. The `rcon` binary enforces loopback-only connections and will
> refuse any host that does not resolve to `127.0.0.0/8` or `::1`. For
> additional defence, keep the RCON port off the network: do not bind it to a
> public interface or publish it through a firewall (default port `25575`).

---

## Development

### Building packages

```bash
bash scripts/build.sh mc-server
bash scripts/build.sh mc-rcon
```

Built `.deb` files are written to `dist/`.

### Publishing to the repo

```bash
bash scripts/publish.sh dist/mc-server_0.1.0_all.deb
bash scripts/publish.sh dist/mc-rcon_0.1.0_amd64.deb
```

Requires `reprepro` and the private signing key imported into your GPG keyring.

### CI/CD

Pushing to `main` (with changes under `packages/`) or pushing a `v*` tag
triggers the [publish workflow](.github/workflows/publish.yml), which builds
packages, signs them, and publishes a new Docker image to
`ghcr.io/dylanbulmer/apt`.
