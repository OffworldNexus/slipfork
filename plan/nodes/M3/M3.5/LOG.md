# Log M3.5

## 2026-11-16T12:31 [conductor]

created (task) DigitalOcean dev-VM harness: droplet + ZFS volume + Incus pool, remote live IncusEngine tests (tools/do_vm.py)

## 2026-11-16T12:31 [lead]

status ready -> active

## 2026-11-16T12:31 [lead]

attempt 1/3 started

## 2026-11-16T12:31 [lead]

base dcc270e8a938ea26870549724061815b0dba6751

## 2026-11-16T12:49 [implementer]

Implemented tools/do_vm.py (up/status/sync/test/ssh/down), idempotent by tag with droplet-only/volume-only reconciliation, ams3+s-4vcpu-8gb defaults, all-visible SSH keys + --authorized-key install/fingerprint evidence, internal docs, 19 fake-doctl/ssh tests. Acceptance green; no real cloud resources created.

## 2026-11-16T12:49 [lead]

status active -> review

## 2026-11-16T12:54 [lead]

Quad Gate round 1: reviewer PASS, legality PASS, ioc FAIL (mutation-adapter/git-seam/state-machine smells + rust-toolchain blind spot), cruft FAIL (blocker claiming default-all keys/warn contradicts the owner SSH addendum). Fix round 1 dispatched: fail-fast on publickey denial, summary/down before fingerprint probe, secret scan limited to the tarred list, batch=False for interactive ssh, null-safety, bash -n + apt-cache fail-closed tests, reuse-mode size note, assert->branches, _ready_probe, tag aliases, docs typo/provenance, rust prereq line. Rejected as owner overrides: ams3 default and default-all keys with warn-only --authorized-key.

## 2026-11-16T13:00 [implementer]

Round 2 gate fixes: SSH auth fail-fast + --ssh-key/--authorized-key guidance, summary-before-fingerprint billing safety, exact-tarred-list .env scan, --ssh-key scope line, batch=False for interactive ssh, null-safe payloads, bash -n/incus-none tests, tagged-resource note, assert/alias cleanup, docs and Rust >=1.85 prereq. 67 tests; make check green; no cloud resources.

## 2026-11-16T13:26 [implementer]

Final polish: single _poll retry ladder (only auth/host-key abort; last detail on timeout, SSH_TRANSIENT deleted), regions() payload validation, docs bounded SSH-reachability/auth sentence. pytest 67 passed, ruff clean, make check green, no cloud resources.

## 2026-11-16T13:31 [lead]

status review -> done
