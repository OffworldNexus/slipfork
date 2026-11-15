---
id: M2.3
title: "slipfork-world: WorldOrigin plus deterministic <=31-byte WeaverServiceName derivation"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M2.2]
paths: ["crates/slipfork-world/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T18:38"
---

# Goal

Land `slipfork-world`'s M2 domain additions — the single-origin `WorldOrigin` type and the deterministic `<= 31`-byte Tunnel Weaver service label — and clear the M1.6 `slipfork-world` follow-ups, without any OS socket/FD type leaking into the crate.

# Context

- `crates/slipfork-world` is the pure identity crate (`serde` + `thiserror`, no OS); `LocalHostRoute` is already the sole constructor of local origins and `validate_fork_name` already reserves the `test-<u32>` prefix.
- Tunnel Weaver's service field is `^[a-z0-9]+(-[a-z0-9]+)*$` capped at **31 bytes** (`SERVICE_NAME_MAX`, `weaver-proto/src/control.rs:97`), and the combined `<person>-<machine>-<service>` label is exactly 63 bytes worst case, so a valid 31-byte service alone is length-safe. There is **no** truncation helper upstream: Slipfork must supply it (`docs/research/tunnel-weaver-and-systemd-facts.md` §1.3).
- M1.6 follow-ups in scope: #1 (duplicate in-module route test), #2 (`is_reserved_test_slot_label` visibility), #22 (`pub mod conformance` test double in the shipped API), #23 (`LocalHostRoute::parse` triple construction), #27 (`is_lease_active` `pub` only for its own test).

# Design / Invariants

1. **`WorldOrigin` (`ADR-0005` §1, `H-008`, `H-010` #2):** `enum WorldOrigin { Local { url }, Shared { url } }` with serde `#[serde(tag = "kind", rename_all = "snake_case")]`, `WorldOrigin::url(&self) -> &str`, and fail-closed `Deserialize` rejecting empty or non-`http(s)` URLs. `WorldOrigin::local(route: &LocalHostRoute, ingress_port: u16) -> Result<Self, WorldError>` (`route.snow_domain()` must be `None`) is the **sole** constructor of `Local`: it renders `http://<host>` when `ingress_port == 80` and `http://<host>:<ingress_port>` otherwise. `WorldOrigin::shared(url)` mints `Shared` from a validated lease URL.
2. **`derive_weaver_service_name(full_label: &str) -> Result<WeaverServiceName, WorldError>`:** normalize to lowercase dash-separated runs; if the result is `<= 31` bytes it is used verbatim; otherwise `prefix` is truncated to `31 - 1 - HASH_LEN`, trailing dashes are stripped in a loop, an empty prefix drops the separator entirely, and the result is `<prefix>-<hash>` (or `<hash>`) with `HASH_LEN = 12` lowercase hex chars of SHA-256 over the **full** normalized label (so two different full labels never share a suffix by construction, and the function is idempotent/stable across runs and hosts).
   **Suffix-collision guard (mandatory):** a normalized label that is already `<= 31` bytes but whose tail matches the truncated-suffix shape `-[0-9a-f]{12}$` could be byte-identical to the output produced for some longer label, so it is routed through the same `<prefix>-<sha256_hex12>` path instead of being used verbatim. A short crafted input can therefore never collide with a truncated one, and the mapping stays a pure function of the full label alone.

3. **`WeaverServiceName` newtype** validates charset `^[a-z0-9]+(-[a-z0-9]+)*$` and `len() <= 31` in its only constructor; `Display`/`as_str` exist, `Deserialize` fails closed.
4. **Follow-up cleanups:** delete the duplicated in-module port test and keep the overflow case in the route suite (#1); make `is_reserved_test_slot_label` private (#2); move the in-memory `TunnelRegistrar` double out of the production library into `crates/slipfork-world/tests/support/mod.rs` and drop `pub mod conformance` from the shipped API (#22, #27); collapse `LocalHostRoute::parse`'s single-label branch to resolve-then-construct once (#23).
5. Zero `std::net`, zero `std::fs`, zero `std::env` in the crate; no `match kind` ladder added.

# Acceptance

```bash
# 1. Unit + critical suites (31/32-byte boundary, dash stripping, empty prefix, collision-freedom).
cargo test -p slipfork-world --all-targets
cargo test -p slipfork-world --test critical_world weaver
cargo test -p slipfork-world --test critical_world world_origin

# 2. The truncated-suffix shape can never be forged by a short label.
cargo test -p slipfork-world --test critical_world weaver_collision

# 3. The test double is no longer part of the shipped API.
! rg -q 'pub mod conformance' crates/slipfork-world/src
! rg -q 'pub fn is_reserved_test_slot_label' crates/slipfork-world/src

# 4. Purity + lint gates.
! rg -q 'std::net|std::fs|std::env' crates/slipfork-world/src
cargo clippy -p slipfork-world --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: a 32-byte label truncates to exactly 31 bytes with a fixed suffix, `a-` / `a--b` style joins are impossible, and the derived name matches the 31/32-byte boundary cases pinned in `critical_world.rs`, and a 31-byte label shaped like `foo-0123456789ab` is rewritten through the hash path rather than used verbatim. `world_origin` critical tests assert both `ingress_port == 80` port omission and `ingress_port != 80` `:<port>` formatting.
