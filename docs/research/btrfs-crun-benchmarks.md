# Btrfs + `crun` World Benchmark Report (`docs/research/btrfs-crun-benchmarks.md`)

- **Date recorded:** 2026-11-16
- **Provenance:** owner-supplied measurements, first recorded in `plan/nodes/M3/M3.11/NODE.md` and `ADR-0010` Context §2; transcribed here with the exact wording the owner record uses for each figure.
- **Substrate:** one Btrfs subvolume per world (`ADR-0010` §2) — the `Dockerfile`-derived `L0` rootfs tree **and** `/var/lib/slipfork/state/{pgdata,redis,s3}` live inside that single subvolume, so sealing or cloning a world is one metadata ioctl with no file content copied and no directory tree walked.
- **Runtime:** Linux `userns` + `netns` + `ipcns` + `pidns` + `mntns` + `pivot_root` + daemonless static `crun`, i.e. `BtrfsEngine` (`ADR-0010` §1, §6).
- **Host / versions:** not supplied with the figures — see §6. The machine holding this document's checkout (Linux `6.12.117+deb13-amd64`, `x86_64`, XFS data filesystem, no `btrfs` or `crun` binaries installed) is **not** the measurement host and ran none of the operations below.
- **Re-measurement:** `plan/nodes/M3/M3.19/NODE.md` re-measures on a Btrfs host; §6 constrains how an in-repo harness may take each figure.

---

## 1. Question

What does a world cost when the whole world is one Btrfs subvolume? Specifically: how long does a read-only snapshot of a world subvolume take, how long does a writable clone take, how long until a clone answers SQL, and how much resident memory does one running world hold?

The metric names and units below are the owner record's own words. Where the record does not define a term or a timing window, §6 says so instead of filling it in.

---

## 2. Results

| # | Metric (owner record wording) | Unit | Measured |
|---|---|---|---:|
| 1 | read-only snapshot of a world subvolume | `ms` | **`19.5 ms`** |
| 2 | writable clone | `ms` | **`11–24 ms`** |
| 3 | to a warm-SQL world | `ms` | **`39 ms`** |
| 4 | RAM per world | `MiB` | **`6.3 MiB`** |

Metrics 1–3 are wall-clock milliseconds; metric 4 is mebibytes of resident set size. Metric 2 is a range, not a point value. The record names the operation each figure measures but documents neither the timing window nor the sampling procedure — both are listed in §6, and no figure is decomposed anywhere in this report.

---

## 3. Method (the ioctl lifecycle a re-measurement reproduces)

The owner record does not include its harness, so the operations below are the `BTRFS_IOC_*` lifecycle `ADR-0010` §3 specifies for each named operation, given so `M3.19` can reproduce it. **Measurement route for metrics 1–2:** an in-repo harness times `BtrfsEngine` round-trips — `create_snapshot`/`freeze_world` for metric 1, `clone_world` for metric 2 — so every timed ioctl goes through the sole `SubvolumeDriver` seam (`ADR-0010` §5); a `poc/` harness that issued `BTRFS_IOC_*` itself would sit outside that seam, so host-level ioctl timings are external/host-only measurements, like the readiness and RSS routes in §3.4 and §3.7. Whether the supplied figures came from either route, and over exactly which windows, is unresolved (§6). Constants and struct layouts come from `<linux/btrfs.h>` (`BTRFS_SUBVOL_RDONLY = 1ULL << 1`).

1. **Subvolume creation (setup, not a measured metric):** `ioctl(fd, BTRFS_IOC_SUBVOL_CREATE_V2, &args)` with `flags = 0` and `args.vol_args_v2` naming the new subvolume, issued on an `O_RDONLY | O_DIRECTORY | O_CLOEXEC` descriptor of the parent directory.

