---
id: M2.1
title: "Author ADR-0008 (OSPP v1 WebSocket, tpmd P-256, ResourceStrategy, world UX) and align ADRs/ARCHITECTURE/target-DX"
kind: task
status: active
owner: architect
priority: 3
depends_on: []
paths: [ARCHITECTURE.md, "docs/decisions/**", "docs/design/**"]
attempts: 1
max_attempts: 3
escalation: architect
created: "2026-11-15T16:40"
updated: "2026-11-15T16:46"
---

# Goal

Author `docs/decisions/ADR-0008-ospp-v1-websocket-tpmd-resource-strategy-and-world-ux.md` and align `ADR-0001`/`ADR-0003`/`ADR-0005`/`ADR-0007`, `ARCHITECTURE.md`, and `docs/design/target-dx.md` so that M2's DI tree is complete, non-leaky, and vaporware-free. Published `docs/**` pages and `README.md` are NOT touched by this node.

# Context

- Primary-source facts live in `docs/research/tunnel-weaver-and-systemd-facts.md`: service label cap is **31 bytes** (`weaver-proto/src/control.rs:97 SERVICE_NAME_MAX = 31`, charset `^[a-z0-9]+(-[a-z0-9]+)*$`), device keys are **ECDSA P-256 + SHA-256**, `KeyId` **is** the SEC1 compressed 33-byte public key rendered `p256:<66 lowercase hex>`, signatures are raw fixed-width `r||s` (64 bytes), the transcript is `"weaver-mux-v1" || nonce_s(32) || nonce_c(32) || server_name`, pairing handles are 32 random bytes as 64 lowercase hex chars delivered only in the URL **fragment**, `tpmd` client candidates are `$XDG_RUNTIME_DIR/tpmd/tpmd.sock` then `/run/tpmd/tpmd.sock`, and systemd hands sockets over at `fd >= 3` in **blocking** mode with `LISTEN_PID` = the child PID.
- `H-008` (single-origin, `slipforkd` sole ingress) and `H-009` (Option B support tiers) are closed and must be reflected, not re-litigated. `H-004` (`incusd`) and `H-006` (`weaved` IPC) stay open and are only referenced as gates.
- `plan/nodes/M2/LOG.md` records a conductor directive of "64-char service label truncation"; the measured cap is **31** and this ADR supersedes that number (`docs/research/tunnel-weaver-and-systemd-facts.md:10`).

# Design / Invariants

