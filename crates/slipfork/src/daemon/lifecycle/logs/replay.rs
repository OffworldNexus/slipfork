//! Capture replay and catalog listing for [`super::LogStream`].

use std::path::Path;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use slipfork_world::control::{PROTOCOL_VERSION, Request, StreamClient, StreamFrame};
use slipfork_world::pod_log::{LogKind, LogMerge, REORDER_MS, now_ms, read_range};
use slipfork_world::run_log::RunRecord;

use super::super::super::Failure;
use super::LogStream;

impl LogStream {
    /// Emits one `runs` frame carrying every catalog entry, newest last.
    pub(super) fn list_runs(&self, emit: &mut dyn FnMut(Value) -> bool) -> Result<Value, Failure> {
        let store = self.store.as_ref().ok_or_else(|| {
            Failure::new("no-world", "✗ no world log store\nnext: run `slipfork up`")
        })?;
        let catalog = store.records().map_err(|error| {
            Failure::new(
                "run-index",
                format!(
                    "✗ cannot read `{}`: {error}\nnext: check that `{}` is readable",
                    store.index().display(),
                    store.run_dir().display()
                ),
            )
        })?;
        let rows: Vec<Value> = catalog
            .records
            .iter()
            .filter_map(|record| serde_json::to_value(record).ok())
            .collect();
        emit(json!({ "event": "runs", "rows": rows, "skipped": catalog.skipped }));
        Ok(json!({ "runs": catalog.records.len(), "skipped": catalog.skipped }))
    }

    /// Replays one already-resolved run through the indexed window reader.
    pub(super) fn replay_run(
        &self,
        run_id: &str,
        record: Option<&RunRecord>,
        emit: &mut dyn FnMut(Value) -> bool,
        alive: &mut dyn FnMut() -> bool,
    ) -> Result<Value, Failure> {
        let store = self.store.as_ref().ok_or_else(|| {
            Failure::new("no-world", "✗ no world log store\nnext: run `slipfork up`")
        })?;
        let mut reader = read_range(&store.run_dir(), run_id, self.since_ms, self.until_ms)
            .map_err(|error| {
                Failure::new(
                    "no-run",
                    format!(
                        "✗ cannot read run `{run_id}`: {error}\nnext: retry with `slipfork logs -r last`"
                    ),
                )
            })?;
        // `-r --json` mirrors the catalog record the replay belongs to, then its captured lines.
        let mut roster = json!({
            "kind": "roster",
            "pod": run_id,
            "stream": "combined",
            "line": "",
            "pods": [run_id],
        });
        if let Some(record) = record
            && let Ok(value) = serde_json::to_value(record)
        {
            roster["run"] = value;
        }
        if !emit(roster) {
            return Ok(json!({ "closed": true }));
        }
        loop {
            let mut lines = Vec::new();
            let read = reader
                .read_lines(&mut |line| lines.push(line.clone()))
                .unwrap_or(0);
            for line in lines {
                emit(line_event(
                    run_id,
                    kind_name(line.kind),
                    line.stream.as_str(),
                    &line.text,
                    Some(line.ts_ms),
                ));
            }
            // A replay must reach EOF before flushing: one pass fills a single 64 KiB chunk, so a
            // 240 KiB capture needs several drains before its trailing partial line is emitted.
            let finishing = !self.follow || !alive();
            if finishing && read == 0 {
                let mut rest = Vec::new();
                reader.flush(&mut |line| rest.push(line.clone()));
                for line in rest {
                    emit(line_event(
                        run_id,
                        kind_name(line.kind),
                        line.stream.as_str(),
                        &line.text,
                        Some(line.ts_ms),
                    ));
                }
                return Ok(json!({ "closed": false }));
            }
            if !finishing {
                thread::sleep(Duration::from_millis(40));
            }
        }
    }

    /// Streams the world's captured output for the resolved ids; the roster fixes the gutter.
    pub(super) fn stream_pods(
        &self,
        pods_dir: &Path,
        emit: &mut dyn FnMut(Value) -> bool,
        alive: &mut dyn FnMut() -> bool,
    ) -> Result<Value, Failure> {
        let mut client = StreamClient::open(pods_dir)
            .map_err(|error| Failure::new("world-unavailable", error.to_string()))?;
        client
            .send(&Request::Logs {
                v: PROTOCOL_VERSION,
                id: "daemon-logs".to_owned(),
                service: self.service.clone(),
                pods: self.pods.clone(),
                follow: self.follow,
                since_ms: self.since_ms,
                until_ms: self.until_ms,
            })
            .map_err(|error| Failure::new("world-unavailable", error.to_string()))?;
        if !emit(json!({
            "kind": "roster",
            "pod": "",
            "stream": "combined",
            "line": "",
            "pods": self.roster,
        })) {
            client.close();
            return Ok(json!({ "closed": true }));
        }
        loop {
            if !alive() {
                client.close();
                return Ok(json!({ "closed": true }));
            }
            match client.poll() {
                Ok(Some(StreamFrame::Output(event))) => {
                    if !emit(line_event(
                        &event.pod,
                        event.kind.as_str(),
                        event.stream.as_str(),
                        &event.line,
                        event.ts_ms,
                    )) {
                        client.close();
                        return Ok(json!({ "closed": true }));
                    }
                }
                Ok(Some(StreamFrame::Reply { ok, error })) => {
                    return super::super::stream::finish(ok, error);
                }
                Ok(None) => thread::sleep(Duration::from_millis(40)),
                Err(error) => return Err(Failure::new("world-stream", error.to_string())),
            }
        }
    }

