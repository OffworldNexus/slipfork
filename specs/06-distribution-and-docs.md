# 06 — Distribution & Documentation

One `slipfork` binary, native on every Linux distribution your team uses, plus a docs site that only promises what the binary does. A developer should get a working world from one command on a fresh machine.

```console
$ curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh
$ slipfork up
✓ Ready -> http://acme-shop.localhost
```

---

## 1. Artifact Matrix

Two architectures only — `x86_64`/`amd64` and `aarch64`/`arm64` — and every leg runs natively (no emulated compilers as evidence).

| Family | Formats | Runtime contract | Manager |
| :--- | :--- | :--- | :--- |
| Debian / Ubuntu | `.deb` | `libc6 (>= <measured>)`, `libgcc-s1` when `DT_NEEDED` names it, `crun`, `btrfs-progs` | `dpkg-deb --root-owner-group` |
| Fedora / RHEL / Rocky / Alma / openSUSE | `.rpm` | `glibc >= <measured>`, `crun`, `btrfs-progs` | `rpmbuild` |
| Arch Linux | `.pkg.tar.zst` + AUR `PKGBUILD`/`.SRCINFO` | `glibc`, `crun`, `btrfs-progs` | `makepkg` |
| Alpine / musl | `.apk` + dynamic-musl `.tar.gz` | `libgcc`, `crun`, `btrfs-progs` | `abuild` / `apk` |
| Any glibc host | `slipfork-<target>-gnu.tar.gz` | glibc floor, no `RUNPATH` | `install.sh` |
| Any musl host | `slipfork-<target>-musl.tar.gz` | `libgcc`, dynamic (not static-pie) | `install.sh` |

`slipfork` needs `crun` and a Btrfs filesystem; both are declared, never bundled. The binary itself links only libc, libm, libgcc_s, and the dynamic loader — language runtimes are `dlopen`ed or containerized, never `NEEDED`.

---

## 2. Debian / Ubuntu `.deb`

```text
/usr/bin/slipfork                      -> ../share/slipfork/lib/slipfork
/usr/share/slipfork/{LICENSE,lib,share}
/usr/lib/systemd/system/slipfork-ingress.socket
/usr/lib/systemd/system/slipfork-ingress@.service
/usr/lib/systemd/user/slipfork.service
/etc/apparmor.d/slipfork
/usr/share/man/man1/slipfork.1.gz
/usr/share/bash-completion/completions/slipfork
/usr/share/zsh/site-functions/_slipfork
/usr/share/fish/vendor_completions.d/slipfork.fish
/usr/share/vim/vimfiles/ftdetect/forkfile.vim
/usr/share/vim/vimfiles/syntax/forkfile.vim
/usr/share/vim/vimfiles/indent/forkfile.vim
/usr/share/vim/vimfiles/ftplugin/forkfile.vim
```

| Requirement | Contract |
| :--- | :--- |
| Ownership | Built with `dpkg-deb --root-owner-group`; no host install during the build |
| Dependency floor | `Depends: libc6 (>= X.Y)` is **measured** by `scripts/check-portable-elf.sh --max-glibc`, never typed by hand |
| Ceiling | `--max-glibc <declared>` refuses a bundle built on too new a host instead of shipping an unverified floor |
| Ingress on `:80` | Socket-activated: `slipfork-ingress.socket` listens on `127.0.0.1:80`, and one template instance per connection hands the fd to the user daemon |
| AppArmor | `/etc/apparmor.d/slipfork` ships and is enforced; `slipfork doctor` reports when it is not loaded |
| `lintian` | Clean — no errors and no warnings that name a real packaging defect |
| Reproducibility | `SOURCE_DATE_EPOCH` fixes ar-member timestamps; two builds of one commit are byte-identical |

The package enables the **user** unit and the system socket only for the installing `SUDO_USER`, so worlds stay in the developer's session; a rootless or container install prints nothing. There is one binary: the per-user unit runs the same `slipfork` binary in its daemon mode, never a separate `slipforkd`. The install hook renders `slipfork-ingress@.service` with that user, enables `slipfork-ingress.socket` and loads the AppArmor profile; the instance forwards the accepted connection fd to `daemon.sock` while the daemon does the routing.

---

## 3. Fedora / RHEL / Rocky / Alma / openSUSE `.rpm`

