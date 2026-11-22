#!/usr/bin/env bash
# Assembles the pacman package host-natively: a staged tree plus a valid
# `.PKGINFO`, tarred with sorted names and a fixed mtime, then zstd. `makepkg`
# is not required (and is absent on most CI runners).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

version=""
release="1"
bundle="target/package-bundle"
arch=""
max_glibc="auto"
out="dist/packages"

help() {
    cat <<'USAGE'
usage: scripts/build-arch.sh --version <ver> [--release <n>] [--bundle <dir>]
                             [--arch x86_64|aarch64] [--max-glibc <m_n>|auto] [--out <dir>]
USAGE
}

usage() {
    help >&2
    exit 2
}
while [ "$#" -gt 0 ]; do
    case "$1" in
        --version) [ "$#" -ge 2 ] || usage; version=$2; shift 2 ;;
        --release) [ "$#" -ge 2 ] || usage; release=$2; shift 2 ;;
        --bundle) [ "$#" -ge 2 ] || usage; bundle=$2; shift 2 ;;
        --arch) [ "$#" -ge 2 ] || usage; arch=$2; shift 2 ;;
        --max-glibc) [ "$#" -ge 2 ] || usage; max_glibc=$2; shift 2 ;;
        --out) [ "$#" -ge 2 ] || usage; out=$2; shift 2 ;;
        -h | --help) help; exit 0 ;;
        *) echo "error: unknown argument $1; next: run with --help" >&2; exit 2 ;;
    esac
done
[ -n "$version" ] || usage
[ -d "$bundle/share/slipfork/lib" ] || { echo "error: $bundle is not a bundle; next: run scripts/build-bundle.sh" >&2; exit 2; }
case "${arch:-}" in
    "") case "$(uname -m)" in x86_64 | aarch64) arch=$(uname -m) ;; *) echo "error: unsupported host $(uname -m)" >&2; exit 2 ;; esac ;;
    x86_64 | aarch64) ;;
    *) echo "error: --arch $arch is not x86_64|aarch64" >&2; exit 2 ;;
esac
command -v zstd >/dev/null 2>&1 || { echo "error: zstd is missing; next: install zstd" >&2; exit 2; }
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0}
scripts/check-portable-elf.sh --max-glibc "$max_glibc" "$bundle"/share/slipfork/lib/* >/dev/null

tmp=$(mktemp -d "${TMPDIR:-/var/tmp}/slipfork-arch.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM
stage="$tmp/pkg"
install -d "$stage/usr/bin" "$stage/usr/share/slipfork/lib" "$stage/usr/share/man/man1" \
    "$stage/usr/lib/systemd/system" "$stage/usr/lib/systemd/user" "$stage/etc/apparmor.d" \
    "$stage/usr/share/bash-completion/completions" "$stage/usr/share/zsh/site-functions" \
    "$stage/usr/share/fish/vendor_completions.d"
install -m 0755 "$bundle"/share/slipfork/lib/* "$stage/usr/share/slipfork/lib/"
install -m 0644 "$bundle/share/slipfork/LICENSE" "$stage/usr/share/slipfork/LICENSE"
for bin in slipfork slipfork-lsp slipfork-world-init; do
    ln -s "../share/slipfork/lib/$bin" "$stage/usr/bin/$bin"
done
install -m 0644 "$bundle"/share/slipfork/systemd/slipfork-ingress* "$stage/usr/lib/systemd/system/"
install -m 0644 "$bundle/share/slipfork/systemd/slipfork.service" "$stage/usr/lib/systemd/user/"
install -m 0644 "$bundle/share/slipfork/apparmor/slipfork" "$stage/etc/apparmor.d/slipfork"
install -m 0644 "$bundle"/share/slipfork/man/*.gz "$stage/usr/share/man/man1/"
install -m 0644 "$bundle/share/slipfork/completions/slipfork.bash" "$stage/usr/share/bash-completion/completions/slipfork"
install -m 0644 "$bundle/share/slipfork/completions/slipfork.zsh" "$stage/usr/share/zsh/site-functions/_slipfork"
install -m 0644 "$bundle/share/slipfork/completions/slipfork.fish" "$stage/usr/share/fish/vendor_completions.d/slipfork.fish"

size=$(du -sb "$stage" | cut -f1)
cat >"$stage/.PKGINFO" <<PKGINFO
pkgname = slipfork
pkgbase = slipfork
pkgver = $version-$release
pkgdesc = Copy-on-write Btrfs development environments, Docker-free
url = https://github.com/OffworldNexus/slipfork
builddate = $SOURCE_DATE_EPOCH
packager = Offworld Nexus <hello@offworld.nexus>
size = $size
arch = $arch
license = custom
depend = glibc
depend = crun
depend = btrfs-progs
PKGINFO

mkdir -p "$out"
artifact="$out/slipfork-$version-$release-$arch.pkg.tar.zst"
tar --sort=name --mtime="@$SOURCE_DATE_EPOCH" --owner=0 --group=0 --numeric-owner \
    -cf - -C "$stage" .PKGINFO usr etc | zstd -q -19 -T0 -f -o "$artifact"
listing=$(tar --zstd -tf "$artifact")
for expected in \
    .PKGINFO usr/bin/slipfork usr/bin/slipfork-lsp usr/bin/slipfork-world-init \
    usr/lib/systemd/system/slipfork-ingress.socket usr/lib/systemd/system/slipfork-ingress@.service \
    usr/lib/systemd/user/slipfork.service etc/apparmor.d/slipfork \
    usr/share/man/man1/slipfork.1.gz usr/share/man/man1/slipfork-lsp.1.gz \
    usr/share/bash-completion/completions/slipfork usr/share/zsh/site-functions/_slipfork \
    usr/share/fish/vendor_completions.d/slipfork.fish; do
    printf '%s\n' "$listing" | grep -qE "(^|/)$expected\$" ||
        { echo "error: $artifact is missing $expected; next: fix the build-arch staging list" >&2; exit 1; }
done
( cd "$out" && sha256sum "$(basename "$artifact")" >"$(basename "$artifact").sha256" )
printf 'arch: %s\n' "$artifact"
