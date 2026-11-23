---
id: M3.78
title: "Stop publishing to non-existent Homebrew/Scoop taps (the rest of 0.1.0 is live)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: [.github/workflows/packages.yml]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-23T23:51"
updated: "2026-11-23T23:52"
---

# Problem

The stable publish for `v0.1.0` (run 45281987744, commit `2bb9a1a`) uploaded `offworld-dist` and the publisher synced **all 18 artifacts** (2 deb, 2 rpm, 2 apk, 2 arch, 6 tarballs, 2 macOS, 2 Windows; `latest_stable: 0.1.0`). Its final step then failed:

    Client error '404 Not Found' for url 'https://api.github.com/repos/OffworldNexus/homebrew-tap/contents/Formula/slipfork-lsp.rb'

`OffworldNexus/homebrew-tap` and `OffworldNexus/scoop-bucket` do not exist (the API returns 404 for the repositories themselves), and neither `tpmd` nor `blastit` passes `--homebrew-json`/`--scoop-json`. The tap update is therefore premature and turns an otherwise complete release into a failed publish check.

# Change

In `.github/workflows/packages.yml` remove, from **both** the dry-run and the staging step:

- the `HOMEBREW: >- {…}` and `SCOOP: >- {…}` env entries, and
- the `--homebrew-json "$HOMEBREW" \` and `--scoop-json "$SCOOP" \` arguments.

Add one short comment where they were: the org taps (`OffworldNexus/homebrew-tap`, `OffworldNexus/scoop-bucket`) do not exist yet; re-add both flags once they do. Do not touch `packaging/stage-dist.py` (the flags stay optional) or `.offworld/project.toml` (the `homebrew`/`scoop` formats stay declared as *planned*; the site renders them as planned until published).

# Acceptance

- `uv run --no-project python -c 'import yaml;yaml.safe_load(open(".github/workflows/packages.yml"))'`
- `grep -rn "homebrew-json\|scoop-json" .github` → no hits
- `actionlint` if installed
- Commit `[M3.78] ...`; `M3.78 -> done`.
