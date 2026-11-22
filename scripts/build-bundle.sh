#!/usr/bin/env bash
# Builds the one bundle every package is assembled from.
#
#   scripts/build-bundle.sh --out target/package-bundle
#   scripts/build-bundle.sh --out target/package-bundle-musl \
#       --target x86_64-unknown-linux-musl --container rust:alpine
#
# The bundle is `bin/` symlinks into `share/slipfork/lib/` plus the static
# payload the packages install (LICENSE, systemd units, AppArmor, man pages,
# completions). Completions are generated from the freshly built binary, so a
# verb can never ship in `--help` and go missing from bash completion.
# Every ELF is measured with `check-portable-elf.sh` before the bundle exists.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

out=""
target=""
max_glibc="auto"
container=""
in_container=0

help() {
    cat <<'USAGE'
usage: scripts/build-bundle.sh --out <dir> [--target <triple>]
                               [--max-glibc <major>_<minor>|auto] [--container <image>]
  --out         where bin/ and share/slipfork/ are staged
  --target      default: this host's rustc triple; musl targets link dynamically
  --max-glibc   measured ceiling enforced by check-portable-elf.sh
  --container   build inside this image (the host lacks a musl toolchain)
USAGE
}

usage() {
    help >&2
    exit 2
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --out) [ "$#" -ge 2 ] && [ -n "$2" ] || usage; out=$2; shift 2 ;;
        --target) [ "$#" -ge 2 ] && [ -n "$2" ] || usage; target=$2; shift 2 ;;
        --max-glibc) [ "$#" -ge 2 ] && [ -n "$2" ] || usage; max_glibc=$2; shift 2 ;;
        --container) [ "$#" -ge 2 ] && [ -n "$2" ] || usage; container=$2; shift 2 ;;
        --in-container) in_container=1; shift ;;
        -h | --help) help; exit 0 ;;
        *) echo "error: unknown argument $1; next: run with --help" >&2; exit 2 ;;
    esac
done
[ -n "$out" ] || usage
[ -n "$target" ] || target=$(rustc -vV | sed -n 's/^host: //p')

if [ -n "$container" ] && [ "$in_container" -eq 0 ]; then
    command -v docker >/dev/null 2>&1 ||
        { echo "error: $target needs a container build but docker is missing; next: install docker" >&2; exit 2; }
    mkdir -p "$out"
    out_abs=$(cd "$out" && pwd)
    uid=$(id -u)
    gid=$(id -g)
    exec docker run --rm -e CARGO_TARGET_DIR=/src/target \
        -v "$root":/src -v "$out_abs":/out -w /src "$container" sh -c "
            apk add --no-cache bash binutils musl-dev gcc >/dev/null &&
            /src/scripts/build-bundle.sh --in-container --out /out --target $target --max-glibc $max_glibc &&
            chown -R $uid:$gid /src/target /out"
fi

if [ "$in_container" -eq 1 ] && command -v apk >/dev/null 2>&1; then
    apk add --no-cache musl-dev gcc >/dev/null 2>&1 || true
fi

rustflags=""
case "$target" in
    *-musl*) rustflags="-C target-feature=-crt-static" ;;
esac
export RUSTFLAGS="$rustflags${RUSTFLAGS:+ $RUSTFLAGS}"
for pair in slipfork:slipfork slipfork-lsp:slipfork-lsp slipfork-world:slipfork-world-init; do
    pkg=${pair%%:*}
    bin=${pair##*:}
    printf '[build] %s (%s)\n' "$bin" "$target"
    cargo build --release --locked --target "$target" -p "$pkg" --bin "$bin"
done

bindir="target/$target/release"
for bin in slipfork slipfork-lsp slipfork-world-init; do
    [ -x "$bindir/$bin" ] || { echo "error: $bindir/$bin was not built; next: run cargo build -p $bin" >&2; exit 1; }
done

case "$out" in
    "" | "/") echo "error: refusing to clean '$out'; next: pass --out <dir>" >&2; exit 2 ;;
esac
# `$out` may be a bind mount inside a container, so clean its contents instead of
# removing the directory itself (`rm -rf /out` is EBUSY).
install -d "$out"
find "$out" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
install -d "$out/bin" "$out/share/slipfork/lib" "$out/share/slipfork/systemd" \
    "$out/share/slipfork/apparmor" "$out/share/slipfork/man" "$out/share/slipfork/completions"
install -m 0755 "$bindir/slipfork" "$bindir/slipfork-lsp" "$bindir/slipfork-world-init" "$out/share/slipfork/lib/"
for bin in slipfork slipfork-lsp slipfork-world-init; do
    ln -s "../share/slipfork/lib/$bin" "$out/bin/$bin"
done

if [ -f LICENSE ]; then
    install -m 0644 LICENSE "$out/share/slipfork/LICENSE"
else
    printf 'Slipfork is distributed under the terms published with the source at\nhttps://github.com/OffworldNexus/slipfork\n' >"$out/share/slipfork/LICENSE"
fi
install -m 0644 packaging/systemd/slipfork.service packaging/systemd/slipfork-ingress.socket \
    packaging/systemd/slipfork-ingress@.service "$out/share/slipfork/systemd/"
install -m 0644 packaging/apparmor/slipfork "$out/share/slipfork/apparmor/slipfork"
# Regenerate slipfork(1) from the freshly built clap tree so the shipped page cannot drift.
"$bindir/slipfork" --manpage >packaging/man/slipfork.1
for page in packaging/man/*.1; do
    gzip -9 -n -c "$page" >"$out/share/slipfork/man/$(basename "$page").gz"
done
"$bindir/slipfork" completions bash >"$out/share/slipfork/completions/slipfork.bash"
"$bindir/slipfork" completions zsh >"$out/share/slipfork/completions/slipfork.zsh"
"$bindir/slipfork" completions fish >"$out/share/slipfork/completions/slipfork.fish"

report=$(scripts/check-portable-elf.sh --max-glibc "$max_glibc" \
    "$out/share/slipfork/lib/slipfork" "$out/share/slipfork/lib/slipfork-lsp" \
    "$out/share/slipfork/lib/slipfork-world-init")
printf '%s\n' "$report"
case "$target" in
    *-musl*)
        printf '%s\n' "$report" | grep -q 'needed:.*libc.musl-' ||
            { echo "error: $target produced a bundle with no musl libc; next: build in an Alpine image (--container rust:alpine)" >&2; exit 1; } ;;
esac

out_abs=$(cd "$out" && pwd)
( cd "$out/share/slipfork/lib" && sha256sum slipfork slipfork-lsp slipfork-world-init >"$out_abs/SHA256SUMS" )
printf 'bundle: %s (%s)\n' "$out" "$target"