| Requirement | Contract |
| :--- | :--- |
| Spec | `scripts/rpm/slipfork.spec` is checked in and only moves the staged tree into `%{buildroot}` |
| Builder | Host `rpmbuild` when present, else a `rockylinux:9` container; the builder is not the supported floor |
| Requires | `glibc >= <measured>` plus `crun`/`btrfs-progs`; measured by the same ELF check as `.deb` |
| Layout | Same paths as the `.deb` under `/usr`, with the systemd units in `/usr/lib/systemd/system` |
| Nightly stamp | `0.1.1-0.nightly.YYYYMMDD.rN.gSHA` — the `-0.` prefix sorts below the release |
| Verify | `rpm -qpl`, then install and boot one world inside a disposable root |

---

## 4. Arch Linux

- **Repository package:** `slipfork-<ver>-<rel>-x86_64.pkg.tar.zst` built by `makepkg` from the checked-in `PKGBUILD`; install with `pacman -U`.
- **AUR:** the same `PKGBUILD` plus a generated `.SRCINFO` and the release `sha256sums`, so `yay -S slipfork-bin` and `paru -S slipfork-bin` work.
- **Units:** a `slipfork-ingress.socket` + `slipfork-ingress@.service` pair on `127.0.0.1:80`, and a user `slipfork.service` — systemd, exactly as Debian. The package's `.INSTALL` hook enables both for the installing user and loads the AppArmor profile.
- **Dependencies:** `glibc`, `crun`, `btrfs-progs`; `btrfs-progs` is required because `mkfs`/subvolume flags matter, not just the kernel module.

---

## 5. Alpine / musl `.apk`

| Requirement | Contract |
| :--- | :--- |
| Build | `abuild` in an `alpine:3.21` container, `x86_64` and `aarch64` |
| Runtime | `crun`, `btrfs-progs`, `libgcc`; the binary is **dynamically** linked musl (a static musl binary cannot `dlopen`) |
| `:80` ingress | OpenRC service `slipfork-ingress` binds `127.0.0.1:80`; the per-user daemon connects over its Unix socket |
| Paths | `/usr/bin/slipfork`, `/usr/share/slipfork/**`, `/usr/share/vim/vimfiles/**`, `/etc/conf.d/slipfork-ingress` |
| Verify | `apk add ./slipfork-*.apk`, `printf 'SLIPFORK_INGRESS_USER=%s\n' "$(id -un)" \| sudo tee /etc/conf.d/slipfork-ingress`, `rc-update add slipfork-ingress default`, `rc-service slipfork-ingress start`, boot one world, `slipfork down` |

The package ships the system `slipfork-ingress` OpenRC script that owns `127.0.0.1:80`, but its post-install hook is deliberately a silent `#!/bin/sh` that exits 0: `apk` runs maintainer scripts with a PATH-only environment, so it never sees `SUDO_USER`. The installing user points the ingress at themselves by writing `/etc/conf.d/slipfork-ingress` with the three-line snippet in `docs/install.md` (`printf … "$(id -un)"`, then `rc-update add slipfork-ingress default` and `rc-service slipfork-ingress start`). Install-hook auto-enable is a systemd feature of the `.deb`, `.rpm`, and pacman formats only; on systemd hosts `docs/install.sh` does the equivalent socket setup when run as root with `SUDO_USER`. Only `openrc` is supported on Alpine; do not ship a systemd unit that silently no-ops.

---

## 6. Portable Archives & Checksums

`slipfork-<target>.tar.gz` holds `slipfork`, `LICENSE`, and `SHA256SUMS` over both files.

| Target triple | Libc | Floor assertion |
| :--- | :--- | :--- |
| `x86_64-unknown-linux-gnu` | glibc | `--max-glibc` measured, `RUNPATH` refused |
| `aarch64-unknown-linux-gnu` | glibc | same |
| `x86_64-unknown-linux-musl` | musl | no glibc floor; must link `libgcc_s.so.1` |
| `aarch64-unknown-linux-musl` | musl | same |

`scripts/check-portable-elf.sh` is the gate and prints two machine-readable lines that packaging derives from:

```text
max-glibc: 2.28 (/path/to/slipfork)
needed: libc.so.6 libgcc_s.so.1 libm.so.6 ld-linux-x86-64.so.2
```

It refuses `NEEDED libpython*`/`libphp*`, any absolute `RPATH`/`RUNPATH` into a build host, and any `DT_NEEDED` that is not libc, libm, libgcc, the loader, or a pre-2.34 glibc object. `--allow-origin-rpath` is the single exception, for `$ORIGIN`-relative layouts only.

### `slipfork-lsp` language server

`slipfork-lsp` ships as a standalone portable binary, separate from `slipfork` (which stays Linux-only): it touches argv and stdio, never Btrfs or `crun`, so it builds for every desktop an editor runs on.

