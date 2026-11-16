# Log M2.20

## 2026-11-15T16:40 [planner]

created (task) Release 0.2.0: version bump, unused-dependency audit, full make check and read-only make bench

## 2026-11-15T18:57 [lead]

edited fields

## 2026-11-15T21:03 [lead]

edited fields

## 2026-11-15T21:03 [lead]

edited fields

## 2026-11-15T23:16 [lead]

status ready -> active

## 2026-11-15T23:16 [lead]

attempt 1/3 started

## 2026-11-15T23:16 [lead]

base e08a85f153a794ab29c67a5fdac7726cf9329c43

## 2026-11-15T23:51 [implementer]

dependency audit (declared dep => first consumer): slipfork: clap=>src/bin/slipforkd.rs, hex=>src/main.rs, sha2=>src/runtime.rs, serde/serde_json=>src/daemon.rs, slipfork-dag=>src/daemon.rs, slipfork-edge=>src/bin/git-remote-slipfork.rs, slipfork-engine/ospp/snow=>src/bin/slipforkd.rs, slipfork-vcs/world=>src/bin/git-remote-slipfork.rs, tempfile(dev)=>src/project.rs; slipfork-dag: hex/sha2=>src/fs.rs, serde/slipfork-world/thiserror/toml=>src/lib.rs, tempfile(dev)=>src/fs.rs; slipfork-edge: httparse=>src/ingress.rs, listenfd=>src/activation.rs, serde=>src/daemon.rs, serde_json/slipfork-engine/world=>src/daemon/client.rs, thiserror=>src/lib.rs; slipfork-engine: hex=>src/reflink_netns.rs, libc/serde_json=>src/bin/slipfork-world-init.rs, serde=>src/env.rs, sha2=>src/rootfs/input.rs, slipfork-world=>src/conformance.rs, thiserror=>src/lib.rs, tempfile(dev)=>src/oci.rs; slipfork-ospp: hex/rustls/serde/serde_json/slipfork-dag/snow/world/tungstenite/webpki-roots=>src/client.rs, p256/sha2=>src/device_key.rs, thiserror=>src/error.rs, tokio=>src/lib.rs, tpmd-client/tpmd-ipc=>src/tpmd.rs, zstd=>src/disk_dump.rs, tempfile/tpmd-proto(dev)=>tests/critical_ospp.rs; slipfork-snow: base64/hex/serde_json=>src/faceless.rs, getrandom=>src/provider.rs, serde/slipfork-world/thiserror=>src/model.rs, serde-saphyr=>src/model.rs, tempfile(dev)=>src/provider.rs; slipfork-vcs: slipfork-world=>src/git.rs, thiserror=>src/ports.rs, tempfile(dev)=>tests/support/mod.rs; slipfork-world: hex/serde/sha2/thiserror=>src/lib.rs, serde_json(dev)=>tests/critical_world.rs. Orphans deleted: socket2 (slipfork-edge + workspace) and getrandom (slipfork-ospp); tpmd-proto pin hoisted to [workspace.dependencies].

## 2026-11-16T01:26 [lead]

bench note: default 16x8 exceeds 50 min on this host (harness hangs after seeding under load); a bounded --num-tests 1 --workers 1 run exits 0, proving the pipeline. A 3h default make bench is running to satisfy acceptance #3; if it cannot finish, this node stays open rather than weakening the acceptance.
