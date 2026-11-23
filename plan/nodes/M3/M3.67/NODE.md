---
id: M3.67
title: "Fix ps stopped-fork placement & remove duplicate <primary_domain>.<world>.localhost authority"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.66]
paths:
  - "crates/slipfork-ingress/**"
  - "crates/slipfork/**"
  - "crates/slipfork-forkfile/**"
  - "support/demo/**"
  - "specs/**"
  - "docs/**"
  - TESTING_PLAN.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-23T02:18"
updated: "2026-11-23T02:33"
---

# Goal

Live inspection of `slipfork ps` on `165.232.86.174` caught two visual/routing regressions:

1. **Duplicate `http://main.demo.localhost` alongside `http://demo.localhost` (`crates/slipfork-ingress/src/route/mod.rs:28-38`):**
   - The first declared `domain` (`domain main` / `domain dom_a`) IS the app's default domain (`<world>.localhost` locally, matching `main.name == "<world>.localhost"` and `self.domain == "<world>.localhost"`).
   - Currently `route_authorities(primary_domain, domain)` returns `[None, Some("main")]` for unfiltered routes (creating a bogus duplicate `http://main.demo.localhost` in both ingress and `slipfork ps`) AND returns `[Some("dom_a")]` (`dom_a.<world>.localhost`) when a service writes `route / domain dom_a` for the first declared domain (diverging from `dom_a.name == "<world>.localhost"`!).
   - **Fix (`route_authorities` in `crates/slipfork-ingress/src/route/mod.rs:28-38`):**
     - `None` -> `vec![None]` (`<world>.localhost`)
     - `Some(name) if primary_domain == Some(name)` -> `vec![None]` (`<world>.localhost`, matching `<name>.name == "<world>.localhost"`!)
     - `Some(name)` (any alternative domain `2..N`) -> `vec![Some(name.to_owned())]` (`<name>.<world>.localhost`, matching `<name>.name == "<name>.<world>.localhost"`!)
   - Update `crates/slipfork-ingress/tests/*`, `crates/slipfork/tests/*`, `specs/01-forkfile.md`, and `docs/` where the old `"primary world domain and the first declared domain"` duplicate-host wording appeared.

2. **`○ <stopped_fork>` rendered ABOVE parent world's `├─` tree (`crates/slipfork/src/status/tree.rs:268-289`):**
   - Currently `tree.rs:268` prints `for fork in &world.stopped_forks` BEFORE `rows.domains` and `rows.support` (line 282), making `demo`'s `├─ http://demo.localhost` tree visually hang underneath `○ tralala ~/demo/.forks/tralala stopped` instead of underneath `● demo ~/demo`!
   - **Fix (`crates/slipfork/src/status/tree.rs`):** Render `world`'s own `├─ / └─` domains and `support` tree FIRST under `● <world>`, and render `○ <stopped_fork> <path> stopped` as separate flat top-level world entries AFTER the running world entries (separated cleanly, never wedged between `● demo` and `demo`'s `├─` children!). Add a regression test in `crates/slipfork` locking exact line order when a running world has both domains/support AND a stopped `.forks/<name>`.

3. **Polish `support/demo/Forkfile` `api` `run:` with `on dev ... else ...` + clean `/usr/local/bin/slipfork*` on `165.232.86.174`:**
   - In `support/demo/Forkfile` (`service api`), use `on dev: run: ... else: run: ...` cleanly instead of unconditional `run:` overwritten by `on dev: run:`.
   - Rebuild the `glibc <= 2.34` `.deb`, install on `165.232.86.174`, remove stale `/usr/local/bin/slipfork*` copies so `/usr/bin/slipfork` (from `.deb`) is used directly, sync `/home/slipfork/demo` (keeping any existing `.forks/*` intact!), and verify `slipfork ps` renders `● demo` (with its tree directly underneath, zero `http://main.demo.localhost`), `● feat-auth`, and `○ tralala` cleanly!

# Acceptance

- `make check` and `make docs-check` exit `0`.
- `critical_status_live` passes on `165.232.86.174`, and `slipfork ps` in `/home/slipfork/demo` renders `● demo` -> tree, `● feat-auth` -> tree, and `○ tralala` at the bottom with zero `main.demo.localhost`.
