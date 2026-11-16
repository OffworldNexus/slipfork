# Spike B — KVM microVM snapshot/restore vs. XFS reflink CoW (measured)

- **Date:** 2026-11-16
- **Host:** Ubuntu 24.04.5, kernel `6.8.0-147-generic`, 4 vCPU (`DO-Regular`, VT-x, **nested KVM enabled**), 7.9 GiB RAM
- **Storage:** 60 GiB XFS loopback (`/dev/loop1`) with `reflink=1,rmapbt=1,crc=1,bigtime=1,bsize=4096,noquota`
- **Versions:** Cloud Hypervisor `v54.1`, Firecracker `v1.18.0`, virtiofsd `1.10.0`
- **Guest:** Alpine 3.22 minirootfs + PostgreSQL `17.12`, 2 vCPU / 1 GiB, 768 MiB ext4 rootfs seeded with `items` = **10,000 rows**, booted with `init=/opt/slipinit.sh` (starts PG, `socat VSOCK-LISTEN:5432` → `127.0.0.1:5432`, `VSOCK-LISTEN:9999` → shell, `mount -t virtiofs hostshare /shared`)
- **Status:** every number below was measured on the host above. Numbers marked *pass 1* were taken earlier in the same spike on the same host with the same guest image; numbers without that mark were re-measured or newly measured in this pass.

---

## 1. Question

Slipfork's fork primitive needs one of:

1. **Disk-only CoW** (XFS `FICLONE`) + cold process start, or
2. **RAM snapshot** of a live world (warm PostgreSQL, warm app) restored per fork.

Option B claims the second. The make-or-break question is **not** RAM: it is whether the *shared host workspace* (`$UID:$GID` files the agent edits) survives a snapshot/restore, because a restored world with a dead or stale workspace mount is worse than a cold boot.

---

## 2. Results summary

| ID | Measurement | Result |
|---|---|---:|
| M1 | XFS `cp --reflink=always` of the 768 MiB rootfs (*pass 1*) | **`5.1 ms`** |
| M1 | `pause` (*pass 1* / this pass) | **`22.03 ms`** / **`6.71–7.68 ms`** |
| M1 | `snapshot` of 1 GiB RAM, non-shared memory, dense `memory-ranges` (*pass 1*) | **`969.48 ms`** |
| M1 | `snapshot` of 1 GiB RAM, `shared=on`, sparse `memory-ranges` (this pass) | **`108.07–115.42 ms`** |
| M2 | vsock bind (*pass 1*) | **`33.9 ms`** |
| M2 | restore → PostgreSQL accepts a client, `memory_restore_mode=copyonwrite` median of 10 (*pass 1*) | **`290.87 ms`** |
| M2 | `SELECT count(*)` after restore (*pass 1*) | **`351.0 ms`** |
| M2 | committed `INSERT … RETURNING` after restore (*pass 1*) | **`440.68 ms`** |
| M2 | steady-state `SELECT 1` (*pass 1*) | **`1.3 ms`** |
| M2 | cold VM boot → PG accept (*pass 1*) | **`3,297.72 ms`** |
| M2 | eager restore vs CoW restore RSS/Pss (*pass 1*) | **`911.76 ms` / `1,058 MiB` RSS** vs **`71.1 MiB` Pss / `17.0 MiB` anon** |
| M3 | 8 simultaneous clones from one snapshot, all SQL-correct (*pass 1*) | **`2,356.84 ms` total**, prep `49.2 ms` |
| M3 | host `MemAvailable` delta for those 8 (`~21 MiB`/VM) (*pass 1*) | **`169.5 MiB`** |
| M3 | ΣPss of 8 clones vs 8 GiB eager (*pass 1*) | **`226.1 MiB`** vs `8 GiB` |
| M3 | 8-clone restore re-run (this pass, teardown harness) | **`1,997–2,262 ms`**, prep `32–43 ms` |
| **M4a** | snapshot **with live virtiofs mount** | **accepted** (`pause 7.68 ms`, `snapshot 108.07 ms`) |
| **M4a** | 2 clones restored, both PG-ready | **`293.33 ms`** (prep `13.18 ms`) |
| **M4a** | workspace after restore | **BROKEN — every FUSE op `EBADF`**, host writes `ABSENT` |
| **M4b** | `umount /shared` → snapshot → restore 2 clones | **`309.28 ms`** both PG-ready (prep `16.48 ms`) |
| **M4b** | guest `mount -t virtiofs hostshare /shared` on wake | **`21.09 ms` / `49.80 ms`**, `rc=0` |
| **M4b** | first `ls`+`cat` after remount | **`69.05 ms` / `43.77 ms`** |
| **M4b** | post-snapshot host edits visible in both clones | **yes** |
| **M4b** | guest writes as uid 1000 land on host as | **`1000:1000`** |
| FC | Firecracker cold boot → PG accept | **`3,161.51 ms`** |
| FC | Firecracker `PATCH /vm {"state":"Paused"}` | **`0.85 ms`** |
| FC | Firecracker **Full** snapshot (1 GiB dense `mem.bin`) | **`3,399.56 ms`** (`snap.bin` 23,390 B) |
| FC | `PUT /snapshot/load` (returns `204`) | **`7.56 ms`** |
| FC | load → PG accept | **`193.39 ms`** |
| FC | committed `INSERT` after restore | **`234.06 ms`** |
| FC | restored VMM memory (`MAP_PRIVATE` file backend) | **`67.4 MiB` Pss / `6.5 MiB` anon** |
| M5 | Cloud Hypervisor `vmgenid` on fork (*pass 1*) | kernel logs **`random: crng reseeded due to virtual machine fork`**; `/dev/urandom`, `boot_id`, `gen_random_uuid()` diverge; `/proc/uptime` identical |
| M7 | `SIGTERM` 8 running VMMs → all reaped, exit `0` | **`75.03 ms`** |
| M7 | `SIGKILL` 8 running VMMs → all reaped, `-9` | **`130.46 ms`** |
| M7 | `unlink` 8 reflinked 768 MiB rootfs files | **`3.20 ms`** (`0.40 ms` each) |
| M7 | `rm -rf` of the 8 clone directories | **`2.91 ms`** |
| M7 | `shutdown-vmm` API request / VMM exit after it | **`32.04 ms`** / **`1.21 ms`** |

