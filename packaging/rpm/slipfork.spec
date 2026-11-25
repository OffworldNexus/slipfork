%global debug_package %{nil}

%define _slipfork_version %{?slipfork_version}%{!?slipfork_version:0.1.1}
%define _slipfork_release %{?slipfork_release}%{!?slipfork_release:1}
%define _slipfork_arch %{?slipfork_arch}%{!?slipfork_arch:x86_64}
%define _slipfork_glibc %{?slipfork_glibc}%{!?slipfork_glibc:%{error:slipfork_glibc must come from check-portable-elf.sh}}

Name:           slipfork
Version:        %{_slipfork_version}
Release:        %{_slipfork_release}
Summary:        Copy-on-write Btrfs development environments, Docker-free
License:        custom
URL:            https://github.com/OffworldNexus/slipfork
BuildArch:      %{_slipfork_arch}
AutoReqProv:    no
Requires:       glibc >= %{_slipfork_glibc}
Requires:       crun
Requires:       slirp4netns
# Rocky 9 dropped btrfs-progs from its base repos, so this must not block `dnf install`.
Recommends:     btrfs-progs
Source0:        payload.tar.gz

%description
Slipfork builds and runs a project's images, databases, caches, and workers from
one Forkfile on native Btrfs subvolumes and crun, behind one .localhost domain.
Forks clone code, dependencies, and live data in milliseconds.

%prep
%setup -q -c -T

%build

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}
tar -xzf %{SOURCE0} -C %{buildroot}

%post
if command -v apparmor_parser >/dev/null 2>&1 && [ -f /etc/apparmor.d/slipfork ]; then
    apparmor_parser -r -W /etc/apparmor.d/slipfork >/dev/null 2>&1 || true
