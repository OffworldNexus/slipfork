# Result M2.10

Status: **done**

`crates/slipfork-edge` now renders the non-template `slipforkd.socket` / `slipforkd.service` pair
from typed values (`ADR-0008` §5), validates it fail-closed, and pins both bytes and the real
`systemd-socket-activate` handover in the protected critical suite. The renderer is pure: no
filesystem, environment, clock, or account lookup; `User=` is always the explicit account the CLI
composition boundary resolved.

## What landed

| File | Change |
|---|---|
| `src/unit.rs` | `UnitSpec { user, listen_addrs, forward_port, executable, state_dir, runtime_dir }` + `UnitSpec::validate()` (empty/whitespace/`%` user -> `UnitInvalidUser`; non-UTF-8 or control-character path -> `UnitInvalidPath`; empty listeners -> `UnitNoListener`; forward port equal to an ingress port -> `UnitPortCollision`; privileged `:80` **allowed**). `render_socket_unit` emits `ListenStream=` in exact activation fd order (ingress addrs, then `127.0.0.1:<forward_port>`), `Backlog=1024`, `NoDelay=true`, `Service=slipforkd.service`; `render_service_unit` emits exactly one `Type=simple`, `User=<account>`, `ExecStart=<executable> --state-dir … --runtime-dir …`, empty `CapabilityBoundingSet=`, `NoNewPrivileges=yes`, `ProtectSystem=strict`, `ReadWritePaths=<state_dir> <runtime_dir>`, `PrivateTmp=yes`, `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`, `LimitNOFILE=65536`. Paths are quoted only when systemd needs it (space/quote/backslash). |
| `src/lib.rs` | `EdgeError::{UnitInvalidUser, UnitInvalidPath, UnitNoListener, UnitPortCollision}`; re-exports `UnitSpec`, `render_socket_unit`, `render_service_unit`; module docs replace the `M2.10` placeholder. |
| `src/activation.rs` | Stale "`M2.10` will render" comment now links the renderer that defines the fd order. |
| `tests/golden/slipforkd.{socket,service}` (new) | Golden pair for the shipped production spec (`slipforktest`, `/usr/local/bin/slipforkd`, `~/.local/share/slipfork`, `/run/user/1000/slipfork`, `127.0.0.1:80` + `[::1]:80`, forward `7080`). |
| `tests/critical_edge.rs` | New `unit_golden_renders_byte_exactly`, `unit_validate_rejects_invalid_specs`, `unit_privileged_80_is_allowed_and_renders`, and `socket_activate_systemd_handover` plus its `systemd_socket_activate` probe arm. The live test re-execs the probe under the **real** `systemd-socket-activate`, asserts `LISTEN_PID == getpid()`, `LISTEN_FDS == 1`, HTTP-only role assignment, `O_NONBLOCK` adoption (`/proc/self/fdinfo`), and one real 404 through `HttpIngress`; a missing tool is a panic with an actionable message, never a skip. |

## Acceptance (all commands exit 0)

### 1. Golden units render byte-exactly (explicit account, zero `%`)

```text
$ cargo test -p slipfork-edge --test critical_edge -- --nocapture unit_golden
running 1 test
test unit_golden_renders_byte_exactly ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 30 filtered out; finished in 0.00s

$ rg -q 'User=%' crates/slipfork-edge
rg rc=1        # no template specifier anywhere in the crate
```

The test pins both files against `include_str!("golden/…")`, then independently asserts: exactly one
`Type=` line and it is `simple`, `User=slipforktest`, no `%` in either unit, every hardening
directive, `ExecStart` with both explicit directories, `ReadWritePaths` = state + runtime only, and
`ListenStream=` order `127.0.0.1:80`, `[::1]:80`, `127.0.0.1:7080`.

### 2. Real `systemd-socket-activate` handover end-to-end

```text
$ cargo test -p slipfork-edge --test critical_edge -- --nocapture socket_activate
running 1 test
test socket_activate_systemd_handover ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 30 filtered out; finished in 0.02s
```

The child asserts `LISTEN_PID` names itself and `LISTEN_FDS == 1`, adopts `fd 3` non-blocking, serves
the ingress, and the parent receives `HTTP/1.1 404 Not Found` naming
`nope.localhost`; the parent also requires the tool's own `Communication attempt on fd 3.` line and
the child test's libtest output, so a silent no-op child cannot pass. Running the test with the tool
off `PATH` fails (rc 101) with
`systemd-socket-activate is required for the live activation conformance test but could not be spawned: No such file or directory (os error 2); install systemd (this test fails closed instead of skipping)`.

