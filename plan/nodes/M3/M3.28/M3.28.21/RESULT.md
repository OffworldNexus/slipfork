# M3.28.21 RESULT — `daemon install` owns the socket handover (version-agnostic, ownership-proven)

- **Status:** `done` — every listed gate green (§4); the acceptance `grep` is green in the shared
  working tree with `M3.28.12`'s harness cleanup (§5).
- **Node:** `M3.28.21` (`release/0.1.0`, base `9e392f6`, attempt 3/3)
- **Paths:** `crates/slipfork/src/{daemon_unit_install.rs,main.rs}`,
  `crates/slipfork/tests/critical_cli.rs`, plus the approved widening
  `crates/slipfork-engine/src/linux_cow.rs`. **`crates/slipfork/src/lib.rs` is not touched** (it
  carries `M3.28.7`'s in-flight `pub mod status;`).

## 1. What changed

### The audited syscall home (`slipfork-engine/src/linux_cow.rs`)

- `peer_pid(&UnixStream)` — `getsockopt(SO_PEERCRED)`, now through `checked_peer_pid`, which
  **refuses `pid <= 0`** (`kill(0, …)` would address the caller's whole process group; a peer
  outside this PID namespace reports `0`). The doc states the snapshot TOCTOU: the number can be
  recycled after the call, so a caller that signals must pin it first.
- `open_pidfd(pid)` — `pidfd_open(2)`, giving a descriptor that names **that** process.
- `signal_pidfd(pidfd, signal)` — `pidfd_send_signal(2)`, the only way this node signals: a
  recycled pid is unreachable through the fd.
- Tests: `peer_pid` on a `socketpair`, `a_peer_pid_of_zero_or_less_is_refused`, and
  `a_pidfd_signals_exactly_the_pinned_process` (a real `sleep` child is SIGTERM'd and reaped; the
  same fd then answers `ESRCH` instead of retargeting).

### The one handover (`slipfork::daemon_unit_install`)

`release_running_daemon` stays deleted. The port is now **one adapter, one socket path**:

| Verb | Effect |
|---|---|
| `stop_units(&[ManagedUnit])` | `systemctl show --property=LoadState --value`; `not-found` skipped, anything else `systemctl stop` (no-op when inactive) — names come from `ManagedUnit::unit_name()` |
| `is_served(runtime_dir)` | one connect, dropped at once: the bounded wait's only question |
| `shutdown(runtime_dir)` | the one `DaemonRequest::Shutdown` round trip |
| `socket_holder(runtime_dir)` | the one ownership probe (below) |
| `signal_proven(&OwnedFd, ProcessSignal)` | `pidfd_send_signal(2)` through the pid that was proven |

`release_daemon(installer, clock, runtime_dir)` is a single ordered function:

1. stop the units it writes;
2. `shutdown` + bounded await of the socket's disappearance;
3. anything that still answers is proved by `probe_socket_holder` — `SO_PEERCRED` pid, then the
   **basename** of `/proc/<pid>/exe` (`slipforkd`, tolerating the ` (deleted)` link left by an
   upgrade), then `open_pidfd` — and only then `SIGTERM` → grace → `SIGKILL`, each through the
   pidfd;
4. `Foreign`, `Unprovable` and `Absent` are **not** release decisions: each re-probes `is_served`
   and refuses in one line if the socket still answers. `Foreign` names the path, the pid and
   `sudo kill <pid>`; `Unprovable`/`Absent` name the path and
   `sudo systemctl stop slipforkd.socket slipforkd.service` — never `sudo kill 0`.

`probe_socket_holder(runtime_dir)` lives in this module (not in `main.rs`) because it is pure over
an explicit runtime dir and is exercised end to end by the tests; `OsUnitInstaller` delegates to it.
`HandoverClock: MonotonicClock` stays the bounded-wait port (production is the engine's
`SystemClock`), and the budget is now **one** public pair — `DAEMON_GRACE` / `DAEMON_POLL` — which
`main.rs`'s auto-spawn wait also uses (`SPAWN_TIMEOUT`/`SPAWN_POLL` deleted). The unreachable
non-absolute-runtime-dir guard and its test are gone.

No re-exports were added: `install`, `release_daemon`, the port and `SocketHolder` are the public
surface, and `critical_cli` imports `SystemClock`/`ProcessSignal` from `slipfork_engine` directly.

## 2. Tests (scripted installer, one real proof; no real kill)

`cargo test -p slipfork --lib daemon_unit_install` — 15 tests, 7 of them the handover:

- **no holder** ⇒ one `stop_units`, zero signals, zero waits;
- **typed daemon** (accepts `shutdown`) ⇒ released with **zero** signals, no grace consumed;
- **foreign holder proven `slipforkd`** ⇒ exactly `[Terminate, Kill]` (both through the scripted
  pidfd) and the fake clock waited `>= DAEMON_GRACE`;
- **unowned holder** (`Foreign { pid: 777 }`) ⇒ exactly one line naming the socket path, `pid 777`
  and `sudo kill 777`, **zero** signals;
- **a probe that cannot name the holder** (`Absent` *and* `Unprovable`, socket still answering) ⇒
  refused in one line with the manual command and **zero** signals — the BLOCKER‑1 regression;
- **a real listener that is not `slipforkd`** ⇒ the real `SO_PEERCRED` + `/proc/<pid>/exe` proof
  reports `Foreign` for this real test process, and `release_daemon` refuses with the real pid and
  `sudo kill <pid>`, **zero** signals.

The install tests keep their byte/idempotency/order assertions; `first_run…` still pins
`stop_units(...) < enable_now(...)` and exactly `[Socket, Service]`.

`critical_cli`'s `CapturedInstaller` was updated to the new port surface (five no-op verbs; it
answers `Absent`) and its local `ingress_hint_line` now renders
`slipfork::status::ingress_hint`, so the `:80` line is never re-typed. **No assertion was weakened
or added there**, and the node's "extend `critical_cli`" budget was not used: the real binary cannot
install `/etc` in a test.

