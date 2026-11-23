# Log M3.73

## 2026-11-23T20:16 [conductor]

created (task) Move the slipfork-lsp darwin/aarch64 archive leg to macos-14 (macos-15 arm64 pool cancels queued jobs after 15 min)

## 2026-11-23T20:16 [lead]

status ready -> active

## 2026-11-23T20:16 [lead]

attempt 1/3 started

## 2026-11-23T20:16 [lead]

base e29acf9c6c0fca71deaeb947bf5c42964dbc2e64

## 2026-11-23T20:16 [lead]

status active -> review

## 2026-11-23T20:16 [lead]

status review -> done

## 2026-11-23T20:23 [conductor]

status done -> ready: macos-14 is x64 and retired 2026-11-02 (1s cancel). Use macos-26 (current macos-latest arm64) for the archive leg instead.

## 2026-11-23T20:23 [conductor]

status ready -> active

## 2026-11-23T20:46 [lead]

edited fields

## 2026-11-23T20:46 [lead]

attempt 2/3 started

## 2026-11-23T20:46 [lead]

attempt 2/3: macos-26 leg on ddbae7f acquired a runner (GitHub Actions 1000009798, labels=macos-26) and passed in 30s; archive --version step success

## 2026-11-23T20:46 [lead]

Mach-O diagnostic from the published macos-26 artifact: platform macos, sdk 26.5, minos 11.0 (same as macos-15 artifact: sdk 15.5, minos 11.0) -> floor did NOT rise; no MACOSX_DEPLOYMENT_TARGET pin added

## 2026-11-23T20:47 [lead]

ddbae7f run 45248545947: 26/26 Packages jobs success; CI and Docs success on the same SHA

## 2026-11-23T20:47 [lead]

status active -> done
