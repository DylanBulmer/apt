#!/usr/bin/env bash
# Regression suite for mc-server / mc-rcon.
#
#   tests/run.sh                 unit + integration            (~30 s, no network)
#   tests/run.sh --all           + the install-type matrix     (~2 min, network)
#   tests/run.sh --unit          unit only
#   tests/run.sh unit/ports      one suite (path under tests/suites, no .sh)
#   tests/run.sh --shell         drop into the container with everything installed
#
# Everything runs inside Docker. That is not ceremony: these packages target
# Debian 13 with bash 5.x and GNU coreutils, and on any other combination
# several of these assertions are not merely awkward but actively misleading
# (see the testing skill).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
IMAGE="mc-tests:local"
WORK="${TMPDIR:-/tmp}/mc-tests.$$"

RUN_UNIT=yes
RUN_INTEGRATION=yes
RUN_INSTALL_TYPES=no
SHELL_ONLY=no
SUITES=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --all)         RUN_INSTALL_TYPES=yes; shift ;;
        --unit)        RUN_INTEGRATION=no; shift ;;
        --integration) RUN_UNIT=no; shift ;;
        --shell)       SHELL_ONLY=yes; shift ;;
        -h|--help)     sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*)            echo "Unknown option: $1" >&2; exit 2 ;;
        *)             SUITES+=("$1"); RUN_UNIT=no; RUN_INTEGRATION=no; shift ;;
    esac
done

command -v docker >/dev/null 2>&1 || {
    echo "docker is required: these suites are only meaningful on the Debian target." >&2
    exit 1
}

echo "==> building $IMAGE"
docker build -q -t "$IMAGE" "$HERE" >/dev/null

# Build the .debs ONCE and share them with every container. Rebuilding per suite
# is the single biggest waste in a full run.
echo "==> building packages"
mkdir -p "$WORK/dist"
trap 'rm -rf "$WORK"' EXIT
# `set -e` inside the container, and an explicit count afterwards. Without both,
# a failed build here is silent: the surviving .deb from the other package still
# gets copied, and the first symptom is a suite failing on a glob that matched
# nothing, several minutes and one wrong diagnosis later.
docker run --rm -v "$ROOT":/src:ro -v "$WORK/dist":/dist --entrypoint bash "$IMAGE" -c '
    set -e
    cp -r /src /b && cd /b && rm -rf dist staging
    bash scripts/build.sh mc-server >/dev/null
    bash scripts/build.sh mc-rcon   >/dev/null
    cp dist/*.deb /dist/'
ls -1 "$WORK/dist" | sed 's/^/    /'
built=$(ls -1 "$WORK/dist"/*.deb 2>/dev/null | wc -l | tr -d ' ')
[[ "$built" -eq 2 ]] || { echo "expected 2 .deb files, got $built" >&2; exit 1; }

# Mount the repo read-only and install the freshly built packages. Read-only is
# deliberate: a suite must never be able to edit the tree it is testing.
docker_run() {
    docker run --rm \
        -v "$ROOT":/work:ro \
        -v "$WORK/dist":/dist:ro \
        -e MC_REPO=/work \
        --entrypoint bash "$IMAGE" "$@"
}

install_cmd='
    mkdir -p /run/systemd/system
    dpkg -i /dist/mc-server_*.deb >/dev/null 2>&1
    dpkg -i /dist/mc-rcon_*.deb   >/dev/null 2>&1
'

if [[ "$SHELL_ONLY" == yes ]]; then
    echo "==> container shell (packages installed, repo at /work)"
    exec docker run --rm -it \
        -v "$ROOT":/work:ro -v "$WORK/dist":/dist:ro -e MC_REPO=/work \
        --entrypoint bash "$IMAGE" -c "$install_cmd; exec bash"
fi

FAILED=()

run_suite() { # run_suite <relative path under tests/suites, no .sh>
    local name="$1"
    printf '\n==> %s\n' "$name"
    if docker_run -c "$install_cmd bash /work/tests/suites/${name}.sh"; then :; else
        FAILED+=("$name")
    fi
}

if [[ ${#SUITES[@]} -gt 0 ]]; then
    for s in "${SUITES[@]}"; do run_suite "$s"; done
else
    if [[ "$RUN_UNIT" == yes ]]; then
        for f in "$HERE"/suites/unit/*.sh; do run_suite "unit/$(basename "$f" .sh)"; done
    fi
    if [[ "$RUN_INTEGRATION" == yes ]]; then
        for f in "$HERE"/suites/integration/*.sh; do run_suite "integration/$(basename "$f" .sh)"; done
    fi
fi

# The install-type matrix is separate: it is the only thing here that reaches the
# network, it is minutes rather than seconds, and each type needs its own clean
# /opt/minecraft — so the four run as four parallel containers rather than
# sequentially in one.
if [[ "$RUN_INSTALL_TYPES" == yes ]]; then
    printf '\n==> install-types (vanilla, paper, fabric, neoforge in parallel)\n'
    mkdir -p "$WORK/results"
    for t in vanilla paper fabric neoforge; do
        docker run --rm \
            -v "$ROOT":/work:ro -v "$WORK/dist":/dist:ro -v "$WORK/results":/res \
            -e MC_REPO=/work -e MC_TYPE="$t" \
            --entrypoint bash "$IMAGE" \
            -c "$install_cmd bash /work/tests/suites/install-types.sh" >/dev/null 2>&1 &
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
