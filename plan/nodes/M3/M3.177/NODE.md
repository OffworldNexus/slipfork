---
id: M3.177
title: "M3.148a: status/doctor/commands on Report; enable the RawPrint gate"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork/src/status/**"
  - "crates/slipfork/src/doctor/**"
  - crates/slipfork/src/commands/mod.rs
  - crates/slipfork/src/commands/auth.rs
  - crates/slipfork/src/commands/env.rs
  - crates/slipfork/src/commands/secret.rs
  - crates/slipfork/src/commands/features.rs
  - crates/slipfork/src/commands/open.rs
  - crates/slipfork/src/commands/fmtmigrate.rs
  - "crates/slipfork/src/ui/report/**"
  - crates/slipfork/src/world/up.rs
  - tools/guard.py
  - tools/tests/test_tools.py
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T18:26"
updated: "2026-11-25T18:38"
---

# Goal

Move `status/**`, `doctor/**` and `commands/{mod,env,secret,features,open,fmtmigrate}.rs` onto `Report`; delete dormant `commands/auth.rs`, `status::rows::State` and `report_details`. Then **enable the RawPrint rule in `tools/guard.py`**: user-facing `println!`/`eprintln!` outside the renderer fail the guard, with a documented allowlist (`ui/report/**`, the boot append sink, the `daemon/**`/`supervise`/`up` journal, `prompt_secret`, clap help).

# Acceptance

- `ps`/`status`/`doctor`/`env`/`secret`/`features`/`open`/`fmt --check` byte-identical before/after (styled + `NO_COLOR` + `--json`), `ps` keeps lineage/broken/offline behaviour.
- Guard fails on a deliberately planted raw print and passes on the branch; allowlist documented in the guard file.
- `make check` (`CARGO_TARGET_DIR=/var/tmp/m3177-target CARGO_PROFILE_DEV_DEBUG=0`, delete after); Triad (all three); commit `[M3.177] ...`, done, no push.
