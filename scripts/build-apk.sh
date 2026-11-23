#!/usr/bin/env bash
# Builds the Alpine `.apk` with `abuild` inside `alpine:3.21` (the host has no
# abuild). The bundle must be a dynamically linked musl build: a static musl
# binary cannot dlopen, so it is refused up front instead of shipping broken.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

version=""
pkgrel=0
bundle="target/package-bundle-musl"
arch=""
out="dist/packages"

help() {
    cat <<'USAGE'
usage: scripts/build-apk.sh --version <ver> [--pkgrel <n>] --bundle <musl bundle> [--arch x86_64|aarch64] [--out <dir>]
  --version is the Alpine pkgver: 0.2.0, or 0.2.0_alpha2026112242 for a nightly
  (Alpine rejects `~` and `+`; release-stamp.py computes the value).
USAGE
}

usage() {
    help >&2
    exit 2
}
while [ "$#" -gt 0 ]; do
    case "$1" in
        --version) [ "$#" -ge 2 ] || usage; version=$2; shift 2 ;;
        --pkgrel) [ "$#" -ge 2 ] || usage; pkgrel=$2; shift 2 ;;
        --bundle) [ "$#" -ge 2 ] || usage; bundle=$2; shift 2 ;;
        --arch) [ "$#" -ge 2 ] || usage; arch=$2; shift 2 ;;
        --out) [ "$#" -ge 2 ] || usage; out=$2; shift 2 ;;
        -h | --help) help; exit 0 ;;
        *) echo "error: unknown argument $1; next: run with --help" >&2; exit 2 ;;
    esac
