---
id: M3.63
title: "Forkfile 0.1 language upgrades: on/else, self.url/self.domain, \"${...}\" string interpolation & secret/var(KEY, generator)"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: [M3.60, M3.61]
paths: ["crates/slipfork-forkfile/**", "crates/slipfork-ingress/**", "crates/slipfork/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-23T01:06"
updated: "2026-11-23T01:11"
---

# Goal

Implement four cohesive `# forkfile: 0.1` language improvements cleanly inside `crates/slipfork-forkfile` (and wire runtime `self` + persisted `var(KEY, <generator>)` + strict primary-domain routing in `crates/slipfork` / `crates/slipfork-ingress`):

1. **`on <expr> ... else ...` everywhere `on` is valid:**
   - Support optional `else` branches (`else { ... }`, `else: <item>`, and chained `else on <expr> ...`) in top-level items, `image` items, `service` items, `s3` items, and `{ ... }` command blocks (`before-start`, `pre-deploy`, `post-deploy`, `failed-deploy`, `run`).
   - `slipfork fmt` formats `} else {` / `} else on <expr> {` and `else: <item>` canonically.
2. **Built-in `self` primary world domain (`self.url`, `self.domain` — zero bogus `domain main`):**
   - `self` is a built-in reserved object in `Forkfile` expressions exposing:
     - `self.domain` -> `<world>.localhost` (`<fork>--<world>.localhost` on forks)
     - `self.url` -> `http://<world>.localhost` (`http://<fork>--<world>.localhost` on forks; or with `:port` only if non-80 ingress port)
   - Declaring a component or `domain` named `self` is rejected statically (`'self' is reserved for the primary world domain`).
   - Unfiltered `route /path` binds **strictly** to the primary world domain (`self`), NOT to `"the first declared domain"`! `domain <id>` (e.g. `domain mail`) is strictly for extra subdomains targeted by `route ... domain <id>` and referenced via `<id>.url` / `<id>.domain`.
3. **Double-quoted string interpolation `"${<expr>}"`:**
   - Inside `"..."` double-quoted strings in `env` and `arg` expressions, parse and evaluate `${<expr>}` segments (e.g. `LOGIN_URL = "${self.url}/login"`, `API_ORIGIN = "${var(PUBLIC_ORIGIN) ?? self.url}/api"`), with `\${` escaping a literal `${`. Single-quoted `'...'` strings remain literal.
   - Secret taint propagates through string interpolation (`"${secret(TOKEN)}"` is secret-tainted -> rejected in build-time `arg` and redacted in `slipfork env`!).
   - Scorched-earth delete any `${{ ... }}` expression syntax if any exists.
4. **`secret(KEY[, <generator>])` & `var(KEY[, <generator>])` ("set if unset") vs deterministic `??`:**
   - **Generators:** `random_string(n)` (alphanumeric `[A-Za-z0-9]` of length `n` >= 1; **replaces `random_alnum(n)`** via scorched-earth pre-1.0 deletion!), `random_hex(n)`, `random_base64(n)`, `random_uuid()`.
   - **2nd argument of `secret` / `var` ONLY:** `secret(KEY, <generator>)` and `var(KEY, <generator>)` read `KEY` from the world's secret vault / local env-var store, and **if `KEY` is unset, generate the value once and persist it** into the secret vault / local env-var store!
   - **Ban function calls on RHS of `??` and standalone generators:**
     - `secret(SECRET_KEY) ?? random_hex(32)` or `var(X) ?? var(Y)` or `FOO = random_string(50)` is a **static compile error** (`function calls are not allowed as '??' defaults` / `generators are only allowed as the second argument of secret(KEY, ...) or var(KEY, ...)` with a crisp `next:` remedy like `next: write secret(SECRET_KEY, random_string(50))`!).
     - `<expr> ?? <default>` allows strictly non-call deterministic value expressions on the RHS (string literals, interpolated strings `"${self.url}/..."`, booleans/numbers, `<id>.<prop>` like `self.url`), e.g. `THIRD_PARTY_API_URL = var(THIRD_PARTY_API_URL) ?? "https://thirdparty.com"`.
   - Update `slipfork migrate` and crate tests (`crates/slipfork-forkfile`, `crates/slipfork-ingress`, `crates/slipfork`) to match the clean grammar (`domain main`, `random_alnum`, and `?? random_*` removed everywhere in Rust tests!).

# Acceptance

- `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings` exits `0`.
- `cargo test -p slipfork-forkfile -p slipfork-ingress -p slipfork` exits `0`.
