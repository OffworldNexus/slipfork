---
name: Rust Standards (`crates/*`)
description: Idiomatic Rust 2024, strict IoC/DI trait boundaries, error handling, unsafe confinement, and formatting/linting rules for Slipfork crates.
---

# Rust Standards (`crates/*`)

1. **Edition & Formatting:**
   - Workspace uses Rust `edition = "2024"` (`rustfmt.toml` pins `edition = "2024"` and `style_edition = "2024"`).
   - Always verify with `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings`.
2. **Strict IoC / DI by Construction:**
   - Domain crates (`slipfork-snow`, `slipfork-dag`, `slipfork-test`) are **100% pure of ambient OS globals**: they never call `std::env::var`, `std::process::Command`, or wall-clock `SystemTime::now()` directly — inject traits (`SecretProvider`, `WorldEngine`, `Clock`)!
   - Adapter implementations (`FileVaultProvider`, `HttpSecretProvider`, `IncusEngine`, `ReflinkNetnsEngine`) live in clearly named adapter modules and implement the domain traits, tested against **shared trait conformance suites**.
   - Only the binary crate (`crates/slipfork/src/main.rs`) acts as the **Composition Root**.
3. **Safety & Error Handling:**
   - `#![forbid(unsafe_code)]` on every crate except `slipfork-engine`'s thin Linux syscall module (`ficlone` / namespace wrapper), where every `unsafe` block carries an explicit `// SAFETY:` invariant proof.
   - Zero `unwrap()` / `expect()` / `panic!()` in non-test library code; return typed `thiserror` errors with actionable user diagnostics (never leaking secret values in `Debug` or `Display`!).
   - Redact secret strings in custom `Debug` implementations (`SecretValue` prints `"[REDACTED]"`).
4. **Minimal Dependencies:**
   - Keep `Cargo.toml` lean; reuse workspace dependencies (`[workspace.dependencies]`) and never add a crate when stdlib or an existing workspace crate suffices.
