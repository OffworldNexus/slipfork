# Log M3.29

## 2026-11-18T00:08 [planner]

created (epic) Implement OSPP v1 (docs/design/ospp.md): live subscriptions, HPKE end-to-end secrets, and the zero-disk-secret lifecycle in 0.1.0

## 2026-11-18T00:09 [lead]

OWNER DIRECTIVE 2026-11-18: docs/design/ospp.md (9098b20) is the sole normative OSPP v1 spec and its implementation is 0.1.0, not 0.2.0. DAG: M3.29.1 architect ripples the spec through ARCHITECTURE/ADR-0001/ADR-0008/VISION/dx/target-dx and deletes the obsolete wire + disk vaults (parallel with Phase C, docs-only paths); M3.29.2 clean-sheet slipfork-ospp + testkit (depends .1, M3.28.9, owns Cargo.toml/lock for hpke+p256); M3.29.3 deletes FileVaultProvider/vaults.rs/slipfork secret, adds in-memory session secrets, live slipforkd subscriptions, streaming HPKE dump restore and auth pairing (depends .2 + M3.28.15 so cli/main/daemon stay single-writer); M3.29.4 publishes docs/reference/ospp.md and purges every stale vault/wire claim. M3.28.13 now also depends on M3.29.3/.4, M3.28.14 on .4, M3.28.16 on .3 so 0.1.0 gates as one coherent release. Also: M3.28.20 created for the two VM-proven harness blockers (AF_NETLINK in RestrictAddressFamilies; ensure_daemon on Btrfs) and M3.28.12 depends on it.

## 2026-11-18T00:30 [lead]

OWNER CLARIFICATION (2026-11-18) — supersedes the 'delete the file vault' part of the M3.29 brief: KEEP FileVaultProvider (0600 local vault) + 'slipfork secret' strictly for LOCAL credentials/overrides (locally-provisioned postgres/redis passwords that must survive slipforkd/host restarts, local-mode !!, locally-managed vars/secrets). Local vault keys + locally-provisioned resources/domains are what populate OSPP claims (ids only, never values). Remote OSPP secrets (env_resolved/env_updated encrypted values and encrypted dump streams) are decrypted strictly in slipforkd memory and NEVER written to the vault or any disk file. Routed to the active M3.29.1 architect session; M3.29.3 and M3.29.4 re-scoped below.

## 2026-11-18T04:45 [lead]

gate acceptance corrected for the owner's two-tier ruling: the vault is KEPT and scoped, so 'no-disk-vaults' became 'local-vault-kept-scoped' + 'two-tiers'. Gate awaits M3.28.13's live evidence.

## 2026-11-18T22:19 [planner]

status ready -> done: Remaining scope absorbed into M3.30.
