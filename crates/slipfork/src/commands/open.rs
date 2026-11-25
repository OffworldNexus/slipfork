//! `slipfork open`: the world root URL, or one service route, headless-aware.

use std::process::{Command, ExitCode, Stdio};

use serde_json::json;

use crate::commands::Session;
use crate::ui::Palette;
use crate::ui::report::Report;
use crate::world::WorldIdentities;

/// Parsed `slipfork open` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenArgs {
    /// Open this service's route instead of the world root.
    pub service: Option<String>,
    /// Emit `{url, opened}` with zero ANSI.
    pub json: bool,
}

impl Session {
    /// Opens the world or service URL, or prints it on a headless host.
    pub fn open(&self, args: &OpenArgs) -> ExitCode {
        let identities = match WorldIdentities::open(self.host().state_dir.clone()) {
            Ok(identities) => identities,
            Err(error) => {
                return self.report(
                    "ledger",
                    &error,
                    "run `slipfork doctor` and retry",
                    args.json,
                );
            }
        };
        let world = match identities.name(&self.cwd, None) {
            Ok(world) => world.as_str().to_owned(),
            Err(error) => {
                return self.report(
                    "world",
                    &error,
                    "run `slipfork up` to register this checkout",
                    args.json,
                );
            }
        };
        let base = format!("http://{world}.localhost");
        // No argument selects the world root (`/`), so a multi-route Forkfile never refuses `open`.
        let candidates = match route_candidates(&self.cwd, &base, args.service.as_deref()) {
            Ok(candidates) => candidates,
            Err((code, message, next)) => {
                return self.report(code, &message, &next, args.json);
            }
        };
        let url = if args.service.is_some() {
            if candidates.len() > 1 {
                if !args.json {
                    let palette = Palette::new(self.color());
                    let lines: Vec<String> = candidates
                        .iter()
                        .map(|candidate| palette.url(candidate))
                        .collect();
                    let _ = self.renderer().emit_lines_exit(lines);
                }
                return self.report(
                    "ambiguous-target",
                    "several routes match",
                    "pass a service with one route: `slipfork open <service>`",
                    args.json,
                );
            }
            candidates.into_iter().next().unwrap_or(base)
        } else {
            base
        };
        let headless = self.host.ssh || !self.host.display;
        if args.json {
            return self.renderer().json_exit(
                &json!({ "url": url, "opened": !headless && self.host.opener.is_some() }),
            );
        }
        let palette = Palette::new(self.color());
        if headless {
            return self.renderer().emit_lines_exit(vec![
                format!("{} {}", palette.icon_info(), palette.url(&url)),
                "next: open it on a machine with a browser".to_owned(),
            ]);
        }
        let Some(opener) = &self.host.opener else {
            return self.renderer().emit_lines_exit(vec![
                palette.url(&url),
                "next: install xdg-open, or open the URL in a browser".to_owned(),
            ]);
        };
        match Command::new(opener).arg(&url).stdin(Stdio::null()).status() {
            Ok(status) if status.success() => self.renderer().emit_exit(&Report::notice(format!(
                "{} Opened {}",
                palette.icon_ok(),
                palette.url(&url)
            ))),
            _ => self.report(
                "open-failed",
                &format!("cannot open {url}"),
                "open the URL in a browser, then retry",
                args.json,
            ),
        }
    }
}

/// Route URLs for the world, narrowed to `service` when one was named.
fn route_candidates(
    cwd: &std::path::Path,
    base: &str,
    service: Option<&str>,
) -> Result<Vec<String>, (&'static str, String, String)> {
    let path = cwd.join("Forkfile");
    let source = std::fs::read_to_string(&path).map_err(|error| {
        (
            "forkfile",
            format!("cannot read `{}`: {error}", path.display()),
            "create a Forkfile in this directory, then run `slipfork up`".to_owned(),
        )
    })?;
    let checked = slipfork_forkfile::compile(&source).map_err(|diagnostics| {
        (
            "forkfile",
            diagnostics.first().map_or_else(
                || "invalid Forkfile".to_owned(),
                |item| format!("Forkfile:{}: {}", item.span.line(), item.message),
            ),
            diagnostics
                .first()
                .map_or("fix the Forkfile", |item| item.next.as_str())
                .to_owned(),
        )
    })?;
    let features = crate::world::features::WorldFeatures::load_resolved(cwd, &checked.document)
        .map_err(|error| {
            (
                "feature",
                error,
                crate::world::features::WorldFeatures::REMEDY.to_owned(),
            )
        })?;
    let declared = checked.document.services_with(&features);
    if let Some(name) = service
        && !declared.iter().any(|declared| declared.id == name)
    {
        return Err((
            "no-target",
            format!("`{name}` is not a service in this Forkfile"),
            "run `slipfork ps` to list this world's services".to_owned(),
        ));
    }
    let mut urls: Vec<String> = Vec::new();
    for declared in &declared {
        if service.is_some_and(|name| name != declared.id) {
            continue;
        }
        for route in &declared.routes {
            let path = route.path.clone().unwrap_or_default();
            let url = if path.is_empty() {
                base.to_owned()
            } else {
                format!("{base}{path}")
            };
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    Ok(urls)
}
