# Log M3.107

## 2026-11-24T12:02 [conductor]

created (task) Boot-failure and run-replay visual hierarchy; keep captures pure (stream, no synthetic summary)

## 2026-11-24T12:02 [conductor]

status ready -> active

## 2026-11-24T12:02 [lead]

attempt 1/3 started

## 2026-11-24T12:02 [lead]

base 33a3fb63a98796acf4c7045385fb3f6f4a021417

## 2026-11-24T12:04 [lead]

OWNER CLARIFICATION: show the executed RUN command (not image id twice); stream RUN stdout/stderr live during up (indented out/err, TUI rows intact); demote `crun exited with code N` to -v/raw log; envelope = command + shell stderr tail + exactly one next:; capture = command output only, stderr renders err. Proof: RUN printing to both streams + exit non-zero.

## 2026-11-24T12:04 [lead]

edited fields

## 2026-11-24T12:08 [lead]

Visual contract v2 (Opus) written into NODE.md; owner withdrew the earlier colour table

## 2026-11-24T12:11 [lead]

Owner addendum: live per-instruction Dockerfile build; split M3.107.2 (engine/daemon events) + M3.107.3 (client live rendering)

## 2026-11-24T12:15 [lead]

OWNER BAN: never run tools/do_vm.py sync (wiped /home/slipfork/demo twice); never touch ~/demo or its supervisor. VM proof = rsync crates/ Cargo.toml Cargo.lock into /home/slipfork/slipfork via do_vm key + cargo build as slipfork, scratch project /home/slipfork/m3107, script -qec PTY capture, clean scratch worlds. Gutter design approved.