**Headline:** M4b is the only viable shape. Cloud Hypervisor accepts snapshots with a `virtiofs` device attached, restores in ~300 ms, and the shared workspace works **provided the mount is torn down before the snapshot and re-mounted after restore** (`+21–50 ms`). Keeping the mount live across snapshot/restore yields a mount that still *lists* but returns `EBADF` for every operation.

> The brief lists M1–M5, M7 and an optional Firecracker fallback; there is no M6 task defined in it, so the Firecracker comparison is reported as its own section (§4.6) rather than mislabelled.

---

## 3. Method

### 3.1 Guest image

Alpine 3.22 minirootfs unpacked into a 768 MiB ext4 image (`/mnt/spike-b/snap/rootfs.ext4`), PostgreSQL 17.12 initialised under `/var/lib/postgresql/data` owned by `app` (uid/gid 1000), `items` table seeded with 10,000 rows, plus this `/opt/slipinit.sh`:

```sh
#!/bin/sh
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
mount -t proc proc /proc; mount -t sysfs sysfs /sys; mount -t devtmpfs devtmpfs /dev
mkdir -p /dev/pts /dev/shm; mount -t devpts devpts /dev/pts
mount -t tmpfs tmpfs /dev/shm; chmod 1777 /dev/shm
hostname microvm
ip link set lo up
mkdir -p /shared /run/postgresql; chown app:app /run/postgresql
if mount -t virtiofs hostshare /shared 2>/dev/null; then echo "GUEST: virtiofs mounted"; else echo "GUEST: virtiofs absent"; fi
su app -s /bin/sh -c "/usr/libexec/postgresql17/pg_ctl -D /var/lib/postgresql/data -l /tmp/pg.log -w start" && echo "GUEST: postgres started"
socat VSOCK-LISTEN:5432,fork,reuseaddr TCP:127.0.0.1:5432 &
socat VSOCK-LISTEN:9999,fork,reuseaddr EXEC:/bin/sh,pty,stderr,setsid,sigint,sane &
echo "GUEST: MICROVM-READY vsock 5432/9999"
while true; do sleep 3600; done
```

