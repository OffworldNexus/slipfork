//! Deterministic `snow.yml` -> `# forkfile: 0.1` migration.
//!
//! Typed input is checked for cross-references, rendered as a `Forkfile` AST
//! through the canonical formatter, then re-parsed before the caller sees it.

mod build;
mod declarations;
mod render;
mod snow;
mod spans;
mod template;

use crate::diag::{Diagnostic, Diagnostics};
use crate::span::Span;
use build::Builder;
use snow::Snow;

/// Migrates legacy `snow.yml` text into a validated `Forkfile`.
pub fn migrate(source: &str) -> Result<String, Diagnostics> {
    let snow: Snow =
        serde_yaml_ng::from_str(source).map_err(|error| deserialize_error(source, &error))?;
    let mut diags = Diagnostics::new();
    refuse_disabled(source, &snow, &mut diags);
    if !diags.is_empty() {
        return Err(diags);
    }
    let mut builder = Builder::new(source);
    let model = builder.model(&snow);
    if !builder.diags.is_empty() {
        return Err(builder.diags);
    }
    let text = render::render(&model);
    crate::parse(&text).map(|_| text)
}

fn deserialize_error(source: &str, error: &serde_yaml_ng::Error) -> Diagnostics {
    let (line, column) = error.location().map_or((1, 1), |location| {
        (location.line() as u32, location.column() as u32)
    });
    let raw = error.to_string();
    let raw = raw.split(" at line ").next().unwrap_or(raw.as_str());
    let (message, next) = classify(raw);
    let mut diags = Diagnostics::new();
    diags.push(Diagnostic::new(
        Span::at(spans::position(source, line, column)),
        message,
        next,
    ));
    diags
}

fn classify(message: &str) -> (String, &'static str) {
    if message.contains("untagged enum") {
        (
            "value does not match the `snow.yml` schema (a name string or `{ id: … }`)".to_string(),
            "use a name string or `{ id: … }` for `domains` / `s3_buckets` entries",
        )
    } else if message.contains("unknown field") {
        (
            message.to_string(),
            "remove the unknown key; use only the fields documented for that block (top level: `images`, `resources`, `domains`, `envs`, `components`, `ingresses`, `hooks`, `modes`)",
        )
    } else if message.contains("did not find")
        || message.contains("could not find")
        || message.contains("mapping values are not allowed")
        || message.contains("while scanning")
    {
        (
            message.to_string(),
            "fix the YAML at this line (check indentation/quoting)",
        )
    } else {
        (
            message.to_string(),
            "fix the value at this path; see `specs/05-ospp-v1.md` §10 for the accepted shape",
        )
    }
}

fn refuse_disabled(source: &str, snow: &Snow, diags: &mut Diagnostics) {
    for (mode, spec) in &snow.modes {
        for (id, over) in &spec.components {
            if over.disabled == Some(true) {
                let path = [
                    "modes",
                    mode.as_str(),
                    "components",
                    id.as_str(),
                    "disabled",
                ];
                diags.push(Diagnostic::new(
                    Span::at(spans::locate(source, &path)),
                    format!(
                        "modes.{mode}.components.{id}.disabled: a `Forkfile` cannot delete a declared service for one mode"
                    ),
                    "remove the `disabled` override; gate the service with `on <feature>` instead",
                ));
            }
        }
    }
}
