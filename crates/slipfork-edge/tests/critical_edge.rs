//! Protected critical tests for `slipfork-edge` (`M2.8`, `ADR-0008` §5, `H-011` #4).
//!
//! Every activation assertion is driven through a **real fd-3 handover**: the parent binds a
//! listening socket, hands it to a child process on `fd 0`, and the child's `sh` wrapper
//! duplicates it to `fd 3` before `exec` and exports `LISTEN_PID=$$ LISTEN_FDS=<n>` — exactly the
//! descriptor layout `systemd` produces (`docs/research/tunnel-weaver-and-systemd-facts.md` §5.2),
//! and the child proves the adopted descriptor is non-blocking.
//!
//! The registry, ingress, proxy, and forward paths need a live world engine, so they run in
//! `critical_edge_live.rs` (`make rs-test-live`) over a real `BtrfsEngine`. This binary stays
//! engine-free and runs on any host (`make rs-test`).

#![forbid(unsafe_code)]

use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use slipfork_edge::{
    EdgeError, ListenConfig, UnitSpec, adopt_systemd_listener, render_service_unit,
    render_socket_unit, resolve_listener_set,
};
use slipfork_wire::control::{
    DaemonStatus, IngressInfo, WorldProcessReport, WorldServiceSummary, WorldState, WorldSummary,
};
use slipfork_world::{
    BaseLineage, CacheOutcome, RootfsLineage, SeedLineage, ServiceImageLineage, ServiceKind,
    ServiceState, WorldLineage, WorldServiceInfo,
};
use slipfork_world::{LocalHostRoute, ProjectSlug, WorldOrigin, WorldTarget};

const PROBE_MODE_ENV: &str = "SLIPFORK_EDGE_PROBE";
const PROBE_STATE_ENV: &str = "SLIPFORK_EDGE_STATE_DIR";
const PROBE_READY_ENV: &str = "SLIPFORK_EDGE_READY_FILE";
const PROBE_PORT_ENV: &str = "SLIPFORK_EDGE_EXPECTED_PORT";

const CHILD_ARGS: &str = "--exact activation_probe_child --test-threads=1 --nocapture";

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

fn cached_test_dir(label: &str) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME is required for the edge test scratch space");
    // Short and unique: `<state_dir>/http-ingress.sock` must fit the 107-byte AF_UNIX budget.
    let dir = PathBuf::from(home)
        .join(".cache")
        .join("sfedge")
        .join(format!("{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the edge test state directory");
    dir
}

fn listen_local() -> ListenConfig {
    ListenConfig {
        addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        forward_port: None,
    }
}

// ---------------------------------------------------------------------------------------------
// Real fd-3 activation handover
// ---------------------------------------------------------------------------------------------

// ---------------------------------------------------------------------------------------------
// Real fd-3 activation handover
// ---------------------------------------------------------------------------------------------

/// Build the `sh` wrapper: duplicate the inherited socket onto `fd 3` (`fd 3..=n+2` when several
/// descriptors are requested), export the systemd handover variables, and re-exec this test binary
/// as the single `activation_probe_child` test.
fn probe_script(prefix: &str) -> String {
    format!("{prefix}; exec \"$0\" {CHILD_ARGS}")
}

