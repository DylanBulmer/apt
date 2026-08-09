---
name: testing
description: How to test and verify changes to the mc-server / mc-rcon Debian packages — the tests/ regression suite, the Docker environment it needs, what can and cannot be verified without a real systemd or Minecraft server, and a catalogue of bugs this project has hit more than once (bash traps, packaging traps, upstream API drift) with the resolution for each. Use BEFORE writing a test, running one, claiming a change is verified, or debugging a suite that fails for an unclear reason.
---

# Testing apt.bulmer.dev

## Run it

```sh
tests/run.sh                 # unit + integration      ~30 s, no network
tests/run.sh --all           # + install-type matrix   ~2 min, hits real APIs
tests/run.sh --unit          # unit only
tests/run.sh unit/ports      # one suite (path under tests/suites, no .sh)
tests/run.sh --shell         # container shell, packages installed, repo at /work
```

`run.sh` builds `tests/Dockerfile`, builds both `.deb`s **once** into a shared
dir, then runs each suite in its own container with the repo mounted read-only
at `/work`. Exit status is the suite result; `ALL SUITES PASSED` is the only
success line.

**Everything runs in Docker, and that is not ceremony.** The target is Debian 13
with bash 5.x and GNU coreutils. Running a suite under an older bash (3.x is
still `/bin/bash` on some systems) or a non-GNU userland does not merely make
assertions awkward — it produces *wrong* answers. See "bash traps" below.

## Layout

```
tests/run.sh                     orchestrator (build image → build debs → run suites)
tests/Dockerfile                 debian:13-slim + both JREs + toolchain
tests/systemctl-stub             fake systemctl; logs calls, reports DEGRADED
tests/lib/assert.sh              check/check_has/check_lacks/report, lib_section, sandbox_init
tests/suites/unit/*.sh           no package install needed
tests/suites/integration/*.sh    require `dpkg -i` of both packages
tests/suites/install-types.sh    one real `mc install` of $MC_TYPE (network)
```

A suite is a plain bash script: source `lib/assert.sh`, call `check*`, end with
`report`. No framework. Each runs standalone
(`bash tests/suites/unit/ports.sh`) as well as under `run.sh`.

**Unit suites cannot source `lib.sh`** — its first statement is
`source /usr/lib/mc/common.sh`, an absolute path that exists only once
installed. Use `lib_section 'Cleanup registry' 'Process lock'` to `eval` just
the section under test, and `sandbox_init` to repoint the path globals (they are
assigned at source time, so they can only be overridden afterwards).

## What still cannot be verified

`systemd-analyze verify` on the unit (the container has a stub, not systemd),
and anything needing a live Minecraft server with players. Say so rather than
implying it was covered.

---

# Recurring issues

Each of these has cost real debugging time in this repo more than once.

## bash traps

**`set -e` is disabled inside `( … ) || rc=$?`.** A subshell on the left of
`||` is a tested context, and bash disables `set -e` *within* it. An abort-path
test written that way passes vacuously — the abort never fires.
→ Run the scenario as a **separate `bash` process** and capture `$?` with
`set +e` around it. This is documented bash behaviour, not a quirk of one
version — it reproduces on 5.x as readily as on 3.x.

**`out=$(cmd) 2>&1` redirects the assignment, not the substitution.** The
assignment produces no stderr, so the command's stderr bypasses the capture and
prints to the terminal — and `$out` is empty of exactly the lines you wanted on
failure. → `out=$( { cmd; } 2>&1 )`.

**`trap … RETURN` is not scoped to the function that sets it.** It stays armed
and fires *again* when the caller returns, when the local it references is gone
and `set -u` aborts with `unbound variable`. → Don't hand-roll traps; use the
cleanup registry in `lib.sh`, or stage temp files inside an
already-registered staging dir.

**`grep … | cut …` returns grep's status.** Under `pipefail`, a key that is
simply absent returns 1, and a plain assignment then aborts the whole run under
`set -e`. → Parse through `mc_sprop_get`, which always succeeds and reports
"absent" as the empty string.

