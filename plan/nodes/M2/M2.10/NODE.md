---
id: M2.10
title: "slipfork-edge: .socket/.service unit generator plus live systemd-socket-activate test"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M2.9]
paths: ["crates/slipfork-edge/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T20:49"
---

# Goal

`crates/slipfork-edge` part 3: the non-template unprivileged systemd unit generator (`slipforkd.socket` plus `slipforkd.service` rendered from typed values, golden-file tested) and a protected end-to-end conformance test that drives the daemon through a **real** `systemd-socket-activate -l 127.0.0.1:0` handover.

# Context

- Measured host facts (`docs/research/tunnel-weaver-and-systemd-facts.md` sections 5.1–5.4): unprivileged port 80 binding is impossible here, activation arrives as `LISTEN_PID`/`LISTEN_FDS` with the socket at `fd 3` in blocking mode, and the reference implementation ships golden unit files rendered by the same code that installs them (`weaver-server setup`).
- `ADR-0008` §5 fixes the non-template unit shape: explicit `User=<developer-user>` (resolved once at the CLI composition boundary — never a `%` template specifier or an empty `User=`), `ExecStart=<executable> --state-dir <state_dir> --runtime-dir <runtime_dir>`, empty `CapabilityBoundingSet=`, `NoNewPrivileges=yes`, `ReadWritePaths=<state_dir> <runtime_dir>` + `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX` (covering both `<runtime_dir>/daemon.sock` and `<state_dir>/http-ingress.sock`, `H-011` #4 Variant a), and `Service=slipforkd.service`; `H-008` fixes the planned static forward port the socket listens on.
- The CLI subcommand that prints these units (`slipfork daemon unit`) is wired in `M2.16`; this node ships only the pure renderer plus its golden tests.

# Design / Invariants

1. **Typed input, no stringly paths:** `UnitSpec { user: String, listen_addrs: Vec<SocketAddr>, forward_port: u16, executable: PathBuf, state_dir: PathBuf, runtime_dir: PathBuf }` with `validate()` failing closed on an empty or `%`-template `user` (`EdgeError::UnitInvalidUser`), an **empty** `listen_addrs` (`EdgeError::UnitNoListener`), or a `forward_port` colliding with one of `listen_addrs` (`EdgeError::UnitPortCollision`). It must **allow privileged ports**: the shipped `slipforkd.socket` legitimately lists `127.0.0.1:80` and `[::1]:80`, because PID 1 `systemd` (root) binds `:80` inside the `.socket` unit and passes `fd 3..` to the unprivileged `User=<developer-user>` `slipforkd.service`. `validate()` therefore never rejects a port number on a loopback address, and the "cannot bind unprivileged" hazard surfaces as a **runtime** diagnostic (`EdgeError::ActivationMismatch` / `EdgeError::NoListener`), never as a spec rejection.
2. **Pure renderer:** `render_socket_unit(&UnitSpec) -> String` and `render_service_unit(&UnitSpec) -> String`, no filesystem access; output is byte-exact against `crates/slipfork-edge/tests/golden/slipforkd.{socket,service}` using a fixed test username (for example `slipforktest`; golden files are additions, allowed by `tools/critical_guard.py`).
3. **Non-template unit contract (`ADR-0008` §5, `H-010` #1):** `[Socket]` emits `ListenStream=` in fixed deterministic order (`listen_addrs` first — e.g. `127.0.0.1:80`, `[::1]:80` — followed by `127.0.0.1:<forward_port>`) matching `M2.8`'s fd-order `adopt_systemd_listener`, plus `Backlog=1024`, `NoDelay=true`, `Service=slipforkd.service`; `[Service]` sets `Type=simple`, `User=<spec.user>`, `ExecStart=<executable> --state-dir <state_dir> --runtime-dir <runtime_dir>`, empty `CapabilityBoundingSet=`, `NoNewPrivileges=yes`, `ProtectSystem=strict`, `ReadWritePaths=<state_dir> <runtime_dir>`, `PrivateTmp=yes`, `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`, `LimitNOFILE=65536`.
4. **Live activation conformance (`critical_edge.rs`):** Re-executes the test probe under `systemd-socket-activate -l 127.0.0.1:0` (`LISTEN_FDS == 1` HTTP-only role assignment), asserts `LISTEN_PID == getpid()`, non-blocking socket adoption, and one real HTTP response (failing closed if `systemd-socket-activate` is absent).
5. No new dependencies (reuses the activation module from `M2.8`); no `std::env` outside the activation boundary and the test binary.

# Acceptance

```bash
# 1. Golden units render byte-exactly (non-template, explicit test username, zero % specifiers).
cargo test -p slipfork-edge --test critical_edge -- --nocapture unit_golden
! rg -q 'User=%' crates/slipfork-edge

# 2. Real systemd socket activation end-to-end.
cargo test -p slipfork-edge --test critical_edge -- --nocapture socket_activate

# 3. Invalid specs fail closed (empty/template user, empty listeners, port collision; :80 passes).
cargo test -p slipfork-edge --test critical_edge -- --nocapture unit_validate
cargo test -p slipfork-edge --test critical_edge -- --nocapture unit_privileged_80

# 4. Gates.
cargo test -p slipfork-edge --all-targets
cargo clippy -p slipfork-edge --all-targets -- -D warnings
uv run --no-project tools/critical_guard.py --help > /dev/null
make check
```

All commands exit `0`. Required adversarial cases: empty or `%`-prefixed `user` is refused with `UnitInvalidUser`; empty `listen_addrs` is refused with `UnitNoListener`; a `forward_port` equal to a listed listener port is refused with `UnitPortCollision`; the production pair `127.0.0.1:80` + `[::1]:80` **passes** `validate()` and renders byte-exactly against the golden files; and a real unprivileged attempt to bind `:80` yields the actionable runtime diagnostic instead of a silent self-bind.
