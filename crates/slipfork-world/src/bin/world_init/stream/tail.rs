//! Rolling `logs` replay: indexed readers merged by timestamp, with a short reorder window.
//!
//! Every id, live or stopped, reads the canonical `<store>/logs/<world>/<id>.log` through
//! [`read_range`], so a `--since`/`--until` window seeks past the head of a large capture instead
//! of scanning it. Live pods additionally stay open and stream new lines as the pumps append them.

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::thread;

use slipfork_world::control::{InitFrame, PROTOCOL_VERSION, Stream};
use slipfork_world::pod_log::{LogMerge, LogRange, REORDER_MS, log_path, now_ms, read_range};

use crate::Fault;
use crate::pod::WorldShared;

use super::lines::{IDLE, output_kind, peer_closed, refuse, reply, write_output};

/// Replays (and optionally follows) captured pod output into `stream`, merged by timestamp.
#[allow(clippy::too_many_arguments)]
pub(crate) fn serve_logs(
    shared: Arc<WorldShared>,
    request_id: String,
    service: Option<String>,
    pods: Option<Vec<String>>,
    follow: bool,
    since_ms: Option<u64>,
    until_ms: Option<u64>,
    mut stream: UnixStream,
) -> Result<(), Fault> {
    let logs_root = shared.logs_root();
    let mut ids = shared.ids();
    if let Some(root) = &logs_root
        && let Ok(entries) = std::fs::read_dir(root)
    {
        for entry in entries.flatten() {
            if let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".log"))
                && !ids.iter().any(|live| live == id)
            {
                ids.push(id.to_owned());
            }
        }
    }
    ids.sort();
    if let Some(service) = &service {
        if !ids.iter().any(|id| id == service) {
            return refuse(
                &mut stream,
                &request_id,
                "no-pod",
                format!("pod `{service}` is not running and has no capture file"),
            );
        }
        ids = vec![service.clone()];
    } else if let Some(selected) = pods {
        ids.retain(|id| selected.iter().any(|wanted| wanted == id));
    }
    let mut readers: Vec<(String, LogRange)> = Vec::new();
    let mut failure = None;
    if let Some(root) = &logs_root {
        for id in &ids {
            let path = shared
                .info(id)
                .map_or_else(|| log_path(root, id), |info| info.log);
            if !path.is_file() {
                continue;
            }
            match read_range(root, id, since_ms, until_ms) {
                Ok(reader) => readers.push((id.clone(), reader)),
                Err(source) => {
                    failure = Some(Fault::cause(
                        format!("replay pod `{id}` log"),
                        "inspect the pod log and its `.idx` under `<store>/logs/<world>`",
                        source,
                    ));
                    break;
                }
            }
        }
    }
    let live = |id: &str| shared.info(id).is_some();
    let mut merge = LogMerge::default();
    loop {
        let mut progress = false;
        for (id, reader) in &mut readers {
            let mut route = |line| merge.push(id, line);
            match reader.read_lines(&mut route) {
                Ok(read) => progress |= read > 0,
                Err(source) => {
                    failure = Some(Fault::cause(
                        format!("replay pod `{id}` log"),
                        "inspect the pod log under `<store>/logs/<world>`",
                        source,
                    ));
                    break;
                }
            }
        }
        let batch = if follow {
            merge.drain(now_ms().saturating_sub(REORDER_MS))
        } else {
            merge.flush()
        };
        for (pod, line) in batch {
            write_output(
                &mut stream,
                &request_id,
                &pod,
                line.stream,
                output_kind(line.kind),
                Some(line.ts_ms),
                &line.text,
            )?;
        }
        let all_stopped = ids.iter().all(|id| !live(id));
        if failure.is_some()
            || !follow
            || (all_stopped && !progress)
            || shared.stopped()
            || peer_closed(&stream)
        {
            break;
        }
        if !progress {
            thread::sleep(IDLE);
        }
    }
    for (pod, line) in merge.flush() {
        write_output(
            &mut stream,
            &request_id,
            &pod,
            line.stream,
            output_kind(line.kind),
            Some(line.ts_ms),
            &line.text,
        )?;
    }
    if let Some(fault) = failure {
        eprintln!("world init: {fault}");
        // Report the typed failure before EOF so the client renders a remedy, not an empty replay.
        return refuse(&mut stream, &request_id, "replay-failed", fault.to_string());
    }
    for id in &ids {
        write_output(
            &mut stream,
            &request_id,
            id,
            Stream::Combined,
            slipfork_world::OutputKind::Eof,
            Some(now_ms()),
            "",
        )?;
    }
    reply(
        &mut stream,
        &InitFrame::Reply {
            v: PROTOCOL_VERSION,
            id: request_id,
            ok: Some(serde_json::json!({ "pods": ids, "follow": follow })),
            error: None,
        },
    )
}
