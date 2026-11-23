---
id: M3.44.1
title: "Forkfile s3 policy/cdn surface, RFC 1123 DnsLabel & static-cdn purge"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork-lsp/**"
  - "crates/slipfork-ingress/src/route/rules.rs"
  - "crates/slipfork-ingress/tests/route_table_journey.rs"
  - crates/slipfork/src/daemon/view.rs
  - "editors/**"
  - "docs/**"
  - README.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-21T20:17"
updated: "2026-11-21T20:27"
---

# Goal
Make the `Forkfile` surface spec-exact: explicit `s3 <bucket> { policy private|public-read; cdn <id> }` with fail-closed `cdn`/`policy` pairing, strict RFC 1123 `DnsLabel` validation on every hostname slot, `_`→`-` slug normalization, and `SecretValue`/`BucketPolicy` in `slipfork-forkfile`. This is the only slice allowed to change the compiler; downstream slices consume the resulting AST.

# Context
- Extend canonical owner modules (never re-scan `$`/tokens downstream): `crates/slipfork-forkfile/src/resources.rs:88-101` (`S3`, `S3Item::Cdn|On` — add `Policy(PolicyDecl)`); `decls.rs:180-205` `parse_s3_item` (add `"policy"` arm, remedy `use \`policy private|public-read\`, \`cdn <id>\` or \`on <expr>\``); `check/mod.rs:410` `check_s3_items`; `check/symbols.rs:258` `collect_s3_items`; `fmt.rs:340` `render_s3_item` (canonical order `policy` before `cdn`, single-line when short).
- Add `BucketPolicy { Private, PublicRead }` (default `Private`) and `SecretValue` (redacted `Debug`/`Display` = `[REDACTED]`, zeroize-on-drop) per `specs/01-forkfile.md:145`; taint `<bucket>.url` and `.secret_key`; keep `<bucket>.endpoint/.bucket/.access_key` non-secret.
- One `DnsLabel` validator (new `crates/slipfork-forkfile/src/dns.rs`): `1..=63` lowercase `[a-z0-9-]`, alnum edges, no `_`/uppercase/dots; `s3 <bucket>` additionally `3..=63` and rejects IPv4-shaped literals. Called from `domain <id>`, `cdn <id>`, `s3 <bucket>`, and `route ... domain <prefix>.*` (specs/01:86-91).
- Pairing diagnostic (specs/01:127-136): `error: 'cdn <id>' inside 's3 <bucket>' requires 'policy public-read'`, caret at FK line:col pointing at the `cdn` statement, then exactly `next: add 'policy public-read' inside 's3 <bucket>' to enable 'cdn <id>'`. Emit in new `crates/slipfork-forkfile/src/check/policy.rs` (~80 LOC) so `check/mod.rs` does not grow past 500.
- Slug normalization `_`→`-` for repo/branch `.localhost` hostnames, then purge `static_cdn` within this slice's paths: `crates/slipfork-forkfile/src/**` (incl. `src/tests/{mod,parser,check}.rs`), `crates/slipfork-forkfile/tests/**`, `crates/slipfork-lsp/**`, `editors/**`, `docs/**`, `README.md`; rewrite every fenced example to `static-cdn` (zero legacy alias).
- `<= 500 LOC`: `resources.rs` is 239 today; put policy parsing/printing in `decls.rs`/`fmt.rs` helpers, checks in `check/policy.rs`, label logic in `dns.rs`.
- Out of scope: `crates/slipfork/**`, `crates/slipfork-world/**`, `specs/**` (authoritative, do not edit).

# Acceptance
- `cargo test -p slipfork-forkfile --test critical_forkfile` green: `policy` parse+fmt round-trip, `cdn` without `policy public-read` (exact message/line:col/`next:`), `cdn` with `policy private` rejected, RFC 1123 rejects (`_`, `A`, `-x`, `x-`, 64 chars, s3 `< 3`, `1.2.3.4`), `_`→`-` normalization; every `forkfile` fence in `README.md`/`docs/**` still passes.
- `cargo test -p slipfork-forkfile` and `cargo test -p slipfork-lsp --test critical_lsp` green (completions/references include `policy`).
- `rg -c static_cdn crates/slipfork-forkfile crates/slipfork-lsp editors docs README.md` → no matches.
- `cargo clippy -p slipfork-forkfile -p slipfork-lsp --all-targets -- -D warnings` and `uv run --no-project tools/guard.py` exit 0.
