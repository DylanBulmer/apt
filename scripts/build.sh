#!/usr/bin/env bash
# Build a .deb from packages/<name>/
#
# Packages with a src/ directory are compiled first; the binary is installed
# into a temporary staging directory before dpkg-deb runs, keeping the source
# tree clean.
set -euo pipefail

PACKAGE="${1:-}"
[[ -n "$PACKAGE" ]] || { echo "Usage: $0 <package-name>"; exit 1; }

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PKG_DIR="$ROOT/packages/$PACKAGE"
DIST_DIR="$ROOT/dist"
STAGING_DIR="$ROOT/staging/$PACKAGE"

[[ -d "$PKG_DIR" ]] || { echo "Package directory not found: $PKG_DIR"; exit 1; }

VERSION=$(grep '^Version:'      "$PKG_DIR/DEBIAN/control" | awk '{print $2}')
ARCH=$(   grep '^Architecture:' "$PKG_DIR/DEBIAN/control" | awk '{print $2}')
mkdir -p "$DIST_DIR"

if [[ -d "$PKG_DIR/src" ]]; then
    # ── Compiled package ───────────────────────────────────────────────────
    # Detect the build machine's architecture and use it for the output name,
    # regardless of what the control file says (it may say 'amd64' as a default).
    ARCH=$(dpkg --print-architecture)
    OUTPUT="${DIST_DIR}/${PACKAGE}_${VERSION}_${ARCH}.deb"

    echo "Compiling $PACKAGE..."
    make -C "$PKG_DIR/src" clean
    make -C "$PKG_DIR/src"

    # Build staging tree: copy non-src package files, install compiled binary
    rm -rf "$STAGING_DIR"
    mkdir -p "$STAGING_DIR"
    rsync -a --exclude=src/ "$PKG_DIR/" "$STAGING_DIR/"

    # Patch Architecture in staging control to match actual build arch
    sed -i "s/^Architecture:.*/Architecture: ${ARCH}/" "$STAGING_DIR/DEBIAN/control"

    make -C "$PKG_DIR/src" install DESTDIR="$STAGING_DIR"

    BUILD_DIR="$STAGING_DIR"
else
    # ── Script/data package ────────────────────────────────────────────────
    # 'any' is only meaningful for packages built from source; without a src/
    # dir there is nothing to compile, so resolve it to the build machine's
    # architecture rather than emitting a bogus '_any.deb'.
    if [[ "$ARCH" == "any" ]]; then
        ARCH=$(dpkg --print-architecture)
        echo "Warning: $PACKAGE declares 'Architecture: any' but has no src/;" \
             "using build host architecture '${ARCH}'." >&2
    fi
    OUTPUT="${DIST_DIR}/${PACKAGE}_${VERSION}_${ARCH}.deb"
    BUILD_DIR="$PKG_DIR"
fi

# Fix permissions required by dpkg-deb
chmod 755 "$BUILD_DIR/DEBIAN"
for f in postinst prerm postrm preinst; do
    [[ -f "$BUILD_DIR/DEBIAN/$f" ]] && chmod 755 "$BUILD_DIR/DEBIAN/$f"
done

find "$BUILD_DIR/usr/bin"   -type f              -exec chmod 755 {} \; 2>/dev/null || true

# Shell files under /usr/lib default to 644: most are *sourced*, not executed
# (lib.sh, common.sh, commands.d/*.sh). They install root:root, and root sources
# and runs them as root, so they must not be writable by anyone but root — and
# they need no execute bit to be sourced. Only the scripts systemd actually
# invokes are made executable, individually, below.
find "$BUILD_DIR/usr/lib"   -type f -name '*.sh' -exec chmod 644 {} \; 2>/dev/null || true
find "$BUILD_DIR/usr/lib"   -type f -name '*.py' -exec chmod 644 {} \; 2>/dev/null || true

# Exec targets of minecraft.service (ExecStart=/ExecStop=/ExecReload=).
for _exec in start.sh stop.sh reload.sh; do
    [[ -f "$BUILD_DIR/usr/lib/mc/$_exec" ]] && chmod 755 "$BUILD_DIR/usr/lib/mc/$_exec"
done

find "$BUILD_DIR/lib/systemd" -type f            -exec chmod 644 {} \; 2>/dev/null || true
find "$BUILD_DIR/usr/share"   -type f            -exec chmod 644 {} \; 2>/dev/null || true
find "$BUILD_DIR/etc"         -type f            -exec chmod 644 {} \; 2>/dev/null || true

dpkg-deb --build --root-owner-group "$BUILD_DIR" "$OUTPUT"

echo "Built: $OUTPUT"
