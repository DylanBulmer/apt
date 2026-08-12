#!/usr/bin/env bash
# Container suites for the mc packages.
#
# Invoked from anywhere — it resolves its own root — so the paths below are
# written as they read from the repository root.
#
#   mc/tests/run.sh                 integration suites       (~1 min, no network)
#   mc/tests/run.sh --all           + the install-type matrix (~3 min, real APIs)
#   mc/tests/run.sh integration/plugins  one suite (path under tests/suites, no .sh)
#   mc/tests/run.sh --shell         container shell with everything installed
#
# THIS IS TIER 4 ONLY. Everything that does not need a Debian root — parsing,
# validation, install/upgrade ordering, the exit-code policy, hook semantics —
# is `cargo test`, runs on any host in about a second, and is where a new test
# should go by default. What is left here is what genuinely needs dpkg, real
# ownership, and the service account:
#
#   integration/packaging       file modes, ownership, control metadata
#   integration/access-control  root vs the minecraft group vs neither
#   integration/plugins         install/remove plugins, ABI gate, discovery
#   install-types               one real `mc install` per server type (network)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
TEST_IMAGE="mc-tests:local"
BUILD_IMAGE="mc-build:local"
WORK="${TMPDIR:-/tmp}/mc-tests.$$"

RUN_INTEGRATION=yes
RUN_INSTALL_TYPES=no
SHELL_ONLY=no
SUITES=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --all)      RUN_INSTALL_TYPES=yes; shift ;;
        --shell)    SHELL_ONLY=yes; shift ;;
        -h|--help)  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*)         echo "Unknown option: $1" >&2; exit 2 ;;
        *)          SUITES+=("$1"); RUN_INTEGRATION=no; shift ;;
    esac
done

command -v docker >/dev/null 2>&1 || {
    echo "docker is required: these suites are only meaningful on the Debian target." >&2
    exit 1
}

mkdir -p "$WORK/dist"
trap 'rm -rf "$WORK"' EXIT

echo "==> building images"
docker build -q -t "$BUILD_IMAGE" -f "$HERE/Dockerfile.build" "$HERE" >/dev/null
docker build -q -t "$TEST_IMAGE"                              "$HERE" >/dev/null

# Build the .debs ONCE and share them with every container.
#
# The cargo registry and target dir are cached in named volumes: without them
# every run recompiles the whole dependency graph, which dominates the runtime
# of the suite far more than any test does.
#
# A single workspace build compiles all binaries at once. --release has no
# incremental compilation, so building 5 packages separately means 5 full
# compilations. build.sh skips the cargo invocation when binaries exist.
echo "==> building packages"
docker run --rm \
    -v "$ROOT":/src:ro \
    -v "$WORK/dist":/dist \
    -v mc-cargo-registry:/usr/local/cargo/registry \
    -v mc-cargo-target:/build/target \
    "$BUILD_IMAGE" bash -c '
        set -e
        cp -a /src/. /build/
        rm -rf /build/dist /build/staging
        echo "  compiling workspace..."
        cargo build --release --locked --workspace
        for pkg in mc-server mc-rcon mc-backup mc-mrpack mc-mgmt; do
            bash scripts/build.sh "$pkg" >/dev/null
        done
        cp dist/*.deb /dist/'

ls -1 "$WORK/dist" | sed 's/^/    /'
# An explicit count: without it a failed build is silent, and the first symptom
# is a suite failing on a glob that matched nothing, several minutes and one
# wrong diagnosis later.
built=$(ls -1 "$WORK/dist"/*.deb 2>/dev/null | wc -l | tr -d ' ')
[[ "$built" -eq 5 ]] || { echo "expected 5 .deb files, got $built" >&2; exit 1; }

# Mount the repo read-only: a suite must never be able to edit the tree it is
# testing.
docker_run() {
    docker run --rm \
        -v "$ROOT":/work:ro \
        -v "$WORK/dist":/dist:ro \
        -e MC_REPO=/work \
        "$TEST_IMAGE" bash "$@"
}

# Core first, then the plugins: each plugin Depends: mc-server, and dpkg refuses
# to configure a package whose dependency is not configured yet.
install_all='
    dpkg -i /dist/mc-server_*.deb >/dev/null 2>&1
    dpkg -i /dist/mc-rcon_*.deb /dist/mc-mgmt_*.deb /dist/mc-backup_*.deb /dist/mc-mrpack_*.deb >/dev/null 2>&1
'

if [[ "$SHELL_ONLY" == yes ]]; then
    echo "==> container shell (packages installed, repo at /work, debs at /dist)"
    exec docker run --rm -it \
        -v "$ROOT":/work:ro -v "$WORK/dist":/dist:ro -e MC_REPO=/work \
        "$TEST_IMAGE" bash -c "$install_all; exec bash"
fi

FAILED=()

run_suite() { # run_suite <relative path under tests/suites, no .sh>
    local name="$1"
    printf '\n==> %s\n' "$name"
    if docker_run -c "bash /work/tests/suites/${name}.sh"; then :; else
        FAILED+=("$name")
    fi
}

if [[ ${#SUITES[@]} -gt 0 ]]; then
    for s in "${SUITES[@]}"; do run_suite "$s"; done
elif [[ "$RUN_INTEGRATION" == yes ]]; then
    for f in "$HERE"/suites/integration/*.sh; do
        run_suite "integration/$(basename "$f" .sh)"
    done
fi

# The install-type matrix is separate: it is the only thing here that reaches
# the network, it is minutes rather than seconds, and each type needs its own
# clean /opt/minecraft — so the four run as four parallel containers.
#
# IT IS ALSO THE ONLY THING THAT CATCHES UPSTREAM DRIFT. Every API this talks to
# has changed shape at least once (PaperMC v2 was sunset outright), and the
# offline fixtures the cargo tests use cannot notice. Run it on a schedule, not
# only when something changes here.
if [[ "$RUN_INSTALL_TYPES" == yes ]]; then
    printf '\n==> install-types (vanilla, paper, fabric, neoforge in parallel)\n'
    mkdir -p "$WORK/results"
    for t in vanilla paper fabric neoforge; do
        docker run --rm \
            -v "$ROOT":/work:ro -v "$WORK/dist":/dist:ro -v "$WORK/results":/res \
            -e MC_REPO=/work -e MC_TYPE="$t" \
            "$TEST_IMAGE" \
            bash -c "$install_all bash /work/tests/suites/install-types.sh" >/dev/null 2>&1 &
    done
    wait
    cat "$WORK/results"/*.txt
    if grep -q FAIL "$WORK/results"/*.txt; then FAILED+=("install-types"); fi
fi

echo
if [[ ${#FAILED[@]} -eq 0 ]]; then
    echo "ALL SUITES PASSED"
else
    printf 'FAILED: %s\n' "${FAILED[*]}"
    exit 1
fi