1. **`OSPP v1` WebSocket replaces the REST sketch (delete, do not deprecate).** `ADR-0001` §3's `GET /v1/projects/{slug}/modes/{mode}/bundle` + `POST .../secrets:ensure` endpoints and the `OsppHttpProvider` name are deleted outright (workspace is `< 1.0.0`: zero compatibility burden). `ADR-0001` §3 keeps only: OSPP is a transport-neutral capability, `OsppClient` implements `SecretProvider`, and `ADR-0008` is normative for the wire.
2. **`ADR-0008` §auth:** subprotocol `ospp-v1`, server-speaks-first `CHALLENGE { nonce_s }`, client `HELLO { version: u16 = 1, key_id, nonce_c, sig }`, signed bytes exactly `"ospp-v1" || nonce_s(32) || nonce_c(32) || server_name` (server_name = the dialed hostname string, identical on both sides), server `WELCOME { params }` / `REJECT { UnknownKey | BadSignature | UnsupportedVersion { min, max } }`, bounded 16 KiB frames and a bounded first-message deadline. `DeviceKeySigner` is the DI port (`TpmdDeviceKeySigner` and `SoftwareP256Signer` adapters, both probing the two `tpmd` socket candidates in order and using the software key only when the agent is absent/refused, with an explicit diagnostic — never silent). TLS channel binding is documented as *not* enabled (same posture as the reference mux) with the exporter hook reserved.
3. **`ADR-0008` §capabilities:** (a) `vars::` + `secrets::` + `secrets.ensure` (`!!` write-once semantics, `put_if_absent`); (b) remote **resource coordinates** (`resources::s3_buckets.<id>.*` and friends) enabling a shared dev S3 bucket in local dev; (c) **streaming** PostgreSQL seed dumps (`pg_databases.<id>` metadata + binary `zstd` body, applied via `pg_restore` inside the world, cached by dump digest, materialized into the `base` CoW snapshot **before** `pre-deploy` migrations).
4. **`ADR-0008` §`[resources]` `ResourceStrategy`:** mode-aware defaults — `local` dev (`up`/`fork`) may use OSPP-offered remote S3 and defaults to pulling the team dev DB dump; `test` (`slipfork test`) is **hermetic by default** (ephemeral local S3 + clean `initdb -> migrate -> [fixtures.*]`, never mutating shared dev S3, never pulling dev dumps unless explicitly configured); offline laptops are 100% functional on the `0600` vault + local resources; Slipfork-assigned `WorldOrigin` always wins over `domains::`.
5. **`ADR-0008` §world UX:** first-class hook & migration failure debugging — live stdout/stderr streaming on a TTY, full un-truncated output + exact command + world ID on failure, failed worlds kept inspectable (never destroyed), and 1-line rescue commands (`slipfork hook run <hook_id> [--world <fork>]` with interactive TTY stdio for `pdb`, `slipfork logs [<fork>] [<component_or_hook>]`, `slipfork shell [<fork>]`); plus `slipfork db [<db_id>] [--world <fork>]`, `slipfork redis [<cluster_id>] [--world <fork>]`, `slipfork forward [--world <fork>] [<target>...]` (host `127.0.0.1:<port>` TCP bridge via `WorldEngine::dial_world_tcp`, printing ready-to-paste GUI URIs), and the `WorldEngine` streaming/interactive-TTY exec contracts.
6. **`ADR-0008` §unprivileged `slipforkd`:** systemd socket activation with `User=<user>`, empty `CapabilityBoundingSet=`, `LISTEN_PID == getpid()`, `LISTEN_FDS` sockets starting at `fd >= 3`, mandatory immediate `set_nonblocking(true)` before adoption, fail-closed on a mismatched `LISTEN_PID`/absent listener (never a silent self-bind), `--listen <addr>` for unprivileged runs (tested with `systemd-socket-activate -l 127.0.0.1:0`), and a `slipfork daemon unit` generator emitting the `.socket`/`.service` pair.
7. **`ADR-0008` §Tunnel service labels:** deterministic `<= 31`-byte derivation — use the normalized full `<project>(-<fork>)?` when it is `<= 31` bytes and non-colliding, else `<prefix_trimmed_of_trailing_dashes>-<fixed_hash_id>` with the total `<= 31` and charset `^[a-z0-9]+(-[a-z0-9]+)*$`; an empty trimmed prefix drops the dash entirely.
8. **`ADR-0008` §crates/DI:** a new net-facing `crates/slipfork-ospp` owns the WebSocket client, `DeviceKeySigner`, `ResourceBundle`, and the dump streamer; `crates/slipfork-snow`/`crates/slipfork-dag` stay clock/net/filesystem-free (`FileVaultProvider` is the only filesystem adapter and keeps `0600` semantics). `share`/`share --stop` are specified to **fail closed** while `H-006` blocks the `weaved` wire adapter (no fake registrar).
9. **Amendments:** `ADR-0001` (delete REST, point to `ADR-0008`, keep `!!`/layering rules); `ADR-0003` §1 (`per_world` **shipped** in `0.1.0`, the `"per-world"` hash discriminator, and the write-side rule that per-world digests never enter `DigestCache` — M1.9 follow-up #3); `ADR-0005` (`LISTEN_FDS` activation, the `<= 31`-byte service label, the planned static forward port; `308`/`421` semantics unchanged); `ADR-0007` (`H-009` Option B ratified: the 5-tier scale is scoped strictly to **user-reachable** integrations, pre-CLI internal adapters are omitted until `declared`; plus the `--json` view emitting `required_evidence` so the ceiling consumes the one Rust ladder, and the status rewording of M1.9 #4).
10. **`ARCHITECTURE.md`:** crate table gains `crates/slipfork-edge`, `crates/slipfork-vcs`, `crates/slipfork-ospp`; complete `WorldEngine` (multi-mount `WorldSpec`, `WorldMount` RO/RW, redacted `ExecEnv`, `freeze_world`, `spawn_service`/`stop_service`, streaming + interactive exec, `dial_world_tcp`), `SecretProvider` (`Send + Sync`, `&self`), `TunnelRegistrar`, and `DaemonControl` contracts; extend the `unsafe` policy table to the new PTY module; fix the M1.9 accuracy items (composition-root `SupportRegistry` wiring is M2, `[test]`/`fixtures`/support schemas ship in `0.1.0`, all present evidence fields are checked, single tier→evidence ladder).
11. **`docs/design/target-dx.md`:** complete M2 UX — `up`/`down`/`list` (exactly one URL per world)/`prune`/`shell`/`logs`/`hook run`/`db`/`redis`/`forward`/`share [--stop]`/`secret set`/`auth login`/`daemon unit`, with a rescue-command example and the `--no-dump`/`--refresh-dump` flags.
12. **Critique:** record the goldfish critic artifacts as `docs/design/adr-0008.review-ioc-upward.md` and `docs/design/adr-0008.review-dx.md` (dx + ai-ergonomics lenses). If a clean DI tree cannot be drawn (e.g. the dump streamer needs a `downcast` or an opaque extras bag), stop and file `plan.py human add --kind direction-challenge` instead of papering over it.

# Acceptance

```bash
# 1. ADR-0008 exists and locks the four contract families.
test -f docs/decisions/ADR-0008-ospp-v1-websocket-tpmd-resource-strategy-and-world-ux.md
rg -q 'ospp-v1|p256:<66|LISTEN_FDS|SERVICE_NAME_MAX' docs/decisions/ADR-0008-ospp-v1-websocket-tpmd-resource-strategy-and-world-ux.md

# 2. The old REST sketch is fully deleted from ADR-0001 (exit 0 = no matches).
! rg -q 'secrets:ensure|/bundle|OsppHttpProvider' docs/decisions/ADR-0001-snow-modes-and-secret-protocol.md

# 3. ARCHITECTURE.md names the new crates and the 31-byte cap.
rg -q 'crates/slipfork-edge' ARCHITECTURE.md
rg -q 'crates/slipfork-vcs' ARCHITECTURE.md
rg -q 'crates/slipfork-ospp' ARCHITECTURE.md
rg -q 'SERVICE_NAME_MAX|31 bytes' ARCHITECTURE.md

# 4. Critic artifacts were recorded.
ls docs/design/adr-0008.review-ioc-upward.md docs/design/adr-0008.review-dx.md

# 5. Published docs remain vaporware-free and every gate is green.
uv run --no-project tools/check_docs_honesty.py
make docs-check
make check
```

Every command above must exit `0`. `docs/` (published), `README.md`, `zensical.toml`, and all code are untouched by this node.
