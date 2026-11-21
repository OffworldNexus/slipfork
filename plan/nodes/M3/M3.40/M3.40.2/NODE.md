---
id: M3.40.2
title: "OSPP v1 client, <state_dir>/ospp.json pairing, 0600 offline vault, loopback reference server"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.40.1]
paths: ["crates/slipfork-ospp/**", Cargo.toml, Cargo.lock, "specs/05-ospp-v1.md", TESTING_PLAN.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T12:50"
updated: "2026-11-21T12:50"
---

# Goal
Implement the OSPP v1 client end-to-end in `crates/slipfork-ospp` per `specs/05-ospp-v1.md`, plus the offline secret vault and the loopback reference server used by E2E.

# Context
- `crates/slipfork-ospp/src/lib.rs` is a 12-line `OsppEndpoint` marker stub; `crates/slipfork-ospp/Cargo.toml` has an empty `[dependencies]`.
- `specs/05-ospp-v1.md` (256 lines) is authoritative: `version: 2` frames, P-256 device keypair auth, ALTCHA + PoP `#handle=` pairing, Git-remote `subscribe` carrying the evaluated `Forkfile`, in-memory `hpke-p256` secret decryption with live `env_updated` service restarts, HTTPS `dump_ready` encrypted dump streaming.
- Evaluate the `Forkfile` through `slipfork-forkfile` (canonical owner) - never re-scan `$` tokens or service names here.
- Public pairing state only: `<state_dir>/ospp.json` = `{endpoint, account, machine}`; secret material stays in the local `0600` vault, decrypted in memory, never written to `ospp.json`.
- Canonical port from the `specs/03` crate & port map; config/state paths are injected by the composition root. Zero `std::env`, `OnceLock`/`LazyLock`, or `static` state in this crate.
- E2E uses a real `127.0.0.1:0` loopback OSPP reference server under `crates/slipfork-ospp/tests/` (allowed). Zero `Mock*`/`Fake*`/`Recording*`.

# Acceptance
- `cargo test -p slipfork-ospp`: loopback E2E proves pairing writes `ospp.json`, `subscribe` sends the evaluated Forkfile, `env_updated` restarts a real service process, `dump_ready` streams a multi-MB dump and verifies the digest; vault file mode asserted `0600`.
- `cargo clippy -p slipfork-ospp --all-targets -- -D warnings`.
- `! grep -rn 'std::env::var\|OnceLock\|LazyLock' crates/slipfork-ospp/src`.
- Test guard: no secret plaintext appears in `ospp.json` (grep assertion inside the pairing test).
- `uv run --no-project tools/critical_guard.py`.
