---
id: M4
title: "Live OSPP v1 Cloud Pairing (slipfork auth) & CBOR STP v1 Polyglot Test Orchestrator (slipfork test) (0.2.0)"
kind: milestone
status: ready
priority: 4
depends_on: [M3]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-14T22:03"
updated: "2026-11-25T19:21"
---

# Goal

Deliver Slipfork **`0.2.0`** (`release/0.2.0`): live `OSPP v1` cloud pairing (`slipfork auth`) and the CBOR `STP v1` polyglot test orchestrator (`slipfork test`) built directly on `SnapshotStore` + `ForkLedger`:

1. **`slipfork auth` & live `OSPP v1` cloud integration (`specs/05-ospp-v1.md`):** re-enable `slipfork auth [<endpoint>]` (`ALTCHA+PoP` `#handle=` pairing, `0600` `<state_dir>/ospp.json`), daemon subscription attach with ID-only `claims`, in-memory `hpke-p256` `env_resolved`/`env_updated` restarts, and stateful HPKE `zstd` PG seed dump streaming into `SnapshotStore` — proven against Kerfufoo's live `OSPP v1` provider and the loopback reference server.
2. **CBOR `STP v1` test orchestrator (`slipfork test`):** author `specs/07-testing-protocol.md` first, then build `slipfork test` over `BuildGraph`/`SnapshotStore` (cached setup/fixture RO subvolumes) and `ForkLedger` (ephemeral warm `TestSlot` worlds at `test-<slot>.<world>.localhost`, `SLIPFORK_PROTOCOL_FD`, surviving `SLIPFORK_OUTPUT_DIR`, hermetic `ResourceMode::Test`, protocol-native coverage & `ReportRenderer` failure replay).
3. **Turnkey polyglot `STP v1` runners:** `pytest`, `vitest`, and `cargo nextest` adapters plus zero-plugin command execution proven on real Btrfs + `crun` lab E2E suites.

# Acceptance

- [ ] `specs/07-testing-protocol.md` accepted before runner implementation; zero vaporware in published `docs/` (`tools/check_docs_honesty.py`).
- [ ] `slipfork auth` + live `OSPP v1` secret push & sealed PG dump restore pass end-to-end with zero remote secrets written to disk.
- [ ] `slipfork test` warm `TestSlot` pool, `SLIPFORK_PROTOCOL_FD`, coverage gates, and runner suites pass `make check`, `make live-test`, `tools/guard.py`, and `tools/plan.py validate`.
