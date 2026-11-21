#!/usr/bin/env bash
# Asserts a staged bundle is exactly the shape the packages and installer assume:
# three binaries behind relative symlinks, the static payload, and no strays.
set -eu
prefix=${1:-}
scripts=$(cd "$(dirname "$0")" && pwd)
help() { echo "usage: scripts/check-bundle-layout.sh <bundle-dir>"; }
usage() { help >&2; exit 2; }
case "${1:-}" in -h | --help) help; exit 0 ;; esac
[ -n "$prefix" ] && [ -d "$prefix" ] || usage
prefix=$(cd "$prefix" && pwd)
cd "$prefix"

fail() { echo "error: $1" >&2; echo "next: $2" >&2; exit 1; }

expected="slipfork slipfork-lsp slipfork-world-init"
for bin in $expected; do
    [ -L "bin/$bin" ] || fail "bin/$bin is not a symlink" "stage the bundle with scripts/build-bundle.sh"
    [ -x "share/slipfork/lib/$bin" ] || fail "share/slipfork/lib/$bin is missing or not executable" "rebuild the binaries, then the bundle"
    target=$(readlink "bin/$bin")
    [ -f "bin/$target" ] || fail "bin/$bin points at $target, which does not exist" "use a ../share/slipfork/lib/<bin> relative target"
done

actual=$(find share/slipfork/lib -mindepth 1 -maxdepth 1 -printf '%f\n' | sort | tr '\n' ' ')
[ "$actual" = "$expected " ] || fail "share/slipfork/lib holds '$actual', not exactly '$expected'" "delete strays; packages ship three binaries"

for artifact in share/slipfork/lib/*; do
    readelf -h "$artifact" >/dev/null 2>&1 || fail "$artifact is not an ELF" "rebuild it from crates/"
done
"$scripts/check-portable-elf.sh" share/slipfork/lib/* >/dev/null ||
    fail "a staged ELF is not portable" "run scripts/check-portable-elf.sh on the staged binaries"

[ -s share/slipfork/LICENSE ] || fail "share/slipfork/LICENSE is missing" "rebuild the bundle; it writes the fallback notice"
[ -f share/slipfork/systemd/slipfork.service ] || fail "the user daemon unit is missing" "copy packaging/systemd/* into the bundle"
[ -f share/slipfork/systemd/slipfork-ingress.socket ] || fail "the :80 socket unit is missing" "copy packaging/systemd/* into the bundle"
[ -f share/slipfork/systemd/slipfork-ingress@.service ] || fail "the ingress template unit is missing" "copy packaging/systemd/* into the bundle"
[ -f share/slipfork/apparmor/slipfork ] || fail "the AppArmor profile is missing" "copy packaging/apparmor/slipfork into the bundle"
[ -f share/slipfork/man/slipfork.1.gz ] || fail "the slipfork man page is missing" "gzip packaging/man/*.1 into the bundle"
[ -f share/slipfork/man/slipfork-lsp.1.gz ] || fail "the slipfork-lsp man page is missing" "gzip packaging/man/*.1 into the bundle"
for shell in bash zsh fish; do
    [ -s "share/slipfork/completions/slipfork.$shell" ] ||
        fail "the $shell completion is empty or missing" "generate it with slipfork completions $shell"
done
[ -s SHA256SUMS ] || fail "SHA256SUMS is missing" "rebuild the bundle; it checksums the three binaries"
echo "ok   bundle $prefix: three binaries, payload, completions all present"
