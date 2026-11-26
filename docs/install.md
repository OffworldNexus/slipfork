# Install

Slipfork is a Linux CLI built on Btrfs subvolumes and rootless `crun`. Four `slipfork doctor` rows check the host and name the exact remedy for anything missing.

## One command

```console
$ curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh
$ slipfork doctor
✓ Ready
```

Run it as your normal user, not `sudo sh`: the script asks for `sudo` when it needs the distro package, then on systemd the package provisions your user daemon and the system `:80` socket. Packages are signature-verified; archive downloads verify the published SHA-256 when one exists.

The one-liner adds the signed repository and installs the package. These are the equivalent commands once that repository is configured:

| Distro | Command |
| :--- | :--- |
| Debian, Ubuntu | `sudo apt-get install -y slipfork` |
| Fedora, RHEL, Rocky, Alma | `sudo dnf install -y slipfork` |
| openSUSE | `sudo zypper install -y slipfork` |
| Alpine | `sudo apk add slipfork` |
| Arch Linux | `sudo pacman -S --needed slipfork` |

Every package ships `slipfork`, `slipfork-lsp` and `slipfork-world-init`, plus the `slipfork` and `slipfork-lsp` man pages. Packages pull in `crun` and `slirp4netns`; install `btrfs-progs` too if your distro does not ship it.

!!! note "Alpine: set the ingress user yourself"

    `apk` runs install hooks without `SUDO_USER`, so the hook cannot name you; the ingress fails closed until you set it.

    ```console
    $ printf 'SLIPFORK_INGRESS_USER=%s\n' "$(id -un)" | sudo tee /etc/conf.d/slipfork-ingress
    $ sudo rc-update add slipfork-ingress default
    $ sudo rc-service slipfork-ingress start
    ```

## Tarball, no root

```console
$ curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh -s -- --user
```

This unpacks the same three binaries under `~/.local/bin` and, on a systemd host, starts your user daemon; the `:80` ingress still needs root, so on a host where you cannot get it, run Slipfork in a VM you control. Pin a release with `--version 0.1.1`, or choose another directory with `--prefix "$HOME/.local"`.

## Check the host

```console
$ cd ~/projects/hello
$ slipfork doctor
✓ Ready
```

The four rows are Storage, Containers, Daemon and Domains; a failing row prints its own `next:` command. Run `doctor` from inside the checkout you want to boot, because Storage probes that path; [Troubleshooting](troubleshooting.md) maps every failing row.

## On ext4 or XFS: mount a Btrfs workspace image in your home folder

You do not have to repartition. A sparse Btrfs image, mounted by a systemd `.mount` unit, gives your projects real subvolumes inside your home folder.

```console
$ sudo apt install btrfs-progs          # or: sudo dnf install btrfs-progs / sudo pacman -S btrfs-progs
$ mkdir -p ~/.local/share ~/projects
$ truncate -s 64G ~/.local/share/slipfork-btrfs.img
$ mkfs.btrfs -f ~/.local/share/slipfork-btrfs.img
```

The image reports `64G` but occupies almost nothing until files are written. `discard=async` returns freed blocks to ext4 or XFS, so dropping a fork gives the space back.

Let systemd own the mount so the image is mounted now and on every boot:

```bash
UNIT="$(systemd-escape --path --suffix=mount "$HOME/projects")"
sudo tee "/etc/systemd/system/$UNIT" >/dev/null <<EOF
[Unit]
Description=Slipfork Btrfs workspace image

[Mount]
What=$HOME/.local/share/slipfork-btrfs.img
Where=$HOME/projects
Type=btrfs
Options=loop,user_subvol_rm_allowed,noatime,compress=zstd,discard=async

[Install]
WantedBy=multi-user.target
EOF
sudo systemctl daemon-reload
sudo systemctl enable --now "$UNIT"
sudo chown "$USER:$(id -gn)" "$HOME/projects"
```

Move or clone your checkouts under `~/projects`, then run `slipfork doctor` inside one; the Storage row probes that path. Grow the image later with `truncate`, `losetup -c` and `btrfs filesystem resize max ~/projects`.

## Editors and LSP

Every package installs `slipfork-lsp` beside `slipfork`. The Vim/Neovim plugin and the Zed extension live in [the repository](https://github.com/OffworldNexus/slipfork/tree/develop/editors); Zed currently installs its extension as a dev extension.

The language server provides diagnostics, hover and format-on-save on Linux, macOS and Windows. The `slipfork` CLI itself is Linux-only because worlds need Btrfs and Linux namespaces.

## Uninstall

```console
$ sudo apt-get remove slipfork       # Debian, Ubuntu
$ sudo dnf remove slipfork           # Fedora, RHEL, Rocky, Alma
$ sudo zypper remove slipfork        # openSUSE
$ sudo apk del slipfork              # Alpine
$ sudo pacman -Rns slipfork          # Arch
```

The package removes the binaries and stops the units. Worlds and images stay in `<btrfs_mount>/.slipfork-store`, while local secrets live under `$XDG_STATE_HOME/slipfork`; remove the store with `scripts/purge-store.sh <store>` from the repository when you want the space back.

## Where next

* [Tutorial](tutorial.md) — build a Python API, add a database, add a front and fork it.
* [Troubleshooting](troubleshooting.md) — one line and one remedy for every failed check.
* [CLI](cli.md) — every verb and the terminal contract.