done
[ -n "$version" ] || usage
[ -d "$bundle/share/slipfork/lib" ] || { echo "error: $bundle is not a bundle; next: run scripts/build-bundle.sh --out $bundle --target x86_64-unknown-linux-musl --container rust:alpine" >&2; exit 2; }
report=$(scripts/check-portable-elf.sh --max-glibc auto "$bundle"/share/slipfork/lib/*)
printf '%s\n' "$report"
printf '%s\n' "$report" | grep -q 'needed:.*libc.musl-' ||
    { echo "error: $bundle is not a dynamically linked musl build; next: build the musl bundle first (static musl cannot dlopen)" >&2; exit 2; }
case "${arch:-}" in
    "") case "$(uname -m)" in x86_64 | aarch64) arch=$(uname -m) ;; *) echo "error: unsupported host $(uname -m)" >&2; exit 2 ;; esac ;;
    x86_64 | aarch64) ;;
    *) echo "error: --arch $arch is not x86_64|aarch64" >&2; exit 2 ;;
esac
command -v docker >/dev/null 2>&1 || { echo "error: abuild needs the alpine:3.21 image and docker is missing; next: install docker" >&2; exit 2; }

tmp=$(mktemp -d "${TMPDIR:-/var/tmp}/slipfork-apk.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM
stage="$tmp/payload"
install -d "$stage/usr/bin" "$stage/usr/share/slipfork/lib" "$stage/usr/share/man/man1" \
    "$stage/etc/init.d" "$stage/etc/conf.d" "$stage/etc/sysctl.d" \
    "$stage/usr/share/bash-completion/completions" "$stage/usr/share/zsh/site-functions" \
    "$stage/usr/share/fish/vendor_completions.d" "$stage/usr/share/vim/vimfiles/ftdetect" \
    "$stage/usr/share/vim/vimfiles/syntax" "$stage/usr/share/vim/vimfiles/indent" \
    "$stage/usr/share/vim/vimfiles/ftplugin"
install -m 0755 "$bundle"/share/slipfork/lib/* "$stage/usr/share/slipfork/lib/"
install -m 0644 "$bundle/share/slipfork/LICENSE" "$stage/usr/share/slipfork/LICENSE"
for bin in slipfork slipfork-lsp slipfork-world-init; do
    ln -s "../share/slipfork/lib/$bin" "$stage/usr/bin/$bin"
done
install -m 0644 "$bundle"/share/slipfork/man/*.gz "$stage/usr/share/man/man1/"
install -m 0644 "$bundle/share/slipfork/completions/slipfork.bash" "$stage/usr/share/bash-completion/completions/slipfork"
install -m 0644 "$bundle/share/slipfork/completions/slipfork.zsh" "$stage/usr/share/zsh/site-functions/_slipfork"
install -m 0644 "$bundle/share/slipfork/completions/slipfork.fish" "$stage/usr/share/fish/vendor_completions.d/slipfork.fish"
for group in ftdetect syntax indent ftplugin; do
    install -m 0644 "$bundle/share/slipfork/vim/$group/forkfile.vim" "$stage/usr/share/vim/vimfiles/$group/forkfile.vim"
done
install -m 0755 packaging/alpine/slipfork-ingress.initd "$stage/etc/init.d/slipfork-ingress"
install -m 0644 packaging/alpine/slipfork-ingress.confd "$stage/etc/conf.d/slipfork-ingress"
printf 'net.ipv4.ip_unprivileged_port_start=80\n' >"$stage/etc/sysctl.d/99-slipfork-ingress.conf"
chmod 0644 "$stage/etc/sysctl.d/99-slipfork-ingress.conf"
work="$tmp/work"
install -d "$work/payload"
cp -a "$stage/." "$work/payload/"
tar --sort=name --mtime="@${SOURCE_DATE_EPOCH:-0}" --owner=0 --group=0 --numeric-owner \
    -czf "$work/payload.tar.gz" -C "$work" payload
sed -e "s|^pkgver=.*|pkgver=$version|" -e "s|^pkgrel=.*|pkgrel=$pkgrel|" \
    packaging/alpine/APKBUILD >"$work/APKBUILD"
# `install=` in APKBUILD names these files; abuild packages them as .post-install
# and .pre-deinstall. Alpine runs OpenRC, so the hook writes the ingress conf and
# starts `slipfork-ingress`; it does not touch systemd.
install -m 0644 packaging/alpine/slipfork.post-install \
    packaging/alpine/slipfork.pre-deinstall "$work/"
docker run --rm -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    -v "$work":/work -w /work alpine:3.21 sh -eu -c '
    trap "chown -R $HOST_UID:$HOST_GID /work || echo \"warning: could not restore /work ownership; next: sudo chown -R $HOST_UID:$HOST_GID <host work dir>\" >&2" EXIT
    apk add --no-cache abuild sudo tar build-base crun btrfs-progs slirp4netns libgcc >/dev/null
    adduser -D -h /home/builder builder
    addgroup builder abuild
    echo "builder ALL=(ALL) NOPASSWD: ALL" >/etc/sudoers.d/builder
    su builder -c "abuild-keygen -a -n >/dev/null && sudo cp /home/builder/.abuild/*.pub /etc/apk/keys/"
    chown -R builder /work
    su builder -c "cd /work && REPODEST=/work/packages abuild -r"
'
built=$(find "$work/packages" -name '*.apk' -type f | head -n 1)
[ -n "$built" ] || { echo "error: abuild produced no .apk; next: run the container by hand to read abuild output" >&2; exit 1; }
listing=$(tar -tzf "$built" 2>/dev/null || true)
for expected in \
    .PKGINFO usr/bin/slipfork usr/bin/slipfork-lsp usr/bin/slipfork-world-init \
    usr/share/slipfork/LICENSE usr/share/man/man1/slipfork.1.gz usr/share/man/man1/slipfork-lsp.1.gz \
    usr/share/bash-completion/completions/slipfork usr/share/zsh/site-functions/_slipfork \
    usr/share/fish/vendor_completions.d/slipfork.fish \
    usr/share/vim/vimfiles/ftdetect/forkfile.vim usr/share/vim/vimfiles/syntax/forkfile.vim \
    usr/share/vim/vimfiles/indent/forkfile.vim usr/share/vim/vimfiles/ftplugin/forkfile.vim \
    etc/init.d/slipfork-ingress etc/conf.d/slipfork-ingress \
    etc/sysctl.d/99-slipfork-ingress.conf; do
    printf '%s\n' "$listing" | grep -qE "(^|/)$expected\$" ||
        { echo "error: $built is missing $expected; next: fix the build-apk staging list" >&2; exit 1; }
done
tar -xzOf "$built" .PKGINFO | grep -qx 'depend = slirp4netns' ||
    { echo "error: $built does not depend on slirp4netns; next: add it to packaging/alpine/APKBUILD depends so world egress works" >&2; exit 1; }
mkdir -p "$out"
# One apk per architecture must have a unique asset name for the release.
artifact="$out/slipfork-$version-r$pkgrel-$arch.apk"
cp "$built" "$artifact"
( cd "$out" && sha256sum "$(basename "$artifact")" >"$(basename "$artifact").sha256" )
printf 'apk: %s\n' "$artifact"
