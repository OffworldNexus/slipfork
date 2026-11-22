---
id: M3.56
title: "Scorched-earth delete domain <prefix>.* wildcard syntax: require declared domain <id> everywhere (Forkfile, ingress, LSP, editors, specs & docs)"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.54]
paths:
  - "specs/**"
  - TESTING_PLAN.md
  - ARCHITECTURE.md
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork-ingress/**"
  - "crates/slipfork-lsp/**"
  - "crates/slipfork/**"
  - "editors/**"
  - "docs/**"
  - README.md
  - "support/demo/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-22T21:01"
updated: "2026-11-22T21:01"
---

# Goal

`Forkfile` is a pure infrastructure-agnostic grocery list of resources (`postgres`, `redis`, `s3`, `domain <id>`) — it has ZERO business assuming subdomain wildcards (`mail.*`) or target DNS layout! If a service (like `mailpit` or `admin`) wants its own domain, it MUST declare `domain <id>` (`on mail { domain mail ... }`) and reference `route / domain <id>` (`route / domain mail`).

# Scorched-Earth Deletion & Alignment

1. **`crates/slipfork-forkfile` (Lexer, Parser, AST, Checker, Eval, Formatter, Migrator):**
   - **Delete `.*` / `SubdomainPrefix` / `DomainFilter::Prefix` completely** from the lexer/parser/AST/IR/formatter/migrator!
   - `route` `domain` modifier accepts **strictly a declared `domain <id>` identifier** (`route / domain mail`, `route /v1 domain api`).
   - Any `route ... domain <unknown>` or `route ... domain mail.*` fails closed with a crisp `Forkfile:line:col` caret error (`next: declare 'domain mail' at top level and write 'route / domain mail'`).
   - Also check: when multiple `domain <id>`s are declared (`domain main`, `domain mail`), `route /api` without an explicit `domain <id>` binds to the primary (first declared) `domain` (`main`) — NEVER wildcard-matching across unrelated `domain mail` / `domain admin`! Verify `slipfork-ingress` + `status` tree match this 1:1 (so `http://demo.localhost` shows `/api -> api` and `/ -> front`, while `http://mail.demo.localhost` shows ONLY `/ -> mailpit`, NOT `/api` and `/` leaking onto `mail.demo.localhost`!).

2. **`crates/slipfork-ingress` & `crates/slipfork` (`status` / `ps` / `daemon`):**
   - Delete `Authority::Prefix` / `PsDomain::Prefix` (`prefix.*`) completely! Routes either bind to the primary domain (when `domain` is omitted on `route`) or to a specific declared `domain <id>` (`<id>.<world>.localhost` locally, `<world>.localhost` for the primary domain).

3. **`crates/slipfork-lsp`, `editors/tree-sitter-forkfile`, `editors/zed`, `specs/**`, `TESTING_PLAN.md` & `docs/**`:**
   - Purge `mail.*` / `<prefix>.*` everywhere across `specs/01..05`, `TESTING_PLAN.md`, `docs/**`, `README.md`, `editors/**`, and all test fixtures, replacing `on mail { image mailpit ... { service mailpit { port 8025; route / domain mail.* } } }` with:
     ```forkfile
     on mail {
       domain mail

       image mailpit from axllent/mailpit:latest {
         service mailpit {
           port 8025
           route / domain mail
         }
       }
     }
     ```
   - Regenerate Tree-sitter parser (`npx tree-sitter generate && npx tree-sitter test`) and verify `editors/zed/check.sh` + `make docs-check`!

# Acceptance

- `git grep -E 'mail\.\*|prefix\.\*'` across `specs/`, `docs/`, `crates/`, `editors/`, `support/` returns **zero** route-wildcard hits (except negative parser rejection test).
- `route / domain mail.*` is rejected statically by `slipfork-forkfile` with a caret diagnostic.
- Unfiltered `route /api` attaches to the primary `domain` only (never leaking onto `domain mail`), and `make check` + `make docs-check` exit `0`.
