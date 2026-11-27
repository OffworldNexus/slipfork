# Log M3.148

## 2026-11-25T03:42 [planner]

created (task) Status/doctor/commands on Report + narrow ui helpers + guard.py no-raw-print gate

## 2026-11-25T18:24 [conductor]

status ready -> active

## 2026-11-25T18:26 [lead]

SCOPE SPLIT REQUIRED — as written M3.148 crosses 4 out-of-path subsystems. Evidence: (1) Palette pub(super) impossible: Palette::new remains in boot.rs:123, boot/render.rs:236,255,323,335,349,361, boot/status.rs:26, daemon/client.rs:66, commands/runs.rs:69, commands/docker/registry.rs:105,139,172,271 (+boot/build/tests, boot/phase tests, boot/render tests, boot/status tests, boot/progress/tests, commands/docker/build.rs:522). (2) LocalClock deletion needs main.rs:105, commands/runs.rs:68, commands/streams.rs:154. (3) ui/failure.rs split_remedy still used by main.rs:52, world/supervise.rs:44, daemon/reconcile/progress.rs:312. (4) RawPrint rule also fires on boot.rs:160,203,222,361 and world/up.rs:490. Proposed ordered split: M3.148a = status/doctor/commands on Report + delete auth.rs/status::rows::State/report_details + enable RawPrint (allowlist: ui/report/**, boot append sink, daemon+supervise journal, prompt_secret, clap help/version) [current node paths + guard]; M3.148b = runs/streams on ReportRenderer, delete LocalClock/Session::clock + ui/failure.rs [commands/{runs,streams,mod}.rs, main.rs, world/supervise.rs, daemon/{reconcile/progress,client}.rs, ui/failure.rs, timeparse.rs]; M3.148c = narrow Palette/paint/table pub(super) [boot.rs, boot/**, commands/docker/registry.rs, ui/{palette,paint,table,mod}.rs + boot tests]. Dependencies a -> b -> c. No code edited; node parked.

## 2026-11-25T18:26 [lead]

status active -> ready: needs split into M3.148a/b/c; see LOG for exact ownership (Palette/LocalClock/ui::failure/RawPrint cross out-of-path modules)

## 2026-11-25T18:26 [conductor]

Split approved (conductor) after the lead proved the acceptance crosses 4 out-of-path subsystems: 148a status/doctor/commands on Report + RawPrint gate; 148b runs/streams on ReportRenderer + delete LocalClock/ui/failure.rs; 148c narrow Palette/paint/table to pub(super). Serialized a->b->c.

## 2026-11-25T18:26 [conductor]

status ready -> superseded
