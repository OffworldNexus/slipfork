#!/bin/sh
# The universal Slipfork installer. One POSIX shell script, no bashisms.
#
#   curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh
#   curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh -s -- --user
#   curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh -s -- --prefix "$HOME/.local"
#
# stdin carries this script, so nothing here reads stdin: questions and sudo read
# /dev/tty, and every child gets /dev/null. The trust anchor is the package
# signature (apt/dnf verification is never disabled), and an overridden archive
# origin requires SLIPFORK_SHA256 (or SLIPFORK_SHA256SUMS_FILE).
set -eu

REPO_ORG=offworld-nexus
REPO_NAME=slipfork
CLOUDSMITH_BASE=${SLIPFORK_CLOUDSMITH_BASE:-https://dl.cloudsmith.io/public/$REPO_ORG/$REPO_NAME}
FORGEJO_BASE=${SLIPFORK_FORGEJO_BASE:-https://forge.offworld.nexus/api/packages/$REPO_ORG/generic/$REPO_NAME}
RELEASES_BASE=${SLIPFORK_RELEASES_BASE:-https://github.com/OffworldNexus/slipfork/releases}

VERSION=""
NIGHTLY=0
MIRROR=cloudsmith
PREFIX=""
USER_INSTALL=0
ASSUME_YES=0
DEVELOP=0
DEVELOP_REF=""

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-}" != dumb ]; then
    bold=$(printf '\033[1m'); cyan=$(printf '\033[36m'); dim=$(printf '\033[2m')
    green=$(printf '\033[32m'); red=$(printf '\033[31m'); yellow=$(printf '\033[33m')
    reset=$(printf '\033[0m')
else
    bold="" cyan="" dim="" green="" red="" yellow="" reset=""
fi

say() { printf '%s\n' "$*"; }
info() { printf '%s%s%s\n' "$cyan" "$*" "$reset"; }
warn() { printf '%s%swarning:%s %s\n' "$yellow" "$dim" "$reset" "$*" >&2; }
die() {
    printf '%serror:%s %s\n' "$red" "$reset" "$1" >&2
    printf 'next: %s\n' "$2" >&2
    exit 1
}
help() {
    cat <<'USAGE'
usage: install.sh [options]
  --version <ver>          install exactly this release
  --nightly                use the nightly build; re-run without it for releases
  --mirror cloudsmith|forgejo  choose the repository (default: cloudsmith)
  --user                   no root; unpack under ~/.local
  --prefix <dir>           no root; unpack under an absolute directory you own
  --develop [ref]          use the archive built for that ref (requires SLIPFORK_SHA256)
  -y, --yes                do not ask for confirmation
  -h, --help               print this help
USAGE
}

usage() {
    [ "$#" -eq 0 ] || printf '%serror:%s %s\n' "$red" "$reset" "$1" >&2
    printf 'next: run --help for the flags\n' >&2
    help >&2
    exit 2
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version) [ "$#" -ge 2 ] || usage "option --version needs a value"; VERSION=$2; shift 2 ;;
        --nightly) NIGHTLY=1; shift ;;
        --mirror) [ "$#" -ge 2 ] || usage "option --mirror needs a value"; MIRROR=$2; shift 2 ;;
        --user) USER_INSTALL=1; shift ;;
        --prefix) [ "$#" -ge 2 ] || usage "option --prefix needs a value"; PREFIX=$2; shift 2 ;;
        --develop)
            DEVELOP=1; shift
            if [ "$#" -gt 0 ]; then
                case "$1" in -*) ;; *) DEVELOP_REF=$1; shift ;; esac
            fi ;;
        -y | --yes) ASSUME_YES=1; shift ;;
        -h | --help) help; exit 0 ;;
        *) usage "unknown option $1" ;;
    esac
