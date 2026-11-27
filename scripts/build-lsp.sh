#!/usr/bin/env bash
# Builds every `slipfork-lsp` asset whose target toolchain exists on this host,
# with the exact names the Zed extension requests (`editors/zed/src/lib.rs`
# `asset_for`). A cross target without a linker/SDK is skipped with one line,
# never faked.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

out="dist/lsp"
help() { echo "usage: scripts/build-lsp.sh [--out <dir>]"; }

usage() {
    help >&2
    exit 2
}
while [ "$#" -gt 0 ]; do
    case "$1" in
        --out) [ "$#" -ge 2 ] || usage; out=$2; shift 2 ;;
        -h | --help) help; exit 0 ;;
        *) echo "error: unknown argument $1; next: run with --help" >&2; exit 2 ;;
    esac
done
mkdir -p "$out"

host=$(rustc -vV | sed -n 's/^host: //p')
installed=$(rustup target list --installed 2>/dev/null || true)

license="$root/LICENSE"
if [ ! -f "$license" ]; then
    license=$(mktemp "${TMPDIR:-/var/tmp}/slipfork-license.XXXXXX")
    printf 'Slipfork is distributed under the terms published with the source at\nhttps://github.com/OffworldNexus/slipfork\n' >"$license"
fi

built=0
for entry in \
    "linux:x86_64:x86_64-unknown-linux-gnu:tar.gz" \
    "linux:aarch64:aarch64-unknown-linux-gnu:tar.gz" \
    "darwin:x86_64:x86_64-apple-darwin:tar.gz" \
    "darwin:aarch64:aarch64-apple-darwin:tar.gz" \
    "windows:x86_64:x86_64-pc-windows-gnu:zip" \
    "windows:aarch64:aarch64-pc-windows-gnullvm:zip"
do
    platform=${entry%%:*}; rest=${entry#*:}
    arch=${rest%%:*}; rest=${rest#*:}
    target=${rest%%:*}; kind=${rest##*:}
    if ! printf '%s\n' "$installed" | grep -qx "$target"; then
        echo "skip: slipfork-lsp $platform/$arch ($target toolchain is not installed; next: rustup target add $target on a $platform runner)"
        continue
    fi
    if [ "$kind" = zip ] && ! command -v zip >/dev/null 2>&1; then
        echo "skip: slipfork-lsp $platform/$arch (zip is missing; next: install zip)"
        continue
    fi
    printf '[build] slipfork-lsp %s/%s\n' "$platform" "$arch"
    if ! cargo build --release --locked -p slipfork-lsp --bin slipfork-lsp --target "$target"; then
        echo "skip: slipfork-lsp $platform/$arch ($target has no usable linker on this host)"
        continue
    fi
    name="slipfork-lsp-$platform-$arch"
    stage=$(mktemp -d "${TMPDIR:-/var/tmp}/slipfork-lsp.XXXXXX")
    install -d "$stage/$name"
    bindir="${CARGO_TARGET_DIR:-target}/$target/release"
    if [ "$kind" = zip ]; then
        install -m 0755 "$bindir/slipfork-lsp.exe" "$stage/$name/slipfork-lsp.exe"
    else
        install -m 0755 "$bindir/slipfork-lsp" "$stage/$name/slipfork-lsp"
    fi
    install -m 0644 "$license" "$stage/$name/LICENSE"
    if [ "$kind" = zip ]; then
        ( cd "$stage" && zip -qr "$root/$out/$name.zip" "$name" )
        printf 'lsp: %s\n' "$out/$name.zip"
    else
        tar --sort=name --mtime="@${SOURCE_DATE_EPOCH:-0}" --owner=0 --group=0 --numeric-owner \
            -czf "$out/$name.tar.gz" -C "$stage" "$name"
        printf 'lsp: %s\n' "$out/$name.tar.gz"
    fi
    rm -rf "$stage"
    built=$((built + 1))
done
[ "$built" -gt 0 ] || { echo "error: no slipfork-lsp target could be built on $host; next: run on a runner whose target is installed, or rustup target add <triple>" >&2; exit 1; }
