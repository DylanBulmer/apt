# tests

Regression suite for the `mc-server` and `mc-rcon` packages.

```sh
tests/run.sh              # unit + integration      ~30 s, no network
tests/run.sh --all        # + install-type matrix   ~2 min, hits real APIs
tests/run.sh --unit       # unit only
tests/run.sh unit/ports   # one suite
tests/run.sh --shell      # container shell, packages installed
```

Docker is required. The suites deliberately do not run directly on a developer
machine: these are Debian packages that run under bash 5.x with GNU coreutils,
and an older bash or a non-GNU userland gives different — sometimes silently
wrong — answers.

| | |
|---|---|
| `run.sh` | builds the image, builds both `.deb`s once, runs suites in containers |
| `Dockerfile` | `debian:13-slim` + both JREs + the build toolchain |
| `systemctl-stub` | fake `systemctl`; logs calls, reports the system as degraded |
| `lib/assert.sh` | `check`/`check_has`/`check_lacks`/`report`, `lib_section`, `sandbox_init` |
| `suites/unit/` | no package install needed |
| `suites/integration/` | require `dpkg -i` of both packages |
| `suites/install-types.sh` | one real `mc install` per server type (network) |

## Adding a suite

Drop a script in `suites/unit/` or `suites/integration/` — `run.sh` picks it up
by glob. Source `lib/assert.sh`, call `check*`, end with `report`; its exit
status is the suite's.

Before writing one, read `.claude/skills/testing/SKILL.md`. It documents the
fixtures (`lib_section` for testing `lib.sh` without installing it,
`sandbox_init` for redirecting the path globals) and a catalogue of bugs this
project has hit repeatedly — several of which are ways a test can *pass
vacuously*.