## 3. Idempotency

No holder ⇒ one `stop_units` (a `systemctl stop` no-op on an inactive unit) and nothing else; a
re-run with identical bytes still prints `[ok] unchanged`. A run over active units stops and
re-enables them — invariant 1a's order, restored by `enable --now`.

## 4. Gates (exact transcripts)

```console
$ cargo test -p slipfork-engine --lib
test result: ok. 69 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test -p slipfork --lib daemon_unit_install
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 58 filtered out

$ cargo test -p slipfork --test critical_cli -- --test-threads=1
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 104.99s

$ RUSTDOCFLAGS='-D warnings' cargo doc -p slipfork --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.41s

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.56s

$ cargo fmt --all -- --check
fmt-clean

$ ! grep -rn 'ss -xlpnH\|render_daemon_handover' tools/ && echo harness-has-no-lifecycle
harness-has-no-lifecycle
```

Guard/plan gates: see §6.

## 5. Acceptance item 3 — `M3.28.12`'s cleanup, green in the shared tree

The renderer is gone and `tools/do_vm.py` calls `slipfork daemon install` — that is `M3.28.12`'s
half of the split (node invariant 4), landed in the same working tree while this node ran; `tools/`
is not in this node's paths. At base `9e392f6` the grep printed nothing and
`render_daemon_handover` lived at `tools/do_vm.py:521`.

## 6. Repository gates

```console
$ uv run --no-project tools/critical_guard.py --base 9e392f6 --node M3.28.21
critical-guard: 9e392f6..working tree, 50 changed file(s)
  M  crates/slipfork/tests/critical_cli.rs  [authorized by crates/slipfork/tests/**]
critical-guard: OK: authorized by M3.28.21

$ uv run --no-project tools/guard.py
guard: OK (590 file(s) checked)

$ uv run --no-project tools/plan.py validate
plan OK
```

The full `make check` also passed on this tree (`make-check rc=0`; slipfork lib `73 passed`,
`critical_cli` `21 passed`, rustdoc `-D warnings`, `fmt`, `zensical --strict`, docs honesty).

## 7. Attempt history

1. `blocked` / `DIRECTION_CHALLENGE`: `SO_PEERCRED` had no stable safe API and every in-scope file
   is `#![forbid(unsafe_code)]`.
2. Option 1 opened `crates/slipfork-engine/src/linux_cow.rs`; the gate then found two safety
   BLOCKERs (a `NotFound` `/proc` read treated as "released"; `SIGKILL` sent to a pid captured 15 s
   earlier).
3. This attempt: pidfd-pinned signals, a tri-state proof that re-probes before trusting "absent",
   `peer_pid` refusing pid 0, one probe through the port, one shared budget, no `lib.rs` edit.