2. **Read-only snapshot — metric 1:**
   - `freeze_world` stops `WorldProcess` services, then `OciContainer` services (`SIGTERM`, bounded grace), then seals the world subvolume with `ioctl(fd, BTRFS_IOC_SNAP_CREATE_V2, &args)` where `args.flags = BTRFS_SUBVOL_RDONLY` (`1ULL << 1`), `args.fd = source_fd` (`<state_dir>/worlds/<world_id>`), destination parent `<state_dir>/snapshots`, and `args.name = <snapshot_id>`.
   - No timing window came with the figure: the record states only "read-only snapshot of a world subvolume", so whether the figure covers the whole `freeze_world` path (service shutdown included) or only the `BTRFS_IOC_SNAP_CREATE_V2` call is unresolved (§6).

3. **Writable clone — metric 2:**
   - `ioctl(fd, BTRFS_IOC_SNAP_CREATE_V2, &args)` with `args.flags = 0` (writable), `args.fd = source_fd` (a sealed read-only snapshot under `<state_dir>/snapshots`), destination parent `<state_dir>/worlds`, `args.name = <world_id>`.
   - `clone_world` copies nothing: the `L0` rootfs tree and `state/{pgdata,redis,s3}` arrive by copy-on-write extents; the clone gets a fresh PID 1 and its own `WorldSpec.control_dir`.

4. **Warm-SQL world — metric 3:** the owner wording is "to a warm-SQL world". **Slipfork's adopted definition** (the one an in-repo `M3.19` harness must measure) is: time from `clone_world` returning to `BtrfsEngine::spawn_service` returning `Ok(ServiceHandle)` for the world's `postgres` container, which `ADR-0002` §2 only does once `ReadinessProbe::Exec` has exited `0`. A harness must take readiness from that probe, never from a bespoke `psql`/socket poll; if the figure can only be obtained outside the repository, the measurement is external/host-only and is recorded as such. What the owner's figure actually timed is not recorded (§6).

5. **World namespace entry:** `unshare --user --map-user=$UID --map-group=$GID --keep-caps --mount --net --ipc --pid --fork --mount-proc` (and `nsenter --user --preserve-credentials --mount --net --ipc --pid` on re-entry), per `ADR-0010` §6. `--ipc` appears in this set; §5 explains the SysV collision it avoids.

6. **Backing services:** each `postgres` / `redis` / `s3` container is `crun` with `"namespaces": ["user", "pid", "mount", "ipc", "uts"]` (`ADR-0009` §2), identity `uidMappings`/`gidMappings` (`$UID -> $UID`), and **no** `network` namespace, so it shares the world's `127.0.0.1:5432`.

7. **Per-world footprint — metric 4:** the owner wording is "RAM per world". Any in-repo sampler for this metric is a `poc/` benchmark adapter that reads process statistics from outside the world (a `/proc`-based sampler in `poc/`): no `/proc` or clock reads from a domain crate; timing/RSS sampling lives in the `poc/` or test harness that calls `BtrfsEngine`/`SubvolumeDriver`. The owner's sampling method is not recorded (§6).

8. **Teardown ordering (required for every measured snapshot to be collectable):**
   - `ioctl(fd, BTRFS_IOC_SUBVOL_GETFLAGS, &flags)` to read the subvolume flags.
   - `ioctl(fd, BTRFS_IOC_SUBVOL_SETFLAGS, &(flags & !BTRFS_SUBVOL_RDONLY))` to clear read-only when set.
   - `ioctl(fd, BTRFS_IOC_SNAP_DESTROY_V2, &args)` to remove the subvolume.

---

## 4. What this means for Slipfork

- **Sealing is cheap enough to do constantly.** Every STP stage — migrations applied, seed factories materialized, S3 state populated — can be frozen into a read-only snapshot as part of ordinary test setup instead of being replayed per test or per worker.
- **Forking a world is a metadata operation.** The clone shares extents and metadata rather than copying the rootfs tree, `pgdata`, and caches, which is what makes forking on `worktree.create` reasonable.
- **A SQL-ready world costs tens of milliseconds.** A rolling warm pool of worlds can hand each test slot its own PostgreSQL without seed replay and without cross-test shared state; the spread between the clone figure and the warm-SQL figure is presumably process and container start, which is an inference, not a measured decomposition (§6).
- **Keeping worlds warm is memory-cheap.** The per-world figure counts the world's init process plus its service containers, so an idle pool is bounded by those service processes; whether a large pool's aggregate cost stays additive is not measured (§6).
- **The results table above is the cost model cited by `ADR-0010` Context §2** for the single-subvolume layout (snapshot → clone → SQL → steady state). `ADR-0010` §2–§6 remain the normative home for the layout and lifecycle; this report only records the figures.