| Platform | Architecture | Asset |
| :--- | :--- | :--- |
| Linux | `x86_64`, `aarch64` | `slipfork-lsp-linux-<arch>.tar.gz` |
| macOS | `x86_64`, `aarch64` | `slipfork-lsp-darwin-<arch>.tar.gz` |
| Windows | `x86_64`, `aarch64` | `slipfork-lsp-windows-<arch>.zip` |

Each archive holds one top-level `slipfork-lsp-<platform>-<arch>/` directory with the executable and `LICENSE`. The Zed extension consumes these assets directly; a `cargo install --path crates/slipfork-lsp` build is the documented local override via `lsp.slipfork-lsp.binary.path`.

---

## 7. Universal `docs/install.sh`

One POSIX `sh` script, published at `https://offworldnexus.github.io/slipfork/install.sh`, with `set -eu` and no bashisms. It adds exactly one signed repository from one `*.offworld-nexus.app` host per package manager and otherwise installs a TLS-trusted tarball, with the published `.sha256` verified when present.

```console
$ curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh
$ curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh -s -- --user
$ curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh -s -- --prefix "$HOME/.local"
```

| Host | What it serves |
| :--- | :--- |
| `apt.offworld-nexus.app` | Debian/Ubuntu repo: `Suites: any-distro`, `Components: main` (stable) or `nightly`, `Signed-By: /etc/apt/keyrings/offworld-nexus.gpg` |
| `rpm.offworld-nexus.app` | `.rpm` repo at `{stable,nightly}/$basearch/` with `gpgcheck=1`, `repo_gpgcheck=1`, `gpgkey=…/gpg.key` |
| `apk.offworld-nexus.app` | Alpine repo signed by `packages@offworld.nexus.rsa.pub`, at `{stable,nightly}` |
| `arch.offworld-nexus.app` | pacman `[offworld]` at `{stable,nightly}/$arch`, key `E9DD32C63321505EBBCF86386E08120B3306C720` |
| `dl.offworld-nexus.app` | `tarballs/{stable,nightly}/…` plus `macos/` and `windows/` LSP builds |

| Path | When it is taken | What it does |
| :--- | :--- | :--- |
| Native package (default) | Root or usable `sudo`, with `apt-get`/`dnf`/`zypper`/`pacman`/`apk` | Writes the key + repository file for that manager, then installs `slipfork`, so upgrades come from the package manager |
| Standalone tarball | `--user`, `--prefix`, `--develop`, a musl host, no supported package manager, or no usable `sudo` | Verifies the published `.sha256` when present; an overridden origin requires a digest, then stages under a prefix with `bin/` |

| Flag | Effect |
| :--- | :--- |
| `--version <ver>` | Install exactly this version |
| `--nightly` | Use the `nightly` channel; re-run without it to return to releases |
| `--user` | No root; tarball under `~/.local` |
| `--prefix <dir>` | No root; tarball under an absolute directory you own |
| `--develop [ref]` | Use the CI archive named by `SLIPFORK_ARCHIVE_URL`; unsigned, so a digest is mandatory |
| `-y`, `--yes` | Do not ask for confirmation |

!!! info "Trust rules"

    - Package managers verify signatures through the repository key, and that verification is never disabled.
    - The default tarball origin is HTTPS on `dl.offworld-nexus.app`, and its published `.sha256` is checked when present.
    - An overridden origin (`SLIPFORK_ARCHIVE_URL`, `--develop`) requires `SLIPFORK_SHA256` or `SLIPFORK_SHA256SUMS_FILE`, and a mismatch always aborts.
    - `stdin` carries the piped script, so prompts read `/dev/tty` only; `NO_COLOR`/`TERM=dumb` emit zero escapes.

---

## 8. Release CI Legs

| Job | Matrix | Proof |
| :--- | :--- | :--- |
| Build bundle | native runner per arch, in the declared floor image | `check-portable-elf.sh --max-glibc` passes; layout assert finds no strays |
| Package | `.deb` × 2 arch, `.rpm` × 2 arch, `.pkg.tar.zst`, `.apk`, 4 archives | Each artifact is built from that bundle, never from a host build |
| Install | Debian/Ubuntu, Fedora, Rocky, openSUSE, Arch, Alpine × 2 arch | Install the package in the distro image, boot one world, `slipfork doctor` reports `✓ Ready`, tear down |
| Stage | one `stage-dist.py --dry-run` on every run; `offworld-dist` uploaded only when `should_publish` | One validated bundle carries every format and architecture, and the workflow itself holds no publishing secret |
| Docs | `zensical build --strict` | Honesty check passes; nav files exist |