PostgreSQL is reached over **hybrid vsock** (CH) / **vsock UDS** (FC) with the identical handshake (`CONNECT <port>\n` → `OK\n`), so the same Python client measures both hypervisors. Latency is measured to the first successful `PQconnectdb` (PG accept) and to a committed `INSERT … RETURNING`; SQL is a hand-rolled frontend protocol client (`pgraw.py`) plus `libpq` for convenience — no TCP forwarding is involved at steady state.

### 3.2 Cloud Hypervisor boot / snapshot / clone-restore

```sh
# 1 GiB guest RAM, 768 MiB reflinked rootfs, hybrid vsock, vmgenid+rng, optional virtiofs
ch-static --api-socket $D/clone/ch.sock \
  --kernel /root/spike-b/dl/vmlinux-x86_64 \
  --cpus boot=2 --memory size=1024M[,shared=on] \
  --disk path=$D/clone/rootfs.ext4,image_type=raw \
  --serial file=$D/clone/serial.log \
  --cmdline "root=/dev/vda rw console=ttyS0 init=/opt/slipinit.sh" \
  --vsock cid=3,socket=$D/clone/vsock.sock \
  --rng src=/dev/urandom \
  [--fs tag=hostshare,socket=$D/clone/vfsd.sock]

# snapshot (VM must be paused first)
ch-remote --api-socket $D/ch.sock pause
ch-remote --api-socket $D/ch.sock snapshot file:///mnt/spike-b/snap/base

# restore with CoW guest RAM (no 1 GiB private copy per clone)
ch-static --api-socket $D/clone/ch.sock \
  --restore source_url=file://$D/clone,memory_restore_mode=copyonwrite,resume=true
```

Per-clone preparation rewrites only the snapshot's `config.json` (the RAM file is shared read-only and symlinked):

```python
cfg["disks"][0]["path"]   = f"{d}/rootfs.ext4"     # reflink of the snapshot rootfs
cfg["serial"]             = {"file": f"{d}/serial.log", "mode": "File"}
cfg["vsock"]["socket"]    = f"{d}/vsock.sock"      # per-clone vsock UDS (cid stays 3)
for f in cfg.get("fs", []):
    f["socket"] = f"{d}/vfsd.sock"                 # per-clone virtiofsd
```

`cp --reflink=always` **must** run on the same filesystem as the source: doing it from `/root` (overlay) failed with `Invalid cross-device link`.

### 3.3 virtiofsd

```sh
virtiofsd --socket-path=$D/clone/vfsd.sock \
  --shared-dir=/mnt/spike-b/hostshare \
  --cache=never --sandbox=none --xattr --log-level=info
```

`--sandbox=none` is required because the daemon runs as root here. `--memory shared=on` is required for CH's vhost-user-fs device. The host directory is `chown 1000:1000`; no uid map is configured, so guest uid 1000 maps to host 1000 (verified).

### 3.4 Firecracker

```sh
firecracker --api-sock $D/fc.sock      # v1.18 renamed --api-socket -> --api-sock
# PUT /boot-source   {"kernel_image_path": fc-vmlinux-6.1.155,
#                      "boot_args": "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/opt/slipinit.sh"}
# PUT /drives/rootfs {"drive_id":"rootfs","path_on_host":$D/rootfs.ext4,"is_root_device":true}
# PUT /machine-config {"vcpu_count":2,"mem_size_mib":1024}
# PUT /vsock         {"guest_cid":3,"uds_path":$D/v.sock}
# PUT /actions       {"action_type":"InstanceStart"}
# PATCH /vm          {"state":"Paused"}
# PUT /snapshot/create {"snapshot_type":"Full","snapshot_path":$D/snap.bin,"mem_file_path":$D/mem.bin}
# PUT /snapshot/load   {"snapshot_path":..., "mem_backend":{"backend_type":"File","backend_path":...},
#                       "enable_diff_snapshots":false,"resume_vm":true}
```

---

## 4. Results

### 4.1 M1 — artifact CoW is free; RAM snapshot is paid per touched byte

- Reflink of the 768 MiB rootfs: `5.1 ms` (*pass 1*) — metadata-only, no data copy.
- `pause`: `22.03 ms` (*pass 1*) and `6.71 / 7.68 ms` (this pass, 2 vCPU).
- `snapshot` of a 1 GiB guest, **non-shared** memory (*pass 1*): `969.48 ms`, `memory-ranges` = 1,073,741,824 B and `du` reports **1.0 G allocated** (dense).
- `snapshot` of a 1 GiB guest, `--memory shared=on` (this pass): `108.07 / 115.42 ms`, `memory-ranges` apparent size 1,073,741,824 B but `du` reports **121–122 MiB allocated** (sparse).

