# Log M3.164

## 2026-11-25T12:29 [conductor]

created (task) Child processes emit colour when asked (env hints, no fake TTY); logs strips colour only at the viewing end

## 2026-11-25T12:30 [conductor]

edited fields

## 2026-11-25T12:30 [conductor]

status ready -> active

## 2026-11-25T12:30 [lead]

attempt 1/3 started

## 2026-11-25T12:30 [lead]

base 767e14ce8badd59a7e090f3fbcb034bf67a6f945

## 2026-11-25T12:35 [conductor]

edited fields

## 2026-11-25T13:01 [lead]

status active -> review

## 2026-11-25T13:01 [lead]

env hints: TERM=xterm-256color, CLICOLOR=1, CLICOLOR_FORCE=1, COLORTERM=truecolor for service/resource/boot pods and build RUN; explicit image/Forkfile values win and a declared NO_COLOR opts out. FORCE_COLOR deliberately skipped: it is a Node/chalk convention that overrides NO_COLOR inside JS libraries and buys no non-Node tool. No pty, no isatty shim, never TERM=dumb; an isatty-only program stays plain and that is accepted per owner direction. Storage is byte-faithful: supervise.rs BootSink/EventLog, runtime/capture.rs and reconcile/progress.rs no longer strip before capture. Viewing owns filtering: one ui::ansi {strip, colour_only, segments, visible_width}; logs/logs -r/exec/shell, the live up narrative and docker build keep only SGR on a real colour TTY, while a pipe/NO_COLOR/--json emits zero ESC; wrap/truncate are escape-aware so gutters never shear and an untrusted child cannot inject cursor/OSC controls. Triad: review-arch/ux found the per-char wrap bug and control-byte passthrough, both fixed in the one pass with tests.

## 2026-11-25T13:12 [lead]

status review -> done

## 2026-11-25T13:12 [lead]

landed as 49ab85b on release/0.1.1 (parent 3ac92be); re-verified on 3ac92be with CARGO_TARGET_DIR=/var/tmp/m3164-target CARGO_PROFILE_DEV_DEBUG=0: fmt, clippy --workspace --all-targets -D warnings, targeted ansi/render/pod_env/runtime suites and cargo test --workspace green, guard OK, critical_guard --node M3.164 no protected change. VM live: logs_colour_live ok and pod_run_live ok. Triad complete: review-arch and review-ux approved after the one wrap/control-sequence fix pass; review-tests approved.