fn probe_command(prefix: &str, mode: &str, state: &Path, port: Option<u16>) -> Command {
    let executable = std::env::current_exe().expect("current test executable");
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(probe_script(prefix))
        .arg(executable)
        .env(PROBE_MODE_ENV, mode)
        .env(PROBE_STATE_ENV, state)
        .env(PROBE_READY_ENV, state.join("probe.ready"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(port) = port {
        command.env(PROBE_PORT_ENV, port.to_string());
    }
    command
}

/// Run a non-serving probe child to completion and fail with its output when it refuses to pass.
fn run_probe(prefix: &str, mode: &str, state: &Path) -> Output {
    let output = probe_command(prefix, mode, state, None)
        .stdin(Stdio::null())
        .output()
        .expect("spawn the activation probe child");
    assert_probe_ok(mode, &output);
    output
}

fn assert_probe_ok(mode: &str, output: &Output) {
    assert!(
        output.status.success(),
        "activation probe '{mode}' failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn activation_probe_child() {
    let Ok(mode) = std::env::var(PROBE_MODE_ENV) else {
        return;
    };
    match mode.as_str() {
        "descriptor_order" => probe_descriptor_order(),
        "pid_mismatch" => probe_pid_mismatch(),
        "zero_descriptors" => probe_zero_descriptors(),
        "broken_descriptor" => probe_broken_descriptor(),
        "no_listener" => probe_no_listener(),
        "listen_mode" => probe_listen_mode(),
        other => panic!("unknown probe mode '{other}'"),
    }
}

fn probe_expected_port() -> u16 {
    std::env::var(PROBE_PORT_ENV)
        .expect("probe expected port")
        .parse()
        .expect("numeric probe port")
}

fn assert_would_block(listener: &TcpListener, role: &str) {
    let err = listener
        .accept()
        .expect_err("a non-blocking adopted listener with no pending connection must not accept");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::WouldBlock,
        "adopted {role} must be non-blocking"
    );
}

fn probe_descriptor_order() {
    let expected_port = probe_expected_port();
    let listeners = adopt_systemd_listener()
        .expect("adoption succeeds")
        .expect("activation is present");
    assert_eq!(
        listeners.http_listeners.len(),
        2,
        "LISTEN_FDS=3 keeps two leading HTTP listeners"
    );
    for (index, listener) in listeners.http_listeners.iter().enumerate() {
        assert_would_block(listener, &format!("HTTP ingress listener {index}"));
    }
    let forward = listeners
        .forward_listener
        .as_ref()
        .expect("the trailing LISTEN_FDS=3 descriptor is the forward listener");
    assert_would_block(forward, "forward listener");
    assert_eq!(
        listeners.http_addrs().expect("bound port")[0].port(),
        expected_port
    );
    assert_eq!(
        forward.local_addr().expect("forward address").port(),
        expected_port
    );
}

fn probe_pid_mismatch() {
    let listen = listen_local();
    let err = resolve_listener_set(Some(&listen))
        .expect_err("a foreign LISTEN_PID must be refused, never replaced by a self-bind");
    match err {
        EdgeError::ActivationMismatch {
            listen_pid,
            self_pid,
        } => {
            assert_eq!(listen_pid, "1");
            assert_eq!(self_pid, std::process::id());
        }
        other => panic!("expected ActivationMismatch, got {other:?}"),
    }
}

fn probe_zero_descriptors() {
    let err = adopt_systemd_listener().expect_err("LISTEN_FDS=0 advertises no listener");
    assert!(
        matches!(&err, EdgeError::ActivationBroken { reason } if reason.contains("LISTEN_FDS=0")),
        "expected ActivationBroken about LISTEN_FDS=0, got {err:?}"
    );
}

fn probe_broken_descriptor() {
    // The activation variables alone select activation mode: a descriptor that is not a socket is
    // fatal even though a usable `--listen` address was supplied, so a broken handover can never
    // degrade into an unrelated self-bind.
    let listen = listen_local();
    let err = resolve_listener_set(Some(&listen))
        .expect_err("a broken activation environment must never fall back to --listen");
    assert!(
        matches!(&err, EdgeError::ActivationBroken { reason } if reason.contains("fd 3")),
        "expected ActivationBroken naming fd 3, got {err:?}"
    );
}

fn probe_no_listener() {
    assert!(
        adopt_systemd_listener()
            .expect("absent activation is not an error")
            .is_none(),
        "no activation variables means no adopted listener set"
    );
    let err = resolve_listener_set(None).expect_err("startup must fail closed without a listener");
    assert!(matches!(err, EdgeError::NoListener), "got {err:?}");
}

fn probe_listen_mode() {
    let without_forward = resolve_listener_set(Some(&listen_local())).expect("bind --listen");
    assert_eq!(without_forward.http_listeners.len(), 1);
    assert!(
        without_forward.forward_listener.is_none(),
        "--listen without --forward-port must not bind a forward listener"
    );
    assert_ne!(
        without_forward.http_addrs().expect("bound port")[0].port(),
        0
    );
    assert_would_block(
        &without_forward.http_listeners[0],
        "--listen HTTP ingress listener",
    );

    let with_forward = resolve_listener_set(Some(&ListenConfig {
        addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        forward_port: Some(0),
    }))
    .expect("bind --listen --forward-port");
    let forward = with_forward
        .forward_listener
        .as_ref()
        .expect("forward listener");
    assert_eq!(
        forward.local_addr().expect("forward address").ip(),
        Ipv4Addr::LOCALHOST,
        "the planned forward listener is loopback-only"
    );
    assert_ne!(
        with_forward.http_addrs().expect("http port")[0].port(),
        forward.local_addr().expect("forward address").port()
    );
}

#[test]
fn activation_descriptor_order_splits_forward_listener() {
    let state = cached_test_dir("activation-order");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind the probe listener");
    let port = listener.local_addr().expect("probe address").port();
    // `UnitSpec`'s order is `127.0.0.1:80`, `[::1]:80`, `127.0.0.1:<forward_port>`; the role
    // assignment must not inspect ports, so three duplicated descriptors prove the split.
    let prefix = "exec 3<&0 4<&0 5<&0; LISTEN_PID=$$; LISTEN_FDS=3; export LISTEN_PID LISTEN_FDS";
    listener
        .set_nonblocking(false)
        .expect("start blocking like systemd");
    let output = probe_command(prefix, "descriptor_order", &state, Some(port))
        .stdin(Stdio::from(OwnedFd::from(listener)))
        .output()
        .expect("spawn the descriptor-order probe");
    assert_probe_ok("descriptor_order", &output);
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn activation_listen_mode_without_forward_port() {
    let state = cached_test_dir("activation-listen");
    run_probe("unset LISTEN_PID LISTEN_FDS", "listen_mode", &state);
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn fail_closed_pid_mismatch_never_falls_back() {
    let state = cached_test_dir("fail-closed-pid");
    run_probe(
        "exec 3<&0; LISTEN_PID=1; LISTEN_FDS=1; export LISTEN_PID LISTEN_FDS",
        "pid_mismatch",
        &state,
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn fail_closed_zero_descriptors() {
    let state = cached_test_dir("fail-closed-zero");
    run_probe(
        "LISTEN_PID=$$; LISTEN_FDS=0; export LISTEN_PID LISTEN_FDS",
        "zero_descriptors",
        &state,
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn fail_closed_non_socket_descriptor() {
    let state = cached_test_dir("fail-closed-broken");
    run_probe(
        "exec 3</dev/null; LISTEN_PID=$$; LISTEN_FDS=1; export LISTEN_PID LISTEN_FDS",
        "broken_descriptor",
        &state,
    );
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn fail_closed_no_listener_at_all() {
    let state = cached_test_dir("fail-closed-none");
    run_probe("unset LISTEN_PID LISTEN_FDS", "no_listener", &state);
    let _ = fs::remove_dir_all(&state);
}

/// The lineage of a world that recorded no boot fact: every fact is `None`, `services` is empty,
/// so a renderer prints `-`.
fn empty_lineage() -> WorldLineage {
    WorldLineage {
        rootfs: None,
        services: Vec::new(),
        seed: None,
        base: None,
    }
}

// ---------------------------------------------------------------------------------------------
// slipforkd.socket / slipforkd.service non-template units (M2.10, ADR-0008 §5)
// ---------------------------------------------------------------------------------------------

/// Fixed test account for the golden pair: any real account name renders identically, and this one
/// can never be a systemd `%`-template.
const GOLDEN_USER: &str = "slipforktest";

fn golden_unit_spec() -> UnitSpec {
    UnitSpec {
        user: GOLDEN_USER.to_owned(),
        listen_addrs: vec![
            SocketAddr::from((Ipv4Addr::LOCALHOST, 80)),
            SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 80)),
        ],
        forward_port: 7080,
        executable: PathBuf::from("/usr/local/bin/slipforkd"),
        state_dir: PathBuf::from("/home/slipforktest/.local/share/slipfork"),
        runtime_dir: PathBuf::from("/run/user/1000/slipfork"),
    }
}

#[test]
fn unit_golden_renders_byte_exactly() {
    let spec = golden_unit_spec();
    spec.validate()
        .expect("the shipped production pair (127.0.0.1:80 + [::1]:80 + forward 7080) validates");
    let socket = render_socket_unit(&spec).expect("render slipforkd.socket");
    let service = render_service_unit(&spec).expect("render slipforkd.service");

    assert_eq!(
        socket,
        include_str!("golden/slipforkd.socket"),
        "slipforkd.socket must render byte-exactly"
    );
    assert_eq!(
        service,
        include_str!("golden/slipforkd.service"),
        "slipforkd.service must render byte-exactly"
    );

    // Exactly one service type, and it is the one the daemon actually implements.
    assert_eq!(
        service.matches("Type=").count(),
        1,
        "a unit must declare exactly one Type=:\n{service}"
    );
    assert!(service.contains("\nType=simple\n"), "got:\n{service}");

    // Explicit account, never a template specifier.
    assert!(
        service.contains(&format!("\nUser={GOLDEN_USER}\n")),
        "the service must name the resolved account:\n{service}"
    );
    assert!(
        !service.contains('%') && !socket.contains('%'),
        "non-template units must never contain a systemd %-specifier"
    );

    // Non-template hardening contract (ADR-0008 §5). The emitted `RestrictAddressFamilies=` line is
    // pinned by the golden fixture compared byte-exactly above, and its `AF_NETLINK` requirement is
    // pinned by name in `unit_service_sandbox_allows_the_netlink_family_world_init_needs`.
    for directive in [
        "\nCapabilityBoundingSet=\n",
        "\nNoNewPrivileges=yes\n",
        "\nProtectSystem=strict\n",
        "\nPrivateTmp=yes\n",
        "\nLimitNOFILE=65536\n",
    ] {
        assert!(
            service.contains(directive),
            "missing {directive:?}:\n{service}"
        );
    }
    assert!(
        service.contains(&format!(
            "\nExecStart={} --state-dir {} --runtime-dir {}\n",
            spec.executable.display(),
            spec.state_dir.display(),
            spec.runtime_dir.display()
        )),
        "ExecStart must be the executable plus both explicit directories:\n{service}"
    );
    assert!(
        service.contains(&format!(
            "\nReadWritePaths={} {}\n",
            spec.state_dir.display(),
            spec.runtime_dir.display()
        )),
        "ReadWritePaths must cover exactly the state and runtime directories:\n{service}"
    );

    // Activation fd order is the M2.8 contract: ingress listeners first, forward listener last.
    assert_eq!(
        socket
            .lines()
            .filter(|line| line.starts_with("ListenStream="))
            .collect::<Vec<_>>(),
        vec![
            "ListenStream=127.0.0.1:80",
            "ListenStream=[::1]:80",
            "ListenStream=127.0.0.1:7080",
        ],
        "ListenStream= order must match the activation fd order:\n{socket}"
    );
    assert!(socket.contains("\nBacklog=1024\n"), "got:\n{socket}");
    assert!(socket.contains("\nNoDelay=true\n"), "got:\n{socket}");
    assert!(
        socket.contains("\nService=slipforkd.service\n"),
        "the socket must name the service it activates:\n{socket}"
    );
}

/// The service sandbox must keep `AF_NETLINK`, or no world can boot through the installed unit.
///
/// `slipfork-world-init` brings loopback up with `ip link set lo up`, which opens an
/// `AF_NETLINK`/`NETLINK_ROUTE` socket. `M3.28.12` proved the failure live on the DO VM: with
/// `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX` the installed `slipforkd.service` reported
/// "slipfork-world-init: `ip link set lo up` failed with exit status: 1: Cannot open netlink
/// socket: Address family not supported by protocol" and every boot died, while the same engine
/// outside the unit (and with `AF_NETLINK` added via a `systemd-run` scope) returned `rc 0`.
/// This assertion names the family, so trimming it back fails here and not only on a VM; the exact
/// list and its order stay pinned by the golden fixture through `unit_golden_renders_byte_exactly`.
#[test]
fn unit_service_sandbox_allows_the_netlink_family_world_init_needs() {
    let service = render_service_unit(&golden_unit_spec()).expect("render slipforkd.service");
    let families = service
        .lines()
        .find_map(|line| line.strip_prefix("RestrictAddressFamilies="))
        .expect("the service must restrict address families");
    assert!(
        families
            .split_whitespace()
            .any(|family| family == "AF_NETLINK"),
        "the service sandbox must allow AF_NETLINK for `ip link set lo up`:\n{service}"
    );
}

#[test]
fn unit_validate_rejects_invalid_specs() {
    let base = golden_unit_spec();

    for user in ["", "   ", "%i", "%u", "dev user", "dev\tuser"] {
        let mut spec = base.clone();
        spec.user = user.to_owned();
        match spec.validate() {
            Err(EdgeError::UnitInvalidUser { user: refused }) => assert_eq!(refused, user),
            other => panic!("user {user:?} must be refused with UnitInvalidUser, got {other:?}"),
        }
        assert!(
            matches!(
                render_socket_unit(&spec),
                Err(EdgeError::UnitInvalidUser { .. })
            ),
            "an invalid user must never render a socket unit"
        );
        assert!(
            matches!(
                render_service_unit(&spec),
                Err(EdgeError::UnitInvalidUser { .. })
            ),
            "an invalid user must never render a service unit"
        );
    }

    let mut no_listener = base.clone();
    no_listener.listen_addrs.clear();
    assert!(matches!(
        no_listener.validate(),
        Err(EdgeError::UnitNoListener)
    ));
    assert!(matches!(
        render_socket_unit(&no_listener),
        Err(EdgeError::UnitNoListener)
    ));

    let mut colliding_v4 = base.clone();
    colliding_v4.forward_port = 80;
    match colliding_v4.validate() {
        Err(EdgeError::UnitPortCollision {
            forward_port,
            listener,
        }) => {
            assert_eq!(forward_port, 80);
            assert_eq!(listener, SocketAddr::from((Ipv4Addr::LOCALHOST, 80)));
        }
        other => panic!("a v4 forward/ingress collision must fail closed, got {other:?}"),
    }
    assert!(matches!(
        render_socket_unit(&colliding_v4),
        Err(EdgeError::UnitPortCollision { .. })
    ));

    let mut colliding_v6 = golden_unit_spec();
    colliding_v6.listen_addrs = vec![SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 80))];
    colliding_v6.forward_port = 80;
    match colliding_v6.validate() {
        Err(EdgeError::UnitPortCollision { listener, .. }) => {
            assert_eq!(
                listener,
                SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 80))
            );
        }
        other => panic!("a v6 forward/ingress collision must fail closed, got {other:?}"),
    }

    // A non-colliding forward port on an otherwise valid spec passes.
    let mut fine = base.clone();
    fine.forward_port = 7080;
    fine.validate()
        .expect("7080 does not collide with the :80 ingress");
}

#[test]
fn unit_privileged_80_is_allowed_and_renders() {
    // `systemd` (root, in the `.socket` unit) binds :80 and hands fd 3.. to the unprivileged
    // service, so a privileged loopback listener is a legitimate spec, never a validation error.
    let spec = golden_unit_spec();
    spec.validate()
        .expect(":80 on loopback must validate: systemd binds it, not slipforkd");
    let socket = render_socket_unit(&spec).expect("render the :80 socket unit");
    assert!(socket.contains("\nListenStream=127.0.0.1:80\n"), "{socket}");
    assert!(socket.contains("\nListenStream=[::1]:80\n"), "{socket}");

    // The "cannot bind unprivileged" hazard is a runtime diagnostic, never a spec rejection.
    match resolve_listener_set(Some(&ListenConfig {
        addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 80)),
        forward_port: None,
    })) {
        Ok(listeners) => {
            // A privileged runner (root in CI) may bind :80; that is the successful path.
            assert_eq!(listeners.http_addrs().expect("bound :80")[0].port(), 80);
        }
        Err(EdgeError::BindListener { addr, message }) => {
            assert_eq!(addr, "127.0.0.1:80");
            assert!(
                !message.is_empty(),
                "the diagnostic must carry the OS error, got '{message}'"
            );
        }
        Err(other) => panic!(
            "an unprivileged bind of :80 must surface BindListener, not an unrelated error: {other:?}"
        ),
    }
}

/// One `*.localhost` origin for the wire-shape tests (`:80` renders port-less).
fn test_origin(project: &str, port: u16) -> WorldOrigin {
    let project = ProjectSlug::parse(project).expect("valid project slug");
    let route = LocalHostRoute::new(project, WorldTarget::Canonical, None, &[])
        .expect("default-ingress route");
    WorldOrigin::local(&route, port).expect("local origin")
}

/// The one ingress fixture: the `:80` model bind the daemon composes.
fn ingress_info(port: u16) -> IngressInfo {
    IngressInfo::new(vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))]).expect("valid ingress")
}

