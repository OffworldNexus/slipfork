# Install

Slipfork is a Linux CLI built on Btrfs subvolumes and rootless `crun`; `slipfork doctor` checks the host and names the exact remedy for anything missing.

```console
$ curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh
$ slipfork doctor
✓ Ready
```

## You need Btrfs {#on-ext4-or-xfs-mount-a-btrfs-workspace-image-in-your-home-folder}

Worlds are Btrfs subvolumes, so the checkout you boot must sit on a Btrfs mount; on ext4 or XFS you mount a sparse image there instead. Slipfork finds its store from the checkout's mount, with no CLI setting.

The mount point is `$WS` (default `~/slipfork`), and a systemd `.mount` unit keeps it mounted across reboots. The snippet below reads `SLIPFORK_WORKSPACE`, not the `slipfork` binary, so export it in your profile to choose another directory.

```bash
WS="${SLIPFORK_WORKSPACE:-$HOME/slipfork}"   # ← your mount point
IMG="$WS.img"

sudo apt install btrfs-progs                 # Debian/Ubuntu; dnf: btrfs-progs, zypper: btrfsprogs, pacman: btrfs-progs
sudo install -d -o "$USER" -g "$(id -gn)" "$WS"
truncate -s 64G "$IMG" && mkfs.btrfs -q -f "$IMG"

UNIT="$(systemd-escape --path --suffix=mount "$WS")"
sudo tee "/etc/systemd/system/$UNIT" >/dev/null <<EOF
[Unit]
Description=Slipfork Btrfs workspace image

[Mount]
What=$IMG
Where=$WS
Type=btrfs
Options=loop,user_subvol_rm_allowed,noatime,compress=zstd,discard=async

[Install]
WantedBy=multi-user.target
EOF
sudo systemctl daemon-reload && sudo systemctl enable --now "$UNIT"
sudo chown "$USER:$(id -gn)" "$WS"
```

Grow it later: `truncate -s +64G "$IMG"; sudo losetup -c "$(losetup -j "$IMG" | cut -d: -f1)"; sudo btrfs filesystem resize max "$WS"`.

Images are sparse and `discard=async` returns freed blocks, so dropping a fork gives the space back.

A fresh Btrfs top level is root-owned, so the final `chown` makes `$WS` writable for `slipfork up` and `doctor`. Run `doctor` inside the checkout you will boot; its Storage row probes that path, and [Troubleshooting](troubleshooting.md) maps failures.

## Packages

The installer adds the signed repository and installs the package; these are the equivalent commands once it is configured:

| Distro | Command |
| :--- | :--- |
| Debian, Ubuntu | `sudo apt-get install -y slipfork` |
| Fedora, RHEL, Rocky, Alma | `sudo dnf install -y slipfork` |
| openSUSE | `sudo zypper install -y slipfork` |
| Alpine | `sudo apk add slipfork` |
| Arch Linux | `sudo pacman -S --needed slipfork` |

Run it as your normal user — the script asks for `sudo` itself. Packages ship `slipfork`, `slipfork-lsp`, `slipfork-world-init` and their man pages, and depend on `crun`, `slirp4netns` and `btrfs-progs`.

!!! note "Alpine: set the ingress user yourself"

    `apk` runs install hooks without `SUDO_USER`, so the hook cannot name you. The ingress fails closed until you set it:

    ```console
    $ printf 'SLIPFORK_INGRESS_USER=%s\n' "$(id -un)" | sudo tee /etc/conf.d/slipfork-ingress
    $ sudo rc-update add slipfork-ingress default
    $ sudo rc-service slipfork-ingress start
    ```

## Tarball, no root

```console
$ curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh -s -- --user
```

This unpacks the same three binaries under `~/.local/bin` and, on systemd, starts your user daemon; the `:80` ingress still needs root, so use a VM you control when you cannot get it. Pin a release with `--version 0.1.1`, or pick another directory with `--prefix "$HOME/.local"`.

## Editors and LSP

Every package installs `slipfork-lsp` beside `slipfork`. The Vim/Neovim plugin and the Zed extension live in [the repository](https://github.com/OffworldNexus/slipfork/tree/develop/editors); Zed currently installs its extension as a dev extension.

The language server provides diagnostics, hover and format-on-save on Linux, macOS and Windows. The CLI itself is Linux-only because worlds need Btrfs and Linux namespaces.

## Uninstall

```console
$ sudo apt-get remove slipfork       # Debian, Ubuntu
$ sudo dnf remove slipfork           # Fedora, RHEL, Rocky, Alma
$ sudo zypper remove slipfork        # openSUSE
$ sudo apk del slipfork              # Alpine
$ sudo pacman -Rns slipfork          # Arch
```

The package removes the binaries and stops the units. Worlds stay in `<btrfs_mount>/.slipfork-store` (with this recipe, `~/slipfork/.slipfork-store`), local secrets under `$XDG_STATE_HOME/slipfork`; run `scripts/purge-store.sh <store>` from the repository to reclaim the space.

## Where next

* [Tutorial](tutorial.md) — build a Python API, add a database, add a front and fork it.
* [Troubleshooting](troubleshooting.md) — one line and one remedy for every failed check.
* [CLI](cli.md) — every verb and the terminal contract.
