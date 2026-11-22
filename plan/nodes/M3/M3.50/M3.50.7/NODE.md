---
id: M3.50.7
title: "Domain-first status tree + always-<bucket>.<world>.localhost S3/CDN local domains (zero ports/ages)"
kind: task
status: done
owner: implementer
priority: 4
depends_on: [M3.50.3, M3.50.4, M3.50.9]
paths:
  - "crates/slipfork-forkfile/src/eval/**"
  - "crates/slipfork-forkfile/src/expr.rs"
  - "crates/slipfork-ingress/src/**"
  - "crates/slipfork/src/status/**"
  - crates/slipfork/src/daemon/ps.rs
  - crates/slipfork/src/daemon/view.rs
  - crates/slipfork/src/daemon/lifecycle/ps.rs
  - crates/slipfork/tests/status_surface.rs
  - crates/slipfork/tests/ansi_matrix.rs
  - crates/slipfork/tests/tree_domain_surface.rs
  - "support/demo/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T13:13"
updated: "2026-11-22T15:49"
---

# Goal
Redesign the human `slipfork` / `slipfork ps` tree to be domain-first (flat worlds, domains -> mapped sub-paths, zero ports/ages), give every S3 bucket its own `<bucket>.<world>.localhost` domain, and make a declared `cdn <id>` alias the bucket domain locally (never a separate cdn hostname).

# Context
- Owner verdict: the current nested `forks` subtree with `:port` and `6h` ages is unreadable. Human output must show worlds FLAT, then domains, then sub-path -> service.
- `crates/slipfork/src/status/{model,rows,tree,widths}.rs` + `crates/slipfork/src/daemon/{ps,view,lifecycle/ps}.rs` build the rows; `ps` is the same tree filtered to the cwd world/fork.
- `crates/slipfork-forkfile/src/eval/mod.rs:434-478` binds `cdn <id>` as `BoundSymbol::Cdn`; `<cdn_id>.domain`/`<cdn_id>.url` must resolve to `<bucket>.<world>.localhost` / `http://<bucket>.<world>.localhost`.
- `crates/slipfork-ingress/src` owns the `.localhost` route table: every bucket must register `<bucket>.<world>.localhost` (fork: `<bucket>.<fork>--<world>.localhost`), and a `cdn` declaration must not add a second hostname.
- `support/demo/Forkfile` uses `s3 assets { policy public-read; cdn static-cdn }`; the demo README/front ARG will follow in M3.50.5.
- `--json` must keep every structured field (`port`, `started_at_ms`, `url`, `routes`, `orphaned`) even though human output drops them.

# Acceptance
- Human tree exactly: blank line between worlds; header `● demo  ~/demo` (fork: `● feat-auth  ~/demo/.forks/feat-auth`); rows `http://demo.localhost`, `http://mail.demo.localhost`, `http://uploads.demo.localhost  s3 (private)`, `http://assets.demo.localhost   s3 (public-read)`, then `support` with `worker service`, `pg postgres 18`, `cache redis 7`; zero `:port`, zero ages; non-default route mode suffix only.
- `orphaned` pods (from M3.50.4) render yellow under `support`.
- `curl -H "Host: assets.demo.localhost" http://127.0.0.1/` serves the bucket; `static-cdn.demo.localhost` no longer resolves.
- `--json` unchanged fields + new `orphaned`; `NO_COLOR`/pipes keep zero `\x1b[`.
- `cargo test -p slipfork-forkfile`, `cargo test -p slipfork-ingress`, `cargo test -p slipfork --test status_surface --test critical_status_live`, `guard.py`.,