```mermaid
flowchart LR
  V["version"] --> B["build bundle"]
  B --> P["packages"]
  P --> I["install + verify per distro"]
  I --> L["lsp assets"]
  L --> S["stage offworld-dist"]
  S -.->|"workflow_run"| X["org publisher signs + syncs"]
```

The pipeline runs with `permissions: contents: read`. An external organization publisher verifies branch/tag policy, signs the packages and repository indexes, then syncs `*.offworld-nexus.app`; a `release_set` retains the newest 7 nightlies and every stable release, so retention lives server-side rather than in this repository.

---

## 9. Documentation Site

| File / tool | Contract |
| :--- | :--- |
| `zensical.toml` | `docs_dir = "docs"`, nav lists only shipped pages, Mermaid fences enabled, `validation.invalid_links = true` |
| `README.md` | Problem-first: what a world is, then a copy-pasteable `slipfork up` in the first 25 lines |
| `CHANGELOG.md` | Keep-a-Changelog sections per release; each entry names a user-visible behavior |
| `docs/*.md` | Scannable tables, `!!! info` / `!!! warning` admonitions, one Mermaid workflow diagram per page, paragraphs ≤ 2 sentences |
| Excluded dirs | `docs/design/`, `docs/decisions/`, `docs/research/`, `docs/knowledge/`, `docs/VISION.md` — blueprints and research never reach the public site |
| Forkfile highlighting | `docs/javascripts/forkfile-highlight.js` plus a `forkfile` fence language; no build-time plugin |
| Build gate | `uvx --offline zensical build --strict` — a warning is a failure |
| Honesty gate | `uv run --no-project tools/check_docs_honesty.py` — every `slipfork <cmd>` in published docs must exist in `slipfork --help` |

Every published command, flag, and output line is executed by the test plan in [04](./04-testing-plan.md); pre-code UX blueprints live only in `docs/design/dx-*.md`.

---

## 10. Repository Layout

One home per artifact: build/check helpers live in `scripts/` (`build-bundle`, `build-deb`, `build-rpm`, `build-arch`, `build-apk`, `build-lsp`, `build-standalone`, `check-bundle-layout.sh`, `check-install.sh`, `check-portable-elf.sh`, `release_stamp.py`), the universal installer is `docs/install.sh`, and distro recipes, systemd units plus `packaging/stage-dist.py` live in `packaging/`. `stage-dist.py` validates naming, nightly sort stamps and control headers before emitting the single `offworld-dist` bundle, and `release_stamp.py` derives the stable or nightly version stamp. `packaging/` never carries a second installer implementation, and `scripts/` never hardcodes a dependency floor that `check-portable-elf.sh` did not measure.

## 11. OpenCode Plugin (`@slipfork/opencode`)

Parallel agents editing one checkout collide on files, ports, and databases. `@slipfork/opencode` gives every OpenCode session its own Btrfs fork — code, `.venv`/`node_modules`, and live PostgreSQL/S3 state — behind one `http://<fork>--<world>.localhost` URL.

```console
$ bun add -d @slipfork/opencode
```

```jsonc
// .opencode/opencode.jsonc
{
  "plugin": ["@slipfork/opencode"]
}
```

The plugin shells out to the `slipfork` binary already on your `PATH` (or the `binary` option), so it adds no second CLI and no embedded world logic. Boot the parent world once with `slipfork up`; every fork then appears under it in `slipfork status`.

```mermaid
flowchart LR
  Session["OpenCode session"] --> WT["worktree.transform"]
  WT -->|"slipfork fork <fork>"| Checkout["<project>/.forks/<fork>"]
  Checkout --> URL["http://<fork>--<world>.localhost"]
  Session --> Shell["shell create.before"]
  Shell --> Env["SLIPFORK_WORLD + SLIPFORK_URL"]
  Session --> Idle["experimental.idle"]
  Idle -->|"slipfork drop <fork>"| Gone["fork reaped"]
```

Inside a fork checkout the plugin exports `SLIPFORK_WORLD` and `SLIPFORK_URL` so your shell commands and previews can target the fork. It also appends one context line naming that URL and reaps the fork with `slipfork drop <fork>` when the session goes idle.

!!! warning "Sessions are ephemeral"
    A fork is snapshotted at create time. Commit work you need to keep before ending the session; the plugin drops only the forks it created and never touches an existing `.forks/<name>` checkout.

When the CLI refuses — a non-Btrfs project, a dirty checkout — the plugin surfaces the CLI's own `message` + `next:` remedy verbatim. Run `slipfork doctor` when no decoded remedy arrives, and set `binary` in the plugin options when `slipfork` is not on `PATH`.
