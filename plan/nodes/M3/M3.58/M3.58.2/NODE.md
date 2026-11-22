---
id: M3.58.2
title: slirp4netns packaging/GHA deps + install docs
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["packaging/**", "scripts/**", ".github/workflows/**", "docs/**", README.md]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T22:54"
updated: "2026-11-22T23:22"
---

# Goal
Ship slirp4netns as a runtime dependency (deb/rpm/arch/apk) beside crun; install it in CI live + package-verify containers; document it in install/getting-started.

# Acceptance
- `make packages` + `make packages-test` exit 0; deps carry slirp4netns.
- Workflows install it where live worlds boot; docs honest.
