#!/usr/bin/env bash
# Assembles a .deb from a staged bundle. The dependency floor is *measured* from
# the shipped ELF (`check-portable-elf.sh --max-glibc`), never typed by hand, and
# the whole build is stampable with SOURCE_DATE_EPOCH.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

version=""
bundle="target/package-bundle"
arch=""
max_glibc="auto"
out="dist/packages"

help() {
    cat <<'USAGE'
usage: scripts/build-deb.sh --version <ver> [--bundle <dir>] [--arch amd64|arm64]
                            [--max-glibc <major>_<minor>|auto] [--out <dir>]
  --version may be a release (0.2.0) or a nightly stamp
  (0.2.0~nightly20261122.r42+g90cb2db); both are legal dpkg versions.
USAGE
}

usage() {
    help >&2
    exit 2
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version) [ "$#" -ge 2 ] || usage; version=$2; shift 2 ;;
        --bundle) [ "$#" -ge 2 ] || usage; bundle=$2; shift 2 ;;
        --arch) [ "$#" -ge 2 ] || usage; arch=$2; shift 2 ;;
        --max-glibc) [ "$#" -ge 2 ] || usage; max_glibc=$2; shift 2 ;;
        --out) [ "$#" -ge 2 ] || usage; out=$2; shift 2 ;;
        -h | --help) help; exit 0 ;;
        *) echo "error: unknown argument $1; next: run with --help" >&2; exit 2 ;;
    esac
done
[ -n "$version" ] || usage
[ -d "$bundle/share/slipfork/lib" ] || { echo "error: $bundle is not a bundle; next: run scripts/build-bundle.sh --out $bundle" >&2; exit 2; }
case "${arch:-}" in
    "") case "$(uname -m)" in x86_64) arch=amd64 ;; aarch64) arch=arm64 ;; *) echo "error: unsupported host $(uname -m); next: pass --arch amd64|arm64" >&2; exit 2 ;; esac ;;
    amd64 | arm64) ;;
    *) echo "error: --arch $arch is not amd64|arm64; next: pass a real Debian architecture" >&2; exit 2 ;;
esac
command -v dpkg-deb >/dev/null 2>&1 || { echo "error: dpkg-deb is missing; next: install dpkg-dev" >&2; exit 2; }
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0}

report=$(scripts/check-portable-elf.sh --max-glibc "$max_glibc" "$bundle"/share/slipfork/lib/*)
printf '%s\n' "$report"
glibc=$(printf '%s\n' "$report" | sed -n 's/^max-glibc: \([0-9][0-9.]*\) (.*/\1/p' | head -n 1)
needed=$(printf '%s\n' "$report" | sed -n 's/^needed: //p' | head -n 1)
[ -n "$glibc" ] || { echo "error: check-portable-elf.sh measured no glibc floor for a gnu .deb" >&2;
    echo "next: rebuild the bundle on a glibc host, then re-run build-deb.sh" >&2; exit 1; }
deps="libc6 (>= $glibc)"
case " $needed " in *" libgcc_s.so.1 "*) deps="${deps:+$deps, }libgcc-s1" ;; esac
deps="${deps:+$deps, }crun, btrfs-progs"

tmp=$(mktemp -d "${TMPDIR:-/var/tmp}/slipfork-deb.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM
stage="$tmp/root"
install -d "$stage/usr/bin" "$stage/usr/share/slipfork/lib" "$stage/usr/share/man/man1" \
    "$stage/usr/lib/systemd/system" "$stage/usr/lib/systemd/user" "$stage/etc/apparmor.d" \
    "$stage/usr/share/bash-completion/completions" "$stage/usr/share/zsh/site-functions" \
    "$stage/usr/share/fish/vendor_completions.d" "$stage/DEBIAN"
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
sed -e "s|@VERSION@|$version|g" -e "s|@ARCH@|$arch|g" -e "s|@DEPENDS@|$deps|g" \
    packaging/deb/control >"$stage/DEBIAN/control"
install -m 0755 packaging/deb/postinst packaging/deb/prerm "$stage/DEBIAN/"

mkdir -p "$out"
artifact="$out/slipfork_${version}_${arch}.deb"
dpkg-deb --root-owner-group -Zxz --build "$stage" "$artifact" >/dev/null
contents=$(dpkg-deb -c "$artifact")
for expected in \
    usr/bin/slipfork usr/bin/slipfork-lsp usr/bin/slipfork-world-init \
    usr/lib/systemd/system/slipfork-ingress.socket usr/lib/systemd/system/slipfork-ingress@.service \
    usr/lib/systemd/user/slipfork.service etc/apparmor.d/slipfork \
    usr/share/man/man1/slipfork.1.gz usr/share/man/man1/slipfork-lsp.1.gz \
    usr/share/bash-completion/completions/slipfork usr/share/zsh/site-functions/_slipfork \
    usr/share/fish/vendor_completions.d/slipfork.fish; do
    printf '%s\n' "$contents" | grep -q "$expected" ||
        { echo "error: $artifact is missing $expected; next: fix the build-deb staging list" >&2; exit 1; }
done
printf 'deb: %s\n' "$artifact"
printf 'deb-deps: %s\n' "$deps"
if command -v lintian >/dev/null 2>&1; then
    lintian --no-tag-display-limit "$artifact" || { echo "error: lintian reported defects in $artifact" >&2; exit 1; }
fi