/// The one service-row fixture: a wire row with no engine-measured facts yet.
fn service_row(
    world: &str,
    id: &str,
    kind: ServiceKind,
    state: ServiceState,
) -> WorldServiceSummary {
    WorldServiceSummary {
        world: world.to_owned(),
        info: WorldServiceInfo {
            id: id.to_owned(),
            kind,
            state,
            pid: None,
            port: None,
            uptime_secs: None,
        },
    }
}

const FULL_DIGEST: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

#[test]
fn ingress_info_round_trips_port_80_and_ephemeral_binds() {
    // Socket activation: `127.0.0.1:80` and `[::1]:80`; the order of `addrs` is non-semantic.
    let eighty: Vec<SocketAddr> = vec![
        "127.0.0.1:80".parse().expect("v4 port 80"),
        "[::1]:80".parse().expect("v6 port 80"),
    ];
    let info = IngressInfo::new(eighty.clone()).expect("a real :80 listener set");
    assert_eq!(info.port(), 80);
    assert_eq!(info.addrs(), eighty.as_slice());
    let mut reversed = eighty.clone();
    reversed.reverse();
    assert_eq!(
        IngressInfo::new(reversed)
            .expect("order is non-semantic")
            .port(),
        info.port()
    );
    let json = serde_json::to_value(&info).expect("serialize ingress info");
    assert_eq!(
        json,
        serde_json::json!({ "addrs": ["127.0.0.1:80", "[::1]:80"], "port": 80 })
    );
    assert_eq!(
        serde_json::from_value::<IngressInfo>(json).expect("round-trip ingress info"),
        info
    );

    // Ephemeral `--listen 127.0.0.1:0`: the bound port (never `0`) is the origin port.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind an ephemeral listener");
    let bound = listener.local_addr().expect("bound address");
    assert_ne!(bound.port(), 0, "a bound listener never reports :0");
    let ephemeral = IngressInfo::new(vec![bound]).expect("a real ephemeral bind");
    assert_eq!(ephemeral.port(), bound.port());
    assert!(
        ephemeral
            .addrs()
            .iter()
            .any(|addr| addr.port() == ephemeral.port())
    );
    let ephemeral_json = serde_json::to_value(&ephemeral).expect("serialize ephemeral ingress");
    assert_eq!(
        serde_json::from_value::<IngressInfo>(ephemeral_json).expect("ephemeral round-trip"),
        ephemeral
    );

    // Fail closed: no listener, an unbound `:0`, or listeners that disagree on the port.
    assert!(matches!(
        IngressInfo::new(Vec::new()).map_err(EdgeError::from),
        Err(EdgeError::IngressMissing)
    ));
    assert!(matches!(
        IngressInfo::new(vec!["127.0.0.1:0".parse().expect("port 0")]).map_err(EdgeError::from),
        Err(EdgeError::IngressPortInvalid { .. })
    ));
    assert!(matches!(
        IngressInfo::new(vec![
            "127.0.0.1:80".parse().expect("port 80"),
            "[::1]:8080".parse().expect("port 8080"),
        ])
        .map_err(EdgeError::from),
        Err(EdgeError::IngressPortInvalid { .. })
    ));
    // A forged frame whose `port` is not carried by `addrs` is a typed error, never a silent pass.
    let forged = serde_json::json!({ "addrs": ["127.0.0.1:80"], "port": 81 });
    let err = serde_json::from_value::<IngressInfo>(forged)
        .expect_err("an origin port absent from addrs must fail closed");
    assert!(
        err.to_string()
            .contains("not carried by any bound listener"),
        "the refusal must name the unbound port, got: {err}"
    );
}

