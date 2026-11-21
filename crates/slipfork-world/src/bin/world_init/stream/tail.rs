//! Rolling `logs` replay: always-open readers merged by timestamp, with a short reorder window.

use std::os::unix::net::UnixStream;
use std::thread;

use slipfork_world::control::{InitFrame, PROTOCOL_VERSION, Stream};
use slipfork_world::pod_log::{LogLine, LogMerge, LogReader, REORDER_MS, now_ms};

use crate::Fault;
use crate::pod::WorldShared;

use super::lines::{IDLE, output_kind, peer_closed, refuse, reply, write_output};

/// Replays (and optionally follows) `<pods>/logs/<id>.log` into `stream`, merged by timestamp.
pub(crate) fn serve_logs(
    shared: std::sync::Arc<WorldShared>,
    request_id: String,
    service: Option<String>,
    pods: Option<Vec<String>>,
    follow: bool,
    mut stream: UnixStream,
) -> Result<(), Fault> {
    let mut ids = shared.ids();
    ids.sort();
    if let Some(service) = &service {
        if !ids.iter().any(|id| id == service) {
            return refuse(
                &mut stream,
                &request_id,
                "no-pod",
                format!("pod `{service}` is not running"),
            );
        }
        ids = vec![service.clone()];
    } else if let Some(selected) = pods {
        ids.retain(|id| selected.iter().any(|wanted| wanted == id));
    }
    let mut readers: Vec<(String, LogReader)> = Vec::new();
    for id in &ids {
        let Some(info) = shared.info(id) else {
            continue;
        };
        if let Ok(reader) = LogReader::open(&info.log) {
            readers.push((id.clone(), reader));
        }
    }
    let mut merge = LogMerge::default();
    let mut closed = false;
    loop {
        let mut progress = false;
        for (id, reader) in &mut readers {
            let mut route = |line: LogLine| merge.push(id, line);
            match reader.read_lines(&mut route) {
                Ok(read) => progress |= read > 0,
                Err(source) => {
                    let _ = Fault::cause(
                        format!("replay pod `{id}` log"),
                        "inspect the pod log",
                        source,
                    );
                    closed = true;
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
                Stream::Combined,
                output_kind(line.kind),
                &line.text,
            )?;
        }
        if closed || !follow || shared.stopped() || peer_closed(&stream) {
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
            Stream::Combined,
            output_kind(line.kind),
            &line.text,
        )?;
    }
    for id in &ids {
        write_output(
            &mut stream,
            &request_id,
            id,
            Stream::Combined,
            slipfork_world::OutputKind::Eof,
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
