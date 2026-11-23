---
id: M3.65
title: "Implement Forkfile 0.1 on/else, domain.name vs self.{url,domain,scheme}, \"${...}\" interpolation & secret/var(KEY, generator)"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.60, M3.61]
paths: ["crates/slipfork-forkfile/**", "crates/slipfork-ospp/**", "crates/slipfork/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-23T01:11"
updated: "2026-11-23T01:11"
---

# Goal

Implement the locked `# forkfile: 0.1` expression & conditional upgrades cleanly and purge every trace of `domain` vs `url` confusion, `?? <fn_call>` defaults, and `random_alnum` from the compiler and runtime:

## 1. `domain <id>` vs `self` (Strict Domain != URL Separation)

- **`domain <id>` stays (`domain main`, `domain dom_a`, `domain dom_b`, `domain mail`):**
  - The **first declared `domain`** (`doc.app_domains().first()`) is the app's **default domain** (`<world>.localhost` locally, `<fork>--<world>.localhost` on forks).
  - Subsequent declared `domain <id>`s are **alternative domains** (`<id>.<world>.localhost` locally, `<id>.<fork>--<world>.localhost` on forks).
  - **Every `domain <id>` exposes `<id>.name` ONLY** (`SymbolKind::Domain` properties = `&["name"]` in `crates/slipfork-forkfile/src/check/symbols.rs` & `eval/values.rs:112`). A domain is a bare hostname without a scheme (`"demo.localhost"`, `"mail.demo.localhost"`).
  - Writing `<domain_id>.url` or `<domain_id>.domain` MUST fail statically with a sharp diagnostic + `next: a domain is a bare hostname without a scheme; use '<id>.name' for the hostname, 'self.url' for the app's primary URL, or '"${self.scheme}://${<id>.name}"' for an alternate domain's URL`.
- **`self` built-in reserved symbol (`self.url`, `self.domain`, `self.scheme`):**
  - `self` represents the app itself and is reserved across all declarations (`domain self`, `service self`, `image self`, etc. fail statically).
  - Properties of `self` (strictly checked in `check/expr.rs` — **delete** the unchecked `if base == "app" || base == "deployment"` bypass at `check/expr.rs:109`!):
    - `self.url` -> `"http://<world>.localhost"` (`"http://<fork>--<world>.localhost"` on forks)
    - `self.domain` -> `"<world>.localhost"` (`"<fork>--<world>.localhost"` on forks)
    - `self.scheme` -> `"http"` locally (`"https"` in production cloud deployments)
  - In `migrate/template.rs`: `domains::<id>` migrates to `<id>.name` (NOT `<id>.domain`), and `resources::app.<prop>` / `deployment::<prop>` migrate to `self.<prop>` (for `url`, `domain`, `scheme`; reject unknown properties cleanly).

## 2. `"${<expr>}"` String Interpolation in Double-Quoted Strings

- Inside `"..."` double-quoted strings in `arg` and `env` expressions (`lexer.rs` / `expr_parse.rs`), parse `${<expr>}` interpolations (e.g. `LOGIN_URL = "${self.url}/login"`, `ALT_URL = "${self.scheme}://${dom_b.name}/callback"`), with `\${` escaping literal `${`. Single-quoted `'...'` strings remain literal without interpolation.
- Type-check every interpolated `<expr>` in `check/expr.rs` and propagate secret taint (`"${secret(TOKEN)}"` is secret-tainted -> rejected in build-time `arg` and redacted in `slipfork env`).
- `slipfork fmt` round-trips interpolated `"..."` strings cleanly.

## 3. `secret(KEY[, <generator>])` & `var(KEY[, <generator>])` ("Set If Unset") vs Deterministic `??`

- **Generators (`GeneratorSpec` — NOT general expressions!):**
  - `random_string(n)` (`[A-Za-z0-9]` of length `n` in `1..=256`; **scorched-earth delete `random_alnum`** everywhere!), `random_hex(n)` (`n` hex chars), `random_base64(n)` (`n` URL-safe unpadded base64 chars), `random_uuid()` (v4 UUID).
  - **Generators are ONLY valid as the 2nd argument of `secret(KEY, <generator>)` or `var(KEY, <generator>)`!**
  - Calling `random_*` anywhere else (standalone `FOO = random_string(50)`, inside `"${...}"`, or on the RHS of `??` like `secret(K) ?? random_hex(32)`) MUST fail statically with a sharp diagnostic (`next: pass the generator as the second argument: secret(KEY, random_string(50))`).
- **Ban ALL function calls on the RHS of `??`:**
  - In `<left> ?? <right>`, `<right>` MUST NOT contain any function call (`secret(...)`, `var(...)`, `random_*(...)`). `var(THIRD_PARTY_API_URL) ?? "https://thirdparty.com"`, `var(ORIGIN) ?? self.url`, and `var(CALLBACK) ?? "${self.url}/cb"` are valid; `var(A) ?? var(B)` or `secret(K) ?? random_hex(32)` fails statically (`function calls are not allowed as '??' defaults`).
- **Real Generate-Once-and-Persist Semantics (Law 1 IoC/DI Clean!):**
  - Keep `slipfork-forkfile` free of ambient OS I/O: pass `GeneratorSpec` to the `secret_resolve` / `var_resolve` callbacks supplied by `crates/slipfork` (`world/runtime.rs`, `daemon/host.rs`, `daemon/lifecycle/ps.rs`).
  - In `crates/slipfork`, when `secret(KEY, gen)` sees `KEY` unset in `FileSecretVault` (`secrets.json`), generate from CSPRNG (`/dev/urandom` / `getrandom`) and **persist** into `FileSecretVault` so `slipfork secret list`, subsequent `up`s, and `fork`s all reuse the stored secret! Same for `var(KEY, gen)` into a `0600` `vars.json` in `state_dir` (when `KEY` is absent from both process env and `vars.json`). (`env` / `ps` inspection commands can either persist-on-first-resolve or read consistently so `slipfork env` and `slipfork up` agree!)
  - Update `migrate` so legacy `secret(K) ?? random_*(n)` / Snow generated secrets emit `secret(K, random_*(n))` (and `random_alnum` -> `random_string`).

## 4. `on <expr> ... else ...` Everywhere `on` Is Valid

- Support optional `else` clauses (`else { ... }`, `else: <item>`, and chained `else on <expr> ...`) in all 5 `on` scopes: top-level items, `image` items, `service` items, `s3` items, and `{ ... }` command blocks (`before-start`, `pre-deploy`, `post-deploy`, `failed-deploy`, `run`).
- Evaluate `else` iff `<expr>` is `false`; validate/type-check/taint-check both branches statically; format `} else {`, `} else on <expr> {`, and `else: <item>` canonically in `slipfork fmt`.

# Acceptance

- `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings` exits `0`.
- `cargo test -p slipfork-forkfile -p slipfork-ospp -p slipfork-ingress -p slipfork` exits `0`.
