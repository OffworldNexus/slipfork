# Log M3.26

## 2026-11-17T14:21 [lead]

created (task) slipfork fork fails closed when .gitignore ignores .worktrees/.slipfork

## 2026-11-17T14:26 [lead]

edited fields

## 2026-11-17T14:26 [lead]

status ready -> active

## 2026-11-17T14:26 [lead]

attempt 1/3 started

## 2026-11-17T14:26 [lead]

base 775776f161844e34cddec1c46cff7f8d3dfbe792

## 2026-11-17T14:26 [lead]

Live-verified owner path (clean env as uid 1000): 'slipfork doctor' exit 0 (filesystem btrfs, subvolume_ioctl ok, unshare_userns ok, netns ok) and 'slipfork list' exit 0 with state_dir /home/slipfork/.cache/slipfork. Note for the record: invoking the CLI under another user's inherited XDG_RUNTIME_DIR fails closed with 'cannot create /run/user/0/slipfork: Permission denied' — correct fail-closed behaviour, but the message could name the variable.

## 2026-11-17T14:39 [lead]

Gate passed: one canonical staging path (bare 'git add -A -- .' then 'git rm -r -f --cached --ignore-unmatch .slipfork .worktrees'), dead pathspec constants deleted, new critical_vcs test fails on the old argv and passes now, conformance matrix strengthened, make check exit 0.

## 2026-11-17T14:39 [lead]

status active -> done
