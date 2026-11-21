---
id: M3.42.6
title: "RFC 1123 DNS/bucket label validation, hyphen identifiers, and static_cdn -> static-cdn sweep"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: [M3.42.5]
paths:
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork-world/**"
  - "crates/slipfork-ingress/**"
  - "crates/slipfork/**"
  - "specs/**"
  - TESTING_PLAN.md
  - "docs/**"
  - README.md
  - "editors/**"
  - "support/**"
  - "packages/**"
  - "tools/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T19:21"
updated: "2026-11-21T19:40"
---

# Goal

`static_cdn` proves the compiler accepts a DNS/bucket identifier that is impossible to serve: `_` violates RFC 1123 LDH (`[a-z0-9-]`), so `static_cdn.demo.localhost`, TLS/HTTP validators and S3 bucket naming all reject it. Fix the language, the slug derivation, and every spec/doc/fixture.

# Context

- `slipfork-forkfile`: lexer identifiers must accept `-` (Forkfile has no subtraction), so `static-cdn`, `static-cdn.domain`, `static-cdn.url` parse.
- `check`: statically reject non-RFC-1123 labels with a `Forkfile:line:col` caret span + `next: rename static_cdn to static-cdn` on EVERY DNS/bucket-bearing declaration: `domain <id>`, `cdn <id>`, `s3 <bucket>`, and `route ... domain <prefix>.*` prefixes. Reject `_`, uppercase, leading/trailing `-`, empty, `> 63` chars.
- `migrate` (snow.yml) and world/fork `.localhost` slug derivation (repo folder / git branch like `my_repo`, `feat_auth`) sanitize `_` to `-` and enforce the same label rule, so Slipfork never emits an underscore hostname.
- Occurrences to sweep (`grep -rn static_cdn`): `specs/01-forkfile.md`, `specs/02-cli-ux.md`, `specs/03-under-the-hood.md`, `specs/04-testing-plan.md`, `TESTING_PLAN.md`, `docs/**`, `README.md`, `crates/slipfork/src/daemon/ps.rs`, `crates/slipfork-forkfile/src/tests/{mod,parser,check}.rs`, `crates/slipfork-world/src/resources.rs`, `crates/slipfork-ingress/src/route/rules.rs`, `crates/slipfork/tests/{critical_world_journey,critical_daemon,status_surface,critical_status_live}.rs`, plus Tree-sitter/Zed/highlighter fixtures in `editors/**` and `support/demo`.

# Acceptance

- New section-7 focused tests in `slipfork-forkfile`: `s3 assets { cdn static_cdn }`, `domain my_dom`, `s3 my_bucket`, `route ... domain My_Prefix.*` are rejected with the RFC 1123 diagnostic + caret span; `static-cdn` accepted; hyphen identifiers parse in every position.
- New section-2 E2E (ingress/world lab): underscore-named `cdn`/`s3`/`domain` refuses to boot with the one-line remedy, and a hyphenated world/fork name derived from `my_repo`/`feat_auth` yields only RFC 1123 labels.
- Zero `static_cdn` remains outside `plan/` history; `zensical build --strict` + `tools/check_docs_honesty.py` pass; `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` exit 0.
