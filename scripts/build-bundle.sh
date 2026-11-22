#!/usr/bin/env bash
# Builds the one bundle every package is assembled from.
#
#   scripts/build-bundle.sh --out target/package-bundle \
#       --max-glibc 2_34 --container rockylinux:9 \
#       --target-dir target/package-gnu-target --cargo-home target/package-cargo-home
#   scripts/build-bundle.sh --out target/package-bundle-musl \
#       --target x86_64-unknown-linux-musl --container rust:alpine
#
# The bundle is `bin/` symlinks into `share/slipfork/lib/` plus the static
# payload the packages install (LICENSE, systemd units, AppArmor, man pages,
# completions). Completions are generated from the freshly built binary, so a
# verb can never ship in `--help` and go missing from bash completion.
# Every ELF is measured with `check-portable-elf.sh` before the bundle exists.
#
# One gnu bundle, compiled on glibc 2.34, so Rocky 9 installs it: the gnu bundle
# must be linked against 2.34, so callers pass
# `--container rockylinux:9 --max-glibc 2_34` when their host is newer (the
# `packages` targets in the Makefile do that automatically).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

out=""
target=""
max_glibc="auto"
container=""
cargo_home=""
target_dir=""
in_container=0

help() {
    cat <<'USAGE'
usage: scripts/build-bundle.sh --out <dir> [--target <triple>]
                               [--max-glibc <major>_<minor>|auto] [--container <image>]
                               [--target-dir <dir>] [--cargo-home <dir>]
  --out         where bin/ and share/slipfork/ are staged
  --target      default: this host's rustc triple; musl targets link dynamically
  --max-glibc   measured ceiling enforced by check-portable-elf.sh
  --container   build inside this image (the host lacks a musl toolchain, or its
                glibc is newer than the floor the packages promise)
  --target-dir  host cargo target dir, bind-mounted at /target inside the image,
                so a container build stays out of ./target and reuses its cache
  --cargo-home  host cargo home, bind-mounted at /cargo inside the image
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
        --target-dir) [ "$#" -ge 2 ] && [ -n "$2" ] || usage; target_dir=$2; shift 2 ;;
        --cargo-home) [ "$#" -ge 2 ] && [ -n "$2" ] || usage; cargo_home=$2; shift 2 ;;
        --in-container) in_container=1; shift ;;
        -h | --help) help; exit 0 ;;
        *) echo "error: unknown argument $1; next: run with --help" >&2; exit 2 ;;
    esac
done
[ -n "$out" ] || usage
[ -n "$target" ] || target=$(rustc -vV | sed -n 's/^host: //p')

# In-container toolchain bootstrap, by image family. Runs as its own function so
# `if ! setup_container ...` can turn any failure into one remedy line; its output
# streams live because a silent 1-2 minute setup is the worst part of a first build.
setup_container() {
    case "$1" in
        *alpine*) apk add --no-cache bash binutils musl-dev gcc ;;
        *rocky* | *almalinux* | *centos*)
            # rockylinux:9 already ships `curl-minimal`, so asking dnf for `curl`
            # conflicts and fails; rustup's installer uses what is there.
            dnf -y install gcc binutils gzip tar findutils &&
                curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
                sh -s -- -y --default-toolchain stable --profile minimal &&
                export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH" &&
                cargo --version >/dev/null
            ;;
        *)
            echo "error: no container bootstrap for $1; next: add it to the case in scripts/build-bundle.sh" >&2
            return 2
            ;;
    esac
}

if [ -n "$container" ] && [ "$in_container" -eq 0 ]; then
    command -v docker >/dev/null 2>&1 ||
        { echo "error: $target needs a container build but docker is missing; next: install docker" >&2; exit 2; }
    mkdir -p "$out"
    out_abs=$(cd "$out" && pwd)
    envs=(-e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" -e "SLIPFORK_CONTAINER_IMAGE=$container")
    mounts=(-v "$root":/src -v "$out_abs":/out)
    # `<container path>=<host path>` for every directory the container writes as
    # root; the in-container EXIT trap hands each one back to the invoking user.
    # `packaging/man` is regenerated in place from the freshly built binary.
    chown_dirs="/out=$out_abs /src/packaging/man=$root/packaging/man"
    if [ -n "$target_dir" ]; then
        mkdir -p "$target_dir"
        target_abs=$(cd "$target_dir" && pwd)
        mounts+=(-v "$target_abs":/target)
        envs+=(-e CARGO_TARGET_DIR=/target)
        chown_dirs="/target=$target_abs $chown_dirs"
    else
        envs+=(-e CARGO_TARGET_DIR=/src/target)
        chown_dirs="/src/target=$root/target $chown_dirs"
    fi
    if [ -n "$cargo_home" ]; then
        mkdir -p "$cargo_home"
        cargo_home_abs=$(cd "$cargo_home" && pwd)
        mounts+=(-v "$cargo_home_abs":/cargo)
        envs+=(-e CARGO_HOME=/cargo)
        chown_dirs="/cargo=$cargo_home_abs $chown_dirs"
    fi
    envs+=(-e "SLIPFORK_CHOWN_DIRS=$chown_dirs")
    # `rust:alpine` ships no bash, and the script is bash: install one first, then
    # exec so the in-container `EXIT` trap restores ownership of every bind mount.
    exec docker run --rm "${envs[@]}" "${mounts[@]}" -w /src "$container" \
        sh -c 'command -v bash >/dev/null || apk add --no-cache bash >/dev/null; exec bash /src/scripts/build-bundle.sh "$@"' \
        _ --in-container --out /out --target "$target" --max-glibc "$max_glibc"
fi

if [ "$in_container" -eq 1 ]; then
    image="${SLIPFORK_CONTAINER_IMAGE:-container}"
    # Restore ownership even when the build fails: a root-owned host cache would
    # break the next `make packages` with a permission error. Each `SLIPFORK_CHOWN_DIRS`
    # entry is `<container path>=<host path>`, so the remedy is copy-pasteable.
    if [ -n "${HOST_UID:-}" ] && [ -n "${HOST_GID:-}" ]; then
        trap 'rc=$?; for pair in ${SLIPFORK_CHOWN_DIRS:-}; do chown -R "$HOST_UID:$HOST_GID" "${pair%%=*}" 2>/dev/null ||
            echo "warning: could not restore ${pair#*=} ownership; next: sudo chown -R $HOST_UID:$HOST_GID ${pair#*=}" >&2; done; exit $rc' EXIT
    fi
    printf '[setup] %s toolchain\n' "$image"
    if ! setup_container "$image"; then
        echo "error: toolchain setup failed in $image; next: docker run --rm -it $image bash" >&2
        exit 2
    fi
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

bindir="${CARGO_TARGET_DIR:-target}/$target/release"
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
