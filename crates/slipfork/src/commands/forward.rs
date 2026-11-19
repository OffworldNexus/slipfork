//! `slipfork forward`: daemon-owned listeners bridged into a world, held until Ctrl-C.

use std::io::{self, Write};
use std::process::ExitCode;

use serde_json::json;
use slipfork_edge::DaemonControl;
use slipfork_wire::control::{ForwardRequest, ForwardSummary};

use crate::CliApp;
use crate::commands::control::connect;

/// Bind the daemon's `127.0.0.1:0` listeners and keep the control connection open as the bridge.
pub(crate) fn forward(
    app: &CliApp,
    world: Option<String>,
    targets: Vec<String>,
) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    let request = ForwardRequest {
        cwd: app.ui.cwd()?,
        world,
        targets,
    };
    client
        .forward(&request, &mut |summary: ForwardSummary| {
            print_forward(app.output_mode.json, &summary);
            // The open control connection *is* the bridge lifetime (`ADR-0008` §4): hold it until
            // the process is interrupted, at which point the socket closes and `slipforkd` drops
            // every host listener.
            loop {
                std::thread::park();
            }
        })
        .map_err(|err| err.to_string())?;
    Ok(ExitCode::SUCCESS)
}

fn print_forward(json: bool, summary: &ForwardSummary) {
    if json {
        println!(
            "{}",
            json!({
                "world": &summary.world,
                "targets": summary.targets.iter().map(|target| json!({
                    "id": &target.id,
                    "host_addr": &target.host_addr,
                    "uri": &target.uri,
                })).collect::<Vec<_>>(),
            })
        );
    } else {
        for target in &summary.targets {
            println!(
                "[forward] {:<24} {} ({})",
                target.id, target.uri, target.host_addr
            );
        }
    }
    let _ = io::stdout().flush();
}