Raw:

```
1.0G	/mnt/spike-b/snap/fsb/memory-ranges     # apparent
122M	/mnt/spike-b/snap/fsb/memory-ranges     # allocated
```

Both runs move roughly **1.05 GB/s of *allocated* guest RAM** (969 ms/1 GiB vs 110 ms/122 MiB). So the 8–9× difference is *allocated bytes*, not a different snapshot algorithm: a world that has touched 122 MiB costs ~110 ms to snapshot, one that has touched 1 GiB costs ~1 s. Sparse-vs-dense behaviour for shared memory specifically was not isolated (§7).

`state.json` = `43.8 KB` (*pass 1*) / `46.5–46.6 KB` with an `fs` device; `config.json` = `1.0–1.2 KB`.

### 4.2 M2 — one warm world in ~0.3 s

Restore→PG-accept median of 10 with `memory_restore_mode=copyonwrite` was `290.87 ms` (*pass 1*), `SELECT count(*)` at `351.0 ms`, committed `INSERT … RETURNING` at `440.68 ms`, steady-state `SELECT 1` at `1.3 ms`, versus `3,297.72 ms` for a cold boot. Re-measured this pass: two concurrent clones from the same snapshot were PG-ready in `291.31` and `291.35 ms` (M4a) and `303.81`/`308.68 ms` (M4b).

Memory: eager restore `911.76 ms` and `1,058 MiB` RSS; CoW restore `71.1 MiB` Pss / `17.0 MiB` anonymous (*pass 1*). The guest's 1 GiB of RAM is not copied per world.

### 4.3 M3 — 8 clones from one snapshot

*Pass 1*, 8 simultaneous restores on the 4-vCPU droplet, every clone returning `count(*) = 10000` and committing a distinct `INSERT`:

| Metric | Value |
|---|---:|
| Total (prep + launch + all SQL) | `2,356.84 ms` |
| Clone prep (disk reflink + config) | `49.2 ms` |
| Host `MemAvailable` delta, all 8 | `169.5 MiB` (~`21 MiB`/VM) |
| ΣPss (8 VMMs) | `226.1 MiB` |
| Eager-equivalent | `8 GiB` |

Re-run in this pass (same harness, new dirs): 8-clone `restore_all` `1,997–2,262 ms`, prep `32–43 ms`.

### 4.4 M4 — shared `virtiofs` workspace across snapshot/restore (**decisive**)

#### 4a. Snapshot with the mount *live* → snapshot accepted, workspace dead

Boot: PG ready in `3,258.41 ms` with `--fs` + `--memory shared=on` (vs `3,297.72 ms` without an `fs` device, *pass 1*), so virtiofs costs no measurable boot time. Pre-snapshot the mount worked and guest writes as `app` (1000) landed on the host as `1000:1000` (`tester tester`).

```
pause    rc=0   7.68 ms
snapshot rc=0 108.07 ms   # fsa/: config.json 1,185 B, state.json 46,568 B, memory-ranges 1 GiB
restore 2 clones: prep 13.18 ms, both PG-ready 293.33 ms (291.35 / 291.31 ms)
virtiofsd (each clone):  "Client connected, servicing requests"
```

Guest state after restore: the mount is **still listed**, and every filesystem operation fails with `EBADF`:

```
hostshare on /shared type virtiofs (rw,relatime)
--- ls ---
ls: /shared: Bad file descriptor
--- tail ---
pre-snapshot-host-line
tail: read error: Bad file descriptor
sh: can't create /shared/w0.txt: Bad file descriptor
cat: can't open '/shared/host-post.txt': Bad file descriptor
--- dmesg ---
fuse: init (API version 7.45)
virtiofs virtio3: discovered new tag: hostshare
```

Both clones behaved identically; no host file was created (`host_w: ABSENT`), and host edits made after the snapshot were invisible. `dmesg` showed no `ESTALE`/`EIO`.

**Mechanism (measured symptom, not speculation):** a vhost-user-fs device is 1:1 with its backend. A restored VM must connect to a *fresh* virtiofsd process, which has no `FUSE_INIT`/session or inode table from the pre-snapshot session; the guest kernel restores its mount from RAM and never re-sends `FUSE_INIT`, so the backend rejects every request with `EBADF`. The mount's *presence* is therefore not evidence that the workspace works.

