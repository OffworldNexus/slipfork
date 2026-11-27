---
id: M3.197
title: "install.md: Btrfs first and a single WS mount-point variable (owner rejected the buried/hardcoded recipe)"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: []
paths: [docs/install.md]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-26T07:35"
updated: "2026-11-26T07:38"
---

# Problem

`docs/install.md` (M3.192) buries the Btrfs requirement at line 55, after five sections of prose, and its recipe hardcodes `~/projects` as the mount point — the owner cannot set the mount point without editing the unit by hand. Their words: "a lot of rambling before the btrfs thing and you put it back as it was (no way to easily set the mount point…)".

# Restructure (lean)

1. **Top, ≤15 lines**: what ships, the one-liner, `slipfork doctor`.
2. **"You need Btrfs"** immediately after it: two sentences, and the recipe right there — not at the bottom.
3. Then, compressed: distro table, Alpine ingress note (3 lines), tarball/`--user`, editors/LSP, uninstall.
4. Delete the duplicate "Check the host" section (fold its one useful sentence into the top or the recipe).

# Mount point must be a single variable

The recipe must lead with one variable the reader sets, used everywhere — no `~/projects` hardcoded:

```bash
WS="${SLIPFORK_WORKSPACE:-$HOME/slipfork}"   # ← your mount point
IMG="$WS.img"

sudo apt install btrfs-progs                 # or dnf/zypper/pacman
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
```

State that the mount point is `$WS` (default `~/slipfork`), that it survives reboots via the `.mount` unit, and that growing it later is `truncate -s +64G "$IMG"; sudo losetup -c "$(losetup -j "$IMG" | cut -d: -f1)"; sudo btrfs filesystem resize max "$WS"`.

# Acceptance

- The Btrfs recipe appears within the first third of the page, before the distro table.
- One variable sets the mount point; no `~/projects` anywhere.
- ≤2-sentence paragraphs; no duplicated doctor section.
- `make docs-check` green; commit `[M3.197] ...` with `docs/install.md` only, done, no push.
