#!/usr/bin/env bash
# Build .debs and spin up an interactive container for testing mc packages.
#
#   bash scripts/dev-shell.sh           # run (builds if image missing)
#   bash scripts/dev-shell.sh --build    # build only
#   bash scripts/dev-shell.sh --run      # run only
#   bash scripts/dev-shell.sh --clean    # remove image + temp files
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/dist"
IMAGE="mc-dev-shell"
BUILD_ONLY=no
RUN_ONLY=no
CLEAN=no

for arg in "$@"; do
    case "$arg" in
        --build) BUILD_ONLY=yes ;;
        --run)   RUN_ONLY=yes ;;
        --clean) CLEAN=yes ;;
        *)       echo "Usage: $0 [--build|--run|--clean]" >&2; exit 1 ;;
    esac
done

if [[ "$CLEAN" == yes ]]; then
    docker rmi "$IMAGE" >/dev/null 2>&1 || true
    rm -rf "$DIST"
    echo "Cleaned."
    exit 0
fi

# ── Build packages ────────────────────────────────────────────────────────────
if [[ "$RUN_ONLY" != yes ]] || ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "==> building packages"
    mkdir -p "$DIST"

    # Build inside the build container so glibc matches Debian 13
    docker build -q -t mc-build-local -f "$ROOT/tests/Dockerfile.build" "$ROOT" >/dev/null 2>&1

    docker run --rm \
        -v "$ROOT":/src:ro \
        -v "$DIST":/dist \
        mc-build-local bash -c '
            set -e
            cp -a /src/. /build/
            rm -rf /build/dist /build/staging
            for pkg in mc-server mc-rcon mc-mgmt mc-backup mc-mrpack; do
                echo "  building $pkg"
                bash scripts/build.sh "$pkg" >/dev/null
            done
            cp dist/*.deb /dist/
        '

    built=$(ls -1 "$DIST"/*.deb 2>/dev/null | wc -l | tr -d ' ')
    if [[ "$built" -ne 5 ]]; then
        echo "ERROR: expected 5 .deb files, got $built" >&2
        exit 1
    fi
    ls -1 "$DIST"/*.deb | sed 's/^/  /'
    echo
fi

# ── Build test image ──────────────────────────────────────────────────────────
echo "==> building $IMAGE"
docker build -q -t "$IMAGE" -f "$ROOT/scripts/Dockerfile.dev" "$ROOT" >/dev/null
echo "done"

if [[ "$BUILD_ONLY" == yes ]]; then
    exit 0
fi

# ── Run ───────────────────────────────────────────────────────────────────────
echo
echo "  mc plugins   — list installed plugins"
echo "  mc install    — install a server"
echo "  mc backup     — take a backup"
echo "  mc restore    — restore from backup"
echo "  su minecraft  — switch to service account"
echo
exec docker run --rm -it "$IMAGE"