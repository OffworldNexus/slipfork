---
id: M3.29.5
title: "slipfork auth <wss-url> saves the OSPP endpoint in <state_dir>/ospp.json (public metadata only) so pairing survives reboots"
kind: task
status: ready
owner: implementer
priority: 1
depends_on: [M3.29.3, M3.28.24]
paths:
  - "crates/slipfork/src/**"
  - "crates/slipfork-edge/src/**"
  - "crates/slipfork/tests/**"
  - docs/reference/cli.md
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-18T07:46"
updated: "2026-11-18T07:46"
---

# Goal

Make `slipfork auth <wss-url>` the durable pairing front door (owner's answer to `H-007`: *"Save on slipfork auth `<url>`"*): the endpoint is given once on the CLI, and after `pair_approved` `slipforkd` persists **public metadata only** to `<state_dir>/ospp.json` (`0600`) so pairing survives reboots and `daemon install` with **zero** systemd-unit edits.

# Context

`H-007` (closed by the owner): the OSPP endpoint had no durable source of truth — auto-spawn never received `--ospp-endpoint`, the units render no `Environment=`, and there is no `daemon stop`. Design of record: `<state_dir>/ospp.json`.

# Design / Invariants

1. **`slipfork auth <wss-url>`** takes the endpoint directly; `slipforkd --ospp-endpoint` becomes an **optional override** only (highest precedence, never required).
2. Precedence at daemon boot, one rule: `--ospp-endpoint` > `<state_dir>/ospp.json:endpoint` > none (unpaired/local-only). One resolver, logged once at boot (`ospp: endpoint <url> (flag|saved|unpaired)`).
3. On `pair_approved`, persist **atomically** (same-dir temp + `rename`, `0600`, directory `0700`) exactly `{ "endpoint": "<wss-url>", "account": "<name>", "machine": "<name>" }`. **Zero secrets, zero tokens, zero keys** — the device key stays where it is; nothing else may be added to this file.
4. Loading is fail-closed: an unreadable/malformed `ospp.json` prints one actionable line naming the path and stops (never silently unpaired); a `0644` file is corrected to `0600` or refused.
5. Pairing against an endpoint that differs from a saved one requires no flag — the CLI value wins for that pairing, and the saved file is replaced only after approval.
6. `slipfork auth` with no URL uses the flag > saved endpoint; with neither, it fails closed naming `slipfork auth <wss-url>`.
7. No ambient reads outside the composition root; the file IO lives in an adapter behind a narrow `PairingStore` (`load`/`save`) so tests inject a fake.

# Acceptance

```console
$ cargo test -p slipfork --lib auth
... test result: ok
$ cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1
... test result: ok
$ cargo run -q -p slipfork -- auth; echo "exit=$?"
slipfork: no OSPP endpoint — run `slipfork auth <wss-url>` once to save it
exit=1
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- `critical_ospp_daemon` proves the owner's exact path: `slipfork auth <ws-url>` against the reference server → `pair_approved` → `<state_dir>/ospp.json` exists `0600` with only `endpoint`/`account`/`machine` → **restart `slipforkd` with NO `--ospp-endpoint`** → `slipfork up` connects and resolves the remote secret (the in-memory-only assertion still holds: no secret bytes under `<state_dir>`).
- `RESULT.md` records the file's exact bytes (redacted account/machine names are fine) and the restart-without-flag transcript.
