---
id: M3.70.3
title: "Image engine fidelity: inherit base-image ENV/CMD and resolve relative COPY against WORKDIR"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork-image/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-23T14:06"
updated: "2026-11-23T14:08"
---

# Goal

Fix two native Dockerfile fidelity bugs exposed by the demo prod stages so !dev builds behave exactly like Docker.

1. builder/mod.rs::base_state builds config: ImageConfig::default() for FROM <oci-image>, dropping PulledImage.facts.env/entrypoint/cmd. A later ENV PATH="/app/.venv/bin:$PATH" keeps the literal $PATH and worker exits 127. Seed ImageConfig from image.facts (env map, entrypoint, cmd).
2. crates/slipfork-image/src/copy.rs::resolve_dest(rootfs, dest) always roots at /, so WORKDIR /app + COPY package.json . lands at /. Pass the stage config.workdir from builder/run.rs COPY/ADD dispatch into copy_context/copy_rootfs/add_context; resolve a non-absolute dest as workdir/dest, keep the .. escape refusal.

# Anchors

- crates/slipfork-image/src/builder/mod.rs:204-221 (base_state)
- crates/slipfork-image/src/puller.rs:24-35 (PulledImage.facts), dockerfile/config.rs:13 (ImageConfig)
- crates/slipfork-image/src/copy.rs:15-30,60-70,105-120,202
- crates/slipfork-image/src/builder/run.rs:140-230 (WORKDIR state, COPY/ADD dispatch)

# Acceptance

- Real-disk unit tests: base image with PATH=/usr/bin:/bin + ENV PATH="/app/.venv/bin:$PATH" expands to /app/.venv/bin:/usr/bin:/bin and inherited CMD survives unless overridden; WORKDIR /app then COPY package.json ./ and COPY src ./src land under /app and workdir stays /app.
- cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p slipfork-image exit 0.
- Do not touch other crates, docs, or support/.
