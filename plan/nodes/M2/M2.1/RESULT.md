# Result — M2.1 (`STATUS: blocked`, `BLOCK_KIND: direction-challenge`, `H-010`)

## Completed Drafts, Two-Round Critic Gate & Post-Gate Remediation (`A`..`O`)

- Authored `docs/decisions/ADR-0008-ospp-v1-websocket-tpmd-resource-strategy-and-world-ux.md` (`Status: Proposed — blocked on H-010 (direction-challenge)`), amended `ADR-0001` (deleted M1 REST sketch outright; sole normative home for `effective_components`), `ADR-0003`, `ADR-0004` (single env-injection owner in `slipfork exec`), `ADR-0005`, `ADR-0007` (`H-009` Option B ratified), `ARCHITECTURE.md`, `docs/design/target-dx.md`, and `docs/design/m1-core-and-poc.md`.
- Applied all Round-2 Quad Gate follow-ups (`A`..`O` in `plan/nodes/M2/M2.1/FOLLOWUPS.md`):
  - **Honesty (`A`–`D`):** `ADR-0008` marked `Proposed — blocked on H-010` (dropped `LOG.md` 64-char reference); future-tense `(M2.13–M2.14)` test reference; explicit `tungstenite` + `rustls` + `webpki-roots` TLS stack; `httparse` with `<= 16 KiB` header policy bound.
  - **Design consistency (`E`–`O`):** explicit fd-order `LISTEN_FDS` roles (`LISTEN_FDS == 1` -> single HTTP ingress with `forward_listener: None` for `systemd-socket-activate -l 127.0.0.1:0`; `>= 2` -> `UnitSpec` fd order; else `EdgeError::ActivationBroken`); idempotent no-op `unshare_world` vs fail-closed `share_world`; mutually exclusive `--no-dump | --refresh-dump` without a precedence ladder; single `--world <fork>` selector everywhere; single `effective_components` home (`ADR-0001` §1); deleted `EngineCapabilities` boolean bag; generic `FallbackDeviceKeySigner<P, F>`; single `ResourcePlan::resolve(...)` owner; single env-injection owner in `slipfork exec`; milestone tags (`SnowError::ForbiddenTopLevelKey` `M2.4`, `slipfork-engine -> slipfork-world` `M2.2`, `OsppBundle` deletion `M2.4`); and `SeedDumpStore::write_from`, non-`Deserialize` `ResourceMode`, defined `SoftwareP256Key`, zero dangling type names, and single `list --json` home (`target-dx.md` §1.5).
- All repository gates (`make check`, `make docs-check`, `tools/check_docs_honesty.py`, `tools/guard.py`) and `M2.1` file/regex checks exit `0`.

## Open `H-010` (`direction-challenge`) Items Left Untouched Pending Owner Ruling

1. **Cross-Process `WorldEngine` / `WorldHandle` & `DaemonControl` Split (`M2.6` vs `M2.8`/`M2.9` vs `M2.16`):**
   - **Option A1 (Recommended):** Keep `ExecEnv` secrets & CLI `InteractiveTty` fds in the CLI process; add state-verified `WorldEngine::open_world(&self, project: &ProjectSlug, target: &WorldTarget) -> Result<WorldHandle, EngineError>` + `WorldRegistration { project, target, snow_domains, default_ingress_port }` on `DaemonControl::register_world`.
   - **Option B1:** Move `WorldEngine`, `SnowFile` resolution, and `SecretProvider` inside `slipforkd`, passing CLI PTY fds over `AF_UNIX` `SCM_RIGHTS`.
2. **Portless `WorldOrigin::Local` (`ADR-0005` §1) vs Unprivileged `--listen <addr>` & In-Netns `$BASE_URL` (`M2.8` / `M2.18`):**
   - **Option A2 (Recommended):** Parameterize `WorldOrigin::local(&LocalHostRoute, ingress_port: u16)` (`http://<host>` when `80`, `http://<host>:<port>` otherwise) + in-netns loopback forwarder so `$BASE_URL` works from host browsers and inside `slipfork exec` / `slipfork test`.
   - **Option B2:** Keep `WorldOrigin::Local` strictly portless `:80` only.
3. **`tpmd` Binary `postcard` Wire Framing vs `M2.2` & `M2.13` (`Iron Law #1`):**
   - **Option A3 (Recommended):** Add `postcard = { version = "1", features = ["alloc"] }` in `M2.2`, lock `tpmd` v1 `u32be + postcard` framing + golden byte vectors in `ADR-0008` §1, and test `TpmdDeviceKeySigner` in `M2.13` against golden vectors + live `/run/tpmd/tpmd.sock`.
   - **Option B3:** Isolate live `TpmdDeviceKeySigner` in a gated leaf until `tpmd-proto` is published, shipping `SoftwareP256Signer` in `M2.13`.
