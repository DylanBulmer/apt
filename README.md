# apt.bulmer.dev

APT packages by Dylan Bulmer, hosted at [apt.bulmer.dev](https://apt.bulmer.dev).

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
Supports Paper, Vanilla, and Fabric server types with systemd-based lifecycle management,
automated backups, and multi-version Java support.

```bash
sudo apt install mc-server
```

Requires Java 17+ (Java 21 recommended for Minecraft 1.20.5+). `mc install` will tell
you exactly which version to install for your chosen Minecraft version.

**Commands**

```
mc install <name>          Download and configure the server jar
mc update <name>           Update to the latest build
mc start <name>            Start the server
mc stop <name>             Stop the server gracefully
mc restart <name>          Restart the server
mc status <name>           Show systemd service status
mc backup <name>           Create a timestamped backup
mc restore <name> <file>   Restore from a backup archive
mc logs <name>             Follow the server log
mc delete <name>           Permanently remove a server
```

Servers run as the `minecraft` system user under `systemd`. Data lives in `/opt/minecraft/<name>`,
backups in `/var/backups/minecraft/<name>`, and per-server config in `/etc/minecraft/<name>.conf`.

---

### `mc-rcon`

Adds the `mc rcon` subcommand, providing interactive RCON console access and single-command
execution against a running server.

```bash
sudo apt install mc-rcon
```

```
mc rcon <name>             Open an interactive RCON session
mc rcon <name> <command>   Run a single command and print the response
```

RCON is enabled automatically during `mc install`. The password is stored in
`/etc/minecraft/<name>.passwd` (readable only by root and the `minecraft` user).

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

Pushing to `main` (with changes under `packages/`) or pushing a `v*` tag triggers the
[publish workflow](.github/workflows/publish.yml), which builds packages, signs them,
and publishes a new Docker image to `ghcr.io/dylanbulmer/apt`.