---

## 5. Traps (observed kernel behaviour)

1. **A separate `netns` does not isolate System V shared memory.** Two `postgres` instances in *separate* network namespaces still land in the same System V IPC namespace and collide on SysV shared-memory key `90000`; isolating `:5432` behind a private `netns` changes nothing for `shmget`. The binding namespace set — `--ipc` (`CLONE_NEWIPC`) for the world (`ADR-0010` §6) and an `ipc` entry in each `crun` `config.json` (`ADR-0009` §2) — is specified there.
2. **A read-only subvolume cannot be destroyed.** `BTRFS_IOC_SNAP_DESTROY_V2` rejects a subvolume that still carries `BTRFS_SUBVOL_RDONLY`; the kernel requires `BTRFS_IOC_SUBVOL_GETFLAGS`, then `BTRFS_IOC_SUBVOL_SETFLAGS` with `flags & !BTRFS_SUBVOL_RDONLY`, then destroy. Where that ordering lives (inside `SubvolumeDriver::destroy_subvolume`, with no separate mode argument) is specified in `ADR-0010` §3 and §5.
3. **Unprivileged destroy needs `user_subvol_rm_allowed`.** Deleting a subvolume as the developer's `$UID` fails with `EPERM` unless the Btrfs mount carries `-o user_subvol_rm_allowed` (alongside `noatime`). `ADR-0010` §4 specifies that `slipfork doctor` reports exactly this as `BtrfsProbeReport::user_subvol_rm_allowed` evidence (live create → RO snapshot → clear-RO → destroy round-trip recording `Ok(())` or the raw `errno`), together with the required mount options and the fail-closed probe.
4. **No nested child subvolumes inside a world.** `BTRFS_IOC_SNAP_CREATE_V2` does not recurse into nested subvolumes, so everything inside `<subvol>` must be plain directories and CoW-shared extents — one subvolume per world is what makes a single ioctl snapshot the entire world atomically (`ADR-0010` §2).

---

## 6. Unverified / Requires Host Access

The following were **not** part of the supplied record and are not asserted anywhere above:

- **Measurement host fingerprint:** distribution, kernel version, CPU count, RAM, and the date the figures were taken.
- **Tool versions:** `btrfs-progs` and `crun` versions, and the `postgres` image tag used for the warm-SQL figure.
- **Btrfs mount options of the measurement host** (the string, including `user_subvol_rm_allowed`/`noatime`, was not recorded).
- **Timing window of the read-only-snapshot figure:** whether it includes `freeze_world` service shutdown or only the `BTRFS_IOC_SNAP_CREATE_V2` seal. Related and equally unmeasured: whether `freeze_world` cost is dominated by stopping services rather than by data volume — no split measurement was supplied, so this is an open hypothesis, not a result.
- **Timing windows and sampling method of metrics 2–3:** what the writable-clone range bounds, and whether the warm-SQL figure includes the clone. It is also unrecorded whether each figure is a single run, a median, or a best-of-N.
- **Warm-SQL procedure used for the owner figure:** "to a warm-SQL world" is defined by Slipfork as in §3.4 (through `ReadinessProbe::Exec`, `ADR-0002` §2); whether the owner's figure followed that definition, or an external/host-only procedure, is not recorded.
- **RSS collection method** for the per-world RAM figure (e.g. `smaps_rollup` vs `/proc/<pid>/statm`, and how shared page cache is attributed), and the location of any sampler — an in-repo one is a `poc/` benchmark adapter, never `/proc` reads from a domain crate (§3.7).
- **Harness source:** no Btrfs run of `poc/run_benchmark.py` has been recorded yet; `M3.19` performs the re-measurement on a Btrfs host and reports deviations against this record.
