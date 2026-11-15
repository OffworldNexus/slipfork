---
id: M2.26
title: "slipfork-dag: [world] config (world.image + world.stage only, fail-closed)"
kind: task
status: review
owner: implementer
priority: 1
depends_on: [M2.24]
paths: ["crates/slipfork-dag/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-15T20:40"
updated: "2026-11-15T21:03"
---

# Goal

Land the sole `[world]` schema from `ADR-0009` §`Composition Root & DI Tree` in `crates/slipfork-dag`: exactly two optional keys, `deny_unknown_fields`, zero fallbacks, zero precedence ladder.

```rust
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldImageConfig {
    pub image: Option<String>, // snow.yml BuildImage id
    pub stage: Option<String>, // named `FROM ... AS <stage>`
}
```

# Context

- `ADR-0009` is the normative source (read it first: `docs/decisions/ADR-0009-world-base-image-and-image-contract.md`).
- Validation is a pure function over `(&WorldImageConfig, &SnowFile)`: it must NOT depend on `slipfork-snow` types inside the serde struct (dag stays dependency-light); accept the resolved list of build-image ids + named stages as plain arguments.
- Error variants must be typed (`WorldConfigError`), never stringly `Toml(...)`: unknown key, image not declared in `snow.yml`, image ambiguous (>= 2 build images and no `image`), stage named but absent in the Dockerfile's stage list (list is passed in by the caller), stage ambiguous.
- Zero code may read the filesystem here (Dockerfile parsing is `M2.27`'s job at the composition root).

# Acceptance

- [ ] `cargo test -p slipfork-dag --test critical_world_config` exits 0 and covers: both keys absent + single image = ok; both absent + two images = fail closed naming `world.image`; `image` naming an undeclared id = fail closed; unknown key (`base`, `rootfs`, `dockerfile`) = fail closed; `stage` set = ok; both set = ok.
- [ ] `rg -n "world\.base|slipfork_base|CuratedBase|prebuilt" crates/slipfork-dag` exits 1.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
- [ ] `make check` exits 0.
