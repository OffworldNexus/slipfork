%global debug_package %{nil}

%define _slipfork_version %{?slipfork_version}%{!?slipfork_version:0.1.0}
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

%preun
if [ "$1" = 0 ] && command -v systemctl >/dev/null 2>&1; then
    systemctl --system stop slipfork-ingress.socket >/dev/null 2>&1 || true
    systemctl --system disable slipfork-ingress.socket >/dev/null 2>&1 || true
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
