# Log M3.22

## 2026-11-16T18:15 [lead]

created (task) Delete leftover slipfork-ospp test-server Cargo feature; move reference OSPP server doubles into tests/support

## 2026-11-16T18:35 [lead]

SCOPE ADD (lead): also replace the one-shot 60s S3 readiness wait in crates/slipfork/tests/critical_ospp_daemon.rs with bounded polling (the transient flake observed during M3.1 attempt 3), plus delete the test-server feature. Same file, same determinism goal.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T19:16 [lead]

edited fields

## 2026-11-16T19:16 [lead]

depends_on widened to M3.1 + M3.15: M3.22 edits crates/slipfork/Cargo.toml and crates/slipfork/tests/support/mod.rs, which M3.15 also writes, so the path lock must serialize them (M3.15 owns the daemon/CLI rewrite; M3.22 lands after).
