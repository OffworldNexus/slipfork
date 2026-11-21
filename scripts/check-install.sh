#!/usr/bin/env bash
# End-to-end check of the universal installer without a package manager or a
# network: build a standalone archive, hand it to docs/install.sh as a file://
# origin with its SHA-256, and run the installed binary.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

bundle="target/package-bundle"
prefix=""

help() {
    cat <<'USAGE'
usage: scripts/check-install.sh [--bundle <dir>] [--prefix <dir>]
USAGE
}

usage() {
    help >&2
    exit 2
}
while [ "$#" -gt 0 ]; do
    case "$1" in
        --bundle) [ "$#" -ge 2 ] || usage; bundle=$2; shift 2 ;;
        --prefix) [ "$#" -ge 2 ] || usage; prefix=$2; shift 2 ;;
        -h | --help) help; exit 0 ;;
        *) echo "error: unknown argument $1; next: run with --help" >&2; exit 2 ;;
    esac
done
[ -d "$bundle/share/slipfork/lib" ] || { echo "error: $bundle is not a bundle; next: run scripts/build-bundle.sh" >&2; exit 2; }
tmp=$(mktemp -d "${TMPDIR:-/var/tmp}/slipfork-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM
[ -n "$prefix" ] || prefix="$tmp/prefix"
scripts/build-standalone.sh --bundle "$bundle" --out "$tmp/archive" >/dev/null
archive=$(find "$tmp/archive" -name '*.tar.gz' -type f | head -n 1)
[ -n "$archive" ] || { echo "error: no archive was produced; next: run scripts/build-standalone.sh by hand" >&2; exit 1; }
sha=$(sha256sum "$archive" | cut -d' ' -f1)

SLIPFORK_ARCHIVE_URL="file://$archive" SLIPFORK_SHA256="$sha" \
    sh docs/install.sh --develop local --prefix "$prefix" --yes --mirror cloudsmith >/dev/null
"$prefix/bin/slipfork" --version || { echo "error: $prefix/bin/slipfork does not run; next: inspect the install prefix" >&2; exit 1; }
[ -f "$prefix/share/man/man1/slipfork.1.gz" ] || { echo "error: the man page was not installed; next: ship man/ in the archive" >&2; exit 1; }
echo "ok   install: $prefix/bin/slipfork runs and the man page landed under share/man"
