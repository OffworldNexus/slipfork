---
id: M3.44.2
title: "Delete SeaweedFS, native Rust Btrfs S3 server with SigV4 & per-bucket policy"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.44.1]
paths:
  - Cargo.toml
  - Cargo.lock
  - "crates/slipfork-world/src/resources.rs"
  - "crates/slipfork-world/src/resources/**"
  - "crates/slipfork-world/src/control/**"
  - "crates/slipfork-world/src/bin/world_init/**"
  - "crates/slipfork-world/src/lib.rs"
  - "crates/slipfork-world/tests/**"
  - crates/slipfork/src/world/runtime/helpers.rs
  - crates/slipfork/src/world/runtime.rs
  - crates/slipfork/src/daemon/view.rs
  - "crates/slipfork-ingress/src/route/**"
  - "crates/slipfork-ingress/tests/route_table_journey.rs"
  - "crates/slipfork-ingress/tests/critical_ingress.rs"
  - crates/slipfork/tests/critical_world_journey.rs
  - crates/slipfork/tests/critical_daemon.rs
  - crates/slipfork/tests/status_surface.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T20:17"
updated: "2026-11-21T20:27"
---

# Goal
Remove SeaweedFS 100% and serve S3 natively inside `slipfork-world` over `<world>/s3/<bucket>/` on Btrfs: `< 1ms` boot, no extra container, path + virtual-host addressing, SigV4 HMAC-SHA256, sidecar metadata, per-bucket `policy`.

# Context
- Delete: `crates/slipfork-world/src/resources/s3.rs` (321 LOC `SeaweedfsPlan`), `resources.rs:141` `seaweedfs()` + `:149-170` 30s `weed shell` wait, `crates/slipfork/src/world/runtime/helpers.rs:80-105` weed spawn/poke, `crates/slipfork/src/daemon/view.rs:44` seaweed row, static_cdn/seaweed remnants in `crates/slipfork-ingress/src/route/rules.rs` (+ `crates/slipfork-ingress/tests/route_table_journey.rs`) and the `weed`/`static_cdn` assertions in `crates/slipfork/tests/critical_world_journey.rs:526,661,683`, `critical_daemon.rs`, `status_surface.rs`, `crates/slipfork-world/tests/{critical_pod_runtime.rs,pod_control_lab.rs}`.
- New cohesive modules under `crates/slipfork-world/src/resources/s3/`: `server.rs` (`S3Server` on `hyper 1.12` + `hyper-util` + `http-body-util`, one `TcpListener` on `:8333`, HTTP/1.1 keep-alive, bounded body, streaming responses; <= 250 LOC), `object_ops.rs` (`S3BucketStore`: PUT/GET/HEAD/DELETE over `<world>/s3/<bucket>/<key>` with tmp+rename atomicity and `Content-Type`/`ETag`(md5 via `md-5`)/`Content-Length` sidecars in a sibling `.meta` file; <= 300), `listing.rs` (`ListObjectsV2` `?list-type=2`, `prefix`, `continuation-token`, `max-keys`, `Contents`/`IsTruncated`/`NextContinuationToken`; <= 200), `policy.rs` (`BucketPolicy` enforcement: private → 403 anon GET/HEAD; public-read → 200 anon GET/HEAD, 403 anon PUT/DELETE/list), `sigv4.rs` (`SigV4Verifier` with `hmac 0.12.1` + `sha2 0.10` + `subtle`, `UNSIGNED-PAYLOAD` accepted on GET/HEAD, 403 on bad signature; <= 200).
- Addressing: path-style `http://<host>:8333/<bucket>/<key>` and virtual-host `<bucket>.s3.<world>.localhost`; resolve bucket from host or first path segment in exactly one place (`server.rs`), never per-handler.
- Control: `crates/slipfork-world/src/control/mod.rs:50` add `Request::ConfigureS3(S3WorldConfig)` (buckets, policy, access_key, secret_key, root path); keep `Dial { port }` at `:96`; wire through `crates/slipfork-world/src/bin/world_init/{main.rs,control.rs,respond.rs}` — the init loop binds the listener itself (no child process, no readiness wait) and reaps it on teardown.
- Daemon: `crates/slipfork/src/daemon/view.rs` renders bucket rows (endpoint/policy; `<bucket>.url` secret never printed); `crates/slipfork/src/world/runtime.rs` resolves `s3 <bucket> { policy ... }` into `S3WorldConfig`; `helpers.rs` drops weed entirely.
- Deps already cached (offline-safe): `hyper 1.12`, `hyper-util 0.1.21`, `http-body-util 0.1.5`, `hmac 0.12.1`, `subtle`, `md-5` inherited through `[workspace.dependencies]`.
- `<= 500 LOC`: `resources.rs` shrinks to a thin registry (target <= 180) with `resources/mod.rs` re-exports; each `s3/*.rs` stays within the limits above.
- Out of scope: boot DAG/stamps (`M3.44.4`) and terminal rendering (`M3.44.6`). Zero mocks — only the OSPP loopback reference server is a permitted substitute.

# Acceptance
- `cargo test -p slipfork --test critical_world_journey` green: real PUT/GET/HEAD/DELETE/ListObjectsV2 over path + vhost, `Content-Type`/`ETag`/`Content-Length`, good vs bad SigV4 (403), `403` anon GET on private bucket, `200` anon GET + `403` anon PUT/DELETE on `policy public-read`, `Request::ConfigureS3` + `Dial { port: 8333 }`.
- `cargo test -p slipfork-world --test critical_pod_runtime` and `cargo test -p slipfork-world` (`pod_control_lab`, `world_plan_features`) green; `cargo test -p slipfork-ingress` green.
- `rg -c 'seaweedfs|weed ' crates` and `rg -c static_cdn crates` → no matches; `cargo test -p slipfork --test critical_daemon --test status_surface` green.
- `cargo clippy -p slipfork-world -p slipfork -p slipfork-ingress --all-targets -- -D warnings` and `uv run --no-project tools/guard.py` exit 0.
