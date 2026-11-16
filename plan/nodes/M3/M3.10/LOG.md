# Log M3.10

## 2026-11-16T16:58 [planner]

created (task) Rewrite docs/design/do-vm-harness.md: Btrfs volume, crun, AppArmor userns, persistent --identity

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T18:59 [lead]

REQUIREMENT (from M3.7 legality review): the AppArmor userns profile section must state that the profile permits the world-init path to create user namespaces (i.e. it cannot simply deny userns globally) and name /usr/bin/unshare explicitly, matching what M3.16 provisions on the VM.

## 2026-11-16T20:48 [lead]

status ready -> active

## 2026-11-16T20:48 [lead]

attempt 1/3 started

## 2026-11-16T20:48 [lead]

base 4d5b02efd7444c3413a6d238271c2a700ed9fd5a

## 2026-11-16T21:01 [lead]

status active -> review

## 2026-11-16T21:02 [lead]

edited fields

## 2026-11-16T21:02 [lead]

QUAD GATE attempt 1 (legality FAIL): BLOCKER silent destructive fallback — `blkid ... || true` coerces any blkid error (rc 4 = I/O error/unreadable/concurrently detaching) into "fresh volume" then mkfs.btrfs -f, contradicting up-never-destroys; must branch on exit status (0 use value, 2 blank -> mkfs, else fail closed naming device/rc/stderr). MINOR: label --only apparmor as M3.15 target state; state that slipfork-world-init AppArmor attachment is only proven at first live suite; cite or re-verify the DigitalOcean udev-automount claim. depends_on widened to M3.25 (crun host-vs-rootfs + host-gate ruling).

## 2026-11-16T21:03 [lead]

IOC GATE attempt 1 (FAIL): MAJOR identity default lives inside the product cache root (<state_dir>/$XDG_CACHE_HOME) — keep the owner-specified absolute default ~/.cache/slipfork/do-vm/id_ed25519 but resolve it independently of Layout::resolve/--state-dir and document it as harness-owned, never product state; MAJOR two AppArmor profile writers (harness golden text vs `slipfork daemon unit --only apparmor`) — one renderer/owner + preflight equivalence check; MAJOR hardcoded world-init path — derive from the same build-resolved path or fail closed on test -x; MINOR explicit --state-dir/--runtime-dir injection instead of ambient HOME coupling; MINOR one HOME contract per command; MINOR --identity semantics per subcommand and per-worktree known_hosts caveat.

## 2026-11-16T21:35 [lead]

status review -> active

## 2026-11-16T21:35 [lead]

attempt 2/3 started

## 2026-11-16T21:35 [lead]

base 7cf08b5202c58593b3f6c4031bebf200a8e24f97 (attempt 2)

## 2026-11-16T21:44 [lead]

status active -> review

## 2026-11-16T21:45 [lead]

attempt 3/3 started

## 2026-11-16T21:45 [lead]

status review -> active

## 2026-11-16T21:45 [lead]

CRUFT GATE attempt 2 FAIL (dedupe only): MAJOR the AppArmor body-equivalence preflight rule is stated twice near-verbatim (§1 step 5 vs §3) -> keep once in §3, §1 points; MINOR define the suite prefix once (7-line runuser/env -i prefix pasted 3x), one "no host crun" statement, one marker statement, drop duplicated .pub-derivation and single-HOME rules from §4/§5 with pointers.
