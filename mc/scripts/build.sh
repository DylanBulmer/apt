#!/usr/bin/env bash
# Build one .deb from packages/<name>/ plus the binaries its crate produces.
#
# packages/<name>/ mirrors the target filesystem root and holds everything that
# is NOT compiled — maintainer scripts, units, conffiles, plugin manifests. The
# binaries come from the cargo workspace and are copied into a staging tree, so
# the source tree stays clean and `packages/` stays readable as "what lands on
# disk".
set -euo pipefail

PACKAGE="${1:-}"
[[ -n "$PACKAGE" ]] || { echo "Usage: $0 <package-name>"; exit 1; }

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PKG_DIR="$ROOT/packages/$PACKAGE"
DIST_DIR="$ROOT/dist"
STAGING_DIR="$ROOT/staging/$PACKAGE"

[[ -d "$PKG_DIR" ]] || { echo "Package directory not found: $PKG_DIR"; exit 1; }

VERSION=$(grep '^Version:' "$PKG_DIR/DEBIAN/control" | awk '{print $2}')
ARCH=$(dpkg --print-architecture)
OUTPUT="${DIST_DIR}/${PACKAGE}_${VERSION}_${ARCH}.deb"
mkdir -p "$DIST_DIR"

# ── Which binaries this package ships, and where they install ──────────────
#
# THE SOURCE NAMES MUST MATCH THE [[bin]] ENTRIES in the crates' Cargo.toml.
# A rename on one side and not the other produces a .deb with a missing
# executable and a build that still reports success — so the copy below fails
# loudly instead of skipping.
declare -a BINARIES=()
case "$PACKAGE" in
    mc-server)  BINARIES=("mc:usr/bin/mc") ;;
    mc-rcon)    BINARIES=("mc-rcon:usr/libexec/mc/mc-rcon" "rcon:usr/bin/rcon") ;;
    mc-backup)  BINARIES=("mc-backup:usr/libexec/mc/mc-backup") ;;
    mc-mrpack)  BINARIES=("mc-mrpack:usr/libexec/mc/mc-mrpack") ;;
    mc-mgmt)    BINARIES=("mc-mgmt:usr/libexec/mc/mc-mgmt") ;;
    *)          echo "Unknown package: $PACKAGE" >&2; exit 1 ;;
esac

echo "Compiling $PACKAGE..."
# --locked so a build never silently resolves a different dependency graph than
# the one Cargo.lock records and CI tested.
( cd "$ROOT" && cargo build --release --locked $(printf -- '--bin %s ' "${BINARIES[@]%%:*}") )

rm -rf "$STAGING_DIR"
mkdir -p "$STAGING_DIR"
cp -a "$PKG_DIR/." "$STAGING_DIR/"

for spec in "${BINARIES[@]}"; do
    src="$ROOT/target/release/${spec%%:*}"
    dst="$STAGING_DIR/${spec#*:}"
    [[ -f "$src" ]] || { echo "Built binary missing: $src" >&2; exit 1; }
    mkdir -p "$(dirname "$dst")"
    cp "$src" "$dst"
done

# ── Manual pages ───────────────────────────────────────────────────────────
#
# mc.1's SYNOPSIS and COMMANDS are rendered from the clap tree by xtask, for
# the same reason completions are: a subcommand added to cli.rs and nowhere
# else still reaches the page. Everything else — the section-5 pages, and each
# plugin's own page — is prose that lives in packages/<name>/usr/share/man/ and
# is copied above with the rest of the tree.
#
# xtask is built for the HOST, not for the package's architecture. CI builds
# every architecture on a native runner, so the two are the same thing here;
# writing it this way is what keeps it true if that ever stops being so.
if [[ "$PACKAGE" == "mc-server" ]]; then
    echo "Generating mc.1..."
    ( cd "$ROOT" && cargo run --release --locked -p xtask -- man "$STAGING_DIR/usr/share/man/man1" )
fi

# ── Completions ────────────────────────────────────────────────────────────
#
# Core-only completions generated from the clap tree. Plugin subcommands are
# not included — `mc completions <shell>` is the dynamic path that discovers
# them at runtime. The static file is a convenience baseline so completions
# work immediately after install.
if [[ "$PACKAGE" == "mc-server" ]]; then
    echo "Generating completions..."
    mkdir -p "$STAGING_DIR/usr/share/bash-completion/completions"
    mkdir -p "$STAGING_DIR/usr/share/zsh/vendor-completions"
    ( cd "$ROOT" && cargo run --release --locked -p xtask \
        -- completions bash "$STAGING_DIR/usr/share/bash-completion/completions" )
    ( cd "$ROOT" && cargo run --release --locked -p xtask \
        -- completions zsh "$STAGING_DIR/usr/share/zsh/vendor-completions" )
