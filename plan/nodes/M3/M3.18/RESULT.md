# Result M3.18 — Published docs match the shipped `0.1.0` Btrfs-only surface

## Summary

The six published pages now describe the shipped Btrfs-only `0.1.0` surface with zero vaporware and zero pre-`1.0` cruft. Attempts 1–2 rewrote the pages from the Incus/ZFS/reflink era; this round closed the **M3.18 cruft-gate findings** (seven items, all inside the node's six paths) and then re-ran the `dx`, `ai-ergonomics`, and `cruft` critics, which found further real defects (a dead-ending README mount one-liner, a `dag plan` line that fails on a fresh repo, a `tee`-into-a-directory command, `--crun` attributed to `slipfork up`, and an overstated `--world` cwd-independence). Every BLOCKER and MAJOR is applied; three MINORs are rejected with code-verified reasons. No page exceeds 160 lines; no Incus/ZFS/`FICLONE`/reflink, `RunnerProtocol`, `CommandTestAdapter`, `CoverageDriver`, `pytest-slipfork`, NDJSON, or "Option A/B" text remains.

## Cruft-gate findings (this round)

| # | Finding | Fix applied |
|---|---|---|
| 1 | `AGENTS.md:3` "`0.1.0` ships the planner only" contradicted the shipped worlds/daemon/OSPP/plugin | `0.1.0` ships Btrfs worlds + the `snow.yml` resolver + the `dag plan` planner; the parallel test orchestrator is `0.2.0` (split into two sentences so the boundary is skimmable) |
| 2 | `docs/index.md:28` "the single mount fix" | "one `fix:` line per failed check, in `btrfs -> unshare_userns -> netns` order" (matches `doctor::run`, which pushes up to three) |
| 3 | README/index "zero `sudo` after the one-time Btrfs mount" false on Ubuntu 24.04 | "no `sudo` for day-to-day worlds. Root is needed only for the one-time Btrfs mount and the Ubuntu 24.04 AppArmor `userns` profile"; getting-started §6 is now "Ubuntu 24.04 AppArmor (**required on stock kernels**), and the optional systemd daemon" |
| 4 | `cli.md:88` documented `--repo` as required-to-be `<vcs_root>` | Option deleted from `cli.md` and `testing-dag.md`; the binary rejects every other value, so it is a user no-op |
| 5 | `testing-dag.md:74` called the digest cache "planned 0.2.0" | Digest lookup ships in `0.1.0` (`FsDigestCache` checks `<state_dir>/{artifacts,fixtures}/<digest>`); only materialization is `0.2.0` |
| 6 | Duplicated, already-diverging Btrfs recipes (README vs getting-started) | getting-started keeps the canonical four-line block (root, `tmpfs` warning, `--state-dir`); README carries one self-sufficient `mkdir`/`mkfs.btrfs`/`mount`/`chown` line plus a link to it |
| 7 | `cli.md` `--world` rows described a bare fork only | Documented the printed round-trip grammar from `resolve_world_selector`: bare `<fork>`, the `<project>:<fork>` id `slipfork list` prints, canonical `<project>`; a foreign project prefix fails closed |

Item 7 coordination: `crates/slipfork/src/cli.rs` (M3.15, in `review`) still carries the old `--world` clap help ("World fork to boot; omit for the canonical world of `cwd`'s repository"), so the grammar was taken from the shipped `resolve_world_selector` in `crates/slipfork/src/daemon.rs:287-319` and `world_id` in `crates/slipfork-edge/src/registry.rs:614-620`. The help-text drift is logged below, outside this node's paths.

## Critic dispositions

Three critics ran in parallel on the rewritten pages. They were instructed **not** to write `docs/design/*.review-*.md`, because `docs/design/` is outside M3.18's path locks while M3.15/M3.24 are active; findings were returned inline.

| Finding | Lens / severity | Disposition |
|---|---|---|
| README mount one-liner had no `mkdir`/`chown`, so it failed on a fresh host and could not fix ownership | dx BLOCKER, ai-ergonomics BLOCKER | **Applied** — one line now runs `mkdir -p` … `mkfs.btrfs` … `mount` … `chown`; still one line, per the dedup directive, and the link owns `tmpfs`/`--state-dir` |
| `slipfork dag plan` sat in the copy-paste quickstart but fails closed on a fresh repo | dx MAJOR, cruft MAJOR | **Applied** — removed from both quickstart blocks; a prose line after each block points at `dag plan` once `slipfork.toml` exists |
| §6 heading said AppArmor "(required)" unqualified | dx MAJOR | **Applied** — "(required on stock kernels)"; forward link and anchor updated |
| `docs/index.md:5` showed commands before install/mount | dx MAJOR | **Applied** — "after [installing and mounting the Btrfs state directory](getting-started.md#prerequisites)" |
| getting-started:52 "prints the one fix" | cruft MAJOR | **Applied** — "one `fix:` line per failed check — here a single mount fix" |
| README prerequisite list had drifted from getting-started (no `git` repo, duplicated `crun` paragraph) | cruft MAJOR | **Applied** — README adds "a non-bare `git` repo", defers the full list to getting-started, and the duplicated `crun` paragraph is gone (detail lives in the prerequisite row) |
| "if `up` reports no buildable image" — that string does not exist | ai-ergonomics MAJOR | **Applied** — quotes the real `snow.yml declares no buildable Dockerfile under 'images:'` and names `id`/`context`/`dockerfile` |
| `sudo tee /etc/systemd/system/` (a directory) exits 1 | ai-ergonomics MAJOR | **Applied** — both pages name `slipforkd.socket` / `slipforkd.service` |
| "`--crun <PATH>` skips it" implies `slipfork up --crun` (exits 2) | ai-ergonomics MAJOR | **Applied** — "`slipforkd --crun <PATH>` skips the download" |
| `--world` implied cwd-independence; `select()` resolves the checkout first | ai-ergonomics MAJOR | **Applied** — "`--world` only picks which world of the current project to use, so run these from inside that project's Git/JJ checkout" |
| Prerequisites never forbade `sudo slipfork` (root-owned `.worktrees/` + vault) | ai-ergonomics MAJOR | **Applied** — new row: Docker usable as your own user; **never** run `slipfork`/`slipforkd` under `sudo` |
| `daemon.log` path wrong when `XDG_RUNTIME_DIR` is unset | ai-ergonomics MINOR | **Applied** — inline `# <state_dir>/run/daemon.log when XDG_RUNTIME_DIR is unset` |
| `list` row omitted the state strings | ai-ergonomics MINOR | **Applied** — `running`/`stopped`/`failed` (verified in `world_state_label`) |
| `forward` blocks until Ctrl-C | ai-ergonomics MINOR | **Applied** — "in the background" |
| `prune --dry-run` documented as "(default)" | cruft MINOR | **Applied** — "Preview orphaned worlds by default; `--yes` reaps them", example `slipfork prune` |
| `share --stop` "documents a success path for a feature that never exists" | cruft MINOR | **Rejected** — `Registry::unshare_world` is idempotent by contract (`registry.rs:405-419`): no lease → success, returns the local origin. Documenting it stops agents treating `share --stop` as an error |
| Mount `noatime` stated as a hard requirement while `doctor` treats it as advisory | cruft MINOR | **Rejected** — the literal `user_subvol_rm_allowed,noatime` is the mount line `doctor` prints as its fix; `cli.md` already documents `noatime` as advisory and never part of `ok` |
| Doctor contract restated in index and getting-started instead of only `cli.md` | cruft MINOR | **Rejected** — the node's design invariant requires the `fix:` contract on the landing page; the parent directive for item 2 names `docs/index.md` explicitly |
| AppArmor two-command block duplicated in `cli.md` and getting-started | dx MINOR | **Rejected** — a reference page must show the two install commands it is describing; both use the stable `/etc/apparmor.d/slipfork` path, and the profile text itself lives only in `cli.md` |
| `dag plan` example precedes the manifest it needs | dx MINOR | **Applied** — `# after writing slipfork.toml (see below)` comment in the console block |
| `snow.yml` "top-level `project:` refused" read as contradicting `slipfork.toml`'s `project` | dx MINOR | **Applied** — "**in `snow.yml`** … (`project` is a `slipfork.toml` key)" |
| `AGENTS.md:3` one 700-character sentence hid the `0.1.0`/`0.2.0` boundary | dx MINOR | **Applied** — split into two sentences (framework list kept: it is the only place naming the target suites) |

## Real transcripts captured earlier in this node (unchanged)

`slipfork doctor` (text, exit `1`) and `--json` (13 keys: `btrfs_super_magic`, `filesystem`, `fixes`, `fstype`, `gid`, `netns`, `noatime`, `ok`, `probed_path`, `state_dir`, `subvolume_ioctl`, `uid`, `unshare_userns`) reproduce the `getting-started.md` §1 and `cli.md` transcripts exactly.

```console
$ slipfork doctor
state_dir: /home/remy/.cache/slipfork
filesystem: xfs
btrfs_super_magic: mismatch
subvolume_ioctl: skipped
noatime: missing
unshare_userns: ok
netns: ok
fix: sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> <state_dir>
```

`slipfork dag plan` on a scratch `acme-shop` checkout (real digests; a `front/src/app.ts` edit changed only the artifact digest, a `api/migrations/0001_schema.sql` edit changed all three fixture digests and left `front` identical):

```console
$ slipfork dag plan
artifact front            Rebuild (65f537d6f3b4)
wave 0 fixture migrate      Rebuild (1476fead1385)
wave 1 fixture seed_catalog Rebuild (c0ba2a8330c1)
wave 1 fixture seed_minimal Rebuild (60a17079cace)
```

Install split proven from the workspace metadata: `slipfork-engine` → `slipfork-world-init`; `slipfork` → `git-remote-slipfork`, `slipfork`, `slipforkd`. `slipfork daemon unit --only apparmor` renders the four documented lines (`abi <abi/4.0>,`, `profile slipfork <bin_dir>/slipfork{,d} flags=(unconfined) {`, `userns,`, `}`).

## Acceptance

| # | Command | Exit | Evidence |
|---|---|---:|---|
| 1 | `! grep -rniE 'incus\|zfs\|ficlone\|reflink' README.md docs/index.md docs/getting-started.md docs/reference/cli.md AGENTS.md` | 0 | no output |
| 2 | `grep -n 'user_subvol_rm_allowed,noatime' README.md docs/getting-started.md docs/reference/cli.md` | 0 | 9 hits across all three pages |
| 3 | `grep -n 'btrfs' README.md docs/index.md docs/getting-started.md docs/reference/cli.md` | 0 | 16 hits across all four pages |
| 4 | `grep -n -- '--only apparmor' docs/reference/cli.md` | 0 | 4 hits |
| 5 | `make docs-check` | 0 | Zensical `--strict`: "No issues found"; honesty: OK (9 published docs) |
| + | `uv run --no-project tools/guard.py` | 0 | OK (499 files) |
| + | `! grep -riE 'option [ab]\|spike [ab]' <six pages>` | 0 | no output |
| + | `! grep -rn 'planner only\|single mount fix\|zero `sudo` after\|--repo\|--dry-run' <six pages>` | 0 | no output (remaining hit is the corrected "one `fix:` line per failed check — here a single mount fix") |
| + | page lengths `wc -l` (six pages) | 0 | 62 / 65 / 159 / 158 / 70 / 81, all ≤ 160 |
| + | anchors `#prerequisites`, `#6-ubuntu-2404-apparmor-required-on-stock-kernels-and-the-optional-systemd-daemon`, `#slipfork-daemon-unit` | 0 | present in the built site (`site/getting-started/index.html`) |

## Follow-ups (outside this node's `paths`)

- **`--world` clap help drift:** `crates/slipfork/src/cli.rs:57,72,124,137,150,157,183,192,199,223` still say "World fork to boot/stop/…; omit for the canonical world of `cwd`'s repository". The shipped grammar is `<fork>`, `<project>:<fork>`, canonical `<project>`; the help text should name it (M3.15 owner).
- **`dag plan --repo` is a user no-op** (`main.rs:1287-1374` accepts only the VCS root). Docs no longer mention it; either delete the flag or make it meaningful — a documented no-op is exactly what this gate removed.
- **`prune --dry-run`** still exists in `cli.rs` but is now undocumented (bare `prune` previews). Keep it for scripts or drop it.
- **Version single-source:** workspace `Cargo.toml` still says `0.2.0` (`slipfork --version` → `slipfork 0.2.0`), `CHANGELOG.md` has a `0.2.0` section, and `zensical.toml` labels the CLI page `CLI (0.2.0)`, while the owner's binding ruling makes published pages `0.1.0` (logged on M3.23).
- **`run_command` is not templated** but three published pages use `${{ components::<id>.private_port }}` inside it: `docs/reference/manifest.md:16,44` and `docs/snow-modes.md:17,51` (out of paths; they fail at boot with `sh: Bad substitution`).
- `docs/reference/manifest.md:157-158` still says coverage and per-world snapshot execution "land in Milestone 3"; should become `0.2.0`.
- `crates/slipfork/src/cli.rs` `prune` help says "orphaned worlds and snapshots", while `prune` does not GC sealed snapshots yet.
- `docs/design/m1-core-and-poc.md:23,45` still describes `RunnerProtocol v1`/NDJSON (excluded from the site, but stale vocabulary for the next design sweep).
