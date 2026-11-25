//! Ambient host state, resolved exactly once by the composition root.
//!
//! Nothing below this module reads `std::env`, `/proc`, the terminal, or the working directory:
//! `main` calls these functions once and injects the resulting plain values.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use slipfork_image::RegistryCredentials;

use crate::mounts;
use crate::ui::Terminal;
use crate::ui::report::WallZone;
use crate::world::HostEnv;

/// Resolves the output mode, terminal geometry, and stdin tty exactly once.
pub fn terminal(args: &[OsString]) -> Terminal {
    Terminal::detect(args)
}

/// Resolves the one local wall clock for this invocation, reading `$TZ` then `/etc/localtime`.
pub fn wall_zone() -> WallZone {
    WallZone::from_time_zone(jiff::tz::TimeZone::system())
}

/// Assembles the injected host environment for this invocation.
pub fn host(_args: &[OsString], _cwd: &Path, terminal: Terminal) -> Result<HostEnv, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("cannot locate the slipfork binary: {error}"))?;
    // SAFETY: `geteuid(2)`/`getegid(2)` only read this process's credentials.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(format!("/var/tmp/slipfork-state-{uid}")));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    // `XDG_CONFIG_HOME` must be absolute; a relative one is ignored per the XDG base spec.
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.as_ref().map(|home| home.join(".config")))
        .unwrap_or_else(|| PathBuf::from(format!("/var/tmp/slipfork-config-{uid}")));
    let docker_config = home.as_ref().map(|home| home.join(".docker/config.json"));
    let registries_path = config_dir.join("slipfork/registries.json");
    let registry_credentials =
        RegistryCredentials::load(&registries_path, docker_config.as_deref()).map_err(|error| {
            format!(
                "cannot load `{}`: {}\nnext: fix the JSON in `{}` or delete it to pull anonymously",
                registries_path.display(),
                error.cause(),
                registries_path.display()
            )
        })?;
    let path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_default();
    let opener = ["xdg-open", "open"].iter().find_map(|name| {
        path.iter()
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    });
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")
        .map(|text| mounts::parse_mountinfo(&text))
        .unwrap_or_default();
    let install_root = std::env::var_os("SLIPFORK_INSTALL_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    Ok(HostEnv {
        exe,
        path,
        runtime_dir: std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}"))),
        state_dir: state_home.join("slipfork"),
        mounts,
        arch: std::env::consts::ARCH,
        uid,
        gid,
        resolv_conf: pod_resolv_conf(
            Path::new("/etc/resolv.conf"),
            Path::new("/run/systemd/resolve/resolv.conf"),
        ),
        home,
        install_root,
        hostname: host_name(),
        display: std::env::var_os("DISPLAY").is_some()
            || std::env::var_os("WAYLAND_DISPLAY").is_some(),
        ssh: std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some(),
        color: terminal.mode.color(),
        opener,
        env: std::env::vars().collect(),
        config_dir,
        registry_credentials,
    })
}

