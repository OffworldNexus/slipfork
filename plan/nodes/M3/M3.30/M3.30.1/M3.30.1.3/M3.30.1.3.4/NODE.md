---
id: M3.30.1.3.4
title: "Docs/tooling: surviving port list + delete vacuous stub-adapters guard"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30.1.3.3]
paths: [ARCHITECTURE.md, Makefile, "tools/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-18T22:46"
updated: "2026-11-18T22:46"
---

# Goal
Slice 4 of `plan/nodes/M3/M3.30/M3.30.1/M3.30.1.3/NODE.md`: living spec tells the truth and the now-vacuous stub guard is gone.

# Work
- `ARCHITECTURE.md`: list only surviving genuine ports (`WorldEngine`, `SecretProvider`, `UnitPort`, `TunnelRegistrar`, `VcsWorkspace`; keep `SubvolumeDriver` ONLY if slice 1 found a real second backend) with concrete-wiring notes naming the composition roots. Remove every deleted double/trait row.
- Delete `tools/check_stub_adapters.py`, its `tools/tests` cases, and the Makefile `stub-check` target + `check` wiring (the guard only policed stubs that no longer exist). Keep `make check` valid.

# Constraints
No doc vaporware, no aliases, no RESULT.md.

# Acceptance
- `! grep -rEn "TestTreeDriver|RecordingInstaller|NoOciStore|StubRootfs|FakeDaemon|check_stub_adapters|stub-check" ARCHITECTURE.md Makefile tools/`
- `uv run --no-project --with pytest pytest -q tools/tests`
- `make -n check` exits 0
- `uv run --no-project tools/guard.py`
