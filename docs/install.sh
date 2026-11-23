#!/bin/sh
# The universal Slipfork installer. One POSIX shell script, no bashisms.
#
#   curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh
#   curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh -s -- --user
#   curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh -s -- --prefix "$HOME/.local"
#
# stdin carries this script, so nothing here reads stdin: questions and sudo read
# /dev/tty, and every child gets /dev/null. The trust anchor is the package
# signature (apt/dnf/pacman/apk verification is never disabled); an overridden
# archive origin requires SLIPFORK_SHA256 (or SLIPFORK_SHA256SUMS_FILE).
set -eu

APT_BASE=${SLIPFORK_APT_BASE:-https://apt.offworld-nexus.app}
RPM_BASE=${SLIPFORK_RPM_BASE:-https://rpm.offworld-nexus.app}
APK_BASE=${SLIPFORK_APK_BASE:-https://apk.offworld-nexus.app}
ARCH_BASE=${SLIPFORK_ARCH_BASE:-https://arch.offworld-nexus.app}
DL_BASE=${SLIPFORK_DL_BASE:-https://dl.offworld-nexus.app}

VERSION=""
NIGHTLY=0
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
warn() {
    printf '%s%swarning:%s %s\n' "$yellow" "$dim" "$reset" "$1" >&2
    shift
    [ "$#" -eq 0 ] || printf '%s\n' "$@" >&2
}
die() {
    printf '%serror:%s %s\n' "$red" "$reset" "$1" >&2
    printf 'next: %s\n' "$2" >&2
    exit 1
}
help() {
    cat <<'USAGE'
usage: install.sh [options]
  --version <ver>          install exactly this release
  --nightly                use the nightly channel; re-run without it for releases
  --user                   no root; unpack under ~/.local
  --prefix <dir>           no root; unpack under an absolute directory you own
  --develop [ref]          use the CI archive in SLIPFORK_ARCHIVE_URL (digest mandatory)
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
[ -z "$PREFIX" ] || case "$PREFIX" in /*) ;; *) die "--prefix $PREFIX is not absolute" "pass an absolute directory you own, e.g. --prefix $HOME/.local" ;; esac
[ "$USER_INSTALL" -eq 0 ] || PREFIX=${PREFIX:-$HOME/.local}
if [ "$DEVELOP" -eq 1 ] && [ -z "${SLIPFORK_ARCHIVE_URL:-}" ]; then
    die "--develop needs the CI archive URL" \
        "set SLIPFORK_ARCHIVE_URL=<archive> and SLIPFORK_SHA256=<digest>, then re-run"
fi

component=stable
if [ "$NIGHTLY" -eq 1 ]; then component=nightly; fi

os=$(uname -s)
arch=$(uname -m)
case "$arch" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) die "no Slipfork build for $arch" "supported architectures are x86_64 and aarch64" ;;
esac
if [ "$os" != Linux ]; then
    case "$os" in
        Darwin) platform=macos; asset="slipfork-lsp-darwin-$arch.tar.gz" ;;
        *NT* | MINGW* | MSYS* | CYGWIN*) platform=windows; asset="slipfork-lsp-windows-$arch.zip" ;;
        *)
            say "no editor asset for $os: slipfork needs Linux Btrfs and crun, and"
            say "slipfork-lsp is published only for Linux, macOS, and Windows."
            exit 0 ;;
    esac
    say "slipfork needs Linux Btrfs and crun, so it is not installed on $os."
    info "Your editor still works: download $asset from $DL_BASE/$platform/$component/$asset"
    exit 0
fi

libc=gnu
if ldd --version 2>&1 | grep -qi musl || [ -e "/lib/ld-musl-$arch.so.1" ]; then
    libc=musl
fi
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
    asset="slipfork-$arch-unknown-linux-$libc.tar.gz"
    if [ -n "${SLIPFORK_ARCHIVE_URL:-}" ]; then
        printf '%s\n' "$SLIPFORK_ARCHIVE_URL"
    elif [ -n "$VERSION" ]; then
        die "--version needs a package repository or an explicit archive" \
            "set SLIPFORK_ARCHIVE_URL=<versioned tarball> and SLIPFORK_SHA256=<digest>, then re-run (or drop --version)"
    else
        printf '%s/tarballs/%s/%s\n' "$DL_BASE" "$component" "$asset"
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
    trap 'rm -rf "$tmp"' EXIT
    trap 'rm -rf "$tmp"; exit 130' INT TERM
    info "Downloading $url"
    fetch "$url" "$tmp/archive.tar.gz" || die "the archive did not download" "check the URL and your network, then re-run"
    want=$(expected_sha "$url")
    overridden=0
    if [ -n "${SLIPFORK_ARCHIVE_URL:-}" ] || [ "$DEVELOP" -eq 1 ]; then overridden=1; fi
    if [ -z "$want" ]; then
        if [ "$overridden" -eq 1 ]; then
            die "no SHA-256 is published for $(basename "$url") from an overridden origin" \
                "set SLIPFORK_SHA256 to the CI digest, then re-run"
        fi
        fetch "$url.sha256" "$tmp/archive.sha256" 2>/dev/null || true
        if [ -s "$tmp/archive.sha256" ]; then
            want=$(cut -d' ' -f1 "$tmp/archive.sha256")
        else
            warn "no published SHA-256 for $(basename "$url"); trusting HTTPS on $DL_BASE" \
                "next: set SLIPFORK_SHA256 to pin the digest"
        fi
    fi
    if [ -n "$want" ]; then
        got=$(sha256_of "$tmp/archive.tar.gz")
        [ "$want" = "$got" ] || die "SHA-256 mismatch: expected $want, got $got" \
            "delete the download and re-run; do not install an unverified archive"
    fi
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

install_apt() {
    $RUN install -d -m 0755 /etc/apt/keyrings ||
        die "cannot create /etc/apt/keyrings" "re-run with sudo, or install the tarball with --user"
    fetch "$APT_BASE/keyring.gpg" "$tmp/keyring.gpg" ||
        die "the Offworld Nexus apt keyring did not download" "check that $APT_BASE is reachable, then re-run"
    $RUN install -m 0644 "$tmp/keyring.gpg" /etc/apt/keyrings/offworld-nexus.gpg ||
        die "cannot write /etc/apt/keyrings/offworld-nexus.gpg" "re-run with sudo, then install slipfork"
    cat > "$tmp/offworld-nexus.sources" <<EOF
Types: deb
URIs: $APT_BASE
Suites: any-distro
Components: $component
Signed-By: /etc/apt/keyrings/offworld-nexus.gpg
EOF
    $RUN install -m 0644 "$tmp/offworld-nexus.sources" /etc/apt/sources.list.d/offworld-nexus.sources ||
        die "cannot write /etc/apt/sources.list.d/offworld-nexus.sources" "re-run with sudo, then install slipfork"
    $RUN apt-get update </dev/null ||
        die "apt-get update rejected $APT_BASE" "read the log above, then re-run"
    if [ -n "$VERSION" ]; then
        $RUN apt-get install -y "slipfork=$VERSION" </dev/null ||
            die "apt-get install slipfork=$VERSION failed" "check that $VERSION is in $APT_BASE/$component, then re-run"
    else
        $RUN apt-get install -y slipfork </dev/null ||
            die "apt-get install slipfork failed" "read the log above, then re-run with --user"
    fi
}

install_rpm() {
    channel_name=Stable
    if [ "$NIGHTLY" -eq 1 ]; then channel_name=Nightly; fi
    $RUN install -d -m 0755 /etc/yum.repos.d ||
        die "cannot create /etc/yum.repos.d" "re-run with sudo, or install the tarball with --user"
    cat > "$tmp/offworld-nexus.repo" <<EOF
[offworld-nexus]
name=Offworld Nexus ($channel_name)
baseurl=$RPM_BASE/$component/\$basearch/
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=$RPM_BASE/gpg.key
EOF
    $RUN install -m 0644 "$tmp/offworld-nexus.repo" /etc/yum.repos.d/offworld-nexus.repo ||
        die "cannot write /etc/yum.repos.d/offworld-nexus.repo" "re-run with sudo, then install slipfork"
    if [ "$pm" = zypper ]; then
        if [ -n "$VERSION" ]; then
            $RUN zypper --non-interactive install "slipfork-$VERSION" </dev/null ||
                die "zypper install slipfork-$VERSION failed" "check that $VERSION is in $RPM_BASE/$component, then re-run"
        else
            $RUN zypper --non-interactive install slipfork </dev/null ||
                die "zypper install slipfork failed" "read the log above, then re-run with --user"
        fi
    elif [ -n "$VERSION" ]; then
        $RUN "$pm" install -y "slipfork-$VERSION" </dev/null ||
            die "$pm install slipfork-$VERSION failed" "check that $VERSION is in $RPM_BASE/$component, then re-run"
    else
        $RUN "$pm" install -y slipfork </dev/null ||
            die "$pm install slipfork failed" "read the log above, then re-run with --user"
    fi
}

install_pacman() {
    fetch "$ARCH_BASE/gpg.key" "$tmp/gpg.key" ||
        die "the Offworld Nexus pacman key did not download" "check that $ARCH_BASE is reachable, then re-run"
    $RUN pacman-key --add "$tmp/gpg.key" </dev/null ||
        die "pacman-key --add failed" "re-run with sudo, then install slipfork"
    $RUN pacman-key --lsign-key E9DD32C63321505EBBCF86386E08120B3306C720 </dev/null ||
        die "pacman-key --lsign-key failed" "re-run with sudo, then install slipfork"
    if $RUN grep -q '^\[offworld\]' /etc/pacman.conf 2>/dev/null; then
        $RUN sed -i "s|^Server = $ARCH_BASE/.*|Server = $ARCH_BASE/$component/\$arch|" /etc/pacman.conf ||
            die "cannot update [offworld] in /etc/pacman.conf" "re-run with sudo, then install slipfork"
    else
        printf '\n[offworld]\nServer = %s/%s/$arch\n' "$ARCH_BASE" "$component" |
            $RUN tee -a /etc/pacman.conf >/dev/null ||
            die "cannot append [offworld] to /etc/pacman.conf" "re-run with sudo, then install slipfork"
    fi
    if [ -n "$VERSION" ]; then
        $RUN pacman -Sy --needed --noconfirm "slipfork=$VERSION-1" </dev/null ||
            die "pacman install slipfork=$VERSION-1 failed" "check that $VERSION is in $ARCH_BASE/$component, then re-run"
    else
        $RUN pacman -Sy --needed --noconfirm slipfork </dev/null ||
            die "pacman install slipfork failed" "read the log above, then re-run with --user"
    fi
}

install_apk() {
    fetch "$APK_BASE/packages@offworld.nexus.rsa.pub" "$tmp/packages@offworld.nexus.rsa.pub" ||
        die "the Offworld Nexus apk key did not download" "check that $APK_BASE is reachable, then re-run"
    $RUN install -d -m 0755 /etc/apk/keys ||
        die "cannot create /etc/apk/keys" "re-run with sudo, or install the tarball with --user"
    $RUN install -m 0644 "$tmp/packages@offworld.nexus.rsa.pub" /etc/apk/keys/packages@offworld.nexus.rsa.pub ||
        die "cannot write /etc/apk/keys/packages@offworld.nexus.rsa.pub" "re-run with sudo, then install slipfork"
    if $RUN grep -q "^$APK_BASE/" /etc/apk/repositories 2>/dev/null; then
        $RUN sed -i "s|^$APK_BASE/.*|$APK_BASE/$component|" /etc/apk/repositories ||
            die "cannot update $APK_BASE in /etc/apk/repositories" "re-run with sudo, then install slipfork"
    else
        printf '%s\n' "$APK_BASE/$component" | $RUN tee -a /etc/apk/repositories >/dev/null ||
            die "cannot append $APK_BASE/$component to /etc/apk/repositories" "re-run with sudo, then install slipfork"
    fi
    $RUN apk update </dev/null ||
        die "apk update rejected $APK_BASE" "read the log above, then re-run"
    if [ -n "$VERSION" ]; then
        $RUN apk add "slipfork=$VERSION-r0" </dev/null ||
            die "apk add slipfork=$VERSION-r0 failed" "check that $VERSION is in $APK_BASE/$component, then re-run"
    else
        $RUN apk add slipfork </dev/null ||
            die "apk add slipfork failed" "read the log above, then re-run with --user"
    fi
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
    tmp=$(mktemp -d "${TMPDIR:-/var/tmp}/slipfork-repo.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    trap 'rm -rf "$tmp"; exit 130' INT TERM
    case "$pm" in
        apt-get) install_apt ;;
        dnf | yum | zypper) install_rpm ;;
        pacman) install_pacman ;;
        apk) install_apk ;;
        *) die "no install path for $pm" "install the tarball with --user" ;;
    esac
    rm -rf "$tmp"
    command -v slipfork >/dev/null 2>&1 ||
        die "the package installed but slipfork is not on PATH" "open a new shell, or install with --user"
    say "${green}$(printf '\342\234\223')${reset} Installed ${bold}slipfork${reset} with $pm"
    say "next: run ${bold}slipfork doctor${reset} inside your project, then ${bold}slipfork up${reset}"
}

if [ "$DEVELOP" -eq 1 ] || [ "$USER_INSTALL" -eq 1 ] || [ -n "$PREFIX" ]; then
    install_archive
elif install_package; then
    :
else
    warn "no usable package manager or sudo; falling back to the archive install"
    install_archive
fi