done
case "$MIRROR" in cloudsmith | forgejo) ;; *) usage "--mirror $MIRROR is not cloudsmith|forgejo" ;; esac
[ -z "$PREFIX" ] || case "$PREFIX" in /*) ;; *) die "--prefix $PREFIX is not absolute" "pass an absolute directory you own, e.g. --prefix $HOME/.local" ;; esac
[ "$USER_INSTALL" -eq 0 ] || PREFIX=${PREFIX:-$HOME/.local}

os=$(uname -s)
arch=$(uname -m)
case "$arch" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) die "no Slipfork build for $arch" "supported architectures are x86_64 and aarch64" ;;
esac
if [ "$os" != Linux ]; then
    case "$os" in
        Darwin) asset="slipfork-lsp-darwin-$arch.tar.gz" ;;
        *NT* | MINGW* | MSYS* | CYGWIN*) asset="slipfork-lsp-windows-$arch.zip" ;;
        *)
            say "no editor asset for $os: slipfork needs Linux Btrfs and crun, and"
            say "slipfork-lsp is published only for Linux, macOS, and Windows."
            exit 0 ;;
    esac
    say "slipfork needs Linux Btrfs and crun, so it is not installed on $os."
    info "Your editor still works: download $asset from $RELEASES_BASE/latest"
    exit 0
fi

libc=gnu
if ldd --version 2>&1 | grep -qi musl || [ -e "/lib/ld-musl-$arch.so.1" ]; then
    libc=musl
fi
triple="$arch-unknown-linux-$libc"

RUN=""
if [ "$(id -u)" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
        RUN="sudo -n"
    fi
fi
if [ "$(id -u)" -eq 0 ]; then RUN=""; fi

ask() {
    [ "$ASSUME_YES" -eq 1 ] && return 0
    [ -r /dev/tty ] || return 1
    printf '%s [Y/n] ' "$1" >/dev/tty
    answer=y
    read -r answer </dev/tty || answer=y
    case "$answer" in "" | y | Y | yes | YES) return 0 ;; *) return 1 ;; esac
}

fetch() {
    case "$1" in
        file://*) cp "${1#file://}" "$2" ;;
        https://* | http://*)
            if command -v curl >/dev/null 2>&1; then
                curl -fsSL "$1" -o "$2" </dev/null
            elif command -v wget >/dev/null 2>&1; then
                wget -q -O "$2" "$1" </dev/null
            else
                die "neither curl nor wget is installed" "install curl, then re-run the installer"
            fi ;;
        *) die "unsupported archive origin $1" "use an https:// or file:// origin" ;;
    esac
}

archive_url() {
    base=$RELEASES_BASE
    asset="slipfork-$arch-unknown-linux-$libc.tar.gz"
    if [ -n "${SLIPFORK_ARCHIVE_URL:-}" ]; then
        printf '%s\n' "$SLIPFORK_ARCHIVE_URL"
    elif [ "$DEVELOP" -eq 1 ]; then
        [ -n "$DEVELOP_REF" ] || die "--develop needs a ref or SLIPFORK_ARCHIVE_URL" \
            "re-run with --develop <ref>, or set SLIPFORK_ARCHIVE_URL to a CI archive"
        printf '%s/download/nightly-%s/%s\n' "$base" "$DEVELOP_REF" "$asset"
    elif [ "$NIGHTLY" -eq 1 ]; then
        printf '%s/download/nightly/%s\n' "$base" "$asset"
    elif [ -n "$VERSION" ]; then
        printf '%s/download/v%s/%s\n' "$base" "$VERSION" "$asset"
    else
        printf '%s/latest/download/%s\n' "$base" "$asset"
    fi
}

expected_sha() {
    if [ -n "${SLIPFORK_SHA256:-}" ]; then
        printf '%s\n' "$SLIPFORK_SHA256" | cut -d' ' -f1
        return 0
    fi
    if [ -n "${SLIPFORK_SHA256SUMS_FILE:-}" ]; then
        basename=$(basename "$1")
        awk -v name="$basename" '$2 == name || $2 == "*" name { print $1; exit }' "$SLIPFORK_SHA256SUMS_FILE"
        return 0
    fi
    printf ''
}

digest_for() {
    url=$1
    path=$2
    want=$(expected_sha "$url")
    if [ -z "$want" ]; then
        fetch "$url.sha256" "$tmp/sidecar.sha256" 2>/dev/null || true
        [ -s "$tmp/sidecar.sha256" ] && want=$(cut -d' ' -f1 "$tmp/sidecar.sha256")
    fi
    printf '%s\n' "$want"
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

install_archive() {
    url=$(archive_url)
    tmp=$(mktemp -d "${TMPDIR:-/var/tmp}/slipfork-install.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT INT TERM
    info "Downloading $url"
    fetch "$url" "$tmp/archive.tar.gz" || die "the archive did not download" "check the URL and your network, then re-run"
    want=$(expected_sha "$url")
    if [ -z "$want" ] && [ -z "${SLIPFORK_ARCHIVE_URL:-}" ] && [ "$DEVELOP" -eq 0 ]; then
        fetch "$url.sha256" "$tmp/archive.sha256" 2>/dev/null || true
        [ -s "$tmp/archive.sha256" ] && want=$(cut -d' ' -f1 "$tmp/archive.sha256")
    fi
    [ -n "$want" ] || die "no SHA-256 was published for $(basename "$url")" \
        "set SLIPFORK_SHA256 (an overridden origin is unsigned), then re-run"
    got=$(sha256_of "$tmp/archive.tar.gz")
    [ "$want" = "$got" ] || die "SHA-256 mismatch: expected $want, got $got" \
        "delete the download and re-run; do not install an unverified archive"
    mkdir -p "$tmp/unpack"
    tar -xzf "$tmp/archive.tar.gz" -C "$tmp/unpack" </dev/null
    root=$(find "$tmp/unpack" -mindepth 1 -maxdepth 1 -type d | head -n 1)
    [ -n "$root" ] || die "the archive has no top-level directory" "re-publish the archive with scripts/build-standalone.sh"
    prefix=${PREFIX:-/usr/local}
    install -d "$prefix" 2>/dev/null || die "cannot write to $prefix" \
        "re-run with --user (installs under ~/.local) or sudo, or pass a --prefix you own"
    [ -w "$prefix" ] || die "cannot write to $prefix" \
        "re-run with --user (installs under ~/.local) or sudo, or pass a --prefix you own"
    install -d "$prefix/bin" "$prefix/share/man/man1"
    for bin in slipfork slipfork-lsp slipfork-world-init; do
        [ -f "$root/$bin" ] || die "archive is missing $bin" "re-publish with scripts/build-standalone.sh"
        install -m 0755 "$root/$bin" "$prefix/bin/$bin"
    done
    if [ -d "$root/share" ]; then
        cp -R "$root/share/." "$prefix/share/" </dev/null
    fi
    [ -f "$root/LICENSE" ] && install -m 0644 "$root/LICENSE" "$prefix/share/slipfork-LICENSE"
    "$prefix/bin/slipfork" --version </dev/null >/dev/null ||
        die "$prefix/bin/slipfork does not run" "run $prefix/bin/slipfork doctor and check the loader with ldd"
    say "${green}$(printf '\342\234\223')${reset} Installed ${bold}slipfork${reset} into $prefix/bin"
    say "next: run ${bold}slipfork doctor${reset} inside your project, then ${bold}slipfork up${reset}"
}

install_package() {
    [ -z "$PREFIX" ] || return 1
    [ "$DEVELOP" -eq 0 ] || return 1
    pm=""
    for candidate in apt-get dnf yum zypper pacman apk; do
        if command -v "$candidate" >/dev/null 2>&1; then pm=$candidate; break; fi
    done
    [ -n "$pm" ] || return 1
    if [ -z "$RUN" ] && [ "$(id -u)" -ne 0 ]; then
        command -v sudo >/dev/null 2>&1 || return 1
        ask "Install Slipfork system-wide with sudo?" || return 1
        RUN=sudo
    fi
    if [ "$MIRROR" = forgejo ]; then
        warn "the Forgejo mirror publishes archives only; installing the signed Cloudsmith package instead"
    fi
    tmp=$(mktemp -d "${TMPDIR:-/var/tmp}/slipfork-repo.XXXXXX")
    case "$pm" in
        apt-get)
            fetch "$CLOUDSMITH_BASE/setup.deb.sh" "$tmp/setup.sh" || return 1
            $RUN sh "$tmp/setup.sh" </dev/null || return 1
            if [ -n "$VERSION" ]; then
                $RUN apt-get install -y "slipfork=$VERSION" </dev/null ||
                    die "apt-get install failed" "read the log above, then re-run with --user"
            else
                $RUN apt-get install -y slipfork </dev/null ||
                    die "apt-get install failed" "read the log above, then re-run with --user"
            fi ;;
        dnf | yum)
            fetch "$CLOUDSMITH_BASE/setup.rpm.sh" "$tmp/setup.sh" || return 1
            $RUN sh "$tmp/setup.sh" </dev/null || return 1
            $RUN "$pm" install -y slipfork </dev/null ||
                die "$pm install failed" "read the log above, then re-run with --user" ;;
        zypper)
            fetch "$CLOUDSMITH_BASE/setup.rpm.sh" "$tmp/setup.sh" || return 1
            $RUN sh "$tmp/setup.sh" </dev/null || return 1
            $RUN zypper --non-interactive install slipfork </dev/null ||
                die "zypper install failed" "read the log above, then re-run with --user" ;;
        pacman)
            [ -n "$VERSION" ] || return 1
            artifact="slipfork-$VERSION-1-$arch.pkg.tar.zst"
            artifact_url="$RELEASES_BASE/download/v$VERSION/$artifact"
            fetch "$artifact_url" "$tmp/$artifact" || return 1
            want=$(digest_for "$artifact_url" "$tmp/$artifact") || return 1
            [ -n "$want" ] || die "no SHA-256 was published for $artifact" \
                "set SLIPFORK_SHA256=<digest>, then re-run"
            [ "$want" = "$(sha256_of "$tmp/$artifact")" ] || die "SHA-256 mismatch for $artifact" \
                "delete the download and re-run; do not install an unverified package"
            $RUN pacman -U --noconfirm "$tmp/$artifact" </dev/null ||
                die "pacman install failed" "read the log above, then re-run with --user" ;;
        apk)
            # An unsigned local .apk is trusted only by its published digest: there
            # is no repository key on this path, so the SHA-256 is the anchor.
            [ -n "$VERSION" ] || return 1
            component=stable; [ "$NIGHTLY" -eq 1 ] && component=nightly
            apk_file=""
            for candidate in "slipfork-$VERSION-r0-$arch.apk"; do
                candidate_url="$FORGEJO_BASE/$component/$candidate"
                fetch "$candidate_url" "$tmp/$candidate" 2>/dev/null || continue
                want=$(digest_for "$candidate_url" "$tmp/$candidate") || return 1
                [ -n "$want" ] || die "no SHA-256 was published for $candidate" \
                    "set SLIPFORK_SHA256=<digest>, then re-run"
                [ "$want" = "$(sha256_of "$tmp/$candidate")" ] || die "SHA-256 mismatch for $candidate" \
                    "delete the download and re-run; do not install an unverified package"
                apk_file="$tmp/$candidate"
                break
            done
            [ -n "$apk_file" ] || die "no slipfork $VERSION .apk was published" \
                "check --version and the Forgejo mirror, then re-run"
            $RUN apk add --allow-untrusted "$apk_file" </dev/null ||
                die "apk install failed" "read the log above, then re-run with --user" ;;
    esac
    rm -rf "$tmp"
    command -v slipfork >/dev/null 2>&1 ||
        die "the package installed but slipfork is not on PATH" "open a new shell, or install with --user"
    say "${green}$(printf '\342\234\223')${reset} Installed ${bold}slipfork${reset} with $pm"
    say "next: run ${bold}slipfork doctor${reset} inside your project, then ${bold}slipfork up${reset}"
}

if [ "$DEVELOP" -eq 1 ] || [ "$USER_INSTALL" -eq 1 ] || [ -n "$PREFIX" ] || [ "$MIRROR" = forgejo ]; then
    install_archive
elif install_package; then
    :
else
    warn "no usable package manager or sudo; falling back to the SHA-256-verified archive"
    install_archive
fi
