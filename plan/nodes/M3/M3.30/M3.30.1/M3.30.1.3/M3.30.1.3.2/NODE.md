---
id: M3.30.1.3.2
title: "Rootfs ports: delete AbiProbe/BuildxExporter/OciImageStore traits, concrete disk/ELF/status"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30.1.3.1]
paths: [Cargo.toml, Cargo.lock, "crates/slipfork-engine/**"]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-18T22:46"
updated: "2026-11-18T23:09"
---

# Goal
Slice 2 of `plan/nodes/M3/M3.30/M3.30.1/M3.30.1.3/NODE.md`: rootfs ports become concrete.

# Work
- Delete `AbiProbe`, `BuildxExporter`, `OciImageStore` traits + `ElfAbiProbe`/`CliBuildxExporter`/`DockerOciImageStore`/3 `NoOciStore`/`NoServices`/`FixtureExporter` and any single-impl shim.
- Wire the concrete docker-backed store + buildx exporter + ELF ABI prober at the composition root; `crates/slipfork-engine/src/rootfs/mod.rs` keeps the `WorldRootfs` contract + `guest_binary`.
- Rewrite `critical_rootfs_contract` + `critical_rootfs_live` on real disk/ELF/status probes and the real exporter (no fakes, disk tempdirs under `~/.cache/slipfork`).

# Constraints
Contract 6-rule fail-closed behavior must stay byte-identical; no mocks; no aliases. No RESULT.md.

# Acceptance
- `! grep -rEn "AbiProbe|BuildxExporter|OciImageStore|NoOciStore|NoServices|FixtureExporter|ElfAbiProbe|CliBuildxExporter|DockerOciImageStore" crates/`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork-engine --test critical_rootfs_contract --test critical_rootfs_live`
- `cargo test -p slipfork-engine`
