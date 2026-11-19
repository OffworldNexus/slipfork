//! `slipfork snow resolve`: daemon-owned `snow.yml` resolution and secret persistence.

use std::path::Path;
use std::process::ExitCode;

use slipfork_edge::DaemonControl;
use slipfork_wire::control::SnowResolveRequest;

use crate::CliApp;
use crate::commands::control::connect;
use crate::commands::render_json;

/// Resolve one `snow.yml` mode through `slipforkd`, redacted unless explicitly exposed.
pub(crate) fn resolve(
    app: &CliApp,
    file: &Path,
    mode: &str,
    component: Option<&str>,
    expose_secrets: bool,
) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    let summary = client
        .snow_resolve(&SnowResolveRequest {
            cwd: app.ui.cwd()?,
            file: file.to_path_buf(),
            mode: mode.to_owned(),
            component: component.map(str::to_owned),
            expose_secrets,
        })
        .map_err(|err| err.to_string())?;
    if app.output_mode.json {
        println!("{}", render_json(&summary.vars)?);
    } else {
        for (key, value) in &summary.vars {
            println!("{key}={value}");
        }
    }
    Ok(ExitCode::SUCCESS)
}
