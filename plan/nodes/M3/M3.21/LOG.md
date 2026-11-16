# Log M3.21

## 2026-11-16T17:25 [strategist]

created (task) Atomic scorched-earth deletion: remove IncusEngine, ReflinkNetnsEngine, and FICLONE helpers

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.
