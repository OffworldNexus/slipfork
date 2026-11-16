---
id: M2.22
title: "Isolated: concrete WeavedRegistrar IPC adapter and declared support promotion (gated on H-006)"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: [M2.20]
paths:
  - "crates/slipfork-edge/src/**"
  - "crates/slipfork-edge/tests/critical_edge.rs"
  - "crates/slipfork/src/**"
  - support/integrations.toml
  - "docs/support-matrix.md"
  - "docs/reference/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-16T06:34"
---

# Goal

Implement the concrete tunnel-registrar wire adapter inside `crates/slipfork-edge` and promote the tunnel integration in the support registry — **only** if the upstream IPC wire specification exists. When it does not, this node performs zero file edits and returns `STATUS: blocked`.

# Context

- The edge crate ships a `TunnelRegistrar` port plus an unavailable adapter that fails closed; the middleware that terminates tunnels forwards public hostnames to the daemon planned static port.
- `H-006` is an open human item: the upstream control IPC wire format has not been published. Iron Law #1 applies again: inventing a frame layout, a JSON shape, a length prefix, or an authentication handshake for a third-party socket is forbidden.
- This node is deliberately **terminal and isolated**: nothing depends on it, so a blocked result costs the milestone nothing. If the specification arrives, this is the only place that encodes it.
- `ADR-0007` and `H-009` Option B: the integration is omitted from `support/integrations.toml` until the adapter is user-reachable.

# Design / Invariants

1. **Precondition first.** Before any edit, check for a published wire specification: a recorded reference in the open human item, a vendored schema under `docs/research/`, or a documented public protocol description. Absent all three, stop.
2. **Blocked outcome is exact.** Zero file edits, zero registry changes, and a report of `STATUS: blocked` with `BLOCK_KIND: missing-input`, citing `H-006`. Never a guessed frame layout, never a protocol-shaped stub promoted to production, never a fake local tunnel that makes `share` appear to work.
3. **Adapter shape when unblocked.** One adapter implements `TunnelRegistrar` (`register` returning a lease with `public_url` and `lease_id`, plus revoke-on-drop) and owns every byte of framing, authentication, and reconnect policy. It is constructed only at the composition root with injected endpoint, credentials, and clock; no module-level state and no `std::env` reads outside that boundary.
4. **Failure semantics.** Registration fails closed with a typed error naming the cause (unreachable, rejected, expired); revocation is idempotent; a lease lost mid-session is surfaced to the CLI as a diagnostic, never masked by a re-registration loop that changes the public URL.
5. **Conformance.** `crates/slipfork-edge/tests/critical_edge.rs` gains a conformance suite driven by a recorded fixture of the published protocol plus a live smoke case that skips only when the operator explicitly disables it, and `WorldRegistry::share_world`/`unshare_world` are proven against the real adapter.
6. **Support promotion and isolation.** After conformance passes, move the integration to `declared` with a real `recipe_page`, a real `critical_test`, regenerate `docs/support-matrix.md` through the documented mutator, and keep `depends_on` at `M2.20` only.

# Acceptance

```bash
# 0. H-006 precondition. Prints exactly one verdict and always exits 0.
uv run --no-project python -c 'import pathlib; hits=[p.as_posix() for p in pathlib.Path("docs/research").glob("*weaved*ipc*")]; print("H-006: wire spec present at " + hits[0]) if hits else print("H-006: absent -> zero edits, STATUS: blocked (missing-input)")'

# Everything below runs ONLY when the verdict above is "wire spec present".
cargo test -p slipfork-edge --test critical_edge -- --nocapture registrar_conformance
cargo test -p slipfork-edge --test critical_edge -- --nocapture share_unshare
cargo test -p slipfork --test critical_cli -- --nocapture share
uv run --no-project tools/check_support.py --check
git diff --exit-code docs/support-matrix.md
make check
```

When the verdict is `absent`, the only acceptable outcome is a blocked report with zero file edits; the acceptance for a blocked run is the single verdict line above plus `git status --porcelain` reporting no change to any tracked file.
