//! Non-template `slipforkd.socket` / `slipforkd.service` renderer (`ADR-0008` §5).
//!
//! [`UnitSpec`] is the typed, already-resolved input the CLI composition boundary hands over
//! (`User=<developer-account>` is resolved once there via `UserIdentity`; this module never reads
//! the environment, the filesystem, the clock, or the account database). Both renderers are pure:
//! they validate the spec and return the unit text, so `slipfork daemon unit` writes exactly the
//! bytes the golden tests pin.
//!
//! Listener order is the activation contract of [`crate::activation`]: every
//! [`UnitSpec::listen_addrs`] entry becomes a `ListenStream=` line **in the given order**, followed
//! by the planned static forward listener on `127.0.0.1:<forward_port>` (`H-008`). `systemd` (root,
//! inside the `.socket` unit) binds them and passes `fd 3..` to the unprivileged
//! `User=<developer-account>` service, which is why privileged ports such as `:80` are legitimate
//! here and are never rejected by [`UnitSpec::validate`].
//!
//! Measured host facts (`docs/research/tunnel-weaver-and-systemd-facts.md` §5): descriptors start at
//! `fd 3`, `LISTEN_PID` names the child, and the inherited sockets are blocking. The unit shape
//! (empty `CapabilityBoundingSet=`, `NoNewPrivileges=yes`, `ProtectSystem=strict`, and the
//! `RestrictAddressFamilies=` allow-list in [`SERVICE_RESTRICT_ADDRESS_FAMILIES`]) mirrors the
//! reference implementation's golden units: the service itself never needs a capability because
//! `systemd` owns the binds.
//!
//! The address-family list is a sandbox contract, not a style choice: read
//! [`SERVICE_RESTRICT_ADDRESS_FAMILIES`] before touching it — trimming one family silently breaks
//! world boot inside the installed unit.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::EdgeError;

/// The two loopback ingress listeners systemd binds for port-less `.localhost` origins.
pub const INGRESS_ADDRS: [&str; 2] = ["127.0.0.1:80", "[::1]:80"];

/// The single port every world origin omits (`http://<project>.localhost`).
pub const INGRESS_PORT: u16 = 80;

/// The exact `RestrictAddressFamilies=` value `slipforkd.service` is rendered with.
///
/// `AF_INET`/`AF_INET6`/`AF_UNIX` cover the socket-activated listeners, the ingress socket, and the
/// daemon control socket. `AF_NETLINK` is **required**, not an extra: the `slipfork-world-init` this
/// service spawns brings loopback up with `ip link set lo up`, and `ip` opens an
/// `AF_NETLINK`/`NETLINK_ROUTE` socket. Without it every boot inside the installed unit dies with
/// `EAFNOSUPPORT` (`Cannot open netlink socket: Address family not supported by protocol`). The list
/// stays an explicit allow-list — never a `~` wildcard, never trimmed back to the three inet
/// families — and it is emitted from this one constant, never re-typed by a test.
pub const SERVICE_RESTRICT_ADDRESS_FAMILIES: &str = "AF_INET AF_INET6 AF_UNIX AF_NETLINK";

/// Every value needed to render the non-template `slipforkd.socket` / `slipforkd.service` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitSpec {
    /// Explicit developer account name (`User=<developer-account>`), already resolved; never a
    /// systemd `%`-template specifier and never empty.
    pub user: String,
    /// TCP HTTP ingress listeners, emitted as `ListenStream=` in this exact activation fd order
    /// (`127.0.0.1:80`, `[::1]:80` in the shipped unit).
    pub listen_addrs: Vec<SocketAddr>,
    /// Planned static forward port (`H-008`); becomes the trailing `ListenStream=127.0.0.1:<port>`
    /// and therefore the trailing activation descriptor.
    pub forward_port: u16,
    /// `slipforkd` binary path used as `ExecStart=`.
    pub executable: PathBuf,
    /// State directory passed to `--state-dir`.
    pub state_dir: PathBuf,
    /// Runtime directory passed to `--runtime-dir` (holds `<runtime_dir>/daemon.sock`).
    pub runtime_dir: PathBuf,
}