fi
systemctl --system daemon-reload >/dev/null 2>&1 || true
# The package ships both units; enable and start them for the installing user.
# A container/chroot with no systemd stays silent; on systemd a missing SUDO_USER
# prints one hint.
if [ -d /run/systemd/system ] && [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then
    runuser=$(command -v runuser 2>/dev/null || true)
    if [ -z "$runuser" ] && [ -x /usr/sbin/runuser ]; then runuser=/usr/sbin/runuser; fi
    if [ -z "$runuser" ] && [ -x /sbin/runuser ]; then runuser=/sbin/runuser; fi
    if command -v systemctl >/dev/null 2>&1 &&
        command -v loginctl >/dev/null 2>&1 &&
        [ -n "$runuser" ] && uid=$(id -u "$SUDO_USER" 2>/dev/null); then
        loginctl enable-linger "$SUDO_USER" >/dev/null 2>&1 || true
        systemctl start "user@$uid.service" >/dev/null 2>&1 || true
        "$runuser" -u "$SUDO_USER" -- env \
            XDG_RUNTIME_DIR="/run/user/$uid" \
            DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" \
            systemctl --user enable --now slipfork.service >/dev/null 2>&1 || true
    else
        echo "next: loginctl enable-linger $SUDO_USER && sudo -iu $SUDO_USER systemctl --user enable --now slipfork.service" >&2
    fi
fi

# The :80 socket is independent of the user daemon: it needs only the name and
# uid of the installing user. Render the template for them, enable the socket,
# and report a real failure once instead of swallowing it.
ingress_setup() {
    [ -d /run/systemd/system ] || return 0
    command -v systemctl >/dev/null 2>&1 || return 0
    src=/usr/lib/systemd/system/slipfork-ingress@.service
    dst=/etc/systemd/system/slipfork-ingress@.service
    if [ ! -f "$src" ]; then
        echo "! slipfork: the ingress unit template is missing ($src)" >&2
        echo "next: reinstall the slipfork package" >&2
        return 0
    fi
    if [ -z "${SUDO_USER:-}" ] || [ "$SUDO_USER" = root ]; then
        echo "next: install with sudo from your user session, or set User=<you> in $dst and run: systemctl enable --now slipfork-ingress.socket" >&2
        return 0
    fi
    uid=$(id -u "$SUDO_USER" 2>/dev/null) || uid=${SUDO_UID:-}
    if [ -z "$uid" ]; then
        echo "next: install with sudo from your user session, or set User=<you> in $dst and run: systemctl enable --now slipfork-ingress.socket" >&2
        return 0
    fi
    if ! install -Dm 0644 "$src" "$dst" 2>/dev/null; then
        echo "! slipfork: could not write $dst" >&2
        echo "next: check that /etc/systemd/system is writable and has free space, then reinstall the package" >&2
        return 0
    fi
    if ! printf 'User=%s\nEnvironment=XDG_RUNTIME_DIR=/run/user/%s\n' "$SUDO_USER" "$uid" >>"$dst" 2>/dev/null; then
        echo "! slipfork: could not write $dst" >&2
        echo "next: check that /etc/systemd/system is writable and has free space, then reinstall the package" >&2
        return 0
    fi
    systemctl daemon-reload >/dev/null 2>&1 || true
    systemctl enable --now slipfork-ingress.socket >/dev/null 2>&1 || true
    systemctl is-active --quiet slipfork-ingress.socket 2>/dev/null && return 0
    if [ "$(systemctl is-enabled slipfork-ingress.socket 2>/dev/null)" = masked ]; then
        echo "! slipfork: slipfork-ingress.socket is masked" >&2
        echo "next: systemctl unmask slipfork-ingress.socket && systemctl enable --now slipfork-ingress.socket" >&2
    elif command -v ss >/dev/null 2>&1 && [ -n "$(ss -Hltn 'sport = :80' 2>/dev/null)" ]; then
        echo "! slipfork: the :80 ingress did not start; another process already listens on port 80" >&2
        echo "next: ss -ltn 'sport = :80' then free the port and run: systemctl restart slipfork-ingress.socket" >&2
    else
        echo "! slipfork: the :80 ingress did not start" >&2
        echo "next: journalctl -u slipfork-ingress.socket" >&2
    fi
    return 0
}

ingress_setup

%preun
if [ "$1" = 0 ] && command -v systemctl >/dev/null 2>&1; then
    systemctl --system stop slipfork-ingress.socket >/dev/null 2>&1 || true
    systemctl --system disable slipfork-ingress.socket >/dev/null 2>&1 || true
    rm -f /etc/systemd/system/slipfork-ingress@.service
    systemctl --system daemon-reload >/dev/null 2>&1 || true
fi
if [ "$1" = 0 ] && [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then
    runuser=$(command -v runuser 2>/dev/null || true)
    if [ -z "$runuser" ] && [ -x /usr/sbin/runuser ]; then runuser=/usr/sbin/runuser; fi
    if [ -z "$runuser" ] && [ -x /sbin/runuser ]; then runuser=/sbin/runuser; fi
    if [ -n "$runuser" ] && uid=$(id -u "$SUDO_USER" 2>/dev/null); then
        "$runuser" -u "$SUDO_USER" -- env \
            XDG_RUNTIME_DIR="/run/user/$uid" \
            DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" \
            systemctl --user disable --now slipfork.service >/dev/null 2>&1 || true
    fi
fi

%files
%{_bindir}/slipfork
%{_bindir}/slipfork-lsp
%{_bindir}/slipfork-world-init
/usr/share/slipfork
/usr/lib/systemd/system/slipfork-ingress.socket
/usr/lib/systemd/system/slipfork-ingress@.service
/usr/lib/systemd/user/slipfork.service
/etc/apparmor.d/slipfork
%{_mandir}/man1/slipfork.1.*
%{_mandir}/man1/slipfork-lsp.1.*
/usr/share/bash-completion/completions/slipfork
/usr/share/zsh/site-functions/_slipfork
/usr/share/fish/vendor_completions.d/slipfork.fish
/usr/share/vim/vimfiles/ftdetect/forkfile.vim
/usr/share/vim/vimfiles/syntax/forkfile.vim
/usr/share/vim/vimfiles/indent/forkfile.vim
/usr/share/vim/vimfiles/ftplugin/forkfile.vim