Related observation from the same run: after snapshot→`resume`, the original VM's `umount /shared` did not return within 25 s and the `:9999` control channel stopped answering — at snapshot time a `tail -f` held an open file on the mount. In the clean run below (idle mount, no open fds) `resume` + `ls /shared` was healthy in `13.22 ms`, so the hang involves live open fds across the snapshot, not `resume` itself.

#### 4b. `umount` before snapshot → remount on wake (**works**)

```
umount /shared        rc=0
pause    rc=0    6.71 ms
snapshot rc=0  115.42 ms   # fsb/: config.json 1,205 B, state.json 46,493 B
restore 2 clones: prep 16.48 ms, both PG-ready 309.28 ms (308.68 / 303.81 ms)
guest mount -t virtiofs hostshare /shared   rc=0   21.09 ms / 49.80 ms
first ls + cat after mount                        69.05 ms / 43.77 ms
```

The remount timings are host-measured wall time around a `:9999` shell command and therefore include vsock round-trip and shell spawn (≈15–40 ms on this host); the mount itself is the remainder. Both clones then:

- saw `host-post.txt` (created on the host **after** the snapshot) with the correct contents `post-snapshot-host-line`;
- wrote as `app` (uid 1000) and the host saw `w0.txt`/`w1.txt` as `1000:1000`;
- showed no `virtiofs`/`fuse`/`error` lines in `dmesg`; both virtiofsd logs stayed at `Client connected, servicing requests`.

So the fork recipe for a shared workspace is **restore → `mount -t virtiofs` → run**, costing `+21–50 ms` on top of a ~`300 ms` restore.

### 4.5 M5 — entropy/vmgenid on fork

*Pass 1.* Cloud Hypervisor's built-in `vmgenid` made the guest kernel log `random: crng reseeded due to virtual machine fork`; `/dev/urandom`, `random/boot_id`, and `gen_random_uuid()` all diverged across clones while `/proc/uptime` stayed identical. Two consequences: (a) never use uptime as a clone identity, (b) the reseed is automatic with CH's default device — no guest agent required.

### 4.6 Firecracker 1.18 control comparison

Firecracker has **no virtiofs**, so it cannot satisfy the shared-workspace requirement without extra machinery; it was measured to isolate the RAM-restore cost.

| Step | Firecracker |
|---|---:|
| Cold boot → PG accept | `3,161.51 ms` (`count(*) = 10000`) |
| Pause | `0.85 ms` |
| Full snapshot (`mem.bin` = 1,073,741,824 B dense) | `3,399.56 ms` |
| `PUT /snapshot/load` → `204` | `7.56 ms` |
| Load → PG accept | `193.39 ms` |
| Restored `SELECT count(*)` / committed `INSERT` | `10001` / `10002` at `234.06 ms` |
| Restored VMM | `67.4 MiB` Pss, `6.5 MiB` anon |

Restore is ~100 ms faster than CH (`193` vs `291 ms`) and RSS is slightly lower, but snapshots are ~3.4 s because Firecracker writes the **whole** 1 GiB memory file (278 MB/s here). Two operational traps make the multi-clone story harder than CH's:

1. **Snapshots pin absolute paths** (root block device *and* vsock UDS). Loading with a new directory's `fc.sock` failed with `VsockUnixBackend: Error binding to the host-side Unix socket: Address in use` until the stale `v.sock` from the previous VMM was unlinked. Per-clone disks at distinct paths therefore need a private mount namespace bind-mounting the clone's reflink at the snapshot's pinned path — CH instead stores paths in `config.json`, which the clone prep rewrites directly.
2. The CLI flag is `--api-sock` in 1.18 (not `--api-socket`), and the HTTP API keeps connections alive, so a naive "read until EOF" client stalls until its timeout and makes a healthy VMM look dead.

### 4.7 M7 — teardown of 8 worlds

Measured with `Popen.wait()` on the actual children (see trap 6):

```
SIGTERM × 8  -> all reaped, exit code 0      75.03 ms
SIGKILL × 8  -> all reaped, signal -9       130.46 ms
unlink × 8 reflinked 768 MiB rootfs           3.20 ms   (0.40 ms each)
rm -rf × 8 clone dirs                         2.91 ms
shutdown-vmm API request                     32.04 ms, VMM exit 1.21 ms later
```

