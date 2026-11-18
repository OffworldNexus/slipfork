---
id: M3.29.3
title: "Scope the local 0600 vault to local credentials, prove remote secrets never hit disk, and add live slipforkd OSPP subscriptions + HPKE dump restore + auth pairing"
kind: task
status: ready
owner: implementer
priority: 1
depends_on: [M3.29.2, M3.28.15]
paths:
  - "crates/slipfork/src/**"
  - "crates/slipfork-snow/src/**"
  - "crates/slipfork-snow/tests/**"
  - "crates/slipfork/tests/**"
  - "crates/slipfork-edge/src/**"
  - "crates/slipfork-edge/tests/**"
  - "crates/slipfork-engine/src/**"
  - "crates/slipfork-dag/src/**"
  - "crates/slipfork-vcs/src/**"
  - docs/reference/cli.md
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-18T00:08"
updated: "2026-11-18T01:26"
---

# Goal

Make the two-tier secret model real and make `slipforkd` the live OSPP v1 client. **Local tier:** `FileVaultProvider` (`0600` `vaults/<project>.json`) keeps **only local** credentials and overrides — locally-provisioned PostgreSQL/Redis passwords that must survive `slipforkd`/host restarts so a world can still connect to its Btrfs data, plus locally-managed `vars`/`secrets` (`slipfork secret`, local-mode `!!`). **Remote tier:** everything the OSPP server resolves is decrypted **strictly in memory** and injected into child environments — never written to the vault or any file. The daemon holds the WebSocket open while worlds run, restarts affected services on `env_updated`, streams HPKE-encrypted dumps straight into `pg_restore`, and `slipfork auth` pairs with `<url>#handle=<handle>`.

# Context

`docs/design/ospp.md` (`9098b20`) is normative; `M3.29.2` ships the client/server crate; `M3.29.1` draws the two-tier model in the architecture. Owner clarification (2026-11-18): *"I guess we still need a file vault, for example to store the local postgresql credentials, but definitely not for the remote secrets."* So the vault is **kept and scoped**, not deleted: it is the home of credentials that must outlive a daemon restart, and its keys are what make `claims` derivable. `snow.yml`'s `!!` values also stay local-vault-backed.

This node is serialized after `M3.28.15` because `cli.rs`/`main.rs`/`daemon.rs` are Phase C's single-writer files.

# Design / Invariants

1. **Two tiers, one boundary.** Keep `FileVaultProvider` (`crates/slipfork/src/vaults.rs`, `vaults/<project>.json`, `0600`) and `slipfork secret set|unset|list` **scoped to local credentials**: locally-provisioned backing-service passwords (Postgres/Redis) that must survive `slipforkd`/host restarts, locally-managed `vars`/`secrets`, and local-mode `!!` values. Nothing remote may ever be written there.
2. **Remote secrets never touch disk.** `env_resolved`/`env_updated` `{ "encrypted": … }` values are HPKE-decrypted **in `slipforkd` memory**, substituted in memory, and injected directly into child environments. No code path writes them to the vault, a temp file, a log, or `--json`. Prove it: after a full boot with a known remote secret plaintext, a byte scan of `<state_dir>` finds **zero** occurrences.
3. **`claims` derive from the local side.** Every `vars`/`secrets` key present in the local vault, plus locally-provisioned `resources.<kind>` and `domains` (and locally-declared `components`), is what the daemon lists in `claims` — **ids only, never values** — so the server leaves those `${{ … }}` expressions untouched and the daemon substitutes them locally from the vault + local coordinates.
4. **Local-only runs without a vault entry** generate an ephemeral **in-memory** session secret (documented as a *session* secret: a world needing stability across boots must either `slipfork secret set` it or get it from CI).
5. **Live subscription lifecycle.** `slipforkd` owns one subscription per project while its worlds run: `subscribe { id, remote, encryption, snow, claims }`, apply `env_resolved`, then on `env_updated` restart the **affected** services with the new environment (reuse `M3.28.17`'s restart path, do not compose `down`+`up`), re-`subscribe` with the same `id` when `snow.yml`/claims change, and `unsubscribe` when the last world stops. Disconnect/backoff must be honest: fail closed with one actionable line, never silently fall back to an unauthenticated world.
6. **Dumps.** `dump_request`/`dump_ready` + out-of-band HTTPS `GET`, decoded with `M3.29.2`'s streaming reader, piped into `zstd -d | pg_restore` (or the existing restore path); zero plaintext dump bytes on disk (prove with a test that watches the world's temp dir).
7. **Pairing.** `slipfork auth` starts `pair_hello`, solves the ALTCHA payload, sends `pair_begin` with `hostname`, receives `pair_handle` and opens `<url>#handle=<handle>` through the single `BrowserOpener` (`crates/slipfork/src/browser.rs`), then waits for the push verdict with a bounded timeout (Ctrl-C cancels). No second opener, no URL computed client-side.
8. **No ambient IO.** The WebSocket client is injected behind `M3.29.2`'s capability trait; `slipforkd`'s composition root builds it, tests build the fake. No secrets in logs, `--json`, or error strings (a secret that appears in any output is a BLOCKER).
9. **Protected tests prove the promises:** `critical_ospp_daemon` must show (a) a known remote-secret plaintext appearing **nowhere** under `<state_dir>` after a full boot, (b) a live `env_updated` restarting only the affected service, (c) an encrypted dump restoring a real `pg_dump` with no plaintext file left behind, (d) a vault-backed local credential still present after a daemon restart; `critical_cli` must show `slipfork secret set|list` still works against the local vault, `auth` prints the fragment URL, and no command echoes a secret value.

# Acceptance

```console
$ cargo run -q -p slipfork -- secret list; echo "exit=$?"
... exit=0
$ ! grep -rn 'remote.*vault\|vault.*remote secret' crates/slipfork/src crates/slipfork-snow/src && echo two-tiers
two-tiers
$ cargo test -p slipfork --lib
... test result: ok
$ cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1
... test result: ok
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
... test result: ok
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
$ git grep -c 'encrypted' -- crates/slipfork-snow/src | head -1
```

- `critical_ospp_daemon` asserts: (a) a full boot writes **no remote** secret value anywhere under `<state_dir>` (byte scan for the known plaintext), (b) `env_updated` restarts only the affected component, (c) the encrypted dump restores and leaves no plaintext file behind, (d) a claimed `${{ vars::DEBUG }}` arrives verbatim and is substituted locally, (e) a vault-backed local Postgres password still works after a daemon restart (the vault is doing its job).
- `RESULT.md` records the byte-scan proof, the restart transcript, and the two-tier boundary it proves.