#[test]
fn world_summary_parent_and_lineage_survive_serde() {
    let canonical = WorldSummary {
        world: "acme".to_owned(),
        parent: None,
        state: WorldState::Running,
        origin: test_origin("acme", 80),
        worktree: PathBuf::from("/repo"),
        lineage: empty_lineage(),
    };
    let fork = WorldSummary {
        world: "acme:feat-ux".to_owned(),
        parent: Some("acme".to_owned()),
        state: WorldState::Stopped,
        origin: test_origin("acme", 80),
        worktree: PathBuf::from("/repo/.worktrees/feat-ux"),
        lineage: empty_lineage(),
    };

    let canonical_json = serde_json::to_value(&canonical).expect("serialize canonical summary");
    assert_eq!(
        canonical_json.get("parent"),
        Some(&serde_json::Value::Null),
        "the canonical world's parent must be present and null, never omitted"
    );
    assert_eq!(
        canonical_json.get("lineage"),
        Some(&serde_json::json!({
            "rootfs": null,
            "services": [],
            "seed": null,
            "base": null,
        })),
        "an unknown lineage must carry every key with null/[] and no zeroed fact"
    );
    let fork_json = serde_json::to_value(&fork).expect("serialize fork summary");
    assert_eq!(
        fork_json.get("parent"),
        Some(&serde_json::Value::String("acme".to_owned()))
    );
    assert_eq!(
        serde_json::from_value::<WorldSummary>(fork_json).expect("fork summary round-trip"),
        fork
    );

    // Every `Option` fact present: nulls stay null, a measured 0 stays 0, enums keep their spelling.
    let measured = WorldLineage {
        rootfs: Some(RootfsLineage {
            digest: FULL_DIGEST.to_owned(),
            cache: CacheOutcome::Hit,
            millis: None,
        }),
        services: vec![ServiceImageLineage {
            id: "postgres:18".to_owned(),
            cache: CacheOutcome::Miss,
            millis: Some(12),
        }],
        seed: Some(SeedLineage {
            digest: None,
            rebuilt: Some(false),
            millis: None,
        }),
        base: Some(BaseLineage {
            key: FULL_DIGEST.to_owned(),
            millis: Some(0),
        }),
    };
    let measured_json = serde_json::to_value(&measured).expect("serialize measured lineage");
    assert_eq!(measured_json["rootfs"]["cache"], serde_json::json!("hit"));
    assert_eq!(measured_json["rootfs"]["millis"], serde_json::Value::Null);
    assert_eq!(
        measured_json["services"][0]["cache"],
        serde_json::json!("miss")
    );
    assert_eq!(
        measured_json["services"][0]["millis"],
        serde_json::json!(12)
    );
    assert_eq!(measured_json["seed"]["digest"], serde_json::Value::Null);
    assert_eq!(measured_json["seed"]["rebuilt"], serde_json::json!(false));
    assert_eq!(measured_json["base"]["millis"], serde_json::json!(0));
    assert_eq!(
        serde_json::from_value::<WorldLineage>(measured_json).expect("lineage round-trip"),
        measured
    );

    // `DaemonStatus` carries `ingress` and never a daemon-wide lineage or the retired port field.
    let status = DaemonStatus {
        pid: 1420,
        ingress: ingress_info(80),
        forward_port: Some(7080),
        worlds: vec![canonical, fork],
    };
    let status_json = serde_json::to_value(&status).expect("serialize daemon status");
    assert_eq!(status_json["ingress"]["port"], serde_json::json!(80));
    assert_eq!(
        status_json["ingress"]["addrs"],
        serde_json::json!(["127.0.0.1:80"])
    );
    assert!(
        status_json.get("lineage").is_none(),
        "there is no daemon-wide lineage: a world's facts live on its own summary"
    );
    assert!(
        status_json.get("http_ingress_port").is_none(),
        "the retired scalar port must not reappear beside the typed ingress"
    );
    assert_eq!(
        serde_json::from_value::<DaemonStatus>(status_json).expect("status round-trip"),
        status
    );
}

