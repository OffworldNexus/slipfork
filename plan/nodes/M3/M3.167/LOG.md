# Log M3.167

## 2026-11-25T12:50 [conductor]

created (task) Cached from-only image fails with 'no recorded USER' (M3.153 quirk)

## 2026-11-25T16:40 [conductor]

status ready -> active

## 2026-11-25T16:41 [lead]

edited fields

## 2026-11-25T16:41 [lead]

status active -> active

## 2026-11-25T16:41 [lead]

attempt 1/3 started

## 2026-11-25T16:41 [lead]

base 5a315bd081dfb90046b40350f93cc91d8e795ef5; widened paths to the real defect site helpers.rs; cached from-only head falls through because only state=='pulled' defaults to root

## 2026-11-25T16:49 [lead]

VM live: cached from-only got past USER, then failed: cannot list sealed snapshots (snapshots dir never exists for a pure pull) — second fix needed: missing snapshots root = empty scan. Also built slipfork-world-init from the private tree.

## 2026-11-25T16:51 [lead]

VM live pass 2: ghost Dockerfile (metadata-only, head key == alpine base key) wrote an image child record parented at the base; from-only pub picked up USER ghost. Fix 3: provenance by built.rootfs (images/oci -> root) before any record scan.

## 2026-11-25T16:56 [lead]

status active -> review

## 2026-11-25T16:56 [lead]

status review -> done