impl UnitSpec {
    /// Fail closed on a spec that would render a broken or ambiguous unit.
    ///
    /// Refused: an empty, whitespace, or `%`-template `user`
    /// ([`EdgeError::UnitInvalidUser`]), a path that cannot be written into a UTF-8 unit line
    /// ([`EdgeError::UnitInvalidPath`]), an empty [`Self::listen_addrs`]
    /// ([`EdgeError::UnitNoListener`]), and a [`Self::forward_port`] equal to a listed ingress port
    /// ([`EdgeError::UnitPortCollision`]).
    ///
    /// Privileged ports are deliberately **allowed**: `systemd` binds `:80` as root inside the
    /// `.socket` unit and hands the descriptor to this unprivileged service. The "cannot bind
    /// unprivileged" hazard therefore belongs to the runtime diagnostics
    /// ([`EdgeError::ActivationMismatch`] / [`EdgeError::NoListener`] / [`EdgeError::BindListener`]),
    /// never to spec validation.
    pub fn validate(&self) -> Result<(), EdgeError> {
        if self.user.is_empty()
            || self.user.contains('%')
            || self
                .user
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(EdgeError::UnitInvalidUser {
                user: self.user.clone(),
            });
        }
        for (role, path) in [
            ("ExecStart", &self.executable),
            ("--state-dir", &self.state_dir),
            ("--runtime-dir", &self.runtime_dir),
        ] {
            let text = path.to_str().ok_or_else(|| EdgeError::UnitInvalidPath {
                role,
                path: path.display().to_string(),
            })?;
            if text.is_empty() || text.contains('%') || text.chars().any(char::is_control) {
                return Err(EdgeError::UnitInvalidPath {
                    role,
                    path: text.to_owned(),
                });
            }
        }
        if self.listen_addrs.is_empty() {
            return Err(EdgeError::UnitNoListener);
        }
        if let Some(listener) = self
            .listen_addrs
            .iter()
            .find(|addr| addr.port() == self.forward_port)
        {
            return Err(EdgeError::UnitPortCollision {
                forward_port: self.forward_port,
                listener: *listener,
            });
        }
        Ok(())
    }

    /// The listener list in activation fd order: ingress listeners first, forward listener last.
    fn activation_order(&self) -> impl Iterator<Item = String> + '_ {
        self.listen_addrs
            .iter()
            .map(SocketAddr::to_string)
            .chain(std::iter::once(format!("127.0.0.1:{}", self.forward_port)))
    }
}

/// Render `slipforkd.socket` byte-exactly as pinned by
/// `tests/golden/slipforkd.socket`.
///
/// The `[Install]` section exists so the documented
/// `systemctl enable --now slipforkd.socket` works; the socket names the service it activates
/// (`Service=slipforkd.service`).
pub fn render_socket_unit(spec: &UnitSpec) -> Result<String, EdgeError> {
    spec.validate()?;
    let mut unit = String::from(
        "[Unit]\n\
         Description=Slipfork daemon socket (systemd binds the ingress and forward ports as root, then hands them to the unprivileged user service)\n\
         \n\
         [Socket]\n",
    );
    for listener in spec.activation_order() {
        writeln!(unit, "ListenStream={listener}").expect("writing into a String cannot fail");
    }
    unit.push_str(
        "Backlog=1024\n\
         NoDelay=true\n\
         Service=slipforkd.service\n\
         \n\
         [Install]\n\
         WantedBy=sockets.target\n",
    );
    Ok(unit)
}

/// Render `slipforkd.service` byte-exactly as pinned by
/// `tests/golden/slipforkd.service`.
///
/// `Requires=`/`After=slipforkd.socket` make the service start with the socket (including on every
/// boot) and `[Install] WantedBy=multi-user.target` lets `systemctl enable --now
/// slipforkd.socket slipforkd.service` start it immediately. Exactly one `Type=` line is emitted
/// (`simple`); the daemon does not implement the `sd_notify` protocol, so `Type=notify` would
/// leave the unit stuck activating forever.
///
/// `RestrictAddressFamilies=` carries the allow-list in
/// [`SERVICE_RESTRICT_ADDRESS_FAMILIES`], whose doc comment explains why every family in it (notably
/// `AF_NETLINK`) is required for a world to boot inside the installed unit.
pub fn render_service_unit(spec: &UnitSpec) -> Result<String, EdgeError> {
    spec.validate()?;
    let executable = quote_argument(&spec.executable);
    let state_dir = quote_argument(&spec.state_dir);
    let runtime_dir = quote_argument(&spec.runtime_dir);
    let mut unit = String::from(
        "[Unit]\n\
         Description=Slipfork daemon (unprivileged HTTP/WS ingress, world registry, and daemon control)\n\
         Requires=slipforkd.socket\n\
         After=slipforkd.socket\n\
         \n\
         [Service]\n\
         Type=simple\n",
    );
    writeln!(unit, "User={}", spec.user).expect("writing into a String cannot fail");
    writeln!(
        unit,
        "ExecStart={executable} --state-dir {state_dir} --runtime-dir {runtime_dir}"
    )
    .expect("writing into a String cannot fail");
    unit.push_str(
        "CapabilityBoundingSet=\n\
         NoNewPrivileges=yes\n\
         ProtectSystem=strict\n",
    );
    writeln!(unit, "ReadWritePaths={state_dir} {runtime_dir}")
        .expect("writing into a String cannot fail");
    unit.push_str("PrivateTmp=yes\n");
    writeln!(
        unit,
        "RestrictAddressFamilies={SERVICE_RESTRICT_ADDRESS_FAMILIES}"
    )
    .expect("writing into a String cannot fail");
    unit.push_str(
        "LimitNOFILE=65536\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
    );
    Ok(unit)
}