**Arithmetic contexts execute code.** `[[ "$x" -ge 26 ]]` evaluates its operand
as an expression and performs command substitution inside array subscripts, so
`PATH[$(rm -rf /)]` runs. `set -u` does not save you — the base variable exists.
→ Force to digits before comparing; `ports.sh` asserts this with a canary file.

**`stat -f '%OLp'` (BSD) vs `stat -c '%a'` (GNU).** → `file_mode` in
`lib/assert.sh`.

## Packaging traps

**`systemctl is-system-running` is a health check, not a presence check.** It
succeeds only for the exact state `running` and returns non-zero for `degraded`
— any machine with one failed unit, anywhere. Gating a postinst on it silently
skips `daemon-reload`, and if the failed unit is your own the skip is
self-sustaining. → Gate on `[ -d /run/systemd/system ]`. `packaging.sh` runs
with the stub reporting degraded, so the healthy case is never the only one
tested.

**A change without a version bump never reaches installed systems.** CI
publishes on every push touching `packages/**` and reprepro regenerates per run,
so several commits under one unchanged `Version:` republish that number as
different artifacts; whoever installed the earlier one is pinned to it forever.
→ Bump in the *same commit*. Verify what the mirror actually holds:
```sh
curl -s https://apt.bulmer.dev/dists/stable/main/binary-amd64/Packages | grep -A1 '^Package: mc-'
dpkg-deb -x mc-server_<ver>_all.deb /tmp/x && grep -r <a-function-you-added> /tmp/x/usr/lib/mc/
```

**mc-rcon calls shell functions out of mc-server's `common.sh`**, so
`Depends: mc-server` alone is not a strong enough claim — dpkg will configure it
against an older library and the maintainer script dies with `command not found`
(exit 127), leaving the package half-installed. → Keep the version floor in
`mc-rcon/DEBIAN/control` current. `packaging.sh` asserts the floor matches the
mc-server being shipped, and that every borrowed function exists.

**`gcc` alone cannot compile `rcon.c`** — no libc headers, so
`#include <arpa/inet.h>` fails, mc-rcon never builds, and the first symptom is a
glob matching nothing much later. → `libc6-dev` is in the Dockerfile; `run.sh`
asserts two `.deb`s exist before running anything.

**Root-owned `server.properties` is a silent killer.** 0640 is readable only
because the owner is the service account; root-owned, the JVM can neither read
nor write it, falls back to compiled-in defaults, and generates a stray world
next to the real one. → Anything that writes it runs as `$MC_USER` or calls
`sprop_secure` afterwards. Assert mode *and* owner.

## Upstream drift

**PaperMC v2 is sunset** — `api.papermc.io/v2` returns HTTP 410
`{"ok":false,"error":"sunset"}`. → v3 lives at `fill.papermc.io/v3`, and the
shape differs: `.versions` is an object keyed by release family (newest first),
builds are a bare newest-first array, and each build carries a ready-made URL on
a separate host. `tests/run.sh --all` is what catches this class of breakage.

**Don't judge a launcher by its exit code.** NeoForge's FML wrapper exits 1 even
when `--initSettings` succeeded: the flag returns without ever starting the
server thread, so FML logs `Initialized '…/server.properties'` and *then* throws
`Couldn't find Minecraft server thread`. → Assert on the outcome — did the file
gain the key — not on the proxy.

**NeoForge and Minecraft now share a version scheme** (`26.2.0.54-beta`), so
`mc_required_java` happens to be right for current builds. It is still wrong for
older pinned NeoForge (`21.1.66` parses as minor=1 → Java 8, which Debian 13
does not ship). Known, unfixed.

## Test-environment gotchas

- The image has an **ENTRYPOINT**; override it (`--entrypoint bash`) when
  running containers by hand.
- Mount the repo **read-only** and copy it inside before building —
  `scripts/build.sh` writes `dist/`, `staging/`, and chmods the tree.
- **Bake the JREs into the image.** Letting `ensure_java` apt-install one per
  run dominates the runtime of the install matrix.
- `shellcheck`'s **SC2034 "appears unused" is a false positive** across sourced
  files — `common.sh` globals are consumed by `lib.sh`/`start.sh`. A hit on a
  *colour code* or a helper nothing sources is real dead code.