fi

# ── Dependencies ───────────────────────────────────────────────────────────
#
# ${shlibs:Depends} is a debhelper substitution variable, and nothing
# substitutes it under plain dpkg-deb — it would be shipped literally, and apt
# would refuse the package. Resolve it here from the binaries actually built,
# so the floor matches the glibc they were linked against rather than a guess.
if grep -q '${shlibs:Depends}' "$STAGING_DIR/DEBIAN/control"; then
    SHLIB_DEPS=""
    if command -v dpkg-shlibdeps >/dev/null 2>&1; then
        BIN_PATHS=()
        for spec in "${BINARIES[@]}"; do BIN_PATHS+=("$STAGING_DIR/${spec#*:}"); done
        # dpkg-shlibdeps expects a debian/ directory to write into.
        mkdir -p "$STAGING_DIR/debian"
        : > "$STAGING_DIR/debian/control"
        if ( cd "$STAGING_DIR" && dpkg-shlibdeps -O --ignore-missing-info "${BIN_PATHS[@]}" 2>/dev/null ) \
                > "$STAGING_DIR/.shlibdeps"; then
            SHLIB_DEPS=$(sed 's/^shlibs:Depends=//' "$STAGING_DIR/.shlibdeps")
        fi
        rm -rf "$STAGING_DIR/debian" "$STAGING_DIR/.shlibdeps"
    fi
    # A Rust binary against Debian's glibc needs libc6 and nothing else; TLS is
    # rustls, so there is no OpenSSL to depend on. Falling back to the bare name
    # is honest when dpkg-shlibdeps is unavailable (building on a non-Debian
    # host), rather than shipping an unsubstituted placeholder.
    SHLIB_DEPS="${SHLIB_DEPS:-libc6}"
    sed -i.bak "s/\${shlibs:Depends}/${SHLIB_DEPS}/" "$STAGING_DIR/DEBIAN/control"
    rm -f "$STAGING_DIR/DEBIAN/control.bak"
fi

sed -i.bak "s/^Architecture:.*/Architecture: ${ARCH}/" "$STAGING_DIR/DEBIAN/control"
rm -f "$STAGING_DIR/DEBIAN/control.bak"

# ── Permissions ────────────────────────────────────────────────────────────
chmod 755 "$STAGING_DIR/DEBIAN"
for f in preinst postinst prerm postrm; do
    [[ -f "$STAGING_DIR/DEBIAN/$f" ]] && chmod 755 "$STAGING_DIR/DEBIAN/$f"
done

# Executables. Everything else installs 0644 root:root — plugin manifests and
# unit files are read by root and must not be writable by anyone else.
find "$STAGING_DIR/usr/bin"     -type f -exec chmod 755 {} \; 2>/dev/null || true
find "$STAGING_DIR/usr/libexec" -type f -exec chmod 755 {} \; 2>/dev/null || true
find "$STAGING_DIR/etc"         -type f -exec chmod 644 {} \; 2>/dev/null || true
find "$STAGING_DIR/lib/systemd" -type f -exec chmod 644 {} \; 2>/dev/null || true
find "$STAGING_DIR/usr/lib"     -type f -exec chmod 644 {} \; 2>/dev/null || true
find "$STAGING_DIR/usr/share"   -type f -exec chmod 644 {} \; 2>/dev/null || true

# ── Manual page compression ────────────────────────────────────────────────
#
# Debian policy requires man pages be gzipped, and man(1) finds them either
# way, so this is about the archive size and about matching what dh_compress
# would have produced. -n omits the timestamp and the original filename, which
# is what keeps two builds of the same source byte-identical.
#
# A page that is only a `.so` redirect (mc-restore.1) becomes a symlink to the
# compressed target, exactly as dh_compress does: gzipping the stub instead
# would leave `man mc-restore` chasing a name that no longer exists.
if [[ -d "$STAGING_DIR/usr/share/man" ]]; then
    while IFS= read -r page; do
        target=$(sed -n 's/^\.so \(.*\)$/\1/p' "$page")
        if [[ -n "$target" && $(wc -l < "$page") -eq 1 ]]; then
            rm -f "$page"
            ln -s "../${target}.gz" "${page}.gz"
        else
            gzip -9n "$page"
        fi
    done < <(find "$STAGING_DIR/usr/share/man" -type f -name '*.[1-9]')
fi

dpkg-deb --build --root-owner-group "$STAGING_DIR" "$OUTPUT"
echo "Built: $OUTPUT"