#[test]
fn world_process_reports_are_world_scoped_and_keep_stopped_rows() {
    let canonical = WorldProcessReport::new(
        WorldState::Running,
        empty_lineage(),
        vec![
            service_row("acme", "api", ServiceKind::Component, ServiceState::Running),
            service_row(
                "acme",
                "pg_databases.main",
                ServiceKind::BackingService,
                ServiceState::Stopped,
            ),
        ],
    )
    .expect("unique ids");
    let fork = WorldProcessReport::new(
        WorldState::Stopped,
        empty_lineage(),
        vec![service_row(
            "acme:feat",
            "api",
            ServiceKind::Component,
            ServiceState::Exited,
        )],
    )
    .expect("unique ids");

    // Two worlds never share a row: every row carries its own world id.
    assert!(
        canonical
            .services
            .iter()
            .all(|service| service.world == "acme")
    );
    assert!(
        fork.services
            .iter()
            .all(|service| service.world == "acme:feat")
    );
    for left in &canonical.services {
        assert!(
            !fork
                .services
                .iter()
                .any(|right| right.world == left.world && right.info.id == left.info.id),
            "a row may never appear in two worlds"
        );
    }

    // A stopped service keeps its row; its unknown facts stay `None` (rendered `-`, never `0`).
    let stopped = canonical
        .services
        .iter()
        .find(|service| service.info.id == "pg_databases.main")
        .expect("the stopped backing service keeps its row");
    assert_eq!(stopped.info.state, ServiceState::Stopped);
    assert_eq!(stopped.info.kind, ServiceKind::BackingService);
    assert_eq!(stopped.info.pid, None);
    assert_eq!(stopped.info.port, None);
    assert_eq!(stopped.info.uptime_secs, None);

    // The engine's `ServiceState` crosses the wire as its snake_case spelling, fail-closed.
    let json = serde_json::to_value(&canonical).expect("serialize the process report");
    assert_eq!(json["state"], serde_json::json!("running"));
    assert_eq!(json["services"][1]["state"], serde_json::json!("stopped"));
    assert_eq!(
        json["services"][1]["kind"],
        serde_json::json!("backing_service")
    );
    assert_eq!(json["services"][1]["pid"], serde_json::Value::Null);
    assert_eq!(
        serde_json::from_value::<WorldProcessReport>(json).expect("process report round-trip"),
        canonical
    );
    let mut unknown_state = serde_json::to_value(&canonical).expect("serialize the process report");
    unknown_state["services"][0]["state"] = serde_json::json!("zombie");
    assert!(
        serde_json::from_value::<WorldProcessReport>(unknown_state).is_err(),
        "an unknown service state must fail closed, never default"
    );

    // Duplicate ids are refused: `services[].id` stays a 1:1 join key.
    let duplicate = WorldProcessReport::new(
        WorldState::Running,
        empty_lineage(),
        vec![
            service_row("acme", "api", ServiceKind::Component, ServiceState::Running),
            service_row(
                "acme",
                "api",
                ServiceKind::BackingService,
                ServiceState::Running,
            ),
        ],
    )
    .expect_err("two rows with one id must fail closed");
    assert!(
        duplicate.to_string().contains("unique ids"),
        "the refusal must name the id-uniqueness rule, got: {duplicate}"
    );
}
