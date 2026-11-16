---
id: M2.21
title: "Isolated: live IncusEngine conformance and declared support promotion (gated on H-004)"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: [M2.20]
paths:
  - "crates/slipfork-engine/src/incus.rs"
  - "crates/slipfork-engine/tests/critical_engine.rs"
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

<!-- splice: replace everything after the frontmatter of plan/nodes/M2/M2.21/NODE.md with this file. Frontmatter requires `depends_on: [M2.20, M2.28]` (apply with `uv run --no-project tools/plan.py edit M2.21 --depends M2.20,M2.28`) because live conformance now exercises `WorldSpec.rootfs`. `M2.21` stays terminal: nothing may depend on it. Source of truth: ADR-0009 §3 + ARCHITECTURE.md §3.3 (`M2.25`). -->

# Goal

Prove the live `IncusEngine` against a real project-scoped daemon and promote the incus integration in the support registry — **only** if the missing host input is available. When it is not, this node performs zero file edits and returns `STATUS: blocked`.

# Context

- `M2.6` and `M2.7` compiled `IncusEngine` against the `WorldEngine` trait and the shared conformance suite through a protocol harness; no live daemon has ever been contacted.
- **`ADR-0009` replaces the unexplained `base_instance: String`:** `M2.28` makes `IncusEngine` import `WorldSpec.rootfs.app_rootfs` once per `rootfs.digest` as the immutable image `slipfork-l0-<digest12>` (the same artifact `ReflinkNetnsEngine` overlays read-only), clone it with `POST /1.0/instances` (`source.type = "copy"`), and attach `rootfs.host_caches` / `rootfs.service_images` read-only with `shift = "true"`. Live conformance therefore needs a real `Dockerfile`-derived `WorldRootfs`, not a pre-built instance, and must never construct one by hand.
- `H-004` is an open human item: a project-scoped `incusd` socket has not been provided on this host. Iron Law #1 applies: inventing a socket path, a fake in-memory engine, a degraded fallback, or an environment side-door around the missing credential is forbidden.
- This node is deliberately **terminal and isolated**: nothing else in the milestone depends on it, so a blocked result costs the milestone nothing except this node.
- `ADR-0007` forbids listing an integration in `support/integrations.toml` before it is user-reachable; `H-009` Option B is why the entry is currently omitted.

# Design / Invariants

1. **Precondition first.** Before any edit, resolve the daemon endpoint from the injected configuration and probe it read-only. Accepted sources: the socket path recorded in `H-004`, then the documented project-scoped defaults. If none answers, stop.
2. **Blocked outcome is exact.** Zero file edits, zero manifest changes, zero registry changes, and a report of `STATUS: blocked` with `BLOCK_KIND: missing-input`, citing `H-004` and the probed paths. Never a synthesized socket, never a `todo!()`, never a mock promoted to production.
3. **Live conformance over the real `WorldRootfs`.** With a daemon present: the adapter materializes one `WorldRootfs` through the injected `BaseImageBuilder` (`FakeBaseImageBuilder` only in unit tests), imports it as `slipfork-l0-<digest12>`, and passes the full `assert_world_engine_conformance` matrix (multi-mount, freeze lineage with `WorldProcess`-then-`OciContainer` stop ordering, `ServiceRuntime` supervision of `postgres:<v>`/`redis:<v>`, streaming exec, `exec_in_service`, `service_entry_command`, `dial_world_tcp`) plus a live lifecycle case. Any capability the daemon genuinely lacks is reported as a typed `EngineError`, not silently emulated; no `base_instance` parameter and no second rootfs path may reappear.
4. **Support promotion.** Only after live conformance passes: add or update the integration entry at `declared` with a real `recipe_page` under `docs/reference/`, a real `critical_test` path, and evidence that the CLI path reaches it. Regenerate `docs/support-matrix.md` through the documented mutator so the generated table stays byte-identical.
5. **Isolation.** `depends_on` remains `M2.20` + `M2.28` only. A blocked result must never delay or invalidate another node, and no other node may list this one as a dependency.

# Acceptance

```bash
# 0. H-004 precondition. Prints exactly one verdict and always exits 0.
uv run --no-project python -c 'import os; c=["%s/incus/unix.socket" % os.environ.get("XDG_RUNTIME_DIR", "/run/user/%d" % os.getuid()), "/run/incus/unix.socket", "/var/lib/incus/unix.socket"]; hit=[p for p in c if os.path.exists(p)]; print("H-004: live incusd socket at " + hit[0]) if hit else print("H-004: absent -> zero edits, STATUS: blocked (missing-input)")'

# Everything below runs ONLY when the verdict above is "live incusd socket".
cargo test -p slipfork-engine --test critical_engine -- --nocapture incus_conformance
cargo test -p slipfork-engine --test critical_engine -- --nocapture incus_lifecycle
cargo test -p slipfork-engine --test critical_engine -- --nocapture incus_rootfs_provenance
cargo test -p slipfork --test critical_cli -- --nocapture incus_up
uv run --no-project tools/check_support.py --check
git diff --exit-code docs/support-matrix.md
make check
```

When the verdict is `absent`, the only acceptable outcome is a blocked report with zero file edits; the acceptance for a blocked run is the single verdict line above plus `git status --porcelain` reporting no change to any tracked file.
