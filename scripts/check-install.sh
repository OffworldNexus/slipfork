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
[ -f "$bundle/share/slipfork/vim/syntax/forkfile.vim" ] || { echo "error: $bundle is missing the Vim plugin; next: rebuild it with scripts/build-bundle.sh" >&2; exit 2; }
tmp=$(mktemp -d "${TMPDIR:-/var/tmp}/slipfork-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM
[ -n "$prefix" ] || prefix="$tmp/prefix"
scripts/build-standalone.sh --bundle "$bundle" --out "$tmp/archive" >/dev/null
archive=$(find "$tmp/archive" -name '*.tar.gz' -type f | head -n 1)
[ -n "$archive" ] || { echo "error: no archive was produced; next: run scripts/build-standalone.sh by hand" >&2; exit 1; }
sha=$(sha256sum "$archive" | cut -d' ' -f1)

mkdir -p "$tmp/home/.config"
SLIPFORK_ARCHIVE_URL="file://$archive" SLIPFORK_SHA256="$sha" \
    HOME="$tmp/home" XDG_CONFIG_HOME="$tmp/home/.config" SLIPFORK_INSTALL_NO_SERVICE=1 \
    sh docs/install.sh --develop local --prefix "$prefix" --yes >/dev/null
[ ! -e "$tmp/home/.config/systemd/user/slipfork.service" ] ||
    { echo "error: SLIPFORK_INSTALL_NO_SERVICE wrote a user unit; next: honor the opt-out in docs/install.sh" >&2; exit 1; }
"$prefix/bin/slipfork" --version || { echo "error: $prefix/bin/slipfork does not run; next: inspect the install prefix" >&2; exit 1; }
[ -f "$prefix/share/man/man1/slipfork.1.gz" ] || { echo "error: the man page was not installed; next: ship man/ in the archive" >&2; exit 1; }
[ -f "$prefix/share/vim/vimfiles/syntax/forkfile.vim" ] || { echo "error: the Vim syntax file was not installed; next: ship share/vim/vimfiles in the archive" >&2; exit 1; }
sh -n packaging/deb/postinst
sh -n packaging/deb/prerm
bash -n packaging/arch/slipfork.install
post=$(sed -n '/^%post$/,/^%preun$/p' packaging/rpm/slipfork.spec)
printf '%s\n' "$post" | grep -qx '%preun' ||
    { echo "error: rpm spec lost its %post/%preun markers; next: restore them or update check-install.sh" >&2; exit 1; }
printf '%s\n' "$post" | sed '1d;$d' | sh -n
grep -q '^post_upgrade()' packaging/arch/slipfork.install ||
    { echo "error: arch hook lost post_upgrade; next: make pacman upgrades run the ingress setup" >&2; exit 1; }
for hook in packaging/deb/postinst packaging/rpm/slipfork.spec packaging/arch/slipfork.install; do
    grep -q '^[[:space:]]*ingress_setup$' "$hook" ||
        { echo "error: $hook lost the ingress_setup call; next: restore it" >&2; exit 1; }
done
sh -n packaging/alpine/slipfork.post-install
sh -n packaging/alpine/slipfork.pre-deinstall
sh -n docs/install.sh
echo "ok   install: $prefix/bin/slipfork runs, and the man page + Vim plugin landed under share"
echo "ok   hooks: every package install hook parses, and the service opt-out wrote no unit"
