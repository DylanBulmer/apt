#!/usr/bin/env bash
# Sign a .deb and include it in the reprepro repository.
set -euo pipefail

DEB="${1:-}"
CODENAME="${2:-stable}"

[[ -n "$DEB" ]] || { echo "Usage: $0 <path/to/package.deb> [codename]"; exit 1; }
[[ -f "$DEB" ]] || { echo "File not found: $DEB"; exit 1; }

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO_DIR="$ROOT/repo"

reprepro -b "$REPO_DIR" includedeb "$CODENAME" "$DEB"
echo "Published $(basename "$DEB") to '$CODENAME'."
