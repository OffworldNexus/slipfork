# Log M3.21

## 2026-11-16T17:25 [strategist]

created (task) Atomic scorched-earth deletion: remove IncusEngine, ReflinkNetnsEngine, and FICLONE helpers

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-17T11:17 [lead]

ADD (flake, pre-existing): crates/slipfork-engine/src/linux_cow.rs:386-408 tests compare a process-wide /proc/self/fd COUNT, so parallel lib tests race (seen: '12 -> 13', '16 -> 18'); compare the fd SET or serialize those two tests behind a test-only Mutex. linux_cow.rs is in your paths.

## 2026-11-17T11:37 [lead]

ADD: crates/slipfork-engine/Cargo.toml package description still reads 'Incus REST (/1.0) and Linux FICLONE + unprivileged netns/userns drivers' — update it to BtrfsEngine/SubvolumeDriver in your deletion sweep.

## 2026-11-17T11:37 [lead]

status ready -> active

## 2026-11-17T11:37 [lead]

attempt 1/3 started

## 2026-11-17T11:37 [lead]

base 053596ee33193d5e947e933bf2c0506f366ff3f8

## 2026-11-17T12:17 [lead]

edited fields

## 2026-11-17T12:17 [lead]

SCOPE WIDENED (Quad gate cruft MAJOR + legality/ioc MINORs): also fix the stale 'Incus image name' doc phrase in rootfs/mod.rs:82, the 'only unsafe module' claim in bin/slipfork-world-init.rs:14, the plural-adapter crate rustdoc in lib.rs:1-12, two no-op/lying dead_code attributes in rootfs/{dockerfile,input}.rs, and a missing SAFETY comment on the test-only mem::zeroed in linux_cow.rs.

## 2026-11-17T12:18 [lead]

Quad gate 1/2 results: ioc PASS (1 MINOR: missing SAFETY comment on test-only mem::zeroed; verified descriptors_aliasing is #[cfg(test)], only impl WorldEngine is btrfs_engine.rs:1401). legality PASS (2 MINOR: slipfork-world-init.rs:14 'only unsafe module' lie; rootfs/mod.rs:82 'Incus image name'). cruft MAJOR: rootfs/mod.rs:82 Incus residue → path-widened into M3.21, not M3.23, because the node's own acceptance grep is workspace-scoped for crates/. All applied by a follow-up implementer.

## 2026-11-17T12:36 [lead]

Gate passed: acceptance grep is 2 hits, both NEGATIVE assertions that a Btrfs refusal never advertises a reflink fallback (crates/slipfork/tests/support/mod.rs:307, packages/opencode-slipfork/tests/e2e.critical.test.ts:355) — deleting the word there would weaken a contract; tracked as cross-node residual for M3.23. clippy --workspace -D warnings, make check (gate 1 + rs-lint) and cargo test -p slipfork-engine --lib 62 passed are green; critical_guard --base 053596e --node M3.21 has no unauthorized change (all 6 protected edits belong to M3.15's paths).

## 2026-11-17T12:36 [lead]

status active -> done

## 2026-11-17T12:38 [lead]

FINAL: functional PASS, legality PASS (minors: RESULT inventory understates 9 files; the rootfs/mod.rs 'Incus image name' residual is already fixed; the 2 remaining grep hits are legitimate negative assertions outside paths). make check green, 62 engine lib tests, fd-race identity-based. Committing.

## 2026-11-17T12:38 [lead]

status done -> done
