---
id: M3.28.6
title: "slipfork daemon install: :80 socket activation, AppArmor profile, idempotent and fail-closed"
kind: task
status: done
owner: implementer
priority: 2
depends_on: [M3.28.3, M3.28.4, M3.28.17]
paths:
  - crates/slipfork/src/main.rs
  - crates/slipfork/src/daemon_unit_install.rs
  - crates/slipfork/src/cli.rs
  - crates/slipfork/src/apparmor.rs
  - crates/slipfork/src/user.rs
  - crates/slipfork/src/doctor.rs
  - crates/slipfork/src/lib.rs
  - crates/slipfork/src/layout.rs
  - crates/slipfork/src/vaults.rs
  - crates/slipfork/src/daemon.rs
  - crates/slipfork/src/bin/slipforkd.rs
  - "crates/slipfork/src/bin/git-remote-slipfork.rs"
  - "crates/slipfork/tests/**"
  - "crates/slipfork-edge/src/**"
  - "crates/slipfork-edge/tests/**"
  - docs/reference/cli.md
  - "docs/decisions/ADR-0011-*.md"
attempts: 4
max_attempts: 4
escalation: none
created: "2026-11-17T16:33"
updated: "2026-11-17T23:36"
---

# Goal

Ship `slipfork daemon install`: install `slipfork.socket` + `slipfork.service` + the AppArmor profile and `systemctl enable --now slipforkd.socket`, so `127.0.0.1:80` and `[::1]:80` are bound by systemd socket activation (no `CAP_NET_BIND_SERVICE`, no root daemon). Idempotent; fails closed with actionable text when unprivileged.

# Context

Fact 1: the daemon auto-spawns `--listen 127.0.0.1:0`, so `http://demo.localhost` was refused and every origin carried a port; `slipforkd.socket` was never installed. `docs/research/tunnel-weaver-and-systemd-facts.md` §5: no unprivileged component can bind 80 — a system `.socket` unit is the only legal path. `UnitSpec` already renders the three `ListenStream=` entries (`edge/src/unit.rs:121-179`) and `render_userns_profile(bin_dir)` the four-line profile (`src/apparmor.rs`).

# Design / Invariants

1. `daemon install [--user <ACCOUNT>] [--bin-dir <DIR>]... [--forward-port <PORT>]`: writes `/etc/systemd/system/slipforkd.{socket,service}` and `/etc/apparmor.d/slipfork` with the **exact bytes** `daemon unit` renders, then `systemctl daemon-reload`, `apparmor_parser -r /etc/apparmor.d/slipfork`, `systemctl enable --now slipforkd.socket slipforkd.service`.
2. Idempotent: rewrite a file only when its bytes differ; print `[wrote]`/`[ok] unchanged`/`[enabled]` per action; a second run performs zero writes.
3. Fail closed, no fallback: non-root prints exactly `slipfork: daemon install writes /etc and enables system units; run it as root (sudo slipfork daemon install --user <account>)` and changes nothing. Same-directory temp file + `rename` so a failure never truncates a unit. No `--force`, no user-unit alternative.
4. `--user` defaults to the invoking account and resolves that account's home → state/runtime dir through the existing account-database adapter (`src/user.rs`, fixed `PATH`, `env_clear`); unknown account or `%`-specifier fails closed. This removes the `getent passwd` + `sudo -u` detour (fact 10).
5. `daemon unit --bin-dir` is repeatable (default: this executable's dir); the AppArmor profile names every dir in one leading-slash brace expression and stays exactly four lines.
6. `doctor` gains an ingress line through a new `IngressProbe` capability (subprocess adapter over `systemctl is-active`/`show -p Listen`, wired only in `main.rs`, recording double in tests): `:80 active (port-less origins)` / `not installed (ephemeral port; run `slipfork daemon install`)` / `installed but inactive (run `sudo systemctl start slipforkd.socket`)`. `--json` carries the typed fields; `doctor` still exits correctly with no daemon and no unit.
7. All effects live in `crates/slipfork/src/daemon_unit_install.rs` behind a `UnitInstaller` trait (`write_file`, `read_file`, `run`) with `OsUnitInstaller` built in `main.rs`; no `std::fs`/`Command` in dispatch arms, no `unsafe`, no new dependency.
8. **One runtime dir, no client override** (`M3.28.1` R4): `--runtime-dir` is a `slipforkd`-only flag and the install writes units pinned to `<state_dir>/run`; delete it from the CLI's global options so a client can never point at a different `daemon.sock` than the installed unit. `--state-dir` stays global because both sides derive from it.
9. **Only `up`/`fork` may auto-spawn** (`M3.28.1` R5); the auto-spawn passes `--listen 127.0.0.1:0 --state-dir <state_dir> --runtime-dir <state_dir>/run`, so the spawned daemon and the installed unit agree on the socket path.
10. `docs/reference/cli.md` documents `daemon install` and `daemon unit --bin-dir`; the old three-step `| sudo tee` recipe is deleted in the same edit.

# Acceptance

```console
$ cargo test -p slipfork --lib
... test result: ok
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
... test result: ok
$ cargo run -q -p slipfork -- daemon unit --bin-dir /usr/local/bin --bin-dir /tmp/ux/bin --only apparmor | grep -c slipfork
1
$ [ "$(cargo run -q -p slipfork -- daemon unit --bin-dir /usr/local/bin --only apparmor | wc -l)" -eq 4 ] && echo four-lines
four-lines
$ cargo run -q -p slipfork -- --json doctor | grep -q '"ingress"' && echo ingress-in-json
ingress-in-json
$ ! grep -rn 'sudo tee' docs/reference/cli.md && echo recipe-gone
recipe-gone
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- Unit tests with the recording installer prove: non-root → exit 1, the exact one-line remedy, zero writes; identical bytes → zero rewrites; `enable --now` called once per run; rendered unit bytes byte-identical to `daemon unit`.
