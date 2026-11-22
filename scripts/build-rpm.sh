#!/usr/bin/env bash
# Builds the .rpm with host `rpmbuild`, or inside `rockylinux:9` when the host has
# none. The glibc floor comes from the measured ELF report, never from a constant.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

version=""
release="1"
bundle="target/package-bundle"
arch=""
max_glibc="auto"
out="dist/packages"
in_container=0

help() {
    cat <<'USAGE'
usage: scripts/build-rpm.sh --version <ver> [--release <n>] [--bundle <dir>]
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
        --in-container) in_container=1; shift ;;
        -h | --help) help; exit 0 ;;
        *) echo "error: unknown argument $1; next: run with --help" >&2; exit 2 ;;
    esac
done
[ -n "$version" ] || usage
[ -d "$bundle/share/slipfork/lib" ] || { echo "error: $bundle is not a bundle; next: run scripts/build-bundle.sh --out $bundle" >&2; exit 2; }
case "${arch:-}" in
    "") case "$(uname -m)" in x86_64 | aarch64) arch=$(uname -m) ;; *) echo "error: unsupported host $(uname -m); next: pass --arch x86_64|aarch64" >&2; exit 2 ;; esac ;;
    x86_64 | aarch64) ;;
    *) echo "error: --arch $arch is not x86_64|aarch64" >&2; exit 2 ;;
esac

if ! command -v rpmbuild >/dev/null 2>&1 && [ "$in_container" -eq 0 ]; then
    command -v docker >/dev/null 2>&1 ||
        { echo "error: rpmbuild and docker are both missing; next: install rpm-build or docker" >&2; exit 2; }
    mkdir -p "$out"
    out_abs=$(cd "$out" && pwd)
    exec docker run --rm -e SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-0}" \
        -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
        -v "$root":/src -v "$(cd "$bundle" && pwd)":/bundle:ro -v "$out_abs":/out -w /src rockylinux:9 \
        bash -c 'set -eu; trap "chown -R $HOST_UID:$HOST_GID /out || echo \"warning: could not restore /out ownership; next: sudo chown -R $HOST_UID:$HOST_GID <host out dir>\" >&2; chown -R $HOST_UID:$HOST_GID /src/target || echo \"warning: could not restore /src/target ownership; next: sudo chown -R $HOST_UID:$HOST_GID <host target dir>\" >&2" EXIT; dnf -y -q install rpm-build tar gzip >/dev/null; /src/scripts/build-rpm.sh --in-container --bundle /bundle --out /out "$@"' \
        _ --version "$version" --release "$release" --arch "$arch" --max-glibc "$max_glibc"
fi

export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0}
report=$(scripts/check-portable-elf.sh --max-glibc "$max_glibc" "$bundle"/share/slipfork/lib/*)
printf '%s\n' "$report"
glibc=$(printf '%s\n' "$report" | sed -n 's/^max-glibc: \([0-9][0-9.]*\) (.*/\1/p' | head -n 1)
[ -n "$glibc" ] || { echo "error: check-portable-elf.sh measured no glibc floor for a gnu rpm" >&2;
    echo "next: rebuild the bundle on a glibc host, then re-run build-rpm.sh" >&2; exit 1; }

tmp=$(mktemp -d "${TMPDIR:-/var/tmp}/slipfork-rpm.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM
stage="$tmp/payload"
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
tar --sort=name --mtime="@$SOURCE_DATE_EPOCH" --owner=0 --group=0 --numeric-owner \
    -czf "$tmp/payload.tar.gz" -C "$stage" .

top="$tmp/rpmbuild"
install -d "$top/BUILD" "$top/BUILDROOT" "$top/RPMS" "$top/SOURCES" "$top/SPECS" "$top/SRPMS"
cp "$tmp/payload.tar.gz" "$top/SOURCES/"
cp packaging/rpm/slipfork.spec "$top/SPECS/"
if ! rpmbuild -bb --define "_topdir $top" --define "slipfork_version $version" \
    --define "slipfork_release $release" --define "slipfork_arch $arch" \
    --define "slipfork_glibc $glibc" "$top/SPECS/slipfork.spec" >"$tmp/rpm.log" 2>&1; then
    tail -n 25 "$tmp/rpm.log" >&2
    echo "error: rpmbuild failed; next: read the log above, then fix packaging/rpm/slipfork.spec" >&2
    exit 1
fi
built=$(find "$top/RPMS" -name '*.rpm' -type f | head -n 1)
[ -n "$built" ] || { echo "error: rpmbuild produced no .rpm; next: run rpmbuild -bb by hand to see why" >&2; exit 1; }
mkdir -p "$out"
listing=$(rpm -qpl "$built")
for expected in \
    /usr/bin/slipfork /usr/bin/slipfork-lsp /usr/bin/slipfork-world-init \
    /usr/lib/systemd/system/slipfork-ingress.socket /usr/lib/systemd/system/slipfork-ingress@.service \
    /usr/lib/systemd/user/slipfork.service /etc/apparmor.d/slipfork \
    /usr/share/man/man1/slipfork.1.gz /usr/share/man/man1/slipfork-lsp.1.gz \
    /usr/share/bash-completion/completions/slipfork /usr/share/zsh/site-functions/_slipfork \
    /usr/share/fish/vendor_completions.d/slipfork.fish; do
    printf '%s\n' "$listing" | grep -qx "$expected" ||
        { echo "error: the rpm is missing $expected; next: fix the %files list in packaging/rpm/slipfork.spec" >&2; exit 1; }
done
cp "$built" "$out/"
printf 'rpm: %s\n' "$out/$(basename "$built")"