    /// Replays `<store>/logs/<world>/<id>.log` through the indexed window reader.
    pub(super) fn read_captures(
        &self,
        emit: &mut dyn FnMut(Value) -> bool,
        alive: &mut dyn FnMut() -> bool,
    ) -> Result<Value, Failure> {
        let Some(store) = &self.store else {
            return Err(Failure::new(
                "no-target",
                "✗ no world log store\nnext: run `slipfork up`",
            ));
        };
        let logs_dir = store.world_logs();
        // A single image id replays its own run capture, so its header and its lines always come
        // from the same run instead of a stale append-only `<image>.log`.
        let image_run = (self.replay_ids.len() == 1)
            .then(|| {
                self.catalog
                    .as_ref()
                    .and_then(|index| index.newest_for_target(&self.replay_ids[0]))
                    .filter(|record| record.kind == "image")
                    .cloned()
            })
            .flatten();
        let mut readers = Vec::new();
        for id in &self.replay_ids {
            if let Some(record) = &image_run {
                let reader = read_range(
                    &store.run_dir(),
                    &record.run_id,
                    self.since_ms,
                    self.until_ms,
                )
                .map_err(|error| {
                    Failure::new(
                        "no-log",
                        format!(
                            "✗ cannot read run `{}` for `{id}`: {error}\nnext: retry with `slipfork logs -r last`",
                            record.run_id
                        ),
                    )
                })?;
                readers.push((id.clone(), reader));
                continue;
            }
            match read_range(&logs_dir, id, self.since_ms, self.until_ms) {
                Ok(reader) => readers.push((id.clone(), reader)),
                Err(_) if !self.explicit => {}
                Err(error) => {
                    // A failed build/hook wrote its one capture under `runs/`, not a live
                    // `<world_logs>/<id>.log`; replay the request's catalog record for this target.
                    if let Some(record) = self
                        .catalog
                        .as_ref()
                        .and_then(|index| index.newest_for_target(id))
                        && let Ok(reader) = read_range(
                            &store.run_dir(),
                            &record.run_id,
                            self.since_ms,
                            self.until_ms,
                        )
                    {
                        readers.push((id.clone(), reader));
                        continue;
                    }
                    return Err(Failure::new(
                        "no-log",
                        format!(
                            "✗ no captured logs for `{id}` in `{}` ({error})\nnext: check that `{}` is readable",
                            logs_dir.display(),
                            logs_dir.display()
                        ),
                    ));
                }
            }
        }
        if readers.is_empty() {
            return Err(Failure::new(
                "no-target",
                format!(
                    "✗ no captured logs in `{}`\nnext: run `slipfork up`",
                    logs_dir.display()
                ),
            ));
        }
        let roster: Vec<String> = readers.iter().map(|(id, _)| id.clone()).collect();
        let mut frame = json!({
            "kind": "roster",
            "pod": "",
            "stream": "combined",
            "line": "",
            "pods": roster,
        });
        // One resolved capture still names its catalog record, so the replay's header describes the
        // run its lines came from. A service capture keeps no header.
        if let Some(record) = &image_run
            && let Ok(value) = serde_json::to_value(record)
        {
            frame["run"] = value;
        }
        if !emit(frame) {
            return Ok(json!({ "closed": true }));
        }
        let mut merge = LogMerge::default();
        loop {
            let mut read_any = false;
            for (id, reader) in &mut readers {
                let read = reader
                    .read_lines(&mut |line| {
                        read_any = true;
                        merge.push(id, line);
                    })
                    .unwrap_or(0);
                read_any |= read > 0;
            }
            for (pod, line) in merge.drain(now_ms().saturating_sub(REORDER_MS)) {
                emit(line_event(
                    &pod,
                    kind_name(line.kind),
                    line.stream.as_str(),
                    &line.text,
                    Some(line.ts_ms),
                ));
            }
            if !self.follow {
                // A replay must reach EOF across every capture before flushing: one pass fills a
                // single 64 KiB chunk per reader, so keep merging until no file has bytes left.
                if read_any {
                    continue;
                }
                for (id, reader) in &mut readers {
                    reader.flush(&mut |line| merge.push(id, line));
                }
                for (pod, line) in merge.flush() {
                    emit(line_event(
                        &pod,
                        kind_name(line.kind),
                        line.stream.as_str(),
                        &line.text,
                        Some(line.ts_ms),
                    ));
                }
                return Ok(json!({ "closed": false }));
            }
            if !alive() {
                return Ok(json!({ "closed": true }));
            }
            if !read_any {
                thread::sleep(Duration::from_millis(40));
            }
        }
    }
}

/// One daemon event carrying `ts_ms` only when the capture knew it and always its stream.
fn line_event(pod: &str, kind: &str, stream: &str, line: &str, ts_ms: Option<u64>) -> Value {
    let mut event = json!({
        "kind": kind,
        "pod": pod,
        "stream": stream,
        "line": line,
    });
    if let Some(ts_ms) = ts_ms {
        event["ts_ms"] = json!(ts_ms);
    }
    event
}

fn kind_name(kind: LogKind) -> &'static str {
    match kind {
        LogKind::Line => "line",
        LogKind::Divider => "divider",
        LogKind::Step => "step",
    }
}