Teardown of 8 clones is ~`81 ms` (kill + unlink + rmdir) versus ~`2.0–2.4 s` to create and query them. `SIGKILL` is *slower* than `SIGTERM` because the kernel reclaims 8 × 1 GiB of mappings at once; graceful termination exits cleanly with status 0.

---

## 5. What this means for Slipfork

1. **Option B's fork primitive is `restore + remount` (≈`320 ms` to a PG-ready, workspace-attached world), not `restore` alone.** The `mount -t virtiofs` step is guest-side, so it belongs inside the guest init/boot hook, not in host orchestration: the host must not report a world "ready" until the mount has been re-established and probed.
2. **A restored virtiofs mount must be treated as *hostile state*.** `mount` output still names it while every operation returns `EBADF`. Any `WorldEngine` capability that exposes a workspace must probe it (create/unlink a sentinel) rather than inspect the mount table; otherwise a fork silently produces a world whose edits vanish.
3. **The `umount`-before-snapshot rule must be enforced by the engine, not by convention.** Because the guest cannot know a snapshot is imminent, the host must issue an explicit "quiesce workspace" step (guest agent: `sync; umount /shared`), then pause, then snapshot. The reverse order is measurable and silently destructive.
4. **`shared=on` is mandatory for vhost-user-fs anyway, and it keeps snapshot cost proportional to *touched* RAM** (`108–115 ms` for this 122 MiB-touched guest vs `969 ms` for a dense 1 GiB one). Warm-world pools therefore get cheaper the idler the world is, which rewards pre-warming a pool over snapshotting busy worlds.
5. **Per-clone backend processes are unavoidable**: one `virtiofsd` per restored VM (1:1 vhost-user), one reflinked rootfs, plus the per-clone `config.json` rewrite. This is a per-world process and file cost of ~`21 MiB` Pss (`169.5 MiB` for 8) that the engine's bookkeeping must own and reap.
6. **Firecracker is not a drop-in alternative** while the workspace is a host directory: no virtiofs, and path-pinned snapshots need mount-namespace gymnastics per clone. Its `193 ms` restore is attractive only for worlds that need no shared filesystem.
7. **Teardown is not the bottleneck** (`~81 ms` for 8 worlds). Engineering effort belongs in the restore/remount path and in workspace liveness probing, not in faster cleanup.

---

## 6. Traps discovered (all verified on this host)

1. **virtiofs across snapshot/restore is silently fatal.** Restore the VM with a fresh virtiofsd and the mount still *lists*, but every `open`/`ls`/`create` returns `EBADF`; the guest kernel never re-sends `FUSE_INIT` because the mount came back from RAM. `umount` before snapshot and `mount -t virtiofs` after resume.
2. **Do not `umount` a virtiofs mount that had live open fds at snapshot time.** After snapshot→resume the original VM's `umount` (a `tail -f` held the file) never returned and the `:9999` shell stopped answering for >25 s; an idle mount resumes and `ls`s in `13.22 ms`.
3. **CH snapshot cost ∝ allocated guest RAM** (~1.05 GB/s): `969 ms` for a dense 1 GiB `memory-ranges`, `108–115 ms` for a 121–122 MiB-allocated one (`du`, not apparent size, tells the truth).
4. **CH's vhost-user-fs needs `--memory size=…,shared=on`** and one `virtiofsd` **per VM**; a single backend socket cannot serve two restored VMs.
5. **virtiofsd as root needs `--sandbox=none`** (`--cache=never --xattr` used here); guest uid 1000 passes through to host `1000:1000` with no uid map.
6. **`pgrep -x` counts zombies.** After `pkill`, a parent that never `wait()`s leaves `ch-static` visible in `/proc`, so polling `pgrep` reported "8 still alive after 45 s". Measure teardown with `Popen.wait()`; the real number is `75.03 ms` for 8 VMs.
7. **`pkill -f "<pattern>"` over SSH can match your own remote shell** (its command line contains the pattern) and kills the session with exit 255. Use `pkill -x <comm>`.
8. **`cp --reflink=always` requires the same filesystem.** Cloning into `/root` (overlay) from `/mnt/…` (XFS) fails with `Invalid cross-device link`; keep clone dirs on the XFS volume.
9. **Firecracker 1.18 renamed `--api-socket` to `--api-sock`**, and its HTTP API uses keep-alive: a client that reads until EOF hangs until timeout and looks like "API not up". Parse `Content-Length`.
10. **Firecracker snapshots pin absolute paths** (root disk and vsock UDS); a stale `v.sock` makes `snapshot/load` fail with `Address in use`, and per-clone disks at different paths require a bind mount at the pinned path.
11. **`state.json` grows with devices** (`43.8 KB` bare → `46.5 KB` with an `fs` device) but stays cheap; the memory file dominates, and it is shared read-only across clones (one 1 GiB file for *N* worlds).

