---
id: M2
title: "Single-Origin .localhost Edge (slipforkd), OSPP v1 WebSocket + ResourceStrategy, Live VCS Worlds & Service CLI (0.2.0)"
kind: milestone
status: ready
priority: 2
depends_on: [M1]
paths: []
attempts: 0
max_attempts: 3
escalation: strategist
created: "2026-11-14T22:03"
updated: "2026-11-15T16:29"
---

# Goal

Deliver Slipfork `0.2.0`:
1. **Architecture & DX Lock (`M2.1`):** Amend `ADR-0001`/`ADR-0005`/`ADR-0007`, author `ADR-0008` (`OSPP v1` WebSocket protocol replacing M1's REST sketch + `tpmd` ECDSA P-256 `p256:<hex>` challenge-response auth & browser-fragment pairing + remote resource coordinates + streaming `zstd`/`pg_restore` dev DB dumps + mode-aware `[resources]` `ResourceStrategy` + inspectable hook/migration failure UX + service access/forwarding), and update `ARCHITECTURE.md` & `docs/design/target-dx.md` with zero published vaporware.
2. **Single-Origin `.localhost` Edge (`slipforkd`) & `<= 31`-Byte Tunnel Service Labels:** `WorldOrigin = Local { url } | Shared { url }` + deterministic `<= 31`-byte Tunnel Weaver service-label truncation (`<prefix>-<hash>`) in `crates/slipfork-world`; unprivileged `slipforkd` sole local + shared ingress (`LISTEN_FDS` socket activation with `set_nonblocking(true)` + `--listen` tested via `systemd-socket-activate -l 127.0.0.1:0`, `slipfork daemon unit`), `WorldRegistry`, `shared_hosts` map, and `*.localhost`-scoped `308`/`421` rules (`H-008`).
3. **Full `WorldEngine`, Live Hook Debugging & Service Access CLI:** Complete `WorldEngine` (`freeze_world`, `spawn_service`, interactive TTY exec + live-streamed hook output, `dial_world_tcp`, multi-mount `WorldSpec`, redacted `ExecEnv` & `S3Coordinates` `Debug`) on live `ReflinkNetnsEngine`; failed hooks/migrations keep the world inspectable and print 1-line `slipfork hook run`, `slipfork logs`, and `slipfork shell` rescue commands; interactive lifecycle & service CLI (`up`, `down`, `shell`, `hook run`, `logs`, `db`, `redis`, `forward`, `share [--stop]`, `list`, `prune`, `secret set`, `auth login`).
4. **`OSPP v1` WebSocket Client, `VcsWorkspace`, `@slipfork/opencode` E2E & `H-009` Support Hygiene:** `OSPP v1` WebSocket client + `tpmd`/software P-256 signer + streaming DB seed/remote S3 `ResourceStrategy` verified against an in-tree WebSocket server conformance harness (while offline laptops stay 100% functional on `0600` `FileVaultProvider`); `VcsWorkspace` (`GitWorktreeAdapter` + `JjWorkspaceAdapter` + `git-remote-slipfork`); `H-009` Option B cleanup (`incus-engine` omitted from `support/integrations.toml` until user-reachable CLI ships), `slipfork support [--json]`, generated docs support matrix, read-only `make check` ceiling + `make support-sync`, and non-contaminating `make bench`; plus strictly isolated leaf tasks for live `IncusEngine` (gated on `H-004`) and `WeavedRegistrar` (gated on `H-006`) blocking nothing else in `M2`.

# Acceptance

- [ ] `M2.1` (`ADR-0001`/`0005`/`0007` amendments + `ADR-0008` + `ARCHITECTURE.md` + `docs/design/target-dx.md`) locks a leak-free DI tree for `OSPP v1` WebSocket + `tpmd` P-256 auth + `ResourceStrategy`, `LISTEN_FDS` `slipforkd`, `<= 31`-byte service labels, inspectable hook debugging, and `db`/`redis`/`forward` while keeping `make docs-check` (`check_docs_honesty.py`) 100% vaporware-free at every commit.
- [ ] Unprivileged `slipforkd` passes `systemd-socket-activate -l 127.0.0.1:0` and `--listen` tests enforcing the single-origin invariant (`ADR-0005` §1–§2, `H-008`): `WorldOrigin` assigned before materialization without requiring OSPP, exact 1-URL `slipfork list` output, `shared_hosts` proxying via `dial_world_tcp`, `*.localhost`-scoped `308`/`421` rules, and `<= 31`-byte Tunnel Weaver service-label conformance.
- [ ] Live `ReflinkNetnsEngine` worlds verify mode-scoped `components.<id>` supervision, live-streamed hook failure preservation + `slipfork hook run` / `logs` / `shell` rescue UX, `slipfork db` / `redis` / `forward` TCP bridges, `OSPP v1` WebSocket `tpmd`/P-256 challenge-response + remote resource/streaming DB dump `ResourceStrategy`, `VcsWorkspace` (`Git` + `Jj` + `git-remote-slipfork`) conformance, and `@slipfork/opencode` e2e tests.
- [ ] `H-009` Option B ratified in `support/integrations.toml`, `slipfork support [--json]` + generated docs matrix + read-only `make check` compliance ceiling + `make support-sync` + non-contaminating `make bench --check` ship green; live `IncusEngine` (`H-004`) and `WeavedRegistrar` (`H-006`) live in isolated leaf tasks with zero blocking dependencies from other `M2` tasks; `make check` exits 0.