/// Reads this host's name once; the kernel truncates anything longer than 255 bytes.
fn host_name() -> String {
    let mut buffer = [0_u8; 256];
    // SAFETY: the kernel writes at most `buffer.len()` bytes into the live array.
    let ret = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if ret != 0 {
        return "localhost".to_owned();
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

/// Resolves the one resolver file every pod and image step binds as `/etc/resolv.conf`.
///
/// A pod runs in its own network namespace, so a loopback stub names a listener that only
/// exists in the host namespace: Ubuntu's systemd-resolved `/etc/resolv.conf`
/// (`nameserver 127.0.0.53`) makes every pod fail with `Temporary failure in name resolution`.
/// When `etc` lists nameservers but all of them are loopback, prefer systemd-resolved's
/// upstream file, which holds the real resolvers; an absent, unreadable, or still-loopback
/// upstream keeps `etc` untouched — never invent a resolver.
#[must_use]
pub fn pod_resolv_conf(etc: &Path, resolved: &Path) -> PathBuf {
    let configured = std::fs::read_to_string(etc).unwrap_or_default();
    let nameservers = resolv_nameservers(&configured);
    let stubbed = !nameservers.is_empty() && nameservers.iter().all(std::net::IpAddr::is_loopback);
    if !stubbed {
        return etc.to_path_buf();
    }
    let upstream = std::fs::read_to_string(resolved).unwrap_or_default();
    if resolv_nameservers(&upstream)
        .iter()
        .any(|address| !address.is_loopback())
    {
        resolved.to_path_buf()
    } else {
        etc.to_path_buf()
    }
}

/// The `nameserver` addresses of one `resolv.conf`; comments, `search`, and `options` are ignored.
fn resolv_nameservers(text: &str) -> Vec<std::net::IpAddr> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next() == Some("nameserver")).then(|| fields.next())?
        })
        .filter_map(|address| address.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{pod_resolv_conf, resolv_nameservers};
    use std::fs;
    use std::path::PathBuf;

    /// Real resolver files under `/var/tmp` (disk, never RAM `/tmp`), one tree per case.
    struct Lab {
        root: tempfile::TempDir,
    }

    impl Lab {
        fn new() -> Self {
            let root = tempfile::Builder::new()
                .prefix("slipfork-resolv-")
                .tempdir_in("/var/tmp")
                .expect("create the resolver lab under /var/tmp");
            Self { root }
        }

        fn etc(&self, body: &str) -> PathBuf {
            self.write("etc/resolv.conf", body)
        }

        fn upstream(&self, body: &str) -> PathBuf {
            self.write("run/systemd/resolve/resolv.conf", body)
        }

        fn upstream_path(&self) -> PathBuf {
            self.root.path().join("run/systemd/resolve/resolv.conf")
        }

        fn write(&self, relative: &str, body: &str) -> PathBuf {
            let path = self.root.path().join(relative);
            fs::create_dir_all(path.parent().expect("resolver path has a parent")).unwrap();
            fs::write(&path, body).unwrap();
            path
        }
    }

    const STUB: &str = "nameserver 127.0.0.53\noptions edns0 trust-ad\nsearch .\n";
    const REAL: &str =
        "# systemd-resolved upstream list\nnameserver 10.0.1.2\nnameserver 2606:4700:4700::1111\n";

    #[test]
    fn stub_only_etc_prefers_the_resolved_upstream_file() {
        let lab = Lab::new();
        let etc = lab.etc(STUB);
        let resolved = lab.upstream(REAL);
        assert_eq!(pod_resolv_conf(&etc, &resolved), resolved);
    }

    #[test]
    fn public_nameserver_keeps_etc() {
        let lab = Lab::new();
        let etc = lab.etc("nameserver 1.1.1.1\n");
        let resolved = lab.upstream(REAL);
        assert_eq!(pod_resolv_conf(&etc, &resolved), etc);
    }

    #[test]
    fn stub_only_without_an_upstream_file_fails_closed_to_etc() {
        let lab = Lab::new();
        let etc = lab.etc(STUB);
        assert_eq!(pod_resolv_conf(&etc, &lab.upstream_path()), etc);
    }

    #[test]
    fn loopback_or_nameserverless_upstream_fails_closed_to_etc() {
        let lab = Lab::new();
        let etc = lab.etc("nameserver ::1\n");
        let loopback = lab.upstream("nameserver 127.0.0.53\n# nothing upstream yet\n");
        assert_eq!(pod_resolv_conf(&etc, &loopback), etc);
        let nameserverless = lab.write(
            "invalid/resolv.conf",
            "search internal\noptions timeout:1\n",
        );
        assert_eq!(pod_resolv_conf(&etc, &nameserverless), etc);
    }

    #[test]
    fn comments_search_and_options_never_count_as_nameservers() {
        let lab = Lab::new();
        let etc = lab.etc(
            "# nameserver 9.9.9.9\nsearch example.internal\noptions ndots:2\nnameserver 1.0.0.1\n",
        );
        let resolved = lab.upstream(REAL);
        assert_eq!(pod_resolv_conf(&etc, &resolved), etc);
        assert_eq!(
            resolv_nameservers("# nameserver 9.9.9.9\nnameserver\t1.0.0.1\n").len(),
            1
        );
    }

    #[test]
    fn every_loopback_form_is_a_stub() {
        let lab = Lab::new();
        let etc = lab.etc("nameserver ::1\nnameserver 127.0.0.1\n");
        let resolved = lab.upstream(REAL);
        assert_eq!(pod_resolv_conf(&etc, &resolved), resolved);
    }
}