---

## 7. Unverified / Requires Host Access

These were **not** measured and must not be assumed from the numbers above:

1. **PostgreSQL crash-consistency under snapshot.** Snapshots were taken with PG running but with no in-flight client transaction. Behaviour with open client connections, uncommitted transactions, or an fsync in progress at the pause instant is untested; WAL recovery on restore was not exercised.
2. **Durability of a virtiofs write racing the pause.** The VM is paused before snapshotting (so no in-flight FUSE request should exist), but host page cache / `fsync` ordering at that boundary was not verified.
3. **Why shared memory snapshots are sparse.** The correlation (allocated bytes ≈ 122 MiB, elapsed ≈ 110 ms) is measured; whether CH skips untouched/zero pages only for memfd-backed (`shared=on`) memory is not established, and no dense-vs-sparse test was run at equal touched-byte counts.
4. **Scale beyond 8 clones** (e.g. 64–256 worlds), host RAM/`MemAvailable` exhaustion behaviour, and `memory_restore_mode=eager` combined with an `fs` device.
5. **virtiofsd variants**: `--sandbox=namespace`, `--uid-map`, `cache=auto|always`, DAX, and non-root daemons were not exercised (only `--sandbox=none --cache=never --xattr` as root).
6. **Firecracker with `--enable-pci`**, diff snapshots (`enable_diff_snapshots: true`), a shared-dir alternative, and whether an FC equivalent of CH's `vmgenid` reseed exists. FC RNG divergence was **not** tested; M5 is a Cloud Hypervisor result only.
7. **Bare-metal / AMD / non-nested KVM.** All numbers come from a 4-vCPU droplet with nested VT-x; latencies and `vmgenid`/MSR warnings (`WARN:hypervisor/src/kvm/mod.rs: Detected faulty MSR …`) may differ.
8. **Live migration semantics** (`send-migration`/`receive-migration`) and snapshot portability between hosts were not tested — only local file snapshot/restore.
9. **Host-side `virtiofsd` crash** while a world is running (FUSE reconnect behaviour) was not tested; only the snapshot-induced restart path.

---

## 8. Reproduce

All harness code lived on the spike host (`/root/spike-b/bin/{harness.py,m4.py,m4b.py,m4c.py,m7c.py,fc.py,pgraw.py,chvsock.py}`) and the host was fully torn down after the run (§9). The guest image was built from `alpine-minirootfs-3.22` + PostgreSQL 17.12 packages with `/opt/slipinit.sh` above; the kernel was a locally built `vmlinux` for CH and Firecracker's `fc-vmlinux-6.1.155` for FC. The Cloud Hypervisor and Firecracker binaries were the upstream static releases (`v54.1`, `v1.18.0`), `virtiofsd 1.10.0` from the Ubuntu 24.04 package.

Snapshot artifacts: `config.json` (~1.2 KB), `state.json` (~46 KB), `memory-ranges` (1 GiB apparent, sparse) for CH; `snap.bin` (~23 KB) + `mem.bin` (1 GiB dense) for FC.

## 9. Host cleanup performed

`pkill firecracker/virtiofsd/ch-static` → `umount /mnt/spike-b` → loop device released (`losetup -a` empty) → `rm -rf /mnt/spike-b /root/spike-b`. Verified afterwards: no `cloud-hypervisor`/`ch-static`/`virtiofsd`/`firecracker` processes, no `spike` mounts, no loop devices, `/bin/busybox` absent on the host, `/root/spike-a` and `/mnt/spike-a` untouched (absent). The 60 GiB image was deleted, returning the host to `2.4 G` used of `154 G`.
