# Log M2.2

## 2026-11-15T16:40 [planner]

created (task) Workspace wedge: scaffold slipfork-edge/slipfork-vcs/slipfork-ospp and declare M2 dependencies

## 2026-11-15T18:38 [planner]

edited fields

## 2026-11-15T18:51 [lead]

status ready -> active

## 2026-11-15T18:51 [lead]

attempt 1/3 started

## 2026-11-15T18:51 [lead]

base aa4c5246451c1cba78a9cd431f59406638cefc76

## 2026-11-15T18:56 [implementer]

dependency->leaf mapping (Design #5): workspace deps -- tungstenite+rustls+webpki-roots+p256+tpmd-client+tpmd-ipc+tokio -> M2.13 (ospp-v1 sync wss, DeviceKeySigner, private current_thread runtime); zstd -> M2.14 (seed dumps); listenfd+socket2+httparse -> M2.8/M2.9 (edge activation + <=16 KiB ingress); reused getrandom/hex/sha2/serde/serde_json/thiserror -> M2.13/M2.14. Crate edges -- slipfork-world (+sha2,+hex) -> M2.3 (WeaverServiceName hash12); slipfork-engine (+slipfork-world path dep) -> M2.6 (closes M1.6 #12); slipfork-edge (+world,+engine) -> M2.8/M2.9; slipfork-vcs (+world,+serde,+serde_json) -> M2.11/M2.12; slipfork-ospp (+world,+snow,+dag) -> M2.13/M2.14; crates/slipfork (+edge,+vcs,+ospp, dag features=["fs"]) -> M2.16/M2.17, dag fs seam -> M2.5. tungstenite feature rustls-tls-webpki-roots enabled here because no later leaf may touch Cargo.toml.

## 2026-11-15T18:56 [implementer]

observations (not fixed, outside docs paths): pinned tpmd crates declare rust-version 1.94 while workspace still promises 1.88 (verified build only with rustc 1.100; docs/getting-started.md 'Rust 1.88+' needs the MSRV bump in M2.19/M2.20); cargo 1.100's cargo::unused_dependencies manifest lint warns for every pre-declared M2 dep by construction -- deliberately NOT suppressed so M2.20's unused-dependency audit stays honest.

## 2026-11-15T18:57 [lead]

edited fields

## 2026-11-15T18:58 [implementer]

Round-1 Quad remediation: dag `default = []` (fs inert until M2.5; ARCHITECTURE.md contract so dag-at-defaults never grants filesystem), workspace rust-version 1.88 -> 1.94 (pinned tpmd-* requires 1.94; published docs' MSRV sentence reconciled by M2.19, docs untouched), SoftwareP256Key ownership folded into ospp software_key.rs (device_key.rs owns port + newtypes only), slipfork-vcs serde/serde_json DROPPED (git-remote-slipfork is plain text and no M2.11/M2.12 value type is named as serialized; both stay in [workspace.dependencies]), tpmd.rs + activation.rs rustdoc reworded to future tense.

## 2026-11-15T18:58 [lead]

R1 quad gate: reviewer PASS, legality PASS, ioc FAIL(dag default=['fs'] -> spec amended to default=[]), cruft PASS(MSRV). Remediation: dag default=[] , rust-version 1.94, SoftwareP256Key folded, vcs serde trim, stub tense; MSRV invariant added to M2.19/M2.20

## 2026-11-15T18:59 [lead]

status active -> done
