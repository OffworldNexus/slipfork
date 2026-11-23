#!/usr/bin/env bash
# Packs the portable archives `docs/install.sh` falls back to.
#
#   scripts/build-standalone.sh --bundle target/package-bundle --out dist/packages
#
# Each archive holds the three binaries at its top level plus LICENSE and a
# SHA256SUMS covering them, and a `.sha256` sidecar is written next to the
# archive so an installer can verify the download before unpacking. The triple is
# derived from the bundle's own ELF, so a musl bundle never gets a `-gnu` name.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

bundle="target/package-bundle"
out="dist/packages"
target=""

help() {
    cat <<'USAGE'
usage: scripts/build-standalone.sh --bundle <dir> --out <dir> [--target <triple>]
USAGE
}

usage() {
    help >&2
    exit 2
}
while [ "$#" -gt 0 ]; do
    case "$1" in
        --bundle) [ "$#" -ge 2 ] || usage; bundle=$2; shift 2 ;;
        --out) [ "$#" -ge 2 ] || usage; out=$2; shift 2 ;;
        --target) [ "$#" -ge 2 ] || usage; target=$2; shift 2 ;;
        -h | --help) help; exit 0 ;;
        *) echo "error: unknown argument $1; next: run with --help" >&2; exit 2 ;;
    esac
done
[ -d "$bundle/share/slipfork/lib" ] || { echo "error: $bundle is not a bundle; next: run scripts/build-bundle.sh --out $bundle" >&2; exit 2; }

libc=gnu
if readelf -d "$bundle/share/slipfork/lib/slipfork" | grep -q 'NEEDED.*libc.musl-'; then
    libc=musl
fi
if [ -z "$target" ]; then
    target=$(rustc -vV | sed -n 's/^host: //p')
    target=${target%-gnu}
    target=${target%-musl}
fi
base=${target%-gnu}
base=${base%-musl}
name="slipfork-$base-$libc"

tmp=$(mktemp -d "${TMPDIR:-/var/tmp}/slipfork-standalone.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM
install -d "$tmp/$name"
install -m 0755 "$bundle/share/slipfork/lib/slipfork" "$bundle/share/slipfork/lib/slipfork-lsp" \
    "$bundle/share/slipfork/lib/slipfork-world-init" "$tmp/$name/"
install -d "$tmp/$name/share/man/man1" "$tmp/$name/share/bash-completion/completions" "$tmp/$name/share/zsh/site-functions" "$tmp/$name/share/fish/vendor_completions.d" "$tmp/$name/share/vim/vimfiles/ftdetect" "$tmp/$name/share/vim/vimfiles/syntax" "$tmp/$name/share/vim/vimfiles/indent" "$tmp/$name/share/vim/vimfiles/ftplugin"
install -m 0644 "$bundle"/share/slipfork/man/*.gz "$tmp/$name/share/man/man1/"
install -m 0644 "$bundle/share/slipfork/completions/slipfork.bash" "$tmp/$name/share/bash-completion/completions/"
install -m 0644 "$bundle/share/slipfork/completions/slipfork.zsh" "$tmp/$name/share/zsh/site-functions/_slipfork"
install -m 0644 "$bundle/share/slipfork/completions/slipfork.fish" "$tmp/$name/share/fish/vendor_completions.d/"
for group in ftdetect syntax indent ftplugin; do
    install -m 0644 "$bundle/share/slipfork/vim/$group/forkfile.vim" "$tmp/$name/share/vim/vimfiles/$group/forkfile.vim"
done
install -m 0644 "$bundle/share/slipfork/LICENSE" "$tmp/$name/LICENSE"
( cd "$tmp/$name" && sha256sum slipfork slipfork-lsp slipfork-world-init LICENSE >SHA256SUMS )
mkdir -p "$out"
artifact="$out/$name.tar.gz"
tar --sort=name --mtime="@${SOURCE_DATE_EPOCH:-0}" --owner=0 --group=0 --numeric-owner \
    -czf "$artifact" -C "$tmp" "$name"
( cd "$out" && sha256sum "$name.tar.gz" >"$name.tar.gz.sha256" )
printf 'standalone: %s (%s/%s)\n' "$artifact" "$base" "$libc"
