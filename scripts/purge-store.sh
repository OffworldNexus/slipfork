#!/bin/sh
# Purge a Slipfork store, including the child subvolumes sealed with BTRFS_SUBVOL_RDONLY
# (ro=true). Destroy refuses a sealed subvolume, and a parent refuses while a child exists,
# so enumerate every subvolume root (inode 256) deepest-first inside one mount (-xdev skips
# nested pod mounts), unseal the whole list, then delete it in a second pass. Quiet on success:
# prints nothing and exits 0 only when the store is gone.
set -eu

store=${1:?usage: purge-store.sh <store>}
[ -e "$store" ] || exit 0

subs=$(find "$store" -xdev -depth -inum 256 2>/dev/null || true)
if [ -n "$subs" ] && command -v btrfs >/dev/null 2>&1; then
	printf '%s\n' "$subs" | while IFS= read -r sub; do
		btrfs property set -ts "$sub" ro false >/dev/null 2>&1 || true
	done
	printf '%s\n' "$subs" | while IFS= read -r sub; do
		btrfs subvolume delete "$sub" >/dev/null 2>&1 || true
	done
fi

rm -rf "$store" 2>/dev/null || true
[ -e "$store" ] || exit 0

left=$(find "$store" -xdev -depth -inum 256 2>/dev/null || true)
count=0
first=$store
if [ -n "$left" ]; then
	count=$(printf '%s\n' "$left" | grep -c . || true)
	first=$(printf '%s\n' "$left" | head -n 1)
fi
printf 'purge-store: %s still holds %s subvolume(s) (first: %s); next: stop stale slipfork daemons (pkill -f "slipfork __daemon") and retry\n' \
	"$store" "$count" "$first" >&2
exit 1
