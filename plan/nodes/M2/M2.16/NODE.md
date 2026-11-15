---
id: M2.16
title: "CLI part 1: composition root, slipforkd binary, up/down/list/prune/secret/share/daemon unit"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M2.4, M2.5, M2.7, M2.10, M2.12, M2.14]
paths: ["crates/slipfork/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T16:40"
---

# Goal

Wire the single composition root and ship CLI part 1: the unprivileged `slipforkd` binary plus the core world lifecycle commands (`up`, `down`, `list`, `prune`, `secret set`, `share [--stop]`, `daemon unit`) with fail-closed preconditions.

# Context

- `M2.8` to `M2.10` shipped the edge daemon, routing, proxying, and unit renderer as a library; `M2.6`/`M2.7` shipped the engine contract; `M2.3` to `M2.5` shipped the domain types. Nothing yet assembles them.
- `ADR-0008` fixes exactly one composition root: every adapter is constructed there and injected as a trait object, so no library module ever reads `std::env`, the clock, or the filesystem on its own.
- `H-006` is still open, so the tunnel registrar injected by the root must be an explicit unavailable adapter that fails closed and names the blocker; it is never a fake local tunnel.
- `list` must print exactly one URL per world (`H-008`): the `WorldOrigin` assigned before materialization is the single source of that URL.

# Design / Invariants

1. **Composition root.** `crates/slipfork/src/composition.rs` builds the tree once per process: `FileVaultProvider` (`0600`), `ReflinkNetnsEngine`, `WorldRegistry`, the unavailable `TunnelRegistrar` adapter, the `VcsWorkspaceFactory` chosen by a resolver (Git first, Jj when detected), the optional `OsppClient`, and the `SupportRegistry`. All paths, ports, and socket locations arrive as injected values. `std::env` is read **only** in `main.rs` and in the activation/systemd boundary of the edge crate.
2. **`slipforkd` binary.** Adopts activation sockets (or `--listen <addr>`), serves local and shared routing, exposes the daemon control socket, and exits non-zero with an actionable message when neither listener source is available. It never inspects the engine kind.
3. **`slipfork up`.** Resolve the repo root, load the manifest, compute `effective_components(mode)`, assign the `WorldOrigin` **before** materialization, ensure secrets, resolve `ResourceStrategy` (honouring `--no-dump` and `--refresh-dump`), create the world, run components and hooks with streamed output, freeze it, then print exactly one URL line. A failed world is left inspectable, never destroyed.
4. **`list`.** One line per world, one URL per line; ordering is stable. `--json` prints a typed array and is the only machine format.
5. **`down` / `prune`.** `down` releases one world and is idempotent. `prune` destroys worlds and snapshots whose owning repo root no longer matches, prints what it removed, and refuses to run when the ownership record is missing (no blind reaping).
6. **`secret set`.** Writes through `SecretProvider` semantics only, respects `!!` write-once (refuses to overwrite), never echoes the value, and never logs it.
7. **`share [--stop]`.** Delegates to the edge registry; with `H-006` open it fails closed naming the blocker and exits non-zero.
8. **`daemon unit`.** Prints the rendered `.socket` and `.service` units from the `M2.10` renderer to stdout, with no filesystem writes.
9. **`HOME` unset** is a fail-closed precondition checked before any mutation or network call, with a one-line actionable message. Exit codes: `0` success, `1` runtime error, `2` usage error.

# Acceptance

```bash
# 1. Both binaries build and expose help without side effects.
cargo build -p slipfork --bins
cargo run -q -p slipfork --bin slipfork -- --help > /dev/null
cargo run -q -p slipfork --bin slipforkd -- --help > /dev/null

# 2. Lifecycle, one-URL-per-world, HOME-unset, and share fail-closed paths.
cargo test -p slipfork --all-targets
cargo test -p slipfork --test critical_cli -- --nocapture up_down
cargo test -p slipfork --test critical_cli -- --nocapture one_url_per_world
cargo test -p slipfork --test critical_cli -- --nocapture home_unset
cargo test -p slipfork --test critical_cli -- --nocapture share_blocked

# 3. Units render, registry and secrets behave.
cargo run -q -p slipfork --bin slipfork -- daemon unit | rg -c "ListenStream="
cargo clippy -p slipfork --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: `up` with `HOME` unset mutates nothing, `list` emits exactly one URL per world (asserted by line count), a second `down` is idempotent, `secret set` on an existing `!!` key is refused, and `share` exits non-zero while the registrar is unavailable.