/// Quote one `ExecStart=`/`ReadWritePaths=` path for systemd's unit parser when it needs it.
///
/// Paths without whitespace, quotes, or backslashes are emitted verbatim (the golden files pin the
/// common case); anything else is wrapped in double quotes with the two characters systemd treats
/// specially inside quotes escaped.
fn quote_argument(path: &std::path::Path) -> String {
    let text = path
        .to_str()
        .expect("UnitSpec::validate guarantees every path is UTF-8");
    if !text
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\\')
    {
        return text.to_owned();
    }
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        if character == '"' || character == '\\' {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt as _;

    use super::*;

    fn spec() -> UnitSpec {
        UnitSpec {
            user: "slipforktest".to_owned(),
            listen_addrs: vec![
                "127.0.0.1:80".parse().expect("loopback ingress"),
                "[::1]:80".parse().expect("loopback v6 ingress"),
            ],
            forward_port: 7080,
            executable: PathBuf::from("/usr/local/bin/slipforkd"),
            state_dir: PathBuf::from("/home/slipforktest/.local/share/slipfork"),
            runtime_dir: PathBuf::from("/run/user/1000/slipfork"),
        }
    }

    #[test]
    fn activation_order_lists_ingress_then_forward() {
        assert_eq!(
            spec().activation_order().collect::<Vec<_>>(),
            vec![
                "127.0.0.1:80".to_owned(),
                "[::1]:80".to_owned(),
                "127.0.0.1:7080".to_owned()
            ]
        );
    }

    #[test]
    fn paths_needing_quotes_are_quoted() {
        let mut spec = spec();
        spec.state_dir = PathBuf::from("/home/slipfork test/state");
        spec.executable = PathBuf::from("/opt/slipfork bin/slipforkd");
        let unit = render_service_unit(&spec).expect("a spacey path is renderable");
        assert!(
            unit.contains("ExecStart=\"/opt/slipfork bin/slipforkd\" --state-dir \"/home/slipfork test/state\""),
            "quoted paths must stay one argument each:\n{unit}"
        );
        assert!(
            unit.contains("ReadWritePaths=\"/home/slipfork test/state\" /run/user/1000/slipfork"),
            "ReadWritePaths must quote the same way:\n{unit}"
        );
    }

    #[test]
    fn non_utf8_paths_are_refused() {
        let mut spec = spec();
        spec.state_dir = PathBuf::from(std::ffi::OsString::from_vec(
            b"/home/slipfork/\xff/state".to_vec(),
        ));
        assert!(matches!(
            spec.validate(),
            Err(EdgeError::UnitInvalidPath { role, .. }) if role == "--state-dir"
        ));
    }

    #[test]
    fn percent_specifiers_in_unit_paths_are_refused() {
        for (role, mutate) in [("--state-dir", 0), ("--runtime-dir", 1), ("ExecStart", 2)] {
            let mut spec = spec();
            match mutate {
                0 => spec.state_dir = PathBuf::from("/tmp/%h/state"),
                1 => spec.runtime_dir = PathBuf::from("/tmp/%h/run"),
                _ => spec.executable = PathBuf::from("/tmp/%h/slipforkd"),
            }
            assert!(
                matches!(
                    spec.validate(),
                    Err(EdgeError::UnitInvalidPath { role: hit, .. }) if hit == role
                ),
                "{role} with a %-specifier must fail closed"
            );
            assert!(
                render_service_unit(&spec).is_err(),
                "{role} with a %-specifier must never render"
            );
        }
    }
}
