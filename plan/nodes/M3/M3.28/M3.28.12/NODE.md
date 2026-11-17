---
id: M3.28.12
title: "VM harness: :80 bound via daemon install, tracked support/demo template, login quickstart banner"
kind: task
status: active
owner: implementer
priority: 2
depends_on: [M3.28.6, M3.28.20]
paths: [tools/do_vm.py, tools/tests/test_do_vm.py, "support/demo/**"]
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-17T16:33"
updated: "2026-11-18T00:18"
---

# Goal

Make the Btrfs DO VM reproduce the shipped story: provisioning ends with `:80` bound through the product verb, a **tracked** demo template replaces the VM-only files, and login states the three commands instead of forcing `getent passwd` archaeology.

# Context

Fact 1: the deployed daemon ran `--listen 127.0.0.1:0`, so `http://demo.localhost` was refused. Fact 10: the owner needed `getent passwd` + `sudo -u slipfork -i`. Fact 9: the demo README's origin recipe used a mystery port. The demo lives only on the VM today (untracked), so nothing is reproducible. `M3.28.6` ships `slipfork daemon install`. Parallel with `M3.28.8`–`M3.28.11`.

# Design / Invariants

1. Provisioning installs the four binaries into `/usr/local/bin`, then `slipfork daemon install --user slipfork --bin-dir /usr/local/bin --bin-dir <remote_tree>/target/{debug,release}`, then asserts `systemctl is-active slipforkd.socket` = `active`. Re-running provisioning twice leaves both runs `active` and rewrites zero unit files.
2. Add a tracked demo template at `support/demo/**`: `snow.yml` (one build image, component on an internal port, `/` ingress, `pg_clusters[{id: psql, version: "18"}]`, `pg_databases[{id: demo, cluster: psql}]`, `modes.local`), `Dockerfile` (python app stage), `app/` + `db/*.sql`, `slipfork.toml` (`project = "demo"`), and a README with the port-less quickstart. `do_vm.py sync` copies it to `/home/slipfork/demo` (owned `slipfork:slipfork`, git identity preset, commit made once) and never clobbers owner edits after the first stamp.
3. The harness's own AppArmor renderer is deleted: the profile comes from `slipfork daemon unit --only apparmor --bin-dir …` (one renderer). `tools/tests/test_do_vm.py` enforces byte-equality against that command, not a second copy.
4. Login banner (one file written by provisioning) prints, as `slipfork`: the account, `cd ~/demo`, `slipfork`, `slipfork up`, `slipfork open`. No password/`getent` step anywhere.
5. Every new provisioning step fails closed (no `|| true`); no secrets, no client names, no VM-only scripts.

# Acceptance

```console
$ uv run --no-project pytest tools/tests -q
... passed
$ uv run --no-project ruff check tools && uvx --offline ruff format --check tools
All checks passed!
$ grep -q 'daemon install' tools/do_vm.py && echo product-install
product-install
$ ! grep -rn 'render_apparmor_body' tools/do_vm.py && echo single-renderer
single-renderer
$ test -f support/demo/snow.yml && test -f support/demo/Dockerfile && test -f support/demo/README.md && echo demo-tracked
demo-tracked
$ uv run --no-project tools/do_vm.py sync
synced N file(s) to /home/slipfork/slipfork on 165.232.86.174
$ uv run --no-project tools/do_vm.py ssh -- systemctl is-active slipforkd.socket
active
```

- `do_vm.py test` (five live suites) stays green after the harness change; record it in RESULT.
- Demo template contains no `.gitignore` that breaks `fork` (see `M3.26`).