### 3. Invalid specs fail closed, `:80` passes

```text
$ cargo test -p slipfork-edge --test critical_edge -- --nocapture unit_validate
running 1 test
test unit_validate_rejects_invalid_specs ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 30 filtered out; finished in 0.00s

$ cargo test -p slipfork-edge --test critical_edge -- --nocapture unit_privileged_80
running 1 test
test unit_privileged_80_is_allowed_and_renders ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 30 filtered out; finished in 0.00s
```

Empty/whitespace/`%i`/`%u`/space/tab users are refused with `UnitInvalidUser` by `validate()` **and**
by both renderers; empty `listen_addrs` is `UnitNoListener`; forward `80` against a v4 or v6 `:80`
listener is `UnitPortCollision`; the production `127.0.0.1:80` + `[::1]:80` + forward `7080` spec
validates and renders. The privileged-80 test then attempts a real unprivileged bind: on this host it
surfaces `EdgeError::BindListener { addr: "127.0.0.1:80", .. }` (runtime diagnostic), and a
privileged runner simply succeeds.

### 4. Gates

```text
$ cargo test -p slipfork-edge --all-targets
test result: ok. 3 passed (lib) / 31 passed; 0 failed; 0 ignored

$ cargo clippy -p slipfork-edge --all-targets -- -D warnings
clippy rc=0   # only the pre-existing `unused dependency socket2` manifest warning

$ uv run --no-project tools/critical_guard.py --help > /dev/null
rc=0

$ make check
make check rc=0
```

Additional gates re-run for this node:

```text
$ cargo fmt --all -- --check                 -> rc=0
$ RUSTDOCFLAGS="-D warnings" cargo doc -p slipfork-edge --no-deps -> rc=0
$ uv run --no-project tools/guard.py         -> guard: OK (304 file(s) checked)
$ uv run --no-project tools/critical_guard.py --base ed975ce --node M2.10
critical-guard: OK: authorized by M2.10
$ systemd-analyze verify ./slipforkd.socket ./slipforkd.service
verify rc=0      # golden pair copied to /tmp with ExecStart=/bin/true (the repo ships no installed
                 # /usr/local/bin/slipforkd); every other byte identical
```

## Deviations and notes

1. **`-l 127.0.0.1:0` is rejected by systemd 257's tool.** Measured:
   `systemd-socket-activate --listen=127.0.0.1:0 /bin/true` ->
   `Failed to parse socket address "127.0.0.1:0": Invalid argument` (rc 1); `-l 127.0.0.1` (no port)
   fails identically. The test therefore reserves an ephemeral loopback port with
   `TcpListener::bind(("127.0.0.1", 0))`, drops it, and has the **real** tool bind that concrete port
   (`--listen=127.0.0.1:<port>`). The handover itself is untouched: real tool, real `fd 3`, real
   `LISTEN_PID`/`LISTEN_FDS`, real HTTP. A stolen port makes the tool exit and the test panic with
   its stderr.
2. **`--setenv` is mandatory.** `systemd-socket-activate` starts children from a minimal environment
   (measured: only `TERM`/`PATH`/`USER`/`HOME` plus `LISTEN_*` survive), so the probe mode and state
   directory cross the exec via the tool's own `--setenv=` flags.
3. **Non-blocking proof uses `/proc/self/fdinfo`.** The triggering connection is already queued when
   the child starts, so `accept()` cannot distinguish blocking from non-blocking; the child reads the
   descriptor's real flags (`O_NONBLOCK == 0o4000`) instead. The older fd-dup probes still use
   `WouldBlock`.
4. **Renderers return `Result<String, EdgeError>` and call `validate()` first** (the node sketch said
   `-> String`). A renderer that can emit an invalid unit if a caller forgets `validate()` is a
   fail-open API; validation is now impossible to skip, and no caller needs an infallible form.
5. **`[Install] WantedBy=sockets.target` on the socket unit only.** The documented
   `systemctl enable --now slipforkd.socket` requires an install section; the service is started by
   the socket (`Service=slipforkd.service`) and deliberately carries none. Every directive was
   accepted by `systemd-analyze verify`.
6. `UnitInvalidPath` (UTF-8, control-free paths) and space-quoting are fail-closed additions so a
   resolved `$HOME` containing spaces/newlines cannot corrupt the unit; both are unit-tested.
