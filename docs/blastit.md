# Blastit Integration

[Blastit](https://github.com/OffworldNexus/blastit) enforces per-route zero-trust sandboxes and wire-level SQL/Redis/HTTP allowlists (`blastit.toml`). Slipfork and Blastit are designed as complementary halves:

| Layer | Responsibility |
|---|---|
| **Blastit** (inside the world) | Per-route UID sandboxes, WASM ingress validation, credential stripping, and wire-level query allowlists |
| **Slipfork** (around the world) | Provisioning backing services from `snow.yml`, CoW snapshot DAGs, parallel per-test worlds, and worktree forks |

---

## Fast Parallel `--learn` & `blastit check`

Running `blastit check` or `blastit dev --learn` serially across a large browser/BDD test suite normally bottlenecked on sequential test execution.

With Slipfork, every parallel CoW test world boots your application under Blastit's wire observer and streams its observed `(route, proxy, query)` grants back to the Slipfork coordinator before the ephemeral world is destroyed:

```console
# Learn every route & wire grant across 64 parallel CoW test clones in one pass:
$ slipfork test --blastit-learn
[dag]     snapshots warm (0 ms)
[run]     64 tests across 16 CoW worlds ... 64 passed in 4.1s
[blastit] merged 142 wire observations across 19 routes -> updated blastit.toml

# Enforce strict 4-way completeness in CI (unrouted, missing grant, dead grant, unexercised route):
$ slipfork test --blastit-check --json
```

### How Observation Merging Works

1. Each CoW test clone mounts a dedicated collector socket (`ObsTarget` JSONL sink) managed by Slipfork's `ObsCollector` adapter.
2. Route hits (`executor_ran == true`) and parameterized SQL/Redis/HTTP wire frames emitted inside Clone `1..N` are unioned deterministically on the host after all tests pass.
3. Slipfork invokes `blastit` audit/merge once over the complete union — giving **100% single-pass completeness** at **full CoW parallelism**!
